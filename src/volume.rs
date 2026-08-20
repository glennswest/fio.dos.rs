//! The public API: read, write, mkdir, unlink, stat, list.
//!
//! A [`Volume`] is a FAT filesystem you can put files into. Everything below it
//! — the allocator, the directory slots, the long names — exists so that this
//! surface can be small.
//!
//! # What it maintains
//!
//! Every mutation keeps in step the things `fsck.fat` checks: the FAT (in every
//! copy of it), the FSInfo free count and next-free hint on FAT32, each
//! directory entry's size and first cluster, and the timestamps. A filesystem
//! written through this crate passes `fsck.fat` afterwards, and that is the test
//! the crate is held to.
//!
//! # Durability
//!
//! Nothing is durable until [`Volume::flush`]. The FAT is cached in memory
//! while the volume is open — see [`crate::alloc`] for why — and directory and
//! file writes reach the device as they are made, but the FAT and the FSInfo
//! sector go out together at the end. A volume dropped without a flush leaves
//! the file data on the device and no chain pointing at it.

use mkfs_dos::device::BlockDevice;
use mkfs_dos::fs::Filesystem;
use mkfs_dos::params::FatType;
use mkfs_dos::structs::dirent::{Attributes, DirEntry, DosTime};

use crate::alloc::Allocator;
use crate::dir;
use crate::error::{Error, Result};
use crate::name;

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name, long if the file has one.
    pub name: String,
    /// The 8.3 name, which is what a driver without long-name support sees.
    pub short_name: String,
    /// Is it a directory?
    pub is_dir: bool,
    /// Size in bytes. Always zero for a directory — FAT does not record one.
    pub size: u64,
    /// Attribute bits.
    pub attributes: Attributes,
    /// Last modification, as a Unix timestamp in UTC.
    pub modified: i64,
    /// Creation, as a Unix timestamp in UTC.
    pub created: i64,
    /// First cluster of the chain, zero for an empty file.
    pub first_cluster: u32,
}

/// What [`Volume::stat`] reports.
pub type Stat = Entry;

/// Attributes to give a file as it is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    /// The attribute bits.
    pub attributes: Attributes,
    /// Modification time, Unix seconds. `None` takes the volume's clock.
    pub modified: Option<i64>,
}

impl Attrs {
    /// Read-only, so a DOS or Windows driver refuses to change the file.
    pub fn read_only() -> Self {
        Self {
            attributes: Attributes::READ_ONLY,
            ..Default::default()
        }
    }

    /// Hidden, and a system file — what a boot loader's own files often carry.
    pub fn system() -> Self {
        Self {
            attributes: Attributes::HIDDEN | Attributes::SYSTEM,
            ..Default::default()
        }
    }

    /// Fix the modification time, for a reproducible image.
    pub fn modified_at(mut self, secs: i64) -> Self {
        self.modified = Some(secs);
        self
    }
}

/// A FAT filesystem, open for reading and writing.
pub struct Volume<D: BlockDevice> {
    fs: Filesystem<D>,
    alloc: Allocator,
    time: Option<i64>,
}

/// Where a directory's contents live: the fixed root, or a cluster chain.
type DirRef = Option<u32>;

impl<D: BlockDevice> Volume<D> {
    /// Open a volume on a device.
    ///
    /// Reads the boot sector and the FAT. It does not check the filesystem —
    /// `mkfs_dos::fsck` does that — but a boot sector that does not decode is
    /// refused here rather than at the first read.
    pub async fn open(device: D) -> Result<Self> {
        let fs = Filesystem::open(device).await?;
        let alloc = Allocator::load(&fs).await?;
        Ok(Self {
            fs,
            alloc,
            time: None,
        })
    }

    /// Fix the clock, so that an image built twice comes out the same.
    ///
    /// Without it, every file written carries the time it was written, and two
    /// runs of the same build produce different images.
    pub fn set_time(&mut self, secs: i64) {
        self.time = Some(secs);
    }

    /// The filesystem underneath.
    pub fn filesystem(&self) -> &Filesystem<D> {
        &self.fs
    }

    /// Clusters not in use.
    pub fn free_clusters(&self) -> u32 {
        self.alloc.free_count()
    }

    /// Bytes not in use.
    pub fn free_bytes(&self) -> u64 {
        self.alloc.free_count() as u64 * self.fs.cluster_size() as u64
    }

    /// Write out everything still held in memory.
    ///
    /// The FAT and the FSInfo sector. Until this returns, the volume on the
    /// device does not name the clusters the files were written into.
    pub async fn flush(&mut self) -> Result<()> {
        self.alloc.flush(&self.fs).await?;
        self.fs.flush().await?;
        Ok(())
    }

    /// The volume label, from the root directory entry that carries it.
    ///
    /// The boot sector holds a copy, and this reads the directory entry —
    /// which is the one Windows shows and the one that is authoritative when
    /// the two disagree.
    pub async fn label(&self) -> Result<Option<String>> {
        let root = self.read_dir_bytes(None).await?;
        Ok(dir::find_label(&root).map(|(_, e)| e.short_name()))
    }

    /// Set the volume label, in the root directory and in the boot sector.
    ///
    /// Both, always: a volume whose two labels disagree shows one name to
    /// Windows and another to `blkid`.
    pub async fn set_label(&mut self, label: &str) -> Result<()> {
        let raw = mkfs_dos::params::encode_label(label)
            .map_err(|e| Error::invalid_name(label, e.to_string()))?;

        let mut root = self.read_dir_bytes(None).await?;
        let now = self.now();
        match dir::find_label(&root) {
            Some((slot, mut entry)) => {
                entry.name = raw;
                entry.time = now.time;
                entry.date = now.date;
                dir::write_short(&mut root, slot, &entry);
            }
            None => {
                let entry = DirEntry {
                    name: raw,
                    attr: Attributes::VOLUME_ID,
                    ctime: now.time,
                    cdate: now.date,
                    adate: now.date,
                    time: now.time,
                    date: now.date,
                    ..Default::default()
                };
                let slot = match dir::find_free_run(&root, 1) {
                    Some(slot) => slot,
                    None => {
                        self.grow_dir(None, &mut root).await?;
                        dir::find_free_run(&root, 1).expect("a grown directory has a free slot")
                    }
                };
                dir::write_short(&mut root, slot, &entry);
            }
        }
        self.write_dir_bytes(None, &root).await?;

        // And the copy in the boot sector.
        let mut sector = vec![0u8; self.fs.boot().bytes_per_sector as usize];
        self.fs.device().read_at(0, &mut sector).await?;
        let mut boot = self.fs.boot().clone();
        boot.volume_label = raw;
        boot.encode(&mut sector);
        self.fs.device().write_at(0, &sector).await?;
        if self.fs.fat_type() == FatType::Fat32 && self.fs.boot().backup_boot != 0 {
            let off = self
                .fs
                .boot()
                .sector_offset(self.fs.boot().backup_boot as u64);
            self.fs.device().write_at(off, &sector).await?;
        }
        Ok(())
    }

    /// Does this path exist?
    pub async fn exists(&self, path: &str) -> Result<bool> {
        Ok(self.find(path).await?.is_some())
    }

    /// Metadata for a path.
    pub async fn stat(&self, path: &str) -> Result<Stat> {
        if is_root(path) {
            return Ok(Entry {
                name: "/".to_string(),
                short_name: "/".to_string(),
                is_dir: true,
                size: 0,
                attributes: Attributes::DIRECTORY,
                modified: 0,
                created: 0,
                first_cluster: if self.fs.fat_type() == FatType::Fat32 {
                    self.fs.boot().root_cluster
                } else {
                    0
                },
            });
        }
        let found = self
            .find(path)
            .await?
            .ok_or_else(|| Error::NotFound(path.to_string()))?;
        Ok(to_entry(&found.entry))
    }

    /// List a directory.
    ///
    /// `.` and `..` are not listed — they are bookkeeping, not names — and
    /// neither is the volume label.
    pub async fn read_dir(&self, path: &str) -> Result<Vec<Entry>> {
        let dir = self.dir_ref(path).await?;
        let bytes = self.read_dir_bytes(dir).await?;
        Ok(dir::parse(&bytes)
            .iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(to_entry)
            .collect())
    }

    /// Read a whole file.
    pub async fn read(&self, path: &str) -> Result<Vec<u8>> {
        let found = self
            .find(path)
            .await?
            .ok_or_else(|| Error::NotFound(path.to_string()))?;
        if found.entry.is_dir() {
            return Err(Error::IsADirectory(path.to_string()));
        }

        let size = found.entry.entry.size as usize;
        let first = found.entry.first_cluster();
        if size == 0 || first == 0 {
            return Ok(Vec::new());
        }

        let chain = self.alloc.chain(first)?;
        let cluster_size = self.fs.cluster_size() as usize;
        let mut out = vec![0u8; chain.len() * cluster_size];
        for (i, &cluster) in chain.iter().enumerate() {
            self.fs
                .device()
                .read_at(
                    self.fs.cluster_offset(cluster),
                    &mut out[i * cluster_size..(i + 1) * cluster_size],
                )
                .await?;
        }
        // The chain is a whole number of clusters; the size says where the file
        // really ends inside the last one.
        out.truncate(size.min(out.len()));
        Ok(out)
    }

    /// Write a file, replacing it if it exists.
    pub async fn write(&mut self, path: &str, data: &[u8]) -> Result<()> {
        self.write_with(path, data, &Attrs::default()).await
    }

    /// Write a file with attributes.
    pub async fn write_with(&mut self, path: &str, data: &[u8], attrs: &Attrs) -> Result<()> {
        if data.len() as u64 > u32::MAX as u64 {
            return Err(Error::FileTooLarge {
                path: path.to_string(),
                size: data.len() as u64,
            });
        }
        let (parent, name) = self.split(path).await?;
        let mut bytes = self.read_dir_bytes(parent).await?;
        let entries = dir::parse(&bytes);
        let existing = entries.iter().find(|e| name::names_equal(&e.name, &name));

        let cluster_size = self.fs.cluster_size() as usize;
        let needed = data.len().div_ceil(cluster_size) as u32;

        // For a new file the slots are reserved first. Allocating the data and
        // then failing to place the name — a full fixed root, most often —
        // would leave the clusters allocated and unreachable, which is exactly
        // the "lost clusters" `fsck.fat` reports.
        let placement = match existing {
            Some(_) => None,
            None => {
                let (fragments, short) = dir::build_set(&name, &entries)?;
                let at = self
                    .free_run(parent, &mut bytes, fragments.len() + 1)
                    .await?;
                Some((fragments, short, at))
            }
        };

        let chain = match existing {
            Some(found) if found.is_dir() => {
                return Err(Error::IsADirectory(path.to_string()));
            }
            Some(found) => {
                // Reuse what the file already has, so rewriting a file in place
                // does not move it and does not need the space twice.
                let first = found.first_cluster();
                let have = if first == 0 {
                    Vec::new()
                } else {
                    self.alloc.chain(first)?
                };
                self.fit_chain(have, needed)?
            }
            None => self.alloc.allocate(needed)?,
        };

        self.write_data(&chain, data).await?;

        let now = attrs.modified.map(DosTime::from_unix).unwrap_or(self.now());
        let mut entry = match existing {
            Some(found) => found.entry.clone(),
            None => DirEntry {
                ctime: now.time,
                cdate: now.date,
                ..Default::default()
            },
        };
        entry.attr = attrs.attributes | Attributes::ARCHIVE;
        entry.size = data.len() as u32;
        entry.set_first_cluster(chain.first().copied().unwrap_or(0));
        entry.time = now.time;
        entry.date = now.date;
        entry.adate = now.date;

        match (existing, placement) {
            (Some(found), _) => {
                let slot = found.short_slot;
                dir::write_short(&mut bytes, slot, &entry);
            }
            (None, Some((fragments, short, at))) => {
                entry.name = short.raw;
                entry.lcase = short.lcase;
                dir::write_set(&mut bytes, at, &fragments, &entry);
            }
            (None, None) => unreachable!("a new file always reserves its slots"),
        }
        self.write_dir_bytes(parent, &bytes).await?;
        Ok(())
    }

    /// Append to a file, creating it if it does not exist.
    pub async fn append(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let mut existing = match self.find(path).await? {
            Some(found) if found.entry.is_dir() => {
                return Err(Error::IsADirectory(path.to_string()))
            }
            Some(_) => self.read(path).await?,
            None => Vec::new(),
        };
        existing.extend_from_slice(data);
        self.write(path, &existing).await
    }

    /// Create a directory. Its parent must exist.
    pub async fn mkdir(&mut self, path: &str) -> Result<()> {
        let (parent, name) = self.split(path).await?;
        let mut bytes = self.read_dir_bytes(parent).await?;
        let entries = dir::parse(&bytes);
        if entries.iter().any(|e| name::names_equal(&e.name, &name)) {
            return Err(Error::Exists(path.to_string()));
        }

        // The slots come first here too: a directory whose cluster is allocated
        // and whose name will not fit is a lost cluster.
        let (fragments, short) = dir::build_set(&name, &entries)?;
        let at = self
            .free_run(parent, &mut bytes, fragments.len() + 1)
            .await?;

        // A directory is one cluster holding "." and "..", and nothing else.
        let chain = self.alloc.allocate(1)?;
        let cluster = chain[0];
        let now = self.now();

        let mut contents = vec![0u8; self.fs.cluster_size() as usize];
        let mut dot = DirEntry {
            name: *b".          ",
            attr: Attributes::DIRECTORY,
            ctime: now.time,
            cdate: now.date,
            adate: now.date,
            time: now.time,
            date: now.date,
            ..Default::default()
        };
        dot.set_first_cluster(cluster);
        let mut dotdot = dot.clone();
        dotdot.name = *b"..         ";
        // ".." names the parent — and names cluster zero when the parent is the
        // root, on every width, including a FAT32 root that has a cluster
        // number of its own.
        dotdot.set_first_cluster(parent.unwrap_or(0));
        dir::write_short(&mut contents, 0, &dot);
        dir::write_short(&mut contents, 1, &dotdot);
        self.fs.device()
            .write_at(self.fs.cluster_offset(cluster), &contents)
            .await?;

        let mut entry = DirEntry {
            name: short.raw,
            lcase: short.lcase,
            attr: Attributes::DIRECTORY,
            ctime: now.time,
            cdate: now.date,
            adate: now.date,
            time: now.time,
            date: now.date,
            ..Default::default()
        };
        entry.set_first_cluster(cluster);

        dir::write_set(&mut bytes, at, &fragments, &entry);
        self.write_dir_bytes(parent, &bytes).await?;
        Ok(())
    }

    /// Create a directory and every parent it needs.
    pub async fn mkdir_all(&mut self, path: &str) -> Result<()> {
        let mut so_far = String::new();
        for component in components(path) {
            so_far.push('/');
            so_far.push_str(component);
            match self.find(&so_far).await? {
                Some(found) if found.entry.is_dir() => continue,
                Some(_) => return Err(Error::NotADirectory(so_far)),
                None => self.mkdir(&so_far).await?,
            }
        }
        Ok(())
    }

    /// Remove a file.
    pub async fn unlink(&mut self, path: &str) -> Result<()> {
        let (parent, name) = self.split(path).await?;
        let mut bytes = self.read_dir_bytes(parent).await?;
        let entries = dir::parse(&bytes);
        let found = entries
            .iter()
            .find(|e| name::names_equal(&e.name, &name))
            .ok_or_else(|| Error::NotFound(path.to_string()))?;
        if found.is_dir() {
            return Err(Error::IsADirectory(path.to_string()));
        }

        if found.first_cluster() != 0 {
            self.alloc.free_chain(found.first_cluster())?;
        }
        dir::erase_set(&mut bytes, found);
        self.write_dir_bytes(parent, &bytes).await?;
        Ok(())
    }

    /// Remove an empty directory.
    pub async fn rmdir(&mut self, path: &str) -> Result<()> {
        let (parent, name) = self.split(path).await?;
        let mut bytes = self.read_dir_bytes(parent).await?;
        let entries = dir::parse(&bytes);
        let found = entries
            .iter()
            .find(|e| name::names_equal(&e.name, &name))
            .ok_or_else(|| Error::NotFound(path.to_string()))?;
        if !found.is_dir() {
            return Err(Error::NotADirectory(path.to_string()));
        }

        let contents = self.read_dir_bytes(Some(found.first_cluster())).await?;
        let live = dir::parse(&contents)
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .count();
        if live > 0 {
            return Err(Error::NotEmpty(path.to_string()));
        }

        self.alloc.free_chain(found.first_cluster())?;
        dir::erase_set(&mut bytes, found);
        self.write_dir_bytes(parent, &bytes).await?;
        Ok(())
    }

    /// Remove a path and everything under it.
    pub async fn remove_all(&mut self, path: &str) -> Result<()> {
        let found = match self.find(path).await? {
            Some(found) => found,
            None => return Ok(()),
        };
        if !found.entry.is_dir() {
            return self.unlink(path).await;
        }

        // Depth first, since a directory can only be removed once it is empty.
        let children = self.read_dir(path).await?;
        for child in children {
            let child_path = join(path, &child.name);
            if child.is_dir {
                Box::pin(self.remove_all(&child_path)).await?;
            } else {
                self.unlink(&child_path).await?;
            }
        }
        self.rmdir(path).await
    }

    /// Rename or move a file or directory.
    pub async fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let (from_parent, from_name) = self.split(from).await?;
        let (to_parent, to_name) = self.split(to).await?;

        let from_bytes = self.read_dir_bytes(from_parent).await?;
        let source = dir::parse(&from_bytes)
            .into_iter()
            .find(|e| name::names_equal(&e.name, &from_name))
            .ok_or_else(|| Error::NotFound(from.to_string()))?;

        let same_dir = from_parent == to_parent;
        let mut to_bytes = if same_dir {
            from_bytes.clone()
        } else {
            self.read_dir_bytes(to_parent).await?
        };
        let to_entries = dir::parse(&to_bytes);
        if to_entries.iter().any(|e| name::names_equal(&e.name, &to_name))
            && !(same_dir && name::names_equal(&from_name, &to_name))
        {
            return Err(Error::Exists(to.to_string()));
        }
        if source.is_dir() && to.starts_with(&format!("{}/", from.trim_end_matches('/'))) {
            return Err(Error::InvalidPath(format!(
                "{to} is inside {from}; a directory cannot be moved into itself"
            )));
        }

        // Remove the old name first when both names live in the same directory,
        // so the new set can reuse the slots the old one held.
        let mut working = to_bytes.clone();
        if same_dir {
            dir::erase_set(&mut working, &source);
            to_bytes = working.clone();
        }

        let existing = dir::parse(&to_bytes);
        let (fragments, short) = dir::build_set(&to_name, &existing)?;
        let mut entry = source.entry.clone();
        entry.name = short.raw;
        entry.lcase = short.lcase;

        let at = self
            .free_run(to_parent, &mut to_bytes, fragments.len() + 1)
            .await?;
        dir::write_set(&mut to_bytes, at, &fragments, &entry);
        self.write_dir_bytes(to_parent, &to_bytes).await?;

        if !same_dir {
            let mut from_bytes = self.read_dir_bytes(from_parent).await?;
            if let Some(found) = dir::parse(&from_bytes)
                .into_iter()
                .find(|e| name::names_equal(&e.name, &from_name))
            {
                dir::erase_set(&mut from_bytes, &found);
                self.write_dir_bytes(from_parent, &from_bytes).await?;
            }

            // A directory that moved has a ".." that now points at the wrong
            // parent. Leaving it is how a tree ends up with a cycle in it.
            if source.is_dir() {
                let cluster = source.first_cluster();
                let mut contents = self.read_dir_bytes(Some(cluster)).await?;
                if let Some(dotdot) = dir::parse(&contents)
                    .into_iter()
                    .find(|e| e.name == "..")
                {
                    let mut updated = dotdot.entry.clone();
                    updated.set_first_cluster(to_parent.unwrap_or(0));
                    dir::write_short(&mut contents, dotdot.short_slot, &updated);
                    self.write_dir_bytes(Some(cluster), &contents).await?;
                }
            }
        }
        Ok(())
    }

    /// Set a file's attribute bits.
    pub async fn set_attributes(&mut self, path: &str, attributes: Attributes) -> Result<()> {
        self.update_entry(path, |entry| {
            // The directory bit describes what the entry *is* and is not the
            // caller's to change; a file that claims to be a directory is a
            // filesystem `fsck.fat` reports.
            let kind = entry.attr & Attributes::DIRECTORY;
            entry.attr = (attributes - Attributes::DIRECTORY) | kind;
        })
        .await
    }

    /// Set a file's modification time, in Unix seconds.
    pub async fn set_modified(&mut self, path: &str, secs: i64) -> Result<()> {
        let t = DosTime::from_unix(secs);
        self.update_entry(path, |entry| {
            entry.time = t.time;
            entry.date = t.date;
            entry.adate = t.date;
        })
        .await
    }

    // ---- internals ------------------------------------------------------

    /// The timestamp to stamp on a change.
    fn now(&self) -> DosTime {
        match self.time {
            Some(secs) => DosTime::from_unix(secs),
            None => DosTime::from_unix(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
            ),
        }
    }

    /// Read a directory's bytes.
    ///
    /// The chain is followed through the allocator's cached FAT rather than the
    /// device's, because the device's copy is behind until the next flush. A
    /// directory grown a moment ago is a chain the device does not know about
    /// yet, and reading it from there gets one cluster and stops.
    async fn read_dir_bytes(&self, dir: DirRef) -> Result<Vec<u8>> {
        if dir.is_none() && self.fs.fat_type() != FatType::Fat32 {
            let ss = self.fs.boot().bytes_per_sector as usize;
            let len = self.fs.boot().root_dir_sectors() as usize * ss;
            let mut buf = vec![0u8; len];
            self.fs
                .device()
                .read_at(self.fs.boot().root_dir_offset(), &mut buf)
                .await?;
            return Ok(buf);
        }

        let start = dir.unwrap_or(self.fs.boot().root_cluster);
        let chain = self.alloc.chain(start)?;
        let cluster_size = self.fs.cluster_size() as usize;
        let mut buf = vec![0u8; chain.len() * cluster_size];
        for (i, &cluster) in chain.iter().enumerate() {
            self.fs
                .device()
                .read_at(
                    self.fs.cluster_offset(cluster),
                    &mut buf[i * cluster_size..(i + 1) * cluster_size],
                )
                .await?;
        }
        Ok(buf)
    }

    /// Write a directory's bytes back where they came from.
    async fn write_dir_bytes(&self, dir: DirRef, bytes: &[u8]) -> Result<()> {
        match dir {
            None if self.fs.fat_type() != FatType::Fat32 => {
                self.fs
                    .device()
                    .write_at(self.fs.boot().root_dir_offset(), bytes)
                    .await?;
            }
            other => {
                let start = other.unwrap_or(self.fs.boot().root_cluster);
                let chain = self.alloc.chain(start)?;
                let cluster_size = self.fs.cluster_size() as usize;
                for (i, &cluster) in chain.iter().enumerate() {
                    let from = i * cluster_size;
                    if from >= bytes.len() {
                        break;
                    }
                    let to = (from + cluster_size).min(bytes.len());
                    self.fs
                        .device()
                        .write_at(self.fs.cluster_offset(cluster), &bytes[from..to])
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Find a run of free slots, growing the directory if there is none.
    async fn free_run(&mut self, dir: DirRef, bytes: &mut Vec<u8>, needed: usize) -> Result<usize> {
        if let Some(at) = dir::find_free_run(bytes, needed) {
            return Ok(at);
        }
        self.grow_dir(dir, bytes).await?;
        dir::find_free_run(bytes, needed)
            .ok_or_else(|| Error::Unsupported(format!(
                "a name needing {needed} slots does not fit in one cluster; \
                 raise the cluster size"
            )))
    }

    /// Add one cluster to a directory.
    async fn grow_dir(&mut self, dir: DirRef, bytes: &mut Vec<u8>) -> Result<()> {
        if dir.is_none() && self.fs.fat_type() != FatType::Fat32 {
            return Err(Error::RootDirectoryFull {
                entries: self.fs.boot().root_entries as u32,
            });
        }
        let start = dir.unwrap_or(self.fs.boot().root_cluster);
        let chain = self.alloc.chain(start)?;
        let last = *chain.last().expect("a directory has at least one cluster");
        let added = self.alloc.extend(last, 1)?;

        // A fresh directory cluster must be zeroed: a slot holding whatever the
        // cluster held before would be read as an entry.
        let cluster_size = self.fs.cluster_size() as usize;
        let zeroes = vec![0u8; cluster_size];
        self.fs
            .device()
            .write_at(self.fs.cluster_offset(added[0]), &zeroes)
            .await?;
        bytes.extend_from_slice(&zeroes);
        Ok(())
    }

    /// Grow or shrink an existing chain to `needed` clusters.
    fn fit_chain(&mut self, have: Vec<u32>, needed: u32) -> Result<Vec<u32>> {
        let current = have.len() as u32;
        if needed == 0 {
            if let Some(&first) = have.first() {
                self.alloc.free_chain(first)?;
            }
            return Ok(Vec::new());
        }
        if current == 0 {
            return self.alloc.allocate(needed);
        }
        match needed.cmp(&current) {
            std::cmp::Ordering::Equal => Ok(have),
            std::cmp::Ordering::Less => {
                self.alloc.truncate(have[0], needed)?;
                Ok(have[..needed as usize].to_vec())
            }
            std::cmp::Ordering::Greater => {
                let mut chain = have;
                let added = self.alloc.extend(*chain.last().unwrap(), needed - current)?;
                chain.extend(added);
                Ok(chain)
            }
        }
    }

    /// Write file data across a chain, zero-filling the tail of the last
    /// cluster.
    ///
    /// The tail is zeroed rather than left alone so that a rewritten file does
    /// not carry the end of the previous one in the slack space a reader can
    /// still get at.
    async fn write_data(&self, chain: &[u32], data: &[u8]) -> Result<()> {
        let cluster_size = self.fs.cluster_size() as usize;
        for (i, &cluster) in chain.iter().enumerate() {
            let from = i * cluster_size;
            let to = (from + cluster_size).min(data.len());
            let offset = self.fs.cluster_offset(cluster);
            if to - from == cluster_size {
                self.fs.device().write_at(offset, &data[from..to]).await?;
            } else {
                let mut buf = vec![0u8; cluster_size];
                buf[..to - from].copy_from_slice(&data[from..to]);
                self.fs.device().write_at(offset, &buf).await?;
            }
        }
        Ok(())
    }

    /// Change one directory entry in place.
    async fn update_entry(
        &mut self,
        path: &str,
        change: impl FnOnce(&mut DirEntry),
    ) -> Result<()> {
        let (parent, name) = self.split(path).await?;
        let mut bytes = self.read_dir_bytes(parent).await?;
        let found = dir::parse(&bytes)
            .into_iter()
            .find(|e| name::names_equal(&e.name, &name))
            .ok_or_else(|| Error::NotFound(path.to_string()))?;
        let mut entry = found.entry.clone();
        change(&mut entry);
        dir::write_short(&mut bytes, found.short_slot, &entry);
        self.write_dir_bytes(parent, &bytes).await?;
        Ok(())
    }

    /// Resolve a path to the entry it names, or `None` when the last component
    /// does not exist.
    async fn find(&self, path: &str) -> Result<Option<Found>> {
        let parts: Vec<&str> = components(path).collect();
        if parts.is_empty() {
            return Ok(None);
        }
        let mut dir: DirRef = None;
        for (i, part) in parts.iter().enumerate() {
            let bytes = self.read_dir_bytes(dir).await?;
            let entry = dir::parse(&bytes)
                .into_iter()
                .find(|e| name::names_equal(&e.name, part));
            let entry = match entry {
                Some(e) => e,
                None if i == parts.len() - 1 => return Ok(None),
                None => {
                    return Err(Error::NotFound(
                        parts[..=i].iter().fold(String::new(), |a, p| a + "/" + p),
                    ))
                }
            };
            if i == parts.len() - 1 {
                return Ok(Some(Found { entry }));
            }
            if !entry.is_dir() {
                return Err(Error::NotADirectory(
                    parts[..=i].iter().fold(String::new(), |a, p| a + "/" + p),
                ));
            }
            dir = Some(entry.first_cluster());
        }
        Ok(None)
    }

    /// The directory a path names.
    async fn dir_ref(&self, path: &str) -> Result<DirRef> {
        if is_root(path) {
            return Ok(None);
        }
        let found = self
            .find(path)
            .await?
            .ok_or_else(|| Error::NotFound(path.to_string()))?;
        if !found.entry.is_dir() {
            return Err(Error::NotADirectory(path.to_string()));
        }
        Ok(Some(found.entry.first_cluster()))
    }

    /// Split a path into the directory holding it and the final name.
    async fn split(&self, path: &str) -> Result<(DirRef, String)> {
        let parts: Vec<&str> = components(path).collect();
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| Error::InvalidPath(path.to_string()))?;
        let mut dir: DirRef = None;
        let mut walked = String::new();
        for part in parents {
            walked.push('/');
            walked.push_str(part);
            let bytes = self.read_dir_bytes(dir).await?;
            let entry = dir::parse(&bytes)
                .into_iter()
                .find(|e| name::names_equal(&e.name, part))
                .ok_or_else(|| Error::NotFound(walked.clone()))?;
            if !entry.is_dir() {
                return Err(Error::NotADirectory(walked.clone()));
            }
            dir = Some(entry.first_cluster());
        }
        Ok((dir, name.to_string()))
    }
}

/// An entry that was found, and where.
struct Found {
    entry: dir::Entry,
}

/// Turn a parsed directory entry into the public one.
fn to_entry(found: &dir::Entry) -> Entry {
    let entry = &found.entry;
    Entry {
        name: found.name.clone(),
        short_name: entry.short_name(),
        is_dir: entry.attr.contains(Attributes::DIRECTORY),
        size: entry.size as u64,
        attributes: entry.attr,
        modified: DosTime {
            date: entry.date,
            time: entry.time,
            centiseconds: 0,
        }
        .to_unix(),
        created: DosTime {
            date: entry.cdate,
            time: entry.ctime,
            centiseconds: entry.ctime_cs,
        }
        .to_unix(),
        first_cluster: entry.first_cluster(),
    }
}

/// The components of a path, with empty ones dropped.
fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|p| !p.is_empty() && *p != ".")
}

/// Is this the root directory?
fn is_root(path: &str) -> bool {
    components(path).next().is_none()
}

/// Join a directory path and a name.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

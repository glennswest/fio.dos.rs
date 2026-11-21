//! The public API: read, write, mkdir, unlink, rename, stat, list.
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
//!
//! # When a write fails
//!
//! Clusters are allocated before the directory entry naming them is written,
//! and freed only after the entry that named them is gone, so the cached FAT
//! never holds a chain the directory does not name, nor frees one it still
//! does. When a directory write fails, the directory is read back: if it is as
//! it was, the allocation is taken back and the error returned; if the new
//! entry is there, the change is finished and the error still returned. If it
//! is neither, or cannot be read, the volume is *poisoned*: every change and
//! every flush after that returns [`Error::Poisoned`], because writing the FAT
//! out over a directory in an unknown state could join two files' clusters.
//!
//! A failed write of a file's *data* is undone the same way, with one limit:
//! rewriting an existing file writes over its clusters in place, so a failure
//! part way leaves the file's old size and chain with some of the new bytes in
//! them. The filesystem stays consistent; that file's contents do not.
//!
//! # Paths
//!
//! `/` separates components, empty ones and `.` are skipped, and names are
//! matched without regard to ASCII case, by long name or by 8.3 alias. `..`
//! goes up a directory when reading. A path that is *changed* — written,
//! created, removed, renamed, its attributes set — may not contain `..`: the
//! `.` and `..` entries are a directory's bookkeeping, never a target.

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
    /// Why the volume refuses changes, once a failed directory write left it
    /// in a state that could not be established.
    poisoned: Option<String>,
}

/// Where a directory's contents live: the fixed root, or a cluster chain.
type DirRef = Option<u32>;


/// What a path resolves to.
enum Target {
    /// The root directory, which has no entry of its own.
    Root,
    /// A directory entry. For a path ending in `..`, the `..` entry itself,
    /// whose cluster names the directory it points at.
    Entry(dir::Entry),
}

/// A directory write already made by an operation, kept so it can be undone:
/// the directory, its bytes before, its bytes after.
type Done = (DirRef, Vec<u8>, Vec<u8>);

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
            poisoned: None,
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

    /// Is the volume refusing changes? See [`Error::Poisoned`].
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.is_some()
    }

    /// Write out everything still held in memory.
    ///
    /// The FAT and the FSInfo sector. Until this returns, the volume on the
    /// device does not name the clusters the files were written into.
    pub async fn flush(&mut self) -> Result<()> {
        self.usable()?;
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
        self.usable()?;
        self.alloc.begin();
        let result = self.set_label_inner(label).await;
        self.settle(result)
    }

    async fn set_label_inner(&mut self, label: &str) -> Result<()> {
        let raw = mkfs_dos::params::encode_label(label)
            .map_err(|e| Error::invalid_name(label, e.to_string()))?;

        let mut root = self.read_dir_bytes(None).await?;
        let now = self.now();
        let slot = match dir::find_label(&root) {
            Some((slot, mut entry)) => {
                entry.name = raw;
                entry.time = now.time;
                entry.date = now.date;
                Some((slot, entry))
            }
            None => None,
        };
        let (slot, entry) = match slot {
            Some(found) => found,
            None => {
                let slot = self.free_run(None, &mut root, 1).await?;
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
                (slot, entry)
            }
        };
        let before = root.clone();
        dir::write_short(&mut root, slot, &entry);
        let late = self.store_dir(None, &before, &root).await?;

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
        late.map_or(Ok(()), Err)
    }

    /// Does this path exist?
    pub async fn exists(&self, path: &str) -> Result<bool> {
        Ok(self.resolve(path).await?.is_some())
    }

    /// Metadata for a path.
    pub async fn stat(&self, path: &str) -> Result<Stat> {
        match self.resolve(path).await? {
            None => Err(Error::NotFound(path.to_string())),
            Some(Target::Root) => Ok(self.root_entry()),
            Some(Target::Entry(found)) => Ok(to_entry(&found)),
        }
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
            .filter(|e| !is_dot(e))
            .map(to_entry)
            .collect())
    }

    /// Read a whole file.
    ///
    /// A file whose recorded size its chain cannot hold — a nonempty file with
    /// no first cluster, or a chain that ends early — is reported as corrupt
    /// rather than returned short: a short read that succeeds is data lost
    /// without anyone being told.
    pub async fn read(&self, path: &str) -> Result<Vec<u8>> {
        let found = match self.resolve(path).await? {
            None => return Err(Error::NotFound(path.to_string())),
            Some(Target::Root) => return Err(Error::IsADirectory(path.to_string())),
            Some(Target::Entry(found)) => found,
        };
        if found.is_dir() {
            return Err(Error::IsADirectory(path.to_string()));
        }

        let size = found.entry.size as usize;
        if size == 0 {
            return Ok(Vec::new());
        }
        let first = found.first_cluster();
        if first == 0 {
            return Err(corrupt(
                "file",
                format!("{path} records {size} bytes and has no cluster"),
            ));
        }

        let chain = self.alloc.chain(first)?;
        let cluster_size = self.fs.cluster_size() as usize;
        let needed = size.div_ceil(cluster_size);
        if chain.len() < needed {
            return Err(corrupt(
                "file",
                format!(
                    "{path} records {size} bytes and its chain holds {} clusters ({} bytes)",
                    chain.len(),
                    chain.len() * cluster_size
                ),
            ));
        }
        // A chain longer than the size needs is slack, not data: only the
        // clusters the size covers are read.
        let mut out = vec![0u8; needed * cluster_size];
        for (i, &cluster) in chain[..needed].iter().enumerate() {
            self.fs
                .device()
                .read_at(
                    self.fs.cluster_offset(cluster),
                    &mut out[i * cluster_size..(i + 1) * cluster_size],
                )
                .await?;
        }
        // The last cluster is whole; the size says where the file ends in it.
        out.truncate(size);
        Ok(out)
    }

    /// Write a file, replacing it if it exists.
    pub async fn write(&mut self, path: &str, data: &[u8]) -> Result<()> {
        self.write_with(path, data, &Attrs::default()).await
    }

    /// Write a file with attributes.
    ///
    /// Only read-only, hidden, system and archive can be given; archive is
    /// always set. The directory and volume-label bits are refused with
    /// [`Error::InvalidAttributes`] — [`Volume::mkdir`] and
    /// [`Volume::set_label`] make those entries.
    pub async fn write_with(&mut self, path: &str, data: &[u8], attrs: &Attrs) -> Result<()> {
        self.usable()?;
        self.alloc.begin();
        let result = self.write_with_inner(path, data, attrs).await;
        self.settle(result)
    }

    async fn write_with_inner(&mut self, path: &str, data: &[u8], attrs: &Attrs) -> Result<()> {
        if data.len() as u64 > u32::MAX as u64 {
            return Err(Error::FileTooLarge {
                path: path.to_string(),
                size: data.len() as u64,
            });
        }
        let attributes = file_attributes(path, attrs.attributes)?;
        let (parent, name) = self.split(path).await?;
        let mut bytes = self.read_dir_bytes(parent).await?;
        let entries = dir::parse(&bytes);
        let existing = lookup(&entries, &name).cloned();
        if existing.as_ref().is_some_and(|e| e.is_dir()) {
            return Err(Error::IsADirectory(path.to_string()));
        }

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
        let before = bytes.clone();

        // Allocate now; free only once the directory entry no longer names
        // what is freed. A shrink keeps the whole old chain until then, so a
        // directory write that never lands leaves the old size over the old
        // chain.
        let mut shrink: Option<(u32, u32)> = None;
        let chain = match &existing {
            Some(found) => {
                // Reuse what the file already has, so rewriting a file in place
                // does not move it and does not need the space twice.
                let first = found.first_cluster();
                let have = if first == 0 {
                    Vec::new()
                } else {
                    self.alloc.chain(first)?
                };
                let current = have.len() as u32;
                if needed > current {
                    let added = match have.last() {
                        Some(&last) => self.alloc.extend(last, needed - current)?,
                        None => self.alloc.allocate(needed)?,
                    };
                    [have, added].concat()
                } else {
                    if needed < current {
                        shrink = Some((first, needed));
                    }
                    have[..needed as usize].to_vec()
                }
            }
            None => self.alloc.allocate(needed)?,
        };

        self.write_data(&chain, data).await?;

        let now = attrs.modified.map(DosTime::from_unix).unwrap_or(self.now());
        let mut entry = match &existing {
            Some(found) => found.entry.clone(),
            None => DirEntry {
                ctime: now.time,
                cdate: now.date,
                ..Default::default()
            },
        };
        entry.attr = attributes | Attributes::ARCHIVE;
        entry.size = data.len() as u32;
        entry.set_first_cluster(chain.first().copied().unwrap_or(0));
        entry.time = now.time;
        entry.date = now.date;
        entry.adate = now.date;

        match (&existing, placement) {
            (Some(found), _) => dir::write_short(&mut bytes, found.short_slot, &entry),
            (None, Some((fragments, short, at))) => {
                entry.name = short.raw;
                entry.lcase = short.lcase;
                dir::write_set(&mut bytes, at, &fragments, &entry);
            }
            (None, None) => unreachable!("a new file always reserves its slots"),
        }
        let late = self.store_dir(parent, &before, &bytes).await?;

        // The entry now records the new size, so the tail can go.
        if let Some((first, keep)) = shrink {
            self.alloc.truncate(first, keep)?;
        }
        late.map_or(Ok(()), Err)
    }

    /// Append to a file, creating it if it does not exist.
    pub async fn append(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let mut existing = match self.resolve(path).await? {
            Some(Target::Root) => return Err(Error::IsADirectory(path.to_string())),
            Some(Target::Entry(found)) if found.is_dir() => {
                return Err(Error::IsADirectory(path.to_string()))
            }
            Some(Target::Entry(_)) => self.read(path).await?,
            None => Vec::new(),
        };
        existing.extend_from_slice(data);
        self.write(path, &existing).await
    }

    /// Create a directory. Its parent must exist.
    pub async fn mkdir(&mut self, path: &str) -> Result<()> {
        self.usable()?;
        self.alloc.begin();
        let result = self.mkdir_inner(path).await;
        self.settle(result)
    }

    async fn mkdir_inner(&mut self, path: &str) -> Result<()> {
        let (parent, name) = self.split(path).await?;
        let mut bytes = self.read_dir_bytes(parent).await?;
        let entries = dir::parse(&bytes);
        if lookup(&entries, &name).is_some() {
            return Err(Error::Exists(path.to_string()));
        }

        // The slots come first here too: a directory whose cluster is allocated
        // and whose name will not fit is a lost cluster.
        let (fragments, short) = dir::build_set(&name, &entries)?;
        let at = self
            .free_run(parent, &mut bytes, fragments.len() + 1)
            .await?;
        let before = bytes.clone();

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
        self.fs
            .device()
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
        let late = self.store_dir(parent, &before, &bytes).await?;
        late.map_or(Ok(()), Err)
    }

    /// Create a directory and every parent it needs.
    pub async fn mkdir_all(&mut self, path: &str) -> Result<()> {
        refuse_dotdot(path)?;
        let mut so_far = String::new();
        for component in components(path) {
            so_far.push('/');
            so_far.push_str(component);
            match self.resolve(&so_far).await? {
                Some(Target::Entry(found)) if !found.is_dir() => {
                    return Err(Error::NotADirectory(so_far))
                }
                Some(_) => continue,
                None => self.mkdir(&so_far).await?,
            }
        }
        Ok(())
    }

    /// Remove a file.
    pub async fn unlink(&mut self, path: &str) -> Result<()> {
        self.usable()?;
        self.alloc.begin();
        let result = self.remove_entry(path, false).await;
        self.settle(result)
    }

    /// Remove an empty directory.
    pub async fn rmdir(&mut self, path: &str) -> Result<()> {
        self.usable()?;
        self.alloc.begin();
        let result = self.remove_entry(path, true).await;
        self.settle(result)
    }

    /// Remove a file (`want_dir` false) or an empty directory (true).
    ///
    /// The entry is erased on the device first and the chain freed after. The
    /// other order frees clusters a live entry still names whenever the
    /// directory write fails, and the next allocation hands them to another
    /// file.
    async fn remove_entry(&mut self, path: &str, want_dir: bool) -> Result<()> {
        let (parent, name) = self.split(path).await?;
        let mut bytes = self.read_dir_bytes(parent).await?;
        let found = lookup(&dir::parse(&bytes), &name)
            .cloned()
            .ok_or_else(|| Error::NotFound(path.to_string()))?;
        match (want_dir, found.is_dir()) {
            (false, true) => return Err(Error::IsADirectory(path.to_string())),
            (true, false) => return Err(Error::NotADirectory(path.to_string())),
            _ => {}
        }

        if want_dir {
            let contents = self.read_dir_bytes(Some(found.first_cluster())).await?;
            if dir::parse(&contents).iter().any(|e| !is_dot(e)) {
                return Err(Error::NotEmpty(path.to_string()));
            }
        }

        let before = bytes.clone();
        dir::erase_set(&mut bytes, &found);
        let late = self.store_dir(parent, &before, &bytes).await?;
        if found.first_cluster() != 0 {
            self.alloc.free_chain(found.first_cluster())?;
        }
        late.map_or(Ok(()), Err)
    }

    /// Remove a path and everything under it.
    pub async fn remove_all(&mut self, path: &str) -> Result<()> {
        refuse_dotdot(path)?;
        let found = match self.resolve(path).await? {
            None => return Ok(()),
            Some(Target::Root) => return Err(Error::InvalidPath(path.to_string())),
            Some(Target::Entry(found)) => found,
        };
        if !found.is_dir() {
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
    ///
    /// A directory cannot be moved into itself or anything under it, however
    /// the destination is spelled: the destination's ancestry is walked
    /// through the `..` entries on disk, not compared as text.
    pub async fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.usable()?;
        self.alloc.begin();
        let result = self.rename_inner(from, to).await;
        self.settle(result)
    }

    async fn rename_inner(&mut self, from: &str, to: &str) -> Result<()> {
        let (from_parent, from_name) = self.split(from).await?;
        let (to_parent, to_name) = self.split(to).await?;

        let from_bytes = self.read_dir_bytes(from_parent).await?;
        let source = lookup(&dir::parse(&from_bytes), &from_name)
            .cloned()
            .ok_or_else(|| Error::NotFound(from.to_string()))?;

        let same_dir = from_parent == to_parent;
        let mut to_bytes = if same_dir {
            from_bytes.clone()
        } else {
            self.read_dir_bytes(to_parent).await?
        };
        if let Some(target) = lookup(&dir::parse(&to_bytes), &to_name) {
            // Renaming a name to itself, in another case, is allowed.
            if !(same_dir && target.short_slot == source.short_slot) {
                return Err(Error::Exists(to.to_string()));
            }
        }
        if source.is_dir() && !same_dir {
            self.refuse_move_into_itself(source.first_cluster(), to_parent, from, to)
                .await?;
        }
        let mut to_before = to_bytes.clone();

        // Remove the old name first when both names live in the same directory,
        // so the new set can reuse the slots the old one held.
        if same_dir {
            dir::erase_set(&mut to_bytes, &source);
        }

        let (fragments, short) = dir::build_set(&to_name, &dir::parse(&to_bytes))?;
        let mut entry = source.entry.clone();
        entry.name = short.raw;
        entry.lcase = short.lcase;

        let at = self
            .free_run(to_parent, &mut to_bytes, fragments.len() + 1)
            .await?;
        // A grown directory's new cluster was zeroed on the device already.
        to_before.resize(to_bytes.len(), 0);
        dir::write_set(&mut to_bytes, at, &fragments, &entry);
        let mut late = self.store_dir(to_parent, &to_before, &to_bytes).await?;
        if same_dir {
            return late.map_or(Ok(()), Err);
        }
        let mut done: Vec<Done> = vec![(to_parent, to_before, to_bytes)];

        let mut from_after = from_bytes.clone();
        dir::erase_set(&mut from_after, &source);
        match self.store_dir(from_parent, &from_bytes, &from_after).await {
            Ok(l) => late = late.or(l),
            Err(e) => return Err(self.revert(&done, e).await),
        }
        done.push((from_parent, from_bytes, from_after));

        // A directory that moved has a ".." that now points at the wrong
        // parent. Leaving it is how a tree ends up with a cycle in it.
        if source.is_dir() {
            let cluster = source.first_cluster();
            let contents = self.read_dir_bytes(Some(cluster)).await?;
            if let Some(dotdot) = dir::parse(&contents).into_iter().find(|e| e.name == "..") {
                let mut updated = dotdot.entry.clone();
                updated.set_first_cluster(to_parent.unwrap_or(0));
                let mut after = contents.clone();
                dir::write_short(&mut after, dotdot.short_slot, &updated);
                match self.store_dir(Some(cluster), &contents, &after).await {
                    Ok(l) => late = late.or(l),
                    Err(e) => return Err(self.revert(&done, e).await),
                }
            }
        }
        late.map_or(Ok(()), Err)
    }

    /// Set a file's attribute bits.
    ///
    /// Read-only, hidden, system and archive. The directory bit says what the
    /// entry *is*: it is kept on a directory and refused on a file, and the
    /// volume-label bit is refused on both ([`Error::InvalidAttributes`]).
    pub async fn set_attributes(&mut self, path: &str, attributes: Attributes) -> Result<()> {
        self.update_entry(path, |entry| {
            let is_dir = entry.attr.contains(Attributes::DIRECTORY);
            if attributes.contains(Attributes::DIRECTORY) && !is_dir {
                return Err(Error::InvalidAttributes {
                    path: path.to_string(),
                    bits: attributes.bits(),
                });
            }
            let wanted = file_attributes(path, attributes - Attributes::DIRECTORY)?;
            entry.attr = if is_dir {
                wanted | Attributes::DIRECTORY
            } else {
                wanted
            };
            Ok(())
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
            Ok(())
        })
        .await
    }

    // ---- internals ------------------------------------------------------

    /// Refuse a change once the volume is poisoned.
    fn usable(&self) -> Result<()> {
        match &self.poisoned {
            Some(why) => Err(Error::Poisoned(why.clone())),
            None => Ok(()),
        }
    }

    /// Poison the volume, and return the error that says so.
    fn poison(&mut self, why: String) -> Error {
        self.poisoned = Some(why.clone());
        Error::Poisoned(why)
    }

    /// End an operation begun with [`Allocator::begin`]: keep its allocations
    /// if it succeeded, take them back if it failed before its directory
    /// write landed. One that landed has already committed, and this takes
    /// nothing back.
    fn settle<T>(&mut self, result: Result<T>) -> Result<T> {
        match &result {
            Ok(_) => self.alloc.commit(),
            Err(_) => self.alloc.rollback(),
        }
        result
    }

    /// Write a directory's new bytes — the point at which an operation's
    /// allocations are committed — and establish what happened if it fails.
    ///
    /// - `Ok(None)`: written.
    /// - `Ok(Some(e))`: the device reported `e`, and the new bytes are there
    ///   all the same. The operation finishes, then returns `e`.
    /// - `Err(e)`: the device holds the old bytes; nothing changed, and the
    ///   caller's allocations are taken back.
    /// - `Err(Poisoned)`: neither, or the read-back failed too.
    async fn store_dir(
        &mut self,
        dir: DirRef,
        before: &[u8],
        after: &[u8],
    ) -> Result<Option<Error>> {
        let err = match self.write_dir_bytes(dir, after).await {
            Ok(()) => {
                self.alloc.commit();
                return Ok(None);
            }
            Err(e) => e,
        };
        match self.read_dir_bytes(dir).await {
            Ok(now) if now == after => {
                self.alloc.commit();
                Ok(Some(err))
            }
            Ok(now) if now == before => Err(err),
            Ok(_) => Err(self.poison(format!(
                "a directory write failed ({err}) and left the directory neither as it was \
                 nor as intended"
            ))),
            Err(read) => Err(self.poison(format!(
                "a directory write failed ({err}) and reading it back failed too ({read})"
            ))),
        }
    }

    /// Undo the directory writes an operation already made, newest first,
    /// after a later one failed with `cause`. If an undo fails as well, the
    /// volume is poisoned: two directories may now name one chain.
    async fn revert(&mut self, done: &[Done], cause: Error) -> Error {
        if matches!(cause, Error::Poisoned(_)) {
            return cause;
        }
        for (dir, before, after) in done.iter().rev() {
            if let Err(e) = self.store_dir(*dir, after, before).await {
                return self.poison(format!(
                    "a rename failed ({cause}) and undoing its first half failed too ({e})"
                ));
            }
        }
        cause
    }

    /// Refuse to move the directory at `source` under `to_parent` when
    /// `to_parent` is that directory or one beneath it.
    async fn refuse_move_into_itself(
        &self,
        source: u32,
        to_parent: DirRef,
        from: &str,
        to: &str,
    ) -> Result<()> {
        let mut at = to_parent;
        let mut steps = 0u32;
        while let Some(cluster) = at {
            if cluster == source {
                return Err(Error::InvalidPath(format!(
                    "{to} is inside {from}; a directory cannot be moved into itself"
                )));
            }
            steps += 1;
            if steps > self.alloc.max_cluster() {
                return Err(corrupt(
                    "directory",
                    format!("the '..' entries above {to} never reach the root"),
                ));
            }
            at = self.dir_of(self.dotdot(cluster, to).await?.first_cluster());
        }
        Ok(())
    }

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

    /// What [`Volume::stat`] says about `/`.
    fn root_entry(&self) -> Entry {
        Entry {
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
        }
    }

    /// The directory a cluster number in a `..` entry names.
    ///
    /// Zero is the root on every width — that is what a `..` in a directory
    /// just under the root holds — and so is a FAT32 root's own cluster,
    /// which some writers put there instead.
    fn dir_of(&self, cluster: u32) -> DirRef {
        if cluster == 0
            || (self.fs.fat_type() == FatType::Fat32 && cluster == self.fs.boot().root_cluster)
        {
            None
        } else {
            Some(cluster)
        }
    }

    /// The directory an ordinary directory entry names. Cluster zero there is
    /// damage, not the root.
    fn subdir(&self, entry: &dir::Entry, path: &str) -> Result<DirRef> {
        match entry.first_cluster() {
            0 => Err(corrupt(
                "directory",
                format!("{path} is a directory with no cluster"),
            )),
            cluster => Ok(self.dir_of(cluster)),
        }
    }

    /// A directory's `..` entry.
    async fn dotdot(&self, dir: u32, path: &str) -> Result<dir::Entry> {
        let bytes = self.read_dir_bytes(Some(dir)).await?;
        dir::parse(&bytes)
            .into_iter()
            .find(|e| e.name == "..")
            .ok_or_else(|| corrupt("directory", format!("{path}: a directory has no '..' entry")))
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

    /// Find a run of free slots, growing the directory until there is one.
    ///
    /// A run may span clusters (`write_dir_bytes` writes across the chain), so
    /// a name of 21 slots in a directory of 512-byte clusters, 16 slots each,
    /// takes as many clusters as it needs (b1f840bab51e). Each growth adds at
    /// least one slot to the free tail, so this ends with a run, or with the
    /// error that the volume is full or the fixed FAT12/16 root is. The
    /// clusters added are in the caller's allocation journal, so a failure
    /// gives them all back.
    async fn free_run(&mut self, dir: DirRef, bytes: &mut Vec<u8>, needed: usize) -> Result<usize> {
        loop {
            if let Some(at) = dir::find_free_run(bytes, needed) {
                return Ok(at);
            }
            self.grow_dir(dir, bytes).await?;
        }
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
        let Some(&last) = chain.last() else {
            return Err(corrupt(
                "directory",
                format!("the directory at cluster {start} has no clusters to grow"),
            ));
        };
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
        change: impl FnOnce(&mut DirEntry) -> Result<()>,
    ) -> Result<()> {
        self.usable()?;
        let (parent, name) = self.split(path).await?;
        let mut bytes = self.read_dir_bytes(parent).await?;
        let found = lookup(&dir::parse(&bytes), &name)
            .cloned()
            .ok_or_else(|| Error::NotFound(path.to_string()))?;
        let mut entry = found.entry.clone();
        change(&mut entry)?;
        let before = bytes.clone();
        dir::write_short(&mut bytes, found.short_slot, &entry);
        let late = self.store_dir(parent, &before, &bytes).await?;
        late.map_or(Ok(()), Err)
    }

    /// Resolve a path, or `None` when its last component does not exist.
    async fn resolve(&self, path: &str) -> Result<Option<Target>> {
        let parts: Vec<&str> = components(path).collect();
        let Some((last, parents)) = parts.split_last() else {
            return Ok(Some(Target::Root));
        };
        let dir = self.walk(parents).await?;
        if *last == ".." {
            let Some(cluster) = dir else {
                // The root's parent is the root.
                return Ok(Some(Target::Root));
            };
            let up = self.dotdot(cluster, path).await?;
            return Ok(Some(match self.dir_of(up.first_cluster()) {
                None => Target::Root,
                Some(_) => Target::Entry(up),
            }));
        }
        let bytes = self.read_dir_bytes(dir).await?;
        Ok(lookup(&dir::parse(&bytes), last)
            .cloned()
            .map(Target::Entry))
    }

    /// The directory a path names.
    async fn dir_ref(&self, path: &str) -> Result<DirRef> {
        match self.resolve(path).await? {
            None => Err(Error::NotFound(path.to_string())),
            Some(Target::Root) => Ok(None),
            Some(Target::Entry(found)) if !found.is_dir() => {
                Err(Error::NotADirectory(path.to_string()))
            }
            Some(Target::Entry(found)) if is_dot(&found) => Ok(self.dir_of(found.first_cluster())),
            Some(Target::Entry(found)) => self.subdir(&found, path),
        }
    }

    /// Follow directory components from the root. `..` goes up through the
    /// directory's own `..` entry, and stays put at the root.
    async fn walk(&self, parts: &[&str]) -> Result<DirRef> {
        let mut dir: DirRef = None;
        let mut walked = String::new();
        for part in parts {
            walked.push('/');
            walked.push_str(part);
            if *part == ".." {
                if let Some(cluster) = dir {
                    dir = self.dir_of(self.dotdot(cluster, &walked).await?.first_cluster());
                }
                continue;
            }
            let bytes = self.read_dir_bytes(dir).await?;
            let entry = lookup(&dir::parse(&bytes), part)
                .cloned()
                .ok_or_else(|| Error::NotFound(walked.clone()))?;
            if !entry.is_dir() {
                return Err(Error::NotADirectory(walked.clone()));
            }
            dir = self.subdir(&entry, &walked)?;
        }
        Ok(dir)
    }

    /// Split a path that is about to be changed into the directory holding it
    /// and the final name. `..` is refused anywhere in it, and so is a path
    /// with no name at all.
    async fn split(&self, path: &str) -> Result<(DirRef, String)> {
        refuse_dotdot(path)?;
        let parts: Vec<&str> = components(path).collect();
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| Error::InvalidPath(path.to_string()))?;
        Ok((self.walk(parents).await?, name.to_string()))
    }
}

/// The entry in a directory a name refers to.
///
/// A long name first, then an 8.3 alias — `LONGFI~1.TXT` is as much a name of
/// `Long File.txt` as the long one is, to every FAT driver. The `.` and `..`
/// entries are never returned: they are bookkeeping, and a name that resolves
/// to one would let a change land on it.
fn lookup<'a>(entries: &'a [dir::Entry], name: &str) -> Option<&'a dir::Entry> {
    let live = || entries.iter().filter(|e| !is_dot(e));
    live()
        .find(|e| name::names_equal(&e.name, name))
        .or_else(|| live().find(|e| name::names_equal(&e.entry.short_name(), name)))
}

/// Is this a directory's `.` or `..` entry? No real short name starts with a
/// dot — the 8.3 field cannot hold one there.
fn is_dot(entry: &dir::Entry) -> bool {
    entry.entry.name[0] == b'.'
}

/// The attribute bits a caller may give a file, or why not.
fn file_attributes(path: &str, attributes: Attributes) -> Result<Attributes> {
    let allowed =
        Attributes::READ_ONLY | Attributes::HIDDEN | Attributes::SYSTEM | Attributes::ARCHIVE;
    if allowed.contains(attributes) {
        Ok(attributes)
    } else {
        Err(Error::InvalidAttributes {
            path: path.to_string(),
            bits: attributes.bits(),
        })
    }
}

/// A corruption error from this crate's own checks.
fn corrupt(structure: &'static str, detail: String) -> Error {
    mkfs_dos::Error::Corrupt { structure, detail }.into()
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

/// The components of a path, with empty ones and `.` dropped.
fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|p| !p.is_empty() && *p != ".")
}

/// Refuse `..` in a path that is about to be changed.
fn refuse_dotdot(path: &str) -> Result<()> {
    if components(path).any(|p| p == "..") {
        return Err(Error::InvalidPath(path.to_string()));
    }
    Ok(())
}

/// Join a directory path and a name.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

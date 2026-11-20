//! Cluster allocation, and the two counters it has to keep in step.
//!
//! Every allocation and every free touches the FAT; on FAT32 it also touches
//! the FSInfo sector's free count and its "start looking here" hint. A writer
//! that updates one and not the other leaves a filesystem `fsck.fat` reports.
//!
//! The FAT is held in memory while a volume is open. It is the only structure
//! an allocator has to search, and reading it an entry at a time would mean one
//! device round trip per cluster of every file — for a 100 MB file, tens of
//! thousands of them. Writes go into the cache and mark the sector dirty;
//! [`Allocator::flush`] pushes the dirty sectors out to *every* FAT.
//!
//! The size of that cache is the FAT's own: 4 bytes per cluster on FAT32, so a
//! megabyte for a 1 GiB volume and 128 MB for a 1 TB one. That is the trade,
//! and it is stated here rather than discovered.

use std::collections::BTreeSet;

use mkfs_dos::device::BlockDevice;
use mkfs_dos::fat;
use mkfs_dos::fs::Filesystem;
use mkfs_dos::params::FatType;
use mkfs_dos::structs::FsInfo;

use crate::error::{Error, Result};

/// The FAT, cached, plus the free-cluster bookkeeping.
pub struct Allocator {
    fat: Vec<u8>,
    fat_type: FatType,
    max_cluster: u32,
    sector_size: u32,
    /// Where the next search starts — FSInfo's `next_cluster`, kept live.
    next_free: u32,
    free_count: u32,
    dirty_sectors: BTreeSet<u32>,
    fsinfo_dirty: bool,
    /// The undo record of the change in progress, between [`Allocator::begin`]
    /// and [`Allocator::commit`] or [`Allocator::rollback`].
    journal: Option<Journal>,
}

/// What [`Allocator::rollback`] needs to put the cache back as it was.
struct Journal {
    /// Every entry changed, with the value it had before — oldest first.
    entries: Vec<(u32, u32)>,
    free_count: u32,
    next_free: u32,
    fsinfo_dirty: bool,
}

impl Allocator {
    /// Read the FAT and count what is free.
    ///
    /// The count is computed rather than taken from the FSInfo sector. That
    /// field is a hint a previous writer may have left stale, and every
    /// decision here depends on it being right.
    pub async fn load<D: BlockDevice>(fs: &Filesystem<D>) -> Result<Self> {
        let fat = fs.read_fat(0).await?;
        let fat_type = fs.fat_type();
        let max_cluster = fs.max_cluster();

        let mut free_count = 0;
        for cluster in 2..=max_cluster {
            if fat::get_entry(&fat, fat_type, cluster) == 0 {
                free_count += 1;
            }
        }

        // Start the search where the last writer left off when that is
        // plausible, so a volume filled by successive runs does not rescan the
        // clusters it already filled.
        let next_free = match fs.read_fsinfo().await? {
            Some(info) if (2..=max_cluster).contains(&info.next_cluster) => info.next_cluster,
            _ => 2,
        };

        Ok(Self {
            fat,
            fat_type,
            max_cluster,
            sector_size: fs.boot().bytes_per_sector as u32,
            next_free,
            free_count,
            dirty_sectors: BTreeSet::new(),
            fsinfo_dirty: false,
            journal: None,
        })
    }

    /// Clusters not in use.
    pub fn free_count(&self) -> u32 {
        self.free_count
    }

    /// The highest cluster number the volume has.
    pub fn max_cluster(&self) -> u32 {
        self.max_cluster
    }

    /// Read an entry from the cache.
    pub fn entry(&self, cluster: u32) -> u32 {
        fat::get_entry(&self.fat, self.fat_type, cluster)
    }

    /// Write an entry into the cache, marking the sectors it touches.
    ///
    /// Two sectors, not one, when a FAT12 entry straddles the boundary — the
    /// twelve-bit entry's second byte can be the first byte of the next sector,
    /// and flushing only the first leaves half an entry on disk.
    pub fn set_entry(&mut self, cluster: u32, value: u32) {
        if let Some(journal) = self.journal.as_mut() {
            journal
                .entries
                .push((cluster, fat::get_entry(&self.fat, self.fat_type, cluster)));
        }
        let offset = fat::entry_offset(self.fat_type, cluster);
        let span = fat::entry_span(self.fat_type);
        fat::set_entry(&mut self.fat, self.fat_type, cluster, value);
        let first = offset / self.sector_size as u64;
        let last = (offset + span - 1) / self.sector_size as u64;
        for sector in first..=last {
            self.dirty_sectors.insert(sector as u32);
        }
    }

    /// Start recording, so the changes from here on can be taken back.
    ///
    /// A volume operation allocates before it knows whether the directory
    /// entry naming the clusters will reach the device. If it does not, the
    /// allocation has to be undone, or a later flush writes a chain nothing
    /// names. A `begin` while a change is already open keeps the older record:
    /// the change it belongs to has not ended.
    pub fn begin(&mut self) {
        if self.journal.is_none() {
            self.journal = Some(Journal {
                entries: Vec::new(),
                free_count: self.free_count,
                next_free: self.next_free,
                fsinfo_dirty: self.fsinfo_dirty,
            });
        }
    }

    /// Keep everything since [`Allocator::begin`].
    pub fn commit(&mut self) {
        self.journal = None;
    }

    /// Put every entry and counter back as it was at [`Allocator::begin`].
    ///
    /// The restored sectors stay marked dirty: writing back what the device
    /// already holds costs a write and changes nothing. Without an open
    /// change, this does nothing.
    pub fn rollback(&mut self) {
        let Some(journal) = self.journal.take() else {
            return;
        };
        for &(cluster, old) in journal.entries.iter().rev() {
            self.set_entry(cluster, old);
        }
        self.free_count = journal.free_count;
        self.next_free = journal.next_free;
        self.fsinfo_dirty = journal.fsinfo_dirty;
    }

    /// Is this a cluster the volume has?
    pub fn is_data_cluster(&self, cluster: u32) -> bool {
        (2..=self.max_cluster).contains(&cluster)
    }

    /// Follow a chain from the cache, without touching the device.
    ///
    /// A chain that leaves the volume or does not end is reported rather than
    /// followed; both mean the filesystem is damaged, and following either one
    /// reads for ever or reads someone else's data.
    pub fn chain(&self, start: u32) -> Result<Vec<u32>> {
        let mut chain = Vec::new();
        let mut cluster = start;
        if start == 0 {
            return Ok(chain);
        }
        loop {
            if !self.is_data_cluster(cluster) {
                return Err(mkfs_dos::Error::Corrupt {
                    structure: "cluster chain",
                    detail: format!(
                        "cluster {cluster} is outside the 2..={} this volume has",
                        self.max_cluster
                    ),
                }
                .into());
            }
            chain.push(cluster);
            if chain.len() > self.max_cluster as usize {
                return Err(mkfs_dos::Error::Corrupt {
                    structure: "cluster chain",
                    detail: format!("the chain from {start} does not end"),
                }
                .into());
            }
            let next = self.entry(cluster);
            if self.fat_type.is_end_of_chain(next) {
                return Ok(chain);
            }
            cluster = next;
        }
    }

    /// Allocate `count` clusters as one chain, and return them in order.
    ///
    /// The search wraps: it starts where the last allocation ended and comes
    /// round to cluster 2, so a volume with a hole early on still fills it.
    /// Nothing is written until every cluster has been found — a half-allocated
    /// chain on a full volume would be a leak.
    pub fn allocate(&mut self, count: u32) -> Result<Vec<u32>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if count > self.free_count {
            return Err(Error::NoSpace {
                needed: count,
                free: self.free_count,
            });
        }

        let mut found = Vec::with_capacity(count as usize);
        let span = self.max_cluster - 1;
        let mut cluster = self.next_free.max(2);
        for _ in 0..span {
            // The walk starts at one cluster and wraps once, visiting each of
            // the `span` clusters exactly once, so no cluster can be found
            // twice and nothing needs checking against what was found.
            if self.entry(cluster) == 0 {
                found.push(cluster);
                if found.len() == count as usize {
                    break;
                }
            }
            cluster += 1;
            if cluster > self.max_cluster {
                cluster = 2;
            }
        }
        if found.len() < count as usize {
            // The free count said there was room, so this means the count and
            // the FAT disagree — which is corruption, not a full volume.
            return Err(Error::NoSpace {
                needed: count,
                free: found.len() as u32,
            });
        }

        for (i, &c) in found.iter().enumerate() {
            let value = match found.get(i + 1) {
                Some(&next) => next,
                None => self.fat_type.eof_marker(),
            };
            self.set_entry(c, value);
        }
        self.free_count -= count;
        self.next_free = found[found.len() - 1] + 1;
        if self.next_free > self.max_cluster {
            self.next_free = 2;
        }
        self.fsinfo_dirty = true;
        Ok(found)
    }

    /// Add `count` clusters to the end of an existing chain.
    pub fn extend(&mut self, last: u32, count: u32) -> Result<Vec<u32>> {
        let added = self.allocate(count)?;
        if let Some(&first) = added.first() {
            self.set_entry(last, first);
        }
        Ok(added)
    }

    /// Free a whole chain, returning how many clusters came back.
    pub fn free_chain(&mut self, start: u32) -> Result<u32> {
        let chain = self.chain(start)?;
        for &cluster in &chain {
            self.set_entry(cluster, 0);
        }
        self.free_count += chain.len() as u32;
        if let Some(&first) = chain.first() {
            // Reuse freed space before scanning past it, which keeps a
            // repeatedly rewritten file roughly where it was.
            self.next_free = first;
        }
        self.fsinfo_dirty = true;
        Ok(chain.len() as u32)
    }

    /// Keep the first `keep` clusters of a chain and free the rest.
    ///
    /// `keep` of zero frees the whole chain.
    pub fn truncate(&mut self, start: u32, keep: u32) -> Result<()> {
        if keep == 0 {
            self.free_chain(start)?;
            return Ok(());
        }
        let chain = self.chain(start)?;
        if chain.len() as u32 <= keep {
            return Ok(());
        }
        let last_kept = chain[keep as usize - 1];
        self.set_entry(last_kept, self.fat_type.eof_marker());
        for &cluster in &chain[keep as usize..] {
            self.set_entry(cluster, 0);
        }
        self.free_count += chain.len() as u32 - keep;
        self.next_free = chain[keep as usize];
        self.fsinfo_dirty = true;
        Ok(())
    }

    /// Write every dirty FAT sector out, to every FAT, and update the FSInfo.
    ///
    /// Every FAT, always. The second FAT is not a fallback a driver reaches for
    /// when the first looks wrong — it is a copy every driver assumes is
    /// current.
    ///
    /// A sector stops being dirty only once every copy of it has been written,
    /// and the FSInfo only once its write has succeeded. So a flush that fails
    /// part way, or is cancelled at one of its awaits, leaves everything it
    /// did not finish still marked, and the next flush writes it: a retry that
    /// returns `Ok` has put the whole cache on the device. Rewriting a sector
    /// a failed flush had already written is harmless.
    pub async fn flush<D: BlockDevice>(&mut self, fs: &Filesystem<D>) -> Result<()> {
        let sector_size = self.sector_size as usize;
        let sectors: Vec<u32> = self.dirty_sectors.iter().copied().collect();
        for sector in sectors {
            let start = sector as usize * sector_size;
            let end = (start + sector_size).min(self.fat.len());
            let data = &self.fat[start..end];
            for index in 0..fs.boot().num_fats {
                let offset = fs.fat_offset(index) + start as u64;
                fs.device().write_at(offset, data).await?;
            }
            self.dirty_sectors.remove(&sector);
        }

        if self.fsinfo_dirty {
            if fs.read_fsinfo().await?.is_some() {
                fs.write_fsinfo(&FsInfo {
                    free_clusters: self.free_count,
                    next_cluster: self.next_free,
                })
                .await?;
            }
            self.fsinfo_dirty = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mkfs_dos::device::MemDevice;
    use mkfs_dos::{format, Params};

    const MIB: u64 = 1024 * 1024;

    async fn volume(size: u64) -> (Filesystem<MemDevice>, Allocator) {
        let dev = MemDevice::new(size);
        format(&dev, &Params::new()).await.unwrap();
        let fs = Filesystem::open(dev).await.unwrap();
        let alloc = Allocator::load(&fs).await.unwrap();
        (fs, alloc)
    }

    #[tokio::test]
    async fn a_fresh_volume_has_every_cluster_free() {
        let (fs, alloc) = volume(64 * MIB).await;
        assert_eq!(alloc.free_count(), fs.cluster_count());
    }

    #[tokio::test]
    async fn an_allocated_chain_is_a_chain() {
        let (_fs, mut alloc) = volume(64 * MIB).await;
        let chain = alloc.allocate(5).unwrap();
        assert_eq!(chain.len(), 5);
        assert_eq!(alloc.chain(chain[0]).unwrap(), chain);
        assert!(alloc.fat_type.is_end_of_chain(alloc.entry(chain[4])));
    }

    #[tokio::test]
    async fn freeing_gives_the_clusters_back() {
        let (fs, mut alloc) = volume(64 * MIB).await;
        let before = alloc.free_count();
        let chain = alloc.allocate(9).unwrap();
        assert_eq!(alloc.free_count(), before - 9);
        assert_eq!(alloc.free_chain(chain[0]).unwrap(), 9);
        assert_eq!(alloc.free_count(), before);
        assert_eq!(fs.cluster_count(), before);
    }

    #[tokio::test]
    async fn truncation_keeps_the_head_and_ends_it() {
        let (_fs, mut alloc) = volume(64 * MIB).await;
        let chain = alloc.allocate(10).unwrap();
        alloc.truncate(chain[0], 4).unwrap();
        let left = alloc.chain(chain[0]).unwrap();
        assert_eq!(left, chain[..4]);
        for &freed in &chain[4..] {
            assert_eq!(alloc.entry(freed), 0);
        }
    }

    #[tokio::test]
    async fn a_flush_reaches_every_fat_and_the_fsinfo() {
        let (fs, mut alloc) = volume(1024 * MIB).await;
        let chain = alloc.allocate(3).unwrap();
        alloc.flush(&fs).await.unwrap();

        assert_eq!(fs.read_fat(0).await.unwrap(), fs.read_fat(1).await.unwrap());
        assert_eq!(fs.fat_entry(chain[0]).await.unwrap(), chain[1]);
        let info = fs.read_fsinfo().await.unwrap().unwrap();
        assert_eq!(info.free_clusters, alloc.free_count());
    }

    /// FAT12 packs two entries into three bytes, so the entry at a sector's
    /// edge has a byte on each side of it. Flushing only the sector the entry
    /// "belongs to" would write half of it.
    #[tokio::test]
    async fn a_fat12_entry_across_a_sector_boundary_is_flushed_whole() {
        let dev = MemDevice::new(8 * MIB);
        format(&dev, &Params::new()).await.unwrap();
        let fs = Filesystem::open(dev).await.unwrap();
        assert_eq!(fs.fat_type(), FatType::Fat12);
        let mut alloc = Allocator::load(&fs).await.unwrap();

        // 512 bytes hold 341 and a third entries, so entry 341 straddles.
        let straddling = fs.boot().bytes_per_sector as u32 * 8 / 12;
        alloc.set_entry(straddling, 0x777);
        alloc.flush(&fs).await.unwrap();
        assert_eq!(fs.fat_entry(straddling).await.unwrap(), 0x777);
    }

    /// Finding 0d6b59b69c3d: an allocation whose directory entry never
    /// reached the device has to be taken back whole.
    #[tokio::test]
    async fn a_rollback_takes_back_every_change_since_begin() {
        let (_fs, mut alloc) = volume(64 * MIB).await;
        let kept = alloc.allocate(3).unwrap();
        let free = alloc.free_count();
        let before: Vec<u32> = (2..=alloc.max_cluster()).map(|c| alloc.entry(c)).collect();

        alloc.begin();
        alloc.extend(kept[2], 4).unwrap();
        alloc.allocate(5).unwrap();
        alloc.truncate(kept[0], 1).unwrap();
        alloc.rollback();

        let after: Vec<u32> = (2..=alloc.max_cluster()).map(|c| alloc.entry(c)).collect();
        assert_eq!(before, after);
        assert_eq!(alloc.free_count(), free);
        assert_eq!(alloc.chain(kept[0]).unwrap(), kept);
    }

    #[tokio::test]
    async fn a_commit_keeps_the_changes() {
        let (_fs, mut alloc) = volume(64 * MIB).await;
        alloc.begin();
        let chain = alloc.allocate(3).unwrap();
        alloc.commit();
        alloc.rollback();
        assert_eq!(alloc.chain(chain[0]).unwrap(), chain);
    }

    /// A device that fails, or never finishes, writes touching one range.
    struct Faulty {
        inner: MemDevice,
        fault: std::sync::Mutex<Option<(u64, u64, bool)>>,
    }

    impl Faulty {
        /// Fail writes to `[start, start + len)`, or hang on them if `hang`.
        fn arm(&self, start: u64, len: u64, hang: bool) {
            *self.fault.lock().unwrap() = Some((start, start + len, hang));
        }

        fn disarm(&self) {
            *self.fault.lock().unwrap() = None;
        }
    }

    #[async_trait::async_trait]
    impl BlockDevice for Faulty {
        fn size(&self) -> u64 {
            self.inner.size()
        }

        async fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::result::Result<(), mkfs_dos::Error> {
            self.inner.read_at(offset, buf).await
        }

        async fn write_at(&self, offset: u64, buf: &[u8]) -> std::result::Result<(), mkfs_dos::Error> {
            let fault = *self.fault.lock().unwrap();
            if let Some((start, end, hang)) = fault {
                if offset < end && offset + buf.len() as u64 > start {
                    if hang {
                        std::future::pending::<()>().await;
                    }
                    return Err(mkfs_dos::Error::Io {
                        offset,
                        source: std::io::Error::other("injected fault"),
                    });
                }
            }
            self.inner.write_at(offset, buf).await
        }

        async fn flush(&self) -> std::result::Result<(), mkfs_dos::Error> {
            self.inner.flush().await
        }
    }

    async fn faulty(size: u64) -> (Filesystem<Faulty>, Allocator) {
        let inner = MemDevice::new(size);
        format(&inner, &Params::new()).await.unwrap();
        let dev = Faulty {
            inner,
            fault: std::sync::Mutex::new(None),
        };
        let fs = Filesystem::open(dev).await.unwrap();
        let alloc = Allocator::load(&fs).await.unwrap();
        (fs, alloc)
    }

    /// The FAT on the device, every copy, against the cache.
    async fn assert_on_device(fs: &Filesystem<Faulty>, alloc: &Allocator) {
        for index in 0..fs.boot().num_fats {
            assert_eq!(fs.read_fat(index).await.unwrap(), alloc.fat, "FAT {index}");
        }
        if let Some(info) = fs.read_fsinfo().await.unwrap() {
            assert_eq!(info.free_clusters, alloc.free_count());
            assert_eq!(info.next_cluster, alloc.next_free);
        }
    }

    /// dcad66f4201f, ffa48a70d950, 25d7393c6594: a flush that fails keeps
    /// what it did not finish, and the retry writes it — whichever FAT copy,
    /// or the FSInfo, the failure hit.
    #[tokio::test]
    async fn a_failed_flush_is_finished_by_the_retry() {
        for (size, target) in [
            (64 * MIB, "FAT 0"),
            (64 * MIB, "FAT 1"),
            (1024 * MIB, "FAT 1"),
            (1024 * MIB, "FSInfo"),
        ] {
            let (fs, mut alloc) = faulty(size).await;
            // Enough clusters to dirty several FAT sectors.
            let chain = alloc.allocate(2000).unwrap();
            let sector = fs.boot().bytes_per_sector as u64;
            let (start, len) = match target {
                "FAT 0" => (fs.fat_offset(0) + 2 * sector, sector),
                "FAT 1" => (fs.fat_offset(1) + 2 * sector, sector),
                _ => (fs.boot().sector_offset(fs.boot().info_sector as u64), sector),
            };
            fs.device().arm(start, len, false);
            assert!(alloc.flush(&fs).await.is_err(), "{target}: the flush failed");
            fs.device().disarm();

            alloc.flush(&fs).await.unwrap();
            assert_on_device(&fs, &alloc).await;
            assert_eq!(fs.fat_entry(chain[0]).await.unwrap(), chain[1], "{target}");
        }
    }

    /// The same for a flush cancelled at an await: dropping it part way loses
    /// nothing either.
    #[tokio::test]
    async fn a_cancelled_flush_is_finished_by_the_retry() {
        let (fs, mut alloc) = faulty(1024 * MIB).await;
        alloc.allocate(2000).unwrap();
        let sector = fs.boot().bytes_per_sector as u64;
        fs.device().arm(fs.fat_offset(1) + 2 * sector, sector, true);
        let cancelled =
            tokio::time::timeout(std::time::Duration::from_millis(100), alloc.flush(&fs)).await;
        assert!(cancelled.is_err(), "the flush hung and was dropped");
        fs.device().disarm();

        alloc.flush(&fs).await.unwrap();
        assert_on_device(&fs, &alloc).await;
    }

    /// 36d21b732805: allocating is linear in the volume. Every free cluster of
    /// a 1 GiB FAT32 in one chain: with the old duplicate scan that is tens of
    /// billions of comparisons, minutes even optimised; without it, a pass.
    #[tokio::test]
    async fn allocating_the_whole_volume_is_one_pass() {
        let (_fs, mut alloc) = volume(1024 * MIB).await;
        let free = alloc.free_count();
        assert!(free > 100_000, "{free} clusters is not a large enough test");

        let began = std::time::Instant::now();
        let chain = alloc.allocate(free).unwrap();
        let took = began.elapsed();
        assert!(took < std::time::Duration::from_secs(10), "took {took:?}");

        assert_eq!(chain.len() as u32, free);
        assert_eq!(alloc.free_count(), 0);
        let distinct: BTreeSet<u32> = chain.iter().copied().collect();
        assert_eq!(distinct.len(), chain.len(), "no cluster twice");
        assert_eq!(alloc.chain(chain[0]).unwrap(), chain);
    }

    #[tokio::test]
    async fn a_full_volume_says_so_rather_than_half_allocating() {
        let (fs, mut alloc) = volume(8 * MIB).await;
        let free = alloc.free_count();
        let err = alloc.allocate(free + 1).unwrap_err();
        assert!(matches!(err, Error::NoSpace { .. }), "{err}");
        // And nothing was taken.
        assert_eq!(alloc.free_count(), free);
        assert_eq!(fs.fat_entry(2).await.unwrap(), 0);
    }
}

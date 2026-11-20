//! A device that fails, and what is left after (#12, finding 0d6b59b69c3d).
//!
//! The rule under test: a failed change leaves a filesystem `fsck.fat` passes
//! and a cached FAT that agrees with the directories — the change either
//! happened or did not — and when that cannot be established, the volume
//! refuses everything after rather than flush a FAT that might join two files.

use std::sync::Mutex;

use fio_dos::mkfs_dos::device::{BlockDevice, MemDevice};
use fio_dos::mkfs_dos::fsck::{check_filesystem, FsckOptions};
use fio_dos::mkfs_dos::{format, FatType, Filesystem, Params};
use fio_dos::{Error, Volume};

const MIB: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
enum Mode {
    /// Fail without writing anything — the usual EIO.
    Refuse,
    /// Write, then report a failure anyway.
    WriteThenFail,
}

#[derive(Clone, Copy, Debug)]
struct Fault {
    start: u64,
    end: u64,
    mode: Mode,
    /// Fail reads of the range too, once a write to it has failed — so the
    /// read-back after a failed write fails, and the reads before it do not.
    reads: bool,
    tripped: bool,
}

/// A memory device that fails I/O touching one byte range, while armed.
struct Faulty {
    inner: MemDevice,
    fault: Mutex<Option<Fault>>,
}

impl Faulty {
    fn arm(&self, (start, len): (u64, u64), mode: Mode, reads: bool) {
        *self.fault.lock().unwrap() = Some(Fault {
            start,
            end: start + len,
            mode,
            reads,
            tripped: false,
        });
    }

    fn disarm(&self) {
        *self.fault.lock().unwrap() = None;
    }

    /// The fault, if this I/O touches its range. A write that touches it
    /// trips the read fault.
    fn hit(&self, offset: u64, len: usize, write: bool) -> Option<Fault> {
        let mut guard = self.fault.lock().unwrap();
        let fault = guard.as_mut()?;
        if offset < fault.end && offset + len as u64 > fault.start {
            if write {
                fault.tripped = true;
            }
            Some(*fault)
        } else {
            None
        }
    }
}

fn injected(offset: u64) -> fio_dos::mkfs_dos::Error {
    fio_dos::mkfs_dos::Error::Io {
        offset,
        source: std::io::Error::other("injected fault"),
    }
}

#[async_trait::async_trait]
impl BlockDevice for Faulty {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn logical_sector_size(&self) -> u32 {
        self.inner.logical_sector_size()
    }

    async fn read_at(
        &self,
        offset: u64,
        buf: &mut [u8],
    ) -> std::result::Result<(), fio_dos::mkfs_dos::Error> {
        if self
            .hit(offset, buf.len(), false)
            .is_some_and(|f| f.reads && f.tripped)
        {
            return Err(injected(offset));
        }
        self.inner.read_at(offset, buf).await
    }

    async fn write_at(
        &self,
        offset: u64,
        buf: &[u8],
    ) -> std::result::Result<(), fio_dos::mkfs_dos::Error> {
        match self.hit(offset, buf.len(), true) {
            None => self.inner.write_at(offset, buf).await,
            Some(Fault {
                mode: Mode::Refuse, ..
            }) => Err(injected(offset)),
            Some(Fault {
                mode: Mode::WriteThenFail,
                ..
            }) => {
                self.inner.write_at(offset, buf).await?;
                Err(injected(offset))
            }
        }
    }

    async fn flush(&self) -> std::result::Result<(), fio_dos::mkfs_dos::Error> {
        self.inner.flush().await
    }
}

/// A FAT16 volume: its root is a fixed region of its own, apart from the
/// data clusters, so a fault can hit one and not the other.
async fn volume() -> Volume<Faulty> {
    let inner = MemDevice::new(64 * MIB);
    format(&inner, &Params::new().label("TESTVOL").invariant())
        .await
        .unwrap();
    let device = Faulty {
        inner,
        fault: Mutex::new(None),
    };
    let mut vol = Volume::open(device).await.unwrap();
    assert_eq!(vol.filesystem().fat_type(), FatType::Fat16);
    vol.set_time(1_426_325_213);
    vol
}

fn device(vol: &Volume<Faulty>) -> &Faulty {
    vol.filesystem().device()
}

/// The fixed root directory.
fn root(vol: &Volume<Faulty>) -> (u64, u64) {
    let boot = vol.filesystem().boot();
    (
        boot.root_dir_offset(),
        boot.root_dir_sectors() as u64 * boot.bytes_per_sector as u64,
    )
}

/// Every data cluster.
fn data(vol: &Volume<Faulty>) -> (u64, u64) {
    let start = vol.filesystem().cluster_offset(2);
    (start, vol.filesystem().device().size() - start)
}

/// One cluster.
fn cluster(vol: &Volume<Faulty>, c: u32) -> (u64, u64) {
    let fs = vol.filesystem();
    (fs.cluster_offset(c), fs.cluster_size() as u64)
}

fn bytes(vol: &Volume<Faulty>, clusters: usize) -> Vec<u8> {
    let size = vol.filesystem().cluster_size() as usize * clusters;
    (0..size).map(|i| (i % 251) as u8).collect()
}

fn is_io(err: &Error) -> bool {
    matches!(err, Error::Fs(fio_dos::mkfs_dos::Error::Io { .. }))
}

async fn assert_clean(vol: &mut Volume<Faulty>, context: &str) {
    device(vol).disarm();
    vol.flush().await.unwrap();
    let image = device(vol).inner.to_vec();
    let fs = Filesystem::open(MemDevice::from_vec(image)).await.unwrap();
    let report = check_filesystem(&fs, &FsckOptions::check_only())
        .await
        .unwrap();
    assert!(
        report.is_clean(),
        "{context}: fsck found {:#?}",
        report.problems
    );
}

#[tokio::test]
async fn a_failed_data_write_takes_the_allocation_back() {
    let mut vol = volume().await;
    vol.write("/keep.txt", b"kept").await.unwrap();
    let free = vol.free_clusters();

    device(&vol).arm(data(&vol), Mode::Refuse, false);
    let data = bytes(&vol, 4);
    let err = vol.write("/new.bin", &data).await.unwrap_err();
    assert!(is_io(&err), "{err}");
    device(&vol).disarm();

    assert_eq!(vol.free_clusters(), free, "the clusters came back");
    assert!(!vol.exists("/new.bin").await.unwrap());
    assert!(!vol.is_poisoned());
    assert_clean(&mut vol, "failed data write").await;
}

#[tokio::test]
async fn a_refused_directory_write_takes_the_allocation_back() {
    let mut vol = volume().await;
    let free = vol.free_clusters();

    device(&vol).arm(root(&vol), Mode::Refuse, false);
    let data = bytes(&vol, 3);
    let err = vol.write("/new.bin", &data).await.unwrap_err();
    assert!(is_io(&err), "{err}");
    device(&vol).disarm();

    assert_eq!(vol.free_clusters(), free);
    assert!(!vol.exists("/new.bin").await.unwrap());
    assert!(!vol.is_poisoned());

    // And the volume goes on working.
    vol.write("/new.bin", &data).await.unwrap();
    assert_eq!(vol.read("/new.bin").await.unwrap(), data);
    assert_clean(&mut vol, "refused directory write").await;
}

#[tokio::test]
async fn a_directory_write_that_landed_finishes_the_change() {
    let mut vol = volume().await;
    let free = vol.free_clusters();

    device(&vol).arm(root(&vol), Mode::WriteThenFail, false);
    let data = bytes(&vol, 3);
    let err = vol.write("/new.bin", &data).await.unwrap_err();
    assert!(is_io(&err), "the device's error is still reported: {err}");
    device(&vol).disarm();

    assert_eq!(vol.read("/new.bin").await.unwrap(), data);
    assert_eq!(vol.free_clusters(), free - 3);
    assert_clean(&mut vol, "landed directory write").await;
}

#[tokio::test]
async fn a_failed_shrink_keeps_the_old_chain() {
    let mut vol = volume().await;
    let big = bytes(&vol, 10);
    vol.write("/f.bin", &big).await.unwrap();
    let free = vol.free_clusters();

    device(&vol).arm(root(&vol), Mode::Refuse, false);
    let err = vol.write("/f.bin", b"short").await.unwrap_err();
    assert!(is_io(&err), "{err}");
    device(&vol).disarm();

    // The entry still says ten clusters, and the chain still has them.
    assert_eq!(vol.stat("/f.bin").await.unwrap().size, big.len() as u64);
    assert_eq!(vol.free_clusters(), free);
    assert_eq!(vol.read("/f.bin").await.unwrap().len(), big.len());
    assert_clean(&mut vol, "failed shrink").await;

    // The shrink, done properly, frees the tail only once the entry is short.
    vol.write("/f.bin", b"short").await.unwrap();
    assert_eq!(vol.free_clusters(), free + 9);
    assert_eq!(vol.read("/f.bin").await.unwrap(), b"short");
    assert_clean(&mut vol, "shrink").await;
}

#[tokio::test]
async fn a_failed_grow_takes_the_extension_back() {
    let mut vol = volume().await;
    let small = bytes(&vol, 1);
    vol.write("/g.bin", &small).await.unwrap();
    let free = vol.free_clusters();

    device(&vol).arm(data(&vol), Mode::Refuse, false);
    let big = bytes(&vol, 6);
    let err = vol.write("/g.bin", &big).await.unwrap_err();
    assert!(is_io(&err), "{err}");
    device(&vol).disarm();

    assert_eq!(vol.free_clusters(), free);
    assert_eq!(vol.stat("/g.bin").await.unwrap().size, small.len() as u64);
    assert_clean(&mut vol, "failed grow").await;
}

#[tokio::test]
async fn a_failed_unlink_frees_nothing() {
    let mut vol = volume().await;
    let data = bytes(&vol, 4);
    vol.write("/u.bin", &data).await.unwrap();
    let free = vol.free_clusters();

    device(&vol).arm(root(&vol), Mode::Refuse, false);
    let err = vol.unlink("/u.bin").await.unwrap_err();
    assert!(is_io(&err), "{err}");
    device(&vol).disarm();

    assert_eq!(vol.free_clusters(), free, "nothing freed under a live entry");
    // The next file does not get the clusters the live one still has.
    let other = vec![0xeeu8; data.len()];
    vol.write("/other.bin", &other).await.unwrap();
    assert_eq!(vol.read("/u.bin").await.unwrap(), data);
    assert_eq!(vol.read("/other.bin").await.unwrap(), other);
    assert_clean(&mut vol, "failed unlink").await;
}

#[tokio::test]
async fn a_failed_mkdir_takes_its_cluster_back() {
    let mut vol = volume().await;
    let free = vol.free_clusters();

    device(&vol).arm(data(&vol), Mode::Refuse, false);
    let err = vol.mkdir("/d").await.unwrap_err();
    assert!(is_io(&err), "{err}");
    device(&vol).disarm();

    assert_eq!(vol.free_clusters(), free);
    assert!(!vol.exists("/d").await.unwrap());
    assert_clean(&mut vol, "failed mkdir").await;
}

#[tokio::test]
async fn a_rename_that_fails_half_way_is_undone() {
    let mut vol = volume().await;
    vol.mkdir("/d1").await.unwrap();
    vol.mkdir("/d2").await.unwrap();
    vol.write("/d1/f.txt", b"moving").await.unwrap();
    let d1 = vol.stat("/d1").await.unwrap().first_cluster;

    // The new name lands in d2; erasing the old one from d1 is refused.
    device(&vol).arm(cluster(&vol, d1), Mode::Refuse, false);
    let err = vol.rename("/d1/f.txt", "/d2/f.txt").await.unwrap_err();
    assert!(is_io(&err), "{err}");
    device(&vol).disarm();

    assert!(!vol.is_poisoned());
    assert_eq!(vol.read("/d1/f.txt").await.unwrap(), b"moving");
    assert!(!vol.exists("/d2/f.txt").await.unwrap(), "the first half was undone");
    assert_clean(&mut vol, "half a rename").await;
}

#[tokio::test]
async fn an_unknown_directory_state_poisons_the_volume() {
    let mut vol = volume().await;
    vol.write("/before.txt", b"x").await.unwrap();

    // The write fails, and so does reading the directory back.
    device(&vol).arm(root(&vol), Mode::Refuse, true);
    let err = vol.write("/p.txt", b"x").await.unwrap_err();
    assert!(matches!(err, Error::Poisoned(_)), "{err}");
    device(&vol).disarm();

    assert!(vol.is_poisoned());
    for result in [
        vol.write("/q.txt", b"x").await,
        vol.mkdir("/q").await,
        vol.unlink("/before.txt").await,
        vol.rename("/before.txt", "/after.txt").await,
        vol.set_label("OTHER").await,
        vol.flush().await,
    ] {
        assert!(matches!(result, Err(Error::Poisoned(_))), "{result:?}");
    }
    // Reading still works.
    assert_eq!(vol.read("/before.txt").await.unwrap(), b"x");
}

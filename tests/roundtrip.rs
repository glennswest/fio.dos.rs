//! What goes in comes out, and `fsck.fat` is happy afterwards.
//!
//! Every test here ends by checking the filesystem. A writer that leaves the
//! checker complaining has damaged the filesystem, not written a file — so the
//! check is part of the assertion rather than a separate suite.

use fio_dos::mkfs_dos::device::MemDevice;
use fio_dos::mkfs_dos::fsck::{check_filesystem, FsckOptions};
use fio_dos::mkfs_dos::{format, FatType, Filesystem, Params};
use fio_dos::{Attributes, Attrs, Volume};

const MIB: u64 = 1024 * 1024;

/// The three widths, at sizes that produce each one.
const WIDTHS: [(u64, FatType); 3] = [
    (8 * MIB, FatType::Fat12),
    (64 * MIB, FatType::Fat16),
    (1024 * MIB, FatType::Fat32),
];

async fn volume(size: u64) -> Volume<MemDevice> {
    let device = MemDevice::new(size);
    format(&device, &Params::new().label("TESTVOL").invariant())
        .await
        .unwrap();
    let mut vol = Volume::open(device).await.unwrap();
    // A fixed clock, so a test that compares two images compares the files.
    vol.set_time(1_426_325_213);
    vol
}

/// Flush, then hand the volume to the checker. Panics with what it found.
async fn assert_clean(vol: &mut Volume<MemDevice>, context: &str) {
    vol.flush().await.unwrap();
    let image = vol.filesystem().device().to_vec();
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
async fn a_file_written_reads_back_on_every_width() {
    for (size, want) in WIDTHS {
        let mut vol = volume(size).await;
        assert_eq!(vol.filesystem().fat_type(), want);

        vol.write("/hello.txt", b"hello, world\n").await.unwrap();
        assert_eq!(vol.read("/hello.txt").await.unwrap(), b"hello, world\n");
        assert_clean(&mut vol, want.name()).await;
    }
}

/// The sizes either side of a cluster boundary are where an off-by-one in the
/// chain arithmetic shows up, and nowhere else.
#[tokio::test]
async fn files_of_every_awkward_size_round_trip() {
    let mut vol = volume(64 * MIB).await;
    let cluster = vol.filesystem().cluster_size() as usize;

    let sizes = [
        0,
        1,
        cluster - 1,
        cluster,
        cluster + 1,
        cluster * 2,
        cluster * 3 + 7,
        1024 * 1024,
    ];
    for (i, &size) in sizes.iter().enumerate() {
        let data: Vec<u8> = (0..size).map(|n| (n % 251) as u8).collect();
        let path = format!("/size{i}.bin");
        vol.write(&path, &data).await.unwrap();
        assert_eq!(vol.read(&path).await.unwrap(), data, "size {size}");
        assert_eq!(vol.stat(&path).await.unwrap().size, size as u64);
    }
    assert_clean(&mut vol, "awkward sizes").await;
}

#[tokio::test]
async fn nested_directories_are_created_and_walked() {
    for (size, _) in WIDTHS {
        let mut vol = volume(size).await;
        vol.mkdir_all("/EFI/BOOT/deep/deeper").await.unwrap();
        vol.write("/EFI/BOOT/BOOTX64.EFI", b"MZ\x90\x00").await.unwrap();
        vol.write("/EFI/BOOT/deep/deeper/leaf.txt", b"leaf").await.unwrap();

        assert!(vol.exists("/EFI/BOOT/deep/deeper").await.unwrap());
        assert_eq!(vol.read("/EFI/BOOT/deep/deeper/leaf.txt").await.unwrap(), b"leaf");

        let listing = vol.read_dir("/EFI/BOOT").await.unwrap();
        let names: Vec<_> = listing.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"BOOTX64.EFI"), "{names:?}");
        assert!(names.contains(&"deep"), "{names:?}");
        // "." and ".." are bookkeeping, not names.
        assert!(!names.contains(&"."), "{names:?}");
        assert!(!names.contains(&".."), "{names:?}");

        assert_clean(&mut vol, "nested directories").await;
    }
}

#[tokio::test]
async fn long_names_survive_the_round_trip() {
    let mut vol = volume(64 * MIB).await;
    let names = [
        "A Long File Name With Spaces.txt",
        "ReadMe.md",
        "two.dots.in.here.tar.gz",
        ".config",
        "café ☕.txt",
        &"x".repeat(200),
    ];
    for (i, name) in names.iter().enumerate() {
        vol.write(&format!("/{name}"), format!("contents {i}").as_bytes())
            .await
            .unwrap();
    }

    let listing = vol.read_dir("/").await.unwrap();
    for (i, name) in names.iter().enumerate() {
        let entry = listing
            .iter()
            .find(|e| e.name == *name)
            .unwrap_or_else(|| panic!("{name} is missing from {listing:#?}"));
        assert_ne!(entry.short_name, entry.name, "{name} should have a short name too");
        assert_eq!(
            vol.read(&format!("/{name}")).await.unwrap(),
            format!("contents {i}").as_bytes()
        );
    }
    assert_clean(&mut vol, "long names").await;
}

#[tokio::test]
async fn a_name_that_fits_needs_no_long_entry() {
    let mut vol = volume(64 * MIB).await;
    vol.write("/README.TXT", b"a").await.unwrap();
    vol.write("/readme2.txt", b"b").await.unwrap();

    let listing = vol.read_dir("/").await.unwrap();
    for name in ["README.TXT", "readme2.txt"] {
        let entry = listing.iter().find(|e| e.name == name).unwrap();
        assert_eq!(entry.short_name, name, "{name} is stored as itself");
    }
    assert_clean(&mut vol, "short names").await;
}

#[tokio::test]
async fn names_are_found_whatever_case_they_are_asked_for() {
    let mut vol = volume(64 * MIB).await;
    vol.write("/Config.SYS", b"x").await.unwrap();
    assert_eq!(vol.read("/CONFIG.sys").await.unwrap(), b"x");
    assert!(vol.exists("/config.sys").await.unwrap());
    assert_clean(&mut vol, "case").await;
}

#[tokio::test]
async fn rewriting_a_file_reuses_its_clusters() {
    let mut vol = volume(64 * MIB).await;
    let cluster = vol.filesystem().cluster_size() as usize;

    vol.write("/file.bin", &vec![1u8; cluster * 4]).await.unwrap();
    let free_after_first = vol.free_clusters();
    let first_cluster = vol.stat("/file.bin").await.unwrap().first_cluster;

    vol.write("/file.bin", &vec![2u8; cluster * 4]).await.unwrap();
    assert_eq!(vol.free_clusters(), free_after_first, "space was allocated twice");
    assert_eq!(vol.stat("/file.bin").await.unwrap().first_cluster, first_cluster);
    assert_eq!(vol.read("/file.bin").await.unwrap(), vec![2u8; cluster * 4]);

    // Shrinking gives the difference back.
    vol.write("/file.bin", &vec![3u8; cluster]).await.unwrap();
    assert_eq!(vol.free_clusters(), free_after_first + 3);
    assert_clean(&mut vol, "rewrite").await;
}

#[tokio::test]
async fn deleting_everything_gives_every_cluster_back() {
    for (size, _) in WIDTHS {
        let mut vol = volume(size).await;
        let free_at_start = vol.free_clusters();

        vol.mkdir_all("/a/b/c").await.unwrap();
        for i in 0..20 {
            vol.write(&format!("/a/b/c/file{i}.bin"), &vec![7u8; 5000])
                .await
                .unwrap();
        }
        assert!(vol.free_clusters() < free_at_start);

        vol.remove_all("/a").await.unwrap();
        assert_eq!(
            vol.free_clusters(),
            free_at_start,
            "clusters were leaked at {size} bytes"
        );
        assert!(vol.read_dir("/").await.unwrap().is_empty());
        assert_clean(&mut vol, "remove_all").await;
    }
}

#[tokio::test]
async fn unlink_and_rmdir_refuse_the_wrong_kind() {
    let mut vol = volume(64 * MIB).await;
    vol.mkdir("/dir").await.unwrap();
    vol.write("/file", b"x").await.unwrap();
    vol.write("/dir/inside", b"x").await.unwrap();

    assert!(vol.unlink("/dir").await.is_err(), "unlink on a directory");
    assert!(vol.rmdir("/file").await.is_err(), "rmdir on a file");
    assert!(vol.rmdir("/dir").await.is_err(), "rmdir on a full directory");

    vol.unlink("/dir/inside").await.unwrap();
    vol.rmdir("/dir").await.unwrap();
    assert!(!vol.exists("/dir").await.unwrap());
    assert_clean(&mut vol, "unlink and rmdir").await;
}

#[tokio::test]
async fn rename_moves_a_file_and_a_directory() {
    let mut vol = volume(64 * MIB).await;
    vol.mkdir_all("/one/two").await.unwrap();
    vol.write("/one/file.txt", b"contents").await.unwrap();

    // Within a directory.
    vol.rename("/one/file.txt", "/one/renamed.txt").await.unwrap();
    assert!(!vol.exists("/one/file.txt").await.unwrap());
    assert_eq!(vol.read("/one/renamed.txt").await.unwrap(), b"contents");

    // Into another directory.
    vol.rename("/one/renamed.txt", "/one/two/moved.txt").await.unwrap();
    assert_eq!(vol.read("/one/two/moved.txt").await.unwrap(), b"contents");

    // A directory, whose ".." must follow it.
    vol.mkdir("/elsewhere").await.unwrap();
    vol.rename("/one/two", "/elsewhere/two").await.unwrap();
    assert_eq!(vol.read("/elsewhere/two/moved.txt").await.unwrap(), b"contents");
    assert_clean(&mut vol, "rename").await;
}

#[tokio::test]
async fn a_moved_directory_points_at_its_new_parent() {
    let mut vol = volume(64 * MIB).await;
    vol.mkdir_all("/from/child").await.unwrap();
    vol.mkdir("/to").await.unwrap();
    vol.rename("/from/child", "/to/child").await.unwrap();
    vol.flush().await.unwrap();

    // The checker is what proves it: a ".." left pointing at the old parent is
    // exactly what it looks for.
    assert_clean(&mut vol, "moved directory").await;

    let to_cluster = vol.stat("/to").await.unwrap().first_cluster;
    let child = vol.stat("/to/child").await.unwrap();
    let fs = vol.filesystem();
    let contents = fs.read_directory(Some(child.first_cluster)).await.unwrap();
    let dotdot = fio_dos::dir::parse(&contents)
        .into_iter()
        .find(|e| e.name == "..")
        .expect("a directory has a ..");
    assert_eq!(dotdot.first_cluster(), to_cluster);
}

#[tokio::test]
async fn attributes_and_timestamps_are_kept() {
    let mut vol = volume(64 * MIB).await;
    vol.write_with("/ro.txt", b"x", &Attrs::read_only()).await.unwrap();
    vol.write_with("/sys.txt", b"x", &Attrs::system().modified_at(1_000_000_000))
        .await
        .unwrap();

    let ro = vol.stat("/ro.txt").await.unwrap();
    assert!(ro.attributes.contains(Attributes::READ_ONLY));
    let sys = vol.stat("/sys.txt").await.unwrap();
    assert!(sys.attributes.contains(Attributes::HIDDEN | Attributes::SYSTEM));
    // Two-second resolution, so the timestamp comes back rounded down.
    assert_eq!(sys.modified, 1_000_000_000);

    vol.set_attributes("/ro.txt", Attributes::ARCHIVE).await.unwrap();
    assert!(!vol
        .stat("/ro.txt")
        .await
        .unwrap()
        .attributes
        .contains(Attributes::READ_ONLY));
    assert_clean(&mut vol, "attributes").await;
}

#[tokio::test]
async fn a_directory_grows_past_one_cluster() {
    let mut vol = volume(64 * MIB).await;
    vol.mkdir("/many").await.unwrap();
    let per_cluster = vol.filesystem().cluster_size() / 32;

    // Long names take three slots each, so this fills several clusters.
    let count = per_cluster * 2;
    for i in 0..count {
        vol.write(&format!("/many/A Long File Name {i}.txt"), b"x")
            .await
            .unwrap();
    }
    let listing = vol.read_dir("/many").await.unwrap();
    assert_eq!(listing.len(), count as usize);
    assert_clean(&mut vol, "grown directory").await;
}

/// The fixed root of a FAT12 or FAT16 volume cannot grow. Filling it must be an
/// error and not a corrupted directory.
#[tokio::test]
async fn the_fixed_root_directory_fills_up_and_says_so() {
    let mut vol = volume(64 * MIB).await;
    let root_entries = vol.filesystem().boot().root_entries as usize;

    let mut created = 0;
    let mut last_error = None;
    for i in 0..root_entries + 10 {
        match vol.write(&format!("/FILE{i:04}.TXT"), b"x").await {
            Ok(()) => created += 1,
            Err(e) => {
                last_error = Some(e);
                break;
            }
        }
    }
    assert!(created > 0);
    assert!(created <= root_entries);
    assert!(
        matches!(last_error, Some(fio_dos::Error::RootDirectoryFull { .. })),
        "{last_error:?}"
    );
    assert_clean(&mut vol, "full root").await;
}

/// FAT32's root is an ordinary chain, so it has no such limit.
#[tokio::test]
async fn the_fat32_root_directory_grows() {
    let mut vol = volume(1024 * MIB).await;
    for i in 0..600 {
        vol.write(&format!("/FILE{i:04}.TXT"), b"x").await.unwrap();
    }
    assert_eq!(vol.read_dir("/").await.unwrap().len(), 600);
    assert_clean(&mut vol, "grown FAT32 root").await;
}

#[tokio::test]
async fn a_full_volume_refuses_rather_than_corrupting() {
    let mut vol = volume(8 * MIB).await;
    let cluster = vol.filesystem().cluster_size() as usize;
    let free = vol.free_clusters() as usize;

    let err = vol
        .write("/toobig.bin", &vec![0u8; (free + 2) * cluster])
        .await
        .unwrap_err();
    assert!(matches!(err, fio_dos::Error::NoSpace { .. }), "{err}");
    // And the volume is untouched: nothing half-written, nothing leaked.
    assert_eq!(vol.free_clusters() as usize, free);
    assert!(!vol.exists("/toobig.bin").await.unwrap());
    assert_clean(&mut vol, "full volume").await;
}

#[tokio::test]
async fn the_volume_label_is_read_and_written() {
    for (size, _) in WIDTHS {
        let mut vol = volume(size).await;
        assert_eq!(vol.label().await.unwrap().as_deref(), Some("TESTVOL"));

        vol.set_label("NEWLABEL").await.unwrap();
        assert_eq!(vol.label().await.unwrap().as_deref(), Some("NEWLABEL"));

        // The boot sector's copy has to agree — it is what blkid reads.
        vol.flush().await.unwrap();
        let image = vol.filesystem().device().to_vec();
        let fs = Filesystem::open(MemDevice::from_vec(image)).await.unwrap();
        assert_eq!(fs.boot().label(), "NEWLABEL");

        assert_clean(&mut vol, "label").await;
    }
}

#[tokio::test]
async fn append_extends_a_file() {
    let mut vol = volume(64 * MIB).await;
    vol.write("/log.txt", b"first\n").await.unwrap();
    vol.append("/log.txt", b"second\n").await.unwrap();
    vol.append("/missing.txt", b"created\n").await.unwrap();

    assert_eq!(vol.read("/log.txt").await.unwrap(), b"first\nsecond\n");
    assert_eq!(vol.read("/missing.txt").await.unwrap(), b"created\n");
    assert_clean(&mut vol, "append").await;
}

#[tokio::test]
async fn missing_paths_are_reported_not_invented() {
    let vol = volume(64 * MIB).await;
    assert!(matches!(
        vol.read("/nope.txt").await.unwrap_err(),
        fio_dos::Error::NotFound(_)
    ));
    assert!(matches!(
        vol.read_dir("/nope").await.unwrap_err(),
        fio_dos::Error::NotFound(_)
    ));
    assert!(!vol.exists("/nope.txt").await.unwrap());
}

#[tokio::test]
async fn a_volume_reopened_sees_what_was_written() {
    let mut vol = volume(64 * MIB).await;
    vol.mkdir("/dir").await.unwrap();
    vol.write("/dir/file.bin", &vec![9u8; 100_000]).await.unwrap();
    vol.flush().await.unwrap();

    let image = vol.filesystem().device().to_vec();
    let reopened = Volume::open(MemDevice::from_vec(image)).await.unwrap();
    assert_eq!(reopened.read("/dir/file.bin").await.unwrap(), vec![9u8; 100_000]);
    assert_eq!(reopened.read_dir("/dir").await.unwrap().len(), 1);
}

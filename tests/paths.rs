//! Paths, names and what a change may land on (#12).
//!
//! Each test names the review finding it covers. Every test ends with the
//! filesystem checked, as in `roundtrip.rs`: a refused change that damaged
//! the tree on its way to being refused has not been refused.

use fio_dos::mkfs_dos::device::MemDevice;
use fio_dos::mkfs_dos::fsck::{check_filesystem, FsckOptions};
use fio_dos::mkfs_dos::{format, FatType, Filesystem, Params};
use fio_dos::{Attributes, Attrs, Error, Volume};

const MIB: u64 = 1024 * 1024;

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
    vol.set_time(1_426_325_213);
    vol
}

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

fn names(entries: &[fio_dos::Entry]) -> Vec<String> {
    let mut names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
    names.sort();
    names
}

/// f16592d1a3b5: a `..` holding cluster zero is the root, on every width.
#[tokio::test]
async fn dotdot_under_the_root_is_the_root() {
    for (size, width) in WIDTHS {
        let mut vol = volume(size).await;
        vol.mkdir("/a").await.unwrap();
        vol.mkdir("/a/b").await.unwrap();
        vol.write("/root.txt", b"at the root").await.unwrap();
        vol.write("/a/in-a.txt", b"in a").await.unwrap();

        assert_eq!(
            names(&vol.read_dir("/a/..").await.unwrap()),
            ["a", "root.txt"],
            "{width:?}"
        );
        assert_eq!(names(&vol.read_dir("/..").await.unwrap()), ["a", "root.txt"]);
        assert_eq!(vol.read("/a/../root.txt").await.unwrap(), b"at the root");
        assert_eq!(vol.read("/a/b/../in-a.txt").await.unwrap(), b"in a");
        assert_eq!(
            names(&vol.read_dir("/a/b/..").await.unwrap()),
            ["b", "in-a.txt"]
        );
        assert_eq!(names(&vol.read_dir("/a/b/../..").await.unwrap()), ["a", "root.txt"]);
        assert!(vol.stat("/a/..").await.unwrap().is_dir);
        assert!(vol.exists("/a/../root.txt").await.unwrap());

        // Creating through `..` is refused, not a panic in grow_dir.
        assert!(matches!(
            vol.write("/a/../new.txt", b"x").await.unwrap_err(),
            Error::InvalidPath(_)
        ));
        assert!(matches!(
            vol.mkdir("/a/../newdir").await.unwrap_err(),
            Error::InvalidPath(_)
        ));
        assert!(!vol.exists("/new.txt").await.unwrap());
        assert_clean(&mut vol, &format!("{width:?} dotdot")).await;
    }
}

/// 6fa79638bce5, 3ff6b13b3ec5: `.` and `..` entries are never a target.
#[tokio::test]
async fn dot_entries_are_never_changed() {
    for (size, width) in WIDTHS {
        let mut vol = volume(size).await;
        vol.mkdir("/A").await.unwrap();
        vol.mkdir("/a2").await.unwrap();
        vol.mkdir("/a2/b").await.unwrap();

        for result in [
            vol.rmdir("/A/..").await,
            vol.unlink("/A/..").await,
            vol.remove_all("/A/..").await,
            vol.rename("/a2/b/..", "/x").await,
            vol.rename("/a2/b", "/a2/b/../c").await,
            vol.set_attributes("/A/..", Attributes::HIDDEN).await,
            vol.set_modified("/A/..", 0).await,
            vol.mkdir_all("/A/../B").await,
        ] {
            assert!(
                matches!(result, Err(Error::InvalidPath(_))),
                "{width:?}: {result:?}"
            );
        }
        // Only `.` and `..` remain in A; and the tree is as it was.
        assert!(vol.read_dir("/A").await.unwrap().is_empty());
        assert_eq!(names(&vol.read_dir("/").await.unwrap()), ["A", "a2"]);
        assert_eq!(names(&vol.read_dir("/a2").await.unwrap()), ["b"]);
        assert!(!vol.exists("/x").await.unwrap());
        // `/A/.` is `/A`: `.` is skipped, so this removes A itself, properly.
        vol.rmdir("/A/.").await.unwrap();
        assert!(!vol.exists("/A").await.unwrap());
        assert_clean(&mut vol, &format!("{width:?} dot entries")).await;
    }
}

/// 44347c5c1704, 7086d9cf795f, e5fe266defb1: a directory is not moved into
/// itself, however the destination is spelled.
#[tokio::test]
async fn a_directory_is_not_moved_into_itself() {
    for (size, width) in WIDTHS {
        let mut vol = volume(size).await;
        vol.mkdir_all("/A/B/C").await.unwrap();
        vol.write("/A/B/C/f.txt", b"deep").await.unwrap();

        for to in [
            "/a/child",
            "/A/child",
            "/a/B/moved",
            "//a//./B/moved",
            "/A/./b/c/moved",
        ] {
            let err = vol.rename("/A", to).await.unwrap_err();
            assert!(matches!(err, Error::InvalidPath(_)), "{width:?} {to}: {err}");
        }
        let err = vol.rename("/A/B", "/a/b/C/x").await.unwrap_err();
        assert!(matches!(err, Error::InvalidPath(_)), "{width:?}: {err}");

        // Nothing moved.
        assert_eq!(names(&vol.read_dir("/").await.unwrap()), ["A"]);
        assert_eq!(vol.read("/A/B/C/f.txt").await.unwrap(), b"deep");

        // A real move up and out still works, and a case-only rename too.
        vol.rename("/A/B/C", "/C").await.unwrap();
        vol.rename("/A", "/a").await.unwrap();
        assert_eq!(names(&vol.read_dir("/").await.unwrap()), ["C", "a"]);
        assert_eq!(vol.read("/C/f.txt").await.unwrap(), b"deep");
        assert_clean(&mut vol, &format!("{width:?} self move")).await;
    }
}

/// bbc3584c0279: a file answers to its 8.3 alias as well as its long name.
#[tokio::test]
async fn a_short_alias_names_the_same_file() {
    for (size, width) in WIDTHS {
        let mut vol = volume(size).await;
        vol.write("/Long File.txt", b"one").await.unwrap();
        let alias = vol.stat("/Long File.txt").await.unwrap().short_name;
        assert_eq!(alias, "LONGFI~1.TXT", "{width:?}");

        assert_eq!(vol.read("/LONGFI~1.TXT").await.unwrap(), b"one");
        assert_eq!(vol.read("/longfi~1.txt").await.unwrap(), b"one");
        assert!(vol.exists("/LONGFI~1.TXT").await.unwrap());

        // Writing by the alias replaces the file, not a second one beside it.
        vol.write("/LONGFI~1.TXT", b"two").await.unwrap();
        assert_eq!(vol.read("/Long File.txt").await.unwrap(), b"two");
        assert_eq!(names(&vol.read_dir("/").await.unwrap()), ["Long File.txt"]);

        // And it collides as a name.
        assert!(matches!(
            vol.mkdir("/LONGFI~1.TXT").await.unwrap_err(),
            Error::Exists(_)
        ));
        vol.write("/other.txt", b"x").await.unwrap();
        assert!(matches!(
            vol.rename("/other.txt", "/LONGFI~1.TXT").await.unwrap_err(),
            Error::Exists(_)
        ));

        // Rename and remove by the alias too.
        vol.mkdir("/dir").await.unwrap();
        vol.rename("/LONGFI~1.TXT", "/dir/Renamed Long.txt").await.unwrap();
        assert_eq!(vol.read("/dir/RENAME~1.TXT").await.unwrap(), b"two");
        vol.unlink("/dir/RENAME~1.TXT").await.unwrap();
        assert!(vol.read_dir("/dir").await.unwrap().is_empty());
        assert_clean(&mut vol, &format!("{width:?} alias")).await;
    }
}

/// c68efc8833ae, bdefde03a16e: a size the chain cannot hold is corruption,
/// not a short read.
#[tokio::test]
async fn a_size_the_chain_cannot_hold_is_reported() {
    let mut vol = volume(64 * MIB).await;
    let cluster = vol.filesystem().cluster_size() as usize;
    vol.write("/F.BIN", &vec![7u8; cluster]).await.unwrap();
    vol.write("/E.BIN", b"").await.unwrap();
    vol.flush().await.unwrap();
    let image = vol.filesystem().device().to_vec();
    let root = vol.filesystem().boot().root_dir_offset() as usize;
    let slot = |image: &[u8], name: &[u8; 11]| {
        (0..512)
            .map(|i| root + i * 32)
            .find(|&off| &image[off..off + 11] == name)
            .expect("the entry is in the root")
    };

    // Three clusters recorded, one in the chain.
    let mut longer = image.clone();
    let f = slot(&longer, b"F       BIN");
    longer[f + 28..f + 32].copy_from_slice(&(3 * cluster as u32).to_le_bytes());
    let vol = Volume::open(MemDevice::from_vec(longer)).await.unwrap();
    let err = vol.read("/F.BIN").await.unwrap_err();
    assert!(
        matches!(err, Error::Fs(fio_dos::mkfs_dos::Error::Corrupt { .. })),
        "{err}"
    );

    // Bytes recorded, no cluster at all.
    let mut empty = image.clone();
    let e = slot(&empty, b"E       BIN");
    empty[e + 28..e + 32].copy_from_slice(&100u32.to_le_bytes());
    let vol = Volume::open(MemDevice::from_vec(empty)).await.unwrap();
    let err = vol.read("/E.BIN").await.unwrap_err();
    assert!(
        matches!(err, Error::Fs(fio_dos::mkfs_dos::Error::Corrupt { .. })),
        "{err}"
    );

    // Fewer bytes than the chain holds is the ordinary case: the tail of the
    // last cluster is slack.
    let mut shorter = image;
    let f = slot(&shorter, b"F       BIN");
    shorter[f + 28..f + 32].copy_from_slice(&10u32.to_le_bytes());
    let vol = Volume::open(MemDevice::from_vec(shorter)).await.unwrap();
    assert_eq!(vol.read("/F.BIN").await.unwrap(), vec![7u8; 10]);
}

/// d7a26c69492e: a caller cannot make a file into a directory or a label.
#[tokio::test]
async fn a_file_cannot_be_given_a_structural_kind() {
    let mut vol = volume(64 * MIB).await;
    for bits in [
        Attributes::VOLUME_ID,
        Attributes::DIRECTORY,
        Attributes::DIRECTORY | Attributes::HIDDEN,
        Attributes::from_bits_retain(0x0f),
        Attributes::from_bits_retain(0x40),
    ] {
        let attrs = Attrs {
            attributes: bits,
            ..Default::default()
        };
        let err = vol.write_with("/bad.txt", b"x", &attrs).await.unwrap_err();
        assert!(matches!(err, Error::InvalidAttributes { .. }), "{bits:?}: {err}");
    }
    assert!(!vol.exists("/bad.txt").await.unwrap());

    vol.write("/file.txt", b"data").await.unwrap();
    for bits in [Attributes::VOLUME_ID, Attributes::DIRECTORY] {
        let err = vol.set_attributes("/file.txt", bits).await.unwrap_err();
        assert!(matches!(err, Error::InvalidAttributes { .. }), "{bits:?}: {err}");
    }
    let file = vol.stat("/file.txt").await.unwrap();
    assert!(!file.is_dir);
    assert_eq!(vol.read("/file.txt").await.unwrap(), b"data");

    // A directory keeps its kind whatever it is given.
    vol.mkdir("/dir").await.unwrap();
    vol.set_attributes("/dir", Attributes::HIDDEN).await.unwrap();
    let dir = vol.stat("/dir").await.unwrap();
    assert!(dir.is_dir);
    assert!(dir.attributes.contains(Attributes::HIDDEN));
    vol.set_attributes("/dir", Attributes::DIRECTORY).await.unwrap();
    assert!(vol.stat("/dir").await.unwrap().is_dir);
    assert!(vol
        .set_attributes("/dir", Attributes::VOLUME_ID)
        .await
        .is_err());

    assert_eq!(names(&vol.read_dir("/").await.unwrap()), ["dir", "file.txt"]);
    assert_clean(&mut vol, "attributes").await;
}

/// f81439860dd8: bytes past a directory's end marker are not part of it, and
/// a name written on the marker must not bring them back. An image another
/// writer left — an entry whose first byte was zeroed, the entries after it
/// intact — is made here by zeroing one byte on the device.
#[tokio::test]
async fn stale_slots_past_the_end_marker_stay_unreachable() {
    for (size, width) in WIDTHS {
        let mut vol = volume(size).await;
        // Everything that must stay visible comes before the entry whose
        // first byte is zeroed: that zero ends the directory.
        vol.write("/A.TXT", b"kept").await.unwrap();
        vol.mkdir("/sub").await.unwrap();
        vol.write("/B.TXT", b"").await.unwrap();
        vol.write("/C.TXT", b"").await.unwrap();
        vol.write("/sub/D.TXT", b"").await.unwrap();
        vol.write("/sub/E.TXT", b"").await.unwrap();
        vol.flush().await.unwrap();

        let mut image = vol.filesystem().device().to_vec();
        for stale in [b"B       TXT", b"D       TXT"] {
            let at = image
                .windows(11)
                .position(|w| w == stale)
                .expect("the entry is on the device");
            image[at] = 0;
        }
        let mut vol = Volume::open(MemDevice::from_vec(image)).await.unwrap();
        vol.set_time(1_426_325_213);
        assert_eq!(names(&vol.read_dir("/").await.unwrap()), ["A.TXT", "sub"], "{width:?}");
        assert_eq!(names(&vol.read_dir("/sub").await.unwrap()), [] as [&str; 0]);

        // A one-slot name on the root's marker, and a long name on the
        // subdirectory's.
        vol.write("/NEW.TXT", b"new").await.unwrap();
        vol.write("/sub/A Long Name.txt", b"long").await.unwrap();
        assert_eq!(
            names(&vol.read_dir("/").await.unwrap()),
            ["A.TXT", "NEW.TXT", "sub"],
            "{width:?}: C.TXT stayed past the end"
        );
        assert_eq!(names(&vol.read_dir("/sub").await.unwrap()), ["A Long Name.txt"]);
        assert!(!vol.exists("/C.TXT").await.unwrap());
        assert!(!vol.exists("/sub/E.TXT").await.unwrap());
        assert_eq!(vol.read("/NEW.TXT").await.unwrap(), b"new");
        assert_eq!(vol.read("/sub/A Long Name.txt").await.unwrap(), b"long");
        assert_clean(&mut vol, &format!("{width:?} stale")).await;
    }
}

/// 43bd3bc2b137: an 8.3 name another entry answers to as its alias is that
/// entry, in every way a name can be given.
#[tokio::test]
async fn an_alias_is_an_occupied_name() {
    for (size, width) in WIDTHS {
        let mut vol = volume(size).await;
        vol.mkdir("/Program Files").await.unwrap();
        assert_eq!(vol.stat("/Program Files").await.unwrap().short_name, "PROGRA~1");
        assert!(matches!(vol.mkdir("/PROGRA~1").await.unwrap_err(), Error::Exists(_)));
        assert!(matches!(
            vol.write("/progra~1", b"x").await.unwrap_err(),
            Error::IsADirectory(_)
        ));
        vol.write("/x.txt", b"x").await.unwrap();
        assert!(matches!(
            vol.rename("/x.txt", "/PROGRA~1").await.unwrap_err(),
            Error::Exists(_)
        ));
        vol.write("/PROGRA~1/inside.txt", b"in").await.unwrap();
        assert_eq!(vol.read("/Program Files/inside.txt").await.unwrap(), b"in");
        assert_eq!(names(&vol.read_dir("/").await.unwrap()), ["Program Files", "x.txt"], "{width:?}");
        assert_clean(&mut vol, &format!("{width:?} alias occupied")).await;
    }
}

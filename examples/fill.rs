//! Fill a fresh image with a known set of files, and print what was written.
//!
//! Used by `tests/verify-on-linux.sh`: this side writes the files, a real Linux
//! kernel reads them back, and the manifest is what makes "reads them back"
//! checkable rather than a look at a directory listing.
//!
//! ```sh
//! cargo run --example fill -- out.img 64 fat16
//! ```
//!
//! Each file's contents are generated from its index, so the far side can
//! regenerate them without the bytes travelling with the manifest.

use fio_dos::mkfs_dos::device::FileDevice;
use fio_dos::mkfs_dos::{format, FatType, Params};
use fio_dos::Volume;

/// The contents of file `index`: deterministic, and not a repeating byte, so a
/// misplaced cluster shows up as a difference rather than as more of the same.
fn contents(index: usize, size: usize) -> Vec<u8> {
    (0..size).map(|j| ((index * 37 + j) % 251) as u8).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: fill <path> <size-mib> [fat12|fat16|fat32]")?;
    let size_mib: u64 = args.next().ok_or("missing size in MiB")?.parse()?;

    let mut params = Params::new().invariant().label("FIOTEST");
    if let Some(width) = args.next() {
        params.fat_type = Some(width.parse::<FatType>()?);
    }

    let device = FileDevice::create(&path, size_mib * 1024 * 1024).await?;
    let report = format(&device, &params).await?;
    eprintln!("{path}: {report}");

    let mut vol = Volume::open(device).await?;
    vol.set_time(1_426_325_213);

    // Sizes chosen around the cluster boundary, where an off-by-one lives, and
    // one file large enough to need a chain of hundreds.
    let files: Vec<(&str, usize)> = vec![
        ("/README.TXT", 13),
        ("/empty.txt", 0),
        ("/one-byte.bin", 1),
        ("/EFI/BOOT/BOOTX64.EFI", 100_000),
        ("/EFI/BOOT/A Long File Name.txt", 4095),
        ("/EFI/BOOT/exactly-4096.bin", 4096),
        ("/EFI/BOOT/one-more-4097.bin", 4097),
        ("/nested/a/b/c/deep.bin", 300_000),
        ("/café ☕.txt", 500),
        ("/mixed.Case.Name.TXT", 2000),
    ];

    vol.mkdir_all("/EFI/BOOT").await?;
    vol.mkdir_all("/nested/a/b/c").await?;
    vol.mkdir_all("/many").await?;

    for (index, (path, size)) in files.iter().enumerate() {
        vol.write(path, &contents(index, *size)).await?;
        println!("{index}\t{size}\t{path}");
    }

    // Enough names in one directory that it has to grow past a cluster.
    for i in 0..300 {
        let name = format!("/many/A Long File Name {i:03}.txt");
        let index = 100 + i;
        vol.write(&name, &contents(index, 200)).await?;
        println!("{index}\t200\t{name}");
    }

    vol.flush().await?;
    eprintln!("{} clusters free", vol.free_clusters());
    Ok(())
}

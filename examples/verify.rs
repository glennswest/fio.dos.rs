//! Read an image back and check every file against the manifest `fill` printed.
//!
//! The other half of `tests/verify-on-linux.sh`: after a real kernel has
//! mounted the image and written to it, this reads it with no kernel at all and
//! must find every file exactly as it was left.
//!
//! ```sh
//! cargo run --example verify -- out.img manifest.tsv
//! cargo run --example verify -- --dir /mnt/fat manifest.tsv
//! ```
//!
//! With `--dir`, the files are read from a directory instead — the mount point
//! of the same image — so this is also how the kernel's side is judged: the
//! kernel's FAT driver reads every file and the bytes are compared here.

use fio_dos::mkfs_dos::device::FileDevice;
use fio_dos::Volume;

fn contents(index: usize, size: usize) -> Vec<u8> {
    (0..size).map(|j| ((index * 37 + j) % 251) as u8).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: verify <image> <manifest> | verify --dir <mountpoint> <manifest>";
    let mut first = args.next().ok_or(usage)?;
    let dir = first == "--dir";
    if dir {
        first = args.next().ok_or(usage)?;
    }
    let manifest = args.next().ok_or("missing manifest")?;

    let vol = if dir {
        None
    } else {
        Some(Volume::open(FileDevice::open(&first).await?).await?)
    };

    let manifest = std::fs::read_to_string(manifest)?;
    let mut checked = 0;
    let mut failures = 0;

    for line in manifest.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.split('\t');
        let index: usize = fields.next().ok_or("bad manifest line")?.parse()?;
        let size: usize = fields.next().ok_or("bad manifest line")?.parse()?;
        let path = fields.next().ok_or("bad manifest line")?;

        let read = match &vol {
            Some(vol) => vol.read(path).await.map_err(|e| e.to_string()),
            None => std::fs::read(format!("{first}{path}")).map_err(|e| e.to_string()),
        };
        match read {
            Ok(data) if data == contents(index, size) => checked += 1,
            Ok(data) => {
                failures += 1;
                println!(
                    "MISMATCH {path}: {} bytes, expected {size}{}",
                    data.len(),
                    first_difference(&data, &contents(index, size))
                );
            }
            Err(e) => {
                failures += 1;
                println!("UNREADABLE {path}: {e}");
            }
        }
    }

    println!("{checked} files verified, {failures} failed");
    if failures > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn first_difference(got: &[u8], want: &[u8]) -> String {
    match got.iter().zip(want).position(|(a, b)| a != b) {
        Some(at) => format!(", first differing byte at {at}"),
        None => String::new(),
    }
}

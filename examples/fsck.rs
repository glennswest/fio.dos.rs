//! Check an image with `mkfs_dos::fsck`, the checker this crate's tests use.
//!
//! The test container's own judge beside dosfstools' `fsck.fat`: it is the
//! mkfs-dos commit `Cargo.toml` pins, read-only, and it prints what the
//! `fsck-fat` binary prints. Exit 0 clean, 4 problems found, 8 not checked.
//!
//! ```sh
//! cargo run --example fsck -- out.img
//! ```

use fio_dos::mkfs_dos::device::FileDevice;
use fio_dos::mkfs_dos::fsck::{check, FsckOptions, Severity};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let Some(image) = std::env::args().nth(1) else {
        eprintln!("usage: fsck <image>");
        return std::process::ExitCode::from(8);
    };
    let report = match FileDevice::open(&image).await {
        Ok(device) => check(&device, &FsckOptions::check_only()).await,
        Err(e) => {
            eprintln!("fsck: cannot open {image}: {e}");
            return std::process::ExitCode::from(8);
        }
    };
    let report = match report {
        Ok(report) => report,
        Err(e) => {
            eprintln!("fsck: {image}: {e}");
            return std::process::ExitCode::from(8);
        }
    };

    for problem in &report.problems {
        let mark = match problem.severity {
            Severity::Info => "note",
            Severity::Fixable => "fixable",
            Severity::Serious => "ERROR",
        };
        println!("pass {} [{mark}] {}", problem.pass, problem.message);
    }
    println!(
        "{image}: {} files, {} directories, {}/{} clusters used",
        report.files, report.directories, report.clusters_used, report.cluster_count
    );
    if report.is_clean() {
        println!("{image}: clean");
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::from(4)
    }
}

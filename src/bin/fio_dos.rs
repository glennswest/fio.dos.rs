//! `fio-dos` — read and write files inside a FAT image, without mounting it.
//!
//! ```sh
//! fio-dos esp.img mkdir /EFI/BOOT
//! fio-dos esp.img put bootx64.efi /EFI/BOOT/BOOTX64.EFI
//! fio-dos esp.img ls -l /EFI/BOOT
//! ```

use clap::{Parser, Subcommand};

use fio_dos::mkfs_dos::device::FileDevice;
use fio_dos::{Attributes, Volume};

#[derive(Parser, Debug)]
#[command(
    name = "fio-dos",
    about = "Read and write files inside a FAT12/FAT16/FAT32 image",
    version
)]
struct Args {
    /// The image or device to work on.
    image: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List a directory.
    Ls {
        /// Path inside the image.
        #[arg(default_value = "/")]
        path: String,
        /// Show size, attributes, short name and modification time.
        #[arg(short, long)]
        long: bool,
    },
    /// List everything, recursively.
    Tree {
        /// Path inside the image.
        #[arg(default_value = "/")]
        path: String,
    },
    /// Print a file.
    Cat {
        /// Path inside the image.
        path: String,
    },
    /// Copy a host file into the image.
    Put {
        /// File on this machine.
        source: String,
        /// Destination inside the image.
        dest: String,
    },
    /// Copy a file out of the image.
    Get {
        /// Path inside the image.
        source: String,
        /// Destination on this machine.
        dest: String,
    },
    /// Create a directory, and any parents it needs.
    Mkdir {
        /// Path inside the image.
        path: String,
    },
    /// Remove a file.
    Rm {
        /// Path inside the image.
        path: String,
        /// Remove a directory and everything under it.
        #[arg(short, long)]
        recursive: bool,
    },
    /// Remove an empty directory.
    Rmdir {
        /// Path inside the image.
        path: String,
    },
    /// Rename or move a file or directory.
    Mv {
        /// Path inside the image.
        from: String,
        /// New path inside the image.
        to: String,
    },
    /// Show the volume label, or set it.
    Label {
        /// The new label. Omit to print the current one.
        label: Option<String>,
    },
    /// Show what the volume is and how full it is.
    Info,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let device = FileDevice::open(&args.image)
        .await
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", args.image))?;
    let mut vol = Volume::open(device).await?;

    match args.command {
        Command::Ls { path, long } => {
            let mut entries = vol.read_dir(&path).await?;
            entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            for entry in entries {
                if long {
                    println!(
                        "{} {:>10} {:>12} {:>12}  {}",
                        attribute_string(entry.attributes),
                        entry.size,
                        entry.short_name,
                        entry.modified,
                        entry.name
                    );
                } else {
                    println!("{}{}", entry.name, if entry.is_dir { "/" } else { "" });
                }
            }
        }
        Command::Tree { path } => {
            tree(&vol, &path, 0).await?;
        }
        Command::Cat { path } => {
            let data = vol.read(&path).await?;
            use tokio::io::AsyncWriteExt;
            tokio::io::stdout().write_all(&data).await?;
        }
        Command::Put { source, dest } => {
            let data = tokio::fs::read(&source).await?;
            vol.write(&dest, &data).await?;
            vol.flush().await?;
            println!("{source} -> {dest} ({} bytes)", data.len());
        }
        Command::Get { source, dest } => {
            let data = vol.read(&source).await?;
            tokio::fs::write(&dest, &data).await?;
            println!("{source} -> {dest} ({} bytes)", data.len());
        }
        Command::Mkdir { path } => {
            vol.mkdir_all(&path).await?;
            vol.flush().await?;
        }
        Command::Rm { path, recursive } => {
            if recursive {
                vol.remove_all(&path).await?;
            } else {
                vol.unlink(&path).await?;
            }
            vol.flush().await?;
        }
        Command::Rmdir { path } => {
            vol.rmdir(&path).await?;
            vol.flush().await?;
        }
        Command::Mv { from, to } => {
            vol.rename(&from, &to).await?;
            vol.flush().await?;
        }
        Command::Label { label } => match label {
            Some(label) => {
                vol.set_label(&label).await?;
                vol.flush().await?;
            }
            None => println!("{}", vol.label().await?.unwrap_or_default()),
        },
        Command::Info => {
            let fs = vol.filesystem();
            let boot = fs.boot();
            println!("Type:          {}", fs.fat_type());
            println!("Label:         {}", vol.label().await?.unwrap_or_default());
            println!("Sector size:   {}", boot.bytes_per_sector);
            println!("Cluster size:  {}", fs.cluster_size());
            println!("Clusters:      {}", fs.cluster_count());
            println!("Free clusters: {}", vol.free_clusters());
            println!(
                "Free space:    {} MiB of {} MiB",
                vol.free_bytes() / (1024 * 1024),
                fs.cluster_count() as u64 * fs.cluster_size() as u64 / (1024 * 1024)
            );
        }
    }
    Ok(())
}

/// `drwha` — the five attribute bits, in the order `attrib` shows them.
fn attribute_string(attrs: Attributes) -> String {
    let flag = |bit: Attributes, c: char| if attrs.contains(bit) { c } else { '-' };
    [
        flag(Attributes::DIRECTORY, 'd'),
        flag(Attributes::READ_ONLY, 'r'),
        flag(Attributes::HIDDEN, 'h'),
        flag(Attributes::SYSTEM, 's'),
        flag(Attributes::ARCHIVE, 'a'),
    ]
    .iter()
    .collect()
}

/// Print a directory and everything under it.
async fn tree(
    vol: &Volume<FileDevice>,
    path: &str,
    depth: usize,
) -> anyhow::Result<()> {
    let mut entries = vol.read_dir(path).await?;
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    for entry in entries {
        println!(
            "{:indent$}{}{}",
            "",
            entry.name,
            if entry.is_dir {
                "/".to_string()
            } else {
                format!(" ({} bytes)", entry.size)
            },
            indent = depth * 2
        );
        if entry.is_dir {
            let child = if path.ends_with('/') {
                format!("{path}{}", entry.name)
            } else {
                format!("{path}/{}", entry.name)
            };
            Box::pin(tree(vol, &child, depth + 1)).await?;
        }
    }
    Ok(())
}

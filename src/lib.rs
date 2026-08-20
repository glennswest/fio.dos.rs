//! Async userspace file I/O into a **FAT12 / FAT16 / FAT32** filesystem.
//!
//! Read and write files inside a filesystem image or volume with no kernel, no
//! mount and no loop device — which means it works on a Mac, in a container
//! without privileges, and against storage that is not a block device at all.
//!
//! ```no_run
//! use fio_dos::Volume;
//! use fio_dos::mkfs_dos::device::FileDevice;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let device = FileDevice::open("esp.img").await?;
//! let mut vol = Volume::open(device).await?;
//!
//! vol.mkdir_all("/EFI/BOOT").await?;
//! vol.write("/EFI/BOOT/BOOTX64.EFI", b"MZ...").await?;
//!
//! let back = vol.read("/EFI/BOOT/BOOTX64.EFI").await?;
//! assert_eq!(&back[..2], b"MZ");
//!
//! // Nothing is durable until the FAT and the FSInfo sector are written back.
//! vol.flush().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # What it is for
//!
//! Building the contents of a FAT image, and inspecting one, from a program
//! rather than from a shell with root. An EFI system partition, an SD card
//! image, a firmware update volume. The companion
//! [`mkfs-dos`](https://github.com/glennswest/mkfs.dos.rs) crate creates the
//! filesystem and checks it; this one fills it in.
//!
//! # What it maintains
//!
//! Every write keeps the things `fsck.fat` checks in step — the FAT, in every
//! copy of it; the FSInfo free count and next-free hint on FAT32; each
//! directory entry's size, first cluster and timestamps. A filesystem written
//! through this crate passes `fsck.fat` afterwards, and passes it after a real
//! kernel has mounted and written to it as well.
//!
//! # Long names
//!
//! A name that fits the 8.3 field is stored in it — including a lowercase one,
//! which uses the two Windows NT case flags rather than a long name. Anything
//! else gets long-name slots and a generated `NAME~1` short name, with the
//! number climbing on a collision, exactly as Windows does it. See [`name`].
//!
//! # What FAT does not have
//!
//! No ownership, no permissions beyond the read-only bit, no symbolic links, no
//! hard links, no extended attributes, and one timestamp with two-second
//! resolution. Calls for those do not exist here rather than existing and
//! quietly doing nothing.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod alloc;
pub mod dir;
pub mod error;
pub mod name;
pub mod volume;

pub use error::{Error, Result};
pub use volume::{Attrs, Entry, Stat, Volume};

/// The attribute bits a FAT directory entry carries.
pub use mkfs_dos::structs::dirent::Attributes;

/// The filesystem crate underneath, re-exported so a caller needs one
/// dependency rather than two.
pub use mkfs_dos;

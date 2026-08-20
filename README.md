# fio-dos

Async userspace file I/O into a **FAT12 / FAT16 / FAT32** filesystem. No kernel,
no mount, no loop device — so it works on a Mac, in an unprivileged container,
and against storage that is not a block device at all.

```rust
use fio_dos::Volume;
use fio_dos::mkfs_dos::device::FileDevice;

let device = FileDevice::open("esp.img").await?;
let mut vol = Volume::open(device).await?;

vol.mkdir_all("/EFI/BOOT").await?;
vol.write("/EFI/BOOT/BOOTX64.EFI", &loader).await?;
vol.flush().await?;      // nothing is durable until this returns
```

Or from the command line:

```sh
fio-dos esp.img mkdir /EFI/BOOT
fio-dos esp.img put bootx64.efi /EFI/BOOT/BOOTX64.EFI
fio-dos esp.img tree /
fio-dos esp.img info
```

## What it is for

Building the contents of a FAT image, and inspecting one, from a program rather
than from a shell with root. An EFI system partition, an SD card image, a
firmware update volume. The companion
[`mkfs-dos`](https://github.com/glennswest/mkfs.dos.rs) crate creates the
filesystem and checks it; this one fills it in.

## What it maintains

Every write keeps in step the things `fsck.fat` checks:

- the FAT — **in every copy of it**, because the second FAT is not a backup a
  driver falls back to, it is a copy every driver assumes is current;
- the FSInfo free count and next-free hint on FAT32;
- each directory entry's size, first cluster and timestamps;
- `.` and `..` in every directory, including after a directory is moved.

A filesystem written through this crate passes `fsck.fat` — and passes it after
a real kernel has mounted it and written to it as well.

## Long names

A name that fits the 8.3 field is stored in it, including a lowercase one:
`readme.txt` uses the two Windows NT case flags rather than a long name, which
is what every other implementation does and what keeps a simple directory
simple. Anything else — `A Long File Name.txt`, `ReadMe.md`, `café ☕.txt`,
`two.dots.tar.gz`, `.config` — gets long-name slots and a generated `NAME~1`
short name, with the number climbing on a collision.

The reverse direction is checked too: fragments whose checksum does not match
the short entry they precede are *ignored*, and the short name is used. That is
the case where something without long-name support renamed the file, and
trusting the fragments would give it a name it no longer has.

## Verified

`./tests/verify-on-linux.sh` writes an image here with no kernel involved, ships
it to a Linux host, and has the kernel's own FAT driver read **every file back
byte for byte** — 310 files per image, sizes from 0 bytes to 300 KB, long names,
Unicode names, nested directories and a directory grown past a cluster. Then the
kernel writes to the image, and the result is read back here and compared again.

Both directions pass on FAT12, FAT16 and FAT32:

```
fill (no kernel) -> fsck.fat -> mount -> kernel reads all 310 files
  -> kernel writes 700 KiB and a long-named directory -> unmount -> fsck.fat
  -> read back here -> our fsck.fat
```

"We can read our own files" and "the filesystem is right" are different claims.
The kernel settles the second.

## What FAT does not have

No ownership, no permissions beyond the read-only bit, no symbolic links, no
hard links, no extended attributes, and one timestamp with two-second
resolution. Those calls do not exist here rather than existing and quietly doing
nothing. A file's size is a 32-bit byte count, so 4 GiB minus one byte is the
ceiling, and that is the format's rather than this implementation's.

## Durability

Nothing is durable until `flush()`. The FAT is cached in memory while a volume
is open — an allocator that read it an entry at a time would make one device
round trip per cluster of every file — and it goes out, to every FAT, along with
the FSInfo sector, when you flush. A volume dropped without a flush leaves the
file data on the device and no chain pointing at it.

The cache costs what the FAT costs: 4 bytes per cluster on FAT32, so a megabyte
for a 1 GiB volume.

## Licence

`MIT OR Apache-2.0`, at your option.

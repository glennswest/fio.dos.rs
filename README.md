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

## Using it

As a library, take it by git, pinned to a tag:

```toml
[dependencies]
fio-dos = { git = "https://github.com/glennswest/fio.dos.rs", tag = "v0.1.1", default-features = false }
```

`default-features = false` drops the `cli` feature (clap, anyhow and the
multi-threaded tokio runtime), which only the binary needs. `mkfs-dos` is
re-exported as `fio_dos::mkfs_dos`, so one dependency is enough. It comes in
by git, pinned to a commit, so building needs network access to GitHub. Crates.io does
not have either crate.

The binary:

```sh
cargo install --git https://github.com/glennswest/fio.dos.rs --tag v0.1.1
```

Install from `v0.1.1` or later, not from `v0.1.0`. That tag's `Cargo.toml`
still carries a `[patch]` pointing at a sibling `../mkfs.dos.rs` checkout, and
`cargo install` applies it and fails (#4). A library dependency on `v0.1.0`
still resolves, because cargo ignores `[patch]` in a dependency.

There is no container image, service, port or configuration file. It is a
library and a command-line tool, and nothing else.

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

## The API

`Volume::open(device)` reads the boot sector and the whole FAT. It takes
anything that implements `mkfs_dos::device::BlockDevice`. `FileDevice` (an
image file or a device node) is the usual choice. From there, every path is
absolute inside the volume and uses `/`:

| Call | Does |
|---|---|
| `read(path)` | the whole file, as a `Vec<u8>` |
| `write(path, data)` / `write_with(path, data, &Attrs)` | create or replace a whole file |
| `append(path, data)` | add to the end of a file (it reads the file and writes it back whole) |
| `mkdir` / `mkdir_all` | one directory, or a path with its parents |
| `unlink` / `rmdir` / `remove_all` | a file, an empty directory, or a tree |
| `rename(from, to)` | move or rename, across directories, with `..` kept right |
| `stat` / `exists` / `read_dir` | look without changing anything |
| `set_attributes` / `set_modified` | the attribute bits and the timestamp |
| `label` / `set_label` | the volume label |
| `free_clusters` / `free_bytes` / `filesystem` | space and geometry |
| `set_time(secs)` | fix the clock, so an image built twice is identical |
| `flush()` | write the FAT (every copy) and FSInfo back |

`Attrs::read_only()`, `Attrs::system()` (hidden + system) and
`.modified_at(secs)` give a file its attributes as it is created.

`read_dir` returns `Entry` values, and `stat` returns a `Stat` (the same type).
Each one carries the long name, the short name, `is_dir`, the size, the
`Attributes` bits, the modified and created times in Unix seconds, and the
first cluster. Every call returns `fio_dos::Result`, whose error is
`fio_dos::Error`. `Volume`, `Attrs`, `Entry`, `Stat`, `Attributes`, `Error` and
`Result` are all re-exported at the crate root.

Writes replace the whole file. There are no partial writes at an offset and no
streaming yet, so a file is held in memory in full on the way in and out.

## The command line

```
fio-dos <image> <command>
```

| Command | Does |
|---|---|
| `ls [path] [-l]` | list a directory (default `/`). `-l` adds attributes (`drhsa`), size, short name and modification time |
| `tree [path]` | list everything under a directory, with sizes |
| `cat <path>` | write a file to stdout |
| `put <host-file> <path>` | copy a file into the image |
| `get <path> <host-file>` | copy a file out of the image |
| `mkdir <path>` | create a directory and any parents it needs |
| `rm <path> [-r]` | remove a file, or with `-r` a directory and everything in it |
| `rmdir <path>` | remove an empty directory |
| `mv <from> <to>` | rename or move |
| `label [new]` | print the volume label, or set it |
| `info` | FAT type, label, sector and cluster size, cluster count, free space |

Each command that changes the image flushes before it exits. The image must
already hold a filesystem. Make one with
[`mkfs-dos`](https://github.com/glennswest/mkfs.dos.rs).

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

The kernel check writes an image here with no kernel involved, then has the
kernel's own FAT driver read **every file back byte for byte** — 310 files per
image, sizes from 0 bytes to 300 KB, long names, Unicode names, nested
directories and a directory grown past a cluster. Then the kernel writes to the
image, and the result is read back here and compared again.

```
fill (no kernel) -> fsck.fat, fsck-fat -> loop-mount -> kernel reads all 310 files
  -> kernel writes 700 KiB and a long-named directory -> unmount
  -> fsck.fat, fsck-fat -> read back here, every file and the kernel's
```

It runs as this crate's test container (`test/`, per the stormcos test
standard), on a test machine, in a privileged pod:

```sh
stormcentral test run fio.dos.rs short    # FAT12, FAT16, FAT32, both directions
stormcentral test run fio.dos.rs medium   # and a second round on each: we rename,
                                          # remove and add after the kernel wrote,
                                          # and the kernel mounts it again
stormcentral test run fio.dos.rs long     # and a 2 GiB FAT32 with a 200 MiB file
```

The judges are the node's kernel, `fsck.fat -n` from dosfstools, and
`mkfs-dos`'s own checker (`examples/fsck`, staged as `fsck-fat`). A node without a loop device or without vfat
reports the kernel checks as skip and the run as "could not run" (exit 2),
never as a pass.

"We can read our own files" and "the filesystem is right" are different claims.
The kernel settles the second.

## Testing

`cargo test` (what `sc-build` runs, after `cargo build`) runs the round-trip suite (`tests/roundtrip.rs`) on FAT12, FAT16
and FAT32 images it creates in temporary files. Every test ends with a check by
`mkfs_dos::fsck`, the Rust reimplementation of `fsck.fat` in the companion
crate. It needs no root and no kernel.

`test/` is the kernel check described under *Verified*: `test/build.sh`
stages static `fio-dos`, the `fill`, `verify` and `fsck` examples (the last over the pinned
mkfs-dos checker, staged as `fsck-fat`), `test/Containerfile` packages them on fedora-minimal with dosfstools
and util-linux, and `/test <suite>` (`test/test.sh`) prints one JSON line per
check. It needs root only inside its own pod, never a login.

`tests/verify-on-linux.sh user@host` is the same check by hand, over ssh. It
needs a host where that login can loop-mount (root, in practice) with `fsck.fat`
and `python3`, builds where it runs, and expects the `mkfs.dos.rs` checkout next
to this one for `fsck-fat`.

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

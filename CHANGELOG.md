# Changelog

## [Unreleased]

### 2026-10-06
- **test:** The kernel check runs as a test container (`test/`, the stormcos
  test standard), with no root login and no build where it runs (#1).
  `/test short|medium|long` writes FAT12/FAT16/FAT32 images with no kernel,
  loop-mounts them in a privileged pod, has the kernel read every file and
  write its own, and reads that back here; `fsck.fat -n` and `fsck-fat` judge
  before and after. `medium` adds a second round in which we change what the
  kernel wrote and the kernel mounts it again; `long` adds a 2 GiB FAT32 with
  a 200 MiB file. No loop device or no vfat: skip, and exit 2.
- **feat:** `examples/verify --dir <mountpoint> <manifest>` reads the files
  from a directory (the kernel's mount) instead of the image; `examples/fill`
  takes an optional fourth argument, the MiB of an extra `/big.bin`; new
  `examples/fsck <image>` checks an image read-only with the pinned
  `mkfs_dos::fsck` and prints what mkfs-dos's `fsck-fat` prints.
- **chore:** `tests/verify-on-linux.sh` takes the host as a required argument;
  it no longer defaults to a root login.
- **docs:** README *Verified* and *Testing*, and CLAUDE.md, describe the test
  container.

## [v0.1.1] — 2026-10-06

A packaging fix. The library and the binary behave exactly as in v0.1.0.

### Fixed
- `cargo install --git https://github.com/glennswest/fio.dos.rs --tag v0.1.1`
  works. The `v0.1.0` tag's `Cargo.toml` carried a `[patch]` to a sibling
  `../mkfs.dos.rs` checkout, which `cargo install` applies and cannot resolve
  (#4). `mkfs-dos` is now taken by git pinned to commit `a55c537` (the
  mkfs.dos.rs `v0.1.0` tag's commit) with no `[patch]`, which also fixes
  `sc-build` (#2, decided in #3).

### Documentation
- README refreshed from the code: taking the crate by git tag, installing the
  binary from `v0.1.1`, the full `Volume` API with `Entry`, `Stat` and the
  crate-root re-exports, the CLI command reference, and what `cargo test` and
  `verify-on-linux.sh` each need. It states that no container, service, port or
  configuration file exists, that `append` rewrites the whole file, and that the
  kernel check last passed on 2026-08-19 and is not run by `sc-build` (#1).
- The attribute-string comment in the binary is corrected (`drhsa`).

## [v0.1.0] — 2026-08-19

First working release. Files written here are read back byte for byte by a real
Linux kernel, and files the kernel writes are read back here.

### Added
- `alloc` — cluster allocation over a FAT cached in memory, with the free count
  and the next-free hint kept in step and flushed to every FAT.
- `name` — the 8.3 field, the Windows NT case flags, generated `NAME~1` short
  names with collision numbering, and long-name fragments in both directions
  (including the name that exactly fills its fragments and so has no
  terminator).
- `dir` — directory slots: assembling a name from its set of them, finding a
  free run, writing and erasing a whole set.
- `volume` — the public API: `read`, `write`, `write_with`, `append`, `mkdir`,
  `mkdir_all`, `unlink`, `rmdir`, `remove_all`, `rename`, `stat`, `read_dir`,
  `exists`, `set_attributes`, `set_modified`, `label`, `set_label`, `flush`.
- `fio-dos` binary — `ls`, `tree`, `cat`, `put`, `get`, `mkdir`, `rm`, `rmdir`,
  `mv`, `label`, `info`.
- `examples/fill.rs` and `examples/verify.rs` — write a known set of files, and
  check an image against it.

### Changed
- Take `mkfs-dos` by git tag rather than by path, with a `[patch]` for local
  development. A path dependency inside a git dependency does not resolve for a
  consumer. Committed on 2026-08-20 and included in the `v0.1.0` tag.

### Testing
- Round-trip suite across FAT12, FAT16 and FAT32, every test ending with a
  `fsck.fat` check of the filesystem it produced: awkward file sizes either side
  of a cluster, long and Unicode names, directory growth, the fixed root filling
  up, rename across directories with `..` following, cluster reuse on rewrite,
  and a full volume refusing rather than corrupting.
- `tests/verify-on-linux.sh` — 310 files per image read back by the kernel's own
  FAT driver, then written to by the kernel and read back here. All three widths
  pass in both directions.

## [Unreleased]
<!-- New unreleased changes go here -->

### 2026-10-06
- **docs:** Work plan records #4 done: `v0.1.1` installs with `cargo install --tag`.

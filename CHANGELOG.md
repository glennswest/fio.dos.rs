# Changelog

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

### 2026-09-27
- **docs:** Refresh the README from the code. It now covers taking the crate by
  git tag, installing the binary, the full `Volume` API, the CLI command
  reference, and what `cargo test` and `verify-on-linux.sh` each need. It also
  states that no container, service, port or configuration file exists. The
  `build:` entry that sat under Unreleased moved into v0.1.0, since the tag
  includes it. The attribute-string comment in the binary is corrected
  (`drhsa`). Filed #1: the kernel verification needs root ssh, so it cannot run
  under `sc-build` or the test standard.

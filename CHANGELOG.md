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

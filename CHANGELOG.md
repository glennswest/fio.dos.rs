# Changelog

## [Unreleased]

### 2026-08-19
- **feat:** `alloc` — cluster allocation over a cached FAT, with the free count
  and the next-free hint kept in step and flushed to every FAT
- **feat:** `name` — 8.3 names, the NT case flags, generated `NAME~1` short
  names, and long-name fragments in both directions
- **feat:** `dir` — directory slots: parsing a name from its set of them,
  finding a run, writing and erasing a set
- **feat:** `volume` — the public API: read, write, append, mkdir, mkdir_all,
  unlink, rmdir, remove_all, rename, stat, read_dir, attributes, timestamps,
  volume label
- **test:** round-trip suite across FAT12, FAT16 and FAT32, every test ending
  with a `fsck.fat` check of the filesystem it produced

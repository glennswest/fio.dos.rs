# CLAUDE.md — fio-dos

Async userspace file I/O into a FAT12/FAT16/FAT32 filesystem. No kernel, no
mount, no loop device.

- **Crate:** `fio-dos` (lib `fio_dos`)
- **Version:** 0.1.0 — `Cargo.toml` is the single version location
- **Licence:** MIT OR Apache-2.0
- **Repo:** https://github.com/glennswest/fio.dos.rs
- **Sibling:** `../mkfs.dos.rs` provides the on-disk format, the `BlockDevice`
  seam, the read layer and `fsck`. The two are developed together.

## Dependency form

`mkfs-dos` is taken **by git, pinned to a tag**, with a `[patch]` section
pointing at `../mkfs.dos.rs` for local development:

```toml
mkfs-dos = { git = "https://github.com/glennswest/mkfs.dos.rs", tag = "v0.1.0", default-features = false }

[patch."https://github.com/glennswest/mkfs.dos.rs"]
mkfs-dos = { path = "../mkfs.dos.rs" }
```

It must stay that way. A path dependency inside a git dependency only resolves
when the path is inside the same repository, so a path form here makes this
crate unusable by a consumer that takes it by git — the same trap as
fio.ext4.rs#1. The patch applies only to the crate being built, so a consumer
never sees it. Verified by building a throwaway crate that takes this one by
git; the local checkout building is not evidence, since the patch hides the
problem.

## Shape

| Module | What it owns |
|---|---|
| `alloc` | cluster allocation over a cached FAT, and the counters every allocation moves |
| `name` | 8.3 names, the NT case flags, generated short names, long-name fragments |
| `dir` | directory slots: parsing a name from its set, finding a run, writing and erasing |
| `volume` | the public API: read, write, mkdir, unlink, rename, stat, list |

## Rules

1. **Every mutation goes through `alloc`.** A FAT entry changed without the free
   count is a filesystem that fails `fsck.fat`.
2. **A name is a set of slots, not a slot.** Deleting or renaming has to take
   the long-name fragments with it.
3. **Reads follow the cached FAT, never the device's.** The device's copy is
   behind until the next flush; a directory grown a moment ago is a chain the
   device does not know about yet.
4. **Reserve the directory slots before allocating data.** Allocating first and
   failing to place the name — a full FAT16 root, most often — strands the
   clusters, which is precisely the "lost clusters" `fsck.fat` reports.
5. **Every test ends by checking the filesystem.** A writer that leaves
   `fsck.fat` complaining has damaged the filesystem, not written a file.
6. **The kernel is the judge.** `tests/verify-on-linux.sh` is the test that
   counts, and it runs in both directions.

## Work plan

- [x] `alloc`, `name`, `dir`, `volume`, and the round-trip suite
- [x] `fio-dos` binary — ls, tree, cat, put, get, mkdir, rm, rmdir, mv, label, info
- [x] `tests/verify-on-linux.sh` — the kernel reads every file we wrote, then
      writes, and we read that back. All three widths pass in both directions.
- [ ] Partial writes at an offset, rather than whole-file replace
- [ ] Streaming reads and writes, so a file larger than memory can be handled
- [ ] Unpack a tar archive straight into a volume, as `fio-ext4` does — FAT has
      no ownership or symlinks, so it is a smaller job here and worth doing for
      the same reason: building an image without a kernel
- [ ] Free-space defragmentation for a volume rewritten many times

# CLAUDE.md — fio-dos

Async userspace file I/O into a FAT12/FAT16/FAT32 filesystem. No kernel, no
mount, no loop device.

- **Crate:** `fio-dos` (lib `fio_dos`)
- **Version:** 0.1.1 — `Cargo.toml` is the version location (`Cargo.lock` follows it)
- **Licence:** MIT OR Apache-2.0
- **Repo:** https://github.com/glennswest/fio.dos.rs
- **Sibling:** `../mkfs.dos.rs` provides the on-disk format, the `BlockDevice`
  seam, the read layer and `fsck`. It is taken by pinned commit, not by the
  sibling checkout (see below).

## Dependency form

`mkfs-dos` is taken **by git, pinned to a commit**. No `[patch]` points at a
sibling checkout. This was decided in #3.

```toml
mkfs-dos = { git = "https://github.com/glennswest/mkfs.dos.rs", rev = "a55c537d890c2007a7e12d48fc7ad50aa4ef57d5", default-features = false }
```

`a55c537` is the `v0.1.0` tag's commit. Bump the `rev` deliberately when
mkfs.dos.rs changes: push the mkfs.dos.rs change first, then move the `rev`
here in its own commit. `sc-build` builds one pushed commit with nothing beside
it, and that stays the rule, so a `[patch]` path to `../mkfs.dos.rs` breaks
every build (#2).

It must not become a path dependency either. A path dependency inside a git
dependency only resolves when the path is inside the same repository, so a
path form here makes this crate unusable by a consumer that takes it by git.
That is the same trap as fio.ext4.rs#1.

## How it ships

A library crate plus the `fio-dos` binary (behind the default `cli` feature).
Consumers take it by git tag. There is no crates.io release, container image,
service, port or configuration file. It is not a stormcentral component with a
golden. Build and test with `sc-build` (`cargo build && cargo test`). The
round-trip suite needs no root. The kernel check is the test container in
`test/` (#1): `stormcentral test run fio.dos.rs short|medium|long --url
http://stormcentral.g8.lo` builds it on the build box and runs it as a
privileged Job on a test machine. `tests/verify-on-linux.sh user@host` is the
same check by hand, and needs root on that host.

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
6. **The kernel is the judge.** The test container in `test/` is the test
   that counts, and it runs in both directions:
   `stormcentral test run fio.dos.rs short` (#1).

## Work plan

- [x] `alloc`, `name`, `dir`, `volume`, and the round-trip suite
- [x] `fio-dos` binary — ls, tree, cat, put, get, mkdir, rm, rmdir, mv, label, info
- [x] `tests/verify-on-linux.sh` — the kernel reads every file we wrote, then
      writes, and we read that back. All three widths passed in both directions
      when v0.1.0 was cut (2026-08-19), the last recorded run (#1).
- [ ] Partial writes at an offset, rather than whole-file replace
- [ ] Streaming reads and writes, so a file larger than memory can be handled
- [ ] Unpack a tar archive straight into a volume, as `fio-ext4` does — FAT has
      no ownership or symlinks, so it is a smaller job here and worth doing for
      the same reason: building an image without a kernel
- [ ] Free-space defragmentation for a volume rewritten many times
- [x] Docs refreshed from the code (2026-09-27)
- [x] #3/#2 — mkfs-dos pinned to commit a55c537 with no `[patch]`. sc-build
      passes (2026-09-27).
- [ ] #1 — kernel verification as a `test/` container, with no root ssh.
      IN PROGRESS (2026-10-06). `stormcentral test run fio.dos.rs <suite>`,
      image `test-fio-dos-rs-<suite>`. `test/build.sh` stages static musl
      `fio-dos`, examples `fill`/`verify`/`fsck` (the pinned checker);
      `test/Containerfile` is fedora-minimal + dosfstools + util-linux;
      `/test` (`test/test.sh`) runs what `verify-on-linux.sh` ran, inside the
      pod: fill, fsck.fat, loop-mount, the kernel reads every file
      (`verify --dir`), the kernel writes, fsck.fat, we read it back. JSON
      lines out. `requires.toml`: privileged. No loop device or no vfat:
      skip, never pass. short = the three widths; medium adds a second round
      (we rename/remove/add after the kernel, the kernel mounts again);
      long adds a 2 GiB FAT32 with a 200 MiB file.
      `verify-on-linux.sh` stays for manual use but takes the host as a
      required argument, no root default.
      State (2026-10-07): done and pushed (4da20ab). sc-build passes;
      test/build.sh stages static binaries on the build box, and the
      unprivileged dry run takes the skip path with exit 2. Run 99e14c33ed
      (short, pvetest2, 7074bda): every kernel check passed, but the runner
      recorded no results, because rustkube-node#136 mangles lines with
      spaces (fixed in a2d2e16, so `·` is used). Run 90cf1bd754 (a2d2e16):
      results parsed, but it skipped because `losetup -f` says
      "/dev/loop0 (lost)" when the pod has no node for it (fixed in 4da20ab).
      Every run since errors on the test-image build: dev.g8.lo is retired
      (stormcentral#521) and the runner still sends to it, stormcentral#526.
      Proposed --after stormcentral#526. Next: `stormcentral test run
      fio.dos.rs short|medium --tag <machine>` at 4da20ab or later; on a
      recorded pass, close #1.
- [x] #4 — `v0.1.1` cut from `main` (2026-10-06). sc-build of
      `cargo install --git … --tag v0.1.1` installs and runs `fio-dos 0.1.1`;
      the same against `v0.1.0` still fails on the `[patch]`, as expected.
      Nothing in progress. Next: #1, then the open items above.
- [x] Docs re-checked against the code; README install line and #4 (2026-09-27)

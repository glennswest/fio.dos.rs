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
round-trip suite needs no root. The kernel check is the throwaway VM the owner ruled for on
2026-10-06 (#1): `tests/vm/build-image.sh` makes a kernel+busybox UEFI disk
(built by `sc-build`, carried off with `SC_BUILD_OUT`) whose init runs fill →
`fsck.fat -n` → loop-mount → the kernel reads and writes → `fsck.fat -n` → we
read back and change it → the kernel mounts again, and prints `VERIFY PASS` /
`VERIFY FAIL <why>` on serial; `stormcentral testhost boot nanatest1` boots it
(README "Verified"). The `test/` container (`stormcentral test run fio.dos.rs
short|medium|long`) runs the same sequence as a privileged Job; it is kept
as the pod form (#6 answered, 2026-10-13). `tests/verify-on-linux.sh user@host` is the same check by hand, and needs root
on that host. The build does not yet fail on warnings (#5).

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
6. **The kernel is the judge.** The kernel check is the test that counts, and
   it runs in both directions: the `tests/vm/` image, booted with
   `stormcentral testhost boot nanatest1`, as the owner ruled on #1.

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
- [x] #1 — kernel verification with no root ssh. First as a `test/`
      container (2026-10-06), then as the owner ruled, a testhost boot VM. `stormcentral test run fio.dos.rs <suite>`,
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
      Runs after that errored on the test-image build (dev.g8.lo retired,
      stormcentral#521); stormcentral#526 is closed, so test images now build
      on their own build VM. A pod-form re-run
      (`stormcentral test run fio.dos.rs short|medium --tag <machine>` at
      4da20ab or later) waits only on a test machine.
      NOT DONE BY THIS: the owner ruled on 2026-10-06 (#1, before the pod
      work was pushed) that the kernel check runs in a throwaway VM via
      `stormcentral testhost boot nanatest1 --image F --expect "VERIFY
      PASS" --fail "VERIFY FAIL" --project fio.dos.rs`, not in a privileged
      pod. Still to do: a kernel+busybox image (SC_BUILD_OUT) carrying
      fio-dos/fill/verify/fsck-fat and dosfstools, whose init runs the
      test.sh sequence and prints VERIFY PASS/FAIL on serial; the boot
      machine must list fio.dos.rs (stormcentral#478). #1 closes on a
      VERIFY PASS from that, then `test/` is retired or kept as the pod
      form if the owner wants it.
      DONE (2026-10-13): the VM form, after fio.ext4.rs's `tests/vm/`.
      `tests/vm/build-image.sh OUT.img` makes a UEFI disk (Shell -> the
      build VM's kernel + a busybox initramfs with fsck.fat, the
      loop/fat/vfat/nls modules, fio-dos and the fill/verify/fsck
      examples); `tests/vm/init.sh` (PID 1) runs test.sh's long sequence,
      both rounds, and prints VERIFY PASS/FAIL. Verified: image built by
      sc-build from 6a74877 (kernel 7.2.8-200.fc44, mkfs-dos a55c537);
      `testhost boot nanatest1` run 7e583db0d4 printed VERIFY PASS after
      11.5 s, FAT12/FAT16/FAT32/2 GiB FAT32 all clean; again from fca8020
      (dosfstools 4.2), run eed50112ef, 13.7 s. sc-build passes at fca8020. `test/` is left as
      it is; the owner answered #6 (2026-10-13): kept as the pod form.
- [x] #4 — `v0.1.1` cut from `main` (2026-10-06). sc-build of
      `cargo install --git … --tag v0.1.1` installs and runs `fio-dos 0.1.1`;
      the same against `v0.1.0` still fails on the `[patch]`, as expected.
      Nothing in progress. Next: #5, then the open items above.
- [x] Docs re-checked against the code; README install line and #4 (2026-09-27)
- [x] Docs refreshed from the code since 2026-10-02 (2026-10-10): CLI long
      flags, `/test` exit codes and `$STORM_SUITE`, the examples' arguments,
      the owner's VM ruling on #1, stormcentral#526 as the blocker.
- [x] Docs refreshed (2026-11-07): no code changed since 2026-10-16; the
      stale #6 and stormcentral#526 statements corrected (#9, #11).
- [x] #15 — the 3 minor review findings (2026-11-21). 9aef8ceed235: `validate`
      counts UTF-16 units (`MAX_LONG_UNITS` 255). b1f840bab51e: `free_run`
      grows the directory until a run fits (the run may span clusters);
      the fixed root still says `RootDirectoryFull`. f1e178d2225e:
      `test/test.sh` `attach`/`kmount` leave the device in `$LOOP_DEV` and
      run in the parent shell, `detach` untracks, INT/TERM exit through the
      trap; `tests/test_sh.rs` runs `tests/sh/test-sh-cleanup.sh` with
      losetup/mount mocked. DONE (2026-11-21, c617f19 + f37a77d, the
      fixed-root test reading the root size the formatter chose — #18):
      sc-build job c0b7b1a401 passed — 38 unit, 9 fault, 10 path, 20
      round-trip and the mocked test.sh trap test, every one ending in fsck.
      That build includes 6a6c8f2, so #14's tests passed in it too.
- [ ] #5 — make warnings fail the build (`[lints.rust]`, clippy -D warnings).
- [x] #12 — the 11 review findings in `src/volume.rs` (2026-11-20). Plan:
      paths: `..` navigates on reads (cluster 0 / the FAT32 root cluster in a
      `..` entry is the root) and is refused in every mutating path; dot
      entries are never a mutation target. One matching helper for lookup and
      collisions that sees the long name and the short alias; generated
      aliases avoid other entries' long names. `read` refuses a size its
      chain cannot hold. Caller attributes limited to RO/HIDDEN/SYSTEM/ARCHIVE.
      Rename refuses a move into the source or a descendant by walking the
      destination's `..` chain. Failure safety: an undo journal in
      `Allocator` (begin/commit/rollback); allocate before, free only after
      the directory write lands; a failed directory write is read back —
      unchanged → roll back, landed → finish, anything else → the volume is
      poisoned (mutations and flush refused). Tests with a fault-injecting
      device in `tests/faults.rs`.
      DONE (2026-11-20, 0ed8926): sc-build job 3a305a6f2c passed — 29 unit,
      9 fault, 6 path and 20 round-trip tests, every one ending in fsck.
      #13 (alloc flush retry) and #14 (dir terminator) are still open.
- [ ] #13 — the 4 review findings in `src/alloc.rs` (2026-11-20). Plan:
      `allocate` drops the `found.contains` scan (the bounded wrap visits each
      cluster once); `flush` removes a sector from the dirty set only once
      every FAT copy of it is written, and clears `fsinfo_dirty` only once the
      FSInfo write succeeds, so an error or a cancelled flush leaves the rest
      for the retry. Unit tests with a failing and a hanging device.
- [ ] #14 — the 2 review findings in `src/dir.rs` (2026-11-20). Plan:
      f81439860dd8: `find_free_run` treats every slot after the first zero
      as free (bytes past the end marker are never read, so need not be
      zero); `write_set` and `write_short` clear the slot after the written
      run when the run consumed the end marker, so stale bytes past it stay
      unreachable — files and the volume label alike. 43bd3bc2b137:
      `build_set` refuses an exact 8.3 name another entry already answers to
      (alias or long name) with `Exists`, instead of quietly generating a
      second alias (`lookup` already resolves aliases since #12). Unit tests
      in `dir.rs`, a volume test with stale bytes past the root's end marker
      in `tests/paths.rs`, fsck at the end.
      State (2026-11-21): fixes in 2e39e15, test corrected in 6a6c8f2.
      sc-build of 2e39e15 ran: 37 unit (the new `dir` ones too), 9 fault
      tests passed; the new volume test failed on the test's own setup
      (`sub` made after the zeroed entry, so hidden by it) — fixed in
      6a6c8f2. sc-build of 6a6c8f2 cannot start: empty job id, then 404
      "no such build job", three times — stormcentral#1638 (/home full,
      database in I/O error). Parked behind it; next: sc-build 6a6c8f2,
      then close #14.

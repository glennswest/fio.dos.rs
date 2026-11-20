# Changelog

## [Unreleased]

### 2026-11-20
- **fix:** The review findings on `src/volume.rs` (#12):
  - `..` under the root (cluster 0, or the FAT32 root's own cluster) resolves
    to the root on every width, in one place used by every path walk;
    `read_dir("/a/..")` lists the root and `/a/../file` finds it, where before
    it was an empty directory and creating through it panicked (f16592d1a3b5).
  - `..` is refused in every path that is changed, and `.`/`..` entries are
    never a target: `rmdir("/A/..")` and `rename("/a/b/..", …)` no longer erase
    or move a directory's bookkeeping (6fa79638bce5, 3ff6b13b3ec5).
  - Rename refuses to move a directory into itself or a descendant by walking
    the destination's `..` entries on disk, not by comparing path text, so
    case, repeated `/` and `.` no longer get past it (44347c5c1704,
    7086d9cf795f, e5fe266defb1).
  - A file answers to its 8.3 alias for lookup, replace and collision, and a
    generated alias avoids other entries' long names (bbc3584c0279).
  - `read` returns a corruption error for a nonempty file with no cluster or a
    chain shorter than its size, instead of a short read (c68efc8833ae,
    bdefde03a16e).
  - `write_with` and `set_attributes` refuse the directory and volume-label
    bits and anything outside read-only/hidden/system/archive
    (`Error::InvalidAttributes`); a directory keeps its kind (d7a26c69492e).
  - Failure safety (0d6b59b69c3d): an undo journal in the allocator; clusters
    are freed only after the directory write that drops them lands; a failed
    directory write is read back and the change rolled back, finished, or —
    when neither can be established — the volume poisoned
    (`Error::Poisoned`, `Volume::is_poisoned`); a rename that fails half way
    undoes its first half. Tested with a fault-injecting device
    (`tests/faults.rs`); the path findings in `tests/paths.rs`.
- **feat:** `Volume::is_poisoned`, `Error::Poisoned`, `Error::InvalidAttributes`;
  `exists("/")` is now true and `remove_all("/")` is refused rather than doing
  nothing.

### 2026-11-14
- **docs:** Re-checked README, docs/ and CLAUDE.md against the code. Nothing
  changed since 2026-11-07 except a research note (`docs/research/2026-11-14.md`),
  so no statement needed correcting. #9 and #11 were fixed on 2026-11-07.

### 2026-11-07
- **docs:** Refreshed from the code. No code has changed since 2026-10-16
  (only a research note). Corrected the statements that had gone stale:
  `test/` is kept as the pod form (the owner answered #6 on 2026-10-13), and
  stormcentral#526 is closed, so it no longer blocks a pod-form re-run (README,
  CLAUDE.md; #9, #11). The 2026-10-10 and 2026-10-13 entries below are history
  and are left as written.

### 2026-10-13
- **test:** The kernel check runs in a throwaway VM, as the owner ruled on #1:
  `tests/vm/build-image.sh` makes a UEFI disk whose Shell starts the build
  VM's kernel with a busybox initramfs (fsck.fat, the loop/fat/vfat/nls
  modules, `fio-dos` and the `fill`/`verify`/`fsck` examples), and
  `tests/vm/init.sh` runs as PID 1: FAT12, FAT16, FAT32 and a 2 GiB FAT32
  with a 200 MiB file, written with no kernel, judged by `fsck.fat -n`,
  `fsck-fat` and the kernel's loop mount in both directions and over a second
  round, then `VERIFY PASS` / `VERIFY FAIL <why>` on serial. Built with
  `sc-build` (`SC_BUILD_OUT`), booted with `stormcentral testhost boot
  nanatest1`; runs `7e583db0d4` and `eed50112ef` passed. `test/` is unchanged pending #6.

### 2026-10-10
- **docs:** Refreshed from the code. README: the CLI's `--long` and
  `--recursive` spellings; `/test`'s exit codes (0 pass, 1 fail, 2 could not
  run), its `$STORM_SUITE` default and cleanup; `build.sh` only stages and the
  runner builds the image; the `fill`, `verify` and `fsck` examples' arguments;
  no run recorded as passed yet, blocked on stormcentral#526. README and
  CLAUDE.md now record the owner's ruling on #1 (2026-10-06): the kernel check
  belongs in a throwaway VM booted with `stormcentral testhost boot`, which is
  not built yet; the privileged pod in `test/` is what exists today. Filed #5:
  the build does not fail on warnings.

### 2026-10-07
- **fix(test):** `test/build.sh` only stages binaries in `test/.stage/`; the
  runner builds the image from `test/Containerfile` with no container runtime
  (stormcentral#121).
- **fix(test):** `losetup -f` names a loop device the pod has no node for as
  `/dev/loopN (lost)`; `/test` strips that and makes the node with `mknod`
  (privileged pods may), instead of reporting no loop device.

### 2026-10-06
- **test:** The kernel check runs as a test container (`test/`, the stormcos
  test standard), with no root login and no build where it runs (#1).
  `/test short|medium|long` writes FAT12/FAT16/FAT32 images with no kernel,
  loop-mounts them in a privileged pod, has the kernel read every file and
  write its own, and reads that back here; `fsck.fat -n` and `fsck-fat` judge
  before and after. `medium` adds a second round in which we change what the
  kernel wrote and the kernel mounts it again; `long` adds a 2 GiB FAT32 with
  a 200 MiB file. No loop device or no vfat: skip, and exit 2. The JSON lines
  carry no spaces (`·` in strings), since the kubelet log drops the first
  three words of a line with three or more (rustkube-node#136).
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

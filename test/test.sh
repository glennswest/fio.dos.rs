#!/bin/bash
# /test — fio-dos's test, per stormcentral docs/test-standard.md.
#
#   /test short|medium|long
#
# CLAUDE.md rule 6: the kernel is the judge. Inside this pod, with no kernel
# involved, `fill` writes an image; then the node's own FAT driver mounts it
# over a loop device and reads every file back, byte for byte, and writes to
# it; then the image is read back here with no kernel again. Both fsck.fat
# (dosfstools) and our own fsck-fat check it before and after. This is what
# tests/verify-on-linux.sh does over ssh, without the ssh or the root login.
#
#   short   FAT12 8 MiB, FAT16 64 MiB, FAT32 512 MiB, in both directions
#   medium  short, then a second round on each: we rename, remove and add
#           after the kernel wrote, and the kernel mounts it again
#   long    medium, plus a 2 GiB FAT32 holding a 200 MiB file
#
# Images are sparse files in the pod's own temp directory, deleted on exit,
# and every loop device and mount made here is undone on exit too. A node
# without a loop driver or without vfat reports the kernel checks as skip,
# never pass, and the run exits 2 (could not run).
#
# stdout is one JSON object per line per check and a final summary; the
# commands' own output goes to stderr. The lines carry no spaces (`·` in the
# strings): the node's kubelet log drops the first three words of a line with
# three or more spaces (rustkube-node#136), and the runner would lose them.

set -uo pipefail

SUITE="${1:-${STORM_SUITE:-short}}"
case "$SUITE" in
    short|medium|long) ;;
    *) echo "usage: /test short|medium|long" >&2; exit 2 ;;
esac

BIN="${FIO_DOS_BIN:-/usr/local/bin}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/fio-dos-test.XXXXXX")"
PASS=0 FAIL=0 SKIP=0
KERNEL_RAN=0
MOUNTS=() LOOPS=()

cleanup() {
    for m in "${MOUNTS[@]}"; do
        mountpoint -q "$m" 2>/dev/null && umount "$m" 2>/dev/null
    done
    for d in "${LOOPS[@]}"; do losetup -d "$d" 2>/dev/null; done
    rm -rf "$WORK"
}
trap cleanup EXIT

now_ms() { date +%s%3N; }
json_str() {
    local s="$1"
    s="${s//\\/\\\\}"; s="${s//\"/\\\"}"
    s="${s//$'\n'/ | }"; s="${s//$'\t'/ }"; s="${s//$'\r'/}"
    s="${s// /·}"
    printf '"%s"' "$s"
}
report() { # test status started-ms detail
    local ms=$(( $(now_ms) - $3 ))
    case "$2" in pass) PASS=$((PASS+1)) ;; fail) FAIL=$((FAIL+1)) ;; skip) SKIP=$((SKIP+1)) ;; esac
    printf '{"test":%s,"status":"%s","ms":%d,"detail":%s}\n' \
        "$(json_str "$1")" "$2" "$ms" "$(json_str "${4:-}")"
}
# check name detail-on-pass command... — pass when the command succeeds; on
# failure the detail is the command's output, trimmed.
check() {
    local name="$1" detail="$2"; shift 2
    local t0 out
    t0=$(now_ms)
    if out=$("$@" 2>&1); then
        report "$name" pass "$t0" "$detail${out:+: $(tail -1 <<<"$out")}"
    else
        report "$name" fail "$t0" "$(tail -20 <<<"$out")"
        echo "$out" >&2
        return 1
    fi
}

# Attach an image to a free loop device and print it. A pod's /dev is a copy
# made when it started, so a loop device the node created since may have no
# node here: make it (privileged pods may).
attach() {
    local d n
    d=$(losetup -f) || return 1
    # With no node for it here, losetup names it "/dev/loopN (lost)".
    d=${d%% *}
    n=${d#/dev/loop}
    [ -b "$d" ] || mknod "$d" b 7 "$n" || return 1
    losetup "$d" "$1" || return 1
    LOOPS+=("$d")
    echo "$d"
}
detach() { losetup -d "$1"; }
# kmount image mountpoint [options] — attach, then mount; prints the device.
kmount() {
    local d
    d=$(attach "$1") || return 1
    if mount -t vfat -o "${3:-utf8}" "$d" "$2"; then
        echo "$d"
    else
        detach "$d"; return 1
    fi
}
kumount() { umount "$1" && detach "$2"; }

# Can this node judge at all? A loop device and the vfat driver, both in the
# node's kernel; nothing here assumes the machine.
KERNEL_OK=1 KERNEL_WHY=""
t0=$(now_ms)
probe="$WORK/probe.img"
"$BIN/fill" "$probe" 1 fat12 >/dev/null 2>&1 || truncate -s 1M "$probe"
if ! loopdev=$(attach "$probe" 2>&1); then
    KERNEL_OK=0 KERNEL_WHY="no loop device on this node: $loopdev"
else
    mkdir -p "$WORK/probe.mnt"
    if mount -t vfat -o ro "$loopdev" "$WORK/probe.mnt" 2>"$WORK/probe.err"; then
        umount "$WORK/probe.mnt"
    elif ! grep -qw vfat /proc/filesystems; then
        KERNEL_OK=0 KERNEL_WHY="no vfat in this node's kernel: $(cat "$WORK/probe.err")"
    fi
    detach "$loopdev"
fi
rm -rf "$probe" "$WORK/probe.mnt" "$WORK/probe.err"
if [ "$KERNEL_OK" = 1 ]; then
    report "kernel-can-judge" pass "$t0" "loop device and vfat present ($(uname -r))"
else
    report "kernel-can-judge" skip "$t0" "$KERNEL_WHY"
fi

kskip() { report "$1" skip "$(now_ms)" "$KERNEL_WHY"; }

# fsck.fat from dosfstools (-n: never write) must find nothing to say.
dosfsck_clean() {
    local out rc
    out=$(fsck.fat -n "$1" 2>&1); rc=$?
    echo "$out"
    [ "$rc" -eq 0 ] || return 1
    ! grep -qE "Dirty bit|Free cluster summary wrong|[Ll]ost|[Bb]ad |[Ee]rror|differ" <<<"$out"
}

# One case: fill, judge with the kernel, read back. name size-mib width [big-mib]
run_case() {
    local name="$1" size="$2" width="$3" big="${4:-0}"
    local img="$WORK/$name.img" man="$WORK/$name.manifest" mnt="$WORK/$name.mnt"
    local t0

    t0=$(now_ms)
    if ! "$BIN/fill" "$img" "$size" "$width" "$big" >"$man" 2>"$WORK/$name.fill.err"; then
        report "$name/fill" fail "$t0" "$(tail -5 "$WORK/$name.fill.err")"; return
    fi
    local files; files=$(wc -l <"$man")
    report "$name/fill" pass "$t0" "$files files in ${size} MiB $width, with no kernel"

    check "$name/our-fsck-before" "fsck-fat clean" "$BIN/fsck-fat" "$img"
    check "$name/dosfsck-before" "fsck.fat -n clean" dosfsck_clean "$img"

    if [ "$KERNEL_OK" != 1 ]; then
        for t in kernel-mount kernel-reads-every-file kernel-writes dosfsck-after we-read-after; do
            kskip "$name/$t"
        done
        return
    fi
    KERNEL_RAN=1

    mkdir -p "$mnt"
    MOUNTS+=("$mnt")
    local dev
    t0=$(now_ms)
    if dev=$(kmount "$img" "$mnt" 2>"$WORK/$name.mount.err"); then
        report "$name/kernel-mount" pass "$t0" "mounted on $dev"
    else
        report "$name/kernel-mount" fail "$t0" "$(cat "$WORK/$name.mount.err")"; return
    fi
    check "$name/kernel-reads-every-file" "$files files read by the kernel" \
        "$BIN/verify" --dir "$mnt" "$man"

    # The kernel writes, so the next read is of something we did not write.
    t0=$(now_ms)
    if dd if=/dev/urandom of="$mnt/kernel-wrote.bin" bs=1K count=700 status=none \
        && mkdir -p "$mnt/Kernel Made This/deeper" \
        && echo "written by the kernel" >"$mnt/Kernel Made This/deeper/note.txt"; then
        local want; want=$(sha256sum "$mnt/kernel-wrote.bin" | cut -d' ' -f1)
        sync
        if kumount "$mnt" "$dev"; then
            report "$name/kernel-writes" pass "$t0" "700 KiB and a long-named directory, unmounted"
        else
            report "$name/kernel-writes" fail "$t0" "umount failed"; return
        fi
    else
        report "$name/kernel-writes" fail "$t0" "the kernel's write failed"
        kumount "$mnt" "$dev" 2>/dev/null; return
    fi

    check "$name/dosfsck-after" "fsck.fat -n clean after the kernel wrote" dosfsck_clean "$img"
    check "$name/our-fsck-after" "fsck-fat clean after the kernel wrote" "$BIN/fsck-fat" "$img"
    check "$name/we-read-after" "every file read back here after the kernel wrote" \
        "$BIN/verify" "$img" "$man"

    t0=$(now_ms)
    local got
    got=$("$BIN/fio-dos" "$img" get /kernel-wrote.bin "$WORK/$name.kernel.bin" >/dev/null 2>&1 \
          && sha256sum "$WORK/$name.kernel.bin" | cut -d' ' -f1)
    [ -n "$got" ] && [ "$got" = "$want" ] \
        && report "$name/we-read-the-kernels-file" pass "$t0" "sha256 $got" \
        || report "$name/we-read-the-kernels-file" fail "$t0" "kernel $want, ours ${got:-unreadable}"
    t0=$(now_ms)
    "$BIN/fio-dos" "$img" cat "/Kernel Made This/deeper/note.txt" 2>&1 | grep -q "written by the kernel" \
        && report "$name/we-read-the-kernels-dir" pass "$t0" "the kernel's long-named directory reads here" \
        || report "$name/we-read-the-kernels-dir" fail "$t0" "not readable here"

    [ "$SUITE" = short ] && return

    # Round two: we change what the kernel left, the kernel judges again.
    t0=$(now_ms)
    local f="$WORK/$name.round2.txt" out
    echo "written by fio-dos after the kernel" >"$f"
    if out=$( { "$BIN/fio-dos" "$img" mv "/Kernel Made This" "/Renamed By fio-dos" \
        && "$BIN/fio-dos" "$img" rm /kernel-wrote.bin \
        && "$BIN/fio-dos" "$img" rm -r /many \
        && "$BIN/fio-dos" "$img" mkdir "/Second Round/with a long name" \
        && "$BIN/fio-dos" "$img" put "$f" "/Second Round/with a long name/after.txt"; } 2>&1); then
        report "$name/we-change-the-kernels-files" pass "$t0" "mv, rm, rm -r, mkdir, put"
    else
        report "$name/we-change-the-kernels-files" fail "$t0" "$out"; return
    fi
    grep -v $'\t/many/' "$man" >"$man.2"
    check "$name/round2-our-fsck" "fsck-fat clean" "$BIN/fsck-fat" "$img"
    check "$name/round2-dosfsck" "fsck.fat -n clean" dosfsck_clean "$img"
    t0=$(now_ms)
    if dev=$(kmount "$img" "$mnt" 2>"$WORK/$name.mount.err"); then
        report "$name/round2-kernel-mount" pass "$t0" "mounted again on $dev"
    else
        report "$name/round2-kernel-mount" fail "$t0" "$(cat "$WORK/$name.mount.err")"; return
    fi
    t0=$(now_ms)
    if "$BIN/verify" --dir "$mnt" "$man.2" >"$WORK/$name.r2" 2>&1 \
        && [ ! -e "$mnt/kernel-wrote.bin" ] && [ ! -e "$mnt/many" ] \
        && grep -q "written by the kernel" "$mnt/Renamed By fio-dos/deeper/note.txt" \
        && cmp -s "$f" "$mnt/Second Round/with a long name/after.txt"; then
        report "$name/round2-kernel-reads" pass "$t0" "the kernel sees our rename, removals and new files: $(tail -1 "$WORK/$name.r2")"
    else
        report "$name/round2-kernel-reads" fail "$t0" "$(tail -5 "$WORK/$name.r2"; ls -la "$mnt" 2>&1)"
    fi
    kumount "$mnt" "$dev"
    check "$name/round2-dosfsck-after" "fsck.fat -n clean" dosfsck_clean "$img"
}

run_case fat12 8 fat12
run_case fat16 64 fat16
run_case fat32 512 fat32
[ "$SUITE" = long ] && run_case fat32-big 2048 fat32 200
rm -f "$WORK"/*.img

printf '{"summary":{"pass":%d,"fail":%d,"skip":%d}}\n' "$PASS" "$FAIL" "$SKIP"
[ "$FAIL" -gt 0 ] && exit 1
# Without the kernel's side nothing was judged: that is "could not run".
[ "$KERNEL_RAN" = 1 ] || exit 2
exit 0

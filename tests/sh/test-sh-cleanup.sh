#!/bin/bash
# test/test.sh's EXIT trap undoes every loop device and mount the run made,
# with losetup, mount, umount, mountpoint and mknod mocked (f1e178d2225e).
# Run by tests/test_sh.rs; needs nothing but bash.
#
#   tests/sh/test-sh-cleanup.sh path/to/test.sh
#
# Exits 0 when every scenario's log is what it should be, 1 otherwise.

set -u
TEST_SH="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT
MOCKS="$SCRATCH/bin"
mkdir -p "$MOCKS"

# Each mock appends what it was asked to do to $MOCK_LOG. losetup -f hands
# out loop10, loop11, ... in turn, in the "(lost)" form a pod's /dev gives.
cat >"$MOCKS/losetup" <<'MOCK'
#!/bin/bash
case "$1" in
    -f) n=$(cat "$MOCK_STATE/next" 2>/dev/null || echo 10)
        echo $((n + 1)) >"$MOCK_STATE/next"
        echo "/dev/loop$n (lost)" ;;
    -d) echo "detach $2" >>"$MOCK_LOG" ;;
    *)  echo "attach $1 $2" >>"$MOCK_LOG" ;;
esac
MOCK
cat >"$MOCKS/mount" <<'MOCK'
#!/bin/bash
[ -n "${MOCK_MOUNT_FAILS:-}" ] && { echo "mount: mocked failure" >&2; exit 32; }
echo "mount ${*: -1}" >>"$MOCK_LOG"
touch "${*: -1}/.mounted"
MOCK
cat >"$MOCKS/umount" <<'MOCK'
#!/bin/bash
echo "umount $1" >>"$MOCK_LOG"
rm -f "$1/.mounted"
MOCK
cat >"$MOCKS/mountpoint" <<'MOCK'
#!/bin/bash
[ -e "${*: -1}/.mounted" ]
MOCK
cat >"$MOCKS/mknod" <<'MOCK'
#!/bin/bash
echo "mknod $1" >>"$MOCK_LOG"
MOCK
chmod +x "$MOCKS"/*

FAILED=0
# scenario name expected-log body — body runs after test.sh's functions are
# sourced, and ends the shell however it ends; the trap has the last word.
scenario() {
    local name="$1" want="$2" body="$3" dir="$SCRATCH/$1"
    mkdir -p "$dir/state" "$dir/tmp" "$dir/mnt" "$dir/mnt2"
    : >"$dir/log"
    ( cd "$dir" && PATH="$MOCKS:$PATH" MOCK_LOG="$dir/log" MOCK_STATE="$dir/state" \
        TMPDIR="$dir/tmp" FIO_DOS_TEST_FUNCTIONS_ONLY=1 \
        bash -c "source '$TEST_SH' short; $body" ) >"$dir/out" 2>&1
    local got; got=$(grep -v '^mknod' "$dir/log")
    if [ "$got" != "$want" ]; then
        echo "FAIL $name"; echo "--- want"; echo "$want"; echo "--- got"; echo "$got"
        echo "--- output"; cat "$dir/out"
        FAILED=1
    elif [ -n "$(ls -A "$dir/tmp")" ]; then
        echo "FAIL $name: the work directory was left behind"; FAILED=1
    else
        echo "ok $name"
    fi
}

# An error path: two images attached, one mounted, then the run dies.
scenario interrupted "attach /dev/loop10 a.img
attach /dev/loop11 b.img
mount mnt
umount mnt
detach /dev/loop10
detach /dev/loop11" '
MOUNTS+=(mnt)   # as run_case does
attach a.img || exit 9
kmount b.img mnt || exit 9
[ "$LOOP_DEV" = /dev/loop11 ] || exit 9
exit 1'

# A device detached by the run is not detached again by the trap.
scenario detached-once "attach /dev/loop10 a.img
mount mnt
umount mnt
detach /dev/loop10
attach /dev/loop11 b.img
detach /dev/loop11" '
kmount a.img mnt || exit 9
kumount mnt "$LOOP_DEV" || exit 9
attach b.img || exit 9
exit 0'

# A mount that fails gives its device back at once, and only once.
scenario mount-fails "attach /dev/loop10 a.img
detach /dev/loop10" '
MOCK_MOUNT_FAILS=1 kmount a.img mnt && exit 9
[ -z "$LOOP_DEV" ] || exit 9
exit 0'

# A signal ends the run through the same trap.
scenario terminated "attach /dev/loop10 a.img
mount mnt
umount mnt
detach /dev/loop10" '
MOUNTS+=(mnt)
kmount a.img mnt || exit 9
kill -TERM $$
sleep 5
exit 9'

exit $FAILED

#!/bin/bash
# verify-on-linux.sh — put the files this crate writes in front of a real kernel.
#
# The round-trip tests prove we can read back what we wrote, which proves the
# two halves agree with each other and nothing more. This writes an image with
# no kernel involved, ships it to a Linux host, and asks the kernel's own FAT
# driver to read every file — then has the kernel write to it and reads that
# back here.
#
# Both directions matter. "We can read our own files" and "the filesystem is
# right" are different claims.
#
#   ./tests/verify-on-linux.sh [user@host]
#
# Defaults to root@dev.g8.lo.

set -uo pipefail

HOST="${1:-root@dev.g8.lo}"
REMOTE_DIR=/root/fio-dos-verify
FAILURES=0

GREEN='' RED='' CYAN='' BOLD='' RESET=''
if [ -t 1 ]; then
    GREEN='\033[0;32m'; RED='\033[0;31m'; CYAN='\033[0;36m'
    BOLD='\033[1m'; RESET='\033[0m'
fi
ok()  { echo -e "  ${GREEN}OK${RESET}: $1"; }
bad() { echo -e "  ${RED}FAIL${RESET}: $1"; FAILURES=$((FAILURES+1)); }
hdr() { echo; echo -e "${BOLD}${CYAN}-- $1 --${RESET}"; }

# name:size-mib:width
CASES=(
    "fat12:8:fat12"
    "fat16:64:fat16"
    "fat32:512:fat32"
)

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

hdr "building"
cargo build --quiet --release --example fill --example verify --bin fio-dos || {
    echo "cargo build failed"; exit 1;
}
(cd ../mkfs.dos.rs && cargo build --quiet --release --bin fsck-fat) || {
    echo "building fsck-fat failed"; exit 1;
}

for case in "${CASES[@]}"; do
    IFS=: read -r name size width <<< "$case"
    hdr "$name (${size} MiB)"

    ./target/release/examples/fill "$WORK/$name.img" "$size" "$width" \
        > "$WORK/$name.manifest" 2> "$WORK/$name.fill.log" || {
        bad "fill failed"; cat "$WORK/$name.fill.log"; continue;
    }
    files=$(wc -l < "$WORK/$name.manifest" | tr -d ' ')
    ok "wrote $files files with no kernel"

    ssh "$HOST" "rm -rf $REMOTE_DIR && mkdir -p $REMOTE_DIR" || exit 1
    scp -q "$WORK/$name.img" "$WORK/$name.manifest" "$HOST:$REMOTE_DIR/"

    # The kernel's turn: check the image, mount it, regenerate every file's
    # contents from the manifest and compare, then write to it and unmount.
    out=$(ssh "$HOST" "bash -s" <<REMOTE
set -uo pipefail
cd $REMOTE_DIR
echo "### fsck-before"
fsck.fat -n -v $name.img 2>&1 | tail -15
mkdir -p /mnt/fio-$name
mount -o loop -t vfat -o utf8 $name.img /mnt/fio-$name 2>&1 && echo MOUNTED
if mountpoint -q /mnt/fio-$name; then
    python3 - <<'PY'
import sys
root = "/mnt/fio-$name"
bad = 0
checked = 0
for line in open("$REMOTE_DIR/$name.manifest"):
    line = line.rstrip("\n")
    if not line:
        continue
    index, size, path = line.split("\t", 2)
    index, size = int(index), int(size)
    want = bytes((index * 37 + j) % 251 for j in range(size))
    try:
        with open(root + path, "rb") as f:
            got = f.read()
    except OSError as e:
        print("UNREADABLE", path, e)
        bad += 1
        continue
    if got != want:
        print("MISMATCH", path, len(got), "want", size)
        bad += 1
    else:
        checked += 1
print("KERNEL-VERIFIED", checked, "files,", bad, "bad")
PY
    # And the kernel writes, so the next pass reads something we did not write.
    dd if=/dev/urandom of=/mnt/fio-$name/kernel-wrote.bin bs=1K count=700 2>/dev/null && echo KERNELWRITE-OK
    mkdir -p "/mnt/fio-$name/Kernel Made This/deeper" && echo "written by the kernel" > "/mnt/fio-$name/Kernel Made This/deeper/note.txt" && echo KERNELDIR-OK
    sha256sum /mnt/fio-$name/kernel-wrote.bin | awk '{print "SHA", \$1}'
    sync
    umount /mnt/fio-$name && echo UNMOUNTED
fi
echo "### fsck-after"
fsck.fat -n -v $name.img 2>&1 | tail -20
rmdir /mnt/fio-$name
REMOTE
)

    echo "$out" | grep -q "MOUNTED"        && ok "kernel mounted it" || bad "mount failed"
    if echo "$out" | grep -q "KERNEL-VERIFIED .* 0 bad"; then
        ok "kernel read every file back byte for byte: $(echo "$out" | grep -o 'KERNEL-VERIFIED.*')"
    else
        bad "kernel found differences"; echo "$out" | grep -E "MISMATCH|UNREADABLE|KERNEL-VERIFIED" | sed 's/^/      /'
    fi
    echo "$out" | grep -q "KERNELWRITE-OK"  && ok "kernel wrote 700 KiB" || bad "kernel write failed"
    echo "$out" | grep -q "KERNELDIR-OK"    && ok "kernel made a long-named directory" || bad "kernel mkdir failed"
    echo "$out" | grep -q "UNMOUNTED"       && ok "unmounted cleanly" || bad "unmount failed"

    for phase in before after; do
        text=$(echo "$out" | sed -n "/### fsck-$phase/,/###/p")
        echo "$text" | grep -qE "Dirty bit|Free cluster summary wrong|Bad|error|Cluster" \
            && { bad "fsck.fat complained $phase"; echo "$text" | sed 's/^/      /'; } \
            || ok "fsck.fat clean $phase the kernel's writes"
    done

    # Back here: read the image the kernel wrote to, with no kernel.
    scp -q "$HOST:$REMOTE_DIR/$name.img" "$WORK/$name.after.img"

    ./target/release/examples/verify "$WORK/$name.after.img" "$WORK/$name.manifest" \
        > "$WORK/$name.verify" 2>&1 \
        && ok "we read every file back after the kernel wrote to it: $(tail -1 "$WORK/$name.verify")" \
        || { bad "our reader disagrees"; sed 's/^/      /' "$WORK/$name.verify"; }

    ./target/release/fio-dos "$WORK/$name.after.img" get /kernel-wrote.bin "$WORK/$name.kernel.bin" > /dev/null 2>&1
    if [ -f "$WORK/$name.kernel.bin" ]; then
        want=$(echo "$out" | awk '/^SHA /{print $2}')
        got=$(shasum -a 256 "$WORK/$name.kernel.bin" 2>/dev/null | awk '{print $1}' \
              || sha256sum "$WORK/$name.kernel.bin" | awk '{print $1}')
        [ -n "$want" ] && [ "$want" = "$got" ] \
            && ok "the 700 KiB the kernel wrote reads back identical here" \
            || bad "the kernel's file differs: kernel $want, ours $got"
    else
        bad "could not read the kernel's file"
    fi

    ./target/release/fio-dos "$WORK/$name.after.img" cat "/Kernel Made This/deeper/note.txt" 2>/dev/null \
        | grep -q "written by the kernel" \
        && ok "we read the kernel's long-named directory" \
        || bad "the kernel's long-named directory is not readable here"

    ../mkfs.dos.rs/target/release/fsck-fat "$WORK/$name.after.img" > "$WORK/$name.ourfsck" 2>&1 \
        && ok "our fsck.fat calls it clean" \
        || { bad "our fsck.fat disagrees"; sed 's/^/      /' "$WORK/$name.ourfsck"; }
done

hdr "result"
if [ "$FAILURES" -eq 0 ]; then
    echo -e "${GREEN}every configuration passes in both directions${RESET}"
else
    echo -e "${RED}$FAILURES check(s) failed${RESET}"
fi
exit $((FAILURES > 0))

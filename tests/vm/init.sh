#!/bin/busybox sh
# The init of the kernel-verification VM (#1), run as PID 1 from the
# initramfs tests/vm/build-image.sh makes. CLAUDE.md rule 6: the kernel is
# the judge. This is test/test.sh's `long` suite, as PID 1 under a real
# kernel instead of in a pod. For each case, on a sparse image in tmpfs:
#
#   fill                              written with no kernel; a manifest
#   fsck-fat, fsck.fat -n             clean, as fill wrote it
#   the kernel loop-mounts it         every file in the manifest compared
#                                     byte for byte (verify --dir)
#   the kernel writes                 700 KiB and a long-named directory
#   fsck.fat -n, fsck-fat             clean, after the kernel wrote
#   we read it back                   every file, and the kernel's, here
#   we rename, remove and add         fio-dos over the kernel's work
#   fsck-fat, fsck.fat -n             clean
#   the kernel mounts it again        and sees exactly what we left
#   fsck.fat -n                       clean
#
# Cases: FAT12 8 MiB, FAT16 64 MiB, FAT32 512 MiB, and a 2 GiB FAT32 holding
# a 200 MiB file. Prints `VERIFY PASS` or `VERIFY FAIL <why>` on the serial
# console, then powers off. stormcentral's testhost boot watches for those
# lines.
/bin/busybox mount -t proc proc /proc
/bin/busybox --install -s /bin
export PATH=/bin:/sbin
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev
mkdir -p /mnt /work
mount -t tmpfs -o size=90% tmpfs /work
echo 1 > /proc/sys/kernel/printk 2>/dev/null

say() { echo "FIO-DOS-VERIFY: $*"; }
fail() {
    say "FAIL: $*"
    dmesg | grep -iE 'fat|loop' | tail -20 | sed 's/^/FIO-DOS-VERIFY dmesg: /'
    [ -s /work/out ] && tail -40 /work/out | sed 's/^/FIO-DOS-VERIFY out: /'
    echo "VERIFY FAIL $*"
    sync; poweroff -f; sleep 30
}

say "kernel $(uname -r), $(cat /build-info 2>/dev/null)"
for m in $(cat /modules.order 2>/dev/null); do
    insmod "/modules/$m" || fail "insmod $m"
done
grep -qw vfat /proc/filesystems || fail "the kernel has no vfat"

sha() { sha256sum "$1" | cut -d' ' -f1; }

# run what command... — the command's output to /work/out, failing with it.
run() {
    what=$1; shift
    "$@" >/work/out 2>&1 || fail "$what (exit $?): $(tail -3 /work/out | tr '\n' ' ')"
}

# fsck.fat from dosfstools (-n: never write) must find nothing to say, and
# our own fsck-fat (mkfs-dos's checker) must agree.
dosfsck() { # image what
    run "fsck.fat -n $2" fsck.fat -n "$1"
    if grep -qE "Dirty bit|Free cluster summary wrong|[Ll]ost|[Bb]ad |[Ee]rror|differ" /work/out; then
        fail "fsck.fat -n $2: $(head -3 /work/out | tr '\n' ' ')"
    fi
}
ourfsck() { run "fsck-fat $2" fsck-fat "$1"; }

kmount() { # image what
    mount -t vfat -o loop,utf8 "$1" /mnt 2>/work/out || fail "$2: mount: $(cat /work/out)"
    grep -q " /mnt vfat rw" /proc/mounts || fail "$2: not mounted vfat read-write"
}
kumount() { umount /mnt || fail "$1: umount"; }

# case name size-mib width [big-mib]
dos_case() {
    name=$1 size=$2 width=$3 big=${4:-0}
    img=/work/$name.img man=/work/$name.manifest
    rm -f "$img"
    fill "$img" "$size" "$width" "$big" >"$man" 2>/work/out || fail "$name: fill: $(tail -3 /work/out)"
    files=$(wc -l <"$man")
    ourfsck "$img" "$name: as fill wrote it"
    dosfsck "$img" "$name: as fill wrote it"

    kmount "$img" "$name"
    run "$name: the kernel reads every file" verify --dir /mnt "$man"
    dd if=/dev/urandom of="/mnt/kernel-wrote.bin" bs=1K count=700 2>/dev/null \
        || fail "$name: the kernel's 700 KiB write"
    mkdir -p "/mnt/Kernel Made This/deeper" \
        && echo "written by the kernel" >"/mnt/Kernel Made This/deeper/note.txt" \
        || fail "$name: the kernel's long-named directory"
    want=$(sha /mnt/kernel-wrote.bin)
    sync
    kumount "$name"
    dosfsck "$img" "$name: after the kernel wrote"
    ourfsck "$img" "$name: after the kernel wrote"

    run "$name: we read every file after the kernel wrote" verify "$img" "$man"
    run "$name: we read the kernel's file" fio-dos "$img" get /kernel-wrote.bin /work/kernel.bin
    [ "$(sha /work/kernel.bin)" = "$want" ] || fail "$name: the kernel's file reads here as $(sha /work/kernel.bin), want $want"
    run "$name: we read the kernel's directory" fio-dos "$img" cat "/Kernel Made This/deeper/note.txt"
    grep -q "written by the kernel" /work/out || fail "$name: the kernel's note reads here as '$(cat /work/out)'"
    rm -f /work/kernel.bin

    # Round two: we change what the kernel left, the kernel judges again.
    echo "written by fio-dos after the kernel" >/work/after.txt
    run "$name: fio-dos mv" fio-dos "$img" mv "/Kernel Made This" "/Renamed By fio-dos"
    run "$name: fio-dos rm" fio-dos "$img" rm /kernel-wrote.bin
    run "$name: fio-dos rm -r" fio-dos "$img" rm -r /many
    run "$name: fio-dos mkdir" fio-dos "$img" mkdir "/Second Round/with a long name"
    run "$name: fio-dos put" fio-dos "$img" put /work/after.txt "/Second Round/with a long name/after.txt"
    grep -v "$(printf '\t')/many/" "$man" >"$man.2"
    ourfsck "$img" "$name: after our second round"
    dosfsck "$img" "$name: after our second round"

    kmount "$img" "$name (second mount)"
    run "$name: the kernel reads our second round" verify --dir /mnt "$man.2"
    [ ! -e /mnt/kernel-wrote.bin ] || fail "$name: the kernel still sees kernel-wrote.bin after our rm"
    [ ! -e /mnt/many ] || fail "$name: the kernel still sees many/ after our rm -r"
    grep -q "written by the kernel" "/mnt/Renamed By fio-dos/deeper/note.txt" 2>/dev/null \
        || fail "$name: the kernel does not see our rename"
    cmp -s /work/after.txt "/mnt/Second Round/with a long name/after.txt" \
        || fail "$name: the kernel reads our new file wrong"
    kumount "$name (second mount)"
    dosfsck "$img" "$name: after the kernel's second mount"

    say "$name: $files files in $size MiB $width; fill, fsck clean; the kernel read every file, wrote, fsck clean; we read it all back; we renamed, removed and added, fsck clean; the kernel read that, fsck clean"
    rm -f "$img" "$man" "$man.2"
}

dos_case fat12 8 fat12
dos_case fat16 64 fat16
dos_case fat32 512 fat32
dos_case fat32-big 2048 fat32 200

echo "VERIFY PASS"
sync; poweroff -f; sleep 30

#!/bin/sh
# Build fio-dos's test image context for the commit checked out.
#
#   test/build.sh [target]        default x86_64-unknown-linux-musl
#
# Per stormcentral docs/test-standard.md this runs first, in the checkout on
# the build box, and stages static binaries in test/.stage/;
# test/Containerfile (context: the repo root) packages them.
#
# fsck-fat is built from this crate's own dependency graph, so it is exactly
# the mkfs-dos commit Cargo.toml pins. Its `cli` feature pulls crates this
# crate's Cargo.lock does not list, so that one build is not --locked; the
# mkfs-dos rev is fixed either way.
# With STAGE_ONLY=1 it stops after staging and prints the stage path;
# otherwise it also runs `podman build` and tags fio-dos-test.
set -eu
target=${1:-x86_64-unknown-linux-musl}
root=$(cd "$(dirname "$0")/.." && pwd)
commit=$(git -C "$root" rev-parse HEAD)
manifest="$root/Cargo.toml"

cargo build --release --locked --target "$target" --manifest-path "$manifest" \
    --bin fio-dos --example fill --example verify
cargo build --release --target "$target" --manifest-path "$manifest" \
    -p mkfs-dos --features mkfs-dos/cli --bin fsck-fat
tdir=$(cargo metadata --format-version 1 --no-deps --manifest-path "$manifest" |
    sed 's/.*"target_directory":"\([^"]*\)".*/\1/')
out="$tdir/$target/release"
stage="$root/test/.stage"
rm -rf "$stage"
mkdir -p "$stage"
cp "$out/fio-dos" "$out/fsck-fat" "$out/examples/fill" "$out/examples/verify" "$stage/"
cp "$root/test/test.sh" "$stage/test"
chmod 755 "$stage/test"

if [ "${STAGE_ONLY:-0}" = 1 ]; then
    echo "$stage"
    exit 0
fi
podman build -f "$root/test/Containerfile" --build-arg COMMIT="$commit" -t fio-dos-test "$root"

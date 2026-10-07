#!/bin/sh
# Build fio-dos's test image context for the commit checked out.
#
#   test/build.sh [target]        default x86_64-unknown-linux-musl
#
# Per stormcentral docs/test-standard.md this runs first, in the checkout on
# the build box, and stages static binaries in test/.stage/;
# test/Containerfile (context: the repo root) packages them.
#
# fsck-fat is examples/fsck.rs: the checker from the mkfs-dos commit
# Cargo.toml pins, built as part of this crate (mkfs-dos's own fsck-fat binary
# sits behind a feature a dependency's build cannot be given).
# It only stages: the runner makes the image from test/Containerfile itself,
# with no container runtime (stormcentral#121). It prints the stage path.
set -eu
target=${1:-x86_64-unknown-linux-musl}
root=$(cd "$(dirname "$0")/.." && pwd)
manifest="$root/Cargo.toml"

cargo build --release --locked --target "$target" --manifest-path "$manifest" \
    --bin fio-dos --example fill --example verify --example fsck
tdir=$(cargo metadata --format-version 1 --no-deps --manifest-path "$manifest" |
    sed 's/.*"target_directory":"\([^"]*\)".*/\1/')
out="$tdir/$target/release"
stage="$root/test/.stage"
rm -rf "$stage"
mkdir -p "$stage"
cp "$out/fio-dos" "$out/examples/fill" "$out/examples/verify" "$stage/"
cp "$out/examples/fsck" "$stage/fsck-fat"
cp "$root/test/test.sh" "$stage/test"
chmod 755 "$stage/test"
echo "$stage"

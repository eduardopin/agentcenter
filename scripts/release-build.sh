#!/usr/bin/env bash
# Builds the release tarball: optimised binaries with the build machine's
# paths remapped away, THIRD_PARTY_LICENSES.html from cargo-about, and a
# tarball that packs to the same bytes for the same commit.
#
#   bash scripts/release-build.sh                 build + licences + pack
#   bash scripts/release-build.sh --package-only  pack what is already built
#
# RELEASE_DIST (default dist/) is where the tarball lands. Needs cargo-about
# (0.9.3; the release workflow installs that version). No network beyond what cargo itself fetches.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
root="$(pwd)"

if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi

dist="${RELEASE_DIST:-dist}"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
version="$(cargo metadata --format-version 1 --no-deps \
    | python3 -I -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "agentcenter"))')"
arch="$(uname -m)"
name="agentcenter-$version-$arch-linux"

# The same commit must give the same tarball: stamp files with the commit time.
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH="$(git log -1 --format=%ct 2>/dev/null || echo 0)"
fi
export SOURCE_DATE_EPOCH

if [ "${1:-}" != "--package-only" ]; then
    # Every absolute prefix rustc can print into a binary (panic locations,
    # debug info, dependency sources) is mapped to a fixed name. rustc applies
    # the LAST matching mapping, so the broad prefixes go first and the
    # narrower ones, which may sit inside them, go after.
    cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
    flags="--remap-path-prefix=$HOME=/home-removed"
    flags="$flags --remap-path-prefix=$root=/src"
    flags="$flags --remap-path-prefix=$rustup_home=/rustup"
    flags="$flags --remap-path-prefix=$cargo_home=/cargo"
    flags="$flags --remap-path-prefix=$target_dir=/target"
    export RUSTFLAGS="$flags"
    cargo build --release --locked -p agentcenter -p agentcenter-cli
    cargo about generate --locked --fail about.hbs -o "$target_dir/THIRD_PARTY_LICENSES.html"
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name"
cp "$target_dir/release/agentcenter" "$target_dir/release/agentcenter-cli" "$stage/$name/"
cp "$target_dir/THIRD_PARTY_LICENSES.html" LICENSE NOTICE "$stage/$name/"

mkdir -p "$dist"
tar --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime="@$SOURCE_DATE_EPOCH" -C "$stage" -cf - "$name" \
    | gzip -n -9 > "$dist/$name.tar.gz"
echo "release: $dist/$name.tar.gz"

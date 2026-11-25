#!/usr/bin/env bash
# Checks the release build keeps local paths and licences straight:
#   1. the release config exists (remap flags, cargo-about config, workflow);
#   2. the release binaries carry no /home or /root path (`strings`);
#   3. the tarball ships THIRD_PARTY_LICENSES.html and packs byte-for-byte
#      the same twice.
# `--compare A.tar.gz B.tar.gz` checks two builds of one commit (made in
# different checkout paths) hold byte-identical binaries; release.yml uses it.
# `--static` stops after step 1 (fast, no build; what scripts/ci.sh runs).
# Without it the script builds with scripts/release-build.sh, or reuses the
# tarball in RELEASE_DIST when RELEASE_SKIP_BUILD=1 (the pack-twice step is
# then skipped unless the built binaries are still in the target dir).
# No network.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [ "${1:-}" = "--compare" ]; then
    [ $# -eq 3 ] && [ -f "$2" ] && [ -f "$3" ] \
        || { echo "usage: $0 --compare <first.tar.gz> <second.tar.gz>" >&2; exit 2; }
    cmp_dir="$(mktemp -d)"
    trap 'rm -rf "$cmp_dir"' EXIT
    mkdir "$cmp_dir/a" "$cmp_dir/b"
    tar -xzf "$2" -C "$cmp_dir/a"
    tar -xzf "$3" -C "$cmp_dir/b"
    status=0
    for bin in agentcenter agentcenter-cli; do
        fa="$(find "$cmp_dir/a" -type f -name "$bin" | head -n1)"
        fb="$(find "$cmp_dir/b" -type f -name "$bin" | head -n1)"
        [ -n "$fa" ] && [ -n "$fb" ] || { echo "FAIL: $bin missing from a tarball"; status=1; continue; }
        sa="$(sha256sum < "$fa" | cut -d' ' -f1)"
        sb="$(sha256sum < "$fb" | cut -d' ' -f1)"
        echo "$bin first=$sa second=$sb"
        [ "$sa" = "$sb" ] || { echo "FAIL: $bin differs between the two builds"; status=1; }
    done
    [ "$status" -eq 0 ] && echo "ok: both builds hold identical binaries"
    exit "$status"
fi

fails=0
fail() { echo " - $*"; fails=$((fails + 1)); }
need() { [ -f "$1" ] || fail "missing: $1"; }

echo "release: static checks"
need scripts/release-build.sh
need scripts/release.sh
need scripts/release-deb.sh
need about.toml
need about.hbs
need .github/workflows/release.yml
if [ -f scripts/release-build.sh ]; then
    grep -q -- '--remap-path-prefix' scripts/release-build.sh \
        || fail "scripts/release-build.sh: no --remap-path-prefix"
    grep -q 'cargo about\|cargo-about' scripts/release-build.sh \
        || fail "scripts/release-build.sh: does not run cargo-about"
    grep -q 'THIRD_PARTY_LICENSES.html' scripts/release-build.sh \
        || fail "scripts/release-build.sh: does not pack THIRD_PARTY_LICENSES.html"
fi
if [ -f .github/workflows/release.yml ]; then
    grep -q 'scripts/release.sh' .github/workflows/release.yml \
        || fail "release.yml: does not call scripts/release.sh"
    grep -q 'release-build.sh' scripts/release.sh \
        || fail "scripts/release.sh: does not call scripts/release-build.sh"
    grep -q 'scripts/test-release-paths.sh' .github/workflows/release.yml \
        || fail "release.yml: does not run scripts/test-release-paths.sh"
    grep -q 'test-release-paths.sh --compare' .github/workflows/release.yml \
        || fail "release.yml: does not compare the two builds with --compare"
    if grep -Eq 'gh release (edit|publish)|--draft=false|softprops/action-gh-release' .github/workflows/release.yml; then
        fail "release.yml: publishes the release (it must stay a draft)"
    fi
    if grep -q 'secrets\.' .github/workflows/release.yml; then
        fail "release.yml: reads a repository secret (only github.token is allowed)"
    fi
fi

if [ "$fails" -ne 0 ]; then
    echo "FAIL ($fails)"
    exit 1
fi
[ "${1:-}" = "--static" ] && { echo "ok: release config present (static)"; exit 0; }

dist="${RELEASE_DIST:-dist}"
if [ "${RELEASE_SKIP_BUILD:-0}" != "1" ]; then
    echo "release: building"
    RELEASE_DIST="$dist" bash scripts/release-build.sh
fi

# The exact file this checkout builds, not whatever older release dist/ holds.
version="$(cargo metadata --format-version 1 --no-deps \
    | python3 -I -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "agentcenter"))')"
tarball="$dist/agentcenter-$version-$(uname -m)-linux.tar.gz"
[ -f "$tarball" ] || { echo "FAIL: $tarball not found"; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
tar -xzf "$tarball" -C "$work"

echo "release: licences in $tarball"
[ -n "$(find "$work" -name THIRD_PARTY_LICENSES.html -size +1k)" ] \
    || fail "tarball has no THIRD_PARTY_LICENSES.html (or it is empty)"

echo "release: strings on the binaries"
bins=0
for name in agentcenter agentcenter-cli; do
    bin="$(find "$work" -type f -name "$name" | head -n1)"
    [ -n "$bin" ] || { fail "tarball has no $name binary"; continue; }
    bins=$((bins + 1))
    hits="$(strings -a "$bin" | grep -E '/(home|root)/' | head -n5 || true)"
    [ -z "$hits" ] || fail "$name embeds a local path: $(echo "$hits" | tr '\n' ' ')"
done

deb="$(find "$dist" -maxdepth 1 -name "agentcenter_*.deb" | head -n1)"
if [ -n "$deb" ]; then
    echo "release: strings on the .deb binaries (same check as the tarball)"
    mkdir "$work/deb"
    dpkg-deb -x "$deb" "$work/deb"
    for name in agentcenter agentcenter-cli; do
        bin="$work/deb/usr/bin/$name"
        [ -f "$bin" ] || { fail ".deb has no $name binary"; continue; }
        hits="$(strings -a "$bin" | grep -E '/(home|root)/' | head -n5 || true)"
        [ -z "$hits" ] || fail ".deb $name embeds a local path: $(echo "$hits" | tr '\n' ' ')"
    done
else
    echo "release: no .deb in $dist, skipped"
fi

target_dir="${CARGO_TARGET_DIR:-target}"
if [ -f "$target_dir/release/agentcenter" ] && [ -f "$target_dir/THIRD_PARTY_LICENSES.html" ]; then
    echo "release: packing twice gives the same bytes"
    sum1="$(sha256sum < "$tarball")"
    RELEASE_DIST="$dist" bash scripts/release-build.sh --package-only >/dev/null
    sum2="$(sha256sum < "$tarball")"
    [ "$sum1" = "$sum2" ] || fail "tarball differs between two packs"
else
    echo "release: pack-twice skipped (no built binaries in $target_dir)"
fi

if [ "$fails" -ne 0 ]; then
    echo "FAIL ($fails)"
    exit 1
fi
echo "ok: release binaries are path-free and ship licences"

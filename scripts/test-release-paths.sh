#!/usr/bin/env bash
# Checks the release build keeps local paths and licences straight:
#   1. the release config exists (remap flags, cargo-about config, workflow);
#   2. the release binaries carry no /home or /root path (`strings`);
#   3. the tarball ships THIRD_PARTY_LICENSES.html and packs byte-for-byte
#      the same twice.
# `--static` stops after step 1 (fast, no build; what scripts/ci.sh runs).
# Without it the script builds with scripts/release-build.sh, or reuses the
# tarball in RELEASE_DIST when RELEASE_SKIP_BUILD=1. No network.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

fails=0
fail() { echo " - $*"; fails=$((fails + 1)); }
need() { [ -f "$1" ] || fail "missing: $1"; }

echo "release: static checks"
need scripts/release-build.sh
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
    grep -q 'scripts/release-build.sh' .github/workflows/release.yml \
        || fail "release.yml: does not call scripts/release-build.sh"
    grep -q 'scripts/test-release-paths.sh' .github/workflows/release.yml \
        || fail "release.yml: does not run scripts/test-release-paths.sh"
    # Publishing is the next card; this workflow only uploads an artifact.
    if grep -Eq 'secrets\.|environment:|softprops/action-gh-release|gh release|contents: write' .github/workflows/release.yml; then
        fail "release.yml: publishes or uses secrets (out of scope for this workflow)"
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

tarball="$(ls "$dist"/agentcenter-*.tar.gz 2>/dev/null | head -n1 || true)"
[ -n "$tarball" ] || { echo "FAIL: no tarball in $dist"; exit 1; }

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

echo "release: packing twice gives the same bytes"
sum1="$(sha256sum < "$tarball")"
RELEASE_DIST="$dist" bash scripts/release-build.sh --package-only >/dev/null
sum2="$(sha256sum < "$tarball")"
[ "$sum1" = "$sum2" ] || fail "tarball differs between two packs"

if [ "$fails" -ne 0 ]; then
    echo "FAIL ($fails)"
    exit 1
fi
echo "ok: release binaries are path-free and ship licences"

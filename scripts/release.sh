#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

dist="${RELEASE_DIST:-dist}"
export RELEASE_DIST="$dist"

bash scripts/release-build.sh "$@"
tarball="$(find "$dist" -maxdepth 1 -name 'agentcenter-*.tar.gz' | sort | tail -n1)"
[ -n "$tarball" ] || { echo "release: no tarball in $dist" >&2; exit 1; }
bash scripts/release-deb.sh "$tarball"

(cd "$dist" && LC_ALL=C sha256sum agentcenter-*.tar.gz agentcenter_*.deb > SHA256SUMS)
echo "release: $dist/SHA256SUMS"

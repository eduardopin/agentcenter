#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

dist="${RELEASE_DIST:-dist}"
export RELEASE_DIST="$dist"

if [ "${GITHUB_REF_TYPE:-}" = "tag" ]; then
    version="$(cargo metadata --format-version 1 --no-deps \
        | python3 -I -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "agentcenter"))')"
    if [ "${GITHUB_REF_NAME#v}" != "$version" ]; then
        echo "release: tag ${GITHUB_REF_NAME:-} does not match Cargo version $version" >&2
        exit 1
    fi
fi

bash scripts/release-build.sh "$@"
tarball="$(find "$dist" -maxdepth 1 -name 'agentcenter-*.tar.gz' | sort | tail -n1)"
[ -n "$tarball" ] || { echo "release: no tarball in $dist" >&2; exit 1; }
bash scripts/release-deb.sh "$tarball"

(cd "$dist" && LC_ALL=C sha256sum agentcenter-*.tar.gz agentcenter_*.deb > SHA256SUMS)
echo "release: $dist/SHA256SUMS"

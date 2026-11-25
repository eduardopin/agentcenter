#!/usr/bin/env bash
# Offline checks for the tag-driven release: what the workflow promises, and
# what scripts/release.sh produces from a fake build.
#   1. release.yml: build -> attest -> draft-release, SHA-pinned actions, write
#      permissions only on the jobs that need them, the draft job gated by the
#      `release` environment and never publishing;
#   2. scripts/release.sh --package-only on fake binaries gives a tarball, a
#      .deb and a SHA256SUMS that `sha256sum -c` accepts and rejects tampering;
#   3. the .deb lists the binaries and the desktop file, and carries no path.
# What only a tag run can prove (attestation, draft release, approval) is not
# claimed here. `--static` runs step 1 only. No network, no cargo build.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

wf=.github/workflows/release.yml
fails=0
fail() { echo " - $*"; fails=$((fails + 1)); }

# Text between "  <job>:" and the next job at the same indent.
job_block() {
    awk -v job="$1" '
        $0 ~ "^  " job ":[[:space:]]*$" { on = 1; next }
        on && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { exit }
        on { print }
    ' "$wf"
}

echo "release workflow: static checks"
[ -f "$wf" ] || { echo "FAIL: missing $wf"; exit 1; }

for job in build attest draft-release; do
    [ -n "$(job_block "$job")" ] || fail "$wf: no job '$job'"
done

# Pinned by 40-hex commit SHA, as every other workflow in the repo.
while IFS= read -r line; do
    ref="${line#*uses:}"
    ref="$(echo "$ref" | awk '{print $1}')"
    echo "$ref" | grep -Eq '@[0-9a-f]{40}$' || fail "$wf: action not pinned by commit SHA: $ref"
done < <(grep -E '^[[:space:]-]*uses:' "$wf")

build="$(job_block build)"
attest="$(job_block attest)"
draft="$(job_block draft-release)"

# Top level grants read only; jobs opt in.
top="$(awk '/^permissions:/{on=1;next} on&&/^[^ ]/{exit} on{print}' "$wf")"
echo "$top" | grep -Eq 'contents:[[:space:]]*read' || fail "$wf: top-level permissions are not contents: read"
echo "$top" | grep -Eq 'write' && fail "$wf: top-level permissions grant write"

echo "$build" | grep -Eq 'write' && fail "build job holds a write permission"
echo "$build" | grep -q 'scripts/release.sh' || fail "build job does not run scripts/release.sh"
echo "$build" | grep -q 'SHA256SUMS' || fail "build job does not upload SHA256SUMS"
echo "$build" | grep -Eq '\.deb' || fail "build job does not upload the .deb"

echo "$attest" | grep -q 'actions/attest-build-provenance@' || fail "attest job does not use actions/attest-build-provenance"
echo "$attest" | grep -Eq 'id-token:[[:space:]]*write' || fail "attest job lacks id-token: write"
echo "$attest" | grep -Eq 'attestations:[[:space:]]*write' || fail "attest job lacks attestations: write"
echo "$attest" | grep -Eq 'contents:[[:space:]]*write' && fail "attest job holds contents: write"
echo "$attest" | grep -Eq 'environment:' && fail "attest job runs in an environment"

echo "$draft" | grep -Eq '^[[:space:]]+environment:[[:space:]]*release[[:space:]]*$' \
    || fail "draft-release job does not run in environment: release"
echo "$draft" | grep -Eq 'contents:[[:space:]]*write' || fail "draft-release job lacks contents: write"
echo "$draft" | grep -Eq 'id-token:|attestations:[[:space:]]*write' && fail "draft-release job holds attestation permissions"
echo "$draft" | grep -q 'gh release create' || fail "draft-release job does not run gh release create"
echo "$draft" | grep -q -- '--draft' || fail "draft-release job does not create a DRAFT release"
echo "$draft" | grep -Eq 'gh release (edit|publish)|--draft=false' && fail "draft-release job publishes the release"
echo "$draft" | grep -q 'sha256sum -c' || fail "draft-release job does not check SHA256SUMS"
echo "$draft" | grep -q 'gh attestation verify' || fail "draft-release job does not verify the attestation"
echo "$draft" | grep -q 'SHA256SUMS' || fail "draft-release job does not attach SHA256SUMS"
echo "$draft" | grep -Eq 'needs:.*attest' || fail "draft-release job does not wait for attest"
echo "$attest" | grep -Eq 'needs:.*build' || fail "attest job does not wait for build"
# A dispatch run has no tag to release.
for j in "$attest" "$draft"; do
    echo "$j" | grep -q "startsWith(github.ref, 'refs/tags/v')" || fail "a release job is not limited to v* tags"
done

if [ "$fails" -ne 0 ]; then
    echo "FAIL ($fails)"
    exit 1
fi
[ "${1:-}" = "--static" ] && { echo "ok: release workflow (static)"; exit 0; }

echo "release workflow: dist from fake binaries"
for tool in dpkg-deb sha256sum strings; do
    command -v "$tool" >/dev/null 2>&1 || { echo "FAIL: $tool not found"; exit 1; }
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/target/release"
# Real ELF files so `strings` and dpkg-deb behave as on the real binaries.
cp /usr/bin/true "$work/target/release/agentcenter"
cp /usr/bin/true "$work/target/release/agentcenter-cli"
printf '<html>%0*d</html>\n' 2000 0 > "$work/target/THIRD_PARTY_LICENSES.html"

dist="$work/dist"
CARGO_TARGET_DIR="$work/target" RELEASE_DIST="$dist" SOURCE_DATE_EPOCH=1700000000 \
    bash scripts/release.sh --package-only >"$work/release.log" 2>&1 \
    || { cat "$work/release.log"; fail "scripts/release.sh --package-only failed"; echo "FAIL ($fails)"; exit 1; }

tarball="$(find "$dist" -name "agentcenter-*.tar.gz" | head -n1)"
deb="$(find "$dist" -name "agentcenter_*.deb" | head -n1)"
[ -n "$tarball" ] || fail "no tarball in dist"
[ -n "$deb" ] || fail "no .deb in dist"
[ -f "$dist/SHA256SUMS" ] || fail "no SHA256SUMS in dist"
[ "$fails" -eq 0 ] || { echo "FAIL ($fails)"; exit 1; }

echo "release workflow: sha256sum -c"
grep -q "$(basename "$tarball")" "$dist/SHA256SUMS" || fail "SHA256SUMS does not list the tarball"
grep -q "$(basename "$deb")" "$dist/SHA256SUMS" || fail "SHA256SUMS does not list the .deb"
grep -q '/' "$dist/SHA256SUMS" && fail "SHA256SUMS holds a path, not a bare file name"
(cd "$dist" && sha256sum -c SHA256SUMS >/dev/null) || fail "sha256sum -c fails on a fresh dist"
mkdir "$work/download"
cp "$dist"/* "$work/download/"
(cd "$work/download" && sha256sum -c SHA256SUMS >/dev/null) || fail "sha256sum -c fails on a copy of the dist"
printf 'x' >> "$work/download/$(basename "$deb")"
if (cd "$work/download" && sha256sum -c SHA256SUMS >/dev/null 2>&1); then
    fail "sha256sum -c accepts a tampered .deb"
fi

echo "release workflow: .deb contents"
listing="$(dpkg-deb -c "$deb")"
for path in ./usr/bin/agentcenter ./usr/bin/agentcenter-cli \
    ./usr/share/applications/io.github.eduardopin.AgentCenter.desktop \
    ./usr/share/metainfo/io.github.eduardopin.AgentCenter.metainfo.xml \
    ./usr/share/glib-2.0/schemas/io.github.eduardopin.AgentCenter.gschema.xml \
    ./usr/share/doc/agentcenter/THIRD_PARTY_LICENSES.html; do
    echo "$listing" | grep -Eq "[[:space:]]${path}\$" || fail ".deb does not list $path"
done
echo "$listing" | grep -E '[[:space:]]\./usr/bin/agentcenter$' | grep -q '^-rwxr-xr-x root/root' \
    || fail ".deb binary is not root-owned 0755"
dpkg-deb -f "$deb" Package | grep -qx agentcenter || fail ".deb Package field is not agentcenter"
dpkg-deb -f "$deb" Depends | grep -q 'libgtk-4-1' || fail ".deb does not depend on libgtk-4-1"
dpkg-deb -f "$deb" Depends | grep -q 'libadwaita-1-0' || fail ".deb does not depend on libadwaita-1-0"

echo "release workflow: same bytes twice"
sum1="$(sha256sum < "$deb")"
rm -rf "$work/dist2"
CARGO_TARGET_DIR="$work/target" RELEASE_DIST="$work/dist2" SOURCE_DATE_EPOCH=1700000000 \
    bash scripts/release.sh --package-only >/dev/null 2>&1
[ "$sum1" = "$(sha256sum < "$work/dist2/$(basename "$deb")")" ] || fail ".deb differs between two builds"
cmp -s "$dist/SHA256SUMS" "$work/dist2/SHA256SUMS" || fail "SHA256SUMS differs between two builds"

echo "release workflow: path check covers the .deb"
if ! RELEASE_SKIP_BUILD=1 RELEASE_DIST="$dist" CARGO_TARGET_DIR="$work/none" \
    bash scripts/test-release-paths.sh >"$work/paths.log" 2>&1; then
    cat "$work/paths.log"; fail "test-release-paths.sh fails on a clean dist"
fi
grep -q 'strings on the .deb' "$work/paths.log" || fail "test-release-paths.sh does not look inside the .deb"

if [ "$fails" -ne 0 ]; then
    echo "FAIL ($fails)"
    exit 1
fi
echo "ok: release workflow and dist"

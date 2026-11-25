#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

wf=.github/workflows/release.yml
fails=0
fail() { echo " - $*"; fails=$((fails + 1)); }

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

while IFS= read -r line; do
    ref="${line#*uses:}"
    ref="$(echo "$ref" | awk '{print $1}')"
    echo "$ref" | grep -Eq '@[0-9a-f]{40}$' || fail "$wf: action not pinned by commit SHA: $ref"
done < <(grep -E '^[[:space:]-]*uses:' "$wf")

build="$(job_block build)"
attest="$(job_block attest)"
draft="$(job_block draft-release)"

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
for glob in 'dist/agentcenter-\*\.tar\.gz' 'dist/agentcenter_\*\.deb'; do
    echo "$attest" | grep -Eq "^[[:space:]]+$glob\$" || fail "attest subject-path lacks $glob"
    echo "$build" | grep -Eq "^[[:space:]]+$glob\$" || fail "build upload path lacks $glob"
done
echo "$draft" | grep -q -- '--prerelease' || fail "draft-release job has no --prerelease for hyphenated tags"
echo "$draft" | grep -q -- '--verify-tag' || fail "draft-release job does not pass --verify-tag"
echo "$draft" | grep -qF -- "--signer-workflow \"\$GITHUB_REPOSITORY/.github/workflows/release.yml\"" \
    || fail "gh attestation verify does not pin --signer-workflow"
grep -Eq "^[[:space:]]+cancel-in-progress:[[:space:]]*\\$\{\{ !startsWith\(github.ref, 'refs/tags/'\) \}\}" "$wf" \
    || fail "$wf: cancel-in-progress can cancel a tag run waiting for approval"
inject="$(awk '
    function indent(l) { match(l, /^ */); return RLENGTH }
    block && NF && indent($0) <= base { block = 0 }
    block && /\$\{\{[[:space:]]*github\./ { print NR ": " $0 }
    !block && /^[[:space:]-]*run:/ {
        if ($0 ~ /\$\{\{[[:space:]]*github\./) print NR ": " $0
        if ($0 ~ /run:[[:space:]]*[|>]/) { block = 1; base = indent($0) }
    }
' "$wf")"
[ -z "$inject" ] || fail "$wf: github.* expression inside run: $inject"
for j in "$attest" "$draft"; do
    echo "$j" | grep -q "startsWith(github.ref, 'refs/tags/v')" || fail "a release job is not limited to v* tags"
done

if [ "$fails" -ne 0 ]; then
    echo "FAIL ($fails)"
    exit 1
fi
grep -Eq '^bash scripts/test-release-workflow\.sh$' scripts/ci.sh \
    || fail "scripts/ci.sh: does not run the full release workflow test, so the .deb checks never run in CI"
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

echo "release workflow: tag must match the Cargo version"
cargo_version="$(cargo metadata --format-version 1 --no-deps \
    | python3 -I -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "agentcenter"))')"
if GITHUB_REF_TYPE=tag GITHUB_REF_NAME="v$cargo_version-rc.1" CARGO_TARGET_DIR="$work/target" \
    RELEASE_DIST="$work/dist-bad" bash scripts/release.sh --package-only >/dev/null 2>&1; then
    fail "release.sh accepts a tag that differs from the Cargo version"
fi
[ ! -e "$work/dist-bad" ] || fail "release.sh built files for a mismatched tag"
GITHUB_REF_TYPE=tag GITHUB_REF_NAME="v$cargo_version" CARGO_TARGET_DIR="$work/target" \
    RELEASE_DIST="$work/dist-ok" SOURCE_DATE_EPOCH=1700000000 bash scripts/release.sh --package-only >/dev/null 2>&1 \
    || fail "release.sh rejects a tag equal to the Cargo version"

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
dpkg-deb -f "$deb" Depends | grep -q 'libgtk-4-1 (>= 4.22)' || fail ".deb lacks the libgtk-4-1 (>= 4.22) bound"
dpkg-deb -f "$deb" Depends | grep -q 'libadwaita-1-0 (>= 1.9)' || fail ".deb lacks the libadwaita-1-0 (>= 1.9) bound"
mkdir "$work/debx"
dpkg-deb -x "$deb" "$work/debx"
[ "$(od -An -tx1 -j4 -N4 "$work/debx/usr/share/man/man1/agentcenter-cli.1.gz" | tr -d ' \n')" = "00000000" ] \
    || fail "man page gzip header carries an mtime"
cp "$tarball" "$work/agentcenter-$cargo_version-rc.1-$(uname -m)-linux.tar.gz"
RELEASE_DIST="$work/dist-rc" SOURCE_DATE_EPOCH=1700000000 \
    bash scripts/release-deb.sh "$work/agentcenter-$cargo_version-rc.1-$(uname -m)-linux.tar.gz" >/dev/null
rc_deb="$(find "$work/dist-rc" -name '*.deb' | head -n1)"
[ "$(dpkg-deb -f "$rc_deb" Version)" = "$cargo_version~rc.1" ] || fail "prerelease .deb Version is not $cargo_version~rc.1"
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

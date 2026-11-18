#!/usr/bin/env bash
# Checks the AppStream metainfo, the .desktop entry and the icon set in data/.
#
#   bash scripts/test-appstream.sh
#
# appstreamcli validate --pedantic must exit 0 and report nothing beyond the
# allowed hints below; desktop-file-validate must exit 0; icons exist at 16, 32,
# 48, 128 and 256 px (PNG, with the size the directory says) and scalable (SVG);
# the metainfo has a release for the Cargo workspace version, an OARS rating, and
# https screenshots whose files exist under site/img in this checkout.
# appstreamcli runs with --no-net: the screenshots point at main, which a branch
# may not have pushed yet, and a test must not depend on the network.
# A missing validator is skipped with a printed reason on a workstation; under CI
# (CI=true) it fails, so the check cannot pass by being skipped.
# Called by scripts/ci.sh.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

APP_ID=io.github.eduardopin.AgentCenter
meta="data/$APP_ID.metainfo.xml"
desktop="data/$APP_ID.desktop"
icons="data/icons/hicolor"

fail() { echo "test-appstream: FAIL: $*" >&2; exit 1; }
skip() { [[ ${CI:-} == true ]] && fail "$* (CI must install it)"; echo "skip: $*"; }
ok() { echo "ok: $*"; }

[[ -f $meta ]] || fail "missing $meta"

# The component id is fixed by the .desktop name and the flatpak/GitHub id
# scheme, so this one pedantic hint cannot be fixed; anything else can.
allowed_hints='cid-contains-uppercase-letter'
if command -v appstreamcli >/dev/null 2>&1; then
    rc=0
    out="$(appstreamcli validate --pedantic --no-net "$meta" 2>&1)" || rc=$?
    [[ $rc == 0 ]] || { echo "$out" >&2; fail "appstreamcli validate --pedantic --no-net exited $rc"; }
    extra="$(grep -E '^[EWIP]: ' <<<"$out" | grep -Ev "^P: [^ ]+ ($allowed_hints)( |$)" || true)"
    [[ -z $extra ]] || { echo "$extra" >&2; fail "appstreamcli reported findings"; }
    ok "appstreamcli validate --pedantic --no-net"
else
    skip "appstreamcli not found (apt install appstream)"
fi

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$desktop" || fail "desktop-file-validate"
    ok "desktop-file-validate"
else
    skip "desktop-file-validate not found (apt install desktop-file-utils)"
fi

for size in 16 32 48 128 256; do
    png="$icons/${size}x${size}/apps/$APP_ID.png"
    [[ -f $png ]] || fail "missing icon $png"
    dims="$(python3 -I -c '
import struct, sys
d = open(sys.argv[1], "rb").read(24)
assert d[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
print(*struct.unpack(">II", d[16:24]))' "$png")" || fail "$png is not a PNG"
    [[ $dims == "$size $size" ]] || fail "$png is $dims, want $size $size"
done
svg="$icons/scalable/apps/$APP_ID.svg"
{ [[ -f $svg ]] && grep -q "<svg" "$svg"; } || fail "missing or bad $svg"
ok "icons at 16, 32, 48, 128, 256 and scalable"

version="$(python3 -I -c '
import sys, tomllib
print(tomllib.load(open(sys.argv[1], "rb"))["workspace"]["package"]["version"])' Cargo.toml)" ||
    fail "no [workspace.package] version in Cargo.toml"

python3 -I - "$meta" "$version" <<'PY' || fail "metainfo content"
import os
import sys
import xml.etree.ElementTree as ET

meta, version = sys.argv[1], sys.argv[2]
root = ET.parse(meta).getroot()
releases = {r.get("version"): r for r in root.findall("./releases/release")}
assert version in releases, "no release %s (the Cargo workspace version)" % version
assert releases[version].get("date"), "release %s has no date" % version
rating = root.find("content_rating")
assert rating is not None and rating.get("type", "").startswith("oars-"), "no OARS rating"
shots = root.findall("./screenshots/screenshot/image")
assert shots, "no screenshots"
prefix = "https://raw.githubusercontent.com/eduardopin/agentcenter/main/"
for s in shots:
    url = (s.text or "").strip()
    assert url.startswith(prefix), "screenshot is not under %s: %r" % (prefix, url)
    path = url[len(prefix):]
    assert os.path.isfile(path), "screenshot %s is not in the repository" % path
assert root.find("./screenshots/screenshot[@type='default']") is not None, "no default screenshot"
PY
ok "metainfo has release $version, OARS rating and screenshots in site/img"

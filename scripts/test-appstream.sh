#!/usr/bin/env bash
# Checks the AppStream metainfo, the .desktop entry and the icon set in data/.
#
#   bash scripts/test-appstream.sh
#
# appstreamcli validate --pedantic must exit 0 and report nothing beyond the
# allowed hints below; desktop-file-validate must exit 0; icons exist at 16, 32,
# 48, 128 and 256 px (PNG, with the size the directory says) and scalable (SVG);
# the metainfo has a 1.0.0 release, an OARS rating and https screenshots.
# A missing validator is skipped with a printed reason (CI installs both).
# Called by scripts/ci.sh.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

APP_ID=io.github.eduardopin.AgentCenter
meta="data/$APP_ID.metainfo.xml"
desktop="data/$APP_ID.desktop"
icons="data/icons/hicolor"

fail() { echo "test-appstream: FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }

[[ -f $meta ]] || fail "missing $meta"

# The component id is fixed by the .desktop name and the flatpak/GitHub id
# scheme, so this one pedantic hint cannot be fixed; anything else can.
allowed_hints='cid-contains-uppercase-letter'
if command -v appstreamcli >/dev/null 2>&1; then
    rc=0
    out="$(appstreamcli validate --pedantic "$meta" 2>&1)" || rc=$?
    [[ $rc == 0 ]] || { echo "$out" >&2; fail "appstreamcli validate --pedantic exited $rc"; }
    extra="$(grep -E '^[EWIP]: ' <<<"$out" | grep -Ev "^P: [^ ]+ ($allowed_hints)( |$)" || true)"
    [[ -z $extra ]] || { echo "$extra" >&2; fail "appstreamcli reported findings"; }
    ok "appstreamcli validate --pedantic"
else
    echo "skip: appstreamcli not found (apt install appstream)"
fi

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$desktop" || fail "desktop-file-validate"
    ok "desktop-file-validate"
else
    echo "skip: desktop-file-validate not found (apt install desktop-file-utils)"
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
[[ -f $svg ]] && grep -q '<svg' "$svg" || fail "missing or bad $svg"
ok "icons at 16, 32, 48, 128, 256 and scalable"

python3 -I - "$meta" <<'PY' || fail "metainfo content"
import sys
import xml.etree.ElementTree as ET

root = ET.parse(sys.argv[1]).getroot()
releases = {r.get("version"): r for r in root.findall("./releases/release")}
assert "1.0.0" in releases, "no release 1.0.0"
assert releases["1.0.0"].get("date"), "release 1.0.0 has no date"
rating = root.find("content_rating")
assert rating is not None and rating.get("type", "").startswith("oars-"), "no OARS rating"
shots = root.findall("./screenshots/screenshot/image")
assert shots, "no screenshots"
for s in shots:
    assert (s.text or "").strip().startswith("https://"), "screenshot is not https: %r" % s.text
assert root.find("./screenshots/screenshot[@type='default']") is not None, "no default screenshot"
PY
ok "metainfo has release 1.0.0, OARS rating and https screenshots"

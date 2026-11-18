#!/usr/bin/env bash
# Renders the PNG app icons (16, 32, 48, 128, 256 px) from the scalable SVG,
# so a change to the SVG reaches every size. Needs headless Chrome or Chromium.
#
#   bash scripts/render-icons.sh
#
# Writes data/icons/hicolor/<n>x<n>/apps/io.github.eduardopin.AgentCenter.png;
# commit them together with the SVG.
#
# The SVG is drawn on a canvas and read back with toDataURL, not taken with
# --screenshot: headless Chrome paints only part of a small window (a 128 px
# window came out cropped to its top 45 px), so the window size cannot be
# trusted to equal the icon size.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

APP_ID=io.github.eduardopin.AgentCenter
icons="$PWD/data/icons/hicolor"
svg="$icons/scalable/apps/$APP_ID.svg"
chrome="$(command -v google-chrome || command -v chromium || command -v chromium-browser || true)"
[[ -n $chrome ]] || { echo "render-icons: needs google-chrome or chromium" >&2; exit 1; }
[[ -f $svg ]] || { echo "render-icons: missing $svg" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
# A data: URL keeps the canvas untainted, so toDataURL is allowed.
src="data:image/svg+xml;base64,$(base64 -w0 "$svg")"
for n in 16 32 48 128 256; do
    cat >"$tmp/i.html" <<EOF
<!doctype html><html><body><script>
const img = new Image();
img.onload = () => {
  const c = document.createElement('canvas');
  c.width = c.height = $n;
  c.getContext('2d').drawImage(img, 0, 0, $n, $n);
  const out = document.createElement('pre');
  out.id = 'out';
  out.textContent = c.toDataURL('image/png').split(',')[1];
  document.body.appendChild(out);
};
img.src = '$src';
</script></body></html>
EOF
    out="$icons/${n}x${n}/apps/$APP_ID.png"
    mkdir -p "$(dirname "$out")"
    "$chrome" --headless=new --disable-gpu --no-sandbox --user-data-dir="$tmp/profile" \
        --virtual-time-budget=5000 --dump-dom "file://$tmp/i.html" 2>/dev/null |
        sed -n 's|.*<pre id="out">\([^<]*\)</pre>.*|\1|p' | base64 -d >"$tmp/out.png"
    # An empty or non-PNG result means the page never drew; keep the old file.
    [[ "$(head -c 8 "$tmp/out.png" | od -An -tx1 | tr -d ' \n')" == 89504e470d0a1a0a ]] ||
        { echo "render-icons: no PNG for $n px" >&2; exit 1; }
    mv "$tmp/out.png" "$out"
    echo "wrote $out"
done

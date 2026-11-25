#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
root="$(pwd)"

tarball="${1:-}"
[ -f "$tarball" ] || { echo "usage: $0 <release.tar.gz>" >&2; exit 2; }
tarball="$(cd "$(dirname "$tarball")" && pwd)/$(basename "$tarball")"
dist="${RELEASE_DIST:-dist}"

APP_ID=io.github.eduardopin.AgentCenter
version="$(basename "$tarball" | sed -E 's/^agentcenter-(.+)-[^-]+-linux\.tar\.gz$/\1/')"
deb_version="${version/-/\~}"
case "$(uname -m)" in
    x86_64) arch=amd64 ;;
    aarch64) arch=arm64 ;;
    *) echo "release-deb: no Debian architecture for $(uname -m)" >&2; exit 2 ;;
esac

if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH="$(git log -1 --format=%ct 2>/dev/null || echo 0)"
fi
export SOURCE_DATE_EPOCH

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
tar -xzf "$tarball" -C "$stage"
src="$(find "$stage" -mindepth 1 -maxdepth 1 -type d | head -n1)"
pkg="$stage/pkg"

install -d "$pkg/DEBIAN" "$pkg/usr/bin" "$pkg/usr/share/doc/agentcenter" \
    "$pkg/usr/share/applications" "$pkg/usr/share/metainfo" \
    "$pkg/usr/share/glib-2.0/schemas" "$pkg/usr/share/man/man1"
install -m 0755 "$src/agentcenter" "$src/agentcenter-cli" "$pkg/usr/bin/"
install -m 0644 "$src/THIRD_PARTY_LICENSES.html" "$src/LICENSE" "$src/NOTICE" "$pkg/usr/share/doc/agentcenter/"
install -m 0644 "$root/data/$APP_ID.desktop" "$pkg/usr/share/applications/"
install -m 0644 "$root/data/$APP_ID.metainfo.xml" "$pkg/usr/share/metainfo/"
install -m 0644 "$root/data/$APP_ID.gschema.xml" "$pkg/usr/share/glib-2.0/schemas/"
gzip -n -9 -c "$root/man/agentcenter-cli.1" > "$pkg/usr/share/man/man1/agentcenter-cli.1.gz"
chmod 0644 "$pkg/usr/share/man/man1/agentcenter-cli.1.gz"
(cd "$root/data/icons" && find . -type f -print0 | while IFS= read -r -d '' f; do
    install -D -m 0644 "$f" "$pkg/usr/share/icons/$f"
done)

size_kb="$(du -sk --exclude=DEBIAN "$pkg" | cut -f1)"
cat > "$pkg/DEBIAN/control" <<CONTROL
Package: agentcenter
Version: $deb_version
Architecture: $arch
Maintainer: AgentCenter maintainers <noreply@users.noreply.github.com>
Installed-Size: $size_kb
Depends: libgtk-4-1 (>= 4.22), libadwaita-1-0 (>= 1.9)
Section: utils
Priority: optional
Homepage: https://github.com/eduardopin/agentcenter
Description: See what each AI coding session costs your machine
 AgentCenter is a GTK4 + libadwaita resource monitor for AI coding
 sessions, with a companion command line tool, agentcenter-cli.
CONTROL

find "$pkg" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
mkdir -p "$dist"
out="$dist/agentcenter_${deb_version}_${arch}.deb"
dpkg-deb --root-owner-group -Zxz --build "$pkg" "$out" >/dev/null
echo "release: $out"

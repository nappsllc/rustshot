#!/usr/bin/env bash
# Build dist/rustshot-<version>-amd64.deb from target/release/rustshot.
set -euo pipefail
cd "$(dirname "$0")/../.."

case "$(uname -s)" in
  Linux) ;;
  *) echo "error: deb.sh must run on Linux (uname=$(uname -s))" >&2; exit 1 ;;
esac

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
bin=target/release/rustshot
[ -x "$bin" ] || { echo "error: $bin missing (cargo build --release first)" >&2; exit 1; }

stage="dist/stage-deb"
rm -rf "$stage"
mkdir -p "$stage/DEBIAN" \
         "$stage/usr/bin" \
         "$stage/usr/share/applications" \
         "$stage/usr/share/metainfo" \
         "$stage/usr/share/doc/rustshot"

install -m755 "$bin" "$stage/usr/bin/rustshot"
install -m644 packaging/linux/io.github.nappsllc.rustshot.desktop "$stage/usr/share/applications/io.github.nappsllc.rustshot.desktop"
install -m644 packaging/linux/io.github.nappsllc.rustshot.metainfo.xml "$stage/usr/share/metainfo/io.github.nappsllc.rustshot.metainfo.xml"
for s in 128 256 512; do
  install -Dm644 "packaging/icons/rustshot-$s.png" "$stage/usr/share/icons/hicolor/${s}x${s}/apps/io.github.nappsllc.rustshot.png"
done
install -m644 LICENSE "$stage/usr/share/doc/rustshot/copyright"
install -m644 THIRD_PARTY_NOTICES.md "$stage/usr/share/doc/rustshot/THIRD_PARTY_NOTICES.md"

cat > "$stage/DEBIAN/control" <<EOF
Package: rustshot
Version: $version
Section: utils
Priority: optional
Architecture: amd64
Maintainer: nappsllc <noreply@github.com>
Depends: libx11-6, libxrandr2
Installed-Size: $(du -sk "$stage/usr" | cut -f1)
Description: screenshot tool with annotation and upload
 rustshot captures the screen or a region, lets you annotate the
 shot (arrows, text, shapes, marker, blur), then saves it, copies
 it to the clipboard or uploads it to imgur. Global hotkeys are
 provided by the background daemon.
EOF

mkdir -p dist
dpkg-deb --build --root-owner-group "$stage" "dist/rustshot-$version-amd64.deb"
echo "wrote dist/rustshot-$version-amd64.deb"

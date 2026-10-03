#!/usr/bin/env bash
# Build dist/rustshot-<version>-x86_64.AppImage from target/release/rustshot.
# Downloads appimagetool (its own AppImage, run with APPIMAGE_EXTRACT_AND_RUN=1
# so no FUSE is required on CI).
set -euo pipefail
cd "$(dirname "$0")/../.."

case "$(uname -s)" in
  Linux) ;;
  *) echo "error: appimage.sh must run on Linux (uname=$(uname -s))" >&2; exit 1 ;;
esac

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
bin=target/release/rustshot
[ -x "$bin" ] || { echo "error: $bin missing (cargo build --release first)" >&2; exit 1; }

stage="dist/AppDir"
rm -rf "$stage"
mkdir -p "$stage"
cp "$bin" "$stage/rustshot"
chmod 755 "$stage/rustshot"
cp packaging/linux/rustshot.desktop "$stage/rustshot.desktop"
cp packaging/icons/rustshot-256.png "$stage/rustshot.png"
ln -sf rustshot.png "$stage/.DirIcon"
ln -sf rustshot "$stage/AppRun"

tool="dist/appimagetool.AppImage"
if [ ! -x "$tool" ]; then
  curl -fsSL -o "$tool" \
    https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "$tool"
fi

mkdir -p dist
export ARCH=x86_64
export APPIMAGE_EXTRACT_AND_RUN=1
"$tool" --no-appstream "$stage" "dist/rustshot-$version-x86_64.AppImage"
echo "wrote dist/rustshot-$version-x86_64.AppImage"

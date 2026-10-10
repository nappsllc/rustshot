#!/usr/bin/env bash
# Package dist/Rustshot.app (from bundle.sh, optionally signed/notarized
# by CI before this runs) into:
#   dist/rustshot-<version>-macos-universal.zip
#   dist/rustshot-<version>-macos-universal.dmg   (drag-to-Applications layout)
set -euo pipefail
cd "$(dirname "$0")/../.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
app="dist/Rustshot.app"
[ -d "$app" ] || { echo "error: $app missing (run packaging/macos/bundle.sh first)" >&2; exit 1; }

mkdir -p dist
(cd dist && zip -qry "rustshot-$version-macos-universal.zip" Rustshot.app)

staging="dist/dmg-staging"
rm -rf "$staging"
mkdir -p "$staging"
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -volname "Rustshot $version" -srcfolder "$staging" -ov -format UDZO \
  "dist/rustshot-$version-macos-universal.dmg"
rm -rf "$staging"

echo "wrote dist/rustshot-$version-macos-universal.zip and .dmg"

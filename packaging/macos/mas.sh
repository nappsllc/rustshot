#!/usr/bin/env bash
# Sign a copy of dist/Rustshot.app for the Mac App Store (App Sandbox) and
# wrap it in a signed installer package:
#   dist/rustshot-<version>-mas.pkg
# Env: MAS_TEAM_ID, MAS_APP_IDENTITY ("Apple Distribution: …"),
#      MAS_INSTALLER_IDENTITY ("3rd Party Mac Developer Installer: …"),
#      MAS_PROFILE (path to the Mac App Store .provisionprofile).
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${MAS_TEAM_ID:?}" "${MAS_APP_IDENTITY:?}" "${MAS_INSTALLER_IDENTITY:?}" "${MAS_PROFILE:?}"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
src="dist/Rustshot.app"
[ -d "$src" ] || { echo "error: $src missing (run packaging/macos/bundle.sh first)" >&2; exit 1; }

work="dist/mas"
rm -rf "$work"
mkdir -p "$work"
app="$work/Rustshot.app"
cp -R "$src" "$app"
cp "$MAS_PROFILE" "$app/Contents/embedded.provisionprofile"

ent="$work/rustshot.entitlements"
cp packaging/macos/mas.entitlements "$ent"
/usr/libexec/PlistBuddy -c "Add :com.apple.application-identifier string $MAS_TEAM_ID.io.github.nappsllc.rustshot" "$ent"
/usr/libexec/PlistBuddy -c "Add :com.apple.developer.team-identifier string $MAS_TEAM_ID" "$ent"

codesign --force --timestamp --options runtime --entitlements "$ent" --sign "$MAS_APP_IDENTITY" "$app"
codesign --verify --strict --verbose=2 "$app"
productbuild --component "$app" /Applications --sign "$MAS_INSTALLER_IDENTITY" \
  "dist/rustshot-$version-mas.pkg"
pkgutil --check-signature "dist/rustshot-$version-mas.pkg"
rm -rf "$work"
echo "wrote dist/rustshot-$version-mas.pkg"

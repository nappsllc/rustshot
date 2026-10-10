#!/usr/bin/env bash
# Bundle the macOS release into Rustshot.app (universal binary) + zip:
#   dist/rustshot-<version>-macos-universal.zip
# Usage: packaging/macos/bundle.sh   (run after cargo build --release)
set -euo pipefail
cd "$(dirname "$0")/../.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
build="${BUILD_NUMBER:-$version}"

# Build both architectures when possible; fall back to the host arch.
rustup target add x86_64-apple-darwin aarch64-apple-darwin >/dev/null 2>&1 || true
built=()
for t in x86_64-apple-darwin aarch64-apple-darwin; do
  if cargo build --release --target "$t"; then
    built+=("target/$t/release/rustshot")
  fi
done
[ ${#built[@]} -gt 0 ] || { echo "error: no macos binary built" >&2; exit 1; }

bin=""
if [ ${#built[@]} -eq 2 ]; then
  bin="dist/rustshot-universal"
  mkdir -p dist
  lipo -create "${built[0]}" "${built[1]}" -output "$bin"
else
  bin="${built[0]}"
fi

app="dist/Rustshot.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/rustshot"
cp LICENSE "$app/Contents/Resources/LICENSE"
cp THIRD_PARTY_NOTICES.md "$app/Contents/Resources/THIRD_PARTY_NOTICES.md"
iconset="dist/rustshot.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  cp "packaging/icons/rustshot-$s.png" "$iconset/icon_${s}x${s}.png"
  cp "packaging/icons/rustshot-$((s * 2)).png" "$iconset/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/rustshot.icns"
rm -rf "$iconset"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>rustshot</string>
  <key>CFBundleIdentifier</key><string>io.github.nappsllc.rustshot</string>
  <key>CFBundleName</key><string>Rustshot</string>
  <key>CFBundleDisplayName</key><string>Rustshot</string>
  <key>CFBundleIconFile</key><string>rustshot</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$build</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSHighResolutionCapable</key><true/>
  <key>LSUIElement</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>NSHumanReadableCopyright</key><string>GPL-3.0-only</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>ITSAppUsesNonExemptEncryption</key><false/>
</dict>
</plist>
EOF

echo "built $app"

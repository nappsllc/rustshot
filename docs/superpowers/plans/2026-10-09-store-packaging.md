# Store & Package-Manager Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build every distribution format the spec (`docs/superpowers/specs/2026-10-09-store-packaging-design.md`) lists — MSIX, Mac App Store pkg, Flatpak, Snap, winget, Homebrew cask, AUR — in GitHub Actions, and publish each one from `release.yml` once its account exists.

**Architecture:** Shared metadata (app id `io.github.nappsllc.rustshot`, AppStream metainfo, desktop file, icon set, privacy policy) lives under `packaging/` and is consumed by every format. `ci.yml` builds and uploads every artifact on push/PR; `release.yml` reuses it on `v*` tags, attaches all artifacts to the GitHub Release, then runs one publish job per channel, each gated by a repository variable `PUBLISH_<CHANNEL> == 'true'`. Account values come from repo variables (identity) and secrets (credentials); without them builds use placeholders and publish jobs are skipped.

**Tech Stack:** GitHub Actions; Windows SDK `makeappx`/`signtool`, `msstore` CLI, `wingetcreate`; macOS `codesign`/`productbuild`/`iconutil`/`altool`; `flatpak-builder` (freedesktop 24.08 + rust-stable), `snapcraft` (core24), `makepkg`; PowerShell `System.Drawing` for icons.

## Global Constraints

- App id `io.github.nappsllc.rustshot` everywhere (bundle id, desktop file, icon name, AppStream id, Flatpak id). Snap name `rustshot`; winget id `nappsllc.rustshot`; AUR package `rustshot-bin`.
- Version comes only from `Cargo.toml` (`x.y.z`, no suffix). MSIX uses `x.y.z.0`; macOS `CFBundleVersion` is `BUILD_NUMBER` (CI: `github.run_number`) or the version locally.
- Every publish job: `if: vars.PUBLISH_<CHANNEL> == 'true'`. Missing secrets must never fail a push/PR build — signing steps print a skip message and `exit 0`, like the existing notarization step.
- `LICENSE` and `THIRD_PARTY_NOTICES.md` ship inside every package.
- Third-party actions pinned to a major tag; downloaded tools come only from their official release URLs.
- Work after the UI plan (`2026-10-09-modern-ui.md`), which creates `THIRD_PARTY_NOTICES.md`; store screenshots come from the new UI.
- Local verification is possible only on Windows (MSIX). Linux/macOS steps are verified by the CI jobs they add — push the branch and check the named job.
- Commit after every task; messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `packaging/linux/io.github.nappsllc.rustshot.desktop` | rename of `rustshot.desktop` | launcher; `Icon=io.github.nappsllc.rustshot` |
| `packaging/linux/io.github.nappsllc.rustshot.metainfo.xml` | new | AppStream (Flathub, Snap, deb) |
| `PRIVACY.md` | new | privacy policy linked from store listings |
| `THIRD_PARTY_NOTICES.md` | modify | add Rust crate licenses |
| `packaging/icons/make-icon.ps1` | modify | `-Size`, `-CanvasW`, `-Out` parameters |
| `packaging/icons/make-all.ps1` | new | regenerates every committed icon |
| `packaging/icons/rustshot-{16..1024}.png`, `packaging/icons/msix/*.png` | new (generated) | icon set |
| `packaging/linux/{package,deb,appimage}.sh` | modify | app-id names, metainfo, icon sizes, notices |
| `packaging/macos/bundle.sh` | modify | bundle id, iconutil icns, plist keys, notices |
| `packaging/macos/mas.entitlements`, `packaging/macos/mas.sh` | new | App Sandbox signing + pkg |
| `packaging/windows/AppxManifest.xml.in`, `packaging/windows/msix.ps1` | new | MSIX |
| `packaging/installer.nsi` | modify | ship notices |
| `packaging/flatpak/io.github.nappsllc.rustshot.yml`, `packaging/flatpak/cargo-sources.json` | new | Flatpak |
| `snap/snapcraft.yaml` | new | Snap |
| `packaging/homebrew/rustshot.rb.in` | new | cask template |
| `packaging/aur/PKGBUILD.in` | new | AUR template |
| `.github/workflows/ci.yml` | modify | metainfo validation, MSIX, MAS, flatpak, snap jobs |
| `.github/workflows/release.yml` | modify | per-channel publish jobs |
| `docs/STORES.md` | new | account/variable/secret checklist |

---

### Task 1: App-id metadata (desktop file, AppStream, privacy, notices)

**Files:**
- Rename: `packaging/linux/rustshot.desktop` → `packaging/linux/io.github.nappsllc.rustshot.desktop`
- Create: `packaging/linux/io.github.nappsllc.rustshot.metainfo.xml`, `PRIVACY.md`, `docs/screenshots/.gitkeep`
- Modify: `THIRD_PARTY_NOTICES.md`, `packaging/linux/package.sh`, `packaging/linux/deb.sh`, `packaging/linux/appimage.sh`, `packaging/installer.nsi`, `packaging/macos/bundle.sh`, `.github/workflows/ci.yml`

**Interfaces:**
- Produces: desktop id `io.github.nappsllc.rustshot.desktop`; icon name `io.github.nappsllc.rustshot`; metainfo path used by Tasks 5–6.

- [ ] **Step 1: Write the failing validation (CI)** — in `.github/workflows/ci.yml` `linux` job, change the install step to:

```yaml
      - name: Install X11 headers and validators
        run: sudo apt-get update && sudo apt-get install -y libx11-dev desktop-file-utils appstream
```

and add, right after it:

```yaml
      - name: Validate desktop entry and AppStream metadata
        run: |
          desktop-file-validate packaging/linux/io.github.nappsllc.rustshot.desktop
          appstreamcli validate --no-net packaging/linux/io.github.nappsllc.rustshot.metainfo.xml
```

Expected (before the files exist): the step fails with "No such file". Optional local check on Linux/WSL: same two commands.

- [ ] **Step 2: Rename and update the desktop file**

```bash
git mv packaging/linux/rustshot.desktop packaging/linux/io.github.nappsllc.rustshot.desktop
```

Change its `Icon=rustshot` line to `Icon=io.github.nappsllc.rustshot`.

- [ ] **Step 3: AppStream metainfo** — create `packaging/linux/io.github.nappsllc.rustshot.metainfo.xml`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>io.github.nappsllc.rustshot</id>
  <metadata_license>CC0-1.0</metadata_license>
  <project_license>GPL-3.0-only</project_license>
  <name>rustshot</name>
  <summary>Capture, annotate and share screenshots</summary>
  <developer id="io.github.nappsllc">
    <name>nappsllc</name>
  </developer>
  <description>
    <p>
      rustshot captures the whole screen or a region and lets you annotate it
      with arrows, lines, shapes, text, a highlighter and pixelation before
      saving it, copying it to the clipboard or uploading it to imgur.
    </p>
    <p>
      A small background daemon provides global hotkeys. rustshot currently
      needs an X11 session (or XWayland).
    </p>
  </description>
  <launchable type="desktop-id">io.github.nappsllc.rustshot.desktop</launchable>
  <url type="homepage">https://github.com/nappsllc/rustshot</url>
  <url type="bugtracker">https://github.com/nappsllc/rustshot/issues</url>
  <url type="vcs-browser">https://github.com/nappsllc/rustshot</url>
  <content_rating type="oars-1.1"/>
  <provides>
    <binary>rustshot</binary>
  </provides>
  <supports>
    <control>pointing</control>
    <control>keyboard</control>
  </supports>
  <categories>
    <category>Graphics</category>
    <category>Utility</category>
  </categories>
  <keywords>
    <keyword>screenshot</keyword>
    <keyword>capture</keyword>
    <keyword>annotate</keyword>
  </keywords>
  <screenshots>
    <screenshot type="default">
      <caption>Annotating a selection</caption>
      <image>https://raw.githubusercontent.com/nappsllc/rustshot/main/docs/screenshots/overlay-dark.png</image>
    </screenshot>
  </screenshots>
  <releases>
    <release version="0.1.0" date="2026-10-09"/>
  </releases>
</component>
```

Create an empty `docs/screenshots/.gitkeep` (the PNG is added after the UI redesign; `docs/STORES.md` tracks it).

- [ ] **Step 4: Privacy policy** — create `PRIVACY.md`:

```markdown
# rustshot privacy policy

rustshot does not collect, store or transmit personal data or telemetry.

- Screenshots stay on your computer unless you choose **Upload**. Upload sends
  only the selected image to imgur (https://imgur.com) using rustshot's
  public client id; imgur's privacy policy then applies to that image. The
  returned link is copied to your clipboard and printed to the console.
- Settings are stored locally in `config.toml` in your user config folder.
- rustshot makes no other network requests.

Questions: https://github.com/nappsllc/rustshot/issues
```

- [ ] **Step 5: Crate notices** — append to `THIRD_PARTY_NOTICES.md` (if the file does not exist yet, create it with a first line `# Third-party notices` and the sentence `rustshot is GPL-3.0-only (see LICENSE). It embeds the following works.`):

```markdown
## Rust crates (statically linked or build-time)

All are GPL-3.0-compatible (Apache-2.0, MIT, Zlib, 0BSD, Unicode-3.0).
Full texts: https://www.apache.org/licenses/LICENSE-2.0,
https://opensource.org/license/mit, https://zlib.net/zlib_license.html.

| Crate | License |
|---|---|
```

then append the rows generated from the lock file (they reflect the UI work's
dependency changes, e.g. `fdeflate` may be gone):

```bash
cargo metadata --format-version 1 --locked | python -c "
import json, sys
m = json.load(sys.stdin)
ws = set(m['workspace_members'])
for p in sorted(m['packages'], key=lambda p: (p['name'], p['version'])):
    if p['id'] not in ws:
        print(f\"| {p['name']} {p['version']} | {p['license']} |\")
" >> THIRD_PARTY_NOTICES.md
```

Expected: ~30 rows, among them `ab_glyph … | Apache-2.0 |`, `zlib-rs … | Zlib |`, `windows 0.62.2 | MIT OR Apache-2.0 |`. Any license outside the list in the paragraph above → stop and report it.

- [ ] **Step 6: Linux packages use the app id and ship metainfo + notices**

`packaging/linux/package.sh`: replace `cp packaging/linux/rustshot.desktop "$stage/"` with

```bash
cp packaging/linux/io.github.nappsllc.rustshot.desktop "$stage/"
cp packaging/icons/rustshot-256.png "$stage/io.github.nappsllc.rustshot.png"
cp THIRD_PARTY_NOTICES.md "$stage/"
```

and in its `INSTALL.txt` heredoc replace the step-2 line with

```
       install -Dm644 io.github.nappsllc.rustshot.desktop ~/.local/share/applications/io.github.nappsllc.rustshot.desktop
       install -Dm644 io.github.nappsllc.rustshot.png ~/.local/share/icons/hicolor/256x256/apps/io.github.nappsllc.rustshot.png
```

`packaging/linux/deb.sh`: in the `mkdir -p` list add `"$stage/usr/share/metainfo" \`; replace the desktop and icon `install` lines with

```bash
install -m644 packaging/linux/io.github.nappsllc.rustshot.desktop "$stage/usr/share/applications/io.github.nappsllc.rustshot.desktop"
install -m644 packaging/linux/io.github.nappsllc.rustshot.metainfo.xml "$stage/usr/share/metainfo/io.github.nappsllc.rustshot.metainfo.xml"
for s in 128 256 512; do
  install -Dm644 "packaging/icons/rustshot-$s.png" "$stage/usr/share/icons/hicolor/${s}x${s}/apps/io.github.nappsllc.rustshot.png"
done
install -m644 THIRD_PARTY_NOTICES.md "$stage/usr/share/doc/rustshot/THIRD_PARTY_NOTICES.md"
```

and drop `"$stage/usr/share/icons/hicolor/256x256/apps"` from the `mkdir -p` list (the loop's `install -D` creates it). Note: `rustshot-128.png`/`-512.png` arrive in Task 2 — Tasks 1 and 2 land in the same push.

`packaging/linux/appimage.sh`: replace the desktop/icon/`.DirIcon` lines with

```bash
cp packaging/linux/io.github.nappsllc.rustshot.desktop "$stage/io.github.nappsllc.rustshot.desktop"
cp packaging/icons/rustshot-256.png "$stage/io.github.nappsllc.rustshot.png"
cp THIRD_PARTY_NOTICES.md "$stage/"
ln -sf io.github.nappsllc.rustshot.png "$stage/.DirIcon"
```

`packaging/installer.nsi`: after `File "..\LICENSE"` add `File "..\THIRD_PARTY_NOTICES.md"`, and after `Delete "$INSTDIR\LICENSE"` add `Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"`.

`packaging/macos/bundle.sh`: after `cp LICENSE "$app/Contents/Resources/LICENSE"` add `cp THIRD_PARTY_NOTICES.md "$app/Contents/Resources/THIRD_PARTY_NOTICES.md"`.

- [ ] **Step 7: Commit** (verification happens with Task 2's push)

```bash
git add -A packaging PRIVACY.md THIRD_PARTY_NOTICES.md docs/screenshots .github/workflows/ci.yml
git commit -m "packaging: app id io.github.nappsllc.rustshot, AppStream metainfo, privacy policy, notices in every package

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Icon set for every store

**Files:**
- Modify: `packaging/icons/make-icon.ps1`, `packaging/macos/bundle.sh`
- Create: `packaging/icons/make-all.ps1`, generated PNGs

**Interfaces:**
- Produces: `packaging/icons/rustshot-{16,32,48,64,128,256,512,1024}.png`; `packaging/icons/msix/{Square44x44Logo.png, Square44x44Logo.targetsize-{16,24,32,48,256}_altform-unplated.png, Square150x150Logo.png, Wide310x150Logo.png, StoreLogo.png}`.

- [ ] **Step 1: Parameterize the generator** — at the top of `packaging/icons/make-icon.ps1` (after the comment header) add:

```powershell
param(
    [int]$Size = 256,
    [int]$CanvasW = 0,
    [string]$Out = (Join-Path $PSScriptRoot 'rustshot-256.png')
)
```

Replace `$w = 256` with `$w = 256  # design grid; scaled to -Size below`, and replace the bitmap/graphics creation lines

```powershell
$bmp = New-Object -TypeName System.Drawing.Bitmap -ArgumentList @($w, $w, $fmt)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)
```

with

```powershell
$cw = if ($CanvasW -gt 0) { $CanvasW } else { $Size }
$bmp = New-Object -TypeName System.Drawing.Bitmap -ArgumentList @($cw, $Size, $fmt)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)
$g.TranslateTransform([float](($cw - $Size) / 2), 0)
$g.ScaleTransform([float]($Size / $w), [float]($Size / $w))
```

At the end replace

```powershell
$out = Join-Path $PSScriptRoot 'rustshot-256.png'
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
```

with

```powershell
$out = $Out
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
```

- [ ] **Step 2: Batch script** — create `packaging/icons/make-all.ps1`:

```powershell
# Regenerates every committed icon (Windows only: uses System.Drawing).
# Run: powershell -File packaging\icons\make-all.ps1
$ErrorActionPreference = 'Stop'
$d = $PSScriptRoot
$mk = Join-Path $d 'make-icon.ps1'
foreach ($s in 16, 32, 48, 64, 128, 256, 512, 1024) {
    & $mk -Size $s -Out (Join-Path $d "rustshot-$s.png")
}
$m = Join-Path $d 'msix'
New-Item -ItemType Directory -Force $m | Out-Null
& $mk -Size 44 -Out (Join-Path $m 'Square44x44Logo.png')
foreach ($s in 16, 24, 32, 48, 256) {
    & $mk -Size $s -Out (Join-Path $m "Square44x44Logo.targetsize-${s}_altform-unplated.png")
}
& $mk -Size 150 -Out (Join-Path $m 'Square150x150Logo.png')
& $mk -Size 150 -CanvasW 310 -Out (Join-Path $m 'Wide310x150Logo.png')
& $mk -Size 50 -Out (Join-Path $m 'StoreLogo.png')
```

- [ ] **Step 3: Generate and inspect**

Run: `powershell -File packaging\icons\make-all.ps1`
Expected: 18 "wrote …" lines. Open `packaging\icons\rustshot-1024.png`, `rustshot-16.png` and `msix\Wide310x150Logo.png` with the Read tool: same artwork, crisp at 1024, recognisable at 16, centred tile on a transparent wide canvas. `git diff --stat packaging/icons/rustshot-256.png` may show a tiny binary change — acceptable.

- [ ] **Step 4: Full icns for macOS** — in `packaging/macos/bundle.sh`, replace

```bash
sips -s format icns packaging/icons/rustshot-256.png \
  --out "$app/Contents/Resources/rustshot.icns" >/dev/null
```

with

```bash
iconset="dist/rustshot.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  cp "packaging/icons/rustshot-$s.png" "$iconset/icon_${s}x${s}.png"
  cp "packaging/icons/rustshot-$((s * 2)).png" "$iconset/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/rustshot.icns"
rm -rf "$iconset"
```

- [ ] **Step 5: Verify in CI** — commit, push the branch, and check the `ci` run: `linux` (desktop/AppStream validation, deb, AppImage, tarball) and `macos` (bundle with iconutil) must be green.

```bash
git add packaging/icons packaging/macos/bundle.sh
git commit -m "packaging: parameterized icon generator; full icon set incl. 1024 px and MSIX assets

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
```

If `appstreamcli validate` fails, fix the metainfo per its message and amend with a new commit (not `--amend`).

---

### Task 3: macOS bundle id and Mac App Store package

**Files:**
- Modify: `packaging/macos/bundle.sh`, `.github/workflows/ci.yml` (macos job)
- Create: `packaging/macos/mas.entitlements`, `packaging/macos/mas.sh`

**Interfaces:**
- Consumes: `dist/Rustshot.app` from `bundle.sh`.
- Produces: `dist/rustshot-<v>-mas.pkg` (only when MAS secrets exist), included in the `rustshot-macos` artifact.

- [ ] **Step 1: Bundle id, build number, store plist keys** — in `packaging/macos/bundle.sh`:
- after the `version=$(...)` line add `build="${BUILD_NUMBER:-$version}"`;
- in the plist heredoc change `com.nappsllc.rustshot` to `io.github.nappsllc.rustshot`, change `<key>CFBundleVersion</key><string>$version</string>` to `<key>CFBundleVersion</key><string>$build</string>`, and add before `</dict>`:

```xml
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>ITSAppUsesNonExemptEncryption</key><false/>
```

- [ ] **Step 2: Entitlements** — create `packaging/macos/mas.entitlements`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.app-sandbox</key><true/>
  <key>com.apple.security.network.client</key><true/>
  <key>com.apple.security.files.user-selected.read-write</key><true/>
  <key>com.apple.security.assets.pictures.read-write</key><true/>
</dict>
</plist>
```

- [ ] **Step 3: MAS signing script** — create `packaging/macos/mas.sh`:

```bash
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
```

Run `chmod +x packaging/macos/mas.sh` (Git Bash on Windows: `git update-index --chmod=+x packaging/macos/mas.sh` after adding).

- [ ] **Step 4: CI step** — in `.github/workflows/ci.yml` `macos` job, give the `Bundle` step `env: { BUILD_NUMBER: "${{ github.run_number }}" }`, and add directly after it (before "Sign and notarize", which re-signs `dist/Rustshot.app` in place):

```yaml
      # Mac App Store build — runs only when the MAS secrets are configured.
      - name: Mac App Store package
        env:
          MAS_APP_CERT_P12: ${{ secrets.MAS_APP_CERT_P12 }}
          MAS_INSTALLER_CERT_P12: ${{ secrets.MAS_INSTALLER_CERT_P12 }}
          MAS_CERT_PASSWORD: ${{ secrets.MAS_CERT_PASSWORD }}
          MAS_PROVISION_PROFILE: ${{ secrets.MAS_PROVISION_PROFILE }}
          MAS_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
        run: |
          if [ -z "$MAS_APP_CERT_P12" ]; then
            echo "MAS secrets not configured — skipping Mac App Store package"
            exit 0
          fi
          set -euo pipefail
          kc="$RUNNER_TEMP/mas-keychain"
          security create-keychain -p ci "$kc"
          security set-keychain-settings "$kc"
          security unlock-keychain -p ci "$kc"
          for v in MAS_APP_CERT_P12 MAS_INSTALLER_CERT_P12; do
            echo "${!v}" | base64 --decode > "$RUNNER_TEMP/$v.p12"
            security import "$RUNNER_TEMP/$v.p12" -k "$kc" -P "$MAS_CERT_PASSWORD" \
              -T /usr/bin/codesign -T /usr/bin/productbuild
            rm -f "$RUNNER_TEMP/$v.p12"
          done
          security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k ci "$kc"
          security list-keychains -d user -s "$kc"
          export MAS_APP_IDENTITY=$(security find-identity -v -p codesigning "$kc" | sed -n 's/.*"\(Apple Distribution[^"]*\)".*/\1/p' | head -n1)
          export MAS_INSTALLER_IDENTITY=$(security find-identity -v "$kc" | sed -n 's/.*"\(3rd Party Mac Developer Installer[^"]*\)".*/\1/p' | head -n1)
          export MAS_PROFILE="$RUNNER_TEMP/rustshot.provisionprofile"
          echo "$MAS_PROVISION_PROFILE" | base64 --decode > "$MAS_PROFILE"
          bash packaging/macos/mas.sh
          security delete-keychain "$kc"
```

and add `dist/*-mas.pkg` to the macos `upload-artifact` `path:` list.

- [ ] **Step 5: Verify** — commit, push, check the `macos` job: green, log shows "MAS secrets not configured — skipping Mac App Store package", and the downloaded `.app`'s `Info.plist` has `io.github.nappsllc.rustshot`.

```bash
git add packaging/macos .github/workflows/ci.yml
git commit -m "packaging: bundle id io.github.nappsllc.rustshot; Mac App Store sandboxed pkg (when secrets exist)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
```

---

### Task 4: MSIX for the Microsoft Store

**Files:**
- Create: `packaging/windows/AppxManifest.xml.in`, `packaging/windows/msix.ps1`
- Modify: `.github/workflows/ci.yml` (windows job)

**Interfaces:**
- Consumes: `target/release/rustshot.exe`, `packaging/icons/msix/*`.
- Produces: `dist/rustshot-<v>-x64.msix` (+ `-sideload.msix` when signing secrets exist); artifact `rustshot-windows-msix`.

- [ ] **Step 1: Manifest template** — create `packaging/windows/AppxManifest.xml.in`:

```xml
<?xml version="1.0" encoding="utf-8"?>
<Package
  xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
  xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
  xmlns:uap3="http://schemas.microsoft.com/appx/manifest/uap/windows10/3"
  xmlns:uap10="http://schemas.microsoft.com/appx/manifest/uap/windows10/10"
  xmlns:desktop="http://schemas.microsoft.com/appx/manifest/desktop/windows10"
  xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
  IgnorableNamespaces="uap uap3 uap10 desktop rescap">
  <Identity Name="@IDENTITY_NAME@" Publisher="@PUBLISHER@" Version="@VERSION@" ProcessorArchitecture="x64" />
  <Properties>
    <DisplayName>rustshot</DisplayName>
    <PublisherDisplayName>@PUBLISHER_DISPLAY@</PublisherDisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
    <Description>Screenshot capture with annotation, upload and global hotkeys</Description>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.19041.0" MaxVersionTested="10.0.26100.0" />
  </Dependencies>
  <Resources>
    <Resource Language="en-us" />
  </Resources>
  <Applications>
    <Application Id="Rustshot" Executable="rustshot.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements DisplayName="rustshot" Description="Capture and annotate a screenshot"
        BackgroundColor="transparent" Square150x150Logo="Assets\Square150x150Logo.png"
        Square44x44Logo="Assets\Square44x44Logo.png">
        <uap:DefaultTile Wide310x150Logo="Assets\Wide310x150Logo.png" />
      </uap:VisualElements>
      <Extensions>
        <uap3:Extension Category="windows.appExecutionAlias" Executable="rustshot.exe" EntryPoint="Windows.FullTrustApplication">
          <uap3:AppExecutionAlias>
            <desktop:ExecutionAlias Alias="rustshot.exe" />
          </uap3:AppExecutionAlias>
        </uap3:Extension>
      </Extensions>
    </Application>
    <!-- Hidden second entry: the hotkey daemon, started at sign-in. -->
    <Application Id="RustshotDaemon" Executable="rustshot.exe" EntryPoint="Windows.FullTrustApplication" uap10:Parameters="daemon">
      <uap:VisualElements DisplayName="rustshot hotkeys" Description="Global screenshot hotkeys"
        BackgroundColor="transparent" Square150x150Logo="Assets\Square150x150Logo.png"
        Square44x44Logo="Assets\Square44x44Logo.png" AppListEntry="none" />
      <Extensions>
        <desktop:Extension Category="windows.startupTask" Executable="rustshot.exe" EntryPoint="Windows.FullTrustApplication">
          <desktop:StartupTask TaskId="RustshotDaemon" Enabled="true" DisplayName="rustshot hotkeys" />
        </desktop:Extension>
      </Extensions>
    </Application>
  </Applications>
  <Capabilities>
    <Capability Name="internetClient" />
    <rescap:Capability Name="runFullTrust" />
  </Capabilities>
</Package>
```

- [ ] **Step 2: Build script** — create `packaging/windows/msix.ps1`:

```powershell
# Build dist\rustshot-<version>-x64.msix for the Microsoft Store (unsigned; the
# Store signs it). Identity (Partner Center > Product identity) comes from env:
#   MSIX_IDENTITY_NAME, MSIX_PUBLISHER (CN=...), MSIX_PUBLISHER_DISPLAY
# Optional sideload copy: MSIX_SIGN_PFX (base64 .pfx whose subject equals
# MSIX_PUBLISHER) + MSIX_SIGN_PASSWORD -> dist\rustshot-<version>-x64-sideload.msix
$ErrorActionPreference = 'Stop'
Set-Location (Resolve-Path (Join-Path $PSScriptRoot '..\..'))

$v = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if ($v -notmatch '^\d+\.\d+\.\d+$') { throw "MSIX needs a plain x.y.z version, got '$v'" }
$exe = 'target\release\rustshot.exe'
if (-not (Test-Path $exe)) { throw "$exe missing (cargo build --release first)" }

function Or($a, $b) { if ($a) { $a } else { $b } }
$name = Or $env:MSIX_IDENTITY_NAME 'nappsllc.rustshot.dev'
$pub = Or $env:MSIX_PUBLISHER 'CN=nappsllc-dev'
$disp = Or $env:MSIX_PUBLISHER_DISPLAY 'nappsllc'
$esc = { param($s) [System.Security.SecurityElement]::Escape($s) }

$stage = 'dist\stage-msix'
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force "$stage\Assets" | Out-Null
Copy-Item $exe $stage
Copy-Item LICENSE "$stage\LICENSE.txt"
Copy-Item THIRD_PARTY_NOTICES.md $stage
Copy-Item packaging\icons\msix\*.png "$stage\Assets"
$manifest = (Get-Content packaging\windows\AppxManifest.xml.in -Raw).
    Replace('@IDENTITY_NAME@', (& $esc $name)).
    Replace('@PUBLISHER@', (& $esc $pub)).
    Replace('@PUBLISHER_DISPLAY@', (& $esc $disp)).
    Replace('@VERSION@', "$v.0")
Set-Content -Path "$stage\AppxManifest.xml" -Value $manifest -Encoding utf8

$bin = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\makeappx.exe" |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $bin) { throw 'makeappx.exe not found (install the Windows 10/11 SDK)' }
$sdk = $bin.DirectoryName

$out = "dist\rustshot-$v-x64.msix"
& "$sdk\makeappx.exe" pack /o /d $stage /p $out
if ($LASTEXITCODE) { throw "makeappx failed ($LASTEXITCODE)" }
Write-Host "wrote $out"

if ($env:MSIX_SIGN_PFX) {
    $pfx = Join-Path ([System.IO.Path]::GetTempPath()) 'rustshot-sideload.pfx'
    [System.IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:MSIX_SIGN_PFX))
    $signed = "dist\rustshot-$v-x64-sideload.msix"
    Copy-Item $out $signed -Force
    & "$sdk\signtool.exe" sign /fd SHA256 /f $pfx /p $env:MSIX_SIGN_PASSWORD $signed
    $rc = $LASTEXITCODE
    Remove-Item $pfx -Force
    if ($rc) { throw "signtool failed ($rc)" }
    Write-Host "wrote $signed"
}
```

- [ ] **Step 3: Build locally**

```powershell
cargo build --release
powershell -File packaging\windows\msix.ps1
```

Expected: `wrote dist\rustshot-0.1.0-x64.msix`, no makeappx errors (a schema error names the bad manifest line — fix the template).

- [ ] **Step 4: Sideload test (local, with the user's OK)** — ask before installing: *"OK to create a self-signed test certificate in your CurrentUser store, trust it, and install the sideload MSIX? I'll remove all three afterwards."* Then:

```powershell
$cert = New-SelfSignedCertificate -Type Custom -Subject 'CN=nappsllc-dev' -KeyUsage DigitalSignature `
  -FriendlyName 'rustshot sideload test' -CertStoreLocation Cert:\CurrentUser\My `
  -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
$pw = ConvertTo-SecureString -String 'test' -Force -AsPlainText
Export-PfxCertificate -Cert $cert -FilePath $env:TEMP\rs.pfx -Password $pw | Out-Null
Import-Certificate -FilePath (Export-Certificate -Cert $cert -FilePath $env:TEMP\rs.cer).FullName -CertStoreLocation Cert:\CurrentUser\TrustedPeople | Out-Null
$env:MSIX_SIGN_PFX = [Convert]::ToBase64String([IO.File]::ReadAllBytes("$env:TEMP\rs.pfx")); $env:MSIX_SIGN_PASSWORD = 'test'
powershell -File packaging\windows\msix.ps1
Add-AppxPackage dist\rustshot-0.1.0-x64-sideload.msix
```

Check: Start menu shows "rustshot" (one entry); launching it opens the capture overlay; `rustshot --help` works in a new terminal (execution alias); Task Manager › Startup apps lists "rustshot hotkeys"; after sign-out/in (or `explorer shell:AppsFolder\<PackageFamilyName>!RustshotDaemon`) a `rustshot.exe daemon` process runs (verify with `Get-CimInstance Win32_Process -Filter "Name='rustshot.exe'" | Select CommandLine`). If the daemon starts without the `daemon` argument, record it in `docs/STORES.md` "Known issues" — do not work around it in this task.

Clean up: `Get-AppxPackage *rustshot* | Remove-AppxPackage`; remove the certificate from `Cert:\CurrentUser\My` and `Cert:\CurrentUser\TrustedPeople`; delete `$env:TEMP\rs.pfx`, `$env:TEMP\rs.cer`; clear `$env:MSIX_SIGN_PFX`.

- [ ] **Step 5: CI** — in `.github/workflows/ci.yml` `windows` job, after the `Installer` step add:

```yaml
      - name: MSIX
        env:
          MSIX_IDENTITY_NAME: ${{ vars.MSIX_IDENTITY_NAME }}
          MSIX_PUBLISHER: ${{ vars.MSIX_PUBLISHER }}
          MSIX_PUBLISHER_DISPLAY: ${{ vars.MSIX_PUBLISHER_DISPLAY }}
          MSIX_SIGN_PFX: ${{ secrets.MSIX_SIGN_PFX }}
          MSIX_SIGN_PASSWORD: ${{ secrets.MSIX_SIGN_PASSWORD }}
        run: packaging\windows\msix.ps1
      - uses: actions/upload-artifact@v4
        with:
          name: rustshot-windows-msix
          path: dist/*.msix
```

- [ ] **Step 6: Commit and verify**

```bash
git add packaging/windows .github/workflows/ci.yml
git commit -m "packaging: MSIX (Microsoft Store) with startup-task daemon and CLI alias

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
```

Expected: `windows` job green, artifact `rustshot-windows-msix` present.

---

### Task 5: Flatpak (Flathub)

**Files:**
- Create: `packaging/flatpak/io.github.nappsllc.rustshot.yml`, `packaging/flatpak/cargo-sources.json`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: CI artifact containing `rustshot.flatpak`.

- [ ] **Step 1: Manifest** — create `packaging/flatpak/io.github.nappsllc.rustshot.yml`:

```yaml
id: io.github.nappsllc.rustshot
runtime: org.freedesktop.Platform
runtime-version: "24.08"
sdk: org.freedesktop.Sdk
sdk-extensions:
  - org.freedesktop.Sdk.Extension.rust-stable
command: rustshot
finish-args:
  # X11-only today (capture + global hotkeys); see metainfo.
  - --socket=x11
  - --share=ipc
  - --share=network
  - --filesystem=xdg-pictures
build-options:
  append-path: /usr/lib/sdk/rust-stable/bin
  env:
    CARGO_HOME: /run/build/rustshot/cargo
modules:
  - name: rustshot
    buildsystem: simple
    build-commands:
      - cargo --offline build --release --locked
      - install -Dm755 target/release/rustshot /app/bin/rustshot
      - install -Dm644 packaging/linux/io.github.nappsllc.rustshot.desktop /app/share/applications/io.github.nappsllc.rustshot.desktop
      - install -Dm644 packaging/linux/io.github.nappsllc.rustshot.metainfo.xml /app/share/metainfo/io.github.nappsllc.rustshot.metainfo.xml
      - for s in 128 256 512; do install -Dm644 packaging/icons/rustshot-$s.png /app/share/icons/hicolor/${s}x${s}/apps/io.github.nappsllc.rustshot.png; done
      - install -Dm644 LICENSE /app/share/licenses/io.github.nappsllc.rustshot/LICENSE
      - install -Dm644 THIRD_PARTY_NOTICES.md /app/share/licenses/io.github.nappsllc.rustshot/THIRD_PARTY_NOTICES.md
    sources:
      - type: dir
        path: ../..
        skip:
          - target
          - dist
          - _flameshot
      - cargo-sources.json
```

- [ ] **Step 2: Vendored crate sources** — generate locally (ask the user before installing Python packages and downloading the generator):

```bash
python -m pip install --user aiohttp tomlkit
curl -fsSL -o "$SCRATCH/flatpak-cargo-generator.py" \
  https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
python "$SCRATCH/flatpak-cargo-generator.py" Cargo.lock -o packaging/flatpak/cargo-sources.json
```

Expected: `packaging/flatpak/cargo-sources.json` lists one `archive` entry per crate in `Cargo.lock` (Windows-only crates included — harmless).

- [ ] **Step 3: CI** — add to `.github/workflows/ci.yml`:

In the `linux` job, after "Validate desktop entry and AppStream metadata":

```yaml
      - name: Flatpak cargo sources are up to date
        run: |
          pip install --user aiohttp tomlkit
          curl -fsSL -o "$RUNNER_TEMP/fcg.py" \
            https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
          python3 "$RUNNER_TEMP/fcg.py" Cargo.lock -o "$RUNNER_TEMP/cargo-sources.json"
          diff -u packaging/flatpak/cargo-sources.json "$RUNNER_TEMP/cargo-sources.json" \
            || { echo "::error::regenerate packaging/flatpak/cargo-sources.json (see docs/STORES.md)"; exit 1; }
```

New job:

```yaml
  flatpak:
    runs-on: ubuntu-latest
    container:
      image: ghcr.io/flathub-infra/flatpak-github-actions:freedesktop-24.08
      options: --privileged
    steps:
      - uses: actions/checkout@v4
      - uses: flatpak/flatpak-github-actions/flatpak-builder@v6
        with:
          bundle: rustshot.flatpak
          manifest-path: packaging/flatpak/io.github.nappsllc.rustshot.yml
          cache-key: flatpak-builder-${{ github.sha }}
```

- [ ] **Step 4: Commit and verify**

```bash
git add packaging/flatpak .github/workflows/ci.yml
git commit -m "packaging: Flatpak manifest with vendored cargo sources; CI bundle build

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
```

Expected: `flatpak` job green and its run has a `rustshot-x86_64` artifact containing `rustshot.flatpak`; the `linux` job's cargo-sources check passes. If the diff check fails only on ordering/whitespace from a newer generator, regenerate with the CI command and commit the result.

---

### Task 6: Snap

**Files:**
- Create: `snap/snapcraft.yaml`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: artifact `rustshot-snap` containing `rustshot_<v>_amd64.snap`.

- [ ] **Step 1: snapcraft.yaml** — create `snap/snapcraft.yaml`:

```yaml
name: rustshot
base: core24
adopt-info: rustshot
summary: Capture, annotate and share screenshots
description: |
  rustshot captures the whole screen or a region and lets you annotate it
  with arrows, lines, shapes, text, a highlighter and pixelation before
  saving it, copying it to the clipboard or uploading it to imgur. A small
  background daemon provides global hotkeys. Requires an X11 session.
grade: stable
confinement: strict
license: GPL-3.0-only
icon: packaging/icons/rustshot-256.png
platforms:
  amd64:

apps:
  rustshot:
    command: bin/rustshot
    common-id: io.github.nappsllc.rustshot
    desktop: usr/share/applications/io.github.nappsllc.rustshot.desktop
    plugs: [x11, network, home, desktop]

parts:
  rustshot:
    plugin: rust
    source: .
    rust-channel: stable
    build-packages: [libx11-dev]
    stage-packages: [libx11-6]
    parse-info: [usr/share/metainfo/io.github.nappsllc.rustshot.metainfo.xml]
    override-build: |
      craftctl default
      install -Dm644 packaging/linux/io.github.nappsllc.rustshot.desktop \
        "$CRAFT_PART_INSTALL/usr/share/applications/io.github.nappsllc.rustshot.desktop"
      install -Dm644 packaging/linux/io.github.nappsllc.rustshot.metainfo.xml \
        "$CRAFT_PART_INSTALL/usr/share/metainfo/io.github.nappsllc.rustshot.metainfo.xml"
      install -Dm644 packaging/icons/rustshot-256.png \
        "$CRAFT_PART_INSTALL/usr/share/icons/hicolor/256x256/apps/io.github.nappsllc.rustshot.png"
      install -Dm644 LICENSE "$CRAFT_PART_INSTALL/usr/share/doc/rustshot/LICENSE"
      install -Dm644 THIRD_PARTY_NOTICES.md "$CRAFT_PART_INSTALL/usr/share/doc/rustshot/THIRD_PARTY_NOTICES.md"
```

- [ ] **Step 2: CI job** — add to `.github/workflows/ci.yml`:

```yaml
  snap:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: snapcore/action-build@v1
        id: build
      - uses: actions/upload-artifact@v4
        with:
          name: rustshot-snap
          path: ${{ steps.build.outputs.snap }}
```

- [ ] **Step 3: Commit and verify**

```bash
git add snap .github/workflows/ci.yml
git commit -m "packaging: snapcraft (core24, strict, X11) with CI build

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
```

Expected: `snap` job green; artifact `rustshot-snap` holds `rustshot_0.1.0_amd64.snap` (version adopted from the metainfo `<release>`). If snapcraft rejects the desktop file's `Icon=` name, change the `desktop:`-referenced copy's icon line in `override-build` with `sed -i 's|^Icon=.*|Icon=${SNAP}/usr/share/icons/hicolor/256x256/apps/io.github.nappsllc.rustshot.png|'` on the installed file.

---

### Task 7: Homebrew cask and AUR templates

**Files:**
- Create: `packaging/homebrew/rustshot.rb.in`, `packaging/aur/PKGBUILD.in`

**Interfaces:**
- Placeholders `@VERSION@`, `@SHA256@`, rendered by release jobs in Task 8.

- [ ] **Step 1: Cask** — create `packaging/homebrew/rustshot.rb.in`:

```ruby
cask "rustshot" do
  version "@VERSION@"
  sha256 "@SHA256@"

  url "https://github.com/nappsllc/rustshot/releases/download/v#{version}/rustshot-#{version}-macos-universal.dmg"
  name "rustshot"
  desc "Screenshot capture with annotation, upload and global hotkeys"
  homepage "https://github.com/nappsllc/rustshot"

  depends_on macos: ">= :big_sur"

  app "Rustshot.app"
  binary "#{appdir}/Rustshot.app/Contents/MacOS/rustshot"

  zap trash: "~/.config/rustshot"
end
```

- [ ] **Step 2: PKGBUILD** — create `packaging/aur/PKGBUILD.in`:

```bash
# Maintainer: nappsllc <noreply@github.com>
pkgname=rustshot-bin
pkgver=@VERSION@
pkgrel=1
pkgdesc="Screenshot capture with annotation, upload and global hotkeys (X11)"
arch=('x86_64')
url="https://github.com/nappsllc/rustshot"
license=('GPL-3.0-only')
depends=('libx11')
provides=('rustshot')
conflicts=('rustshot')
source=("https://github.com/nappsllc/rustshot/releases/download/v${pkgver}/rustshot-${pkgver}-linux-x86_64.tar.gz")
sha256sums=('@SHA256@')

package() {
  install -Dm755 rustshot "$pkgdir/usr/bin/rustshot"
  install -Dm644 io.github.nappsllc.rustshot.desktop "$pkgdir/usr/share/applications/io.github.nappsllc.rustshot.desktop"
  install -Dm644 io.github.nappsllc.rustshot.png "$pkgdir/usr/share/icons/hicolor/256x256/apps/io.github.nappsllc.rustshot.png"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 THIRD_PARTY_NOTICES.md "$pkgdir/usr/share/licenses/$pkgname/THIRD_PARTY_NOTICES.md"
}
```

(The tarball contains every file named here since Task 1.)

- [ ] **Step 3: Render check** (Git Bash):

```bash
sed -e "s/@VERSION@/0.1.0/" -e "s/@SHA256@/$(printf 0%.0s {1..64})/" packaging/aur/PKGBUILD.in | bash -n
sed -e "s/@VERSION@/0.1.0/" -e "s/@SHA256@/0/" packaging/homebrew/rustshot.rb.in | grep -c "0.1.0"
```

Expected: `bash -n` prints nothing (valid syntax); grep prints `1`.

- [ ] **Step 4: Commit**

```bash
git add packaging/homebrew packaging/aur
git commit -m "packaging: Homebrew cask and AUR rustshot-bin templates

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Publish jobs in `release.yml`

**Files:**
- Modify: `.github/workflows/release.yml`

**Interfaces:**
- Consumes artifacts: `rustshot-windows-msix`, `rustshot-windows-installer`, `rustshot-macos`, `rustshot-linux`, `rustshot-snap`.
- Each job `needs: publish` (so the GitHub Release with download URLs exists first).

- [ ] **Step 1: Append the jobs** to `.github/workflows/release.yml` under `jobs:`:

```yaml
  msstore:
    needs: publish
    if: vars.PUBLISH_MSSTORE == 'true'
    runs-on: windows-latest
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: rustshot-windows-msix
          path: artifacts
      - uses: microsoft/setup-msstore-cli@v1
      - name: Submit to Partner Center
        run: |
          msstore reconfigure --tenantId "${{ secrets.MSSTORE_TENANT_ID }}" --sellerId "${{ vars.MSSTORE_SELLER_ID }}" `
            --clientId "${{ secrets.MSSTORE_CLIENT_ID }}" --clientSecret "${{ secrets.MSSTORE_CLIENT_SECRET }}"
          $pkg = Get-ChildItem artifacts\*.msix | Where-Object Name -notlike '*sideload*' | Select-Object -First 1
          msstore publish $pkg.FullName -id "${{ vars.MSSTORE_APP_ID }}"

  winget:
    needs: publish
    if: vars.PUBLISH_WINGET == 'true'
    runs-on: windows-latest
    steps:
      - name: Open winget-pkgs PR
        env:
          WINGET_TOKEN: ${{ secrets.WINGET_TOKEN }}
        run: |
          $v = "${{ github.ref_name }}".TrimStart('v')
          $url = "https://github.com/${{ github.repository }}/releases/download/${{ github.ref_name }}/rustshot-$v-setup.exe"
          Invoke-WebRequest https://aka.ms/wingetcreate/latest -OutFile wingetcreate.exe
          .\wingetcreate.exe update nappsllc.rustshot --version $v --urls $url --submit --token $env:WINGET_TOKEN

  mas:
    needs: publish
    if: vars.PUBLISH_MAS == 'true'
    runs-on: macos-latest
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: rustshot-macos
          path: artifacts
      - name: Upload to App Store Connect
        env:
          ASC_KEY_ID: ${{ secrets.ASC_KEY_ID }}
          ASC_ISSUER_ID: ${{ secrets.ASC_ISSUER_ID }}
          ASC_KEY_P8: ${{ secrets.ASC_KEY_P8 }}
        run: |
          set -euo pipefail
          pkg=$(ls artifacts/*-mas.pkg)
          mkdir -p ~/.appstoreconnect/private_keys
          printf '%s' "$ASC_KEY_P8" > ~/.appstoreconnect/private_keys/AuthKey_$ASC_KEY_ID.p8
          xcrun altool --upload-app -f "$pkg" -t macos --apiKey "$ASC_KEY_ID" --apiIssuer "$ASC_ISSUER_ID"

  homebrew:
    needs: publish
    if: vars.PUBLISH_HOMEBREW == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with:
          name: rustshot-macos
          path: artifacts
      - name: Render cask
        run: |
          v="${GITHUB_REF_NAME#v}"
          sum=$(sha256sum "artifacts/rustshot-$v-macos-universal.dmg" | cut -d' ' -f1)
          sed -e "s/@VERSION@/$v/" -e "s/@SHA256@/$sum/" packaging/homebrew/rustshot.rb.in > "$RUNNER_TEMP/rustshot.rb"
      - uses: actions/checkout@v4
        with:
          repository: nappsllc/homebrew-tap
          token: ${{ secrets.HOMEBREW_TAP_TOKEN }}
          path: tap
      - name: Push cask
        run: |
          mkdir -p tap/Casks
          cp "$RUNNER_TEMP/rustshot.rb" tap/Casks/rustshot.rb
          cd tap
          git config user.name "rustshot release"
          git config user.email "noreply@github.com"
          git add Casks/rustshot.rb
          git commit -m "rustshot ${GITHUB_REF_NAME#v}"
          git push

  snap:
    needs: publish
    if: vars.PUBLISH_SNAP == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: rustshot-snap
          path: artifacts
      - id: find
        run: echo "snap=$(ls artifacts/*.snap)" >> "$GITHUB_OUTPUT"
      - uses: snapcore/action-publish@v1
        env:
          SNAPCRAFT_STORE_CREDENTIALS: ${{ secrets.SNAPCRAFT_STORE_CREDENTIALS }}
        with:
          snap: ${{ steps.find.outputs.snap }}
          release: edge

  aur:
    needs: publish
    if: vars.PUBLISH_AUR == 'true'
    runs-on: ubuntu-latest
    container: archlinux:base-devel
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with:
          name: rustshot-linux
          path: artifacts
      - name: Render and push rustshot-bin
        env:
          AUR_SSH_KEY: ${{ secrets.AUR_SSH_KEY }}
        run: |
          set -euo pipefail
          pacman -Syu --noconfirm git openssh
          git config --global --add safe.directory '*'
          v="${GITHUB_REF_NAME#v}"
          sum=$(sha256sum "artifacts/rustshot-$v-linux-x86_64.tar.gz" | cut -d' ' -f1)
          mkdir -p ~/.ssh
          printf '%s\n' "$AUR_SSH_KEY" > ~/.ssh/aur
          chmod 600 ~/.ssh/aur
          ssh-keyscan aur.archlinux.org >> ~/.ssh/known_hosts
          export GIT_SSH_COMMAND="ssh -i $HOME/.ssh/aur"
          git clone ssh://aur@aur.archlinux.org/rustshot-bin.git aur
          sed -e "s/@VERSION@/$v/" -e "s/@SHA256@/$sum/" packaging/aur/PKGBUILD.in > aur/PKGBUILD
          useradd -m builder
          chown -R builder aur
          (cd aur && sudo -u builder makepkg --printsrcinfo > .SRCINFO)
          cd aur
          git config user.name "rustshot release"
          git config user.email "noreply@github.com"
          git add PKGBUILD .SRCINFO
          git commit -m "Update to $v"
          git push
```

- [ ] **Step 2: Validate** — `python -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"` → no error. Then check the workflow with GitHub's parser by pushing the branch: Actions shows `release` with no "Invalid workflow file" banner (it will not run without a tag).

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "release: per-channel publish jobs (Store, winget, MAS, Homebrew, Snap, AUR) behind PUBLISH_* switches

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
```

---

### Task 9: `docs/STORES.md` checklist

**Files:**
- Create: `docs/STORES.md`

- [ ] **Step 1: Write the checklist** — create `docs/STORES.md`:

```markdown
# Publishing rustshot

Push a tag that matches `Cargo.toml` (`git tag v0.1.0 && git push origin v0.1.0`).
`release.yml` builds everything and attaches it to a GitHub Release. Each store
job below runs only after its repository variable `PUBLISH_<X>` is `true`
(Settings › Secrets and variables › Actions). Set the switch last, after a
manual first submission where noted.

## Before any store: screenshots
- [ ] Capture `docs/screenshots/overlay-dark.png` (and `overlay-light.png`) from
      the redesigned UI, 1440×900 or larger; commit to `main`.
- [ ] Add a `<release>` entry to `packaging/linux/io.github.nappsllc.rustshot.metainfo.xml`
      for every tag (newest first).

## Microsoft Store (MSIX)
1. Partner Center account (individual or company): https://partner.microsoft.com/dashboard
2. Reserve the name "rustshot" › Product identity. Copy into repo **variables**:
   `MSIX_IDENTITY_NAME` (Package/Identity/Name), `MSIX_PUBLISHER` (CN=…),
   `MSIX_PUBLISHER_DISPLAY`, `MSSTORE_APP_ID` (Store ID), `MSSTORE_SELLER_ID`.
3. First submission by hand: upload `rustshot-<v>-x64.msix` from the release,
   listing text, screenshots, age rating, privacy policy URL
   `https://github.com/nappsllc/rustshot/blob/main/PRIVACY.md`, and the
   `runFullTrust` justification ("desktop screenshot tool: global hotkeys and
   screen capture").
4. Azure AD app for the Store API (Partner Center › Account settings › User
   management › Azure AD applications, role Manager). **Secrets**:
   `MSSTORE_TENANT_ID`, `MSSTORE_CLIENT_ID`, `MSSTORE_CLIENT_SECRET`.
5. Optional testers' build: self-signed cert with subject = `MSIX_PUBLISHER`,
   exported as base64 PFX → secrets `MSIX_SIGN_PFX`, `MSIX_SIGN_PASSWORD`.
6. Set `PUBLISH_MSSTORE=true`.

## winget
1. Fork https://github.com/microsoft/winget-pkgs under the account that owns the token.
2. Classic PAT with `public_repo` → secret `WINGET_TOKEN`.
3. First manifest by hand: `wingetcreate new <setup.exe URL from the release>`
   — id `nappsllc.rustshot`, installer type `nullsoft`, scope `user`; submit.
4. After it merges, set `PUBLISH_WINGET=true`.

## Mac App Store
1. Apple Developer Program membership (https://developer.apple.com/programs/).
2. Identifiers › App ID `io.github.nappsllc.rustshot` (platform macOS).
3. Certificates: "Apple Distribution" and "Mac Installer Distribution"
   (shows as "3rd Party Mac Developer Installer"). Export both as .p12 with one
   password. **Secrets**: `MAS_APP_CERT_P12`, `MAS_INSTALLER_CERT_P12` (base64),
   `MAS_CERT_PASSWORD`, `APPLE_TEAM_ID`.
4. Profiles › Mac App Store profile for the App ID → secret
   `MAS_PROVISION_PROFILE` (base64).
5. App Store Connect › new macOS app with that bundle id; privacy URL as above;
   App Privacy: "Data not collected".
6. App Store Connect API key (Users and Access › Integrations, role App
   Manager) → secrets `ASC_KEY_ID`, `ASC_ISSUER_ID`, `ASC_KEY_P8` (file contents).
7. Before the first upload, run the sandboxed build locally and verify:
   capture works after granting Screen Recording, hotkeys fire, Save to
   Pictures works, **Upload works** (it spawns `/usr/bin/curl`; if blocked,
   the uploader must move to NSURLSession before submitting).
8. Set `PUBLISH_MAS=true`; submit each uploaded build for review in App Store Connect.

## Developer ID (dmg) — needed for Homebrew
Secrets `APPLE_CERT_P12`, `APPLE_CERT_PASSWORD`, `APPLE_ID`, `APPLE_TEAM_ID`,
`APPLE_APP_PASSWORD` (see the "Sign and notarize" step in `ci.yml`).

## Homebrew
1. Create public repo `nappsllc/homebrew-tap` (users run `brew install --cask nappsllc/tap/rustshot`).
2. Fine-grained PAT with Contents: read/write on that repo → secret `HOMEBREW_TAP_TOKEN`.
3. Requires the notarized dmg (Developer ID secrets above). Set `PUBLISH_HOMEBREW=true`.

## Flathub
1. Wait for the screenshots, then open a PR to https://github.com/flathub/flathub
   (branch `new-pr`) with `io.github.nappsllc.rustshot.yml` copied from
   `packaging/flatpak/`, its `dir` source replaced by
   `type: git, url: https://github.com/nappsllc/rustshot.git, tag: v<x.y.z>, commit: <sha>`,
   plus `cargo-sources.json`. Flathub verifies `io.github.nappsllc` via the GitHub org.
2. After acceptance, update the Flathub repo per release (tag + commit +
   regenerated `cargo-sources.json`) or add `x-checker-data`.
3. Regenerate sources whenever `Cargo.lock` changes:
   `python flatpak-cargo-generator.py Cargo.lock -o packaging/flatpak/cargo-sources.json`.

## Snap Store
1. https://snapcraft.io account; `snapcraft register rustshot`.
2. `snapcraft export-login --snaps=rustshot --channels=edge,stable --acls=package_upload -`
   → secret `SNAPCRAFT_STORE_CREDENTIALS`.
3. Set `PUBLISH_SNAP=true`. Builds land on `edge`; promote with
   `snapcraft release rustshot <rev> stable`.

## AUR
1. https://aur.archlinux.org account with an SSH key; private key → secret `AUR_SSH_KEY`.
2. First push creates the package: the release job clones
   `ssh://aur@aur.archlinux.org/rustshot-bin.git` (empty repo is fine).
3. Set `PUBLISH_AUR=true`.

## Known issues
- Linux builds are X11-only; Wayland sessions need XWayland (stated in metainfo).
```

- [ ] **Step 2: Commit**

```bash
git add docs/STORES.md
git commit -m "docs: store publishing checklist (accounts, variables, secrets, first submissions)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
```

- [ ] **Step 3: Final check** — on the pushed branch the `ci` run is green across `linux`, `macos`, `windows`, `flatpak`, `snap`, and the artifacts list contains the tarball, `.deb`, AppImage, zip, dmg, `.exe`, `-setup.exe`, `.msix`, `.flatpak`, `.snap`.

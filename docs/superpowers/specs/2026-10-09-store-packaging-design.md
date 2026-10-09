# Store & package-manager distribution — design

Date: 2026-10-09
Builds on: `bdccf0d` (LICENSE in every package; tag-triggered `release.yml`
reusing `ci.yml`).

## Goal

Ship rustshot through every mainstream channel on all three OSes, built and
(where the channel allows) published from GitHub Actions on a `v*` tag:

| OS | Channel | Artifact | Publish |
|---|---|---|---|
| Windows | Microsoft Store | `.msix` | CI via `msstore` CLI (Partner Center API) |
| Windows | winget | existing NSIS `-setup.exe` | CI opens PR to `microsoft/winget-pkgs` |
| macOS | Mac App Store | sandboxed universal `.pkg` | CI via `altool --upload-package` |
| macOS | Homebrew | existing notarized `.dmg` | CI updates cask in `nappsllc/homebrew-tap` |
| Linux | Flathub | Flatpak manifest (+ `.flatpak` bundle in CI) | one-time PR to `flathub/flathub`, then Flathub's own builds |
| Linux | Snap Store | `.snap` | CI to `edge`; promote by hand |
| Linux | AUR | `rustshot-bin` PKGBUILD | CI pushes to AUR git |

No accounts exist yet. Every account-specific value comes from repository
**variables** (non-secret identity) or **secrets** (credentials); each publish
job is skipped unless its `vars.PUBLISH_<CHANNEL> == 'true'` switch is set.
`docs/STORES.md` is the checklist of what to create and paste where.

## Shared decisions

- **App ID** `io.github.nappsllc.rustshot` everywhere: macOS
  `CFBundleIdentifier` (was `com.nappsllc.rustshot`), desktop file
  `io.github.nappsllc.rustshot.desktop`, icon name, AppStream id, Flatpak id.
  Snap name `rustshot`; winget id `nappsllc.rustshot`; MSIX identity from
  Partner Center (`vars.MSIX_IDENTITY_NAME`).
- **Icons**: `packaging/icons/make-icon.ps1` takes `-Size` and `-Out`, and a
  wrapper `make-all.ps1` regenerates the committed set: PNG 16, 32, 48, 64,
  128, 256, 512, 1024; MSIX `Square44x44Logo` (44, plus `targetsize-16/24/32/48/256`
  unplated), `Square150x150Logo` (150), `Wide310x150Logo` (310×150, glyph
  centred on transparent), `StoreLogo` (50). Generated on Windows
  (System.Drawing) and committed — CI never regenerates them.
- **AppStream metainfo** `packaging/linux/io.github.nappsllc.rustshot.metainfo.xml`:
  id, name, summary, description, `project_license` GPL-3.0-only,
  `metadata_license` CC0-1.0, developer `nappsllc`, `url` homepage/bugtracker,
  `content_rating` OARS 1.1 (none), `releases` (one entry per tag),
  `screenshots` pointing at `docs/screenshots/*.png` raw GitHub URLs,
  `launchable` desktop-id. Validated in CI with `appstreamcli validate --no-net`.
  Shipped by deb (`/usr/share/metainfo`), Flatpak and Snap (`adopt-info`).
- **Screenshots** (`docs/screenshots/`) are produced after the UI redesign
  lands; until then the metainfo validates with a placeholder and Flathub
  submission waits (checklist item).
- **Privacy policy** `PRIVACY.md`: no telemetry; uploads go to imgur only on
  explicit user action; config stays local. Linked from both store listings.
- **Third-party notices** `THIRD_PARTY_NOTICES.md` (Lucide ISC, Inter OFL from
  the UI work; plus a hand-maintained list of the four Rust dependencies —
  ab_glyph, anyhow, png, windows — all MIT/Apache-2.0). Shipped beside
  `LICENSE` everywhere.

## Windows

### MSIX (Microsoft Store)
- `packaging/windows/AppxManifest.xml.in`, filled by
  `packaging/windows/msix.ps1` from env: `IDENTITY_NAME`, `PUBLISHER`
  (`CN=…` from Partner Center), `PUBLISHER_DISPLAY`, `VERSION` (`x.y.z.0`).
  Local defaults are obvious placeholders so the package always builds.
- Two `Application`s in one package:
  - `Rustshot` — visible in Start, runs `rustshot.exe` (capture), with
    `uap3:AppExecutionAlias` `rustshot.exe` so the CLI works in terminals.
  - `RustshotDaemon` — `AppListEntry="none"`, `uap10:Parameters="daemon"`,
    `desktop:StartupTask` (TaskId `RustshotDaemon`, enabled) replacing the
    NSIS `Run` key for Store installs.
- Capabilities: `rescap:runFullTrust`, `internetClient`. Min OS
  `10.0.19041.0` (needed for `uap10:Parameters`).
- `MakeAppx pack` (Windows SDK on `windows-latest`) →
  `dist/rustshot-<v>-x64.msix`, unsigned (the Store signs). If
  `secrets.MSIX_SIGN_PFX` exists, also a signed `-sideload.msix` for testers.
- Publish: `msstore publish` with `secrets.MSSTORE_TENANT_ID`, `_CLIENT_ID`,
  `_CLIENT_SECRET`, `vars.MSSTORE_SELLER_ID`, `vars.MSSTORE_APP_ID`. The first
  submission (listing, screenshots, age rating, privacy URL) is manual.

### winget
- Release job runs `wingetcreate update nappsllc.rustshot` against the
  release's `-setup.exe` (NSIS supports `/S` silent; per-user scope) with
  `secrets.WINGET_TOKEN` (classic PAT, `public_repo`, owner has a fork of
  `winget-pkgs`). First version: `wingetcreate new` run locally once
  (checklist).

## macOS

### Universal binary
`ci.yml` macOS job builds `aarch64-apple-darwin` and `x86_64-apple-darwin`
and `lipo`s them into `target/universal/rustshot`; `bundle.sh` takes the
binary path.

### Developer ID (.dmg, Homebrew) — existing path
Unchanged signing/notarization (hardened runtime, no sandbox). Bundle id and
icns change: `iconutil` from a full `.iconset` (16…1024 incl. @2x).

### Mac App Store
- `packaging/macos/mas.entitlements`: `app-sandbox`, `network.client`,
  `files.user-selected.read-write`, `assets.pictures.read-write`.
- `packaging/macos/mas.sh`: copy the `.app`, embed
  `embedded.provisionprofile` (`secrets.MAS_PROVISION_PROFILE`, base64),
  `codesign` with "Apple Distribution" (`secrets.MAS_APP_CERT_P12`),
  `productbuild --sign "3rd Party Mac Developer Installer"`
  (`secrets.MAS_INSTALLER_CERT_P12`) → `dist/rustshot-<v>-mas.pkg`.
- Upload: `xcrun altool --upload-package` with App Store Connect API key
  (`secrets.ASC_KEY_ID`, `ASC_ISSUER_ID`, `ASC_KEY_P8`), `vars.MAS_APPLE_ID`
  (numeric app id), bundle id/version.
- Info.plist additions: `LSApplicationCategoryType`
  `public.app-category.productivity`, `ITSAppUsesNonExemptEncryption` false
  (HTTPS only via the system).
- **Sandbox compatibility** (code, small): the UI plan's dark-mode probe
  must use `NSUserDefaults` instead of spawning `defaults`; imgur upload
  currently spawns `/usr/bin/curl` — kept, but verified under the sandbox
  in the checklist; if App Review rejects it, replace with `NSURLSession`
  (follow-up, not in this scope).

### Homebrew
- Tap repo `nappsllc/homebrew-tap`, `Casks/rustshot.rb` (template in
  `packaging/homebrew/rustshot.rb.in`: version, sha256, dmg URL,
  `app "Rustshot.app"`, `zap` config dir). Release job renders it and pushes
  with `secrets.HOMEBREW_TAP_TOKEN`. Requires the dmg to be notarized
  (otherwise Gatekeeper blocks it on macOS 15) — job skipped when the
  Developer ID secrets are absent.

## Linux

### Flathub
- `packaging/flatpak/io.github.nappsllc.rustshot.yml`: runtime
  `org.freedesktop.Platform//24.08`, sdk extension `rust-stable`,
  `finish-args`: `--socket=x11`, `--share=ipc`, `--share=network`,
  `--filesystem=xdg-pictures`, `--device=dri` not needed. Build:
  `cargo --offline build --release` from `cargo-sources.json`
  (generated by `flatpak-cargo-generator.py` from `Cargo.lock`; CI fails if
  it is stale). Installs binary, desktop file, metainfo, icons 128/256/512.
- CI builds a `.flatpak` bundle with `flatpak/flatpak-github-actions` (artifact
  only). Flathub submission = PR to `flathub/flathub` (checklist); later
  releases flow via Flathub's `x-checker-data` on the git tag.

### Snap Store
- `snap/snapcraft.yaml`: `name: rustshot`, `base: core24`,
  `confinement: strict`, `adopt-info` from metainfo, part `rustshot`
  (`plugin: rust`, `stage-packages: [libx11-6]`), app `rustshot`
  (`plugs: [x11, network, home, desktop]`, `desktop:` the app-id desktop file).
- CI: `snapcore/action-build` (artifact); release publishes to `edge` with
  `snapcore/action-publish` and `secrets.SNAPCRAFT_STORE_CREDENTIALS`.

### AUR
- `packaging/aur/PKGBUILD` for `rustshot-bin` (downloads the release tarball,
  installs binary/desktop/icon/LICENSE, `depends=(libx11)`), `.SRCINFO`
  rendered in CI. Release job pushes with
  `KSXGitHub/github-actions-deploy-aur` and `secrets.AUR_SSH_KEY`.

### deb/AppImage/tarball updates
Rename desktop file + icon to the app id; deb also installs metainfo and
icons at 128/256/512.

## CI layout

- `ci.yml` (push/PR, and reused by release): adds MSIX build to `windows`,
  universal + MAS pkg (when secrets present) to `macos`, and new jobs
  `flatpak` and `snap` on Ubuntu; `linux` job validates metainfo and desktop
  file.
- `release.yml`: `build` → `publish` (GitHub Release, unchanged) → per-channel
  jobs `msstore`, `winget`, `mas`, `homebrew`, `snap`, `aur`, each
  `if: vars.PUBLISH_<X> == 'true'` and downloading only the artifact it needs.
- All third-party actions pinned to a release tag.

## Error handling
- Missing identity variables → placeholders, build still succeeds, publish
  job skipped by its switch.
- Version mismatch already fails `release.yml`.
- MSIX version must be 4-part and monotonically increasing; `msix.ps1`
  derives `x.y.z.0` and fails on a pre-release suffix.

## Testing
- Local (Windows): `msix.ps1` builds; `Add-AppxPackage` of the signed
  sideload package installs, Start entry launches capture, `rustshot` alias
  works, startup task appears in Task Manager › Startup apps, uninstall clean.
- CI: `appstreamcli validate`, `desktop-file-validate`, `flatpak-builder`
  bundle build, `snapcraft` build, `makeappx` build, `lipo -info` shows both
  archs, `codesign --verify` on MAS app when signed, `pkgutil --check-signature`.
- Store-side verification is manual per `docs/STORES.md`.

## Out of scope
Wayland support (affects Flathub/Snap reviews — documented in metainfo as
X11-only), `NSURLSession` uploader, Chocolatey/Scoop, Debian/Fedora official
repos, ARM64 Windows/Linux builds.

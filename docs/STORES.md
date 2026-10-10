# Publishing Rustshot

Push a tag that matches `Cargo.toml` (`git tag v0.1.0 && git push origin v0.1.0`).
`release.yml` builds everything and attaches it to a GitHub Release. Each store
job below runs only after its repository variable `PUBLISH_<X>` is `true`
(Settings › Secrets and variables › Actions). Set the switch last, after a
manual first submission where noted.

## Before any store: screenshots
- [ ] Capture `docs/screenshots/overlay-dark.png` (and `overlay-light.png`) from
      the redesigned UI, 1440×900 or larger; commit to `main`.
- [ ] Add a `<release>` entry to packaging/linux/io.github.nappsllc.rustshot.metainfo.xml
      for every tag (newest first).

## Microsoft Store (MSIX)
1. Partner Center account (individual or company): https://partner.microsoft.com/dashboard
2. Reserve the name "Rustshot" › Product identity. Copy into repo **variables**:
   `MSIX_IDENTITY_NAME` (Package/Identity/Name), `MSIX_PUBLISHER` (CN=…),
   `MSIX_PUBLISHER_DISPLAY`, `MSSTORE_APP_ID` (Store ID), `MSSTORE_SELLER_ID`.
   Set the identity variables (`MSIX_IDENTITY_NAME`, `MSIX_PUBLISHER`) before
   pushing the first tag: the release's .msix is built with them, and the
   `msstore` job fails if they are empty.
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
   Export the public certificate as `rustshot-sideload.cer` for testers. They
   must trust it in an elevated PowerShell, in `Cert:\LocalMachine\TrustedPeople`
   (`CurrentUser\TrustedPeople` is not honoured by `Add-AppxPackage`):
   `Import-Certificate -FilePath rustshot-sideload.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople`
6. Set `PUBLISH_MSSTORE=true`.

Note: the unsigned `rustshot-<v>-x64.msix` attached to GitHub Releases is the
Store upload package; end users cannot install it directly.

## winget
1. Fork https://github.com/microsoft/winget-pkgs under the account that owns the token.
2. Classic PAT with `public_repo` → secret `WINGET_TOKEN`.
3. First manifest by hand: `wingetcreate new <setup.exe URL from the release>`
   — id `nappsllc.rustshot`, installer type `nullsoft`, scope `user`; submit.
4. After it merges, set `PUBLISH_WINGET=true`.

## Known issues
- Linux builds are X11-only; Wayland sessions need XWayland (stated in metainfo).

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

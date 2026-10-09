# Publishing rustshot

Push a tag that matches `Cargo.toml` (`git tag v0.1.0 && git push origin v0.1.0`).
`release.yml` builds everything and attaches it to a GitHub Release. Each store
job below runs only after its repository variable `PUBLISH_<X>` is `true`
(Settings › Secrets and variables › Actions). Set the switch last, after a
manual first submission where noted.

## Before any store: screenshots
- [ ] Capture `docs/screenshots/overlay-dark.png` (and `overlay-light.png`) from
      the redesigned UI, 1440×900 or larger; commit to `main`.

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
   Export the public certificate as `rustshot-sideload.cer` for testers. They
   must trust it in an elevated PowerShell, in `Cert:\LocalMachine\TrustedPeople`
   (`CurrentUser\TrustedPeople` is not honoured by `Add-AppxPackage`):
   `Import-Certificate -FilePath rustshot-sideload.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople`
6. Set `PUBLISH_MSSTORE=true`.

## winget
1. Fork https://github.com/microsoft/winget-pkgs under the account that owns the token.
2. Classic PAT with `public_repo` → secret `WINGET_TOKEN`.
3. First manifest by hand: `wingetcreate new <setup.exe URL from the release>`
   — id `nappsllc.rustshot`, installer type `nullsoft`, scope `user`; submit.
4. After it merges, set `PUBLISH_WINGET=true`.

## Known issues
- Linux builds are X11-only; Wayland sessions need XWayland (stated in metainfo).

## macOS and Linux
Mac App Store, Homebrew, Flathub, Snap Store and AUR packaging are planned
(see docs/superpowers/plans/2026-10-09-store-packaging.md) and not built yet.

# Rustshot privacy policy

Rustshot does not collect, store or transmit personal data or telemetry.

- Screenshots stay on your computer unless you choose **Upload**. Upload sends
  only the selected image to imgur (https://imgur.com) using Rustshot's
  public client id; imgur's privacy policy then applies to that image. The
  returned link is copied to your clipboard and printed to the console.
- Settings are stored locally in `config.toml` in your user config folder.
- Update check (direct downloads only; off with `check_updates = false` or in
  Settings): at most once a day, and when you choose *Check for updates*, the
  daemon sends one anonymous request to the GitHub Releases API
  (`api.github.com`) for the latest release. Store, Flathub and Snap installs
  do not check. If you choose **Update**, the new version and its
  `SHA256SUMS` are downloaded from `github.com/nappsllc/rustshot/releases`
  (GitHub redirects the download to its own servers,
  `objects.githubusercontent.com` / `release-assets.githubusercontent.com`).
  These requests carry no data about you or your screenshots; GitHub sees
  your IP address and a `rustshot/<version>` user agent, as with any download.
- Rustshot makes no other network requests.

Questions: https://github.com/nappsllc/rustshot/issues

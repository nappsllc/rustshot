# Update Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Tell users of direct downloads (setup.exe / dmg / AppImage / deb / tarball) when a newer rustshot release exists, without touching store-managed installs.

**Architecture:** `src/update.rs` holds pure logic (version compare, GitHub JSON parsing, schedule, managed-install detection) plus `check_now()` built on a per-OS `http_get` (WinHTTP on Windows, `curl` on Linux/macOS — the uploader's existing approach). The daemon runs a background checker thread at most once per 24 h (timestamp file in the config dir); a found update is shown as a 6-second Info toast the next time the capture overlay opens. `rustshot update` checks immediately and opens the release page.

**Tech Stack:** Rust 2024, WinHTTP via `windows` 0.62 (+ `Win32_Storage_Packaging_Appx` feature for MSIX detection), `curl`; no new crates.

## Global Constraints
- Source of truth: `GET https://api.github.com/repos/nappsllc/rustshot/releases/latest` with headers `User-Agent: rustshot/<version>` and `Accept: application/vnd.github+json`. Fields used: `tag_name` (e.g. `v0.2.0`), `html_url`, `draft`, `prerelease` (skip drafts/prereleases).
- Versions are `x.y.z` (optional leading `v`); anything else is "not comparable" → no notice.
- Config `check_updates` (bool, default `true`); `false` disables the daemon checker (the `update` command still works).
- Managed installs never run the background check; `rustshot update` prints "rustshot is managed by <channel>; update it there." Detection: Windows MSIX package (`GetCurrentPackageFullName` succeeds), `FLATPAK_ID` set (Flathub), `SNAP` set (Snap Store), `APP_SANDBOX_CONTAINER_ID` set (Mac App Store).
- Background checker: first check 60 s after daemon start, then re-evaluated every 6 h; a check runs only if the last successful check is ≥ 24 h old (`<config_dir>/update-check`, unix seconds). Network errors are silent in the daemon (stderr only) and do not update the timestamp.
- No new dependencies; every task keeps `cargo test` and clippy `-D warnings` clean on Windows and `--target x86_64-unknown-linux-gnu` / `--target x86_64-apple-darwin`.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 1: `update` module, HTTP GET, `rustshot update` command

**Files:** Create `src/update.rs`. Modify `src/main.rs` (module, `update` subcommand + help text), `src/config.rs` (`check_updates`), `src/export_win.rs`, `src/export_linux.rs`, `src/export_macos.rs` (`http_get`), `Cargo.toml` (feature).

**Produces:**
- `pub fn http_get(host: &str, path: &str, headers: &[(&str, &str)]) -> Result<String, String>` in each `export_*` backend, re-exported like `do_upload` (check how `export.rs` exposes `do_upload` and follow it). Windows: factor the WinHTTP session/connect/request/read code out of `upload_winhttp` into a private `fn winhttp(method: &str, host: &str, path: &str, headers: &str, body: &[u8]) -> Result<(u32, String), String>` (status via `WinHttpQueryHeaders` with `WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER`), used by both upload and GET; non-2xx → `Err("HTTP <code>")`. Linux/macOS: `curl -fsS --max-time 15 -H … https://host/path`.
- `update.rs`:
  - `pub fn parse_version(s: &str) -> Option<(u32, u32, u32)>`
  - `pub fn is_newer(latest: &str, current: &str) -> bool`
  - `pub struct Release { pub version: String, pub url: String }` (`Clone, Debug, PartialEq`)
  - `pub fn parse_latest(json: &str) -> Option<Release>` (None for draft/prerelease, missing fields, or non-comparable tag; `version` without the `v`)
  - `pub fn managed_by(env: impl Fn(&str) -> Option<String>, msix: bool) -> Option<&'static str>` → "Microsoft Store" / "Flathub" / "Snap Store" / "Mac App Store"; `pub fn managed_install() -> Option<&'static str>` calls it with `std::env::var(..).ok()` and the Windows MSIX probe (false elsewhere)
  - `pub fn due(last_unix: Option<u64>, now_unix: u64) -> bool` (≥ 86 400 s or never)
  - `pub fn check_now() -> Result<Option<Release>, String>` — GET, parse, `Some` only if newer than `env!("CARGO_PKG_VERSION")`
  - `pub fn open_url(url: &str)` — Windows `rundll32 url.dll,FileProtocolHandler <url>`, macOS `open <url>`, Linux `xdg-open <url>` (spawn, ignore errors)
  - Minimal JSON helpers local to update.rs: string field (handle `\"` escapes and `\/`), boolean field (`"draft":true`).
- `rustshot update`: if `managed_install()` → print the managed message, exit 0. Else `check_now()`: newer → print `rustshot <v> is available: <url>` and `open_url`; up to date → `rustshot <current> is up to date`; Err → `error: update check failed: <e>` exit 1.
- Config: `check_updates: bool` (default `true`), parsed with `as_bool`, in `to_toml`.

- [ ] Step 1: failing tests in `update.rs`: version parsing (`v0.2.0`, `0.10.3`, rejects `1.2`, `1.2.3-beta`, `x.y.z`); `is_newer` (0.1.10 > 0.1.9, equal → false, older → false, non-comparable → false); `parse_latest` (real-shaped GitHub JSON with nested `author` object containing its own `html_url` AFTER the top-level one — must return the top-level release URL: use the first `"html_url"` that appears before `"assets"`/`"author"`, or better parse the top-level `"html_url"` by taking the first occurrence — GitHub puts the release `html_url` before `author`; include such a fixture), prerelease/draft → None; `managed_by` for each env var and for `msix = true`; `due` (None → true, 23 h → false, 24 h → true). Config roundtrip includes `check_updates`.
- [ ] Step 2: implement; Windows MSIX probe uses `windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName` (add `"Win32_Storage_Packaging_Appx"` to Cargo features): call with a zero length and null buffer; result `ERROR_INSUFFICIENT_BUFFER` means packaged, `APPMODEL_ERROR_NO_PACKAGE` (15700) means not.
- [ ] Step 3: refactor upload to the shared `winhttp` helper; existing upload test (`live_imgur_upload`, ignored) still compiles.
- [ ] Step 4: verify; run `cargo run -- update` once (network GET to api.github.com; read-only) and report its output. Commit `update: version check against GitHub releases; rustshot update command`.

### Task 2: Daemon background checker + toast

**Files:** Modify `src/update.rs`, `src/editor/mod.rs`, `src/main.rs` (pass config).

**Produces:**
- `update.rs`: `pub fn spawn_checker(enabled: bool) -> Option<std::sync::mpsc::Receiver<Release>>` — `None` if `!enabled` or `managed_install().is_some()`; else spawns a thread: sleep 60 s, then loop { read stamp file; if `due` → `check_now()`; on `Ok` write `now` to the stamp file and send `Some(r)` results; on `Err` eprintln; sleep 6 h }. Stamp path `crate::config::config_dir().join("update-check")` (create the dir if missing).
- `editor/mod.rs`: `App` gets `updates: Option<Receiver<Release>>` and `update_pending: Option<Release>`; `run()` sets `updates = update::spawn_checker(cfg.check_updates)` for `RunKind::Daemon` only. `pump()` drains `try_recv` into `update_pending`. `begin_capture` takes `update_pending` and sets `edit.notice = Some(Toast::new(format!("rustshot {} is available — run `rustshot update`", r.version), ToastKind::Info).with_ttl(6000.0))`.
- `Toast` gets a `ttl: f32` field initialised from the kind in `new` (1600 / 4000), `fn with_ttl(self, ms: f32) -> Self`, and `ttl_ms()` returns the field (fade logic unchanged).

- [ ] Step 1: failing tests: `Toast::with_ttl(6000.0)` → not expired at 6050 ms, expired after 6100 ms; `spawn_checker(false)` → None.
- [ ] Step 2: implement; verify; commit `update: daemon checks once a day and announces new releases on the next capture`.

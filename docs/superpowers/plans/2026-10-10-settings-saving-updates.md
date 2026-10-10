# Settings, Saving, Shortcuts and In-App Updates — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement `docs/superpowers/specs/2026-10-10-settings-saving-updates-design.md`: dialog-free saving into dated folders in PNG/JPEG/BMP, remappable shortcuts, an update dialog that downloads, verifies and installs, and a native Settings window.

**Architecture:** Saving and shortcuts are pure-logic changes in `config.rs`, `export.rs`, a new `keymap.rs`, and `editor/mod.rs`. Updates split into logic (`update.rs`: release assets, SHA-256, install kinds, download) and installers (`update_install.rs`). A windowed mode in `wind_*` plus a small immediate-mode control kit (`src/ui/`) back two windows: the update dialog and the Settings window, both run from the daemon on their own thread with their own message loop.

**Tech Stack:** Rust 2024, `windows` 0.62 (WinHTTP, Shell IFileOpenDialog, Win32 windowing), Xlib FFI, AppKit FFI, new crate `jpeg-encoder` (approved by the user, default features off, `std` only).

## Global Constraints
- Spec values are binding: config keys/defaults (`save_path` default `<Pictures>/rustshot`, `save_subfolder = true`, `subfolder_pattern = "%F"`, `save_format = "png"`, `jpeg_quality = 90`, `save_dialog = false`, `skip_version = ""`), shortcut defaults table, dialog texts and buttons ("Update", "Skip this version", "Cancel"; "rustshot is up to date" + "OK"; "Retry"/"Close").
- Downloads only from `https://github.com/nappsllc/rustshot/releases/download/`, redirects only to `objects.githubusercontent.com` / `release-assets.githubusercontent.com`; install refused on SHA-256 mismatch or a missing `SHA256SUMS` entry.
- Exe growth ≤ 120 KB total (measure release exe size before Task 1 and after each task; record in the report).
- UI follows the existing theme tokens, Inter Medium, Lucide icons; dark and light; preview PNGs for every new screen state.
- Every task: `cargo test`, `cargo clippy --all-targets -- -D warnings` on Windows, plus `cargo clippy --target x86_64-unknown-linux-gnu` and `--target x86_64-apple-darwin` (check builds). CI uses clippy 1.99 (local may be older): avoid `chunks_exact(n)` on byte slices (use `as_chunks`).
- Never kill processes by name; kill only PIDs you started. The user runs rustshot from `%LOCALAPPDATA%\rustshot` and `target\release`: build with a separate `CARGO_TARGET_DIR` (e.g. `target-sub`), and e2e tests must never signal the user's daemon (use a unique instance name/env override as the existing e2e tests do).
- Never commit certificates or secrets; no `${{ }}` inside workflow `run:` bodies (pass via `env:`).
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 1: Saving — folders, formats, Ctrl+S without dialog
**Files:** `Cargo.toml`, `src/config.rs`, `src/export.rs`, `src/editor/mod.rs`, `src/editor/toolbar.rs`/`chrome.rs` (Save As tooltip), `src/main.rs` (CLI help text), `THIRD_PARTY_NOTICES.md` (jpeg-encoder licence row), tests in the same files.

**Interfaces produced:**
- `config::Config` fields `save_subfolder: bool`, `subfolder_pattern: String`, `save_format: String`, `jpeg_quality: u8`, `save_dialog: bool` (parsed, validated, written by `to_toml`).
- `export::Format { Png, Jpeg, Bmp }` with `Format::from_ext(&str) -> Option<Format>`, `Format::ext(self) -> &'static str`, `Format::from_config(&Config) -> Format`.
- `export::encode(img: &PixBuf, fmt: Format, quality: u8) -> Result<Vec<u8>>`.
- `export::auto_save_path(cfg: &Config, now: Tm) -> PathBuf` (pure; `Tm` = the local-time tuple `format_filename` already uses, factored so tests pass a fixed time) and `export::sanitize_component(&str) -> String`.
- `export::Task::Save { path: Option<PathBuf>, ask: bool }` — `ask=false` → `auto_save_path`; `ask=true` → dialog seeded with that folder and name.
- `export::default_save_dir(cfg)` returns `<Pictures>/rustshot` when `save_path` is empty.

- [ ] Write failing tests: `auto_save_path` with/without subfolder, custom patterns, `save_path` empty → Pictures/rustshot; sanitize replaces `<>:"/\|?*` and control chars with `-`, strips `..` path components and trailing dots/spaces; `Format::from_ext` for png/jpg/jpeg/bmp/unknown; BMP header bytes for a 2×2 image (file size, offset, BI_BITFIELDS masks, negative height top-down) and pixel bytes; JPEG output starts `FF D8`, ends `FF D9`, SOF0/SOF dimensions equal the image, quality 30 output smaller than quality 95; config round trip of new keys and rejection of `save_format = "gif"` / `jpeg_quality = 0`.
- [ ] Add `jpeg-encoder` (latest 0.6.x; `default-features = false, features = ["std"]`). Use 4:2:0 sampling below quality 90 and 4:4:4 at ≥ 90; drop alpha (composite over white is not needed — capture pixels are opaque).
- [ ] Implement BMP writer by hand (32-bit, BI_BITFIELDS, top-down via negative height).
- [ ] `save_image` uses `encode` with the format from the path's extension; remove the "JPEG not supported" error. CLI `--path foo.jpg` works.
- [ ] Editor: Ctrl+S → `Save { ask: cfg.save_dialog }`; Ctrl+Shift+S → `Save { ask: true }`; toolbar Save button = Ctrl+S behaviour. Toast shows the full saved path (existing message flow).
- [ ] Clipboard and upload stay PNG.
- [ ] Run all checks; record exe size delta; commit.

### Task 2: Keymap and shortcuts
**Files:** create `src/keymap.rs`; modify `src/config.rs` (`[shortcuts]` table), `src/hotkey.rs` (expose chord parser if needed), `src/editor/mod.rs` (`handle_key`), `src/editor/chrome.rs` (tooltip key caps from the keymap), `src/main.rs` (`config --check` warnings).

**Interfaces produced:**
- `keymap::Action` enum covering: each existing tool (only tools present in `Tool`), `Copy, Save, SaveAs, Upload, Undo, Redo, SelectAll, DeleteObject, TogglePalette, Accept, Cancel`. (Arrow nudges/resizes stay built in, not remappable.) `Action::id(self) -> &'static str` (config key, e.g. `"tool_pencil"`, `"save_as"`) and `Action::label(self) -> &'static str` (Settings table description).
- `keymap::Chord { vk: u32, ctrl: bool, shift: bool, alt: bool, meta: bool }`, `Chord::parse(&str) -> Option<Chord>`, `Chord::display(&self) -> String` ("Ctrl+Shift+S").
- `keymap::Keymap` with `Keymap::defaults()`, `Keymap::from_config(&BTreeMap<String,String>) -> (Keymap, Vec<String> warnings)`, `resolve(vk, Mods) -> Option<Action>`, `chords(Action) -> &[Chord]`, `conflicts() -> Vec<(Chord, Vec<Action>)>`, `to_config() -> BTreeMap<String,String>` (only non-default entries).
- `Config.shortcuts: BTreeMap<String, String>`.

- [ ] Failing tests: defaults match the spec table (P D A S R C M T N B I G where the tool exists; Ctrl+C/S/Shift+S/U/Z/Shift+Z/Y/A, Del, Space, Enter, Esc); parse/display round trip incl. `Meta`, `Del`, `Space`, letters, F-keys; override replaces default; empty string unbinds; unknown action → warning; conflict detection; first action in table order wins on conflict; `resolve` matches modifiers exactly (P does not fire on Shift+P or Ctrl+P).
- [ ] Read `src/editor/toolbar.rs` `Tool`/`Act` and `objects.rs` first: bind only tools that exist; if the editor has no notion of a selected object, implement `DeleteObject` as "delete the most recently committed object" only if that matches an existing undoable operation, otherwise leave `DeleteObject` out of the enum and say so in the report.
- [ ] Replace the hard-coded `if vk == 'Z'` chain with `keymap.resolve`; text-editing mode still consumes keys first; Esc/Enter keep their current semantics. `SelectAll` sets the selection to the whole shot (the area of all monitors in the shot).
- [ ] Toolbar tooltips show the first bound chord of their action (replace hard-coded `&[MOD, "S"]` pairs).
- [ ] `config --check` prints keymap warnings and conflicts.
- [ ] Checks; commit.

### Task 3: Update logic — assets, notes, SHA-256, install kind, download
**Files:** `src/update.rs`, create `src/sha256.rs`, `src/export_win.rs` / `export_linux.rs` / `export_macos.rs` (download-to-file), `src/config.rs` (`skip_version`), `.github/workflows/release.yml` (publish `SHA256SUMS`).

**Interfaces produced:**
- `update::Release { version, url, notes: String, assets: Vec<Asset> }`, `update::Asset { name: String, url: String, size: u64 }`; `parse_latest` fills them (only assets whose `browser_download_url` passes `is_safe_download_url`).
- `update::is_safe_download_url(&str) -> bool` (prefix `https://github.com/nappsllc/rustshot/releases/download/`), `update::is_allowed_redirect_host(&str) -> bool`.
- `update::notes_excerpt(body: &str, max_lines: usize) -> String` (markdown headings/bullets/links/emphasis stripped to plain text).
- `update::InstallKind { WinInstaller, WinPortable, AppImage(PathBuf), Tarball, LinuxManaged, MacApp(PathBuf), Store(&'static str) }`, `update::detect_install() -> InstallKind` with a pure core `detect_from(exe: &Path, env, probes) -> InstallKind` for tests.
- `update::pick_asset(kind: &InstallKind, rel: &Release) -> Option<&Asset>` (patterns: `rustshot-*-setup.exe`, `rustshot-*-windows-x86_64.exe`, `*.AppImage`, `rustshot-*-linux-x86_64.tar.gz`, `rustshot-*-macos-universal.zip` — check exact names against `release.yml`).
- `sha256::digest(&[u8]) -> [u8; 32]`, streaming `Sha256::{new, update, finish}`, `sha256::hex`.
- `update::parse_sums(text) -> HashMap<String, [u8;32]>` (`sha256sum` format, `*` binary marker tolerated).
- Platform `download_to(url, dest: &Path, progress: &dyn Fn(u64, Option<u64>) -> bool /*false = cancel*/) -> Result<(), String>`; Windows via WinHTTP following redirects manually and checking each hop with `is_allowed_redirect_host`; Linux/macOS via `curl -fL --proto =https --proto-redir =https --max-redirs 5 -o` with progress parsed from `--progress-bar`-free polling of the dest file size; after download the final URL host is validated (`curl -w '%{url_effective}'`).
- `update::fetch_verified(rel, asset, dir, progress) -> Result<PathBuf, String>`: downloads `SHA256SUMS` + asset, verifies, deletes on failure or cancel.
- `Config.skip_version`; `spawn_checker` skips a release whose version equals it.

- [ ] Failing tests: NIST SHA-256 vectors ("", "abc", 448-bit message, 1,000,000 × 'a'); parse_sums; parse_latest with assets/notes from a captured JSON fixture (craft one modelled on GitHub's `releases/latest` shape incl. an unsafe asset URL that must be dropped); notes_excerpt; pick_asset per kind; detect_from cases (uninstall.exe present → WinInstaller, else portable; `APPIMAGE` env; `/usr/bin` → LinuxManaged; writable dir → Tarball; Store envs → Store); redirect host guard; skip_version logic.
- [ ] release.yml: after artifacts are collected, `sha256sum * > SHA256SUMS` over the uploaded files and attach it to the release (env-only values in `run:`).
- [ ] Implement; checks; commit.

### Task 4: Installers — apply the downloaded update
**Files:** create `src/update_install.rs`; modify `packaging/installer.nsi`, `src/main.rs` (startup cleanup of `*.old`; hidden `--relaunch-after <pid>` not needed if NSIS handles it), `src/instance.rs` if the daemon must release the mutex before the installer starts it.

**Interfaces produced:**
- `update_install::apply(kind: &InstallKind, file: &Path) -> Result<Applied, String>`, `Applied { RestartingNow, OpenedPage }`. The caller (daemon) exits right after `RestartingNow`.
- `update_install::cleanup_previous()` called at startup (deletes `<exe>.old`, the `%TEMP%\rustshot-update` dir).
- WinInstaller: spawn `setup.exe /S /RELAUNCH` detached, return. NSIS: `.onInit` with `/S` waits (up to 10 s) for any running `rustshot.exe` from `$INSTDIR` to exit — loop `FindWindow "rustshot_tray"` (the daemon's hidden window class) — and at the end of a silent install with `/RELAUNCH` executes `"$INSTDIR\rustshot.exe" daemon`. Keep the existing interactive installer behaviour unchanged.
- WinPortable: write new exe next to the current as `rustshot.exe.new`, `MoveFileExW(current → .old)`, `.new → current`, spawn `current daemon` with an env var telling the new process to wait for the old PID to exit before acquiring the instance mutex, return.
- AppImage/Tarball: same swap with `chmod 755`, `rename`.
- LinuxManaged / MacApp (until tested) / Store: open the release page → `OpenedPage`.

- [ ] Failing tests: swap logic on temp dirs with a fake exe (pure function over paths: `swap_in_place(current, new) -> Result<()>` and rollback when the second rename fails); cleanup removes `.old`; NSIS script builds with `makensis` if available locally (otherwise CI covers it).
- [ ] Manual/e2e check (Windows): a portable copy in a temp dir updated from a local fixture file (inject the file directly into `apply`, no network) ends running the new binary; kill only that PID.
- [ ] Checks; commit.

### Task 4b: Size — bake the font at build time
Baseline (cargo bloat, 983ef2e): ttf_parser ~69.5 KB + ab_glyph ~4.4 KB + ab_glyph_rasterizer ~5.4 KB of .text. User decision: only this item; keep `anyhow` and float formatting as they are.
**Files:** `build.rs`, `src/fonts.rs`, callers of `ab_glyph` (grep), `Cargo.toml` (move `ab_glyph`/`ttf-parser` to `[build-dependencies]` only), `THIRD_PARTY_NOTICES.md` (crate table: build-only).
- [ ] build.rs parses the embedded Inter Medium subset and writes `$OUT_DIR/font_baked.rs`: per glyph (codepoint, advance, outline as quadratic/line segments in font units, i16), kerning pairs, ascender/descender/line gap, units-per-em. Compact: no per-glyph allocation at runtime, data in `static` arrays.
- [ ] Runtime text rendering rasterises the baked outlines with the existing anti-aliased path code (the icon rasteriser in `raster.rs`/`icon_path.rs`, flattening quadratics) with the same metrics, hinting-free like today.
- [ ] Tests: text measurement (advance widths, kerning) equals the previous ab_glyph values for the whole subset at sizes 11–20 px × scales 1, 1.25, 1.5, 2; rendered glyph coverage within ±8/255 per pixel of the previous output for a sample string (store reference renders generated with the old code before removing it); preview PNGs byte-identical or within that tolerance — view them.
- [ ] Release exe size before/after (expect −70 KB or more); report.

### Task 5: Windowed mode in `wind`
**Files:** `src/wind.rs`, `src/wind_win.rs`, `src/wind_linux.rs`, `src/wind_macos.rs`.

**Interfaces produced:**
- `wind::WindowSpec { title: String, w: u32, h: u32 /* logical px */, resizable: bool, min: (u32, u32) }`.
- `wind::run_window(spec, drv: &mut dyn Driver) -> Result<()>`: creates a normal decorated, DPI-aware, centred top-level window on the monitor under the cursor, runs its message loop until closed (close button → `Ev::Close` to the driver; the driver ends it with `wind::close(hwnd)`), software `frame()` presentation, resize → `Ev::Resize(w, h)` (physical px) with `wind::scale(hwnd) -> f32`.
- New `Ev` variants as needed: `Close`, `Resize`, `Char(char)` (text input; reuse an existing text-input event if the overlay already has one), `Focus(bool)`, `Wheel`.
- Must be callable from a non-main thread on Windows and Linux (own `XOpenDisplay` connection per call). macOS: compile-only implementation using NSWindow on the main thread; document the limitation.
- Window icon from the exe resource (Windows) / `_NET_WM_ICON` from the PNG set (Linux).

- [ ] Test: an ignored-by-default interactive test opening a 400×300 window that renders a filled rect; an automated Windows test that creates the window, posts `WM_CLOSE`, asserts the loop returns and no GDI objects leaked (`GetGuiResources`).
- [ ] Checks; commit.

### Task 6: Control kit (`src/ui/`)
**Files:** create `src/ui/mod.rs`, `src/ui/controls.rs`, `src/ui/text_field.rs`, `src/ui/layout.rs`, `src/ui/folder_dialog.rs` (per-OS: Windows `IFileOpenDialog` with `FOS_PICKFOLDERS`; Linux `zenity --file-selection --directory` then `kdialog --getexistingdirectory`; macOS NSOpenPanel compile-only).

**Interfaces produced:**
- Immediate-mode context `Ui<'a> { fb: &mut Fb, theme: &Theme, input: &Input, focus: &mut FocusState, k: f32 }` with `label`, `button(id, text, primary) -> bool`, `toggle(id, &mut bool) -> bool`, `text_field(id, &mut TextState) -> bool` (caret, selection, Ctrl+A/C/V/X, Home/End, Backspace/Del, clipboard via existing clipboard code — add text get/set if missing), `dropdown(id, &[&str], &mut usize) -> bool`, `slider(id, &mut u8, min, max) -> bool`, `tabs(id, &[&str], &mut usize)`, `table(id, rows, &mut TableState)` with scroll and row selection, `progress(fraction)`, `key_capture(id, &mut Option<Chord>) -> bool`.
- `Input` = per-frame mouse pos/buttons/wheel, typed chars, keys with mods; `FocusState` handles Tab/Shift+Tab order, Enter/Space activation, Esc reported to the caller.
- Visuals: reuse toolbar/popover styling (radii, shadows, accent, hover/pressed states, focus ring) from `editor/chrome.rs`; no new fonts.
- `ui::preview` test helper rendering a closure into a PixBuf and writing PNGs under `RUSTSHOT_PREVIEW_DIR`.

- [ ] Tests: event-driven tests for button click/keyboard activation, toggle, text editing (insert/delete/selection/clipboard paste with an injected clipboard), Tab order, dropdown open/select/close, table scroll and selection, key_capture records a chord and Backspace clears; preview PNGs of a gallery of all controls in dark and light (view them before reporting).
- [ ] Checks; commit.

### Task 7: Update dialog wired into the daemon and tray
**Files:** create `src/update_ui.rs`; modify `src/update.rs` (checker emits `Release` to the daemon instead of opening the browser), `src/main.rs`/daemon loop, `src/tray.rs`, `src/tray_win.rs`, `src/actions.rs`.

**Interfaces produced:**
- `update_ui::show(state: DialogState)` spawns (or focuses, if open) the dialog window on its own thread. `DialogState::{Available(Release), UpToDate, Error(String), Managed(&'static str)}`; internal states Downloading(progress), Verifying, Installing, Failed(msg).
- Layout ≈ 440×300 logical px: app icon + title line, "rustshot X is available (you have Y)", notes excerpt (12 lines, scroll), progress bar while downloading, buttons right-aligned: **Update** (primary), **Skip this version**, **Cancel**. Up to date: message + **OK**. Error: message + **Retry** / **Close**. Managed: "Updates for this install come from <store>." + **OK**.
- Update → `fetch_verified` on a worker thread → `update_install::apply` → on `RestartingNow` the daemon exits cleanly (tray removed, mutex released). Cancel during download aborts and deletes partial files. Skip → write `skip_version` via config save (Task 8's writer if present, else `to_toml` of the loaded config with the one field changed) and close.
- Tray "Check for updates" → `check_now` on a worker → `show(...)`.

- [ ] Tests: dialog state machine (pure: events → state/actions) including cancel mid-download, skip writes the version, retry after error; preview PNGs for each state, dark and light (view them).
- [ ] Manual Windows check: run a dev daemon with an env override pointing the release API at a local fixture JSON (add `RUSTSHOT_UPDATE_FEED` honoured only in debug builds or tests, never in release), see the dialog, click Skip and Cancel paths; kill only your PID.
- [ ] Checks; commit.

### Task 8: Settings window
**Files:** create `src/settings_ui.rs` (and `src/settings_ui/*.rs` per tab if it grows past ~600 lines); modify `src/config.rs` (writer + reload notification), `src/main.rs` (`rustshot settings` command; daemon reload), `src/hotkey*.rs` (re-register after reload), `src/tray.rs`/`tray_win.rs` (menu: "Settings…" replaces "Open config file"), `README.md` (settings + shortcuts docs).

**Interfaces produced:**
- `settings_ui::show()` (single instance, focuses if open). ≈ 640×520 logical px, tabs **General**, **Saving**, **Shortcuts**, buttons **OK** / **Cancel** / **Apply**.
- General: theme dropdown (Auto/Dark/Light), renderer dropdown (Windows only), start at login toggle (hidden for managed installs), check for updates toggle, capture hotkey + quit hotkey key_capture fields, "Open config file" link button.
- Saving: folder text field + **Browse…**, daily subfolders toggle + pattern field, format dropdown (PNG/JPEG/BMP) + JPEG quality slider (disabled unless JPEG), "Ask where to save" toggle, filename editor: token buttons in two columns (Century (00-99) `%C`, Day (001-366) `%j`, Day (01-31) `%d`, Day of Month (1-31) `%e`, Full Date (%Y-%m-%d) `%F`, Full Date (%d-%m-%Y) — inserts `%d-%m-%Y`, Hour (00-23) `%H`, Hour (01-12) `%I`, Minute (00-59) `%M`, Month (01-12) `%m`, Second (00-59) `%S`, Week (01-53) `%V`, Week Day (1-7) `%u`, Year (00-99) `%y`, Year (2000) `%Y`) inserting at the caret, the pattern field, live preview of the full resulting path, **Restore** (default pattern) / **Clear**. Add any tokens above missing from `format_filename` (`%C`, `%e`, `%V`, `%u`) with tests.
- Shortcuts: table Description | Key from `keymap::Action::label` and chords; select a row, press a chord to rebind (key_capture), Backspace clears, conflicting rows shown in the error colour with a note; **Reset all**.
- `config::save(&Config) -> io::Result<()>` (atomic: write temp + rename). Before the first overwrite, if the on-disk text differs from `to_toml(&parse(text))` (i.e. has comments/unknown keys), ask once in-window: "Saving will rewrite config.toml and remove comments." **Save** / **Cancel**.
- Daemon reload: after Apply/OK the daemon reloads config (channel message), re-registers hotkeys, applies `check_updates`. Next capture uses the new config (captures already read config per capture — verify).

- [ ] Tests: pure "form model ↔ Config" round trip; token insertion at caret; preview path equals `auto_save_path`; conflict marking; save is atomic and preserves values; preview PNGs of all three tabs in dark and light plus the rebind state and the rewrite-confirmation (view them).
- [ ] Manual Windows check with a dev daemon: open from the tray, change format to JPEG and folder, Ctrl+S in a capture writes `…\<date>\…jpg`; rebind a tool key and see it work; kill only your PID.
- [ ] Checks; commit.

### Task 9: Verify, measure, document
- [ ] Release exe size vs. the Task 1 baseline (≤ 120 KB growth) and `cargo bloat` top entries; report.
- [ ] Re-run e2e tests; Windows Sandbox clean-system harness, extended with: Settings window opens from the tray; Ctrl+S writes into the dated folder; portable self-update from a local fixture.
- [ ] Update README (Saving, Shortcuts table, Settings, Updates incl. the integrity limitation), `docs/STORES.md` if store behaviour changed, metainfo/NSIS untouched (no release in this plan).
- [ ] Final whole-branch review; fix wave; push `handroll`; CI green on all 5 jobs.

### Task 10: Test hardening
Baseline (cargo llvm-cov, Windows, 024a5fe): 69.7% lines overall; `editor/mod.rs` 56%, `main.rs` 44%, `capture.rs` 47%, `instance.rs` 0% (tests ignored), Windows platform files 0–6%.
**Files:** tests in `src/editor/mod.rs` (or a new `src/editor/tests.rs`), `src/instance.rs`, `tests/e2e.rs`, `.github/workflows/ci.yml`.
- [ ] Editor state-machine tests driving `on_event` with synthetic input: selection create/move/resize from every handle, clamping to the shot and across monitors, every drawing tool's draft→commit, text editing (insert, caret moves, multi-line, commit/cancel), undo/redo across tools and the undo limit, palette/size changes, toast lifecycle, accept/cancel paths, upload result handling (injected receiver). Target ≥ 80% lines in `editor/mod.rs`.
- [ ] `instance.rs`: make the ignored tests deterministic (unique instance names per test, no shared global state, explicit synchronisation instead of sleeps) and un-ignore them; cover second-launch signalling and stale-lock recovery on all platforms the CI runs.
- [ ] `main.rs` CLI: argument parsing and command dispatch tests for every subcommand without touching the screen (pure parse → `Cmd`).
- [ ] CI: run the e2e tests on the Windows runner (`cargo test -- --include-ignored` for the e2e suite with a unique instance name; the runner has an interactive desktop session); Linux job runs tests under `xvfb-run` including X11 capture and window tests (enable the ignored X11 ones there).
- [ ] Coverage job (Linux + Windows) running `cargo llvm-cov --summary-only` and printing the table in the job summary; no threshold gate yet.
- [ ] Report the before/after coverage table.

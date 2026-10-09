# Tray Daemon, Launch Behaviour and App Icon — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Launching rustshot with no arguments starts the background daemon with a tray icon (never a capture). Launching it again while the daemon runs triggers a capture in the running daemon. The tray menu offers Capture, Open config file, Check for updates, Start at login, Quit. The Windows executable and installer carry the app icon.

**Architecture:** The daemon's existing event channel (`hotkey::Hotkeys` → `mpsc::Receiver<HotEvent>`, polled by `editor::App::pump`) gains more senders: the tray and the single-instance listener send `HotEvent::Capture` / `HotEvent::Quit`. Non-editor actions (open config, check updates, toggle autostart) run on the tray's own thread via a shared `actions` module. Per-OS backends: `tray_win.rs` (Shell_NotifyIcon on a message-only window thread), `tray_macos.rs` (NSStatusItem on the AppKit main thread, installed through the existing `wind_macos` hook mechanism), `tray_linux.rs` (StatusNotifierItem + com.canonical.dbusmenu over a hand-rolled D-Bus client). Single instance: Windows named mutex + message to the tray window; Unix: a lock-protected Unix-domain socket.

**Tech Stack:** Rust 2024, `windows` 0.62, AppKit via the repo's objc helpers, D-Bus wire protocol over `std::os::unix::net::UnixStream`; no new crates. Windows icon compiled with the SDK's `rc.exe` from `build.rs`.

## Global Constraints
- User decisions (2026-10-09): default launch = daemon + tray, no capture; second launch while running = capture in the running daemon; tray menu = Capture, Open config file, Check for updates, Start at login (toggle), Quit; Linux tray = StatusNotifierItem; app/tray icons required.
- Left-click on the tray icon = Capture (Windows, Linux `Activate`). macOS: click opens the menu (platform convention).
- "Start at login" is hidden for managed installs (`update::managed_install().is_some()`); MSIX keeps its StartupTask.
- Autostart locations: Windows `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `rustshot` = `"<exe>" daemon` (same value the NSIS installer writes); macOS `~/Library/LaunchAgents/io.github.nappsllc.rustshot.plist` (`ProgramArguments` = [exe, "daemon"], `RunAtLoad` true); Linux `$XDG_CONFIG_HOME/autostart/io.github.nappsllc.rustshot.desktop` (`Exec=<exe> daemon`).
- "Open config file": create `config.toml` with all defaults if missing (make `config::to_toml` non-test), then open it with the OS default handler (`rundll32 url.dll,FileProtocolHandler` / `open` / `xdg-open`) — local file path only, never a URL from outside.
- "Check for updates": `update::check_now()`; newer → `update::open_url`; up to date / error → a native notification is out of scope: show a tray balloon/tooltip on Windows (`NIF_INFO`), and on macOS/Linux print to stderr (acceptable for now).
- Single instance: only the daemon holds the instance lock; one-shot commands (`gui`, `full`, …) are unaffected.
- Every task: `cargo test`, clippy `-D warnings` on Windows, `--target x86_64-unknown-linux-gnu`, `--target x86_64-apple-darwin` (check-only) clean. Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 1: App icon in the Windows executable and installer

**Files:** `packaging/icons/make-all.ps1` (also write `rustshot.ico`), `packaging/icons/rustshot.ico` (generated), `build.rs` (new), `packaging/windows/rustshot.rc` (new), `packaging/installer.nsi`.

- `make-all.ps1` additionally writes `packaging/icons/rustshot.ico`: an ICO container (ICONDIR + ICONDIRENTRY + PNG-compressed images) with sizes 16, 20, 24, 32, 40, 48, 64, 256 (render missing sizes with `make-icon.ps1 -Size N` to temp files). Byte layout: ICONDIR {reserved 0, type 1, count}; each entry {width (0 = 256), height, 0 colors, 0, planes 1, bitcount 32, bytes, offset}; then the PNG bytes.
- `packaging/windows/rustshot.rc`: `1 ICON "../icons/rustshot.ico"` plus a VERSIONINFO block (FileDescription "rustshot", ProductName "rustshot", CompanyName "nappsllc", FileVersion/ProductVersion from `CARGO_PKG_VERSION`, OriginalFilename "rustshot.exe", LegalCopyright "GPL-3.0-only") — generate the .rc into `OUT_DIR` from build.rs so the version is filled in.
- `build.rs`: only when `CARGO_CFG_TARGET_OS == "windows"` and `CARGO_CFG_TARGET_ENV == "msvc"`: locate `rc.exe` (newest `C:\Program Files (x86)\Windows Kits\10\bin\10.*\x64\rc.exe` by version; or `RC` env var), compile to `OUT_DIR/rustshot.res`, emit `cargo:rustc-link-arg-bins=<res path>`, and `cargo:rerun-if-changed` for the .ico/.rc template. If rc.exe is missing, emit `cargo:warning=` and continue (no icon) — never fail the build.
- `installer.nsi`: `Icon "..\packaging\icons\rustshot.ico"` and `UninstallIcon` same (relative to the .nsi: `icons\rustshot.ico`).
- Verify: `cargo build --release`; PowerShell `[System.Drawing.Icon]::ExtractAssociatedIcon('target\release\rustshot.exe')` returns an icon whose bitmap is not the default (compare to `notepad.exe`'s handle differs — or check the PE has an RT_GROUP_ICON resource: `(Get-Item target\release\rustshot.exe).VersionInfo.FileDescription -eq 'rustshot'`). Commit `packaging: embed the app icon and version info in rustshot.exe; installer icon`.

### Task 2: Default launch = daemon; single instance; second launch captures

**Files:** `src/main.rs`, `src/instance.rs` (new), `src/hotkey.rs` (expose a sender), `src/editor/mod.rs` (wire), `packaging/windows/AppxManifest.xml.in`.

- `main.rs`: no command → `Cmd::Daemon` (help text: `daemon` gets "(default)", `gui` loses it); parser test updated.
- `hotkey::Hotkeys::sender(&self) -> std::sync::mpsc::Sender<HotEvent>`.
- `instance.rs`: `pub enum Instance { Primary(Guard), Signalled }` and `pub fn acquire_or_signal() -> Instance`.
  - Windows: `CreateMutexW(None, true, "Local\\rustshot-daemon")`; if `GetLastError() == ERROR_ALREADY_EXISTS` → find the tray window (`FindWindowW("rustshot_tray", None)`, created in Task 3; before Task 3 exists, use a message-only window created here with class `rustshot_tray` and keep it in the Guard) and `PostMessageW(hwnd, WM_APP + 7, 0, 0)` → `Signalled`. The Guard owns the mutex handle and the window; a `pub fn listen(&self, tx: Sender<HotEvent>)` makes `WM_APP + 7` send `HotEvent::Capture` (the window procedure runs on a dedicated thread with its own message loop).
  - Unix: socket `$XDG_RUNTIME_DIR/rustshot.sock` (Linux; fallback `/tmp/rustshot-<uid>.sock`) or `$TMPDIR/rustshot-<uid>.sock` (macOS). Try `UnixStream::connect` → write `capture\n` → `Signalled`. Else remove a stale socket, `UnixListener::bind`, and `listen` spawns a thread: each connection reading `capture` sends `HotEvent::Capture`.
- `main.rs` `Cmd::Daemon`: `match instance::acquire_or_signal() { Signalled => exit 0, Primary(g) => keep g alive for the daemon's lifetime and call g.listen(hotkeys sender) }` — wire via `editor::run` taking an extra `Option<instance::Guard>` or an `extra_senders` hook; keep editor changes minimal.
- `AppxManifest.xml.in`: the main `Rustshot` application now launches the daemon by default (no args) — keep both entries; no manifest change needed unless tests show otherwise; note it.
- Tests: parser default; Unix socket path logic (pure fn); a unit test of the Unix signal round-trip in a temp dir (cfg(unix), runs in CI); Windows: ignored test that acquires twice in-process (second must be Signalled).
- Commit `daemon: default launch, single instance; launching again captures`.

### Task 3: Shared tray actions + Windows tray

**Files:** `src/actions.rs` (new), `src/autostart.rs` (new), `src/config.rs` (`to_toml` non-test, `ensure_config_file() -> PathBuf`), `src/tray.rs` (new, shared), `src/tray_win.rs` (new), `src/main.rs`/`editor` wiring, `Cargo.toml` features (`Win32_UI_Shell` if needed).

- `autostart.rs`: `pub fn is_enabled() -> bool`, `pub fn set(on: bool) -> Result<(), String>` per OS as in Global Constraints (exe path from `std::env::current_exe()`; on macOS use the path inside the .app bundle if running from one). Pure helpers with tests: `launch_agent_plist(exe) -> String`, `autostart_desktop(exe) -> String`.
- `actions.rs`: `open_config()`, `check_updates() -> Result<Option<Release>, String>` (wraps update), `toggle_autostart() -> Result<bool, String>`.
- `tray.rs`: `pub enum MenuItem { Capture, OpenConfig, CheckUpdates, Autostart(bool), Quit }`, `pub fn menu(autostart: Option<bool>) -> Vec<MenuItem>` (autostart None = hidden for managed installs) with tests; `pub fn spawn(tx: Sender<HotEvent>)` dispatching to the backend; menu labels: "Capture", "Open config file", "Check for updates", "Start at login", "Quit rustshot".
- `tray_win.rs`: thread with message-only window class `rustshot_tray` (merge with Task 2's window: the instance guard's window becomes the tray window); `Shell_NotifyIconW(NIM_ADD)` with `LoadImageW(GetModuleHandleW(None), MAKEINTRESOURCE(1), IMAGE_ICON, SM_CXSMICON, SM_CYSMICON, LR_DEFAULTCOLOR)` (falls back to `IDI_APPLICATION`), tooltip "rustshot", callback `WM_APP + 1`; left button up → Capture; right button up / context menu → `CreatePopupMenu` + `TrackPopupMenu` (call `SetForegroundWindow` first, `PostMessage(WM_NULL)` after); `TaskbarCreated` registered message → re-add icon (Explorer restart); NIM_DELETE on quit. Update-check results shown with `NIF_INFO` balloon.
- Tests: menu composition; plist/desktop generation; ignored live test that creates and removes the tray icon.
- Commit `tray: Windows notification-area icon with Capture/Config/Updates/Start at login/Quit`.

### Task 4: macOS tray (NSStatusItem)

**Files:** `src/tray_macos.rs`, `src/wind_macos.rs` (hook install/pump on the main thread, like `HotkeyHook`).

- Install on the main thread: `NSStatusBar.systemStatusBar` → `statusItemWithLength:` (NSVariableStatusItemLength = -1); button image = template `NSImage` built from a 18×18 (and @2x 36×36) monochrome RGBA glyph rendered with the repo rasterizer (the capture-bracket glyph from `make-icon.ps1`: four corner brackets + centre dot, black on transparent), `setTemplate:YES`.
- `NSMenu` with `NSMenuItem`s whose target is an instance of a registered ObjC class `RustshotTrayTarget` with method `rsAction:` (sender's `tag` = item index) → sends the action on the existing channel / runs actions. "Start at login" uses `setState:` (NSControlStateValueOn = 1).
- Compile-check only here (`--target x86_64-apple-darwin`); CI builds it. Commit `tray: macOS menu-bar item`.

### Task 5: Linux tray (StatusNotifierItem over a hand-rolled D-Bus client)

**Files:** `src/dbus.rs` (new, cfg(unix) but only used on Linux), `src/tray_linux.rs`, `packaging/flatpak/io.github.nappsllc.rustshot.yml` (`--talk-name=org.kde.StatusNotifierWatcher`), `snap/snapcraft.yaml` (plug `unity7`), `.github/workflows/ci.yml` (Linux job: `dbus-x11` or `dbus` package + a `dbus-run-session -- cargo test -- --ignored dbus_` smoke step).

- `dbus.rs`: connect to `DBUS_SESSION_BUS_ADDRESS` (`unix:path=` and `unix:abstract=`), SASL `EXTERNAL` with the hex-encoded uid, `BEGIN`; little-endian message marshalling (header fields PATH, INTERFACE, MEMBER, ERROR_NAME, REPLY_SERIAL, DESTINATION, SENDER, SIGNATURE; body types used: `y b i u s o g a{sv} v (ii) a(iiay) (ia{sv}av) ay as`) with 8-byte alignment rules; `Hello`; method calls, replies, errors, signals; reading incoming messages. Unit tests against byte fixtures computed from the D-Bus spec (header layout, padding, string/array encoding).
- `tray_linux.rs`: register using the connection's unique name as the item service (`org.kde.StatusNotifierWatcher.RegisterStatusNotifierItem(unique_name)`) — works in Flatpak without own-name; serve `/StatusNotifierItem` (`org.kde.StatusNotifierItem`: properties Category "ApplicationStatus", Id "rustshot", Title "rustshot", Status "Active", IconName "io.github.nappsllc.rustshot", IconPixmap (ARGB32 big-endian at 22/32/48 px rendered by the repo rasterizer), ItemIsMenu false, Menu "/MenuBar"; methods Activate → Capture, ContextMenu/SecondaryActivate no-op; `org.freedesktop.DBus.Properties.Get/GetAll`; `org.freedesktop.DBus.Introspectable.Introspect`) and `/MenuBar` (`com.canonical.dbusmenu`: GetLayout, GetGroupProperties, Event(id, "clicked") → action, AboutToShow → false; properties Version 3, Status "normal"). Emit `LayoutUpdated` after toggling autostart. If the watcher isn't present (GNOME without extension), log once and keep the daemon running without a tray.
- Tests: marshalling fixtures; GetLayout reply shape; ignored `dbus_hello` smoke test run in CI under `dbus-run-session`.
- Commit `tray: Linux StatusNotifierItem with dbusmenu over a built-in D-Bus client`.

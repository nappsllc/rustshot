# Settings, Saving, Shortcuts and In-App Updates — Design

Approved 2026-10-10. Formats: PNG, BMP, JPEG (via the `jpeg-encoder` crate).

## Goals
1. Ctrl+S saves without a dialog into a configured folder, optionally in
   per-day subfolders, in PNG, JPEG or BMP.
2. Keyboard shortcuts for every tool and action, remappable.
3. Updates: a dialog (Update / Skip this version / Cancel) that downloads,
   verifies and installs the new version.
4. A native Settings window (General, Saving, Shortcuts) instead of editing
   `config.toml` by hand.

Order: saving → shortcuts → widget kit + update window/installer → Settings
window. Windows first; Linux shares the code (needs windowed mode in
`wind_linux.rs`); macOS compiles but stays untested until a Mac is available.

## 1. Saving

New config keys (Flameshot-style names where they exist):

| Key | Default | Meaning |
|---|---|---|
| `save_path` | `""` = `<Pictures>/rustshot` | base folder (exists today; default gains `/rustshot`) |
| `save_subfolder` | `true` | create a subfolder per capture date |
| `subfolder_pattern` | `"%F"` | pattern for that subfolder (same tokens as filenames) |
| `filename_pattern` | `"%F_%H-%M"` | exists today |
| `save_format` | `"png"` | `png` \| `jpg` \| `bmp` |
| `jpeg_quality` | `90` | 1–100 |
| `save_dialog` | `false` | `true` = Ctrl+S always asks (old behaviour) |

- Ctrl+S: path = `save_path / [format(subfolder_pattern)] / format(filename_pattern).ext`,
  directories created, `unique_path` collision suffix as today; toast shows
  the full path. Ctrl+Shift+S: Save As dialog seeded with that folder/name,
  format inferred from the chosen extension.
- `format_filename` rejects path separators and characters invalid on
  Windows (`<>:"/\|?*`) in the expansion (replaced with `-`); subfolder
  pattern may not contain `..`.
- Encoders behind one `encode(img, Format, quality) -> Vec<u8>`:
  PNG (existing `png` crate), BMP (hand-written 32-bit BI_BITFIELDS, top-down
  rows as bottom-up; ~40 lines), JPEG (`jpeg-encoder`, RGB, alpha dropped,
  4:2:0 below quality 90, 4:4:4 at ≥90 to keep text sharp).
- CLI `--path x.jpg` works through the same function (today's "JPEG not
  supported" error goes away).
- Clipboard and upload stay PNG.

## 2. Keyboard shortcuts

Defaults (Flameshot's where they exist):

| Action | Key | Action | Key |
|---|---|---|---|
| Pencil | P | Copy | Ctrl+C |
| Line | D | Save | Ctrl+S |
| Arrow | A | Save As | Ctrl+Shift+S |
| Selection (move) | S | Upload | Ctrl+U |
| Rectangle | R | Undo | Ctrl+Z |
| Circle | C | Redo | Ctrl+Shift+Z, Ctrl+Y |
| Marker | M | Select whole screen | Ctrl+A |
| Text | T | Delete selected object | Del |
| Counter | N | Toggle palette | Space |
| Pixelate | B | Accept (default action) | Enter |
| Invert | I | Cancel | Esc |
| Colour picker | G | Nudge / resize | arrows / Shift+arrows |
| | | Symmetric resize | Ctrl+Shift+arrows |

- Only tools that exist in the toolbar get keys; unknown actions in config
  are warned about by `config --check` and ignored.
- Config: a `[shortcuts]` table, `action = "Ctrl+Shift+S"`; empty string
  unbinds. Parsing reuses the hotkey parser (`hotkey.rs`).
- `Keymap::resolve(vk, mods) -> Option<Action>` replaces the hard-coded
  `if vk == 'Z'` chain in `editor::handle_key`; text-editing mode still
  consumes keys first. Conflicts (two actions on one chord) are reported by
  `config --check` and in the Settings window; the first in table order wins.
- Toolbar tooltips show the bound key (they already render key caps).

## 3. Updates

### Dialog
Shown by the daily check (instead of opening the browser) and by tray
"Check for updates". Content: "rustshot 0.1.2 is available (you have
0.1.1)", the first ~12 lines of the release notes (markdown stripped to
text), buttons **Update**, **Skip this version**, **Cancel**. Manual check
with nothing new → same window, "rustshot is up to date", **OK**. Errors
show in the window with **Retry** / **Close**.
- Skip writes `skip_version = "0.1.2"`; the daily check ignores that version,
  a manual check still shows it.
- Managed installs (Store, Flathub, Snap, MAS) keep today's behaviour (no
  checker; manual check says the store handles updates).

### Install kinds
`InstallKind` detected at runtime:

| Kind | Detection | Asset | Install |
|---|---|---|---|
| Windows installer (also winget) | exe dir has `uninstall.exe` + HKCU uninstall key | `rustshot-X-setup.exe` | run `setup.exe /S /RELAUNCH`, daemon exits; NSIS waits for the old process, installs, starts `rustshot.exe daemon` |
| Windows portable | otherwise on Windows | `rustshot-X-windows-x86_64.exe` | write `rustshot.exe.new` next to the exe, rename running exe → `.old`, `.new` → exe, relaunch daemon, delete `.old` on next start |
| AppImage | `$APPIMAGE` set | `*.AppImage` | download beside it, `chmod +x`, rename over, relaunch |
| Linux tarball | exe dir writable, not under `/usr` | tarball | extract binary, swap as portable |
| deb / not writable | otherwise on Linux | — | open release page (needs root) |
| macOS .app | bundle path writable | `*.zip` | unzip beside, swap bundle, relaunch — compiled, disabled until tested on a Mac (opens page) |

Exact asset names are matched against the release's asset list from the
API (`assets[].name`, `browser_download_url`), not constructed.

### Download and integrity
- Downloads only from `https://github.com/nappsllc/rustshot/releases/download/…`
  (redirects to `objects.githubusercontent.com` / `release-assets.githubusercontent.com`
  followed; nothing else). Windows: WinHTTP to a file with progress callback;
  Linux/macOS: `curl -fL --proto =https -o`.
- Progress bar in the dialog; Cancel aborts and deletes the partial file.
- Release workflow publishes `SHA256SUMS` (sha256 of every asset). The
  updater downloads it, SHA-256s the asset (hand-written, tested against
  NIST vectors) and refuses to install on mismatch or missing entry.
- Limitation, stated in docs: this protects against corrupted or substituted
  downloads but not a compromised GitHub account/release. When code signing
  lands, Windows additionally requires a valid Authenticode signature from
  the expected publisher (WinVerifyTrust) before running anything.
- Files go to a per-user temp dir (`%TEMP%\rustshot-update\`), removed after.

## 4. Widget kit and Settings window

### Windowed mode
`wind` gains `open_window(title, w, h, resizable, driver)`: a normal
decorated, DPI-aware top-level window (Win32 `WS_OVERLAPPEDWINDOW`; X11
normal window with WM_DELETE_WINDOW; AppKit NSWindow). Same `Driver` trait,
software `frame()` path. One settings/update window at a time, owned by the
daemon's UI thread.

### Controls (`src/ui/`)
Immediate-mode, drawn with `uifb` + Inter + theme tokens (dark/light follow
the OS): label, button (primary/secondary), toggle, text field (caret,
selection, clipboard paste, IME not in scope), dropdown, tab bar, scrolling
list/table, progress bar, folder picker button (native folder dialog:
IFileOpenDialog / zenity-or-kdialog / NSOpenPanel). Keyboard: Tab focus
order, Enter/Space activate, Esc closes.

### Settings window (≈ 640×520 logical px)
- **General**: theme, renderer (Windows), start at login, check for updates,
  capture hotkey (press-to-record field), quit hotkey.
- **Saving**: folder + Browse, daily subfolders + pattern, format + JPEG
  quality slider, filename editor: token buttons (Century, Day 001-366,
  Day 01-31, Full date, Hour 00-23/01-12, Minute, Month, Second, Week,
  Weekday, Year 2/4 digit, Epoch), edit field, live preview of the full
  path, Restore default / Clear. "Ask where to save" toggle.
- **Shortcuts**: table Description | Key; click a row then press a chord to
  rebind, Backspace clears, conflicts shown in red inline; Reset all.
- **Apply/OK/Cancel**. The file is rewritten from the config struct (as
  today's `to_toml`); comments are not preserved, so the first save asks
  for confirmation when the file has content rustshot would not write.
- The daemon reloads config after Apply (hotkeys re-registered).
- Entry points: tray "Settings…" (replaces "Open config file", which moves
  to a link in General), `rustshot settings`.

## Testing
- Unit: path building (subfolders, invalid chars), BMP bytes, JPEG output (SOI/EOI markers, SOF0 dimensions,
  quality changes size),
  keymap parse/resolve/conflicts, SHA-256 vectors, SHA256SUMS parsing,
  asset selection per InstallKind, skip_version logic, release-notes
  stripping.
- Widgets: preview PNGs of Settings tabs and update dialog states (dark and
  light) via the existing `render_preview_pngs` mechanism; event-driven tests
  for focus, text field editing, shortcut capture.
- e2e (Windows): Ctrl+S writes into the dated folder; portable self-update
  against a local fixture (download source injectable for tests, the URL
  guard stays in production); installer update in the Windows Sandbox
  harness from 0.1.x → a test build.

## Size budget
Exe growth target ≤ 120 KB total (jpeg-encoder ~30–50 KB, widgets and
windows ~50–70 KB).

## Out of scope
Video recording, pinning, IME in text fields, preserving config comments,
delta updates, background silent auto-install without the dialog.

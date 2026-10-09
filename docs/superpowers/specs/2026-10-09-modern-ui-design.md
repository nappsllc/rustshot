# Modern overlay UI — design

Date: 2026-10-09
Source design: Claude Design canvas "rustshot UI"
(https://claude.ai/artifact/7kjFGk5t8vBpHrtJ2xhVCH — boards 01 Overlay dark,
02 Overlay light, 03 Close-ups, 04 Wrapped & edge placement, 05 Spec sheet).

## Goal

Replace the Flameshot-era overlay chrome (flat #2D2D2D 26px buttons, purple
#740096 accent, Material PNG icons, plain black dim) with the canvas design,
identically on Windows, macOS and Linux, without growing the binary
meaningfully (target ≤ 640 KB release on Windows; today 611 KB).

## Decisions

| Topic | Decision |
|---|---|
| Accent | Periwinkle: dark `#8B93FF`, light `#5B63F5` |
| Themes | Dark + light; `theme = "auto" \| "dark" \| "light"` in config, default `auto` follows the OS |
| UI font | Embedded **Inter Medium (500) only**, subset; every "600" role in the canvas renders at 500 |
| Annotation text font | System font (any script), Inter as fallback |
| Icons | Lucide SVG path data, parsed and stroked at runtime by our rasterizer ("option A") |
| Motion | Included in this pass, all ≤ 150 ms ease-out |

## Tokens (logical px; multiply by `shot.scale` at draw time)

### Colors

| Token | Dark | Light |
|---|---|---|
| surface | `#1B1C20` @ 96% | `#FAFAFC` @ 97% |
| surface.border (1px inner) | `#FFFFFF` @ 9% | `#000000` @ 8% |
| separator | `#FFFFFF` @ 10% | `#000000` @ 10% |
| icon | `#A3A7B3` | `#5F6473` |
| icon.hover | `#ECEEF3` | `#15171E` |
| bg.hover | `#FFFFFF` @ 6% | `#000000` @ 6% |
| bg.pressed | `#FFFFFF` @ 11% | `#000000` @ 10% |
| text | `#ECEEF3` | `#15171E` |
| text.muted | `#9296A3` | `#737889` |
| accent | `#8B93FF` | `#5B63F5` |
| accent.bg (active) | accent @ 18% | accent @ 12% |
| accent.bg.hover | accent @ 24% | accent @ 18% |
| accent.ring | accent @ 30% | accent @ 32% |
| accent.fg (active icon) | `#B7BCFF` | `#4047D6` |
| tooltip.bg (both stay dark) | `#0E0F12` @ 97% | `#16171C` @ 97% |
| tooltip.text | `#F2F3F6` | `#F2F3F6` |
| tooltip.key bg / text | `#FFFFFF` @ 14% / `#C4C7D0` | same |
| danger.bg / fg | `#FF6B6B` @ 16% / `#FF8F8F` | `#DC2626` @ 10% / `#C62828` |
| success / error | `#4ADE9A` / `#FF7A7A` | `#1E9E63` / `#D64545` |
| dim | `#07080B` @ 58% | `#07080B` @ 46% |
| swatch rim | `#FFFFFF` @ 22% | `#000000` @ 18% |
| selection outer / inner line | `#000` @ 40% / `#000` @ 28% | `#FFF` @ 55% / `#FFF` @ 35% |

### Shadow (stacked rounded rects, back to front; radius = surface radius + spread)

| Layer | Offset y / spread | Dark | Light |
|---|---|---|---|
| 1 contact | +1 / 0 | `#000` @ 26% | `#000` @ 14% |
| 2 | +3 / +2 | `#000` @ 15% | `#000` @ 9% |
| 3 | +7 / +5 | `#000` @ 8% | `#000` @ 5% |
| 4 ambient | +12 / +9 | `#000` @ 4% | `#000` @ 2.5% |

Toolbar, palette, toast: all four layers. Size label and tooltip: layers 1 + 3.

### Sizes

| Token | Value |
|---|---|
| spacing | 2 in-group · 6 between groups · 8 to selection · 12 screen margin |
| button | 32×32, radius 8, icon 20 centred |
| toolbar | padding 6, radius 12, row height 44; single row ≈ 646 wide |
| separator | 1×16 vertical; wrapped variant 1px horizontal rule inset 4 |
| size value | fixed 56×32 |
| swatch | 18 ⌀ + 1px rim; palette targets 28×28 radius 7, gap 4 |
| tooltip | h 26, padding 9 / 6, radius 7, key cap h 18 r 4 min-w 18 pad 5; 8 above target |
| size label | h 22, padding 8, radius 6, gap 8 |
| toast | h 36, padding 10 / 14, radius 12, icon 16, 28 above screen bottom |
| text box | 1px dashed accent, padding 4, radius 4; caret 2 × (font × 1.15) |
| selection | 1px accent + 1px outer line + 1px inner line |
| handle | 6 ⌀ white + 2px accent ring + 1px `#000`@45% halo (12 visual, 20 hit); hover ring 3 |
| icon stroke | 1.8 at 24-unit grid, round caps/joins, scaled to 20 × scale px |

### Typography (Inter Medium only)

| Role | Size | Where |
|---|---|---|
| body | 12 | toast, tooltip label |
| value | 12, tabular | "1280 × 720", stroke size, OK |
| caption | 11 | unit ("line"), coordinates |
| key cap | 11 (10.5 rendered at ≥1.5×) | tooltip shortcuts |
| hint | 13 | empty-state hint (text @ 70%) |

Use `×` (U+00D7). Subset: U+0020–007E plus `× · … — ⇧ ⌘ ⌥ ⏎ ←→↑↓`; hinting
and unused OpenType tables stripped (target ≤ 30 KB). Script
`tools/subset-font.sh` (pyftsubset) regenerates it; not needed at build time.

### Annotation palette (default `user_colors`)

`#F04438 #FF8A1F #FFC532 #2DC06F #19B5D6 #3B82F6 #8B5CF6 #EC4899 #FFFFFF #111318`,
default red. Existing configs with `user_colors` keep theirs.

Stroke size range 1–24, default 3, remembered per group: line (pencil, line,
arrow), shape (rect, ellipse), mark, text, block (pixelate, invert). Marker:
chosen color @ 35%, square cap, 3× size. Arrow head: length max(10, 4 × size),
28° half-angle, filled.

### Icons (Lucide names)

pencil · slash (line) · arrow-up-right · rectangle-horizontal · circle ·
highlighter · type · grid-3x3 · contrast · undo-2 · redo-2 · minus · plus ·
copy · download (save) · cloud-upload · x · check · circle-check · info ·
circle-alert · loader-circle. Path data is copied verbatim from the canvas'
SVG symbols (Lucide, ISC license — add notice to packaging).

## Architecture

### `src/theme.rs` (new)
`struct Theme { …tokens… }`, `const DARK`, `const LIGHT`, and
`Theme::resolve(cfg) -> &'static Theme`. `auto` calls
`platform::prefers_dark() -> Option<bool>`:
- Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`
  (adds `Win32_System_Registry` feature).
- macOS: `NSUserDefaults` `AppleInterfaceStyle == "Dark"`.
- Linux: `gsettings get org.gnome.desktop.interface color-scheme`
  (`prefer-dark`); command missing/failing → `None`.
`None` → dark. Resolved once per capture.

### `src/icon_path.rs` (new)
Parser for SVG path `d` (M m L l H h V v C c S s Q q A a Z z) plus `rect`,
`circle` element shorthands, producing polylines in the 24-unit grid.
Curves flattened by adaptive subdivision (tolerance 0.1 device px); arcs via
endpoint→centre conversion. `draw_icon(fb, name, x, y, size, color)` scales
and strokes each polyline with `raster::stroke_polyline` (round caps/joins),
width `1.8 × size / 24`. Polylines cached per (name, scale).
`src/icons.rs` and `assets/icons/*` are removed; with them the only
`png::Decoder` user goes away (`PixBuf::from_png` deleted).

### `src/uifb.rs` / `src/raster.rs` additions
- `surface(rect, radius, layers, theme)`: shadow layers → fill → 1px inner border.
- `stroke_dashed_round_rect` for the text box.
- `handle(center, ring_w, theme)`.
- Alpha multiplier on every draw (for fades): a `Fb::opacity` scope.

### Fonts
`assets/fonts/Inter-Medium.subset.ttf` via `include_bytes!` → `ui_font()`.
`load_system_font()` replaces the Windows-only `editor::load_font`, using the
cross-platform candidate list already in `uifb` tests (Segoe UI, SF/Helvetica,
DejaVu/Liberation/Noto), falling back to Inter.

### Editor split (`src/editor.rs` → `src/editor/`)
- `mod.rs` — state machine, event handling (existing logic, moved).
- `toolbar.rs` — tile model, layout, wrapping, placement, hit-testing.
- `chrome.rs` — drawing: dim, selection, handles, size label, toolbar,
  palette, tooltip, toast, hint, text box.
- `anim.rs` — tweens.

### Toolbar layout & placement (`toolbar.rs`)
Groups: tools (9) | undo, redo | minus, value, plus, swatch | copy, save,
upload, close. While tasks are running: copy/save/upload → one OK button,
tools disabled, bar width preserved. Redo disabled when nothing to redo;
undo likewise.

- Single row if width ≤ selection width + 48; else two rows split after
  history; row 2 right-aligned; horizontal rule between rows.
- Wrap decision uses max(selection width, 260) so tiny selections keep a
  single row when the screen allows.
- Vertical placement: below (gap 8) → above → inside along the bottom edge
  (only if selection height > 120) → clamped.
- Horizontal: right-aligned to the selection, clamped to a 12px margin.
- Tiny selection (< 260 wide): bar centred on the selection centre (not on
  the screen, as one canvas note suggests), clamped, must not overlap the
  selection.
- Size label: 8 above the selection top-left; if < 30 px above, 8 inset
  inside. Shows "W × H" while dragging, adds "x, y" at rest.
- Palette popover: 6 above the swatch button, right-aligned to it; 10 dots;
  selected dot gets accent ring.

### Tooltips, toast, hint
- Tooltip after 400 ms hover, centred 8 above the button (below if no room):
  label + key caps. macOS shows `⌘`/`⇧` instead of `Ctrl`/`Shift`.
- Toast bottom centre; kinds Info (accent icon), Success (green), Progress
  (spinner, accent), Error (red). Dismiss after 1.6 s, errors 4 s.
- Empty-state hint centred over the dim before any selection:
  "Drag to select · Enter to copy · Esc to cancel" with key caps; hides when a
  drag starts.

### Motion (`anim.rs`)
`Tween { from, to, start: Instant, dur }`, ease-out cubic. Animated values:

| Element | In | Out |
|---|---|---|
| dim | 0 → 1, 120 ms | 100 ms |
| toolbar | fade + scale 0.96 → 1 from anchor edge, 120 ms | fade 80 ms |
| toolbar move | snap | — |
| button hover bg | 60 ms | 90 ms |
| tooltip | fade 80 ms | instant |
| palette | fade + scale 0.96 → 1, 100 ms | 80 ms |
| toast | fade + 4 px rise, 120 ms | fade 100 ms |
| hint | fade 120 ms | fade 100 ms |
| caret | blink 530 ms (no tween) | |

Window backends get `set_fast_timer(hwnd, bool)`: 16 ms tick while any tween
is active, back to the existing 150 ms otherwise (Win `SetTimer`, X11 poll
timeout, macOS run-loop timer). `Ev::Timer` triggers a redraw only when
something animates or the caret phase changes.

## Error handling
- Font subset fails to parse → fall back to system font (never panic).
- Unknown icon name or bad path data → draw nothing, `debug_assert!` in tests.
- OS theme probe fails → dark.
- No system font for annotations → Inter (covers Latin only; documented).

## Testing
- `icon_path`: each command, relative forms, arc conversion, flattening
  tolerance; every bundled icon parses to ≥ 1 polyline within 0..24.
- `toolbar`: single-row width ≈ 646; wrap threshold; each placement fallback;
  clamping at all four edges; tiny selection never overlapped; label flip.
- `theme`: config override beats OS; `None` → dark.
- `anim`: easing endpoints, completion, `any_active`.
- Golden-pixel tests on small `Fb`s: surface border/shadow alpha, active
  button tint, disabled opacity.
- Existing e2e (`cargo test -- --ignored`) updated for new hit targets and
  passing on Windows.
- Size check: `cargo bloat --release --crates` before/after; target
  ≤ 640 KB release on Windows.

## Out of scope
Wayland capture/hotkeys, settings UI, custom color picker/eyedropper (palette
slot reserved but not implemented), keyboard focus ring navigation, `std`
size trimming.

## Measured
Windows, `cargo build --release` (default profile):
- `target/release/rustshot.exe`: 567,296 bytes (554 KiB), budget 655,360 bytes
  (640 KiB). Within budget.
- `fdeflate` is still linked at 530 B of `.text` (expected to disappear;
  it is a small residual). `png` is 4.9 KiB of `.text`.

`cargo bloat --release --crates -n 12` (debug symbols kept for the analysis
build only; the table is an estimate):

| File .text | Size | Crate |
|-----------:|-----:|-------|
| 37.0% | 150.6 KiB | std |
| 25.9% | 105.7 KiB | rustshot |
| 17.1% | 69.5 KiB | ttf_parser |
| 1.9% | 7.8 KiB | anyhow |
| 1.8% | 7.3 KiB | miniz_oxide |
| 1.8% | 7.2 KiB | flate2 |
| 1.3% | 5.4 KiB | ab_glyph_rasterizer |
| 1.2% | 4.9 KiB | crc32fast |
| 1.2% | 4.9 KiB | png |
| 1.1% | 4.6 KiB | enum2$<rustshot |
| 1.1% | 4.4 KiB | ab_glyph |
| 0.4% | 2.2 KiB | simd_adler32 |

.text section: 407.5 KiB of a 553.5 KiB file.

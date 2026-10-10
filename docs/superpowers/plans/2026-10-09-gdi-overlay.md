# GDI-Composited Overlay (Windows) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** On Windows, cut rustshot's private memory during a capture from ~60–90 MB (5120×1440) to roughly 5–15 MB plus an annotation layer sized to what was drawn, by keeping the screenshot in GDI bitmaps and painting only what changed. Visual output and behaviour stay identical.

**Architecture:** The screenshot lives in two GDI bitmaps (DDBs): `plain` (the capture) and `dimmed` (the capture with the theme dim applied), both built from the screen in horizontal bands so rustshot never holds a full-size pixel buffer. Painting a dirty rect = `BitBlt` from `dimmed` outside the selection and from `plain` (or the annotation layer) inside it, then the chrome elements intersecting the rect are rendered by the existing software code into small per-element buffers (background read back from the bitmaps) and written with `SetDIBitsToDevice`. DWM composites the window, so no back buffer is needed. The editor tracks dirty rects (old ∪ new bounds of every element that changed) and invalidates only those. Annotations are rendered into a layer covering the union of their bounding boxes; export reads only the selection from `plain` and bakes the objects into a selection-sized buffer. Linux/macOS keep the full-frame path, but through the same rect-composition code so both paths are tested together.

**Tech Stack:** Rust 2024, `windows` 0.62 GDI (CreateCompatibleBitmap, BitBlt, GetDIBits/SetDIBits, SetDIBitsToDevice, AlphaBlend for the dim fade, InvalidateRect); no new crates.

## Global Constraints
- Visual output identical to today's (preview PNGs byte-identical where the preview path is used; a new GDI-path test compares the composed window region against the software path within ±1 per channel).
- No full-size buffer in rustshot's own memory on Windows during a capture except, unavoidably, an annotation layer when objects cover a large area, and the export crop (selection-sized, transient).
- Frame time must not regress: full repaint ≤ 16 ms at 5120×1440; typical interactions (drag, hover) repaint only small rects.
- Dim fade-in: keep it (AlphaBlend a 1×1 black source stretched over the four outside rects with `SourceConstantAlpha`, over `plain`, only during the 120 ms fade; afterwards blit `dimmed`).
- Measurement: peak private bytes of a one-shot `rustshot gui --region …` run, captured by a test harness (`GetProcessMemoryInfo` PeakPagefileUsage/PrivateUsage logged on exit behind `RUSTSHOT_MEMLOG=<path>`), before vs after.
- Every task: `cargo test`, clippy `-D warnings` on Windows, `--target x86_64-unknown-linux-gnu`, `--target x86_64-apple-darwin`. Don't kill processes by name; don't disturb the user's running rustshot (use a separate CARGO_TARGET_DIR). Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 1: Offset + channel-order drawing surfaces
**Files:** `src/uifb.rs`, `src/raster.rs`, `src/objects.rs`.
- `Fb` and `Surf` gain an origin offset `(ox, oy)` (draw calls keep using image coordinates; pixels outside the buffer are clipped) and a channel order (`Rgba` | `Bgra`) applied in their single blend paths and fast fills.
- `Obj::render` gains a variant that renders into a `Surf` with an offset (pixelate/invert read the buffer's own pixels, which the caller pre-fills with the background).
- Tests: drawing every primitive into an offset BGRA buffer equals drawing into a full RGBA buffer, cropped and channel-swapped (byte-exact); objects likewise.

### Task 2: Rect-based composition in the editor
**Files:** `src/editor/mod.rs`, `src/editor/chrome.rs` (no visual changes).
- `trait Backdrop { fn fill(&self, r: IRect, src: Source, out: &mut [u8], order: Order); }` with `Source::{Plain, Dimmed(alpha)}`; a `PixBufBackdrop` implementation (today's in-memory base/composed) used by Linux/macOS and tests.
- `Edit::chrome_rects(&self) -> Vec<IRect>`: bounds (with shadow/AA margins) of selection border strips, handles, size label, toolbar(+popover), tooltip, toast, hint, text box, draft object.
- `Edit::compose_rect(&self, r, backdrop, out, order)`: background from backdrop (plain inside selection, dimmed outside, annotation layer where present), then all chrome drawn through offset `Fb`/`Surf` clipped to `r`.
- `Edit::dirty_rects(prev_state) -> Vec<IRect>`: union of previous and current chrome rects that changed, plus selection-edge bands when the selection moved/resized; whole window on capture start and theme/dim-alpha changes.
- The existing full-frame path becomes "compose_rect over the whole image" (one call) so Linux/macOS behaviour is unchanged.
- Tests: composing the whole image via many small rects (random tiling) equals one full-frame compose (byte-exact) for preview states A–D; `dirty_rects` covers every changed pixel between two consecutive states (render both, diff, assert every differing pixel is inside a dirty rect) for drag, hover, palette toggle, toast set/clear, caret blink, tooltip appear.

### Task 3: Windows GDI backend
**Files:** `src/capture_win.rs`, `src/capture.rs` (Shot holds either pixels or a platform backdrop), `src/wind_win.rs`, `src/wind.rs` (Driver: paint rects), `src/editor/mod.rs` (wiring).
- Capture: `BitBlt` screen → `plain` DDB (no `GetDIBits` of the whole screen). Build `dimmed` DDB bandwise (64 rows: `GetDIBits` band from `plain`, dim LUT/SWAR, `SetDIBits` into `dimmed`).
- `GdiBackdrop` implements `Backdrop::fill` via `GetDIBits` of the rect from `plain`/`dimmed` into the small buffer (BGRA, top-down).
- WM_PAINT: for `ps.rcPaint`: BitBlt `dimmed` for the parts outside the selection and `plain` for the parts inside (or SetDIBitsToDevice the annotation layer crop); during the dim fade use AlphaBlend instead of `dimmed`; then for each chrome rect ∩ rcPaint: allocate/reuse a small BGRA buffer, `compose_rect`, `SetDIBitsToDevice`. Editor invalidates via `InvalidateRect(hwnd, &rect)` per dirty rect instead of whole-window.
- Driver trait: add `fn paint_rects(&mut self, clip: IRect, sink: &mut dyn PaintSink)` (Windows) while `frame()` stays for Linux/macOS.
- Release DDBs/DCs on hide; no GDI handle leaks (test: GDI object count before/after 20 captures via `GetGuiResources`).

### Task 4: Annotation layer + export from GDI
**Files:** `src/editor/mod.rs`, `src/objects.rs`.
- `AnnotLayer { rect: IRect, px: Vec<u8> (BGRA) }` = union bbox of committed objects (+AA margin); rebuilt on commit/undo/redo from `plain` (Backdrop::fill) + objects rendered with offset; dropped when no objects remain. compose_rect uses it as the background inside its rect (dimmed via LUT outside the selection).
- Export: selection-sized buffer from `plain` + objects rendered with offset → existing export (PNG/clipboard/upload) in RGBA.
- Tests: export output equals today's crop of `composed` (byte-exact) for captures with and without objects, objects partly outside the selection, pixelate/invert objects.

### Task 5: Measure + verify
- Add `RUSTSHOT_MEMLOG` (Windows: write PrivateUsage/PeakPagefileUsage on exit). Run one-shot captures (e2e harness style: launch `gui --region 5120x1440+0+0`, draw a rect, Enter) before (main) and after; report peak private bytes.
- Bench full repaint and typical-interaction repaint (dirty rect area and time).
- Re-run the e2e tests and the Windows Sandbox clean-system test.

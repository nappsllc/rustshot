# Multi-monitor Placement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** When the overlay spans several monitors, keep the toolbar, size label, palette, tooltip, toast and hint on a real monitor (the one holding the selection, or the pointer), and give Linux real per-monitor geometry via XRandR 1.5.

**Architecture:** `Shot` carries its monitors' rectangles in image coordinates. The editor picks a work area per frame (`pick_area`) and passes it to `toolbar::layout` / `chrome::*` instead of the full image size; the existing clamp logic becomes area-relative. On Linux, `monitors()` asks XRandR (`XRRGetMonitors`) and falls back to the single root-window monitor.

**Tech Stack:** Rust 2024; X11 + libXrandr FFI (link `Xrandr`); no new crates.

## Global Constraints
- Approved design (user, 2026-10-09): per-monitor placement; Linux links libXrandr directly; mixed-DPI spanning out of scope (unchanged: single monitor under cursor).
- Area choice: selection present → the monitor with the largest overlap with the selection; otherwise the monitor containing the pointer; otherwise the whole image.
- The dim still covers the whole image; only chrome placement changes.
- Linux: if the RandR extension is missing, `XRRGetMonitors` returns 0 monitors, or the call fails → today's single root-window monitor. Scale stays 1.0.
- Every task: `cargo test`, `cargo clippy --all-targets -- -D warnings` on Windows **and** `--target x86_64-unknown-linux-gnu` / `--target x86_64-apple-darwin` (check-only) clean.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 1: Monitor rectangles on `Shot`

**Files:** Modify `src/capture.rs`, `src/editor/mod.rs` (test helper `synthetic_shot`).

**Produces:** `pub type IRect = (i32, i32, u32, u32);` and `Shot.monitors: Vec<IRect>` — each monitor intersected with the shot, in image coordinates (origin = shot top-left). Never empty: at least `(0, 0, size.0, size.1)`.

- [ ] Step 1: failing tests in `capture.rs` tests:

```rust
    #[test]
    fn monitors_in_shot_are_image_relative_and_clipped() {
        let mons = vec![
            MonInfo { x: -1920, y: 200, w: 1920, h: 1080, scale: 1.0, primary: false },
            MonInfo { x: 0, y: 0, w: 2560, h: 1440, scale: 1.0, primary: true },
        ];
        let (ox, oy, w, h) = union_rect(&mons);
        assert_eq!((ox, oy, w, h), (-1920, 0, 4480, 1440));
        let r = monitors_in_shot(&mons, (ox, oy), (w, h));
        assert_eq!(r, vec![(0, 200, 1920, 1080), (1920, 0, 2560, 1440)]);
    }

    #[test]
    fn monitors_in_shot_never_empty() {
        assert_eq!(monitors_in_shot(&[], (0, 0), (800, 600)), vec![(0, 0, 800, 600)]);
    }
```

- [ ] Step 2: implement in `capture.rs`:

```rust
/// A rectangle in image pixels: x, y, w, h.
pub type IRect = (i32, i32, u32, u32);

/// Monitors intersected with a shot at `origin`/`size`, in image coordinates.
/// Never empty: falls back to the whole shot.
pub fn monitors_in_shot(mons: &[MonInfo], origin: (i32, i32), size: (u32, u32)) -> Vec<IRect> {
    let (sw, sh) = (size.0 as i32, size.1 as i32);
    let mut out: Vec<IRect> = mons
        .iter()
        .filter_map(|m| {
            let x0 = (m.x - origin.0).max(0);
            let y0 = (m.y - origin.1).max(0);
            let x1 = (m.x + m.w as i32 - origin.0).min(sw);
            let y1 = (m.y + m.h as i32 - origin.1).min(sh);
            (x1 > x0 && y1 > y0).then(|| (x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
        })
        .collect();
    if out.is_empty() {
        out.push((0, 0, size.0, size.1));
    }
    out
}
```

Add `pub monitors: Vec<IRect>` to `Shot` (doc: "Monitors covered by this shot, image coordinates; never empty."). `grab_monitor`: `monitors: vec![(0, 0, m.w, m.h)]`. `grab_span`: `monitors: monitors_in_shot(mons, (ox, oy), (w, h))`. Editor test helper `synthetic_shot`: `monitors: vec![(0, 0, w, h)]`.

- [ ] Step 3: verify (all clippy targets), commit `capture: record monitor rectangles on Shot (image coordinates)`.

---

### Task 2: Per-monitor work area for chrome

**Files:** Modify `src/editor/toolbar.rs`, `src/editor/chrome.rs`, `src/editor/mod.rs`.

**Interfaces:**
- `toolbar::Input.screen: (f32, f32)` → `area: FRect` (work area in image px). `layout` clamps x to `[area.x + m, area.x1() - m - bw]`, y to `[area.y + m, area.y1() - m - bh]`; the "room" for wrapping uses `area.w - 2m`; above/below/inside tests use `area.y`/`area.y1()` instead of `0`/`sh`. Palette popover clamps to the area the same way. `label_rect(sel, w, s, area: FRect)` clamps x to the area and uses `area.y` for the "room above" test.
- `chrome::size_label(.., area: FRect, k)`, `chrome::toast(.., area: FRect, k)` (centred in the area, 28 px above `area.y1()`), `chrome::tooltip(.., area: FRect, k)` (clamped to the area; below the anchor if no room above `area.y`), `chrome::hint(.., area: FRect, k)` (centred in the area).
- `mod.rs`: `fn pick_area(monitors: &[IRect], sel: Option<FRect>, pointer: Pt) -> FRect` — largest-overlap monitor with `sel` (ties → first), else the monitor containing `pointer`, else the first monitor. `frame()` computes `let area = pick_area(&edit.shot.monitors, edit.sel, Pt::new(self.mouse.0 as f32, self.mouse.1 as f32));` once and passes it to `toolbar::layout` and every chrome call above.

- [ ] Step 1: failing tests.
  - `mod.rs` tests: `pick_area` — selection mostly on monitor 2 → monitor 2; selection exactly split → first; no selection, pointer on monitor 2 → monitor 2; pointer in a gap → first monitor.
  - `toolbar.rs` tests: convert the existing tests' `screen: SCREEN` to `area: FRect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 }` (no expected values change), and add `area_offset_clamps_inside_monitor`: area `(1920, 0, 1280, 720)`, selection `(1920, 600, 1280, 120)` at the bottom of that monitor → bar placed above (`tb.above`), `tb.bar.x >= 1932` and `tb.bar.x1() <= 3188`; and a label test with area `y = 200` and selection `y = 210` → label inside the selection.
- [ ] Step 2: implement as specified; keep behaviour identical when the area is the full image (all existing tests unchanged in values).
- [ ] Step 3: extend `render_preview_pngs` with `dark-two-monitors.png`: a 2400×900 synthetic shot with `monitors = [(0, 200, 1200, 700), (1200, 0, 1200, 900)]` (left monitor shorter and offset), selection `(100, 700, 800, 180)` on the left monitor → toolbar must sit on the left monitor (above the selection), toast at the bottom of the left monitor. Run it (`RUSTSHOT_PREVIEW_DIR=…/scratchpad/preview`) and report the file path.
- [ ] Step 4: verify (all targets), commit `ui: place toolbar, label, tooltip, toast and hint on the active monitor`.

---

### Task 3: Linux monitors via XRandR 1.5

**Files:** Modify `src/capture_linux.rs`, `.github/workflows/ci.yml`, `packaging/linux/deb.sh`, `snap/snapcraft.yaml`, `packaging/aur/PKGBUILD.in`.

- [ ] Step 1: in `capture_linux.rs` add the binding and use it in `monitors()`:

```rust
#[repr(C)]
struct XRRMonitorInfo {
    name: c_ulong,
    primary: c_int,
    automatic: c_int,
    noutput: c_int,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    mwidth: c_int,
    mheight: c_int,
    outputs: *mut c_ulong,
}

#[link(name = "Xrandr")]
unsafe extern "C" {
    fn XRRQueryExtension(dpy: *mut c_void, event_base: *mut c_int, error_base: *mut c_int) -> c_int;
    fn XRRGetMonitors(dpy: *mut c_void, window: c_ulong, get_active: c_int, n: *mut c_int) -> *mut XRRMonitorInfo;
    fn XRRFreeMonitors(monitors: *mut XRRMonitorInfo);
}
```

`monitors()`: open display, root = `XRootWindow(dpy, XDefaultScreen(dpy))`; if `XRRQueryExtension` succeeds, call `XRRGetMonitors(dpy, root, 1, &mut n)`; map each entry with positive width/height to `MonInfo { x, y, w, h, scale: 1.0, primary: primary != 0 }`; free with `XRRFreeMonitors`. If that yields nothing, return today's single root-window `MonInfo`. Close the display on every path. Factor the pure mapping into `fn monitors_from_xrr(entries: &[(i32, i32, i32, i32, bool)]) -> Vec<MonInfo>` with a test (skips zero-size entries; marks primary; empty input → empty).
- [ ] Step 2: packaging deps — CI linux job apt line adds `libxrandr-dev`; `deb.sh` control `Depends: libx11-6, libxrandr2`; `snap/snapcraft.yaml` `build-packages: [libx11-dev, libxrandr-dev]`, `stage-packages: [libx11-6, libxrandr2]`; `PKGBUILD.in` `depends=('libx11' 'libxrandr')`. (Flatpak's freedesktop runtime already ships libXrandr.)
- [ ] Step 3: verify — Windows tests/clippy, linux-gnu clippy (check only; links aren't resolved), YAML parse of snapcraft/ci, `bash -n deb.sh`. Commit `linux: per-monitor geometry via XRandR 1.5 (falls back to the root window)`. Push and confirm the CI `linux`, `flatpak` and `snap` jobs are green (they link it).

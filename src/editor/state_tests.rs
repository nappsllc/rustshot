//! The editor state machine driven the way the window drives it: synthetic
//! `wind::Ev` input through `Driver::on_event` (selection, every tool,
//! text editing, history, palette and sizes, toasts, accept/cancel, the
//! upload result and `begin_capture` with a queued test shot). Nothing
//! touches the screen, the clipboard or the network: `grab` returns the
//! queued shot, exports go to `Task::Geometry` or a temp folder.

use super::tests::{edit_of, preview_app, preview_app_with, synthetic_shot, two_monitor_shot, upload_in_flight};
use super::*;
use crate::wind::key;

fn r(x: f32, y: f32, w: f32, h: f32) -> FRect {
    FRect { x, y, w, h }
}

const NONE: Mods = Mods { shift: false, ctrl: false, alt: false };
const SHIFT: Mods = Mods { shift: true, ctrl: false, alt: false };
const CTRL: Mods = Mods { shift: false, ctrl: true, alt: false };
const CTRL_SHIFT: Mods = Mods { shift: true, ctrl: true, alt: false };

/// The preview editor (1440x900 shot, one monitor), never stealing focus
/// and never copying an uploaded URL to the clipboard.
fn app(sel: Option<FRect>) -> App {
    quiet(preview_app(theme::DARK, sel))
}

fn quiet(mut a: App) -> App {
    a.focus_tries = u8::MAX;
    a.cfg.copy_url_after_upload = false;
    a
}

/// One second before now, for wheel-throttle stamps (no underflow panic
/// on a clock that started less than a second ago).
fn a_second_ago() -> Instant {
    let now = Instant::now();
    now.checked_sub(Duration::from_secs(1)).unwrap_or(now)
}

fn ed(a: &mut App) -> &mut Edit {
    edit_of(a)
}

fn down(a: &mut App, x: i32, y: i32) {
    a.on_event(Ev::Down { x, y });
}
fn mv(a: &mut App, x: i32, y: i32) {
    a.on_event(Ev::Move { x, y });
}
fn up(a: &mut App, x: i32, y: i32) {
    a.on_event(Ev::Up { x, y });
}
/// Press, move halfway, move to `to`, release.
fn drag(a: &mut App, from: (i32, i32), to: (i32, i32)) {
    down(a, from.0, from.1);
    mv(a, (from.0 + to.0) / 2, (from.1 + to.1) / 2);
    mv(a, to.0, to.1);
    up(a, to.0, to.1);
}
fn click(a: &mut App, x: i32, y: i32) {
    down(a, x, y);
    up(a, x, y);
}
fn key_m(a: &mut App, vk: u32, mods: Mods) {
    a.on_event(Ev::Key { vk, up: false, repeat: false, mods });
}
fn key(a: &mut App, vk: u32) {
    key_m(a, vk, NONE);
}
fn repeat(a: &mut App, vk: u32) {
    a.on_event(Ev::Key { vk, up: false, repeat: true, mods: NONE });
}
fn type_str(a: &mut App, s: &str) {
    for u in s.encode_utf16() {
        a.on_event(Ev::Char(u));
    }
}

/// Drags see `m` held until the guard drops.
struct HeldMods;
fn hold(m: Mods) -> HeldMods {
    FAKE_MODS.with(|c| c.set(Some(m)));
    HeldMods
}
impl Drop for HeldMods {
    fn drop(&mut self) {
        FAKE_MODS.with(|c| c.set(Some(Mods::default())));
    }
}

fn hidden(a: &App) -> bool {
    matches!(a.st, State::Hidden)
}

fn code(a: &App) -> i32 {
    a.exit_code.load(Ordering::SeqCst)
}

/// A fresh temp folder for exports (removed on drop).
struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("rustshot-state-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- selection --------------------------------------------------------------

#[test]
fn drag_creates_a_selection_and_a_click_selects_everything() {
    let mut a = app(None);
    drag(&mut a, (100, 100), (300, 250));
    assert_eq!(ed(&mut a).sel, Some(r(100.0, 100.0, 200.0, 150.0)));
    assert!(matches!(ed(&mut a).interact, Interact::None));

    // Dragging up-left from the anchor normalises the rect.
    let mut a = app(None);
    drag(&mut a, (300, 250), (100, 100));
    assert_eq!(ed(&mut a).sel, Some(r(100.0, 100.0, 200.0, 150.0)));

    // A click (or a move under CLICK_PX) selects the whole shot.
    for wiggle in [0, 1] {
        let mut a = app(None);
        down(&mut a, 500, 500);
        mv(&mut a, 500 + wiggle, 500);
        up(&mut a, 500 + wiggle, 500);
        assert_eq!(ed(&mut a).sel, Some(r(0.0, 0.0, 1440.0, 900.0)), "wiggle {wiggle}");
    }
}

#[test]
fn shift_drag_makes_a_square_selection() {
    let mut a = app(None);
    let _m = hold(SHIFT);
    drag(&mut a, (100, 100), (300, 160));
    assert_eq!(ed(&mut a).sel, Some(r(100.0, 100.0, 200.0, 200.0)));
}

#[test]
fn new_selection_is_clamped_to_the_shot() {
    let mut a = app(None);
    drag(&mut a, (1000, 600), (5000, 4000));
    let s = ed(&mut a).sel.unwrap();
    assert!(s.x >= 0.0 && s.y >= 0.0 && s.x1() <= 1440.0 && s.y1() <= 900.0, "{s:?}");
    let mut a = app(None);
    drag(&mut a, (100, 100), (-300, -300));
    assert_eq!(ed(&mut a).sel, Some(r(0.0, 0.0, 100.0, 100.0)), "stops at the edge, never past the anchor");
    let mut a = app(None);
    drag(&mut a, (1000, 600), (5000, 700));
    assert_eq!(ed(&mut a).sel, Some(r(1000.0, 600.0, 440.0, 100.0)));
}

#[test]
fn shift_drag_past_the_shot_keeps_the_anchor_and_stays_square_inside() {
    // (anchor, pointer far past the shot, expected square). 1440x900 shot.
    for (from, to, want) in [
        ((1000, 600), (5000, 4000), r(1000.0, 600.0, 300.0, 300.0)), // bottom-right corner
        ((100, 200), (-500, -300), r(0.0, 100.0, 100.0, 100.0)),      // top-left corner
        ((1300, 100), (3000, -50), r(1300.0, 0.0, 100.0, 100.0)),     // top-right corner
        ((200, 800), (-400, 1200), r(100.0, 800.0, 100.0, 100.0)),    // bottom-left corner
        ((1000, 600), (5000, 700), r(1000.0, 600.0, 300.0, 300.0)),   // right edge
        ((400, 500), (300, -2000), r(0.0, 100.0, 400.0, 400.0)),       // top edge
        ((400, 300), (-900, 350), r(0.0, 300.0, 400.0, 400.0)),       // left edge
        ((700, 400), (750, 3000), r(700.0, 400.0, 500.0, 500.0)),     // bottom edge
    ] {
        let mut a = app(None);
        let _m = hold(SHIFT);
        drag(&mut a, from, to);
        let s = ed(&mut a).sel.unwrap();
        assert_eq!(s, want, "{from:?} -> {to:?}");
        assert_eq!(s.w, s.h, "square {from:?} -> {to:?}");
        assert!(s.x >= 0.0 && s.y >= 0.0 && s.x1() <= 1440.0 && s.y1() <= 900.0, "inside {s:?}");
        let corners = [(s.x, s.y), (s.x1(), s.y), (s.x, s.y1()), (s.x1(), s.y1())];
        assert!(corners.contains(&(from.0 as f32, from.1 as f32)), "anchor fixed {from:?} {s:?}");
    }
}

#[test]
fn dragging_inside_moves_the_selection_and_clamps_it() {
    let mut a = app(Some(r(100.0, 100.0, 200.0, 100.0)));
    drag(&mut a, (150, 150), (250, 200));
    assert_eq!(ed(&mut a).sel, Some(r(200.0, 150.0, 200.0, 100.0)));
    drag(&mut a, (250, 200), (-1000, -1000));
    assert_eq!(ed(&mut a).sel, Some(r(0.0, 0.0, 200.0, 100.0)), "clamped top-left");
    drag(&mut a, (100, 50), (5000, 5000));
    assert_eq!(ed(&mut a).sel, Some(r(1240.0, 800.0, 200.0, 100.0)), "clamped bottom-right");
}

#[test]
fn every_handle_resizes_its_edges() {
    let sel = r(400.0, 300.0, 200.0, 100.0);
    // (handle point, drag target, expected rect): each pulled 20 px outward.
    let cases = [
        ("NW", (400, 300), (380, 280), r(380.0, 280.0, 220.0, 120.0)),
        ("N", (500, 300), (500, 280), r(400.0, 280.0, 200.0, 120.0)),
        ("NE", (600, 300), (620, 280), r(400.0, 280.0, 220.0, 120.0)),
        ("E", (600, 350), (620, 350), r(400.0, 300.0, 220.0, 100.0)),
        ("SE", (600, 400), (620, 420), r(400.0, 300.0, 220.0, 120.0)),
        ("S", (500, 400), (500, 420), r(400.0, 300.0, 200.0, 120.0)),
        ("SW", (400, 400), (380, 420), r(380.0, 300.0, 220.0, 120.0)),
        ("W", (400, 350), (380, 350), r(380.0, 300.0, 220.0, 100.0)),
    ];
    for (name, at, to, want) in cases {
        let mut a = app(Some(sel));
        down(&mut a, at.0, at.1);
        assert!(matches!(ed(&mut a).interact, Interact::Resize { .. }), "{name}: grabbed");
        mv(&mut a, to.0, to.1);
        up(&mut a, to.0, to.1);
        assert_eq!(ed(&mut a).sel, Some(want), "{name}");
    }
}

#[test]
fn resize_modifiers_flip_and_clamp() {
    let sel = r(400.0, 300.0, 200.0, 100.0);
    // Shift: symmetric about the centre.
    let mut a = app(Some(sel));
    {
        let _m = hold(SHIFT);
        drag(&mut a, (600, 400), (620, 420));
    }
    assert_eq!(ed(&mut a).sel, Some(r(380.0, 280.0, 240.0, 140.0)));
    // Ctrl: keeps the 2:1 aspect, centred on the dragged box.
    let mut a = app(Some(sel));
    {
        let _m = hold(CTRL);
        drag(&mut a, (600, 400), (620, 500));
    }
    assert_eq!(ed(&mut a).sel, Some(r(400.0, 345.0, 220.0, 110.0)));
    // Past the opposite edge: the rect flips.
    let mut a = app(Some(sel));
    drag(&mut a, (600, 350), (300, 350));
    assert_eq!(ed(&mut a).sel, Some(r(300.0, 300.0, 100.0, 100.0)));
    // Off the shot: moved back inside, size kept.
    let mut a = app(Some(sel));
    drag(&mut a, (400, 300), (-50, -50));
    assert_eq!(ed(&mut a).sel, Some(r(0.0, 0.0, 650.0, 450.0)));
}

#[test]
fn selection_spans_and_clamps_across_monitors() {
    // 2400x900: left monitor (0,200) 1200x700, right (1200,0) 1200x900.
    let mut a = quiet(preview_app_with(theme::DARK, None, two_monitor_shot()));
    drag(&mut a, (1000, 300), (1300, 400));
    assert_eq!(ed(&mut a).sel, Some(r(1000.0, 300.0, 300.0, 100.0)), "spans both monitors");
    drag(&mut a, (1100, 350), (5000, 350));
    assert_eq!(ed(&mut a).sel, Some(r(2100.0, 300.0, 300.0, 100.0)), "clamped to the whole shot");
    // Chrome follows the monitor with the larger overlap.
    let now = Instant::now();
    ed(&mut a).prepare(None, (0, 0), now);
    assert_eq!(ed(&mut a).area_drawn, Some(r(1200.0, 0.0, 1200.0, 900.0)));

    // No selection: the monitor under the pointer.
    let mut a = quiet(preview_app_with(theme::DARK, None, two_monitor_shot()));
    ed(&mut a).prepare(None, (100, 500), now);
    assert_eq!(ed(&mut a).area_drawn, Some(r(0.0, 200.0, 1200.0, 700.0)));
    mv(&mut a, 1500, 100); // area moved: a repaint is asked, nothing else
    let m = a.mouse;
    ed(&mut a).prepare(None, m, now);
    assert_eq!(ed(&mut a).area_drawn, Some(r(1200.0, 0.0, 1200.0, 900.0)));
}

#[test]
fn arrow_keys_nudge_and_grow_the_selection() {
    let mut a = app(Some(r(100.0, 100.0, 50.0, 40.0)));
    for (vk, want) in [
        (key::LEFT, r(99.0, 100.0, 50.0, 40.0)),
        (key::RIGHT, r(100.0, 100.0, 50.0, 40.0)),
        (key::UP, r(100.0, 99.0, 50.0, 40.0)),
        (key::DOWN, r(100.0, 100.0, 50.0, 40.0)),
    ] {
        key(&mut a, vk);
        assert_eq!(ed(&mut a).sel, Some(want), "nudge {vk:#x}");
    }
    for (vk, want) in [
        (key::LEFT, r(99.0, 100.0, 51.0, 40.0)),
        (key::RIGHT, r(99.0, 100.0, 52.0, 40.0)),
        (key::UP, r(99.0, 99.0, 52.0, 41.0)),
        (key::DOWN, r(99.0, 99.0, 52.0, 42.0)),
    ] {
        key_m(&mut a, vk, SHIFT);
        assert_eq!(ed(&mut a).sel, Some(want), "grow {vk:#x}");
    }
    // Arrows auto-repeat; Ctrl+arrow is not a nudge.
    repeat(&mut a, key::RIGHT);
    assert_eq!(ed(&mut a).sel.unwrap().x, 100.0);
    key_m(&mut a, key::RIGHT, CTRL);
    assert_eq!(ed(&mut a).sel.unwrap().x, 100.0);
    // Clamped at the shot edge.
    let mut a = app(Some(r(0.0, 0.0, 50.0, 40.0)));
    key(&mut a, key::LEFT);
    key(&mut a, key::UP);
    assert_eq!(ed(&mut a).sel, Some(r(0.0, 0.0, 50.0, 40.0)));
    // HiDPI: one step is one logical pixel.
    let mut a = app(Some(r(100.0, 100.0, 50.0, 40.0)));
    ed(&mut a).shot.scale = 2.0;
    key(&mut a, key::RIGHT);
    assert_eq!(ed(&mut a).sel.unwrap().x, 102.0);
    // No selection: arrows do nothing.
    let mut a = app(None);
    key(&mut a, key::LEFT);
    assert_eq!(ed(&mut a).sel, None);
}

#[test]
fn select_all_and_a_second_press_while_dragging() {
    let mut a = app(Some(r(10.0, 10.0, 20.0, 20.0)));
    down(&mut a, 15, 15); // grabs the NW handle
    key_m(&mut a, 'A' as u32, CTRL);
    assert_eq!(ed(&mut a).sel, Some(r(0.0, 0.0, 1440.0, 900.0)));
    assert!(matches!(ed(&mut a).interact, Interact::None), "select-all ends the drag");
    // A press while a drag runs is ignored.
    let mut a = app(None);
    down(&mut a, 100, 100);
    down(&mut a, 400, 400);
    let Interact::NewSel { anchor, .. } = ed(&mut a).interact else { panic!("still the first drag") };
    assert_eq!(anchor, Pt::new(100.0, 100.0));
}

#[test]
fn cursor_follows_handles_selection_and_tool() {
    let a = app(None);
    assert_eq!(a.cursor(), Cursor::Cross, "no selection");
    let mut a = app(Some(r(400.0, 300.0, 200.0, 100.0)));
    for ((x, y), want) in [
        ((400, 300), Cursor::SizeNWSE),
        ((600, 400), Cursor::SizeNWSE),
        ((600, 300), Cursor::SizeNESW),
        ((400, 400), Cursor::SizeNESW),
        ((500, 300), Cursor::SizeNS),
        ((500, 400), Cursor::SizeNS),
        ((400, 350), Cursor::SizeWE),
        ((600, 350), Cursor::SizeWE),
        ((500, 350), Cursor::Move),
        ((50, 50), Cursor::Cross),
    ] {
        mv(&mut a, x, y);
        assert_eq!(a.cursor(), want, "at {x},{y}");
    }
    key(&mut a, 'R' as u32);
    assert_eq!(a.cursor(), Cursor::Cross, "a tool");
    key(&mut a, 'T' as u32);
    click(&mut a, 450, 320);
    assert_eq!(a.cursor(), Cursor::IBeam, "text draft");
    a.st = State::Hidden;
    assert_eq!(a.cursor(), Cursor::Arrow, "nothing open");
}

// ---- drawing ---------------------------------------------------------------

#[test]
fn every_tool_drafts_then_commits_on_release() {
    let cases: [(char, Tool); 8] = [
        ('P', Tool::Path),
        ('D', Tool::Line),
        ('A', Tool::Arrow),
        ('R', Tool::Rect),
        ('C', Tool::Ellipse),
        ('M', Tool::Marker),
        ('B', Tool::Pixelate),
        ('I', Tool::Invert),
    ];
    for (k, tool) in cases {
        let mut a = app(Some(r(100.0, 100.0, 800.0, 600.0)));
        key(&mut a, k as u32);
        assert_eq!(ed(&mut a).tool, Some(tool), "{k}");
        down(&mut a, 200, 200);
        mv(&mut a, 250, 230);
        mv(&mut a, 300, 260);
        let draft = ed(&mut a).draft.clone().unwrap_or_else(|| panic!("{tool:?}: draft while dragging"));
        assert!(ed(&mut a).objects.is_empty(), "{tool:?}: nothing committed yet");
        up(&mut a, 300, 260);
        let e = ed(&mut a);
        assert_eq!(e.objects, vec![draft.clone()], "{tool:?}: the draft is committed");
        assert!(e.draft.is_none() && e.stroke_pts.is_empty());
        let ok = match (tool, &draft) {
            (Tool::Path, Obj::Path { pts, .. }) => pts.len() == 3,
            (Tool::Line, Obj::Line { a, b, .. }) | (Tool::Arrow, Obj::Arrow { a, b, .. }) => {
                *a == Pt::new(200.0, 200.0) && *b == Pt::new(300.0, 260.0)
            }
            (Tool::Marker, Obj::Marker { a, b, color, width }) => {
                *a == Pt::new(200.0, 200.0) && *b == Pt::new(300.0, 260.0) && color.a == 90 && *width == e.sizes.marker
            }
            (Tool::Rect, Obj::Rect { r: rr, .. })
            | (Tool::Ellipse, Obj::Ellipse { r: rr, .. })
            | (Tool::Pixelate, Obj::Pixelate { r: rr, .. })
            | (Tool::Invert, Obj::Invert { r: rr }) => *rr == r(200.0, 200.0, 100.0, 60.0),
            _ => false,
        };
        assert!(ok, "{tool:?}: {draft:?}");
        // The export bakes it in.
        e.rebuild();
        let img = e.export(r(100.0, 100.0, 800.0, 600.0));
        assert_eq!(img.dimensions(), (800, 600));
        assert!(e.composed.is_some(), "{tool:?}: composed after commit");
    }
}

#[test]
fn tiny_strokes_are_dropped() {
    for k in ['D', 'A', 'M', 'R', 'C', 'B', 'I'] {
        let mut a = app(Some(r(100.0, 100.0, 800.0, 600.0)));
        key(&mut a, k as u32);
        down(&mut a, 200, 200);
        mv(&mut a, 200, 201);
        up(&mut a, 200, 201);
        assert!(ed(&mut a).objects.is_empty(), "{k}: a 1 px stroke is no object");
    }
    // A pencil click still draws a dot (two points).
    let mut a = app(Some(r(100.0, 100.0, 800.0, 600.0)));
    key(&mut a, 'P' as u32);
    click(&mut a, 200, 200);
    assert_eq!(ed(&mut a).objects.len(), 1);
}

#[test]
fn shift_constrains_lines_and_shapes() {
    let mut a = app(Some(r(0.0, 0.0, 1440.0, 900.0)));
    key(&mut a, 'D' as u32);
    {
        let _m = hold(SHIFT);
        drag(&mut a, (200, 200), (300, 210));
    }
    let Obj::Line { b, .. } = ed(&mut a).objects[0] else { panic!("line") };
    assert!((b.y - 200.0).abs() < 0.01 && (b.x - 200.0 - 100.5f32).abs() < 0.5, "snapped flat: {b:?}");
    {
        let _m = hold(CTRL);
        drag(&mut a, (200, 200), (300, 290));
    }
    let Obj::Line { b, .. } = ed(&mut a).objects[1] else { panic!("line") };
    assert!((b.x - 200.0 - (b.y - 200.0)).abs() < 0.01, "snapped to 45 degrees: {b:?}");
    key(&mut a, 'R' as u32);
    {
        let _m = hold(SHIFT);
        drag(&mut a, (500, 500), (560, 700));
    }
    let Obj::Rect { r: rr, .. } = ed(&mut a).objects[2] else { panic!("rect") };
    assert_eq!(rr, r(500.0, 500.0, 200.0, 200.0), "a square");
    // Pencil points closer than 2 px are merged.
    key(&mut a, 'P' as u32);
    down(&mut a, 10, 10);
    for x in 11..20 {
        mv(&mut a, x, 10);
    }
    up(&mut a, 19, 10);
    let Obj::Path { pts, .. } = &ed(&mut a).objects[3] else { panic!("path") };
    assert_eq!(pts.len(), 5, "{pts:?}");
}

#[test]
fn tools_toggle_and_escape_steps_back() {
    let mut a = app(Some(r(100.0, 100.0, 300.0, 200.0)));
    key(&mut a, 'R' as u32);
    key(&mut a, 'R' as u32);
    assert_eq!(ed(&mut a).tool, None, "same key twice: off");
    key(&mut a, 'R' as u32);
    key(&mut a, 'C' as u32);
    assert_eq!(ed(&mut a).tool, Some(Tool::Ellipse), "another tool replaces it");
    // Esc mid-drag drops the draft and the tool, keeps the capture.
    down(&mut a, 150, 150);
    mv(&mut a, 200, 200);
    key(&mut a, key::ESCAPE);
    let e = ed(&mut a);
    assert!(e.tool.is_none() && e.draft.is_none() && matches!(e.interact, Interact::None) && !e.done);
    key(&mut a, key::SPACE);
    assert!(ed(&mut a).palette_open);
    key(&mut a, key::ESCAPE);
    assert!(!ed(&mut a).palette_open, "Esc closes the palette first");
    // Keys other than arrows and editing keys are edge-triggered.
    repeat(&mut a, 'R' as u32);
    assert_eq!(ed(&mut a).tool, None);
    // Key-up events are ignored.
    a.on_event(Ev::Key { vk: 'R' as u32, up: true, repeat: false, mods: NONE });
    assert_eq!(ed(&mut a).tool, None);
}

// ---- text --------------------------------------------------------------------

/// Text tool, a click at (300, 300) and `s` typed.
fn typing(s: &str) -> App {
    let mut a = app(Some(r(100.0, 100.0, 800.0, 600.0)));
    key(&mut a, 'T' as u32);
    click(&mut a, 300, 300);
    assert!(ed(&mut a).text.is_some(), "a click opens the draft");
    type_str(&mut a, s);
    a
}

fn draft(a: &mut App) -> (String, usize) {
    let td = ed(a).text.as_ref().expect("text draft");
    (td.text.clone(), td.caret)
}

#[test]
fn text_insert_and_caret_moves() {
    let mut a = typing("Héllo");
    assert_eq!(draft(&mut a), ("Héllo".into(), 6));
    key(&mut a, key::LEFT);
    key(&mut a, key::LEFT);
    assert_eq!(draft(&mut a).1, 4);
    type_str(&mut a, "日");
    assert_eq!(draft(&mut a), ("Hél日lo".into(), 7));
    key(&mut a, key::HOME);
    assert_eq!(draft(&mut a).1, 0);
    key(&mut a, key::LEFT);
    assert_eq!(draft(&mut a).1, 0, "stays at the start");
    key(&mut a, key::RIGHT);
    key(&mut a, key::RIGHT);
    assert_eq!(draft(&mut a).1, 3, "over the two-byte é");
    key(&mut a, key::END);
    assert_eq!(draft(&mut a).1, "Hél日lo".len());
    key(&mut a, key::RIGHT);
    assert_eq!(draft(&mut a).1, "Hél日lo".len(), "stays at the end");
    // Control characters (Enter's '\r', Tab) are not inserted.
    type_str(&mut a, "\r\t\u{8}");
    assert_eq!(draft(&mut a).0, "Hél日lo");
    // Tools keys type nothing and switch nothing while editing.
    key(&mut a, 'R' as u32);
    assert_eq!(ed(&mut a).tool, Some(Tool::Text));
}

#[test]
fn text_backspace_and_delete_repeat() {
    let mut a = typing("ab日c");
    key(&mut a, key::BACK);
    assert_eq!(draft(&mut a), ("ab日".into(), 5));
    repeat(&mut a, key::BACK);
    assert_eq!(draft(&mut a), ("ab".into(), 2), "Backspace auto-repeats over a 3-byte char");
    key(&mut a, key::HOME);
    key(&mut a, key::DELETE);
    assert_eq!(draft(&mut a), ("b".into(), 0));
    repeat(&mut a, key::DELETE);
    assert_eq!(draft(&mut a), ("".into(), 0));
    key(&mut a, key::DELETE);
    key(&mut a, key::BACK);
    assert_eq!(draft(&mut a), ("".into(), 0), "nothing left to delete");
}

#[test]
fn text_commit_cancel_and_click_outside() {
    // Enter commits at the click point with the current colour and size.
    let mut a = typing("Hello  ");
    key(&mut a, key::RETURN);
    let e = ed(&mut a);
    assert!(e.text.is_none() && !e.done, "Enter commits the draft, not the capture");
    assert_eq!(
        e.objects,
        vec![Obj::Text { pos: Pt::new(300.0, 300.0), text: "Hello  ".into(), color: e.color, size: e.sizes.font }]
    );
    // Shift+Enter neither commits nor inserts (single-line drafts).
    let mut a = typing("x");
    key_m(&mut a, key::RETURN, SHIFT);
    assert_eq!(draft(&mut a).0, "x");
    // Esc drops the draft, keeps the tool.
    key(&mut a, key::ESCAPE);
    let e = ed(&mut a);
    assert!(e.text.is_none() && e.objects.is_empty() && e.tool == Some(Tool::Text) && !e.done);
    // Whitespace only: nothing to commit.
    let mut a = typing("   ");
    key(&mut a, key::RETURN);
    assert!(ed(&mut a).objects.is_empty());
    // A click inside the box keeps editing; one outside commits.
    let mut a = typing("abc");
    click(&mut a, 302, 305);
    assert!(ed(&mut a).text.is_some(), "inside the draft box");
    click(&mut a, 700, 600);
    let e = ed(&mut a);
    assert!(e.text.is_none() && e.objects.len() == 1, "outside commits");
    // The next click starts a new draft there.
    click(&mut a, 700, 600);
    assert_eq!(ed(&mut a).text.as_ref().map(|t| t.pos), Some(Pt::new(700.0, 600.0)));
    // The wheel does not change sizes while typing.
    let before = ed(&mut a).sizes.font;
    ed(&mut a).last_wheel = a_second_ago();
    a.on_event(Ev::Wheel { delta: 120, x: 0, y: 0 });
    assert_eq!(ed(&mut a).sizes.font, before);
}

// ---- history -----------------------------------------------------------------

/// Draw a rect from (x, 100) to (x + 50, 150) with the current tool.
fn stroke(a: &mut App, x: i32) {
    drag(a, (x, 100), (x + 50, 150));
}

#[test]
fn undo_redo_across_tools() {
    let mut a = app(Some(r(0.0, 0.0, 1440.0, 900.0)));
    key(&mut a, 'R' as u32);
    stroke(&mut a, 100);
    key(&mut a, 'C' as u32);
    stroke(&mut a, 200);
    key(&mut a, 'B' as u32);
    stroke(&mut a, 300);
    let kinds = |a: &mut App| ed(a).objects.len();
    assert_eq!(kinds(&mut a), 3);
    key_m(&mut a, 'Z' as u32, CTRL);
    key_m(&mut a, 'Z' as u32, CTRL);
    assert!(matches!(ed(&mut a).objects[..], [Obj::Rect { .. }]));
    key_m(&mut a, 'Y' as u32, CTRL);
    assert!(matches!(ed(&mut a).objects[..], [Obj::Rect { .. }, Obj::Ellipse { .. }]));
    key_m(&mut a, 'Z' as u32, CTRL_SHIFT);
    assert_eq!(kinds(&mut a), 3, "Ctrl+Shift+Z redoes too");
    key_m(&mut a, 'Y' as u32, CTRL);
    assert_eq!(kinds(&mut a), 3, "nothing more to redo");
    for _ in 0..5 {
        key_m(&mut a, 'Z' as u32, CTRL);
    }
    assert!(ed(&mut a).objects.is_empty(), "undo stops at the empty start");
    assert!(ed(&mut a).dirty);
    // Rebuilt with no objects: the composed copy is dropped.
    ed(&mut a).rebuild();
    assert!(ed(&mut a).composed.is_none());
    // A new object after an undo discards the redo branch.
    key_m(&mut a, 'Y' as u32, CTRL);
    key(&mut a, 'I' as u32);
    stroke(&mut a, 400);
    assert!(matches!(ed(&mut a).objects[..], [Obj::Rect { .. }, Obj::Invert { .. }]));
    key_m(&mut a, 'Y' as u32, CTRL);
    assert_eq!(ed(&mut a).objects.len(), 2, "the old redo branch is gone");
}

#[test]
fn undo_limit_drops_the_oldest_states() {
    let mut a = app(Some(r(0.0, 0.0, 1440.0, 900.0)));
    ed(&mut a).cfg.undo_limit = 3;
    key(&mut a, 'R' as u32);
    for i in 0..5 {
        stroke(&mut a, 100 + i * 100);
    }
    assert_eq!(ed(&mut a).hist.len(), 3, "limit");
    for _ in 0..10 {
        key_m(&mut a, 'Z' as u32, CTRL);
    }
    assert_eq!(ed(&mut a).objects.len(), 3, "only two steps back");
    // undo_limit = 0 behaves as 1: no undo at all.
    let mut a = app(Some(r(0.0, 0.0, 1440.0, 900.0)));
    ed(&mut a).cfg.undo_limit = 0;
    key(&mut a, 'R' as u32);
    stroke(&mut a, 100);
    stroke(&mut a, 200);
    key_m(&mut a, 'Z' as u32, CTRL);
    assert_eq!(ed(&mut a).objects.len(), 2);
}

// ---- palette, sizes, toolbar ------------------------------------------------

#[test]
fn wheel_and_size_acts_change_the_active_tool_size() {
    let mut a = app(Some(r(100.0, 100.0, 600.0, 400.0)));
    let wheel = |a: &mut App, delta: i32| {
        ed(a).last_wheel = a_second_ago();
        a.on_event(Ev::Wheel { delta, x: 10, y: 10 });
    };
    let line = ed(&mut a).sizes.line;
    wheel(&mut a, 120);
    assert_eq!(ed(&mut a).sizes.line, line + 1.0);
    assert_eq!(ed(&mut a).notice.as_ref().map(|t| t.text.clone()), Some(format!("Size {}", line as i32 + 1)));
    // A second notch within 160 ms is ignored, as is a zero delta.
    a.on_event(Ev::Wheel { delta: 120, x: 10, y: 10 });
    wheel(&mut a, 0);
    assert_eq!(ed(&mut a).sizes.line, line + 1.0);
    for (k, lo, hi, unit) in [('R', 1.0, 50.0, "shape"), ('M', 1.0, 50.0, "mark"), ('B', 4.0, 100.0, "block"), ('I', 4.0, 100.0, "block"), ('T', 8.0, 96.0, "text"), ('D', 1.0, 50.0, "line")] {
        key(&mut a, k as u32);
        assert_eq!(ed(&mut a).size_label().1, unit, "{k}");
        for _ in 0..120 {
            wheel(&mut a, -120);
        }
        let v = |a: &mut App| ed(a).size_label().0.parse::<f32>().unwrap();
        assert_eq!(v(&mut a), lo, "{k}: floor");
        for _ in 0..120 {
            let State::Edit(e) = std::mem::replace(&mut a.st, State::Hidden) else { unreachable!() };
            let mut e = *e;
            a.apply_act(&mut e, Act::Size(1));
            a.st = State::Edit(Box::new(e));
        }
        assert_eq!(v(&mut a), hi, "{k}: ceiling");
        key(&mut a, k as u32); // tool off again
    }
}

#[test]
fn palette_opens_only_with_a_selection_and_picks_a_colour() {
    let mut a = app(None);
    key(&mut a, key::SPACE);
    assert!(!ed(&mut a).palette_open, "no toolbar without a selection");
    let mut a = app(Some(r(100.0, 100.0, 600.0, 400.0)));
    key(&mut a, key::SPACE);
    assert!(ed(&mut a).palette_open);
    let now = Instant::now();
    ed(&mut a).prepare(None, (0, 0), now);
    let tb = ed(&mut a).toolbar.clone().expect("toolbar");
    let dots: Vec<_> = tb.items.iter().filter_map(|it| if let toolbar::Kind::Dot(c) = it.kind { Some((c, it.r)) } else { None }).collect();
    assert!(!dots.is_empty() && dots.len() <= palette_colors(&ed(&mut a).cfg).len());
    let (c, dr) = *dots.last().unwrap();
    click(&mut a, (dr.x + dr.w / 2.0) as i32, (dr.y + dr.h / 2.0) as i32);
    assert_eq!(ed(&mut a).color, c);
    assert!(!ed(&mut a).palette_open, "picking a colour closes it");
}

/// Centre of the toolbar button for `act` (lays the toolbar out first).
fn button(a: &mut App, act: Act) -> (i32, i32) {
    let m = a.mouse;
    ed(a).prepare(None, m, Instant::now());
    let tb = ed(a).toolbar.as_ref().expect("toolbar");
    let it = tb.items.iter().find(|i| i.kind == toolbar::Kind::Btn(act)).unwrap_or_else(|| panic!("{act:?} button"));
    ((it.r.x + it.r.w / 2.0) as i32, (it.r.y + it.r.h / 2.0) as i32)
}

#[test]
fn toolbar_buttons_act_and_consume_the_press() {
    let mut a = app(Some(r(100.0, 100.0, 600.0, 400.0)));
    let (x, y) = button(&mut a, Act::Tool(Tool::Rect));
    down(&mut a, x, y);
    assert_eq!(ed(&mut a).tool, Some(Tool::Rect));
    assert!(ed(&mut a).pressed.is_some() && matches!(ed(&mut a).interact, Interact::None), "no drag starts");
    up(&mut a, x, y);
    assert!(ed(&mut a).pressed.is_none());
    // Hovering a button is tracked (tooltip).
    mv(&mut a, x, y);
    assert!(ed(&mut a).hover.is_some());
    mv(&mut a, 5, 5);
    assert!(ed(&mut a).hover.is_none());
    // Disabled Undo consumes the press but does nothing.
    let (ux, uy) = button(&mut a, Act::Undo);
    click(&mut a, ux, uy);
    assert!(ed(&mut a).objects.is_empty() && matches!(ed(&mut a).interact, Interact::None));
    // Size buttons and the palette button.
    let line = ed(&mut a).sizes.shape;
    let (sx, sy) = button(&mut a, Act::Size(1));
    click(&mut a, sx, sy);
    assert_eq!(ed(&mut a).sizes.shape, line + 1.0);
    let (px, py) = button(&mut a, Act::Palette);
    click(&mut a, px, py);
    assert!(ed(&mut a).palette_open);
    // A press on the bar commits an open text draft first.
    let mut a = typing("note");
    let (tx, ty) = button(&mut a, Act::Tool(Tool::Text));
    down(&mut a, tx, ty);
    let e = ed(&mut a);
    assert!(e.text.is_none() && e.objects.len() == 1 && e.tool.is_none(), "committed, and the Text tool toggled off");
}

#[test]
fn output_buttons_and_keys_set_the_tasks() {
    // (act, tasks) without running the export.
    let run = |act: Act, save_dialog: bool| {
        let mut a = app(Some(r(10.0, 10.0, 50.0, 50.0)));
        let State::Edit(e) = std::mem::replace(&mut a.st, State::Hidden) else { unreachable!() };
        let mut e = *e;
        e.cfg.save_dialog = save_dialog;
        a.apply_act(&mut e, act);
        e
    };
    assert!(matches!(run(Act::Copy, false).tasks[..], [Task::Copy]));
    assert!(matches!(run(Act::Upload, false).tasks[..], [Task::Upload]));
    assert!(matches!(run(Act::Save, false).tasks[..], [Task::Save { path: None, ask: false }]));
    assert!(matches!(run(Act::Save, true).tasks[..], [Task::Save { path: None, ask: true }]), "save_dialog asks");
    assert!(matches!(run(Act::SaveAs, false).tasks[..], [Task::Save { path: None, ask: true }]));
    let e = run(Act::Exit, false);
    assert!(e.done && e.cancelled);
    let e = run(Act::Accept, false);
    assert!(e.done && !e.cancelled && e.tasks.is_empty());
    for (vk, mods, act) in [('C', CTRL, Act::Copy), ('U', CTRL, Act::Upload), ('S', CTRL, Act::Save)] {
        let mut a = app(Some(r(10.0, 10.0, 50.0, 50.0)));
        let State::Edit(e) = std::mem::replace(&mut a.st, State::Hidden) else { unreachable!() };
        let mut e = *e;
        a.handle_key(&mut e, vk as u32, false, mods);
        assert!(e.done, "{act:?}");
        assert_eq!(action_of(act).and_then(act_of), Some(act), "{act:?} round-trips through its keymap action");
    }
    assert_eq!(action_of(Act::Exit), Some(Action::Cancel));
    assert_eq!(action_of(Act::Accept), Some(Action::Accept));
    assert_eq!(action_of(Act::Color(C4::rgb(1, 2, 3))), None);
    assert_eq!(action_of(Act::Size(1)), None);
    for a in [Action::SelectAll, Action::Accept, Action::Cancel] {
        assert_eq!(act_of(a), None);
    }
}

// ---- accept / cancel -------------------------------------------------------

#[test]
fn escape_cancels_one_shot_with_code_2_and_hides_a_daemon() {
    let mut a = app(Some(r(10.0, 10.0, 50.0, 50.0)));
    key(&mut a, key::ESCAPE);
    assert!(hidden(&a));
    assert_eq!(code(&a), 2);
    let mut a = app(None);
    a.kind = RunKind::Daemon;
    key(&mut a, key::ESCAPE);
    assert!(hidden(&a), "daemon: hidden, waiting for the next hotkey");
    assert_eq!(code(&a), 0);
    // The toolbar's exit button does the same.
    let mut a = app(Some(r(100.0, 100.0, 600.0, 400.0)));
    let (x, y) = button(&mut a, Act::Exit);
    down(&mut a, x, y);
    assert!(hidden(&a));
    assert_eq!(code(&a), 2);
}

#[test]
fn enter_exports_the_selection_and_closes_a_one_shot() {
    let dir = TempDir::new("enter");
    let mut a = app(Some(r(100.0, 80.0, 300.0, 200.0)));
    ed(&mut a).tasks = vec![Task::Save { path: Some(dir.0.clone()), ask: false }, Task::Geometry];
    ed(&mut a).cfg.filename_pattern = "shot".into();
    key(&mut a, 'R' as u32);
    stroke(&mut a, 150);
    key(&mut a, key::RETURN);
    assert!(hidden(&a), "export ran and the one-shot closed");
    assert_eq!(code(&a), 0);
    let saved = dir.0.join("shot.png");
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&saved).expect("saved PNG")));
    let reader = dec.read_info().expect("png");
    assert_eq!((reader.info().width, reader.info().height), (300, 200), "the selection, cropped");
    assert!(a.upload_slot.lock().unwrap().is_none());
}

#[test]
fn enter_without_a_selection_takes_the_whole_shot_and_errors_set_code_1() {
    let dir = TempDir::new("all");
    // The parent "folder" is a file: the save fails.
    let blocker = dir.0.join("file");
    std::fs::write(&blocker, b"x").unwrap();
    let mut a = app(None);
    a.kind = RunKind::Daemon;
    ed(&mut a).tasks = vec![Task::Save { path: Some(blocker.join("out.png")), ask: false }];
    key(&mut a, key::RETURN);
    assert!(hidden(&a), "daemon: back to hidden after the export");
    assert_eq!(code(&a), 1, "a failed task sets the exit code");
    // accept_key alone: no selection selects all; no tasks means Save.
    let mut a = app(None);
    let State::Edit(e) = std::mem::replace(&mut a.st, State::Hidden) else { unreachable!() };
    let mut e = *e;
    accept_key(&mut e);
    assert_eq!(e.sel, Some(r(0.0, 0.0, 1440.0, 900.0)));
    assert!(e.done && matches!(e.tasks[..], [Task::Save { path: None, ask: false }]));
    // With accept_on_select it accepts right away, keeping the given tasks.
    let mut e = {
        let mut a = app(None);
        let State::Edit(e) = std::mem::replace(&mut a.st, State::Hidden) else { unreachable!() };
        *e
    };
    e.accept_on_select = true;
    accept_key(&mut e);
    assert!(e.done && e.tasks.is_empty());
}

#[test]
fn accept_on_select_finishes_when_the_selection_ends() {
    let mut a = app(None);
    ed(&mut a).accept_on_select = true;
    ed(&mut a).tasks = vec![Task::Geometry];
    drag(&mut a, (10, 10), (110, 60));
    assert!(hidden(&a));
    assert_eq!(code(&a), 0);
}

#[test]
fn dirty_objects_are_baked_before_the_export() {
    let mut a = app(Some(r(0.0, 0.0, 100.0, 100.0)));
    let e = ed(&mut a);
    e.objects.push(Obj::Invert { r: r(0.0, 0.0, 10.0, 10.0) });
    e.dirty = true;
    let before = e.base.as_raw()[..4].to_vec();
    let State::Edit(e) = std::mem::replace(&mut a.st, State::Hidden) else { unreachable!() };
    let mut e = *e;
    a.finish(&mut e, false);
    let State::Finish(job) = &a.st else { panic!("finish job") };
    assert_eq!(job.img.dimensions(), (100, 100));
    assert_ne!(job.img.as_raw()[..3], before[..3], "the inverted corner is in the export");
    assert_eq!(job.sel_global, (0, 0));
}

// ---- toasts and timers ------------------------------------------------------

fn aged(t: &mut Toast, ms: u64) {
    t.at = Instant::now().checked_sub(Duration::from_millis(ms)).expect("uptime");
}

#[test]
fn toast_lifecycle() {
    let mut t = Toast::new("Size 4", ToastKind::Info);
    assert_eq!(t.ttl_ms(), 1600.0);
    assert_eq!(Toast::new("x", ToastKind::Error).ttl_ms(), 4000.0);
    assert!(t.animating(Instant::now()), "fading in");
    aged(&mut t, 800);
    let now = Instant::now();
    assert!(!t.animating(now) && (t.opacity(now) - 1.0).abs() < 1e-6 && !t.expired(now), "fully shown");
    aged(&mut t, 1650);
    let now = Instant::now();
    assert!(t.animating(now) && t.opacity(now) < 1.0 && t.opacity(now) > 0.0 && !t.expired(now), "fading out");
    aged(&mut t, 1800);
    assert!(t.expired(Instant::now()));

    // The pump drops expired toasts (the editor's and the app's).
    let mut a = app(Some(r(10.0, 10.0, 50.0, 50.0)));
    let mut old = Toast::new("Size 2", ToastKind::Info);
    aged(&mut old, 5000);
    ed(&mut a).notice = Some(old);
    let mut gone = Toast::new("Uploaded", ToastKind::Success);
    aged(&mut gone, 5000);
    a.notice = Some(gone);
    assert!(a.on_event(Ev::Timer), "the toast went away: a new frame");
    assert!(ed(&mut a).notice.is_none() && a.notice.is_none());
    // A live toast stays.
    ed(&mut a).notice = Some(Toast::new("Size 3", ToastKind::Info));
    a.on_event(Ev::Timer);
    assert!(ed(&mut a).notice.is_some());
}

#[test]
fn timer_repaints_only_while_something_changes() {
    let mut a = app(Some(r(10.0, 10.0, 200.0, 100.0)));
    assert!(a.on_event(Ev::Timer), "fade-in running");
    let e = ed(&mut a);
    e.prepare(None, (0, 0), Instant::now());
    e.toast_drawn = None;
    e.caret_drawn = None;
    let e = ed(&mut a);
    for t in [&mut e.mo.dim, &mut e.mo.bar, &mut e.mo.pop, &mut e.mo.hint, &mut e.mo.hover] {
        t.snap(t.target());
    }
    assert!(!a.on_event(Ev::Timer), "settled: no frame");
    // Hidden: only a state change asks for a frame.
    a.st = State::Hidden;
    assert!(!a.on_event(Ev::Timer));
}

#[test]
fn window_events_the_overlay_ignores() {
    let mut a = app(Some(r(10.0, 10.0, 50.0, 50.0)));
    assert!(!a.on_event(Ev::Close));
    assert!(!a.on_event(Ev::Resize(10, 10)));
    assert!(!a.on_event(Ev::Focus(true)));
    assert!(matches!(a.st, State::Edit(_)));
    // Input with nothing open is harmless.
    a.st = State::Hidden;
    for ev in [Ev::Down { x: 1, y: 1 }, Ev::Move { x: 2, y: 2 }, Ev::Up { x: 2, y: 2 }, Ev::Char('a' as u16)] {
        assert!(a.on_event(ev));
    }
    key(&mut a, key::RETURN);
    assert!(hidden(&a) && a.mouse == (2, 2));
    assert!(a.frame().is_none() && a.damage().is_none());
}

#[test]
fn software_frame_and_no_gdi_damage() {
    let mut a = app(Some(r(100.0, 100.0, 300.0, 200.0)));
    let f = a.frame().expect("software frame");
    assert_eq!(f.dimensions(), (1440, 900));
    assert!(a.damage().is_none(), "software: the whole window");
    #[cfg(windows)]
    assert!(!a.paint(windows::Win32::Graphics::Gdi::HDC::default(), &[]), "no GDI screen");
    // The frame buffer is reused.
    let p = a.frame().unwrap().as_raw().as_ptr();
    assert_eq!(a.frame().unwrap().as_raw().as_ptr(), p);
}

// ---- upload results --------------------------------------------------------

#[test]
fn upload_results_become_toasts() {
    let mut a = app(None);
    a.st = State::Hidden;
    let tx = upload_in_flight(&a);
    a.on_event(Ev::Timer);
    assert!(a.upload_slot.lock().unwrap().is_some(), "still in flight");
    assert!(a.notice.is_none());
    tx.send(Ok("https://i.imgur.com/x.png".into())).unwrap();
    a.on_event(Ev::Timer);
    assert!(a.upload_slot.lock().unwrap().is_none());
    let t = a.notice.as_ref().expect("toast");
    assert!(t.text == "Uploaded https://i.imgur.com/x.png" && t.kind == ToastKind::Success, "{}", t.text);

    let tx = upload_in_flight(&a);
    tx.send(Err("HTTP 429".into())).unwrap();
    a.poll_upload();
    let t = a.notice.as_ref().expect("toast");
    assert!(t.text == "Upload failed: HTTP 429" && t.kind == ToastKind::Error, "{}", t.text);

    // The uploader went away without a word: just forget it.
    a.notice = None;
    drop(upload_in_flight(&a));
    a.poll_upload();
    assert!(a.upload_slot.lock().unwrap().is_none() && a.notice.is_none());
}

// ---- begin_capture ---------------------------------------------------------

fn queue(shot: anyhow::Result<Shot>) {
    FAKE_GRAB.with(|g| *g.borrow_mut() = Some(shot));
}

/// A hidden app waiting to start `pending`.
fn waiting(kind: RunKind, pending: Pending) -> App {
    let mut a = app(None);
    a.st = State::Hidden;
    a.kind = kind;
    a.pending = Some(pending);
    a
}

#[test]
fn begin_capture_maps_the_region_and_takes_the_overrides() {
    let mut shot = synthetic_shot();
    shot.origin = (-1440, 100);
    queue(Ok(shot));
    let mut p = Pending::editor();
    p.region = Some((-1400, 150, 300, 200));
    p.filename = Some("%F_x".into());
    p.tasks = vec![Task::Geometry];
    let mut a = waiting(RunKind::OneShot, p);
    a.cfg.draw_color = "#00ff00".into();
    assert!(a.on_event(Ev::Timer), "the editor opened");
    let e = ed(&mut a);
    assert_eq!(e.sel, Some(r(40.0, 50.0, 300.0, 200.0)), "global → image coordinates");
    assert_eq!(e.cfg.filename_pattern, "%F_x");
    assert_eq!(e.color, C4::rgb(0, 255, 0));
    assert!(e.shot.image.as_raw().is_empty() && e.base.dimensions() == (1440, 900), "pixels held once");
    assert!(matches!(e.tasks[..], [Task::Geometry]));
    assert!(a.pending.is_none());

    // A region past the shot is clamped; a degenerate one is no selection.
    queue(Ok(synthetic_shot()));
    let mut p = Pending::editor();
    p.region = Some((1400, 880, 300, 200));
    let mut a = waiting(RunKind::OneShot, p);
    a.on_event(Ev::Timer);
    assert_eq!(ed(&mut a).sel, Some(r(1140.0, 700.0, 300.0, 200.0)));
    queue(Ok(synthetic_shot()));
    let mut p = Pending::editor();
    p.region = Some((10, 10, 0, 50));
    let mut a = waiting(RunKind::OneShot, p);
    a.on_event(Ev::Timer);
    assert_eq!(ed(&mut a).sel, None);
}

#[test]
fn begin_capture_accepts_a_given_region_right_away() {
    queue(Ok(synthetic_shot()));
    let mut p = Pending::editor();
    p.region = Some((0, 0, 64, 32));
    p.accept_on_select = true;
    p.tasks = vec![Task::Geometry];
    let mut a = waiting(RunKind::OneShot, p);
    a.on_event(Ev::Timer);
    assert!(hidden(&a), "accepted, exported, closed");
    assert_eq!(code(&a), 0);
    // Without a region the editor waits for the selection.
    queue(Ok(synthetic_shot()));
    let mut p = Pending::editor();
    p.accept_on_select = true;
    let mut a = waiting(RunKind::OneShot, p);
    a.on_event(Ev::Timer);
    assert!(matches!(a.st, State::Edit(_)));
}

#[test]
fn capture_failure_exits_a_one_shot_and_keeps_a_daemon() {
    queue(Err(anyhow::anyhow!("no display")));
    let mut a = waiting(RunKind::OneShot, Pending::editor());
    a.on_event(Ev::Timer);
    assert!(hidden(&a));
    assert_eq!(code(&a), 1);
    queue(Err(anyhow::anyhow!("no display")));
    let mut a = waiting(RunKind::Daemon, Pending::editor());
    a.on_event(Ev::Timer);
    assert!(hidden(&a) && a.pending.is_none());
    assert_eq!(code(&a), 0, "the daemon keeps running");
}

#[test]
fn daemon_capture_cycle() {
    let mut a = app(None);
    a.st = State::Hidden;
    a.kind = RunKind::Daemon;
    a.on_create(Hwnd::default());
    assert!(hidden(&a));
    // A hotkey press opens the editor; a second press while it is open is ignored.
    assert!(a.on_hot(HotEvent::Capture));
    queue(Ok(synthetic_shot()));
    assert!(a.on_event(Ev::Timer));
    assert!(matches!(a.st, State::Edit(_)));
    assert!(a.on_hot(HotEvent::Capture));
    assert!(a.pending.is_none());
    // Select, copy... (Geometry here), back to hidden: the daemon stays.
    ed(&mut a).tasks = vec![Task::Geometry];
    drag(&mut a, (10, 10), (60, 60));
    key(&mut a, key::RETURN);
    assert!(hidden(&a));
    assert_eq!(code(&a), 0);
    // Quit closes it.
    assert!(!a.on_hot(HotEvent::Quit));
}

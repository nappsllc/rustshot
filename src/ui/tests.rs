//! Event-driven control tests (synthetic `wind::Ev` streams through
//! `Input` → `Ui` frames) and the preview gallery.

use super::preview::{self, FakeClip};
use super::*;
use crate::keymap::Chord;
use crate::pixbuf::PixBuf;
use crate::theme::{DARK, LIGHT};

const W: u32 = 480;
const HH: u32 = 360;

fn r(x: f32, y: f32, w: f32, h: f32) -> FRect {
    FRect { x, y, w, h }
}

const NONE: Mods = Mods { ctrl: false, shift: false, alt: false };
const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };
/// AltGr arrives as Ctrl+Alt on Windows.
const ALTGR: Mods = Mods { ctrl: true, shift: false, alt: true };

/// A window's worth of control state, driven one frame at a time.
struct Harness {
    input: Input,
    focus: FocusState,
    clip: FakeClip,
    img: PixBuf,
    /// DPI scale (physical px per logical px).
    k: f32,
}

impl Harness {
    fn new() -> Harness {
        Harness::scaled(1.0)
    }

    fn scaled(k: f32) -> Harness {
        let (w, h) = ((W as f32 * k).round() as u32, (HH as f32 * k).round() as u32);
        Harness { input: Input::default(), focus: FocusState::default(), clip: FakeClip::default(), img: PixBuf::new(w, h), k }
    }

    fn frame<R>(&mut self, f: impl FnOnce(&mut Ui) -> R) -> (R, Out) {
        let stride = self.img.width() as usize;
        let fb = Fb::new(self.img.as_raw_mut(), stride);
        let area = r(0.0, 0.0, W as f32, HH as f32);
        let mut ui = Ui::new(fb, &DARK, &self.input, &mut self.focus, &mut self.clip, self.k, area);
        let v = f(&mut ui);
        let out = ui.finish();
        self.input.end_frame();
        (v, out)
    }

    fn ev(&mut self, e: Ev) -> &mut Self {
        self.input.feed(&e);
        self
    }

    fn click(&mut self, x: i32, y: i32) -> &mut Self {
        self.ev(Ev::Move { x, y }).ev(Ev::Down { x, y }).ev(Ev::Up { x, y })
    }

    fn key(&mut self, vk: u32, mods: Mods) -> &mut Self {
        self.ev(Ev::Key { vk, up: false, repeat: false, mods })
    }

    fn typed(&mut self, s: &str) -> &mut Self {
        for u in s.encode_utf16() {
            self.ev(Ev::Char(u));
        }
        self
    }
}

// ---- button ----

fn one_button(ui: &mut Ui) -> bool {
    ui.place(r(20.0, 20.0, 100.0, 32.0)).button("ok", "OK", true)
}

#[test]
fn button_clicks_on_release_inside() {
    let mut h = Harness::new();
    assert!(!h.frame(one_button).0);
    h.click(50, 30);
    assert!(h.frame(one_button).0, "press+release inside");
    assert!(!h.frame(one_button).0, "only once");
    h.click(300, 300);
    assert!(!h.frame(one_button).0, "outside");
}

#[test]
fn button_press_dragged_off_does_not_click() {
    let mut h = Harness::new();
    h.ev(Ev::Down { x: 50, y: 30 });
    assert!(!h.frame(one_button).0);
    h.ev(Ev::Move { x: 300, y: 300 }).ev(Ev::Up { x: 300, y: 300 });
    assert!(!h.frame(one_button).0);
    // Pressed elsewhere, released over it: no click either.
    h.ev(Ev::Down { x: 300, y: 300 });
    h.frame(one_button);
    h.ev(Ev::Up { x: 50, y: 30 });
    assert!(!h.frame(one_button).0);
}

#[test]
fn button_keyboard_activation() {
    let mut h = Harness::new();
    h.frame(one_button);
    h.key(key::TAB, NONE);
    h.frame(one_button);
    assert!(h.focus.is_focused("ok"));
    h.key(key::RETURN, NONE);
    let (clicked, out) = h.frame(one_button);
    assert!(clicked && !out.enter, "Enter goes to the focused button");
    h.key(key::SPACE, NONE).typed(" ");
    assert!(h.frame(one_button).0);
}

#[test]
fn disabled_button_is_inert_and_skipped() {
    let mut h = Harness::new();
    let f = |ui: &mut Ui| {
        ui.enabled = false;
        let a = one_button(ui);
        ui.enabled = true;
        let b = ui.place(r(20.0, 80.0, 100.0, 32.0)).button("b", "B", false);
        (a, b)
    };
    h.frame(f);
    h.click(50, 30);
    assert_eq!(h.frame(f).0, (false, false));
    h.key(key::TAB, NONE);
    h.frame(f);
    assert!(h.focus.is_focused("b"), "Tab skips the disabled one");
}

// ---- toggle ----

#[test]
fn toggle_by_click_and_space() {
    let mut h = Harness::new();
    let mut on = false;
    let f = |h: &mut Harness, on: &mut bool| h.frame(|ui| ui.place(r(20.0, 20.0, 36.0, 32.0)).toggle("t", on)).0;
    f(&mut h, &mut on);
    h.click(30, 36);
    assert!(f(&mut h, &mut on) && on);
    h.key(key::SPACE, NONE);
    assert!(f(&mut h, &mut on), "focused by the click, Space flips");
    assert!(!f(&mut h, &mut on));
    assert!(!on, "flipped twice");
}

// ---- text field ----

fn field(h: &mut Harness, st: &mut TextState) -> bool {
    h.frame(|ui| ui.place(r(20.0, 20.0, 300.0, 32.0)).text_field("f", st)).0
}

#[test]
fn text_field_typing_and_deletion() {
    let mut h = Harness::new();
    let mut st = TextState::new("");
    field(&mut h, &mut st);
    h.click(100, 36);
    field(&mut h, &mut st);
    h.typed("hello");
    assert!(field(&mut h, &mut st));
    assert_eq!(st.text, "hello");
    h.key(key::LEFT, NONE).key(key::LEFT, NONE).key(key::BACK, NONE);
    field(&mut h, &mut st);
    assert_eq!((st.text.as_str(), st.caret()), ("helo", 2));
    h.key(key::DELETE, NONE).typed("X");
    field(&mut h, &mut st);
    assert_eq!(st.text, "heXo");
    h.key(key::HOME, NONE).key(key::DELETE, NONE).key(key::END, NONE).typed("!");
    field(&mut h, &mut st);
    assert_eq!(st.text, "eXo!");
    // Control codes (Ctrl+letter, Backspace) arrive as chars too: ignored.
    h.ev(Ev::Char(0x08)).ev(Ev::Char(0x01));
    assert!(!field(&mut h, &mut st));
    assert_eq!(st.text, "eXo!");
}

#[test]
fn text_field_selection_replace_and_words() {
    let mut h = Harness::new();
    let mut st = TextState::new("save path here");
    h.focus.focus("f");
    field(&mut h, &mut st);
    h.key(key::LEFT, Mods { ctrl: true, shift: true, alt: false });
    field(&mut h, &mut st);
    assert_eq!(st.selected(), "here");
    h.typed("there");
    field(&mut h, &mut st);
    assert_eq!(st.text, "save path there");
    h.key(key::HOME, SHIFT);
    field(&mut h, &mut st);
    assert_eq!(st.selected(), "save path there");
    h.key(key::RIGHT, NONE).key(key::BACK, CTRL);
    field(&mut h, &mut st);
    assert_eq!(st.text, "save path ", "Ctrl+Backspace deletes the word");
    h.typed("😀"); // two UTF-16 units
    field(&mut h, &mut st);
    assert_eq!(st.text, "save path 😀");
    h.key(key::BACK, NONE);
    field(&mut h, &mut st);
    assert_eq!(st.text, "save path ");
}

#[test]
fn text_field_clipboard() {
    let mut h = Harness::new();
    let mut st = TextState::new("alpha beta");
    h.focus.focus("f");
    field(&mut h, &mut st);
    h.key(0x41, CTRL).key(0x43, CTRL); // Ctrl+A, Ctrl+C
    field(&mut h, &mut st);
    assert_eq!(h.clip.0.as_deref(), Some("alpha beta"));
    h.clip.0 = Some("one\ntwo\r\n".into());
    h.key(key::END, NONE).key(0x56, CTRL); // Ctrl+V
    assert!(field(&mut h, &mut st));
    assert_eq!(st.text, "alpha betaone two", "single line: breaks become spaces");
    h.key(key::LEFT, Mods { ctrl: true, shift: true, alt: false }).key(0x58, CTRL); // Ctrl+X
    assert!(field(&mut h, &mut st));
    assert_eq!((st.text.as_str(), h.clip.0.as_deref()), ("alpha betaone ", Some("two")));
    h.clip.0 = None;
    h.key(0x56, CTRL);
    assert!(!field(&mut h, &mut st), "empty clipboard pastes nothing");
}

#[test]
fn text_field_altgr_is_not_a_ctrl_shortcut() {
    let mut h = Harness::new();
    let mut st = TextState::new("kot");
    h.focus.focus("f");
    field(&mut h, &mut st);
    // Polish AltGr+A: Key{A, Ctrl+Alt} then the typed character.
    h.key(0x41, ALTGR).typed("ą");
    assert!(field(&mut h, &mut st));
    assert_eq!((st.text.as_str(), st.selected()), ("kotą", ""), "no select-all, no replace");
    st.select(0..3);
    h.clip.0 = Some("clip".into());
    h.key(0x58, ALTGR).key(0x56, ALTGR).key(0x43, ALTGR); // AltGr+X/V/C
    assert!(!field(&mut h, &mut st));
    assert_eq!((st.text.as_str(), st.selected(), h.clip.0.as_deref()), ("kotą", "kot", Some("clip")));
    h.key(key::END, NONE).key(key::BACK, ALTGR);
    field(&mut h, &mut st);
    assert_eq!(st.text, "kot", "AltGr+Backspace deletes one char, not a word");
}

#[test]
fn text_field_shift_insert_and_shift_delete() {
    let mut h = Harness::new();
    let mut st = TextState::new("one two");
    h.focus.focus("f");
    field(&mut h, &mut st);
    h.key(key::LEFT, Mods { ctrl: true, shift: true, alt: false }).key(key::DELETE, SHIFT); // cut "two"
    assert!(field(&mut h, &mut st));
    assert_eq!((st.text.as_str(), h.clip.0.as_deref()), ("one ", Some("two")));
    h.key(key::HOME, NONE).key(key::INSERT, SHIFT); // paste at the start
    assert!(field(&mut h, &mut st));
    assert_eq!(st.text, "twoone ");
    h.key(key::DELETE, SHIFT); // nothing selected: plain Delete
    field(&mut h, &mut st);
    assert_eq!(st.text, "twone ");
}

#[test]
fn text_field_click_places_caret_and_drag_selects() {
    let mut h = Harness::new();
    let mut st = TextState::new("abcdefghij");
    field(&mut h, &mut st);
    h.click(30, 36); // left edge: before "a"
    field(&mut h, &mut st);
    assert!(h.focus.is_focused("f"));
    assert_eq!(st.caret(), 0);
    h.ev(Ev::Down { x: 30, y: 36 });
    field(&mut h, &mut st);
    h.ev(Ev::Move { x: 310, y: 36 });
    field(&mut h, &mut st);
    h.ev(Ev::Up { x: 310, y: 36 });
    field(&mut h, &mut st);
    assert_eq!(st.selected(), "abcdefghij");
}

#[test]
fn text_field_lets_enter_and_escape_through() {
    let mut h = Harness::new();
    let mut st = TextState::new("x");
    h.focus.focus("f");
    h.key(key::RETURN, NONE).key(key::ESCAPE, NONE);
    let (_, out) = h.frame(|ui| ui.place(r(20.0, 20.0, 300.0, 32.0)).text_field("f", &mut st));
    assert!(out.enter && out.escape);
}

#[test]
fn input_combines_surrogates_and_drops_strays() {
    let mut i = Input::default();
    for u in [0xD83D, 0xDE00, 0xDE00, 0xD83D, 0x41] {
        i.feed(&Ev::Char(u));
    }
    assert_eq!(i.keys, vec![KeyIn::Text('😀'), KeyIn::Text('A')]);
}

// ---- focus order ----

fn three(ui: &mut Ui) {
    ui.button("a", "A", false);
    ui.button("b", "B", false);
    let mut on = false;
    ui.toggle("c", &mut on);
}

#[test]
fn tab_order_follows_draw_order_and_wraps() {
    let mut h = Harness::new();
    h.key(key::TAB, NONE);
    let (_, out) = h.frame(three);
    assert!(h.focus.is_focused("a") && out.redraw, "first frame: resolved at finish");
    for want in ["b", "c", "a"] {
        h.key(key::TAB, NONE);
        h.frame(three);
        assert!(h.focus.is_focused(want), "{want}");
    }
    h.key(key::TAB, SHIFT);
    h.frame(three);
    assert!(h.focus.is_focused("c"), "Shift+Tab wraps backwards");
    // The focused control disappears: focus is dropped.
    h.frame(|ui| ui.button("a", "A", false));
    assert!(!h.focus.is_focused("c"));
}

#[test]
fn consumed_keys_tracked_past_64_events() {
    let mut h = Harness::new();
    h.frame(one_button);
    h.focus.focus("ok");
    for _ in 0..70 {
        h.key(0x10, SHIFT); // Shift presses: nobody uses them
    }
    h.key(key::RETURN, NONE);
    let (clicked, out) = h.frame(one_button);
    assert!(clicked && !out.enter, "Enter at index 70 is consumed by the button");
    // A text field with 100 typed chars then Esc: all typed, Esc reported.
    let mut st = TextState::new("");
    h.focus.focus("f");
    field(&mut h, &mut st);
    h.typed(&"x".repeat(100)).key(key::BACK, NONE).key(key::ESCAPE, NONE);
    let (_, out) = h.frame(|ui| ui.place(r(20.0, 20.0, 300.0, 32.0)).text_field("f", &mut st));
    assert_eq!(st.text.len(), 99);
    assert!(out.escape);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "share the id")]
fn duplicate_ids_are_caught() {
    let mut h = Harness::new();
    h.frame(|ui| {
        ui.button("dup", "A", false);
        ui.enabled = false;
        ui.button("dup", "B", false);
    });
}

#[test]
fn escape_and_enter_reported_when_unconsumed() {
    let mut h = Harness::new();
    h.key(key::ESCAPE, NONE);
    assert!(h.frame(three).1.escape);
    h.key(key::RETURN, NONE);
    let out = h.frame(three).1;
    assert!(out.enter && !out.escape);
}

// ---- dropdown ----

const FMT: [&str; 3] = ["PNG", "JPEG", "BMP"];

fn dd(h: &mut Harness, sel: &mut usize) -> (bool, Out) {
    h.frame(|ui| ui.place(r(20.0, 20.0, 160.0, 32.0)).dropdown("fmt", &FMT, sel))
}

#[test]
fn dropdown_mouse_open_select_close() {
    let mut h = Harness::new();
    let mut sel = 0;
    dd(&mut h, &mut sel);
    h.click(60, 36);
    dd(&mut h, &mut sel);
    assert!(h.focus.busy(), "open");
    // List starts 4 px below the box (y 56) with 4 px padding; items 28 tall.
    h.click(60, 56 + 4 + 28 + 14); // second item
    let (changed, _) = dd(&mut h, &mut sel);
    assert!(changed && sel == 1 && !h.focus.busy());
    // Reopen, then a click on the box closes it again.
    h.click(60, 36);
    dd(&mut h, &mut sel);
    assert!(h.focus.busy());
    h.click(60, 36);
    dd(&mut h, &mut sel);
    assert!(!h.focus.busy(), "box toggles");
    // Open, click elsewhere: closes without changing.
    h.click(60, 36);
    dd(&mut h, &mut sel);
    h.click(400, 300);
    let (changed, _) = dd(&mut h, &mut sel);
    assert!(!changed && !h.focus.busy() && sel == 1);
}

#[test]
fn dropdown_keyboard() {
    let mut h = Harness::new();
    let mut sel = 0;
    h.focus.focus("fmt");
    h.key(key::DOWN, NONE);
    assert!(dd(&mut h, &mut sel).0 && sel == 1, "Down while closed steps");
    h.key(key::SPACE, NONE);
    dd(&mut h, &mut sel);
    assert!(h.focus.busy());
    h.key(key::DOWN, NONE).key(key::RETURN, NONE);
    let (changed, out) = dd(&mut h, &mut sel);
    assert!(changed && sel == 2 && !h.focus.busy() && !out.enter);
    h.key(key::RETURN, NONE);
    dd(&mut h, &mut sel);
    h.key(key::UP, NONE).key(key::ESCAPE, NONE);
    let (changed, out) = dd(&mut h, &mut sel);
    assert!(!changed && sel == 2 && !h.focus.busy());
    assert!(!out.escape, "Esc closing the list is not the dialog's Esc");
    h.key(key::ESCAPE, NONE);
    assert!(dd(&mut h, &mut sel).1.escape);
}

#[test]
fn dropdown_highlight_follows_only_a_moving_pointer() {
    let mut h = Harness::new();
    let mut sel = 0;
    h.click(60, 36); // opens; the pointer then rests over the first item
    dd(&mut h, &mut sel);
    h.ev(Ev::Move { x: 60, y: 56 + 4 + 14 });
    dd(&mut h, &mut sel);
    h.key(key::DOWN, NONE);
    dd(&mut h, &mut sel);
    h.key(key::DOWN, NONE);
    dd(&mut h, &mut sel);
    h.key(key::RETURN, NONE);
    let (changed, _) = dd(&mut h, &mut sel);
    assert!(changed && sel == 2, "keyboard highlight kept under a still pointer: {sel}");
}

#[test]
fn dropdown_with_no_items_closes() {
    let mut h = Harness::new();
    let mut sel = 0;
    h.click(60, 36);
    dd(&mut h, &mut sel);
    assert!(h.focus.busy());
    h.ev(Ev::Move { x: 60, y: 70 }).key(key::DOWN, NONE).key(key::END, NONE);
    let (changed, _) = h.frame(|ui| ui.place(r(20.0, 20.0, 160.0, 32.0)).dropdown("fmt", &[], &mut sel));
    assert!(!changed && !h.focus.busy() && sel == 0);
    h.click(60, 36).key(key::RETURN, NONE);
    h.frame(|ui| ui.place(r(20.0, 20.0, 160.0, 32.0)).dropdown("fmt", &[], &mut sel));
    assert!(!h.focus.busy(), "an empty dropdown does not open");
}

#[test]
fn open_list_blocks_controls_beneath() {
    let mut h = Harness::new();
    let mut sel = 0;
    let f = |ui: &mut Ui, sel: &mut usize| {
        ui.place(r(20.0, 20.0, 160.0, 32.0)).dropdown("fmt", &FMT, sel);
        ui.place(r(20.0, 60.0, 160.0, 32.0)).button("under", "Under", false)
    };
    h.frame(|ui| f(ui, &mut sel));
    h.click(60, 36);
    h.frame(|ui| f(ui, &mut sel));
    h.click(60, 70); // first item, over the button
    let (clicked, _) = h.frame(|ui| f(ui, &mut sel));
    assert!(!clicked && sel == 0 && !h.focus.busy());
}

// ---- table ----

fn rows() -> Vec<String> {
    (0..30).map(|i| format!("Action {i}")).collect()
}

fn tbl(h: &mut Harness, st: &mut TableState, names: &[String]) -> bool {
    let cells: Vec<[&str; 2]> = names.iter().map(|n| [n.as_str(), "Ctrl+K"]).collect();
    let rs: Vec<Row> = cells.iter().map(|c| Row { cells: c, error: false }).collect();
    let cols = [Col { title: "Description", frac: 0.6 }, Col { title: "Key", frac: 0.4 }];
    // 28 header + 8 rows of 28 = 252.
    h.frame(|ui| ui.place(r(20.0, 20.0, 300.0, 252.0)).table("tbl", &cols, &rs, st)).0
}

#[test]
fn table_click_selects_and_wheel_scrolls() {
    let mut h = Harness::new();
    let mut st = TableState::default();
    let names = rows();
    tbl(&mut h, &mut st, &names);
    h.click(100, 20 + 28 + 28 + 10); // second body row
    assert!(tbl(&mut h, &mut st, &names));
    assert_eq!(st.selected, Some(1));
    h.ev(Ev::Wheel { delta: -120, x: 100, y: 100 });
    tbl(&mut h, &mut st, &names);
    assert_eq!(st.scroll, 3.0 * 28.0, "one notch = 3 rows");
    h.click(100, 20 + 28 + 10); // first visible row is now row 3
    tbl(&mut h, &mut st, &names);
    assert_eq!(st.selected, Some(3));
    h.ev(Ev::Wheel { delta: 120 * 10, x: 100, y: 100 });
    tbl(&mut h, &mut st, &names);
    assert_eq!(st.scroll, 0.0, "clamped at the top");
    h.ev(Ev::Wheel { delta: -120 * 100, x: 100, y: 100 });
    tbl(&mut h, &mut st, &names);
    assert_eq!(st.scroll, (30.0 - 8.0) * 28.0, "clamped at the bottom");
    assert_eq!(st.row_rect(29).map(|r| r.y1()), Some(272.0));
}

#[test]
fn table_keyboard_moves_and_scrolls_into_view() {
    let mut h = Harness::new();
    let mut st = TableState::default();
    let names = rows();
    h.focus.focus("tbl");
    h.key(key::DOWN, NONE);
    assert!(tbl(&mut h, &mut st, &names));
    assert_eq!(st.selected, Some(0));
    h.key(key::END, NONE);
    tbl(&mut h, &mut st, &names);
    assert_eq!((st.selected, st.scroll), (Some(29), 22.0 * 28.0));
    h.key(key::PAGEUP, NONE);
    tbl(&mut h, &mut st, &names);
    assert_eq!(st.selected, Some(21));
    h.key(key::HOME, NONE);
    tbl(&mut h, &mut st, &names);
    assert_eq!((st.selected, st.scroll), (Some(0), 0.0));
    h.key(key::UP, NONE);
    assert!(!tbl(&mut h, &mut st, &names), "stays at the top");
}

#[test]
fn table_max_scroll_shows_the_last_row_at_fractional_scale() {
    let mut h = Harness::scaled(1.1);
    let mut st = TableState::default();
    let names = rows();
    tbl(&mut h, &mut st, &names);
    h.ev(Ev::Wheel { delta: -120 * 100, x: 100, y: 100 });
    tbl(&mut h, &mut st, &names);
    let (body, _) = st.body.unwrap();
    let last = st.row_rect(29).expect("last row visible");
    assert!((last.y1() - body.y1()).abs() < 0.01, "last row bottom {} vs body bottom {}", last.y1(), body.y1());
    // End scrolls to the same place.
    h.focus.focus("tbl");
    st.scroll = 0.0;
    h.key(key::END, NONE);
    tbl(&mut h, &mut st, &names);
    assert!((st.row_rect(29).unwrap().y1() - body.y1()).abs() < 0.01);
}

#[test]
fn table_wheel_and_click_in_one_frame_hit_the_shown_row() {
    let mut h = Harness::new();
    let mut st = TableState { scroll: (30.0 - 8.0) * 28.0, ..Default::default() };
    let names = rows();
    tbl(&mut h, &mut st, &names);
    // Scrolling further down is clamped before the click is hit-tested.
    h.ev(Ev::Wheel { delta: -120, x: 100, y: 100 }).click(100, 20 + 28 + 10);
    tbl(&mut h, &mut st, &names);
    assert_eq!(st.selected, Some(22), "first visible row at max scroll");
}

/// Mouse interaction at 150 %: list item, table row and caret positions
/// all scale with `k`.
#[test]
fn interaction_at_150_percent() {
    let mut h = Harness::scaled(1.5);
    let mut sel = 0;
    // Dropdown box (20,20,160,32) → physical (30,30,240,48); the list
    // starts 6 px below with 6 px padding; items 42 px.
    dd(&mut h, &mut sel);
    h.click(90, 50);
    dd(&mut h, &mut sel);
    assert!(h.focus.busy());
    h.click(90, 78 + 6 + 6 + 42 + 21);
    assert!(dd(&mut h, &mut sel).0 && sel == 1);
    // Table at (30,30); header and rows 42 px.
    let mut st = TableState::default();
    let names = rows();
    tbl(&mut h, &mut st, &names);
    h.click(150, 30 + 42 + 42 + 21);
    tbl(&mut h, &mut st, &names);
    assert_eq!(st.selected, Some(1));
    // Text field at x 30 with 15 px padding.
    let mut ts = TextState::new("abcdefghij");
    field(&mut h, &mut ts);
    let x = 45.0 + crate::uifb::text_width(&crate::fonts::UI, 13.0 * 1.5, "abc");
    h.click(x.round() as i32, 50);
    field(&mut h, &mut ts);
    assert_eq!(ts.caret(), 3);
}

// ---- slider / tabs ----

#[test]
fn slider_keys_and_drag() {
    let mut h = Harness::new();
    let mut v = 90u8;
    let f = |h: &mut Harness, v: &mut u8| h.frame(|ui| ui.place(r(20.0, 20.0, 240.0, 32.0)).slider("q", v, 1, 100)).0;
    h.focus.focus("q");
    h.key(key::RIGHT, NONE).key(key::PAGEUP, NONE);
    assert!(f(&mut h, &mut v));
    assert_eq!(v, 100, "clamped to max");
    h.key(key::HOME, NONE);
    f(&mut h, &mut v);
    assert_eq!(v, 1);
    h.ev(Ev::Down { x: 20, y: 36 });
    f(&mut h, &mut v);
    h.ev(Ev::Move { x: 400, y: 36 });
    f(&mut h, &mut v);
    assert_eq!(v, 100, "drag follows the pointer past the end");
    h.ev(Ev::Up { x: 400, y: 36 }).ev(Ev::Move { x: 20, y: 36 });
    f(&mut h, &mut v);
    assert_eq!(v, 100, "released: no more dragging");
}

#[test]
fn tabs_click_and_arrows() {
    let mut h = Harness::new();
    let mut sel = 0;
    let labels = ["General", "Saving", "Shortcuts"];
    let f = |h: &mut Harness, sel: &mut usize| h.frame(|ui| ui.tabs("tabs", &labels, sel)).0;
    f(&mut h, &mut sel);
    h.key(key::TAB, NONE);
    f(&mut h, &mut sel);
    h.key(key::RIGHT, NONE).key(key::RIGHT, NONE).key(key::RIGHT, NONE);
    assert!(f(&mut h, &mut sel));
    assert_eq!(sel, 2);
    h.click(20, 20); // first segment
    f(&mut h, &mut sel);
    assert_eq!(sel, 0);
}

// ---- key capture ----

fn kc(h: &mut Harness, c: &mut Option<Chord>) -> bool {
    h.frame(|ui| ui.place(r(20.0, 20.0, 160.0, 32.0)).key_capture("kc", c)).0
}

#[test]
fn key_capture_records_and_backspace_clears() {
    let mut h = Harness::new();
    let mut c = Chord::parse("Ctrl+S");
    kc(&mut h, &mut c);
    h.click(60, 36);
    kc(&mut h, &mut c);
    assert!(h.focus.busy(), "recording");
    h.key(0x11, CTRL); // Ctrl alone waits
    assert!(!kc(&mut h, &mut c));
    h.key(0x4B, Mods { ctrl: true, shift: true, alt: false });
    assert!(kc(&mut h, &mut c));
    assert_eq!(c, Chord::parse("Ctrl+Shift+K"));
    assert!(!h.focus.busy(), "one chord per recording");
    h.key(key::RETURN, NONE);
    kc(&mut h, &mut c);
    h.key(key::BACK, NONE);
    assert!(kc(&mut h, &mut c));
    assert_eq!(c, None);
    // Esc is a bindable key while recording, and not the dialog's Esc.
    h.key(key::SPACE, NONE);
    kc(&mut h, &mut c);
    h.key(key::ESCAPE, NONE);
    let (changed, out) = h.frame(|ui| ui.place(r(20.0, 20.0, 160.0, 32.0)).key_capture("kc", &mut c));
    assert!(changed && !out.escape && c == Chord::parse("Esc"));
}

#[test]
fn key_capture_tab_and_click_away_stop_recording() {
    let mut h = Harness::new();
    let mut c = None;
    let f = |h: &mut Harness, c: &mut Option<Chord>| {
        h.frame(|ui| {
            let ch = ui.place(r(20.0, 20.0, 160.0, 32.0)).key_capture("kc", c);
            ui.place(r(20.0, 80.0, 100.0, 32.0)).button("b", "B", false);
            ch
        })
        .0
    };
    f(&mut h, &mut c);
    h.click(60, 36);
    f(&mut h, &mut c);
    h.key(key::TAB, NONE);
    assert!(!f(&mut h, &mut c));
    assert!(h.focus.is_focused("b") && !h.focus.busy() && c.is_none());
    h.click(60, 36);
    f(&mut h, &mut c);
    h.click(400, 300);
    f(&mut h, &mut c);
    assert!(!h.focus.busy());
}

// ---- drawing ----

#[test]
fn clipped_drawing_stays_inside() {
    let mut img = PixBuf::from_pixel(40, 30, [0, 0, 0, 255]);
    let mut fb = Fb::new(img.as_raw_mut(), 40);
    clipped(&mut fb, r(10.0, 5.0, 10.0, 10.0), |fb| fb.fill_rect(0, 0, 40, 30, crate::uifb::C4::rgb(255, 0, 0)));
    assert_eq!(img.get_pixel(15, 10), [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(9, 10), [0, 0, 0, 255]);
    assert_eq!(img.get_pixel(20, 10), [0, 0, 0, 255]);
    assert_eq!(img.get_pixel(15, 15), [0, 0, 0, 255]);
}

#[test]
fn primary_button_is_accent_filled() {
    let mut h = Harness::new();
    h.frame(|ui| {
        ui.place(r(20.0, 20.0, 100.0, 32.0)).button("p", "Go", true);
        ui.place(r(20.0, 80.0, 100.0, 32.0)).button("s", "Go", false);
    });
    let p = h.img.get_pixel(24, 36);
    assert_eq!([p[0], p[1], p[2]], [0x8B, 0x93, 0xFF]);
    let s = h.img.get_pixel(24, 96);
    assert!(s[2] < 60, "secondary is a faint tint: {s:?}");
}

// ---- gallery ----

/// Every control in its main states (the Settings/update-dialog vocabulary).
fn gallery(ui: &mut Ui) {
    let mut st = Gallery::default();
    gallery_with(ui, &mut st);
}

struct Gallery {
    on: bool,
    off: bool,
    pattern: TextState,
    path: TextState,
    fmt: usize,
    theme: usize,
    q: u8,
    tab: usize,
    table: TableState,
    capture: Option<Chord>,
    empty: Option<Chord>,
    rec: Option<Chord>,
}

impl Default for Gallery {
    fn default() -> Gallery {
        let mut pattern = TextState::new("%F_%H-%M-%S");
        pattern.select(3..8);
        Gallery {
            on: true,
            off: false,
            pattern,
            path: TextState::new(r"C:\Users\denis\Pictures\rustshot\screenshots\daily"),
            fmt: 1,
            theme: 0,
            q: 90,
            tab: 1,
            table: TableState { selected: Some(2), ..Default::default() },
            capture: Chord::parse("Ctrl+Shift+S"),
            empty: None,
            rec: None,
        }
    }
}

fn gallery_with(ui: &mut Ui, g: &mut Gallery) {
    let pad = 24.0;
    let col = (ui.bounds().w - 3.0 * pad) / 2.0;
    let left = r(pad, pad, col, ui.bounds().h - 2.0 * pad);
    let right = r(2.0 * pad + col, pad, col, ui.bounds().h - 2.0 * pad);
    ui.area(left, |ui| {
        ui.heading("Buttons");
        ui.row(|ui| {
            ui.button("update", "Update", true);
            ui.button("skip", "Skip this version", false);
        });
        ui.row(|ui| {
            ui.button("cancel", "Cancel", false);
            ui.enabled = false;
            ui.button("off", "Disabled", false);
            ui.enabled = true;
        });
        ui.heading("Toggles");
        ui.row(|ui| {
            ui.toggle("t1", &mut g.on);
            ui.label("Start at login");
        });
        ui.row(|ui| {
            ui.toggle("t2", &mut g.off);
            ui.label("Check for updates");
        });
        ui.row(|ui| {
            ui.enabled = false;
            let mut on = true;
            ui.toggle("t3", &mut on);
            ui.label("Disabled");
            ui.enabled = true;
        });
        ui.heading("Text fields");
        ui.text_field("pattern", &mut g.pattern);
        ui.text_field("path", &mut g.path);
        ui.row(|ui| {
            ui.dropdown("theme", &["Auto", "Dark", "Light"], &mut g.theme);
            ui.dropdown("fmt", &FMT, &mut g.fmt);
        });
        ui.note("Format of saved screenshots");
    });
    ui.area(right, |ui| {
        ui.tabs("tabs", &["General", "Saving", "Shortcuts"], &mut g.tab);
        ui.heading("JPEG quality");
        ui.slider("q", &mut g.q, 1, 100);
        ui.enabled = false;
        let mut q = 40;
        ui.slider("q2", &mut q, 1, 100);
        ui.enabled = true;
        ui.heading("Progress");
        ui.progress(0.42);
        ui.progress(1.0);
        ui.heading("Shortcuts");
        ui.row(|ui| {
            ui.key_capture("kc1", &mut g.capture);
            ui.key_capture("kc2", &mut g.empty);
        });
        ui.key_capture("kc3", &mut g.rec);
        let names: Vec<[&str; 2]> = vec![
            ["Pencil", "P"],
            ["Line", "D"],
            ["Arrow", "A"],
            ["Rectangle", "R"],
            ["Save", "Ctrl+S"],
            ["Save As", "Ctrl+S"],
            ["Undo", "Ctrl+Z"],
            ["Redo", "Ctrl+Shift+Z, Ctrl+Y"],
            ["Copy", "Ctrl+C"],
            ["Upload", "Ctrl+U"],
        ];
        let rows: Vec<Row> = names.iter().enumerate().map(|(i, c)| Row { cells: c, error: i == 4 || i == 5 }).collect();
        let cols = [Col { title: "Description", frac: 0.55 }, Col { title: "Key", frac: 0.45 }];
        let h = ui.bounds().y1() - ui.cursor_y();
        ui.height(h).table("table", &cols, &rows, &mut g.table);
    });
}

fn shot(name: &str, th: &Theme, k: f32, setup: impl FnOnce(&mut Input, &mut FocusState)) -> PixBuf {
    let (w, h) = (640, 600);
    let mut focus = FocusState::default();
    let mut input = Input::default();
    // Frame 1 learns the focus order; frame 2 is the picture.
    preview::render(w, h, k, th, &input, &mut focus, gallery);
    setup(&mut input, &mut focus);
    let img = preview::render(w, h, k, th, &input, &mut focus, gallery);
    preview::save(name, &img);
    img
}

/// Gallery: focused text field with a selection, open dropdown, a
/// recording shortcut box and a hovered button.
fn gallery_state(k: f32) -> impl FnOnce(&mut Input, &mut FocusState) {
    move |input, focus| {
        focus.focus("pattern");
        focus.open = Some((id_of("fmt"), 2, r(0.0, 0.0, 0.0, 0.0)));
        focus.recording = Some(id_of("kc3"));
        input.mouse = Pt::new(200.0 * k, 72.0 * k);
    }
}

/// Keyboard focus ring on Cancel, pressed primary.
fn states(input: &mut Input, focus: &mut FocusState) {
    focus.focus("cancel");
    focus.active = Some(id_of("update"));
    input.held = true;
    input.mouse = Pt::new(60.0, 72.0);
}

#[test]
fn preview_gallery_pngs() {
    for (name, th) in [("dark", &DARK), ("light", &LIGHT)] {
        let img = shot(&format!("ui-gallery-{name}.png"), th, 1.0, gallery_state(1.0));
        // Not blank: the accent appears (primary button, toggle, slider).
        let accent = th.accent;
        let px = img.as_raw().as_chunks::<4>().0;
        assert!(px.iter().any(|p| p[..3] == [accent.r, accent.g, accent.b]), "{name}");
        shot(&format!("ui-states-{name}.png"), th, 1.0, states);
    }
    shot("ui-gallery-dark-150.png", &DARK, 1.5, gallery_state(1.5));
}

#[cfg(windows)]
fn shell(script: &str) -> std::process::Command {
    let mut c = std::process::Command::new("cmd");
    c.args(["/C", script]);
    c
}

#[cfg(not(windows))]
fn shell(script: &str) -> std::process::Command {
    let mut c = std::process::Command::new("sh");
    c.args(["-c", script]);
    c
}

#[test]
fn output_within_returns_stdout_or_gives_up() {
    use std::time::{Duration, Instant};
    let out = output_within(shell("echo hi"), Duration::from_secs(5)).expect("echo");
    assert_eq!(String::from_utf8_lossy(&out).trim(), "hi");
    assert!(output_within(shell("exit 3"), Duration::from_secs(5)).is_none(), "failure status");
    #[cfg(windows)]
    let slow = shell("ping -n 6 127.0.0.1 >NUL");
    #[cfg(not(windows))]
    let slow = shell("sleep 5");
    let t = Instant::now();
    assert!(output_within(slow, Duration::from_millis(300)).is_none());
    assert!(t.elapsed() < Duration::from_secs(3), "killed at the deadline: {:?}", t.elapsed());
}

#[test]
#[ignore = "touches the live system clipboard"]
fn system_clipboard_roundtrip() {
    let mut c = SystemClipboard;
    let old = c.get();
    c.set("rustshot ui clipboard ✓");
    let got = c.get();
    if let Some(o) = old {
        c.set(&o); // put the user's text back
    }
    assert_eq!(got.as_deref(), Some("rustshot ui clipboard ✓"));
}

// ---- wrapped text ----

#[test]
fn wrap_breaks_at_spaces_and_inside_long_words() {
    let m = |s: &str| s.chars().count() as f32;
    assert_eq!(controls::wrap("the quick brown fox", 9.0, m), ["the quick", "brown fox"]);
    assert_eq!(controls::wrap("a\n\nb c", 9.0, m), ["a", "", "b c"]);
    assert_eq!(controls::wrap("abcdefghijk xy", 4.0, m), ["abcd", "efgh", "ijk", "xy"]);
    assert_eq!(controls::wrap("", 4.0, m), [""]);
    // Even a too-narrow box keeps one character per line.
    assert_eq!(controls::wrap("ab", 0.5, m), ["a", "b"]);
    assert_eq!(controls::wrap("x  y\r\n", 9.0, m), ["x y", ""]);
}

fn notes_text() -> String {
    (1..=12).map(|i| format!("- release note line {i}")).collect::<Vec<_>>().join("\n")
}

#[test]
fn text_view_scrolls_by_wheel_and_keys_and_clamps() {
    let mut h = Harness::new();
    let text = notes_text();
    let mut scroll = 0.0;
    let tv = |h: &mut Harness, s: &mut f32| {
        h.frame(|ui| ui.place(r(20.0, 20.0, 300.0, 100.0)).text_view("notes", &text, s));
    };
    tv(&mut h, &mut scroll);
    h.ev(Ev::Wheel { delta: -120, x: 100, y: 60 });
    tv(&mut h, &mut scroll);
    assert_eq!(scroll, 60.0, "one notch = 3 lines");
    // Wheel outside the box does nothing.
    h.ev(Ev::Wheel { delta: -120, x: 400, y: 300 });
    tv(&mut h, &mut scroll);
    assert_eq!(scroll, 60.0);
    // 12 lines x 20 px in an 84 px viewport: at most 156.
    h.click(100, 60);
    tv(&mut h, &mut scroll);
    h.key(key::END, NONE);
    tv(&mut h, &mut scroll);
    assert_eq!(scroll, 156.0);
    h.key(key::UP, NONE);
    tv(&mut h, &mut scroll);
    assert_eq!(scroll, 136.0);
    h.key(key::HOME, NONE);
    tv(&mut h, &mut scroll);
    assert_eq!(scroll, 0.0);
    h.ev(Ev::Wheel { delta: 600, x: 100, y: 60 });
    tv(&mut h, &mut scroll);
    assert_eq!(scroll, 0.0, "clamped at the top");
}

#[test]
fn paragraph_wraps_to_the_width_and_height() {
    let mut h = Harness::new();
    let (y, _) = h.frame(|ui| {
        ui.area(r(0.0, 0.0, 120.0, 400.0), |ui| {
            ui.paragraph("one two three four five six seven eight nine ten", false);
            ui.cursor_y()
        })
    });
    assert!(y > 2.0 * 20.0, "wrapped onto several lines: {y}");
    // Nothing drawn outside the given height.
    let mut h = Harness::new();
    h.frame(|ui| {
        ui.place(r(0.0, 0.0, 120.0, 20.0)).paragraph("one two three four five six seven eight nine ten", false);
    });
    let w = h.img.width() as usize;
    let below = h.img.as_raw()[25 * w * 4..60 * w * 4].iter().any(|&b| b != 0);
    assert!(!below, "lines past the height are not drawn");
}

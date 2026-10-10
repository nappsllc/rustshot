use super::*;
use crate::theme::{DARK, LIGHT};
use crate::ui::preview;

const NOW: export::Tm = (2026, 10, 10, 14, 30, 5);

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustshot-set-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn custom() -> Config {
    Config {
        save_path: r"D:\Shots".into(),
        save_subfolder: false,
        subfolder_pattern: "%Y/%m".into(),
        filename_pattern: "shot_%H%M".into(),
        save_format: "jpg".into(),
        jpeg_quality: 75,
        save_dialog: true,
        theme: "light".into(),
        renderer: "software".into(),
        check_updates: false,
        capture_hotkey: "Ctrl+Alt+F7".into(),
        quit_hotkey: "Ctrl+Alt+Shift+F8".into(),
        skip_version: "0.1.9".into(),
        shortcuts: [("save".to_string(), "Ctrl+Alt+S".to_string()), ("bogus".to_string(), "Q".to_string())]
            .into_iter()
            .collect(),
        ..Config::default()
    }
}

#[test]
fn form_round_trip_keeps_every_value() {
    for cfg in [Config::default(), custom()] {
        let f = Form::new(&cfg, Some(true));
        assert_eq!(config::to_toml(&f.apply(&cfg, &cfg)), config::to_toml(&cfg));
    }
    // Values the form cannot show survive when left alone.
    let odd = Config {
        theme: "Midnight".into(),
        renderer: "vulkan".into(),
        capture_hotkey: "Hyper+X".into(),
        save_format: "JPEG".into(),
        ..Config::default()
    };
    let f = Form::new(&odd, None);
    assert_eq!((f.theme, f.format, f.capture), (0, 1, None));
    assert_eq!(config::to_toml(&f.apply(&odd, &odd)), config::to_toml(&odd));
}

#[test]
fn form_writes_what_changed() {
    let cfg = Config::default();
    let mut f = Form::new(&cfg, None);
    f.theme = 2;
    f.renderer = 1;
    f.check_updates = false;
    f.capture = Chord::parse("Ctrl+Alt+F9");
    f.quit = None;
    f.folder.set(r"E:\caps");
    f.subfolder = false;
    f.subfolder_pattern.set("%Y");
    f.format = 2;
    f.quality = 50;
    f.ask = true;
    f.filename.set("x_%S");
    f.keys.set(Action::ToolPencil, vec![Chord::parse("Shift+K").unwrap()]);
    f.keys.set(Action::Copy, vec![]);
    let c = f.apply(&cfg, &cfg);
    assert_eq!((c.theme.as_str(), c.renderer.as_str(), c.check_updates), ("light", "software", false));
    assert_eq!((c.capture_hotkey.as_str(), c.quit_hotkey.as_str()), ("Ctrl+Alt+F9", ""));
    assert_eq!((c.save_path.as_str(), c.save_subfolder, c.subfolder_pattern.as_str()), (r"E:\caps", false, "%Y"));
    assert_eq!((c.save_format.as_str(), c.jpeg_quality, c.save_dialog), ("bmp", 50, true));
    assert_eq!(c.filename_pattern, "x_%S");
    assert_eq!(c.shortcuts.len(), 2);
    assert_eq!((c.shortcuts["tool_pencil"].as_str(), c.shortcuts["copy"].as_str()), ("Shift+K", ""));
    // And it reads back as the same form.
    let back = Form::new(&c, None);
    assert_eq!((back.theme, back.renderer, back.format, back.quality), (2, cfg!(windows) as usize, 2, 50));
    assert_eq!(back.keys, f.keys);
}

#[test]
fn untouched_values_follow_the_file() {
    // The file changed while the window was open (Skip this version, an
    // edit by hand): only the user's own change is written over it.
    let loaded = Config::default();
    let mut f = Form::new(&loaded, None);
    f.format = 1;
    let disk = Config { skip_version: "0.2.0".into(), theme: "dark".into(), ..Config::default() };
    let c = f.apply(&loaded, &disk);
    assert_eq!((c.save_format.as_str(), c.skip_version.as_str(), c.theme.as_str()), ("jpg", "0.2.0", "dark"));
    // Unknown shortcut entries stay when the keymap is edited.
    let cfg = custom();
    let mut f = Form::new(&cfg, None);
    f.keys.set(Action::Undo, vec![Chord::parse("F2").unwrap()]);
    let c = f.apply(&cfg, &cfg);
    assert_eq!(c.shortcuts.get("bogus").map(String::as_str), Some("Q"));
    assert_eq!(c.shortcuts["undo"], "F2");
    assert_eq!(c.shortcuts["save"], "Ctrl+Alt+S");
}

#[test]
fn empty_save_path_shows_the_default_folder() {
    let cfg = Config::default();
    let f = Form::new(&cfg, None);
    assert_eq!(f.folder.text, export::default_save_dir(&cfg).display().to_string());
    assert_eq!(f.apply(&cfg, &cfg).save_path, "", "left alone it stays the default");
}

#[test]
fn tokens_insert_at_the_caret() {
    let mut f = Form::new(&Config::default(), None);
    f.filename.set("ab");
    f.filename.select(1..1);
    f.insert(Pattern::File, "%H");
    assert_eq!(f.filename.text, "a%Hb");
    assert_eq!(f.filename.caret(), 3);
    f.insert(Pattern::File, TOKENS[5].1);
    assert_eq!(f.filename.text, "a%H%d-%m-%Yb");
    // A selection is replaced.
    f.filename.select(0..1);
    f.insert(Pattern::File, "%C");
    assert_eq!(f.filename.text, "%C%H%d-%m-%Yb");
    f.subfolder_pattern.set("%F");
    f.insert(Pattern::Sub, "/%V");
    assert_eq!(f.subfolder_pattern.text, "%F/%V");
    // Every token expands to something (none is left as a literal %X).
    for (label, tok) in TOKENS {
        let s = export::format_filename(tok);
        assert!(!s.contains('%') && !s.is_empty(), "{label}: {s}");
    }
}

#[test]
fn preview_is_the_auto_save_path() {
    let cfg = custom();
    let mut f = Form::new(&cfg, None);
    assert_eq!(f.preview(&cfg, NOW), export::auto_save_path(&cfg, NOW));
    f.subfolder = true;
    f.format = 0;
    f.filename.set("%F %H");
    let edited = Config { save_subfolder: true, save_format: "png".into(), filename_pattern: "%F %H".into(), ..cfg.clone() };
    assert_eq!(f.preview(&cfg, NOW), export::auto_save_path(&edited, NOW));
    assert_eq!(f.preview(&cfg, NOW), PathBuf::from(r"D:\Shots").join("2026").join("10").join("2026-10-10 14.png"));
}

#[test]
fn conflicts_are_marked() {
    let mut f = Form::new(&Config::default(), None);
    assert!(f.shortcut_rows().iter().all(|r| !r.2));
    assert_eq!(f.conflict_note(), None);
    f.keys.set(Action::Cancel, vec![Chord::parse("P").unwrap()]);
    let rows = f.shortcut_rows();
    let bad: Vec<&str> = rows.iter().filter(|r| r.2).map(|r| r.0).collect();
    assert_eq!(bad, ["Pencil", "Cancel"]);
    assert_eq!(rows[0].1, "P");
    assert_eq!(f.conflict_note().unwrap(), "P is bound to Pencil and Cancel; Pencil wins.");
    f.keys.set(Action::Undo, vec![Chord::parse("Ctrl+Y").unwrap()]);
    assert!(f.conflict_note().unwrap().ends_with(" 1 more conflict."));
    f.keys.set(Action::Copy, vec![]);
    let rows = f.shortcut_rows();
    assert_eq!(rows.iter().find(|r| r.0 == "Copy").unwrap().1, "None");
    assert_eq!(rows.iter().find(|r| r.0 == "Redo").unwrap().1, "Ctrl+Shift+Z, Ctrl+Y");
    // Hotkey problems.
    f.capture = Chord::parse("Ctrl+Alt+Shift+Q");
    assert_eq!(f.hotkey_note(), Some("Capture and Quit use the same keys."));
    f.capture = Chord::parse("X");
    assert!(f.hotkey_note().unwrap().contains("every app"));
    f.capture = Chord::parse("PrintScreen");
    assert_eq!(f.hotkey_note(), None);
}

fn settings_at(path: &Path) -> Settings {
    let cfg = std::fs::read_to_string(path).ok().and_then(|t| config::parse_config(&t).ok()).unwrap_or_default();
    let mut s = Settings::new(cfg, None, path.to_path_buf());
    s.set_autostart = |_| Err("not in tests".into());
    s
}

use std::path::Path;

#[test]
fn apply_and_ok_save_only_when_dirty() {
    let dir = scratch("apply");
    let p = dir.join("config.toml");
    config::save_at(&p, &Config::default()).unwrap();
    let mut s = settings_at(&p);
    assert!(!s.dirty());
    assert_eq!(s.on(Click::Apply), vec![]);
    s.form.format = 1;
    assert!(s.dirty());
    let acts = s.on(Click::Apply);
    assert!(matches!(&acts[..], [Act::Saved(c)] if c.save_format == "jpg"), "{acts:?}");
    assert!(!s.dirty());
    assert_eq!(config::parse_config(&std::fs::read_to_string(&p).unwrap()).unwrap().save_format, "jpg");
    // OK with nothing changed just closes; with a change it saves and closes.
    assert_eq!(s.on(Click::Ok), vec![Act::Close]);
    s.form.quality = 40;
    let acts = s.on(Click::Enter);
    assert!(matches!(&acts[..], [Act::Saved(c), Act::Close] if c.jpeg_quality == 40), "{acts:?}");
    // Cancel / Esc / the close button discard.
    s.form.quality = 41;
    assert_eq!(s.on(Click::Escape), vec![Act::Close]);
    assert_eq!(config::parse_config(&std::fs::read_to_string(&p).unwrap()).unwrap().jpeg_quality, 40);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn first_save_over_comments_asks_once() {
    let dir = scratch("confirm");
    let p = dir.join("config.toml");
    std::fs::write(&p, "# my settings\ntheme = \"dark\"\nmystery = 1\n").unwrap();
    let mut s = settings_at(&p);
    s.form.ask = true;
    // Asked; Cancel leaves the file alone.
    assert_eq!(s.on(Click::Apply), vec![]);
    assert_eq!(s.modal, Some(Modal::Rewrite { close: false }));
    assert_eq!(s.on(Click::Escape), vec![]);
    assert_eq!(s.modal, None);
    assert!(std::fs::read_to_string(&p).unwrap().starts_with("# my settings"));
    // OK asks too; Save (Enter) writes and closes.
    assert_eq!(s.on(Click::Ok), vec![]);
    assert_eq!(s.modal, Some(Modal::Rewrite { close: true }));
    let acts = s.on(Click::Enter);
    assert!(matches!(&acts[..], [Act::Saved(_), Act::Close]), "{acts:?}");
    let text = std::fs::read_to_string(&p).unwrap();
    let c = config::parse_config(&text).unwrap();
    assert!(c.save_dialog && c.theme == "dark", "values kept");
    assert!(!text.contains("# my settings") && !text.contains("mystery"));
    // Not asked again, even if the file gains a comment meanwhile.
    std::fs::write(&p, format!("# again\n{text}")).unwrap();
    s.form.ask = false;
    assert!(matches!(&s.on(Click::Apply)[..], [Act::Saved(_)]));
    // A file rustshot wrote itself is never asked about.
    let mut s = settings_at(&p);
    s.form.theme = 2;
    assert!(matches!(&s.on(Click::Apply)[..], [Act::Saved(_)]));
    // Neither is a missing file (it is created).
    let q = dir.join("new").join("config.toml");
    let mut s = settings_at(&q);
    s.form.theme = 2;
    assert!(matches!(&s.on(Click::Apply)[..], [Act::Saved(_)]));
    assert_eq!(config::parse_config(&std::fs::read_to_string(&q).unwrap()).unwrap().theme, "light");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_errors_show_in_the_window() {
    let dir = scratch("err");
    // The config path is a directory: the write fails.
    let mut s = settings_at(&dir);
    s.form.theme = 1;
    assert_eq!(s.on(Click::Ok), vec![]);
    assert!(matches!(&s.modal, Some(Modal::Error(e)) if e.starts_with("Could not save")));
    assert!(s.dirty(), "nothing was saved");
    assert_eq!(s.on(Click::Enter), vec![], "Enter dismisses the error");
    assert_eq!(s.modal, None);
    // Start at login failing after the file was saved: the error shows,
    // the window stays.
    let p = dir.join("config.toml");
    let mut s = settings_at(&p);
    s.autostart = Some(false);
    s.form.autostart = Some(true);
    let acts = s.on(Click::Ok);
    assert!(matches!(&acts[..], [Act::Saved(_)]), "{acts:?}");
    assert!(matches!(&s.modal, Some(Modal::Error(e)) if e.contains("Start at login")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restore_clear_and_reset() {
    let mut s = settings_at(&scratch("rc").join("config.toml"));
    s.on(Click::Clear);
    assert_eq!(s.form.filename.text, "");
    s.on(Click::Restore);
    assert_eq!(s.form.filename.text, "%F_%H-%M");
    s.form.keys.set(Action::Save, vec![]);
    s.on(Click::ResetAll);
    assert_eq!(s.form.keys, Keymap::defaults());
    assert_eq!(s.on(Click::Browse), vec![Act::Browse]);
    assert_eq!(s.on(Click::OpenConfig), vec![Act::OpenConfig]);
}

// --- Rendering ---

/// Two frames (focus order, then the picture); returns the image and the
/// click of the second.
fn render(s: &mut Settings, th: &Theme, k: f32, input: &Input, focus: &mut FocusState) -> (PixBuf, Option<Click>) {
    preview::render(W, H, k, th, &Input::default(), focus, |ui| {
        paint(ui, s);
    });
    let mut click = None;
    let img = preview::render(W, H, k, th, input, focus, |ui| click = paint(ui, s));
    (img, click)
}

fn preview_cfg() -> Config {
    Config { save_path: r"C:\Users\denis\Pictures\rustshot".into(), ..Config::default() }
}

fn sample() -> Settings {
    let mut s = Settings::new(preview_cfg(), Some(true), PathBuf::from("config.toml"));
    s.renderer = true;
    s
}

#[test]
fn preview_settings_pngs() {
    for (tname, th) in [("dark", &DARK), ("light", &LIGHT)] {
        for (tab, name) in TABS.iter().enumerate() {
            let mut s = sample();
            s.tab = tab;
            let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut FocusState::default());
            preview::save(&format!("settings-{}-{tname}.png", name.to_lowercase()), &img);
        }
        // Rebinding a shortcut (Text selected, recording).
        let mut s = sample();
        s.tab = 2;
        s.table.selected = Some(6);
        let mut focus = FocusState::default();
        render(&mut s, th, 1.0, &Input::default(), &mut focus);
        focus.record("rebind");
        let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut focus);
        preview::save(&format!("settings-rebind-{tname}.png"), &img);
        // A conflict.
        let mut s = sample();
        s.tab = 2;
        s.form.keys.set(Action::Cancel, vec![Chord::parse("P").unwrap()]);
        s.table.selected = Some(18);
        let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut FocusState::default());
        preview::save(&format!("settings-conflict-{tname}.png"), &img);
        // The rewrite question.
        let mut s = sample();
        s.tab = 1;
        s.modal = Some(Modal::Rewrite { close: true });
        let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut FocusState::default());
        preview::save(&format!("settings-rewrite-{tname}.png"), &img);
        let mut s = sample();
        s.modal = Some(Modal::Error(
            r"Could not save C:\Users\denis\AppData\Roaming\rustshot\config.toml: Access is denied. (os error 5)".into(),
        ));
        let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut FocusState::default());
        preview::save(&format!("settings-error-{tname}.png"), &img);
    }
    // JPEG chosen (quality enabled), a hotkey warning, at 150 %.
    let mut s = sample();
    s.tab = 1;
    s.form.format = 1;
    let (img, _) = render(&mut s, &DARK, 1.5, &Input::default(), &mut FocusState::default());
    preview::save("settings-saving-jpeg-dark-150.png", &img);
    let mut s = sample();
    s.form.capture = Chord::parse("X");
    let (img, _) = render(&mut s, &DARK, 1.5, &Input::default(), &mut FocusState::default());
    preview::save("settings-general-warning-dark-150.png", &img);
}

fn click_at(x: f32, y: f32, k: f32) -> Input {
    let (x, y) = ((x * k) as i32, (y * k) as i32);
    let mut input = Input::default();
    for e in [Ev::Move { x, y }, Ev::Down { x, y }, Ev::Up { x, y }] {
        input.feed(&e);
    }
    input
}

/// The buttons click where they are drawn, at 100 % and 150 %; the
/// rightmost is Apply (disabled until something changes), then Cancel.
#[test]
fn buttons_click_where_drawn() {
    for k in [1.0f32, 1.5] {
        let y = H as f32 - PAD - 16.0;
        let mut s = sample();
        let (_, c) = render(&mut s, &DARK, k, &click_at(W as f32 - PAD - 20.0, y, k), &mut FocusState::default());
        assert_eq!(c, None, "Apply is disabled while nothing changed");
        s.form.ask = true;
        let (_, c) = render(&mut s, &DARK, k, &click_at(W as f32 - PAD - 20.0, y, k), &mut FocusState::default());
        assert_eq!(c, Some(Click::Apply), "k={k}");
        let cancel_x = W as f32 - PAD - button_w("Apply") - GAP - 20.0;
        let (_, c) = render(&mut s, &DARK, k, &click_at(cancel_x, y, k), &mut FocusState::default());
        assert_eq!(c, Some(Click::Cancel), "k={k}");
    }
}

fn button_w(label: &str) -> f32 {
    let mut w = 0.0;
    preview::render(W, H, 1.0, &DARK, &Input::default(), &mut FocusState::default(), |ui| w = ui.button_width(label));
    w
}

/// Clicking a token button inserts it into the pattern field at its caret
/// and gives the field the focus back.
#[test]
fn token_button_click_inserts() {
    let mut s = sample();
    s.tab = 1;
    s.form.filename.set("ab");
    s.form.filename.select(1..1);
    let mut focus = FocusState::default();
    // Hour (00-23) is the 7th button of the first column (rows 22 + 3 px).
    let gy = token_grid_y();
    render(&mut s, &DARK, 1.0, &click_at(PAD + 40.0, gy + 25.0 * 6.0 + 11.0, 1.0), &mut focus);
    assert_eq!(s.form.filename.text, "a%Hb");
    assert!(focus.is_focused("filename"));
}

/// The token grid's top (logical px), laid out as `saving` does it.
fn token_grid_y() -> f32 {
    let mut y = 0.0;
    preview::render(W, H, 1.0, &DARK, &Input::default(), &mut FocusState::default(), |ui| {
        let body = FRect { x: PAD, y: BODY_Y, w: W as f32 - 2.0 * PAD, h: 300.0 };
        ui.area(body, |ui| {
            ui.lay.gap = 6.0;
            for _ in 0..4 {
                ui.row(|ui| ui.width(LABEL_W).label("x"));
            }
            ui.heading("File name");
            ui.space(2.0);
            y = ui.cursor_y();
        });
    });
    y
}

/// A click on a shortcut row starts recording its new keys; the next
/// chord rebinds it.
#[test]
fn row_click_records_then_rebinds() {
    let mut s = sample();
    s.tab = 2;
    let mut focus = FocusState::default();
    render(&mut s, &DARK, 1.0, &Input::default(), &mut focus);
    // Header row is 28 px; the first body row (Pencil) under it.
    render(&mut s, &DARK, 1.0, &click_at(PAD + 40.0, BODY_Y + 28.0 + 14.0, 1.0), &mut focus);
    assert_eq!(s.table.selected, Some(0));
    let mut input = Input::default();
    input.feed(&Ev::Key { vk: 'K' as u32, up: false, repeat: false, mods: crate::wind::Mods { shift: true, ctrl: false, alt: false } });
    render(&mut s, &DARK, 1.0, &input, &mut focus);
    assert_eq!(s.form.keys.chords(Action::ToolPencil), [Chord::parse("Shift+K").unwrap()]);
    // Backspace (while recording) unbinds.
    focus.record("rebind");
    let mut input = Input::default();
    input.feed(&Ev::Key { vk: wind::key::BACK, up: false, repeat: false, mods: Default::default() });
    render(&mut s, &DARK, 1.0, &input, &mut focus);
    assert!(s.form.keys.chords(Action::ToolPencil).is_empty());
}

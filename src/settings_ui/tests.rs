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
    // The renderer choice only exists on Windows; elsewhere the form already
    // starts at "software", so the default spelling is kept.
    let renderer = if cfg!(windows) { "software" } else { "gdi" };
    assert_eq!((c.theme.as_str(), c.renderer.as_str(), c.check_updates), ("light", renderer, false));
    assert_eq!((c.capture_hotkey.as_str(), c.quit_hotkey.as_str()), ("Ctrl+Alt+F9", ""));
    assert_eq!((c.save_path.as_str(), c.save_subfolder, c.subfolder_pattern.as_str()), (r"E:\caps", false, "%Y"));
    assert_eq!((c.save_format.as_str(), c.jpeg_quality, c.save_dialog), ("bmp", 50, true));
    assert_eq!(c.filename_pattern, "x_%S");
    assert_eq!(c.shortcuts.len(), 2);
    assert_eq!((c.shortcuts["tool_pencil"].as_str(), c.shortcuts["copy"].as_str()), ("Shift+K", ""));
    // And it reads back as the same form.
    let back = Form::new(&c, None);
    assert_eq!((back.theme, back.renderer, back.format, back.quality), (2, 1, 2, 50));
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
    let mut s = Settings::open(path.to_path_buf(), None);
    s.set_autostart = |_| Err("not in tests".into());
    s
}

/// After a save the form is rebuilt from what was written: nothing stale
/// or dirty, the folder trimmed.
#[test]
fn apply_rebuilds_the_form() {
    let dir = scratch("rebuild");
    let p = dir.join("config.toml");
    config::save_at(&p, &Config::default()).unwrap();
    let mut s = settings_at(&p);
    s.form.folder.set(r"  E:\caps  ");
    assert!(s.dirty());
    assert!(matches!(&s.on(Click::Apply)[..], [Act::Saved(c)] if c.save_path == r"E:\caps"));
    assert_eq!(s.form.folder.text, r"E:\caps");
    assert!(!s.dirty());
    // Only spaces around it: not a change.
    s.form.folder.set(r" E:\caps ");
    assert!(!s.dirty());
    let _ = std::fs::remove_dir_all(&dir);
}

const BROKEN: &str = "theme = \"dark\"\noops\n";

/// A file that does not parse opens in the error state: nothing can be
/// written over it, only opened, reset or closed.
#[test]
fn broken_file_is_never_written() {
    let dir = scratch("broken");
    let p = dir.join("config.toml");
    std::fs::write(&p, BROKEN).unwrap();
    let mut s = settings_at(&p);
    assert_eq!(s.modal, Some(Modal::Broken("line 2: expected key = value".into())));
    assert_eq!(broken_text("line 2: expected key = value"), "config.toml has an error (line 2: expected key = value). Fix it or reset it.");
    s.form.theme = 2; // as if edited anyway
    for c in [Click::Ok, Click::Apply, Click::Enter, Click::Save, Click::Back, Click::Restore, Click::ResetAll, Click::Replace] {
        assert_eq!(s.on(c), vec![], "{c:?}");
        assert!(s.broken(), "{c:?}");
    }
    assert_eq!(std::fs::read_to_string(&p).unwrap(), BROKEN);
    assert!(!backup_path(&p).exists());
    assert_eq!(s.on(Click::OpenConfig), vec![Act::OpenConfig]);
    assert_eq!(s.on(Click::Close), vec![Act::Close]);
    assert_eq!(s.on(Click::Escape), vec![Act::Close]);
    assert_eq!(s.on(Click::Cancel), vec![Act::Close], "the window's close button");
    // Fixed in an editor: the window reads it again and works.
    std::fs::write(&p, "theme = \"light\"\n").unwrap();
    assert!(s.recheck());
    assert_eq!((s.modal.clone(), s.form.theme, s.stale), (None, 2, false));
    assert!(!s.dirty(), "the form is the fixed file's");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reset to defaults asks again; Replace keeps the old file as
/// config.toml.bak and writes the defaults; Cancel goes back.
#[test]
fn reset_writes_defaults_and_a_backup() {
    let dir = scratch("reset");
    let p = dir.join("config.toml");
    std::fs::write(&p, BROKEN).unwrap();
    let mut s = settings_at(&p);
    assert_eq!(s.on(Click::ResetDefaults), vec![]);
    assert_eq!(s.modal, Some(Modal::Reset("line 2: expected key = value".into())));
    assert_eq!(s.on(Click::Enter), vec![], "no reset by a stray Enter");
    assert_eq!(s.on(Click::Escape), vec![]);
    assert!(matches!(s.modal, Some(Modal::Broken(_))), "Esc goes back to the error");
    s.on(Click::ResetDefaults);
    assert_eq!(s.on(Click::Back), vec![]);
    assert!(matches!(s.modal, Some(Modal::Broken(_))));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), BROKEN);
    s.on(Click::ResetDefaults);
    assert_eq!(backup_path(&p), dir.join("config.toml.bak"));
    let acts = s.on(Click::Replace);
    assert!(matches!(&acts[..], [Act::Saved(c)] if **c == Config::default()), "{acts:?}");
    assert_eq!(s.modal, None);
    assert_eq!(std::fs::read_to_string(dir.join("config.toml.bak")).unwrap(), BROKEN);
    assert_eq!(config::read_at(&p), Ok(Some(Config::default())));
    // The window works on the new file.
    assert!(!s.dirty());
    s.form.ask = true;
    assert!(matches!(&s.on(Click::Apply)[..], [Act::Saved(c)] if c.save_dialog));
    // Broken again and reset again: the first backup is not overwritten.
    std::fs::write(&p, "oops 2\n").unwrap();
    let mut s = settings_at(&p);
    s.on(Click::ResetDefaults);
    assert!(matches!(&s.on(Click::Replace)[..], [Act::Saved(_)]));
    assert_eq!(std::fs::read_to_string(dir.join("config.toml.bak")).unwrap(), BROKEN);
    assert_eq!(std::fs::read_to_string(dir.join("config.toml.bak.1")).unwrap(), "oops 2\n");
    assert_eq!(backup_path(&p), dir.join("config.toml.bak.2"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The file was fixed in an editor while the reset question was open:
/// Replace goes back to the form and writes nothing.
#[test]
fn reset_of_a_fixed_file_writes_nothing() {
    let dir = scratch("reset-fixed");
    let p = dir.join("config.toml");
    std::fs::write(&p, BROKEN).unwrap();
    let mut s = settings_at(&p);
    s.on(Click::ResetDefaults);
    let fixed = "theme = \"light\"\nmystery = 1\n";
    std::fs::write(&p, fixed).unwrap();
    assert_eq!(s.on(Click::Replace), vec![]);
    assert_eq!((s.modal.clone(), s.stale, s.form.theme), (None, false, 2));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), fixed, "not overwritten");
    assert!(!dir.join("config.toml.bak").exists(), "no backup either");
    // Still broken, but differently: the reset goes ahead with the new file.
    std::fs::write(&p, "oops\n").unwrap();
    let mut s = settings_at(&p);
    s.on(Click::ResetDefaults);
    std::fs::write(&p, "still broken\n").unwrap();
    assert!(matches!(&s.on(Click::Replace)[..], [Act::Saved(_)]));
    assert_eq!(std::fs::read_to_string(dir.join("config.toml.bak")).unwrap(), "still broken\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The file breaks while the window is open: Save refuses to write
/// (never merges onto the defaults) and shows the error; the edits stay
/// and save once the file is fixed.
#[test]
fn file_broken_before_save_is_not_written() {
    let dir = scratch("broken-later");
    let p = dir.join("config.toml");
    config::save_at(&p, &Config { theme: "dark".into(), ..Config::default() }).unwrap();
    let mut s = settings_at(&p);
    s.form.format = 1;
    std::fs::write(&p, BROKEN).unwrap();
    assert_eq!(s.on(Click::Ok), vec![]);
    assert!(matches!(&s.modal, Some(Modal::Broken(e)) if e.starts_with("line 2")));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), BROKEN);
    // Broken between the rewrite question and its Save, too.
    std::fs::write(&p, "# hand-edited\ntheme = \"dark\"\n").unwrap();
    assert!(s.recheck());
    assert_eq!(s.form.format, 1, "the edit is kept");
    assert_eq!(s.on(Click::Apply), vec![]);
    assert_eq!(s.modal, Some(Modal::Rewrite { close: false }));
    std::fs::write(&p, BROKEN).unwrap();
    assert_eq!(s.on(Click::Save), vec![]);
    assert!(s.broken());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), BROKEN);
    // Fixed: it saves onto the fixed file.
    std::fs::write(&p, "theme = \"light\"\n").unwrap();
    assert!(s.recheck());
    let acts = s.on(Click::Apply);
    assert!(matches!(&acts[..], [Act::Saved(c)] if c.save_format == "jpg" && c.theme == "light"), "{acts:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A message (a hotkey the daemon could not register) shows at once, or
/// after the open question.
#[test]
fn notes_show_in_the_window() {
    let mut s = sample();
    s.notify("one".into());
    assert_eq!(s.modal, Some(Modal::Error("one".into())));
    s.notify("two".into());
    assert_eq!(s.modal, Some(Modal::Error("one\ntwo".into())));
    s.on(Click::Back);
    s.modal = Some(Modal::Rewrite { close: false });
    s.notify("three".into());
    assert_eq!(s.modal, Some(Modal::Rewrite { close: false }));
    s.on(Click::Back);
    assert_eq!(s.modal, Some(Modal::Error("three".into())));
}

#[test]
fn folder_notes() {
    assert_eq!(folder_note(""), None);
    assert!(folder_note("shots").unwrap().contains("relative"));
    assert!(folder_note(r"shots\x").unwrap().contains("relative"));
    let here = std::env::temp_dir().display().to_string();
    assert_eq!(folder_note(&here), None);
    #[cfg(windows)]
    {
        assert_eq!(
            folder_note(r"\shots").as_deref(),
            Some("A folder without a drive is saved relative to the root of the current drive."),
            "no drive"
        );
        assert_eq!(folder_kind(r"C:\Users\x"), FolderKind::Root(PathBuf::from(r"C:\")));
        assert!(!remote_drive(Path::new(r"C:\")), "the system drive is local");
        // A drive letter that is not there (the last one free, if any).
        if let Some(d) = ('D'..='Z').rev().find(|d| !Path::new(&format!(r"{d}:\")).exists()) {
            assert_eq!(folder_note(&format!(r"{d}:\shots")), Some(format!("Drive {d}: is not on this computer.")));
        }
        assert_eq!(folder_note(r"\\server\share\x"), None, "shares are not probed");
    }
    #[cfg(unix)]
    assert_eq!(folder_note("/no-such-root-rustshot/x"), Some("/no-such-root-rustshot does not exist.".into()));
}

/// The drive is probed once per root: typing within it reads the cache.
#[test]
fn folder_probe_is_cached_by_root() {
    let mut s = sample();
    let here = std::env::temp_dir();
    let FolderKind::Root(root) = folder_kind(&here.display().to_string()) else { panic!("an absolute folder") };
    // A cached answer for the root stands in for the disk.
    s.folder_probe = Some((root.clone(), Some("cached".into())));
    for t in [here.display().to_string(), here.join("a").display().to_string(), here.join("ab").display().to_string()] {
        s.form.folder.set(&t);
        assert_eq!(s.folder_note().as_deref(), Some("cached"), "{t}");
    }
    // A relative folder needs no probe and leaves the cache alone.
    s.form.folder.set("shots");
    assert!(s.folder_note().unwrap().contains("relative"));
    assert_eq!(s.folder_probe.as_ref().map(|p| &p.0), Some(&root));
}

#[test]
fn conflict_target_prefers_the_edited_row() {
    let bad = [false, true, false, true];
    assert_eq!(conflict_target(&bad, None), Some(1));
    assert_eq!(conflict_target(&bad, Some(3)), Some(3), "the edited row is in the conflict");
    assert_eq!(conflict_target(&bad, Some(2)), Some(1), "it is not: the first conflict");
    assert_eq!(conflict_target(&[false; 4], Some(1)), None);
    assert_eq!(conflict_target(&bad, Some(9)), Some(1));
}

/// A hotkey the daemon could not register: a note next to its box, and
/// Apply / OK ask the daemon to try again though nothing changed.
#[test]
fn unregistered_hotkey_can_be_retried() {
    use crate::hotkey::{Cause, Failure};
    let dir = scratch("retry");
    let p = dir.join("config.toml");
    config::save_at(&p, &Config::default()).unwrap();
    let mut s = settings_at(&p);
    assert!(!s.retry_hotkeys());
    assert_eq!((s.hotkey_slot_note(0), s.hotkey_slot_note(1)), (None, None));
    assert_eq!(s.on(Click::Apply), vec![]);
    s.hotkey_failures = vec![
        Failure { slot: 0, wanted: "Ctrl+Alt+F9".into(), cause: Cause::InUse, kept: Some("Ctrl+Alt+Shift+Q".into()) },
        Failure { slot: 1, wanted: "Ctrl+Alt+F10".into(), cause: Cause::Unknown, kept: None },
    ];
    assert!(s.retry_hotkeys() && !s.dirty());
    assert_eq!(s.hotkey_slot_note(0).as_deref(), Some("Not registered — using Ctrl+Alt+Shift+Q"));
    assert_eq!(s.hotkey_slot_note(1).as_deref(), Some("Not registered"));
    assert_eq!(s.on(Click::Apply), vec![Act::Reload]);
    assert_eq!(s.on(Click::Ok), vec![Act::Reload, Act::Close]);
    assert_eq!(s.on(Click::Enter), vec![Act::Reload, Act::Close]);
    assert_eq!(s.on(Click::Cancel), vec![Act::Close], "Cancel never retries");
    // With a change it saves (the save makes the daemon reload anyway).
    s.form.ask = true;
    assert!(matches!(&s.on(Click::Apply)[..], [Act::Saved(_)]));
    // Apply is enabled for the retry: a click on it lands.
    let mut s = sample();
    s.hotkey_failures = vec![Failure { slot: 0, wanted: "Ctrl+Alt+F9".into(), cause: Cause::InUse, kept: None }];
    let (_, c) = render(&mut s, &DARK, 1.0, &click_at(W as f32 - PAD - 20.0, H as f32 - PAD - 16.0, 1.0), &mut FocusState::default());
    assert_eq!(c, Some(Click::Apply));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A conflict scrolls its first row into view.
#[test]
fn conflict_scrolls_into_view() {
    let mut s = sample();
    s.tab = 2;
    let mut focus = FocusState::default();
    render(&mut s, &DARK, 1.0, &Input::default(), &mut focus);
    assert_eq!(s.table.scroll, 0.0);
    s.form.keys.set(Action::Accept, vec![Chord::parse("Esc").unwrap()]);
    render(&mut s, &DARK, 1.0, &Input::default(), &mut focus);
    let r = s.table.row_rect(17).unwrap_or_else(|| panic!("Accept is visible: {:?} {:?}", s.table, s.conflict_row));
    assert!(r.h > 0.0 && s.table.scroll > 0.0);
    // Editing a conflicting row far down (the other one is near the top):
    // the edited row is the one revealed.
    let mut s = sample();
    s.tab = 2;
    render(&mut s, &DARK, 1.0, &Input::default(), &mut focus);
    let last = Action::ALL.len() - 1;
    s.table.selected = Some(last);
    s.form.keys.set(Action::ALL[last], vec![Chord::parse("P").unwrap()]); // Pencil's
    render(&mut s, &DARK, 1.0, &Input::default(), &mut focus);
    assert_eq!(s.conflict_row, Some(last));
    assert!(s.table.row_rect(last).is_some_and(|r| r.h > 0.0), "{:?}", s.table);
}

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
    // The config's folder cannot be created (a file is in the way).
    std::fs::write(dir.join("blocker"), "").unwrap();
    let mut s = settings_at(&dir.join("blocker").join("sub").join("config.toml"));
    assert_eq!(s.modal, None, "no file there yet");
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
    // An absolute path on every platform, so no folder note shifts the layout.
    let save_path = if cfg!(windows) { r"C:\Users\denis\Pictures\rustshot" } else { "/home/denis/Pictures/rustshot" };
    Config { save_path: save_path.into(), ..Config::default() }
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
        // A conflict (Accept → Esc, also Cancel's): scrolled into view.
        let mut s = sample();
        s.tab = 2;
        s.form.keys.set(Action::Accept, vec![Chord::parse("Esc").unwrap()]);
        s.table.selected = Some(17);
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
        // config.toml does not parse, and the reset question.
        let mut s = sample();
        s.stale = true;
        s.modal = Some(Modal::Broken("line 7: invalid integer for 'jpeg_quality': high".into()));
        let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut FocusState::default());
        preview::save(&format!("settings-invalid-{tname}.png"), &img);
        s.modal = Some(Modal::Reset(String::new()));
        let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut FocusState::default());
        preview::save(&format!("settings-reset-{tname}.png"), &img);
        // A hotkey the daemon could not register.
        let mut s = sample();
        s.notify("Couldn't register Ctrl+Alt+F9 — it's in use by another app; kept Shift+Win+X.".into());
        let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut FocusState::default());
        preview::save(&format!("settings-hotkey-failed-{tname}.png"), &img);
        // Hotkeys the daemon could not register: notes by their boxes.
        let mut s = sample();
        s.hotkey_failures = vec![
            crate::hotkey::Failure {
                slot: 0,
                wanted: "Ctrl+Alt+F9".into(),
                cause: crate::hotkey::Cause::InUse,
                kept: Some("Ctrl+Alt+Shift+F11".into()),
            },
            crate::hotkey::Failure { slot: 1, wanted: "Ctrl+Alt+F10".into(), cause: crate::hotkey::Cause::Unknown, kept: None },
        ];
        let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut FocusState::default());
        preview::save(&format!("settings-hotkey-unregistered-{tname}.png"), &img);
        // A folder note (a drive that is not there).
        let mut s = sample();
        s.tab = 1;
        s.folder_probe = Some((PathBuf::from(r"C:\"), Some("Drive Q: is not on this computer.".into())));
        let (img, _) = render(&mut s, th, 1.0, &Input::default(), &mut FocusState::default());
        preview::save(&format!("settings-folder-note-{tname}.png"), &img);
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
    let mut s = sample();
    s.tab = 2;
    s.form.keys.set(Action::Accept, vec![Chord::parse("Esc").unwrap()]);
    s.table.selected = Some(17);
    let mut focus = FocusState::default();
    render(&mut s, &LIGHT, 1.5, &Input::default(), &mut focus);
    focus.record("rebind");
    let (img, _) = render(&mut s, &LIGHT, 1.5, &Input::default(), &mut focus);
    preview::save("settings-conflict-rebind-light-150.png", &img);
    let mut s = sample();
    s.stale = true;
    s.modal = Some(Modal::Broken("line 2: expected key = value".into()));
    let (img, _) = render(&mut s, &DARK, 1.5, &Input::default(), &mut FocusState::default());
    preview::save("settings-invalid-dark-150.png", &img);
    let mut s = sample();
    s.form.capture = Chord::parse("X");
    s.hotkey_failures =
        vec![crate::hotkey::Failure { slot: 1, wanted: "Ctrl+Alt+F10".into(), cause: crate::hotkey::Cause::InUse, kept: Some("Ctrl+Alt+Shift+Q".into()) }];
    let (img, _) = render(&mut s, &DARK, 1.5, &Input::default(), &mut FocusState::default());
    preview::save("settings-hotkey-unregistered-dark-150.png", &img);
    // The most it can need: both notes under their boxes and the warning.
    let long = |slot, kept: &str| crate::hotkey::Failure {
        slot,
        wanted: "Ctrl+Alt+F10".into(),
        cause: crate::hotkey::Cause::InUse,
        kept: Some(kept.into()),
    };
    let mut s = sample();
    s.form.capture = Chord::parse("X");
    s.hotkey_failures = vec![long(0, "Ctrl+Alt+Shift+Meta+F11"), long(1, "Ctrl+Alt+Shift+Meta+F12")];
    let (img, _) = render(&mut s, &LIGHT, 1.0, &Input::default(), &mut FocusState::default());
    preview::save("settings-hotkey-unregistered-worst-light.png", &img);
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
    // Hour (00-23) is the 2nd button of the second column.
    let (gy, col_w) = token_grid_y();
    let x = PAD + col_w + TOKEN_GAP + 40.0;
    render(&mut s, &DARK, 1.0, &click_at(x, gy + TOKEN_H + TOKEN_GAP + TOKEN_H / 2.0, 1.0), &mut focus);
    assert_eq!(s.form.filename.text, "a%Hb");
    assert!(focus.is_focused("filename"));
}

/// The token grid's top and column width (logical px), laid out as
/// `saving` does it.
fn token_grid_y() -> (f32, f32) {
    let (mut y, mut w) = (0.0, 0.0);
    preview::render(W, H, 1.0, &DARK, &Input::default(), &mut FocusState::default(), |ui| {
        let body = FRect { x: PAD, y: BODY_Y, w: W as f32 - 2.0 * PAD, h: 300.0 };
        ui.area(body, |ui| {
            ui.lay.gap = 6.0;
            for _ in 0..4 {
                ui.row(|ui| ui.width(LABEL_W).label("x"));
            }
            ui.heading("File name");
            y = ui.cursor_y();
            w = token_col_w(ui);
        });
    });
    (y, w)
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

/// Smoke (needs a display; CI runs it under `xvfb-run` on Linux): the real
/// Settings window opens, draws and closes after about a second.
/// `cargo test window_smoke -- --ignored --test-threads=1`
#[cfg(not(target_os = "macos"))] // macOS windows need the main thread
#[test]
#[ignore = "opens a window"]
fn settings_window_smoke() {
    if !crate::wind::has_display() {
        eprintln!("no display; skipped");
        return;
    }
    let _guard = crate::wind::test_window_lock();
    let dir = scratch("smoke");
    let s = Settings::new(Config::default(), None, dir.join("config.toml"));
    let (_tx, rx) = std::sync::mpsc::channel();
    let mut w = Window::new(s, crate::theme::resolve(&Config::default()), rx);
    let mut d = crate::wind::AutoClose::new(&mut w, 1000);
    let spec = WindowSpec { title: "Rustshot Settings".into(), w: W, h: H, resizable: false, min: (W, H) };
    wind::run_window(spec, &mut d).expect("settings window");
    assert!(d.frames > 0, "drew a frame");
    let _ = std::fs::remove_dir_all(&dir);
}

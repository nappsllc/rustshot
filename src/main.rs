#![cfg_attr(windows, windows_subsystem = "windows")]

mod actions;
mod anim;
mod autostart;
mod capture;
mod config;
mod editor;
mod export;
mod fonts;
mod hotkey;
mod icon_path;
mod instance;
mod keymap;
mod objects;
mod pixbuf;
#[cfg(windows)]
mod proc_win;
mod raster;
// Screen recording pipeline; the platform capture and the UI that start it
// come in later tasks, so nothing outside its tests calls it yet.
#[allow(dead_code)]
mod rec;
mod settings_ui;
mod sha256;
mod text;
mod theme;
mod tray;
#[cfg(windows)]
mod tray_win;
mod ui;
mod uifb;
mod update;
mod update_install;
mod update_ui;
mod wind;

use anyhow::{anyhow, Result};
use capture::Shot;
use config::Config;
use editor::{Pending, RunKind, UploadSlot};
use export::Task;
use std::path::PathBuf;
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

enum Cmd {
    /// Interactive capture: select a region, annotate, then export.
    Gui(CaptureArgs),
    /// Capture the whole desktop directly (add --edit for the editor).
    Full(CaptureArgs),
    /// Capture a single monitor (0 = first) directly (add --edit for the editor).
    Screen {
        number: u32,
        args: CaptureArgs,
    },
    /// Run in the background and wait for the global capture hotkey.
    /// `after_update`: started by an in-app update (hidden `--after-update`):
    /// never signal a still-running old daemon.
    Daemon { after_update: bool },
    /// Check for a newer release and open its download page.
    Update,
    /// Open the Settings window (in the running daemon, if any).
    Settings,
    /// Show or validate the config file.
    Config {
        /// Validate the config file and exit non-zero on errors.
        check: bool,
    },
}

#[derive(Clone, Default)]
struct CaptureArgs {
    /// -p, --path: save to this file or directory.
    path: Option<PathBuf>,
    /// -c, --clip: copy the capture to the clipboard.
    clip: bool,
    /// --raw: write raw PNG bytes to stdout.
    raw: bool,
    /// --geometry: print the capture geometry as WxH+X+Y to stdout.
    geometry: bool,
    /// --upload: upload to Imgur and print the URL.
    upload: bool,
    /// -d, --delay: delay before capturing, in milliseconds.
    delay: u32,
    /// -f, --filename: filename pattern override for saving (e.g. "%F_shot").
    filename: Option<String>,
    /// --region: WxH+X+Y (virtual-screen coords), "all", or "screenN".
    region: Option<String>,
    /// --noedit: do the capture without showing the editor.
    noedit: bool,
    /// --edit: show the editor (default for gui, opt-in for full/screen).
    edit: bool,
}

enum Parsed {
    /// None = no subcommand given (run the background daemon).
    Cmd(Option<Cmd>),
    Help(String),
}

const HELP: &str = "\
Rustshot: screenshot and annotation tool (Flameshot-style, written in Rust)

Usage: rustshot [COMMAND] [OPTIONS]

Commands:
  gui       Interactive capture: select a region, annotate, then export
  full      Capture the whole desktop directly (add --edit for the editor)
  screen    Capture a single monitor (0 = first) directly (add --edit for the editor)
  daemon    Run in the background and wait for the global capture hotkey (default;
            launching again while it runs triggers a capture)
  update    Check for a newer release and open its download page
  settings  Open the Settings window
  config    Show or validate the config file (--check)
  help      Print this help

Options (gui/full/screen):
  -p, --path <PATH>      Save to this file (.png, .jpg or .bmp) or directory
  -c, --clip             Copy the capture to the clipboard
      --raw              Write raw PNG bytes to stdout
      --geometry         Print the capture geometry as WxH+X+Y to stdout
      --upload           Upload to Imgur and print the URL
  -d, --delay <MS>       Delay before capturing, in milliseconds (default 0)
  -f, --filename <PAT>   Filename pattern override (e.g. \"%F_shot\")
      --region <SPEC>    Region: WxH+X+Y (virtual-screen coords), \"all\", or \"screenN\"
      --noedit           Capture without showing the editor (gui)
      --edit             Show the editor (full/screen)
  -n, --number <N>       Monitor index for screen (0-based, default 0)
      --check            (config) Validate the config file and exit non-zero on errors

Other:
  -h, --help             Print this help
  -V, --version          Print version
";

/// `argv` without the program name: non-UTF-8 arguments are an error
/// (instead of the panic `std::env::args` would raise).
fn parse_args(argv: &[std::ffi::OsString]) -> Result<Parsed, String> {
    let args = argv
        .iter()
        .map(|a| a.to_str().map(str::to_owned).ok_or_else(|| format!("invalid UTF-8 in argument {a:?}")))
        .collect::<Result<Vec<_>, _>>()?;
    parse_from(&args)
}

/// Hand-rolled argument parser (replaces clap for binary size).
fn parse_from(args: &[String]) -> Result<Parsed, String> {
    if args.iter().any(|a| a == "-h" || a == "--help")
        || args.first().map(String::as_str) == Some("help")
    {
        return Ok(Parsed::Help(HELP.to_string()));
    }
    if let Some(v) = args.first()
        && (v == "-V" || v == "--version")
    {
        return Ok(Parsed::Help(format!(
            "Rustshot {}\n",
            env!("CARGO_PKG_VERSION")
        )));
    }

    let (name, rest) = match args.first() {
        Some(a) if !a.starts_with('-') => (Some(a.as_str()), &args[1..]),
        Some(a) => return Err(format!("unexpected argument '{a}'")),
        None => (None, args),
    };

    let cmd = match name {
        None => None,
        Some("gui") => Some(Cmd::Gui(parse_capture(rest)?)),
        Some("full") => Some(Cmd::Full(parse_capture(rest)?)),
        Some("screen") => {
            let (number, capture) = parse_screen(rest)?;
            Some(Cmd::Screen { number, args: capture })
        }
        Some("daemon") => {
            let after_update = rest.first().map(String::as_str) == Some(update_install::AFTER_UPDATE_FLAG);
            if let Some(a) = rest.get(after_update as usize) {
                return Err(format!("unexpected argument '{a}'"));
            }
            Some(Cmd::Daemon { after_update })
        }
        Some("update") => {
            if let Some(a) = rest.first() {
                return Err(format!("unexpected argument '{a}'"));
            }
            Some(Cmd::Update)
        }
        Some("settings") => {
            if let Some(a) = rest.first() {
                return Err(format!("unexpected argument '{a}'"));
            }
            Some(Cmd::Settings)
        }
        Some("config") => {
            let mut check = false;
            for a in rest {
                if a == "--check" {
                    check = true;
                } else {
                    return Err(format!("unexpected argument '{a}'"));
                }
            }
            Some(Cmd::Config { check })
        }
        Some(other) => return Err(format!("unrecognized command '{other}'")),
    };
    Ok(Parsed::Cmd(cmd))
}

/// Read the value for a flag: inline (`--flag=v`, `-pv`) or the next argument.
fn opt_value(
    args: &[String],
    i: &mut usize,
    attached: Option<&str>,
    flag: &str,
) -> Result<String, String> {
    if let Some(v) = attached {
        let v = v.strip_prefix('=').unwrap_or(v);
        if !v.is_empty() {
            return Ok(v.to_string());
        }
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn parse_capture(args: &[String]) -> Result<CaptureArgs, String> {
    let mut out = CaptureArgs::default();
    let mut i = 0;
    while i < args.len() {
        let tok = &args[i];
        // Long flags, with optional =value.
        if tok.starts_with("--") {
            let (name, attached) = match tok.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (tok.as_str(), None),
            };
            match name {
                "--path" => {
                    out.path = Some(PathBuf::from(opt_value(args, &mut i, attached, name)?))
                }
                "--clip" | "--raw" | "--geometry" | "--upload" | "--noedit" | "--edit" => {
                    if attached.is_some() {
                        return Err(format!("unexpected value for {name}"));
                    }
                    match name {
                        "--clip" => out.clip = true,
                        "--raw" => out.raw = true,
                        "--geometry" => out.geometry = true,
                        "--upload" => out.upload = true,
                        "--noedit" => out.noedit = true,
                        _ => out.edit = true,
                    }
                }
                "--delay" => {
                    let v = opt_value(args, &mut i, attached, name)?;
                    out.delay = v
                        .parse()
                        .map_err(|_| format!("invalid value for --delay: '{v}'"))?;
                }
                "--filename" => out.filename = Some(opt_value(args, &mut i, attached, name)?),
                "--region" => out.region = Some(opt_value(args, &mut i, attached, name)?),
                other => return Err(format!("unexpected argument '{other}'")),
            }
            i += 1;
            continue;
        }
        // Short flags, possibly clustered (-cf) with inline values (-d500).
        if tok.starts_with('-') && tok.len() > 1 {
            let chars: Vec<char> = tok[1..].chars().collect();
            let mut ci = 0;
            while ci < chars.len() {
                match chars[ci] {
                    'c' => out.clip = true,
                    'p' | 'd' | 'f' => {
                        let inline: String = chars[ci + 1..].iter().collect();
                        let inline = (!inline.is_empty()).then_some(inline.as_str());
                        let v = opt_value(args, &mut i, inline, &format!("-{}", chars[ci]))?;
                        match chars[ci] {
                            'p' => out.path = Some(PathBuf::from(v)),
                            'd' => {
                                out.delay = v
                                    .parse()
                                    .map_err(|_| format!("invalid value for --delay: '{v}'"))?
                            }
                            _ => out.filename = Some(v),
                        }
                        ci = chars.len();
                    }
                    other => return Err(format!("unexpected argument '-{other}'")),
                }
                ci += 1;
            }
            i += 1;
            continue;
        }
        return Err(format!("unexpected argument '{tok}'"));
    }
    Ok(out)
}

fn parse_screen(args: &[String]) -> Result<(u32, CaptureArgs), String> {
    let mut number = 0u32;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-n" || a == "--number" {
            i += 1;
            let v = args
                .get(i)
                .ok_or_else(|| format!("missing value for {a}"))?;
            number = v
                .parse()
                .map_err(|_| format!("invalid value for --number: '{v}'"))?;
        } else if let Some(v) = a.strip_prefix("--number=") {
            number = v
                .parse()
                .map_err(|_| format!("invalid value for --number: '{v}'"))?;
        } else if let Some(v) = a.strip_prefix("-n").filter(|v| !v.is_empty()) {
            number = v
                .parse()
                .map_err(|_| format!("invalid value for -n: '{v}'"))?;
        } else {
            rest.push(a.clone());
        }
        i += 1;
    }
    Ok((number, parse_capture(&rest)?))
}

fn tasks_from(args: &CaptureArgs) -> Vec<Task> {
    let mut t = Vec::new();
    if let Some(p) = &args.path {
        t.push(Task::Save { path: Some(p.clone()), ask: false });
    }
    if args.clip {
        t.push(Task::Copy);
    }
    if args.raw {
        t.push(Task::Raw);
    }
    if args.geometry {
        t.push(Task::Geometry);
    }
    if args.upload {
        t.push(Task::Upload);
    }
    t
}

fn parse_region(spec: &str) -> Result<(i32, i32, u32, u32)> {
    capture::region_of(spec)
}

/// What a capture command does, decided before anything touches the screen.
enum Plan {
    /// Open the editor on this capture.
    Editor(Pending),
    /// Grab and export without the editor: one monitor (`screen`), else
    /// the desktop cropped to `region`.
    Direct {
        screen: Option<u32>,
        region: Option<(i32, i32, u32, u32)>,
    },
}

/// The plan for `gui`/`full`/`screen` (`None` for the other commands):
/// `gui` opens the editor unless `--noedit`, `full`/`screen` capture
/// directly unless `--edit`. A bad `--region` is an error here.
fn plan_capture(cmd: &Cmd) -> Result<Option<Plan>> {
    let (args, screen, edit) = match cmd {
        Cmd::Gui(a) => (a, None, !a.noedit),
        Cmd::Full(a) => (a, None, a.edit),
        Cmd::Screen { number, args } => (args, Some(*number), args.edit),
        _ => return Ok(None),
    };
    // `screen` captures the whole monitor: no region.
    let region = match (&args.region, screen) {
        (Some(spec), None) => Some(parse_region(spec).map_err(|e| anyhow!("{e:#}"))?),
        _ => None,
    };
    if !edit {
        return Ok(Some(Plan::Direct { screen, region }));
    }
    let mut pending = Pending::editor();
    pending.tasks = tasks_from(args);
    pending.screen = screen;
    pending.region = region;
    pending.filename = args.filename.clone();
    Ok(Some(Plan::Editor(pending)))
}

/// The delay before a capture command grabs (0 for the others).
fn delay_of(cmd: &Cmd) -> u32 {
    match cmd {
        Cmd::Gui(a) | Cmd::Full(a) | Cmd::Screen { args: a, .. } => a.delay,
        _ => 0,
    }
}

/// Direct (no UI) capture: grab, optionally crop, run tasks, wait for upload.
fn run_direct(cfg: &Config, args: &CaptureArgs, shot: Shot, region: Option<(i32, i32, u32, u32)>) -> i32 {
    let mut tasks = tasks_from(args);
    if tasks.is_empty() {
        tasks.push(Task::Save { path: None, ask: false });
    }
    let (img, global) = match region {
        Some(r) => match capture::crop_global(&shot, r) {
            Ok(im) => (im, (r.0, r.1)),
            Err(e) => {
                eprintln!("error: {e:#}");
                return 1;
            }
        },
        None => (shot.image.clone(), shot.origin),
    };
    let res = export::run_export(&img, global, &tasks, cfg);
    let raw_stdout = tasks.iter().any(|t| matches!(t, Task::Raw));
    for m in &res.messages {
        if raw_stdout {
            eprintln!("{m}");
        } else {
            println!("{m}");
        }
    }
    let mut code = if res.error { 1 } else { 0 };
    if let Some(rx) = res.upload
        && export::wait_upload(rx, cfg.copy_url_after_upload).is_none() {
            code = 1;
        }
    code
}

fn delayed(ms: u32) {
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms as u64));
    }
}

fn editor_main(cfg: Config, kind: RunKind, pending: Pending) -> i32 {
    let exit_code = Arc::new(AtomicI32::new(0));
    let slot: UploadSlot = Arc::new(Mutex::new(None));
    let mut code = editor::run(cfg.clone(), kind, Some(pending), exit_code, slot.clone(), None);
    if let Some(rx) = slot.lock().unwrap().take()
        && export::wait_upload(rx, cfg.copy_url_after_upload).is_none() {
            code = 1;
        }
    code
}

/// GUI-subsystem builds start without a console; attach to the parent's so CLI
/// output shows in a terminal. Redirected (valid) std handles are kept as-is.
#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetStdHandle,
    };
    use windows::core::w;
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        for std in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let cur = GetStdHandle(std).unwrap_or_default();
            if (cur.is_invalid() || cur == INVALID_HANDLE_VALUE)
                && let Ok(h) = CreateFileW(
                    w!("CONOUT$"),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            {
                let _ = SetStdHandle(std, h);
            }
        }
    }
}

fn main() {
    #[cfg(windows)]
    attach_parent_console();
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// End the process; first, with `RUSTSHOT_MEMLOG=<path>` set (Windows),
/// append this process' private and peak commit to that file (one line:
/// `pid=.. phase=exit private=.. peak_private=..`, bytes) for memory
/// measurements. The editor also logs `phase=overlay` just before the
/// export crop.
fn exit(code: i32) -> ! {
    #[cfg(windows)]
    memlog("exit");
    std::process::exit(code)
}

/// One `RUSTSHOT_MEMLOG` line tagged `phase` (see [`exit`]).
#[cfg(windows)]
pub fn memlog(phase: &str) {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    let Some(path) = std::env::var_os("RUSTSHOT_MEMLOG").filter(|p| !p.is_empty()) else { return };
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut c as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            c.cb,
        )
    };
    if ok.is_err() {
        return;
    }
    let line = format!(
        "pid={} phase={phase} private={} peak_private={} peak_working_set={}\n",
        std::process::id(),
        c.PrivateUsage,
        c.PeakPagefileUsage,
        c.PeakWorkingSetSize
    );
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn run() -> Result<()> {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let cmd = match parse_args(&argv) {
        Ok(Parsed::Cmd(c)) => c.unwrap_or(Cmd::Daemon { after_update: false }),
        Ok(Parsed::Help(h)) => {
            print!("{h}");
            return Ok(());
        }
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("\nUsage: rustshot [COMMAND] --help");
            exit(2);
        }
    };
    capture::enable_dpi_awareness();

    match cmd {
        Cmd::Config { check } => {
            if check {
                config::check().map_err(|e| anyhow!("{e:#}"))?;
            } else {
                println!("{}", config::config_path().display());
                let cfg = config::load();
                println!("save_path = {:?}", cfg.save_path);
                println!("filename_pattern = {:?}", cfg.filename_pattern);
                println!("capture_hotkey = {:?}", cfg.capture_hotkey);
                println!("quit_hotkey = {:?}", cfg.quit_hotkey);
            }
            Ok(())
        }
        Cmd::Settings => {
            // A running daemon opens it (and reloads after a save);
            // otherwise the window runs here.
            if !instance::signal_settings() {
                settings_ui::run_here();
            }
            Ok(())
        }
        Cmd::Update => {
            if let Some(channel) = update::update_channel() {
                println!("Rustshot is managed by {channel}; update it there.");
                return Ok(());
            }
            match update::check_now() {
                Ok(Some(r)) => {
                    println!("Rustshot {} is available: {}", r.version, r.url);
                    update::open_url(&r.url);
                    Ok(())
                }
                Ok(None) => {
                    println!("Rustshot {} is up to date", env!("CARGO_PKG_VERSION"));
                    Ok(())
                }
                Err(e) => {
                    eprintln!("error: update check failed: {e}");
                    exit(1);
                }
            }
        }
        Cmd::Daemon { after_update } => {
            // `_solo` keeps a Solo daemon's lock file locked until the process ends.
            let mut _solo = None;
            // After a portable update: let the old daemon exit first (bounded).
            let waited = update_install::wait_for_previous();
            // An update relaunch never signals the old daemon (that would
            // start a capture in it): it retries, then gives up quietly.
            let inst = match update_install::start_daemon(waited || after_update, instance::try_acquire) {
                update_install::Start::Run(i) => i,
                update_install::Start::AcquireOrSignal => instance::acquire_or_signal(),
                update_install::Start::GiveUp => {
                    eprintln!("Rustshot: the previous daemon is still running after the update; exiting");
                    exit(0);
                }
            };
            let guard = match inst {
                instance::Instance::Signalled => exit(0),
                instance::Instance::Primary(g) => Some(g),
                instance::Instance::Solo(keep) => {
                    _solo = Some(keep);
                    None
                }
            };
            update_install::cleanup_previous();
            let cfg = config::load();
            let exit_code = Arc::new(AtomicI32::new(0));
            let slot: UploadSlot = Arc::new(Mutex::new(None));
            let mut code = editor::run(cfg.clone(), RunKind::Daemon, None, exit_code, slot.clone(), guard);
            if let Some(rx) = slot.lock().unwrap().take()
                && export::wait_upload(rx, cfg.copy_url_after_upload).is_none() {
                    code = 1;
                }
            exit(code);
        }
        Cmd::Gui(ref args) | Cmd::Full(ref args) | Cmd::Screen { ref args, .. } => {
            let plan = plan_capture(&cmd)?.expect("a capture command");
            let cfg = config::load();
            delayed(delay_of(&cmd));
            match plan {
                Plan::Editor(pending) => exit(editor_main(cfg, RunKind::OneShot, pending)),
                Plan::Direct { screen, region } => {
                    let shot = match screen {
                        Some(n) => capture::grab_monitor(n as usize),
                        None => capture::grab_edit(None, cfg.capture_active_monitor),
                    }
                    .map_err(|e| anyhow!("{e:#}"))?;
                    exit(run_direct(&cfg, args, shot, region));
                }
            }
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Parsed, String> {
        parse_from(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn cmd(args: &[&str]) -> Cmd {
        match p(args).unwrap() {
            Parsed::Cmd(c) => c.expect("expected a command"),
            _ => panic!("expected a command, got help"),
        }
    }

    #[test]
    fn default_is_daemon() {
        assert!(matches!(p(&[]).unwrap(), Parsed::Cmd(None)));
        assert!(HELP.contains("(default;"));
        assert!(matches!(cmd(&["gui"]), Cmd::Gui(_)));
    }

    #[test]
    fn gui_flags() {
        match cmd(&["gui", "-c", "--region", "100x50+0+0", "--upload", "--noedit"]) {
            Cmd::Gui(a) => {
                assert!(a.clip && a.upload && a.noedit);
                assert_eq!(a.region.as_deref(), Some("100x50+0+0"));
            }
            _ => panic!("expected gui"),
        }
    }

    #[test]
    fn inline_and_clustered_values() {
        match cmd(&["full", "--path=out.png", "-d500", "-fx"]) {
            Cmd::Full(a) => {
                assert_eq!(a.delay, 500);
                assert_eq!(a.path.as_deref(), Some(std::path::Path::new("out.png")));
                assert_eq!(a.filename.as_deref(), Some("x"));
            }
            _ => panic!("expected full"),
        }
        match cmd(&["gui", "-cd100"]) {
            Cmd::Gui(a) => {
                assert!(a.clip);
                assert_eq!(a.delay, 100);
            }
            _ => panic!("expected gui"),
        }
    }

    #[test]
    fn screen_number() {
        match cmd(&["screen", "-n", "1", "-c"]) {
            Cmd::Screen { number, args } => {
                assert_eq!(number, 1);
                assert!(args.clip);
            }
            _ => panic!("expected screen"),
        }
        match cmd(&["screen", "--number=2", "--geometry"]) {
            Cmd::Screen { number, args } => {
                assert_eq!(number, 2);
                assert!(args.geometry);
            }
            _ => panic!("expected screen"),
        }
        match cmd(&["screen"]) {
            Cmd::Screen { number, .. } => assert_eq!(number, 0),
            _ => panic!("expected screen"),
        }
    }

    #[test]
    fn config_and_daemon() {
        assert!(matches!(cmd(&["config", "--check"]), Cmd::Config { check: true }));
        assert!(matches!(cmd(&["config"]), Cmd::Config { check: false }));
        assert!(matches!(cmd(&["daemon"]), Cmd::Daemon { after_update: false }));
        assert!(matches!(cmd(&["daemon", "--after-update"]), Cmd::Daemon { after_update: true }));
        assert!(p(&["daemon", "--after-update", "x"]).is_err());
        assert!(p(&["daemon", "x", "--after-update"]).is_err());
        assert!(p(&["gui", "--after-update"]).is_err());
    }

    #[test]
    fn update_command() {
        assert!(matches!(cmd(&["update"]), Cmd::Update));
        assert!(p(&["update", "x"]).is_err());
    }

    #[test]
    fn settings_command() {
        assert!(matches!(cmd(&["settings"]), Cmd::Settings));
        assert!(p(&["settings", "--x"]).is_err());
        assert!(HELP.contains("settings  Open the Settings window"));
    }

    #[test]
    fn errors_and_help() {
        assert!(p(&["gui", "--bogus"]).is_err());
        assert!(p(&["bogus"]).is_err());
        assert!(p(&["gui", "--delay", "x"]).is_err());
        assert!(p(&["screen", "-n", "x"]).is_err());
        assert!(p(&["daemon", "-c"]).is_err());
        assert!(matches!(p(&["--help"]).unwrap(), Parsed::Help(_)));
        assert!(matches!(p(&["help"]).unwrap(), Parsed::Help(_)));
        assert!(matches!(p(&["gui", "-h"]).unwrap(), Parsed::Help(_)));
        match p(&["--version"]).unwrap() {
            Parsed::Help(h) => assert!(h.contains(env!("CARGO_PKG_VERSION"))),
            _ => panic!("expected version"),
        }
    }

    fn err(args: &[&str]) -> String {
        match p(args) {
            Err(e) => e,
            Ok(_) => panic!("{args:?} should not parse"),
        }
    }

    fn capture_args(c: Cmd) -> CaptureArgs {
        match c {
            Cmd::Gui(a) | Cmd::Full(a) | Cmd::Screen { args: a, .. } => a,
            _ => panic!("not a capture command"),
        }
    }

    /// Every capture flag, long and short, on every capture command.
    #[test]
    fn every_capture_flag() {
        for name in ["gui", "full", "screen"] {
            let a = capture_args(cmd(&[
                name, "--path", "a.png", "--clip", "--raw", "--geometry", "--upload", "--delay", "250", "--filename",
                "%F", "--region", "all", "--noedit", "--edit",
            ]));
            assert_eq!(a.path.as_deref(), Some(std::path::Path::new("a.png")), "{name}");
            assert!(a.clip && a.raw && a.geometry && a.upload && a.noedit && a.edit, "{name}");
            assert_eq!((a.delay, a.filename.as_deref(), a.region.as_deref()), (250, Some("%F"), Some("all")));
            let a = capture_args(cmd(&[name, "-p", "dir", "-c", "-d", "7", "-f", "x_%T"]));
            assert_eq!(a.path.as_deref(), Some(std::path::Path::new("dir")));
            assert!(a.clip && !a.raw && !a.upload);
            assert_eq!((a.delay, a.filename.as_deref()), (7, Some("x_%T")));
            let a = capture_args(cmd(&[name, "--delay=9", "--filename=n", "--region=screen1", "-pout.jpg"]));
            assert_eq!((a.delay, a.filename.as_deref(), a.region.as_deref()), (9, Some("n"), Some("screen1")));
            assert_eq!(a.path.as_deref(), Some(std::path::Path::new("out.jpg")));
            let a = capture_args(cmd(&[name]));
            assert!(a.path.is_none() && !a.clip && a.delay == 0 && a.region.is_none(), "defaults");
        }
        // `-n` belongs to `screen` only, in every spelling.
        for args in [&["screen", "-n3"][..], &["screen", "--number", "3"], &["screen", "-c", "--number=3"]] {
            assert!(matches!(cmd(args), Cmd::Screen { number: 3, .. }), "{args:?}");
        }
        assert!(p(&["gui", "-n", "1"]).is_err());
    }

    #[test]
    fn flag_errors_name_the_problem() {
        assert_eq!(err(&["gui", "--clip=yes"]), "unexpected value for --clip");
        assert_eq!(err(&["gui", "--path"]), "missing value for --path");
        assert_eq!(err(&["gui", "-p"]), "missing value for -p");
        assert_eq!(err(&["gui", "-d", "-1"]), "invalid value for --delay: '-1'");
        assert_eq!(err(&["gui", "--delay=soon"]), "invalid value for --delay: 'soon'");
        assert_eq!(err(&["gui", "-x"]), "unexpected argument '-x'");
        assert_eq!(err(&["gui", "-cx"]), "unexpected argument '-x'");
        assert_eq!(err(&["gui", "stray"]), "unexpected argument 'stray'");
        assert_eq!(err(&["full", "--bogus=1"]), "unexpected argument '--bogus'");
        assert_eq!(err(&["screen", "-n"]), "missing value for -n");
        assert_eq!(err(&["screen", "--number=x"]), "invalid value for --number: 'x'");
        assert_eq!(err(&["screen", "-nx"]), "invalid value for -n: 'x'");
        assert_eq!(err(&["-c"]), "unexpected argument '-c'");
        assert_eq!(err(&["capture"]), "unrecognized command 'capture'");
        assert_eq!(err(&["config", "--fix"]), "unexpected argument '--fix'");
        assert_eq!(err(&["settings", "now"]), "unexpected argument 'now'");
        assert_eq!(err(&["update", "--check"]), "unexpected argument '--check'");
        // `-V` only as the first argument; help wins anywhere.
        assert!(p(&["gui", "-V"]).is_err());
        assert!(matches!(p(&["-V"]).unwrap(), Parsed::Help(_)));
        assert!(matches!(p(&["screen", "-n", "1", "--help"]).unwrap(), Parsed::Help(h) if h == HELP));
    }

    #[test]
    fn os_string_arguments() {
        use std::ffi::OsString;
        let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(matches!(parse_args(&os(&["full", "-c"])), Ok(Parsed::Cmd(Some(Cmd::Full(a)))) if a.clip));
        assert!(matches!(parse_args(&[]), Ok(Parsed::Cmd(None))));
        #[cfg(windows)]
        let bad = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0x61, 0xD800])
        };
        #[cfg(unix)]
        let bad = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![0x61, 0xFF])
        };
        let e = parse_args(&[OsString::from("gui"), OsString::from("-p"), bad]).err().expect("not UTF-8");
        assert!(e.starts_with("invalid UTF-8 in argument"), "{e}");
    }

    #[test]
    fn tasks_follow_the_output_flags_in_order() {
        let a = capture_args(cmd(&["full", "--upload", "-c", "--raw", "--geometry", "-p", "x.png"]));
        let t = tasks_from(&a);
        assert!(
            matches!(&t[..], [Task::Save { path: Some(p), ask: false }, Task::Copy, Task::Raw, Task::Geometry, Task::Upload] if p.as_os_str() == "x.png"),
            "{t:?}"
        );
        assert!(tasks_from(&CaptureArgs::default()).is_empty());
    }

    fn plan(args: &[&str]) -> Plan {
        plan_capture(&cmd(args)).expect("plans").expect("a capture command")
    }

    /// Dispatch: which capture commands open the editor, and with what.
    #[test]
    fn capture_commands_plan_editor_or_direct() {
        match plan(&["gui", "--region", "100x50+10+20", "-c", "-f", "n"]) {
            Plan::Editor(p) => {
                assert_eq!((p.region, p.screen, p.filename.as_deref()), (Some((10, 20, 100, 50)), None, Some("n")));
                assert!(matches!(p.tasks[..], [Task::Copy]) && !p.accept_on_select);
            }
            Plan::Direct { .. } => panic!("gui opens the editor"),
        }
        assert!(matches!(plan(&["gui", "--noedit"]), Plan::Direct { screen: None, region: None }));
        assert!(matches!(
            plan(&["gui", "--noedit", "--region", "4x3+-5+6"]),
            Plan::Direct { screen: None, region: Some((-5, 6, 4, 3)) }
        ));
        assert!(matches!(plan(&["full"]), Plan::Direct { screen: None, region: None }));
        assert!(matches!(plan(&["full", "--region", "2x2+0+0"]), Plan::Direct { region: Some((0, 0, 2, 2)), .. }));
        match plan(&["full", "--edit", "--raw"]) {
            Plan::Editor(p) => assert!(p.screen.is_none() && matches!(p.tasks[..], [Task::Raw])),
            Plan::Direct { .. } => panic!("--edit opens the editor"),
        }
        assert!(matches!(plan(&["screen", "-n", "2"]), Plan::Direct { screen: Some(2), region: None }));
        // `screen` grabs the whole monitor: a region is not used (nor parsed).
        assert!(matches!(plan(&["screen", "--region", "junk"]), Plan::Direct { screen: Some(0), region: None }));
        match plan(&["screen", "-n1", "--edit", "-f", "m"]) {
            Plan::Editor(p) => assert_eq!((p.screen, p.region, p.filename.as_deref()), (Some(1), None, Some("m"))),
            Plan::Direct { .. } => panic!("--edit opens the editor"),
        }
        // A bad region fails before anything is captured.
        for args in [&["gui", "--region", "bogus"][..], &["full", "--noedit", "--region", "4x4+a+b"]] {
            assert!(plan_capture(&cmd(args)).is_err(), "{args:?}");
        }
        // The other commands are not captures.
        for args in [&["daemon"][..], &["update"], &["settings"], &["config"], &["config", "--check"]] {
            assert!(plan_capture(&cmd(args)).unwrap().is_none(), "{args:?}");
            assert_eq!(delay_of(&cmd(args)), 0);
        }
        assert_eq!(delay_of(&cmd(&["screen", "-d", "40"])), 40);
    }

    fn shot() -> capture::Shot {
        capture::Shot {
            origin: (-100, 50),
            size: (64, 48),
            scale: 1.0,
            image: crate::pixbuf::PixBuf::from_pixel(64, 48, [10, 20, 30, 255]),
            monitors: vec![(0, 0, 64, 48)],
        }
    }

    /// `run_direct`: crops to the region, saves where `--path` says, and
    /// fails (code 1) when a task fails.
    #[test]
    fn direct_capture_crops_and_saves() {
        let dir = std::env::temp_dir().join(format!("rustshot-direct-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("c.png");
        let cfg = Config::default();
        let args = CaptureArgs { path: Some(out.clone()), geometry: true, ..CaptureArgs::default() };
        assert_eq!(run_direct(&cfg, &args, shot(), Some((-90, 60, 20, 10))), 0);
        let reader = png::Decoder::new(std::fs::File::open(&out).expect("saved")).read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (20, 10));
        // The whole shot when no region is given.
        let whole = dir.join("w.png");
        let args = CaptureArgs { path: Some(whole.clone()), ..CaptureArgs::default() };
        assert_eq!(run_direct(&cfg, &args, shot(), None), 0);
        let reader = png::Decoder::new(std::fs::File::open(&whole).unwrap()).read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (64, 48));
        std::fs::write(dir.join("f"), b"x").unwrap();
        let args = CaptureArgs { path: Some(dir.join("f").join("x.png")), ..CaptureArgs::default() };
        assert_eq!(run_direct(&cfg, &args, shot(), None), 1, "save failed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

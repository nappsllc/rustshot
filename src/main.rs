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
mod sha256;
mod text;
mod theme;
mod tray;
#[cfg(windows)]
mod tray_win;
mod uifb;
mod update;
mod update_install;
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse_from(&argv) {
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
        Cmd::Update => {
            if let Some(channel) = update::managed_install() {
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
        Cmd::Gui(args) => {
            let cfg = config::load();
            delayed(args.delay);
            let tasks = tasks_from(&args);
            let region = match &args.region {
                Some(spec) => Some(parse_region(spec).map_err(|e| anyhow!("{e:#}"))?),
                None => None,
            };
            if args.noedit {
                let shot = capture::grab_edit(None, cfg.capture_active_monitor)
                    .map_err(|e| anyhow!("{e:#}"))?;
                exit(run_direct(&cfg, &args, shot, region));
            }
            let mut pending = Pending::editor();
            pending.tasks = tasks;
            pending.region = region;
            pending.filename = args.filename.clone();
            let code = editor_main(cfg, RunKind::OneShot, pending);
            exit(code);
        }
        Cmd::Full(args) => {
            let cfg = config::load();
            delayed(args.delay);
            let region = match &args.region {
                Some(spec) => Some(parse_region(spec).map_err(|e| anyhow!("{e:#}"))?),
                None => None,
            };
            if args.edit {
                let mut pending = Pending::editor();
                pending.tasks = tasks_from(&args);
                pending.region = region;
                pending.filename = args.filename.clone();
                let code = editor_main(cfg, RunKind::OneShot, pending);
                exit(code);
            }
            let shot = capture::grab_edit(None, cfg.capture_active_monitor)
                .map_err(|e| anyhow!("{e:#}"))?;
            exit(run_direct(&cfg, &args, shot, region));
        }
        Cmd::Screen { number, args } => {
            let cfg = config::load();
            delayed(args.delay);
            if args.edit {
                let mut pending = Pending::editor();
                pending.screen = Some(number);
                pending.tasks = tasks_from(&args);
                pending.filename = args.filename.clone();
                let code = editor_main(cfg, RunKind::OneShot, pending);
                exit(code);
            }
            let shot =
                capture::grab_monitor(number as usize).map_err(|e| anyhow!("{e:#}"))?;
            exit(run_direct(&cfg, &args, shot, None));
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
}

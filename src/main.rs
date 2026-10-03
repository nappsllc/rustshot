mod capture;
mod config;
mod editor;
mod export;
mod hotkey;
mod icons;
mod objects;

use anyhow::{anyhow, Result};
use capture::Shot;
use clap::{Args, Parser, Subcommand};
use config::Config;
use editor::{Pending, RunKind, UploadSlot};
use export::Task;
use std::path::PathBuf;
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "rustshot",
    version,
    about = "Screenshot and annotation tool (Flameshot-style, written in Rust)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Interactive capture: select a region, annotate, then export (default).
    Gui(CaptureArgs),
    /// Capture the whole desktop directly (add --edit for the editor).
    Full(CaptureArgs),
    /// Capture a single monitor (0 = first) directly (add --edit for the editor).
    Screen {
        /// Monitor index, 0-based.
        #[arg(short = 'n', long, default_value_t = 0)]
        number: u32,
        #[command(flatten)]
        args: CaptureArgs,
    },
    /// Run in the background and wait for the global capture hotkey.
    Daemon,
    /// Show or validate the config file.
    Config {
        /// Validate the config file and exit non-zero on errors.
        #[arg(long)]
        check: bool,
    },
}

#[derive(Args, Clone, Default)]
struct CaptureArgs {
    /// Save to this file or directory.
    #[arg(short = 'p', long)]
    path: Option<PathBuf>,
    /// Copy the capture to the clipboard.
    #[arg(short = 'c', long)]
    clip: bool,
    /// Write raw PNG bytes to stdout.
    #[arg(long)]
    raw: bool,
    /// Print the capture geometry as WxH+X+Y to stdout.
    #[arg(long)]
    geometry: bool,
    /// Upload to Imgur and print the URL.
    #[arg(long)]
    upload: bool,
    /// Delay before capturing, in milliseconds.
    #[arg(short = 'd', long, default_value_t = 0)]
    delay: u32,
    /// Filename pattern override for saving (e.g. "%F_shot").
    #[arg(short = 'f', long)]
    filename: Option<String>,
    /// Region to capture/select: WxH+X+Y (virtual-screen coords), "all",
    /// or "screenN".
    #[arg(long)]
    region: Option<String>,
    /// Do the capture without showing the editor.
    #[arg(long)]
    noedit: bool,
    /// Show the editor (default for gui, opt-in for full/screen).
    #[arg(long)]
    edit: bool,
}

fn tasks_from(args: &CaptureArgs) -> Vec<Task> {
    let mut t = Vec::new();
    if let Some(p) = &args.path {
        t.push(Task::Save { path: Some(p.clone()) });
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
        tasks.push(Task::Save {
            path: Some(export::default_save_dir(cfg)),
        });
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
    let mut code = editor::run(cfg.clone(), kind, Some(pending), exit_code, slot.clone());
    if let Some(rx) = slot.lock().unwrap().take()
        && export::wait_upload(rx, cfg.copy_url_after_upload).is_none() {
            code = 1;
        }
    code
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    capture::enable_dpi_awareness();

    let cmd = cli.cmd.unwrap_or(Cmd::Gui(CaptureArgs::default()));
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
        Cmd::Daemon => {
            let cfg = config::load();
            let exit_code = Arc::new(AtomicI32::new(0));
            let slot: UploadSlot = Arc::new(Mutex::new(None));
            let mut code = editor::run(cfg.clone(), RunKind::Daemon, None, exit_code, slot.clone());
            if let Some(rx) = slot.lock().unwrap().take()
                && export::wait_upload(rx, cfg.copy_url_after_upload).is_none() {
                    code = 1;
                }
            std::process::exit(code);
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
                std::process::exit(run_direct(&cfg, &args, shot, region));
            }
            let mut pending = Pending::editor();
            pending.tasks = tasks;
            pending.region = region;
            pending.filename = args.filename.clone();
            let code = editor_main(cfg, RunKind::OneShot, pending);
            std::process::exit(code);
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
                std::process::exit(code);
            }
            let shot = capture::grab_edit(None, cfg.capture_active_monitor)
                .map_err(|e| anyhow!("{e:#}"))?;
            std::process::exit(run_direct(&cfg, &args, shot, region));
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
                std::process::exit(code);
            }
            let shot =
                capture::grab_monitor(number as usize).map_err(|e| anyhow!("{e:#}"))?;
            std::process::exit(run_direct(&cfg, &args, shot, None));
        }
    }
}

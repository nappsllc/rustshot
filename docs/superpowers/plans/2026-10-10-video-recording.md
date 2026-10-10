# Screen Recording (MP4 + GIF), Audio and Camera Bubble — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement `docs/superpowers/specs/2026-10-10-video-recording-design.md`: record a selected area to MP4 (H.264 + AAC/Opus) or GIF with optional system sound, microphone and an on-screen camera bubble, on Windows, Linux (X11 + Wayland) and macOS (compile-only).

**Architecture:** A shared pipeline (`src/rec/`) moves frames from a platform `FrameSource` through a bounded queue (4, drop-oldest) to a `VideoEncoder`, with `AudioSource`s mixed to 48 kHz stereo f32 on one monotonic clock (pause cuts time). Encoders: shared hand-written GIF; Windows Media Foundation; macOS AVAssetWriter; Linux ffmpeg subprocess or OpenH264 + Opus + hand-written MP4 muxer. The recording UI reuses the overlay (Record popover), `wind::run_window` for the control bar, and layered/shaped windows for the border and camera bubble. Settings gets a Recording tab built with the `src/ui/` kit.

**Tech Stack:** Rust 2024; `windows` 0.62 (Graphics_Dxgi, Graphics_Direct3D11, Media_MediaFoundation, Media_Audio, Devices camera via MF); Xlib/XShm/XFixes FFI; hand-written D-Bus client; libpipewire-0.3, libpulse-simple, libopus, libopenh264 via `dlopen`; AppKit/ScreenCaptureKit/AVFoundation FFI. No new crates.

## Global Constraints
- Spec values are binding: config keys and defaults (`rec_format = "mp4"`, `rec_fps = 30`, `rec_gif_fps = 15`, `rec_quality = "medium"`, `rec_system_audio = true`, `rec_mic = false`, `rec_mic_device = ""`, `rec_camera = false`, `rec_camera_device = ""`, `rec_camera_shape = "circle"`, `rec_camera_size = "m"`, `rec_camera_mirror = true`, `rec_countdown = true`, `record_hotkey = "Meta+Shift+R"`, `rec_linux_encoder = "auto"`), keymap actions `record` and `stop_recording`, bubble sizes 160/240/320 logical px, MP4 max 4096×2304 (scale to fit), GIF ≤ 15 fps full size, queue capacity 4, border 2 px red, outputs `<save folder>/<daily subfolder>/<filename pattern>.mp4|.gif` written as `.part` then renamed.
- OpenH264 download only from `http://ciscobinary.openh264.org/` with a SHA-256 pinned in source per version/arch; refuse on mismatch. Never bundle libopenh264.
- Memory bounded: no unbounded frame buffering anywhere; encoders stream to disk.
- Privacy: capture of screen/audio/camera only while recording or while the bubble is shown; nothing uploaded.
- Exe growth target ≈ 250–350 KB over the plan (measure the release exe before Task 1 and after each task; report).
- UI follows theme tokens, Inter, Lucide icons, dark and light; preview PNGs for every new screen state, viewed before reporting.
- Every task: `cargo test`, `cargo clippy --all-targets -- -D warnings` on Windows, plus clippy `--target x86_64-unknown-linux-gnu` and `--target x86_64-apple-darwin`. CI clippy is 1.99 (`as_chunks`, not `chunks_exact(n)`).
- Never kill processes by name; kill only PIDs you started. The user runs Rustshot from `%LOCALAPPDATA%\rustshot` and `target\release`: build with `CARGO_TARGET_DIR=target-sub` only (disk is tight); live tests use `RUSTSHOT_INSTANCE`, isolated APPDATA, unusual hotkeys.
- Never commit secrets; no `${{ }}` inside workflow `run:` bodies.
- Display name "Rustshot", ids lowercase. Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 1: Pipeline core, GIF encoder, config keys
**Files:** create `src/rec/mod.rs` (types, clock, queue, session), `src/rec/gif.rs`, `src/rec/mix.rs`; modify `src/config.rs`, `src/keymap.rs` (actions `record`, `stop_recording`), `src/export.rs` (output path helper for `.mp4/.gif` reusing `auto_save_path` rules).
**Interfaces produced:**
- `rec::Frame { w: u32, h: u32, bgra: Vec<u8> /* top-down */, ts: Duration }`; `rec::Clock` (`start`, `pause`, `resume`, `now() -> Duration` excluding paused time).
- `trait FrameSource: Send { fn next(&mut self, deadline: Instant) -> Option<Frame>; fn size(&self) -> (u32, u32); }`
- `trait AudioSource: Send { fn rate(&self) -> u32; fn channels(&self) -> u16; fn read(&mut self, out: &mut Vec<f32>) -> usize /* interleaved, appends */; }`
- `rec::Mixer::new(sources) ` → `fn pull(&mut self, until: Duration) -> Vec<f32>` (48 kHz stereo, linear resample, per-source gain, clamp).
- `trait VideoEncoder: Send { fn push_video(&mut self, f: &Frame) -> Result<()>; fn push_audio(&mut self, pcm: &[f32], ts: Duration) -> Result<()>; fn finish(self: Box<Self>) -> Result<()>; fn audio(&self) -> bool; }`
- `rec::FrameQueue` (cap 4, drop-oldest, `dropped()` counter).
- `rec::Session::start(spec: RecSpec, source, audio: Vec<Box<dyn AudioSource>>, encoder) -> Session` with `pause()`, `resume()`, `stop() -> Result<PathBuf>`, `stats()`; runs capture and encode threads; writes `<out>.part`, renames on success.
- `rec::gif::GifEncoder::create(path, w, h, fps)` implementing `VideoEncoder` (no audio).
- `RecSpec { format: RecFormat::{Mp4, Gif}, fps, quality: Quality::{Low, Medium, High}, area: IRect, system_audio, mic: Option<String>, out: PathBuf }`.
- Config keys from Global Constraints (parse, validate, `to_toml`, Settings `Form` untouched until Task 9).
- [ ] Tests: GIF — LZW round trip through a small test-only decoder, palette ≤ 256, unchanged pixels transparent inside the changed box, frame delays from timestamps (centiseconds, accumulated rounding), NETSCAPE loop block; mixer — resample 44.1→48 kHz length/accuracy, sum + clamp, mono→stereo; clock with pause; queue drop-oldest under a slow consumer; session with a fake source + fake encoder (frame count, `.part` → rename, error removes `.part`); config round trip and validation of each key.
- [ ] Checks; size; commit.

### Task 2: Windows capture, Media Foundation MP4, WASAPI audio
**Files:** create `src/rec/win/{mod.rs,dxgi.rs,gdi_poll.rs,mf.rs,wasapi.rs,nv12.rs}`; modify `Cargo.toml` (windows features).
**Interfaces produced:** `rec::win::DxgiSource::open(area: IRect) -> Result<Self>` (one duplication per intersecting output on its own adapter, GPU crop, cursor composited from pointer info); `GdiPollSource` fallback; `rec::win::MfEncoder::create(path, w, h, fps, quality, audio: bool)` (SinkWriter H.264 + AAC 160 kbps, hardware MFT if available); `WasapiLoopback::open()`, `WasapiCapture::open(device_id: Option<&str>)`, `wasapi::list_mics() -> Vec<(id, name)>`; `nv12::convert(bgra, w, h, out_w, out_h) -> Vec<u8>` (box downscale when > 4096×2304).
- [ ] Tests: NV12 conversion of known colours (BT.709 limited range) and downscale dims; automated integration: record 2 s of a test window that changes colour every 100 ms → MP4 exists, an MF source reader opens it, duration 2 s ± 0.2, frame count within 10 %; GIF of the same ≥ 15 frames; WASAPI loopback produces samples while a test tone plays through the default device (skip with a message if no render device); DXGI failure falls back to GDI polling (force via test hook).
- [ ] Checks; size; commit.

### Task 3: Recording UI (Windows)
**Files:** `src/editor/toolbar.rs` (Record button, Lucide `video`), `src/editor/chrome.rs`/`compose.rs` (Record popover), create `src/rec_ui/{mod.rs,bar.rs,border.rs,countdown.rs}`; `src/editor/mod.rs` (start/stop state, hotkey `record`/`stop_recording`), `src/tray.rs`/`tray_win.rs` (Stop recording item, red dot icon), `src/hotkey*.rs` (record hotkey).
**Interfaces produced:** popover with MP4|GIF segmented control, fps choices (MP4 30/60, GIF 10/15), toggles System sound / Microphone (+device dropdown) / Camera (+device, shape, size — wired in Task 4), **Start recording**; `rec_ui::Border` (layered click-through 2 px red, `WDA_EXCLUDEFROMCAPTURE`); `rec_ui::Bar` via `run_window`-style tool window (timer, Pause/Resume, Stop, mic mute; excluded from capture; placed outside the area on the side with most room); 3-2-1 countdown (setting); on stop: toast "Saved <path>" with Open folder.
- [ ] Tests: popover layout and hit tests; bar placement logic (pure) for areas touching each screen edge and multi-monitor; state machine idle → countdown → recording ↔ paused → stopping → idle, hotkey toggles, tray stop; preview PNGs (popover, bar, countdown) dark/light.
- [ ] Manual (isolated daemon): record a 3 s MP4 and a GIF; border and bar not visible in the output.
- [ ] Checks; size; commit.

### Task 4: Camera bubble (Windows)
**Files:** create `src/cam/{mod.rs,win.rs,bubble.rs}`.
**Interfaces produced:** `cam::list() -> Vec<CamDevice{id,name}>`; `cam::open(id, pref: (1280,720)) -> Result<Box<dyn CamSource>>` (MF source reader, YUY2/NV12 → BGRA; MJPG-only devices rejected with a clear error); `cam::Bubble::show(shape, size, mirror, anchor: IRect)` (layered per-pixel-alpha window via `UpdateLayeredWindow`, accent ring + soft shadow, drag with corner snapping to the area, position remembered relative to the area); bubble NOT excluded from capture.
- [ ] Tests: shape masks (circle, rounded square, square) at each size — preview PNGs and AA edge checks; snapping logic (pure); YUY2/NV12 → BGRA conversion of known values; camera-less machines: `list()` empty → toggle disabled with tooltip.
- [ ] Checks; size; commit.

### Task 5: Linux X11 frames + ffmpeg encoder + Pulse device listing
**Files:** create `src/rec/linux/{mod.rs,xshm.rs,ffmpeg.rs,pulse_list.rs}`.
**Interfaces produced:** `XShmSource::open(area)` (XShmGetImage at target fps, cursor via XFixes); `FfmpegEncoder::create(path, w, h, fps, quality, system_monitor: Option<String>, mic: Option<String>)` (args per spec: rawvideo bgra stdin, `-f pulse` inputs, `amix`, libx264 veryfast crf by quality → fallback libopenh264 / h264_vaapi detected from `ffmpeg -encoders`, aac, `+faststart`; constant frame rate via dup/drop); `ffmpeg::find() -> Option<PathBuf>`; `pulse_list::{monitors(), sources()}` via `pactl -f json list` (fallback text parsing).
- [ ] Tests: ffmpeg argument building for each combination; encoder detection parsing; CFR dup/drop logic; CI Linux job: record Xvfb for 2 s with ffmpeg (install ffmpeg in the job), `ffprobe` duration 2 s ± 0.3.
- [ ] Checks; size; commit.

### Task 6: Linux OpenH264 + Opus + MP4 muxer + PulseAudio + first-use dialog
**Files:** create `src/rec/mp4.rs` (shared muxer), `src/rec/linux/{openh264.rs,opus.rs,pulse.rs,codec_dialog.rs}`; `src/rec/openh264_pins.rs` (version, arch, URL, SHA-256).
**Interfaces produced:** `mp4::Mp4Writer` (ftyp/moov/mdat; avc1 + `avcC`, Opus `dOps`; stts/stss/stsz/stco; moov moved to front on finish); `OpenH264::load() -> Result<Self>` (dlopen from `$XDG_DATA_HOME/rustshot/`), `openh264::download(progress) -> Result<PathBuf>` (bz2 from Cisco, verified, decompressed — bz2 decoder: use the system `bzip2`/`bunzip2` binary via fixed path, or hand-written if absent; decide and document); `Opus::load()` (libopus.so.0); `PulseSource::open(device)` (libpulse-simple.so.0, works on PipeWire); `codec_dialog::ask() -> Choice::{DownloadOpenH264, InstallFfmpeg, Gif, Cancel}` (run_window; distro command from `/etc/os-release` for apt/dnf/pacman/zypper with Copy button); stores `rec_linux_encoder`.
- [ ] Tests: MP4 box tree parsed back (sizes, durations, sample tables, avcC/dOps fields) for a synthetic H.264 + Opus stream; pin table lookup per arch; download verification refuses a wrong hash (local fixture); distro command mapping; encoder selection logic (`auto`: ffmpeg → openh264 → ask); CI Linux: download the pinned OpenH264 and encode 1 s of synthetic frames, ffprobe validates.
- [ ] Checks; size; commit.

### Task 7: D-Bus client, Wayland ScreenCast portal + PipeWire, camera portal; V4L2; Linux bubble
**Files:** create `src/dbus.rs` (shared with the future SNI tray), `src/rec/linux/{portal.rs,pipewire.rs,v4l2.rs}`, `src/cam/linux.rs`, bubble support in `wind_linux.rs` (ARGB visual + XShape) and Wayland transparent toplevel.
**Interfaces produced:** `dbus::Conn::session()` (unix socket, AUTH EXTERNAL, marshal/unmarshal basic + container types, method calls, signal matching); `portal::ScreenCast::start(restore_token) -> Result<(PwFd, node_id, Option<token>)>`; `PipeWireSource::open(fd, node, crop: IRect)` (dlopen libpipewire-0.3; BGRx/RGBx shm buffers); camera portal `Camera.AccessCamera` + PipeWire camera stream; `V4l2Cam` (YUYV) on X11; restore token stored in the state dir.
- [ ] Tests: D-Bus marshalling round trips (strings, arrays, dicts, variants, alignment); portal request/response state machine against a scripted fake bus; PipeWire format negotiation logic (pure parts); V4L2 YUYV conversion. Wayland/PipeWire live paths: manual only (document steps in the report).
- [ ] Checks; size; commit.

### Task 8: macOS (compile-only)
**Files:** create `src/rec/macos/{sck.rs,avwriter.rs}`, `src/cam/macos.rs`, bubble in `wind_macos.rs`.
**Interfaces produced:** `SckSource` (SCStream with source rect, system audio on 13+, exclusion of our border/bar windows), mic via AVCaptureDevice; `AvWriterEncoder` (H.264 + AAC); `cam::macos` AVCaptureSession; transparent NSWindow bubble. Info.plist keys `NSMicrophoneUsageDescription`, `NSCameraUsageDescription` in `packaging/macos/bundle.sh`.
- [ ] Tests: compile on `x86_64-apple-darwin` and `aarch64-apple-darwin` (clippy); pure helpers unit-tested; CI macOS job runs the unit tests.
- [ ] Checks; size; commit.

### Task 9: Packaging, permissions, Settings Recording tab, docs, verification
**Files:** `packaging/windows/AppxManifest.xml.in` (`microphone`, `webcam` DeviceCapabilities), `packaging/macos/mas.entitlements` (audio-input, camera), Flatpak manifest (`--socket=pulseaudio`, `--device=all` or camera portal, `org.freedesktop.Platform.ffmpeg-full` extension, openh264 extension), `snap/snapcraft.yaml` (plugs `audio-record`, `pulseaudio`, `camera`, `screencast-legacy`), deb `Recommends: ffmpeg`, `src/settings_ui.rs` (Recording tab per spec with encoder status line), `PRIVACY.md`, `README.md`, `docs/STORES.md` (permission justifications).
- [ ] Settings Recording tab tests + preview PNGs (dark/light); form round trip of all `rec_*` keys.
- [ ] Windows Sandbox clean-system run extended: record 2 s MP4 + GIF via CLI/hotkey path in an isolated instance; outputs play (MF source reader check).
- [ ] e2e on the host (isolated); size vs. Task 1 baseline; final whole-branch review; fix wave; CI green on all jobs.

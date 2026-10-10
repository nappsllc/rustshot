# Screen Recording (MP4 + GIF), Audio and Camera Bubble — Design

Approved 2026-10-10. Built after the Settings/Saving/Updates plan
(`2026-10-10-settings-saving-updates.md`); it adds a Recording tab to that
Settings window and reuses its controls.

## Decisions (from the user)
- Formats: MP4 and GIF, all three OSes.
- Audio: system sound and microphone, each toggleable.
- Start/stop: Record button in the overlay toolbar plus a hotkey; tray can stop.
- Linux: X11 **and** Wayland. Encoder: ffmpeg if installed, otherwise
  offer on the first recording: install ffmpeg (show the command) or
  download Cisco OpenH264. Choice saved, changeable in Settings.
- Camera: on-screen bubble (circle, rounded square, square), captured with
  the screen like any window.

## Pipeline (shared)
```
FrameSource ──► FrameQueue (cap 4, drop-oldest, counts drops) ──► VideoEncoder
AudioSource(s) ──► Mixer (48 kHz stereo f32, 10 ms blocks) ─────► (same encoder)
```
- `trait FrameSource { fn next(&mut self, deadline) -> Option<Frame> }`, frames
  BGRA top-down with a monotonic timestamp; fps target 10/15 (GIF) or 30/60 (MP4).
- `trait AudioSource { fn read(&mut self, out: &mut [f32]) -> usize }`; the mixer
  resamples (linear) to 48 kHz, sums with per-source gain, clamps.
- `trait VideoEncoder { fn push_video(&mut self, &Frame); fn push_audio(&mut self, &[f32], ts); fn finish(self) -> Result<PathBuf> }`.
- A/V sync: everything stamped from one monotonic clock started at Record;
  pause shifts the clock (paused time is cut, not recorded as frozen frames).
- Memory stays bounded: queue of 4 frames, encoders stream to disk.
- Output: `<save folder>/<daily subfolder>/<filename pattern>.mp4|.gif`
  (Task 1 saving rules), written as `.part` and renamed on success; on error
  the partial file is kept only if it is playable (MP4 finalised), else removed.

## GIF encoder (shared, hand-written)
- Per frame: changed bounding box vs. previous frame; quantise that region to
  ≤ 255 colours (median-cut on a 5-6-5 histogram, ordered dither off by default),
  transparent index for unchanged pixels inside the box, LZW (variable code
  size, clear at 4096), frame delay from timestamps (centiseconds).
- Loop forever (NETSCAPE2.0). No audio; audio toggles disabled for GIF.
- Max 15 fps; full resolution.

## Windows
- Frames: DXGI Desktop Duplication per output intersecting the area (each
  output duplicated on its own adapter); crop on the GPU (CopySubresourceRegion
  into a staging texture), map, copy rows. Fallback: GDI BitBlt polling when
  duplication is unavailable (RDP, some drivers). Cursor drawn from the
  duplication pointer info.
- Audio: WASAPI shared mode — render endpoint loopback (system), capture
  endpoint (mic, chosen device or default).
- MP4: Media Foundation SinkWriter, H.264 (hardware MFT if available, else
  Microsoft software MFT) + AAC 160 kbps; frames converted BGRA→NV12 on CPU
  (SWAR) or by the MFT's colour converter.
- Areas wider than 4096 px (or taller than 2304) are scaled to fit for MP4
  (box filter during NV12 conversion); GIF keeps full size.
- Our recording border and control bar use `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`;
  the camera bubble does not.

## macOS (compile-only until a Mac is available)
- ScreenCaptureKit (`SCStream`, macOS 12.3+): video for the display(s) of the
  area with a source rect; system audio (13+); microphone via
  `AVCaptureDevice` (or SCK mic on 15+).
- MP4: AVAssetWriter H.264 + AAC.
- Our border/bar excluded via `SCContentFilter` excluding rustshot's own
  windows except the camera bubble.
- Info.plist: `NSMicrophoneUsageDescription`, `NSCameraUsageDescription`;
  Screen Recording permission is requested by the system on first use.

## Linux
### Frames
- X11: XShm (`XShmGetImage`) of the area at the target fps; cursor via XFixes.
- Wayland (and Flatpak on any session): xdg-desktop-portal ScreenCast over
  D-Bus (`CreateSession` → `SelectSources` monitor, cursor embedded →
  `Start` → `OpenPipeWireRemote`), then a PipeWire stream (libpipewire-0.3
  loaded with `dlopen`) delivering BGRx/RGBx buffers (shm; dmabuf not
  requested). The portal dialog lets the user pick the screen; the area is
  cropped from it. Restore token stored so later recordings don't re-ask
  (where the portal supports `persist_mode`).
- The hand-written D-Bus client (`src/dbus.rs`: auth EXTERNAL, message
  marshalling, signals) is shared with the planned StatusNotifierItem tray.

### Encoding
- **ffmpeg** (preferred when `ffmpeg` is on PATH): rustshot writes raw BGRA
  frames to its stdin (`-f rawvideo -pix_fmt bgra -s WxH -r FPS -i -`),
  ffmpeg captures audio itself (`-f pulse -i <monitor source>` for system,
  `-f pulse -i <mic>`; mixed with `amix`), output `libx264 -preset veryfast
  -crf 23 -pix_fmt yuv420p` (falls back to `libopenh264` or `h264_vaapi` if
  libx264 is absent) + `aac`, `-movflags +faststart`. Timestamps: constant
  frame rate, rustshot duplicates/drops frames to keep it.
- **OpenH264 + Opus** (when ffmpeg is absent and the user chose it):
  `libopenh264.so` downloaded from `http://ciscobinary.openh264.org/` (the
  official Cisco URL; HTTP by Cisco's design, so the file is verified
  against the SHA-256 published in rustshot for that exact version) into
  `$XDG_DATA_HOME/rustshot/`, loaded with `dlopen`; audio via
  PulseAudio simple API (`libpulse-simple.so.0`, works on PipeWire) encoded
  with system `libopus.so.0`; hand-written MP4 muxer (ftyp/moov/mdat,
  avc1 + Opus `dOps`, fragmented-free, moov written at the end then moved
  to the front). If libopus is missing, video-only with a notice.
- **First recording without ffmpeg**: dialog "Recording MP4 needs an
  encoder" with **Download OpenH264 (Cisco, ~1 MB)**, **Install ffmpeg**
  (shows the distro command: apt/dnf/pacman/zypper detected from
  `/etc/os-release`, with a Copy button), **Record GIF instead**, **Cancel**.
  Choice stored in `rec_linux_encoder = "auto" | "ffmpeg" | "openh264"`.
- Flatpak: add `org.freedesktop.Platform.ffmpeg-full` extension (and the
  runtime's openh264 extension), `--socket=pulseaudio`, `--device=all`
  (camera) or the camera portal; Snap: plugs `audio-record`, `pulseaudio`,
  `camera`, `screencast-legacy` where needed; deb: `Recommends: ffmpeg`.

## Camera bubble
- Capture: Windows Media Foundation source reader (720p, YUY2/NV12
  preferred; MJPG not supported in v1 — pick a non-MJPG mode or lower
  resolution); macOS AVCaptureSession; Linux V4L2 (YUYV) on X11, camera
  portal + PipeWire on Wayland/Flatpak. Frames converted to BGRA.
- Bubble window: borderless, always-on-top, per-pixel alpha (Windows layered
  window with `UpdateLayeredWindow`; X11 ARGB visual + shape; macOS
  transparent NSWindow; Wayland: normal transparent toplevel, compositor
  chooses position). Shapes: circle, rounded square, square; sizes S/M/L
  (160/240/320 logical px); mirror toggle; accent ring + soft shadow; drag
  to move, snaps to the area's corners; remembers position relative to the
  area.
- Appears when Camera is toggled on in the Record menu (before recording),
  stays during recording, closes on Stop. It is captured with the screen
  because it is a real on-screen window (not excluded from capture).
- Background effects: none in rustshot; OS-level effects (Windows Studio
  Effects, macOS Portrait/Center Stage) apply automatically to the feed.

## Recording UI
- Toolbar gets a **Record** button (Lucide `video`). Its popover: format
  segmented control (MP4 | GIF), fps (MP4 30/60, GIF 10/15), toggles
  System sound, Microphone (+ device dropdown), Camera (+ device dropdown,
  shape, size), **Start recording** (primary). Defaults from Settings.
- Recording: overlay closes; red 2 px border (click-through, excluded from
  capture) around the area; control bar (timer, Pause/Resume, Stop, mic
  mute) placed just outside the area on the side with most room (excluded
  from capture). On Wayland no border (compositor indicator instead), bar
  is a normal small window.
- Stop: bar, record hotkey (default `Meta+Shift+R`; when idle it starts area
  selection in record mode), tray "Stop recording" (tray menu shows it while
  recording; tray icon gets a red dot).
- Done: toast/notification "Saved <path>" with Open folder.
- 3-2-1 countdown before the first frame (Settings toggle, default on).

## Settings → Recording tab
Default format, fps, MP4 quality (Low/Medium/High → bitrate/CRF), system
sound and microphone defaults + mic device, camera device/shape/size/mirror,
countdown, record hotkey, Linux encoder (Auto/ffmpeg/OpenH264, with status
"ffmpeg found at …" / "OpenH264 downloaded" / "not available").

## Config keys
`rec_format = "mp4"`, `rec_fps = 30`, `rec_gif_fps = 15`, `rec_quality = "medium"`,
`rec_system_audio = true`, `rec_mic = false`, `rec_mic_device = ""`,
`rec_camera = false`, `rec_camera_device = ""`, `rec_camera_shape = "circle"`,
`rec_camera_size = "m"`, `rec_camera_mirror = true`, `rec_countdown = true`,
`record_hotkey = "Meta+Shift+R"`, `rec_linux_encoder = "auto"`.
Shortcut actions added to the keymap: `record` (overlay), `stop_recording`.

## Privacy and stores
- PRIVACY.md: screen, audio and camera are captured only while recording or
  while the camera bubble is shown; files stay local unless the user
  uploads (MP4/GIF upload is not added in this work).
- MSIX: `<DeviceCapability Name="microphone"/>`, `<DeviceCapability Name="webcam"/>`.
- MAS entitlements: `com.apple.security.device.audio-input`,
  `com.apple.security.device.camera`.
- Flathub/Snap permissions as above, justified in the submission notes
  (docs/STORES.md).

## Testing
- Unit: GIF encoder (LZW round trip with a small decoder in tests,
  palette ≤ 256, transparency for unchanged pixels, frame delays), mixer
  (resample, sum, clamp), A/V clock with pause, MP4 muxer box layout
  (parse back box tree, durations, sample tables), NV12 conversion and
  downscale, ffmpeg argument building, OpenH264 hash check, encoder
  selection logic, D-Bus marshalling round trips, bubble shape masks
  (preview PNGs).
- Integration (Windows, automated): record 2 s of a test window that changes
  colour every 100 ms → MP4 exists, Media Foundation source reader opens it,
  duration ≈ 2 s ± 0.2, frame count within 10 %; GIF decodes with ≥ 15 frames.
- Linux: CI job records Xvfb for 2 s with ffmpeg (ffprobe checks duration);
  OpenH264 path tested with the downloaded library in CI; Wayland tested
  manually (portal needs a session).
- Preview PNGs: Record popover, control bar, bubble shapes, first-use
  encoder dialog, Settings Recording tab (dark and light).

## Size
≈ 9–11k lines; exe growth ≈ 250–350 KB. No new crates; ffmpeg, OpenH264,
libopus, libpulse and libpipewire are optional runtime components on Linux.

## Build order (one reviewed task each)
1. Pipeline core + GIF encoder + config keys.
2. Windows frames (DXGI + fallback) + Media Foundation MP4 + WASAPI audio.
3. Recording UI (Record popover, border, control bar, hotkey, tray, countdown).
4. Camera bubble, Windows (MF capture, layered window).
5. Linux X11 frames + ffmpeg encoder + pulse device listing.
6. Linux OpenH264 + Opus + MP4 muxer + PulseAudio + first-use dialog.
7. D-Bus client + Wayland ScreenCast portal + PipeWire (+ camera portal); V4L2 camera on X11; Linux bubble.
8. macOS (SCK, AVAssetWriter, AVCaptureSession, bubble) compile-only.
9. Packaging/permissions, Settings Recording tab, PRIVACY/README/STORES, e2e and sandbox verification.

## Out of scope
Background blur/removal, video editing/trimming, uploading videos, window
(instead of area) capture, HDR, per-application audio, MJPG cameras.

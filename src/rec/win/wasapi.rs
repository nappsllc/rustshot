//! Sound through WASAPI (shared mode): loopback of the default render
//! endpoint (system sound) and capture endpoints (microphones).
//!
//! Each stream asks for 48 kHz stereo f32 and lets the audio engine convert
//! (`AUTOCONVERTPCM`, including the down-mix of a multichannel device). If
//! the device refuses, the stream runs at its mix format instead: any rate
//! (the mixer resamples), and more than two channels are down-mixed here
//! ([`downmix_matrix`], ITU-R BS.775 style).
//!
//! Each stream runs on its own thread, polling the capture client every
//! 10 ms. Packets go into a ring holding at most one second, each with the
//! instant its first frame was captured (from the packet's QPC position);
//! when the reader falls behind, the oldest packets are dropped. A read
//! hands out the contiguous run of packets from the oldest on and stops at
//! a discontinuity (one the device reports, or packets dropped from the
//! ring), so the mixer can place what follows by its own timestamp.
//! Loopback delivers no packets while nothing plays: the mixer treats the
//! gap as silence.

use super::{com_init, in_mta};
use crate::rec::AudioSource;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// How often the capture thread looks for packets.
const POLL: Duration = Duration::from_millis(10);
/// WASAPI buffer: well above the poll interval.
const BUFFER_HNS: i64 = 2_000_000;
/// The format asked for: the mixer's own.
const WANT_RATE: u32 = 48_000;
/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`.
const FLOAT_SUBTYPE: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const EXTENSIBLE: u16 = 0xFFFE;

/// Sample layout of a mix format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sample {
    F32,
    I16,
    I24,
    I32,
}

/// Parse a mix format: (layout, rate, channels, bytes per frame).
pub(crate) fn parse_format(f: &WAVEFORMATEX, sub: Option<GUID>) -> Result<(Sample, u32, u16, usize)> {
    const PCM: u32 = 1;
    const FLOAT: u32 = 3;
    // Extensible formats name the tag in the first field of the subformat
    // GUID ({tag-0000-0010-8000-00aa00389b71}).
    let tag = if f.wFormatTag == EXTENSIBLE {
        sub.map(|g| g.data1).ok_or_else(|| anyhow!("extensible format without a subformat"))?
    } else {
        u32::from(f.wFormatTag)
    };
    let bits = f.wBitsPerSample;
    let kind = match (tag, bits) {
        (FLOAT, 32) => Sample::F32,
        (PCM, 16) => Sample::I16,
        (PCM, 24) => Sample::I24,
        (PCM, 32) => Sample::I32,
        (t, b) => bail!("unsupported audio format (tag {t}, {b} bits)"),
    };
    let (ch, rate) = (f.nChannels, f.nSamplesPerSec);
    let block = usize::from(f.nBlockAlign);
    let bytes = match kind {
        Sample::I16 => 2,
        Sample::I24 => 3,
        _ => 4,
    };
    if ch == 0 || rate == 0 || block != bytes * usize::from(ch) {
        bail!("unsupported audio format ({ch} channels, {rate} Hz, block {block})");
    }
    Ok((kind, rate, ch, block))
}

/// Append `data` (interleaved samples of `kind`) to `out` as f32.
pub(crate) fn to_f32(kind: Sample, data: &[u8], out: &mut Vec<f32>) {
    match kind {
        Sample::F32 => out.extend(data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b))),
        Sample::I16 => out.extend(data.as_chunks::<2>().0.iter().map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0)),
        Sample::I24 => out.extend(
            data.as_chunks::<3>().0.iter().map(|b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0),
        ),
        Sample::I32 => {
            out.extend(data.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b) as f32 / 2_147_483_648.0))
        }
    }
}

/// The channel mask Windows assumes for `channels` when a format has none
/// (`KSAUDIO_SPEAKER_*`).
fn default_mask(channels: u16) -> u32 {
    match channels {
        1 => 0x4,
        2 => 0x3,
        3 => 0x7,
        4 => 0x33,
        5 => 0x37,
        6 => 0x3F,
        7 => 0x13F,
        _ => 0x63F,
    }
}

/// Stereo gains (left, right) of each channel of a `channels`-channel
/// stream laid out by `mask` (`dwChannelMask`; channels take the set bits
/// in order). ITU-R BS.775 style: front left/right as they are, centre and
/// LFE folded into both at -3 dB, surrounds (side, back) and the top
/// layer at -3 dB on their side, centred rear/top speakers at -6 dB into
/// both. Not normalised: the mixer clamps, and stereo content in a
/// multichannel mix (only FL/FR carry sound) keeps its level. Channels
/// without a speaker in the mask are dropped.
pub(crate) fn downmix_matrix(channels: u16, mask: u32) -> Vec<[f32; 2]> {
    const H: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let mask = if mask == 0 { default_mask(channels) } else { mask };
    let gains = |bit: u32| match bit {
        0x1 | 0x40 => [1.0, 0.0],                 // FL, front left of centre
        0x2 | 0x80 => [0.0, 1.0],                 // FR, front right of centre
        0x4 | 0x8 => [H, H],                      // FC, LFE
        0x10 | 0x200 | 0x1000 | 0x8000 => [H, 0.0], // BL, SL, top front/back left
        0x20 | 0x400 | 0x4000 | 0x20000 => [0.0, H], // BR, SR, top front/back right
        0x100 | 0x800 | 0x2000 | 0x10000 => [0.5, 0.5], // BC, top centre, top front/back centre
        _ => [0.0, 0.0],
    };
    let mut bits = (0..32).map(|i| 1u32 << i).filter(|b| mask & b != 0);
    (0..channels).map(|_| bits.next().map_or([0.0, 0.0], gains)).collect()
}

/// Down-mix interleaved frames of `m.len()` channels to stereo.
pub(crate) fn downmix(m: &[[f32; 2]], data: &[f32], out: &mut Vec<f32>) {
    for f in data.chunks_exact(m.len()) {
        let (mut l, mut r) = (0.0, 0.0);
        for (s, g) in f.iter().zip(m) {
            l += s * g[0];
            r += s * g[1];
        }
        out.extend([l, r]);
    }
}

/// One packet: when its first frame was captured, its samples, and
/// whether it follows on from the packet before it.
struct Chunk {
    at: Instant,
    data: Vec<f32>,
    cont: bool,
}

#[derive(Default)]
struct RingQ {
    chunks: VecDeque<Chunk>,
    len: usize,
}

/// Bounded hand-off from the capture thread.
struct Ring {
    q: Mutex<RingQ>,
    /// Most samples held (one second, set once the format is known).
    cap: AtomicUsize,
}

impl Ring {
    fn new(cap: usize) -> Ring {
        Ring { q: Mutex::new(RingQ::default()), cap: AtomicUsize::new(cap) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RingQ> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue a packet captured at `at`; `cont` = it follows on from the
    /// previous one. Over the cap, the oldest packets go, and what is left
    /// no longer follows on from what was read before.
    fn push(&self, at: Instant, data: &[f32], cont: bool) {
        if data.is_empty() {
            return;
        }
        let mut q = self.lock();
        q.len += data.len();
        q.chunks.push_back(Chunk { at, data: data.to_vec(), cont });
        let cap = self.cap.load(Ordering::Relaxed);
        while q.len > cap
            && let Some(c) = q.chunks.pop_front()
        {
            q.len -= c.data.len();
            if let Some(f) = q.chunks.front_mut() {
                f.cont = false;
            }
        }
    }

    /// Append the oldest packet and those that follow on from it to `out`;
    /// returns the samples appended and when the first was captured.
    fn take(&self, out: &mut Vec<f32>) -> (usize, Option<Instant>) {
        let mut q = self.lock();
        let Some(first) = q.chunks.pop_front() else { return (0, None) };
        let at = first.at;
        let mut n = first.data.len();
        out.extend(first.data);
        while q.chunks.front().is_some_and(|c| c.cont) {
            let c = q.chunks.pop_front().expect("front exists");
            n += c.data.len();
            out.extend(c.data);
        }
        q.len -= n;
        (n, Some(at))
    }
}

/// A capture stream: microphone, or (via [`WasapiLoopback`]) system sound.
pub struct WasapiCapture {
    rate: u32,
    channels: u16,
    ring: Arc<Ring>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// System sound: loopback of the default render device.
pub struct WasapiLoopback(WasapiCapture);

impl WasapiLoopback {
    pub fn open() -> Result<WasapiLoopback> {
        WasapiCapture::start(Endpoint::Loopback).map(WasapiLoopback)
    }
}

#[derive(Clone)]
enum Endpoint {
    Loopback,
    Mic(Option<String>),
}

impl WasapiCapture {
    /// A microphone: the endpoint with `device_id`, or the default capture
    /// device for `None` / `Some("")`.
    pub fn open(device_id: Option<&str>) -> Result<WasapiCapture> {
        WasapiCapture::start(Endpoint::Mic(device_id.filter(|s| !s.is_empty()).map(str::to_owned)))
    }

    fn start(ep: Endpoint) -> Result<WasapiCapture> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(1);
        let ring = Arc::new(Ring::new(0));
        let (stop2, ring2) = (stop.clone(), ring.clone());
        let thread = std::thread::Builder::new()
            .name("rec-audio".into())
            .spawn(move || run(ep, tx, &ring2, &stop2))
            .context("start the audio thread")?;
        let (rate, channels) = match rx.recv() {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                bail!("the audio thread stopped");
            }
        };
        Ok(WasapiCapture { rate, channels, ring, stop, thread: Some(thread) })
    }
}

impl Drop for WasapiCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl AudioSource for WasapiCapture {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn read(&mut self, out: &mut Vec<f32>) -> usize {
        self.read_timed(out).0
    }

    fn read_timed(&mut self, out: &mut Vec<f32>) -> (usize, Option<Instant>) {
        self.ring.take(out)
    }
}

impl AudioSource for WasapiLoopback {
    fn rate(&self) -> u32 {
        self.0.rate()
    }

    fn channels(&self) -> u16 {
        self.0.channels()
    }

    fn read(&mut self, out: &mut Vec<f32>) -> usize {
        self.0.read(out)
    }

    fn read_timed(&mut self, out: &mut Vec<f32>) -> (usize, Option<Instant>) {
        self.0.read_timed(out)
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.context("open the audio device list")
}

fn device(en: &IMMDeviceEnumerator, ep: &Endpoint) -> Result<IMMDevice> {
    unsafe {
        match ep {
            Endpoint::Loopback => en.GetDefaultAudioEndpoint(eRender, eConsole).context("no audio output device"),
            Endpoint::Mic(None) => en.GetDefaultAudioEndpoint(eCapture, eConsole).context("no microphone"),
            Endpoint::Mic(Some(id)) => {
                let w: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
                en.GetDevice(PCWSTR(w.as_ptr())).with_context(|| format!("microphone {id} not found"))
            }
        }
    }
}

struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    kind: Sample,
    rate: u32,
    /// Channels the device delivers.
    channels: u16,
    block: usize,
    /// Down-mix to stereo (more than two channels).
    mix: Option<Vec<[f32; 2]>>,
}

impl Stream {
    /// Channels handed to the mixer.
    fn out_channels(&self) -> u16 {
        if self.mix.is_some() { 2 } else { self.channels }
    }
}

/// 48 kHz stereo f32, as a `WAVEFORMATEXTENSIBLE`.
fn stereo_f32() -> WAVEFORMATEXTENSIBLE {
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: EXTENSIBLE,
            nChannels: 2,
            nSamplesPerSec: WANT_RATE,
            nAvgBytesPerSec: WANT_RATE * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
        dwChannelMask: 0x3,
        SubFormat: FLOAT_SUBTYPE,
    }
}

fn open_stream(ep: &Endpoint) -> Result<Stream> {
    let en = enumerator()?;
    let dev = device(&en, ep)?;
    let base = if matches!(ep, Endpoint::Loopback) { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
    unsafe {
        // The mixer's format, converted by the audio engine.
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).context("activate the audio client")?;
        let want = stereo_f32();
        let flags = base | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        let asked = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_HNS, 0, &want as *const _ as *const WAVEFORMATEX, None);
        let client = match asked {
            Ok(()) => {
                let capture: IAudioCaptureClient = client.GetService().context("get the capture client")?;
                return Ok(Stream { client, capture, kind: Sample::F32, rate: WANT_RATE, channels: 2, block: 8, mix: None });
            }
            // A failed Initialize leaves the client unusable: start over.
            Err(_) => dev.Activate::<IAudioClient>(CLSCTX_ALL, None).context("activate the audio client")?,
        };
        let fmt = client.GetMixFormat().context("read the mix format")?;
        let f = std::ptr::read_unaligned(fmt);
        let ext = (f.wFormatTag == EXTENSIBLE && f.cbSize >= 22)
            .then(|| std::ptr::read_unaligned(fmt as *const WAVEFORMATEXTENSIBLE));
        let (sub, mask) = (ext.map(|e| e.SubFormat), ext.map_or(0, |e| e.dwChannelMask));
        let parsed = parse_format(&f, sub);
        let init = parsed.as_ref().ok().map(|_| client.Initialize(AUDCLNT_SHAREMODE_SHARED, base, BUFFER_HNS, 0, fmt, None));
        CoTaskMemFree(Some(fmt as *const _));
        let (kind, rate, channels, block) = parsed?;
        init.context("no format")?.context("start the audio stream")?;
        let capture: IAudioCaptureClient = client.GetService().context("get the capture client")?;
        let mix = (channels > 2).then(|| downmix_matrix(channels, mask));
        Ok(Stream { client, capture, kind, rate, channels, block, mix })
    }
}

type Ready = mpsc::SyncSender<Result<(u32, u16)>>;

fn run(ep: Endpoint, ready: Ready, ring: &Ring, stop: &AtomicBool) {
    com_init();
    let s = match open_stream(&ep).and_then(|s| unsafe { s.client.Start() }.map(|_| s).context("start capturing")) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    ring.cap.store(s.rate as usize * usize::from(s.out_channels()), Ordering::Relaxed);
    let _ = ready.send(Ok((s.rate, s.out_channels())));
    let qpc = Qpc::new();
    let (mut raw, mut buf) = (Vec::new(), Vec::new());
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(POLL);
        if let Err(e) = drain(&s, &qpc, ring, &mut raw, &mut buf) {
            // Device unplugged or changed: the source goes quiet (silence).
            eprintln!("rustshot: audio capture stopped: {e}");
            break;
        }
    }
    let _ = unsafe { s.client.Stop() };
}

/// Maps QPC positions (100-ns units, as `GetBuffer` reports them) to
/// `Instant`s by the distance from the counter's current value.
struct Qpc {
    freq: i128,
}

impl Qpc {
    fn new() -> Qpc {
        let mut f = 0i64;
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        Qpc { freq: i128::from(f.max(1)) }
    }

    /// The instant of `pos` (100 ns units); `None` if the counter fails or
    /// `pos` is in the future.
    fn instant(&self, pos: u64) -> Option<Instant> {
        let mut c = 0i64;
        let now = Instant::now();
        unsafe { QueryPerformanceCounter(&mut c) }.ok()?;
        let now_hns = i128::from(c) * 10_000_000 / self.freq;
        let ago = u64::try_from(now_hns - i128::from(pos)).ok()?;
        now.checked_sub(Duration::from_nanos(ago.saturating_mul(100)))
    }
}

fn drain(s: &Stream, qpc: &Qpc, ring: &Ring, raw: &mut Vec<f32>, buf: &mut Vec<f32>) -> windows::core::Result<()> {
    unsafe {
        while s.capture.GetNextPacketSize()? > 0 {
            let (mut data, mut frames, mut flags, mut qpos) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
            s.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpos))?;
            raw.clear();
            let n = frames as usize * s.block;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                raw.resize(frames as usize * usize::from(s.channels), 0.0);
            } else {
                to_f32(s.kind, std::slice::from_raw_parts(data, n), raw);
            }
            s.capture.ReleaseBuffer(frames)?;
            let len = Duration::from_nanos(u64::from(frames) * 1_000_000_000 / u64::from(s.rate));
            let stamped = (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 && qpos != 0)
                .then(|| qpc.instant(qpos))
                .flatten();
            // Without a usable timestamp: it was just captured.
            let at = stamped.unwrap_or_else(|| Instant::now().checked_sub(len).unwrap_or_else(Instant::now));
            let cont = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 == 0;
            match &s.mix {
                Some(m) => {
                    buf.clear();
                    downmix(m, raw, buf);
                    ring.push(at, buf, cont);
                }
                None => ring.push(at, raw, cont),
            }
        }
    }
    Ok(())
}

/// Active microphones as (endpoint id, friendly name).
pub fn list_mics() -> Vec<(String, String)> {
    in_mta(|| list(eCapture).unwrap_or_default())
}

fn list(flow: windows::Win32::Media::Audio::EDataFlow) -> Result<Vec<(String, String)>> {
    let en = enumerator()?;
    let mut out = Vec::new();
    unsafe {
        let all = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        for i in 0..all.GetCount()? {
            let Ok(dev) = all.Item(i) else { continue };
            let Ok(id) = dev.GetId() else { continue };
            let ids = id.to_string().unwrap_or_default();
            CoTaskMemFree(Some(id.0 as *const _));
            let name = dev
                .OpenPropertyStore(STGM_READ)
                .and_then(|ps| {
                    let mut v = ps.GetValue(&PKEY_Device_FriendlyName)?;
                    let s = PropVariantToStringAlloc(&v);
                    let _ = PropVariantClear(&mut v);
                    let s = s?;
                    let r = s.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(s.0 as *const _));
                    Ok(r)
                })
                .unwrap_or_default();
            let name = if name.is_empty() { ids.clone() } else { name };
            out.push((ids, name));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(tag: u16, ch: u16, rate: u32, bits: u16) -> WAVEFORMATEX {
        let block = ch * bits / 8;
        WAVEFORMATEX {
            wFormatTag: tag,
            nChannels: ch,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: 0,
        }
    }

    #[test]
    fn mix_formats_parse() {
        let float_sub = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
        let pcm_sub = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
        assert_eq!(parse_format(&fmt(0xFFFE, 2, 48000, 32), Some(float_sub)).unwrap(), (Sample::F32, 48000, 2, 8));
        assert_eq!(parse_format(&fmt(0xFFFE, 6, 44100, 24), Some(pcm_sub)).unwrap(), (Sample::I24, 44100, 6, 18));
        assert_eq!(parse_format(&fmt(1, 1, 16000, 16), None).unwrap(), (Sample::I16, 16000, 1, 2));
        assert_eq!(parse_format(&fmt(3, 2, 96000, 32), None).unwrap(), (Sample::F32, 96000, 2, 8));
        assert!(parse_format(&fmt(0xFFFE, 2, 48000, 32), None).is_err());
        assert!(parse_format(&fmt(3, 2, 48000, 64), None).is_err());
        assert!(parse_format(&fmt(1, 0, 48000, 16), None).is_err());
        // What the stream asks for parses as itself.
        let w = stereo_f32();
        assert_eq!(parse_format(&w.Format, Some(w.SubFormat)).unwrap(), (Sample::F32, 48000, 2, 8));
    }

    #[test]
    fn samples_convert_to_f32() {
        let mut out = Vec::new();
        to_f32(Sample::I16, &[0x00, 0x80, 0xFF, 0x7F, 0, 0], &mut out);
        assert_eq!(out, vec![-1.0, 32767.0 / 32768.0, 0.0]);
        out.clear();
        to_f32(Sample::I24, &[0x00, 0x00, 0x80, 0x00, 0x00, 0x40], &mut out);
        assert_eq!(out, vec![-1.0, 0.5]);
        out.clear();
        to_f32(Sample::I32, &(i32::MIN).to_le_bytes(), &mut out);
        assert_eq!(out, vec![-1.0]);
        out.clear();
        to_f32(Sample::F32, &0.25f32.to_le_bytes(), &mut out);
        assert_eq!(out, vec![0.25]);
    }

    const H: f32 = std::f32::consts::FRAC_1_SQRT_2;

    #[test]
    fn downmix_matrix_for_5_1_and_7_1() {
        // 5.1: FL FR FC LFE BL BR.
        let m51 = vec![[1.0, 0.0], [0.0, 1.0], [H, H], [H, H], [H, 0.0], [0.0, H]];
        assert_eq!(downmix_matrix(6, 0x3F), m51);
        // 5.1 (side): FL FR FC LFE SL SR.
        assert_eq!(downmix_matrix(6, 0x60F), m51);
        // No mask: the default 5.1 layout.
        assert_eq!(downmix_matrix(6, 0), m51);
        // 7.1: FL FR FC LFE BL BR SL SR.
        let m71 = downmix_matrix(8, 0x63F);
        assert_eq!(m71, vec![[1.0, 0.0], [0.0, 1.0], [H, H], [H, H], [H, 0.0], [0.0, H], [H, 0.0], [0.0, H]]);
        // A mask naming fewer speakers than channels: the rest are dropped.
        assert_eq!(downmix_matrix(4, 0x3), vec![[1.0, 0.0], [0.0, 1.0], [0.0, 0.0], [0.0, 0.0]]);
        // Applied to frames: FL only stays left; FC goes to both at -3 dB.
        let mut out = Vec::new();
        downmix(&m51, &[0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.2, 0.4], &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(&out[..2], &[0.5, 0.0]);
        assert!((out[2] - (H + 0.2 * H)).abs() < 1e-6 && (out[3] - (H + 0.4 * H)).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn ring_keeps_the_newest_samples_and_reads_up_to_a_discontinuity() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let r = Ring::new(4);
        r.push(t, &[1.0, 2.0], true);
        r.push(t + ms(10), &[3.0, 4.0], true);
        r.push(t + ms(20), &[5.0, 6.0], true);
        // Over the cap: the oldest packet went; what is left is a new run.
        let mut out = Vec::new();
        assert_eq!(r.take(&mut out), (4, Some(t + ms(10))));
        assert_eq!(out, vec![3.0, 4.0, 5.0, 6.0]);
        assert_eq!(r.take(&mut out), (0, None));
        // A discontinuity the device reports splits the reads.
        r.push(t + ms(30), &[7.0], true);
        r.push(t + ms(45), &[8.0], false);
        r.push(t + ms(55), &[9.0], true);
        out.clear();
        assert_eq!(r.take(&mut out), (1, Some(t + ms(30))));
        assert_eq!(r.take(&mut out), (2, Some(t + ms(45))));
        assert_eq!(out, vec![7.0, 8.0, 9.0]);
        assert_eq!(r.lock().len, 0);
    }

    #[test]
    fn qpc_positions_map_to_instants() {
        let q = Qpc::new();
        let mut c = 0i64;
        unsafe { QueryPerformanceCounter(&mut c) }.unwrap();
        let hns = (i128::from(c) * 10_000_000 / q.freq) as u64;
        let now = Instant::now();
        // 20 ms ago.
        let at = q.instant(hns - 200_000).unwrap();
        let ago = now.saturating_duration_since(at);
        assert!(ago >= Duration::from_millis(19) && ago <= Duration::from_millis(25), "{ago:?}");
        assert!(q.instant(hns + 100_000_000).is_none(), "in the future");
    }
}

//! Sound through WASAPI (shared mode): loopback of the default render
//! endpoint (system sound) and capture endpoints (microphones).
//!
//! Each stream runs on its own thread, polling the capture client every
//! 10 ms and converting packets to f32 at the device mix format (whatever
//! rate and channel count the device mixes at; the mixer resamples). The
//! samples go into a ring holding at most one second; when the reader
//! falls behind, the oldest samples are dropped. Loopback delivers no
//! packets while nothing plays: the mixer treats the gap as silence.

use super::{com_init, in_mta};
use crate::rec::AudioSource;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

/// How often the capture thread looks for packets.
const POLL: Duration = Duration::from_millis(10);
/// WASAPI buffer: well above the poll interval.
const BUFFER_HNS: i64 = 2_000_000;

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
    const EXTENSIBLE: u16 = 0xFFFE;
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

/// Bounded hand-off from the capture thread.
struct Ring {
    q: Mutex<VecDeque<f32>>,
    /// Most samples held (one second, set once the format is known).
    cap: AtomicUsize,
}

impl Ring {
    fn new(cap: usize) -> Ring {
        Ring { q: Mutex::new(VecDeque::new()), cap: AtomicUsize::new(cap) }
    }

    fn push(&self, s: &[f32]) {
        let mut q = self.q.lock().unwrap_or_else(|e| e.into_inner());
        q.extend(s);
        let over = q.len().saturating_sub(self.cap.load(Ordering::Relaxed));
        q.drain(..over);
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
        let mut q = self.ring.q.lock().unwrap_or_else(|e| e.into_inner());
        // Whole frames only.
        let n = q.len() / usize::from(self.channels) * usize::from(self.channels);
        out.extend(q.drain(..n));
        n
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
    channels: u16,
    block: usize,
}

fn open_stream(ep: &Endpoint) -> Result<Stream> {
    let en = enumerator()?;
    let dev = device(&en, ep)?;
    unsafe {
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).context("activate the audio client")?;
        let fmt = client.GetMixFormat().context("read the mix format")?;
        let f = std::ptr::read_unaligned(fmt);
        let sub = (f.wFormatTag == 0xFFFE && f.cbSize >= 22)
            .then(|| std::ptr::read_unaligned(fmt as *const WAVEFORMATEXTENSIBLE).SubFormat);
        let parsed = parse_format(&f, sub);
        let flags = if matches!(ep, Endpoint::Loopback) { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
        let init = parsed.as_ref().ok().map(|_| client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_HNS, 0, fmt, None));
        CoTaskMemFree(Some(fmt as *const _));
        let (kind, rate, channels, block) = parsed?;
        init.context("no format")?.context("start the audio stream")?;
        let capture: IAudioCaptureClient = client.GetService().context("get the capture client")?;
        Ok(Stream { client, capture, kind, rate, channels, block })
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
    ring.cap.store(s.rate as usize * usize::from(s.channels), Ordering::Relaxed);
    let _ = ready.send(Ok((s.rate, s.channels)));
    let mut buf = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(POLL);
        if let Err(e) = drain(&s, ring, &mut buf) {
            // Device unplugged or changed: the source goes quiet (silence).
            eprintln!("rustshot: audio capture stopped: {e}");
            break;
        }
    }
    let _ = unsafe { s.client.Stop() };
}

fn drain(s: &Stream, ring: &Ring, buf: &mut Vec<f32>) -> windows::core::Result<()> {
    unsafe {
        while s.capture.GetNextPacketSize()? > 0 {
            let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
            s.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            buf.clear();
            let n = frames as usize * s.block;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                buf.resize(frames as usize * usize::from(s.channels), 0.0);
            } else {
                to_f32(s.kind, std::slice::from_raw_parts(data, n), buf);
            }
            s.capture.ReleaseBuffer(frames)?;
            ring.push(buf);
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

    #[test]
    fn ring_keeps_the_newest_samples() {
        let r = Ring::new(4);
        r.push(&[1.0, 2.0, 3.0]);
        r.push(&[4.0, 5.0, 6.0]);
        assert_eq!(r.q.lock().unwrap().iter().copied().collect::<Vec<_>>(), vec![3.0, 4.0, 5.0, 6.0]);
    }
}

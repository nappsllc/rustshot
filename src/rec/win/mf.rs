//! MP4 through Media Foundation: an `IMFSinkWriter` with an H.264 video
//! stream (NV12 in, hardware encoder when the system has one) and an AAC
//! stream (160 kbps, 48 kHz stereo, PCM16 in).
//!
//! The Media Foundation DLLs are loaded at run time, not linked: Windows
//! "N" editions ship without them (until the Media Feature Pack is
//! installed) and the program must still start there.
//!
//! Timestamps are the frame / audio timestamps in 100-ns units. Video times
//! that do not increase are nudged 1 ms past the previous frame. The audio
//! track is kept contiguous: overlaps are trimmed and gaps filled with
//! silence, and when no audio arrives the track is filled with silence to
//! keep pace with the video (the sink writer interleaves the two streams).

use super::{com_init, in_mta, nv12};
use crate::rec::{Frame, Quality, VideoEncoder};
use anyhow::{anyhow, bail, Context, Result};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;
use windows::core::{Interface, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

/// Audio sample rate and channels the encoder takes (the mixer's output).
const RATE: u64 = 48_000;
const CHANNELS: usize = 2;
/// 100-ns units per second.
const HNS: i64 = 10_000_000;
/// AAC bit rate: 160 kbps = 20000 bytes/s (one of the four the Microsoft
/// AAC encoder accepts: 12000, 16000, 20000, 24000).
const AAC_BYTES_PER_SEC: u32 = 20_000;

/// Video bit rate for a `w`×`h` picture at `fps`: bits per pixel per frame
/// of 0.05 (low), 0.1 (medium), 0.2 (high), within 0.3-60 Mbit/s.
/// At 1920×1080 and 30 fps that is about 3.1 / 6.2 / 12.4 Mbit/s; screen
/// content (large flat areas) looks clean from medium up.
pub fn bitrate(w: u32, h: u32, fps: u32, q: Quality) -> u32 {
    let bpp = match q {
        Quality::Low => 0.05,
        Quality::Medium => 0.1,
        Quality::High => 0.2,
    };
    let bits = f64::from(w) * f64::from(h) * f64::from(fps.max(1)) * bpp;
    bits.clamp(300_000.0, 60_000_000.0) as u32
}

type FnStartup = unsafe extern "system" fn(u32, u32) -> HRESULT;
type FnCreateAttributes = unsafe extern "system" fn(*mut *mut c_void, u32) -> HRESULT;
type FnCreate = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
type FnCreateMemoryBuffer = unsafe extern "system" fn(u32, *mut *mut c_void) -> HRESULT;
type FnCreateSinkWriter = unsafe extern "system" fn(PCWSTR, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT;
type FnCreateSourceReader = unsafe extern "system" fn(PCWSTR, *mut c_void, *mut *mut c_void) -> HRESULT;

/// The Media Foundation entry points used here.
struct Api {
    create_attributes: FnCreateAttributes,
    create_media_type: FnCreate,
    create_sample: FnCreate,
    create_memory_buffer: FnCreateMemoryBuffer,
    create_sink_writer: FnCreateSinkWriter,
    create_source_reader: FnCreateSourceReader,
}

/// Load the DLLs and call `MFStartup` once per process (never shut down).
fn api() -> Result<&'static Api> {
    static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| unsafe { load() }).as_ref().map_err(|e| anyhow!("{e}"))
}

unsafe fn load() -> std::result::Result<Api, String> {
    unsafe {
        let missing = |dll: &str| {
            format!("Media Foundation is not available ({dll} is missing; on Windows N install the Media Feature Pack)")
        };
        let plat = LoadLibraryExW(windows::core::w!("mfplat.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
            .map_err(|_| missing("mfplat.dll"))?;
        let rw = LoadLibraryExW(windows::core::w!("mfreadwrite.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
            .map_err(|_| missing("mfreadwrite.dll"))?;
        macro_rules! sym {
            ($m:expr, $name:literal, $t:ty) => {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, $t>(
                    GetProcAddress($m, windows::core::s!($name)).ok_or_else(|| format!("{} not found", $name))?,
                )
            };
        }
        let startup: FnStartup = sym!(plat, "MFStartup", FnStartup);
        let api = Api {
            create_attributes: sym!(plat, "MFCreateAttributes", FnCreateAttributes),
            create_media_type: sym!(plat, "MFCreateMediaType", FnCreate),
            create_sample: sym!(plat, "MFCreateSample", FnCreate),
            create_memory_buffer: sym!(plat, "MFCreateMemoryBuffer", FnCreateMemoryBuffer),
            create_sink_writer: sym!(rw, "MFCreateSinkWriterFromURL", FnCreateSinkWriter),
            create_source_reader: sym!(rw, "MFCreateSourceReaderFromURL", FnCreateSourceReader),
        };
        startup(MF_VERSION, MFSTARTUP_FULL).ok().map_err(|e| format!("MFStartup: {e}"))?;
        Ok(api)
    }
}

/// Whether Media Foundation can be loaded on this system.
pub fn available() -> bool {
    api().is_ok()
}

/// Call a creator that writes an interface pointer and wrap it.
unsafe fn make<T: Interface>(f: impl FnOnce(*mut *mut c_void) -> HRESULT) -> Result<T> {
    let mut p = std::ptr::null_mut();
    f(&mut p).ok()?;
    if p.is_null() {
        bail!("Media Foundation returned no object");
    }
    Ok(unsafe { T::from_raw(p) })
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn attributes(api: &Api, n: u32) -> Result<IMFAttributes> {
    unsafe { make(|p| (api.create_attributes)(p, n)) }
}

fn media_type(api: &Api) -> Result<IMFMediaType> {
    unsafe { make(|p| (api.create_media_type)(p)) }
}

fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn video_type(api: &Api, subtype: &GUID, w: u32, h: u32, fps: u32) -> Result<IMFMediaType> {
    let t = media_type(api)?;
    unsafe {
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        // What nv12::convert produces: BT.709, limited range.
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
    }
    Ok(t)
}

fn audio_type(api: &Api, subtype: &GUID, bytes_per_sec: u32) -> Result<IMFMediaType> {
    let t = media_type(api)?;
    unsafe {
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE as u32)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_sec)?;
    }
    Ok(t)
}

/// An MP4 being written. Created on [`crate::rec::RecSpec::part_path`];
/// the container type is set explicitly because that name ends in `.part`.
pub struct MfEncoder {
    writer: IMFSinkWriter,
    video: u32,
    audio: Option<u32>,
    in_w: u32,
    in_h: u32,
    out_w: u32,
    out_h: u32,
    frame_dur: i64,
    /// Time of the last video frame written (100 ns).
    last_video: Option<i64>,
    /// Audio written so far, in 48 kHz frames from time zero.
    audio_pos: u64,
    nv12: Vec<u8>,
    scaled: Vec<u8>,
    pcm16: Vec<u8>,
    encoder_name: String,
    hardware: bool,
    /// A write failed: the file is incomplete and must not be kept.
    poisoned: bool,
}

// SAFETY: Media Foundation's sink writer is free-threaded; the encoder is
// used by one thread at a time (created on an MTA helper thread, then owned
// by the session's encode thread, which joins the MTA before using it).
unsafe impl Send for MfEncoder {}

impl MfEncoder {
    /// Start an MP4 at `path` for `w`×`h` frames at `fps` (frames larger
    /// than 4096×2304 are scaled to fit; sizes rounded down to even).
    /// Tries the hardware encoder first and the software one after.
    pub fn create(path: &Path, w: u32, h: u32, fps: u32, quality: Quality, audio: bool) -> Result<MfEncoder> {
        if w == 0 || h == 0 {
            bail!("empty recording area");
        }
        let api = api()?;
        let fps = fps.clamp(1, 60);
        let (out_w, out_h) = nv12::fit(w, h);
        let path = path.to_path_buf();
        let made = in_mta(move || -> Result<SendWriter> {
            match open_writer(api, &path, out_w, out_h, fps, quality, audio, true) {
                Ok(s) => Ok(s),
                Err(hw) => open_writer(api, &path, out_w, out_h, fps, quality, audio, false)
                    .map_err(|e| e.context(format!("hardware encoder: {hw:#}"))),
            }
        })
        .with_context(|| format!("start the MP4 encoder ({out_w}x{out_h} @ {fps} fps)"))?;
        let SendWriter(writer, video, audio, encoder_name, hardware) = made;
        Ok(MfEncoder {
            writer,
            video,
            audio,
            in_w: w,
            in_h: h,
            out_w,
            out_h,
            frame_dur: HNS / i64::from(fps),
            last_video: None,
            audio_pos: 0,
            nv12: Vec::new(),
            scaled: Vec::new(),
            pcm16: Vec::new(),
            encoder_name,
            hardware,
            poisoned: false,
        })
    }

    /// Encoded picture size.
    pub fn out_size(&self) -> (u32, u32) {
        (self.out_w, self.out_h)
    }

    /// Whether the H.264 encoder in use is a hardware MFT.
    pub fn hardware(&self) -> bool {
        self.hardware
    }

    /// The video encoder's friendly name (when it reports one).
    pub fn encoder_name(&self) -> &str {
        &self.encoder_name
    }

    fn sample(&self, bytes: &[u8], time: i64, dur: i64) -> Result<IMFSample> {
        let api = api()?;
        unsafe {
            let buf: IMFMediaBuffer = make(|p| (api.create_memory_buffer)(bytes.len() as u32, p))?;
            let mut data = std::ptr::null_mut();
            buf.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            buf.Unlock()?;
            buf.SetCurrentLength(bytes.len() as u32)?;
            let s: IMFSample = make(|p| (api.create_sample)(p))?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(time)?;
            s.SetSampleDuration(dur)?;
            Ok(s)
        }
    }

    fn write(&mut self, stream: u32, s: &IMFSample) -> Result<()> {
        if let Err(e) = unsafe { self.writer.WriteSample(stream, s) } {
            self.poisoned = true;
            return Err(anyhow::Error::new(e).context("write to the MP4"));
        }
        Ok(())
    }

    /// Append 48 kHz stereo `pcm` at the current audio position.
    fn write_pcm(&mut self, pcm: &[f32]) -> Result<()> {
        let Some(stream) = self.audio else { return Ok(()) };
        let frames = (pcm.len() / CHANNELS) as u64;
        if frames == 0 {
            return Ok(());
        }
        let mut bytes = std::mem::take(&mut self.pcm16);
        bytes.clear();
        bytes.extend(pcm[..frames as usize * CHANNELS].iter().flat_map(|&s| {
            ((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()
        }));
        let t0 = (self.audio_pos * HNS as u64 / RATE) as i64;
        let t1 = ((self.audio_pos + frames) * HNS as u64 / RATE) as i64;
        let r = self.sample(&bytes, t0, t1 - t0).and_then(|s| self.write(stream, &s));
        self.pcm16 = bytes;
        r?;
        self.audio_pos += frames;
        Ok(())
    }

    /// Silence until the audio position reaches `frames`.
    fn fill_silence(&mut self, frames: u64) -> Result<()> {
        const CHUNK: u64 = RATE / 10;
        let zeros = [0f32; CHUNK as usize * CHANNELS];
        while self.audio_pos < frames {
            let n = (frames - self.audio_pos).min(CHUNK) as usize;
            self.write_pcm(&zeros[..n * CHANNELS])?;
        }
        Ok(())
    }

    fn video_end(&self) -> i64 {
        self.last_video.map_or(0, |t| t + self.frame_dur)
    }
}

/// What [`open_writer`] hands back across the MTA thread.
struct SendWriter(IMFSinkWriter, u32, Option<u32>, String, bool);
// SAFETY: see `MfEncoder`.
unsafe impl Send for SendWriter {}

#[allow(clippy::too_many_arguments)]
fn open_writer(
    api: &Api,
    path: &Path,
    w: u32,
    h: u32,
    fps: u32,
    quality: Quality,
    audio: bool,
    hardware: bool,
) -> Result<SendWriter> {
    let attrs = attributes(api, 2)?;
    let url = wide(path);
    let writer: IMFSinkWriter = unsafe {
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, u32::from(hardware))?;
        attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        make(|p| (api.create_sink_writer)(PCWSTR(url.as_ptr()), std::ptr::null_mut(), attrs.as_raw(), p))
            .with_context(|| format!("create {}", path.display()))?
    };
    unsafe {
        let out = video_type(api, &MFVideoFormat_H264, w, h, fps)?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate(w, h, fps, quality))?;
        out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
        let video = writer.AddStream(&out).context("add the H.264 stream")?;
        let input = video_type(api, &MFVideoFormat_NV12, w, h, fps)?;
        input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w)?;
        writer.SetInputMediaType(video, &input, None).context("set the NV12 input")?;
        let audio = if audio {
            let out = audio_type(api, &MFAudioFormat_AAC, AAC_BYTES_PER_SEC)?;
            let a = writer.AddStream(&out).context("add the AAC stream")?;
            let input = audio_type(api, &MFAudioFormat_PCM, RATE as u32 * CHANNELS as u32 * 2)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, CHANNELS as u32 * 2)?;
            input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            writer.SetInputMediaType(a, &input, None).context("set the PCM input")?;
            Some(a)
        } else {
            None
        };
        writer.BeginWriting().context("begin writing")?;
        let (name, hw) = encoder_info(&writer, video);
        Ok(SendWriter(writer, video, audio, name, hw))
    }
}

/// The video encoder MFT the sink writer chose: (friendly name, hardware).
fn encoder_info(writer: &IMFSinkWriter, stream: u32) -> (String, bool) {
    let Ok(ex) = writer.cast::<IMFSinkWriterEx>() else { return (String::new(), false) };
    for i in 0..8 {
        let mut cat = GUID::zeroed();
        let mut mft = None;
        if unsafe { ex.GetTransformForStream(stream, i, Some(&mut cat), &mut mft) }.is_err() {
            break;
        }
        if cat != MFT_CATEGORY_VIDEO_ENCODER {
            continue;
        }
        let Some(attrs) = mft.and_then(|m| unsafe { m.GetAttributes() }.ok()) else { break };
        // Hardware MFTs carry the URL of their device.
        let hw = unsafe { attrs.GetItem(&MFT_ENUM_HARDWARE_URL_Attribute, None) }.is_ok();
        let name = unsafe { alloc_string(&attrs, &MFT_FRIENDLY_NAME_Attribute) }.unwrap_or_default();
        return (name, hw);
    }
    (String::new(), false)
}

unsafe fn alloc_string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    unsafe {
        let mut p = PWSTR::null();
        let mut n = 0u32;
        attrs.GetAllocatedString(key, &mut p, &mut n).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

impl VideoEncoder for MfEncoder {
    fn push_video(&mut self, f: &Frame) -> Result<()> {
        if self.poisoned {
            bail!("the MP4 is incomplete after an earlier write error");
        }
        if (f.w, f.h) != (self.in_w, self.in_h) || f.bgra.len() != f.w as usize * f.h as usize * 4 {
            bail!("frame is {}x{} ({} bytes); the recording is {}x{}", f.w, f.h, f.bgra.len(), self.in_w, self.in_h);
        }
        com_init();
        let mut t = i64::try_from(f.ts.as_nanos() / 100).unwrap_or(i64::MAX);
        if let Some(last) = self.last_video
            && t <= last
        {
            t = last + 10_000;
        }
        // No audio arriving: keep the audio track within a second of the video.
        if self.audio.is_some() {
            let behind = t - (self.audio_pos * HNS as u64 / RATE) as i64;
            if behind > HNS {
                self.fill_silence((t - HNS / 2).max(0) as u64 * RATE / HNS as u64)?;
            }
        }
        let mut nv = std::mem::take(&mut self.nv12);
        nv12::convert_into(&f.bgra, f.w, f.h, self.out_w, self.out_h, &mut nv, &mut self.scaled);
        let r = self.sample(&nv, t, self.frame_dur).and_then(|s| self.write(self.video, &s));
        self.nv12 = nv;
        r?;
        self.last_video = Some(t);
        Ok(())
    }

    fn push_audio(&mut self, pcm: &[f32], ts: Duration) -> Result<()> {
        if self.audio.is_none() {
            return Ok(());
        }
        if self.poisoned {
            bail!("the MP4 is incomplete after an earlier write error");
        }
        com_init();
        let start = (ts.as_nanos() * u128::from(RATE) / 1_000_000_000) as u64;
        // Rounding of the caller's timestamps is not a gap.
        let start = if start.abs_diff(self.audio_pos) <= 2 { self.audio_pos } else { start };
        let frames = (pcm.len() / CHANNELS) as u64;
        if start + frames <= self.audio_pos {
            return Ok(()); // already covered (by filler)
        }
        if start > self.audio_pos {
            self.fill_silence(start)?;
        }
        let skip = (self.audio_pos - start.min(self.audio_pos)) as usize * CHANNELS;
        self.write_pcm(&pcm[skip..])
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        if self.poisoned {
            bail!("the MP4 is incomplete after a write error");
        }
        com_init();
        if self.audio.is_some() {
            let end = self.video_end().max(0) as u64 * RATE / HNS as u64;
            self.fill_silence(end)?;
        }
        unsafe { self.writer.Finalize() }.context("finalise the MP4")
    }

    fn audio(&self) -> bool {
        self.audio.is_some()
    }
}

/// Facts about an MP4, read back with an `IMFSourceReader` (tests, and a
/// sanity check for callers that want one).
#[derive(Debug, Default)]
pub struct Probe {
    pub duration: Duration,
    pub frames: usize,
    pub size: (u32, u32),
    pub has_audio: bool,
    /// Luma at the picture centre of each decoded frame.
    pub center_luma: Vec<u8>,
}

/// A source reader on `path` (call on an MTA thread).
pub(crate) fn open_reader(path: &Path) -> Result<IMFSourceReader> {
    let api = api()?;
    let url = wide(path);
    unsafe { make(|p| (api.create_source_reader)(PCWSTR(url.as_ptr()), std::ptr::null_mut(), p)) }
        .with_context(|| format!("open {}", path.display()))
}

/// An empty media type.
pub(crate) fn new_media_type() -> Result<IMFMediaType> {
    media_type(api()?)
}

/// Open `path` with a source reader, decode every video frame to NV12.
pub fn probe(path: &Path) -> Result<Probe> {
    let api = api()?;
    let path = path.to_path_buf();
    in_mta(move || unsafe {
        let reader = open_reader(&path)?;
        let mut out = Probe::default();
        let mut pv = reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)?;
        let hns = windows::Win32::System::Com::StructuredStorage::PropVariantToUInt64(&pv)?;
        let _ = windows::Win32::System::Com::StructuredStorage::PropVariantClear(&mut pv);
        out.duration = Duration::from_nanos(hns * 100);
        let first_video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        out.has_audio = reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32).is_ok();
        let want = media_type(api)?;
        want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        want.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        reader.SetCurrentMediaType(first_video, None, &want)?;
        let cur = reader.GetCurrentMediaType(first_video)?;
        let fs = cur.GetUINT64(&MF_MT_FRAME_SIZE)?;
        out.size = ((fs >> 32) as u32, fs as u32);
        let stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or(out.size.0 as usize, |s| s as i32 as usize);
        loop {
            let mut flags = 0u32;
            let mut sample = None;
            reader.ReadSample(first_video, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if let Some(s) = sample {
                out.frames += 1;
                let buf = s.ConvertToContiguousBuffer()?;
                let mut data = std::ptr::null_mut();
                let mut len = 0u32;
                buf.Lock(&mut data, None, Some(&mut len))?;
                let at = (out.size.1 as usize / 2) * stride + out.size.0 as usize / 2;
                if at < len as usize {
                    out.center_luma.push(*data.add(at));
                }
                buf.Unlock()?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrates_scale_with_quality_and_size() {
        assert_eq!(bitrate(1920, 1080, 30, Quality::Medium), 6_220_800);
        assert_eq!(bitrate(1920, 1080, 30, Quality::Low), 3_110_400);
        assert_eq!(bitrate(1920, 1080, 30, Quality::High), 12_441_600);
        assert_eq!(bitrate(64, 64, 10, Quality::Low), 300_000);
        assert_eq!(bitrate(4096, 2304, 60, Quality::High), 60_000_000);
    }
}

//! Windows recording back end: frames from DXGI Desktop Duplication (GDI
//! polling as the fallback), MP4 through Media Foundation, sound through
//! WASAPI.

pub mod dxgi;
pub mod gdi_poll;
pub mod mf;
pub mod nv12;
pub mod wasapi;

// The recording UI (video Task 3) uses these; remove the allow then.
#[allow(unused_imports)]
pub use dxgi::DxgiSource;
#[allow(unused_imports)]
pub use gdi_poll::GdiPollSource;
#[allow(unused_imports)]
pub use mf::MfEncoder;
#[allow(unused_imports)]
pub use wasapi::{WasapiCapture, WasapiLoopback};

use super::FrameSource;
use crate::capture::IRect;
use anyhow::Result;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Which capture path [`open_source`] picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Dxgi,
    Gdi,
}

/// Frames of `area` (physical virtual-screen pixels): Desktop Duplication
/// when it works, GDI polling otherwise (RDP sessions, some drivers,
/// rotated outputs).
pub fn open_source(area: IRect) -> Result<(Box<dyn FrameSource>, SourceKind)> {
    match DxgiSource::open(area) {
        Ok(s) => Ok((Box::new(s), SourceKind::Dxgi)),
        Err(e) => {
            eprintln!("rustshot: desktop duplication unavailable ({e:#}); recording with GDI");
            Ok((Box::new(GdiPollSource::open(area)?), SourceKind::Gdi))
        }
    }
}

/// Join the multithreaded COM apartment on this thread (once). Only called
/// on threads the recorder owns, so a caller's STA is never touched.
pub(crate) fn com_init() {
    thread_local! {
        static INIT: () = {
            // S_FALSE (already) and RPC_E_CHANGED_MODE are both fine: the
            // objects used here are free-threaded.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        };
    }
    INIT.with(|_| ());
}

/// Run `f` on a short-lived MTA thread, so COM objects are created without
/// touching the caller's apartment (the caller may be the UI thread).
pub(crate) fn in_mta<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        s.spawn(|| {
            com_init();
            f()
        })
        .join()
        .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

#[cfg(test)]
mod tests;

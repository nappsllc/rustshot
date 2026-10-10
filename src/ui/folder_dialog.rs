//! Native "choose a folder" dialog for the Saving tab's Browse… button.
//! Blocking; None when cancelled or unavailable. Windows: IFileOpenDialog
//! with FOS_PICKFOLDERS (COM, apartment-threaded on the calling thread).
//! Linux: zenity, then kdialog. macOS: NSOpenPanel (directories only).

use std::path::{Path, PathBuf};

#[cfg(windows)]
use windows::Win32::UI::Shell::IFileOpenDialog;

/// Balances a successful CoInitializeEx (S_OK or S_FALSE); after
/// RPC_E_CHANGED_MODE (an MTA thread) there is nothing to undo.
#[cfg(windows)]
struct ComGuard(bool);

#[cfg(windows)]
impl ComGuard {
    fn sta() -> ComGuard {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
        ComGuard(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok())
    }
}

#[cfg(windows)]
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

/// A folder-picking IFileOpenDialog opened in `initial` (when it exists).
#[cfg(windows)]
fn folder_dialog(initial: &Path) -> Option<IFileOpenDialog> {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST,
        FOS_PICKFOLDERS,
    };
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST).ok()?;
        if initial.is_dir() {
            let p = HSTRING::from(initial.as_os_str());
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&p, None) {
                let _ = dlg.SetFolder(&item);
            }
        }
        Some(dlg)
    }
}

/// Ask for a folder, starting in `initial` (when it exists), modal to `owner`.
#[cfg(windows)]
pub fn pick_folder(owner: Option<crate::wind::Hwnd>, initial: &Path) -> Option<PathBuf> {
    use windows::core::PWSTR;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::SIGDN_FILESYSPATH;
    let _com = ComGuard::sta();
    let dlg = folder_dialog(initial)?;
    unsafe {
        dlg.Show(owner).ok()?; // HRESULT_FROM_WIN32(ERROR_CANCELLED) on cancel
        let item = dlg.GetResult().ok()?;
        let name: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path.map(PathBuf::from)
    }
}

/// Ask for a folder, starting in `initial`: zenity (GTK) first, then
/// kdialog (KDE); a missing binary falls through to the next.
#[cfg(target_os = "linux")]
pub fn pick_folder(_owner: Option<crate::wind::Hwnd>, initial: &Path) -> Option<PathBuf> {
    use std::process::Command;
    // zenity treats a trailing slash as "start inside this folder".
    let start = format!("{}/", initial.display());
    let zenity =
        Command::new("zenity").args(["--file-selection", "--directory"]).arg(format!("--filename={start}")).output();
    if let Ok(out) = zenity {
        return finish(out);
    }
    let kdialog = Command::new("kdialog").arg("--getexistingdirectory").arg(initial).output();
    kdialog.ok().and_then(finish)
}

#[cfg(target_os = "linux")]
fn finish(out: std::process::Output) -> Option<PathBuf> {
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim_end_matches(['\r', '\n']);
    (out.status.success() && !text.is_empty()).then(|| PathBuf::from(text))
}

/// Ask for a folder with `NSOpenPanel` (directories only, may create).
/// Must run on the main thread, like every AppKit panel.
#[cfg(target_os = "macos")]
pub fn pick_folder(_owner: Option<crate::wind::Hwnd>, initial: &Path) -> Option<PathBuf> {
    use crate::wind::{msg0, msg1, ns_string, objc_cls, objc_sel};
    use core::ffi::{c_char, c_void, CStr};
    unsafe {
        let app = objc_cls(c"NSApplication");
        if app.is_null() {
            return None;
        }
        let _: *mut c_void = msg0(app, objc_sel(c"sharedApplication"));
        let cls = objc_cls(c"NSOpenPanel");
        if cls.is_null() {
            return None;
        }
        let panel: *mut c_void = msg0(cls, objc_sel(c"openPanel"));
        if panel.is_null() {
            return None;
        }
        let _: () = msg1(panel, objc_sel(c"setCanChooseDirectories:"), 1i8);
        let _: () = msg1(panel, objc_sel(c"setCanChooseFiles:"), 0i8);
        let _: () = msg1(panel, objc_sel(c"setAllowsMultipleSelection:"), 0i8);
        let _: () = msg1(panel, objc_sel(c"setCanCreateDirectories:"), 1i8);
        let url: *mut c_void =
            msg1(objc_cls(c"NSURL"), objc_sel(c"fileURLWithPath:"), ns_string(&initial.to_string_lossy()));
        if !url.is_null() {
            let _: () = msg1(panel, objc_sel(c"setDirectoryURL:"), url);
        }
        let code: i64 = msg0(panel, objc_sel(c"runModal"));
        if code != 1 {
            return None; // NSModalResponseOK
        }
        let url: *mut c_void = msg0(panel, objc_sel(c"URL"));
        if url.is_null() {
            return None;
        }
        let path: *mut c_void = msg0(url, objc_sel(c"path"));
        if path.is_null() {
            return None;
        }
        let utf8: *const c_char = msg0(path, objc_sel(c"UTF8String"));
        if utf8.is_null() {
            return None;
        }
        Some(PathBuf::from(CStr::from_ptr(utf8).to_string_lossy().into_owned()))
    }
}

#[cfg(test)]
mod tests {
    /// The dialog object is created with the folder-picking options
    /// (without showing it).
    #[cfg(windows)]
    #[test]
    fn windows_dialog_is_a_folder_picker() {
        use windows::Win32::UI::Shell::FOS_PICKFOLDERS;
        let _com = super::ComGuard::sta();
        let dlg = super::folder_dialog(&std::env::temp_dir()).expect("IFileOpenDialog");
        let opts = unsafe { dlg.GetOptions() }.unwrap();
        assert!(opts.contains(FOS_PICKFOLDERS));
        let dir = unsafe { dlg.GetFolder() }.expect("initial folder set");
        drop(dir);
    }

    #[test]
    #[ignore = "opens the native folder dialog; pick a folder or cancel"]
    fn interactive_pick_folder() {
        let r = super::pick_folder(None, &std::env::temp_dir());
        eprintln!("picked: {r:?}");
    }
}

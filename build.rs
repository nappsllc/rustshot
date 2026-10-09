// Embeds the app icon (resource ID 1) and version info into rustshot.exe.
// Windows MSVC targets only; never fails the build (a missing rc.exe just
// means no icon).
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=packaging/icons/rustshot.ico");
    println!("cargo:rerun-if-env-changed=RC");
    // Check the *target*, not the host: cross-target checks run build.rs on the host.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }
    if let Err(e) = embed() {
        println!("cargo:warning=rustshot: not embedding icon/version info: {e}");
    }
}

fn embed() -> Result<(), String> {
    let rc = find_rc().ok_or("rc.exe not found (Windows SDK)")?;
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let ico = manifest.join("packaging").join("icons").join("rustshot.ico");
    if !ico.exists() {
        return Err(format!("{} missing", ico.display()));
    }
    let ico = ico.to_string_lossy().replace('\\', "\\\\");
    let ver = env::var("CARGO_PKG_VERSION").map_err(|e| e.to_string())?;
    let num = |k: &str| env::var(k).unwrap_or_else(|_| "0".into());
    let (maj, min, pat) = (
        num("CARGO_PKG_VERSION_MAJOR"),
        num("CARGO_PKG_VERSION_MINOR"),
        num("CARGO_PKG_VERSION_PATCH"),
    );
    let rc_src = format!(
        r#"1 ICON "{ico}"

1 VERSIONINFO
FILEVERSION {maj},{min},{pat},0
PRODUCTVERSION {maj},{min},{pat},0
FILEOS 0x4
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "nappsllc"
      VALUE "FileDescription", "rustshot"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "rustshot"
      VALUE "LegalCopyright", "GPL-3.0-only"
      VALUE "OriginalFilename", "rustshot.exe"
      VALUE "ProductName", "rustshot"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let rc_path = out_dir.join("rustshot.rc");
    let res_path = out_dir.join("rustshot.res");
    fs::write(&rc_path, rc_src).map_err(|e| e.to_string())?;
    let out = Command::new(&rc)
        .args(["/nologo", "/fo"])
        .arg(&res_path)
        .arg(&rc_path)
        .output()
        .map_err(|e| format!("running {}: {e}", rc.display()))?;
    if !out.status.success() {
        return Err(format!(
            "rc.exe failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    println!("cargo:rustc-link-arg-bins={}", res_path.display());
    Ok(())
}

fn find_rc() -> Option<PathBuf> {
    if let Some(p) = env::var_os("RC") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let pf = env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
    let bin = pf.join("Windows Kits").join("10").join("bin");
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for e in fs::read_dir(&bin).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("10.") {
            continue;
        }
        let rc = e.path().join("x64").join("rc.exe");
        if !Path::new(&rc).exists() {
            continue;
        }
        let key: Vec<u32> = name.split('.').filter_map(|p| p.parse().ok()).collect();
        if best.as_ref().is_none_or(|(k, _)| key > *k) {
            best = Some((key, rc));
        }
    }
    best.map(|(_, p)| p)
}

// Bakes the UI font (Inter subset) into static tables, and embeds the app
// icon (resource ID 1) and version info into rustshot.exe. The icon is
// Windows MSVC targets only and never fails the build (a missing rc.exe
// just means no icon).
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=packaging/icons/rustshot.ico");
    println!("cargo:rerun-if-env-changed=RC");
    bake_font();
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
      VALUE "FileDescription", "Rustshot"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "rustshot"
      VALUE "LegalCopyright", "GPL-3.0-only"
      VALUE "OriginalFilename", "rustshot.exe"
      VALUE "ProductName", "Rustshot"
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

/// Bake the embedded Inter subset into `$OUT_DIR/font_baked.rs`: metrics,
/// cmap, advances, bounding boxes, kerning pairs and outlines (lines and
/// quadratics in half font units, exactly the curves ab_glyph produces), so
/// the UI font needs no font parser at run time. `OPS` packs 2-bit segment
/// codes four per byte (0 move, 1 line, 2 quad); `PTS` holds the x, y
/// pairs they consume (1, 1 and 2 points).
fn bake_font() {
    use ab_glyph::{Font, FontRef, GlyphId, OutlineCurve, Point};
    use std::fmt::Write as _;
    const SRC: &str = "assets/fonts/Inter-Medium.subset.ttf";
    println!("cargo:rerun-if-changed={SRC}");
    let bytes = fs::read(SRC).expect("read the Inter subset");
    let font = FontRef::try_from_slice(&bytes).expect("parse the Inter subset");

    let mut cmap: Vec<(char, GlyphId)> = font
        .codepoint_ids()
        .filter(|(g, _)| g.0 != 0)
        .map(|(g, c)| (c, g))
        .collect();
    cmap.sort_by_key(|&(c, _)| c);
    cmap.dedup_by_key(|&mut (c, _)| c);
    // Slots: .notdef first (what unmapped characters draw), then each
    // mapped glyph once.
    let mut gids: Vec<GlyphId> = vec![GlyphId(0)];
    for &(c, g) in &cmap {
        assert_eq!(font.glyph_id(c), g, "cmap mismatch for {c:?}");
        if !gids.contains(&g) {
            gids.push(g);
        }
    }
    assert!(gids.len() <= 256, "more than 256 glyphs: widen the slot type");
    let slot = |g: GlyphId| gids.iter().position(|&x| x == g).unwrap();
    let int = |v: f32| -> i16 {
        assert!(v.fract() == 0.0 && v.abs() < 32767.0, "value {v} is not an i16");
        v as i16
    };
    // Half units: implied on-curve points are midpoints of integer points.
    let half = |p: Point, pts: &mut Vec<i16>| {
        pts.push(int(p.x * 2.0));
        pts.push(int(p.y * 2.0));
    };

    let mut glyphs = String::new();
    let mut ops: Vec<u8> = Vec::new();
    let mut pts: Vec<i16> = Vec::new();
    for &g in &gids {
        let adv = font.h_advance_unscaled(g);
        assert!(adv.fract() == 0.0 && (0.0..65535.0).contains(&adv), "advance {adv}");
        let (op0, pt0) = (ops.len(), pts.len());
        let bbox = match font.outline(g) {
            Some(o) => {
                let mut cur: Option<Point> = None;
                for c in &o.curves {
                    let (p0, end) = match *c {
                        OutlineCurve::Line(a, b) | OutlineCurve::Quad(a, _, b) => (a, b),
                        OutlineCurve::Cubic(..) => panic!("cubic curve in a TrueType font"),
                    };
                    if cur != Some(p0) {
                        ops.push(0);
                        half(p0, &mut pts);
                    }
                    match *c {
                        OutlineCurve::Line(_, b) => {
                            ops.push(1);
                            half(b, &mut pts);
                        }
                        OutlineCurve::Quad(_, ctl, b) => {
                            ops.push(2);
                            half(ctl, &mut pts);
                            half(b, &mut pts);
                        }
                        OutlineCurve::Cubic(..) => unreachable!(),
                    }
                    cur = Some(end);
                }
                // ab_glyph's bounds are (x_min, y_max)..(x_max, y_min).
                let b = o.bounds;
                [int(b.min.x), int(b.min.y), int(b.max.x), int(b.max.y)]
            }
            None => [0; 4],
        };
        writeln!(
            glyphs,
            "    G {{ adv: {adv}, bbox: {bbox:?}, op: {op0}, n_ops: {}, pt: {} }},",
            ops.len() - op0,
            pt0 / 2
        )
        .unwrap();
    }
    assert!(ops.len() < 65536 && pts.len() / 2 < 65536);

    let mut kern = Vec::new();
    for (i, &a) in gids.iter().enumerate() {
        for (j, &b) in gids.iter().enumerate() {
            let k = font.kern_unscaled(a, b);
            if k != 0.0 {
                kern.push((i, j, int(k)));
            }
        }
    }

    let mut out = String::new();
    let w = &mut out;
    writeln!(w, "// @generated by build.rs from {SRC}; do not edit.").unwrap();
    writeln!(w, "pub const UNITS_PER_EM: u16 = {};", font.units_per_em().unwrap() as u16).unwrap();
    writeln!(w, "pub const ASCENT: i16 = {};", int(font.ascent_unscaled())).unwrap();
    writeln!(w, "pub const DESCENT: i16 = {};", int(font.descent_unscaled())).unwrap();
    writeln!(w, "pub const LINE_GAP: i16 = {};", int(font.line_gap_unscaled())).unwrap();
    writeln!(w, "/// (codepoint, glyph slot), sorted by codepoint.").unwrap();
    writeln!(w, "pub static CMAP: [(char, u8); {}] = [", cmap.len()).unwrap();
    for &(c, g) in &cmap {
        writeln!(w, "    ({c:?}, {}),", slot(g)).unwrap();
    }
    writeln!(w, "];\npub static GLYPHS: [G; {}] = [\n{glyphs}];", gids.len()).unwrap();
    writeln!(w, "/// (left slot, right slot, adjustment), sorted.").unwrap();
    writeln!(w, "pub static KERN: [(u8, u8, i16); {}] = {kern:?};", kern.len()).unwrap();
    let packed: Vec<u8> = ops
        .chunks(4)
        .map(|c| c.iter().enumerate().fold(0u8, |acc, (i, &o)| acc | (o << (2 * i))))
        .collect();
    writeln!(w, "pub static OPS: [u8; {}] = {packed:?};", packed.len()).unwrap();
    writeln!(w, "pub static PTS: [i16; {}] = {pts:?};", pts.len()).unwrap();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("font_baked.rs"), out).expect("write font_baked.rs");
}

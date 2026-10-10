//! Update check against GitHub releases, plus the logic layer for in-app
//! updates: release assets and notes, install-kind detection, asset choice,
//! `SHA256SUMS` and a verified download.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// Release notes (markdown body); see `notes_excerpt`.
    #[allow(dead_code)] // shown by the update dialog (later task)
    pub notes: String,
    /// Downloadable files; only ones whose URL passes `is_safe_download_url`.
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

const RELEASES_HOST: &str = "api.github.com";
const RELEASES_PATH: &str = "/repos/nappsllc/rustshot/releases/latest";
pub const RELEASES_PREFIX: &str = "https://github.com/nappsllc/rustshot/";
/// Release assets are only ever fetched from under this prefix.
pub const DOWNLOAD_PREFIX: &str = "https://github.com/nappsllc/rustshot/releases/download/";
/// Hosts GitHub redirects asset downloads to.
const REDIRECT_HOSTS: &[&str] = &["objects.githubusercontent.com", "release-assets.githubusercontent.com"];
/// Name of the checksum asset the release workflow publishes.
pub const SUMS_NAME: &str = "SHA256SUMS";
/// Error text of a download the progress callback cancelled.
pub const CANCELLED: &str = "cancelled";

/// No control characters or whitespace (argv injection, terminal escapes),
/// bounded length.
fn is_clean_url(u: &str) -> bool {
    !u.chars().any(|c| c.is_control() || c.is_whitespace()) && u.len() <= 512
}

/// Only release pages of this repo may be printed or opened: no other scheme,
/// host, control characters or whitespace (argv injection, terminal escapes).
fn is_safe_release_url(u: &str) -> bool {
    u.starts_with(RELEASES_PREFIX) && is_clean_url(u)
}

/// A release asset URL of this repo (exact lowercase prefix; no traversal,
/// backslashes or percent-escapes after the prefix).
pub fn is_safe_download_url(u: &str) -> bool {
    u.starts_with(DOWNLOAD_PREFIX)
        && is_clean_url(u)
        && !u.contains('\\')
        && !u[DOWNLOAD_PREFIX.len()..].contains(['%', '@'])
        && !u[DOWNLOAD_PREFIX.len()..].split(['/', '?', '#']).any(|seg| seg == ".." || seg == ".")
}

/// Hosts an asset download may be redirected to.
pub fn is_allowed_redirect_host(host: &str) -> bool {
    REDIRECT_HOSTS.iter().any(|h| host.eq_ignore_ascii_case(h))
}

/// Split an absolute `https://host/path` URL. None for any other scheme, a
/// port, userinfo, or an odd host. Signed asset-host redirect URLs run to
/// about 1 KB, hence the larger bound than `is_clean_url`.
pub fn split_https_url(u: &str) -> Option<(&str, &str)> {
    if u.chars().any(|c| c.is_control() || c.is_whitespace()) || u.len() > 4096 {
        return None;
    }
    let rest = u.strip_prefix("https://")?;
    let (host, path) = match rest.find(['/', '?', '#']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], &rest[i..]),
        Some(_) => return None,
        None => (rest, "/"),
    };
    let host_ok = !host.is_empty()
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    host_ok.then_some((host, path))
}

/// Whether a download may go to (or be redirected to) `url`: an asset URL of
/// this repo, or https on one of GitHub's asset hosts.
pub fn is_allowed_download_hop(url: &str) -> bool {
    is_safe_download_url(url)
        || split_https_url(url).is_some_and(|(host, _)| is_allowed_redirect_host(host))
}

/// Asset names become file names: plain ASCII, no separators or traversal.
fn is_safe_asset_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 128
        && !n.starts_with('.')
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}

/// Parse `x.y.z` with an optional leading `v`; anything else is not comparable.
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut it = s.split('.');
    let mut next = || {
        let part = it.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<u32>().ok()
    };
    let v = (next()?, next()?, next()?);
    if it.next().is_some() {
        return None;
    }
    Some(v)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

// --- Minimal JSON walker ----------------------------------------------------
// Just enough to read top-level fields of an object and walk an array of
// objects, without a JSON crate. Values are `&str` slices starting at the
// value; strings are decoded on demand.

fn ws(s: &str) -> &str {
    s.trim_start_matches([' ', '\t', '\n', '\r'])
}

/// Decode the JSON string at the start of `s`; returns it and the rest.
fn read_string(s: &str) -> Option<(String, &str)> {
    let body = s.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = body.char_indices();
    fn hex4(chars: &mut std::str::CharIndices) -> Option<u32> {
        let mut v = 0;
        for _ in 0..4 {
            v = v * 16 + chars.next()?.1.to_digit(16)?;
        }
        Some(v)
    }
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &body[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                c @ ('"' | '\\' | '/') => out.push(c),
                'u' => {
                    let hi = hex4(&mut chars)?;
                    let cp = if (0xD800..0xDC00).contains(&hi) {
                        let lo = (chars.next()?.1 == '\\' && chars.next()?.1 == 'u')
                            .then(|| hex4(&mut chars))??;
                        if !(0xDC00..0xE000).contains(&lo) {
                            return None;
                        }
                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                    } else {
                        hi
                    };
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                }
                _ => return None,
            },
            c if (c as u32) < 0x20 => return None,
            c => out.push(c),
        }
    }
    None
}

/// Skip one JSON value at the start of `s`; returns the rest.
fn skip_value(s: &str) -> Option<&str> {
    let s = ws(s);
    let b = s.as_bytes();
    match *b.first()? {
        b'"' => {
            let mut i = 1;
            while i < b.len() {
                match b[i] {
                    b'\\' => i += 2,
                    b'"' => return Some(&s[i + 1..]),
                    _ => i += 1,
                }
            }
            None
        }
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = 0;
            while i < b.len() {
                match b[i] {
                    b'"' => {
                        let rest = skip_value(&s[i..])?;
                        i = s.len() - rest.len();
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&s[i + 1..]);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let end = s.find([',', '}', ']', ' ', '\t', '\n', '\r']).unwrap_or(s.len());
            (end > 0).then(|| &s[end..])
        }
    }
}

/// Top-level members of the object at the start of `s`: (key, value slice).
fn members(s: &str) -> Option<Vec<(String, &str)>> {
    let mut s = ws(ws(s).strip_prefix('{')?);
    let mut out = Vec::new();
    if s.starts_with('}') {
        return Some(out);
    }
    loop {
        let (key, rest) = read_string(s)?;
        let value = ws(ws(rest).strip_prefix(':')?);
        let rest = ws(skip_value(value)?);
        out.push((key, value));
        if let Some(r) = rest.strip_prefix(',') {
            s = ws(r);
        } else {
            rest.strip_prefix('}')?;
            return Some(out);
        }
    }
}

/// Elements of the array at the start of `s` (each slice starts at a value).
fn items(s: &str) -> Option<Vec<&str>> {
    let mut s = ws(ws(s).strip_prefix('[')?);
    let mut out = Vec::new();
    if s.starts_with(']') {
        return Some(out);
    }
    loop {
        let rest = ws(skip_value(s)?);
        out.push(s);
        if let Some(r) = rest.strip_prefix(',') {
            s = ws(r);
        } else {
            rest.strip_prefix(']')?;
            return Some(out);
        }
    }
}

fn field<'a>(m: &[(String, &'a str)], key: &str) -> Option<&'a str> {
    m.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
}

fn str_field(m: &[(String, &str)], key: &str) -> Option<String> {
    read_string(field(m, key)?).map(|(s, _)| s)
}

fn bool_field(m: &[(String, &str)], key: &str) -> Option<bool> {
    let v = field(m, key)?;
    if v.starts_with("true") {
        Some(true)
    } else if v.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn u64_field(m: &[(String, &str)], key: &str) -> Option<u64> {
    let v = field(m, key)?;
    let end = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
    v[..end].parse().ok()
}

fn parse_asset(json: &str) -> Option<Asset> {
    let m = members(json)?;
    let name = str_field(&m, "name").filter(|n| is_safe_asset_name(n))?;
    let url = str_field(&m, "browser_download_url").filter(|u| is_safe_download_url(u))?;
    let size = u64_field(&m, "size").unwrap_or(0);
    Some(Asset { name, url, size })
}

/// Parse a `releases/latest` response. None for drafts, prereleases, missing
/// fields or a tag that is not `x.y.z`. Notes and assets are optional; assets
/// with an unexpected name or URL are dropped.
pub fn parse_latest(json: &str) -> Option<Release> {
    let m = members(json)?;
    if bool_field(&m, "draft") == Some(true) || bool_field(&m, "prerelease") == Some(true) {
        return None;
    }
    let tag = str_field(&m, "tag_name")?;
    parse_version(&tag)?;
    let url = str_field(&m, "html_url").filter(|u| is_safe_release_url(u))?;
    let notes = str_field(&m, "body").unwrap_or_default();
    let assets = field(&m, "assets")
        .and_then(items)
        .map(|v| v.into_iter().filter_map(parse_asset).collect())
        .unwrap_or_default();
    Some(Release { version: tag.strip_prefix('v').unwrap_or(&tag).to_string(), url, notes, assets })
}

// --- Release notes ----------------------------------------------------------

/// Inline markdown to text: links/images to their text, emphasis and code
/// markers removed, `<https://...>` autolinks unwrapped.
fn strip_inline(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let alnum = |j: Option<&char>| j.is_some_and(|c| c.is_alphanumeric());
    while i < c.len() {
        let ch = c[i];
        // [text](url) and ![alt](url)
        let bang = ch == '!' && c.get(i + 1) == Some(&'[');
        if ch == '[' || bang {
            let open = i + bang as usize;
            if let Some(close) = (open + 1..c.len()).find(|&j| c[j] == ']')
                && c.get(close + 1) == Some(&'(')
                && let Some(end) = (close + 2..c.len()).find(|&j| c[j] == ')')
            {
                let text: String = c[open + 1..close].iter().collect();
                out.push_str(&strip_inline(&text));
                i = end + 1;
                continue;
            }
        }
        if ch == '<'
            && let Some(end) = (i + 1..c.len()).find(|&j| c[j] == '>')
        {
            let inner: String = c[i + 1..end].iter().collect();
            if inner.starts_with("http://") || inner.starts_with("https://") {
                out.push_str(&inner);
                i = end + 1;
                continue;
            }
        }
        match ch {
            '`' => {}
            '*' | '_' | '~' if c.get(i + 1) == Some(&ch) => i += 1,
            '*' | '_' if !(alnum(i.checked_sub(1).and_then(|j| c.get(j))) && alnum(c.get(i + 1))) => {}
            _ => out.push(ch),
        }
        i += 1;
    }
    out
}

/// The first `max_lines` lines of release notes as plain text: headings,
/// bullets, quotes, links, emphasis, code fences and HTML comments stripped;
/// runs of blank lines collapsed.
#[allow(dead_code)] // used by the update dialog (later task)
pub fn notes_excerpt(body: &str, max_lines: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_comment = false;
    for raw in body.lines() {
        // Drop <!-- ... --> (possibly spanning lines).
        let mut line = String::new();
        let mut rest = raw;
        loop {
            if in_comment {
                match rest.find("-->") {
                    Some(i) => {
                        rest = &rest[i + 3..];
                        in_comment = false;
                    }
                    None => break,
                }
            } else {
                match rest.find("<!--") {
                    Some(i) => {
                        line.push_str(&rest[..i]);
                        rest = &rest[i + 4..];
                        in_comment = true;
                    }
                    None => {
                        line.push_str(rest);
                        break;
                    }
                }
            }
        }
        let mut t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            continue;
        }
        if t.len() >= 3 && t.chars().all(|c| matches!(c, '-' | '*' | '_' | ' ')) {
            t = ""; // horizontal rule
        }
        while let Some(r) = t.strip_prefix('>') {
            t = r.trim_start();
        }
        let mut prefix = "";
        let hashes = t.len() - t.trim_start_matches('#').len();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with([' ', '\t']) {
            t = t[hashes..].trim_start();
        } else if let Some(r) = t.strip_prefix(['-', '*', '+']).filter(|r| r.starts_with(' ')) {
            prefix = "- ";
            t = r.trim_start();
        }
        let text = strip_inline(t);
        let text = text.trim();
        if text.is_empty() {
            if out.last().is_some_and(|l| !l.is_empty()) {
                out.push(String::new());
            }
        } else {
            out.push(format!("{prefix}{text}"));
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.truncate(max_lines);
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

// --- Install kind and asset choice ------------------------------------------

/// How this copy of rustshot was installed, which decides how it updates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// NSIS installer (also winget): `uninstall.exe` next to the exe.
    WinInstaller,
    /// Bare exe anywhere else on Windows.
    WinPortable,
    /// Running from an AppImage (`$APPIMAGE` is its path).
    AppImage(PathBuf),
    /// Unpacked tarball in a user-writable directory.
    Tarball,
    /// deb/rpm/AUR or a non-writable location: the package manager updates it.
    LinuxManaged,
    /// A writable `.app` bundle (the path of the bundle).
    MacApp(PathBuf),
    /// A store or sandbox channel (name as shown to the user).
    Store(&'static str),
    /// Nothing we can update in place (e.g. a read-only macOS bundle).
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    Linux,
    Mac,
}

/// Environment probes for `detect_from`, so tests need no real filesystem.
pub struct Probes<'a> {
    pub os: Os,
    pub msix: bool,
    pub exists: &'a dyn Fn(&Path) -> bool,
    pub writable: &'a dyn Fn(&Path) -> bool,
}

/// Pure install-kind detection from the exe path, environment and probes.
pub fn detect_from(exe: &Path, env: impl Fn(&str) -> Option<String>, p: &Probes) -> InstallKind {
    if let Some(store) = managed_by(&env, p.msix) {
        return InstallKind::Store(store);
    }
    let dir = exe.parent().unwrap_or(Path::new(""));
    match p.os {
        Os::Windows => {
            if (p.exists)(&dir.join("uninstall.exe")) {
                InstallKind::WinInstaller
            } else {
                InstallKind::WinPortable
            }
        }
        Os::Linux => {
            if let Some(a) = env("APPIMAGE").filter(|a| !a.is_empty()) {
                InstallKind::AppImage(a.into())
            } else if exe.starts_with("/usr") || !(p.writable)(dir) {
                InstallKind::LinuxManaged
            } else {
                InstallKind::Tarball
            }
        }
        Os::Mac => {
            // <bundle>.app/Contents/MacOS/<exe>
            match dir.parent().and_then(Path::parent) {
                Some(bundle)
                    if dir.ends_with("Contents/MacOS")
                        && bundle.extension().is_some_and(|e| e == "app")
                        && bundle.parent().is_some_and(|d| (p.writable)(d)) =>
                {
                    InstallKind::MacApp(bundle.to_path_buf())
                }
                _ => InstallKind::Unknown,
            }
        }
    }
}

/// Create and remove a probe file: the only portable writability test.
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".rustshot-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[allow(dead_code)] // used by the update dialog (later task)
pub fn detect_install() -> InstallKind {
    let exe = std::env::current_exe().unwrap_or_default();
    let os = if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else {
        Os::Linux
    };
    let probes =
        Probes { os, msix: is_msix(), exists: &|p: &Path| p.exists(), writable: &dir_writable };
    detect_from(&exe, |k| std::env::var(k).ok(), &probes)
}

/// The release asset that updates an install of `kind`, matched by exact
/// name (`rustshot-<version>-...`) against the release's asset list.
#[allow(dead_code)] // used by the update dialog (later task)
pub fn pick_asset<'a>(kind: &InstallKind, rel: &'a Release) -> Option<&'a Asset> {
    let v = &rel.version;
    let want = match kind {
        InstallKind::WinInstaller => format!("rustshot-{v}-setup.exe"),
        InstallKind::WinPortable => format!("rustshot-{v}-windows-x86_64.exe"),
        InstallKind::AppImage(_) => format!("rustshot-{v}-x86_64.AppImage"),
        InstallKind::Tarball => format!("rustshot-{v}-linux-x86_64.tar.gz"),
        InstallKind::MacApp(_) => format!("rustshot-{v}-macos-universal.zip"),
        InstallKind::LinuxManaged | InstallKind::Store(_) | InstallKind::Unknown => return None,
    };
    rel.assets.iter().find(|a| a.name == want)
}

// --- Integrity --------------------------------------------------------------

/// Parse `sha256sum` output: `<hex>  <name>` or `<hex> *<name>` per line.
/// Malformed lines are skipped. A name listed twice with different digests is
/// poisoned: it is absent from the result, so a lookup finds nothing. Identical
/// duplicates are fine.
#[cfg(test)] // production code uses parse_sums_checked
pub fn parse_sums(text: &str) -> HashMap<String, [u8; 32]> {
    parse_sums_checked(text).0
}

/// Like `parse_sums`, also returning the names that conflict.
fn parse_sums_checked(text: &str) -> (HashMap<String, [u8; 32]>, Vec<String>) {
    let mut out: HashMap<String, [u8; 32]> = HashMap::new();
    let mut poisoned: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some((hex, rest)) = line.trim().split_once(' ') else { continue };
        let Some(digest) = crate::sha256::parse_hex(hex) else { continue };
        let name = rest.trim_start_matches(' ');
        let name = name.strip_prefix('*').unwrap_or(name);
        let name = name.strip_prefix("./").unwrap_or(name);
        if !name.is_empty() {
            match out.get(name) {
                Some(d) if *d != digest => poisoned.push(name.to_string()),
                Some(_) => {}
                None => {
                    out.insert(name.to_string(), digest);
                }
            }
        }
    }
    for n in &poisoned {
        out.remove(n);
    }
    (out, poisoned)
}

/// Largest `SHA256SUMS` we accept (a few hundred bytes in practice).
const SUMS_MAX: u64 = 64 * 1024;

/// Download `SHA256SUMS` and `asset` into `dir`, and verify the asset's
/// SHA-256. Returns the asset's path and the expected digest (so the
/// installer can `verify_file` again right before executing). `dir` must be
/// user-private: stale files of either name are deleted first and the new ones
/// are created exclusively, but a shared dir would still let another user
/// swap the file after verification. Refuses a release without
/// `SHA256SUMS` or without an entry for the asset; on any failure, mismatch
/// or cancel (`progress` returning false; error `CANCELLED`) the downloaded
/// files are deleted. `progress(got, total)` falls back to the asset's
/// API-reported size for the total.
#[allow(dead_code)] // used by the update dialog (later task)
pub fn fetch_verified(
    rel: &Release,
    asset: &Asset,
    dir: &Path,
    progress: &dyn Fn(u64, Option<u64>) -> bool,
) -> Result<(PathBuf, [u8; 32]), String> {
    if !is_safe_asset_name(&asset.name)
        || asset.name.eq_ignore_ascii_case(SUMS_NAME)
        || !is_safe_download_url(&asset.url)
    {
        return Err("refusing an unexpected release asset".into());
    }
    let sums_asset = rel
        .assets
        .iter()
        .find(|a| a.name == SUMS_NAME)
        .ok_or("this release has no SHA256SUMS; refusing to install")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let sums_path = dir.join(SUMS_NAME);
    let dest = dir.join(&asset.name);
    let total = Some(asset.size).filter(|&s| s > 0);
    let _ = std::fs::remove_file(&sums_path);
    let _ = std::fs::remove_file(&dest);
    let result = (|| -> Result<[u8; 32], String> {
        // Abort the SHA256SUMS download once it passes the cap.
        let too_big = std::cell::Cell::new(false);
        let r = crate::export::download_to(&sums_asset.url, &sums_path, &|got, _| {
            if got > SUMS_MAX {
                too_big.set(true);
                return false;
            }
            progress(0, total)
        });
        if too_big.get() {
            return Err("SHA256SUMS is unexpectedly large".into());
        }
        r?;
        let len = std::fs::metadata(&sums_path).map_err(|e| e.to_string())?.len();
        if len > SUMS_MAX {
            return Err("SHA256SUMS is unexpectedly large".into());
        }
        let text = std::fs::read(&sums_path).map_err(|e| e.to_string())?;
        let (sums, poisoned) = parse_sums_checked(&String::from_utf8_lossy(&text));
        if poisoned.contains(&asset.name) {
            return Err(format!("SHA256SUMS lists {} twice", asset.name));
        }
        let want = *sums
            .get(&asset.name)
            .ok_or_else(|| format!("{} is not listed in SHA256SUMS; refusing to install", asset.name))?;
        crate::export::download_to(&asset.url, &dest, &|got, t| progress(got, t.or(total)))?;
        let got = crate::sha256::file_digest(&dest).map_err(|e| format!("read download: {e}"))?;
        if got != want {
            return Err(format!("{} failed its SHA-256 check; refusing to install", asset.name));
        }
        Ok(want)
    })();
    let _ = std::fs::remove_file(&sums_path);
    match result {
        Ok(want) => Ok((dest, want)),
        Err(e) => {
            let _ = std::fs::remove_file(&dest);
            Err(e)
        }
    }
}

/// Re-hash `path` and compare with `expected` (the installer calls this right
/// before executing the download).
#[allow(dead_code)] // used by the installer (later task)
pub fn verify_file(path: &Path, expected: [u8; 32]) -> Result<(), String> {
    let got = crate::sha256::file_digest(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if got == expected {
        Ok(())
    } else {
        Err("the downloaded file does not match its SHA-256; refusing to run it".into())
    }
}

/// The store/sandbox channel that updates this install, if any.
pub fn managed_by(env: impl Fn(&str) -> Option<String>, msix: bool) -> Option<&'static str> {
    if msix {
        Some("Microsoft Store")
    } else if env("FLATPAK_ID").is_some() {
        Some("Flathub")
    } else if env("SNAP").is_some() {
        Some("Snap Store")
    } else if env("APP_SANDBOX_CONTAINER_ID").is_some() {
        Some("Mac App Store")
    } else {
        None
    }
}

pub fn managed_install() -> Option<&'static str> {
    managed_by(|k| std::env::var(k).ok(), is_msix())
}

#[cfg(windows)]
fn is_msix() -> bool {
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut len = 0u32;
    // Packaged: the zero-length probe reports a too-small buffer. Unpackaged:
    // APPMODEL_ERROR_NO_PACKAGE (15700).
    unsafe { GetCurrentPackageFullName(&mut len, None) == ERROR_INSUFFICIENT_BUFFER }
}

#[cfg(not(windows))]
fn is_msix() -> bool {
    false
}

const DAY_SECS: u64 = 86_400;

/// A check is due when none has succeeded yet or the last one is a day old.
pub fn due(last_unix: Option<u64>, now_unix: u64) -> bool {
    match last_unix {
        None => true,
        Some(t) => now_unix.saturating_sub(t) >= DAY_SECS,
    }
}

const STAMP_FILE: &str = "update-check";
const FIRST_DELAY: std::time::Duration = std::time::Duration::from_secs(60);
const RECHECK: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// Parse the stamp file's text; anything unreadable counts as "never".
fn parse_stamp(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// One checker iteration. Runs `check` only when due; returns the stamp to
/// persist (success only) and a newer release, if any. Errors are silent
/// (stderr) and leave the stamp alone.
fn tick(
    last: Option<u64>,
    now: u64,
    check: impl FnOnce() -> Result<Option<Release>, String>,
) -> (Option<u64>, Option<Release>) {
    if !due(last, now) {
        return (None, None);
    }
    match check() {
        Ok(r) => (Some(now), r),
        Err(e) => {
            eprintln!("update check failed: {e}");
            (None, None)
        }
    }
}

/// The user chose "Skip this version" for this release (`skip_version`,
/// optional leading `v`). Only the daily check honours it.
fn is_skipped(rel: &Release, skip_version: &str) -> bool {
    let s = skip_version.trim();
    !s.is_empty() && s.strip_prefix('v').unwrap_or(s) == rel.version
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Start the daemon's background checker. `None` when disabled or when the
/// install is managed by a store. The thread is detached and never blocks exit.
/// A release equal to the config's `skip_version` (re-read each time) is not
/// reported.
pub fn spawn_checker(enabled: bool) -> Option<std::sync::mpsc::Receiver<Release>> {
    if !enabled || managed_install().is_some() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let dir = crate::config::config_dir();
        let stamp = dir.join(STAMP_FILE);
        std::thread::sleep(FIRST_DELAY);
        loop {
            let last = std::fs::read_to_string(&stamp).ok().and_then(|t| parse_stamp(&t));
            let (new_stamp, release) = tick(last, unix_now(), check_now);
            if let Some(t) = new_stamp {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(&stamp, t.to_string());
            }
            let release = release.filter(|r| !is_skipped(r, &crate::config::load().skip_version));
            if let Some(r) = release
                && tx.send(r).is_err()
            {
                return;
            }
            std::thread::sleep(RECHECK);
        }
    });
    Some(rx)
}

/// Ask GitHub for the latest release; `Some` only if newer than this build.
pub fn check_now() -> Result<Option<Release>, String> {
    let ua = format!("rustshot/{}", env!("CARGO_PKG_VERSION"));
    let body = crate::export::http_get(
        RELEASES_HOST,
        RELEASES_PATH,
        &[("User-Agent", &ua), ("Accept", "application/vnd.github+json")],
    )?;
    let release = parse_latest(&body);
    Ok(release.filter(|r| is_newer(&r.version, env!("CARGO_PKG_VERSION"))))
}

/// Open a URL in the default browser (best effort).
pub fn open_url(url: &str) {
    #[cfg(not(windows))]
    use std::process::Command;
    if !is_safe_release_url(url) {
        eprintln!("refusing to open an unexpected URL");
        return;
    }
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        if let Some(exe) = crate::proc_win::system_exe("rundll32.exe") {
            let _ = crate::proc_win::spawn_detached(&exe, &[OsStr::new("url.dll,FileProtocolHandler"), OsStr::new(url)]);
        }
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    #[cfg(not(windows))]
    let _ = cmd.spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_checker_disabled_is_none() {
        assert!(spawn_checker(false).is_none());
    }

    #[test]
    fn stamp_parsing_tolerates_garbage() {
        assert_eq!(parse_stamp("1700000000\n"), Some(1_700_000_000));
        assert_eq!(parse_stamp(""), None);
        assert_eq!(parse_stamp("nope"), None);
    }

    #[test]
    fn tick_respects_schedule_and_errors() {
        let rel = || Release { version: "9.9.9".into(), url: "u".into(), ..Default::default() };
        let (s, r) = tick(Some(1000), 1100, || panic!("must not run"));
        assert_eq!((s, r), (None, None));
        assert_eq!(tick(None, 5, || Ok(Some(rel()))), (Some(5), Some(rel())));
        assert_eq!(tick(None, 5, || Ok(None)), (Some(5), None));
        assert_eq!(tick(None, 5, || Err("x".into())), (None, None));
    }

    #[test]
    fn skip_version() {
        let rel = Release { version: "0.1.2".into(), ..Default::default() };
        assert!(is_skipped(&rel, "0.1.2"));
        assert!(is_skipped(&rel, " v0.1.2 "));
        assert!(!is_skipped(&rel, ""));
        assert!(!is_skipped(&rel, "0.1.1"));
        assert!(!is_skipped(&rel, "0.1.20"));
    }

    const FIXTURE: &str = r#"{
  "url": "https://api.github.com/repos/nappsllc/rustshot/releases/1",
  "html_url": "https://github.com/nappsllc/rustshot/releases/tag/v0.2.0",
  "id": 1,
  "author": {
    "login": "someone",
    "html_url": "https://github.com/someone"
  },
  "tag_name": "v0.2.0",
  "draft": false,
  "prerelease": false,
  "body": "notes with \"quotes\" and a link https:\/\/example.com",
  "assets": []
}"#;

    /// Modelled on GitHub's `releases/latest` (fields trimmed), with nested
    /// objects whose keys shadow the release's, an escaped body that mentions
    /// `"assets"`, and one asset on a foreign host that must be dropped.
    const FIXTURE_ASSETS: &str = r###"{
  "url": "https://api.github.com/repos/nappsllc/rustshot/releases/2",
  "assets_url": "https://api.github.com/repos/nappsllc/rustshot/releases/2/assets",
  "author": {"login": "bot", "html_url": "https://github.com/bot", "name": "x", "size": 1},
  "html_url": "https://github.com/nappsllc/rustshot/releases/tag/v0.1.2",
  "tag_name": "v0.1.2",
  "name": "rustshot v0.1.2",
  "draft": false,
  "prerelease": false,
  "assets": [
    {
      "url": "https://api.github.com/repos/nappsllc/rustshot/releases/assets/10",
      "id": 10,
      "name": "rustshot-0.1.2-setup.exe",
      "label": "",
      "uploader": {"login": "github-actions[bot]", "name": "evil.exe", "html_url": "https://github.com/apps/github-actions"},
      "content_type": "application/x-msdownload",
      "size": 812345,
      "digest": "sha256:00",
      "browser_download_url": "https://github.com/nappsllc/rustshot/releases/download/v0.1.2/rustshot-0.1.2-setup.exe"
    },
    {
      "name": "rustshot-0.1.2-windows-x86_64.exe",
      "size": 700000,
      "browser_download_url": "https://github.com/nappsllc/rustshot/releases/download/v0.1.2/rustshot-0.1.2-windows-x86_64.exe"
    },
    {
      "name": "rustshot-0.1.2-linux-x86_64.tar.gz",
      "size": 900000,
      "browser_download_url": "https://evil.example/nappsllc/rustshot/releases/download/v0.1.2/rustshot-0.1.2-linux-x86_64.tar.gz"
    },
    {
      "name": "rustshot-0.1.2-x86_64.AppImage",
      "size": 1200000,
      "browser_download_url": "https://github.com/nappsllc/rustshot/releases/download/v0.1.2/rustshot-0.1.2-x86_64.AppImage"
    },
    {
      "name": "rustshot-0.1.2-macos-universal.zip",
      "size": 1500000,
      "browser_download_url": "https://github.com/nappsllc/rustshot/releases/download/v0.1.2/rustshot-0.1.2-macos-universal.zip"
    },
    {
      "name": "SHA256SUMS",
      "size": 812,
      "browser_download_url": "https://github.com/nappsllc/rustshot/releases/download/v0.1.2/SHA256SUMS"
    }
  ],
  "tarball_url": "https://api.github.com/repos/nappsllc/rustshot/tarball/v0.1.2",
  "body": "## What's Changed\r\n* Fix \"save\" \\ path by @someone in https://github.com/nappsllc/rustshot/pull/7\r\n\r\n\"assets\": [] \u2014 \ud83d\ude80\r\n\r\n**Full Changelog**: https://github.com/nappsllc/rustshot/compare/v0.1.1...v0.1.2"
}"###;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("0.10.3"), Some((0, 10, 3)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3-beta"), None);
        assert_eq!(parse_version("x.y.z"), None);
    }

    #[test]
    fn newer() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("junk", "0.1.0"));
        assert!(!is_newer("0.2.0", "junk"));
    }

    #[test]
    fn latest_release() {
        let r = parse_latest(FIXTURE).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.url, "https://github.com/nappsllc/rustshot/releases/tag/v0.2.0");
        assert_eq!(r.notes, "notes with \"quotes\" and a link https://example.com");
        assert!(r.assets.is_empty());
    }

    #[test]
    fn latest_with_assets_and_notes() {
        let r = parse_latest(FIXTURE_ASSETS).unwrap();
        assert_eq!(r.version, "0.1.2");
        assert_eq!(r.url, "https://github.com/nappsllc/rustshot/releases/tag/v0.1.2");
        let names: Vec<&str> = r.assets.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "rustshot-0.1.2-setup.exe",
                "rustshot-0.1.2-windows-x86_64.exe",
                "rustshot-0.1.2-x86_64.AppImage",
                "rustshot-0.1.2-macos-universal.zip",
                "SHA256SUMS",
            ]
        );
        assert_eq!(r.assets[0].size, 812_345);
        assert_eq!(
            r.assets[0].url,
            "https://github.com/nappsllc/rustshot/releases/download/v0.1.2/rustshot-0.1.2-setup.exe"
        );
        assert!(r.notes.starts_with("## What's Changed\r\n* Fix \"save\" \\ path"));
        assert!(r.notes.contains("\"assets\": [] \u{2014} \u{1F680}"));
        // A truncated asset array makes the whole response unparsable.
        let start = FIXTURE_ASSETS.find("\"assets\": [").unwrap();
        let broken = format!("{}\"assets\": [{{\"name\": ", &FIXTURE_ASSETS[..start]);
        assert!(parse_latest(&broken).is_none(), "truncated JSON must not parse");
    }

    #[test]
    fn json_walker() {
        assert_eq!(read_string(r#""a\u00e9\n" rest"#), Some(("a\u{e9}\n".into(), " rest")));
        assert_eq!(read_string(r#""\ud83d\ude80""#).map(|s| s.0), Some("\u{1F680}".into()));
        assert_eq!(read_string(r#""\ud83d x""#), None);
        assert_eq!(read_string(r#""\q""#), None);
        assert_eq!(read_string("\"unterminated"), None);
        assert_eq!(skip_value(r#"{"a": [1, "]}", {"b": "\"}"}]}, 2"#), Some(", 2"));
        assert_eq!(skip_value("123}"), Some("}"));
        let m = members(r#"{"a": {"k": "inner"}, "k": "outer", "e": {}}"#).unwrap();
        assert_eq!(str_field(&m, "k").as_deref(), Some("outer"));
        assert_eq!(members("{}").unwrap().len(), 0);
        assert_eq!(items("[]").unwrap().len(), 0);
        assert_eq!(items("[1, {\"x\": [2]}, \"s\"]").unwrap().len(), 3);
        assert!(items("[1, 2").is_none());
    }

    #[test]
    fn latest_rejects() {
        assert!(parse_latest(&FIXTURE.replace("\"draft\": false", "\"draft\": true")).is_none());
        assert!(
            parse_latest(&FIXTURE.replace("\"prerelease\": false", "\"prerelease\": true"))
                .is_none()
        );
        assert!(parse_latest(&FIXTURE.replace("v0.2.0\",\n  \"draft", "nightly\",\n  \"draft")).is_none());
        assert!(parse_latest("{}").is_none());
    }

    #[test]
    fn release_url_guard() {
        assert!(is_safe_release_url("https://github.com/nappsllc/rustshot/releases/tag/v0.2.0"));
        assert!(!is_safe_release_url("https://github.com/nappsllc/rustshot/x y"));
        assert!(!is_safe_release_url("https://github.com/nappsllc/rustshot/\u{1b}[2J"));
        assert!(!is_safe_release_url("-https://github.com/nappsllc/rustshot/"));
        let long = format!("{RELEASES_PREFIX}{}", "a".repeat(512));
        assert!(!is_safe_release_url(&long));
    }

    #[test]
    fn download_url_guard() {
        let ok = "https://github.com/nappsllc/rustshot/releases/download/v0.1.2/SHA256SUMS";
        assert!(is_safe_download_url(ok));
        assert!(is_allowed_download_hop(ok));
        for bad in [
            "https://github.com/nappsllc/rustshot/releases/tag/v0.1.2",
            "http://github.com/nappsllc/rustshot/releases/download/v0.1.2/x",
            "https://github.com/nappsllc/rustshot/releases/download/../../other/x",
            "https://github.com/nappsllc/rustshot/releases/download/v1/x\\y",
            "https://github.com/nappsllc/rustshot/releases/download/v1/x y",
            "https://github.com.evil.example/nappsllc/rustshot/releases/download/v1/x",
            "https://github.com/nappsllc/rustshot/releases/download/%2e%2e/%2e%2e/other/x",
            "https://github.com/nappsllc/rustshot/releases/download/v1%2Fx",
            "https://github.com/nappsllc/rustshot/releases/download/v1/x%5cy",
            "HTTPS://GITHUB.COM/nappsllc/rustshot/releases/download/v1/x",
            "https://GITHUB.COM/nappsllc/rustshot/releases/download/v1/x",
            "https://user@github.com/nappsllc/rustshot/releases/download/v1/x",
            "https://github.com@evil.example/nappsllc/rustshot/releases/download/v1/x",
            "https://github.com/nappsllc/rustshot/releases/download/v1/x@evil.example",
        ] {
            assert!(!is_safe_download_url(bad), "accepted {bad}");
        }
    }

    #[test]
    fn redirect_host_guard() {
        assert!(is_allowed_redirect_host("objects.githubusercontent.com"));
        assert!(is_allowed_redirect_host("release-assets.githubusercontent.com"));
        assert!(is_allowed_redirect_host("Objects.GitHubUserContent.com"));
        assert!(!is_allowed_redirect_host("githubusercontent.com"));
        assert!(!is_allowed_redirect_host("evil.objects.githubusercontent.com"));
        assert!(!is_allowed_redirect_host("objects.githubusercontent.com.evil.example"));
        let hop = "https://release-assets.githubusercontent.com/github-production-release-asset/1?sig=a%2Fb&x=1";
        assert!(is_allowed_download_hop(hop));
        let signed = format!("https://release-assets.githubusercontent.com/a?jwt={}", "x".repeat(900));
        assert!(is_allowed_download_hop(&signed));
        assert!(!is_allowed_download_hop(&format!("{signed}{}", "x".repeat(4096))));
        assert_eq!(
            split_https_url(hop),
            Some(("release-assets.githubusercontent.com", "/github-production-release-asset/1?sig=a%2Fb&x=1"))
        );
        for bad in [
            "http://objects.githubusercontent.com/x",
            "https://objects.githubusercontent.com:8443/x",
            "https://user@objects.githubusercontent.com/x",
            "https://evil.example/objects.githubusercontent.com",
            "https://evil.example?objects.githubusercontent.com",
            "/relative/path",
            "file:///etc/passwd",
        ] {
            assert!(!is_allowed_download_hop(bad), "accepted {bad}");
        }
        assert_eq!(split_https_url("https://example.com"), Some(("example.com", "/")));
    }

    #[test]
    fn latest_rejects_unsafe_urls() {
        const HTML_URL: &str =
            "\"html_url\": \"https://github.com/nappsllc/rustshot/releases/tag/v0.2.0\"";
        for bad in [
            "C:\\\\Windows\\\\System32\\\\calc.exe",
            "file:///etc/passwd",
            "https://evil.example/nappsllc/rustshot/x",
            "https://github.com/nappsllc/rustshot/releases/tag/v9.9.9\\n",
        ] {
            let json = FIXTURE.replace(HTML_URL, &format!("\"html_url\": \"{bad}\""));
            assert_ne!(json, FIXTURE, "fixture was not patched");
            assert!(parse_latest(&json).is_none(), "accepted {bad}");
        }
    }

    #[test]
    fn asset_names() {
        assert!(is_safe_asset_name("rustshot-0.1.2-setup.exe"));
        assert!(is_safe_asset_name("SHA256SUMS"));
        for bad in ["", "../x", "a/b", "a\\b", ".hidden", "a b", "n\u{e9}"] {
            assert!(!is_safe_asset_name(bad), "accepted {bad:?}");
        }
    }

    #[test]
    fn notes() {
        let body = "<!-- Release notes generated using configuration in .github/release.yml -->\r\n\
                    ## What's Changed\r\n\
                    ### Features\r\n\
                    * **Save** to [dated folders](https://example.com/x) by @a in https://github.com/nappsllc/rustshot/pull/7\r\n\
                    - Use `Ctrl+S`, keep snake_case and 2*3 and _emph_ and ~~gone~~\r\n\
                    \r\n\r\n\r\n\
                    > quoted ![logo](https://example.com/l.png) <https://example.com/a>\r\n\
                    ---\r\n\
                    ```\r\ncode line\r\n```\r\n\
                    **Full Changelog**: https://github.com/nappsllc/rustshot/compare/v0.1.1...v0.1.2\r\n";
        assert_eq!(
            notes_excerpt(body, 12),
            "What's Changed\n\
             Features\n\
             - Save to dated folders by @a in https://github.com/nappsllc/rustshot/pull/7\n\
             - Use Ctrl+S, keep snake_case and 2*3 and emph and gone\n\
             \n\
             quoted logo https://example.com/a\n\
             \n\
             code line\n\
             Full Changelog: https://github.com/nappsllc/rustshot/compare/v0.1.1...v0.1.2"
        );
        assert_eq!(notes_excerpt(body, 2), "What's Changed\nFeatures");
        // A cut right after a blank line does not leave it trailing.
        assert_eq!(notes_excerpt("a\n\nb", 2), "a");
        assert_eq!(notes_excerpt("", 12), "");
        assert_eq!(notes_excerpt("<!-- multi\nline -->\n#nohash\nok", 12), "#nohash\nok");
    }

    fn rel_with(names: &[&str]) -> Release {
        Release {
            version: "0.1.2".into(),
            assets: names
                .iter()
                .map(|n| Asset { name: n.to_string(), url: format!("{DOWNLOAD_PREFIX}v0.1.2/{n}"), size: 1 })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn pick_asset_per_kind() {
        let all = [
            "rustshot-0.1.2-setup.exe",
            "rustshot-0.1.2-windows-x86_64.exe",
            "rustshot-0.1.2-x86_64.AppImage",
            "rustshot-0.1.2-linux-x86_64.tar.gz",
            "rustshot-0.1.2_amd64.deb",
            "rustshot-0.1.2-macos-universal.zip",
            "rustshot-0.1.2-macos-universal.dmg",
            "rustshot-0.1.2-x64.msix",
            "SHA256SUMS",
        ];
        let rel = rel_with(&all);
        let pick = |k: InstallKind| pick_asset(&k, &rel).map(|a| a.name.as_str());
        assert_eq!(pick(InstallKind::WinInstaller), Some("rustshot-0.1.2-setup.exe"));
        assert_eq!(pick(InstallKind::WinPortable), Some("rustshot-0.1.2-windows-x86_64.exe"));
        assert_eq!(pick(InstallKind::AppImage("/a".into())), Some("rustshot-0.1.2-x86_64.AppImage"));
        assert_eq!(pick(InstallKind::Tarball), Some("rustshot-0.1.2-linux-x86_64.tar.gz"));
        assert_eq!(pick(InstallKind::MacApp("/A.app".into())), Some("rustshot-0.1.2-macos-universal.zip"));
        assert_eq!(pick(InstallKind::LinuxManaged), None);
        assert_eq!(pick(InstallKind::Store("Flathub")), None);
        assert_eq!(pick(InstallKind::Unknown), None);
        // Another version's file is not picked.
        let old = rel_with(&["rustshot-0.1.1-setup.exe"]);
        assert_eq!(pick_asset(&InstallKind::WinInstaller, &old), None);
    }

    #[test]
    fn detect_kinds() {
        let no_env = |_: &str| -> Option<String> { None };
        let probes = |os: Os, exists: &'static [&'static str], writable: bool| (os, exists, writable);
        let run = |exe: &str,
                   env: &dyn Fn(&str) -> Option<String>,
                   (os, exists, writable): (Os, &[&str], bool),
                   msix: bool| {
            let ex = |p: &Path| exists.iter().any(|e| p == Path::new(e));
            let wr = |_: &Path| writable;
            detect_from(Path::new(exe), env, &Probes { os, msix, exists: &ex, writable: &wr })
        };
        let inst = "C:/Users/u/AppData/Local/rustshot/rustshot.exe";
        assert_eq!(
            run(inst, &no_env, probes(Os::Windows, &["C:/Users/u/AppData/Local/rustshot/uninstall.exe"], true), false),
            InstallKind::WinInstaller
        );
        assert_eq!(run(inst, &no_env, probes(Os::Windows, &[], true), false), InstallKind::WinPortable);
        assert_eq!(
            run(inst, &no_env, probes(Os::Windows, &[], true), true),
            InstallKind::Store("Microsoft Store")
        );
        let appimage = |k: &str| (k == "APPIMAGE").then(|| "/home/u/Apps/rustshot.AppImage".to_string());
        assert_eq!(
            run("/tmp/.mount_x/usr/bin/rustshot", &appimage, probes(Os::Linux, &[], false), false),
            InstallKind::AppImage("/home/u/Apps/rustshot.AppImage".into())
        );
        assert_eq!(run("/usr/bin/rustshot", &no_env, probes(Os::Linux, &[], true), false), InstallKind::LinuxManaged);
        assert_eq!(run("/opt/rustshot/rustshot", &no_env, probes(Os::Linux, &[], false), false), InstallKind::LinuxManaged);
        assert_eq!(run("/home/u/rustshot/rustshot", &no_env, probes(Os::Linux, &[], true), false), InstallKind::Tarball);
        let flatpak = |k: &str| (k == "FLATPAK_ID").then(|| "io.github.nappsllc.rustshot".to_string());
        assert_eq!(run("/app/bin/rustshot", &flatpak, probes(Os::Linux, &[], true), false), InstallKind::Store("Flathub"));
        let snap = |k: &str| (k == "SNAP").then(|| "/snap/rustshot/1".to_string());
        assert_eq!(run("/snap/rustshot/1/rustshot", &snap, probes(Os::Linux, &[], true), false), InstallKind::Store("Snap Store"));
        let mac = "/Applications/Rustshot.app/Contents/MacOS/rustshot";
        assert_eq!(
            run(mac, &no_env, probes(Os::Mac, &[], true), false),
            InstallKind::MacApp("/Applications/Rustshot.app".into())
        );
        assert_eq!(run(mac, &no_env, probes(Os::Mac, &[], false), false), InstallKind::Unknown);
        assert_eq!(run("/usr/local/bin/rustshot", &no_env, probes(Os::Mac, &[], true), false), InstallKind::Unknown);
        let mas = |k: &str| (k == "APP_SANDBOX_CONTAINER_ID").then(|| "x".to_string());
        assert_eq!(run(mac, &mas, probes(Os::Mac, &[], true), false), InstallKind::Store("Mac App Store"));
    }

    #[test]
    fn sums() {
        let a = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let e = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855";
        let text = format!(
            "{a}  rustshot-0.1.2-setup.exe\n{e} *rustshot-0.1.2-x86_64.AppImage\r\n\
             garbage line\n{a}  ./SHA256SUMS\n\n{e}  rustshot-0.1.2-setup.exe\nnothex  x\n"
        );
        let m = parse_sums(&text);
        // setup.exe is listed twice with different digests: poisoned, absent.
        assert_eq!(m.len(), 2);
        assert!(!m.contains_key("rustshot-0.1.2-setup.exe"));
        assert_eq!(m["rustshot-0.1.2-x86_64.AppImage"], crate::sha256::digest(b""));
        assert!(m.contains_key("SHA256SUMS"));
    }

    #[test]
    fn sums_duplicates() {
        let a = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let e = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        // Conflicting digests poison the name; identical ones are fine.
        let m = parse_sums(&format!("{a}  x.exe\n{e}  x.exe\n{a}  y.exe\n{a}  y.exe\n"));
        assert!(!m.contains_key("x.exe"));
        assert_eq!(m["y.exe"], crate::sha256::digest(b"abc"));
        let (_, poisoned) = parse_sums_checked(&format!("{a}  x.exe\n{e}  x.exe\n"));
        assert_eq!(poisoned, ["x.exe"]);
    }

    #[test]
    fn verify_file_checks_digest() {
        let p = std::env::temp_dir().join(format!("rustshot-test-vf-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert!(verify_file(&p, crate::sha256::digest(b"abc")).is_ok());
        assert!(verify_file(&p, crate::sha256::digest(b"abd")).is_err());
        let _ = std::fs::remove_file(&p);
        assert!(verify_file(&p, [0; 32]).is_err());
    }

    #[test]
    fn fetch_verified_refuses_without_sums() {
        let dir = std::env::temp_dir().join(format!("rustshot-test-fv-{}", std::process::id()));
        let rel = rel_with(&["rustshot-0.1.2-setup.exe"]);
        let err = fetch_verified(&rel, &rel.assets[0], &dir, &|_, _| true).unwrap_err();
        assert!(err.contains("SHA256SUMS"), "{err}");
        let mut bad = rel.assets[0].clone();
        bad.url = "https://evil.example/x".into();
        assert!(fetch_verified(&rel, &bad, &dir, &|_, _| true).is_err());
        // An asset named like the checksum file (any case) is refused up front.
        let rel2 = rel_with(&["sha256sums", "SHA256SUMS"]);
        let err = fetch_verified(&rel2, &rel2.assets[0], &dir, &|_, _| true).unwrap_err();
        assert!(err.contains("unexpected release asset"), "{err}");
        let _ = std::fs::remove_dir(&dir);
    }

    /// Manual smoke test of the platform `download_to` against a real
    /// release (follows GitHub's redirect to its asset host); prints the
    /// SHA-256 to compare with `Get-FileHash` / `sha256sum`.
    #[test]
    #[ignore = "network"]
    fn live_download() {
        let url = format!("{DOWNLOAD_PREFIX}v0.1.1/rustshot-0.1.1-windows-x86_64.exe");
        let dir = std::env::temp_dir().join(format!("rustshot-live-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("rustshot-0.1.1-windows-x86_64.exe");
        let calls = std::cell::Cell::new(0u32);
        crate::export::download_to(&url, &dest, &|got, total| {
            calls.set(calls.get() + 1);
            assert!(total.is_none_or(|t| got <= t));
            true
        })
        .unwrap();
        let len = std::fs::metadata(&dest).unwrap().len();
        let hex = crate::sha256::hex(&crate::sha256::file_digest(&dest).unwrap());
        println!("{} bytes, {} progress calls, sha256 {hex}", len, calls.get());
        println!("kept at {}", dest.display());
        // Cancel deletes the partial file.
        let dest2 = dir.join("cancelled.exe");
        let err = crate::export::download_to(&url, &dest2, &|got, _| got < 100_000).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(!dest2.exists());
    }

    #[test]
    fn managed() {
        let none = |_: &str| None;
        assert_eq!(managed_by(none, false), None);
        assert_eq!(managed_by(none, true), Some("Microsoft Store"));
        let only = |k: &'static str| move |n: &str| (n == k).then(|| "x".to_string());
        assert_eq!(managed_by(only("FLATPAK_ID"), false), Some("Flathub"));
        assert_eq!(managed_by(only("SNAP"), false), Some("Snap Store"));
        assert_eq!(managed_by(only("APP_SANDBOX_CONTAINER_ID"), false), Some("Mac App Store"));
    }

    #[test]
    fn schedule() {
        let now = 1_000_000;
        assert!(due(None, now));
        assert!(!due(Some(now - 23 * 3600), now));
        assert!(due(Some(now - 24 * 3600), now));
        assert!(!due(Some(now + 10), now));
    }
}

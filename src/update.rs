//! Update check against GitHub releases.

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

const RELEASES_HOST: &str = "api.github.com";
const RELEASES_PATH: &str = "/repos/nappsllc/rustshot/releases/latest";

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

/// Value position right after `"key"` and its colon, or None.
fn field_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(i) = json[from..].find(&needle) {
        let at = from + i;
        from = at + needle.len();
        // Skip occurrences inside an escaped string (`\"key\"`).
        if json[..at].ends_with('\\') {
            continue;
        }
        let rest = json[from..].trim_start();
        if let Some(v) = rest.strip_prefix(':') {
            return Some(v.trim_start());
        }
    }
    None
}

/// First `"key": "value"` string, decoding the usual escapes (quote, backslash, slash).
fn json_string(json: &str, key: &str) -> Option<String> {
    let rest = field_value(json, key)?.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                c @ ('"' | '\\' | '/') => out.push(c),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// First `"key": true|false`.
fn json_bool(json: &str, key: &str) -> Option<bool> {
    let v = field_value(json, key)?;
    if v.starts_with("true") {
        Some(true)
    } else if v.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Parse a `releases/latest` response. None for drafts, prereleases, missing
/// fields or a tag that is not `x.y.z`.
pub fn parse_latest(json: &str) -> Option<Release> {
    if json_bool(json, "draft") == Some(true) || json_bool(json, "prerelease") == Some(true) {
        return None;
    }
    let tag = json_string(json, "tag_name")?;
    parse_version(&tag)?;
    // GitHub emits the release's own html_url before the nested author's.
    let url = json_string(json, "html_url")?;
    Some(Release { version: tag.strip_prefix('v').unwrap_or(&tag).to_string(), url })
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

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Start the daemon's background checker. `None` when disabled or when the
/// install is managed by a store. The thread is detached and never blocks exit.
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
    use std::process::Command;
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
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
        assert_eq!(parse_stamp("1700000000
"), Some(1_700_000_000));
        assert_eq!(parse_stamp(""), None);
        assert_eq!(parse_stamp("nope"), None);
    }

    #[test]
    fn tick_respects_schedule_and_errors() {
        let rel = || Release { version: "9.9.9".into(), url: "u".into() };
        let (s, r) = tick(Some(1000), 1100, || panic!("must not run"));
        assert_eq!((s, r), (None, None));
        assert_eq!(tick(None, 5, || Ok(Some(rel()))), (Some(5), Some(rel())));
        assert_eq!(tick(None, 5, || Ok(None)), (Some(5), None));
        assert_eq!(tick(None, 5, || Err("x".into())), (None, None));
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

//! Whether a newer bana is out: two minutes after the daemon starts, then
//! once a day, `curl` asks `<releases>/latest` where it redirects, with the
//! settings' PATH. The tag after `/tag/` is kept in memory while it is newer
//! than this bana ([`crate::registry::Registry::latest`]): the health, the
//! page, the menu bar, `bana list` and `bana daemon status` say so. A failure
//! keeps what was known, with one line in the log.
//!
//! Off with `~/.bana/.no-upgrade-check`, or BANA_RELEASES=off. BANA_RELEASES
//! names another releases page (a mirror, or tests).

use crate::notes;
use std::cmp::Ordering;
use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::process::Command;

/// bana's releases.
pub const RELEASES: &str = "https://github.com/tjrb-xyz/bana/releases";
/// The first check, after the daemon starts.
pub const FIRST: Duration = Duration::from_secs(120);
/// Then once a day.
pub const EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// Its file in bana's home turns the check off.
pub const OFF: &str = ".no-upgrade-check";

/// The releases page to ask, from BANA_RELEASES; None when it is off.
pub fn releases(env: Option<&str>) -> Option<String> {
    match env {
        Some("off") => None,
        Some(r) if !r.is_empty() => Some(r.trim_end_matches('/').to_string()),
        _ => Some(RELEASES.to_string()),
    }
}

/// The check is off in bana's home `home`, or for BANA_RELEASES `env`.
pub fn off(home: &Path, env: Option<&str>) -> bool {
    home.join(OFF).exists() || releases(env).is_none()
}

/// `vX.Y.Z` (or `vX.Y.Z-pre`) after `/tag/` in where `latest` redirects.
/// None without one (no release yet: GitHub redirects to the releases).
pub fn tag_from_redirect(url: &str) -> Option<String> {
    let (_, tag) = url.trim().rsplit_once("/tag/")?;
    let (core, pre) = match tag.strip_prefix('v')?.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (tag.strip_prefix('v')?, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let number = |p: &&str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let pre_ok = pre.is_none_or(|p| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
    });
    (parts.len() == 3 && parts.iter().all(number) && pre_ok).then(|| tag.to_string())
}

/// `tag` is a newer bana than version `current`.
pub fn newer(tag: &str, current: &str) -> bool {
    notes::version_cmp(tag, current) == Ordering::Greater
}

/// Asks `<releases>/latest` once, with `path` as PATH. Ok(Some(tag)) when a
/// newer bana than `current` is out, Ok(None) when not (or no release yet),
/// Err when curl failed.
pub async fn check(releases: &str, path: &str, current: &str) -> Result<Option<String>, String> {
    let latest = format!("{releases}/latest");
    let run = Command::new("curl")
        .args(["-fsS", "--max-time", "20", "-o", "/dev/null"])
        .args(["-w", "%{redirect_url}", &latest])
        .env("PATH", path)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let o = match tokio::time::timeout(Duration::from_secs(30), run).await {
        Err(_) => return Err(format!("curl {latest}: took longer than 30 s")),
        Ok(Err(e)) => return Err(format!("curl: {e}")),
        Ok(Ok(o)) => o,
    };
    if !o.status.success() {
        let why = String::from_utf8_lossy(&o.stderr);
        return Err(format!("curl {latest}: {}", why.trim()));
    }
    let tag = tag_from_redirect(&String::from_utf8_lossy(&o.stdout));
    Ok(tag.filter(|t| newer(t, current)))
}

/// What a check found, into what is known: the newer tag, or none; a
/// failure keeps what was known. True when that changed.
pub fn remember(known: &Mutex<Option<String>>, found: &Result<Option<String>, String>) -> bool {
    let Ok(found) = found else { return false };
    let mut k = known.lock().unwrap_or_else(|e| e.into_inner());
    let changed = *k != *found;
    k.clone_from(found);
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn tag_from_redirect_accepts_and_rejects() {
        let r = "https://github.com/tjrb-xyz/bana/releases";
        for (url, want) in [
            (format!("{r}/tag/v0.2.0"), Some("v0.2.0")),
            (format!("{r}/tag/v1.10.3-rc.1\n"), Some("v1.10.3-rc.1")),
            (format!("{r}/tag/v0.2"), None),
            (format!("{r}/tag/0.2.0"), None),
            (format!("{r}/tag/v0.2.0-"), None),
            (format!("{r}/tag/v0.2.0;rm"), None),
            (format!("{r}/tag/v0.2.0/x"), None),
            (format!("{r}/tag/"), None),
            (r.to_string(), None),
            (String::new(), None),
        ] {
            assert_eq!(tag_from_redirect(&url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn newer_only_when_version_cmp_higher() {
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("v0.1.1", "0.1.0"));
        assert!(newer("v0.1.0", "0.1.0-rc.1"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("v0.1.0-rc.1", "0.1.0"));
        assert!(!newer("v0.0.9", "0.1.0"));
    }

    #[test]
    fn off_file_skips() {
        let home = std::env::temp_dir().join(format!("bana-upgrade-off-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        assert!(!off(&home, None));
        assert!(off(&home, Some("off")));
        assert!(!off(&home, Some("http://127.0.0.1:1/r")));
        std::fs::write(home.join(OFF), "").unwrap();
        assert!(off(&home, None));
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(releases(None).as_deref(), Some(RELEASES));
        assert_eq!(
            releases(Some("https://m/r/")).as_deref(),
            Some("https://m/r")
        );
    }

    /// A curl on PATH that answers `$ANSWER`, or fails.
    fn fake_curl(dir: &Path, answer: &str) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let curl = dir.join("curl");
        let script = match answer {
            "fail" => "#!/bin/sh\necho 'curl: (6) no host' >&2\nexit 6\n".to_string(),
            a => format!("#!/bin/sh\nprintf '%s' '{a}'\n"),
        };
        std::fs::write(&curl, script).unwrap();
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
        format!("{}:/usr/bin:/bin", dir.display())
    }

    #[tokio::test]
    async fn check_with_fake_curl_sets_keeps_or_ignores() {
        let dir = std::env::temp_dir().join(format!("bana-upgrade-curl-{}", std::process::id()));
        let r = "https://example.test/o/r/releases";
        let known = Mutex::new(None);
        let path = fake_curl(&dir, &format!("{r}/tag/v9.9.9"));
        let found = check(r, &path, "0.1.0").await;
        assert_eq!(found, Ok(Some("v9.9.9".into())));
        assert!(remember(&known, &found), "set");
        let path = fake_curl(&dir, "fail");
        let found = check(r, &path, "0.1.0").await;
        assert!(found.as_ref().unwrap_err().contains("no host"), "{found:?}");
        assert!(!remember(&known, &found));
        assert_eq!(known.lock().unwrap().as_deref(), Some("v9.9.9"), "kept");
        let path = fake_curl(&dir, &format!("{r}/tag/v0.1.0"));
        let found = check(r, &path, "0.1.0").await;
        assert_eq!(found, Ok(None), "the same: not newer");
        assert!(remember(&known, &found));
        assert_eq!(*known.lock().unwrap(), None);
        let path = fake_curl(&dir, r);
        assert_eq!(check(r, &path, "0.1.0").await, Ok(None), "no release yet");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

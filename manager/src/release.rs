//! Releases: a tag's build that bana asks to publish, and what Publish runs.
//!
//! The owner pushes a tag; bana never makes, moves or deletes one. A build of
//! a tag at `daemon.tag_tier` makes its record, `releases/<tag>.json` in the
//! daemon's directory, and bana asks only when that build passed with files
//! for a release, and again after a publish failed. Nothing is published
//! without the owner's click: the page's Publish, with the rev of the notes
//! the owner saw.
//!
//! States:
//! - building: the tag's build is queued or runs; the notes can be edited;
//! - blocked: it failed, or left no files for a release (`reason`);
//! - asking: it passed with files: the page's card, 🧱 `v0.1.0?`, and
//!   `bana daemon status` ask;
//! - publishing: `gh release create` runs ([`crate::daemon`]'s publish task);
//! - published: with its URL; failed: with gh's words, and bana asks again;
//! - dismissed: Not now. Publish stays on the page while the files are kept.
//!
//! Only the record's current build moves it; a newer build of the tag (a
//! re-run, the tag pushed again) takes it over, but never a published or a
//! publishing one. The notes carry a rev: a save or a publish with another
//! rev is refused, so the owner and Claude never write over each other, and
//! what is published is the text the owner saw. At publish, `## Tested` (the
//! build's CI report table) and `## Install` are added below the notes, never
//! stored in them.
//!
//! This module is the pure part; the daemon runs git and gh.

use crate::notes::{self, Changes, Check, Previous};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Notes are at most this many characters.
pub const NOTES_MAX: usize = 125_000;
/// A title is at most this many characters.
pub const TITLE_MAX: usize = 200;
/// A published or dismissed release keeps its build's files this long after
/// the answer (seconds); a blocked or building one shows on the page as long.
pub const KEEP_ANSWERED: i64 = 7 * 86_400;
/// Why a record left publishing at the daemon's start failed.
pub const INTERRUPTED: &str = "interrupted (bana restarted): Publish again";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Building,
    Blocked,
    Asking,
    Publishing,
    Published,
    Failed,
    Dismissed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Building => "building",
            Self::Blocked => "blocked",
            Self::Asking => "asking",
            Self::Publishing => "publishing",
            Self::Published => "published",
            Self::Failed => "failed",
            Self::Dismissed => "dismissed",
        }
    }
}

/// A release's notes: the text, and who saved it when.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notes {
    pub text: String,
    /// The release's title, when not `<install.name> <tag>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 1 for the first notes, and one more at each save.
    pub rev: u64,
    /// `git` (bana's default notes), `claude` (the MCP) or `you` (the page).
    pub source: String,
    pub saved_at: i64,
}

/// releases/<tag>.json.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Release {
    pub tag: String,
    pub state: State,
    /// The tagged commit its build built.
    pub sha: String,
    /// The build that decides: the newest of the tag at the tag tier.
    pub build: u64,
    /// Why it is blocked, or why the publish failed (gh's last lines).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The release the notes start from, once found (none yet: not found yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<Previous>,
    /// The first-parent commits since it ([`notes::parse_range`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Changes>,
    /// The previous release and the changes are to be found (queued).
    pub seed: bool,
    /// Why git could not read the changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_error: Option<String>,
    /// The title when the notes have none: `<install.name> <tag>`, from the
    /// built commit's bana.conf, once read ([`title`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The built commit's `release.platforms`: those it should have an
    /// archive for ([`not_built`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    /// When a read last asked for the changes again (not saved).
    #[serde(skip)]
    pub retried_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<Notes>,
    /// The notes before the last save.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_notes: Option<Notes>,
    /// What the publish does now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// The release on GitHub, once published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// When it was published or dismissed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<i64>,
    pub updated_at: i64,
    /// The public repository it is published to, when that is not the
    /// project's (`release.repo`, `bana split`): bana's notes then name no
    /// pull request, commit or link of the private one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_repo: Option<String>,
}

impl Release {
    /// The notes' rev: 0 before any.
    pub fn rev(&self) -> u64 {
        self.notes.as_ref().map_or(0, |n| n.rev)
    }

    /// What the notes say of the range's pull requests (nothing for a public
    /// repository's, whose notes name none: [`notes::public_notes`]).
    pub fn check(&self) -> Check {
        if self.public_repo.is_some() {
            return Check::default();
        }
        match (&self.notes, &self.changes) {
            (Some(n), Some(c)) => notes::check(&n.text, c),
            _ => Check::default(),
        }
    }
}

/// Why a release route says no.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No such release (404).
    Missing(String),
    /// Not now: its state, a stale rev, another publish (409).
    Refused(String),
    /// The request itself (400).
    Bad(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(m) | Self::Refused(m) | Self::Bad(m) => f.write_str(m),
        }
    }
}

/// A tag that can have a record: 1 to 128 of `[A-Za-z0-9._-]`, not starting
/// with `.` or `-`. Other tags build, with no release.
pub fn valid_tag(t: &str) -> bool {
    (1..=128).contains(&t.len())
        && !t.starts_with(['.', '-'])
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Build `build` of `tag` at `sha` was queued: the record it makes or takes
/// over, building, with its previous release and changes to find. None: a
/// published or publishing record, which stays as it is.
pub fn queued(
    old: Option<&Release>,
    tag: &str,
    sha: &str,
    build: u64,
    now: i64,
) -> Option<Release> {
    let mut r = match old {
        Some(r) if matches!(r.state, State::Published | State::Publishing) => return None,
        Some(r) => r.clone(),
        None => Release {
            tag: tag.to_string(),
            ..Release::default()
        },
    };
    if r.sha != sha {
        (r.previous, r.changes) = (None, None);
    }
    (r.state, r.sha, r.build, r.seed) = (State::Building, sha.to_string(), build, true);
    (r.reason, r.progress, r.url, r.answered_at) = (None, None, None, None);
    r.updated_at = now;
    Some(r)
}

/// Build `build` ended, or left the queue: Ok when it passed with files for
/// a release (asking), else why not (blocked). Only the record's current
/// build, while it builds, moves it. Says whether it moved.
pub fn ended(r: &mut Release, build: u64, outcome: Result<(), String>, now: i64) -> bool {
    if r.build != build || r.state != State::Building {
        return false;
    }
    (r.state, r.reason) = match outcome {
        Ok(()) => (State::Asking, None),
        Err(why) => (State::Blocked, Some(why)),
    };
    r.updated_at = now;
    true
}

/// The previous release and the changes since it, found for `sha`: bana's
/// default notes go in, unless someone wrote notes (a source other than
/// git). A record taken over since (another sha) is left for its own. Says
/// whether it took them.
pub fn seeded(
    r: &mut Release,
    sha: &str,
    found: Result<(Previous, Changes), String>,
    repo: &str,
    now: i64,
) -> bool {
    if r.sha != sha {
        return false;
    }
    r.seed = false;
    r.updated_at = now;
    let (previous, changes) = match found {
        Ok(f) => f,
        Err(e) => {
            r.seed_error = Some(e);
            return true;
        }
    };
    r.seed_error = None;
    let git = r.notes.as_ref().is_none_or(|n| n.source == "git");
    if git {
        let text = match &r.public_repo {
            Some(_) => notes::public_notes(&changes),
            None => notes::default_notes(&changes, repo, previous.tag.as_deref(), &r.tag),
        };
        if r.notes.as_ref().is_none_or(|n| n.text != text) {
            let rev = r.rev() + 1;
            r.prev_notes = r.notes.take();
            r.notes = Some(Notes {
                text,
                title: None,
                rev,
                source: "git".into(),
                saved_at: now,
            });
        }
    }
    (r.previous, r.changes) = (Some(previous), Some(changes));
    true
}

/// At Publish: the previous release gh gives now, for the record's commit.
/// When it is another release than the notes started from (a lower version
/// published since, or local tags stood in when gh failed), the record takes
/// it ([`seeded`]: git's notes are written again) and the publish stops: why.
/// An answer from local tags changes nothing.
pub fn rebased(
    r: &mut Release,
    sha: &str,
    found: (Previous, Changes),
    repo: &str,
    now: i64,
) -> Option<String> {
    if !found.0.how.starts_with("gh ") {
        return None;
    }
    let was = r.previous.as_ref().map(|p| p.tag.clone());
    let new = found.0.tag.clone();
    let git = r.notes.as_ref().is_none_or(|n| n.source == "git");
    let quiet = r.previous.as_ref() == Some(&found.0);
    if quiet {
        return None;
    }
    seeded(r, sha, Ok(found), repo, now);
    let name = |t: &Option<String>| t.clone().unwrap_or_else(|| "none (a first release)".into());
    // Notes written while bana knew no previous release stay the owner's.
    match was {
        None => None,
        Some(was) if was == new => None,
        Some(was) => Some(format!(
            "the previous release is now {} (the notes started from {}): {}; nothing was published",
            name(&new),
            name(&was),
            if git {
                "bana wrote them again from it, read them, then Publish again"
            } else {
                "they may leave out changes or link the wrong range, read them, then Publish again"
            }
        )),
    }
}

/// Saves notes (the page's, `you`, or Claude's, `claude`) over those at `rev`:
/// the new rev. The text before is kept as `prev_notes`.
pub fn save_notes(
    r: &mut Release,
    text: &str,
    title: Option<&str>,
    rev: u64,
    source: &str,
    now: i64,
) -> Result<u64, Error> {
    if text.chars().count() > NOTES_MAX {
        return Err(Error::Bad(format!("notes: at most {NOTES_MAX} characters")));
    }
    if title.is_some_and(|t| t.chars().count() > TITLE_MAX || t.contains('\n')) {
        return Err(Error::Bad(format!(
            "title: one line of at most {TITLE_MAX} characters"
        )));
    }
    if !matches!(source, "you" | "claude") {
        return Err(Error::Bad("source: you or claude".into()));
    }
    if matches!(r.state, State::Publishing | State::Published) {
        return Err(Error::Refused(format!(
            "{} is {}: its notes stay as they are",
            r.tag,
            r.state.as_str()
        )));
    }
    if rev != r.rev() {
        let by = r.notes.as_ref().map_or("nobody", |n| n.source.as_str());
        return Err(Error::Refused(format!(
            "the notes changed since rev {rev}: rev {} is {by}'s; read it, then save over it",
            r.rev()
        )));
    }
    let next = rev + 1;
    r.prev_notes = r.notes.take();
    r.notes = Some(Notes {
        text: text.to_string(),
        title: title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from),
        rev: next,
        source: source.to_string(),
        saved_at: now,
    });
    r.updated_at = now;
    Ok(next)
}

/// Whether the owner's yes, with the notes' `rev`, may publish it now.
pub fn can_publish(r: &Release, rev: u64) -> Result<(), Error> {
    if !matches!(r.state, State::Asking | State::Failed | State::Dismissed) {
        return Err(Error::Refused(format!(
            "{} is {}: only a release bana asks about can be published",
            r.tag,
            r.state.as_str()
        )));
    }
    if rev != r.rev() {
        return Err(Error::Refused(format!(
            "the notes changed since rev {rev} (now rev {}): read them before publishing",
            r.rev()
        )));
    }
    if r.notes.as_ref().is_none_or(|n| n.text.trim().is_empty()) {
        return Err(Error::Refused("no notes yet: write some first".into()));
    }
    Ok(())
}

/// Not now: bana stops asking.
pub fn dismiss(r: &mut Release, now: i64) -> Result<(), Error> {
    if !matches!(r.state, State::Asking | State::Failed) {
        return Err(Error::Refused(format!(
            "{} is {}: bana is not asking about it",
            r.tag,
            r.state.as_str()
        )));
    }
    (r.state, r.answered_at, r.updated_at) = (State::Dismissed, Some(now), now);
    Ok(())
}

/// At the daemon's start: a publish it was running did not finish. Publish
/// again looks for what gh left first. Says whether it changed.
pub fn interrupted(r: &mut Release) -> bool {
    if r.state != State::Publishing {
        return false;
    }
    (r.state, r.reason, r.progress) = (State::Failed, Some(INTERRUPTED.into()), None);
    true
}

/// The release the summary shows: one publishing, else the newest bana asks
/// about (asking, or failed), else the newest building or blocked one of the
/// last week.
pub fn shown(releases: &BTreeMap<String, Release>, now: i64) -> Option<&Release> {
    let newest = |states: &[State], since: i64| {
        releases
            .values()
            .filter(|r| states.contains(&r.state) && r.updated_at >= since)
            .max_by_key(|r| r.updated_at)
    };
    newest(&[State::Publishing], i64::MIN)
        .or_else(|| newest(&[State::Asking, State::Failed], i64::MIN))
        .or_else(|| newest(&[State::Building, State::Blocked], now - KEEP_ANSWERED))
}

/// The builds whose files a release still needs: while it builds, asks,
/// publishes or failed, and a week after it was published or dismissed.
pub fn kept_builds(releases: &BTreeMap<String, Release>, now: i64) -> BTreeSet<u64> {
    releases
        .values()
        .filter(|r| match r.state {
            State::Building | State::Asking | State::Publishing | State::Failed => true,
            State::Published | State::Dismissed => {
                r.answered_at.unwrap_or(r.updated_at) >= now - KEEP_ANSWERED
            }
            State::Blocked => false,
        })
        .map(|r| r.build)
        .collect()
}

/// The records in `<dir>/releases`.
pub fn load(dir: &Path) -> BTreeMap<String, Release> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir.join("releases"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(tag) = name.strip_suffix(".json").filter(|t| valid_tag(t)) else {
            continue;
        };
        match std::fs::read(e.path())
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<Release>(&b).map_err(|e| e.to_string()))
        {
            Ok(r) if r.tag == tag => {
                out.insert(tag.to_string(), r);
            }
            Ok(_) => eprintln!("bana daemon: releases/{name}: another tag"),
            Err(e) => eprintln!("bana daemon: releases/{name}: {e}"),
        }
    }
    out
}

/// `<dir>/releases/<tag>.json`, whole or not at all.
pub fn save(dir: &Path, r: &Release) {
    let d = dir.join("releases");
    if let Err(e) = std::fs::create_dir_all(&d)
        .and_then(|_| crate::daemon::write_json(&d.join(format!("{}.json", r.tag)), r))
    {
        eprintln!("bana daemon: releases/{}.json: {e}", r.tag);
    }
}

// ---- publishing ------------------------------------------------------------

/// SHA256SUMS: each file's sha256 and name. It lists exactly what a release
/// uploads, but itself.
pub fn manifest(sums: &str) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in sums.lines().filter(|l| !l.trim().is_empty()) {
        let (hash, name) = line
            .split_once(' ')
            .map(|(h, n)| (h, n.trim_start_matches(' ').trim_start_matches('*')))
            .ok_or_else(|| format!("SHA256SUMS: {line:?} is not HASH  NAME"))?;
        let ok_hash = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        // dist/'s names (artifacts::normalize), none an option to gh.
        let ok_name = !name.is_empty()
            && name.len() <= 255
            && !name.starts_with(['.', '-'])
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            && name != "SHA256SUMS";
        if !ok_hash || !ok_name || out.iter().any(|(_, n)| n == name) {
            return Err(format!("SHA256SUMS: {line:?} is not HASH  NAME"));
        }
        out.push((hash.to_ascii_lowercase(), name.to_string()));
    }
    if out.is_empty() {
        return Err("SHA256SUMS lists no file".into());
    }
    Ok(out)
}

/// The tag's commit in `git ls-remote origin refs/tags/T 'refs/tags/T^{}'`:
/// the peeled one of an annotated tag, else the tag's own.
pub fn peeled(ls_remote: &str, tag: &str) -> Option<String> {
    let (plain, deref) = (format!("refs/tags/{tag}"), format!("refs/tags/{tag}^{{}}"));
    let find = |want: &str| {
        ls_remote.lines().find_map(|l| {
            let (sha, r) = l.split_once('\t')?;
            (r.trim() == want).then(|| sha.trim().to_string())
        })
    };
    find(&deref).or_else(|| find(&plain))
}

/// `gh release view`'s fields for [`existing`].
pub const VIEW_FIELDS: &str = "isDraft,url,body,assets";

/// What `gh release view <tag> --json isDraft,url,body,assets` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Existing {
    None,
    /// A draft: gh's own, left when a publish was killed, or anyone's
    /// ([`orphan`] tells).
    Draft {
        body: String,
        assets: Vec<Asset>,
    },
    Published {
        url: String,
        assets: Vec<Asset>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewed {
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// A release's file on GitHub: its name, and `sha256:<hex>` once GitHub has it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(default)]
    pub digest: Option<String>,
}

/// Whether a draft is the one a killed `gh release create` of bana's left:
/// its body is the notes bana last sent (`sent`, releases/<tag>.notes.md),
/// and it holds only files bana uploads. Another draft (one written on
/// GitHub, release-drafter's) is never deleted.
pub fn orphan(body: &str, assets: &[Asset], sent: Option<&str>, upload: &[String]) -> bool {
    let norm = |s: &str| s.replace("\r\n", "\n").trim().to_string();
    sent.is_some_and(|t| !t.trim().is_empty() && norm(t) == norm(body))
        && assets.iter().all(|a| upload.contains(&a.name))
}

/// Whether a published release holds exactly the files bana would upload:
/// `want` is each name and its sha256, and GitHub's digest must be it.
pub fn same_files(assets: &[Asset], want: &[(String, String)]) -> bool {
    assets.len() == want.len()
        && want.iter().all(|(hash, name)| {
            assets.iter().any(|a| {
                a.name == *name
                    && a.digest
                        .as_deref()
                        .is_some_and(|d| d.eq_ignore_ascii_case(&format!("sha256:{hash}")))
            })
        })
}

/// What in a release file's bytes would tell a public release repository's
/// readers about the private code (`bana split`): the first of `needles` it
/// holds, ASCII case aside. A needle `(text, true)` is a repository name,
/// matched as a whole name (`o/r` is not in `o/r-releases`, is in
/// `github.com/o/r.git`); `(text, false)` a path, matched anywhere.
pub fn leak<'a>(bytes: &[u8], needles: &'a [(String, bool)]) -> Option<&'a str> {
    let hay = bytes.to_ascii_lowercase();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    for (text, name) in needles {
        let n = text.to_ascii_lowercase().into_bytes();
        if n.is_empty() || n.len() > hay.len() {
            continue;
        }
        let found = (0..=hay.len() - n.len()).any(|i| {
            hay[i..i + n.len()] == n[..]
                && (!name
                    || ((i == 0 || !(word(hay[i - 1]) || hay[i - 1] == b'.'))
                        && hay.get(i + n.len()).is_none_or(|&b| !word(b))))
        });
        if found {
            return Some(text);
        }
    }
    None
}

/// gh release view's output, or its error: `release not found` is none.
pub fn existing(view: Result<&str, &str>) -> Result<Existing, String> {
    let text = match view {
        Ok(t) => t,
        Err(e) if e.to_ascii_lowercase().contains("not found") => return Ok(Existing::None),
        Err(e) => return Err(format!("gh release view: {}", e.trim())),
    };
    let v: Viewed = serde_json::from_str(text)
        .map_err(|e| format!("gh release view printed no release: {e}"))?;
    Ok(if v.is_draft {
        Existing::Draft {
            body: v.body.unwrap_or_default(),
            assets: v.assets,
        }
    } else {
        Existing::Published {
            url: v.url,
            assets: v.assets,
        }
    })
}

/// `gh release list` for [`flags`]: the published final releases.
pub fn finals_args(repo: &str) -> Vec<String> {
    [
        "release",
        "list",
        "-R",
        repo,
        "--exclude-drafts",
        "--exclude-pre-releases",
        "-L",
        "100",
        "--json",
        "tagName",
    ]
    .map(String::from)
    .to_vec()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Final {
    tag_name: String,
}

/// gh release create's flags for `tag`: a prerelease is one, and no latest;
/// a final version is `--latest` when it is above every published final
/// version ([`finals_args`]), else `--latest=false`, so a hotfix on an old
/// line never becomes Latest. When gh's list cannot be read, `--latest=false`
/// too: GitHub would make it Latest by default.
pub fn flags(tag: &str, finals: Result<&str, &str>) -> Vec<String> {
    if notes::is_prerelease(tag) {
        return vec!["--prerelease".into()];
    }
    if notes::version(tag).is_none() {
        return vec![];
    }
    let Some(listed) = finals
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Final>>(t).ok())
    else {
        return vec!["--latest=false".into()];
    };
    let above = listed
        .iter()
        .filter(|f| f.tag_name != tag && notes::version(&f.tag_name).is_some())
        .all(|f| notes::version_cmp(tag, &f.tag_name) == std::cmp::Ordering::Greater);
    vec![if above { "--latest" } else { "--latest=false" }.into()]
}

/// The create: `--verify-tag`, so gh never makes the tag. In a public
/// release repository (`public`), which has none of the private commits, gh
/// makes the tag itself, on that repository's default branch.
pub fn create_args(
    repo: &str,
    public: bool,
    tag: &str,
    title: &str,
    notes_file: &Path,
    flags: &[String],
    files: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = ["release", "create", tag, "-R", repo]
        .map(String::from)
        .to_vec();
    if !public {
        a.push("--verify-tag".into());
    }
    a.extend(["--title", title, "--notes-file"].map(String::from));
    a.push(notes_file.to_string_lossy().into_owned());
    a.extend(flags.iter().cloned());
    a.extend(files.iter().cloned());
    a
}

/// What gets published: the notes, then `## Tested` and `## Install`.
pub fn body(notes: &str, tested: Option<&str>, install: Option<&str>) -> String {
    let mut s = notes.trim_end().to_string();
    for (head, part) in [("Tested", tested), ("Install", install)] {
        if let Some(p) = part.map(str::trim).filter(|p| !p.is_empty()) {
            s.push_str(&format!("\n\n## {head}\n\n{p}"));
        }
    }
    s.push('\n');
    s
}

/// The build's CI report (report.md) for `## Tested`: its line on where and
/// how it ran, and its standards table.
pub fn tested(report: &str) -> Option<String> {
    let lines: Vec<&str> = report.lines().collect();
    let at = lines.iter().position(|l| l.starts_with("| Standard |"))?;
    let end = lines[at..]
        .iter()
        .position(|l| !l.starts_with('|'))
        .map_or(lines.len(), |n| at + n);
    let meta = lines[..at]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#'));
    let mut s = String::new();
    if let Some(m) = meta {
        s.push_str(m);
        s.push_str("\n\n");
    }
    s.push_str(&lines[at..end].join("\n"));
    Some(s)
}

/// `## Install`: the one-liners for a release with bana's installer, the
/// platforms, and SHA256SUMS. None without an installer.
pub fn install(repo: &str, tag: &str, names: &[String], platforms: &[String]) -> Option<String> {
    let has = |n: &str| names.iter().any(|x| x == n);
    if !has("install.sh") && !has("install.ps1") {
        return None;
    }
    let mut s = String::new();
    if has("install.sh") {
        s.push_str(&format!(
            "```sh\ncurl -fsSL https://github.com/{repo}/releases/download/{tag}/install.sh | sh\n\
             gh release download {tag} -R {repo} -p install.sh -O - | sh   # a private repository\n```\n\n"
        ));
    }
    if has("install.ps1") {
        s.push_str(&format!(
            "```powershell\nirm https://github.com/{repo}/releases/download/{tag}/install.ps1 | iex\n\
             gh release download {tag} -R {repo} -p install.ps1 -O - | Out-String | iex   # a private repository\n```\n\n"
        ));
    }
    if !platforms.is_empty() {
        s.push_str(&format!("Platforms: {}. ", platforms.join(", ")));
    }
    s.push_str("SHA256SUMS lists every file.");
    Some(s)
}

/// The platforms of `declared` (the built commit's `release.platforms`) that
/// the release has no archive for. None declared: none listed.
pub fn not_built(declared: &[String], built: &[String]) -> Vec<String> {
    declared
        .iter()
        .filter(|p| !built.contains(p))
        .cloned()
        .collect()
}

/// A key's value in a bana.conf's text: the last line that sets it.
fn conf_value(conf: &str, key: &str) -> Option<String> {
    let mut found = None;
    for line in conf.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                found = Some(v.trim().to_string());
            }
        }
    }
    found
}

/// The release's title: `<install.name> <tag>`, install.name from the built
/// commit's bana.conf (as `conf` reads it), else the prefix.
pub fn title(conf: &str, prefix: &str, tag: &str) -> String {
    let name = conf_value(conf, "install.name")
        .filter(|n| crate::valid_runner(n))
        .unwrap_or_else(|| prefix.to_string());
    format!("{name} {tag}")
}

/// `release.platforms` in the built commit's bana.conf: the installer's
/// platforms (`linux-arm64 linux-x64 macos-arm64 macos-x64 windows-x64`,
/// spaces or commas) a release should have an archive for.
pub fn platforms(conf: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in conf_value(conf, "release.platforms")
        .unwrap_or_default()
        .split([' ', ',', '\t'])
        .filter(|p| !p.is_empty())
    {
        let ok = p.len() <= 32
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if ok && !out.iter().any(|x| x == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// The last lines a program printed, for the page: at most 8, 1,500
/// characters.
pub fn tail(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    let s = lines[lines.len().saturating_sub(8)..].join("\n");
    let n = s.chars().count();
    if n > 1500 {
        s.chars().skip(n - 1500).collect()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes() -> Changes {
        Changes {
            prs: vec![
                notes::Pr {
                    number: 7,
                    title: "Faster".into(),
                    merge_sha: "b".repeat(40),
                },
                notes::Pr {
                    number: 5,
                    title: "Safer".into(),
                    merge_sha: "c".repeat(40),
                },
            ],
            other: vec![],
            more: 0,
        }
    }

    fn found(tag: Option<&str>) -> Result<(Previous, Changes), String> {
        Ok((
            Previous {
                tag: tag.map(String::from),
                how: "gh release list".into(),
            },
            changes(),
        ))
    }

    #[test]
    fn the_state_machine() {
        let a = "a".repeat(40);
        // Queued gives building, with its previous release and changes to find.
        let mut r = queued(None, "v0.1.0", &a, 3, 10).unwrap();
        assert_eq!(
            (r.state, r.build, r.seed, r.rev()),
            (State::Building, 3, true, 0)
        );
        // Found: bana's default notes, rev 1.
        assert!(seeded(&mut r, &a, found(Some("v0.0.9")), "o/r", 11));
        let n = r.notes.clone().unwrap();
        assert_eq!((n.rev, n.source.as_str(), r.seed), (1, "git", false));
        assert!(n.text.ends_with("/compare/v0.0.9...v0.1.0\n"), "{}", n.text);
        assert_eq!(r.check(), Check::default());
        // Another build's end moves nothing; its own does.
        assert!(!ended(&mut r, 2, Ok(()), 12));
        assert!(ended(&mut r, 3, Ok(()), 12));
        assert_eq!(r.state, State::Asking);
        assert!(
            !ended(&mut r, 3, Err("x".into()), 13),
            "only while building"
        );

        // A re-run takes over, and keeps notes someone wrote.
        assert_eq!(
            save_notes(&mut r, "- Faster (#7)\n", None, 1, "claude", 14),
            Ok(2)
        );
        assert_eq!(r.check().missing, [5]);
        let mut r = queued(Some(&r), "v0.1.0", &a, 4, 15).unwrap();
        assert_eq!(
            (r.state, r.build, r.reason.as_deref()),
            (State::Building, 4, None)
        );
        assert!(seeded(&mut r, &a, found(Some("v0.0.9")), "o/r", 16));
        assert_eq!(
            r.notes.as_ref().unwrap().text,
            "- Faster (#7)\n",
            "Claude's stay"
        );
        assert_eq!(r.rev(), 2);
        // Green without files, or failed, gives blocked.
        assert!(ended(&mut r, 4, Err("no files".into()), 17));
        assert_eq!(
            (r.state, r.reason.as_deref()),
            (State::Blocked, Some("no files"))
        );
        // A seed for another sha (the tag moved since) is left for its own.
        let mut moved = queued(Some(&r), "v0.1.0", &"d".repeat(40), 5, 18).unwrap();
        assert!(moved.previous.is_none() && moved.changes.is_none());
        assert!(!seeded(&mut moved, &a, found(None), "o/r", 19));
        assert!(moved.seed);

        // A published record is untouched by a re-run; so is a publishing one.
        let mut p = r.clone();
        p.state = State::Published;
        assert_eq!(queued(Some(&p), "v0.1.0", &a, 9, 20), None);
        p.state = State::Publishing;
        assert_eq!(queued(Some(&p), "v0.1.0", &a, 9, 20), None);
        // One left publishing at the start failed.
        assert!(interrupted(&mut p));
        assert_eq!(
            (p.state, p.reason.as_deref()),
            (State::Failed, Some(INTERRUPTED))
        );
        assert!(!interrupted(&mut p));

        // git's notes follow the tag when it moves; a failed git log says so.
        let mut g = queued(None, "v1", &a, 1, 0).unwrap();
        assert!(seeded(&mut g, &a, found(None), "o/r", 1));
        assert!(g.notes.as_ref().unwrap().text.ends_with("/commits/v1\n"));
        let mut g = queued(Some(&g), "v1", &a, 2, 2).unwrap();
        assert!(seeded(&mut g, &a, found(Some("v0")), "o/r", 3));
        assert_eq!(
            (g.rev(), g.prev_notes.as_ref().map(|n| n.rev)),
            (2, Some(1))
        );
        let mut g = queued(Some(&g), "v1", &a, 3, 4).unwrap();
        assert!(seeded(&mut g, &a, Err("git log: bad".into()), "o/r", 5));
        assert_eq!(
            (g.seed_error.as_deref(), g.rev()),
            (Some("git log: bad"), 2)
        );
    }

    #[test]
    fn notes_saves_publishes_and_answers() {
        let mut r = queued(None, "v1.0.0", "abc", 1, 0).unwrap();
        assert!(
            matches!(can_publish(&r, 0), Err(Error::Refused(_))),
            "building"
        );
        assert!(ended(&mut r, 1, Ok(()), 1));
        assert!(matches!(can_publish(&r, 0), Err(Error::Refused(m)) if m.contains("no notes")));
        let stale = save_notes(&mut r, "x", None, 3, "you", 2);
        assert!(matches!(stale, Err(Error::Refused(m)) if m.contains("rev 0 is nobody's")));
        let big = "x".repeat(NOTES_MAX + 1);
        assert!(matches!(
            save_notes(&mut r, &big, None, 0, "you", 2),
            Err(Error::Bad(_))
        ));
        let long = "t".repeat(TITLE_MAX + 1);
        assert!(matches!(
            save_notes(&mut r, "x", Some(&long), 0, "you", 2),
            Err(Error::Bad(_))
        ));
        assert!(matches!(
            save_notes(&mut r, "x", None, 0, "me", 2),
            Err(Error::Bad(_))
        ));
        assert_eq!(
            save_notes(&mut r, "First", Some(" Big one "), 0, "you", 3),
            Ok(1)
        );
        assert_eq!(r.notes.as_ref().unwrap().title.as_deref(), Some("Big one"));
        assert_eq!(save_notes(&mut r, "Second", None, 1, "claude", 4), Ok(2));
        assert_eq!(r.prev_notes.as_ref().unwrap().text, "First");
        assert!(
            matches!(can_publish(&r, 1), Err(Error::Refused(m)) if m.contains("rev 1 (now rev 2)"))
        );
        assert_eq!(can_publish(&r, 2), Ok(()));
        assert_eq!(dismiss(&mut r, 5), Ok(()));
        assert_eq!((r.state, r.answered_at), (State::Dismissed, Some(5)));
        assert!(dismiss(&mut r, 6).is_err(), "not asking any more");
        assert_eq!(can_publish(&r, 2), Ok(()), "Publish stays on the page");
        r.state = State::Published;
        assert!(matches!(
            save_notes(&mut r, "x", None, 2, "you", 7),
            Err(Error::Refused(_))
        ));
    }

    #[test]
    fn what_the_summary_shows_and_what_prune_keeps() {
        let rel = |tag: &str, state: State, build: u64, at: i64| {
            (
                tag.to_string(),
                Release {
                    tag: tag.into(),
                    state,
                    build,
                    updated_at: at,
                    answered_at: matches!(state, State::Published | State::Dismissed).then_some(at),
                    ..Release::default()
                },
            )
        };
        let now = 100 * 86_400;
        let mut all: BTreeMap<String, Release> = [
            rel("v1", State::Published, 1, now - 8 * 86_400),
            rel("v2", State::Dismissed, 2, now - 86_400),
            rel("v3", State::Blocked, 3, now - 10),
            rel("v4", State::Asking, 4, now - 50),
            rel("v5", State::Failed, 5, now - 60),
        ]
        .into_iter()
        .collect();
        assert_eq!(shown(&all, now).map(|r| r.tag.as_str()), Some("v4"));
        assert_eq!(
            kept_builds(&all, now).into_iter().collect::<Vec<_>>(),
            [2, 4, 5]
        );
        all.extend([rel("v6", State::Publishing, 6, now - 99)]);
        assert_eq!(shown(&all, now).map(|r| r.tag.as_str()), Some("v6"));
        let quiet: BTreeMap<String, Release> = [
            rel("v3", State::Blocked, 3, now - 10),
            rel("v7", State::Building, 7, now - 5),
            rel("v8", State::Building, 8, now - 9 * 86_400),
        ]
        .into_iter()
        .collect();
        assert_eq!(shown(&quiet, now).map(|r| r.tag.as_str()), Some("v7"));
        assert_eq!(shown(&quiet, now + 8 * 86_400), None, "a week later");
        assert!(valid_tag("v0.1.0-rc1") && valid_tag("1.0_b"));
        for t in ["", ".v", "-v", "v/1", "v 1", "v1^{}", &"v".repeat(129)] {
            assert!(!valid_tag(t), "{t}");
        }
    }

    #[test]
    fn the_manifest_the_tag_and_what_github_has() {
        let h = "ab".repeat(32);
        // A .deb name over 80 characters, as dist/ keeps it (a~b+c is a.b.c there).
        let deb = "example-camilladsp-and-friends_0.1.0.rc1.g1234567..dirty-1.bookworm.local1.plus_arm64.deb";
        let sums = format!("{h}  a-linux-x64.tar.gz\n{h} *install.sh\n{h}  {deb}\n");
        assert_eq!(
            manifest(&sums).unwrap(),
            [
                (h.clone(), "a-linux-x64.tar.gz".into()),
                (h.clone(), "install.sh".into()),
                (h.clone(), deb.into())
            ]
        );
        for bad in [
            "".to_string(),
            format!("{h}  ../x"),
            format!("{h}  -x.tar.gz"),
            format!("{h}  a~b"),
            format!("{h}  SHA256SUMS"),
            "0  a".to_string(),
            format!("{h}  a\n{h}  a"),
        ] {
            assert!(manifest(&bad).is_err(), "{bad:?}");
        }
        let ls = "1111\trefs/tags/v1\n2222\trefs/tags/v1^{}\n";
        assert_eq!(peeled(ls, "v1").as_deref(), Some("2222"), "annotated");
        assert_eq!(
            peeled("3333\trefs/tags/v1\n", "v1").as_deref(),
            Some("3333")
        );
        assert_eq!(peeled("3333\trefs/tags/v10\n", "v1"), None);

        assert_eq!(existing(Err("release not found\n")), Ok(Existing::None));
        assert!(existing(Err("HTTP 401: Bad credentials")).is_err());
        let asset = |name: &str, digest: Option<&str>| Asset {
            name: name.into(),
            digest: digest.map(String::from),
        };
        assert_eq!(
            existing(Ok(r#"{"isDraft":true,"url":"u","body":null,"assets":[]}"#)),
            Ok(Existing::Draft {
                body: String::new(),
                assets: vec![]
            })
        );
        let sha = format!("sha256:{h}");
        let published = format!(
            r#"{{"isDraft":false,"url":"https://github.com/o/r/releases/tag/v1","body":"x","assets":[{{"name":"a","digest":"{sha}"}},{{"name":"SHA256SUMS","digest":null}}]}}"#
        );
        assert_eq!(
            existing(Ok(&published)),
            Ok(Existing::Published {
                url: "https://github.com/o/r/releases/tag/v1".into(),
                assets: vec![asset("a", Some(&sha)), asset("SHA256SUMS", None)]
            })
        );
        assert!(existing(Ok("nope")).is_err());

        // Only a draft with the body bana sent, and bana's files, is gh's orphan.
        let upload = ["a".to_string(), "SHA256SUMS".to_string()];
        let sent = Some("Notes\r\n\n## Tested\n");
        assert!(orphan(
            "Notes\n\n## Tested",
            &[asset("a", None)],
            sent,
            &upload
        ));
        assert!(orphan("Notes\n\n## Tested\n", &[], sent, &upload));
        assert!(
            !orphan("## Next\n- drafted", &[], sent, &upload),
            "release-drafter's"
        );
        assert!(!orphan(
            "Notes\n\n## Tested",
            &[asset("b", None)],
            sent,
            &upload
        ));
        assert!(!orphan("", &[], None, &upload), "bana never sent notes");
        assert!(!orphan("", &[], Some("  \n"), &upload));
        // A published release is this one only with these bytes.
        let want = [
            (h.clone(), "a".to_string()),
            ("cd".repeat(32), "SHA256SUMS".into()),
        ];
        let right = [
            asset("a", Some(&sha)),
            asset("SHA256SUMS", Some(&format!("sha256:{}", "CD".repeat(32)))),
        ];
        assert!(same_files(&right, &want));
        assert!(!same_files(&right[..1], &want));
        assert!(!same_files(
            &[right[0].clone(), asset("SHA256SUMS", None)],
            &want
        ));
        let other = asset("a", Some(&format!("sha256:{}", "0".repeat(64))));
        assert!(!same_files(&[other, right[1].clone()], &want));
    }

    #[test]
    fn the_previous_release_at_publish() {
        let a = "a".repeat(40);
        let prev = |tag: Option<&str>, how: &str| Previous {
            tag: tag.map(String::from),
            how: how.into(),
        };
        let mut r = queued(None, "v0.3.0", &a, 1, 0).unwrap();
        assert!(seeded(&mut r, &a, found(Some("v0.1.0")), "o/r", 1));
        // The same answer, or local tags standing in: nothing changes.
        assert_eq!(
            rebased(
                &mut r,
                &a,
                (prev(Some("v0.1.0"), "gh release list"), changes()),
                "o/r",
                2
            ),
            None
        );
        assert_eq!(
            rebased(
                &mut r,
                &a,
                (prev(Some("v0.0.1"), "tags (gh failed: x)"), changes()),
                "o/r",
                2
            ),
            None
        );
        assert_eq!(r.rev(), 1);
        // v0.2.0 published since: git's notes are written again, and it stops.
        let why = rebased(
            &mut r,
            &a,
            (prev(Some("v0.2.0"), "gh release list"), changes()),
            "o/r",
            3,
        );
        assert_eq!(why.as_deref(), Some("the previous release is now v0.2.0 (the notes started from v0.1.0): bana wrote them again from it, read them, then Publish again; nothing was published"));
        assert!(r
            .notes
            .as_ref()
            .unwrap()
            .text
            .ends_with("/compare/v0.2.0...v0.3.0\n"));
        assert_eq!(
            (r.rev(), r.previous.clone().unwrap().tag.as_deref()),
            (2, Some("v0.2.0"))
        );
        // Claude's notes stay, and say so; local tags that were right, only recorded.
        save_notes(&mut r, "- Faster (#7)\n", None, 2, "claude", 4).unwrap();
        let why = rebased(
            &mut r,
            &a,
            (prev(None, "gh release list"), changes()),
            "o/r",
            5,
        )
        .unwrap();
        assert!(why.starts_with("the previous release is now none (a first release) (the notes started from v0.2.0): they may leave out"), "{why}");
        assert_eq!(r.notes.as_ref().unwrap().text, "- Faster (#7)\n");
        r.previous = Some(prev(None, "tags (gh failed: x)"));
        assert_eq!(
            rebased(
                &mut r,
                &a,
                (prev(None, "gh release list"), changes()),
                "o/r",
                6
            ),
            None
        );
        assert_eq!(r.previous.clone().unwrap().how, "gh release list");
        // Notes written while bana knew no previous release stay the owner's.
        r.previous = None;
        assert_eq!(
            rebased(
                &mut r,
                &a,
                (prev(Some("v0.2.0"), "gh release list"), changes()),
                "o/r",
                7
            ),
            None
        );
    }

    #[test]
    fn latest_only_for_the_newest_final() {
        let list = r#"[{"tagName":"v0.2.0"},{"tagName":"v0.1.0"},{"tagName":"nightly"}]"#;
        assert_eq!(flags("v0.3.0", Ok(list)), ["--latest"]);
        assert_eq!(flags("v0.1.1", Ok(list)), ["--latest=false"], "a hotfix");
        assert_eq!(
            flags("v0.2.0", Ok(list)),
            ["--latest"],
            "itself does not count"
        );
        assert_eq!(flags("v0.3.0-rc1", Ok(list)), ["--prerelease"]);
        assert_eq!(flags("v1.0.0", Ok("[]")), ["--latest"], "the first");
        assert_eq!(flags("v1.0.0", Err("offline")), ["--latest=false"]);
        assert_eq!(flags("v1.0.0", Ok("<html>")), ["--latest=false"]);
        assert!(flags("nightly", Ok(list)).is_empty());
        let files = ["a.tar.gz".to_string(), "SHA256SUMS".into()];
        let args = |repo, public| {
            create_args(
                repo,
                public,
                "v1",
                "demo v1",
                Path::new("/n.md"),
                &flags("v1", Ok("[]")),
                &files,
            )
            .join(" ")
        };
        assert_eq!(
            args("o/r", false),
            "release create v1 -R o/r --verify-tag --title demo v1 --notes-file /n.md --latest a.tar.gz SHA256SUMS"
        );
        // A public release repo has none of the private commits: gh tags its default branch.
        assert_eq!(
            args("o/r-releases", true),
            "release create v1 -R o/r-releases --title demo v1 --notes-file /n.md --latest a.tar.gz SHA256SUMS"
        );
        assert_eq!(
            finals_args("o/r").join(" "),
            "release list -R o/r --exclude-drafts --exclude-pre-releases -L 100 --json tagName"
        );
    }

    #[test]
    fn what_a_public_release_file_must_not_hold() {
        let needles = vec![
            ("Acme/Widget".to_string(), true),
            ("/home/runner/work/".to_string(), false),
        ];
        let leak = |text: &str| leak(text.as_bytes(), &needles);
        assert_eq!(leak("REPO='acme/widget-releases'"), None, "the public one");
        assert_eq!(leak("see bigacme/widget, acme/widgets"), None);
        assert_eq!(leak("git@github.com:acme/widget.git"), Some("Acme/Widget"));
        assert_eq!(leak("ACME/WIDGET"), Some("Acme/Widget"), "case aside");
        assert_eq!(leak("acme/widget"), Some("Acme/Widget"));
        assert_eq!(
            leak("panicked at /home/runner/work/_temp/bana/src/src/main.rs:3"),
            Some("/home/runner/work/")
        );
        assert_eq!(leak(""), None);
    }

    #[test]
    fn what_is_published_below_the_notes() {
        let report = "# CI report: o/r · v1 abc1234 · release · passed\n\nBuild #5 on mbp · act 0.2.89\n\n| Standard | Checks | Tests | Not run here |\n|---|---|---|---|\n| **all** | 100% (3/3) | — | |\n\n## Artifacts\n";
        let t = tested(report).unwrap();
        assert_eq!(
            t,
            "Build #5 on mbp · act 0.2.89\n\n| Standard | Checks | Tests | Not run here |\n|---|---|---|---|\n| **all** | 100% (3/3) | — | |"
        );
        assert_eq!(tested("# nothing\n"), None);
        let names: Vec<String> = ["a-linux-x64.tar.gz", "install.sh", "install.ps1"]
            .map(String::from)
            .to_vec();
        let plats = vec!["linux-x64".to_string()];
        let i = install("o/r", "v1", &names, &plats).unwrap();
        assert!(i.starts_with(
            "```sh\ncurl -fsSL https://github.com/o/r/releases/download/v1/install.sh | sh\n\
             gh release download v1 -R o/r -p install.sh -O - | sh   # a private repository\n```"
        ));
        assert!(i.contains("irm https://github.com/o/r/releases/download/v1/install.ps1 | iex\n"));
        assert!(i.contains("-p install.ps1 -O - | Out-String | iex   # a private repository\n```"));
        assert!(i.ends_with("Platforms: linux-x64. SHA256SUMS lists every file."));
        assert_eq!(install("o/r", "v1", &["a.deb".into()], &[]), None);
        let b = body("Notes\n\n", Some(&t), Some(&i));
        assert!(b.starts_with("Notes\n\n## Tested\n\nBuild #5"));
        assert!(b.ends_with(&format!("## Install\n\n{i}\n")), "{b}");
        assert_eq!(body("Notes", None, None), "Notes\n");
        let conf = "release.platforms = linux-arm64, linux-x64\tmacos-arm64 linux-x64 Bad/1\n";
        let declared = platforms(conf);
        assert_eq!(declared, ["linux-arm64", "linux-x64", "macos-arm64"]);
        assert_eq!(not_built(&declared, &plats), ["linux-arm64", "macos-arm64"]);
        assert!(not_built(&[], &plats).is_empty(), "none declared");
        assert!(platforms("# release.platforms = linux-x64\n").is_empty());
        assert_eq!(title("# x\ninstall.name = demo\n", "p", "v1"), "demo v1");
        assert_eq!(title("install.name = a b\n", "p", "v1"), "p v1");
        assert_eq!(title("", "p", "v1"), "p v1");
        let many: String = (1..=20).map(|i| format!("line {i}\n\n")).collect();
        assert_eq!(tail(&many).lines().count(), 8);
        assert!(tail(&many).starts_with("line 13"));
    }
}

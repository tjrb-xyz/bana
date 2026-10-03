//! A release's notes from local git: the previous release, the commits since
//! it, and bana's default notes.
//!
//! Pure, like [`crate::watch`]. The daemon runs git and gh and hands their
//! output in:
//!
//! 1. [`previous_release`]: the published release that is the nearest ancestor
//!    of the tagged commit, from one `gh release list` ([`list_args`]). A final
//!    tag skips prereleases. When gh fails, local `v*` tags stand in, and the
//!    answer says so.
//! 2. [`parse_range`]: `git log --first-parent` from there ([`log_args`]). Each
//!    first-parent commit is exactly one of: a merged pull request (`Merge pull
//!    request #N from …`, its title in the body, or `Merge #N: title`), a
//!    squashed one (`title (#N)`), or another change. Reverts, merges without a
//!    number and direct pushes are other changes, so none is ever dropped.
//! 3. [`default_notes`], and [`check`]: which of the range's pull requests some
//!    notes leave out, name twice, or name from outside the range.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// At most this many commits are read and listed; the rest are counted.
pub const LOG_LIMIT: usize = 1000;

/// What [`parse_range`] reads: `git log --format=<this>`, one record per
/// commit: sha, parents, author, subject, body.
pub const LOG_FORMAT: &str = "%H%x1f%P%x1f%an%x1f%s%x1f%b%x1e";

/// `gh release list` for [`previous_release`]: published releases only.
pub fn list_args(repo: &str) -> Vec<String> {
    [
        "release",
        "list",
        "-R",
        repo,
        "--exclude-drafts",
        "-L",
        "100",
        "--json",
        "tagName,isPrerelease",
    ]
    .map(String::from)
    .to_vec()
}

/// `git log` for [`parse_range`]: the first-parent commits after `prev` up to
/// `sha`, or all of them for a first release. It reads objects only, so what
/// the clone has checked out does not matter.
pub fn log_args(prev: Option<&str>, sha: &str) -> Vec<String> {
    let range = match prev {
        Some(p) => format!("refs/tags/{p}..{sha}"),
        None => sha.to_string(),
    };
    vec![
        "log".into(),
        "--first-parent".into(),
        format!("--format={LOG_FORMAT}"),
        "-n".into(),
        (LOG_LIMIT + 1).to_string(),
        range,
        "--".into(),
    ]
}

/// The release the notes start from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Previous {
    /// None: a first release, whose range is all history.
    pub tag: Option<String>,
    /// How it was found: `gh release list`, or `tags (gh failed: …)`.
    pub how: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    tag_name: String,
    #[serde(default)]
    is_prerelease: bool,
}

/// The nearest published ancestor of `tag`'s commit. `list` is `gh release
/// list`'s output ([`list_args`]) or its error; `tags` are the tags in the
/// clone; `is_ancestor(t)` says whether tag `t` is an ancestor of the commit,
/// and `distance(t)` how many commits lie between (`git rev-list --count
/// t..sha`). Only listed tags the clone has count, and only versions below
/// `tag`'s (a tag that is no version is not compared); a final tag skips
/// prereleases. The fewest commits win, and a tie goes to the higher version.
/// When gh failed, or printed something else, the clone's `v*` tags stand in,
/// and `how` says so.
pub fn previous_release(
    list: Result<&str, &str>,
    tags: &[String],
    tag: &str,
    is_ancestor: impl Fn(&str) -> bool,
    distance: impl Fn(&str) -> Option<u64>,
) -> Previous {
    let final_tag = !is_prerelease(tag);
    let listed = list.and_then(|text| {
        serde_json::from_str::<Vec<Listed>>(text).map_err(|_| "gh release list printed no list")
    });
    let (candidates, how): (Vec<&str>, String) = match listed {
        Ok(listed) => (
            listed
                .iter()
                .filter(|l| !(final_tag && l.is_prerelease))
                .filter_map(|l| tags.iter().find(|t| **t == l.tag_name))
                .map(String::as_str)
                .collect(),
            "gh release list".into(),
        ),
        Err(e) => (
            tags.iter()
                .map(String::as_str)
                .filter(|t| t.starts_with('v') && !(final_tag && is_prerelease(t)))
                .collect(),
            format!("tags (gh failed: {})", one_line(e)),
        ),
    };
    let mut best: Option<(u64, &str)> = None;
    for t in candidates {
        let below = match (version(t), version(tag)) {
            (Some(_), Some(_)) => version_cmp(t, tag) == Ordering::Less,
            _ => t != tag,
        };
        if !below || !is_ancestor(t) {
            continue;
        }
        let Some(d) = distance(t) else { continue };
        let better = match best {
            None => true,
            Some((bd, bt)) => d < bd || d == bd && version_cmp(t, bt) == Ordering::Greater,
        };
        if better {
            best = Some((d, t));
        }
    }
    Previous {
        tag: best.map(|(_, t)| t.to_string()),
        how,
    }
}

/// An error's first line, cut to 200 characters.
fn one_line(e: &str) -> String {
    let line = e.trim().lines().next().unwrap_or("").trim();
    let line = if line.is_empty() { "no output" } else { line };
    line.chars().take(200).collect()
}

/// A pull request in the range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    /// Its merge or squash commit.
    pub merge_sha: String,
}

/// A first-parent commit that is not a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub subject: String,
    pub author: String,
}

/// The range's first-parent commits, newest first: each is in `prs` or in
/// `other`, once.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    pub prs: Vec<Pr>,
    pub other: Vec<Commit>,
    /// Commits beyond [`LOG_LIMIT`], not read: at least 1 when git printed
    /// more; the daemon may set the exact count.
    pub more: u64,
}

/// The first-parent commits in `git log` output ([`log_args`]). A number seen
/// twice is one pull request, at its newest commit; the older is another
/// change.
pub fn parse_range(text: &str) -> Changes {
    let mut out = Changes::default();
    let mut seen = BTreeSet::new();
    let mut n = 0;
    for rec in text.split('\x1e') {
        let rec = rec.trim_start_matches('\n');
        if rec.is_empty() {
            continue;
        }
        let f: Vec<&str> = rec.splitn(5, '\x1f').collect();
        let field = |i: usize| f.get(i).copied().unwrap_or("");
        let sha = field(0).trim();
        if sha.is_empty() {
            continue;
        }
        n += 1;
        if n > LOG_LIMIT {
            out.more += 1;
            continue;
        }
        let parents = field(1).split_whitespace().count();
        let (subject, body) = (field(3).trim(), field(4));
        match pull_request(parents, subject, body).filter(|(num, _)| seen.insert(*num)) {
            Some((number, title)) => out.prs.push(Pr {
                number,
                title,
                merge_sha: sha.to_string(),
            }),
            None => out.other.push(Commit {
                sha: sha.to_string(),
                subject: subject.to_string(),
                author: field(2).trim().to_string(),
            }),
        }
    }
    out
}

/// A commit's pull request and title, when it is a merge or a squash of one.
fn pull_request(parents: usize, subject: &str, body: &str) -> Option<(u64, String)> {
    if parents >= 2 {
        if let Some(rest) = subject.strip_prefix("Merge pull request #") {
            let (num, from) = leading_number(rest)?;
            if !from.starts_with(" from ") {
                return None;
            }
            let title = body
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or(subject);
            return Some((num, title.to_string()));
        }
        let rest = subject.strip_prefix("Merge #")?;
        let (num, title) = leading_number(rest)?;
        let title = title.strip_prefix(':')?.trim();
        return Some((
            num,
            if title.is_empty() { subject } else { title }.to_string(),
        ));
    }
    if parents != 1 || subject.starts_with("Revert \"") {
        return None;
    }
    let head = subject.strip_suffix(')')?;
    let at = head.rfind(" (#")?;
    let digits = &head[at + 3..];
    let num = number(digits)?;
    let title = head[..at].trim();
    (!title.is_empty()).then(|| (num, title.to_string()))
}

/// The number `text` starts with, and what follows it.
fn leading_number(text: &str) -> Option<(u64, &str)> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    Some((number(&text[..end])?, &text[end..]))
}

/// A pull request's number: digits only, not 0.
fn number(digits: &str) -> Option<u64> {
    if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|n| *n > 0)
}

/// bana's notes before anyone writes better ones: the pull requests, the
/// other changes, and a link to the full changelog. `repo` is `owner/name`.
pub fn default_notes(changes: &Changes, repo: &str, prev: Option<&str>, tag: &str) -> String {
    let mut s = String::new();
    if !changes.prs.is_empty() {
        s.push_str("## Pull requests\n\n");
        for p in &changes.prs {
            s.push_str(&format!("- {} (#{})\n", p.title, p.number));
        }
        s.push('\n');
    }
    if !changes.other.is_empty() || changes.more > 0 {
        s.push_str("## Other changes\n\n");
        for c in &changes.other {
            s.push_str(&format!("- {} ({})\n", c.subject, short_sha(&c.sha)));
        }
        if changes.more > 0 {
            s.push_str(&format!("- and {} more\n", changes.more));
        }
        s.push('\n');
    }
    let link = match prev {
        Some(p) => format!("https://github.com/{repo}/compare/{p}...{tag}"),
        None => format!("https://github.com/{repo}/commits/{tag}"),
    };
    s.push_str(&format!("**Full changelog**: {link}\n"));
    s
}

/// [`default_notes`] for a public release repository whose code is
/// private (`bana split`): the pull requests' titles and the commits'
/// subjects only. No `#N` (it would link the public repo's own), no commit
/// and no changelog link (both 404 there).
pub fn public_notes(changes: &Changes) -> String {
    let mut s = String::new();
    if !changes.prs.is_empty() {
        s.push_str("## Pull requests\n\n");
        for p in &changes.prs {
            s.push_str(&format!("- {}\n", p.title));
        }
        s.push('\n');
    }
    if !changes.other.is_empty() || changes.more > 0 {
        s.push_str("## Other changes\n\n");
        for c in &changes.other {
            s.push_str(&format!("- {}\n", c.subject));
        }
        if changes.more > 0 {
            s.push_str(&format!("- and {} more\n", changes.more));
        }
        s.push('\n');
    }
    if s.is_empty() {
        s.push_str("No changes.\n");
    }
    s.trim_end().to_string() + "\n"
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// What notes say of the range's pull requests. A warning, never a gate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// The range's pull requests the notes never name as `#N`.
    pub missing: Vec<u64>,
    /// Numbers the notes name that are not the range's pull requests (an issue,
    /// or an older release's).
    pub outside_range: Vec<u64>,
    /// Numbers named on more than one line.
    pub duplicated: Vec<u64>,
}

/// Which pull requests `notes` leave out, name from outside the range, or
/// name on two lines.
pub fn check(notes: &str, changes: &Changes) -> Check {
    let mut lines_of: BTreeMap<u64, usize> = BTreeMap::new();
    for line in notes.lines() {
        for n in mentions(line) {
            *lines_of.entry(n).or_default() += 1;
        }
    }
    let range: BTreeSet<u64> = changes.prs.iter().map(|p| p.number).collect();
    Check {
        missing: range
            .iter()
            .filter(|n| !lines_of.contains_key(n))
            .copied()
            .collect(),
        outside_range: lines_of
            .keys()
            .filter(|n| !range.contains(n))
            .copied()
            .collect(),
        duplicated: lines_of
            .iter()
            .filter(|(_, lines)| **lines > 1)
            .map(|(n, _)| *n)
            .collect(),
    }
}

/// The `#N` in a line, each once: `#` not after a letter, digit or `&` (so
/// not `&#39;`), then digits not followed by a letter or digit.
fn mentions(line: &str) -> BTreeSet<u64> {
    let b = line.as_bytes();
    let mut out = BTreeSet::new();
    for (i, &c) in b.iter().enumerate() {
        if c != b'#' || i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'&') {
            continue;
        }
        let end = b[i + 1..]
            .iter()
            .position(|c| !c.is_ascii_digit())
            .map_or(b.len(), |p| i + 1 + p);
        if b.get(end)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            continue;
        }
        if let Some(n) = number(&line[i + 1..end]) {
            out.insert(n);
        }
    }
    out
}

/// A tag's version: `v1.2.3`, `1.2`, `v0.1.0-rc.1`; build metadata after `+`
/// is ignored. None for a tag that is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub numbers: Vec<u64>,
    /// After the first `-`: empty for a final release.
    pub pre: String,
}

pub fn version(tag: &str) -> Option<Version> {
    let t = tag.strip_prefix('v').unwrap_or(tag);
    let t = t.split('+').next().unwrap_or(t);
    let (core, pre) = t.split_once('-').unwrap_or((t, ""));
    let numbers = core
        .split('.')
        .map(number_part)
        .collect::<Option<Vec<u64>>>()?;
    Some(Version {
        numbers,
        pre: pre.to_string(),
    })
}

/// A tag with a `-` after its version: `v0.1.0-rc1`. It gets `--prerelease`.
pub fn is_prerelease(tag: &str) -> bool {
    version(tag).is_some_and(|v| !v.pre.is_empty())
}

/// Semver's order: numbers first (a missing part is 0), then a prerelease
/// below its final, prereleases by their dot-separated parts (numbers below
/// words). Anything that is not a version sorts below every version, and by
/// name among its kind.
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (va, vb) = match (version(a), version(b)) {
        (Some(va), Some(vb)) => (va, vb),
        (Some(_), None) => return Ordering::Greater,
        (None, Some(_)) => return Ordering::Less,
        (None, None) => return a.cmp(b),
    };
    let len = va.numbers.len().max(vb.numbers.len());
    for i in 0..len {
        let (x, y) = (
            va.numbers.get(i).copied().unwrap_or(0),
            vb.numbers.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x.cmp(&y);
        }
    }
    match (va.pre.is_empty(), vb.pre.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    let (mut pa, mut pb) = (va.pre.split('.'), vb.pre.split('.'));
    loop {
        match (pa.next(), pb.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let o = match (number_part(x), number_part(y)) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

/// A version's part: digits only.
fn number_part(p: &str) -> Option<u64> {
    (!p.is_empty() && p.len() <= 18 && p.bytes().all(|c| c.is_ascii_digit()))
        .then(|| p.parse().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const GH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/stand-ins/gh");

    fn git(cwd: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Ada")
            .env("GIT_AUTHOR_EMAIL", "ada@example.com")
            .env("GIT_COMMITTER_NAME", "Ada")
            .env("GIT_COMMITTER_EMAIL", "ada@example.com")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    }

    /// The stand-in gh's `release list`: its stdout, or its error.
    fn gh_list(list: &str, signed_in: bool) -> Result<String, String> {
        let o = Command::new(GH)
            .args(list_args("o/example"))
            .env("FAKE_LOG", "/dev/null")
            .env("FAKE_GH", if signed_in { "1" } else { "0" })
            .env("FAKE_RELEASE_LIST", list)
            .output()
            .unwrap();
        if o.status.success() {
            Ok(String::from_utf8(o.stdout).unwrap())
        } else {
            Err(format!(
                "gh release list: {}{}",
                String::from_utf8_lossy(&o.stderr),
                o.status
            ))
        }
    }

    /// A repository with both merge styles, a squash, a revert of a squash, a
    /// merge without a number and direct commits; a first release (v0.1.0), a
    /// hotfix off it (v0.1.1), a prerelease (v0.2.0-rc1), a tag never
    /// published (v0.1.5), and v0.2.0 at main's head.
    struct Repo {
        dir: PathBuf,
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Repo {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("bana-notes-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let r = Self { dir };
            let d = &r.dir;
            let commit = |m: &str| git(d, &["commit", "-q", "--allow-empty", "-m", m]);
            let merge = |branch: &str, subject: &str, body: &str| {
                git(d, &["checkout", "-q", "-b", branch]);
                commit(&format!("{branch} 1"));
                commit(&format!("{branch} 2"));
                git(d, &["checkout", "-q", "main"]);
                let mut args = vec!["merge", "-q", "--no-ff", branch, "-m", subject];
                if !body.is_empty() {
                    args.extend(["-m", body]);
                }
                git(d, &args);
            };
            let tag = |t: &str| git(d, &["tag", "-a", t, "-m", t]);
            git(d, &["init", "-q", "-b", "main"]);
            commit("Start");
            tag("v0.1.0");
            git(d, &["checkout", "-q", "-b", "hotfix"]);
            commit("Fix a crash (#4)");
            tag("v0.1.1");
            git(d, &["checkout", "-q", "main"]);
            merge(
                "feat-a",
                "Merge pull request #1 from o/feat-a",
                "Add feature A",
            );
            std::fs::write(d.join("b"), "b\n").unwrap();
            git(d, &["add", "b"]);
            commit("Add B (#2)");
            let b = r.sha("HEAD");
            tag("v0.2.0-rc1");
            merge("feat-c", "Merge #3: Add C", "");
            tag("v0.1.5");
            git(d, &["revert", "--no-edit", &b]);
            git(
                d,
                &[
                    "commit",
                    "-q",
                    "--amend",
                    "-m",
                    "Revert \"Add B (#2)\" (#5)",
                ],
            );
            merge("x", "Merge branch 'x'", "");
            commit("Tidy the docs");
            tag("v0.2.0");
            r
        }

        fn sha(&self, rev: &str) -> String {
            git(&self.dir, &["rev-parse", &format!("{rev}^{{commit}}")])
                .trim()
                .to_string()
        }

        fn tags(&self) -> Vec<String> {
            git(&self.dir, &["tag", "-l"])
                .lines()
                .map(String::from)
                .collect()
        }

        /// previous_release for `tag`, as the daemon runs it.
        fn previous(&self, list: Result<&str, &str>, tag: &str) -> Previous {
            let sha = self.sha(tag);
            let ok = |t: &str| {
                Command::new("git")
                    .args([
                        "merge-base",
                        "--is-ancestor",
                        &format!("refs/tags/{t}"),
                        &sha,
                    ])
                    .current_dir(&self.dir)
                    .status()
                    .unwrap()
                    .success()
            };
            let distance = |t: &str| {
                git(
                    &self.dir,
                    &["rev-list", "--count", &format!("refs/tags/{t}..{sha}")],
                )
                .trim()
                .parse()
                .ok()
            };
            previous_release(list, &self.tags(), tag, ok, distance)
        }

        fn changes(&self, prev: Option<&str>, tag: &str) -> Changes {
            let sha = self.sha(tag);
            let args = log_args(prev, &sha);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            parse_range(&git(&self.dir, &args))
        }

        /// The first-parent commits git counts in the range.
        fn first_parent(&self, prev: Option<&str>, tag: &str) -> Vec<String> {
            let range = match prev {
                Some(p) => format!("{p}..{tag}"),
                None => tag.to_string(),
            };
            git(&self.dir, &["rev-list", "--first-parent", &range])
                .lines()
                .map(String::from)
                .collect()
        }
    }

    const LIST: &str = r#"[{"tagName":"v0.2.0-rc1","isPrerelease":true},{"tagName":"v9.9.9","isPrerelease":false},{"tagName":"v0.1.1","isPrerelease":false},{"tagName":"v0.1.0","isPrerelease":false}]"#;

    #[test]
    fn the_previous_release_and_the_range() {
        let r = Repo::new();
        let listed = gh_list(LIST, true).unwrap();
        let list = Ok(listed.as_str());
        let by_gh = |t: &str| Previous {
            tag: Some(t.to_string()),
            how: "gh release list".into(),
        };
        assert_eq!(
            r.previous(list, "v0.2.0"),
            by_gh("v0.1.0"),
            "a final tag skips the prerelease, the hotfix off the line and the unpublished tag"
        );
        assert_eq!(r.previous(list, "v0.2.0-rc1"), by_gh("v0.1.0"));
        let rc2 = r.sha("v0.2.0");
        git(&r.dir, &["tag", "v0.2.0-rc2", &rc2]);
        assert_eq!(
            r.previous(list, "v0.2.0-rc2"),
            by_gh("v0.2.0-rc1"),
            "a prerelease follows the one before"
        );
        assert_eq!(r.previous(list, "v0.1.1"), by_gh("v0.1.0"), "the hotfix");
        assert_eq!(
            r.previous(list, "v0.1.0"),
            Previous {
                tag: None,
                how: "gh release list".into()
            },
            "a first release"
        );
        assert_eq!(
            r.previous(Ok("[]"), "v0.2.0").tag,
            None,
            "nothing published yet"
        );

        let failed = gh_list(LIST, false).unwrap_err();
        let p = r.previous(Err(&failed), "v0.2.0");
        assert_eq!(
            p.tag.as_deref(),
            Some("v0.1.5"),
            "the clone's v* tags stand in"
        );
        assert_eq!(p.how, "tags (gh failed: gh release list: exit status: 1)");
        let p = r.previous(Ok("To get started with GitHub CLI"), "v0.2.0-rc2");
        assert_eq!(
            (p.tag.as_deref(), p.how.as_str()),
            (
                Some("v0.1.5"),
                "tags (gh failed: gh release list printed no list)"
            ),
            "the nearest v* tag, prereleases too for a prerelease"
        );

        // Every first-parent commit, once: as a pull request or another change.
        let c = r.changes(Some("v0.1.0"), "v0.2.0");
        let prs: Vec<(u64, &str)> = c.prs.iter().map(|p| (p.number, p.title.as_str())).collect();
        assert_eq!(prs, [(3, "Add C"), (2, "Add B"), (1, "Add feature A")]);
        let other: Vec<&str> = c.other.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(
            other,
            [
                "Tidy the docs",
                "Merge branch 'x'",
                "Revert \"Add B (#2)\" (#5)"
            ]
        );
        assert_eq!(c.other[0].author, "Ada");
        let mut seen: Vec<String> = c
            .prs
            .iter()
            .map(|p| p.merge_sha.clone())
            .chain(c.other.iter().map(|o| o.sha.clone()))
            .collect();
        let mut want = r.first_parent(Some("v0.1.0"), "v0.2.0");
        seen.sort();
        want.sort();
        assert_eq!(seen, want);
        assert_eq!(c.prs[2].merge_sha, r.sha("v0.2.0-rc1~1"));
        assert_eq!(c.more, 0);

        let first = r.changes(None, "v0.1.0");
        assert!(first.prs.is_empty());
        assert_eq!(first.other.len(), 1);
        assert_eq!(first.other[0].subject, "Start");
        let hotfix = r.changes(Some("v0.1.0"), "v0.1.1");
        assert_eq!(hotfix.prs[0].number, 4);
        assert_eq!(hotfix.prs[0].title, "Fix a crash");
    }

    #[test]
    fn pull_requests_in_the_log() {
        let rec = |sha: &str, parents: &str, subject: &str, body: &str| {
            format!("{sha}\x1f{parents}\x1fAda\x1f{subject}\x1f{body}\x1e\n")
        };
        let text = [
            rec(
                "a1",
                "p q",
                "Merge pull request #12 from o/x",
                "\n  Title from the body\nmore\n",
            ),
            rec("a2", "p q", "Merge pull request #13 from o/y", ""),
            rec("a3", "p q", "Merge #14: Colon style", "body"),
            rec("a4", "p q", "Merge #15:", ""),
            rec("a5", "p", "Squashed (#16)", ""),
            rec("a6", "p", "Squashed again (#16)", ""),
            rec("a7", "p", "Revert \"Squashed (#16)\"", ""),
            rec("a8", "p", "Not a number (#x)", ""),
            rec("a9", "p", "Merge pull request #17 from o/z", ""),
            rec("b1", "p q", "Merge #18 Add D", ""),
            rec("b2", "p q", "Merge pull request #19 into main", ""),
            rec("b3", "", "(#20)", ""),
            rec("b4", "p", "Zero (#0)", ""),
        ]
        .concat();
        let c = parse_range(&text);
        let prs: Vec<(u64, &str, &str)> = c
            .prs
            .iter()
            .map(|p| (p.number, p.title.as_str(), p.merge_sha.as_str()))
            .collect();
        assert_eq!(
            prs,
            [
                (12, "Title from the body", "a1"),
                (13, "Merge pull request #13 from o/y", "a2"),
                (14, "Colon style", "a3"),
                (15, "Merge #15:", "a4"),
                (16, "Squashed", "a5"),
            ]
        );
        let other: Vec<&str> = c.other.iter().map(|o| o.sha.as_str()).collect();
        assert_eq!(
            other,
            ["a6", "a7", "a8", "a9", "b1", "b2", "b3", "b4"],
            "a number seen before, a revert, no number, a rebased merge, other styles"
        );

        let many: String = (0..LOG_LIMIT + 2)
            .map(|i| rec(&format!("{i:040x}"), "p", "change", ""))
            .collect();
        let c = parse_range(&many);
        assert_eq!((c.other.len(), c.more), (LOG_LIMIT, 2));
        assert!(default_notes(&c, "o/r", None, "v1").contains("- and 2 more\n"));
        assert_eq!(parse_range(""), Changes::default());
    }

    #[test]
    fn default_notes_and_their_check() {
        let changes = Changes {
            prs: vec![
                Pr {
                    number: 3,
                    title: "Add C".into(),
                    merge_sha: "c".repeat(40),
                },
                Pr {
                    number: 1,
                    title: "Add feature A".into(),
                    merge_sha: "a".repeat(40),
                },
            ],
            other: vec![Commit {
                sha: "0123456789abcdef".into(),
                subject: "Tidy the docs".into(),
                author: "Ada".into(),
            }],
            more: 0,
        };
        let notes = default_notes(&changes, "o/example", Some("v0.1.0"), "v0.2.0");
        assert_eq!(
            notes,
            "## Pull requests\n\n- Add C (#3)\n- Add feature A (#1)\n\n\
             ## Other changes\n\n- Tidy the docs (0123456)\n\n\
             **Full changelog**: https://github.com/o/example/compare/v0.1.0...v0.2.0\n"
        );
        assert_eq!(check(&notes, &changes), Check::default());
        assert_eq!(
            default_notes(&Changes::default(), "o/example", None, "v0.1.0"),
            "**Full changelog**: https://github.com/o/example/commits/v0.1.0\n"
        );
        // A public release repo's (bana split): titles and subjects, no #N, sha or link.
        let public = public_notes(&changes);
        assert_eq!(
            public,
            "## Pull requests\n\n- Add C\n- Add feature A\n\n## Other changes\n\n- Tidy the docs\n"
        );
        assert!(
            !public.contains("(#") && !public.contains("0123456") && !public.contains("https://")
        );
        assert_eq!(public_notes(&Changes::default()), "No changes.\n");

        let written = "## Highlights\n\n\
                       - Feature A, at last (#1)\n\
                       - C and more (#3), with #3's fix\n\
                       - Fixes #9, from #3\n\
                       - Not these: &#39; abc#2 #2x #0 #_ PR#7 x-#8\n";
        assert_eq!(
            check(written, &changes),
            Check {
                missing: vec![],
                outside_range: vec![8, 9],
                duplicated: vec![3],
            }
        );
        assert_eq!(
            check("Nothing here", &changes),
            Check {
                missing: vec![1, 3],
                ..Check::default()
            }
        );
    }

    #[test]
    fn versions() {
        use Ordering::*;
        for (a, b, want) in [
            ("v1.2.3", "v1.2.3", Equal),
            ("v1.2.10", "v1.2.9", Greater),
            ("v1.10.0", "v1.9.9", Greater),
            ("v2.0.0", "v10.0.0", Less),
            ("v1.2", "v1.2.0", Equal),
            ("1.2.3", "v1.2.3", Equal),
            ("v1.0.0-rc1", "v1.0.0", Less),
            ("v1.0.0-rc.2", "v1.0.0-rc.10", Less),
            ("v1.0.0-alpha", "v1.0.0-alpha.1", Less),
            ("v1.0.0-1", "v1.0.0-alpha", Less),
            ("v1.0.0-beta", "v1.0.0-alpha", Greater),
            ("v1.0.0+g1", "v1.0.0", Equal),
            ("v0.1.0", "nightly", Greater),
            ("latest", "nightly", Less),
            ("v1..2", "v0.0.1", Less),
        ] {
            assert_eq!(version_cmp(a, b), want, "{a} {b}");
            assert_eq!(version_cmp(b, a), want.reverse(), "{b} {a}");
        }
        assert!(is_prerelease("v0.1.0-rc1") && !is_prerelease("v0.1.0"));
        assert!(!is_prerelease("release-1"), "not a version");
        assert_eq!(
            version("v0.1.0-rc.1+g1"),
            Some(Version {
                numbers: vec![0, 1, 0],
                pre: "rc.1".into()
            })
        );
        assert_eq!(version("v"), None);
        assert_eq!(version("vx.1"), None);
    }

    #[test]
    fn a_tie_goes_to_the_higher_version() {
        let tags: Vec<String> = ["v1.0.0", "v1.0.1", "v0.9.0"].map(String::from).to_vec();
        let list = r#"[{"tagName":"v1.0.0"},{"tagName":"v1.0.1"},{"tagName":"v0.9.0"}]"#;
        let p = previous_release(
            Ok(list),
            &tags,
            "v1.1.0",
            |_| true,
            |t| Some(if t == "v0.9.0" { 9 } else { 4 }),
        );
        assert_eq!(p.tag.as_deref(), Some("v1.0.1"));
        let p = previous_release(Ok(list), &tags, "v1.1.0", |t| t != "v1.0.1", |_| None);
        assert_eq!(p.tag, None, "no distance: not a candidate");
        let p = previous_release(
            Err("\n  offline\nmore"),
            &tags,
            "v1.1.0",
            |_| true,
            |_| Some(1),
        );
        assert_eq!(p.how, "tags (gh failed: offline)");
    }
}

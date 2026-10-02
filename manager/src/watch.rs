//! The watcher's rules: which pushes run, what the queue becomes, and the event
//! a build runs with.
//!
//! Pure, like [`crate::actlog`]. After each fetch the daemon lists its clone's
//! heads (`git for-each-ref --format=` [`REFS_FORMAT`], [`parse_refs`]) and
//! compares them with the heads it saved ([`diff`]). It reads the new heads the
//! rules take ([`to_read`], [`Pushed`]), and [`decide`] says what follows:
//! queue a build, move a queued one to the newer head, drop one, cancel the
//! running one, or note a push that is not built. When a build starts,
//! [`before_for`] and [`event_payload`] give the event act runs it with.
//!
//! A push runs when, in this order:
//! 1. its ref matches `daemon.branches` or `daemon.tags` ([`Rules`]); `*` also
//!    matches `/`, and a `!` pattern leaves refs out ([`matches()`]);
//! 2. its head's message has none of GitHub's skip markers ([`has_skip_marker`]);
//! 3. the workflow is in that commit;
//! 4. no build of that commit at that tier passed or failed ([`Built`]), so the
//!    same commit on a new branch, or pushed again, does not run twice. A tag
//!    always runs: its build is the release, even of a commit built already.
//!
//! The rules come from the daemon's install snapshot, never from the pushed
//! commit. The first start only records the heads: nothing is built.

use crate::actlog::{BuildState, QueuedView};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Each ref's head: `refs/heads/main` or `refs/tags/v1.0` → its commit.
/// state.json keeps the heads last seen (`heads`) and each ref's last green
/// head (`green`) this way.
pub type Heads = BTreeMap<String, String>;

/// What the daemon lists heads with: `git for-each-ref --format=<this>
/// refs/remotes/origin refs/tags` in its clone.
pub const REFS_FORMAT: &str =
    "%(refname) %(objectname) %(objecttype) %(*objectname) %(*objecttype) %(symref)";

/// The heads in `git for-each-ref` output ([`REFS_FORMAT`]). The clone's
/// `refs/remotes/origin/<name>` are GitHub's `refs/heads/<name>`, and an
/// annotated tag's head is the commit it tags. A tag of a tree, or of another
/// tag (git peels one level), is left out, as is origin/HEAD.
pub fn parse_refs(text: &str) -> Heads {
    let mut heads = Heads::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let field = |i: usize| f.get(i).copied().unwrap_or("");
        if !field(5).is_empty() {
            continue;
        }
        let git_ref = match field(0).strip_prefix("refs/remotes/origin/") {
            Some("HEAD") | Some("") => continue,
            Some(b) => format!("refs/heads/{b}"),
            None if field(0).len() > "refs/tags/".len() && is_tag(field(0)) => field(0).to_string(),
            None => continue,
        };
        let sha = match (field(2), field(4)) {
            ("commit", _) => field(1),
            ("tag", "commit") => field(3),
            _ => continue,
        };
        if is_sha(sha) {
            heads.insert(git_ref, sha.to_string());
        }
    }
    heads
}

/// The default branch (`main`), from origin/HEAD's line in the same listing
/// (install runs `git remote set-head origin --auto`).
pub fn default_branch(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let f: Vec<&str> = line.split(' ').collect();
        if f.first() != Some(&"refs/remotes/origin/HEAD") {
            return None;
        }
        f.get(5)?
            .strip_prefix("refs/remotes/origin/")
            .filter(|b| !b.is_empty())
            .map(String::from)
    })
}

/// A commit's full name as git prints it: 40 hex digits (64 in a SHA-256 repository).
fn is_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// `main` for `refs/heads/main`, `v1.0` for `refs/tags/v1.0`: how the page and
/// the menu bar name a ref.
pub fn short_ref(git_ref: &str) -> &str {
    git_ref
        .strip_prefix("refs/heads/")
        .or_else(|| git_ref.strip_prefix("refs/tags/"))
        .unwrap_or(git_ref)
}

pub fn is_tag(git_ref: &str) -> bool {
    git_ref.starts_with("refs/tags/")
}

/// Where the daemon pins a ref's green head in its clone, so gc keeps it:
/// `refs/bana/green/heads/main`.
pub fn green_pin(git_ref: &str) -> String {
    format!(
        "refs/bana/green/{}",
        git_ref.strip_prefix("refs/").unwrap_or(git_ref)
    )
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// A ref that moved since the heads were saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    New {
        git_ref: String,
        sha: String,
    },
    /// Pushed to, or force-pushed: which one is git's to say, when the build
    /// starts (`forced` in [`event_payload`]).
    Moved {
        git_ref: String,
        from: String,
        sha: String,
    },
    Deleted {
        git_ref: String,
        sha: String,
    },
}

impl Change {
    pub fn git_ref(&self) -> &str {
        match self {
            Self::New { git_ref, .. }
            | Self::Moved { git_ref, .. }
            | Self::Deleted { git_ref, .. } => git_ref,
        }
    }

    /// The ref's new head; none when it was deleted.
    pub fn head(&self) -> Option<&str> {
        match self {
            Self::New { sha, .. } | Self::Moved { sha, .. } => Some(sha),
            Self::Deleted { .. } => None,
        }
    }
}

/// What changed from the heads saved (`saved`) to those fetched now, by ref.
/// `saved` is none on the first start: every head is then a baseline, and
/// nothing changed. `now` must come from a listing that worked: an empty one
/// reads as every ref deleted.
pub fn diff(saved: Option<&Heads>, now: &Heads) -> Vec<Change> {
    let Some(saved) = saved else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (r, sha) in now {
        match saved.get(r) {
            None => out.push(Change::New {
                git_ref: r.clone(),
                sha: sha.clone(),
            }),
            Some(from) if from != sha => out.push(Change::Moved {
                git_ref: r.clone(),
                from: from.clone(),
                sha: sha.clone(),
            }),
            Some(_) => {}
        }
    }
    for (r, sha) in saved {
        if !now.contains_key(r) {
            out.push(Change::Deleted {
                git_ref: r.clone(),
                sha: sha.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.git_ref().cmp(b.git_ref()));
    out
}

/// `pattern` matches the whole of `name`. `*` matches any run of characters,
/// `/` too; every other character matches only itself.
pub fn glob(pattern: &str, name: &str) -> bool {
    let (p, n) = (pattern.as_bytes(), name.as_bytes());
    let (mut i, mut j) = (0, 0);
    // After a `*`: where the pattern goes on, and how much of the name it took.
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if p.get(i) == Some(&b'*') {
            star = Some((i + 1, j));
            i += 1;
        } else if p.get(i) == Some(&n[j]) {
            i += 1;
            j += 1;
        } else if let Some((after, took)) = star {
            // The `*` takes one more character, and the rest is tried again.
            star = Some((after, took + 1));
            (i, j) = (after, took + 1);
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == b'*')
}

/// Whether a list of patterns takes `name`: the last pattern that matches it
/// decides, and one starting with `!` leaves it out, as in GitHub's branch
/// filters. A name no pattern matches is out, so an empty list takes nothing.
pub fn matches(patterns: &[String], name: &str) -> bool {
    patterns
        .iter()
        .rev()
        .find_map(|p| match p.strip_prefix('!') {
            Some(p) => glob(p, name).then_some(false),
            None => glob(p, name).then_some(true),
        })
        .unwrap_or(false)
}

/// A list setting's patterns, with spaces or commas between them, as bana's
/// other lists: `* !dependabot/* !renovate/*`.
pub fn patterns(list: &str) -> Vec<String> {
    list.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect()
}

/// What a newer push does to a build of the same ref (`daemon.supersede`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Supersede {
    /// It replaces only the ref's queued build.
    #[default]
    Queued,
    /// It also cancels the ref's running push build, as example's old
    /// `cancel-in-progress` did.
    Running,
}

impl Supersede {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            _ => None,
        }
    }
}

/// Which pushes run, and at which tier: the `daemon.*` keys from the install
/// snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    /// `daemon.branches`: `* !dependabot/* !renovate/*` ([`patterns`]).
    pub branches: Vec<String>,
    /// `daemon.tags`: none by default, so no tag runs.
    pub tags: Vec<String>,
    /// `daemon.tier`: branch pushes run it, under the plain `bana` contexts.
    pub tier: String,
    /// `daemon.tag_tier`: tag pushes run it.
    pub tag_tier: String,
    pub supersede: Supersede,
}

impl Rules {
    /// The tier a push to `git_ref` runs at; none when the rules leave it out.
    pub fn tier_for(&self, git_ref: &str) -> Option<&str> {
        if let Some(b) = git_ref.strip_prefix("refs/heads/") {
            matches(&self.branches, b).then_some(self.tier.as_str())
        } else if let Some(t) = git_ref.strip_prefix("refs/tags/") {
            matches(&self.tags, t).then_some(self.tag_tier.as_str())
        } else {
            None
        }
    }
}

/// GitHub's markers for a push that runs no workflow.
pub const SKIP_MARKERS: [&str; 5] = [
    "[skip ci]",
    "[ci skip]",
    "[no ci]",
    "[skip actions]",
    "[actions skip]",
];

/// A head commit's message says not to run CI, anywhere in it, in any case.
pub fn has_skip_marker(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    SKIP_MARKERS.iter().any(|k| m.contains(k))
}

/// Why a build was asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    #[default]
    Push,
    /// Run now, from the page.
    Manual,
    /// Again, with the same commit, tier and before as an earlier build.
    Rerun,
    /// Again after the daemon was interrupted mid-build (attempt 2).
    Retry,
    /// One job of a fix's round ([`crate::rounds`]): the failing commit, or a
    /// snapshot of the fix's worktree. It posts nothing and counts as built
    /// nowhere.
    Fix,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Manual => "manual",
            Self::Rerun => "rerun",
            Self::Retry => "retry",
            Self::Fix => "fix",
        }
    }
}

/// A build asked for: what it builds, and why. The daemon's build.json can
/// start with it (`#[serde(flatten)]`), beside act's side of the build
/// ([`crate::actlog::Build`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    pub id: u64,
    pub trigger: Trigger,
    /// `refs/heads/main`, `refs/tags/v1.0`.
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub sha: String,
    pub tier: String,
    /// 1, or 2 for a retry.
    pub attempt: u32,
    /// Unix seconds.
    pub queued_at: i64,
    /// What its plan job diffs against ([`before_for`]): set when it starts, or
    /// when a re-run or a retry is queued (the build it runs again had it).
    pub before: Option<String>,
    /// A fix's build (its sha7), and a retry of one: the job it runs
    /// (`bana ci -j`) and its round. A manual build of one job (from the page)
    /// has `job` alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
}

impl Request {
    /// A push build whose commit was built while it waited (from another
    /// ref): it is dropped when its turn comes, as it would not be queued now.
    /// A tag's never is.
    pub fn already_built(&self, built: &Built) -> bool {
        self.trigger == Trigger::Push
            && !is_tag(&self.git_ref)
            && built.contains(&self.sha, &self.tier)
    }

    /// A round's build (or its retry): no statuses, not built, no green.
    pub fn is_fix(&self) -> bool {
        self.fix.is_some()
    }

    /// A manual build of one job (and the jobs it needs), not the whole
    /// workflow: it posts its jobs' statuses but not the build's, moves no
    /// green, counts as built nowhere and is no release's. A round, which runs
    /// one job too, is [`Self::is_fix`] instead.
    pub fn is_partial(&self) -> bool {
        self.job.is_some() && !self.is_fix()
    }

    /// How the page and the menu bar show it while it waits.
    pub fn view(&self, waiting: Option<String>) -> QueuedView {
        QueuedView {
            id: self.id,
            git_ref: short_ref(&self.git_ref).to_string(),
            sha: self.sha.clone(),
            tier: self.tier.clone(),
            trigger: self.trigger.as_str().to_string(),
            job: self.job.clone().filter(|_| self.is_partial()),
            queued_at: self.queued_at,
            waiting,
        }
    }
}

/// The commits already built, at each tier. A build counts once it passed or
/// failed. One that ended in error (cancelled, superseded, timed out,
/// interrupted, could not start) does not, so its commit runs when pushed again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Built(BTreeSet<(String, String)>);

impl Built {
    /// Records a build that ended as `state`; the daemon adds its history.
    pub fn add(&mut self, sha: &str, tier: &str, state: BuildState) {
        if matches!(state, BuildState::Success | BuildState::Failure) {
            self.0.insert((sha.to_string(), tier.to_string()));
        }
    }

    pub fn contains(&self, sha: &str, tier: &str) -> bool {
        self.0.contains(&(sha.to_string(), tier.to_string()))
    }
}

/// A new head as the daemon read it, for the rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pushed {
    pub sha: String,
    /// Its message (`git log -1 --format=%B`).
    pub message: String,
    /// The workflow is in it (`git cat-file -e <sha>:.github/workflows/<workflow>`).
    pub workflow: bool,
}

/// The new heads the rules take: the daemon reads each ([`Pushed`]) for
/// [`decide`].
pub fn to_read(changes: &[Change], rules: &Rules) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in changes {
        if let Some(sha) = c.head() {
            if rules.tier_for(c.git_ref()).is_some() && !out.iter().any(|s| s == sha) {
                out.push(sha.to_string());
            }
        }
    }
    out
}

/// What the daemon does about the refs that changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Queue a push build of the ref's new head, at the back.
    Enqueue {
        git_ref: String,
        sha: String,
        tier: String,
    },
    /// The ref's queued push build builds this newer head instead, in the
    /// same place. Nothing was posted for the old one.
    Replace { id: u64, sha: String },
    /// Remove this queued build: its ref was deleted, or moved to a commit
    /// already built.
    Drop { id: u64, why: String },
    /// Cancel the running build (`daemon.supersede = running`), with this reason.
    SupersedeRunning { id: u64, reason: String },
    /// Not built: its message says so. The history shows it as skipped; nothing
    /// is posted, as on GitHub.
    SkippedMarker { git_ref: String, sha: String },
    /// Not built: the workflow is not in that commit (or git could not read it).
    NoWorkflow { git_ref: String, sha: String },
    /// A deleted ref: its green head and its pin ([`green_pin`]) go.
    Forget { git_ref: String },
}

/// What follows from the refs that changed, in their order. `pushed` has what
/// the daemon read of the new heads ([`to_read`]); a head it could not read
/// counts as having no workflow. `queue` is the builds waiting, and `running`
/// the build act runs now.
///
/// Only push builds are ever replaced or dropped: a manual build, a re-run or
/// a retry was asked for as it is. A branch keeps at most one queued push
/// build, and a newer head replaces its commit. A tag's builds are never
/// replaced: each push of a tag is built, even of a commit built already at
/// that tier, since that build is the tag's release. A deleted ref drops its
/// queued push builds.
pub fn decide(
    changes: &[Change],
    rules: &Rules,
    pushed: &[Pushed],
    built: &Built,
    queue: &[Request],
    running: Option<&Request>,
) -> Vec<Action> {
    let mut out = Vec::new();
    for c in changes {
        let git_ref = c.git_ref();
        let queued = queue
            .iter()
            .filter(|q| q.trigger == Trigger::Push && q.git_ref == git_ref);
        let Some(sha) = c.head() else {
            for q in queued {
                out.push(Action::Drop {
                    id: q.id,
                    why: format!("{} was deleted", short_ref(git_ref)),
                });
            }
            out.push(Action::Forget {
                git_ref: git_ref.to_string(),
            });
            continue;
        };
        let Some(tier) = rules.tier_for(git_ref) else {
            continue;
        };
        let (git_ref, sha, tier) = (git_ref.to_string(), sha.to_string(), tier.to_string());
        let head = pushed.iter().find(|p| p.sha == sha);
        if head.is_some_and(|h| has_skip_marker(&h.message)) {
            out.push(Action::SkippedMarker { git_ref, sha });
            continue;
        }
        if !head.is_some_and(|h| h.workflow) {
            out.push(Action::NoWorkflow { git_ref, sha });
            continue;
        }
        let mut queued = queued.filter(|q| q.tier == tier);
        if queued.clone().any(|q| q.sha == sha) {
            continue;
        }
        let replaced = if is_tag(&git_ref) {
            None
        } else {
            queued.next()
        };
        if !is_tag(&git_ref) && built.contains(&sha, &tier) {
            if let Some(q) = replaced {
                out.push(Action::Drop {
                    id: q.id,
                    why: format!(
                        "{} moved to {}, built already",
                        short_ref(&git_ref),
                        short_sha(&sha)
                    ),
                });
            }
            continue;
        }
        let supersede = running.filter(|r| {
            rules.supersede == Supersede::Running
                && !is_tag(&git_ref)
                && r.trigger == Trigger::Push
                && r.git_ref == git_ref
                && r.tier == tier
                && r.sha != sha
        });
        if let Some(r) = supersede {
            out.push(Action::SupersedeRunning {
                id: r.id,
                reason: format!("superseded by {}", short_sha(&sha)),
            });
        }
        out.push(match replaced {
            Some(q) => Action::Replace { id: q.id, sha },
            None => Action::Enqueue { git_ref, sha, tier },
        });
    }
    out
}

/// A commit of zeros, as long as `sha`: GitHub's "no commit" in an event.
pub fn zeros(sha: &str) -> String {
    "0".repeat(if sha.len() == 64 { 64 } else { 40 })
}

pub fn is_zeros(sha: &str) -> bool {
    !sha.is_empty() && sha.bytes().all(|c| c == b'0')
}

/// What a build's plan job diffs against (the event's `before`): the ref's last
/// green head, so the changes of pushes that failed, were replaced while
/// queued, or were cancelled stay in the diff. Zeros when the ref has none, or
/// when it is the commit itself (a manual run of a green head); `bana changed`
/// then compares with the default branch. A re-run or a retry keeps the before
/// its build had.
pub fn before_for(r: &Request, green: &Heads) -> String {
    if let Some(b) = &r.before {
        return b.clone();
    }
    match green.get(&r.git_ref) {
        Some(g) if *g != r.sha => g.clone(),
        _ => zeros(&r.sha),
    }
}

/// What the daemon reads of a commit for its event: `git log -1
/// --format=<this> <sha>`.
pub const COMMIT_FORMAT: &str = "%H%n%cI%n%an%n%ae%n%cn%n%ce%n%B";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub email: String,
}

/// A commit, as the event's `head_commit` has it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub message: String,
    /// The committer's date, ISO 8601: `2026-09-28T15:00:54+02:00`.
    pub timestamp: String,
    pub author: Person,
    pub committer: Person,
}

/// `git log -1 --format=` [`COMMIT_FORMAT`].
pub fn parse_commit(text: &str) -> Option<Commit> {
    let mut f = text.splitn(7, '\n');
    let mut next = || f.next().map(|s| s.trim_end_matches('\r'));
    let sha = next()?;
    if !is_sha(sha) {
        return None;
    }
    let sha = sha.to_string();
    let timestamp = next()?.to_string();
    let mut person = || -> Option<Person> {
        Some(Person {
            name: next()?.to_string(),
            email: next()?.to_string(),
        })
    };
    let (author, committer) = (person()?, person()?);
    Some(Commit {
        sha,
        timestamp,
        author,
        committer,
        message: f.next().unwrap_or("").trim_end().to_string(),
    })
}

/// What every event says about the project: the daemon's settings, and the
/// default branch of its clone ([`default_branch`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Project {
    /// `owner/repo`.
    pub repo: String,
    pub default_branch: String,
    /// The GitHub user the daemon runs for (gh's): the event's sender.
    pub login: String,
    /// The workflow_dispatch input that takes the tier (bana.conf's `tier_input`).
    pub tier_input: String,
}

/// `builds/<id>/event.json`: a push's payload, which act runs as
/// workflow_dispatch (`act workflow_dispatch -e`), so the workflow sees
/// `github.ref`, `github.sha`, `github.event.before` and `inputs.<tier_input>`
/// as it would for a push on GitHub.
/// - `deleted: false` makes act take `github.sha` from `after`;
/// - `created` when `before` is zeros, as on GitHub;
/// - `forced` when `before` is not an ancestor of the commit (git says, when
///   the build starts); never without a `before`;
/// - the tier goes only in `inputs`, as act ignores `--input` once it has an
///   event, and it is always there: a workflow's own default may be another.
pub fn event_payload(p: &Project, r: &Request, before: &str, forced: bool, head: &Commit) -> Value {
    let created = is_zeros(before);
    let short = |s: &str| s.get(..12).unwrap_or(s).to_string();
    let compare = if created {
        format!("https://github.com/{}/commit/{}", p.repo, r.sha)
    } else {
        format!(
            "https://github.com/{}/compare/{}...{}",
            p.repo,
            short(before),
            short(&r.sha)
        )
    };
    let (owner, name) = p.repo.split_once('/').unwrap_or(("", &p.repo));
    let mut inputs = Map::new();
    if !p.tier_input.is_empty() && !r.tier.is_empty() {
        inputs.insert(p.tier_input.clone(), Value::from(r.tier.clone()));
    }
    let person = |who: &Person| json!({"name": who.name, "email": who.email});
    json!({
        "ref": r.git_ref,
        "before": before,
        "after": r.sha,
        "created": created,
        "deleted": false,
        "forced": forced && !created,
        "compare": compare,
        "head_commit": {
            "id": head.sha,
            "message": head.message,
            "timestamp": head.timestamp,
            "author": person(&head.author),
            "committer": person(&head.committer),
        },
        "repository": {
            "full_name": p.repo,
            "name": name,
            "owner": {"login": owner},
            "default_branch": p.default_branch,
            "private": true,
        },
        "sender": {"login": p.login},
        "inputs": inputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const D: &str = "dddddddddddddddddddddddddddddddddddddddd";
    const E: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    /// `main` is `refs/heads/main`, `tags/v1` is `refs/tags/v1`.
    fn full(r: &str) -> String {
        match r.strip_prefix("tags/") {
            Some(t) => format!("refs/tags/{t}"),
            None => format!("refs/heads/{r}"),
        }
    }

    fn heads(list: &[(&str, &str)]) -> Heads {
        list.iter().map(|(r, s)| (full(r), s.to_string())).collect()
    }

    fn rules() -> Rules {
        Rules {
            branches: patterns("* !dependabot/* !renovate/*"),
            tags: patterns("v*"),
            tier: "quick".into(),
            tag_tier: "release".into(),
            supersede: Supersede::Queued,
        }
    }

    fn request(id: u64, trigger: Trigger, r: &str, sha: &str, tier: &str) -> Request {
        Request {
            id,
            trigger,
            git_ref: full(r),
            sha: sha.into(),
            tier: tier.into(),
            attempt: 1,
            ..Request::default()
        }
    }

    fn enqueue(r: &str, sha: &str, tier: &str) -> Action {
        Action::Enqueue {
            git_ref: full(r),
            sha: sha.into(),
            tier: tier.into(),
        }
    }

    /// The daemon's side, as far as the rules go: polls apply [`decide`]'s
    /// actions to its queue; `finish` ends the running build.
    struct Daemon {
        rules: Rules,
        saved: Option<Heads>,
        queue: Vec<Request>,
        running: Option<Request>,
        built: Built,
        /// Commits whose message has a skip marker, and those without the workflow.
        marked: Vec<&'static str>,
        no_workflow: Vec<&'static str>,
        cancelled: Option<String>,
        next_id: u64,
    }

    impl Daemon {
        fn new(rules: Rules) -> Self {
            Self {
                rules,
                saved: None,
                queue: Vec::new(),
                running: None,
                built: Built::default(),
                marked: Vec::new(),
                no_workflow: Vec::new(),
                cancelled: None,
                next_id: 1,
            }
        }

        fn poll(&mut self, now: &[(&str, &str)]) -> Vec<Action> {
            let now = heads(now);
            let changes = diff(self.saved.as_ref(), &now);
            let pushed: Vec<Pushed> = to_read(&changes, &self.rules)
                .into_iter()
                .map(|sha| Pushed {
                    message: if self.marked.iter().any(|m| *m == sha) {
                        "wip\n\n[skip ci]".into()
                    } else {
                        "fix".into()
                    },
                    workflow: !self.no_workflow.iter().any(|m| *m == sha),
                    sha,
                })
                .collect();
            let actions = decide(
                &changes,
                &self.rules,
                &pushed,
                &self.built,
                &self.queue,
                self.running.as_ref(),
            );
            for a in &actions {
                match a {
                    Action::Enqueue { git_ref, sha, tier } => {
                        self.queue.push(Request {
                            id: self.next_id,
                            git_ref: git_ref.clone(),
                            sha: sha.clone(),
                            tier: tier.clone(),
                            attempt: 1,
                            ..Request::default()
                        });
                        self.next_id += 1;
                    }
                    Action::Replace { id, sha } => {
                        let q = self.queue.iter_mut().find(|q| q.id == *id).unwrap();
                        q.sha = sha.clone();
                    }
                    Action::Drop { id, .. } => self.queue.retain(|q| q.id != *id),
                    Action::SupersedeRunning { id, reason } => {
                        assert_eq!(self.running.as_ref().map(|r| r.id), Some(*id));
                        self.cancelled = Some(reason.clone());
                    }
                    _ => {}
                }
            }
            self.saved = Some(now);
            actions
        }

        /// The next queued build starts, unless its commit was built meanwhile.
        fn start(&mut self) -> Option<u64> {
            while !self.queue.is_empty() {
                let q = self.queue.remove(0);
                if !q.already_built(&self.built) {
                    let id = q.id;
                    self.running = Some(q);
                    self.cancelled = None;
                    return Some(id);
                }
            }
            None
        }

        fn finish(&mut self, state: BuildState) {
            let r = self.running.take().unwrap();
            self.built.add(&r.sha, &r.tier, state);
        }

        /// Each queued build: id, short ref, commit.
        fn queued(&self) -> Vec<(u64, &str, &str)> {
            self.queue
                .iter()
                .map(|q| (q.id, short_ref(&q.git_ref), q.sha.as_str()))
                .collect()
        }
    }

    #[test]
    fn globs() {
        for (pattern, name, want) in [
            ("*", "main", true),
            ("*", "feature/x", true),
            ("*", "a/b/c", true),
            ("*", "", true),
            ("main", "main", true),
            ("main", "main2", false),
            ("main", "mai", false),
            (
                "dependabot/*",
                "dependabot/npm_and_yarn/web/vite-5.4.1",
                true,
            ),
            ("dependabot/*", "dependabot", false),
            ("dependabot/*", "my-dependabot/x", false),
            ("v*", "v1.2.0", true),
            ("v*", "release-v1", false),
            ("*-wip", "feature/x-wip", true),
            ("*-wip", "feature/x-wip2", false),
            ("feat/*/done", "feat/a/b/done", true),
            ("feat/*/done", "feat/done", false),
            ("a*b*c", "axxbyybzzc", true),
            ("a*b*c", "axxbyybzz", false),
            ("**", "x/y", true),
            ("*x*", "", false),
            ("", "", true),
            ("", "a", false),
            ("ab", "a", false),
            ("?", "a", false),
            ("[ab]", "a", false),
            ("fix/ü*", "fix/über", true),
        ] {
            assert_eq!(glob(pattern, name), want, "{pattern} {name}");
        }
    }

    #[test]
    fn which_refs_the_patterns_take() {
        let default = patterns("* !dependabot/* !renovate/*");
        assert_eq!(default, ["*", "!dependabot/*", "!renovate/*"]);
        assert_eq!(patterns(" a,b ,, c\t"), ["a", "b", "c"]);
        for (list, name, want) in [
            ("* !dependabot/* !renovate/*", "main", true),
            ("* !dependabot/* !renovate/*", "feature/login", true),
            (
                "* !dependabot/* !renovate/*",
                "dependabot/cargo/serde-1.0.210",
                false,
            ),
            ("* !dependabot/* !renovate/*", "renovate/tokio-1.x", false),
            ("* !dependabot/* !renovate/*", "dependabot", true),
            ("main release/*", "release/1.2", true),
            ("main release/*", "feature/x", false),
            ("* !release/* release/keep", "release/keep", true),
            ("* !release/* release/keep", "release/other", false),
            ("!wip/*", "main", false),
            ("", "main", false),
        ] {
            assert_eq!(matches(&patterns(list), name), want, "{list}: {name}");
        }

        let mut r = rules();
        assert_eq!(r.tier_for("refs/heads/main"), Some("quick"));
        assert_eq!(r.tier_for("refs/heads/dependabot/npm/x"), None);
        assert_eq!(r.tier_for("refs/tags/v1.0"), Some("release"));
        assert_eq!(r.tier_for("refs/tags/nightly-3"), None);
        assert_eq!(
            r.tier_for("refs/heads/v2"),
            Some("quick"),
            "a branch never goes by the tag patterns"
        );
        assert_eq!(r.tier_for("refs/pull/7/head"), None);
        r.tags = patterns("");
        assert_eq!(r.tier_for("refs/tags/v1.0"), None, "no tags by default");
        r.branches = Vec::new();
        assert_eq!(r.tier_for("refs/heads/main"), None);
        assert_eq!(Supersede::parse("running"), Some(Supersede::Running));
        assert_eq!(Supersede::parse("queued"), Some(Supersede::Queued));
        assert_eq!(Supersede::parse("yes"), None);
    }

    #[test]
    fn skip_markers() {
        for m in SKIP_MARKERS {
            assert!(has_skip_marker(&format!("docs: typo {m}")), "{m}");
            assert!(
                has_skip_marker(&format!("docs\n\nbody\n{m}\n")),
                "{m} in the body"
            );
            assert!(has_skip_marker(&m.to_uppercase()), "{m} in capitals");
        }
        for m in [
            "fix the ci skip logic",
            "[skip]",
            "[skip-ci]",
            "[ci  skip]",
            "skip ci",
            "(skip ci)",
            "",
        ] {
            assert!(!has_skip_marker(m), "{m}");
        }
    }

    /// Real `git for-each-ref` output (git 2.43) from a clone with origin/HEAD
    /// set: an annotated, a lightweight, a nested and a tree tag; plus lines no
    /// listing has.
    const REFS: &str = "\
refs/remotes/origin/HEAD 430770d10a9af81a6d5e766905005710a3cd496a commit   refs/remotes/origin/main
refs/remotes/origin/dependabot/npm/x 430770d10a9af81a6d5e766905005710a3cd496a commit
refs/remotes/origin/feature/x 964e88fad9158baa0c5054f3144be80a86ba571b commit
refs/remotes/origin/main 430770d10a9af81a6d5e766905005710a3cd496a commit
refs/tags/ann 964e88fad9158baa0c5054f3144be80a86ba571b tag 430770d10a9af81a6d5e766905005710a3cd496a commit
refs/tags/light 430770d10a9af81a6d5e766905005710a3cd496a commit
refs/tags/nested e8e5cc7bdb94db050ceda841d1e3263e9c671b86 tag 964e88fad9158baa0c5054f3144be80a86ba571b tag
refs/tags/tree-tag 4b825dc642cb6eb9a060e54bf8d69288fbee4904 tree
refs/bana/green/heads/main 430770d10a9af81a6d5e766905005710a3cd496a commit
refs/tags/short 430770d commit
refs/tags/ 430770d10a9af81a6d5e766905005710a3cd496a commit
not a ref line
";

    #[test]
    fn heads_from_the_clone() {
        let (m, f) = (
            "430770d10a9af81a6d5e766905005710a3cd496a",
            "964e88fad9158baa0c5054f3144be80a86ba571b",
        );
        let want: Heads = [
            ("refs/heads/dependabot/npm/x", m),
            ("refs/heads/feature/x", f),
            ("refs/heads/main", m),
            ("refs/tags/ann", m),
            ("refs/tags/light", m),
        ]
        .into_iter()
        .map(|(r, s)| (r.to_string(), s.to_string()))
        .collect();
        assert_eq!(
            parse_refs(REFS),
            want,
            "an annotated tag's head is its commit"
        );
        assert_eq!(default_branch(REFS).as_deref(), Some("main"));
        assert_eq!(
            default_branch("refs/remotes/origin/main x commit   \n"),
            None
        );
        assert!(parse_refs("").is_empty());
        let sha256 = "a".repeat(64);
        assert_eq!(
            parse_refs(&format!("refs/remotes/origin/main {sha256} commit   \n")).len(),
            1,
            "a SHA-256 repository"
        );
        assert_eq!(
            serde_json::to_value(parse_refs(REFS)).unwrap()["refs/heads/main"],
            m,
            "state.json's heads: an object"
        );
        assert_eq!(short_ref("refs/heads/feature/x"), "feature/x");
        assert_eq!(short_ref("refs/tags/v1.0"), "v1.0");
        assert!(is_tag("refs/tags/v1.0") && !is_tag("refs/heads/tags/x"));
        assert_eq!(
            green_pin("refs/heads/feature/x"),
            "refs/bana/green/heads/feature/x"
        );
        assert_eq!(green_pin("refs/tags/v1"), "refs/bana/green/tags/v1");
    }

    #[test]
    fn new_moved_and_deleted_refs() {
        let saved = heads(&[("main", A), ("old", B), ("tags/v1", C), ("same", D)]);
        let now = heads(&[("main", B), ("new", E), ("tags/v1", C), ("same", D)]);
        assert_eq!(
            diff(Some(&saved), &now),
            [
                Change::Moved {
                    git_ref: full("main"),
                    from: A.into(),
                    sha: B.into()
                },
                Change::New {
                    git_ref: full("new"),
                    sha: E.into()
                },
                Change::Deleted {
                    git_ref: full("old"),
                    sha: B.into()
                },
            ]
        );
        assert!(diff(Some(&now), &now).is_empty());
        assert!(
            diff(None, &now).is_empty(),
            "the first start: every head is a baseline"
        );
        let changes = diff(Some(&saved), &now);
        assert_eq!(changes[0].head(), Some(B));
        assert_eq!(changes[2].head(), None);
        assert_eq!(to_read(&changes, &rules()), [B, E]);
        let changes = diff(
            Some(&Heads::new()),
            &heads(&[("a", A), ("b", A), ("dependabot/x", B), ("tags/nightly", C)]),
        );
        assert_eq!(
            to_read(&changes, &rules()),
            [A],
            "each commit once, and only the refs the rules take"
        );
    }

    #[test]
    fn which_pushes_run() {
        struct Case {
            name: &'static str,
            rules: Rules,
            change: Change,
            pushed: Vec<Pushed>,
            built: Built,
            want: Vec<Action>,
        }
        let read = |sha: &str, message: &str, workflow: bool| Pushed {
            sha: sha.into(),
            message: message.into(),
            workflow,
        };
        let new = |r: &str, sha: &str| Change::New {
            git_ref: full(r),
            sha: sha.into(),
        };
        let mut built_a = Built::default();
        built_a.add(A, "quick", BuildState::Failure);
        let mut errored_a = Built::default();
        errored_a.add(A, "quick", BuildState::Error);
        let mut released_a = Built::default();
        released_a.add(A, "release", BuildState::Success);
        let cases = vec![
            Case {
                name: "a new branch",
                rules: rules(),
                change: new("feature/x", A),
                pushed: vec![read(A, "add x", true)],
                built: Built::default(),
                want: vec![enqueue("feature/x", A, "quick")],
            },
            Case {
                name: "a push to main",
                rules: rules(),
                change: Change::Moved {
                    git_ref: full("main"),
                    from: B.into(),
                    sha: A.into(),
                },
                pushed: vec![read(A, "fix", true)],
                built: Built::default(),
                want: vec![enqueue("main", A, "quick")],
            },
            Case {
                name: "a bot's branch",
                rules: rules(),
                change: new("dependabot/cargo/serde-1.0.210", A),
                pushed: vec![],
                built: Built::default(),
                want: vec![],
            },
            Case {
                name: "a tag, with tags off",
                rules: Rules {
                    tags: vec![],
                    ..rules()
                },
                change: new("tags/v1.0", A),
                pushed: vec![read(A, "1.0", true)],
                built: Built::default(),
                want: vec![],
            },
            Case {
                name: "a tag the rules take runs the tag tier",
                rules: rules(),
                change: new("tags/v1.0", A),
                pushed: vec![read(A, "1.0", true)],
                built: Built::default(),
                want: vec![enqueue("tags/v1.0", A, "release")],
            },
            Case {
                name: "a skip marker",
                rules: rules(),
                change: new("main", A),
                pushed: vec![read(A, "docs: typo [skip ci]", true)],
                built: Built::default(),
                want: vec![Action::SkippedMarker {
                    git_ref: full("main"),
                    sha: A.into(),
                }],
            },
            Case {
                name: "a skip marker, even without the workflow",
                rules: rules(),
                change: new("main", A),
                pushed: vec![read(A, "[no ci] drop ci", false)],
                built: Built::default(),
                want: vec![Action::SkippedMarker {
                    git_ref: full("main"),
                    sha: A.into(),
                }],
            },
            Case {
                name: "no workflow in the commit",
                rules: rules(),
                change: new("gh-pages", A),
                pushed: vec![read(A, "site", false)],
                built: Built::default(),
                want: vec![Action::NoWorkflow {
                    git_ref: full("gh-pages"),
                    sha: A.into(),
                }],
            },
            Case {
                name: "a commit git could not read",
                rules: rules(),
                change: new("main", A),
                pushed: vec![read(B, "other", true)],
                built: Built::default(),
                want: vec![Action::NoWorkflow {
                    git_ref: full("main"),
                    sha: A.into(),
                }],
            },
            Case {
                name: "a commit built already, on another branch",
                rules: rules(),
                change: new("feature/copy", A),
                pushed: vec![read(A, "fix", true)],
                built: built_a.clone(),
                want: vec![],
            },
            Case {
                name: "a commit built at the push tier, tagged",
                rules: rules(),
                change: new("tags/v1.0", A),
                pushed: vec![read(A, "fix", true)],
                built: built_a,
                want: vec![enqueue("tags/v1.0", A, "release")],
            },
            Case {
                name: "a tag of a commit built at the tag tier, from main",
                rules: rules(),
                change: new("tags/v1.0", A),
                pushed: vec![read(A, "fix", true)],
                built: released_a.clone(),
                want: vec![enqueue("tags/v1.0", A, "release")],
            },
            Case {
                name: "a branch at that commit, pushed at the tag tier",
                rules: Rules {
                    tier: "release".into(),
                    ..rules()
                },
                change: new("release/1", A),
                pushed: vec![read(A, "fix", true)],
                built: released_a,
                want: vec![],
            },
            Case {
                name: "a commit whose build ended in error runs again",
                rules: rules(),
                change: new("feature/copy", A),
                pushed: vec![read(A, "fix", true)],
                built: errored_a,
                want: vec![enqueue("feature/copy", A, "quick")],
            },
            Case {
                name: "a deleted ref forgets its green head",
                rules: rules(),
                change: Change::Deleted {
                    git_ref: full("dependabot/x"),
                    sha: A.into(),
                },
                pushed: vec![],
                built: Built::default(),
                want: vec![Action::Forget {
                    git_ref: full("dependabot/x"),
                }],
            },
        ];
        for c in cases {
            let got = decide(&[c.change], &c.rules, &c.pushed, &c.built, &[], None);
            assert_eq!(got, c.want, "{}", c.name);
        }
    }

    #[test]
    fn the_first_start_builds_nothing() {
        let mut d = Daemon::new(rules());
        let actions = d.poll(&[("main", A), ("feature/x", B), ("tags/v1.0", C)]);
        assert!(actions.is_empty() && d.queue.is_empty());
        assert_eq!(
            d.poll(&[("main", A), ("feature/x", B), ("tags/v1.0", C)]),
            []
        );
        d.poll(&[("main", D), ("feature/x", B), ("tags/v1.0", C)]);
        assert_eq!(d.queued(), [(1, "main", D)], "then pushes run");
    }

    #[test]
    fn a_ref_keeps_one_queued_build() {
        let mut d = Daemon::new(rules());
        d.poll(&[("main", A), ("dev", A)]);
        d.poll(&[("main", B), ("dev", A)]);
        assert_eq!(d.start(), Some(1));
        // Pushes while main's build runs: one queued build, on the newest head.
        d.poll(&[("main", C), ("dev", A)]);
        let actions = d.poll(&[("main", D), ("dev", E)]);
        assert_eq!(
            actions,
            [
                enqueue("dev", E, "quick"),
                Action::Replace {
                    id: 2,
                    sha: D.into()
                },
            ]
        );
        d.poll(&[("main", E), ("dev", E)]);
        assert_eq!(
            d.queued(),
            [(2, "main", E), (3, "dev", E)],
            "a replaced build keeps its place"
        );
        assert_eq!(d.cancelled, None, "the running build runs on");

        // A force-push back to a commit already built: nothing is left to build.
        d.finish(BuildState::Success);
        assert_eq!(d.start(), Some(2));
        d.finish(BuildState::Success);
        d.built.add(A, "quick", BuildState::Success);
        d.poll(&[("main", E), ("dev", C)]);
        assert_eq!(d.queued(), [(3, "dev", C)]);
        let actions = d.poll(&[("main", E), ("dev", A)]);
        assert_eq!(
            actions,
            [Action::Drop {
                id: 3,
                why: "dev moved to aaaaaaa, built already".into()
            }]
        );
        assert!(d.queue.is_empty());

        // A skip marker leaves the queued build as it was, as on GitHub.
        d.poll(&[("main", E), ("dev", D)]);
        d.marked.push(C);
        let actions = d.poll(&[("main", E), ("dev", C)]);
        assert_eq!(
            actions,
            [Action::SkippedMarker {
                git_ref: full("dev"),
                sha: C.into()
            }]
        );
        assert_eq!(d.queued(), [(4, "dev", D)]);
    }

    #[test]
    fn a_tag_is_never_replaced() {
        let mut d = Daemon::new(rules());
        d.poll(&[("main", A)]);
        d.poll(&[("main", A), ("tags/v1.0", B), ("tags/v1.1", B)]);
        let actions = d.poll(&[("main", A), ("tags/v1.0", C), ("tags/v1.1", B)]);
        assert_eq!(actions, [enqueue("tags/v1.0", C, "release")]);
        assert_eq!(
            d.queued(),
            [(1, "v1.0", B), (2, "v1.1", B), (3, "v1.0", C)],
            "a moved tag is built again; the same commit under two tags waits twice"
        );
        let actions = d.poll(&[("main", A), ("tags/v1.1", B)]);
        assert_eq!(
            actions,
            [
                Action::Drop {
                    id: 1,
                    why: "v1.0 was deleted".into()
                },
                Action::Drop {
                    id: 3,
                    why: "v1.0 was deleted".into()
                },
                Action::Forget {
                    git_ref: full("tags/v1.0")
                },
            ]
        );
        assert_eq!(d.start(), Some(2));
        d.finish(BuildState::Failure);
        assert_eq!(
            d.poll(&[("main", A), ("tags/v1.1", B), ("tags/v1.2", B)]),
            [enqueue("tags/v1.2", B, "release")],
            "a new tag of a commit built already is built: it is a release"
        );
        assert_eq!(
            d.poll(&[("main", A), ("tags/v1.1", B), ("tags/v1.2", B)]),
            [],
            "an unchanged tag"
        );
        assert_eq!(d.start(), Some(4), "not dropped as built while it waited");
    }

    #[test]
    fn a_tag_of_a_built_commit_runs() {
        let mut d = Daemon::new(rules());
        d.poll(&[("main", A)]);
        d.poll(&[("main", B)]);
        assert_eq!(d.start(), Some(1));
        d.finish(BuildState::Success);
        // main's head, built at release too (Run now, or a nightly).
        d.built.add(B, "release", BuildState::Success);
        assert_eq!(
            d.poll(&[("main", B), ("tags/v0.1.0", B)]),
            [enqueue("tags/v0.1.0", B, "release")]
        );
        assert_eq!(d.poll(&[("main", B), ("tags/v0.1.0", B)]), []);
        assert!(!d.queue[0].already_built(&d.built));
        assert_eq!(d.start(), Some(2));
        d.finish(BuildState::Success);
        assert_eq!(
            d.poll(&[("main", B), ("tags/v0.1.0", B), ("hotfix", B)]),
            [],
            "a branch at the built commit is still skipped"
        );
    }

    #[test]
    fn the_same_commit_runs_once() {
        let mut d = Daemon::new(rules());
        d.poll(&[("main", A)]);
        // Pushed to two branches at once: both wait, and the second finds it built.
        d.poll(&[("main", B), ("release/1", B)]);
        assert_eq!(d.queued(), [(1, "main", B), (2, "release/1", B)]);
        assert_eq!(d.start(), Some(1));
        d.finish(BuildState::Failure);
        assert_eq!(d.start(), None, "built while it waited");
        assert_eq!(
            d.poll(&[("main", B), ("release/1", B), ("hotfix", B)]),
            [],
            "a new branch at a built commit"
        );

        // The first ended in error: the second gives the commit its result.
        d.poll(&[("main", C), ("release/1", B), ("hotfix", B), ("x", C)]);
        assert_eq!(d.queued(), [(3, "main", C), (4, "x", C)]);
        assert_eq!(d.start(), Some(3));
        d.finish(BuildState::Error);
        assert_eq!(d.start(), Some(4));

        // Queued twice by a replay (the daemon died before saving its heads).
        let q = vec![request(9, Trigger::Push, "main", D, "quick")];
        let change = Change::New {
            git_ref: full("main"),
            sha: D.into(),
        };
        let read = Pushed {
            sha: D.into(),
            workflow: true,
            ..Pushed::default()
        };
        let again = decide(&[change], &rules(), &[read], &Built::default(), &q, None);
        assert_eq!(again, []);

        // Only push builds are dropped this way: a manual run or a re-run was asked for.
        let mut built = Built::default();
        built.add(D, "quick", BuildState::Success);
        assert!(q[0].already_built(&built));
        for t in [
            Trigger::Manual,
            Trigger::Rerun,
            Trigger::Retry,
            Trigger::Fix,
        ] {
            assert!(!request(9, t, "main", D, "quick").already_built(&built));
        }
        assert!(!request(9, Trigger::Push, "main", D, "nightly").already_built(&built));
        assert!(
            !request(9, Trigger::Push, "tags/v1", D, "quick").already_built(&built),
            "a queued tag push"
        );
    }

    #[test]
    fn only_push_builds_are_replaced_or_dropped() {
        let queue = vec![
            request(1, Trigger::Manual, "main", A, "nightly"),
            request(2, Trigger::Retry, "main", A, "quick"),
            request(3, Trigger::Rerun, "main", A, "quick"),
            request(4, Trigger::Manual, "old", A, "quick"),
            request(5, Trigger::Fix, "main", A, "quick"),
            request(6, Trigger::Fix, "old", A, "quick"),
        ];
        let changes = [
            Change::Moved {
                git_ref: full("main"),
                from: A.into(),
                sha: B.into(),
            },
            Change::Deleted {
                git_ref: full("old"),
                sha: A.into(),
            },
        ];
        let read = Pushed {
            sha: B.into(),
            message: "fix".into(),
            workflow: true,
        };
        assert_eq!(
            decide(&changes, &rules(), &[read], &Built::default(), &queue, None),
            [
                enqueue("main", B, "quick"),
                Action::Forget {
                    git_ref: full("old")
                }
            ]
        );
    }

    #[test]
    fn a_newer_push_may_cancel_the_running_build() {
        let running_rules = Rules {
            supersede: Supersede::Running,
            ..rules()
        };
        let cancel = |id: u64| Action::SupersedeRunning {
            id,
            reason: "superseded by ccccccc".into(),
        };
        // Name, rules, the running build, what follows main's (or v1's) push of C.
        let rows: Vec<(&str, Rules, Request, &str, Vec<Action>)> = vec![
            (
                "supersede = queued: it runs on",
                rules(),
                request(5, Trigger::Push, "main", B, "quick"),
                "main",
                vec![enqueue("main", C, "quick")],
            ),
            (
                "supersede = running: a push build of the ref is cancelled",
                running_rules.clone(),
                request(5, Trigger::Push, "main", B, "quick"),
                "main",
                vec![cancel(5), enqueue("main", C, "quick")],
            ),
            (
                "another ref's build",
                running_rules.clone(),
                request(5, Trigger::Push, "dev", B, "quick"),
                "main",
                vec![enqueue("main", C, "quick")],
            ),
            (
                "a manual build",
                running_rules.clone(),
                request(5, Trigger::Manual, "main", B, "quick"),
                "main",
                vec![enqueue("main", C, "quick")],
            ),
            (
                "a fix's round, never superseded",
                running_rules.clone(),
                request(5, Trigger::Fix, "main", B, "quick"),
                "main",
                vec![enqueue("main", C, "quick")],
            ),
            (
                "a build at another tier",
                running_rules.clone(),
                request(5, Trigger::Push, "main", B, "nightly"),
                "main",
                vec![enqueue("main", C, "quick")],
            ),
            (
                "a tag's build",
                running_rules.clone(),
                request(5, Trigger::Push, "tags/v1", B, "release"),
                "tags/v1",
                vec![enqueue("tags/v1", C, "release")],
            ),
        ];
        for (name, rules, running, r, want) in rows {
            let change = Change::Moved {
                git_ref: full(r),
                from: B.into(),
                sha: C.into(),
            };
            let read = Pushed {
                sha: C.into(),
                message: "fix".into(),
                workflow: true,
            };
            let got = decide(
                &[change],
                &rules,
                &[read],
                &Built::default(),
                &[],
                Some(&running),
            );
            assert_eq!(got, want, "{name}");
        }

        // With a build queued too, both give way.
        let mut d = Daemon::new(running_rules);
        d.poll(&[("main", A)]);
        d.poll(&[("main", B)]);
        d.start();
        d.poll(&[("main", C)]);
        assert_eq!(d.cancelled.as_deref(), Some("superseded by ccccccc"));
        let actions = d.poll(&[("main", D)]);
        assert_eq!(
            actions,
            [
                Action::SupersedeRunning {
                    id: 1,
                    reason: "superseded by ddddddd".into()
                },
                Action::Replace {
                    id: 2,
                    sha: D.into()
                },
            ]
        );
        // A push that is not built cancels nothing.
        d.cancelled = None;
        d.marked.push(E);
        d.poll(&[("main", E)]);
        d.no_workflow.push(A);
        d.poll(&[("main", A)]);
        assert_eq!(d.cancelled, None);
        assert_eq!(d.queued(), [(2, "main", D)]);
    }

    #[test]
    fn before_is_the_last_green_head() {
        let green = heads(&[("main", A), ("dev", B)]);
        let push = |r: &str, sha: &str| request(1, Trigger::Push, r, sha, "quick");
        let zeros40 = "0".repeat(40);
        assert_eq!(before_for(&push("main", C), &green), A);
        assert_eq!(
            before_for(&push("feature/new", C), &green),
            zeros40,
            "a new ref"
        );
        assert_eq!(
            before_for(&request(1, Trigger::Manual, "dev", C, "nightly"), &green),
            B
        );
        assert_eq!(
            before_for(&request(1, Trigger::Manual, "main", A, "nightly"), &green),
            zeros40,
            "a manual run of the green head itself"
        );
        let mut rerun = request(1, Trigger::Rerun, "main", A, "quick");
        rerun.before = Some(D.into());
        assert_eq!(before_for(&rerun, &green), D, "a re-run keeps its before");
        assert_eq!(zeros(&"a".repeat(64)).len(), 64);
        assert!(is_zeros(&zeros40) && !is_zeros("") && !is_zeros(A));
    }

    /// Real `git log -1 --format=` [`COMMIT_FORMAT`] output.
    const LOG: &str = "430770d10a9af81a6d5e766905005710a3cd496a\n2026-09-28T13:24:44+00:00\nAda Lovelace\nada@example.com\nA\na@x\nsecond [skip ci]\n\nbody\n\n";

    #[test]
    fn commits_from_git_log() {
        let c = parse_commit(LOG).unwrap();
        assert_eq!(c.sha, "430770d10a9af81a6d5e766905005710a3cd496a");
        assert_eq!(c.timestamp, "2026-09-28T13:24:44+00:00");
        assert_eq!(
            (c.author.name.as_str(), c.author.email.as_str()),
            ("Ada Lovelace", "ada@example.com")
        );
        assert_eq!(c.committer.name, "A");
        assert_eq!(c.message, "second [skip ci]\n\nbody");
        assert!(has_skip_marker(&c.message));
        let bare = parse_commit(&format!("{A}\nt\n\n\n\n\n")).unwrap();
        assert_eq!((bare.author.name.as_str(), bare.message.as_str()), ("", ""));
        assert_eq!(parse_commit(""), None);
        assert_eq!(parse_commit("fatal: bad object\n"), None);
        assert_eq!(parse_commit(&format!("{A}\nt\nn\n")), None, "cut short");
    }

    fn project() -> Project {
        Project {
            repo: "tjrb-xyz/example".into(),
            default_branch: "main".into(),
            login: "tjrb".into(),
            tier_input: "tier".into(),
        }
    }

    #[test]
    fn the_event_act_runs() {
        let head = parse_commit(LOG).unwrap();
        let mut r = request(7, Trigger::Push, "main", &head.sha, "quick");
        let green = heads(&[("main", A)]);
        let before = before_for(&r, &green);
        let e = event_payload(&project(), &r, &before, false, &head);
        assert_eq!(e["ref"], "refs/heads/main");
        assert_eq!(e["before"], A);
        assert_eq!(e["after"], head.sha);
        assert_eq!(e["deleted"], false, "act takes github.sha from after");
        assert_eq!(e["created"], false);
        assert_eq!(e["forced"], false);
        assert_eq!(e["inputs"], json!({"tier": "quick"}));
        assert_eq!(
            e["compare"],
            "https://github.com/tjrb-xyz/example/compare/aaaaaaaaaaaa...430770d10a9a"
        );
        assert_eq!(e["head_commit"]["id"], head.sha);
        assert_eq!(e["head_commit"]["message"], "second [skip ci]\n\nbody");
        assert_eq!(e["head_commit"]["timestamp"], "2026-09-28T13:24:44+00:00");
        assert_eq!(
            e["head_commit"]["author"],
            json!({"name": "Ada Lovelace", "email": "ada@example.com"})
        );
        assert_eq!(
            e["repository"],
            json!({"full_name": "tjrb-xyz/example", "name": "example", "owner": {"login": "tjrb-xyz"},
                   "default_branch": "main", "private": true})
        );
        assert_eq!(e["sender"]["login"], "tjrb");

        // Force-pushed: git said the green head is not an ancestor.
        let e = event_payload(&project(), &r, &before, true, &head);
        assert_eq!(e["forced"], true);

        // A new ref: no before, so created, and never forced.
        r.git_ref = full("feature/x");
        let before = before_for(&r, &green);
        let e = event_payload(&project(), &r, &before, true, &head);
        assert_eq!(e["before"], "0".repeat(40));
        assert_eq!(e["created"], true);
        assert_eq!(e["forced"], false);
        assert_eq!(
            e["compare"],
            "https://github.com/tjrb-xyz/example/commit/430770d10a9af81a6d5e766905005710a3cd496a"
        );

        // The tier under the workflow's own input name, and none without tiers.
        let p = Project {
            tier_input: "level".into(),
            ..project()
        };
        r.tier = "nightly".into();
        assert_eq!(
            event_payload(&p, &r, &before, false, &head)["inputs"],
            json!({"level": "nightly"})
        );
        r.tier = String::new();
        assert_eq!(
            event_payload(&p, &r, &before, false, &head)["inputs"],
            json!({})
        );
    }

    #[test]
    fn a_build_json_starts_with_its_request() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Record {
            #[serde(flatten)]
            request: Request,
            #[serde(flatten)]
            build: crate::actlog::Build,
        }
        let mut req = request(12, Trigger::Retry, "feature/x", A, "quick");
        (req.attempt, req.queued_at, req.before) = (2, 1_790_600_454, Some(B.into()));
        let rec = Record {
            request: req,
            build: crate::actlog::Build::new(&[(0, "rust".into())], 1_790_600_460),
        };
        let v = serde_json::to_value(&rec).unwrap();
        assert_eq!(v["ref"], "refs/heads/feature/x");
        assert_eq!(v["trigger"], "retry");
        assert_eq!(v["state"], "running");
        assert_eq!(v["jobs"][0]["key"], "rust");
        assert!(v.get("fix").is_none() && v.get("round").is_none(), "{v}");
        let back: Record = serde_json::from_value(v).unwrap();
        assert_eq!(back, rec);
        let old: Request =
            serde_json::from_str(r#"{"id":3,"ref":"refs/tags/v1","sha":"x"}"#).unwrap();
        assert!(!old.is_fix());
        assert_eq!((old.trigger, old.before), (Trigger::Push, None));

        // A round's build says whose, which job and which round.
        let mut round = request(13, Trigger::Fix, "main", A, "quick");
        (round.fix, round.job, round.round) =
            (Some("aaaaaaa".into()), Some("rust".into()), Some(2));
        let v = serde_json::to_value(&round).unwrap();
        assert_eq!(
            (&v["trigger"], &v["fix"], &v["job"], &v["round"]),
            (&json!("fix"), &json!("aaaaaaa"), &json!("rust"), &json!(2))
        );
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), round);
        assert!(round.is_fix());

        let q = rec.request.view(Some("waiting for Docker".into()));
        assert_eq!(
            (
                q.id,
                q.git_ref.as_str(),
                q.trigger.as_str(),
                q.waiting.as_deref()
            ),
            (12, "feature/x", "retry", Some("waiting for Docker"))
        );
        for t in [
            Trigger::Push,
            Trigger::Manual,
            Trigger::Rerun,
            Trigger::Retry,
            Trigger::Fix,
        ] {
            assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
        }
    }
}

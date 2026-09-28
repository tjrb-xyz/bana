//! The daemon: bana's CI on push, on this machine. `bana-manager daemon --dir
//! ~/.bana/<prefix>` fetches the project's own clone, queues the pushes the
//! rules take ([`crate::watch`]), runs each through `bana ci` (act), one build
//! at a time, folds act's log into the build ([`crate::actlog`]) and posts the
//! commit statuses that say so through the GitHub CLI.
//!
//! Three tasks share one state: the watcher (fetch, list the heads, decide),
//! the runner (the gates, then one build) and the poster (statuses, in order,
//! the newest state per context). The page and the menu bar call [`Daemon`]'s
//! methods and read its [`Summary`] from a watch channel.
//!
//! A cancel (the page, the menu bar, daemon.timeout, a newer push, a stop)
//! climbs a ladder: SIGINT to act, 60 s, a second SIGINT, 30 s, then SIGKILL of
//! act's whole tree (20 s and 10 s when the daemon stops). After every build
//! the sweep ([`crate::sweep`]) ends the processes that carry its marker and
//! removes act's host workspaces, and after one that did not end on its own,
//! the job containers too. A build the daemon left running (a stop, a crash) is
//! ended at the next start, and run again once if nothing moved on.
//!
//! Under the directory:
//! - `daemon/settings`: written by `bana daemon install` (below);
//! - `src/`: the clone builds check out, with `refs/bana/green/*` pins;
//! - `state.json`: paused, the next id, the heads last seen, each ref's last
//!   green head, the queue (ids), whether GitHub takes our target_url, and the
//!   pushes not built;
//! - `builds/<id>/`: `build.json` ([`Record`]), `jobs.txt` (`act -l`),
//!   `event.json`, `act.jsonl` (act's lines, and bana's), `artifacts/`, and
//!   `secrets` (0600) while act runs;
//! - `act-cache/`: act's action cache;
//! - `daemon.lock`: locked (flock) while a daemon runs here;
//! - `vars`: optional, the owner's `KEY=value` lines for `vars.*`.
//!
//! JSON goes to a temporary file, is synced, then renamed over the old one.
//!
//! The settings file has `key = value` lines; `#` starts a comment. A key not
//! below is an error.
//!
//!   repo              owner/repo (required)
//!   prefix            bana.conf's prefix (required)
//!   workflow          ci.yml
//!   tiers             quick nightly release (spaces or commas; empty: no tier)
//!   tier_input        tier
//!   daemon.branches   * !dependabot/* !renovate/*
//!   daemon.tags       (empty: no tags)
//!   daemon.tier       the first of tiers: branch pushes run it
//!   daemon.tag_tier   the last of tiers: tag pushes run it
//!   daemon.poll       30: seconds between fetches
//!   daemon.timeout    120: minutes of awake time a build may take (`90s`: seconds)
//!   daemon.supersede  queued, or running: a newer push cancels its ref's build
//!   daemon.token      gh (the jobs' GITHUB_TOKEN is gh's), or none (empty)
//!   port              8470: the page's port, which target_url links to
//!   host              this machine's name in descriptions (hostname -s)
//!   login             the GitHub user the daemon runs for (the event's sender)
//!   path              PATH for all it runs (default: its own PATH)
//!   tray              yes or no: the menu bar (yes on macOS)
//!   home              bana's home, where act.lock is (${BANA_HOME:-$HOME/.bana})
//!   git gh docker caffeinate bash   the programs (default: found on path)
//!   script            the bin/bana builds run (daemon/bin/bana)

use crate::actlog::{
    self, Build, BuildState, BuildView, Event, Report, Status, StatusState, Summary, Watcher,
};
use crate::sweep;
use crate::watch::{
    self, Action, Built, Heads, Project, Pushed, Request, Rules, Supersede, Trigger,
};
use crate::{valid_repo, valid_tier, valid_workflow};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

/// Build directories kept; older ones go after each build.
const KEEP_BUILDS: usize = 100;
/// A build's artifacts are kept this long (seconds).
const KEEP_ARTIFACTS: i64 = 7 * 86_400;
/// The longest wait between tries to post statuses.
const RETRY_MAX: Duration = Duration::from_secs(300);
/// Pushes not built that state.json remembers.
const KEEP_SKIPPED: usize = 100;
/// A log page reads at most this much of act.jsonl.
const LOG_PAGE: u64 = 256 * 1024;
/// The label act's job containers carry, so a sweep finds them.
pub const LABEL: &str = "xyz.tjrb.bana";
/// Why a build stopped when the daemon did. The next start retries it once.
pub const INTERRUPTED: &str = "interrupted (bana restarted)";

const KEYS: &[&str] = &[
    "repo",
    "prefix",
    "workflow",
    "tiers",
    "tier_input",
    "daemon.branches",
    "daemon.tags",
    "daemon.tier",
    "daemon.tag_tier",
    "daemon.poll",
    "daemon.timeout",
    "daemon.supersede",
    "daemon.token",
    "port",
    "host",
    "login",
    "path",
    "tray",
    "home",
    "git",
    "gh",
    "docker",
    "caffeinate",
    "bash",
    "script",
];

/// The jobs' GITHUB_TOKEN (`daemon.token`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobToken {
    /// The GitHub CLI's token, as `bana ci` gives it.
    Gh,
    /// An empty one, so act does not ask gh itself.
    Empty,
}

/// The daemon's settings file, read once at start.
#[derive(Debug, Clone)]
pub struct Settings {
    /// `~/.bana/<prefix>`.
    pub dir: PathBuf,
    pub repo: String,
    pub prefix: String,
    pub workflow: String,
    pub tiers: Vec<String>,
    pub tier_input: String,
    pub rules: Rules,
    pub poll: Duration,
    pub timeout: Duration,
    pub token: JobToken,
    pub port: u16,
    pub machine: String,
    pub login: String,
    pub path: String,
    pub tray: bool,
    /// bana's home: act.lock is here.
    pub home: PathBuf,
    /// The home was named (`home`, or BANA_HOME), so children get BANA_HOME.
    pub home_set: bool,
    pub git: String,
    pub gh: String,
    pub docker: String,
    pub caffeinate: String,
    pub bash: String,
    pub script: PathBuf,
    /// The environment the daemon started with; builds get only an allowlist of it.
    pub env: BTreeMap<String, String>,
    /// The first wait after a failed post; it doubles up to 5 minutes.
    pub retry: Duration,
    /// How often a queue held back by its gates looks again.
    pub recheck: Duration,
    /// The cancel ladder's waits: after the first SIGINT, then after the
    /// second, before act's whole tree is killed.
    pub ladder: [Duration; 2],
    /// The same when the daemon stops (launchd and systemd wait 60 s).
    pub ladder_short: [Duration; 2],
    /// How long the marker's processes have between SIGTERM and SIGKILL.
    pub grace: Duration,
}

impl Settings {
    /// `<dir>/daemon/settings`, with the daemon's own environment.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let file = dir.join("daemon/settings");
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        Self::parse(&text, dir, std::env::vars().collect())
    }

    pub fn parse(text: &str, dir: &Path, env: BTreeMap<String, String>) -> Result<Self, String> {
        let mut kv: BTreeMap<&str, &str> = BTreeMap::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = |what: String| format!("settings, line {}: {what}", n + 1);
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| at("not key = value".into()))?;
            let k = KEYS
                .iter()
                .find(|x| **x == k.trim())
                .ok_or_else(|| at(format!("unknown key {}", k.trim())))?;
            kv.insert(*k, v.trim());
        }
        let get = |k: &str| kv.get(k).copied();
        let or = |k: &str, d: &str| get(k).unwrap_or(d).to_string();
        let bad = |k: &str, what: &str| format!("settings: {k} = {what}");
        let repo = or("repo", "");
        if !valid_repo(&repo) {
            return Err(bad("repo", "owner/repo"));
        }
        let prefix = or("prefix", "");
        if !valid_tier(&prefix) {
            return Err(bad("prefix", "bana.conf's prefix"));
        }
        let workflow = or("workflow", "ci.yml");
        if !valid_workflow(&workflow) {
            return Err(bad("workflow", "a file in .github/workflows"));
        }
        let tiers = watch::patterns(get("tiers").unwrap_or("quick nightly release"));
        let tier_input = or("tier_input", "tier");
        if !tiers.iter().all(|t| valid_tier(t)) || !valid_tier(&tier_input) {
            return Err(bad("tiers and tier_input", "letters, digits, '_' and '-'"));
        }
        let tier = |k: &str, d: Option<&String>| {
            let t = get(k)
                .map(String::from)
                .or_else(|| d.cloned())
                .unwrap_or_default();
            if tiers.contains(&t) || tiers.is_empty() && t.is_empty() {
                Ok(t)
            } else {
                Err(bad(k, "one of tiers"))
            }
        };
        let rules = Rules {
            branches: watch::patterns(
                get("daemon.branches").unwrap_or("* !dependabot/* !renovate/*"),
            ),
            tags: watch::patterns(get("daemon.tags").unwrap_or("")),
            tier: tier("daemon.tier", tiers.first())?,
            tag_tier: tier("daemon.tag_tier", tiers.last())?,
            supersede: Supersede::parse(get("daemon.supersede").unwrap_or("queued"))
                .ok_or_else(|| bad("daemon.supersede", "queued or running"))?,
        };
        let poll = seconds(get("daemon.poll").unwrap_or("30"), 1)
            .ok_or_else(|| bad("daemon.poll", "seconds"))?;
        let timeout = match get("daemon.timeout").unwrap_or("120") {
            t if t.ends_with('s') => seconds(&t[..t.len() - 1], 1),
            t => seconds(t, 60),
        }
        .ok_or_else(|| bad("daemon.timeout", "minutes, or seconds with an s"))?;
        let token = match get("daemon.token").unwrap_or("gh") {
            "gh" => JobToken::Gh,
            "none" => JobToken::Empty,
            _ => return Err(bad("daemon.token", "gh or none")),
        };
        let port = match get("port").unwrap_or("8470").parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => return Err(bad("port", "a port number")),
        };
        let tray = match get("tray").unwrap_or(if cfg!(target_os = "macos") {
            "yes"
        } else {
            "no"
        }) {
            "yes" | "true" | "1" => true,
            "no" | "false" | "0" => false,
            _ => return Err(bad("tray", "yes or no")),
        };
        let home_set = get("home").is_some() || env.contains_key("BANA_HOME");
        let home = get("home")
            .map(PathBuf::from)
            .or_else(|| env.get("BANA_HOME").map(PathBuf::from))
            .or_else(|| env.get("HOME").map(|h| Path::new(h).join(".bana")))
            .ok_or_else(|| bad("home", "a directory (no HOME here)"))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            repo,
            prefix,
            workflow,
            tiers,
            tier_input,
            rules,
            poll,
            timeout,
            token,
            port,
            machine: get("host").map(String::from).unwrap_or_else(machine_name),
            login: or("login", ""),
            path: get("path")
                .map(String::from)
                .or_else(|| env.get("PATH").cloned())
                .unwrap_or_else(|| "/usr/bin:/bin".into()),
            tray,
            home,
            home_set,
            git: or("git", "git"),
            gh: or("gh", "gh"),
            docker: or("docker", "docker"),
            caffeinate: or("caffeinate", "caffeinate"),
            bash: or("bash", "bash"),
            script: get("script")
                .map(PathBuf::from)
                .unwrap_or_else(|| dir.join("daemon/bin/bana")),
            env,
            retry: Duration::from_secs(5),
            recheck: Duration::from_secs(10),
            ladder: [Duration::from_secs(60), Duration::from_secs(30)],
            ladder_short: [Duration::from_secs(20), Duration::from_secs(10)],
            grace: Duration::from_secs(5),
        })
    }

    /// What a build's `bana` runs with: nothing of the daemon's environment
    /// but this allowlist (act copies its whole environment into host jobs).
    fn child_env(&self, id: u64) -> Vec<(String, String)> {
        let mut env = vec![("PATH".to_string(), self.path.clone())];
        for k in [
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "LANG",
            "LC_ALL",
            "TMPDIR",
            "DOCKER_HOST",
        ] {
            if let Some(v) = self.env.get(k) {
                env.push((k.into(), v.clone()));
            }
        }
        if self.home_set {
            env.push(("BANA_HOME".into(), self.home.to_string_lossy().into()));
        }
        let src = self.dir.join("src").to_string_lossy().into_owned();
        env.push(("BANA_PROJECT_ROOT".into(), src));
        env.push(("BANA_ACT_LOCKED".into(), "1".into()));
        env.push(("BANA_BUILD".into(), format!("{}-{id}", self.prefix)));
        env
    }

    /// act's options for a build (after `bana ci … --`): no input from the pushed
    /// tree but the workflow, bana's secrets, and a label on every container.
    fn act_flags(&self, id: u64, port: u16) -> Vec<String> {
        let vars = self.dir.join("vars");
        let vars = if vars.is_file() {
            vars.to_string_lossy().into_owned()
        } else {
            "/dev/null".into()
        };
        let cache = self.dir.join("act-cache").to_string_lossy().into_owned();
        [
            "--json",
            "--rm",
            "--pull=false",
            "--secret-file",
            "secrets",
            "--env-file",
            "/dev/null",
            "--var-file",
            &vars,
            "--input-file",
            "/dev/null",
            "--container-daemon-socket",
            "-",
            &format!("--container-options=--label {LABEL}={}", self.prefix),
            "--artifact-server-path",
            "artifacts",
            "--artifact-server-port",
            &port.to_string(),
            "--action-cache-path",
            &cache,
            "--env",
            &format!("GITHUB_RUN_ID={id}"),
            "--env",
            &format!("GITHUB_RUN_NUMBER={id}"),
            "--env",
            "BANA_DAEMON=1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

fn seconds(s: &str, unit: u64) -> Option<Duration> {
    s.trim()
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .map(|n| Duration::from_secs(n * unit))
}

/// This machine's short name, as descriptions say it (`mbp`).
pub fn machine_name() -> String {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

/// state.json.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub version: u32,
    pub paused: bool,
    pub next_id: u64,
    /// The heads last seen.
    pub heads: Heads,
    /// Each ref's last green head (pinned as [`watch::green_pin`] in src).
    pub green: Heads,
    /// Queued builds, the next first.
    pub queue: Vec<u64>,
    /// GitHub took a status with our target_url (or has not refused one yet).
    pub target_url_ok: bool,
    /// The heads were recorded once: the first start builds nothing.
    pub first_start_done: bool,
    /// Pushes not built, the newest last.
    pub skipped: Vec<Skipped>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            paused: false,
            next_id: 1,
            heads: Heads::new(),
            green: Heads::new(),
            queue: Vec::new(),
            target_url_ok: true,
            first_start_done: false,
            skipped: Vec::new(),
        }
    }
}

/// A push the rules left out: `skip marker`, or `no workflow`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub sha: String,
    pub why: String,
    pub at: i64,
}

/// A status a build wants on GitHub, and whether it got there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Posting {
    pub state: StatusState,
    pub description: String,
    /// When it was wanted, across builds: statuses go out in this order.
    pub seq: u64,
    pub posted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// build.json: what was asked for, what act did, and the statuses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    #[serde(flatten)]
    pub request: Request,
    #[serde(flatten)]
    pub build: Build,
    /// `bana`, or `bana <tier>` ([`Report::new`]).
    pub context_prefix: String,
    /// `before` is not an ancestor of the commit.
    pub forced: bool,
    /// act's pid, and when it started (`ps -o lstart=`).
    pub pid: Option<u32>,
    pub pid_start: Option<String>,
    /// act's artifact server port.
    pub port: Option<u16>,
    /// By context.
    pub statuses: BTreeMap<String, Posting>,
}

/// Lines of one build's log, from a byte offset in act.jsonl.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogPage {
    /// Where the next page starts.
    pub next: u64,
    pub lines: Vec<LogLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogLine {
    /// Unix seconds.
    pub t: Option<i64>,
    /// The job's key; none for bana's and act's own lines.
    pub job: Option<String>,
    pub step: Option<String>,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

struct Running {
    id: u64,
    since: Instant,
    cancel: mpsc::UnboundedSender<String>,
}

struct Inner {
    state: State,
    records: BTreeMap<u64, Record>,
    running: Option<Running>,
    watcher: Watcher,
    /// Why the queue waits (paused, Docker, the lock), when no build runs.
    waiting: Option<String>,
    /// The last [`Posting::seq`] handed out.
    seq: u64,
    /// Polls done, for tests.
    polls: u64,
    stopping: bool,
}

struct Shared {
    settings: Settings,
    inner: Mutex<Inner>,
    summary: tokio::sync::watch::Sender<Summary>,
    poll: Notify,
    run: Notify,
    post: Notify,
    post_now: Notify,
    stop: tokio::sync::watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// daemon.lock, locked while this daemon runs ([`claim`]).
    claimed: Mutex<Option<std::fs::File>>,
}

/// The running daemon. The HTTP routes and the menu bar call these.
#[derive(Clone)]
pub struct Daemon(Arc<Shared>);

impl Daemon {
    /// Reads state.json and the builds, then starts the watcher, the runner and
    /// the poster. The clone (`src`) must exist: `bana daemon install` makes it.
    pub async fn start(settings: Settings) -> Result<Self, String> {
        let dir = settings.dir.clone();
        if !dir.join("src/.git").exists() {
            return Err(format!(
                "no clone at {}: run bana daemon install",
                dir.join("src").display()
            ));
        }
        for d in ["builds", "act-cache"] {
            std::fs::create_dir_all(dir.join(d)).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let claimed = claim(&dir).await?;
        let state: State = match std::fs::read(dir.join("state.json")) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("state.json: {e}"))?,
            Err(_) => State::default(),
        };
        let mut records = BTreeMap::new();
        for e in std::fs::read_dir(dir.join("builds"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let Some(id) = e.file_name().to_str().and_then(|n| n.parse::<u64>().ok()) else {
                continue;
            };
            match std::fs::read(e.path().join("build.json"))
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice::<Record>(&b).map_err(|e| e.to_string()))
            {
                Ok(r) if r.request.id == id => {
                    records.insert(id, r);
                }
                Ok(_) => eprintln!("bana daemon: builds/{id}/build.json: another id"),
                Err(e) => eprintln!("bana daemon: builds/{id}/build.json: {e}"),
            }
        }
        let seq = records
            .values()
            .flat_map(|r| r.statuses.values().map(|p| p.seq))
            .max()
            .unwrap_or(0);
        let summary = tokio::sync::watch::Sender::new(Summary::default());
        let (stop, _) = tokio::sync::watch::channel(false);
        let shared = Arc::new(Shared {
            settings,
            inner: Mutex::new(Inner {
                state,
                records,
                running: None,
                watcher: Watcher {
                    docker: true,
                    ..Watcher::default()
                },
                waiting: None,
                seq,
                polls: 0,
                stopping: false,
            }),
            summary,
            poll: Notify::new(),
            run: Notify::new(),
            post: Notify::new(),
            post_now: Notify::new(),
            stop,
            tasks: Mutex::new(Vec::new()),
            claimed: Mutex::new(Some(claimed)),
        });
        shared.recover().await;
        {
            let inner = shared.lock();
            shared.publish(&inner);
        }
        shared.post.notify_one();
        let tasks = vec![
            tokio::spawn(shared.clone().watcher()),
            tokio::spawn(shared.clone().runner()),
            tokio::spawn(shared.clone().poster()),
        ];
        *shared.tasks.lock().unwrap_or_else(|e| e.into_inner()) = tasks;
        Ok(Self(shared))
    }

    pub fn settings(&self) -> &Settings {
        &self.0.settings
    }

    /// The summary as it changes (the menu bar).
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Summary> {
        self.0.summary.subscribe()
    }

    /// The summary now (`GET /ci/v1/local`).
    pub fn summary(&self) -> Summary {
        let inner = self.0.lock();
        self.0.publish(&inner);
        self.0.summary.borrow().clone()
    }

    /// The heads last fetched: the refs Run now may name.
    pub fn heads(&self) -> Heads {
        self.0.lock().state.heads.clone()
    }

    /// Pushes not built (skip markers, no workflow), the newest last.
    pub fn skipped(&self) -> Vec<Skipped> {
        self.0.lock().state.skipped.clone()
    }

    /// The history, newest first: builds older than `before` (all when none),
    /// at most `limit` (up to 100).
    pub fn builds(&self, before: Option<u64>, limit: usize) -> Vec<BuildView> {
        let inner = self.0.lock();
        inner
            .records
            .values()
            .rev()
            .filter(|r| before.is_none_or(|b| r.request.id < b))
            .take(limit.min(KEEP_BUILDS))
            .map(|r| self.0.view(&inner, r))
            .collect()
    }

    /// One build: build.json, with the jobs `act -l` listed and its compare link.
    pub fn build(&self, id: u64) -> Option<Value> {
        let mut v = serde_json::to_value(self.0.lock().records.get(&id)?).ok()?;
        let dir = self.0.build_dir(id);
        let list = std::fs::read_to_string(dir.join("jobs.txt")).unwrap_or_default();
        v["listed"] = json!(actlog::parse_list(&list));
        let event: Value = std::fs::read(dir.join("event.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        v["compare"] = event["compare"].clone();
        Some(v)
    }

    /// A build's log lines from byte `from` of act.jsonl: one job's (by key), or all.
    pub fn log(&self, id: u64, job: Option<&str>, from: u64) -> Result<LogPage, String> {
        if !self.0.lock().records.contains_key(&id) {
            return Err(format!("no build {id}"));
        }
        let path = self.0.build_dir(id).join("act.jsonl");
        let mut f = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                return Ok(LogPage {
                    next: 0,
                    lines: vec![],
                })
            }
        };
        f.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        Read::by_ref(&mut f)
            .take(LOG_PAGE)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        // Whole lines only; the rest comes with the next page.
        let whole = buf.iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
        if whole == 0 && buf.len() as u64 == LOG_PAGE {
            // One line longer than a page: it is left out, or the log would
            // never get past it.
            let (mut n, mut chunk) = (0u64, vec![0; 64 * 1024]);
            let end = loop {
                let got = f.read(&mut chunk).map_err(|e| e.to_string())?;
                if got == 0 {
                    break false;
                }
                if let Some(i) = chunk[..got].iter().position(|&c| c == b'\n') {
                    n += i as u64 + 1;
                    break true;
                }
                n += got as u64;
            };
            if end {
                return Ok(LogPage {
                    next: from + LOG_PAGE + n,
                    lines: vec![LogLine {
                        t: None,
                        job: None,
                        step: None,
                        msg: format!("(a line of {} bytes, left out)", LOG_PAGE + n),
                        result: None,
                    }],
                });
            }
        }
        let text = String::from_utf8_lossy(&buf[..whole]);
        let lines = text
            .lines()
            .filter_map(|l| match actlog::parse_line(l) {
                Event::Job(l) if job.is_none_or(|j| j == l.key) => {
                    let marker = l.output
                        || l.step_result.is_some()
                        || l.job_result.is_some()
                        || l.msg.contains("⭐ Run ")
                        || l.level == "error";
                    marker.then(|| LogLine {
                        t: l.time,
                        job: Some(l.key.clone()),
                        step: l.step.clone(),
                        msg: l.msg.clone(),
                        result: l.step_result.clone().or(l.job_result.clone()),
                    })
                }
                Event::Text { msg, .. } if job.is_none() => Some(LogLine {
                    t: None,
                    job: None,
                    step: None,
                    msg,
                    result: None,
                }),
                Event::Cancel(msg) if job.is_none() => Some(LogLine {
                    t: None,
                    job: None,
                    step: None,
                    msg,
                    result: Some("cancelled".into()),
                }),
                _ => None,
            })
            .collect();
        Ok(LogPage {
            next: from + whole as u64,
            lines,
        })
    }

    /// Run now: `git_ref` (a head last fetched: `refs/heads/main`, or `main`)
    /// at `tier`, at the front of the queue.
    pub fn run_now(&self, git_ref: &str, tier: &str) -> Result<u64, String> {
        let s = &self.0.settings;
        if !(s.tiers.iter().any(|t| t == tier) || s.tiers.is_empty() && tier.is_empty()) {
            return Err(format!("tier: one of {}", s.tiers.join(", ")));
        }
        let mut inner = self.0.lock();
        let (git_ref, sha) = [
            git_ref.to_string(),
            format!("refs/heads/{git_ref}"),
            format!("refs/tags/{git_ref}"),
        ]
        .into_iter()
        .find_map(|r| inner.state.heads.get(&r).map(|sha| (r, sha.clone())))
        .ok_or_else(|| format!("no branch or tag {git_ref} here"))?;
        let id = self.0.enqueue(
            &mut inner,
            Request {
                trigger: Trigger::Manual,
                git_ref,
                sha,
                tier: tier.to_string(),
                ..Request::default()
            },
            true,
        );
        drop(inner);
        self.0.run.notify_one();
        Ok(id)
    }

    /// A new build of a build's commit, tier and before, at the front of the
    /// queue; it runs even if that commit was built.
    pub fn rerun(&self, id: u64) -> Result<u64, String> {
        let mut inner = self.0.lock();
        let r = inner
            .records
            .get(&id)
            .ok_or_else(|| format!("no build {id}"))?;
        if !r.build.state.finished() {
            return Err(format!("build {id} has not finished"));
        }
        let req = Request {
            trigger: Trigger::Rerun,
            git_ref: r.request.git_ref.clone(),
            sha: r.request.sha.clone(),
            tier: r.request.tier.clone(),
            before: r.request.before.clone(),
            ..Request::default()
        };
        let new = self.0.enqueue(&mut inner, req, true);
        drop(inner);
        self.0.run.notify_one();
        Ok(new)
    }

    /// Removes a queued build, or cancels the running one with `reason`
    /// (`cancelled from the menu bar`).
    pub fn cancel(&self, id: u64, reason: &str) -> Result<(), String> {
        let mut inner = self.0.lock();
        if inner.state.queue.contains(&id) {
            self.0.unqueue(&mut inner, id);
            self.0.save_state(&inner.state);
            self.0.publish(&inner);
            return Ok(());
        }
        match &inner.running {
            Some(r) if r.id == id => {
                let _ = r.cancel.send(reason.to_string());
                Ok(())
            }
            _ => Err(format!("build {id} is not queued or running")),
        }
    }

    /// Pause or resume starting builds; fetching goes on.
    pub fn set_paused(&self, paused: bool) {
        let mut inner = self.0.lock();
        inner.state.paused = paused;
        self.0.save_state(&inner.state);
        self.0.publish(&inner);
        drop(inner);
        self.0.run.notify_one();
    }

    /// Fetch now, and try posting again now.
    pub fn poll_now(&self) {
        self.0.poll.notify_one();
        self.0.post_now.notify_one();
    }

    /// Drops every queued build; says how many.
    pub fn clear_queue(&self) -> usize {
        let mut inner = self.0.lock();
        let ids = inner.state.queue.clone();
        for id in &ids {
            self.0.unqueue(&mut inner, *id);
        }
        self.0.save_state(&inner.state);
        self.0.publish(&inner);
        ids.len()
    }

    /// Each build's statuses: how many were posted, and how many wait to be
    /// (not those a newer build of the commit took over). For the history.
    pub fn statuses(&self) -> BTreeMap<u64, (usize, usize)> {
        let inner = self.0.lock();
        let waiting = to_post(&inner);
        inner
            .records
            .iter()
            .map(|(id, r)| {
                let posted = r.statuses.values().filter(|p| p.posted).count();
                (
                    *id,
                    (posted, waiting.iter().filter(|(w, _)| w == id).count()),
                )
            })
            .collect()
    }

    /// Stops fetching and starting builds, stops the running build (the short
    /// ladder, then the sweeps) and waits for the tasks. The build is left
    /// unfinished, with nothing more to post: the next start decides.
    /// As if the process were killed (-9): the tasks just stop, and act runs on.
    #[cfg(test)]
    fn crash(&self) {
        for t in std::mem::take(&mut *self.0.tasks.lock().unwrap_or_else(|e| e.into_inner())) {
            t.abort();
        }
        // The kernel lets go of the lock when the process dies.
        self.0
            .claimed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }

    pub async fn shutdown(&self) {
        {
            let mut inner = self.0.lock();
            inner.stopping = true;
            if let Some(r) = &inner.running {
                let _ = r.cancel.send(INTERRUPTED.into());
            }
        }
        let _ = self.0.stop.send(true);
        let tasks = std::mem::take(&mut *self.0.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        for t in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(120), t).await;
        }
        self.0
            .claimed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn src(&self) -> PathBuf {
        self.settings.dir.join("src")
    }

    fn build_dir(&self, id: u64) -> PathBuf {
        self.settings.dir.join("builds").join(id.to_string())
    }

    fn lock_dir(&self) -> PathBuf {
        self.settings.home.join("act.lock")
    }

    fn save_state(&self, state: &State) {
        if let Err(e) = write_json(&self.settings.dir.join("state.json"), state) {
            eprintln!("bana daemon: state.json: {e}");
        }
    }

    fn save(&self, rec: &Record) {
        let dir = self.build_dir(rec.request.id);
        if let Err(e) =
            std::fs::create_dir_all(&dir).and_then(|_| write_json(&dir.join("build.json"), rec))
        {
            eprintln!("bana daemon: {}: {e}", dir.display());
        }
    }

    fn report(&self, rec: &Record) -> Report {
        Report {
            context: rec.context_prefix.clone(),
            machine: self.settings.machine.clone(),
            tier: rec.request.tier.clone(),
        }
    }

    fn view(&self, inner: &Inner, r: &Record) -> BuildView {
        let b = &r.build;
        let elapsed = match &inner.running {
            Some(run) if run.id == r.request.id => run.since.elapsed().as_secs(),
            _ => match (b.started_at, b.ended_at) {
                (Some(a), Some(z)) => (z - a).max(0) as u64,
                _ => 0,
            },
        };
        BuildView {
            id: r.request.id,
            git_ref: watch::short_ref(&r.request.git_ref).to_string(),
            sha: r.request.sha.clone(),
            tier: r.request.tier.clone(),
            trigger: r.request.trigger.as_str().to_string(),
            attempt: r.request.attempt,
            state: b.state,
            reason: b.reason.clone(),
            started_at: b.started_at,
            ended_at: b.ended_at,
            elapsed,
            description: self
                .report(r)
                .describe(b)
                .map(|(_, d)| d)
                .unwrap_or_default(),
            jobs: b.chips(),
        }
    }

    fn publish(&self, inner: &Inner) {
        let s = &self.settings;
        let running = inner
            .running
            .as_ref()
            .and_then(|r| inner.records.get(&r.id));
        let waiting = if running.is_none() {
            inner.waiting.clone()
        } else {
            None
        };
        let summary = Summary {
            repo: s.repo.clone(),
            prefix: s.prefix.clone(),
            machine: s.machine.clone(),
            now: now(),
            watcher: Watcher {
                paused: inner.state.paused,
                unposted: to_post(inner).len(),
                ..inner.watcher.clone()
            },
            running: running.map(|r| self.view(inner, r)),
            queue: inner
                .state
                .queue
                .iter()
                .filter_map(|id| inner.records.get(id))
                .map(|r| r.request.view(waiting.clone()))
                .collect(),
            last: inner
                .records
                .values()
                .rev()
                .find(|r| r.build.state.finished())
                .map(|r| self.view(inner, r)),
        };
        self.summary.send_replace(summary);
    }

    /// Queues a build: at the back, or at the front (after the other builds
    /// asked for by hand).
    fn enqueue(&self, inner: &mut Inner, mut req: Request, front: bool) -> u64 {
        let id = inner.state.next_id;
        inner.state.next_id += 1;
        req.id = id;
        req.attempt = req.attempt.max(1);
        req.queued_at = now();
        let rec = Record {
            context_prefix: Report::new(&req.tier, &self.settings.rules.tier, "").context,
            request: req,
            ..Record::default()
        };
        self.save(&rec);
        inner.records.insert(id, rec);
        let at = if front {
            let records = &inner.records;
            inner
                .state
                .queue
                .iter()
                .position(|q| {
                    records
                        .get(q)
                        .is_none_or(|r| r.request.trigger == Trigger::Push)
                })
                .unwrap_or(inner.state.queue.len())
        } else {
            inner.state.queue.len()
        };
        inner.state.queue.insert(at, id);
        self.save_state(&inner.state);
        self.publish(inner);
        id
    }

    /// Takes a queued build out: it never ran, so it leaves nothing behind. A
    /// retry that never runs leaves its interrupted build to say how it ended,
    /// or that build's pendings would stay on GitHub.
    fn unqueue(&self, inner: &mut Inner, id: u64) {
        inner.state.queue.retain(|q| *q != id);
        let Some(rec) = inner
            .records
            .get(&id)
            .filter(|r| r.build.state == BuildState::Queued)
        else {
            return;
        };
        let retry = (rec.request.trigger == Trigger::Retry).then(|| rec.request.clone());
        inner.records.remove(&id);
        let _ = std::fs::remove_dir_all(self.build_dir(id));
        let Some(req) = retry else { return };
        let Inner { records, seq, .. } = inner;
        let first = records.range_mut(..id).rev().map(|(_, r)| r).find(|r| {
            (&r.request.git_ref, &r.request.sha, &r.request.tier)
                == (&req.git_ref, &req.sha, &req.tier)
                && r.build.reason.as_deref() == Some(INTERRUPTED)
        });
        if let Some(r) = first {
            let report = self.report(r);
            want_now(r, &report, seq);
            self.save(r);
            self.post.notify_one();
        }
    }

    /// After a restart: each build left running is ended. An act still running
    /// for it (its pid and start time match) is killed with its tree, and
    /// everything it left is swept. Its log is read again, so the jobs that
    /// finished keep their results. A cancel that was asked for ends it as
    /// cancelled. Otherwise it is queued again once, at the front (a retry,
    /// attempt 2), when its commit is still its ref's head (or it was a tag or
    /// asked for by hand); if not, it ends as interrupted, and that is posted.
    async fn recover(&self) {
        let running: Vec<Record> = {
            let mut inner = self.lock();
            let inner = &mut *inner;
            let records = &inner.records;
            inner.state.queue.retain(|id| {
                records
                    .get(id)
                    .is_some_and(|r| r.build.state == BuildState::Queued)
            });
            // Taken off the queue to start, then the daemon died before act
            // ran (the checkout, the job list): it goes back to the front.
            let lost: Vec<u64> = records
                .values()
                .filter(|r| r.build.state == BuildState::Queued)
                .map(|r| r.request.id)
                .filter(|id| !inner.state.queue.contains(id))
                .collect();
            for (i, id) in lost.iter().enumerate() {
                eprintln!("bana daemon: build {id} had not started; queued again");
                let _ = std::fs::remove_file(self.build_dir(*id).join("secrets"));
                inner.state.queue.insert(i, *id);
            }
            inner
                .records
                .values()
                .filter(|r| r.build.state == BuildState::Running)
                .cloned()
                .collect()
        };
        for rec in running {
            let id = rec.request.id;
            if let (Some(pid), Some(start)) = (rec.pid, &rec.pid_start) {
                if alive(pid)
                    && !start.is_empty()
                    && started(&self.settings.path, pid).await == *start
                {
                    eprintln!("bana daemon: build {id}: act ({pid}) still runs; killing it");
                    self.kill_tree(pid).await;
                }
            }
            self.sweep(id, true).await;
            let dir = self.build_dir(id);
            let list = std::fs::read_to_string(dir.join("jobs.txt")).unwrap_or_default();
            let mut build = Build::new(&actlog::parse_list(&list), 0);
            build.started_at = rec.build.started_at;
            if let Ok(log) = std::fs::read_to_string(dir.join("act.jsonl")) {
                build.fold_lines(&log, now());
            }
            if build.cancel_requested.is_none() {
                build.cancel_requested = rec.build.cancel_requested.clone();
            }
            let asked = build.cancel_requested.clone().filter(|r| r != INTERRUPTED);
            let mut inner = self.lock();
            let inner = &mut *inner;
            let retry = asked.is_none() && retry_eligible(&rec.request, &inner.state.heads);
            let Some(r) = inner.records.get_mut(&id) else {
                continue;
            };
            r.build = build;
            r.build
                .finish(None, Some(asked.as_deref().unwrap_or(INTERRUPTED)), now());
            if !retry {
                let report = self.report(r);
                want_now(r, &report, &mut inner.seq);
            }
            self.save(r);
            if retry {
                let req = Request {
                    trigger: Trigger::Retry,
                    attempt: 2,
                    ..rec.request.clone()
                };
                let new = self.enqueue(inner, req, true);
                eprintln!("bana daemon: build {id} was interrupted; retried as build {new}");
            }
        }
        self.clear_stale_lock().await;
        self.save_state(&self.lock().state);
    }

    // ---- running programs ----------------------------------------------------

    /// `program args` in `cwd`, with the daemon's PATH.
    async fn output(
        &self,
        program: &str,
        args: &[&str],
        cwd: &Path,
        secs: u64,
    ) -> Result<std::process::Output, String> {
        output(program, args, cwd, &self.settings.path, secs).await
    }

    /// git in src: its output, or what it said went wrong.
    async fn git(&self, args: &[&str], secs: u64) -> Result<String, String> {
        let o = self
            .output(&self.settings.git, args, &self.src(), secs)
            .await?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            Err(failure(&o))
        }
    }

    /// `-c` options that make gh git's only credential helper.
    fn credentials(&self) -> [String; 4] {
        [
            "-c".into(),
            "credential.helper=".into(),
            "-c".into(),
            format!(
                "credential.helper=!{} auth git-credential",
                sh_quote(&self.settings.gh)
            ),
        ]
    }

    // ---- the watcher -----------------------------------------------------------

    async fn watcher(self: Arc<Self>) {
        let mut tick = tokio::time::interval(self.settings.poll);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stop = self.stop.subscribe();
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.poll.notified() => {}
                _ = stop.changed() => return,
            }
            self.poll_once().await;
            self.lock().polls += 1;
        }
    }

    async fn poll_once(&self) {
        let cred = self.credentials();
        let mut fetch: Vec<&str> = cred.iter().map(String::as_str).collect();
        fetch.extend([
            "fetch",
            "--prune",
            "--quiet",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
            "+refs/tags/*:refs/tags/*",
        ]);
        let format = format!("--format={}", watch::REFS_FORMAT);
        let listed = match self.git(&fetch, 60).await {
            Ok(_) => {
                self.git(
                    &["for-each-ref", &format, "refs/remotes/origin", "refs/tags"],
                    30,
                )
                .await
            }
            Err(e) => Err(format!("fetch: {e}")),
        };
        // Only a listing that worked is compared: an empty one reads as every
        // ref deleted.
        let listing = match listed {
            Ok(l) => l,
            Err(e) => {
                let mut inner = self.lock();
                inner.watcher.fetch_error = Some(e);
                self.publish(&inner);
                return;
            }
        };
        let now_heads = watch::parse_refs(&listing);
        let (saved, changes) = {
            let mut inner = self.lock();
            inner.watcher.fetched_at = Some(now());
            inner.watcher.fetch_error = None;
            if !inner.state.first_start_done {
                // The first start: a baseline, nothing is built.
                inner.state.heads = now_heads;
                inner.state.first_start_done = true;
                self.save_state(&inner.state);
                self.publish(&inner);
                return;
            }
            let changes = watch::diff(Some(&inner.state.heads), &now_heads);
            (inner.state.heads.clone(), changes)
        };
        if changes.is_empty() {
            self.publish(&self.lock());
            return;
        }
        let mut pushed = Vec::new();
        let mut unread = Vec::new();
        for sha in watch::to_read(&changes, &self.settings.rules) {
            match self.read_head(&sha).await {
                Ok(p) => pushed.push(p),
                Err(e) => {
                    eprintln!("bana daemon: cannot read {sha}: {e}");
                    unread.push(sha);
                }
            }
        }
        // A head git could not read now is read again next time: its ref keeps
        // its old head, so it still differs then.
        let mut heads = now_heads;
        let changes: Vec<_> = changes
            .into_iter()
            .filter(|c| {
                let skip = c.head().is_some_and(|h| unread.iter().any(|u| u == h));
                if skip {
                    match saved.get(c.git_ref()) {
                        Some(old) => heads.insert(c.git_ref().to_string(), old.clone()),
                        None => heads.remove(c.git_ref()),
                    };
                }
                !skip
            })
            .collect();
        let mut unpin = Vec::new();
        {
            let mut inner = self.lock();
            let inner = &mut *inner;
            let built = built(&inner.records);
            let queue: Vec<Request> = inner
                .state
                .queue
                .iter()
                .filter_map(|id| inner.records.get(id).map(|r| r.request.clone()))
                .collect();
            let running = inner
                .running
                .as_ref()
                .and_then(|r| inner.records.get(&r.id))
                .map(|r| r.request.clone());
            let rules = &self.settings.rules;
            for a in watch::decide(&changes, rules, &pushed, &built, &queue, running.as_ref()) {
                match a {
                    Action::Enqueue { git_ref, sha, tier } => {
                        self.enqueue(
                            inner,
                            Request {
                                git_ref,
                                sha,
                                tier,
                                ..Request::default()
                            },
                            false,
                        );
                    }
                    Action::Replace { id, sha } => {
                        if let Some(r) = inner.records.get_mut(&id) {
                            r.request.sha = sha;
                            self.save(r);
                        }
                    }
                    Action::Drop { id, why } => {
                        eprintln!("bana daemon: build {id} dropped: {why}");
                        self.unqueue(inner, id);
                    }
                    Action::SupersedeRunning { id, reason } => {
                        if let Some(r) = inner.running.as_ref().filter(|r| r.id == id) {
                            let _ = r.cancel.send(reason);
                        }
                    }
                    Action::SkippedMarker { git_ref, sha } => {
                        skipped(&mut inner.state, git_ref, sha, "skip marker")
                    }
                    Action::NoWorkflow { git_ref, sha } => {
                        skipped(&mut inner.state, git_ref, sha, "no workflow")
                    }
                    Action::Forget { git_ref } => {
                        inner.state.green.remove(&git_ref);
                        unpin.push(watch::green_pin(&git_ref));
                    }
                }
            }
            inner.state.heads = heads;
            self.save_state(&inner.state);
            self.publish(inner);
        }
        for pin in unpin {
            let _ = self.git(&["update-ref", "-d", &pin], 30).await;
        }
        self.run.notify_one();
    }

    /// What the rules need of a new head: its message, and whether the
    /// workflow is in it. An error means git could not read it now.
    async fn read_head(&self, sha: &str) -> Result<Pushed, String> {
        let message = self.git(&["log", "-1", "--format=%B", sha], 30).await?;
        let wf = format!(".github/workflows/{}", self.settings.workflow);
        let tree = self
            .git(&["ls-tree", "--name-only", sha, "--", &wf], 30)
            .await?;
        Ok(Pushed {
            sha: sha.to_string(),
            message,
            workflow: !tree.trim().is_empty(),
        })
    }

    // ---- the runner --------------------------------------------------------------

    async fn runner(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        loop {
            if *stop.borrow() {
                return;
            }
            match self.next_build().await {
                Some((id, cancel)) => self.run_build(id, cancel).await,
                None => tokio::select! {
                    _ = self.run.notified() => {}
                    _ = tokio::time::sleep(self.settings.recheck) => {}
                    _ = stop.changed() => return,
                },
            }
        }
    }

    /// Marks the queue as waiting, and why.
    fn hold(&self, why: Option<&str>) {
        let mut inner = self.lock();
        inner.waiting = why.map(String::from);
        self.publish(&inner);
    }

    /// The next build, once its gates are open (not paused, Docker up, the lock
    /// ours): it is now the running one, and the lock is held for it.
    async fn next_build(&self) -> Option<(u64, mpsc::UnboundedReceiver<String>)> {
        let id = {
            let mut inner = self.lock();
            let built = built(&inner.records);
            let done: Vec<u64> = inner
                .state
                .queue
                .iter()
                .copied()
                .filter(|id| {
                    inner
                        .records
                        .get(id)
                        .is_none_or(|r| r.request.already_built(&built))
                })
                .collect();
            for id in &done {
                self.unqueue(&mut inner, *id);
            }
            if !done.is_empty() {
                self.save_state(&inner.state);
            }
            let Some(&id) = inner.state.queue.first() else {
                (inner.waiting, inner.watcher.lock_holder) = (None, None);
                self.publish(&inner);
                return None;
            };
            if inner.stopping {
                return None;
            }
            if inner.state.paused {
                inner.waiting = Some("paused".into());
                self.publish(&inner);
                return None;
            }
            id
        };
        let docker = self
            .output(&self.settings.docker, &["info"], &self.settings.dir, 30)
            .await
            .is_ok_and(|o| o.status.success());
        self.lock().watcher.docker = docker;
        if !docker {
            self.hold(Some("waiting for Docker"));
            return None;
        }
        let label = {
            let inner = self.lock();
            let r = &inner.records.get(&id)?.request;
            format!(
                "bana daemon: build {id}, {} {} ({})",
                watch::short_ref(&r.git_ref),
                r.sha.get(..7).unwrap_or(&r.sha),
                self.settings.prefix
            )
        };
        if let Err(holder) = self.take_lock(&label).await {
            self.lock().watcher.lock_holder = Some(holder);
            self.hold(Some("waiting for your bana ci"));
            return None;
        }
        let mut inner = self.lock();
        inner.watcher.lock_holder = None;
        inner.waiting = None;
        // It may have been cancelled, or the daemon paused, meanwhile.
        if inner.state.queue.first() != Some(&id) || inner.state.paused || inner.stopping {
            drop(inner);
            self.release_lock(&[std::process::id()]);
            self.run.notify_one();
            return None;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        inner.state.queue.remove(0);
        inner.running = Some(Running {
            id,
            since: Instant::now(),
            cancel: tx,
        });
        self.save_state(&inner.state);
        self.publish(&inner);
        Some((id, rx))
    }

    async fn run_build(&self, id: u64, cancel: mpsc::UnboundedReceiver<String>) {
        match self.prepare(id).await {
            Ok(list) => self.act(id, list, cancel).await,
            Err(why) => self.could_not_start(id, &why),
        }
        let _ = std::fs::remove_file(self.build_dir(id).join("secrets"));
        let pid = self.lock().records.get(&id).and_then(|r| r.pid);
        self.release_lock(&[std::process::id(), pid.unwrap_or(0)]);
        let mut inner = self.lock();
        inner.running = None;
        self.prune(&mut inner);
        self.save_state(&inner.state);
        self.publish(&inner);
        drop(inner);
        self.post.notify_one();
    }

    /// Everything before act: the checkout, the job list, the event and the
    /// secrets. An error is why the build could not start.
    async fn prepare(&self, id: u64) -> Result<Vec<(u32, String)>, String> {
        let s = &self.settings;
        let req = self
            .lock()
            .records
            .get(&id)
            .map(|r| r.request.clone())
            .ok_or("no such build")?;
        let dir = self.build_dir(id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let short = req.sha.get(..7).unwrap_or(&req.sha).to_string();
        self.git(
            &["checkout", "--quiet", "--force", "--detach", &req.sha],
            120,
        )
        .await
        .map_err(|e| format!("checkout {short}: {e}"))?;
        self.git(&["clean", "-ffdxq"], 120).await?;
        if self.src().join(".gitmodules").exists() {
            let cred = self.credentials();
            let mut args: Vec<&str> = cred.iter().map(String::as_str).collect();
            args.extend([
                "-c",
                "url.https://github.com/.insteadOf=git@github.com:",
                "submodule",
                "update",
                "--init",
                "--recursive",
                "--force",
            ]);
            self.git(&args, 600)
                .await
                .map_err(|e| format!("submodules: {e}"))?;
        }

        let script = s.script.to_string_lossy().into_owned();
        let list = Command::new(&s.bash)
            .args([script.as_str(), "ci", "--list"])
            .env_clear()
            .envs(s.child_env(id))
            .current_dir(&dir)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let list = match tokio::time::timeout(Duration::from_secs(120), list).await {
            Err(_) => return Err("bana ci --list took longer than 120 s".into()),
            Ok(Err(e)) => return Err(format!("{}: {e}", s.bash)),
            Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            Ok(Ok(o)) => return Err(failure(&o)),
        };
        std::fs::write(dir.join("jobs.txt"), &list).map_err(|e| e.to_string())?;

        let green = self.lock().state.green.clone();
        let mut before = watch::before_for(&req, &green);
        let mut forced = false;
        if !watch::is_zeros(&before) {
            let o = self
                .output(
                    &s.git,
                    &["merge-base", "--is-ancestor", &before, &req.sha],
                    &self.src(),
                    30,
                )
                .await?;
            match o.status.code() {
                Some(0) => {}
                Some(1) => forced = true,
                // `before` is not in the clone: diff as for a new ref.
                _ => before = watch::zeros(&req.sha),
            }
        }
        let text = self
            .git(
                &[
                    "log",
                    "-1",
                    &format!("--format={}", watch::COMMIT_FORMAT),
                    &req.sha,
                ],
                30,
            )
            .await?;
        let head =
            watch::parse_commit(&text).ok_or_else(|| format!("cannot read commit {short}"))?;
        let default_branch = self
            .git(
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
                30,
            )
            .await
            .map(|b| b.trim().trim_start_matches("origin/").to_string())
            .unwrap_or_default();
        let project = Project {
            repo: s.repo.clone(),
            default_branch,
            login: s.login.clone(),
            tier_input: s.tier_input.clone(),
        };
        let event = watch::event_payload(&project, &req, &before, forced, &head);
        write_json(&dir.join("event.json"), &event).map_err(|e| e.to_string())?;

        let token = match s.token {
            JobToken::Gh => self
                .output(&s.gh, &["auth", "token"], &dir, 30)
                .await
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default(),
            JobToken::Empty => String::new(),
        };
        write_secret(&dir.join("secrets"), &format!("GITHUB_TOKEN={token}\n"))
            .map_err(|e| format!("secrets: {e}"))?;

        let mut inner = self.lock();
        if let Some(r) = inner.records.get_mut(&id) {
            r.request.before = Some(before);
            r.forced = forced;
            self.save(r);
        }
        Ok(actlog::parse_list(&list))
    }

    fn could_not_start(&self, id: u64, why: &str) {
        eprintln!("bana daemon: build {id} could not start: {why}");
        let mut inner = self.lock();
        let inner = &mut *inner;
        let Some(rec) = inner.records.get_mut(&id) else {
            return;
        };
        let old = rec.build.clone();
        if rec.build.state == BuildState::Queued {
            rec.build = Build::new(&[], now());
        }
        rec.build.last_error = Some(why.to_string());
        rec.build.finish(Some(1), None, now());
        let updates = actlog::status_updates(&old, &rec.build, &self.report(rec));
        want(rec, updates, &mut inner.seq);
        self.save(rec);
    }

    /// Runs act (`bana ci`), streams its log into the build, and ends it.
    async fn act(
        &self,
        id: u64,
        list: Vec<(u32, String)>,
        mut cancel: mpsc::UnboundedReceiver<String>,
    ) {
        let s = &self.settings;
        let dir = self.build_dir(id);
        let tier = {
            let mut inner = self.lock();
            let inner = &mut *inner;
            let Some(rec) = inner.records.get_mut(&id) else {
                return;
            };
            let old = rec.build.clone();
            rec.build = Build::new(&list, now());
            if let Ok(reason) = cancel.try_recv() {
                if inner.stopping {
                    // The daemon stops before act ran: it is queued again as it was.
                    rec.build = Build::default();
                    self.save(rec);
                    inner.state.queue.insert(0, id);
                    self.save_state(&inner.state);
                    return;
                }
                // Cancelled while it was being checked out: nothing ran, nothing is posted.
                rec.build.finish(None, Some(&reason), now());
                self.save(rec);
                return;
            }
            let updates = actlog::status_updates(&old, &rec.build, &self.report(rec));
            want(rec, updates, &mut inner.seq);
            self.save(rec);
            let tier = rec.request.tier.clone();
            self.publish(inner);
            tier
        };
        self.post.notify_one();

        let port = free_port().unwrap_or(34567);
        let mut cmd = Command::new(&s.bash);
        cmd.arg(&s.script).arg("ci");
        if !tier.is_empty() {
            cmd.arg(&tier);
        }
        cmd.args(["--event", "event.json", "--"])
            .args(s.act_flags(id, port))
            .env_clear()
            .envs(s.child_env(id))
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // A daemon started in the background (`&`) ignores SIGINT, and act
        // would inherit that: the cancel's SIGINT must reach it.
        // SAFETY: only signal(), which is async-signal-safe, between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                Ok(())
            });
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let mut inner = self.lock();
                let inner = &mut *inner;
                if let Some(rec) = inner.records.get_mut(&id) {
                    let old = rec.build.clone();
                    rec.build.last_error = Some(format!("{}: {e}", s.bash));
                    rec.build.finish(Some(1), None, now());
                    let updates = actlog::status_updates(&old, &rec.build, &self.report(rec));
                    want(rec, updates, &mut inner.seq);
                    self.save(rec);
                }
                return;
            }
        };
        let pid = child.id().unwrap_or(0);
        // `bana ci` execs act: from here the lock is act's, so it stays held
        // while act runs even if the daemon dies.
        let start = started(&s.path, pid).await;
        let label = self.lock_label();
        self.write_owner(pid, &start, &label);
        {
            let mut inner = self.lock();
            if let Some(rec) = inner.records.get_mut(&id) {
                (rec.pid, rec.pid_start, rec.port) = (Some(pid), Some(start), Some(port));
                self.save(rec);
            }
        }
        if cfg!(target_os = "macos") {
            // Keeps the Mac from idle sleep while act runs; exits with act.
            let _ = Command::new(&s.caffeinate)
                .args(["-i", "-w", &pid.to_string()])
                .env("PATH", &s.path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }

        let (tx, mut lines) = mpsc::unbounded_channel();
        let readers = [
            tokio::spawn(read_lines(child.stdout.take(), tx.clone())),
            tokio::spawn(read_lines(child.stderr.take(), tx)),
        ];
        let mut log = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("act.jsonl"))
        {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("bana daemon: act.jsonl: {e}");
                None
            }
        };
        let deadline = tokio::time::Instant::now() + s.timeout;
        let far = deadline + Duration::from_secs(86_400 * 365);
        let (mut exit, mut drain_until) = (None, None);
        // The cancel ladder: the rung reached (1: SIGINT sent, 2: the second
        // one, 3: the tree killed), and when the next one is due.
        let mut ladder: Option<(u8, tokio::time::Instant)> = None;
        loop {
            tokio::select! {
                line = lines.recv() => match line {
                    Some(l) => self.fold_line(id, log.as_mut(), &l),
                    None => break,
                },
                status = child.wait(), if exit.is_none() => {
                    exit = Some(status.ok().and_then(|s| s.code()));
                    let _ = std::fs::remove_file(dir.join("secrets"));
                    // Something act left behind may hold its pipes open.
                    drain_until = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                }
                _ = tokio::time::sleep_until(drain_until.unwrap_or(far)), if drain_until.is_some() => break,
                Some(reason) = cancel.recv(), if exit.is_none() => match ladder {
                    None => {
                        self.begin_cancel(id, log.as_mut(), &reason, pid);
                        ladder = Some((1, tokio::time::Instant::now() + self.waits()[0]));
                    }
                    // The daemon stops during a cancel: the short waits from here.
                    Some((rung @ 1..=2, due)) if self.lock().stopping => {
                        let short = tokio::time::Instant::now() + s.ladder_short[rung as usize - 1];
                        ladder = Some((rung, due.min(short)));
                    }
                    Some(_) => {}
                },
                _ = tokio::time::sleep_until(ladder.map_or(far, |(_, due)| due)), if exit.is_none() && ladder.is_some_and(|(r, _)| r < 3) => {
                    let rung = ladder.map_or(0, |(r, _)| r);
                    if rung == 1 {
                        // act's force cancel: it stops waiting for its steps.
                        eprintln!("bana daemon: build {id}: act is still running; a second SIGINT");
                        sweep::signal(pid, libc::SIGINT);
                        ladder = Some((2, tokio::time::Instant::now() + self.waits()[1]));
                    } else {
                        eprintln!("bana daemon: build {id}: act is still running; killing it");
                        self.kill_tree(pid).await;
                        ladder = Some((3, far));
                    }
                }
                _ = tokio::time::sleep_until(deadline), if ladder.is_none() && exit.is_none() => {
                    let reason = format!("timed out after {}", minutes(s.timeout));
                    self.begin_cancel(id, log.as_mut(), &reason, pid);
                    ladder = Some((1, tokio::time::Instant::now() + self.waits()[0]));
                }
            }
        }
        for r in &readers {
            r.abort();
        }
        let code = match exit {
            Some(code) => code,
            None => child.wait().await.ok().and_then(|s| s.code()),
        };
        let _ = std::fs::remove_file(dir.join("secrets"));
        // Containers are left behind only when act did not end on its own.
        self.sweep(id, ladder.is_some() || code.is_none()).await;

        let pin = {
            let mut inner = self.lock();
            let inner = &mut *inner;
            let Some(rec) = inner.records.get_mut(&id) else {
                return;
            };
            if inner.stopping {
                // Nothing final is posted: the next start retries it, or ends it.
                self.save(rec);
                return;
            }
            let old = rec.build.clone();
            let state = rec.build.finish(code, None, now());
            let updates = actlog::status_updates(&old, &rec.build, &self.report(rec));
            want(rec, updates, &mut inner.seq);
            self.save(rec);
            let (git_ref, sha) = (rec.request.git_ref.clone(), rec.request.sha.clone());
            (state == BuildState::Success).then(|| {
                inner.state.green.insert(git_ref.clone(), sha.clone());
                self.save_state(&inner.state);
                (git_ref, sha)
            })
        };
        if let Some((git_ref, sha)) = pin {
            if let Err(e) = self
                .git(&["update-ref", &watch::green_pin(&git_ref), &sha], 30)
                .await
            {
                eprintln!("bana daemon: pin {git_ref}: {e}");
            }
        }
    }

    /// The ladder's waits: short while the daemon stops.
    fn waits(&self) -> [Duration; 2] {
        if self.lock().stopping {
            self.settings.ladder_short
        } else {
            self.settings.ladder
        }
    }

    /// SIGKILL to act and to all it runs, found first (the parent links go
    /// once act is gone).
    async fn kill_tree(&self, pid: u32) {
        let tree = sweep::descendants(&self.settings.path, pid).await;
        for p in std::iter::once(pid).chain(tree) {
            sweep::signal(p, libc::SIGKILL);
        }
    }

    /// After a build: the processes with its marker get SIGTERM, then SIGKILL
    /// after the grace; with `docker`, the containers labelled for this prefix
    /// and their volumes go (under the lock nothing else of ours runs); act's
    /// host workspaces go; so does the secrets file.
    async fn sweep(&self, id: u64, docker: bool) {
        let s = &self.settings;
        let marker = format!("{}-{id}", s.prefix);
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        let me = std::process::id();
        let find = || async {
            let mut pids = sweep::marker_pids(&s.path, uid, &marker).await;
            pids.retain(|p| *p != me);
            pids
        };
        let pids = find().await;
        if !pids.is_empty() {
            eprintln!("bana daemon: build {id}: stopping {pids:?}, left with its marker");
            for p in &pids {
                sweep::signal(*p, libc::SIGTERM);
            }
            let until = Instant::now() + s.grace;
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let left = find().await;
                if left.is_empty() {
                    break;
                }
                if Instant::now() >= until {
                    for p in &left {
                        sweep::signal(*p, libc::SIGKILL);
                    }
                    break;
                }
            }
        }
        if docker {
            match sweep::docker_sweep(&s.docker, &s.path, &s.prefix).await {
                Ok(0) => {}
                Ok(n) => eprintln!("bana daemon: build {id}: removed {n} job container(s)"),
                Err(e) => eprintln!("bana daemon: build {id}: docker sweep: {e}"),
            }
        }
        let gone = sweep::act_cache_sweep(&s.dir.join("act-cache"));
        if !gone.is_empty() {
            eprintln!("bana daemon: build {id}: removed act workspaces {gone:?}");
        }
        let _ = std::fs::remove_file(self.build_dir(id).join("secrets"));
    }

    /// One line act (or bana) printed: into act.jsonl, and into the build.
    fn fold_line(&self, id: u64, log: Option<&mut std::fs::File>, raw: &str) {
        if raw.trim().is_empty() {
            return;
        }
        let line = actlog::log_line(raw);
        if let Some(f) = log {
            let _ = writeln!(f, "{line}");
        }
        let ev = actlog::parse_line(&line);
        if ev == Event::Other {
            return;
        }
        self.fold(id, &ev);
    }

    fn fold(&self, id: u64, ev: &Event) {
        let mut inner = self.lock();
        let inner = &mut *inner;
        let Some(rec) = inner.records.get_mut(&id) else {
            return;
        };
        let old = rec.build.clone();
        rec.build.fold(ev, now());
        if rec.build == old {
            return;
        }
        // While the daemon stops, nothing more is wanted: the next start
        // decides what the build says.
        let updates = if inner.stopping {
            Vec::new()
        } else {
            actlog::status_updates(&old, &rec.build, &self.report(rec))
        };
        let jobs_moved = old.jobs.len() != rec.build.jobs.len()
            || old
                .jobs
                .iter()
                .zip(&rec.build.jobs)
                .any(|(a, b)| a.state != b.state);
        let post = !updates.is_empty();
        want(rec, updates, &mut inner.seq);
        if post || jobs_moved || rec.build.cancel_requested != old.cancel_requested {
            self.save(rec);
        }
        self.publish(inner);
        if post {
            self.post.notify_one();
        }
    }

    /// A cancel begins: the mark goes into act.jsonl (so jobs that end from now
    /// on were cancelled, also when read again), then act gets SIGINT, its
    /// graceful stop (post and always() steps run, --rm removes containers).
    /// act() climbs the rest of the ladder.
    fn begin_cancel(&self, id: u64, log: Option<&mut std::fs::File>, reason: &str, pid: u32) {
        if self
            .lock()
            .records
            .get(&id)
            .is_some_and(|r| r.build.cancel_requested.is_some())
        {
            return;
        }
        let mark = actlog::cancel_mark(reason);
        if let Some(f) = log {
            let _ = writeln!(f, "{mark}");
        }
        self.fold(id, &Event::Cancel(reason.to_string()));
        sweep::signal(pid, libc::SIGINT);
    }

    // ---- the lock ---------------------------------------------------------------

    fn lock_label(&self) -> String {
        let inner = self.lock();
        match inner
            .running
            .as_ref()
            .and_then(|r| inner.records.get(&r.id))
        {
            Some(r) => format!(
                "bana daemon: build {}, {} {} ({})",
                r.request.id,
                watch::short_ref(&r.request.git_ref),
                r.request.sha.get(..7).unwrap_or(&r.request.sha),
                self.settings.prefix
            ),
            None => format!("bana daemon ({})", self.settings.prefix),
        }
    }

    /// Takes ~/.bana/act.lock, as `bana ci` does (lib/act.sh act_lock): a live
    /// owner keeps it (the error is its label); a stale one is taken over.
    async fn take_lock(&self, label: &str) -> Result<(), String> {
        let lock = self.lock_dir();
        let _ = std::fs::create_dir_all(&self.settings.home);
        for _ in 0..5 {
            if std::fs::create_dir(&lock).is_ok() {
                let me = std::process::id();
                let start = started(&self.settings.path, me).await;
                self.write_owner(me, &start, label);
                return Ok(());
            }
            let read = || std::fs::read_to_string(lock.join("owner")).unwrap_or_default();
            let mut owner = read();
            if owner.is_empty() {
                // Its taker may be writing it.
                tokio::time::sleep(Duration::from_secs(1)).await;
                owner = read();
            }
            let mut f = owner.lines();
            let (pid, start, holder) = (
                f.next().and_then(|p| p.trim().parse::<u32>().ok()),
                f.next().unwrap_or("").to_string(),
                f.next().unwrap_or("").to_string(),
            );
            if let Some(pid) = pid {
                if alive(pid) && started(&self.settings.path, pid).await == start {
                    return Err(holder);
                }
            }
            // Stale: it goes, unless someone took it over meanwhile.
            if read() == owner {
                let _ = std::fs::remove_dir_all(&lock);
            }
        }
        Err(format!("cannot take {}", lock.display()))
    }

    /// The owner file: pid, its start time, a label; written whole (a
    /// temporary file, then a rename).
    fn write_owner(&self, pid: u32, start: &str, label: &str) {
        let lock = self.lock_dir();
        let tmp = lock.join(format!("owner.{}", std::process::id()));
        let r = std::fs::write(&tmp, format!("{pid}\n{start}\n{label}\n"))
            .and_then(|_| std::fs::rename(&tmp, lock.join("owner")));
        if let Err(e) = r {
            eprintln!("bana daemon: {}: {e}", lock.display());
        }
    }

    /// Removes the lock when its owner is gone (dead, or its pid reused): after
    /// a crash, an orphaned act held it until it was killed.
    async fn clear_stale_lock(&self) {
        let lock = self.lock_dir();
        let owner = std::fs::read_to_string(lock.join("owner")).unwrap_or_default();
        let mut f = owner.lines();
        let Some(pid) = f.next().and_then(|p| p.trim().parse::<u32>().ok()) else {
            return;
        };
        let start = f.next().unwrap_or("");
        if alive(pid) && started(&self.settings.path, pid).await == start {
            return;
        }
        if std::fs::read_to_string(lock.join("owner")).unwrap_or_default() == owner {
            eprintln!("bana daemon: removing a stale {}", lock.display());
            let _ = std::fs::remove_dir_all(&lock);
        }
    }

    /// Removes the lock if one of `pids` owns it.
    fn release_lock(&self, pids: &[u32]) {
        let lock = self.lock_dir();
        let owner = std::fs::read_to_string(lock.join("owner")).unwrap_or_default();
        let pid = owner
            .lines()
            .next()
            .and_then(|p| p.trim().parse::<u32>().ok());
        if pid.is_some_and(|p| p != 0 && pids.contains(&p)) {
            let _ = std::fs::remove_dir_all(&lock);
        }
    }

    // ---- the poster ---------------------------------------------------------------

    async fn poster(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        let mut wait = Duration::ZERO;
        loop {
            let pause = if wait.is_zero() {
                Duration::from_secs(3600)
            } else {
                wait
            };
            tokio::select! {
                _ = self.post.notified(), if wait.is_zero() => {}
                _ = self.post_now.notified() => {}
                _ = tokio::time::sleep(pause) => {}
                _ = stop.changed() => return,
            }
            wait = if self.post_all().await {
                Duration::ZERO
            } else if wait.is_zero() {
                self.settings.retry
            } else {
                (wait * 2).min(RETRY_MAX)
            };
        }
    }

    /// Posts what is waiting, oldest first; false when GitHub (or gh) said no.
    async fn post_all(&self) -> bool {
        loop {
            let next = {
                let inner = self.lock();
                to_post(&inner).first().and_then(|(id, context)| {
                    let rec = inner.records.get(id)?;
                    let p = rec.statuses.get(context)?.clone();
                    let url = inner.state.target_url_ok.then(|| {
                        let mut u = format!("http://127.0.0.1:{}/#build={id}", self.settings.port);
                        if let Some(key) = context
                            .strip_prefix(&rec.context_prefix)
                            .and_then(|k| k.strip_prefix('/'))
                        {
                            u += &format!("&job={}", url_encode(key));
                        }
                        u
                    });
                    Some((*id, context.clone(), rec.request.sha.clone(), p, url))
                })
            };
            let Some((id, context, sha, p, url)) = next else {
                let mut inner = self.lock();
                if inner.watcher.post_error.take().is_some() {
                    self.publish(&inner);
                }
                return true;
            };
            let result = self.post_one(&sha, &context, &p, url.as_deref()).await;
            let mut inner = self.lock();
            let inner = &mut *inner;
            match result {
                Ok(()) => {
                    if let Some(rec) = inner.records.get_mut(&id) {
                        if let Some(q) = rec.statuses.get_mut(&context).filter(|q| q.seq == p.seq) {
                            (q.posted, q.error) = (true, None);
                        }
                        self.save(rec);
                    }
                    self.publish(inner);
                }
                Err(e) if url.is_some() && e.contains("422") && e.contains("target_url") => {
                    // GitHub does not take a loopback link: post without one from now on.
                    eprintln!("bana daemon: GitHub refused target_url; statuses go without it");
                    inner.state.target_url_ok = false;
                    self.save_state(&inner.state);
                }
                Err(e) if e.contains("(HTTP 422)") => {
                    // GitHub will never take this one (no such commit on GitHub,
                    // too many statuses): it is given up, or it would hold back
                    // every status after it.
                    eprintln!("bana daemon: build {id}: {context} not posted: {e}");
                    if let Some(rec) = inner.records.get_mut(&id) {
                        if let Some(q) = rec.statuses.get_mut(&context).filter(|q| q.seq == p.seq) {
                            (q.posted, q.error) = (true, Some(e));
                        }
                        self.save(rec);
                    }
                    self.publish(inner);
                }
                Err(e) => {
                    if let Some(q) = inner
                        .records
                        .get_mut(&id)
                        .and_then(|r| r.statuses.get_mut(&context))
                    {
                        q.error = Some(e.clone());
                    }
                    inner.watcher.post_error = Some(e);
                    self.publish(inner);
                    return false;
                }
            }
        }
    }

    async fn post_one(
        &self,
        sha: &str,
        context: &str,
        p: &Posting,
        url: Option<&str>,
    ) -> Result<(), String> {
        let s = &self.settings;
        let status = Status {
            context: context.to_string(),
            state: p.state,
            description: p.description.clone(),
        };
        post_status(&s.gh, &s.path, &s.dir, &s.repo, sha, &status, url).await
    }

    // ---- pruning ------------------------------------------------------------------

    /// Keeps the newest builds, and a week of artifacts.
    fn prune(&self, inner: &mut Inner) {
        let busy = |id: &u64| {
            inner.state.queue.contains(id) || inner.running.as_ref().is_some_and(|r| r.id == *id)
        };
        let old: Vec<u64> = inner
            .records
            .keys()
            .rev()
            .filter(|id| !busy(id))
            .skip(KEEP_BUILDS)
            .copied()
            .collect();
        for id in old {
            inner.records.remove(&id);
            let _ = std::fs::remove_dir_all(self.build_dir(id));
        }
        let week_ago = now() - KEEP_ARTIFACTS;
        for r in inner.records.values() {
            if r.build.ended_at.is_some_and(|t| t < week_ago) {
                let _ = std::fs::remove_dir_all(self.build_dir(r.request.id).join("artifacts"));
            }
        }
    }
}

/// Records the statuses a build wants now; each gets the next sequence number.
fn want(rec: &mut Record, updates: Vec<Status>, seq: &mut u64) {
    for s in updates {
        *seq += 1;
        rec.statuses.insert(
            s.context,
            Posting {
                state: s.state,
                description: s.description,
                seq: *seq,
                posted: false,
                error: None,
            },
        );
    }
}

/// Wants each status the build has now that it did not want already.
fn want_now(rec: &mut Record, report: &Report, seq: &mut u64) {
    let updates = actlog::statuses(&rec.build, report)
        .into_iter()
        .filter(|s| {
            rec.statuses
                .get(&s.context)
                .is_none_or(|p| p.state != s.state || p.description != s.description)
        })
        .collect();
    want(rec, updates, seq);
}

/// The statuses to post, in the order they were wanted: (build, context). A
/// newer build of the same commit that wants the same context wins; the
/// older one's is never posted.
fn to_post(inner: &Inner) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, u64, String)> = Vec::new();
    for rec in inner.records.values() {
        for (context, p) in rec.statuses.iter().filter(|(_, p)| !p.posted) {
            let newer = inner
                .records
                .range(rec.request.id + 1..)
                .any(|(_, r)| r.request.sha == rec.request.sha && r.statuses.contains_key(context));
            if !newer {
                out.push((p.seq, rec.request.id, context.clone()));
            }
        }
    }
    out.sort();
    out.into_iter().map(|(_, id, c)| (id, c)).collect()
}

fn built(records: &BTreeMap<u64, Record>) -> Built {
    let mut b = Built::default();
    for r in records.values() {
        b.add(&r.request.sha, &r.request.tier, r.build.state);
    }
    b
}

fn skipped(state: &mut State, git_ref: String, sha: String, why: &str) {
    state.skipped.push(Skipped {
        git_ref,
        sha,
        why: why.into(),
        at: now(),
    });
    let extra = state.skipped.len().saturating_sub(KEEP_SKIPPED);
    state.skipped.drain(..extra);
}

async fn read_lines<R: AsyncRead + Unpin>(r: Option<R>, tx: mpsc::UnboundedSender<String>) {
    let Some(r) = r else { return };
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if tx.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
                    return;
                }
            }
        }
    }
}

/// When `pid` started, as `ps -o lstart=` says it with single spaces (the
/// lock's owner file; lib/act.sh act_started). Empty when it is gone.
async fn started(path: &str, pid: u32) -> String {
    let o = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("PATH", path)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .await;
    match o {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    (r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
        && !sweep::zombie(pid as u32)
}

/// Locks `<dir>/daemon.lock` (flock, which the kernel releases when the process
/// ends): a second daemon for the same directory would take the first one's
/// running build for an orphan and kill it. A process forking a child shares
/// the lock until the child execs, so a lock just let go is tried for a second.
async fn claim(dir: &Path) -> Result<std::fs::File, String> {
    use std::os::fd::AsRawFd;
    let path = dir.join("daemon.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    for _ in 0..20 {
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(f);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(format!("another bana daemon runs in {}", dir.display()))
}

/// A build interrupted by a restart runs again when nothing moved on: its
/// first attempt, and its commit still its ref's head (a tag's, or one asked
/// for by hand, always).
fn retry_eligible(r: &Request, heads: &Heads) -> bool {
    r.attempt < 2
        && (matches!(r.trigger, Trigger::Manual | Trigger::Rerun)
            || watch::is_tag(&r.git_ref)
            || heads.get(&r.git_ref) == Some(&r.sha))
}

/// Posts one commit status to `repo`'s `sha` with the GitHub CLI (`gh api`),
/// as the poster does. An error is what went wrong; it has `target_url` in it
/// when GitHub's answer named the link. `bana-manager post-status` runs this
/// alone: bana's own CI asks GitHub whether it takes a loopback link.
pub async fn post_status(
    gh: &str,
    path: &str,
    cwd: &Path,
    repo: &str,
    sha: &str,
    status: &Status,
    url: Option<&str>,
) -> Result<(), String> {
    let api = format!("repos/{repo}/statuses/{sha}");
    let fields = [
        format!("state={}", status.state.as_str()),
        format!("context={}", status.context),
        format!("description={}", status.description),
    ];
    let mut args = vec!["api", "-X", "POST", api.as_str()];
    for f in &fields {
        args.extend(["-f", f.as_str()]);
    }
    let url = url.map(|u| format!("target_url={u}"));
    if let Some(u) = &url {
        args.extend(["-f", u.as_str()]);
    }
    let o = output(gh, &args, cwd, path, 30).await?;
    if o.status.success() {
        return Ok(());
    }
    // gh prints GitHub's answer on stdout, and its own note on stderr.
    let body = String::from_utf8_lossy(&o.stdout);
    let e = failure(&o);
    Err(
        if body.contains("target_url") && !e.contains("target_url") {
            format!("{e} (target_url)")
        } else {
            e
        },
    )
}

/// `program args` in `cwd`, with `path` as PATH: its output, unless it
/// could not run, or took longer than `secs`.
async fn output(
    program: &str,
    args: &[&str],
    cwd: &Path,
    path: &str,
    secs: u64,
) -> Result<std::process::Output, String> {
    let run = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(secs), run).await {
        Err(_) => Err(format!("{program} took longer than {secs} s")),
        Ok(Err(e)) => Err(format!("{program}: {e}")),
        Ok(Ok(o)) => Ok(o),
    }
}

/// A port nobody listens on now, for act's artifact server (its default is
/// fatal when taken, and 0 makes act announce ':0').
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("0.0.0.0:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .ok()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `120m`, or `90s` under a minute.
fn minutes(d: Duration) -> String {
    match d.as_secs() {
        s if s < 60 => format!("{s}s"),
        s => format!("{}m", s / 60),
    }
}

/// What a program said when it failed: the last line of its stderr (without
/// colours), else its exit.
fn failure(o: &std::process::Output) -> String {
    let e = String::from_utf8_lossy(&o.stderr);
    let Some(last) = e.lines().map(str::trim).rfind(|l| !l.is_empty()) else {
        return match o.status.code() {
            Some(n) => format!("exited with {n}"),
            None => "killed".into(),
        };
    };
    let msg = match actlog::parse_line(last) {
        Event::Text { msg, .. } => msg,
        _ => last.to_string(),
    };
    actlog::cut(&msg, 300)
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Percent-encoding for a job key in the page's URL fragment.
fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// JSON, whole or not at all: a temporary file, synced, then renamed.
fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let mut text = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    text.push(b'\n');
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&text)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}

/// A file only its owner can read, made new.
fn write_secret(path: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())
}

/// server.rs's tests use its project and stand-ins too.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command as Std;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/act");

    /// Stand-in `bana`: `ci --list` prints the fixture's `act -l`; `ci …`
    /// records its arguments, environment, secrets and pid, waits while `hold`
    /// exists, then replays the fixture act printed (named by the commit's
    /// `fixture` file) with small delays. SIGINT ends it, unless the `mode`
    /// file says `stubborn` (the first SIGINT is ignored) or `deaf` (all are).
    /// With `orphans`, it first leaves two sleeps that are not its children
    /// (setsid, nohup), carrying the marker, and records their pids.
    const BANA: &str = r#"ctl='CTL'; fx='FX'
[[ $1 == ci ]] || exit 2
shift
f=$(cat "$BANA_PROJECT_ROOT/fixture")
if [[ $1 == --list ]]; then
  grep -v '^time=' "$fx/$f.list"
  grep '^time=' "$fx/$f.list" >&2
  exit 0
fi
b=${BANA_BUILD##*-}
printf '%s\n' "$@" >"$ctl/argv.$b"
env >"$ctl/env.$b"
ls -l secrets | cut -c1-10 >"$ctl/secrets-mode.$b"
cat secrets >"$ctl/secrets.$b"
cp event.json "$ctl/event.$b"
mode=$(cat "$ctl/mode" 2>/dev/null)
n=0
case $mode in
  *stubborn*) trap 'n=$((n+1)); echo "bana: SIGINT $n" >&2; [[ $n -lt 2 ]] || exit 130' INT ;;
  *deaf*) trap 'echo "bana: SIGINT ignored" >&2' INT ;;
  *) trap 'echo "bana: interrupted" >&2; exit 130' INT ;;
esac
if [[ $mode == *orphans* ]]; then
  s=$(command -v setsid || true)
  ($s sleep 1000 </dev/null >/dev/null 2>&1 & echo $! >"$ctl/orphan-setsid.$b")
  (nohup sleep 1000 </dev/null >/dev/null 2>&1 & echo $! >"$ctl/orphan-nohup.$b")
fi
echo $$ >"$ctl/pid.$b"
while [[ -e $ctl/hold ]]; do sleep 0.05; done
sleep 0.2
while IFS= read -r line; do
  case $line in '{'*) printf '%s\n' "$line" ;; *) printf '%s\n' "$line" >&2 ;; esac
  sleep 0.01
done <"$fx/$f.jsonl"
case $f in fail | syntax) exit 1 ;; esac
exit 0
"#;

    /// Stand-in `gh`: answers `auth token` and logs each `api` call (its
    /// arguments, tab-separated). `gh-down` makes calls fail; `gh-no-url`
    /// refuses a target_url as GitHub would (422); `gh-422` refuses statuses
    /// for the commit it names.
    const GH: &str = "#!/bin/sh
ctl='CTL'
if [ \"$1 $2\" = 'auth token' ]; then echo gho_fromgh; exit 0; fi
if [ -e \"$ctl/gh-down\" ]; then echo 'error connecting to api.github.com' >&2; exit 1; fi
if [ -e \"$ctl/gh-422\" ]; then case \"$*\" in *\"$(cat \"$ctl/gh-422\")\"*)
  echo 'gh: No commit found for SHA: x (HTTP 422)' >&2; exit 1 ;; esac; fi
case \"$*\" in *target_url=*)
  if [ -e \"$ctl/gh-no-url\" ]; then
    echo '{\"message\":\"Validation Failed\",\"errors\":[{\"resource\":\"Status\",\"field\":\"target_url\",\"code\":\"invalid\"}]}'
    echo 'gh: Validation Failed (HTTP 422)' >&2
    exit 1
  fi ;;
esac
line=
for a in \"$@\"; do line=\"$line$a\t\"; done
printf '%s\\n' \"$line\" >>\"$ctl/gh.log\"
echo '{}'
";

    /// Stand-in `docker`: `docker-down` makes it fail. It logs each call but
    /// `info`, and `ps` lists the `containers` file.
    const DOCKER: &str = "#!/bin/sh
[ -e 'CTL/docker-down' ] && { echo 'Cannot connect to the Docker daemon' >&2; exit 1; }
[ \"$1\" = info ] && exit 0
echo \"$*\" >>'CTL/docker.log'
[ \"$1\" = ps ] && cat 'CTL/containers' 2>/dev/null
exit 0
";

    /// git, except that `log` fails while `git-log-fails` exists.
    const GIT: &str = "#!/bin/sh
if [ \"$1\" = log ] && [ -e 'CTL/git-log-fails' ]; then echo 'fatal: flaky' >&2; exit 1; fi
exec git \"$@\"
";

    /// A project on a local origin, a clone of it where the daemon keeps
    /// one (`<dir>/src`), and the stand-ins.
    pub(crate) struct Project {
        /// Its own, so one test's sweep never meets another's builds.
        prefix: String,
        root: PathBuf,
        ctl: PathBuf,
        work: PathBuf,
        dir: PathBuf,
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let o = Std::new("git")
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
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    impl Project {
        pub(crate) fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("bana-daemon-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let (ctl, bin) = (root.join("ctl"), root.join("bin"));
            for d in [&ctl, &bin, &root.join("h")] {
                std::fs::create_dir_all(d).unwrap();
            }
            let ctl_s = ctl.to_string_lossy();
            for (file, body) in [("bana", BANA), ("gh", GH), ("docker", DOCKER), ("git", GIT)] {
                let path = bin.join(file);
                std::fs::write(&path, body.replace("CTL", &ctl_s).replace("FX", FIXTURES)).unwrap();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
            git(&root, &["clone", "-q", "origin.git", "work"]);
            let dir = root.join("home/p");
            let p = Self {
                prefix: format!("{name}{}", std::process::id()),
                work: root.join("work"),
                root,
                ctl,
                dir,
            };
            p.commit("pass", "the start");
            p.push("main");
            std::fs::create_dir_all(&p.dir).unwrap();
            let origin = format!("file://{}", p.root.join("origin.git").display());
            git(&p.dir, &["clone", "-q", &origin, "src"]);
            git(
                &p.dir.join("src"),
                &["remote", "set-head", "origin", "--auto"],
            );
            p
        }

        fn settings(&self, extra: &str) -> Settings {
            let bin = self.root.join("bin");
            let text = format!(
                "# as bana daemon install writes it\nrepo = o/r\nprefix = {}\ntiers = quick nightly\nhost = t\nlogin = me\nhome = {}\npath = {}\ngit = {b}/git\ngh = {b}/gh\ndocker = {b}/docker\nscript = {b}/bana\ndaemon.poll = 3600\n{extra}",
                self.prefix,
                self.root.join("bana-home").display(),
                std::env::var("PATH").unwrap(),
                b = bin.display(),
            );
            let env = [
                ("HOME", self.root.join("h").to_string_lossy().into_owned()),
                ("USER", "me".into()),
                ("LANG", "C".into()),
                ("PATH", "/nowhere".into()),
                ("GITHUB_TOKEN", "ghp_leak".into()),
                ("SSH_AUTH_SOCK", "/tmp/agent".into()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
            let mut s = Settings::parse(&text, &self.dir, env).unwrap();
            (s.retry, s.recheck) = (Duration::from_millis(50), Duration::from_millis(50));
            s.grace = Duration::from_millis(500);
            s.ladder = [Duration::from_millis(400); 2];
            s.ladder_short = [Duration::from_millis(200); 2];
            s
        }

        /// A commit whose `bana ci` replays `fixture`.
        pub(crate) fn commit(&self, fixture: &str, message: &str) -> String {
            let wf = self.work.join(".github/workflows");
            std::fs::create_dir_all(&wf).unwrap();
            std::fs::write(wf.join("ci.yml"), "on: workflow_dispatch\n").unwrap();
            std::fs::write(self.work.join("fixture"), fixture).unwrap();
            std::fs::write(self.work.join("change"), message).unwrap();
            git(&self.work, &["add", "-A"]);
            git(&self.work, &["commit", "-q", "-m", message]);
            git(&self.work, &["rev-parse", "HEAD"])
        }

        pub(crate) fn push(&self, branch: &str) {
            let to = format!("HEAD:refs/heads/{branch}");
            git(&self.work, &["push", "-q", "--force", "origin", &to]);
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.ctl.join(name)).unwrap_or_default()
        }

        fn set(&self, flag: &str, on: bool) {
            if on {
                std::fs::write(self.ctl.join(flag), "").unwrap();
            } else {
                let _ = std::fs::remove_file(self.ctl.join(flag));
            }
        }

        fn lock(&self) -> PathBuf {
            self.root.join("bana-home/act.lock")
        }

        /// A test that passed leaves nothing behind; one that failed keeps its files.
        pub(crate) fn remove(self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }

        fn posts(&self) -> Vec<Post> {
            self.read("gh.log")
                .lines()
                .map(|l| {
                    let a: Vec<&str> = l.trim_end_matches('\t').split('\t').collect();
                    let f = |k: &str| a.iter().find_map(|x| x.strip_prefix(k)).map(String::from);
                    Post {
                        sha: a[3].rsplit('/').next().unwrap().to_string(),
                        state: f("state=").unwrap(),
                        context: f("context=").unwrap(),
                        description: f("description=").unwrap(),
                        url: f("target_url="),
                    }
                })
                .collect()
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Post {
        sha: String,
        state: String,
        context: String,
        description: String,
        url: Option<String>,
    }

    pub(crate) async fn until(what: &str, mut ok: impl FnMut() -> bool) {
        for _ in 0..1500 {
            if ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("waited 30 s for {what}");
    }

    async fn poll(d: &Daemon) {
        let n = d.0.lock().polls;
        d.poll_now();
        until("a poll", || d.0.lock().polls > n).await;
    }

    pub(crate) async fn finished(d: &Daemon, id: u64) -> Record {
        until(&format!("build {id} to finish"), || {
            let inner = d.0.lock();
            inner
                .records
                .get(&id)
                .is_some_and(|r| r.build.state.finished())
                && inner.running.as_ref().is_none_or(|r| r.id != id)
        })
        .await;
        d.0.lock().records[&id].clone()
    }

    async fn posted(d: &Daemon) {
        until("statuses posted", || d.summary().watcher.unposted == 0).await;
    }

    pub(crate) async fn start(p: &Project, extra: &str) -> Daemon {
        start_with(p.settings(extra)).await
    }

    async fn start_with(s: Settings) -> Daemon {
        let d = Daemon::start(s).await.unwrap();
        until("the first fetch", || d.0.lock().polls > 0).await;
        d
    }

    /// Gone, or a zombie nobody reaped yet.
    fn gone(pid: &str) -> bool {
        !alive(pid.trim().parse().unwrap())
    }

    /// The live processes that carry build `id`'s marker.
    async fn marked(p: &Project, id: u64) -> Vec<u32> {
        let marker = format!("{}-{id}", p.prefix);
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        let path = std::env::var("PATH").unwrap();
        let mut pids = sweep::marker_pids(&path, uid, &marker).await;
        pids.retain(|p| alive(*p));
        pids
    }

    fn waiting(d: &Daemon) -> Option<String> {
        d.summary().queue.first().and_then(|q| q.waiting.clone())
    }

    #[test]
    fn settings_have_defaults_and_refuse_keys_they_do_not_know() {
        let env: BTreeMap<String, String> = [("HOME", "/home/me"), ("PATH", "/usr/bin")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let dir = Path::new("/x/wid");
        let s = Settings::parse(
            "# bana daemon install\nrepo = o/r\n\nprefix = wid\n",
            dir,
            env.clone(),
        )
        .unwrap();
        assert_eq!(s.tiers, ["quick", "nightly", "release"]);
        assert_eq!(
            (s.rules.tier.as_str(), s.rules.tag_tier.as_str()),
            ("quick", "release")
        );
        assert_eq!(s.rules.branches, ["*", "!dependabot/*", "!renovate/*"]);
        assert!(s.rules.tags.is_empty() && s.rules.supersede == Supersede::Queued);
        assert_eq!(
            (s.poll, s.timeout),
            (Duration::from_secs(30), Duration::from_secs(7200))
        );
        assert_eq!(
            (s.token, s.port, s.workflow.as_str()),
            (JobToken::Gh, 8470, "ci.yml")
        );
        assert_eq!(
            (s.home.as_path(), s.home_set),
            (Path::new("/home/me/.bana"), false)
        );
        assert_eq!(
            (s.path.as_str(), s.git.as_str(), s.gh.as_str()),
            ("/usr/bin", "git", "gh")
        );
        assert_eq!(s.script, Path::new("/x/wid/daemon/bin/bana"));

        let text = "repo=o/r\nprefix=wid\ntiers=\ndaemon.timeout = 90s\ndaemon.token = none\nhome = /h\ndaemon.supersede = running\ndaemon.tags = v*\n";
        let mut env2 = env.clone();
        env2.insert("GITHUB_TOKEN".into(), "ghp_leak".into());
        env2.insert("LANG".into(), "C".into());
        let s = Settings::parse(text, dir, env2).unwrap();
        assert!(s.tiers.is_empty() && s.rules.tier.is_empty());
        assert_eq!(
            (s.timeout, s.token),
            (Duration::from_secs(90), JobToken::Empty)
        );
        assert_eq!(
            (s.rules.supersede, s.rules.tags.clone()),
            (Supersede::Running, vec!["v*".to_string()])
        );
        let child: BTreeMap<String, String> = s.child_env(7).into_iter().collect();
        assert_eq!(child.get("BANA_BUILD").map(String::as_str), Some("wid-7"));
        assert_eq!(child.get("BANA_HOME").map(String::as_str), Some("/h"));
        assert_eq!(
            child.get("BANA_PROJECT_ROOT").map(String::as_str),
            Some("/x/wid/src")
        );
        assert_eq!(child.get("LANG").map(String::as_str), Some("C"));
        assert!(!child.contains_key("GITHUB_TOKEN"), "{child:?}");
        let flags = s.act_flags(7, 4242).join(" ");
        assert!(
            flags.contains("--container-options=--label xyz.tjrb.bana=wid")
                && flags.contains("--artifact-server-port 4242")
                && flags.contains("--var-file /dev/null")
                && flags.contains("--secret-file secrets")
                && flags.contains("--action-cache-path /x/wid/act-cache")
                && flags.contains("--env GITHUB_RUN_ID=7"),
            "{flags}"
        );

        for (text, why) in [
            (
                "repo = o/r\nprefix = w\ncolour = red\n",
                "line 3: unknown key colour",
            ),
            (
                "repo = o/r\nprefix = w\njust words\n",
                "line 3: not key = value",
            ),
            ("prefix = w\n", "repo"),
            ("repo = o/r\n", "prefix"),
            (
                "repo = o/r\nprefix = w\ndaemon.tier = weekly\n",
                "daemon.tier",
            ),
            (
                "repo = o/r\nprefix = w\ndaemon.token = pat\n",
                "daemon.token",
            ),
            ("repo = o/r\nprefix = w\ndaemon.poll = 0\n", "daemon.poll"),
            ("repo = o/r\nprefix = w\nport = 70000\n", "port"),
            (
                "repo = o/r\nprefix = w\ndaemon.supersede = always\n",
                "daemon.supersede",
            ),
        ] {
            let e = Settings::parse(text, dir, env.clone()).unwrap_err();
            assert!(e.contains(why), "{text:?}: {e}");
        }
    }

    #[test]
    fn statuses_go_out_oldest_first_and_a_newer_build_of_a_commit_wins() {
        let rec = |id: u64, sha: &str, statuses: &[(&str, u64, bool)]| Record {
            request: Request {
                id,
                sha: sha.into(),
                ..Request::default()
            },
            statuses: statuses
                .iter()
                .map(|(c, seq, posted)| {
                    (
                        c.to_string(),
                        Posting {
                            state: StatusState::Pending,
                            description: String::new(),
                            seq: *seq,
                            posted: *posted,
                            error: None,
                        },
                    )
                })
                .collect(),
            ..Record::default()
        };
        let records = [
            rec(
                1,
                "a",
                &[
                    ("bana", 5, false),
                    ("bana/rust", 2, false),
                    ("bana/web", 3, true),
                ],
            ),
            rec(2, "b", &[("bana", 4, false)]),
            rec(3, "a", &[("bana/rust", 6, false)]),
        ];
        let inner = Inner {
            state: State::default(),
            records: records.into_iter().map(|r| (r.request.id, r)).collect(),
            running: None,
            watcher: Watcher::default(),
            waiting: None,
            seq: 6,
            polls: 0,
            stopping: false,
        };
        assert_eq!(
            to_post(&inner),
            [
                (2, "bana".to_string()),
                (1, "bana".into()),
                (3, "bana/rust".into())
            ]
        );
        assert_eq!(
            url_encode("package (linux-arm64)"),
            "package%20%28linux-arm64%29"
        );
        assert_eq!(sh_quote("/a b/it's"), r"'/a b/it'\''s'");
        assert_eq!(
            (
                minutes(Duration::from_secs(7200)),
                minutes(Duration::from_secs(5))
            ),
            ("120m".into(), "5s".into())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_push_is_built_and_its_statuses_posted_in_order() {
        let p = Project::new("push");
        let start_sha = git(&p.work, &["rev-parse", "HEAD"]);
        let d = start(&p, "").await;
        // The first start only records the heads.
        assert_eq!(d.heads().get("refs/heads/main"), Some(&start_sha));
        assert!(d.builds(None, 10).is_empty() && p.posts().is_empty());

        let sha = p.commit("pass", "a change");
        p.push("main");
        poll(&d).await;
        let rec = finished(&d, 1).await;
        posted(&d).await;
        assert_eq!(rec.build.state, BuildState::Success, "{rec:?}");
        assert_eq!(
            (rec.request.sha.as_str(), rec.request.trigger),
            (sha.as_str(), Trigger::Push)
        );
        assert_eq!(
            rec.request.before,
            Some(watch::zeros(&sha)),
            "no green head yet"
        );

        let posts = p.posts();
        assert!(posts.iter().all(|x| x.sha == sha), "{posts:?}");
        let first = &posts[0];
        assert_eq!(
            (
                first.context.as_str(),
                first.state.as_str(),
                first.description.as_str()
            ),
            ("bana", "pending", "running on t (quick)")
        );
        assert_eq!(first.url.as_deref(), Some("http://127.0.0.1:8470/#build=1"));
        let last = posts.last().unwrap();
        assert_eq!(
            (last.context.as_str(), last.state.as_str()),
            ("bana", "success")
        );
        assert!(last.description.starts_with("passed on t in "), "{last:?}");
        for job in ["plan", "rust"] {
            let context = format!("bana/{job}");
            let mine: Vec<&Post> = posts.iter().filter(|x| x.context == context).collect();
            let states: Vec<&str> = mine.iter().map(|x| x.state.as_str()).collect();
            assert_eq!(states.last(), Some(&"success"), "{context}: {posts:?}");
            assert!(
                states[..states.len() - 1].iter().all(|s| *s == "pending"),
                "{states:?}"
            );
            let url = format!("http://127.0.0.1:8470/#build=1&job={job}");
            assert_eq!(mine[0].url.as_deref(), Some(url.as_str()));
        }

        // What bana ci ran with.
        let argv: Vec<String> = p.read("argv.1").lines().map(String::from).collect();
        assert_eq!(argv[..4], ["quick", "--event", "event.json", "--"]);
        let label = format!("--container-options=--label xyz.tjrb.bana={}", p.prefix);
        for flag in [
            "--json",
            "--rm",
            "--secret-file",
            "--container-daemon-socket",
            &label,
            "GITHUB_RUN_ID=1",
            "BANA_DAEMON=1",
        ] {
            assert!(argv.iter().any(|a| a == flag), "{flag}: {argv:?}");
        }
        let at = argv
            .iter()
            .position(|a| a == "--artifact-server-port")
            .unwrap();
        assert_eq!(argv[at + 1].parse::<u16>().ok(), rec.port);
        // Its environment: the allowlist, the marker, and no token.
        let env = p.read("env.1");
        let has = |k: &str| env.lines().any(|l| l.starts_with(&format!("{k}=")));
        let is = |kv: &str| env.lines().any(|l| l == kv);
        assert!(!has("GITHUB_TOKEN") && !has("SSH_AUTH_SOCK"), "{env}");
        assert!(
            is(&format!("BANA_BUILD={}-1", p.prefix)) && is("BANA_ACT_LOCKED=1"),
            "{env}"
        );
        assert!(is(&format!(
            "BANA_PROJECT_ROOT={}",
            p.dir.join("src").display()
        )));
        assert!(is(&format!("HOME={}", p.root.join("h").display())) && is("USER=me"));
        assert!(
            is(&format!("PATH={}", std::env::var("PATH").unwrap())),
            "the settings' path"
        );
        // The job token: in a 0600 file, only while act ran.
        assert_eq!(p.read("secrets-mode.1").trim(), "-rw-------");
        assert_eq!(p.read("secrets.1").trim(), "GITHUB_TOKEN=gho_fromgh");
        let build = p.dir.join("builds/1");
        assert!(!build.join("secrets").exists());
        // The event act ran with.
        let event: Value = serde_json::from_str(&p.read("event.1")).unwrap();
        assert_eq!(
            (event["after"].as_str(), event["ref"].as_str()),
            (Some(sha.as_str()), Some("refs/heads/main"))
        );
        assert_eq!(
            (&event["deleted"], &event["created"]),
            (&json!(false), &json!(true))
        );
        assert_eq!(event["inputs"], json!({"tier": "quick"}));
        assert_eq!(event["repository"]["default_branch"], "main");
        assert_eq!(event["sender"]["login"], "me");
        // Its log and job list, kept.
        let lines = std::fs::read_to_string(build.join("act.jsonl")).unwrap();
        let replayed = std::fs::read_to_string(format!("{FIXTURES}/pass.jsonl")).unwrap();
        assert_eq!(lines.lines().count(), replayed.lines().count());
        assert!(std::fs::read_to_string(build.join("jobs.txt"))
            .unwrap()
            .contains("rust"));
        let page = d.log(1, Some("rust"), 0).unwrap();
        assert!(page.lines.iter().all(|l| l.job.as_deref() == Some("rust")));
        assert!(page
            .lines
            .iter()
            .any(|l| l.result.as_deref() == Some("success")));
        let again = d.log(1, None, page.next).unwrap();
        assert!(again.lines.is_empty() && again.next == page.next);
        assert_eq!(
            d.build(1).unwrap()["listed"],
            json!([[0, "plan"], [1, "rust"]])
        );
        // Green, pinned; the lock let go.
        assert_eq!(d.0.lock().state.green.get("refs/heads/main"), Some(&sha));
        let pin = git(
            &p.dir.join("src"),
            &["rev-parse", "refs/bana/green/heads/main"],
        );
        assert_eq!(pin, sha);
        assert!(!p.lock().exists());
        let s = d.summary();
        assert!(s.running.is_none() && s.queue.is_empty() && s.watcher.post_error.is_none());
        assert_eq!(
            s.last.map(|b| (b.id, b.state)),
            Some((1, BuildState::Success))
        );

        // A restart keeps it all, and builds nothing again.
        d.shutdown().await;
        let d = start(&p, "").await;
        poll(&d).await;
        assert_eq!(d.builds(None, 10).len(), 1);
        assert!(d.summary().queue.is_empty());
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ref_keeps_one_queued_build_and_green_moves_only_on_success() {
        let p = Project::new("queue");
        let d = start(&p, "").await;
        p.set("hold", true);
        let a = p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        until("build 1 to start", || !p.read("pid.1").is_empty()).await;

        // The daemon holds the lock from the checkout on; once act runs, act's
        // pid (bana ci execs it) holds it, so it stays held if the daemon dies.
        let pid = p.read("pid.1").trim().to_string();
        let read_owner = || std::fs::read_to_string(p.lock().join("owner")).unwrap_or_default();
        let me = std::process::id().to_string();
        assert!([me.as_str(), pid.as_str()].contains(&read_owner().lines().next().unwrap_or("")));
        until("act's pid in the lock", || {
            read_owner().starts_with(&format!("{pid}\n"))
        })
        .await;
        let owner = read_owner();
        let owner: Vec<&str> = owner.lines().collect();
        let lstart = started(&std::env::var("PATH").unwrap(), pid.parse().unwrap()).await;
        assert_eq!(owner[..2], [pid.as_str(), lstart.as_str()]);
        assert!(
            owner[2].starts_with("bana daemon: build 1, main "),
            "{owner:?}"
        );
        assert_eq!(d.0.lock().records[&1].pid.map(|p| p.to_string()), Some(pid));

        // Two pushes while it runs: one queued build, of the newest.
        let _b = p.commit("pass", "b");
        p.push("main");
        poll(&d).await;
        let c = p.commit("fail", "c");
        p.push("main");
        poll(&d).await;
        let s = d.summary();
        assert_eq!(s.queue.len(), 1, "{s:?}");
        assert_eq!(
            (
                s.queue[0].id,
                s.queue[0].sha.as_str(),
                s.queue[0].waiting.clone()
            ),
            (2, c.as_str(), None)
        );
        // A [skip ci] push is not built, and posts nothing.
        let x = p.commit("pass", "docs\n\n[skip ci]");
        p.push("docs");
        poll(&d).await;
        assert_eq!(d.summary().queue.len(), 1);
        assert_eq!(
            d.skipped()
                .iter()
                .map(|k| (k.sha.as_str(), k.why.as_str()))
                .collect::<Vec<_>>(),
            [(x.as_str(), "skip marker")]
        );

        p.set("hold", false);
        assert_eq!(finished(&d, 1).await.build.state, BuildState::Success);
        let r2 = finished(&d, 2).await;
        assert_eq!(
            (r2.build.state, r2.request.sha.as_str()),
            (BuildState::Failure, c.as_str())
        );
        assert_eq!(
            r2.request.before.as_deref(),
            Some(a.as_str()),
            "diffs against the last green"
        );
        posted(&d).await;
        let posts = p.posts();
        assert!(posts.iter().all(|x| x.sha == a || x.sha == c), "{posts:?}");
        let fin = posts
            .iter()
            .rev()
            .find(|x| x.context == "bana" && x.sha == c)
            .unwrap();
        assert_eq!(fin.state, "failure");
        assert!(
            fin.description
                .starts_with("lint failed at \"cargo clippy\" · "),
            "{fin:?}"
        );
        assert_eq!(d.0.lock().state.green.get("refs/heads/main"), Some(&a));

        // The next push still diffs against a, and turns main green.
        let e = p.commit("pass", "e");
        p.push("main");
        poll(&d).await;
        let r3 = finished(&d, 3).await;
        assert_eq!(
            (r3.build.state, r3.request.before.as_deref()),
            (BuildState::Success, Some(a.as_str()))
        );
        let event: Value = serde_json::from_str(&p.read("event.3")).unwrap();
        assert_eq!(
            (event["before"].as_str(), event["forced"].as_bool()),
            (Some(a.as_str()), Some(false))
        );
        assert_eq!(d.0.lock().state.green.get("refs/heads/main"), Some(&e));
        assert_eq!(
            git(
                &p.dir.join("src"),
                &["rev-parse", "refs/bana/green/heads/main"]
            ),
            e
        );
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn builds_wait_while_paused_without_docker_or_behind_bana_ci() {
        let p = Project::new("gates");
        let d = start(&p, "").await;
        d.set_paused(true);
        p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        until("paused", || waiting(&d).as_deref() == Some("paused")).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(d.summary().running.is_none() && p.read("pid.1").is_empty());
        assert_eq!(
            actlog::tray_view(&d.summary()).title,
            format!("{} paused", actlog::BRICK)
        );

        p.set("docker-down", true);
        d.set_paused(false);
        until("Docker", || {
            waiting(&d).as_deref() == Some("waiting for Docker")
        })
        .await;
        assert!(!d.summary().watcher.docker);

        // A live bana ci holds the lock.
        std::fs::create_dir_all(p.lock()).unwrap();
        let me = std::process::id();
        let lstart = started(&std::env::var("PATH").unwrap(), me).await;
        std::fs::write(
            p.lock().join("owner"),
            format!("{me}\n{lstart}\nbana ci quick (p)\n"),
        )
        .unwrap();
        p.set("docker-down", false);
        until("the lock", || {
            waiting(&d).as_deref() == Some("waiting for your bana ci")
        })
        .await;
        let s = d.summary();
        assert!(s.watcher.docker);
        assert_eq!(s.watcher.lock_holder.as_deref(), Some("bana ci quick (p)"));
        assert!(p.read("pid.1").is_empty());

        // Its owner is gone: the lock is stale, and taken over.
        let mut gone = Std::new("true").spawn().unwrap();
        gone.wait().unwrap();
        std::fs::write(
            p.lock().join("owner"),
            format!(
                "{}\nMon Jan  1 00:00:00 2024\nbana ci quick (p)\n",
                gone.id()
            ),
        )
        .unwrap();
        assert_eq!(finished(&d, 1).await.build.state, BuildState::Success);
        assert!(d.summary().watcher.lock_holder.is_none());
        assert!(!p.lock().exists());
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_rerun_and_run_now() {
        let p = Project::new("cancel");
        let d = start(&p, "").await;
        p.set("hold", true);
        let a = p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        until("build 1 to start", || !p.read("pid.1").is_empty()).await;

        // Run now goes to the front of the queue; a queued build is just removed.
        assert!(d.run_now("main", "weekly").is_err() && d.run_now("nope", "quick").is_err());
        let m = d.run_now("main", "nightly").unwrap();
        assert_eq!(
            d.summary()
                .queue
                .iter()
                .map(|q| (q.id, q.trigger.as_str()))
                .collect::<Vec<_>>(),
            [(m, "manual")]
        );
        d.cancel(m, "cancelled from the page").unwrap();
        assert!(d.summary().queue.is_empty() && !p.dir.join(format!("builds/{m}")).exists());

        // The running build: the mark in its log, SIGINT to act, and an error.
        d.cancel(1, "cancelled from the page").unwrap();
        let r = finished(&d, 1).await;
        assert_eq!(
            (r.build.state, r.build.reason.as_deref()),
            (BuildState::Error, Some("cancelled from the page"))
        );
        let log = std::fs::read_to_string(p.dir.join("builds/1/act.jsonl")).unwrap();
        assert!(
            log.contains(&actlog::cancel_mark("cancelled from the page")),
            "{log}"
        );
        assert!(
            log.contains(r#"{"bana":"stderr","msg":"bana: interrupted"}"#)
                || log.contains("bana: interrupted"),
            "{log}"
        );
        assert!(d.cancel(1, "again").is_err());
        posted(&d).await;
        let last = p.posts().pop().unwrap();
        assert_eq!(
            (
                last.context.as_str(),
                last.state.as_str(),
                last.description.as_str()
            ),
            ("bana", "error", "cancelled from the page")
        );
        assert!(!p.lock().exists());

        // A re-run: the same commit, tier and before, and it runs.
        p.set("hold", false);
        let n = d.rerun(1).unwrap();
        let r = finished(&d, n).await;
        assert_eq!(
            (r.build.state, r.request.trigger),
            (BuildState::Success, Trigger::Rerun)
        );
        assert_eq!(
            (r.request.sha.as_str(), r.request.tier.as_str()),
            (a.as_str(), "quick")
        );
        assert_eq!(r.request.before, Some(watch::zeros(&a)));

        // Run now at another tier posts under its own contexts.
        let m = d.run_now("refs/heads/main", "nightly").unwrap();
        let r = finished(&d, m).await;
        assert_eq!(
            (r.build.state, r.request.trigger),
            (BuildState::Success, Trigger::Manual)
        );
        assert_eq!(
            r.request.before,
            Some(watch::zeros(&a)),
            "main is green at a"
        );
        posted(&d).await;
        let posts: Vec<Post> = p
            .posts()
            .into_iter()
            .filter(|x| x.context.starts_with("bana nightly"))
            .collect();
        assert_eq!(posts[0].description, "running on t (nightly)");
        assert_eq!(
            (
                posts.last().unwrap().context.as_str(),
                posts.last().unwrap().state.as_str()
            ),
            ("bana nightly", "success")
        );
        assert_eq!(p.read(&format!("argv.{m}")).lines().next(), Some("nightly"));
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn statuses_wait_for_gh_and_go_out_once() {
        let p = Project::new("gh");
        p.set("gh-down", true);
        let d = start(&p, "").await;
        p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        finished(&d, 1).await;
        until("the poster to give up", || {
            d.summary().watcher.post_error.is_some()
        })
        .await;
        assert!(p.posts().is_empty());
        assert_eq!(
            d.summary().watcher.unposted,
            3,
            "bana, bana/plan, bana/rust"
        );
        assert!(actlog::tray_view(&d.summary()).title.ends_with("!gh"));

        // GitHub is back, but refuses a loopback link: the statuses go without one.
        p.set("gh-down", false);
        p.set("gh-no-url", true);
        d.poll_now();
        posted(&d).await;
        let posts = p.posts();
        let mut contexts: Vec<&str> = posts.iter().map(|x| x.context.as_str()).collect();
        contexts.sort();
        assert_eq!(
            contexts,
            ["bana", "bana/plan", "bana/rust"],
            "each once: {posts:?}"
        );
        assert!(
            posts
                .iter()
                .all(|x| x.state == "success" && x.url.is_none()),
            "{posts:?}"
        );
        assert_eq!(
            posts.last().unwrap().context,
            "bana",
            "the build's own status last"
        );
        assert!(d.summary().watcher.post_error.is_none());
        let state: State =
            serde_json::from_slice(&std::fs::read(p.dir.join("state.json")).unwrap()).unwrap();
        assert!(!state.target_url_ok);
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_head_git_could_not_read_is_read_again() {
        let p = Project::new("flaky");
        let d = start(&p, "").await;
        let old = d.heads()["refs/heads/main"].clone();
        p.set("git-log-fails", true);
        let a = p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        assert_eq!(d.heads()["refs/heads/main"], old, "not recorded as seen");
        assert!(d.summary().queue.is_empty() && d.skipped().is_empty());
        p.set("git-log-fails", false);
        poll(&d).await;
        let r = finished(&d, 1).await;
        assert_eq!(
            (r.build.state, r.request.sha.as_str()),
            (BuildState::Success, a.as_str())
        );
        assert_eq!(d.heads()["refs/heads/main"], a);
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_climbs_the_ladder_and_the_sweep_takes_what_is_left() {
        let p = Project::new("ladder");
        let cache = p.dir.join("act-cache");
        for d in ["0123456789abcdef/hostexecutor", "actions-checkout@v4"] {
            std::fs::create_dir_all(cache.join(d)).unwrap();
        }
        std::fs::write(p.ctl.join("containers"), "c0ffee act-ci-yml-rust-5d1e\n").unwrap();
        let d = start(&p, "").await;

        // A build that ends on its own: its workspaces go, Docker is left alone.
        p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        assert_eq!(finished(&d, 1).await.build.state, BuildState::Success);
        assert!(!cache.join("0123456789abcdef").exists());
        assert!(cache.join("actions-checkout@v4").exists());
        assert_eq!(p.read("docker.log"), "", "no sweep after a normal end");

        // act takes a second SIGINT to stop, and left two sleeps that are not
        // its children: gone with their marker, and the containers swept.
        p.set("hold", true);
        std::fs::write(p.ctl.join("mode"), "stubborn orphans").unwrap();
        let b = p.commit("pass", "b");
        p.push("main");
        poll(&d).await;
        until("build 2 and its orphans", || !p.read("pid.2").is_empty()).await;
        let orphans = [p.read("orphan-setsid.2"), p.read("orphan-nohup.2")];
        assert!(orphans.iter().all(|o| !gone(o)), "{orphans:?}");
        // Each carries the marker (once the sleeps are through their execs).
        let want: Vec<u32> = [&p.read("pid.2"), &orphans[0], &orphans[1]]
            .iter()
            .map(|x| x.trim().parse().unwrap())
            .collect();
        let mut found = Vec::new();
        for _ in 0..100 {
            found = marked(&p, 2).await;
            if want.iter().all(|x| found.contains(x)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(want.iter().all(|x| found.contains(x)), "{want:?} {found:?}");
        std::fs::create_dir_all(cache.join("fedcba9876543210")).unwrap();
        d.cancel(2, "cancelled from the page").unwrap();
        let r = finished(&d, 2).await;
        assert_eq!(
            (r.build.state, r.build.reason.as_deref()),
            (BuildState::Error, Some("cancelled from the page"))
        );
        let log = std::fs::read_to_string(p.dir.join("builds/2/act.jsonl")).unwrap();
        assert!(
            log.contains("bana: SIGINT 1") && log.contains("bana: SIGINT 2"),
            "{log}"
        );
        assert!(orphans.iter().all(|o| gone(o)), "{orphans:?}");
        assert!(marked(&p, 2).await.is_empty());
        let docker = p.read("docker.log");
        assert!(docker.contains(&format!("ps -a --filter label=xyz.tjrb.bana={}", p.prefix)));
        assert!(docker.contains("rm -f c0ffee\n"), "{docker}");
        assert!(
            docker.contains("volume rm -f act-ci-yml-rust-5d1e act-ci-yml-rust-5d1e-env\n"),
            "{docker}"
        );
        assert!(!cache.join("fedcba9876543210").exists());
        assert!(!p.lock().exists() && !p.dir.join("builds/2/secrets").exists());

        // act ignores SIGINT altogether: its whole tree is killed.
        std::fs::write(p.ctl.join("mode"), "deaf orphans").unwrap();
        std::fs::remove_file(p.ctl.join("docker.log")).unwrap();
        let c = p.commit("pass", "c");
        p.push("main");
        poll(&d).await;
        until("build 3", || !p.read("orphan-nohup.3").is_empty()).await;
        until("build 3", || !p.read("pid.3").is_empty()).await;
        let bana = p.read("pid.3");
        d.cancel(3, "cancelled from the menu bar").unwrap();
        let r = finished(&d, 3).await;
        assert_eq!(
            (r.build.state, r.build.reason.as_deref()),
            (BuildState::Error, Some("cancelled from the menu bar"))
        );
        assert!(gone(&bana) && gone(&p.read("orphan-setsid.3")) && gone(&p.read("orphan-nohup.3")));
        assert!(marked(&p, 3).await.is_empty());
        assert!(p.read("docker.log").contains("rm -f c0ffee\n"));
        posted(&d).await;
        let last = |sha: &str| {
            p.posts()
                .into_iter()
                .rev()
                .find(|x| x.context == "bana" && x.sha == sha)
                .map(|x| (x.state, x.description))
        };
        assert_eq!(
            last(&b),
            Some(("error".into(), "cancelled from the page".into()))
        );
        assert_eq!(
            last(&c),
            Some(("error".into(), "cancelled from the menu bar".into()))
        );
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_build_that_runs_too_long_times_out() {
        let p = Project::new("timeout");
        let d = start(&p, "daemon.timeout = 1s\n").await;
        p.set("hold", true);
        let a = p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        let r = finished(&d, 1).await;
        assert_eq!(
            (r.build.state, r.build.reason.as_deref()),
            (BuildState::Error, Some("timed out after 1s"))
        );
        let started = r.build.started_at.unwrap();
        assert!(r.build.ended_at.unwrap() - started >= 1);
        posted(&d).await;
        let last = p.posts().pop().unwrap();
        assert_eq!(
            (last.sha, last.context, last.state, last.description),
            (
                a,
                "bana".into(),
                "error".into(),
                "timed out after 1s".into()
            )
        );
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_build_cut_short_by_a_restart_runs_again_once() {
        let p = Project::new("restart");
        let mut s = p.settings("");
        // A cancel from the page would wait long; a stop does not.
        s.ladder = [Duration::from_secs(60); 2];
        let d = start_with(s.clone()).await;
        p.set("hold", true);
        std::fs::write(p.ctl.join("mode"), "deaf").unwrap();
        let a = p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        until("build 1", || !p.read("pid.1").is_empty()).await;

        // The daemon stops: act is killed, and the build is left for the next start.
        d.shutdown().await;
        assert!(gone(&p.read("pid.1")) && !p.lock().exists());
        let on_disk: Record =
            serde_json::from_slice(&std::fs::read(p.dir.join("builds/1/build.json")).unwrap())
                .unwrap();
        assert_eq!(on_disk.build.state, BuildState::Running);
        assert_eq!(on_disk.build.cancel_requested.as_deref(), Some(INTERRUPTED));
        assert!(
            p.posts().iter().all(|x| x.state == "pending"),
            "no final post"
        );

        // The next start runs it again, once, at the front: attempt 2.
        let d = start_with(s.clone()).await;
        until("build 2", || !p.read("pid.2").is_empty()).await;
        let (one, two) = {
            let inner = d.0.lock();
            (inner.records[&1].clone(), inner.records[&2].clone())
        };
        assert_eq!(
            (one.build.state, one.build.reason.as_deref()),
            (BuildState::Error, Some(INTERRUPTED))
        );
        assert_eq!(
            (
                two.request.trigger,
                two.request.attempt,
                two.request.sha.as_str()
            ),
            (Trigger::Retry, 2, a.as_str())
        );
        assert_eq!(two.request.before, one.request.before);
        assert!(one.request.before.is_some());

        // Killed (-9) while its retry runs: act lives on, holding the lock.
        let act = p.read("pid.2");
        until("act's pid in the lock", || {
            std::fs::read_to_string(p.lock().join("owner")).is_ok_and(|o| o.starts_with(&act))
        })
        .await;
        d.crash();
        assert!(!gone(&act));
        // The next start kills that act, and, a second time interrupted, the build
        // ends in error: posted, not run again.
        let d = start_with(s.clone()).await;
        assert!(gone(&act), "the orphaned act is killed");
        assert!(marked(&p, 2).await.is_empty() && !p.lock().exists());
        let two = d.0.lock().records[&2].clone();
        assert_eq!(
            (two.build.state, two.build.reason.as_deref()),
            (BuildState::Error, Some(INTERRUPTED))
        );
        assert!(d.summary().queue.is_empty() && d.0.lock().records.len() == 2);
        posted(&d).await;
        let errors: Vec<Post> = p
            .posts()
            .into_iter()
            .filter(|x| x.context == "bana" && x.state != "pending")
            .collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(
            (
                errors[0].sha.as_str(),
                errors[0].state.as_str(),
                errors[0].description.as_str()
            ),
            (a.as_str(), "error", INTERRUPTED)
        );

        // A cancel that was asked for before the crash is a cancel.
        let b = p.commit("pass", "b");
        p.push("main");
        poll(&d).await;
        until("build 3", || !p.read("pid.3").is_empty()).await;
        d.cancel(3, "cancelled from the page").unwrap();
        until("the cancel on disk", || {
            std::fs::read_to_string(p.dir.join("builds/3/build.json"))
                .is_ok_and(|j| j.contains("\"cancel_requested\": \"cancelled from the page\""))
        })
        .await;
        let act = p.read("pid.3");
        d.crash();
        let d = start_with(s).await;
        assert!(gone(&act));
        let three = d.0.lock().records[&3].clone();
        assert_eq!(
            (three.build.state, three.build.reason.as_deref()),
            (BuildState::Error, Some("cancelled from the page"))
        );
        assert!(d.summary().queue.is_empty() && d.0.lock().records.len() == 3);
        posted(&d).await;
        let last = p.posts().pop().unwrap();
        assert_eq!(
            (last.sha, last.context, last.state, last.description),
            (
                b,
                "bana".into(),
                "error".into(),
                "cancelled from the page".into()
            )
        );
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stop_during_a_cancel_takes_the_short_ladder() {
        let p = Project::new("stop");
        let mut s = p.settings("");
        s.ladder = [Duration::from_secs(60); 2];
        let d = start_with(s.clone()).await;
        p.set("hold", true);
        std::fs::write(p.ctl.join("mode"), "deaf").unwrap();
        p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        until("build 1", || !p.read("pid.1").is_empty()).await;
        d.cancel(1, "cancelled from the page").unwrap();
        until("the first SIGINT", || {
            std::fs::read_to_string(p.dir.join("builds/1/act.jsonl"))
                .is_ok_and(|l| l.contains("SIGINT ignored"))
        })
        .await;
        let t = Instant::now();
        d.shutdown().await;
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        assert!(gone(&p.read("pid.1")) && !p.lock().exists());
        // The cancel stands at the next start.
        let d = start_with(s).await;
        let r = d.0.lock().records[&1].clone();
        assert_eq!(
            (r.build.state, r.build.reason.as_deref()),
            (BuildState::Error, Some("cancelled from the page"))
        );
        assert!(d.summary().queue.is_empty());
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_is_lost_or_left_pending_across_restarts() {
        let p = Project::new("lost");
        let s = p.settings("");
        let d = start_with(s.clone()).await;
        // One daemon per directory: a second one would kill the first one's act.
        let e = Daemon::start(s.clone()).await.err().unwrap();
        assert!(e.contains("another bana daemon"), "{e}");

        // Build 1 was taken off the queue to start, and the daemon died during
        // its checkout: the next start queues it again.
        d.set_paused(true);
        let a = p.commit("pass", "a");
        p.push("main");
        poll(&d).await;
        assert_eq!(d.summary().queue.len(), 1);
        d.crash();
        let file = p.dir.join("state.json");
        let mut state: State = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        (state.queue, state.paused) = (vec![], false);
        write_json(&file, &state).unwrap();
        // GitHub refuses build 1's statuses for good; the ones after still go.
        std::fs::write(p.ctl.join("gh-422"), &a).unwrap();
        let d = start_with(s.clone()).await;
        assert_eq!(finished(&d, 1).await.build.state, BuildState::Success);

        // A line longer than a log page is left out; the log goes on after it.
        let log = p.dir.join("builds/1/act.jsonl");
        let size = std::fs::metadata(&log).unwrap().len();
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        writeln!(f, "{}", actlog::log_line(&"x".repeat(300_000))).unwrap();
        writeln!(f, "{}", actlog::log_line("after the long line")).unwrap();
        let page = d.log(1, None, size).unwrap();
        assert!(page.lines[0].msg.contains("left out"), "{page:?}");
        let page = d.log(1, None, page.next).unwrap();
        let msgs: Vec<&str> = page.lines.iter().map(|l| l.msg.as_str()).collect();
        assert_eq!(msgs, ["after the long line"]);

        // Build 2 is cut short by a stop while paused: its retry waits, and is
        // cleared. Build 2 then says it was interrupted, not pending.
        p.set("hold", true);
        let b = p.commit("pass", "b");
        p.push("main");
        poll(&d).await;
        until("build 2", || !p.read("pid.2").is_empty()).await;
        d.set_paused(true);
        d.shutdown().await;
        p.set("hold", false);
        let d = start_with(s).await;
        assert_eq!(d.summary().queue.len(), 1);
        posted(&d).await;
        assert!(p
            .posts()
            .iter()
            .filter(|x| x.sha == b)
            .all(|x| x.state == "pending"));
        assert_eq!(d.clear_queue(), 1);
        posted(&d).await;
        let last = p.posts().pop().unwrap();
        assert_eq!(
            (last.sha, last.context, last.state, last.description),
            (b, "bana".into(), "error".into(), INTERRUPTED.into())
        );
        assert!(p.posts().iter().all(|x| x.sha != a));
        let one = d.0.lock().records[&1].clone();
        assert!(
            one.statuses
                .values()
                .all(|q| q.posted && q.error.as_ref().is_some_and(|e| e.contains("422"))),
            "{:?}",
            one.statuses
        );
        d.shutdown().await;
        p.remove();
    }
}

//! The manager's HTTP side: `GET /` (the page) and the `/ci/v1` API behind
//! the token guard. Everything it runs goes through [`Tools`]: `bana` and the
//! GitHub CLI, with arguments checked here first.
//!
//! In daemon mode ([`daemon_router`]) the same page; its health and the
//! projects ([`Registry`]); and under `/ci/v1/p/<prefix>/` each project's
//! routes ([`project_router`]): the pool's, and the daemon's: its summary,
//! the builds and their logs, what the page's buttons do, the fixes Fix with
//! Claude makes, and their rounds (run_jobs), and the releases bana asks
//! about. Those call [`Daemon`]'s methods, with numeric ids, refs among the
//! heads fetched, tiers from the settings, fixes by their commit's hex
//! digits, jobs by their ids and releases by their tags.

use crate::daemon::{Daemon, RoundError, ROUND_WAIT};
use crate::fix;
use crate::guard::{err, guarded, health, Access};
use crate::registry::Registry;
use crate::release;
use crate::{
    attach_jobs, parse_local, parse_pool, parse_runs, runs_to_detail, valid_ref, valid_runner,
    valid_tier, Local, PoolRunner, RunView,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{any, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

/// How long GitHub's answers are reused (the page asks every few seconds).
const GITHUB_TTL: Duration = Duration::from_secs(8);
/// A task keeps the last this many bytes of its output.
const LOG_KEEP: usize = 64 * 1024;

/// The programs the manager runs, and what it runs them on.
#[derive(Debug, Clone)]
pub struct Tools {
    /// `bin/bana`.
    pub script: PathBuf,
    /// The GitHub CLI.
    pub gh: String,
    /// `owner/repo`.
    pub repo: String,
    /// The workflow *Start a run* dispatches (`ci.yml`).
    pub workflow: String,
    /// The tiers it offers (bana.conf's `tiers`); none: the workflow takes no tier.
    pub tiers: Vec<String>,
    /// The workflow_dispatch input that takes the tier (`tier`).
    pub tier_input: String,
}

impl Tools {
    /// The daemon's: its project, and the programs its settings name.
    pub fn from_settings(s: &crate::daemon::Settings) -> Self {
        Self {
            script: s.script.clone(),
            gh: s.gh.clone(),
            repo: s.repo.clone(),
            workflow: s.workflow.clone(),
            tiers: s.tiers.clone(),
            tier_input: s.tier_input.clone(),
        }
    }

    async fn output(&self, program: &str, args: &[&str], secs: u64) -> Result<String, String> {
        let run = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(Duration::from_secs(secs), run).await {
            Err(_) => Err(format!("{program} took longer than {secs} s")),
            Ok(Err(e)) => Err(format!("{program}: {e}")),
            Ok(Ok(o)) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into()),
            Ok(Ok(o)) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        }
    }

    async fn gh_json(&self, path: &str) -> Result<Value, String> {
        let text = self.output(&self.gh, &["api", path], 20).await?;
        serde_json::from_str(&text).map_err(|e| format!("gh api {path}: {e}"))
    }

    async fn local(&self) -> Result<Local, String> {
        let script = self.script.to_string_lossy().into_owned();
        self.output("bash", &[&script, "status-json"], 30)
            .await
            .map(|t| parse_local(&t))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: u64,
    pub what: String,
    pub running: bool,
    pub ok: Option<bool>,
    pub log: String,
}

struct Github {
    at: Option<Instant>,
    signed_in: bool,
    pool: Result<Vec<PoolRunner>, String>,
    runs: Result<Vec<RunView>, String>,
}

impl Default for Tasks {
    fn default() -> Self {
        Self {
            next: 1,
            list: Vec::new(),
        }
    }
}

struct Tasks {
    next: u64,
    list: Vec<Task>,
}

pub struct Manager {
    pub tools: Tools,
    pub machine: String,
    github: Mutex<Github>,
    tasks: Arc<Mutex<Tasks>>,
}

impl Default for Github {
    fn default() -> Self {
        Self {
            at: None,
            signed_in: false,
            pool: Ok(vec![]),
            runs: Ok(vec![]),
        }
    }
}

impl Manager {
    pub fn new(tools: Tools, machine: String) -> Arc<Self> {
        Arc::new(Self {
            tools,
            machine,
            github: Mutex::new(Github::default()),
            tasks: Arc::new(Mutex::new(Tasks::default())),
        })
    }

    async fn refresh_github(&self, force: bool) {
        let mut g = self.github.lock().await;
        if !force && g.at.is_some_and(|t| t.elapsed() < GITHUB_TTL) {
            return;
        }
        let t = &self.tools;
        g.signed_in = t.output(&t.gh, &["auth", "status"], 10).await.is_ok();
        if !g.signed_in {
            let why = "the GitHub CLI is not signed in here: gh auth login".to_string();
            (g.pool, g.runs, g.at) = (Err(why.clone()), Err(why), Some(Instant::now()));
            return;
        }
        let repo = &t.repo;
        g.pool = t
            .gh_json(&format!("repos/{repo}/actions/runners?per_page=100"))
            .await
            .map(|v| parse_pool(&v));
        g.runs = match t
            .gh_json(&format!("repos/{repo}/actions/runs?per_page=10"))
            .await
        {
            Err(e) => Err(e),
            Ok(runs) => {
                let mut jobs = BTreeMap::new();
                for id in runs_to_detail(&runs, 4) {
                    if let Ok(j) = t
                        .gh_json(&format!("repos/{repo}/actions/runs/{id}/jobs"))
                        .await
                    {
                        jobs.insert(id, j);
                    }
                }
                Ok(parse_runs(&runs, &jobs))
            }
        };
        let runs = g.runs.clone();
        if let (Ok(pool), Ok(runs)) = (&mut g.pool, &runs) {
            attach_jobs(pool, runs);
        }
        g.at = Some(Instant::now());
    }

    pub async fn state(&self) -> Value {
        let local = self.tools.local().await;
        self.refresh_github(false).await;
        let g = self.github.lock().await;
        let tasks = self.tasks.lock().await;
        let split = |r: Result<Value, String>| match r {
            Ok(v) => (v, Value::Null),
            Err(e) => (json!([]), json!(e)),
        };
        let (usb, local) = match local {
            Ok(l) => (json!(l.usb), Ok(json!(l.runners))),
            Err(e) => (json!([]), Err(e)),
        };
        let (local, local_error) = split(local);
        let (pool, pool_error) = split(g.pool.clone().map(|v| json!(v)));
        let (runs, runs_error) = split(g.runs.clone().map(|v| json!(v)));
        json!({
            "machine": self.machine,
            "repo": self.tools.repo,
            "workflow": self.tools.workflow,
            "signed_in": g.signed_in,
            "tiers": self.tools.tiers,
            "local": local, "local_error": local_error,
            "usb": usb,
            "pool": pool, "pool_error": pool_error,
            "runs": runs, "runs_error": runs_error,
            "tasks": tasks.list.iter().rev().take(8).map(|t| {
                let mut t = t.clone();
                // The list shows the end of each log; GET /tasks/{id} has all of it.
                if t.log.len() > 4000 {
                    let cut = t.log.len() - 4000;
                    let cut = (cut..t.log.len()).find(|i| t.log.is_char_boundary(*i)).unwrap_or(cut);
                    t.log = t.log[cut..].to_string();
                }
                t
            }).collect::<Vec<_>>(),
        })
    }

    /// Runs `program args` as a task; `exclusive` tasks (those that change this
    /// machine's runners) run one at a time.
    async fn spawn(
        &self,
        what: String,
        program: String,
        args: Vec<String>,
        exclusive: bool,
    ) -> Result<u64, String> {
        let mut tasks = self.tasks.lock().await;
        if exclusive {
            if let Some(t) = tasks
                .list
                .iter()
                .find(|t| t.running && t.what.starts_with("runners:"))
            {
                return Err(format!("wait for '{}' to finish", t.what));
            }
        }
        let id = tasks.next;
        tasks.next += 1;
        let mut child = Command::new(&program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{program}: {e}"))?;
        tasks.list.push(Task {
            id,
            what,
            running: true,
            ok: None,
            log: format!("$ {program} {}\n", args.join(" ")),
        });
        let all = self.tasks.clone();
        let pipe = |r: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>| {
            let all = all.clone();
            tokio::spawn(async move {
                let Some(r) = r else { return };
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let mut t = all.lock().await;
                    if let Some(task) = t.list.iter_mut().find(|t| t.id == id) {
                        task.log.push_str(&l);
                        task.log.push('\n');
                        if task.log.len() > LOG_KEEP {
                            let cut = task.log.len() - LOG_KEEP;
                            let cut = (cut..task.log.len())
                                .find(|i| task.log.is_char_boundary(*i))
                                .unwrap_or(cut);
                            task.log.drain(..cut);
                        }
                    }
                }
            })
        };
        let out = pipe(child.stdout.take().map(|s| Box::new(s) as _));
        let errs = pipe(child.stderr.take().map(|s| Box::new(s) as _));
        tokio::spawn(async move {
            let status = child.wait().await;
            let _ = (out.await, errs.await);
            let mut t = all.lock().await;
            if let Some(task) = t.list.iter_mut().find(|t| t.id == id) {
                task.running = false;
                task.ok = Some(status.map(|s| s.success()).unwrap_or(false));
            }
            let keep = t.list.len().saturating_sub(30);
            t.list.drain(..keep);
        });
        Ok(id)
    }

    fn script(&self) -> String {
        self.tools.script.to_string_lossy().into_owned()
    }
}

type S = State<Arc<Manager>>;

fn started(r: Result<u64, String>) -> Response {
    match r {
        Ok(id) => Json(json!({ "task": id })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

async fn state(State(m): S) -> Json<Value> {
    Json(m.state().await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Join {
    #[serde(default = "one")]
    linux: u8,
    /// On a Mac with OrbStack: x86_64 Linux runners (ignored elsewhere).
    #[serde(default = "one")]
    x64: u8,
    #[serde(default = "yes")]
    mac: bool,
    #[serde(default)]
    dedicated: bool,
}
fn one() -> u8 {
    1
}
fn yes() -> bool {
    true
}

async fn join(State(m): S, Json(b): Json<Join>) -> Response {
    if b.linux > 8 {
        return err(
            StatusCode::BAD_REQUEST,
            "at most 8 Linux runners per machine",
        );
    }
    let mut args = vec![
        m.script(),
        "up".into(),
        "--linux".into(),
        b.linux.to_string(),
        "--x64".into(),
        b.x64.to_string(),
    ];
    if !b.mac {
        args.push("--no-mac".into());
    }
    if b.dedicated {
        args.push("--dedicated".into());
    }
    let what = format!(
        "runners: join ({} Linux, {} x86_64{}{})",
        b.linux,
        b.x64,
        if b.mac { ", macOS" } else { "" },
        if b.dedicated { ", dedicated" } else { "" }
    );
    started(m.spawn(what, "bash".into(), args, true).await)
}

async fn leave(State(m): S) -> Response {
    let args = vec![m.script(), "down".into()];
    started(
        m.spawn("runners: leave the pool".into(), "bash".into(), args, true)
            .await,
    )
}

async fn runner_action(State(m): S, Path((name, action)): Path<(String, String)>) -> Response {
    if !valid_runner(&name) {
        return err(StatusCode::BAD_REQUEST, "not a runner name");
    }
    if action != "start" && action != "stop" {
        return err(StatusCode::NOT_FOUND, "start or stop");
    }
    let args = vec![m.script(), action.clone(), name.clone()];
    started(
        m.spawn(
            format!("runners: {action} {name}"),
            "bash".into(),
            args,
            true,
        )
        .await,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRun {
    #[serde(default)]
    tier: String,
    #[serde(rename = "ref")]
    git_ref: String,
}

async fn start_run(State(m): S, Json(b): Json<StartRun>) -> Response {
    let t = &m.tools;
    if t.tiers.is_empty() != b.tier.is_empty() || (!b.tier.is_empty() && !t.tiers.contains(&b.tier))
    {
        return err(
            StatusCode::BAD_REQUEST,
            if t.tiers.is_empty() {
                "this workflow takes no tier".to_string()
            } else {
                format!("tier: one of {}", t.tiers.join(", "))
            },
        );
    }
    if !valid_ref(&b.git_ref) {
        return err(StatusCode::BAD_REQUEST, "ref: a branch or tag name");
    }
    let mut args: Vec<String> = [
        "workflow",
        "run",
        &t.workflow,
        "--repo",
        &t.repo,
        "--ref",
        &b.git_ref,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if !b.tier.is_empty() {
        args.extend(["-f".to_string(), format!("{}={}", t.tier_input, b.tier)]);
    }
    let what = if b.tier.is_empty() {
        format!("run {} on {}", t.workflow, b.git_ref)
    } else {
        format!("run {} on {}", b.tier, b.git_ref)
    };
    let r = m.spawn(what, m.tools.gh.clone(), args, false).await;
    m.github.lock().await.at = None; // show it on the next look
    started(r)
}

async fn cancel_run(State(m): S, Path(id): Path<u64>) -> Response {
    let args = vec![
        "run".into(),
        "cancel".into(),
        id.to_string(),
        "--repo".into(),
        m.tools.repo.clone(),
    ];
    let r = m
        .spawn(format!("cancel run {id}"), m.tools.gh.clone(), args, false)
        .await;
    m.github.lock().await.at = None;
    started(r)
}

async fn task(State(m): S, Path(id): Path<u64>) -> Response {
    match m.tasks.lock().await.list.iter().find(|t| t.id == id) {
        Some(t) => Json(t.clone()).into_response(),
        None => err(StatusCode::NOT_FOUND, "no such task (the last 30 are kept)"),
    }
}

async fn page() -> Response {
    (
        [(header::CONTENT_SECURITY_POLICY, "default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'")],
        Html(include_str!("page.html")),
    )
        .into_response()
}

/// The page at `/`, the API at `/ci/v1`, behind the guard.
pub fn router(m: Arc<Manager>, access: Arc<Access>) -> Router {
    let api = pool_routes(m).merge(health("ci", 1));
    guarded(
        Router::new().route("/", get(page)).nest("/ci/v1", api),
        access,
    )
}

/// The one daemon's: the page, a health that says it serves the machine,
/// the projects, and each project's routes under `/ci/v1/p/<prefix>/`.
pub fn daemon_router(r: Arc<Registry>, access: Arc<Access>) -> Router {
    let api = Router::new()
        .route("/health", get(daemon_health))
        .route("/projects", get(projects).post(rescan))
        .route("/p/{prefix}/{*rest}", any(forward))
        .with_state(r);
    guarded(
        Router::new().route("/", get(page)).nest("/ci/v1", api),
        access,
    )
}

/// A project's routes, as `/ci/v1/p/<prefix>/…` reaches them: the pool's
/// (its repo's runners and runs) and its daemon's.
pub fn project_router(d: Daemon) -> Router {
    let m = Manager::new(
        Tools::from_settings(d.settings()),
        d.settings().machine.clone(),
    );
    pool_routes(m).merge(local_routes(d))
}

fn pool_routes(m: Arc<Manager>) -> Router {
    Router::new()
        .route("/state", get(state))
        .route("/pool/join", post(join))
        .route("/pool/leave", post(leave))
        .route("/runners/{name}/{action}", post(runner_action))
        .route("/runs", post(start_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/tasks/{id}", get(task))
        .with_state(m)
}

type R = State<Arc<Registry>>;

/// Open: `bana` looks for it, and a daemon starting asks it.
async fn daemon_health(State(r): R) -> Json<Value> {
    let m = r.machine();
    Json(
        json!({"ok": true, "service": "ci", "api": 1, "daemon": true, "global": true,
        "port": m.port, "machine": m.machine, "projects": r.prefixes()}),
    )
}

/// The projects as JSON, one a line: the shell reads it with sed.
fn rows(rows: Vec<Value>) -> Response {
    let lines: Vec<String> = rows.iter().map(Value::to_string).collect();
    let body = match lines.is_empty() {
        true => "[]\n".to_string(),
        false => format!("[\n{}\n]\n", lines.join(",\n")),
    };
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Each project: its repo, checkout, pause, error, queue, running and last build.
async fn projects(State(r): R) -> Response {
    rows(r.rows())
}

/// Reads the projects' files again (bana add, remove, pause and resume),
/// once that is done: the projects.
async fn rescan(State(r): R) -> Response {
    r.scan().await;
    rows(r.rows())
}

/// `/ci/v1/p/<prefix>/<rest>`: project `prefix`'s `/<rest>`, as a request
/// of its own (its handlers see their own path, and nothing of this one's).
async fn forward(State(r): R, req: axum::extract::Request) -> Response {
    use tower::ServiceExt;
    let path = req.uri().path();
    let Some((prefix, rest)) = path
        .strip_prefix("/ci/v1")
        .unwrap_or(path)
        .strip_prefix("/p/")
        .and_then(|p| p.split_once('/'))
    else {
        return err(StatusCode::NOT_FOUND, "no such route");
    };
    let routes = match r.routes(prefix) {
        None => return err(StatusCode::NOT_FOUND, format!("no project {prefix}")),
        Some(Err(why)) => return err(StatusCode::SERVICE_UNAVAILABLE, format!("{prefix}: {why}")),
        Some(Ok(routes)) => routes,
    };
    let uri = match req.uri().query() {
        Some(q) => format!("/{rest}?{q}"),
        None => format!("/{rest}"),
    };
    let Ok(uri) = uri.parse::<axum::http::Uri>() else {
        return err(StatusCode::BAD_REQUEST, "a bad path");
    };
    let (parts, body) = req.into_parts();
    let mut inner = axum::extract::Request::new(body);
    *inner.method_mut() = parts.method;
    *inner.uri_mut() = uri;
    *inner.version_mut() = parts.version;
    *inner.headers_mut() = parts.headers;
    match routes.oneshot(inner).await {
        Ok(res) => res,
        Err(e) => match e {},
    }
}

type D = State<Daemon>;

fn local_routes(d: Daemon) -> Router {
    Router::new()
        .route("/health", get(project_health))
        .route("/local", get(local))
        .route("/builds", get(builds).post(run_now))
        .route("/builds/{id}", get(build))
        .route("/builds/{id}/log", get(build_log))
        .route("/builds/{id}/report", get(build_report))
        .route("/builds/{id}/files/{name}", get(build_file))
        .route("/builds/{id}/cancel", post(cancel_build))
        .route("/builds/{id}/rerun", post(rerun))
        .route("/builds/{id}/fix", post(fix_build))
        .route("/fixes", get(fixes).post(register_fix))
        .route("/fixes/{fix}", get(fix_state))
        .route("/fixes/{fix}/rounds", post(ask_round))
        .route("/fixes/{fix}/rounds/{n}", get(round))
        .route("/fixes/{fix}/more", post(more_rounds))
        .route("/fixes/{fix}/keep", post(keep_fix))
        .route("/fixes/{fix}/push", post(push_fix))
        .route("/fixes/{fix}/drop", post(drop_fix))
        .route("/fixes/{fix}/forget", post(forget_fix))
        .route("/daemon", post(set_daemon))
        .route("/daemon/poll", post(poll))
        .route("/queue/clear", post(clear_queue))
        .route("/releases", get(releases))
        .route("/releases/{tag}", get(release_state))
        .route("/releases/{tag}/notes", put(release_notes))
        .route("/releases/{tag}/publish", post(publish_release))
        .route("/releases/{tag}/dismiss", post(dismiss_release))
        .with_state(d)
}

/// The releases, the newest first.
async fn releases(State(d): D) -> Json<Value> {
    Json(json!({ "releases": d.releases() }))
}

fn no_release(tag: &str) -> Response {
    err(StatusCode::NOT_FOUND, format!("no release {tag}"))
}

fn release_error(e: release::Error) -> Response {
    match e {
        release::Error::Missing(m) => err(StatusCode::NOT_FOUND, m),
        release::Error::Refused(m) => err(StatusCode::CONFLICT, m),
        release::Error::Bad(m) => err(StatusCode::BAD_REQUEST, m),
    }
}

/// One release: its record, its build's files and CI report table, the
/// changes since the previous release, the notes with their rev and check.
async fn release_state(State(d): D, Path(tag): Path<String>) -> Response {
    if !release::valid_tag(&tag) {
        return no_release(&tag);
    }
    match d.release(&tag) {
        Some(v) => Json(v).into_response(),
        None => no_release(&tag),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveNotes {
    notes: String,
    #[serde(default)]
    title: Option<String>,
    rev: u64,
    /// `you` (the page), or `claude` (the MCP).
    #[serde(default)]
    source: Option<String>,
}

/// Saves the notes over those at `rev`: {rev, missing_prs, outside_range,
/// duplicated}. 409 for a stale rev, or a release publishing or published.
async fn release_notes(State(d): D, Path(tag): Path<String>, Json(b): Json<SaveNotes>) -> Response {
    if !release::valid_tag(&tag) {
        return no_release(&tag);
    }
    let source = b.source.as_deref().unwrap_or("you");
    match d.save_notes(&tag, &b.notes, b.title.as_deref(), b.rev, source) {
        Ok(v) => Json(v).into_response(),
        Err(e) => release_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Publish {
    rev: u64,
}

/// The owner's yes: 202, and the publish runs. 409 unless bana asks about
/// it (or it failed, or was dismissed), the rev is the notes' now, and no
/// other publish runs.
async fn publish_release(State(d): D, Path(tag): Path<String>, Json(b): Json<Publish>) -> Response {
    if !release::valid_tag(&tag) {
        return no_release(&tag);
    }
    match d.publish_release(&tag, b.rev) {
        Ok(v) => (StatusCode::ACCEPTED, Json(v)).into_response(),
        Err(e) => release_error(e),
    }
}

/// Not now.
async fn dismiss_release(State(d): D, Path(tag): Path<String>) -> Response {
    if !release::valid_tag(&tag) {
        return no_release(&tag);
    }
    match d.dismiss_release(&tag) {
        Ok(v) => Json(v).into_response(),
        Err(e) => release_error(e),
    }
}

/// Which project answers.
async fn project_health(State(d): D) -> Json<Value> {
    let s = d.settings();
    Json(
        json!({"ok": true, "service": "ci", "api": 1, "daemon": true, "repo": s.repo, "prefix": s.prefix}),
    )
}

/// The summary, and what Run now may offer: the refs fetched and the tiers.
async fn local(State(d): D) -> Json<Value> {
    let mut v = json!(d.summary());
    v["refs"] = json!(d.heads().keys().collect::<Vec<_>>());
    v["tiers"] = json!(d.settings().tiers);
    v["skipped"] = json!(d.skipped().iter().rev().take(20).collect::<Vec<_>>());
    v["port"] = json!(d.settings().port);
    Json(v)
}

#[derive(Deserialize)]
struct Page {
    before: Option<u64>,
    limit: Option<usize>,
}

/// The history, each build with its statuses posted and waiting; a fix's
/// round with its fix, round and job.
async fn builds(State(d): D, Query(q): Query<Page>) -> Json<Value> {
    let statuses = d.statuses();
    let rounds = d.round_builds();
    let builds: Vec<Value> = d
        .builds(q.before, q.limit.unwrap_or(100).min(100))
        .into_iter()
        .map(|b| {
            let (posted, unposted) = statuses.get(&b.id).copied().unwrap_or_default();
            let mut v = json!(b);
            v["posted"] = json!(posted);
            v["unposted"] = json!(unposted);
            if let Some((fix, round, job)) = rounds.get(&b.id) {
                (v["fix"], v["round"], v["job"]) = (json!(fix), json!(round), json!(job));
            }
            v
        })
        .collect();
    Json(json!({ "builds": builds }))
}

async fn build(State(d): D, Path(id): Path<u64>) -> Response {
    match d.build(id) {
        Some(b) => Json(b).into_response(),
        None => err(StatusCode::NOT_FOUND, format!("no build {id}")),
    }
}

#[derive(Deserialize)]
struct LogQuery {
    job: Option<String>,
    #[serde(default)]
    from: u64,
}

async fn build_log(State(d): D, Path(id): Path<u64>, Query(q): Query<LogQuery>) -> Response {
    let job = q.job.filter(|j| !j.is_empty());
    if job.as_ref().is_some_and(|j| j.len() > 200) {
        return err(StatusCode::BAD_REQUEST, "job: a job's key");
    }
    if d.build(id).is_none() {
        return err(StatusCode::NOT_FOUND, format!("no build {id}"));
    }
    match d.log(id, job.as_deref(), q.from) {
        Ok(page) => Json(page).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// A build's CI report: report.md as `markdown`, and its table as `standards`.
async fn build_report(State(d): D, Path(id): Path<u64>) -> Response {
    match d.ci_report(id).await {
        None => err(StatusCode::NOT_FOUND, format!("no build {id}")),
        Some(None) => err(
            StatusCode::NOT_FOUND,
            format!("no report for build {id}: a build gets one when it ends"),
        ),
        Some(Some(r)) => {
            Json(json!({"build": id, "markdown": r.markdown, "standards": r.standards}))
                .into_response()
        }
    }
}

/// One of a build's collected files, to download: only a name its dist
/// lists (no path, no `..`).
async fn build_file(State(d): D, Path((id, name)): Path<(u64, String)>) -> Response {
    let Some(path) = d.file(id, &name) else {
        return err(
            StatusCode::NOT_FOUND,
            format!("build {id} has no file {name}"),
        );
    };
    match tokio::task::spawn_blocking(move || std::fs::read(path)).await {
        Ok(Ok(bytes)) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{name}\""),
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("{name}: {e}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunNow {
    #[serde(rename = "ref")]
    git_ref: String,
    #[serde(default)]
    tier: String,
}

async fn run_now(State(d): D, Json(b): Json<RunNow>) -> Response {
    if !valid_ref(&b.git_ref) {
        return err(StatusCode::BAD_REQUEST, "ref: a branch or tag name");
    }
    if !b.tier.is_empty() && !valid_tier(&b.tier) {
        return err(StatusCode::BAD_REQUEST, "tier: a word from the settings");
    }
    // Refused unless the ref is a head fetched and the tier a settings' tier.
    match d.run_now(&b.git_ref, &b.tier) {
        Ok(id) => Json(json!({ "build": id })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

async fn cancel_build(State(d): D, Path(id): Path<u64>) -> Response {
    match d.cancel(id, "cancelled from the page") {
        Ok(()) => Json(json!({ "build": id })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

async fn rerun(State(d): D, Path(id): Path<u64>) -> Response {
    if d.build(id).is_none() {
        return err(StatusCode::NOT_FOUND, format!("no build {id}"));
    }
    match d.rerun(id) {
        Ok(new) => Json(json!({ "build": new })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

/// Fix with Claude: the fix for a failed build, made or gone on with, as
/// `bana-manager fix prepare` prints it: {fix, worktree, branch, link,
/// command, dir, reused}, and its rounds (round 0, the recheck, is queued
/// with a new fix). 404 for no such build, 409 for one that did not fail (it
/// passed, was cancelled, timed out, could not start, or runs) or is a
/// fix's round.
async fn fix_build(State(d): D, Path(id): Path<u64>) -> Response {
    match d.fix(id).await {
        Ok(made) => {
            let mut v = json!(made);
            v["rounds"] = d.rounds(&made.fix);
            Json(v).into_response()
        }
        Err(e) => fix_error(e),
    }
}

/// The fixes, each with where it stands: its state, rounds and round 0.
async fn fixes(State(d): D) -> Json<Value> {
    Json(json!({ "fixes": d.fixes().await }))
}

/// A fix's name: 4 to 64 hex digits of its commit (its sha7).
fn fix_name(name: &str) -> bool {
    (4..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One fix, by its sha7 (4 to 64 hex digits of its commit).
async fn fix_state(State(d): D, Path(name): Path<String>) -> Response {
    if !fix_name(&name) {
        return err(StatusCode::BAD_REQUEST, "fix: its commit's hex digits");
    }
    let mut v = match d.fix_state(&name).await {
        Ok(v) => v,
        Err(e) => return fix_error(e),
    };
    // For the card: its new files, and whether origin has the branch.
    let s = d.settings();
    let (git, path) = (s.git.clone(), s.path.clone());
    let f: Option<fix::Fix> = serde_json::from_value(v.clone()).ok();
    if let Some(f) = f {
        let card = tokio::task::spawn_blocking(move || fix::card(&f, &git, Some(&path)))
            .await
            .unwrap_or_default();
        if let (Some(v), Some(card)) = (v.as_object_mut(), card.as_object()) {
            v.extend(card.clone());
        }
    }
    Json(v).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    fix: String,
}

/// A fix `bana fix` prepared in a terminal (a hand run, a pasted log), once it
/// pushed the failing commit into the daemon's clone: its round 0 is queued.
async fn register_fix(State(d): D, Json(b): Json<Register>) -> Response {
    if !fix_name(&b.fix) {
        return err(StatusCode::BAD_REQUEST, "fix: its commit's hex digits");
    }
    match d.register(&b.fix).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => round_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskRound {
    /// The snapshot, pushed to refs/bana/fix/<sha7>/… in src.
    #[serde(default)]
    sha: Option<String>,
    /// Without `sha`: its tree, to ask first whether a round would run.
    #[serde(default)]
    tree: Option<String>,
    #[serde(default)]
    jobs: Option<Vec<String>>,
    #[serde(default)]
    repeat: bool,
}

/// run_jobs: a round at a snapshot. {fix, round, reused, tree, builds,
/// rounds_left}; 409 with the reason (and `running`, the round that runs)
/// when the limits say no or the snapshot is not the fix's. With a `tree`
/// and no `sha`, it only says whether a round would run (run_jobs asks so
/// before it pushes a snapshot): {fix, round, reused, tree, rounds_left}.
async fn ask_round(State(d): D, Path(name): Path<String>, Json(b): Json<AskRound>) -> Response {
    if !fix_name(&name) {
        return err(StatusCode::BAD_REQUEST, "fix: its commit's hex digits");
    }
    let full = |s: &str| matches!(s.len(), 40 | 64) && s.bytes().all(|c| c.is_ascii_hexdigit());
    if b.jobs.as_ref().is_some_and(|j| j.len() > 32) {
        return err(StatusCode::BAD_REQUEST, "jobs: at most 32");
    }
    let done = match (&b.sha, &b.tree) {
        (Some(sha), _) if full(sha) => d.ask_round(&name, sha, b.jobs, b.repeat).await,
        (None, Some(tree)) if full(tree) => d.check_round(&name, tree, b.jobs, b.repeat),
        (None, Some(_)) => return err(StatusCode::BAD_REQUEST, "tree: a snapshot's full tree"),
        _ => return err(StatusCode::BAD_REQUEST, "sha: a snapshot's full commit"),
    };
    match done {
        Ok(v) => Json(v).into_response(),
        Err(e) => round_error(e),
    }
}

#[derive(Deserialize)]
struct Wait {
    wait: Option<u64>,
}

/// One round; `?wait=55` waits up to that many seconds for it to end.
async fn round(
    State(d): D,
    Path((name, n)): Path<(String, u32)>,
    Query(q): Query<Wait>,
) -> Response {
    if !fix_name(&name) {
        return err(StatusCode::BAD_REQUEST, "fix: its commit's hex digits");
    }
    let wait = Duration::from_secs(q.wait.unwrap_or(0).min(ROUND_WAIT));
    match d.round(&name, n, wait).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => round_error(e),
    }
}

/// More rounds, from the fix card.
async fn more_rounds(State(d): D, Path(name): Path<String>) -> Response {
    if !fix_name(&name) {
        return err(StatusCode::BAD_REQUEST, "fix: its commit's hex digits");
    }
    match d.more_rounds(&name) {
        Ok(v) => Json(v).into_response(),
        Err(e) => round_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Keep {
    message: String,
    #[serde(default)]
    include_new_files: bool,
}

/// Keep, on the fix card: the green round's tree committed on the fix's
/// branch, as commit_fix does it. 409 with the reason when the last round is
/// not green, the worktree changed since, the tree is the failing commit's,
/// or the round took in new files and `include_new_files` is not set.
async fn keep_fix(State(d): D, Path(name): Path<String>, Json(b): Json<Keep>) -> Response {
    if !fix_name(&name) {
        return err(StatusCode::BAD_REQUEST, "fix: its commit's hex digits");
    }
    let s = d.settings();
    let (dir, git, path) = (s.dir.clone(), s.git.clone(), s.path.clone());
    let done = tokio::task::spawn_blocking(move || {
        fix::commit_green(
            &dir,
            &name,
            &git,
            Some(&path),
            &b.message,
            b.include_new_files,
        )
    })
    .await
    .unwrap_or_else(|e| Err(fix::Error::Failed(format!("the commit: {e}"))));
    match done {
        Ok(c) => Json(c).into_response(),
        Err(e) => fix_error(e),
    }
}

/// Push, on the fix card (after the owner's yes): the fix's branch to origin,
/// from the checkout, as bana fix push does it; then a poll, so the daemon
/// builds it as a push. {fix, branch, commits, compare, dirty}.
async fn push_fix(State(d): D, Path(name): Path<String>) -> Response {
    if !fix_name(&name) {
        return err(StatusCode::BAD_REQUEST, "fix: its commit's hex digits");
    }
    let s = d.settings();
    let (dir, git, path) = (s.dir.clone(), s.git.clone(), s.path.clone());
    let done =
        tokio::task::spawn_blocking(move || fix::push_fix(&dir, &name, &git, Some(&path), false))
            .await
            .unwrap_or_else(|e| Err(fix::Error::Failed(format!("the push: {e}"))));
    match done {
        Ok(p) => {
            d.poll_now();
            Json(p).into_response()
        }
        Err(e) => fix_error(e),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DropFix {
    #[serde(default)]
    force: bool,
    #[serde(default)]
    delete_branch: bool,
}

/// Discard, on the fix card: the worktree goes (with its changes only when
/// `force`, which the page asks the owner about first), as bana fix drop
/// does it, and the fix's round builds with it.
async fn drop_fix(State(d): D, Path(name): Path<String>, Json(b): Json<DropFix>) -> Response {
    if !fix_name(&name) {
        return err(StatusCode::BAD_REQUEST, "fix: its commit's hex digits");
    }
    let s = d.settings();
    let (dir, git, path) = (s.dir.clone(), s.git.clone(), s.path.clone());
    let done = tokio::task::spawn_blocking(move || {
        fix::drop_fix(&dir, &name, &git, Some(&path), b.force, b.delete_branch)
    })
    .await
    .unwrap_or_else(|e| Err(fix::Error::Failed(format!("the drop: {e}"))));
    match done {
        Ok(dropped) => {
            let builds = d.forget_fix(&dropped.fix).await;
            let mut v = json!(dropped);
            v["builds"] = json!(builds);
            Json(v).into_response()
        }
        Err(e) => fix_error(e),
    }
}

/// A fix `bana fix drop` removed in a terminal: its round builds go (a
/// running one is cancelled), and its refs in src. {fix, builds}. 409 while
/// the fix still has its worktree: Discard drops a fix whole.
async fn forget_fix(State(d): D, Path(name): Path<String>) -> Response {
    if name.len() != 7
        || !name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return err(
            StatusCode::BAD_REQUEST,
            "fix: its commit's first 7 hex digits",
        );
    }
    let dir = d.settings().dir.join("fix");
    let kept: Option<fix::Fix> = std::fs::read(dir.join(format!("{name}.d/fix.json")))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if kept.is_some_and(|f| std::path::Path::new(&f.worktree).exists()) {
        return err(
            StatusCode::CONFLICT,
            format!("fix {name} still has its worktree: bana fix drop {name} first"),
        );
    }
    let builds = d.forget_fix(&name).await;
    Json(json!({"fix": name, "builds": builds})).into_response()
}

fn round_error(e: RoundError) -> Response {
    match e {
        RoundError::Missing(m) => err(StatusCode::NOT_FOUND, m),
        RoundError::Bad(m) => err(StatusCode::BAD_REQUEST, m),
        RoundError::Refused { why, running } => (
            StatusCode::CONFLICT,
            Json(json!({"error": why, "running": running})),
        )
            .into_response(),
        RoundError::Failed(m) => err(StatusCode::INTERNAL_SERVER_ERROR, m),
    }
}

fn fix_error(e: fix::Error) -> Response {
    let code = match &e {
        fix::Error::Missing(_) => StatusCode::NOT_FOUND,
        fix::Error::NotFailed(_) | fix::Error::Refused(_) => StatusCode::CONFLICT,
        fix::Error::Failed(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(code, e.to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonChange {
    paused: bool,
}

async fn set_daemon(State(d): D, Json(b): Json<DaemonChange>) -> Json<Value> {
    d.set_paused(b.paused);
    Json(json!({ "paused": b.paused }))
}

async fn poll(State(d): D) -> Json<Value> {
    d.poll_now();
    Json(json!({ "ok": true }))
}

async fn clear_queue(State(d): D) -> Json<Value> {
    Json(json!({ "cleared": d.clear_queue() }))
}

/// What answers `GET /ci/v1/health` on this machine's `port`, if anything
/// does: before the daemon takes its port, it looks whether bana has it.
pub async fn health_at(port: u16) -> Option<Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ask = async {
        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        let req = format!(
            "GET /ci/v1/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        );
        c.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        c.take(64 * 1024).read_to_end(&mut buf).await.ok()?;
        let text = String::from_utf8_lossy(&buf);
        let (head, body) = text.split_once("\r\n\r\n")?;
        if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
            return None;
        }
        serde_json::from_str::<Value>(body).ok()
    };
    tokio::time::timeout(Duration::from_secs(3), ask)
        .await
        .ok()
        .flatten()
        .filter(|v| v["ok"] == true && v["service"] == "ci")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const TOKEN: &str = "ci0123456789abcdef0123456789abcd";

    /// A manager over stand-in programs: a `bana` that reports one busy runner
    /// and a USB audio device, and a GitHub CLI that is not signed in, or one
    /// that echoes what it was asked (`gh_echo`).
    fn app_with(dir: &std::path::Path, tiers: &[&str], gh_echo: bool) -> Router {
        let script = dir.join("bana");
        std::fs::write(
            &script,
            r#"case $1 in
status-json) echo '{"kind":"runner","name":"p-t-linux-x64-1","machine":"t","labels":"self-hosted,p-linux,usb-audio,usb-1c75-af70","listener_pid":5,"worker_pid":6,"dedicated":false}'
  echo '{"kind":"usb","machine":"t","id":"1c75:af70","name":"MiniFuse 2","label":"usb-1c75-af70"}' ;;
start|stop) echo "$1 $2" ;;
*) echo "args: $*"; exit 3 ;;
esac"#,
        )
        .unwrap();
        let gh = dir.join("gh");
        let body = if gh_echo {
            "#!/bin/sh\necho \"gh $*\"\n"
        } else {
            "#!/bin/sh\necho 'not signed in' >&2\nexit 1\n"
        };
        std::fs::write(&gh, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let m = Manager::new(
            Tools {
                script,
                gh: gh.to_string_lossy().into(),
                repo: "o/r".into(),
                workflow: "ci.yml".into(),
                tiers: tiers.iter().map(|t| t.to_string()).collect(),
                tier_input: "tier".into(),
            },
            "t".into(),
        );
        router(m, Arc::new(Access::loopback(TOKEN, 8470, &["/ci/v1/"])))
    }

    fn app(dir: &std::path::Path) -> Router {
        app_with(dir, &["quick", "nightly", "release"], false)
    }

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        body: Option<Value>,
        token: bool,
    ) -> (u16, Value) {
        let mut b = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:8470");
        if token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        }
        let req = match body {
            Some(v) => b
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string())),
            None => b.body(Body::empty()),
        }
        .unwrap();
        let r = app.clone().oneshot(req).await.unwrap();
        let code = r.status().as_u16();
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bana-manager-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    async fn finished(app: &Router, id: u64) -> Value {
        for _ in 0..100 {
            let (_, t) = call(app, "GET", &format!("/ci/v1/tasks/{id}"), None, true).await;
            if t["running"] == false {
                return t;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("task {id} did not finish");
    }

    #[tokio::test]
    async fn the_page_is_open_and_everything_else_needs_the_token() {
        let d = scratch("guard");
        let app = app(&d);
        let (code, _) = call(&app, "GET", "/ci/v1/state", None, false).await;
        assert_eq!(code, 401);
        let r = app
            .clone()
            .oneshot(
                axum::http::Request::get("/")
                    .header(header::HOST, "127.0.0.1:8470")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let r = app
            .clone()
            .oneshot(
                axum::http::Request::get("/")
                    .header(header::HOST, "evil.example:8470")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 421, "DNS rebinding is refused");
    }

    #[tokio::test]
    async fn state_shows_local_processes_and_why_github_is_missing() {
        let d = scratch("state");
        let app = app(&d);
        let (code, s) = call(&app, "GET", "/ci/v1/state", None, true).await;
        assert_eq!(code, 200, "{s}");
        assert_eq!(s["local"][0]["state"], "busy");
        assert_eq!(s["usb"][0]["label"], "usb-1c75-af70");
        assert_eq!(s["tiers"], json!(["quick", "nightly", "release"]));
        assert_eq!(s["signed_in"], false);
        assert!(s["pool_error"].as_str().unwrap().contains("gh auth login"));
    }

    #[tokio::test]
    async fn actions_run_the_script_with_checked_arguments() {
        let d = scratch("actions");
        let app = app(&d);
        let (code, v) = call(
            &app,
            "POST",
            "/ci/v1/runners/p-t-linux-x64-1/stop",
            None,
            true,
        )
        .await;
        assert_eq!(code, 200, "{v}");
        let t = finished(&app, v["task"].as_u64().unwrap()).await;
        assert_eq!(t["ok"], true);
        assert!(t["log"].as_str().unwrap().contains("stop p-t-linux-x64-1"));

        assert_eq!(
            call(&app, "POST", "/ci/v1/runners/..%2Fx/stop", None, true)
                .await
                .0,
            400
        );
        assert_eq!(
            call(&app, "POST", "/ci/v1/runners/x/rm", None, true)
                .await
                .0,
            404
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/ci/v1/pool/join",
                Some(json!({"linux": 9})),
                true
            )
            .await
            .0,
            400
        );
        let bad_ref = json!({"tier": "nightly", "ref": "--help"});
        assert_eq!(
            call(&app, "POST", "/ci/v1/runs", Some(bad_ref), true)
                .await
                .0,
            400
        );
        let bad_tier = json!({"tier": "weekly", "ref": "main"});
        assert_eq!(
            call(&app, "POST", "/ci/v1/runs", Some(bad_tier), true)
                .await
                .0,
            400
        );

        // Joining runs `up` with the options asked for; the stand-in script refuses it.
        let (_, v) = call(
            &app,
            "POST",
            "/ci/v1/pool/join",
            Some(json!({"linux": 2, "mac": false, "dedicated": true})),
            true,
        )
        .await;
        let t = finished(&app, v["task"].as_u64().unwrap()).await;
        assert_eq!(t["ok"], false);
        assert!(
            t["log"]
                .as_str()
                .unwrap()
                .contains("args: up --linux 2 --x64 1 --no-mac --dedicated"),
            "{t}"
        );
    }

    #[tokio::test]
    async fn runs_start_with_the_configured_workflow_and_tier() {
        let d = scratch("runs");
        let app = app_with(&d, &["quick", "hardware"], true);
        let (code, v) = call(
            &app,
            "POST",
            "/ci/v1/runs",
            Some(json!({"tier": "hardware", "ref": "main"})),
            true,
        )
        .await;
        assert_eq!(code, 200, "{v}");
        let t = finished(&app, v["task"].as_u64().unwrap()).await;
        assert!(
            t["log"]
                .as_str()
                .unwrap()
                .contains("gh workflow run ci.yml --repo o/r --ref main -f tier=hardware"),
            "{t}"
        );
        let no_tier = json!({"ref": "main"});
        assert_eq!(
            call(&app, "POST", "/ci/v1/runs", Some(no_tier), true)
                .await
                .0,
            400
        );

        // A workflow without tiers: no -f, and a tier is refused.
        let d = scratch("runs-plain");
        let app = app_with(&d, &[], true);
        let (_, v) = call(
            &app,
            "POST",
            "/ci/v1/runs",
            Some(json!({"ref": "dev"})),
            true,
        )
        .await;
        let t = finished(&app, v["task"].as_u64().unwrap()).await;
        let log = t["log"].as_str().unwrap();
        assert!(log.contains("--ref dev") && !log.contains("-f"), "{t}");
        let tier = json!({"tier": "nightly", "ref": "main"});
        assert_eq!(
            call(&app, "POST", "/ci/v1/runs", Some(tier), true).await.0,
            400
        );
    }

    #[tokio::test]
    async fn runner_changes_run_one_at_a_time() {
        let d = scratch("serial");
        let script = d.join("bana");
        let app = app(&d);
        std::fs::write(&script, "sleep 1; echo done\n").unwrap();
        let (code, _) = call(&app, "POST", "/ci/v1/pool/leave", None, true).await;
        assert_eq!(code, 200);
        let (code, v) = call(&app, "POST", "/ci/v1/runners/a/start", None, true).await;
        assert_eq!(code, 409, "{v}");
        assert!(v["error"].as_str().unwrap().contains("leave the pool"));
    }

    use crate::daemon::tests::{finished as built, start as start_daemon, until, Project};

    /// A daemon over the stand-ins of daemon.rs's tests, and its router.
    async fn daemon_app(name: &str) -> (Project, Daemon, Router) {
        let p = Project::new(name);
        let d = start_daemon(&p, "").await;
        let access = Arc::new(Access::loopback(TOKEN, 8470, &["/ci/v1/"]));
        let app = daemon_router(Registry::of(vec![d.clone()]), access);
        (p, d, app)
    }

    async fn status_from(
        app: &Router,
        method: &str,
        path: &str,
        host: &str,
        origin: Option<&str>,
    ) -> u16 {
        let mut b = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, host)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        let r = app.clone().oneshot(b.body(Body::empty()).unwrap()).await;
        r.unwrap().status().as_u16()
    }

    /// A job key in a query string.
    fn escape(s: &str) -> String {
        s.bytes()
            .map(|c| match c {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                    (c as char).to_string()
                }
                _ => format!("%{c:02X}"),
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_builds_files_are_served_by_their_names_only() {
        let (p, d, app) = daemon_app("srv-files").await;
        p.commit("files", "packages");
        p.push("main");
        assert_eq!(
            call(&app, "POST", "/ci/v1/p/p/daemon/poll", None, true)
                .await
                .0,
            200
        );
        let rec = built(&d, 1).await;
        assert_eq!(rec.dist.map(|d| d.files.len()), Some(4));
        let get = |path: String, token: bool| {
            let app = app.clone();
            async move {
                let mut b = axum::http::Request::builder()
                    .uri(path)
                    .header(header::HOST, "127.0.0.1:8470");
                if token {
                    b = b.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
                }
                let r = app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
                let code = r.status().as_u16();
                let h = r.headers().clone();
                let bytes = r.into_body().collect().await.unwrap().to_bytes();
                (code, h, bytes.to_vec())
            }
        };
        let (code, h, body) = get("/ci/v1/p/p/builds/1/files/install.sh".into(), true).await;
        assert_eq!((code, body), (200, b"#!/bin/sh\n".to_vec()));
        assert_eq!(h[header::CONTENT_TYPE], "application/octet-stream");
        assert_eq!(
            h[header::CONTENT_DISPOSITION],
            "attachment; filename=\"install.sh\""
        );
        let (code, v) = call(&app, "GET", "/ci/v1/p/p/builds/1", None, true).await;
        assert_eq!(code, 200);
        assert_eq!(v["dist"]["files"][1]["platform"], "linux-x64", "{v}");
        assert_eq!(
            get("/ci/v1/p/p/builds/1/files/install.sh".into(), false)
                .await
                .0,
            401
        );
        for name in [
            "..%2Fbuild.json",
            "..",
            "%2E%2E%2Fact.jsonl",
            "act.jsonl",
            "install.sh%00",
            "INSTALL.SH",
        ] {
            let (code, ..) = get(format!("/ci/v1/p/p/builds/1/files/{name}"), true).await;
            assert_eq!(code, 404, "{name}");
        }
        assert_eq!(
            get("/ci/v1/p/p/builds/1/files/../build.json".into(), true)
                .await
                .0,
            404
        );
        assert_eq!(
            get("/ci/v1/p/p/builds/9/files/install.sh".into(), true)
                .await
                .0,
            404
        );
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_release_is_read_edited_answered_and_published_through_its_routes() {
        let p = Project::new("srv-release");
        let d = start_daemon(&p, "daemon.tags = v*\n").await;
        let access = Arc::new(Access::loopback(TOKEN, 8470, &["/ci/v1/"]));
        let app = daemon_router(Registry::of(vec![d.clone()]), access);
        p.commit("files", "packages");
        p.tag("v0.1.0");
        assert_eq!(
            call(&app, "POST", "/ci/v1/p/p/daemon/poll", None, true)
                .await
                .0,
            200
        );
        until("its release to ask", || {
            d.release("v0.1.0")
                .is_some_and(|v| v["state"] == "asking" && v["seeded"] == true)
        })
        .await;
        let (code, v) = call(&app, "GET", "/ci/v1/p/p/releases", None, true).await;
        assert_eq!(
            (code, &v["releases"][0]["tag"]),
            (200, &json!("v0.1.0")),
            "{v}"
        );
        let (code, v) = call(&app, "GET", "/ci/v1/p/p/releases/v0.1.0", None, true).await;
        assert_eq!(
            (code, &v["state"], &v["notes"]["rev"]),
            (200, &json!("asking"), &json!(1))
        );
        let page = format!("http://127.0.0.1:8470/#p={}&release=v0.1.0", p.prefix);
        assert_eq!(v["page_url"], json!(page));
        let (code, l) = call(&app, "GET", "/ci/v1/p/p/local", None, true).await;
        assert_eq!(
            (code, &l["release"]["state"]),
            (200, &json!("asking")),
            "{l}"
        );
        for bad in ["..", "v1%2Fx", "-v1", "v1%5E%7B%7D", "nope"] {
            let (code, _) = call(
                &app,
                "GET",
                &format!("/ci/v1/p/p/releases/{bad}"),
                None,
                true,
            )
            .await;
            assert_eq!(code, 404, "{bad}");
        }
        let put = |body: Value| {
            let app = app.clone();
            async move {
                call(
                    &app,
                    "PUT",
                    "/ci/v1/p/p/releases/v0.1.0/notes",
                    Some(body),
                    true,
                )
                .await
            }
        };
        let (code, v) = put(json!({"notes": "- Faster (#7)", "rev": 0})).await;
        assert_eq!(code, 409, "a stale rev: {v}");
        assert!(
            v["error"]
                .as_str()
                .unwrap()
                .contains("read it, then save over it"),
            "{v}"
        );
        let big = "x".repeat(release::NOTES_MAX + 1);
        assert_eq!(put(json!({"notes": big, "rev": 1})).await.0, 400);
        assert_eq!(
            put(json!({"notes": "x", "rev": 1, "source": "gh"})).await.0,
            400
        );
        assert_eq!(
            put(json!({"notes": "x", "rev": 1, "extra": 1})).await.0,
            422
        );
        let (code, v) =
            put(json!({"notes": "- Faster (#7)\n- #7 again", "rev": 1, "source": "claude"})).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(
            (
                &v["rev"],
                &v["missing_prs"],
                &v["outside_range"],
                &v["duplicated"]
            ),
            (&json!(2), &json!([]), &json!([7]), &json!([7]))
        );
        let (_, v) = call(&app, "GET", "/ci/v1/p/p/releases/v0.1.0", None, true).await;
        assert_eq!(
            (&v["notes"]["source"], &v["check"]["outside_range"]),
            (&json!("claude"), &json!([7]))
        );
        assert_eq!(v["notes"]["text"], "- Faster (#7)\n- #7 again");
        // The title Publish gives: the notes', else `<install.name> <tag>`.
        let default = format!("{} v0.1.0", d.settings().prefix);
        assert_eq!(
            (&v["title"], &v["default_title"]),
            (&json!(default), &json!(default))
        );

        // Not now, then Publish after all: 409 for a stale rev, 202 with the one seen.
        let post = |path: &'static str, body: Option<Value>| {
            let app = app.clone();
            async move { call(&app, "POST", path, body, true).await }
        };
        assert_eq!(
            post("/ci/v1/p/p/releases/v0.1.0/dismiss", None).await.0,
            200
        );
        assert_eq!(
            post("/ci/v1/p/p/releases/v0.1.0/dismiss", None).await.0,
            409
        );
        assert_eq!(
            call(&app, "GET", "/ci/v1/p/p/local", None, true).await.1["release"],
            Value::Null
        );
        assert_eq!(
            post(
                "/ci/v1/p/p/releases/v0.9.0/publish",
                Some(json!({"rev": 2}))
            )
            .await
            .0,
            404
        );
        assert_eq!(
            post(
                "/ci/v1/p/p/releases/v0.1.0/publish",
                Some(json!({"rev": 1}))
            )
            .await
            .0,
            409
        );
        let (code, v) = post(
            "/ci/v1/p/p/releases/v0.1.0/publish",
            Some(json!({"rev": 2})),
        )
        .await;
        assert_eq!((code, &v["state"]), (202, &json!("publishing")), "{v}");
        until("published", || {
            d.release("v0.1.0")
                .is_some_and(|v| v["state"] == "published")
        })
        .await;
        let (code, v) = post(
            "/ci/v1/p/p/releases/v0.1.0/publish",
            Some(json!({"rev": 2})),
        )
        .await;
        assert_eq!(code, 409, "{v}");
        assert_eq!(put(json!({"notes": "late", "rev": 2})).await.0, 409);
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test]
    async fn daemon_routes_need_the_token_a_known_host_and_no_foreign_origin() {
        let (p, d, app) = daemon_app("srv-guard").await;
        let (code, h) = call(&app, "GET", "/ci/v1/health", None, false).await;
        assert_eq!(code, 200, "{h}");
        assert_eq!(
            (&h["daemon"], &h["global"], &h["projects"]),
            (&json!(true), &json!(true), &json!(["p"]))
        );
        for (method, path) in [
            ("GET", "/ci/v1/projects"),
            ("POST", "/ci/v1/projects"),
            ("GET", "/ci/v1/p/p/health"),
            ("GET", "/ci/v1/p/p/local"),
            ("GET", "/ci/v1/p/p/builds"),
            ("GET", "/ci/v1/p/p/builds/1"),
            ("GET", "/ci/v1/p/p/builds/1/log?from=0"),
            ("GET", "/ci/v1/p/p/builds/1/report"),
            ("GET", "/ci/v1/p/p/builds/1/files/install.sh"),
            ("POST", "/ci/v1/p/p/builds"),
            ("POST", "/ci/v1/p/p/builds/1/cancel"),
            ("POST", "/ci/v1/p/p/builds/1/rerun"),
            ("POST", "/ci/v1/p/p/builds/1/fix"),
            ("GET", "/ci/v1/p/p/fixes"),
            ("GET", "/ci/v1/p/p/fixes/abcd123"),
            ("POST", "/ci/v1/p/p/fixes"),
            ("POST", "/ci/v1/p/p/fixes/abcd123/rounds"),
            ("GET", "/ci/v1/p/p/fixes/abcd123/rounds/0?wait=55"),
            ("POST", "/ci/v1/p/p/fixes/abcd123/more"),
            ("POST", "/ci/v1/p/p/fixes/abcd123/keep"),
            ("POST", "/ci/v1/p/p/fixes/abcd123/push"),
            ("POST", "/ci/v1/p/p/fixes/abcd123/drop"),
            ("POST", "/ci/v1/p/p/fixes/abcd123/forget"),
            ("POST", "/ci/v1/p/p/daemon"),
            ("POST", "/ci/v1/p/p/daemon/poll"),
            ("POST", "/ci/v1/p/p/queue/clear"),
            ("GET", "/ci/v1/p/p/releases"),
            ("GET", "/ci/v1/p/p/releases/v0.1.0"),
            ("PUT", "/ci/v1/p/p/releases/v0.1.0/notes"),
            ("POST", "/ci/v1/p/p/releases/v0.1.0/publish"),
            ("POST", "/ci/v1/p/p/releases/v0.1.0/dismiss"),
            ("GET", "/ci/v1/p/p/state"),
        ] {
            let what = format!("{method} {path}");
            assert_eq!(call(&app, method, path, None, false).await.0, 401, "{what}");
            let evil = Some("https://evil.example");
            let from = |host, origin| status_from(&app, method, path, host, origin);
            assert_eq!(from("127.0.0.1:8470", evil).await, 403, "{what}");
            assert_eq!(from("evil.example:8470", None).await, 421, "{what}");
        }
        let (code, v) = call(&app, "GET", "/ci/v1/p/p/local", None, true).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["repo"], "o/r");
        assert_eq!(v["tiers"], json!(["quick", "nightly"]));
        assert!(
            v["refs"]
                .as_array()
                .unwrap()
                .contains(&json!("refs/heads/main")),
            "{v}"
        );

        // Over a real socket, as a second daemon asks before it takes the port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let access = Arc::new(Access::loopback(TOKEN, port, &["/ci/v1/"]));
        let served = daemon_router(Registry::of(vec![d.clone()]), access);
        let server = tokio::spawn(async move { axum::serve(listener, served).await });
        let h = health_at(port).await.expect("health");
        assert_eq!((&h["daemon"], &h["global"]), (&json!(true), &json!(true)));
        server.abort();
        let _ = server.await;
        assert_eq!(health_at(port).await, None, "nobody answers");
        d.shutdown().await;
        p.remove();
    }

    #[tokio::test]
    async fn the_page_runs_cancels_and_reruns_builds_and_tails_their_logs() {
        let (p, d, app) = daemon_app("srv-builds").await;
        let post = |path: &'static str, body: Option<Value>| {
            let app = app.clone();
            async move { call(&app, "POST", path, body, true).await }
        };
        let (code, v) = post("/ci/v1/p/p/daemon", Some(json!({"paused": true}))).await;
        assert_eq!((code, &v["paused"]), (200, &json!(true)), "{v}");
        // A pause holds pushes only: Docker holds the rest.
        let docker_down = p.flag("docker-down");
        std::fs::write(&docker_down, "").unwrap();

        // Run now takes a head fetched and a tier from the settings, nothing else.
        for (body, why) in [
            (json!({"ref": "nope", "tier": "quick"}), "no branch or tag"),
            (json!({"ref": "main", "tier": "weekly"}), "tier"),
            (json!({"ref": "main"}), "tier"),
            (json!({"ref": "--help", "tier": "quick"}), "ref"),
            (json!({"ref": "main", "tier": "a b"}), "tier"),
        ] {
            let (code, v) = post("/ci/v1/p/p/builds", Some(body.clone())).await;
            assert_eq!(code, 400, "{body}: {v}");
            assert!(v["error"].as_str().unwrap().contains(why), "{body}: {v}");
        }
        let (code, v) = post(
            "/ci/v1/p/p/builds",
            Some(json!({"ref": "main", "tier": "quick"})),
        )
        .await;
        assert_eq!(code, 200, "{v}");
        let queued = v["build"].as_u64().unwrap();
        let (_, l) = call(&app, "GET", "/ci/v1/p/p/local", None, true).await;
        assert_eq!(l["watcher"]["paused"], true);
        assert_eq!(l["queue"][0]["id"], queued, "{l}");
        assert_eq!(l["queue"][0]["trigger"], "manual");
        until("the queue to wait for Docker", || {
            d.summary().queue.first().and_then(|q| q.waiting.clone())
                == Some("waiting for Docker".into())
        })
        .await;
        let cancel = format!("/ci/v1/p/p/builds/{queued}/cancel");
        assert_eq!(call(&app, "POST", &cancel, None, true).await.0, 200);
        let (_, l) = call(&app, "GET", "/ci/v1/p/p/local", None, true).await;
        assert_eq!(l["queue"], json!([]), "removed");
        assert_eq!(call(&app, "POST", &cancel, None, true).await.0, 409);
        assert_eq!(
            call(&app, "POST", "/ci/v1/p/p/builds/x/cancel", None, true)
                .await
                .0,
            400
        );

        for _ in 0..2 {
            post(
                "/ci/v1/p/p/builds",
                Some(json!({"ref": "refs/heads/main", "tier": "nightly"})),
            )
            .await;
        }
        let (code, v) = post("/ci/v1/p/p/queue/clear", None).await;
        assert_eq!((code, &v["cleared"]), (200, &json!(2)), "{v}");

        // A push of a two-entry matrix, built once resumed.
        p.commit("matrix", "two entries");
        p.push("main");
        assert_eq!(post("/ci/v1/p/p/daemon/poll", None).await.0, 200);
        until("the push to be queued", || d.summary().queue.len() == 1).await;
        let id = d.summary().queue[0].id;
        std::fs::remove_file(&docker_down).unwrap();
        until("the push to wait for the pause", || {
            d.summary().queue.first().and_then(|q| q.waiting.clone()) == Some("paused".into())
        })
        .await;
        post("/ci/v1/p/p/daemon", Some(json!({"paused": false}))).await;
        built(&d, id).await;

        until("statuses posted", || d.summary().watcher.unposted == 0).await;
        let (_, v) = call(&app, "GET", "/ci/v1/p/p/builds", None, true).await;
        assert_eq!(v["builds"][0]["id"], id, "newest first: {v}");
        assert!(v["builds"][0]["posted"].as_u64() > Some(0), "{v}");
        assert_eq!(v["builds"][0]["unposted"], 0);
        assert_eq!(v["builds"][0]["trigger"], "push");
        let (_, v) = call(&app, "GET", "/ci/v1/p/p/builds?limit=1", None, true).await;
        assert_eq!(v["builds"].as_array().unwrap().len(), 1);
        let (_, v) = call(
            &app,
            "GET",
            &format!("/ci/v1/p/p/builds?before={id}"),
            None,
            true,
        )
        .await;
        assert!(v["builds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["id"].as_u64() < Some(id)));
        let (code, b) = call(&app, "GET", &format!("/ci/v1/p/p/builds/{id}"), None, true).await;
        assert_eq!(code, 200, "{b}");
        assert!(!b["listed"].as_array().unwrap().is_empty(), "{b}");
        assert_eq!(
            call(&app, "GET", "/ci/v1/p/p/builds/999", None, true)
                .await
                .0,
            404
        );

        // Its CI report, as the build's end wrote it; none without report.md.
        let (code, r) = call(
            &app,
            "GET",
            &format!("/ci/v1/p/p/builds/{id}/report"),
            None,
            true,
        )
        .await;
        assert_eq!(code, 200, "{r}");
        let md = std::fs::read_to_string(d.settings().dir.join(format!("builds/{id}/report.md")));
        assert_eq!(r["markdown"].as_str(), md.as_deref().ok(), "{r}");
        assert!(
            r["markdown"]
                .as_str()
                .unwrap()
                .starts_with("# CI report: o/r · main "),
            "{r}"
        );
        assert_eq!(
            r["standards"].as_array().unwrap().last().unwrap()["name"],
            "all",
            "{r}"
        );
        // An ended build without its report (one from before bana wrote
        // them, or one that ended while the daemon was down) gets it now.
        let kept = d.settings().dir.join(format!("builds/{id}/report.md"));
        std::fs::remove_file(&kept).unwrap();
        let (code, again) = call(
            &app,
            "GET",
            &format!("/ci/v1/p/p/builds/{id}/report"),
            None,
            true,
        )
        .await;
        assert_eq!((code, &again["markdown"]), (200, &r["markdown"]), "{again}");
        assert!(kept.exists());
        let (code, v) = call(&app, "GET", "/ci/v1/p/p/builds/999/report", None, true).await;
        assert_eq!(code, 404, "{v}");
        assert!(
            v["error"].as_str().unwrap().starts_with("no build 999"),
            "{v}"
        );
        assert_eq!(
            call(&app, "GET", "/ci/v1/p/p/builds/999/log", None, true)
                .await
                .0,
            404
        );

        // The log from the start, from the middle, and from its end.
        let log = |q: String| {
            let app = app.clone();
            async move {
                let (code, v) = call(
                    &app,
                    "GET",
                    &format!("/ci/v1/p/p/builds/{id}/log{q}"),
                    None,
                    true,
                )
                .await;
                assert_eq!(code, 200, "{q}: {v}");
                (
                    v["next"].as_u64().unwrap(),
                    v["lines"].as_array().unwrap().clone(),
                )
            }
        };
        let file = d.settings().dir.join(format!("builds/{id}/act.jsonl"));
        let bytes = std::fs::read(&file).unwrap();
        let (next, all) = log(String::new()).await;
        assert_eq!(next, bytes.len() as u64);
        assert!(all.len() > 4, "{all:?}");
        let mid = bytes.len() / 2;
        let mid = mid + bytes[mid..].iter().position(|&c| c == b'\n').unwrap() + 1;
        let (next2, rest) = log(format!("?from={mid}")).await;
        assert_eq!(next2, next);
        assert!(!rest.is_empty() && rest.len() < all.len());
        assert_eq!(
            rest[..],
            all[all.len() - rest.len()..],
            "it resumes where it left off"
        );
        let (next3, none) = log(format!("?from={next}")).await;
        assert_eq!((next3, none.len()), (next, 0));

        // One job's lines only.
        let mut keys: Vec<String> = all
            .iter()
            .filter_map(|l| l["job"].as_str().map(String::from))
            .collect();
        keys.sort();
        keys.dedup();
        assert!(keys.len() >= 2, "{keys:?}");
        let (_, one) = log(format!("?job={}&from=0", escape(&keys[1]))).await;
        assert!(!one.is_empty() && one.len() < all.len());
        assert!(one.iter().all(|l| l["job"] == json!(keys[1])), "{one:?}");

        // Re-run: a finished build only.
        let (code, v) = call(
            &app,
            "POST",
            &format!("/ci/v1/p/p/builds/{id}/rerun"),
            None,
            true,
        )
        .await;
        assert_eq!(code, 200, "{v}");
        let again = v["build"].as_u64().unwrap();
        assert!(again > id);
        assert_eq!(
            call(&app, "POST", "/ci/v1/p/p/builds/999/rerun", None, true)
                .await
                .0,
            404
        );
        d.shutdown().await;
        let (code, v) = call(
            &app,
            "POST",
            &format!("/ci/v1/p/p/builds/{again}/rerun"),
            None,
            true,
        )
        .await;
        assert_eq!(code, 409, "not finished: {v}");
        p.remove();
    }

    /// `%XX` back to bytes, as Claude Code's handler reads the link.
    fn unescape(s: &str) -> String {
        let (b, mut out, mut i) = (s.as_bytes(), Vec::new(), 0);
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[tokio::test]
    async fn fix_with_claude_on_a_failed_build() {
        let p = Project::new("srv-fix");
        let extra = format!("checkout = {}\n", p.checkout().display());
        let d = start_daemon(&p, &extra).await;
        let access = Arc::new(Access::loopback(TOKEN, 8470, &["/ci/v1/"]));
        let app = daemon_router(Registry::of(vec![d.clone()]), access);
        let mut shas = Vec::new();
        for (id, fixture) in [(1, "pass"), (2, "fail"), (3, "syntax")] {
            shas.push(p.commit(fixture, fixture));
            p.push("main");
            call(&app, "POST", "/ci/v1/p/p/daemon/poll", None, true).await;
            built(&d, id).await;
        }
        let (sha, sha7) = (&shas[1], &shas[1][..7]);
        let (_, l) = call(&app, "GET", "/ci/v1/p/p/local", None, true).await;
        assert_eq!(l["failed"], 2, "{l}");

        let (code, v) = call(&app, "POST", "/ci/v1/p/p/builds/2/fix", None, true).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(
            (&v["fix"], &v["branch"], &v["reused"]),
            (
                &json!(sha7),
                &json!(format!("bana/fix-{sha7}")),
                &json!(false)
            )
        );
        let wt = v["worktree"].as_str().unwrap();
        let prompt =
            std::fs::read_to_string(PathBuf::from(v["dir"].as_str().unwrap()).join("prompt.txt"))
                .unwrap();
        // The link opens Claude Code in the worktree with the prompt typed.
        let link = v["link"].as_str().unwrap();
        let query = link.strip_prefix("claude-cli://open?").unwrap();
        let (cwd, q) = query.split_once('&').unwrap();
        assert_eq!(unescape(cwd.strip_prefix("cwd=").unwrap()), wt);
        assert_eq!(unescape(q.strip_prefix("q=").unwrap()), prompt);
        assert!(prompt.contains("lint › cargo clippy"), "{prompt}");
        assert!(
            v["command"]
                .as_str()
                .unwrap()
                .starts_with(&format!("cd '{wt}' && claude -n 'bana fix {sha7}' ")),
            "{v}"
        );
        let (code, again) = call(&app, "POST", "/ci/v1/p/p/builds/2/fix", None, true).await;
        assert_eq!(code, 200, "{again}");
        assert_eq!(
            (&again["fix"], &again["worktree"], &again["reused"]),
            (&v["fix"], &v["worktree"], &json!(true)),
            "a second click goes on with the fix"
        );

        // Only a failed build has one.
        for (path, code, why) in [
            ("/ci/v1/p/p/builds/1/fix", 409, "build 1 passed"),
            (
                "/ci/v1/p/p/builds/3/fix",
                409,
                "build 3 did not fail: could not start",
            ),
            ("/ci/v1/p/p/builds/99/fix", 404, "no build 99"),
        ] {
            let (c, e) = call(&app, "POST", path, None, true).await;
            assert_eq!(c, code, "{path}: {e}");
            assert!(e["error"].as_str().unwrap().starts_with(why), "{path}: {e}");
        }
        assert_eq!(
            call(&app, "POST", "/ci/v1/p/p/builds/x/fix", None, true)
                .await
                .0,
            400
        );

        let (code, all) = call(&app, "GET", "/ci/v1/p/p/fixes", None, true).await;
        assert_eq!(code, 200, "{all}");
        let f = &all["fixes"][0];
        assert_eq!(
            (&f["fix"], &f["build"], &f["sha"], &f["jobs"]),
            (&json!(sha7), &json!(2), &json!(sha), &json!(["lint"])),
            "{all}"
        );
        for name in [sha7, sha.as_str()] {
            let (code, one) =
                call(&app, "GET", &format!("/ci/v1/p/p/fixes/{name}"), None, true).await;
            assert_eq!(code, 200, "{name}: {one}");
            assert_eq!((&one["fix"], &one["ahead"]), (&json!(sha7), &json!(0)));
            assert_eq!(one["link"], again["link"]);
            assert!(
                one["brief"].as_str().unwrap().contains("cargo clippy"),
                "{one}"
            );
        }
        let other = if sha.starts_with("ffff") {
            "0000"
        } else {
            "ffff"
        };
        for (name, code) in [(other, 404), ("xyz1", 400), ("..%2Fx", 400), ("abc", 400)] {
            let (c, e) = call(&app, "GET", &format!("/ci/v1/p/p/fixes/{name}"), None, true).await;
            assert_eq!(c, code, "{name}: {e}");
        }

        // Round 0 ran at the failing commit; its long poll gives the result.
        let path = format!("/ci/v1/p/p/fixes/{sha7}/rounds/0?wait=30");
        let (code, r0) = call(&app, "GET", &path, None, true).await;
        assert_eq!(code, 200, "{r0}");
        assert_eq!((&r0["state"], &r0["sha"]), (&json!("failure"), &json!(sha)));
        let (code, one) = call(&app, "GET", &format!("/ci/v1/p/p/fixes/{sha7}"), None, true).await;
        assert_eq!(
            (code, &one["recheck"]["n"], &one["rounds_left"]),
            (200, &json!(0), &json!(5))
        );
        let (_, b) = call(&app, "GET", "/ci/v1/p/p/builds", None, true).await;
        let round0 = b["builds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == 4)
            .unwrap();
        assert_eq!(
            (&round0["fix"], &round0["round"], &round0["job"]),
            (&json!(sha7), &json!(0), &json!("lint"))
        );

        // Registered again, it has rounds: no second round 0.
        let reg = Some(json!({"fix": sha7}));
        let (code, v) = call(&app, "POST", "/ci/v1/p/p/fixes", reg, true).await;
        assert_eq!((code, &v["recheck"]), (200, &Value::Null), "{v}");
        for (path, body, code) in [
            ("/ci/v1/p/p/fixes", json!({"fix": other}), 404),
            ("/ci/v1/p/p/fixes", json!({"fix": "x/y"}), 400),
            ("/ci/v1/p/p/fixes", json!({"fix": sha7, "more": 1}), 422),
            (
                &format!("/ci/v1/p/p/fixes/{sha7}/rounds") as &str,
                json!({"sha": "abc"}),
                400,
            ),
            (
                &format!("/ci/v1/p/p/fixes/{sha7}/rounds"),
                json!({"sha": "a".repeat(40)}),
                409,
            ),
            (
                &format!("/ci/v1/p/p/fixes/{other}/rounds"),
                json!({"sha": "a".repeat(40)}),
                404,
            ),
        ] {
            let (c, e) = call(&app, "POST", path, Some(body.clone()), true).await;
            assert_eq!(c, code, "{path} {body}: {e}");
        }
        let (code, e) = call(
            &app,
            "GET",
            &format!("/ci/v1/p/p/fixes/{sha7}/rounds/7"),
            None,
            true,
        )
        .await;
        assert_eq!(code, 404, "{e}");
        let (code, m) = call(
            &app,
            "POST",
            &format!("/ci/v1/p/p/fixes/{sha7}/more"),
            None,
            true,
        )
        .await;
        assert_eq!(
            (code, &m["limit"], &m["rounds_left"]),
            (200, &json!(10), &json!(10)),
            "{m}"
        );

        // The fix card: a green round, Keep, Push, Discard.
        let wt = PathBuf::from(wt);
        let work = p.checkout();
        for (k, v) in [("user.name", "Ada"), ("user.email", "ada@example.com")] {
            let ok = std::process::Command::new("git")
                .args(["-C", &work.to_string_lossy(), "config", k, v])
                .status()
                .unwrap();
            assert!(ok.success());
        }
        let keep = format!("/ci/v1/p/p/fixes/{sha7}/keep");
        let (code, e) = call(&app, "POST", &keep, Some(json!({"message": "why"})), true).await;
        assert_eq!(code, 409, "round 0 failed: {e}");
        std::fs::write(wt.join("fixture"), "pass").unwrap();
        std::fs::write(wt.join("notes.txt"), "new\n").unwrap();
        let snap = fix::snapshot("git", None, &wt).unwrap();
        let src = d.settings().dir.join("src");
        let to = format!("{}:refs/bana/fix/{sha7}/{}", snap.commit, &snap.commit[..7]);
        let ok = std::process::Command::new("git")
            .args(["-C", &wt.to_string_lossy(), "push", "-q", "--no-verify"])
            .arg(&src)
            .arg(&to)
            .status()
            .unwrap();
        assert!(ok.success());
        let body = Some(json!({"sha": snap.commit}));
        let rounds = format!("/ci/v1/p/p/fixes/{sha7}/rounds");
        let (code, r1) = call(&app, "POST", &rounds, body, true).await;
        assert_eq!((code, &r1["round"]), (200, &json!(1)), "{r1}");
        let path = format!("/ci/v1/p/p/fixes/{sha7}/rounds/1?wait=30");
        let (_, r1) = call(&app, "GET", &path, None, true).await;
        assert_eq!(r1["state"], "success", "{r1}");
        let (_, one) = call(&app, "GET", &format!("/ci/v1/p/p/fixes/{sha7}"), None, true).await;
        assert_eq!(
            (&one["new_files"], &one["pushed"], &one["worktree_there"]),
            (&json!(["notes.txt"]), &json!(false), &json!(true)),
            "{one}"
        );
        let (code, e) = call(&app, "POST", &keep, Some(json!({"message": "why"})), true).await;
        assert_eq!(code, 409, "{e}");
        assert!(e["error"].as_str().unwrap().contains("notes.txt"), "{e}");
        let body = json!({"message": "The fixture passes", "include_new_files": true});
        let (code, c) = call(&app, "POST", &keep, Some(body), true).await;
        assert_eq!(code, 200, "{c}");
        assert_eq!(
            (&c["round"], &c["files"]),
            (&json!(1), &json!(["fixture", "notes.txt"]))
        );
        let branch = format!("refs/heads/bana/fix-{sha7}");
        let tree = std::process::Command::new("git")
            .args(["-C", &work.to_string_lossy(), "rev-parse"])
            .arg(format!("{branch}^{{tree}}"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&tree.stdout).trim(), snap.tree);

        let push = format!("/ci/v1/p/p/fixes/{sha7}/push");
        let (code, pushed) = call(&app, "POST", &push, None, true).await;
        assert_eq!(code, 200, "{pushed}");
        assert_eq!(
            (&pushed["commits"], &pushed["compare"]),
            (
                &json!(1),
                &json!(format!(
                    "https://github.com/o/r/compare/main...bana/fix-{sha7}"
                ))
            )
        );
        let (_, one) = call(&app, "GET", &format!("/ci/v1/p/p/fixes/{sha7}"), None, true).await;
        assert_eq!((&one["pushed"], &one["ahead"]), (&json!(true), &json!(1)));

        let drop = format!("/ci/v1/p/p/fixes/{sha7}/drop");
        std::fs::write(wt.join("scratch.txt"), "x").unwrap();
        let (code, e) = call(&app, "POST", &drop, Some(json!({})), true).await;
        assert_eq!(code, 409, "{e}");
        assert!(
            e["error"]
                .as_str()
                .unwrap()
                .contains("has changes not committed"),
            "{e}"
        );
        let (code, gone) = call(&app, "POST", &drop, Some(json!({"force": true})), true).await;
        assert_eq!(code, 200, "{gone}");
        assert_eq!(
            (&gone["removed"], &gone["kept"], &gone["builds"]),
            (&json!(true), &json!(1), &json!(2)),
            "the round builds go"
        );
        assert!(!wt.exists());
        let (_, b) = call(&app, "GET", "/ci/v1/p/p/builds", None, true).await;
        assert!(
            b["builds"]
                .as_array()
                .unwrap()
                .iter()
                .all(|x| x["fix"].is_null()),
            "{b}"
        );
        let refs = std::process::Command::new("git")
            .args([
                "-C",
                &src.to_string_lossy(),
                "for-each-ref",
                "refs/bana/fix",
            ])
            .output()
            .unwrap();
        assert!(
            refs.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&refs.stdout)
        );
        let (code, _) = call(
            &app,
            "POST",
            "/ci/v1/p/p/fixes/xyz1/drop",
            Some(json!({})),
            true,
        )
        .await;
        assert_eq!(code, 400);
        d.shutdown().await;
        p.remove();
    }
}

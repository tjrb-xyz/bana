//! The manager's HTTP side: `GET /` (the page) and the `/ci/v1` API behind
//! the token guard. Everything it runs goes through [`Tools`]: `bana` and the
//! GitHub CLI, with arguments checked here first.

use crate::guard::{err, guarded, health, Access};
use crate::{
    attach_jobs, parse_local, parse_pool, parse_runs, runs_to_detail, valid_ref, valid_runner,
    Local, PoolRunner, RunView,
};
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
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
    let api = Router::new()
        .route("/state", get(state))
        .route("/pool/join", post(join))
        .route("/pool/leave", post(leave))
        .route("/runners/{name}/{action}", post(runner_action))
        .route("/runs", post(start_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/tasks/{id}", get(task))
        .with_state(m)
        .merge(health("ci", 1));
    guarded(
        Router::new().route("/", get(page)).nest("/ci/v1", api),
        access,
    )
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
}

//! bana-manager: the dev manager for a pool of self-hosted runners (bana's README).
//!
//! A small web page on each machine that runs CI. It shows the whole pool of
//! runners (from GitHub), this machine's runner processes (a listener waiting
//! for jobs, a worker when one runs) and USB audio devices, and the recent runs
//! with the runner each job landed on. It joins or leaves the pool (`bana`),
//! starts and stops runners, and starts or cancels runs (the GitHub CLI).
//!
//! What it may run is fixed: those two programs, with arguments it checks.
//! It serves loopback only, behind a token guard ([`guard`]).
//!
//! This module is the pure part: the programs' outputs in, the views out.

pub mod actlog;
pub mod daemon;
pub mod guard;
pub mod server;
pub mod sweep;
#[cfg(target_os = "macos")]
pub mod tray;
pub mod watch;

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// One runner on this machine (or its Linux machine), from `bana status-json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalRunner {
    pub name: String,
    pub machine: String,
    pub labels: Vec<String>,
    pub listener_pid: Option<u32>,
    pub worker_pid: Option<u32>,
    pub dedicated: bool,
    /// `busy` (a worker runs a job), `idle` (the listener waits) or `stopped`.
    pub state: &'static str,
}

/// A runner in the pool, as GitHub sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PoolRunner {
    pub name: String,
    /// `online` or `offline`.
    pub status: String,
    pub busy: bool,
    pub labels: Vec<String>,
    /// What it runs now: `#76 rust`, when a listed run says so.
    pub job: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobView {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub runner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunView {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub branch: String,
    pub event: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub created_at: String,
    pub url: String,
    pub jobs: Vec<JobView>,
}

/// A USB audio device on this machine, and the runner label it gives.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsbDevice {
    pub machine: String,
    /// `vid:pid`, lowercase hex.
    pub id: String,
    pub name: String,
    /// `usb-vid-pid`: `runs-on` with it finds the runner that holds the device.
    pub label: String,
}

/// What `bana status-json` says about this machine.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Local {
    pub runners: Vec<LocalRunner>,
    pub usb: Vec<UsbDevice>,
}

/// `bana status-json`: one object per line, `"kind"` `runner` (the default) or `usb`.
pub fn parse_local(text: &str) -> Local {
    let values: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .collect();
    let usb = values
        .iter()
        .filter(|v| v["kind"] == "usb")
        .filter_map(|v| {
            let id = v["id"].as_str()?.to_string();
            Some(UsbDevice {
                machine: v["machine"].as_str().unwrap_or("").to_string(),
                name: v["name"].as_str().unwrap_or("").to_string(),
                label: v["label"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| format!("usb-{}", id.replace(':', "-"))),
                id,
            })
        })
        .collect();
    let runners = values
        .iter()
        .filter(|v| v["kind"].is_null() || v["kind"] == "runner")
        .filter_map(|v| {
            let pid = |k: &str| v[k].as_u64().map(|p| p as u32);
            let (listener_pid, worker_pid) = (pid("listener_pid"), pid("worker_pid"));
            Some(LocalRunner {
                name: v["name"].as_str()?.to_string(),
                machine: v["machine"].as_str().unwrap_or("").to_string(),
                labels: split_labels(v["labels"].as_str().unwrap_or("")),
                listener_pid,
                worker_pid,
                dedicated: v["dedicated"] == true,
                state: match (listener_pid, worker_pid) {
                    (_, Some(_)) => "busy",
                    (Some(_), None) => "idle",
                    (None, None) => "stopped",
                },
            })
        })
        .collect();
    Local { runners, usb }
}

fn split_labels(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// `gh api repos/R/actions/runners`.
pub fn parse_pool(v: &Value) -> Vec<PoolRunner> {
    let mut out: Vec<PoolRunner> = v["runners"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| PoolRunner {
            name: r["name"].as_str().unwrap_or("?").to_string(),
            status: r["status"].as_str().unwrap_or("?").to_string(),
            busy: r["busy"] == true,
            labels: r["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["name"].as_str().map(String::from))
                .collect(),
            job: None,
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `gh api repos/R/actions/runs`, with the jobs of those runs fetched (by run id).
pub fn parse_runs(runs: &Value, jobs: &BTreeMap<u64, Value>) -> Vec<RunView> {
    runs["workflow_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let id = r["id"].as_u64()?;
            Some(RunView {
                id,
                number: r["run_number"].as_u64().unwrap_or(0),
                title: r["display_title"].as_str().unwrap_or("").to_string(),
                branch: r["head_branch"].as_str().unwrap_or("").to_string(),
                event: r["event"].as_str().unwrap_or("").to_string(),
                status: r["status"].as_str().unwrap_or("").to_string(),
                conclusion: r["conclusion"].as_str().map(String::from),
                created_at: r["created_at"].as_str().unwrap_or("").to_string(),
                url: r["html_url"].as_str().unwrap_or("").to_string(),
                jobs: jobs
                    .get(&id)
                    .and_then(|j| j["jobs"].as_array())
                    .into_iter()
                    .flatten()
                    .map(|j| JobView {
                        name: j["name"].as_str().unwrap_or("?").to_string(),
                        status: j["status"].as_str().unwrap_or("").to_string(),
                        conclusion: j["conclusion"].as_str().map(String::from),
                        runner: j["runner_name"]
                            .as_str()
                            .filter(|n| !n.is_empty())
                            .map(String::from),
                    })
                    .collect(),
            })
        })
        .collect()
}

/// The runs whose jobs are worth fetching: those not finished, then the newest.
pub fn runs_to_detail(runs: &Value, most: usize) -> Vec<u64> {
    let all: Vec<&Value> = runs["workflow_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let mut ids: Vec<u64> = all
        .iter()
        .filter(|r| r["status"] != "completed")
        .filter_map(|r| r["id"].as_u64())
        .collect();
    for r in &all {
        if let Some(id) = r["id"].as_u64() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids.truncate(most);
    ids
}

/// Marks each busy runner in the pool with the job it runs, where a run says so.
pub fn attach_jobs(pool: &mut [PoolRunner], runs: &[RunView]) {
    for r in pool.iter_mut() {
        r.job = runs.iter().find_map(|run| {
            run.jobs
                .iter()
                .find(|j| j.status == "in_progress" && j.runner.as_deref() == Some(&r.name))
                .map(|j| format!("#{} {}", run.number, j.name))
        });
    }
}

/// A tier from bana.conf: a plain word (it becomes `-f tier=<it>`).
pub fn valid_tier(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 40
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
}

/// `owner/repo`, as GitHub spells them.
pub fn valid_repo(r: &str) -> bool {
    let mut parts = r.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && !p.starts_with(['-', '.'])
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// A workflow file name, as `gh workflow run` takes it.
pub fn valid_workflow(w: &str) -> bool {
    valid_runner(w) && (w.ends_with(".yml") || w.ends_with(".yaml"))
}

/// A git ref a run may be started on: a branch or tag name, nothing else.
pub fn valid_ref(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 100
        && !r.starts_with(['-', '/', '.'])
        && !r.contains("..")
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}

/// A runner name `bana` made (`<prefix>-<host>-linux-arm64-1`).
pub fn valid_runner(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 80
        && !n.contains("..")
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_runners_say_what_their_processes_do() {
        let text = r#"{"kind":"runner","name":"dsper-mbp-macos","machine":"mbp","dir":"/x","labels":"self-hosted,dsper-macos,usb-audio,usb-1c75-af70","listener_pid":41,"worker_pid":42,"dedicated":false}
{"kind":"runner","name":"dsper-mbp-linux-1","machine":"mbp (bana)","dir":"/y","labels":"self-hosted,dsper-linux","listener_pid":7,"worker_pid":null,"dedicated":true}
not json
{"kind":"usb","machine":"mbp","id":"1c75:af70","name":"Arturia MiniFuse 2","label":"usb-1c75-af70"}
{"name":"dsper-mbp-linux-2","machine":"mbp (bana)","dir":"/z","labels":"","listener_pid":null,"worker_pid":null,"dedicated":false}"#;
        let Local { runners: r, usb } = parse_local(text);
        let states: Vec<_> = r.iter().map(|x| (x.name.as_str(), x.state)).collect();
        assert_eq!(
            states,
            [
                ("dsper-mbp-macos", "busy"),
                ("dsper-mbp-linux-1", "idle"),
                ("dsper-mbp-linux-2", "stopped")
            ]
        );
        assert_eq!(
            r[0].labels,
            ["self-hosted", "dsper-macos", "usb-audio", "usb-1c75-af70"]
        );
        assert!(r[1].dedicated && r[2].labels.is_empty());
        assert_eq!(usb.len(), 1);
        assert_eq!(
            (
                usb[0].id.as_str(),
                usb[0].label.as_str(),
                usb[0].machine.as_str()
            ),
            ("1c75:af70", "usb-1c75-af70", "mbp")
        );
    }

    #[test]
    fn the_pool_shows_who_runs_what() {
        let pool = json!({"runners": [
            {"name": "b", "status": "online", "busy": true, "labels": [{"name": "self-hosted"}, {"name": "dsper-linux"}]},
            {"name": "a", "status": "offline", "busy": false, "labels": []}
        ]});
        let runs = json!({"workflow_runs": [
            {"id": 9, "run_number": 76, "display_title": "ci", "head_branch": "main", "event": "workflow_dispatch",
             "status": "in_progress", "conclusion": null, "created_at": "t", "html_url": "u"},
            {"id": 8, "run_number": 75, "display_title": "old", "head_branch": "main", "event": "push",
             "status": "completed", "conclusion": "success", "created_at": "t", "html_url": "u"}
        ]});
        assert_eq!(runs_to_detail(&runs, 5), [9, 8]);
        assert_eq!(runs_to_detail(&runs, 1), [9], "unfinished runs first");
        let jobs = BTreeMap::from([(
            9,
            json!({"jobs": [
                {"name": "plan", "status": "completed", "conclusion": "success", "runner_name": "b"},
                {"name": "rust", "status": "in_progress", "conclusion": null, "runner_name": "b"},
                {"name": "macos", "status": "queued", "conclusion": null, "runner_name": ""}
            ]}),
        )]);
        let runs = parse_runs(&runs, &jobs);
        assert_eq!(runs[0].jobs[2].runner, None, "not on a runner yet");
        assert!(runs[1].jobs.is_empty(), "its jobs were not fetched");
        let mut pool = parse_pool(&pool);
        assert_eq!(pool[0].name, "a", "sorted by name");
        attach_jobs(&mut pool, &runs);
        assert_eq!(pool[1].job.as_deref(), Some("#76 rust"));
        assert_eq!(pool[0].job, None);
    }

    #[test]
    fn only_plain_names_reach_a_command_line() {
        for ok in ["main", "stages", "feature/x-1", "v1.2.0"] {
            assert!(valid_ref(ok), "{ok}");
        }
        for bad in [
            "", "-f", "--help", "a..b", "a b", "a;rm", "$(x)", "/abs", ".hidden",
        ] {
            assert!(!valid_ref(bad), "{bad}");
        }
        assert!(valid_runner("dsper-mac-mini-linux-arm64-2"));
        for bad in ["", "../x", "a/b", "a b", "..", "x;y"] {
            assert!(!valid_runner(bad), "{bad}");
        }
        assert!(valid_tier("nightly") && valid_tier("hw_full"));
        for bad in ["", "a=b", "a b", "x;y"] {
            assert!(!valid_tier(bad), "{bad}");
        }
        assert!(valid_repo("tjrb-xyz/dsper") && valid_repo("o/r.js"));
        for bad in ["", "o", "o/", "/r", "o/r/x", "-o/r", "o/r;x", "o/.."] {
            assert!(!valid_repo(bad), "{bad}");
        }
        assert!(valid_workflow("ci.yml") && valid_workflow("hardware.yaml"));
        assert!(
            !valid_workflow("../ci.yml") && !valid_workflow("ci") && !valid_workflow("a b.yml")
        );
    }
}

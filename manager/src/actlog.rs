//! act's log, as the daemon reads it: act's `--json` lines in; the build's jobs
//! and steps, its result, and the commit statuses that say so out.
//!
//! Pure, like the rest of this crate's models. The daemon starts a [`Build`]
//! from the job list (`act -l`, [`parse_list`]), folds each line of act's output
//! into it ([`parse_line`], [`Build::fold`]), ends it with act's exit
//! ([`Build::finish`]), and posts what [`status_updates`] says changed. The menu
//! bar's text comes from the daemon's [`Summary`] ([`tray_view`]).
//!
//! What act (0.2.89, tests/fixtures/act) prints:
//! - one JSON object per line: `jobID` and `matrix` (a job's key), `job` (padded
//!   to the longest name), `step` with `stepID` (`stepid` for "Set up job" and
//!   "Complete job"), `stage` (Pre, Main, Post), `raw_output` for a step's own
//!   output, `stepResult` with `executionTime` (ns), and `jobResult` last;
//! - for a matrix entry, the `jobResult` of its matrix so far: an entry that
//!   passes after another failed says `failure` (tests/fixtures/act/matrix-fail);
//! - a composite action's inner steps under their parent's `step`, with a longer
//!   `stepID`;
//! - "Skipping unsupported platform" for a job whose `runs-on` has no platform;
//! - nothing for a job skipped by `if:` or blocked by a failed `needs`: only
//!   `act -l` knows those;
//! - with `-v`, `debug` lines as well, some from jobs that never run (one
//!   skipped by `if:` gets a `jobResult` of `skipped`);
//! - plain text on stderr at the end: `Error: Job 'lint' failed`,
//!   `Error: workflow is not valid. …`.
//!
//! A builder other than act (a custom one, later) has two ways in. It can
//! print the subset of these lines the daemon reads: `jobID`, `matrix`,
//! `step`, `stepID`, `stage`, `msg` with `raw_output`, `stepResult` with
//! `executionTime`, `jobResult`, and optionally `command` (`summary`, `error`,
//! `warning`, `notice`). The fold, the statuses, the page and the log then work
//! as they do for act. Or it writes the build's results.jsonl itself
//! ([`crate::results`], `schema` 1) into the build's directory before the
//! build ends: the daemon takes it as it is, with build.json's ref, commit,
//! tier and times. It is all the CI report ([`crate::report`]) is made from:
//! one JSON object per line, by `kind`:
//! - `build`: schema, builder (`act 0.2.89`), bana, repo, ref, sha, tier,
//!   machine, network, trigger, started, ended, result;
//! - `job`: key, job (its id), matrix, result (success, failure, skipped,
//!   unsupported, not_planned, cancelled, unknown), ms;
//! - `step`: key, step, stage, result, ms, owner (project, bana, act), continued;
//! - `tests`: key, step, tool, passed, failed, skipped, incomplete (go's
//!   counts are packages, shown apart);
//! - `test`: key, step, name, result, binary, at, message;
//! - `rerun`: key, step, target;
//! - `annotation`: key, step, level, message, title, file, line, col;
//! - `notice`: key, step, text, left_out (true leaves its step out, as
//!   report.left_out does), title, file, line, col;
//! - `summary`: key, step, markdown;
//! - `tail`: key, step, lines;
//! - `error`: owner, text, key, step.
//!
//! docs/DAEMON.md has the same, for a project; report.rs's example fixture
//! (tests/fixtures/report/example.jsonl) is one written by hand.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// GitHub's limit for a status description.
pub const DESCRIPTION_MAX: usize = 140;
/// A step name in a description is cut to this, so the rest still fits.
const STEP_MAX: usize = 60;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildState {
    #[default]
    Queued,
    Running,
    Success,
    /// A job failed.
    Failure,
    /// Cancelled, superseded, timed out, interrupted, or it could not start.
    Error,
}

impl BuildState {
    pub fn finished(self) -> bool {
        matches!(self, Self::Success | Self::Failure | Self::Error)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// In `act -l`, and no line from it yet.
    #[default]
    Waiting,
    Running,
    Success,
    Failure,
    /// It never ran: skipped by `if:`, or a job it needs failed.
    Skipped,
    /// Its `runs-on` has no platform here (a macOS job on Linux): not run here.
    Unsupported,
    /// Stopped by a cancel, or by act's end.
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Step {
    /// `Pre`, `Main` or `Post`; empty for "Set up job" and "Complete job".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stage: String,
    /// act's id for it: the workflow's `id:`, its index, or `--setup-job`.
    pub id: String,
    pub name: String,
    /// `success`, `failure`, `skipped`, or `cancelled` when act stopped first;
    /// none while it runs.
    pub result: Option<String>,
    pub ms: Option<u64>,
    /// It failed under `continue-on-error`, so the job went on.
    #[serde(skip_serializing_if = "is_false")]
    pub continued: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One job of the build, or one entry of a job's matrix.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    /// `rust`, or `package (linux-arm64)` for a matrix entry: the end of its
    /// status context.
    pub key: String,
    /// The workflow's job id.
    pub id: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub matrix: BTreeMap<String, String>,
    /// Its stage in `act -l`, 0 first.
    pub stage: Option<u32>,
    pub state: JobState,
    /// Unix seconds of its first line and of its result.
    pub started: Option<i64>,
    pub ended: Option<i64>,
    pub steps: Vec<Step>,
    /// The step that failed it.
    pub failed_step: Option<String>,
}

impl Job {
    /// The step it runs now.
    pub fn current_step(&self) -> Option<&str> {
        if self.state != JobState::Running {
            return None;
        }
        self.steps
            .iter()
            .rev()
            .find(|s| s.result.is_none())
            .map(|s| s.name.as_str())
    }
}

/// A job's key: its id, then its matrix values in key order, the way GitHub
/// names a matrix job.
pub fn job_key(id: &str, matrix: &BTreeMap<String, String>) -> String {
    if matrix.is_empty() {
        return id.to_string();
    }
    let values: Vec<&str> = matrix.values().map(String::as_str).collect();
    format!("{id} ({})", values.join(", "))
}

/// One line of act's output (or the daemon's own, in act.jsonl).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Job(Box<JobLine>),
    /// Text outside any job: act's `Error: …` at the end, bana's own lines.
    Text {
        msg: String,
        error: bool,
    },
    /// The daemon's mark in act.jsonl, `{"bana":"cancel","msg":REASON}`: a
    /// cancel began, so jobs that end from here on were cancelled.
    Cancel(String),
    /// act's other lines (its Docker host, the artifact server).
    Other,
}

/// A line from one job.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobLine {
    pub key: String,
    pub id: String,
    pub matrix: BTreeMap<String, String>,
    /// act's name for the job, `ci/rust` (trimmed: act pads it).
    pub name: String,
    /// Unix seconds.
    pub time: Option<i64>,
    pub stage: String,
    /// The step's id; for a composite action's inner steps, their parent's.
    pub step_id: Option<String>,
    /// From a composite action's inner step: its result is not the step's.
    pub inner: bool,
    /// The step's name, as [`Step::name`].
    pub step: Option<String>,
    pub msg: String,
    /// The step's own output (`raw_output`), not act's notes about it.
    pub output: bool,
    pub level: String,
    pub step_result: Option<String>,
    pub ms: Option<u64>,
    pub job_result: Option<String>,
}

impl JobLine {
    /// act found no platform for the job's `runs-on`, so it will not run here.
    pub fn unsupported(&self) -> bool {
        self.msg.contains("Skipping unsupported platform")
    }
}

/// One line of act's `--json` output, or a line the daemon wrote beside them in
/// act.jsonl (`{"msg": …, "bana": "stderr"}`, or the cancel mark).
pub fn parse_line(line: &str) -> Event {
    let line = line.trim_end();
    if line.trim().is_empty() {
        return Event::Other;
    }
    let v: Value = match serde_json::from_str(line) {
        Ok(v @ Value::Object(_)) => v,
        _ => return text(line),
    };
    let msg = v["msg"].as_str().unwrap_or("");
    if let Some(kind) = v["bana"].as_str() {
        return match kind {
            "cancel" => Event::Cancel(msg.to_string()),
            _ => text(msg),
        };
    }
    let level = v["level"].as_str().unwrap_or("");
    let Some(id) = v["jobID"].as_str() else {
        return match level {
            "error" | "fatal" | "panic" => Event::Text {
                msg: msg.to_string(),
                error: true,
            },
            _ => Event::Other,
        };
    };
    let matrix: BTreeMap<String, String> = v["matrix"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, x)| {
            (
                k.clone(),
                x.as_str().map_or_else(|| x.to_string(), String::from),
            )
        })
        .collect();
    let ids = v["stepID"].as_array().or_else(|| v["stepid"].as_array());
    let stage = v["stage"].as_str().unwrap_or("").to_string();
    let step = v["step"].as_str().map(|s| step_name(&stage, s));
    Event::Job(Box::new(JobLine {
        key: job_key(id, &matrix),
        id: id.to_string(),
        matrix,
        name: v["job"].as_str().unwrap_or("").trim().to_string(),
        time: v["time"].as_str().and_then(parse_time),
        step_id: ids
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(String::from),
        inner: ids.is_some_and(|a| a.len() > 1),
        stage,
        step,
        msg: msg.to_string(),
        output: v["raw_output"] == true,
        level: level.to_string(),
        step_result: v["stepResult"].as_str().map(String::from),
        ms: v["executionTime"].as_u64().map(|ns| ns / 1_000_000),
        job_result: v["jobResult"].as_str().map(String::from),
    }))
}

/// What act.jsonl keeps of one line act (or bana) printed: act's JSON as it is,
/// anything else wrapped as `{"msg": …, "bana": "stderr"}`.
pub fn log_line(raw: &str) -> String {
    let raw = raw.trim_end_matches(['\r', '\n']);
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(_)) => raw.to_string(),
        _ => serde_json::json!({"msg": raw, "bana": "stderr"}).to_string(),
    }
}

/// The daemon's mark in act.jsonl when a cancel begins ([`Event::Cancel`]).
pub fn cancel_mark(reason: &str) -> String {
    serde_json::json!({"bana": "cancel", "msg": reason}).to_string()
}

/// A plain line: bana's `die` prints red, act's own errors start `Error: `.
fn text(raw: &str) -> Event {
    let msg = strip_ansi(raw);
    Event::Text {
        error: raw.contains("\x1b[31m") || msg.starts_with("Error: "),
        msg,
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
        } else if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// A step's name as GitHub shows it: `Post <name>` for a post step, and only
/// the first line of an unnamed multi-line `run:`.
fn step_name(stage: &str, name: &str) -> String {
    let name = name.lines().next().unwrap_or("").trim();
    match stage {
        "Pre" | "Post" => format!("{stage} {name}"),
        _ => name.to_string(),
    }
}

/// `act -l`: its stages and job ids, in its order. act prints a table (the
/// header, then `STAGE JOB-ID NAME …` rows); any other line is skipped.
pub fn parse_list(text: &str) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(stage), Some(id)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(stage) = stage.parse::<u32>() else {
            continue;
        };
        let plain = id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c));
        if plain && !out.iter().any(|(_, j)| j == id) {
            out.push((stage, id.to_string()));
        }
    }
    out
}

/// What act did in one build: its state, and every job with its steps. The
/// daemon keeps it in build.json, beside what it knows itself (the ref, the
/// sha, the statuses posted).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Build {
    pub state: BuildState,
    /// Why it ended in error: `cancelled from the menu bar`, `could not start: …`.
    pub reason: Option<String>,
    /// Unix seconds.
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    /// Set when a cancel began, with its reason.
    pub cancel_requested: Option<String>,
    /// The last error act or bana printed outside a job.
    pub last_error: Option<String>,
    /// In `act -l`'s order by stage; a matrix job's entries take its place.
    pub jobs: Vec<Job>,
}

impl Build {
    /// A build starting now, with every job from `act -l` waiting.
    pub fn new(list: &[(u32, String)], at: i64) -> Self {
        let mut list = list.to_vec();
        list.sort_by_key(|(stage, _)| *stage);
        Self {
            state: BuildState::Running,
            started_at: Some(at),
            jobs: list
                .into_iter()
                .map(|(stage, id)| Job {
                    key: id.clone(),
                    id,
                    stage: Some(stage),
                    ..Job::default()
                })
                .collect(),
            ..Self::default()
        }
    }

    /// Folds every line of `text` (act.jsonl, read again after a restart).
    pub fn fold_lines(&mut self, text: &str, now: i64) {
        for line in text.lines() {
            self.fold(&parse_line(line), now);
        }
    }

    /// Folds one event in. `now` stands in for a line without act's time.
    pub fn fold(&mut self, ev: &Event, now: i64) {
        match ev {
            Event::Job(l) => self.fold_job(l, now),
            Event::Text { msg, error: true } => {
                let msg = msg.strip_prefix("Error: ").unwrap_or(msg).trim();
                if !msg.is_empty() {
                    self.last_error = Some(msg.to_string());
                }
            }
            Event::Cancel(reason) => self.cancel(reason),
            Event::Text { .. } | Event::Other => {}
        }
    }

    /// A cancel began (the first reason stays): a job that fails from now on
    /// was cancelled.
    pub fn cancel(&mut self, reason: &str) {
        if self.cancel_requested.is_none() {
            self.cancel_requested = Some(reason.to_string());
        }
    }

    fn fold_job(&mut self, l: &JobLine, now: i64) {
        // act -v's debug lines say nothing of a job's state, and come for jobs
        // that never run.
        if matches!(l.level.as_str(), "debug" | "trace") {
            return;
        }
        let at = l.time.unwrap_or(now);
        let cancelling = self.cancel_requested.is_some();
        let i = self.job_index(l);
        let job = &mut self.jobs[i];
        if l.unsupported() {
            if job.state == JobState::Waiting {
                job.state = JobState::Unsupported;
            }
            return;
        }
        if job.state == JobState::Waiting {
            job.state = JobState::Running;
        }
        job.started.get_or_insert(at);
        if let (Some(id), Some(name)) = (&l.step_id, &l.step) {
            let s = match job
                .steps
                .iter()
                .position(|s| s.id == *id && s.stage == l.stage)
            {
                Some(s) => s,
                None => {
                    job.steps.push(Step {
                        stage: l.stage.clone(),
                        id: id.clone(),
                        name: name.clone(),
                        ..Step::default()
                    });
                    job.steps.len() - 1
                }
            };
            let step = &mut job.steps[s];
            if !l.inner {
                if l.msg.trim() == "Failed but continue next step" {
                    step.continued = true;
                }
                if let Some(r) = &l.step_result {
                    step.result = Some(r.clone());
                    step.ms = l.ms;
                    if r == "failure" && !step.continued && job.failed_step.is_none() {
                        job.failed_step = Some(step.name.clone());
                    }
                }
            }
        }
        if let Some(r) = &l.job_result {
            job.ended = Some(at);
            job.state = match r.as_str() {
                "success" => JobState::Success,
                _ if cancelling => JobState::Cancelled,
                // A matrix entry gets its matrix's result so far: one that failed
                // no step of its own passed.
                _ if !job.matrix.is_empty() && job.failed_step.is_none() => JobState::Success,
                _ => JobState::Failure,
            };
        }
    }

    /// The job a line is from. A matrix entry's first line takes its job's place
    /// from `act -l`, or goes after the entries before it.
    fn job_index(&mut self, l: &JobLine) -> usize {
        if let Some(i) = self.jobs.iter().position(|j| j.key == l.key) {
            return i;
        }
        let stage = self
            .jobs
            .iter()
            .find(|j| j.id == l.id)
            .and_then(|j| j.stage);
        let job = Job {
            key: l.key.clone(),
            id: l.id.clone(),
            matrix: l.matrix.clone(),
            stage,
            ..Job::default()
        };
        let listed = self
            .jobs
            .iter()
            .position(|j| j.id == l.id && j.matrix.is_empty() && j.state == JobState::Waiting);
        if let Some(i) = listed {
            self.jobs[i] = job;
            return i;
        }
        let i = match self.jobs.iter().rposition(|j| j.id == l.id) {
            Some(i) => i + 1,
            None => self.jobs.len(),
        };
        self.jobs.insert(i, job);
        i
    }

    /// act has exited (`exit` none: killed by a signal). With a cancel (asked
    /// for now or earlier), the build ends in error with its reason; otherwise
    /// it failed if a job failed, passed if act exited 0, and else ended in
    /// error with act's last error. Jobs that never printed a line were skipped
    /// (or, when the build ended in error, cancelled); jobs still running were
    /// cancelled.
    pub fn finish(&mut self, exit: Option<i32>, cancel: Option<&str>, at: i64) -> BuildState {
        if let Some(r) = cancel {
            self.cancel(r);
        }
        let any = |s: JobState| self.jobs.iter().any(|j| j.state == s);
        let started = self.jobs.iter().any(|j| j.started.is_some());
        let (state, reason) = if let Some(r) = &self.cancel_requested {
            (BuildState::Error, Some(r.clone()))
        } else if any(JobState::Failure) {
            (BuildState::Failure, None)
        } else if exit == Some(0) && !any(JobState::Running) {
            (BuildState::Success, None)
        } else {
            let why = match (&self.last_error, exit) {
                (Some(e), _) => e.clone(),
                (None, Some(n)) => format!("act exited with {n}"),
                (None, None) => "act was killed".to_string(),
            };
            let why = if started {
                why
            } else {
                format!("could not start: {why}")
            };
            (BuildState::Error, Some(why))
        };
        for j in &mut self.jobs {
            match j.state {
                JobState::Waiting if state == BuildState::Error => j.state = JobState::Cancelled,
                JobState::Waiting => j.state = JobState::Skipped,
                JobState::Running => {
                    j.state = JobState::Cancelled;
                    j.ended = Some(at);
                    for s in j.steps.iter_mut().filter(|s| s.result.is_none()) {
                        s.result = Some("cancelled".into());
                    }
                }
                _ => {}
            }
        }
        (self.state, self.reason, self.ended_at) = (state, reason, Some(at));
        state
    }

    /// Why it stopped, for the build and for each job it cut short.
    pub fn stop_reason(&self) -> &str {
        self.reason
            .as_deref()
            .or(self.cancel_requested.as_deref())
            .unwrap_or("stopped before it finished")
    }

    /// Each job's state for the page, with the step a running one is at.
    pub fn chips(&self) -> Vec<JobChip> {
        self.jobs
            .iter()
            .map(|j| JobChip {
                key: j.key.clone(),
                state: j.state,
                step: j.current_step().map(String::from),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusState {
    Pending,
    Success,
    Failure,
    Error,
}

impl StatusState {
    /// As GitHub's statuses API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Error => "error",
        }
    }
}

/// A commit status: what `POST repos/R/statuses/SHA` sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub context: String,
    pub state: StatusState,
    pub description: String,
}

/// How a build's statuses are named and described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// `bana`, or `bana nightly` for a build at another tier than pushes run.
    pub context: String,
    /// This machine, as the descriptions name it (`mbp`).
    pub machine: String,
    pub tier: String,
}

impl Report {
    /// Builds at `push_tier` post plain `bana` contexts. Others (tags, a manual
    /// nightly) post `bana <tier>`, so they never overwrite a push's result.
    pub fn new(tier: &str, push_tier: &str, machine: &str) -> Self {
        Self {
            context: if tier.is_empty() || tier == push_tier {
                "bana".to_string()
            } else {
                format!("bana {tier}")
            },
            machine: machine.to_string(),
            tier: tier.to_string(),
        }
    }

    /// What the build's own status says: `passed on mbp in 12m · 5 jobs`.
    pub fn describe(&self, b: &Build) -> Option<(StatusState, String)> {
        let took = duration(span(b.started_at, b.ended_at));
        let count = |s: JobState| b.jobs.iter().filter(|j| j.state == s).count();
        let (state, d) = match b.state {
            BuildState::Queued => return None,
            BuildState::Running if self.tier.is_empty() => {
                (StatusState::Pending, format!("running on {}", self.machine))
            }
            BuildState::Running => (
                StatusState::Pending,
                format!("running on {} ({})", self.machine, self.tier),
            ),
            BuildState::Success => {
                let (ran, skipped) = (count(JobState::Success), count(JobState::Skipped));
                let mut d = format!(
                    "passed on {} in {took} · {ran} job{}",
                    self.machine,
                    if ran == 1 { "" } else { "s" }
                );
                if skipped > 0 {
                    d += &format!(", {skipped} skipped");
                }
                let away: Vec<&str> = b
                    .jobs
                    .iter()
                    .filter(|j| j.state == JobState::Unsupported)
                    .map(|j| j.key.as_str())
                    .collect();
                if !away.is_empty() {
                    d += &format!("; not run here: {}", away.join(", "));
                }
                (StatusState::Success, d)
            }
            BuildState::Failure => {
                let failed: Vec<(usize, &Job)> = b
                    .jobs
                    .iter()
                    .enumerate()
                    .filter(|(_, j)| j.state == JobState::Failure)
                    .collect();
                let first = failed
                    .iter()
                    .min_by_key(|(i, j)| (j.ended.unwrap_or(i64::MAX), *i))
                    .map(|(_, j)| *j);
                let mut d = match first {
                    Some(j) => match &j.failed_step {
                        Some(s) => format!("{} failed at \"{}\"", j.key, cut(s, STEP_MAX)),
                        None => format!("{} failed", j.key),
                    },
                    None => "failed".to_string(),
                };
                if failed.len() > 1 {
                    d += &format!(", +{} more", failed.len() - 1);
                }
                d += &format!(" · {took} on {}", self.machine);
                (StatusState::Failure, d)
            }
            BuildState::Error => (StatusState::Error, b.stop_reason().to_string()),
        };
        Some((state, cut(&d, DESCRIPTION_MAX)))
    }

    fn job(&self, j: &Job, b: &Build) -> Option<(StatusState, String)> {
        j.started?;
        let took = duration(span(j.started, j.ended));
        let (state, d) = match j.state {
            JobState::Waiting | JobState::Running => (StatusState::Pending, "running".to_string()),
            JobState::Success => (StatusState::Success, format!("passed in {took}")),
            JobState::Failure => (
                StatusState::Failure,
                match &j.failed_step {
                    Some(s) => format!("failed at \"{}\" after {took}", cut(s, STEP_MAX)),
                    None => format!("failed after {took}"),
                },
            ),
            JobState::Cancelled => (StatusState::Error, b.stop_reason().to_string()),
            JobState::Skipped | JobState::Unsupported => return None,
        };
        Some((state, cut(&d, DESCRIPTION_MAX)))
    }
}

/// Every status a build has now: `bana` pending while it runs, `bana/<job>` for
/// each job that started (jobs that never ran get none), and `bana` final at
/// the end. A queued build has none.
pub fn statuses(b: &Build, r: &Report) -> Vec<Status> {
    let status = |context: String, (state, description): (StatusState, String)| Status {
        context,
        state,
        description,
    };
    let own = r
        .describe(b)
        .map(|s| status(r.context.clone(), s))
        .into_iter();
    let jobs = b.jobs.iter().filter_map(|j| {
        r.job(j, b)
            .map(|s| status(format!("{}/{}", r.context, j.key), s))
    });
    if b.state.finished() {
        jobs.chain(own).collect()
    } else {
        own.chain(jobs).collect()
    }
}

/// The statuses to post after `before` became `after`, in order: those that
/// are new or changed.
pub fn status_updates(before: &Build, after: &Build, r: &Report) -> Vec<Status> {
    let old = statuses(before, r);
    statuses(after, r)
        .into_iter()
        .filter(|s| !old.contains(s))
        .collect()
}

fn span(from: Option<i64>, to: Option<i64>) -> i64 {
    match (from, to) {
        (Some(a), Some(b)) => (b - a).max(0),
        _ => 0,
    }
}

/// `45s`, `3m12s`, `12m` (seconds dropped from ten minutes), `2h05m`.
pub fn duration(secs: i64) -> String {
    let s = secs.max(0);
    let (h, m) = (s / 3600, s % 3600 / 60);
    match s {
        0..=59 => format!("{s}s"),
        60..=599 if s % 60 == 0 => format!("{m}m"),
        60..=599 => format!("{m}m{}s", s % 60),
        600..=3599 => format!("{m}m"),
        _ => format!("{h}h{m:02}m"),
    }
}

/// `s`, at most `max` characters: a longer one ends in `…`.
pub fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// Unix seconds from act's `time` (RFC 3339 in the machine's zone:
/// `2026-09-28T13:00:54Z`, `2026-09-28T15:00:54+02:00`).
pub fn parse_time(s: &str) -> Option<i64> {
    let num = |a: usize, b: usize| digits(s.get(a..b)?);
    let b = s.as_bytes();
    let seps = [(4, b'-'), (7, b'-'), (13, b':'), (16, b':')];
    if b.len() < 20 || !seps.iter().all(|&(i, c)| b[i] == c) || !matches!(b[10], b'T' | b't' | b' ')
    {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut zone = s.get(19..)?;
    if let Some(frac) = zone.strip_prefix('.') {
        zone = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match zone {
        "Z" | "z" => 0,
        _ if zone.len() == 6 && zone.as_bytes()[3] == b':' => {
            let sign = match zone.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            sign * (digits(zone.get(1..3)?)? * 3600 + digits(zone.get(4..6)?)? * 60)
        }
        _ => return None,
    };
    // Days since 1970-01-01 in the proleptic Gregorian calendar (Hinnant's days_from_civil).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((mo + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// Plain digits as a number (`str::parse` also takes a sign).
fn digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

// ---- the daemon's summary, and the menu bar's view of it --------------------

/// What the daemon is doing: `GET /ci/v1/local`, and what the menu bar shows.
/// The daemon fills it; [`tray_view`] reads it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Summary {
    /// `owner/repo`.
    pub repo: String,
    /// bana.conf's prefix: the project's name here (`example`).
    pub prefix: String,
    pub machine: String,
    /// Unix seconds when it was made; ages count from it.
    pub now: i64,
    pub watcher: Watcher,
    /// The build act runs now.
    pub running: Option<BuildView>,
    /// Builds waiting to start, the next first.
    pub queue: Vec<QueuedView>,
    /// The newest finished build.
    pub last: Option<BuildView>,
    /// The newest build that failed, unless one since passed (builds that
    /// ended in error are passed over): the menu bar's "Fix #N with Claude…".
    pub failed: Option<u64>,
}

/// The daemon's view of GitHub and of this machine.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Watcher {
    /// Unix seconds of the last fetch that worked.
    pub fetched_at: Option<i64>,
    /// Why the last fetch failed (offline, just woke).
    pub fetch_error: Option<String>,
    /// New builds wait; fetching goes on.
    pub paused: bool,
    /// `docker info` answered, last time it was asked.
    pub docker: bool,
    /// The label of a `bana ci` holding ~/.bana/act.lock, when it isn't ours.
    pub lock_holder: Option<String>,
    /// Statuses not posted yet.
    pub unposted: usize,
    /// Why the last post failed (`gh is signed out`).
    pub post_error: Option<String>,
}

/// A running or finished build, as the page and the menu bar show it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct BuildView {
    pub id: u64,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub sha: String,
    pub tier: String,
    /// `push`, `manual`, `rerun` or `retry`.
    pub trigger: String,
    pub attempt: u32,
    pub state: BuildState,
    pub reason: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    /// Seconds it has run while the machine was awake.
    pub elapsed: u64,
    /// What its `bana` status says ([`Report::describe`]).
    pub description: String,
    pub jobs: Vec<JobChip>,
    /// The history's chip from its CI report (`tests 95%`), once it ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobChip {
    pub key: String,
    pub state: JobState,
    /// The step it runs now.
    pub step: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct QueuedView {
    pub id: u64,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub sha: String,
    pub tier: String,
    pub trigger: String,
    pub queued_at: i64,
    /// Why it waits, when not just for the running build: `paused`,
    /// `waiting for Docker`, `waiting for your bana ci`.
    pub waiting: Option<String>,
}

/// The menu bar's icon: a brick, as the status item's title (no image).
pub const BRICK: &str = "🧱";

/// The menu bar's title, tooltip and menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayView {
    /// The status item's whole title: the brick, then what it is doing
    /// (`🧱 4m +2`, `🧱 paused`); the brick alone when idle.
    pub title: String,
    pub tooltip: String,
    /// The menu's first line (disabled).
    pub status_line: String,
    /// `Last: passed main 1a2b3c4 · 12 min ago`, once a build has finished.
    pub last_line: Option<String>,
    pub cancel_enabled: bool,
    /// The check mark on "Pause new builds".
    pub paused: bool,
    /// The build a left click opens: the running one, else the latest.
    pub open_build: Option<u64>,
    /// `Fix #41 with Claude…`, and its build, while the last build failed.
    pub fix_line: Option<String>,
    pub fix_build: Option<u64>,
}

pub fn tray_view(s: &Summary) -> TrayView {
    let w = &s.watcher;
    let n = s.queue.len();
    let gate = if w.paused {
        Some("paused")
    } else if n > 0 && !w.docker {
        Some("waiting for Docker")
    } else if n > 0 && w.lock_holder.is_some() {
        Some("waiting for your bana ci")
    } else {
        None
    };
    let last_failed = s
        .last
        .as_ref()
        .is_some_and(|b| matches!(b.state, BuildState::Failure | BuildState::Error));
    let doing = match &s.running {
        Some(b) if n > 0 => format!("{} +{n}", minutes(b.elapsed)),
        Some(b) => minutes(b.elapsed),
        None if w.paused => "paused".into(),
        None if n > 0 && !w.docker => "no Docker".into(),
        None if n > 0 && w.lock_holder.is_some() => "busy".into(),
        None if w.post_error.is_some() => "!gh".into(),
        None if last_failed => "!".into(),
        None => String::new(),
    };
    let title = match doing.as_str() {
        "" => BRICK.to_string(),
        d => format!("{BRICK} {d}"),
    };
    let queued = (n > 0).then(|| format!("{n} queued"));
    let mut tip: Vec<String> = Vec::new();
    let status_line = match &s.running {
        Some(b) => {
            let at = format!("{} {}", b.git_ref, short_sha(&b.sha));
            let jobs: Vec<&str> = b
                .jobs
                .iter()
                .filter(|j| j.state == JobState::Running)
                .map(|j| j.key.as_str())
                .collect();
            let jobs = (!jobs.is_empty()).then(|| jobs.join(", "));
            tip.push(format!("bana: building {} {at} ({})", s.prefix, b.tier));
            tip.extend(jobs.clone());
            tip.push(minutes(b.elapsed));
            tip.extend(queued);
            if w.paused {
                tip.push("new builds paused".into());
            }
            let mut line = format!("{}: building {at}", s.prefix);
            if let Some(j) = jobs {
                line += &format!(" · {j}");
            }
            line
        }
        None => {
            tip.push(format!("bana: {} {}", s.prefix, gate.unwrap_or("idle")));
            tip.extend(queued);
            if let Some(b) = &s.last {
                tip.push(format!(
                    "last: {} {} {}, {}",
                    outcome(b),
                    b.git_ref,
                    short_sha(&b.sha),
                    ago(s.now, b.ended_at)
                ));
            }
            match (gate, n) {
                (Some(g), 0) => format!("{}: {g}", s.prefix),
                (Some(g), _) => format!("{}: {g} ({n} queued)", s.prefix),
                (None, 0) => format!("{}: idle", s.prefix),
                (None, _) => format!("{}: {n} queued", s.prefix),
            }
        }
    };
    if let Some(e) = &w.post_error {
        tip.push(format!("statuses not posted: {e}"));
    }
    if let Some(e) = &w.fetch_error {
        tip.push(format!("fetch failed: {e}"));
    }
    TrayView {
        title,
        tooltip: tip.join(" · "),
        status_line,
        last_line: s.last.as_ref().map(|b| {
            format!(
                "Last: {} {} {} · {}",
                outcome(b),
                b.git_ref,
                short_sha(&b.sha),
                ago(s.now, b.ended_at)
            )
        }),
        cancel_enabled: s.running.is_some(),
        paused: w.paused,
        open_build: s.running.as_ref().or(s.last.as_ref()).map(|b| b.id),
        fix_line: s.failed.map(|id| format!("Fix #{id} with Claude…")),
        fix_build: s.failed,
    }
}

fn outcome(b: &BuildView) -> &'static str {
    match b.state {
        BuildState::Success => "passed",
        BuildState::Failure => "failed",
        BuildState::Error => "error",
        BuildState::Queued => "queued",
        BuildState::Running => "running",
    }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Awake time in whole minutes, for the title: `4m`, `1h05m`.
fn minutes(secs: u64) -> String {
    let m = secs / 60;
    if m < 60 {
        format!("{m}m")
    } else {
        format!("{}h{:02}m", m / 60, m % 60)
    }
}

fn ago(now: i64, then: Option<i64>) -> String {
    let Some(then) = then else {
        return "a while ago".into();
    };
    match (now - then).max(0) {
        0..=59 => "just now".into(),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s @ 3600..=86_399 => format!("{} h ago", s / 3600),
        s if s < 2 * 86_400 => "1 day ago".into(),
        s => format!("{} days ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use JobState::{Cancelled, Failure, Skipped, Success, Unsupported};

    /// A recorded run (tests/fixtures/act): act 0.2.89's `--json` output, stdout
    /// and stderr as they came, run the way the daemon runs it; and `act -l`.
    /// workflows/ has what produced them.
    macro_rules! fixture {
        ($name:literal) => {
            (
                include_str!(concat!("../tests/fixtures/act/", $name, ".jsonl")),
                include_str!(concat!("../tests/fixtures/act/", $name, ".list")),
            )
        };
    }

    fn first_and_last_time(text: &str) -> (i64, i64) {
        let times: Vec<i64> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["time"].as_str().and_then(parse_time))
            .collect();
        (times[0], *times.last().unwrap())
    }

    /// Plays a run through the model as the daemon would: start with `act -l`,
    /// fold each line, post what changed, finish with act's exit. `cancel`
    /// writes the daemon's cancel mark after the first line containing its
    /// first half. Gives the build, each context's statuses in order, and the
    /// act.jsonl the daemon would have kept.
    fn play(
        (jsonl, list): (&str, &str),
        cancel: Option<(&str, &str)>,
        exit: Option<i32>,
        reason: Option<&str>,
    ) -> (Build, BTreeMap<String, Vec<String>>, String) {
        let r = Report::new("quick", "quick", "mbp");
        let (t0, t1) = first_and_last_time(jsonl);
        let mut posted: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut post = |before: &Build, after: &Build| {
            for s in status_updates(before, after, &r) {
                assert!(s.description.chars().count() <= DESCRIPTION_MAX);
                posted.entry(s.context).or_default().push(format!(
                    "{}: {}",
                    s.state.as_str(),
                    s.description
                ));
            }
        };
        let mut b = Build::new(&parse_list(list), t0);
        post(&Build::default(), &b);
        let mut log = String::new();
        let mut lines: Vec<String> = jsonl.lines().map(String::from).collect();
        if let Some((after, why)) = cancel {
            let at = lines.iter().position(|l| l.contains(after)).unwrap();
            lines.insert(at + 1, cancel_mark(why));
        }
        for line in &lines {
            let before = b.clone();
            b.fold(&parse_line(line), t0);
            post(&before, &b);
            log += &log_line(line);
            log.push('\n');
        }
        let before = b.clone();
        b.finish(exit, reason, t1);
        post(&before, &b);
        (b, posted, log)
    }

    struct Case {
        name: &'static str,
        run: (&'static str, &'static str),
        cancel: Option<(&'static str, &'static str)>,
        exit: Option<i32>,
        reason: Option<&'static str>,
        state: BuildState,
        /// Each job: key, state, failed step.
        jobs: &'static [(&'static str, JobState, Option<&'static str>)],
        /// Each context's statuses, in the order they were posted.
        statuses: &'static [(&'static str, &'static [&'static str])],
    }

    const RUNNING: &str = "pending: running on mbp (quick)";
    const JOB_RUNNING: &str = "pending: running";

    fn cases() -> Vec<Case> {
        vec![
            Case {
                name: "a container job, then a host job that needs it",
                run: fixture!("pass"),
                cancel: None,
                exit: Some(0),
                reason: None,
                state: BuildState::Success,
                jobs: &[("plan", Success, None), ("rust", Success, None)],
                statuses: &[
                    ("bana", &[RUNNING, "success: passed on mbp in 1s · 2 jobs"]),
                    ("bana/plan", &[JOB_RUNNING, "success: passed in 1s"]),
                    ("bana/rust", &[JOB_RUNNING, "success: passed in 0s"]),
                ],
            },
            Case {
                name: "a host job failing at exit 3, and the job that needs it",
                run: fixture!("fail"),
                cancel: None,
                exit: Some(1),
                reason: None,
                state: BuildState::Failure,
                jobs: &[
                    ("lint", Failure, Some("cargo clippy")),
                    ("web", Success, None),
                    ("package", Skipped, None),
                ],
                statuses: &[
                    (
                        "bana",
                        &[
                            RUNNING,
                            "failure: lint failed at \"cargo clippy\" · 1s on mbp",
                        ],
                    ),
                    (
                        "bana/lint",
                        &[JOB_RUNNING, "failure: failed at \"cargo clippy\" after 0s"],
                    ),
                    ("bana/web", &[JOB_RUNNING, "success: passed in 1s"]),
                ],
            },
            Case {
                name: "a 2-entry matrix",
                run: fixture!("matrix"),
                cancel: None,
                exit: Some(0),
                reason: None,
                state: BuildState::Success,
                jobs: &[
                    ("package (linux-arm64)", Success, None),
                    ("package (linux-x64)", Success, None),
                ],
                statuses: &[
                    ("bana", &[RUNNING, "success: passed on mbp in 0s · 2 jobs"]),
                    (
                        "bana/package (linux-arm64)",
                        &[JOB_RUNNING, "success: passed in 0s"],
                    ),
                    (
                        "bana/package (linux-x64)",
                        &[JOB_RUNNING, "success: passed in 0s"],
                    ),
                ],
            },
            Case {
                name: "a matrix entry that passes after another failed (act says failure)",
                run: fixture!("matrix-fail"),
                cancel: None,
                exit: Some(1),
                reason: None,
                state: BuildState::Failure,
                jobs: &[
                    ("package (a)", Failure, Some("build")),
                    ("package (b)", Success, None),
                ],
                statuses: &[
                    (
                        "bana",
                        &[
                            RUNNING,
                            "failure: package (a) failed at \"build\" · 3s on mbp",
                        ],
                    ),
                    (
                        "bana/package (a)",
                        &[JOB_RUNNING, "failure: failed at \"build\" after 0s"],
                    ),
                    ("bana/package (b)", &[JOB_RUNNING, "success: passed in 3s"]),
                ],
            },
            Case {
                name: "jobs skipped by if:",
                run: fixture!("skip"),
                cancel: None,
                exit: Some(0),
                reason: None,
                state: BuildState::Success,
                jobs: &[
                    ("plan", Success, None),
                    ("nightly", Skipped, None),
                    ("web", Skipped, None),
                    ("rust", Success, None),
                ],
                statuses: &[
                    (
                        "bana",
                        &[RUNNING, "success: passed on mbp in 0s · 2 jobs, 2 skipped"],
                    ),
                    ("bana/plan", &[JOB_RUNNING, "success: passed in 0s"]),
                    ("bana/rust", &[JOB_RUNNING, "success: passed in 0s"]),
                ],
            },
            Case {
                name: "SIGINT mid-step from the menu bar: the always() step runs",
                run: fixture!("sigint"),
                cancel: Some(("starting long test", "cancelled from the menu bar")),
                exit: Some(1),
                reason: None,
                state: BuildState::Error,
                jobs: &[("hw", Cancelled, Some("long test"))],
                statuses: &[
                    ("bana", &[RUNNING, "error: cancelled from the menu bar"]),
                    ("bana/hw", &[JOB_RUNNING, "error: cancelled from the menu bar"]),
                ],
            },
            Case {
                name: "SIGINT from someone else: an ordinary failure",
                run: fixture!("sigint"),
                cancel: None,
                exit: Some(1),
                reason: None,
                state: BuildState::Failure,
                jobs: &[("hw", Failure, Some("long test"))],
                statuses: &[
                    (
                        "bana",
                        &[RUNNING, "failure: hw failed at \"long test\" · 1s on mbp"],
                    ),
                    (
                        "bana/hw",
                        &[JOB_RUNNING, "failure: failed at \"long test\" after 1s"],
                    ),
                ],
            },
            Case {
                name: "a second SIGINT (a timeout): the job that had passed stays passed",
                run: fixture!("sigint-twice"),
                cancel: Some(("starting long test", "timed out after 120m")),
                exit: Some(1),
                reason: None,
                state: BuildState::Error,
                jobs: &[("rust", Success, None), ("hw", Cancelled, Some("long test"))],
                statuses: &[
                    ("bana", &[RUNNING, "error: timed out after 120m"]),
                    ("bana/hw", &[JOB_RUNNING, "error: timed out after 120m"]),
                    ("bana/rust", &[JOB_RUNNING, "success: passed in 0s"]),
                ],
            },
            Case {
                name: "SIGKILL at the end of the ladder: no pending is left",
                run: fixture!("sigkill"),
                cancel: Some(("starting long test", "cancelled from the page")),
                exit: None,
                reason: None,
                state: BuildState::Error,
                jobs: &[("rust", Success, None), ("hw", Cancelled, None)],
                statuses: &[
                    ("bana", &[RUNNING, "error: cancelled from the page"]),
                    ("bana/hw", &[JOB_RUNNING, "error: cancelled from the page"]),
                    ("bana/rust", &[JOB_RUNNING, "success: passed in 0s"]),
                ],
            },
            Case {
                name: "killed with the daemon, read again after a restart",
                run: fixture!("sigkill"),
                cancel: None,
                exit: None,
                reason: Some("interrupted (bana restarted)"),
                state: BuildState::Error,
                jobs: &[("rust", Success, None), ("hw", Cancelled, None)],
                statuses: &[
                    ("bana", &[RUNNING, "error: interrupted (bana restarted)"]),
                    (
                        "bana/hw",
                        &[JOB_RUNNING, "error: interrupted (bana restarted)"],
                    ),
                    ("bana/rust", &[JOB_RUNNING, "success: passed in 0s"]),
                ],
            },
            Case {
                name: "killed by something else",
                run: fixture!("sigkill"),
                cancel: None,
                exit: None,
                reason: None,
                state: BuildState::Error,
                jobs: &[("rust", Success, None), ("hw", Cancelled, None)],
                statuses: &[
                    ("bana", &[RUNNING, "error: act was killed"]),
                    ("bana/hw", &[JOB_RUNNING, "error: act was killed"]),
                    ("bana/rust", &[JOB_RUNNING, "success: passed in 0s"]),
                ],
            },
            Case {
                name: "SIGINT to a container job, superseded",
                run: fixture!("sigint-container"),
                cancel: Some(("starting long test", "superseded by 1a2b3c4")),
                exit: Some(1),
                reason: None,
                state: BuildState::Error,
                jobs: &[("linux", Cancelled, Some("long test"))],
                statuses: &[
                    ("bana", &[RUNNING, "error: superseded by 1a2b3c4"]),
                    ("bana/linux", &[JOB_RUNNING, "error: superseded by 1a2b3c4"]),
                ],
            },
            Case {
                name: "a workflow syntax error: act -l fails too",
                run: fixture!("syntax"),
                cancel: None,
                exit: Some(1),
                reason: None,
                state: BuildState::Error,
                jobs: &[],
                statuses: &[(
                    "bana",
                    &[
                        RUNNING,
                        "error: could not start: workflow is not valid. 'broken.yml': yaml: line 7: did not find expected '-' indicator",
                    ],
                )],
            },
            Case {
                name: "a macOS job on Linux: not run here",
                run: fixture!("platform"),
                cancel: None,
                exit: Some(0),
                reason: None,
                state: BuildState::Success,
                jobs: &[("macos", Unsupported, None), ("linux", Success, None)],
                statuses: &[
                    (
                        "bana",
                        &[
                            RUNNING,
                            "success: passed on mbp in 1s · 1 job; not run here: macos",
                        ],
                    ),
                    ("bana/linux", &[JOB_RUNNING, "success: passed in 1s"]),
                ],
            },
            Case {
                name: "a composite action, and a step that may fail",
                run: fixture!("composite"),
                cancel: None,
                exit: Some(0),
                reason: None,
                state: BuildState::Success,
                jobs: &[("rust", Success, None)],
                statuses: &[
                    ("bana", &[RUNNING, "success: passed on mbp in 0s · 1 job"]),
                    ("bana/rust", &[JOB_RUNNING, "success: passed in 0s"]),
                ],
            },
        ]
    }

    #[test]
    fn recorded_runs_give_their_jobs_results_and_statuses() {
        for c in cases() {
            let (b, posted, log) = play(c.run, c.cancel, c.exit, c.reason);
            assert_eq!(b.state, c.state, "{}", c.name);
            let jobs: Vec<(&str, JobState, Option<&str>)> = b
                .jobs
                .iter()
                .map(|j| (j.key.as_str(), j.state, j.failed_step.as_deref()))
                .collect();
            assert_eq!(jobs, c.jobs, "{}", c.name);
            let want: BTreeMap<String, Vec<String>> = c
                .statuses
                .iter()
                .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
                .collect();
            assert_eq!(posted, want, "{}", c.name);
            let back: Build = serde_json::from_value(serde_json::to_value(&b).unwrap()).unwrap();
            assert_eq!(back, b, "{}: build.json keeps everything", c.name);
            let mut again = Build::new(&parse_list(c.run.1), b.started_at.unwrap());
            again.fold_lines(&log, 0);
            again.finish(c.exit, c.reason, b.ended_at.unwrap());
            assert_eq!(again, b, "{}: act.jsonl read again after a restart", c.name);
        }
    }

    #[test]
    fn statuses_go_out_in_order_and_only_once() {
        let (jsonl, list) = fixture!("fail");
        let r = Report::new("quick", "quick", "mbp");
        let (t0, t1) = first_and_last_time(jsonl);
        let mut b = Build::new(&parse_list(list), t0);
        let mut all = status_updates(&Build::default(), &b, &r);
        for line in jsonl.lines() {
            let before = b.clone();
            b.fold(&parse_line(line), t0);
            all.extend(status_updates(&before, &b, &r));
        }
        let before = b.clone();
        b.finish(Some(1), None, t1);
        all.extend(status_updates(&before, &b, &r));
        let seen: Vec<String> = all
            .iter()
            .map(|s| format!("{} {}", s.context, s.state.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                "bana pending",
                "bana/lint pending",
                "bana/web pending",
                "bana/lint failure",
                "bana/web success",
                "bana failure"
            ]
        );
        assert!(status_updates(&b, &b, &r).is_empty());
        assert!(
            statuses(&Build::default(), &r).is_empty(),
            "queued builds post nothing"
        );
    }

    #[test]
    fn jobs_steps_and_what_act_said() {
        let (b, ..) = play(fixture!("composite"), None, Some(0), None);
        let steps: Vec<(&str, Option<&str>, bool)> = b.jobs[0]
            .steps
            .iter()
            .map(|s| (s.name.as_str(), s.result.as_deref(), s.continued))
            .collect();
        assert_eq!(
            steps,
            [
                ("Set up job", Some("success"), false),
                ("actions/checkout@v4", Some("success"), false),
                ("./.github/actions/plan", Some("success"), false),
                ("flaky", Some("failure"), true),
                ("test", Some("success"), false),
                ("Post ./.github/actions/plan", Some("success"), false),
                ("Complete job", Some("success"), false),
            ],
            "a composite's inner steps are its own; continue-on-error fails nothing"
        );
        assert_eq!(b.jobs[0].steps[1].ms, Some(3));

        let (b, ..) = play(fixture!("skip"), None, Some(0), None);
        let rust = &b.jobs[3];
        assert!(
            !rust.steps.iter().any(|s| s.name == "only on nightly"),
            "act says nothing of a step skipped by if:"
        );
        assert_eq!(
            b.jobs[0].steps[1].name,
            "echo \"web=false\" >> \"$GITHUB_OUTPUT\""
        );

        let (b, ..) = play(fixture!("fail"), None, Some(1), None);
        assert_eq!(b.last_error.as_deref(), Some("Job 'lint' failed"));
        let lint = &b.jobs[0];
        assert_eq!(
            lint.steps
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            [
                "Set up job",
                "fmt",
                "cargo clippy",
                "cleanup",
                "Complete job"
            ],
            "the step after the failure was skipped; the always() one ran"
        );
        assert_eq!(lint.steps[0].stage, "");
        assert_eq!(
            (lint.steps[2].stage.as_str(), lint.steps[2].id.as_str()),
            ("Main", "1")
        );

        let (b, ..) = play(fixture!("sigint-twice"), None, Some(1), None);
        assert_eq!(b.last_error.as_deref(), Some("context canceled"));
        assert_eq!(
            (b.state, b.reason.as_deref()),
            (BuildState::Failure, None),
            "a job failed, so act's last error is no reason"
        );

        // Killed mid-step: the step it was at is cut short.
        let (b, ..) = play(fixture!("sigkill"), None, None, None);
        let hw = b.jobs.iter().find(|j| j.key == "hw").unwrap();
        assert_eq!(hw.steps[1].name, "long test");
        assert_eq!(hw.steps[1].result.as_deref(), Some("cancelled"));
    }

    #[test]
    fn act_v_says_more_and_changes_nothing() {
        // Real act -v lines (a host run of two jobs, one skipped by `if: false`).
        let lines = r#"{"dryrun":false,"job":"ci/plan   ","jobID":"plan","level":"debug","matrix":{},"msg":"evaluating expression 'success()'","time":"2026-09-28T13:40:15Z"}
{"dryrun":false,"job":"ci/nightly","jobID":"nightly","level":"debug","matrix":{},"msg":"evaluating expression 'false'","time":"2026-09-28T13:40:15Z"}
{"dryrun":false,"job":"ci/nightly","jobID":"nightly","level":"debug","matrix":{},"msg":"expression 'false' evaluated to 'false'","time":"2026-09-28T13:40:15Z"}
{"dryrun":false,"job":"ci/nightly","jobID":"nightly","jobResult":"skipped","level":"debug","matrix":{},"msg":"Skipping job 'nightly' due to 'false'","time":"2026-09-28T13:40:15Z"}
{"dryrun":false,"job":"ci/plan   ","jobID":"plan","level":"info","matrix":{},"msg":"⭐ Run Set up job","step":"Set up job","stepid":["--setup-job"],"time":"2026-09-28T13:40:15Z"}
{"dryrun":false,"job":"ci/plan   ","jobID":"plan","level":"info","matrix":{},"msg":"  ✅  Success - Set up job","step":"Set up job","stepResult":"success","stepid":["--setup-job"],"time":"2026-09-28T13:40:15Z"}
{"dryrun":false,"job":"ci/plan   ","jobID":"plan","level":"debug","matrix":{},"msg":"Loading revision from git directory","stage":"Main","step":"echo hi","stepID":["0"],"time":"2026-09-28T13:40:15Z"}
{"dryrun":false,"job":"ci/plan   ","jobID":"plan","level":"info","matrix":{},"msg":"⭐ Run Main echo hi","stage":"Main","step":"echo hi","stepID":["0"],"time":"2026-09-28T13:40:15Z"}
{"dryrun":false,"executionTime":4054674,"job":"ci/plan   ","jobID":"plan","level":"info","matrix":{},"msg":"  ✅  Success - Main echo hi [4.054674ms]","stage":"Main","step":"echo hi","stepID":["0"],"stepResult":"success","time":"2026-09-28T13:40:15Z"}
{"dryrun":false,"job":"ci/plan   ","jobID":"plan","jobResult":"success","level":"info","matrix":{},"msg":"🏁  Job succeeded","time":"2026-09-28T13:40:15Z"}"#;
        let mut b = Build::new(&[(0, "plan".into()), (0, "nightly".into())], 0);
        b.fold_lines(lines, 0);
        b.finish(Some(0), None, 1_790_600_415);
        let jobs: Vec<(&str, JobState, usize)> = b
            .jobs
            .iter()
            .map(|j| (j.key.as_str(), j.state, j.steps.len()))
            .collect();
        assert_eq!(jobs, [("plan", Success, 2), ("nightly", Skipped, 0)]);
        let contexts: Vec<String> = statuses(&b, &Report::new("quick", "quick", "mbp"))
            .into_iter()
            .map(|s| format!("{} {}", s.context, s.state.as_str()))
            .collect();
        assert_eq!(contexts, ["bana/plan success", "bana success"]);
    }

    #[test]
    fn a_running_build_shows_each_jobs_step() {
        let (jsonl, list) = fixture!("sigint-twice");
        let mut b = Build::new(&parse_list(list), 0);
        let chips = |b: &Build| -> Vec<(String, JobState, Option<String>)> {
            b.chips()
                .into_iter()
                .map(|c| (c.key, c.state, c.step))
                .collect()
        };
        assert_eq!(
            chips(&b),
            [
                ("rust".to_string(), JobState::Waiting, None),
                ("hw".to_string(), JobState::Waiting, None)
            ]
        );
        for line in jsonl.lines().take_while(|l| !l.contains("Reevaluate")) {
            b.fold(&parse_line(line), 0);
        }
        assert_eq!(
            chips(&b),
            [
                ("rust".to_string(), JobState::Success, None),
                (
                    "hw".to_string(),
                    JobState::Running,
                    Some("long test".to_string())
                )
            ]
        );
    }

    #[test]
    fn a_matrix_takes_its_jobs_place() {
        let (jsonl, list) = fixture!("matrix");
        let mut b = Build::new(&parse_list(list), 0);
        assert_eq!(b.jobs.len(), 1);
        assert_eq!(b.jobs[0].key, "package");
        b.fold_lines(jsonl, 0);
        let keys: Vec<&str> = b.jobs.iter().map(|j| j.key.as_str()).collect();
        assert_eq!(keys, ["package (linux-arm64)", "package (linux-x64)"]);
        assert_eq!(b.jobs[1].matrix["target"], "linux-x64");
        assert_eq!(b.jobs[1].stage, Some(0));

        // Between jobs of the list, and with more than one value.
        let list = [(0, "a".into()), (0, "m".into()), (1, "z".into())];
        let mut b = Build::new(&list, 0);
        for (os, n) in [("mac", 1), ("linux", 2)] {
            let line = format!(
                r#"{{"jobID":"m","job":"ci/m-{n}   ","matrix":{{"os":"{os}","n":{n}}},"msg":"x","time":"2026-09-28T13:00:00Z"}}"#
            );
            b.fold(&parse_line(&line), 0);
        }
        let keys: Vec<&str> = b.jobs.iter().map(|j| j.key.as_str()).collect();
        assert_eq!(keys, ["a", "m (1, mac)", "m (2, linux)", "z"]);
        let mut unfinished = b.clone();
        let line = r#"{"jobID":"m","matrix":{"os":"mac","n":1},"msg":"🏁  Job succeeded","jobResult":"success"}"#;
        b.fold(&parse_line(line), 0);
        b.fold(
            &parse_line(&line.replace("mac", "linux").replace('1', "2")),
            0,
        );
        b.finish(Some(0), None, 0);
        let states: Vec<JobState> = b.jobs.iter().map(|j| j.state).collect();
        assert_eq!(states, [Skipped, Success, Success, Skipped]);

        unfinished.finish(Some(0), None, 0);
        let states: Vec<JobState> = unfinished.jobs.iter().map(|j| j.state).collect();
        assert_eq!(states, [Cancelled; 4], "act exited 0 with jobs unfinished");
        assert_eq!(unfinished.reason.as_deref(), Some("act exited with 0"));
    }

    #[test]
    fn the_list_gives_stages_and_job_ids() {
        assert_eq!(
            parse_list(fixture!("fail").1),
            [(0, "lint".into()), (0, "web".into()), (1, "package".into())]
        );
        assert_eq!(
            parse_list(fixture!("skip").1),
            [
                (0, "plan".into()),
                (0, "nightly".into()),
                (1, "web".into()),
                (1, "rust".into())
            ]
        );
        assert_eq!(
            parse_list(fixture!("composite").1),
            [(0, "rust".into())],
            "a job name with a space"
        );
        assert!(parse_list(fixture!("syntax").1).is_empty());
        assert_eq!(
            parse_list("1 b\n0 a\n0 a\n2 not;an-id\nbana: act\n"),
            [(1, "b".into()), (0, "a".into())]
        );
        let b = Build::new(&parse_list("1 b\n0 a\n"), 7);
        assert_eq!(b.jobs[0].key, "a", "stages first");
        assert_eq!((b.state, b.started_at), (BuildState::Running, Some(7)));
    }

    #[test]
    fn lines_act_and_the_daemon_write() {
        let Event::Job(l) = parse_line(
            r#"{"dryrun":false,"job":"ci/web ","jobID":"web","level":"info","matrix":{},"msg":"  ✅  Success - Set up job","step":"Set up job","stepResult":"success","stepid":["--setup-job"],"time":"2026-09-28T13:01:12Z"}"#,
        ) else {
            panic!("a job line")
        };
        assert_eq!((l.name.as_str(), l.key.as_str()), ("ci/web", "web"));
        assert_eq!(l.step_id.as_deref(), Some("--setup-job"));
        assert_eq!(l.step_result.as_deref(), Some("success"));
        assert_eq!(l.time, Some(1_790_600_472));
        let Event::Job(l) = parse_line(
            r#"{"executionTime":1006519185,"job":"ci/web ","jobID":"web","matrix":{},"msg":"x","stage":"Post","step":"build","stepID":["0","2"]}"#,
        ) else {
            panic!("a job line")
        };
        assert_eq!(
            (l.step_id.as_deref(), l.inner, l.ms, l.step.as_deref()),
            (Some("0"), true, Some(1006), Some("Post build"))
        );
        assert_eq!(
            parse_line("Error: Job 'lint' failed"),
            Event::Text {
                msg: "Error: Job 'lint' failed".into(),
                error: true
            }
        );
        assert_eq!(
            parse_line(
                r#"{"msg":"\u001b[31mDocker is not running: start OrbStack\u001b[0m","bana":"stderr"}"#
            ),
            Event::Text {
                msg: "Docker is not running: start OrbStack".into(),
                error: true
            },
            "bana's die, wrapped by the daemon"
        );
        assert_eq!(
            parse_line("\x1b[1mact: quick from ci.yml\x1b[0m"),
            Event::Text {
                msg: "act: quick from ci.yml".into(),
                error: false
            },
            "bana's say"
        );
        assert_eq!(
            parse_line(r#"{"bana":"cancel","msg":"timed out after 120m"}"#),
            Event::Cancel("timed out after 120m".into())
        );
        assert_eq!(
            parse_line(&cancel_mark("superseded by 1a2b3c4")),
            Event::Cancel("superseded by 1a2b3c4".into())
        );
        // What the daemon keeps in act.jsonl reads as what was printed.
        let json = r#"{"level":"info","msg":"Start server on http://192.0.2.2:0"}"#;
        assert_eq!(log_line(&format!("{json}\n")), json, "act's JSON as it is");
        for raw in [
            "Error: Job 'lint' failed",
            "\x1b[31mDocker is not running\x1b[0m",
            "\x1b[1mact: quick from ci.yml\x1b[0m",
            "42",
            "[1, 2]",
        ] {
            let kept = log_line(raw);
            assert!(kept.contains(r#""bana":"stderr""#), "{kept}");
            assert_eq!(parse_line(&kept), parse_line(raw), "{raw}");
        }
        assert_eq!(
            parse_line(r#"{"level":"info","msg":"Start server on http://192.0.2.2:0"}"#),
            Event::Other
        );
        assert_eq!(parse_line(""), Event::Other);
        assert!(matches!(
            parse_line(r#"{"level":"fatal","msg":"no Docker"}"#),
            Event::Text { error: true, .. }
        ));
        assert!(matches!(parse_line("42"), Event::Text { error: false, .. }));

        // A wrapped die before act ran: could not start, with its words.
        let mut b = Build::new(&[(0, "rust".into())], 0);
        b.fold(
            &parse_line(r#"{"msg":"\u001b[31mtier: one of quick nightly, not 'x'\u001b[0m","bana":"stderr"}"#),
            0,
        );
        assert_eq!(b.finish(Some(1), None, 3), BuildState::Error);
        assert_eq!(
            b.reason.as_deref(),
            Some("could not start: tier: one of quick nightly, not 'x'")
        );
        assert_eq!(b.jobs[0].state, JobState::Cancelled);
        let mut b = Build::new(&[], 0);
        b.finish(Some(2), None, 0);
        assert_eq!(
            b.reason.as_deref(),
            Some("could not start: act exited with 2")
        );
    }

    #[test]
    fn contexts_and_descriptions() {
        let r = Report::new("nightly", "quick", "mbp");
        assert_eq!(r.context, "bana nightly");
        assert_eq!(Report::new("quick", "quick", "mbp").context, "bana");
        assert_eq!(Report::new("", "", "mbp").context, "bana");
        let (b, ..) = play(fixture!("matrix"), None, Some(0), None);
        let contexts: Vec<String> = statuses(&b, &r).into_iter().map(|s| s.context).collect();
        assert_eq!(
            contexts,
            [
                "bana nightly/package (linux-arm64)",
                "bana nightly/package (linux-x64)",
                "bana nightly"
            ]
        );
        let running = Build::new(&[], 0);
        assert_eq!(
            Report::new("", "", "mbp").describe(&running).unwrap().1,
            "running on mbp"
        );

        // Long names are cut, and the failing job and step still show.
        let step = "cargo test --workspace --all-features -- --include-ignored --test-threads 1 and then some more";
        let mut b = Build::new(
            &[(0, "rust".into()), (0, "macos".into()), (0, "web".into())],
            0,
        );
        for (job, t) in [
            ("macos", "13:05:00"),
            ("rust", "13:03:40"),
            ("web", "13:04:00"),
        ] {
            for extra in [
                format!(r#""step":"{step}","stepID":["0"],"stepResult":"failure""#),
                r#""jobResult":"failure""#.to_string(),
            ] {
                let line = format!(
                    r#"{{"jobID":"{job}","matrix":{{}},"msg":"x","time":"2026-09-28T{t}Z",{extra}}}"#
                );
                b.fold(&parse_line(&line), 0);
            }
        }
        b.started_at = parse_time("2026-09-28T13:00:00Z");
        b.finish(Some(1), None, parse_time("2026-09-28T13:05:10Z").unwrap());
        let long = Report::new("quick", "quick", "a-machine-with-a-long-name.local");
        let all = statuses(&b, &long);
        let own = all.last().unwrap();
        assert!(
            own.description
                .starts_with("rust failed at \"cargo test --workspace"),
            "the first to fail: {}",
            own.description
        );
        assert!(own
            .description
            .contains("…\", +2 more · 5m10s on a-machine"));
        let rust = &all[0];
        assert_eq!(rust.context, "bana/rust");
        assert!(
            rust.description.ends_with("…\" after 0s"),
            "{}",
            rust.description
        );
        for s in &all {
            assert!(s.description.chars().count() <= DESCRIPTION_MAX, "{s:?}");
        }
        let mut b = Build::new(&[], 0);
        b.finish(Some(1), Some(&"x".repeat(200)), 0);
        let d = &statuses(&b, &r)[0].description;
        assert_eq!(d.chars().count(), DESCRIPTION_MAX);
        assert!(d.ends_with('…'));
    }

    #[test]
    fn a_cancel_keeps_its_first_reason() {
        let mut b = Build::new(&[(0, "a".into())], 0);
        b.cancel("cancelled from the menu bar");
        b.finish(None, Some("interrupted (bana restarted)"), 5);
        assert_eq!(b.reason.as_deref(), Some("cancelled from the menu bar"));
        assert_eq!(b.jobs[0].state, JobState::Cancelled, "it never started");
        assert!(
            statuses(&b, &Report::new("quick", "quick", "m"))
                .iter()
                .all(|s| s.context == "bana"),
            "a job that never started gets no status"
        );
    }

    #[test]
    fn times_and_durations() {
        let t = parse_time("2026-09-28T13:00:54Z");
        assert_eq!(t, Some(1_790_600_454));
        assert_eq!(parse_time("2026-09-28T15:00:54+02:00"), t);
        assert_eq!(parse_time("2026-09-28T08:30:54-04:30"), t);
        assert_eq!(parse_time("2026-09-28T13:00:54.123456Z"), t);
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-03-01T00:00:00Z"), Some(951_868_800));
        for bad in [
            "",
            "2026-09-28",
            "2026-09-28T13:00:54",
            "2026-13-28T13:00:54Z",
            "2026-09-28T13:00:54+0200",
            "2026-09-28T13:00:54+0a:00",
            "2026-09-28T13:+0:54Z",
            "2026-09-28T13:00:54Zjunk",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
        for (secs, text) in [
            (-3, "0s"),
            (0, "0s"),
            (45, "45s"),
            (60, "1m"),
            (192, "3m12s"),
            (599, "9m59s"),
            (754, "12m"),
            (7200, "2h00m"),
            (7500, "2h05m"),
        ] {
            assert_eq!(duration(secs), text, "{secs}");
        }
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abcd", 3), "ab…");
        assert_eq!(cut("ab cd", 4), "ab…");
        assert_eq!(cut("·····", 2), "·…");
    }

    fn summary() -> Summary {
        Summary {
            repo: "tjrb-xyz/example".into(),
            prefix: "example".into(),
            machine: "mbp".into(),
            now: 10_000,
            watcher: Watcher {
                docker: true,
                fetched_at: Some(9_990),
                ..Watcher::default()
            },
            ..Summary::default()
        }
    }

    fn build(id: u64, state: BuildState, ended_at: Option<i64>) -> BuildView {
        BuildView {
            id,
            git_ref: "main".into(),
            sha: "1a2b3c4d5e6f".into(),
            tier: "quick".into(),
            trigger: "push".into(),
            attempt: 1,
            state,
            ended_at,
            ..BuildView::default()
        }
    }

    fn queued(n: usize) -> Vec<QueuedView> {
        (0..n)
            .map(|i| QueuedView {
                id: 10 + i as u64,
                git_ref: "dev".into(),
                ..QueuedView::default()
            })
            .collect()
    }

    #[test]
    fn the_menu_bar_title_table() {
        let passed = Some(build(3, BuildState::Success, Some(10_000 - 720)));
        let failed = Some(build(3, BuildState::Failure, Some(9_000)));
        let mut running = build(4, BuildState::Running, None);
        running.elapsed = 4 * 60 + 59;
        running.jobs = vec![
            JobChip {
                key: "plan".into(),
                state: JobState::Success,
                step: None,
            },
            JobChip {
                key: "rust".into(),
                state: JobState::Running,
                step: Some("cargo test".into()),
            },
            JobChip {
                key: "macos".into(),
                state: JobState::Running,
                step: None,
            },
        ];
        type Tweak = fn(&mut Summary);
        let rows: Vec<(&str, Tweak, &str)> = vec![
            ("no build yet", |_| {}, "🧱"),
            (
                "idle, last build passed",
                |s| s.last = Some(build(3, BuildState::Success, Some(0))),
                "🧱",
            ),
            (
                "idle after a failure",
                |s| (s.last, s.failed) = (Some(build(3, BuildState::Failure, Some(0))), Some(3)),
                "🧱 !",
            ),
            (
                "idle after an error",
                |s| s.last = Some(build(3, BuildState::Error, Some(0))),
                "🧱 !",
            ),
            (
                "idle after an error that followed a failure",
                |s| (s.last, s.failed) = (Some(build(4, BuildState::Error, Some(0))), Some(3)),
                "🧱 !",
            ),
            ("paused", |s| s.watcher.paused = true, "🧱 paused"),
            (
                "Docker down, nothing queued",
                |s| s.watcher.docker = false,
                "🧱",
            ),
            (
                "queued, Docker down",
                |s| (s.watcher.docker, s.queue) = (false, queued(1)),
                "🧱 no Docker",
            ),
            (
                "queued behind a manual bana ci",
                |s| (s.watcher.lock_holder, s.queue) = (Some("bana ci quick".into()), queued(1)),
                "🧱 busy",
            ),
            (
                "statuses cannot be posted",
                |s| s.watcher.post_error = Some("gh is signed out".into()),
                "🧱 !gh",
            ),
            (
                "building",
                |s| s.running = Some(build(4, BuildState::Running, None)),
                "🧱 0m",
            ),
            (
                "building, paused and queued",
                |s| {
                    let mut b = build(4, BuildState::Running, None);
                    b.elapsed = 3725;
                    (s.running, s.queue, s.watcher.paused) = (Some(b), queued(2), true);
                },
                "🧱 1h02m +2",
            ),
            (
                "building after a failure",
                |s| {
                    s.running = Some(build(4, BuildState::Running, None));
                    (s.last, s.failed) = (Some(build(3, BuildState::Failure, Some(0))), Some(3));
                },
                "🧱 0m",
            ),
        ];
        for (name, tweak, title) in rows {
            let mut s = summary();
            tweak(&mut s);
            let v = tray_view(&s);
            assert_eq!(v.title, title, "{name}");
            assert_eq!(v.cancel_enabled, s.running.is_some(), "{name}");
            assert_eq!(v.paused, s.watcher.paused, "{name}");
            // Fix with Claude: offered for the summary's newest failed build.
            let fix = s.failed.map(|id| (format!("Fix #{id} with Claude…"), id));
            assert_eq!(v.fix_line.zip(v.fix_build), fix, "{name}");
        }

        // The words, building and idle.
        let mut s = summary();
        (s.running, s.queue, s.last) = (Some(running), queued(2), passed.clone());
        let v = tray_view(&s);
        assert_eq!(v.title, "🧱 4m +2");
        assert_eq!(
            v.tooltip,
            "bana: building example main 1a2b3c4 (quick) · rust, macos · 4m · 2 queued"
        );
        assert_eq!(
            v.status_line,
            "example: building main 1a2b3c4 · rust, macos"
        );
        assert_eq!(
            v.last_line.as_deref(),
            Some("Last: passed main 1a2b3c4 · 12 min ago")
        );
        assert_eq!(v.open_build, Some(4), "a click opens the running build");

        let mut s = summary();
        s.last = passed;
        let v = tray_view(&s);
        assert_eq!(v.title, BRICK);
        assert_eq!(
            v.tooltip,
            "bana: example idle · last: passed main 1a2b3c4, 12 min ago"
        );
        assert_eq!(v.status_line, "example: idle");
        assert_eq!(v.open_build, Some(3), "else the latest");

        let mut s = summary();
        s.last = failed;
        (s.watcher.docker, s.queue) = (false, queued(1));
        s.watcher.post_error = Some("gh is signed out".into());
        s.watcher.fetch_error = Some("could not resolve host".into());
        let v = tray_view(&s);
        assert_eq!(v.title, "🧱 no Docker");
        assert_eq!(
            v.tooltip,
            "bana: example waiting for Docker · 1 queued · last: failed main 1a2b3c4, 16 min ago · statuses not posted: gh is signed out · fetch failed: could not resolve host"
        );
        assert_eq!(v.status_line, "example: waiting for Docker (1 queued)");
        assert_eq!(
            v.last_line.as_deref(),
            Some("Last: failed main 1a2b3c4 · 16 min ago")
        );

        let v = tray_view(&summary());
        assert_eq!(
            (v.title.as_str(), v.last_line, v.open_build, v.fix_line),
            (BRICK, None, None, None)
        );
        assert_eq!(v.tooltip, "bana: example idle");
        let mut s = summary();
        s.queue = queued(3);
        assert_eq!(tray_view(&s).status_line, "example: 3 queued");
        s.watcher.paused = true;
        assert_eq!(tray_view(&s).status_line, "example: paused (3 queued)");
    }

    #[test]
    fn ages() {
        assert_eq!(ago(100, Some(90)), "just now");
        assert_eq!(ago(100, Some(200)), "just now", "a clock that moved back");
        assert_eq!(ago(4000, Some(100)), "1 h ago");
        assert_eq!(ago(90_000, Some(0)), "1 day ago");
        assert_eq!(ago(3 * 86_400, Some(0)), "3 days ago");
        assert_eq!(ago(0, None), "a while ago");
        assert_eq!(minutes(59), "0m");
        assert_eq!(minutes(3600), "1h00m");
    }

    #[test]
    fn the_summary_is_json_for_the_page() {
        let mut s = summary();
        s.running = Some(build(4, BuildState::Running, None));
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["running"]["ref"], "main");
        assert_eq!(v["running"]["state"], "running");
        assert_eq!(v["watcher"]["docker"], true);
        assert!(v["last"].is_null() && v["failed"].is_null());
        s.failed = Some(3);
        assert_eq!(serde_json::to_value(&s).unwrap()["failed"], 3);
    }
}

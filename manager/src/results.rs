//! What a build did, step by step, for the fix brief and the CI report: act's
//! `--json` lines (the daemon's act.jsonl) or act's plain text (a hand run's
//! ci/last.log, a pasted log) folded into [`Results`], and results.jsonl out
//! ([`Results::to_jsonl`]).
//!
//! Jobs and steps are [`actlog::Build`]'s, as for the statuses: a line of plain
//! text is read into an [`actlog::JobLine`] first. What each step printed is
//! read here: cargo's and nextest's test lines, the panics of failing tests,
//! cargo's rerun target, annotations (`::error file=…::…`), and its last lines.
//!
//! act's plain text (0.2.89) has `[ci/rust   ] ⭐ Run Main cargo test`, the
//! step's output as `[ci/rust   ]   | …` (or `| …` alone, the bar in the job's
//! colour, when act prints to a terminal), `[ci/rust   ]   ❌  Failure - Main cargo test
//! [4m58.6s]`, `[ci/rust   ] 🏁  Job failed`, and act's own `Error: …` at the
//! end. A pasted log may have bare output lines too: they are the output of the
//! step that ends next. A job's key is its name there (`rust`, or `package
//! (linux-x64)` once act names its matrix), not the workflow's job id.
//!
//! Output is matched at column 0 once ANSI escapes and C0 controls are gone:
//! host-mode steps print through a PTY, so cargo's `test result:` carries
//! colours and \x0f, and nextest indents libtest's own lines, which must not
//! count twice.
//!
//! Who a failure belongs to: bana, for a `*/bana/actions/*` step (Main, Pre or
//! Post), an error naming such an action's cache directory
//! (`…-bana-actions-keep-builds@…`), or bana's own red words; act, for act's
//! other errors outside a job; the project otherwise.

use crate::actlog::{self, BuildState, Event, JobLine, JobState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

/// results.jsonl's version, on its build line.
pub const SCHEMA: u32 = 1;
/// A failed step keeps this many of its last lines.
pub const TAIL: usize = 60;
/// A kept line is cut to this many characters.
const LINE_MAX: usize = 2000;
/// A panic's message: at most this many of the lines after it.
const PANIC_LINES: usize = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    #[default]
    Project,
    Bana,
    Act,
}

/// The build as a whole. The fold fills what the log says (the network, act's
/// first and last times, the result); whoever ran the build fills the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildInfo {
    pub schema: u32,
    /// `act`, or `act 0.2.89` when its version is known.
    pub builder: String,
    /// The bana commit that ran it.
    pub bana: Option<String>,
    pub repo: Option<String>,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub sha: Option<String>,
    pub tier: Option<String>,
    pub machine: Option<String>,
    /// act's `--network`, from bana ci's first line.
    pub network: Option<String>,
    /// `push`, `manual`, `hand` (bana ci), `paste`.
    pub trigger: Option<String>,
    /// Unix seconds.
    pub started: Option<i64>,
    pub ended: Option<i64>,
    /// `success`, `failure` (a job failed), `error` (act or bana failed outside
    /// the jobs, or the build was stopped), or `unknown` (no job in the log).
    pub result: String,
}

impl Default for BuildInfo {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            builder: "act".into(),
            bana: None,
            repo: None,
            git_ref: None,
            sha: None,
            tier: None,
            machine: None,
            network: None,
            trigger: None,
            started: None,
            ended: None,
            result: "unknown".into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Results {
    pub build: BuildInfo,
    /// In the log's order. Output outside any step act named (a paste of a
    /// test run alone) is a last job whose key and step name are empty.
    pub jobs: Vec<Job>,
    /// Errors outside the jobs.
    pub errors: Vec<LogError>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Job {
    /// `rust`, `package (linux-arm64)`.
    pub key: String,
    /// The workflow's job id (in plain text, act's name for the job).
    pub id: String,
    pub matrix: BTreeMap<String, String>,
    /// `success`, `failure`, `skipped`, `unsupported`, `cancelled`, or `unknown`.
    pub result: String,
    pub ms: Option<u64>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Step {
    /// As GitHub shows it: `Post <name>` for a post step.
    pub name: String,
    /// `Pre`, `Main` or `Post`; empty for "Set up job" and "Complete job".
    pub stage: String,
    /// `success`, `failure`, `skipped`, `cancelled`; none if it never ended.
    pub result: Option<String>,
    pub ms: Option<u64>,
    pub owner: Owner,
    /// It failed under `continue-on-error`, so its job went on.
    pub continued: bool,
    /// Test counts, one per tool that printed them (`cargo`, `nextest`).
    pub tests: Vec<Count>,
    /// Each test's own line, in order.
    pub cases: Vec<Case>,
    /// What cargo says to pass to rerun what failed (`-p dsper-engine --test facts`).
    pub reruns: Vec<String>,
    pub annotations: Vec<Annotation>,
    /// A failed step's last lines, ANSI-stripped.
    pub tail: Vec<String>,
}

impl Step {
    pub fn failed(&self) -> bool {
        self.result.as_deref() == Some("failure")
    }

    /// cargo stopped at the first test binary that failed (no `--no-fail-fast`),
    /// so later binaries never ran.
    pub fn incomplete(&self) -> bool {
        self.tests.iter().any(|c| c.incomplete)
    }

    pub fn failed_cases(&self) -> impl Iterator<Item = &Case> {
        self.cases.iter().filter(|c| c.result == "failed")
    }
}

/// A tool's test counts in one step.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Count {
    pub tool: String,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    /// Not every test ran: cargo stopped early, or nextest ran N/M.
    pub incomplete: bool,
}

impl Count {
    fn new(tool: &str) -> Self {
        Self {
            tool: tool.into(),
            ..Self::default()
        }
    }

    fn add(&mut self, c: &Count) {
        self.passed += c.passed;
        self.failed += c.failed;
        self.skipped += c.skipped;
        self.incomplete |= c.incomplete;
    }
}

/// One test.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Case {
    pub name: String,
    /// `passed`, `failed` or `skipped`.
    pub result: String,
    /// cargo's test binary (`tests/facts.rs`, `Doc-tests demo`) or nextest's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// Where it panicked: `crates/dsper-engine/tests/facts.rs:457:18`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// The panic's message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A workflow command's annotation: `::error file=a.rs,line=3::text`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Annotation {
    /// `error`, `warning` or `notice`.
    pub level: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// An error outside the jobs: act's `Error: …`, or bana's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogError {
    pub owner: Owner,
    pub text: String,
    /// The one failed step it goes with, when the log says which.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

impl Results {
    /// The steps that failed, job by job (`continue-on-error` ones left out).
    pub fn failures(&self) -> Vec<(&Job, &Step)> {
        self.jobs
            .iter()
            .flat_map(|j| j.steps.iter().map(move |s| (j, s)))
            .filter(|(_, s)| s.failed() && !s.continued)
            .collect()
    }
}

/// act's `--json` lines, as the daemon keeps them in act.jsonl (with bana's
/// and act's stderr as `{"msg": …, "bana": "stderr"}`).
pub fn fold_json(text: &str) -> Results {
    let mut f = Folder::default();
    for line in text.lines() {
        f.json(line);
    }
    f.finish()
}

/// act's plain text: a hand run's ci/last.log, or a pasted log. act's JSON
/// lines in it (`bana ci -- --json`) are read as [`fold_json`] reads them.
pub fn fold_text(text: &str) -> Results {
    let mut f = Folder::default();
    for line in text.lines() {
        f.text(line);
    }
    f.finish()
}

/// A line as a terminal shows it: what follows its last carriage return, with
/// ANSI escapes and control characters gone (tabs stay).
pub fn clean(s: &str) -> String {
    let s = s.trim_end_matches(['\r', '\n']);
    let s = s.rsplit('\r').find(|p| !p.is_empty()).unwrap_or("");
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: colours, cursor moves.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC (cargo's hyperlinks), up to BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                Some('(' | ')' | '*' | '+' | '#' | '%') => {
                    chars.next();
                }
                _ => {}
            },
            '\t' => out.push(c),
            c if c < ' ' || c == '\x7f' || ('\u{80}'..='\u{9f}').contains(&c) => {}
            c => out.push(c),
        }
    }
    out
}

/// A step: its job's key, its stage and act's id for it, as in [`actlog::Step`].
type StepKey = (String, String, String);

/// Output outside any step act named.
fn loose_key() -> StepKey {
    Default::default()
}

/// A step that started and has not ended yet, in plain text.
struct Open {
    stage: String,
    name: String,
    id: String,
}

#[derive(Default)]
struct Folder {
    build: actlog::Build,
    out: BTreeMap<StepKey, Output>,
    errors: Vec<LogError>,
    network: Option<String>,
    /// act's first and last times (Unix seconds).
    times: (Option<i64>, Option<i64>),
    // Plain text only.
    /// Each job's running step, then the inner steps of a composite action it
    /// runs (act's JSON puts those under their step, and so does this).
    open: BTreeMap<String, Vec<Open>>,
    /// act's names for the jobs (`ci/rust`), and their keys.
    keys: BTreeMap<String, String>,
    /// The job of the last `[wf/job]` line.
    last: Option<String>,
    /// Output no step has taken yet: a paste that starts mid-step.
    pending: Vec<String>,
    /// act marks its output here (`[job]   | …`, or a coloured bar), so a bare
    /// line is the rest of a line before it (a step summary), not output.
    marked: bool,
    /// Each colour's job, by act's name for it, in colour.
    colours: BTreeMap<String, String>,
    ids: u32,
}

impl Folder {
    fn json(&mut self, raw: &str) {
        let ev = actlog::parse_line(raw);
        match &ev {
            Event::Job(_) => self.job(&ev),
            Event::Text { msg, error } => {
                let red = raw.contains("\x1b[31m") || raw.contains("\\u001b[31m");
                self.outside(msg, *error, red);
            }
            Event::Cancel(_) => self.build.fold(&ev, 0),
            Event::Other => {}
        }
    }

    /// A line from a job, from act's JSON or read from its text.
    fn job(&mut self, ev: &Event) {
        let Event::Job(l) = ev else { return };
        // act -v's debug lines: nothing a step printed.
        if matches!(l.level.as_str(), "debug" | "trace") {
            return;
        }
        if let Some(t) = l.time {
            self.times.0.get_or_insert(t);
            self.times.1 = Some(t);
        }
        self.build.fold(ev, 0);
        let (Some(id), Some(_)) = (&l.step_id, &l.step) else {
            return;
        };
        let out = self
            .out
            .entry((l.key.clone(), l.stage.clone(), id.clone()))
            .or_default();
        out.ran |= l.output || l.msg.trim_start().starts_with("⭐ Run");
        if l.output {
            let msg = l.msg.strip_suffix('\n').unwrap_or(&l.msg);
            for line in msg.split('\n') {
                out.read(&clean(line));
            }
        } else if let Some(a) = annotation(&l.msg) {
            out.keep(clean(l.msg.trim()));
            out.annotations.push(a);
        }
    }

    /// A line outside the jobs: act's `Error: …`, bana's words.
    fn outside(&mut self, msg: &str, error: bool, red: bool) {
        let msg = clean(msg);
        let msg = msg.trim();
        if let Some(n) = msg.strip_prefix("act: ").and_then(network) {
            self.network = Some(n);
            return;
        }
        if !error {
            return;
        }
        let act_said = msg.starts_with("Error: ");
        let text = msg.strip_prefix("Error: ").unwrap_or(msg).trim();
        // act's last word names the first job that failed; the jobs say more.
        if text.is_empty() || (text.starts_with("Job '") && text.ends_with("' failed")) {
            return;
        }
        let owner = if bana_action(text).is_some() || (red && !act_said) {
            Owner::Bana
        } else {
            Owner::Act
        };
        if !self.errors.iter().any(|e| e.text == text) {
            self.errors.push(LogError {
                owner,
                text: text.to_string(),
                ..LogError::default()
            });
        }
    }

    fn text(&mut self, raw: &str) {
        if raw.starts_with('{') && act_json(raw) {
            return self.json(raw);
        }
        // To a terminal, act gives each job a colour, for its name and for the
        // bar before its output (`\x1b[33m|\x1b[0m …`), which names no job.
        let colour = raw
            .strip_prefix("\x1b[")
            .and_then(|r| r.split_once('m'))
            .map(|(c, _)| c);
        let red = raw.contains("\x1b[31m");
        let s = clean(raw);
        if let Some((name, rest)) = job_prefix(&s) {
            if let Some(c) = colour {
                self.marked = true;
                self.colours.insert(c.to_string(), name.to_string());
            }
            return self.text_job(name, rest);
        }
        if let Some(o) = s.strip_prefix('|') {
            let o = o.strip_prefix(' ').unwrap_or(o);
            let job = colour
                .and_then(|c| self.colours.get(c))
                .and_then(|name| self.keys.get(name))
                .cloned();
            match job {
                Some(key) => self.output(&key, o),
                None if colour.is_some() || !self.marked => self.orphan(o),
                None => {}
            }
            return;
        }
        if let Some((error, msg)) = logrus(&s) {
            if error {
                self.outside(&msg, true, false);
            }
            return;
        }
        // In a paste, a bare line while a step runs is that step's; bana's own
        // words come in colour (bold, yellow, red for an error).
        let quiet =
            self.marked || (self.pending.is_empty() && self.open.values().all(Vec::is_empty));
        let error = s.starts_with("Error: ") || red;
        if colour.is_some() || s.starts_with("act: ") || (quiet && error) {
            return self.outside(&s, error, red);
        }
        if !self.marked {
            self.orphan(&s);
        }
    }

    /// `[ci/rust   ] REST`.
    fn text_job(&mut self, name: &str, rest: &str) {
        let key = match self.keys.get(name) {
            Some(k) => k.clone(),
            None => {
                let k = name.split_once('/').map_or(name, |(_, j)| j).trim();
                self.keys.insert(name.to_string(), k.to_string());
                k.to_string()
            }
        };
        self.last = Some(key.clone());
        let body = rest.trim();
        if body.starts_with("[DEBUG]") {
            return;
        }
        if let Some(o) = rest.trim_start().strip_prefix('|') {
            self.marked = true;
            return self.output(&key, o.strip_prefix(' ').unwrap_or(o));
        }
        let mut l = JobLine {
            key: key.clone(),
            // A matrix's entries (`package-1`) go after the entries before
            // them, as in act's JSON.
            id: matrix_id(&key).to_string(),
            name: name.to_string(),
            msg: body.to_string(),
            level: "info".into(),
            ..JobLine::default()
        };
        if let Some(what) = body.strip_prefix("⭐ Run ") {
            let (stage, step) = stage_and_name(what);
            if !self.pending.is_empty() {
                self.loose();
            }
            let fresh = self.next_id();
            let stack = self.open.entry(key).or_default();
            let inner = stack
                .first()
                .is_some_and(|o| !o.stage.is_empty() && o.stage == stage);
            if !inner {
                stack.clear();
            }
            let id = stack.first().map_or(fresh, |o| o.id.clone());
            stack.push(Open {
                stage,
                name: step,
                id,
            });
            on_step(&mut l, stack, inner);
        } else if let Some((result, what)) = step_result(body) {
            let (what, ms) = strip_duration(what);
            let (stage, step) = stage_and_name(what);
            let fresh = self.next_id();
            let stack = self.open.entry(key.clone()).or_default();
            let at = stack
                .iter()
                .rposition(|o| o.stage == stage && o.name == step);
            let inner = match at {
                Some(i) => i > 0,
                None => stack
                    .first()
                    .is_some_and(|o| !o.stage.is_empty() && o.stage == stage),
            };
            if inner {
                stack.truncate(at.unwrap_or(stack.len()));
                on_step(&mut l, stack, true);
                l.step_result = Some(result.into());
            } else {
                let id = match at {
                    Some(_) => stack[0].id.clone(),
                    None => fresh,
                };
                stack.clear();
                // A paste's first lines: the output of the step that ends here.
                let out = self
                    .out
                    .entry((key, stage.clone(), id.clone()))
                    .or_default();
                for p in std::mem::take(&mut self.pending) {
                    out.read(&p);
                }
                (l.stage, l.step, l.step_id) = (stage, Some(step), Some(id));
                (l.step_result, l.ms) = (Some(result.into()), ms);
            }
        } else if body.starts_with('🏁') {
            self.open.remove(&key);
            let r = if body.contains("succeeded") {
                "success"
            } else {
                "failure"
            };
            l.job_result = Some(r.into());
        } else if let Some(map) = body
            .strip_prefix('🧪')
            .and_then(|m| m.trim_start().strip_prefix("Matrix: "))
        {
            let map = map.to_string();
            self.job(&Event::Job(Box::new(l)));
            self.rekey(name, &key, &map);
            return;
        } else if let Some(stack) = self.open.get(&key).filter(|s| !s.is_empty()) {
            // act's word on the running step: an annotation, "Failed but continue next step".
            on_step(&mut l, stack, stack.len() > 1);
        }
        self.job(&Event::Job(Box::new(l)));
    }

    fn next_id(&mut self) -> String {
        self.ids += 1;
        format!("t{}", self.ids)
    }

    /// act names a matrix's entries `package-1`, `package-2`, then prints each
    /// one's matrix: its key becomes `package (linux-x64)`, as in act's JSON.
    fn rekey(&mut self, name: &str, old: &str, map: &str) {
        let Some(inner) = map
            .trim()
            .strip_prefix("map[")
            .and_then(|m| m.strip_suffix(']'))
        else {
            return;
        };
        let matrix: BTreeMap<String, String> = inner
            .split(' ')
            .filter_map(|kv| kv.split_once(':'))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let id = matrix_id(old);
        let new = actlog::job_key(id, &matrix);
        if matrix.is_empty() || self.build.jobs.iter().any(|j| j.key == new) {
            return;
        }
        if let Some(j) = self.build.jobs.iter_mut().find(|j| j.key == old) {
            (j.key, j.id, j.matrix) = (new.clone(), id.to_string(), matrix);
        }
        let moved: Vec<StepKey> = self.out.keys().filter(|k| k.0 == old).cloned().collect();
        for k in moved {
            if let Some(o) = self.out.remove(&k) {
                self.out.insert((new.clone(), k.1, k.2), o);
            }
        }
        if let Some(s) = self.open.remove(old) {
            self.open.insert(new.clone(), s);
        }
        self.keys.insert(name.to_string(), new.clone());
        self.last = Some(new);
    }

    /// A line of `key`'s output.
    fn output(&mut self, key: &str, line: &str) {
        match self.open.get(key).and_then(|s| s.first()) {
            Some(o) => {
                let k = (key.to_string(), o.stage.clone(), o.id.clone());
                self.out.entry(k).or_default().read(line);
            }
            None => self.pending.push(line.to_string()),
        }
    }

    /// Output that names no job: the last job's running step's, or the only
    /// running step's; otherwise it waits for the next step to end.
    fn orphan(&mut self, line: &str) {
        let running: Vec<&String> = self
            .open
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(k, _)| k)
            .collect();
        let key = match &self.last {
            Some(k) if running.contains(&k) => Some(k.clone()),
            _ if running.len() == 1 => Some(running[0].clone()),
            _ => None,
        };
        match key {
            Some(k) => self.output(&k, line),
            None => self.pending.push(line.to_string()),
        }
    }

    /// Output no step took goes outside the steps.
    fn loose(&mut self) {
        let out = self.out.entry(loose_key()).or_default();
        for p in std::mem::take(&mut self.pending) {
            out.read(&p);
        }
    }

    fn finish(mut self) -> Results {
        self.loose();
        let Folder {
            mut build,
            mut out,
            mut errors,
            network,
            times,
            ..
        } = self;
        for o in out.values_mut() {
            o.close();
        }
        let failed = build
            .jobs
            .iter()
            .any(|j| j.failed_step.is_some() || j.state == JobState::Failure);
        let exit = i32::from(failed || !errors.is_empty());
        build.finish(Some(exit), None, times.1.unwrap_or(0));
        // In text, a job `x-2` that turned out no matrix entry keeps its name.
        for j in build.jobs.iter_mut().filter(|j| j.matrix.is_empty()) {
            j.id = j.key.clone();
        }
        let cancelled = build.cancel_requested.is_some();
        let mut jobs: Vec<Job> = build
            .jobs
            .iter()
            .map(|j| Job {
                key: j.key.clone(),
                id: j.id.clone(),
                matrix: j.matrix.clone(),
                result: job_result(j, cancelled).into(),
                ms: match (j.started, j.ended) {
                    (Some(a), Some(b)) if a > 0 && b >= a => Some((b - a) as u64 * 1000),
                    _ => None,
                },
                // Not a Pre stage that only fetched its action (act's `git
                // clone` note): only steps that ran.
                steps: j
                    .steps
                    .iter()
                    .filter_map(|s| {
                        let k = (j.key.clone(), s.stage.clone(), s.id.clone());
                        let o = out.remove(&k).unwrap_or_default();
                        let ended = s.result.as_deref().is_some_and(|r| r != "cancelled");
                        (o.ran || ended).then(|| o.step(s, false))
                    })
                    .collect(),
            })
            .collect();
        if let Some(o) = out.remove(&loose_key()).filter(Output::found) {
            let step = o.step(&actlog::Step::default(), true);
            let failed = step.failed_cases().next().is_some()
                || step.tests.iter().any(|c| c.failed > 0)
                || !step.reruns.is_empty();
            jobs.push(Job {
                result: if failed { "failure" } else { "unknown" }.into(),
                steps: vec![step],
                ..Job::default()
            });
        }
        link(&mut errors, &jobs);
        let result = if jobs.iter().any(|j| j.result == "failure") {
            "failure"
        } else if build.jobs.is_empty() {
            if errors.is_empty() {
                "unknown"
            } else {
                "error"
            }
        } else if build.state == BuildState::Success {
            "success"
        } else {
            "error"
        };
        Results {
            build: BuildInfo {
                network,
                started: times.0,
                ended: times.1,
                result: result.into(),
                ..BuildInfo::default()
            },
            jobs,
            errors,
        }
    }
}

/// act names a matrix's entries `package-1`, `package-2`: the job's id.
fn matrix_id(key: &str) -> &str {
    match key.rsplit_once('-') {
        Some((id, n))
            if !id.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) =>
        {
            id
        }
        _ => key,
    }
}

/// Points `l` at the job's running step (a composite's inner steps' lines go
/// to their step, as in act's JSON).
fn on_step(l: &mut JobLine, stack: &[Open], inner: bool) {
    let o = &stack[0];
    (l.stage, l.step, l.step_id) = (o.stage.clone(), Some(o.name.clone()), Some(o.id.clone()));
    l.inner = inner;
}

fn job_result(j: &actlog::Job, cancelled: bool) -> &'static str {
    match j.state {
        JobState::Success => "success",
        JobState::Failure => "failure",
        JobState::Skipped => "skipped",
        JobState::Unsupported => "unsupported",
        // A log that ends mid-job: a step it failed says how it went.
        JobState::Cancelled if !cancelled && j.failed_step.is_some() => "failure",
        JobState::Cancelled | JobState::Waiting | JobState::Running => "cancelled",
    }
}

/// An error naming a bana action's cache directory goes with the one failed
/// step that ran that action, when there is exactly one.
fn link(errors: &mut [LogError], jobs: &[Job]) {
    for e in errors.iter_mut().filter(|e| e.key.is_none()) {
        let Some(action) = bana_action(&e.text) else {
            continue;
        };
        let wanted = format!("bana/actions/{action}");
        let hits: Vec<(&Job, &Step)> = jobs
            .iter()
            .flat_map(|j| j.steps.iter().map(move |s| (j, s)))
            .filter(|(_, s)| s.failed() && s.name.contains(&wanted))
            .collect();
        if let [(j, s)] = hits[..] {
            (e.key, e.step) = (Some(j.key.clone()), Some(s.name.clone()));
        }
    }
}

/// The bana action an error names, by act's cache directory for it
/// (`…/tjrb-xyz-bana-actions-keep-builds@a4b6f87…/…`) or its path:
/// `keep-builds@a4b6f87…`.
fn bana_action(text: &str) -> Option<String> {
    let (_, rest) = text
        .split_once("-bana-actions-")
        .or_else(|| text.split_once("/bana/actions/"))?;
    let a: String = rest
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '/')
        .collect();
    (!a.is_empty()).then_some(a)
}

/// bana's for a step that runs one of bana's actions (`tjrb-xyz/bana/actions/
/// keep-builds@…`, `./tools/bana/actions/plan`), in any stage.
fn step_owner(name: &str) -> Owner {
    let parts: Vec<&str> = name.split('/').collect();
    let bana = parts
        .windows(3)
        .any(|w| w[0] == "bana" && w[1] == "actions" && !w[2].is_empty());
    if bana {
        Owner::Bana
    } else {
        Owner::Project
    }
}

/// An act JSON line (with a job, or its stderr as the daemon keeps it), not a
/// step's own `{…}` output in a paste.
fn act_json(raw: &str) -> bool {
    match serde_json::from_str::<Value>(raw.trim_end()) {
        Ok(Value::Object(o)) => {
            o.contains_key("jobID")
                || o.contains_key("bana")
                || (o.contains_key("level") && o.contains_key("msg"))
        }
        _ => false,
    }
}

/// `[ci/rust   ] REST`: act's name for the job (`workflow/job`), and the rest.
fn job_prefix(s: &str) -> Option<(&str, &str)> {
    let s = s.strip_prefix("*DRYRUN* ").unwrap_or(s);
    let (name, rest) = s.strip_prefix('[')?.split_once(']')?;
    let name = name.trim();
    // Not ninja's `[1/3] …`.
    let named = name.contains('/') && name.chars().any(|c| c.is_alphabetic());
    (named && (rest.is_empty() || rest.starts_with(' '))).then_some((name, rest))
}

/// `Main cargo test`: the stage and the step's name as GitHub shows it
/// (`Post x` for a post step); "Set up job" and "Complete job" have no stage.
fn stage_and_name(what: &str) -> (String, String) {
    for stage in ["Pre", "Main", "Post"] {
        if let Some(name) = what.strip_prefix(stage).and_then(|r| r.strip_prefix(' ')) {
            let name = name.trim();
            let shown = match stage {
                "Main" => name.to_string(),
                _ => format!("{stage} {name}"),
            };
            return (stage.to_string(), shown);
        }
    }
    (String::new(), what.trim().to_string())
}

/// `✅  Success - Main x [1.2s]`, `❌  Failure - Set up job`.
fn step_result(body: &str) -> Option<(&'static str, &str)> {
    let (result, rest) = match body.strip_prefix('✅') {
        Some(r) => ("success", r.trim_start().strip_prefix("Success - ")?),
        None => (
            "failure",
            body.strip_prefix('❌')?
                .trim_start()
                .strip_prefix("Failure - ")?,
        ),
    };
    Some((result, rest))
}

/// `cargo test [4m58.608749125s]`: the name, and the time in ms.
fn strip_duration(what: &str) -> (&str, Option<u64>) {
    if let Some((name, d)) = what.strip_suffix(']').and_then(|w| w.rsplit_once(" [")) {
        if let Some(ms) = go_ms(d) {
            return (name, Some(ms));
        }
    }
    (what, None)
}

/// A Go duration (`4m58.608749125s`, `161.528625ms`, `12.924µs`) in whole ms,
/// as act's `executionTime` (ns) gives them.
fn go_ms(d: &str) -> Option<u64> {
    const UNITS: [(&str, u128); 7] = [
        ("ns", 1),
        ("us", 1_000),
        ("µs", 1_000),
        ("ms", 1_000_000),
        ("s", 1_000_000_000),
        ("m", 60_000_000_000),
        ("h", 3_600_000_000_000),
    ];
    let (mut ns, mut rest) = (0u128, d);
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let n = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        let (num, tail) = rest.split_at(n);
        let (unit, per) = UNITS.into_iter().find(|(u, _)| tail.starts_with(u))?;
        let (whole, frac) = num.split_once('.').unwrap_or((num, ""));
        let frac = &frac[..frac.len().min(9)];
        if whole.is_empty() && frac.is_empty() {
            return None;
        }
        let whole: u128 = if whole.is_empty() {
            0
        } else {
            whole.parse().ok()?
        };
        let part: u128 = if frac.is_empty() {
            0
        } else {
            frac.parse().ok()?
        };
        let part = part * per / 10u128.pow(frac.len() as u32);
        ns = ns.checked_add(whole.checked_mul(per)?.checked_add(part)?)?;
        rest = &tail[unit.len()..];
    }
    u64::try_from(ns / 1_000_000).ok()
}

/// act's own lines on stderr as logrus writes them, `time="…" level=error
/// msg="…"` (or `ERRO[0003] …` on a terminal): whether it is an error, and what
/// it says.
fn logrus(s: &str) -> Option<(bool, String)> {
    let error = |level: &str| matches!(level, "error" | "fatal" | "panic");
    if let Some(rest) = s.strip_prefix("time=\"") {
        let (_, rest) = rest.split_once("\" level=")?;
        let (level, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let msg = rest.strip_prefix("msg=").unwrap_or(rest);
        let msg = msg
            .strip_prefix('"')
            .and_then(|m| m.rsplit_once('"'))
            .map_or(msg, |(m, _)| m);
        return Some((error(level), msg.replace("\\\"", "\"")));
    }
    let (tag, rest) = s.split_once('[')?;
    let level = match tag {
        "ERRO" => "error",
        "FATA" => "fatal",
        "PANI" => "panic",
        "WARN" | "INFO" | "DEBU" | "TRAC" => "",
        _ => return None,
    };
    let (n, msg) = rest.split_once(']')?;
    n.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| (error(level), msg.trim().to_string()))
}

/// The network in bana ci's first line (`act: quick from ci.yml, …, network bridge`).
fn network(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    while let Some(w) = words.next() {
        if w == "network" {
            let n = words
                .next()?
                .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || "-_:".contains(c)));
            return (!n.is_empty()).then(|| n.to_string());
        }
    }
    None
}

/// A workflow command that makes an annotation, as a step prints it or act
/// logs it (with an emoji first): `::error file=a.rs,line=3::text`.
fn annotation(msg: &str) -> Option<Annotation> {
    let s = msg
        .trim()
        .trim_start_matches(|c: char| !c.is_ascii() || c.is_whitespace());
    let rest = s.strip_prefix("::")?;
    let (level, rest) = ["error", "warning", "notice"]
        .into_iter()
        .find_map(|l| rest.strip_prefix(l).map(|r| (l, r)))?;
    let (props, message) = rest.split_once("::")?;
    if !(props.is_empty() || props.starts_with(' ')) {
        return None;
    }
    let mut a = Annotation {
        level: level.into(),
        message: unescape(message.trim_end()),
        ..Annotation::default()
    };
    for kv in props.trim().split(',') {
        match kv.split_once('=') {
            Some(("file", v)) => a.file = Some(unescape(v)),
            Some(("line", v)) => a.line = v.trim().parse().ok(),
            _ => {}
        }
    }
    Some(a)
}

/// Workflow commands escape `%`, CR and LF (and `:` and `,` in properties).
fn unescape(s: &str) -> String {
    s.replace("%0D", "\r")
        .replace("%0A", "\n")
        .replace("%3A", ":")
        .replace("%2C", ",")
        .replace("%25", "%")
}

// ---- one step's output ------------------------------------------------------

struct Panic {
    thread: String,
    at: String,
    lines: Vec<String>,
}

impl Panic {
    fn message(&self) -> Option<String> {
        (!self.lines.is_empty()).then(|| self.lines.join("\n"))
    }
}

/// What one step printed, read line by line.
#[derive(Default)]
struct Output {
    /// act said it runs (`⭐ Run …`), or it printed something.
    ran: bool,
    tail: VecDeque<String>,
    /// Summed `test result:` lines.
    cargo: Option<Count>,
    /// cargo said `error: N target(s) failed`: it ran every binary.
    all_ran: bool,
    /// The test binary cargo runs now (`Running …`, `Doc-tests …`).
    binary: Option<String>,
    nextest: Option<Count>,
    /// nextest runs: its PASS/FAIL lines count until its Summary.
    in_nextest: bool,
    cases: Vec<Case>,
    reruns: Vec<String>,
    annotations: Vec<Annotation>,
    /// A panic whose message lines come next.
    panic: Option<Panic>,
    /// Panics of tests not yet seen failing (`--nocapture` prints them first).
    panics: Vec<Panic>,
}

impl Output {
    fn keep(&mut self, line: String) {
        if self.tail.len() == TAIL {
            self.tail.pop_front();
        }
        self.tail.push_back(actlog::cut(&line, LINE_MAX));
    }

    /// One line of output, ANSI-stripped.
    fn read(&mut self, line: &str) {
        self.keep(line.to_string());
        if let Some(mut p) = self.panic.take() {
            let t = line.trim();
            let done = t.is_empty()
                || t.starts_with("note: ")
                || t.starts_with("stack backtrace:")
                || t.starts_with("thread '")
                || p.lines.len() == PANIC_LINES;
            if !done {
                p.lines.push(t.to_string());
                self.panic = Some(p);
                return;
            }
            self.panicked(p);
        }
        // libtest's lines at column 0 only: nextest indents them in a failing
        // test's output, and counts that test itself.
        let t = line.trim_start();
        if let Some(rest) = line.strip_prefix("test result: ") {
            if let Some(c) = libtest_counts(rest) {
                self.cargo
                    .get_or_insert_with(|| Count::new("cargo"))
                    .add(&c);
            }
        } else if let Some(rest) = line.strip_prefix("test ") {
            if let Some((name, result)) = libtest_case(rest) {
                self.case(name, result, self.binary.clone());
            }
        } else if let Some(rest) = line.strip_prefix("error: test failed, to rerun pass `") {
            if let Some((target, _)) = rest.split_once('`') {
                if !self.reruns.iter().any(|r| r == target) {
                    self.reruns.push(target.to_string());
                }
            }
        } else if line.starts_with("error: ")
            && (line.contains(" target failed") || line.contains(" targets failed"))
        {
            self.all_ran = true;
        } else if let Some(rest) = t.strip_prefix("Running ") {
            // `Running unittests src/lib.rs (target/debug/deps/demo-7f33b6e5)`
            if let Some((binary, _)) = rest
                .trim_end()
                .strip_suffix(')')
                .and_then(|r| r.rsplit_once(" ("))
            {
                self.binary = Some(binary.to_string());
            }
        } else if t.starts_with("Doc-tests ") {
            self.binary = Some(t.trim_end().to_string());
        } else if t.starts_with("thread '") {
            self.panic_line(t);
        } else if let Some(c) = nextest_summary(t) {
            self.in_nextest = false;
            self.nextest
                .get_or_insert_with(|| Count::new("nextest"))
                .add(&c);
        } else if t.starts_with("Nextest run ID")
            || (t.starts_with("Starting ") && t.contains(" across "))
        {
            self.in_nextest = true;
        } else if let Some((result, binary, name)) =
            self.in_nextest.then(|| nextest_case(t)).flatten()
        {
            self.case(name, result, Some(binary.to_string()));
        } else if let Some(a) = annotation(line) {
            self.annotations.push(a);
        }
    }

    /// `thread 'NAME' (TID) panicked at FILE:LINE:COL:`; its message follows.
    fn panic_line(&mut self, t: &str) {
        let Some((thread, rest)) = t.strip_prefix("thread '").and_then(|r| r.split_once("' "))
        else {
            return;
        };
        let rest = match rest.strip_prefix('(') {
            Some(r) => r.split_once(") ").map_or(rest, |(_, r)| r),
            None => rest,
        };
        let Some(at) = rest.strip_prefix("panicked at ") else {
            return;
        };
        // Before Rust 1.73: `panicked at 'MESSAGE', FILE:LINE:COL`.
        if let Some((msg, at)) = at.strip_prefix('\'').and_then(|a| a.rsplit_once("', ")) {
            return self.panicked(Panic {
                thread: thread.into(),
                at: at.trim().into(),
                lines: vec![msg.into()],
            });
        }
        self.panic = Some(Panic {
            thread: thread.into(),
            at: at.trim_end().trim_end_matches(':').into(),
            lines: Vec::new(),
        });
    }

    /// A panic goes with its test, the latest one of that name that failed.
    fn panicked(&mut self, p: Panic) {
        let test = self
            .cases
            .iter_mut()
            .rev()
            .find(|c| c.name == p.thread && c.result == "failed" && c.at.is_none());
        match test {
            Some(c) => (c.at, c.message) = (Some(p.at.clone()), p.message()),
            None => self.panics.push(p),
        }
    }

    fn case(&mut self, name: &str, result: &str, binary: Option<String>) {
        let mut c = Case {
            name: name.into(),
            result: result.into(),
            binary,
            ..Case::default()
        };
        if result == "failed" {
            if let Some(i) = self.panics.iter().position(|p| p.thread == name) {
                let p = self.panics.remove(i);
                (c.at, c.message) = (Some(p.at.clone()), p.message());
            }
        }
        self.cases.push(c);
    }

    /// The step's output has ended.
    fn close(&mut self) {
        if let Some(p) = self.panic.take() {
            self.panicked(p);
        }
    }

    /// Something worth keeping outside any step.
    fn found(&self) -> bool {
        self.cargo.is_some()
            || self.nextest.is_some()
            || !self.cases.is_empty()
            || !self.reruns.is_empty()
            || !self.annotations.is_empty()
            || !self.panics.is_empty()
    }

    fn step(self, s: &actlog::Step, keep_tail: bool) -> Step {
        let mut tests = Vec::new();
        // Without `--no-fail-fast`, cargo stops at the first binary that
        // fails: it says what to rerun, and never that N targets failed.
        if self.cargo.is_some() || !self.reruns.is_empty() {
            let mut c = self.cargo.unwrap_or_else(|| Count::new("cargo"));
            c.incomplete = !self.reruns.is_empty() && !self.all_ran;
            tests.push(c);
        }
        tests.extend(self.nextest);
        let failed = s.result.as_deref() == Some("failure");
        Step {
            name: s.name.clone(),
            stage: s.stage.clone(),
            result: s.result.clone(),
            ms: s.ms,
            owner: step_owner(&s.name),
            continued: s.continued,
            tests,
            cases: self.cases,
            reruns: self.reruns,
            annotations: self.annotations,
            tail: if failed || keep_tail {
                self.tail.into()
            } else {
                Vec::new()
            },
        }
    }
}

/// `FAILED. 21 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.84s`
fn libtest_counts(rest: &str) -> Option<Count> {
    let (_, counts) = rest.split_once(". ")?;
    let n = |what: &str| {
        counts.split(';').find_map(|item| {
            let (num, word) = item.trim().split_once(' ')?;
            (word == what).then(|| num.parse::<u64>().ok()).flatten()
        })
    };
    Some(Count {
        tool: "cargo".into(),
        passed: n("passed")?,
        failed: n("failed")?,
        skipped: n("ignored").unwrap_or(0),
        incomplete: false,
    })
}

/// `NAME ... ok`, `NAME ... FAILED`, `NAME ... ignored, needs hardware`.
fn libtest_case(rest: &str) -> Option<(&str, &'static str)> {
    let (name, result) = rest.split_once(" ... ")?;
    let result = match result.split([' ', ',']).next()? {
        "ok" => "passed",
        "FAILED" => "failed",
        "ignored" => "skipped",
        _ => return None,
    };
    Some((name, result))
}

/// nextest's last word: `Summary [   0.015s] 5 tests run: 4 passed, 1 failed,
/// 1 skipped`, or `8/10 tests run: 5 passed (1 slow, 1 flaky), 2 failed, 1 exec
/// failed, 1 timed out, 2 skipped` when it stopped early.
fn nextest_summary(t: &str) -> Option<Count> {
    let (_, rest) = t.strip_prefix("Summary [")?.split_once("] ")?;
    let (run, items) = rest.split_once(" run:")?;
    let ran = run.split_whitespace().next()?;
    let mut c = Count::new("nextest");
    c.incomplete = ran.split_once('/').is_some_and(|(a, b)| a != b);
    let mut depth = 0u32;
    let plain: String = items
        .chars()
        .filter(|&ch| {
            match ch {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => return depth == 0,
            }
            false
        })
        .collect();
    for item in plain.split(',') {
        let Some((num, what)) = item.trim().split_once(' ') else {
            continue;
        };
        let Ok(num) = num.parse::<u64>() else {
            continue;
        };
        match what.trim() {
            "passed" => c.passed += num,
            "failed" | "exec failed" | "timed out" => c.failed += num,
            "skipped" => c.skipped += num,
            _ => {}
        }
    }
    Some(c)
}

/// `PASS [   0.008s] (1/5) demo::facts fact_one`: the result, the binary, the test.
fn nextest_case(t: &str) -> Option<(&'static str, &str, &str)> {
    let t = match t.strip_prefix("TRY ") {
        Some(r) => r.split_once(' ')?.1.trim_start(),
        None => t,
    };
    let (status, rest) = t.split_once(" [")?;
    let result = match status {
        "PASS" | "LEAK" => "passed",
        "SKIP" => "skipped",
        "FAIL" | "TIMEOUT" | "ABORT" | "LEAK-FAIL" => "failed",
        s if s.starts_with("SIG") && s.bytes().all(|b| b.is_ascii_uppercase()) => "failed",
        _ => return None,
    };
    let rest = rest.split_once("] ")?.1.trim_start();
    let rest = match rest.strip_prefix('(') {
        Some(r) => r.split_once(") ")?.1,
        None => rest,
    };
    let (binary, name) = rest.split_once(' ')?;
    Some((result, binary, name.trim()))
}

// ---- results.jsonl ------------------------------------------------------------

/// One line of results.jsonl.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Line {
    Build(BuildInfo),
    Job {
        key: String,
        job: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        matrix: BTreeMap<String, String>,
        result: String,
        #[serde(default)]
        ms: Option<u64>,
    },
    Step {
        key: String,
        step: String,
        #[serde(default)]
        stage: String,
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        ms: Option<u64>,
        #[serde(default)]
        owner: Owner,
        #[serde(default, skip_serializing_if = "is_false")]
        continued: bool,
    },
    Tests(Keyed<Count>),
    Test(Keyed<Case>),
    Rerun {
        key: String,
        step: String,
        target: String,
    },
    Annotation(Keyed<Annotation>),
    /// A `::notice::`; whether it means "left out" is the report's to say.
    Notice {
        key: String,
        step: String,
        text: String,
        #[serde(default)]
        left_out: bool,
    },
    Tail {
        key: String,
        step: String,
        lines: Vec<String>,
    },
    Error(LogError),
}

/// A line about one step: its job's key and its name, then what it says.
#[derive(Debug, Serialize, Deserialize)]
struct Keyed<T> {
    key: String,
    step: String,
    #[serde(flatten)]
    item: T,
}

fn keyed<T: Clone>(key: &str, step: &str, item: &T) -> Keyed<T> {
    Keyed {
        key: key.to_string(),
        step: step.to_string(),
        item: item.clone(),
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Results {
    /// results.jsonl: the build, then each job with its steps and what they
    /// printed, then the errors outside the jobs; one JSON object per line.
    pub fn to_jsonl(&self) -> String {
        let mut lines = vec![Line::Build(self.build.clone())];
        for j in &self.jobs {
            lines.push(Line::Job {
                key: j.key.clone(),
                job: j.id.clone(),
                matrix: j.matrix.clone(),
                result: j.result.clone(),
                ms: j.ms,
            });
            for s in &j.steps {
                let (key, step) = (j.key.as_str(), s.name.as_str());
                lines.push(Line::Step {
                    key: key.into(),
                    step: step.into(),
                    stage: s.stage.clone(),
                    result: s.result.clone(),
                    ms: s.ms,
                    owner: s.owner,
                    continued: s.continued,
                });
                lines.extend(s.tests.iter().map(|c| Line::Tests(keyed(key, step, c))));
                lines.extend(s.cases.iter().map(|c| Line::Test(keyed(key, step, c))));
                lines.extend(s.reruns.iter().map(|t| Line::Rerun {
                    key: key.into(),
                    step: step.into(),
                    target: t.clone(),
                }));
                lines.extend(s.annotations.iter().map(|a| match a.level.as_str() {
                    "notice" => Line::Notice {
                        key: key.into(),
                        step: step.into(),
                        text: a.message.clone(),
                        left_out: false,
                    },
                    _ => Line::Annotation(keyed(key, step, a)),
                }));
                if !s.tail.is_empty() {
                    lines.push(Line::Tail {
                        key: key.into(),
                        step: step.into(),
                        lines: s.tail.clone(),
                    });
                }
            }
        }
        lines.extend(self.errors.iter().cloned().map(Line::Error));
        let mut out = String::new();
        for l in &lines {
            if let Ok(s) = serde_json::to_string(l) {
                out += &s;
                out.push('\n');
            }
        }
        out
    }

    /// results.jsonl read back; a line it cannot read is skipped.
    pub fn from_jsonl(text: &str) -> Self {
        let mut r = Self::default();
        for line in text.lines() {
            let Ok(l) = serde_json::from_str::<Line>(line) else {
                continue;
            };
            match l {
                Line::Build(b) => r.build = b,
                Line::Job {
                    key,
                    job,
                    matrix,
                    result,
                    ms,
                } => r.jobs.push(Job {
                    key,
                    id: job,
                    matrix,
                    result,
                    ms,
                    steps: Vec::new(),
                }),
                Line::Step {
                    key,
                    step,
                    stage,
                    result,
                    ms,
                    owner,
                    continued,
                } => r.job_mut(&key).steps.push(Step {
                    name: step,
                    stage,
                    result,
                    ms,
                    owner,
                    continued,
                    ..Step::default()
                }),
                Line::Tests(k) => r.step_mut(&k.key, &k.step).tests.push(k.item),
                Line::Test(k) => r.step_mut(&k.key, &k.step).cases.push(k.item),
                Line::Rerun { key, step, target } => r.step_mut(&key, &step).reruns.push(target),
                Line::Annotation(k) => r.step_mut(&k.key, &k.step).annotations.push(k.item),
                Line::Notice {
                    key, step, text, ..
                } => {
                    let a = Annotation {
                        level: "notice".into(),
                        message: text,
                        ..Annotation::default()
                    };
                    r.step_mut(&key, &step).annotations.push(a);
                }
                Line::Tail { key, step, lines } => r.step_mut(&key, &step).tail = lines,
                Line::Error(e) => r.errors.push(e),
            }
        }
        r
    }

    fn job_mut(&mut self, key: &str) -> &mut Job {
        let i = match self.jobs.iter().rposition(|j| j.key == key) {
            Some(i) => i,
            None => {
                self.jobs.push(Job {
                    key: key.into(),
                    result: "unknown".into(),
                    ..Job::default()
                });
                self.jobs.len() - 1
            }
        };
        &mut self.jobs[i]
    }

    /// The step records of a line go to: the latest step of that name, as
    /// [`Results::to_jsonl`] writes a step's records right after it.
    fn step_mut(&mut self, key: &str, step: &str) -> &mut Step {
        let job = self.job_mut(key);
        let i = match job.steps.iter().rposition(|s| s.name == step) {
            Some(i) => i,
            None => {
                job.steps.push(Step {
                    name: step.into(),
                    ..Step::default()
                });
                job.steps.len() - 1
            }
        };
        &mut job.steps[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's log of a failed dsper run under act on a Mac, as pasted (its
    /// repeated "Error occurred running finally" shortened); and the same lines
    /// as act.jsonl would have them (the text is the paste's; the ids and times
    /// around it are made up in act's shape).
    const PASTE: &str = include_str!("../tests/fixtures/results/dsper-paste.txt");
    const PASTE_JSON: &str = include_str!("../tests/fixtures/results/dsper-paste.jsonl");
    /// A real act 0.2.89 run of act-run1.yml (research's run 1): a host-mode
    /// job's cargo test, through a PTY, and nextest; a node job with
    /// annotations and step summaries; a matrix; artifacts.
    const RUN1: &str = include_str!("../tests/fixtures/results/act-run1.jsonl");
    /// cargo 1.94 and cargo-nextest 0.9.146 as they print, on a demo crate.
    const CARGO: &str = include_str!("../tests/fixtures/results/cargo-test.txt");
    const CARGO_ALL: &str = include_str!("../tests/fixtures/results/cargo-test-no-fail-fast.txt");
    const NEXTEST: &str = include_str!("../tests/fixtures/results/nextest.txt");

    const KEEP_BUILDS: &str =
        "tjrb-xyz/bana/actions/keep-builds@a4b6f87212d190304c530041b9bbd5fed72f0dd3";

    fn step<'a>(r: &'a Results, key: &str, name: &str) -> &'a Step {
        r.jobs
            .iter()
            .filter(|j| j.key == key)
            .flat_map(|j| &j.steps)
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no step {key} › {name}"))
    }

    fn failures(r: &Results) -> Vec<(&str, &str, Owner)> {
        r.failures()
            .into_iter()
            .map(|(j, s)| (j.key.as_str(), s.name.as_str(), s.owner))
            .collect()
    }

    fn count(tool: &str, passed: u64, failed: u64, skipped: u64, incomplete: bool) -> Count {
        Count {
            tool: tool.into(),
            passed,
            failed,
            skipped,
            incomplete,
        }
    }

    #[test]
    fn the_owners_paste_as_text_and_as_act_json() {
        let text = fold_text(PASTE);
        let json = fold_json(PASTE_JSON);
        for r in [&text, &json] {
            assert_eq!(r.build.result, "failure");
            assert_eq!(
                failures(r),
                [("rust", "cargo test --workspace", Owner::Project)]
            );
            let s = step(r, "rust", "cargo test --workspace");
            assert_eq!(s.ms, Some(298_608), "[4m58.608749125s]");
            let bad: Vec<&Case> = s.failed_cases().collect();
            assert_eq!(bad.len(), 1);
            assert_eq!(
                bad[0].name,
                "real_c3_the_engine_accepts_only_its_token_and_no_origin"
            );
            assert_eq!(
                bad[0].at.as_deref(),
                Some("crates/dsper-engine/tests/facts.rs:457:18")
            );
            assert_eq!(bad[0].message.as_deref(), Some("accepted"));
            assert_eq!(s.cases.len(), 7, "the paste starts at the last tests");
            assert_eq!(s.reruns, ["-p dsper-engine --test facts"]);
            assert_eq!(s.tests, [count("cargo", 21, 1, 0, true)]);
            assert!(
                s.incomplete(),
                "no --no-fail-fast: later binaries never ran"
            );
            assert_eq!(s.tail.len(), 23, "every line before act's");
            assert_eq!(
                s.tail.last().map(String::as_str),
                Some("error: test failed, to rerun pass `-p dsper-engine --test facts`")
            );
            let post = step(r, "rust", &format!("Post {KEEP_BUILDS}"));
            assert_eq!(
                (post.owner, post.result.as_deref(), post.ms),
                (Owner::Bana, Some("success"), Some(32))
            );
            assert!(post.tail.is_empty(), "only a failed step keeps its lines");
            let toolchain = step(r, "rust", "Post dtolnay/rust-toolchain@stable");
            assert_eq!(toolchain.owner, Owner::Project);
            assert_eq!(r.errors.len(), 1);
            let e = &r.errors[0];
            assert_eq!(
                e.owner,
                Owner::Bana,
                "it names keep-builds' cache directory"
            );
            assert!(
                e.text.starts_with("Error occurred running finally: ")
                    && e.text.contains("symlink log-only /Users/lilly/.cache/act/")
                    && e.text.contains("/tests/stand-ins/apt-get: file exists"),
                "{}",
                e.text
            );
            assert_eq!(e.key, None, "rust's keep-builds Post passed: not that one");
        }
        assert_eq!(
            text.jobs,
            json.jobs
                .iter()
                .cloned()
                .map(|j| Job { ms: None, ..j })
                .collect::<Vec<_>>(),
            "the text says what act's JSON says, but for the times"
        );
        assert_eq!(text.errors, json.errors);
        assert_eq!(json.jobs[0].ms, Some(299_000));
        assert_eq!(
            (json.build.started, json.build.ended),
            (
                actlog::parse_time("2026-09-28T12:04:59Z"),
                actlog::parse_time("2026-09-28T12:09:58Z")
            )
        );
    }

    #[test]
    fn the_macos_keep_builds_post_failure_is_banas() {
        // The macOS job, which the paste leaves out, as act prints it: its
        // keep-builds Post fails on the symlink, and act's error names it.
        let json: Vec<String> = [
            (format!("⭐ Run Post {KEEP_BUILDS}"), ""),
            (
                format!("  ❌  Failure - Post {KEEP_BUILDS} [12.5ms]"),
                r#","stepResult":"failure","executionTime":12500000"#,
            ),
        ]
        .into_iter()
        .map(|(msg, extra)| {
            format!(
                r#"{{"job":"ci/macos","jobID":"macos","level":"info","matrix":{{}},"msg":"{msg}","stage":"Post","step":"{KEEP_BUILDS}","stepID":["1"]{extra}}}"#
            )
        })
        .chain([r#"{"job":"ci/macos","jobID":"macos","level":"info","matrix":{},"msg":"🏁  Job failed","jobResult":"failure"}"#.to_string()])
        .collect();
        let text = format!(
            "[ci/macos] ⭐ Run Post {KEEP_BUILDS}\n[ci/macos]   ❌  Failure - Post {KEEP_BUILDS} [12.5ms]\n[ci/macos] 🏁  Job failed\n"
        );
        let at = PASTE_JSON.find(r#"{"bana":"stderr""#).unwrap();
        let json = fold_json(&format!(
            "{}{}\n{}",
            &PASTE_JSON[..at],
            json.join("\n"),
            &PASTE_JSON[at..]
        ));
        let text = fold_text(&PASTE.replace("Error: ", &format!("{text}Error: ")));
        let post = format!("Post {KEEP_BUILDS}");
        for r in [json, text] {
            assert_eq!(
                failures(&r),
                [
                    ("rust", "cargo test --workspace", Owner::Project),
                    ("macos", post.as_str(), Owner::Bana)
                ]
            );
            let e = &r.errors[0];
            assert_eq!(e.owner, Owner::Bana);
            assert_eq!(
                (e.key.as_deref(), e.step.as_deref()),
                (Some("macos"), Some(post.as_str())),
                "the one failed step that ran that keep-builds"
            );
        }
    }

    #[test]
    fn a_host_mode_run_through_a_pty() {
        assert!(
            RUN1.contains(r#""test result: \u001b[31mFAILED\u001b[m\u000f. 1 passed; "#),
            "cargo's line from a PTY"
        );
        let r = fold_json(RUN1);
        let cargo = step(&r, "host", "cargo test");
        assert_eq!(
            cargo.tests,
            [count("cargo", 5, 1, 1, false)],
            "--no-fail-fast: every binary ran"
        );
        assert_eq!(cargo.reruns, ["--lib"]);
        let bad: Vec<&Case> = cargo.failed_cases().collect();
        assert_eq!(
            (
                bad[0].name.as_str(),
                bad[0].at.as_deref(),
                bad[0].message.as_deref()
            ),
            (
                "tests::accepted",
                Some("src/lib.rs:12:45"),
                Some("accepted")
            )
        );
        let binaries: Vec<(&str, Option<&str>)> = cargo
            .cases
            .iter()
            .map(|c| (c.name.as_str(), c.binary.as_deref()))
            .collect();
        assert_eq!(
            binaries,
            [
                ("tests::slow", Some("unittests src/lib.rs")),
                ("tests::accepted", Some("unittests src/lib.rs")),
                ("tests::adds", Some("unittests src/lib.rs")),
                ("fact_one", Some("tests/facts.rs")),
                ("real::raw_file_end_stops_with_done", Some("tests/facts.rs")),
                ("fact_two", Some("tests/facts.rs")),
                ("src/lib.rs - add (line 3)", Some("Doc-tests demo")),
            ]
        );
        // nextest prints libtest's lines, indented, in a failing test's output.
        let nextest = step(&r, "host", "nextest");
        assert_eq!(nextest.tests, [count("nextest", 4, 1, 1, false)]);
        assert_eq!(nextest.cases.len(), 5, "not its closing FAIL line again");
        let bad: Vec<&Case> = nextest.failed_cases().collect();
        assert_eq!(
            (
                bad[0].name.as_str(),
                bad[0].binary.as_deref(),
                bad[0].at.as_deref()
            ),
            ("tests::accepted", Some("demo"), Some("src/lib.rs:12:45"))
        );
        let notes: Vec<(&str, &str, Option<&str>, Option<u64>)> = step(&r, "node", "annotations")
            .annotations
            .iter()
            .map(|a| {
                (
                    a.level.as_str(),
                    a.message.as_str(),
                    a.file.as_deref(),
                    a.line,
                )
            })
            .collect();
        assert_eq!(
            notes,
            [
                ("notice", "a notice", None, None),
                ("warning", "a warning", Some("demo.test.mjs"), Some(3)),
                ("error", "accepted", Some("demo.test.mjs"), Some(4)),
            ]
        );
        assert_eq!(
            failures(&r),
            [
                ("host", "cargo test", Owner::Project),
                ("host", "nextest", Owner::Project),
                ("host", "actions/upload-artifact@v4", Owner::Project),
                ("node", "node tests", Owner::Project),
            ]
        );
        assert!(
            !r.jobs
                .iter()
                .flat_map(|j| &j.steps)
                .any(|s| s.stage == "Pre"),
            "a Pre stage that only cloned its action did not run"
        );
        // The jobs as actlog has them for the statuses.
        let mut b = actlog::Build::default();
        b.fold_lines(RUN1, 0);
        b.finish(Some(1), None, 0);
        let states: Vec<(&str, &str)> = b
            .jobs
            .iter()
            .map(|j| (j.key.as_str(), job_result(j, false)))
            .collect();
        let ours: Vec<(&str, &str)> = r
            .jobs
            .iter()
            .map(|j| (j.key.as_str(), j.result.as_str()))
            .collect();
        assert_eq!(ours, states);
        assert!(ours.contains(&("matrix (arm64)", "success")));
    }

    #[test]
    fn cargo_and_nextest_as_they_print() {
        // Pasted alone, with no act line around them: a job of their own, keyed "".
        let r = fold_text(CARGO);
        assert_eq!((r.jobs.len(), r.jobs[0].key.as_str()), (1, ""));
        assert_eq!(r.build.result, "failure");
        let s = &r.jobs[0].steps[0];
        assert_eq!(s.tests, [count("cargo", 1, 1, 1, true)]);
        assert_eq!(s.reruns, ["--lib"]);
        let bad: Vec<&Case> = s.failed_cases().collect();
        assert_eq!(
            (bad[0].at.as_deref(), bad[0].message.as_deref()),
            (Some("src/lib.rs:12:45"), Some("accepted")),
            "the backtrace after it is not its message"
        );
        assert_eq!(s.tail.len(), 37, "all its lines, with no step to fail");

        let r = fold_text(CARGO_ALL);
        let s = &r.jobs[0].steps[0];
        assert_eq!(s.tests, [count("cargo", 5, 1, 1, false)]);
        assert!(!s.incomplete(), "1 target failed: every binary ran");
        assert_eq!(s.cases.len(), 7);

        let r = fold_text(NEXTEST);
        let s = &r.jobs[0].steps[0];
        assert_eq!(
            s.tests,
            [count("nextest", 4, 1, 1, false)],
            "no cargo count"
        );
        assert_eq!(s.cases.len(), 5);
        assert_eq!(s.failed_cases().count(), 1);
        assert_eq!(
            s.failed_cases().next().unwrap().message.as_deref(),
            Some("accepted")
        );
    }

    /// act's JSON lines as act prints them without --json: `[job] msg` and
    /// `[job]   | output`, or, to a terminal, `\x1b[34m[job] \x1b[0mmsg` and
    /// `\x1b[34m|\x1b[0m output` with a colour for each job (logger.go's print
    /// and printColored).
    fn as_text(jsonl: &str, colour: bool) -> String {
        let colours = [34, 33, 32, 35, 31, 37, 36];
        let mut jobs: Vec<String> = Vec::new();
        let mut out = String::new();
        for line in jsonl.lines() {
            let v: Value = match serde_json::from_str(line) {
                Ok(v @ Value::Object(_)) => v,
                _ => {
                    out += line;
                    out.push('\n');
                    continue;
                }
            };
            let msg = v["msg"].as_str().unwrap_or("");
            let msg = msg.strip_suffix('\n').unwrap_or(msg);
            let text = match v["job"].as_str() {
                Some(job) => {
                    let debug = if v["level"] == "debug" {
                        "[DEBUG] "
                    } else {
                        ""
                    };
                    let n = jobs.iter().position(|j| j == job).unwrap_or_else(|| {
                        jobs.push(job.to_string());
                        jobs.len() - 1
                    });
                    let c = colours[n % colours.len()];
                    match (v["raw_output"] == true, colour) {
                        (true, true) => format!("\x1b[{c}m|\x1b[0m {msg}"),
                        (true, false) => format!("[{job}]   | {msg}"),
                        (false, true) => format!("\x1b[{c}m[{job}] \x1b[0m{debug}{msg}"),
                        (false, false) => format!("[{job}] {debug}{msg}"),
                    }
                }
                None if v["bana"].is_string() => msg.to_string(),
                None => format!(
                    "time=\"{}\" level={} msg=\"{}\"",
                    v["time"].as_str().unwrap_or(""),
                    v["level"].as_str().unwrap_or("info"),
                    msg.replace('"', "\\\"")
                ),
            };
            out += &text;
            out.push('\n');
        }
        out
    }

    #[test]
    fn acts_plain_text_says_what_its_json_says() {
        macro_rules! act {
            ($name:literal) => {
                (
                    $name,
                    include_str!(concat!("../tests/fixtures/act/", $name, ".jsonl")),
                )
            };
        }
        let runs = [
            act!("pass"),
            act!("fail"),
            act!("matrix"),
            act!("matrix-fail"),
            act!("skip"),
            act!("platform"),
            act!("composite"),
            act!("sigint"),
            act!("sigint-twice"),
            act!("sigint-container"),
            act!("sigkill"),
            act!("syntax"),
            ("run1", RUN1),
            ("paste", PASTE_JSON),
        ];
        for (name, jsonl) in runs {
            let json = fold_json(jsonl);
            for colour in [false, true] {
                let text = fold_text(&as_text(jsonl, colour));
                let what = format!("{name}, coloured: {colour}");
                assert_eq!(text.build.result, json.build.result, "{what}");
                assert_eq!(text.errors, json.errors, "{what}");
                assert_eq!(text.jobs.len(), json.jobs.len(), "{what}");
                for (t, j) in text.jobs.iter().zip(&json.jobs) {
                    // In text a job's key is its name: composite's is "Rust tests".
                    if name != "composite" {
                        assert_eq!(t.key, j.key, "{what}");
                    }
                    assert_eq!(t.result, j.result, "{what}: {}", j.key);
                    assert_eq!(t.steps, j.steps, "{what}: {}", j.key);
                }
                assert_eq!(failures(&text), failures(&json), "{what}");
            }
        }
    }

    #[test]
    fn a_real_act_run_printed_three_ways() {
        // modes.yml run by act 0.2.89 three times on one machine: piped (as
        // bana ci's log has it), to a terminal (CLICOLOR_FORCE=1), and --json.
        let runs = [
            fold_text(include_str!("../tests/fixtures/results/modes-plain.txt")),
            fold_text(include_str!("../tests/fixtures/results/modes-colour.txt")),
            fold_json(include_str!("../tests/fixtures/results/modes.jsonl")),
        ];
        // Three runs: the jobs end in another order, and take other times.
        let jobs = |r: &Results| {
            let mut jobs = r.jobs.clone();
            jobs.sort_by(|a, b| a.key.cmp(&b.key));
            for j in &mut jobs {
                j.ms = None;
                j.steps.iter_mut().for_each(|s| s.ms = None);
            }
            jobs
        };
        for r in &runs {
            assert_eq!(jobs(r), jobs(&runs[2]));
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            assert_eq!(
                failures(r),
                [("rust", "cargo test --workspace", Owner::Project)]
            );
            let s = step(r, "rust", "cargo test --workspace");
            assert_eq!(s.tests, [count("cargo", 1, 1, 0, true)]);
            let bad = s.failed_cases().next().unwrap();
            assert_eq!(
                (bad.name.as_str(), bad.at.as_deref()),
                ("a::two", Some("src/a.rs:3:5"))
            );
            let lint = step(r, "web (b)", "lint");
            assert_eq!(
                (
                    lint.annotations[0].file.as_deref(),
                    lint.annotations[0].line
                ),
                (Some("a.ts"), Some(2))
            );
            let tails = r.jobs.iter().flat_map(|j| &j.steps).flat_map(|s| &s.tail);
            assert!(
                !tails.clone().any(|l| l.contains("| a | b |")),
                "a step summary's table is no step's output"
            );
        }
    }

    #[test]
    fn a_last_line_without_its_newline() {
        // act drops a step's last line when it has no newline; another builder
        // may not.
        let json = r#"{"jobID":"a","matrix":{},"step":"t","stepID":["0"],"stage":"Main","msg":"test result: ok. 2 passed; 0 failed; 0 ignored","raw_output":true}
{"jobID":"a","matrix":{},"step":"t","stepID":["0"],"stage":"Main","msg":"no newline at end","raw_output":true}
{"jobID":"a","matrix":{},"step":"t","stepID":["0"],"stage":"Main","msg":"x","stepResult":"failure","executionTime":2000000}
{"jobID":"a","matrix":{},"msg":"done","jobResult":"failure"}"#;
        let text = "[ci/a] ⭐ Run Main t\n[ci/a]   | test result: ok. 2 passed; 0 failed; 0 ignored\n[ci/a]   | no newline at end\n[ci/a]   ❌  Failure - Main t [2ms]";
        for r in [fold_json(json), fold_text(text)] {
            let s = step(&r, "a", "t");
            assert_eq!((s.result.as_deref(), s.ms), (Some("failure"), Some(2)));
            assert_eq!(
                s.tail,
                [
                    "test result: ok. 2 passed; 0 failed; 0 ignored",
                    "no newline at end"
                ]
            );
            assert_eq!(s.tests, [count("cargo", 2, 0, 0, false)]);
        }
        let r = fold_text("test a ... FAILED\nerror: test failed, to rerun pass `--lib`");
        assert_eq!(r.jobs[0].steps[0].reruns, ["--lib"]);
    }

    #[test]
    fn a_custom_builders_act_shaped_lines() {
        // A builder other than act prints act's subset: jobID, matrix, step,
        // stepID, stage, msg with raw_output, stepResult with executionTime,
        // jobResult.
        let lines = r#"{"jobID":"windows","matrix":{},"step":"cargo test","stepID":["0"],"stage":"Main","msg":"test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n","raw_output":true,"time":"2026-09-29T08:00:00Z"}
{"jobID":"windows","matrix":{},"step":"cargo test","stepID":["0"],"stage":"Main","msg":"ok","stepResult":"success","executionTime":1200000000,"time":"2026-09-29T08:00:02Z"}
{"jobID":"windows","matrix":{},"msg":"done","jobResult":"success","time":"2026-09-29T08:00:02Z"}
{"jobID":"xcode","matrix":{"sdk":"macosx"},"step":"xcodebuild test","stepID":["0"],"stage":"Main","msg":"** TEST FAILED **\n","raw_output":true,"time":"2026-09-29T08:00:00Z"}
{"jobID":"xcode","matrix":{"sdk":"macosx"},"step":"xcodebuild test","stepID":["0"],"stage":"Main","msg":"failed","stepResult":"failure","time":"2026-09-29T08:00:03Z"}
{"jobID":"xcode","matrix":{"sdk":"macosx"},"msg":"done","jobResult":"failure","time":"2026-09-29T08:00:03Z"}"#;
        let r = fold_json(lines);
        let jobs: Vec<(&str, &str, Option<u64>)> = r
            .jobs
            .iter()
            .map(|j| (j.key.as_str(), j.result.as_str(), j.ms))
            .collect();
        assert_eq!(
            jobs,
            [
                ("windows", "success", Some(2000)),
                ("xcode (macosx)", "failure", Some(3000))
            ]
        );
        let windows = step(&r, "windows", "cargo test");
        assert_eq!(
            (windows.ms, &windows.tests[..]),
            (Some(1200), &[count("cargo", 3, 0, 0, false)][..])
        );
        let xcode = step(&r, "xcode (macosx)", "xcodebuild test");
        assert_eq!(xcode.tail, ["** TEST FAILED **"]);
        assert_eq!(r.build.result, "failure");
        // actlog folds the same lines for the statuses, unchanged.
        let mut b = actlog::Build::default();
        b.fold_lines(lines, 0);
        b.finish(Some(1), None, 0);
        let states: Vec<(&str, JobState, Option<&str>)> = b
            .jobs
            .iter()
            .map(|j| (j.key.as_str(), j.state, j.failed_step.as_deref()))
            .collect();
        assert_eq!(
            states,
            [
                ("windows", JobState::Success, None),
                ("xcode (macosx)", JobState::Failure, Some("xcodebuild test"))
            ]
        );
    }

    #[test]
    fn panics_and_their_tests() {
        // --nocapture prints a panic before its test's FAILED line; before Rust
        // 1.73 the message was on the panic's own line.
        let log = "running 2 tests
thread 'a::one' panicked at 'boom', src/a.rs:3:5
test a::one ... FAILED
thread 'a::two' (77) panicked at src/a.rs:9:1:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test a::two ... FAILED
test a::three ... ignored, needs hardware
test result: FAILED. 0 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s
";
        let r = fold_text(log);
        let cases: Vec<(&str, &str, Option<&str>, Option<&str>)> = r.jobs[0].steps[0]
            .cases
            .iter()
            .map(|c| {
                (
                    c.name.as_str(),
                    c.result.as_str(),
                    c.at.as_deref(),
                    c.message.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            cases,
            [
                ("a::one", "failed", Some("src/a.rs:3:5"), Some("boom")),
                (
                    "a::two",
                    "failed",
                    Some("src/a.rs:9:1"),
                    Some("assertion `left == right` failed\nleft: 1\nright: 2")
                ),
                ("a::three", "skipped", None, None),
            ]
        );
    }

    #[test]
    fn what_act_and_bana_say_outside_the_jobs() {
        let log = "\x1b[1mact: quick from ci.yml, Linux jobs in img (linux/arm64, network host), macOS jobs on this Mac\x1b[0m
\x1b[33mtest result: ok. 1 passed; 0 failed; 0 ignored (bana's words in colour)\x1b[0m
time=\"2026-09-29T07:30:01Z\" level=info msg=\"Using docker host 'unix:///var/run/docker.sock'\"
[ci/rust] ⭐ Run Main t
[ci/rust]   | Error: the step's own words
[ci/rust]   ✅  Success - Main t [1ms]
[ci/rust] 🏁  Job succeeded
time=\"2026-09-29T07:30:09Z\" level=error msg=\"failed to remove \\\"act-ci-rust\\\"\"
\x1b[31mact is busy here: bana ci quick (dsper)\x1b[0m
Error: Job 'rust' failed
Error: copy /Users/l/.cache/act/tjrb-xyz-bana-actions-plan@1a2b3c4/x: file exists
";
        let r = fold_text(log);
        assert_eq!(r.build.network.as_deref(), Some("host"));
        let errors: Vec<(Owner, &str)> = r
            .errors
            .iter()
            .map(|e| (e.owner, e.text.as_str()))
            .collect();
        assert_eq!(
            errors,
            [
                (Owner::Act, "failed to remove \"act-ci-rust\""),
                (Owner::Bana, "act is busy here: bana ci quick (dsper)"),
                (
                    Owner::Bana,
                    "copy /Users/l/.cache/act/tjrb-xyz-bana-actions-plan@1a2b3c4/x: file exists"
                ),
            ],
            "act's last word on a job is left to the job"
        );
        assert_eq!(r.build.result, "error", "every job passed; act did not");
        assert!(
            step(&r, "rust", "t").tests.is_empty(),
            "bana's words are no step's"
        );
        assert_eq!(r.jobs.len(), 1);

        // In a paste, a bare `Error: …` in a step's output is the step's.
        let r = fold_text("test a ... FAILED\nError: the step printed this\n[ci/rust]   ❌  Failure - Main t [1ms]\n[ci/rust] 🏁  Job failed\nError: Job 'rust' failed\n");
        assert!(r.errors.is_empty());
        assert_eq!(
            step(&r, "rust", "t").tail,
            ["test a ... FAILED", "Error: the step printed this"]
        );

        // The same in act.jsonl, where the daemon wraps what is not act's JSON.
        let r = fold_json(
            r#"{"bana":"stderr","msg":"\u001b[31mDocker is not running: start OrbStack\u001b[0m"}
{"level":"error","msg":"no container","time":"2026-09-29T07:30:01Z"}
{"bana":"stderr","msg":"\u001b[1mact: quick from ci.yml, Linux jobs in img (linux/arm64, network bridge)\u001b[0m"}
{"bana":"stderr","msg":"Error: Job 'x' failed"}"#,
        );
        let errors: Vec<(Owner, &str)> = r
            .errors
            .iter()
            .map(|e| (e.owner, e.text.as_str()))
            .collect();
        assert_eq!(
            errors,
            [
                (Owner::Bana, "Docker is not running: start OrbStack"),
                (Owner::Act, "no container")
            ]
        );
        assert_eq!(r.build.network.as_deref(), Some("bridge"));
        assert_eq!(r.build.result, "error");
        assert_eq!(fold_text("").build.result, "unknown");
    }

    #[test]
    fn a_matrix_and_a_composite_in_text() {
        let text = "[ci/package-2] ⭐ Run Set up job
[ci/package-2]   ✅  Success - Set up job
[ci/package-2] 🧪  Matrix: map[target:linux-x64]
[ci/package-2] ⭐ Run Main ./tools/bana/actions/plan
[ci/package-2] ⭐ Run Main changed
[ci/package-2]   | changed files
[ci/package-2]   ❌  Failure - Main changed [2ms]
[ci/package-2]   ❌  Failure - Main ./tools/bana/actions/plan [7ms]
[ci/package-2] 🏁  Job failed
";
        let r = fold_text(text);
        let j = &r.jobs[0];
        assert_eq!(
            (j.key.as_str(), j.id.as_str()),
            ("package (linux-x64)", "package")
        );
        assert_eq!(j.matrix["target"], "linux-x64");
        let names: Vec<&str> = j.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["Set up job", "./tools/bana/actions/plan"],
            "a composite's inner steps are its own"
        );
        let plan = &j.steps[1];
        assert_eq!((plan.owner, plan.ms), (Owner::Bana, Some(7)));
        assert_eq!(plan.tail, ["changed files"]);
    }

    #[test]
    fn what_a_line_says() {
        assert_eq!(
            clean("test result: \x1b[31mFAILED\x1b[m\x0f. 1 passed"),
            "test result: FAILED. 1 passed"
        );
        assert_eq!(
            clean("\x1b]8;;file:///x\x1b\\src/lib.rs\x1b]8;;\x07:1"),
            "src/lib.rs:1",
            "cargo's hyperlinks"
        );
        assert_eq!(clean("10%\r20%\r\n"), "20%");
        assert_eq!(clean("\x1b(B\x1b[ma\tb\x07"), "a\tb");
        for (d, ms) in [
            ("4m58.608749125s", Some(298_608)),
            ("161.528625ms", Some(161)),
            ("12.924µs", Some(0)),
            ("2.3s", Some(2300)),
            ("1h2m3s", Some(3_723_000)),
            ("0s", Some(0)),
            ("", None),
            ("5x", None),
            ("1.5", None),
            (".s", None),
            ("99999999999999999999999999999999999h", None),
        ] {
            assert_eq!(go_ms(d), ms, "{d}");
        }
        assert_eq!(strip_duration("test [unit]"), ("test [unit]", None));
        assert_eq!(
            strip_duration("cargo test [1.5s]"),
            ("cargo test", Some(1500))
        );
        assert_eq!(
            stage_and_name("Post x"),
            ("Post".to_string(), "Post x".to_string())
        );
        assert_eq!(
            stage_and_name("Set up job"),
            (String::new(), "Set up job".to_string())
        );
        assert_eq!(
            stage_and_name("Main Post results"),
            ("Main".to_string(), "Post results".to_string())
        );
        assert_eq!(
            job_prefix("[ci/rust            ]   | x"),
            Some(("ci/rust", "   | x"))
        );
        assert_eq!(job_prefix("[1/3] Building CXX object"), None, "ninja's");
        assert_eq!(job_prefix("[INFO] x"), None);
        assert_eq!(job_prefix("[ci/x]y"), None);
        for (name, owner) in [
            (format!("Post {KEEP_BUILDS}"), Owner::Bana),
            ("./tools/bana/actions/plan".into(), Owner::Bana),
            ("Pre ./tools/bana/actions/plan".into(), Owner::Bana),
            ("./actions/plan".into(), Owner::Project),
            ("tjrb-xyz/banana/actions/x".into(), Owner::Project),
            ("bana/actions".into(), Owner::Project),
        ] {
            assert_eq!(step_owner(&name), owner, "{name}");
        }
        assert_eq!(
            logrus(r#"time="2026-09-29T07:30:01Z" level=error msg="failed to \"x\"""#),
            Some((true, "failed to \"x\"".into()))
        );
        assert_eq!(
            logrus("ERRO[0003] no Docker"),
            Some((true, "no Docker".into()))
        );
        assert_eq!(
            logrus("INFO[0000] Using docker host"),
            Some((false, "Using docker host".into()))
        );
        assert_eq!(logrus("WARN[x] y"), None);
        assert_eq!(logrus("FOO[1] x"), None);
        assert_eq!(network("act: quick from ci.yml"), None);
        let a = annotation("  ❗  ::error file=a%2Cb.rs,line=3,col=1::two%0Alines").unwrap();
        assert_eq!(
            (
                a.level.as_str(),
                a.file.as_deref(),
                a.line,
                a.message.as_str()
            ),
            ("error", Some("a,b.rs"), Some(3), "two\nlines")
        );
        assert_eq!(annotation("::errors::x"), None);
        assert_eq!(annotation("::group::x"), None);
        assert_eq!(annotation("::notice::x").unwrap().level, "notice");
        assert_eq!(
            nextest_summary("Summary [   0.020s] 8/10 tests run: 5 passed (1 slow, 1 flaky, 1 leaky), 2 failed, 1 exec failed, 1 timed out, 2 skipped"),
            Some(count("nextest", 5, 4, 2, true))
        );
        assert_eq!(
            nextest_summary("Summary [   0.001s] 1 test run: 1 passed, 0 skipped"),
            Some(count("nextest", 1, 0, 0, false))
        );
        assert_eq!(
            nextest_case("TRY 2 PASS [   0.010s] (3/5) demo::facts fact_two"),
            Some(("passed", "demo::facts", "fact_two"))
        );
        assert_eq!(
            nextest_case("SIGSEGV [   0.100s] demo crash"),
            Some(("failed", "demo", "crash"))
        );
        assert_eq!(nextest_case("SLOW [> 60.000s] demo slow"), None);
        assert_eq!(
            libtest_case("tests::slow ... ignored, needs hardware"),
            Some(("tests::slow", "skipped"))
        );
        assert_eq!(libtest_case("x ... bench:   1 ns/iter"), None);
    }

    #[test]
    fn results_jsonl_reads_back() {
        for r in [fold_text(PASTE), fold_json(RUN1), fold_text(CARGO)] {
            let jsonl = r.to_jsonl();
            for line in jsonl.lines() {
                let v: Value = serde_json::from_str(line).unwrap();
                assert!(v["kind"].is_string(), "{line}");
            }
            let first: Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
            assert_eq!(
                (first["kind"].as_str(), first["schema"].as_u64()),
                (Some("build"), Some(1))
            );
            assert_eq!(Results::from_jsonl(&jsonl), r);
        }
        let r = fold_text(PASTE);
        let lines: Vec<Value> = r
            .to_jsonl()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let kinds = |k: &str| lines.iter().filter(|v| v["kind"] == k).count();
        assert_eq!(
            [
                kinds("job"),
                kinds("step"),
                kinds("tests"),
                kinds("test"),
                kinds("rerun"),
                kinds("tail"),
                kinds("error")
            ],
            [1, 4, 1, 7, 1, 1, 1]
        );
        let test = lines.iter().find(|v| v["kind"] == "test").unwrap();
        assert_eq!(test["at"], "crates/dsper-engine/tests/facts.rs:457:18");
        assert_eq!(test["key"], "rust");
        assert_eq!(test["step"], "cargo test --workspace");
        let error = lines.last().unwrap();
        assert_eq!(
            (error["kind"].as_str(), error["owner"].as_str()),
            (Some("error"), Some("bana"))
        );
        assert_eq!(
            Results::from_jsonl("not json\n{\"kind\":\"what\"}\n"),
            Results::default()
        );
    }
}

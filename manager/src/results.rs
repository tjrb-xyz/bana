//! What a build did, step by step, for the fix brief and the CI report: act's
//! `--json` lines (the daemon's act.jsonl) or act's plain text (a hand run's
//! ci/last.log, a pasted log) folded into [`Results`], and results.jsonl out
//! ([`Results::to_jsonl`]), with the artifacts the daemon collected
//! ([`crate::artifacts`]).
//!
//! Jobs and steps are [`actlog::Build`]'s, as for the statuses: a line of plain
//! text is read into an [`actlog::JobLine`] first. What each step printed is
//! read here: cargo's and nextest's test lines, the panics of failing tests,
//! cargo's rerun target, other tools' summary lines (vitest, jest, node, pytest,
//! unittest, go), annotations (`::error file=…::…`), step summaries
//! (GITHUB_STEP_SUMMARY, which act logs as `⚙  Summary - …`), and its last lines.
//! A job that never ran is `skipped`, `unsupported` (not run here: no platform
//! for it, or bana says why, as `elsewhere`), or `not_planned`: the jobs
//! `act -l` listed ([`fold_json_listed`]) that the plan job set false
//! (`::set-output:: web=false` in a job `plan`, or bana plan's
//! `json={"tier":"quick","web":false}`).
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
//! other errors outside a job but `workflow is not valid`; the project
//! otherwise.

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
    /// What the jobs uploaded, as the daemon collected it ([`crate::artifacts`]).
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Job {
    /// `rust`, `package (linux-arm64)`.
    pub key: String,
    /// The workflow's job id (in plain text, act's name for the job).
    pub id: String,
    pub matrix: BTreeMap<String, String>,
    /// `success`, `failure`, `skipped` (by `if:`, or a job it needs failed),
    /// `unsupported`, `not_planned`, `cancelled`, or `unknown`.
    pub result: String,
    pub ms: Option<u64>,
    pub steps: Vec<Step>,
    /// Why an unsupported job did not run here, when bana says
    /// ([`actlog::BANA_NOT_RUN`]) or act's skip does ([`actlog::ELSEWHERE_SYSTEMD`]).
    pub elsewhere: Option<String>,
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
    /// Test counts, one per tool that printed them.
    pub tests: Vec<Count>,
    /// Each test's own line, in order.
    pub cases: Vec<Case>,
    /// What cargo says to pass to rerun what failed (`-p example-engine --test facts`).
    pub reruns: Vec<String>,
    pub annotations: Vec<Annotation>,
    /// What it wrote to GITHUB_STEP_SUMMARY, as Markdown.
    pub summaries: Vec<String>,
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
    /// `cargo`, `nextest`, `vitest`, `jest`, `node`, `pytest`, `unittest`, or
    /// `go`, whose counts are packages (`ok`, `FAIL`), not tests.
    pub tool: String,
    pub passed: u64,
    /// Errors, cancelled tests and timeouts too.
    pub failed: u64,
    /// Ignored, todo and expected failures too.
    pub skipped: u64,
    /// Not every test ran: cargo stopped early, nextest ran N/M, a tool said
    /// it stopped, or the step never finished.
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
    /// Where it panicked: `crates/example-engine/tests/facts.rs:457:18`.
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
    #[serde(skip_serializing_if = "String::is_empty")]
    pub level: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<u64>,
    /// vitest's is the test: `demo.test.js > accepted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A notice results.jsonl says leaves its step out (`left_out`).
    #[serde(skip)]
    pub left_out: bool,
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

/// An artifact a job uploaded (upload-artifact@v4: one zip).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Artifact {
    pub name: String,
    /// The job and step that uploaded it, when the log says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The zip's size and sha256 (upload-artifact's artifact-digest).
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The files it gave the build's dist/, by their names there.
    pub files: Vec<String>,
    /// Why it was not collected, or a note (an upload-artifact@v3 layout).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
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
    fold_json_listed(text, &[])
}

/// As [`fold_json`], with the jobs `act -l` listed (the daemon's jobs.txt): a
/// listed job that printed nothing was skipped, or not planned.
pub fn fold_json_listed(text: &str, list: &[(u32, String)]) -> Results {
    let mut f = Folder::default();
    if !list.is_empty() {
        f.build = actlog::Build::new(list, 0);
    }
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
    last_cr(&strip(s)).to_string()
}

/// What a terminal shows of a line with carriage returns: its last part.
fn last_cr(s: &str) -> &str {
    s.rsplit('\r').find(|p| !p.is_empty()).unwrap_or("")
}

/// A line with ANSI escapes and control characters gone, but for its
/// carriage returns (and tabs): act's prefix comes before a step's `\r`.
fn strip(s: &str) -> String {
    let s = s.trim_end_matches(['\r', '\n']);
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
            '\t' | '\r' => out.push(c),
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
    /// What the plan job said of each job: `web` false is not planned.
    plan: BTreeMap<String, bool>,
    /// bana plan's tier.
    tier: Option<String>,
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
    /// Each job's last step that ended, which a step summary after it is from.
    ended: BTreeMap<String, Open>,
    /// The step whose summary goes on in the bare lines after act's line.
    summary: Option<StepKey>,
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
        if !l.output {
            if let Some((name, value)) = set_output(&l.msg) {
                self.planned(&l.id, name, value);
            }
        }
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
        } else if let Some(md) = summary(&l.msg) {
            out.summaries.push(md);
        } else if let Some(a) = annotation(&l.msg) {
            out.keep(clean(l.msg.trim()));
            out.annotations.push(a);
        }
    }

    /// A plan job's output: `web=false` from a job `plan`, or bana plan's
    /// `json={"tier":"quick","web":false,…}` from any job.
    fn planned(&mut self, job: &str, name: &str, value: &str) {
        if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(value) {
            if let Some(tier) = o.get("tier").and_then(Value::as_str) {
                self.tier = Some(tier.to_string());
                for (k, v) in &o {
                    if let Some(b) = v.as_bool() {
                        self.plan.insert(k.clone(), b);
                    }
                }
            }
        } else if job == "plan" && matches!(value, "true" | "false") {
            self.plan.insert(name.to_string(), value == "true");
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
        } else if text.starts_with("workflow is not valid") {
            // act could not read the project's workflow.
            Owner::Project
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
            self.summary = None;
            return self.json(raw);
        }
        // To a terminal, act gives each job a colour, for its name and for the
        // bar before its output (`\x1b[33m|\x1b[0m …`), which names no job.
        let colour = raw
            .strip_prefix("\x1b[")
            .and_then(|r| r.split_once('m'))
            .map(|(c, _)| c);
        let red = raw.contains("\x1b[31m");
        // act's prefix and bar come first: a `\r` a step printed after them
        // hides only the rest of its own output, as on a terminal, and makes
        // no line of act's.
        let s = strip(raw);
        let s = s.trim_start_matches('\r');
        if let Some((name, rest)) = job_prefix(s) {
            if let Some(c) = colour {
                self.marked = true;
                self.colours.insert(c.to_string(), name.to_string());
            }
            return self.text_job(name, rest);
        }
        // A step summary's lines after act's first one are bare.
        if let (None, Some(k)) = (colour, &self.summary) {
            if let Some(md) = self.out.get_mut(k).and_then(|o| o.summaries.last_mut()) {
                md.push('\n');
                md.push_str(last_cr(s));
                return;
            }
        }
        if let Some(o) = s.strip_prefix('|') {
            let o = last_cr(o.strip_prefix(' ').unwrap_or(o));
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
        let s = last_cr(s);
        if let Some((error, msg)) = logrus(s) {
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
            return self.outside(s, error, red);
        }
        if !self.marked {
            self.orphan(s);
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
        self.summary = None;
        if let Some(o) = rest.trim_start().strip_prefix('|') {
            self.marked = true;
            return self.output(&key, last_cr(o.strip_prefix(' ').unwrap_or(o)));
        }
        let body = last_cr(rest).trim();
        if body.starts_with("[DEBUG]") {
            return;
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
                let o = Open {
                    stage: stage.clone(),
                    name: step.clone(),
                    id: id.clone(),
                };
                self.ended.insert(key.clone(), o);
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
        } else if summary_start(body).is_some() {
            // After its step's result, or an inner step's.
            match (
                self.open.get(&key).filter(|s| !s.is_empty()),
                self.ended.get(&key),
            ) {
                (Some(stack), _) => on_step(&mut l, stack, stack.len() > 1),
                (None, Some(o)) => {
                    (l.stage, l.step, l.step_id) =
                        (o.stage.clone(), Some(o.name.clone()), Some(o.id.clone()));
                }
                (None, None) => {}
            }
            self.summary = l
                .step_id
                .clone()
                .map(|id| (key.clone(), l.stage.clone(), id));
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
        if let Some(o) = self.ended.remove(old) {
            self.ended.insert(new.clone(), o);
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
            plan,
            tier,
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
        // In text, a job `x-2` that turned out no matrix entry keeps its name,
        // unless its siblings did (`x (arm64)`): then it is an entry that failed
        // before act printed its matrix, and its id stays `x`.
        let named: Vec<String> = build
            .jobs
            .iter()
            .filter(|j| !j.matrix.is_empty())
            .map(|j| j.id.clone())
            .collect();
        for j in build.jobs.iter_mut().filter(|j| j.matrix.is_empty()) {
            if !named.contains(&j.id) {
                j.id = j.key.clone();
            }
        }
        let cancelled = build.cancel_requested.is_some();
        let mut jobs: Vec<Job> = build
            .jobs
            .iter()
            .map(|j| Job {
                key: j.key.clone(),
                id: j.id.clone(),
                matrix: j.matrix.clone(),
                result: match job_result(j, cancelled) {
                    "skipped" if plan.get(&j.id) == Some(&false) => "not_planned",
                    r => r,
                }
                .into(),
                ms: match (j.started, j.ended) {
                    (Some(a), Some(b)) if a > 0 && b >= a => Some((b - a) as u64 * 1000),
                    _ => None,
                },
                elsewhere: j.elsewhere.clone(),
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
            let mut step = o.step(&actlog::Step::default(), true);
            let failed = step.failed_cases().next().is_some()
                || step.tests.iter().any(|c| c.failed > 0)
                || !step.reruns.is_empty();
            // What failed there failed in it: the brief and the prompt say what.
            if failed {
                step.result = Some("failure".into());
            }
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
                tier,
                started: times.0,
                ended: times.1,
                result: result.into(),
                ..BuildInfo::default()
            },
            jobs,
            errors,
            artifacts: Vec::new(),
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
            // A network's name (`host`, `bridge`, `container:ID`), nothing else.
            let name = n
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c));
            return (!n.is_empty() && name).then(|| n.to_string());
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
            Some(("col" | "column", v)) => a.col = v.trim().parse().ok(),
            Some(("title", v)) => a.title = Some(unescape(v)),
            _ => {}
        }
    }
    Some(a)
}

/// act's line for a step's summary, `⚙  Summary - ## node job\n| a | b |…`:
/// the Markdown.
fn summary(msg: &str) -> Option<String> {
    let mut lines = msg.trim_end().split('\n').map(clean);
    let first = lines.next()?;
    let first = summary_start(&first)?.to_string();
    Some(
        std::iter::once(first)
            .chain(lines)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// `⚙  Summary - ## node job`: its first line.
fn summary_start(body: &str) -> Option<&str> {
    let rest = body
        .trim_start()
        .strip_prefix('⚙')?
        .trim_start_matches(|c: char| c.is_whitespace() || c == '\u{fe0f}')
        .strip_prefix("Summary -")?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// act's line for what a step wrote to GITHUB_OUTPUT: `⚙  ::set-output:: web=false`.
fn set_output(msg: &str) -> Option<(&str, &str)> {
    let (name, value) = msg
        .trim()
        .trim_start_matches(|c: char| !c.is_ascii() || c.is_whitespace())
        .strip_prefix("::set-output::")?
        .split_once('=')?;
    Some((name.trim(), value.trim()))
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
    /// libtest said `running N tests` and no `test result:` yet: the first
    /// of that binary's cases.
    libtest_open: Option<usize>,
    /// A binary never said its `test result:` (it crashed, or the step was cut).
    cargo_cut: bool,
    /// cargo said `error: N target(s) failed`: it ran every binary.
    all_ran: bool,
    /// The test binary cargo runs now (`Running …`, `Doc-tests …`).
    binary: Option<String>,
    nextest: Option<Count>,
    /// nextest runs: its PASS/FAIL lines count until its Summary.
    in_nextest: bool,
    /// The first of this nextest run's cases.
    nextest_from: usize,
    /// The other tools' summed counts, in the order they first printed.
    counts: Vec<Count>,
    /// node's `# tests N` (TAP) or `ℹ tests N` (spec), until its last line,
    /// and that N.
    node: Option<(Count, u64)>,
    /// unittest's `Ran N tests`, until its `OK` or `FAILED (…)`.
    unittest: Option<u64>,
    /// unittest's `FAIL: NAME (…)` and `ERROR: NAME (…)` headers before it.
    unittest_heads: Vec<String>,
    /// pytest said it stopped (`-x`, `--maxfail`, an interrupt): its next
    /// count is incomplete; `true` when it stopped while collecting.
    pytest_stop: Option<bool>,
    /// jest ran some of its suites (`Test Suites: 1 failed, 1 of 4 total`).
    jest_stop: bool,
    /// jest's `Tests:` line in the summary block read now: `--bail` can
    /// print the block twice.
    jest_last: Option<String>,
    summaries: Vec<String>,
    cases: Vec<Case>,
    reruns: Vec<String>,
    annotations: Vec<Annotation>,
    /// A panic whose message lines come next.
    panic: Option<Panic>,
    /// Panics of tests not yet seen failing (`--nocapture` prints them first).
    panics: Vec<Panic>,
    /// The test libtest said it runs (`test NAME ... `), its result to come on
    /// a line of its own: one test at a time, with `--nocapture`.
    running: Option<String>,
    /// The failed test whose output libtest shows now (`---- NAME stdout ----`).
    section: Option<String>,
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
            // Only a process's first panic has the note after it: with
            // --nocapture, the next line libtest prints ends the others.
            let done = t.is_empty()
                || t.starts_with("note: ")
                || t.starts_with("stack backtrace:")
                || p.lines.len() == PANIC_LINES
                || self.said(line);
            if !done {
                p.lines.push(t.to_string());
                self.panic = Some(p);
                return;
            }
            self.panicked(p);
        }
        if self.tool_line(line) {
            return;
        }
        // libtest's lines at column 0 only: nextest indents them in a failing
        // test's output, and counts that test itself. With `--no-capture` it
        // passes them through as they are, one test per process: its own
        // lines count then, not libtest's.
        let t = line.trim_start();
        let libtest = !self.in_nextest;
        if !libtest && (line.starts_with("test ") || libtest_running(line)) {
            // nextest's PASS or FAIL for it follows.
        } else if libtest_running(line) {
            self.libtest_cut();
            self.libtest_open = Some(self.cases.len());
        } else if let Some(rest) = line.strip_prefix("test result: ") {
            (self.running, self.section, self.libtest_open) = (None, None, None);
            if let Some(c) = libtest_counts(rest) {
                self.cargo
                    .get_or_insert_with(|| Count::new("cargo"))
                    .add(&c);
            }
        } else if let Some(rest) = line.strip_prefix("test ") {
            self.running = None;
            match libtest_case(rest) {
                Some((name, result)) => self.case(name, result, self.binary.clone()),
                None => self.running = libtest_started(rest).map(String::from),
            }
        } else if let Some(result) = self.running.as_ref().and_then(|_| bare_result(line)) {
            if let Some(name) = self.running.take() {
                self.case(&name, result, self.binary.clone());
            }
        } else if let Some(target) = rerun_hint(line) {
            if cargo_target(target) && !self.reruns.iter().any(|r| r == target) {
                self.reruns.push(target.to_string());
            }
        } else if line.starts_with("error: ")
            && (line.contains(" target failed") || line.contains(" targets failed"))
        {
            self.all_ran = true;
        } else if let Some(binary) = running_binary(t) {
            self.libtest_cut();
            (self.binary, self.running, self.section) = (Some(binary.to_string()), None, None);
        } else if t.starts_with("Doc-tests ") {
            self.libtest_cut();
            (self.running, self.section) = (None, None);
            self.binary = Some(t.trim_end().to_string());
        } else if let Some(name) = section(t) {
            self.section = Some(name.to_string());
        } else if line.trim_end() == "failures:" {
            self.section = None;
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
            if !self.in_nextest {
                self.nextest_from = self.cases.len();
            }
            self.in_nextest = true;
        } else if let Some((result, binary, name, retry)) =
            self.in_nextest.then(|| nextest_case(t)).flatten()
        {
            // A retry's result stands for its test: the last attempt's.
            if retry {
                let from = self.nextest_from;
                if let Some(i) = self.cases[from..]
                    .iter()
                    .rposition(|c| c.name == name && c.binary.as_deref() == Some(binary))
                {
                    self.cases.remove(from + i);
                }
            }
            self.case(name, result, Some(binary.to_string()));
        } else if let Some(a) = annotation(line) {
            self.annotations.push(a);
        }
    }

    /// Another tool's summary line: vitest's, jest's, node's, pytest's,
    /// unittest's, or go's for a package.
    fn tool_line(&mut self, line: &str) -> bool {
        if let Some(ran) = self.unittest.take() {
            if let Some(c) = unittest_result(line, ran, &self.unittest_heads) {
                self.unittest_heads.clear();
                self.count(c);
                return true;
            }
            if line.trim().is_empty() {
                self.unittest = Some(ran);
            }
        }
        if let Some((word, n)) = node_item(line) {
            if word == "tests" {
                self.node_done();
                self.node = Some((Count::new("node"), n.unwrap_or(0)));
            }
            let Some((c, _)) = self.node.as_mut() else {
                return false;
            };
            match word {
                "pass" => c.passed += n.unwrap_or(0),
                "fail" | "cancelled" => c.failed += n.unwrap_or(0),
                "skipped" | "todo" => c.skipped += n.unwrap_or(0),
                "duration_ms" => self.node_done(),
                _ => {}
            }
            return true;
        }
        // Another line inside node's summary (`cmd & node --test`) leaves it
        // open: its last line, a blank one, or the step's end closes it.
        if line.trim().is_empty() {
            self.node_done();
        }
        if let Some(head) = unittest_head(line) {
            self.unittest_heads.push(head.to_string());
        }
        if let Some(n) = ran_tests(line) {
            self.unittest = Some(n);
            return true;
        }
        let t = line.trim();
        if pytest_stopped(t) {
            self.pytest_stop = Some(t.contains(" during collection"));
        }
        let jest_block = [
            "Test Suites:",
            "Tests:",
            "Snapshots:",
            "Time:",
            "Ran all test suites",
        ]
        .iter()
        .any(|w| line.starts_with(w));
        if !jest_block && !t.is_empty() {
            self.jest_last = None;
        }
        if let Some(rest) = line.strip_prefix("Test Suites:") {
            // `1 failed, 1 of 4 total`: --bail stopped it.
            self.jest_stop |= rest.contains(" of ");
        }
        if let Some(mut c) = jest_summary(line) {
            // `--bail` can print its summary block twice over.
            if self.jest_last.as_deref() == Some(line.trim_end()) {
                return true;
            }
            self.jest_last = Some(line.trim_end().to_string());
            c.incomplete = std::mem::take(&mut self.jest_stop);
            self.count(c);
            return true;
        }
        if let Some(mut c) = pytest_summary(line) {
            match self.pytest_stop.take() {
                // Interrupted while collecting: no test ran, and its errors
                // are modules that did not load, not tests.
                Some(true) => {
                    c.failed = c.failed.saturating_sub(pytest_errors(line));
                    c.incomplete = true;
                }
                Some(false) => c.incomplete = true,
                None => {}
            }
            self.count(c);
            return true;
        }
        match vitest_summary(line).or_else(|| go_package(line)) {
            Some(c) => {
                self.count(c);
                true
            }
            None => false,
        }
    }

    /// node's summary ends: it counts, and it is incomplete when its lines do
    /// not add up to its `tests` (some of them missing).
    fn node_done(&mut self) {
        if let Some((mut c, tests)) = self.node.take() {
            c.incomplete |= c.passed + c.failed + c.skipped != tests;
            self.count(c);
        }
    }

    fn count(&mut self, c: Count) {
        match self.counts.iter_mut().find(|x| x.tool == c.tool) {
            Some(x) => x.add(&c),
            None => self.counts.push(c),
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
    /// Another thread's (a doc test's `main`, a thread the test started) goes
    /// with the test whose output libtest shows, or the one that runs now.
    fn panicked(&mut self, mut p: Panic) {
        let open = |c: &Case, name: &str| c.name == name && c.result == "failed" && c.at.is_none();
        let i = self
            .cases
            .iter()
            .rposition(|c| open(c, &p.thread))
            .or_else(|| {
                let s = self.section.as_deref()?;
                self.cases.iter().rposition(|c| open(c, s))
            });
        match i {
            Some(i) => {
                let c = &mut self.cases[i];
                (c.at, c.message) = (Some(p.at.clone()), p.message());
            }
            None => {
                if let Some(name) = &self.running {
                    p.thread.clone_from(name);
                }
                self.panics.push(p)
            }
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

    /// A line libtest, cargo or nextest prints, which no panic's message has.
    fn said(&self, line: &str) -> bool {
        let t = line.trim_start();
        line.starts_with("test result: ")
            || line
                .strip_prefix("test ")
                .is_some_and(|r| libtest_case(r).is_some() || libtest_started(r).is_some())
            || (self.running.is_some() && bare_result(line).is_some())
            || rerun_hint(line).is_some()
            || line.trim_end() == "failures:"
            || section(t).is_some()
            || t.starts_with("thread '")
            || running_binary(t).is_some()
            || t.starts_with("Doc-tests ")
            || nextest_summary(t).is_some()
            || (self.in_nextest && nextest_case(t).is_some())
    }

    /// A test binary ended without its `test result:`: what it said of its
    /// tests counts, as a count cut short.
    fn libtest_cut(&mut self) {
        let Some(from) = self.libtest_open.take() else {
            return;
        };
        let c = tally_cases("cargo", &self.cases[from..]);
        self.cargo
            .get_or_insert_with(|| Count::new("cargo"))
            .add(&c);
        self.cargo_cut = true;
    }

    /// The step's output has ended.
    fn close(&mut self) {
        if let Some(p) = self.panic.take() {
            self.panicked(p);
        }
        self.node_done();
        self.libtest_cut();
        // nextest never said its Summary: its tests so far, cut short.
        if self.in_nextest {
            let mut c = tally_cases("nextest", &self.cases[self.nextest_from..]);
            c.incomplete = true;
            self.nextest
                .get_or_insert_with(|| Count::new("nextest"))
                .add(&c);
            self.in_nextest = false;
        }
        self.running = None;
    }

    /// Something worth keeping outside any step.
    fn found(&self) -> bool {
        self.cargo.is_some()
            || self.nextest.is_some()
            || !self.cases.is_empty()
            || !self.reruns.is_empty()
            || !self.annotations.is_empty()
            || !self.panics.is_empty()
            || !self.counts.is_empty()
            || !self.summaries.is_empty()
    }

    /// The step `s` as its output says; `loose` for output outside any step.
    fn step(self, s: &actlog::Step, loose: bool) -> Step {
        let mut tests = Vec::new();
        // Without `--no-fail-fast`, cargo stops at the first binary that
        // fails: it says what to rerun, and never that N targets failed.
        if self.cargo.is_some() || !self.reruns.is_empty() {
            let mut c = self.cargo.unwrap_or_else(|| Count::new("cargo"));
            c.incomplete = (!self.reruns.is_empty() && !self.all_ran) || self.cargo_cut;
            tests.push(c);
        }
        tests.extend(self.nextest);
        tests.extend(self.counts);
        // A step that never ended (cancelled, timed out, its log cut) may
        // not have run every test it counted.
        let ended = matches!(s.result.as_deref(), Some("success" | "failure"));
        if !ended && !loose {
            tests.iter_mut().for_each(|c| c.incomplete = true);
        }
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
            summaries: self
                .summaries
                .into_iter()
                .map(|m| m.trim_end().to_string())
                .collect(),
            tail: if failed || loose {
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

/// `running 2 tests`: a test binary starts.
fn libtest_running(line: &str) -> bool {
    line.strip_prefix("running ").is_some_and(|r| {
        let (n, word) = r.trim_end().split_once(' ').unwrap_or(("", ""));
        n.parse::<u64>().is_ok() && (word == "tests" || word == "test")
    })
}

/// A count of `cases`, one per test.
fn tally_cases(tool: &str, cases: &[Case]) -> Count {
    let mut c = Count::new(tool);
    for case in cases {
        match case.result.as_str() {
            "passed" => c.passed += 1,
            "failed" => c.failed += 1,
            _ => c.skipped += 1,
        }
    }
    c
}

/// `NAME ... ` with no result yet, or the test's own output after it: one test
/// at a time, with `--nocapture`, libtest prints the result on a line of its
/// own when the test ends.
fn libtest_started(rest: &str) -> Option<&str> {
    let name = match rest.split_once(" ... ") {
        Some((name, _)) => name,
        None => rest.trim_end().strip_suffix(" ...")?,
    };
    (!name.is_empty()).then_some(name)
}

/// That line: `ok` or `FAILED`.
fn bare_result(line: &str) -> Option<&'static str> {
    match line.trim_end() {
        "ok" => Some("passed"),
        "FAILED" => Some("failed"),
        _ => None,
    }
}

/// What cargo says to pass to rerun what failed: ``error: test failed, to
/// rerun pass `-p x --test y` `` (`doctest failed` for doc tests).
fn rerun_hint(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("error: test failed, to rerun pass `")
        .or_else(|| line.strip_prefix("error: doctest failed, to rerun pass `"))?;
    rest.split_once('`').map(|(target, _)| target)
}

/// A target as cargo names one there: `[-p PACKAGE] --lib|--doc`, or with
/// `--bin|--test|--example|--bench NAME`. Anything else is not cargo's, and no
/// command to hand on.
fn cargo_target(t: &str) -> bool {
    let name = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && !s.starts_with('-')
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    };
    let mut words = t.split(' ');
    let mut kind = words.next();
    if kind == Some("-p") {
        if !words.next().is_some_and(name) {
            return false;
        }
        kind = words.next();
    }
    match (kind, words.next(), words.next()) {
        (Some("--lib" | "--doc"), None, None) => true,
        (Some("--bin" | "--test" | "--example" | "--bench"), Some(n), None) => name(n),
        _ => false,
    }
}

/// `Running unittests src/lib.rs (target/debug/deps/demo-7f33b6e5)`: the binary.
fn running_binary(t: &str) -> Option<&str> {
    let (binary, _) = t
        .strip_prefix("Running ")?
        .trim_end()
        .strip_suffix(')')?
        .rsplit_once(" (")?;
    Some(binary)
}

/// `---- NAME stdout ----`: a failed test's output follows.
fn section(t: &str) -> Option<&str> {
    t.trim_end()
        .strip_prefix("---- ")?
        .strip_suffix(" stdout ----")
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

/// `PASS [   0.008s] (1/5) demo::facts fact_one`: the result, the binary, the
/// test, and whether it is a retry (`TRY 2 PASS …`), which stands for the
/// attempts before it.
fn nextest_case(t: &str) -> Option<(&'static str, &str, &str, bool)> {
    let (t, retry) = match t.strip_prefix("TRY ") {
        Some(r) => {
            let (n, rest) = r.split_once(' ')?;
            (rest.trim_start(), n.parse::<u32>().ok()? > 1)
        }
        None => (t, false),
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
    Some((result, binary, name.trim(), retry))
}

/// `N word` items, as the tools list them: each item's number and its words.
fn items(list: &str, sep: char) -> Option<Vec<(u64, &str)>> {
    list.split(sep)
        .map(|item| {
            let (n, word) = item.trim().split_once(' ')?;
            Some((n.parse().ok()?, word.trim()))
        })
        .collect()
}

/// A tool's count from its items: which words pass, fail and skip. A word
/// none of them has makes it no count of that tool's.
fn tally(tool: &str, items: &[(u64, &str)], words: [&[&str]; 3], other: &[&str]) -> Option<Count> {
    let mut c = Count::new(tool);
    let mut any = false;
    for &(n, word) in items {
        match words.iter().position(|w| w.contains(&word)) {
            Some(0) => c.passed += n,
            Some(1) => c.failed += n,
            Some(_) => c.skipped += n,
            None if other.contains(&word) => continue,
            None => return None,
        }
        any = true;
    }
    any.then_some(c)
}

/// vitest's, right-aligned: `      Tests  1 failed | 1 passed | 1 skipped | 1 todo (4)`.
/// Fewer than the total ran when it stopped early (`--bail`).
fn vitest_summary(line: &str) -> Option<Count> {
    let list = line.trim_start().strip_prefix("Tests  ")?;
    let (list, total) = list.trim_end().strip_suffix(')')?.rsplit_once(" (")?;
    let words: [&[&str]; 3] = [&["passed"], &["failed"], &["skipped", "todo"]];
    let mut c = tally("vitest", &items(list, '|')?, words, &[])?;
    let total: u64 = total.parse().ok()?;
    c.incomplete = c.passed + c.failed + c.skipped < total;
    Some(c)
}

/// jest's: `Tests:       1 failed, 1 skipped, 1 todo, 1 passed, 4 total`.
fn jest_summary(line: &str) -> Option<Count> {
    let list = line.strip_prefix("Tests:")?;
    let words: [&[&str]; 3] = [&["passed"], &["failed"], &["skipped", "todo"]];
    tally("jest", &items(list, ',')?, words, &["total"])
}

/// pytest's last line, `1 failed, 1 passed, 1 skipped, 1 xfailed, 1 error in
/// 0.02s`, framed in `=` but under `-q`.
fn pytest_summary(line: &str) -> Option<Count> {
    let t = line.trim().trim_matches('=').trim();
    let (list, time) = t.rsplit_once(" in ")?;
    let secs = time.split(' ').next()?.strip_suffix('s')?;
    secs.parse::<f64>().ok()?;
    let words: [&[&str]; 3] = [
        &["passed", "xpassed"],
        &["failed", "error", "errors"],
        &["skipped", "xfailed"],
    ];
    let other = ["deselected", "warning", "warnings", "rerun"];
    tally("pytest", &items(list, ',')?, words, &other)
}

/// pytest's `!!!!! stopping after 1 failures !!!!!` (`-x`, `--maxfail`), or
/// `!!!!! Interrupted: 1 error during collection !!!!!`: not every test ran.
fn pytest_stopped(t: &str) -> bool {
    let Some(inner) = t.strip_prefix("!!").and_then(|r| r.strip_suffix("!!")) else {
        return false;
    };
    let inner = inner.trim_matches('!').trim();
    inner.starts_with("stopping after ")
        || inner.starts_with("Interrupted: ")
        || inner.starts_with("KeyboardInterrupt")
}

/// The errors on pytest's last line (`1 error in 0.10s`).
fn pytest_errors(line: &str) -> u64 {
    let t = line.trim().trim_matches('=').trim();
    let list = t.rsplit_once(" in ").map_or(t, |(l, _)| l);
    items(list, ',')
        .unwrap_or_default()
        .iter()
        .filter(|(_, w)| matches!(*w, "error" | "errors"))
        .map(|(n, _)| n)
        .sum()
}

/// unittest's `FAIL: test_x (mod.T.test_x)` or `ERROR: …` over a failure,
/// its subtest's `(i=2)` left off: the test (or `setUpClass (mod.T)`).
fn unittest_head(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("FAIL: ")
        .or_else(|| line.strip_prefix("ERROR: "))?
        .trim_end();
    let end = rest.find(')')?;
    rest[..end].contains(" (").then(|| &rest[..=end])
}

/// unittest's `Ran 5 tests in 0.003s`: how many ran.
fn ran_tests(line: &str) -> Option<u64> {
    let (n, rest) = line.strip_prefix("Ran ")?.split_once(' ')?;
    (rest.starts_with("test in ") || rest.starts_with("tests in "))
        .then(|| n.parse().ok())
        .flatten()
}

/// Its next line: `OK`, `OK (skipped=1)`, or `FAILED (failures=1, errors=1,
/// skipped=1, expected failures=1)`. unittest counts each failing subtest in
/// `failures`, but the test once in `Ran`; a class's or module's setUp error
/// in `errors`, and not its tests, which did not run. So the failures are the
/// tests its headers (`heads`) name, when there is one per failure.
fn unittest_result(line: &str, ran: u64, heads: &[String]) -> Option<Count> {
    let line = line.trim_end();
    let rest = line
        .strip_prefix("OK")
        .or_else(|| line.strip_prefix("FAILED"))?;
    let mut c = Count::new("unittest");
    let list = match rest.trim() {
        "" => "",
        r => r.strip_prefix('(')?.strip_suffix(')')?,
    };
    let (mut failures, mut unexpected) = (0, 0);
    for item in list.split(',').filter(|i| !i.trim().is_empty()) {
        let (word, n) = item.trim().split_once('=')?;
        let n: u64 = n.parse().ok()?;
        match word {
            "failures" | "errors" => failures += n,
            "unexpected successes" => unexpected += n,
            "skipped" | "expected failures" => c.skipped += n,
            _ => return None,
        }
    }
    c.failed = failures + unexpected;
    if heads.len() as u64 == failures && failures > 0 {
        let fixture = |h: &String| {
            [
                "setUpClass ",
                "tearDownClass ",
                "setUpModule ",
                "tearDownModule ",
            ]
            .iter()
            .any(|f| h.starts_with(f))
        };
        let mut tests: Vec<&String> = heads.iter().filter(|h| !fixture(h)).collect();
        tests.sort();
        tests.dedup();
        c.failed = tests.len() as u64 + unexpected;
        // A module that did not load (`unittest.loader._FailedTest`), or a
        // setUp that failed: their tests never ran.
        c.incomplete =
            heads.iter().any(fixture) || heads.iter().any(|h| h.contains("._FailedTest"));
    }
    if c.failed + c.skipped > ran {
        c.failed = ran.saturating_sub(c.skipped);
        c.incomplete = true;
    }
    c.passed = ran.saturating_sub(c.failed + c.skipped);
    Some(c)
}

/// node's closing lines, TAP's `# pass 1` or spec's `ℹ pass 1`: the word, and
/// its number (none for `duration_ms 92.4`).
fn node_item(line: &str) -> Option<(&str, Option<u64>)> {
    let rest = line
        .strip_prefix("# ")
        .or_else(|| line.strip_prefix("ℹ "))?;
    let (word, n) = rest.trim().split_once(' ')?;
    let known = [
        "tests",
        "suites",
        "pass",
        "fail",
        "cancelled",
        "skipped",
        "todo",
        "duration_ms",
    ];
    known.contains(&word).then(|| (word, n.trim().parse().ok()))
}

/// go test's line for a package: `ok  \tpkg\t0.009s` or `FAIL\tpkg\t0.006s`
/// (`[build failed]`); it counts packages.
fn go_package(line: &str) -> Option<Count> {
    let (status, rest) = line.split_once('\t')?;
    let pkg = rest.split(['\t', ' ']).next()?;
    if pkg.is_empty() {
        return None;
    }
    let mut c = Count::new("go");
    match status.trim_end() {
        "ok" => c.passed = 1,
        "FAIL" => c.failed = 1,
        _ => return None,
    }
    Some(c)
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        elsewhere: Option<String>,
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
        #[serde(flatten)]
        at: Annotation,
    },
    /// What a step wrote to GITHUB_STEP_SUMMARY.
    Summary {
        key: String,
        step: String,
        markdown: String,
    },
    Tail {
        key: String,
        step: String,
        lines: Vec<String>,
    },
    Artifact(Artifact),
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
    /// printed, then the artifacts, then the errors outside the jobs; one JSON
    /// object per line.
    pub fn to_jsonl(&self) -> String {
        self.to_jsonl_with(|_| false)
    }

    /// [`Self::to_jsonl`], with each notice's `left_out` from `left_out` (its
    /// text: bana.conf's report.left_out), or as results.jsonl said it.
    pub fn to_jsonl_with(&self, left_out: impl Fn(&str) -> bool) -> String {
        let mut lines = vec![Line::Build(self.build.clone())];
        for j in &self.jobs {
            lines.push(Line::Job {
                key: j.key.clone(),
                job: j.id.clone(),
                matrix: j.matrix.clone(),
                result: j.result.clone(),
                ms: j.ms,
                elsewhere: j.elsewhere.clone(),
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
                        left_out: a.left_out || left_out(&a.message),
                        at: Annotation {
                            level: String::new(),
                            message: String::new(),
                            ..a.clone()
                        },
                    },
                    _ => Line::Annotation(keyed(key, step, a)),
                }));
                lines.extend(s.summaries.iter().map(|m| Line::Summary {
                    key: key.into(),
                    step: step.into(),
                    markdown: m.clone(),
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
        lines.extend(self.artifacts.iter().cloned().map(Line::Artifact));
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
                    elsewhere,
                } => r.jobs.push(Job {
                    key,
                    id: job,
                    matrix,
                    result,
                    ms,
                    steps: Vec::new(),
                    elsewhere,
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
                    key,
                    step,
                    text,
                    left_out,
                    at,
                } => {
                    let a = Annotation {
                        level: "notice".into(),
                        message: text,
                        left_out,
                        ..at
                    };
                    r.step_mut(&key, &step).annotations.push(a);
                }
                Line::Summary {
                    key,
                    step,
                    markdown,
                } => r.step_mut(&key, &step).summaries.push(markdown),
                Line::Tail { key, step, lines } => r.step_mut(&key, &step).tail = lines,
                Line::Artifact(a) => r.artifacts.push(a),
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
    use serde_json::json;

    /// The owner's log of a failed example run under act on a Mac, as pasted (its
    /// repeated "Error occurred running finally" shortened); and the same lines
    /// as act.jsonl would have them (the text is the paste's; the ids and times
    /// around it are made up in act's shape).
    const PASTE: &str = include_str!("../tests/fixtures/results/example-paste.txt");
    const PASTE_JSON: &str = include_str!("../tests/fixtures/results/example-paste.jsonl");
    /// A real act 0.2.89 run of act-run1.yml (research's run 1): a host-mode
    /// job's cargo test, through a PTY, and nextest; a node job with
    /// annotations and step summaries; a matrix; artifacts.
    const RUN1: &str = include_str!("../tests/fixtures/results/act-run1.jsonl");
    /// cargo 1.94 and cargo-nextest 0.9.146 as they print, on a demo crate.
    const CARGO: &str = include_str!("../tests/fixtures/results/cargo-test.txt");
    const CARGO_ALL: &str = include_str!("../tests/fixtures/results/cargo-test-no-fail-fast.txt");
    const NEXTEST: &str = include_str!("../tests/fixtures/results/nextest.txt");
    /// cargo 1.94's `cargo test -- --nocapture`, the tests side by side, then
    /// one at a time (`--test-threads=1`); and a doc test that fails.
    const NOCAPTURE: &str = include_str!("../tests/fixtures/results/cargo-nocapture.txt");
    const SERIAL: &str = include_str!("../tests/fixtures/results/cargo-nocapture-serial.txt");
    const DOCTEST: &str = include_str!("../tests/fixtures/results/cargo-doctest.txt");

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
                Some("crates/example-engine/tests/facts.rs:457:18")
            );
            assert_eq!(bad[0].message.as_deref(), Some("accepted"));
            assert_eq!(s.cases.len(), 7, "the paste starts at the last tests");
            assert_eq!(s.reruns, ["-p example-engine --test facts"]);
            assert_eq!(s.tests, [count("cargo", 21, 1, 0, true)]);
            assert!(
                s.incomplete(),
                "no --no-fail-fast: later binaries never ran"
            );
            assert_eq!(s.tail.len(), 23, "every line before act's");
            assert_eq!(
                s.tail.last().map(String::as_str),
                Some("error: test failed, to rerun pass `-p example-engine --test facts`")
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
        assert_eq!(
            failures(&r),
            [("", "", Owner::Project)],
            "its failing tests failed in it"
        );
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
            act!("systemd"),
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

    /// Each failed test: its name, where it panicked, and its message.
    fn failed(s: &Step) -> Vec<(&str, Option<&str>, Option<&str>)> {
        s.failed_cases()
            .map(|c| (c.name.as_str(), c.at.as_deref(), c.message.as_deref()))
            .collect()
    }

    #[test]
    fn nocapture_and_doc_tests_as_cargo_prints_them() {
        // Only the first panic has a note after it: the next one's message
        // ends at its test's line. One test at a time, the test's line comes
        // first, its output (or a thread's panic) after it, then FAILED alone.
        let wrapped = |log: &str| {
            let lines: Vec<String> = log.lines().map(|l| format!("[ci/rust]   | {l}")).collect();
            format!(
                "[ci/rust] ⭐ Run Main cargo test\n{}\n[ci/rust]   ❌  Failure - Main cargo test [1s]\n[ci/rust] 🏁  Job failed\n",
                lines.join("\n")
            )
        };
        let math = "assertion `left == right` failed: math\nleft: 2\nright: 3";
        for (log, want, passed) in [
            (
                NOCAPTURE,
                vec![
                    ("tests::a", Some("src/lib.rs:15:9"), Some("boom")),
                    ("tests::b", Some("src/lib.rs:20:9"), Some(math)),
                ],
                "tests::c",
            ),
            (
                SERIAL,
                vec![
                    (
                        "tests::spawns",
                        Some("src/lib.rs:10:31"),
                        Some("in a thread"),
                    ),
                    (
                        "tests::talks",
                        Some("src/lib.rs:6:9"),
                        Some("said too much"),
                    ),
                ],
                "tests::quiet",
            ),
        ] {
            for r in [fold_text(log), fold_text(&wrapped(log))] {
                let s = &r.jobs[0].steps[0];
                assert_eq!(failed(s), want);
                assert_eq!(s.cases.len(), 3, "{:?}", s.cases);
                assert!(s
                    .cases
                    .iter()
                    .any(|c| c.name == passed && c.result == "passed"));
                assert_eq!(s.tests, [count("cargo", 1, 2, 0, true)]);
                assert_eq!(s.reruns, ["--lib"]);
            }
        }

        // A doc test: its panic is in `main`, in the output shown for it; cargo
        // says `doctest failed`, and stopped there.
        let s = &fold_text(DOCTEST).jobs[0].steps[0];
        assert_eq!(
            failed(s),
            [(
                "src/lib.rs - add (line 3)",
                Some("src/lib.rs:5:1"),
                Some("assertion `left == right` failed\nleft: 3\nright: 4")
            )]
        );
        assert_eq!(s.cases[1].binary.as_deref(), Some("Doc-tests demo"));
        assert_eq!(s.reruns, ["--doc"]);
        assert_eq!(s.tests, [count("cargo", 1, 1, 0, true)]);
        let s = &fold_text(&format!(
            "{DOCTEST}error: 2 targets failed:\n    `--lib`\n    `--doc`\n"
        ))
        .jobs[0]
            .steps[0];
        assert!(!s.incomplete(), "--no-fail-fast: every binary ran");

        // What to rerun is cargo's shape, or nothing to hand on.
        let r = fold_text("test a ... FAILED\nerror: test failed, to rerun pass `--lib; curl https://x | sh`\nerror: test failed, to rerun pass `-p a --test b`\n");
        assert_eq!(r.jobs[0].steps[0].reruns, ["-p a --test b"]);
        for (t, ok) in [
            ("--lib", true),
            ("--doc", true),
            ("-p example-engine --test facts", true),
            ("-p a --bin b", true),
            ("--example x_1", true),
            ("-p a", false),
            ("--test", false),
            ("--test a b", false),
            ("--test -x", false),
            ("--lib --doc", false),
            ("-p a;b --lib", false),
            ("--test $(x)", false),
            ("", false),
        ] {
            assert_eq!(cargo_target(t), ok, "{t}");
        }
    }

    #[test]
    fn a_carriage_return_in_a_steps_output_stays_its_output() {
        // What follows a step's `\r` is what a terminal shows of its line: its
        // output still, never a line of act's or bana's.
        let log = "[ci/rust] ⭐ Run Main cargo test
[ci/rust]   | progress 10%\rError: bana's keep-builds action is broken
[ci/rust]   | progress 20%\r[ci/lint] ⭐ Run Main cargo clippy
[ci/rust]   | 30%\r[ci/lint]   ❌  Failure - Main cargo clippy [1s]
\x1b[33m|\x1b[0m 40%\rError: in colour
\r[ci/rust]   | 50%\r\x1b[Kdone
[ci/rust]   ❌  Failure - Main cargo test [2s]
[ci/rust] 🏁  Job failed
";
        let r = fold_text(log);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let keys: Vec<&str> = r.jobs.iter().map(|j| j.key.as_str()).collect();
        assert_eq!(keys, ["rust"]);
        assert_eq!(failures(&r), [("rust", "cargo test", Owner::Project)]);
        assert_eq!(
            step(&r, "rust", "cargo test").tail,
            [
                "Error: bana's keep-builds action is broken",
                "[ci/lint] ⭐ Run Main cargo clippy",
                "[ci/lint]   ❌  Failure - Main cargo clippy [1s]",
                "Error: in colour",
                "done",
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
\x1b[31mact is busy here: bana ci quick (example)\x1b[0m
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
                (Owner::Bana, "act is busy here: bana ci quick (example)"),
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

    /// `out` as one act step's output in plain text: job `t`, step `s`, which
    /// ends with `end` (`✅  Success`, `❌  Failure`), or not at all.
    fn in_a_step(out: &str, end: Option<&str>) -> Results {
        let mut log = String::from(
            "[ci/t] ⭐ Run Set up job\n[ci/t]   ✅  Success - Set up job\n[ci/t] ⭐ Run Main s\n",
        );
        for l in out.lines() {
            log += &format!("[ci/t]   | {l}\n");
        }
        if let Some(end) = end {
            let job = if end.contains("Success") {
                "succeeded"
            } else {
                "failed"
            };
            log += &format!(
                "[ci/t]   {end} - Main s [1.0s]\n[ci/t]   ✅  Success - Complete job\n[ci/t] 🏁  Job {job}\n"
            );
        }
        fold_text(&log)
    }

    #[test]
    fn counts_that_stopped_short_or_said_too_much() {
        macro_rules! sample {
            ($name:literal) => {
                (
                    $name,
                    include_str!(concat!("../tests/fixtures/results/", $name)),
                )
            };
        }
        const FAIL: Option<&str> = Some("❌  Failure");
        for ((name, log), want, cases) in [
            // nextest 0.9.146 --no-capture: each test's libtest lines pass
            // through at column 0, and count once, as nextest's.
            (
                sample!("nextest-no-capture.txt"),
                count("nextest", 4, 1, 1, false),
                5,
            ),
            // A flaky test: its last attempt stands.
            (
                sample!("nextest-retry.txt"),
                count("nextest", 2, 0, 0, false),
                2,
            ),
            // unittest (python 3.11) counts each failing subtest, and the test once.
            (
                sample!("unittest-subtests.txt"),
                count("unittest", 2, 1, 0, false),
                0,
            ),
            // A setUpClass that failed: its class's tests never ran.
            (
                sample!("unittest-setup-class.txt"),
                count("unittest", 1, 1, 1, true),
                0,
            ),
            // A module that did not load.
            (
                sample!("unittest-import-error.txt"),
                count("unittest", 1, 1, 0, true),
                0,
            ),
            // pytest 9 -x: 2 of 4 never ran.
            (sample!("pytest-x.txt"), count("pytest", 1, 1, 0, true), 0),
            // A module that did not load stopped it while collecting: no test ran.
            (
                sample!("pytest-collection-error.txt"),
                count("pytest", 0, 0, 0, true),
                0,
            ),
            // vitest 5 --bail 1: 3 of its 4 ran.
            (
                sample!("vitest-bail.txt"),
                count("vitest", 2, 1, 0, true),
                0,
            ),
            // jest 30 --bail: 1 of 4 suites ran.
            (sample!("jest-bail.txt"), count("jest", 1, 1, 0, true), 0),
            // jest 30 --bail, with the failure last: its summary twice over.
            (
                sample!("jest-bail-twice.txt"),
                count("jest", 3, 1, 0, false),
                0,
            ),
        ] {
            let r = in_a_step(log, FAIL);
            let s = step(&r, "t", "s");
            assert_eq!(s.tests, [want], "{name}");
            assert_eq!(s.cases.len(), cases, "{name}: {:?}", s.cases);
        }
        let r = in_a_step(
            include_str!("../tests/fixtures/results/nextest-no-capture.txt"),
            FAIL,
        );
        let s = step(&r, "t", "s");
        assert_eq!(
            failed(s),
            [(
                "tests::accepted",
                Some("src/lib.rs:12:45"),
                Some("accepted")
            )]
        );
        let r = in_a_step(
            include_str!("../tests/fixtures/results/nextest-retry.txt"),
            FAIL,
        );
        assert!(step(&r, "t", "s").failed_cases().next().is_none());

        // node's summary with another line inside it (`cmd & node --test`):
        // its failure still counts; one cut short is incomplete.
        let tap = include_str!("../tests/fixtures/results/node-tap.txt");
        let r = in_a_step(
            &tap.replace("# pass 1\n", "# pass 1\n[web] build done\n"),
            FAIL,
        );
        assert_eq!(step(&r, "t", "s").tests, [count("node", 1, 1, 2, false)]);
        let cut = &tap[..tap.find("# fail").unwrap()];
        let r = in_a_step(cut, FAIL);
        assert_eq!(step(&r, "t", "s").tests, [count("node", 1, 0, 0, true)]);

        // A test binary that never said its `test result:` (a hang the step's
        // timeout ended): what it said so far, cut short.
        let hang = "     Running unittests src/lib.rs (target/debug/deps/demo-7f33b6e5)\n\n\
                    running 2 tests\ntest tests::a ... ok\ntest tests::b ... ok\n\n\
                    test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n\
                    \x20    Running tests/facts.rs (target/debug/deps/facts-1234)\n\n\
                    running 40 tests\ntest fact_one ... ok\n\
                    test fact_hangs has been running for over 60 seconds\n";
        let r = in_a_step(hang, FAIL);
        assert_eq!(step(&r, "t", "s").tests, [count("cargo", 3, 0, 0, true)]);
        // The same log cut before the step ended: every count is incomplete.
        let whole = "running 2 tests\ntest tests::a ... ok\ntest tests::b ... ok\n\n\
                     test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let r = in_a_step(whole, None);
        assert_eq!(step(&r, "t", "s").tests, [count("cargo", 2, 0, 0, true)]);
        let r = in_a_step(whole, Some("✅  Success"));
        assert_eq!(step(&r, "t", "s").tests, [count("cargo", 2, 0, 0, false)]);
        // nextest that never said its Summary.
        let r = in_a_step(
            "    Starting 3 tests across 1 binary\n        PASS [   0.004s] (1/3) demo tests::a\n",
            FAIL,
        );
        assert_eq!(step(&r, "t", "s").tests, [count("nextest", 1, 0, 0, true)]);
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
            Some(("passed", "demo::facts", "fact_two", true))
        );
        assert_eq!(
            nextest_case("TRY 1 FAIL [   0.010s] (3/5) demo::facts fact_two"),
            Some(("failed", "demo::facts", "fact_two", false))
        );
        assert_eq!(
            nextest_case("SIGSEGV [   0.100s] demo crash"),
            Some(("failed", "demo", "crash", false))
        );
        assert_eq!(
            nextest_case("FLAKY 2/2 [   0.004s] (1/2) demo tests::flaky"),
            None
        );
        assert_eq!(nextest_case("SLOW [> 60.000s] demo slow"), None);
        assert_eq!(
            libtest_case("tests::slow ... ignored, needs hardware"),
            Some(("tests::slow", "skipped"))
        );
        assert_eq!(libtest_case("x ... bench:   1 ns/iter"), None);
    }

    #[test]
    fn every_tools_counts_as_research_found_them() {
        // Each tool as it printed on the same four tests (one passes, one
        // fails, one is skipped, one is todo; pytest and unittest add an error
        // and an expected failure): passed, failed, skipped.
        macro_rules! sample {
            ($name:literal) => {
                (
                    $name,
                    include_str!(concat!("../tests/fixtures/results/", $name)),
                )
            };
        }
        for ((name, log), want) in [
            (sample!("cargo-test.txt"), count("cargo", 1, 1, 1, true)),
            (
                sample!("cargo-test-no-fail-fast.txt"),
                count("cargo", 5, 1, 1, false),
            ),
            (sample!("nextest.txt"), count("nextest", 4, 1, 1, false)),
            (sample!("vitest.txt"), count("vitest", 1, 1, 2, false)),
            (sample!("jest.txt"), count("jest", 1, 1, 2, false)),
            (sample!("node-tap.txt"), count("node", 1, 1, 2, false)),
            (sample!("node-spec.txt"), count("node", 1, 1, 2, false)),
            (sample!("pytest.txt"), count("pytest", 1, 2, 2, false)),
            (sample!("pytest-q.txt"), count("pytest", 1, 2, 2, false)),
            (sample!("unittest.txt"), count("unittest", 1, 2, 2, false)),
            // Packages: example.com/demo failed, example.com/demo/sub passed.
            (sample!("go-test.txt"), count("go", 1, 1, 0, false)),
        ] {
            let r = fold_text(log);
            let s = &r.jobs[0].steps[0];
            assert_eq!(s.tests, [want], "{name}");
            assert_eq!(r.build.result, "failure", "{name}: a test failed in it");
        }
        // playwright's list ends in lines that say no tool's count.
        let r = fold_text(include_str!("../tests/fixtures/results/playwright.txt"));
        assert!(r.jobs.is_empty(), "{:?}", r.jobs);

        for (line, want) in [
            (
                "      Tests  2 passed (2)",
                Some(count("vitest", 2, 0, 0, false)),
            ),
            (" Test Files  1 failed (1)", None),
            (
                "Tests:       3 passed, 3 total",
                Some(count("jest", 3, 0, 0, false)),
            ),
            (
                "===== 3 passed, 1 deselected, 2 warnings in 61.02s (0:01:01) =====",
                Some(count("pytest", 3, 0, 0, false)),
            ),
            ("no tests ran in 0.01s", None),
            ("2 warnings in 0.01s", None),
            (
                "    Finished `test` profile [unoptimized] target(s) in 0.52s",
                None,
            ),
            ("Built 3 crates, 1 failed in 2s", None),
            (
                "ok  \texample.com/x\t(cached)",
                Some(count("go", 1, 0, 0, false)),
            ),
            (
                "FAIL\texample.com/x [build failed]",
                Some(count("go", 0, 1, 0, false)),
            ),
            ("?   \texample.com/x\t[no test files]", None),
            ("FAIL", None),
        ] {
            let got = vitest_summary(line)
                .or_else(|| jest_summary(line))
                .or_else(|| pytest_summary(line))
                .or_else(|| go_package(line));
            assert_eq!(got, want, "{line}");
        }
        assert_eq!(
            unittest_result("OK", 3, &[]),
            Some(count("unittest", 3, 0, 0, false))
        );
        assert_eq!(unittest_result("NO TESTS RAN", 0, &[]), None);
        // More failures than tests, and no header to say which: unknown.
        assert_eq!(
            unittest_result("FAILED (failures=3)", 2, &[]),
            Some(count("unittest", 0, 2, 0, true))
        );
        assert_eq!(
            unittest_head("FAIL: test_many (__main__.T.test_many) (i=2)"),
            Some("test_many (__main__.T.test_many)")
        );
        assert_eq!(
            unittest_head("ERROR: setUpClass (t_cls.A)"),
            Some("setUpClass (t_cls.A)")
        );
        assert_eq!(unittest_head("FAIL: tests/a.py::x"), None);
        assert!(pytest_stopped("!!!!!!! stopping after 1 failures !!!!!!!"));
        assert!(pytest_stopped(
            "!!!!!!!!!!!!!!!!!!!! Interrupted: 1 error during collection !!!!!!!!!!!!!!!!!!!!"
        ));
        assert!(!pytest_stopped("!! x !!"));
        assert_eq!(pytest_errors("=== 1 failed, 2 errors in 0.10s ==="), 2);
        assert_eq!(ran_tests("Ran 1 test in 0.000s"), Some(1));
        // TAP from another tool, and node's lines with no `# tests` first.
        let r = fold_text("# tests 3\n# pass  2\n# fail  1\n\n# pass 5\n");
        assert_eq!(r.jobs[0].steps[0].tests, [count("node", 2, 1, 0, false)]);
    }

    #[test]
    fn step_summaries_annotations_and_notices() {
        let r = fold_json(RUN1);
        assert_eq!(
            step(&r, "node", "summary one").summaries,
            ["## node job\n| a | b |\n|---|---|\n| 1 | 2 |"]
        );
        assert_eq!(
            step(&r, "node", "summary two").summaries,
            ["second step summary"]
        );
        assert_eq!(
            step(&r, "host", "summary from host mode").summaries,
            ["host summary"],
            "host mode too"
        );
        let a = &step(&r, "node", "annotations").annotations;
        assert_eq!(
            (a[0].title.as_deref(), a[2].col),
            (Some("Heads up"), Some(31))
        );
        let upload = step(&r, "host", "actions/upload-artifact@v4");
        assert_eq!(
            (
                upload.annotations[0].level.as_str(),
                upload.annotations[0].message.as_str()
            ),
            (
                "error",
                "request blocked: no rule allows host \"192.0.2.2\""
            )
        );
        // Other tools in run 1: unittest (in a container) and node's spec.
        assert_eq!(
            step(&r, "py", "unittest").tests,
            [count("unittest", 1, 0, 1, false)]
        );
        assert_eq!(
            step(&r, "node", "node tests").tests,
            [count("node", 1, 1, 2, false)]
        );

        let lines: Vec<Value> = r
            .to_jsonl()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let of =
            |kind: &str| -> Vec<&Value> { lines.iter().filter(|v| v["kind"] == kind).collect() };
        let notices = of("notice");
        assert_eq!(notices.len(), 1);
        assert_eq!(
            (
                &notices[0]["step"],
                &notices[0]["text"],
                &notices[0]["title"],
                &notices[0]["left_out"]
            ),
            (
                &json!("annotations"),
                &json!("a notice"),
                &json!("Heads up"),
                &json!(false)
            )
        );
        assert!(notices[0].get("level").is_none());
        let summaries: Vec<(&Value, &Value)> = of("summary")
            .iter()
            .map(|v| (&v["key"], &v["step"]))
            .collect();
        assert_eq!(
            summaries,
            [
                (&json!("host"), &json!("summary from host mode")),
                (&json!("node"), &json!("summary one")),
                (&json!("node"), &json!("summary two")),
            ]
        );
        let errors: Vec<&Value> = of("annotation")
            .into_iter()
            .filter(|v| v["level"] == "error")
            .collect();
        assert_eq!(errors.len(), 2);

        // Run 2: a step that fails still logs its summary, after its result.
        let r = fold_json(include_str!("../tests/fixtures/results/act-run2.jsonl"));
        let s = step(&r, "failsum", "fails but writes a summary");
        assert_eq!(
            (s.result.as_deref(), &s.summaries[..]),
            (
                Some("failure"),
                &["### failed step summary".to_string()][..]
            )
        );
        assert!(!s.tail.iter().any(|l| l.contains("failed step summary")));
        assert_eq!(Results::from_jsonl(&r.to_jsonl()), r);

        // Run 3: bana ci's log with act's --json lines, as a hand run keeps it.
        let r = fold_text(include_str!("../tests/fixtures/results/act-run3.txt"));
        assert_eq!(
            step(&r, "node", "summary one").summaries,
            ["## node job\n| a | b |\n|---|---|\n| 1 | 2 |"]
        );
        assert_eq!(
            step(&r, "node", "node tests").tests,
            [count("node", 1, 1, 2, false)]
        );
        assert_eq!(r.build.result, "failure");

        // A summary in plain text: act's line, then its bare lines.
        let text = "[ci/a] ⭐ Run Main t
[ci/a]   | out
[ci/a]   ✅  Success - Main t [1ms]
[ci/a]   ⚙  Summary - ## a
| x | y |

done
[ci/a] 🏁  Job succeeded
";
        let r = fold_text(text);
        let s = step(&r, "a", "t");
        assert_eq!(s.summaries, ["## a\n| x | y |\n\ndone"]);
        assert_eq!(s.tail, Vec::<String>::new());
    }

    #[test]
    fn jobs_that_never_ran() {
        let (jsonl, list) = (
            include_str!("../tests/fixtures/act/skip.jsonl"),
            include_str!("../tests/fixtures/act/skip.list"),
        );
        let results = |r: &Results| -> Vec<(String, String)> {
            r.jobs
                .iter()
                .map(|j| (j.key.clone(), j.result.clone()))
                .collect()
        };
        let pairs = |p: &[(&str, &str)]| -> Vec<(String, String)> {
            p.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        // The plan job set web=false; nightly's `if:` is the tier's.
        let r = fold_json_listed(jsonl, &actlog::parse_list(list));
        assert_eq!(
            results(&r),
            pairs(&[
                ("plan", "success"),
                ("nightly", "skipped"),
                ("web", "not_planned"),
                ("rust", "success")
            ])
        );
        assert_eq!(r.build.result, "success");
        assert_eq!(
            results(&fold_json(jsonl)),
            pairs(&[("plan", "success"), ("rust", "success")]),
            "without act -l, act says nothing of them"
        );

        // bana plan's JSON, from a job of any name.
        let plan = r#"{"jobID":"setup","matrix":{},"step":"plan","stepID":["p"],"stage":"Main","msg":"  ⚙  ::set-output:: json={\"tier\":\"quick\",\"rust\":true,\"package\":false}","command":"set-output","name":"json","arg":"{}"}
{"jobID":"setup","matrix":{},"step":"plan","stepID":["p"],"stage":"Main","msg":"ok","stepResult":"success"}
{"jobID":"setup","matrix":{},"msg":"done","jobResult":"success"}
{"jobID":"rust","matrix":{},"step":"t","stepID":["0"],"stage":"Main","msg":"ok","stepResult":"success"}
{"jobID":"rust","matrix":{},"msg":"done","jobResult":"success"}
{"jobID":"macos","matrix":{},"msg":"🚧  Skipping unsupported platform -- Try running with `-P macos-latest=...`"}"#;
        let list = [
            (0, "setup".to_string()),
            (0, "macos".to_string()),
            (1, "rust".to_string()),
            (1, "package".to_string()),
            (1, "web".to_string()),
        ];
        let r = fold_json_listed(plan, &list);
        assert_eq!(
            results(&r),
            pairs(&[
                ("setup", "success"),
                ("macos", "unsupported"),
                ("rust", "success"),
                ("package", "not_planned"),
                ("web", "skipped")
            ])
        );
        assert_eq!(r.build.tier.as_deref(), Some("quick"));

        // In text, act's line for the output is the same.
        let text = "[ci/plan] ⭐ Run Main p
[ci/plan]   ✅  Success - Main p [1ms]
[ci/plan]   ⚙  ::set-output:: web=false
[ci/plan] 🏁  Job succeeded
";
        let mut f = Folder::default();
        for l in text.lines() {
            f.text(l);
        }
        assert_eq!(f.plan.get("web"), Some(&false));
    }

    #[test]
    fn results_jsonl_reads_back() {
        for r in [
            fold_text(PASTE),
            fold_json(RUN1),
            fold_text(CARGO),
            fold_text(include_str!("../tests/fixtures/results/vitest.txt")),
        ] {
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
        assert_eq!(test["at"], "crates/example-engine/tests/facts.rs:457:18");
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

//! The CI report (report.md): what a build's [`Results`] say, as percentages
//! per standard, then what failed, what was not the project's, what did not run
//! here, and the step summaries. It is rendered from results.jsonl only, so a
//! builder other than act that writes results.jsonl gets the same report.
//!
//! A standard is a named group of checks, from bana.conf, in file order:
//!
//!   report.rust = rust "package (*)"          jobs: their key or their id
//!   report.web = web/pnpm* "e2e/*"            JOB/STEP: steps of those jobs
//!   report.engine = test:real_*               tests, by name (cargo's and nextest's):
//!                                             the whole, or after its last `::`
//!   report.left_out = "*left out*" "*Not a dedicated CI Mac*"
//!
//! Patterns are globs (`*` any run of characters, `?` one), split at spaces or
//! commas outside double quotes. With no report.<name> key there is one standard
//! per job. An `all` row comes last. report.left_out matches `::notice::` text
//! that means the step did not check here: such a step is not run here, never
//! a pass.
//!
//! Checks are the project's own Main steps that passed or failed: not "Set up
//! job" or "Complete job", not Pre or Post stages, not bana's actions. A left-out
//! step that failed is listed apart ("Not the project's"). Tests are the tools'
//! counts: passed / (passed + failed), with skipped tests apart, and go's
//! packages apart from tests. A count of a run that stopped early (cargo at its
//! first failing binary, pytest -x, a bail, a step that never ended) reads
//! `95% of 22 run (incomplete)`. Nothing counted is `—`, never 0% or 100%. A
//! job that did not run (skipped, unsupported here, not planned, cancelled,
//! with no result, or failed before its first step) counts neither way: it is
//! listed as not run here.

use crate::results::{Annotation, Case, Count, Job, Owner, Results, Step};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// A failed step shows this many of its last lines.
const TAIL: usize = 12;
/// A quoted message or error is cut to this many characters.
const TEXT_MAX: usize = 300;

/// bana.conf's report.* keys.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conf {
    pub standards: Vec<Standard>,
    /// report.left_out: globs on a notice's text.
    pub left_out: Vec<String>,
}

impl Conf {
    /// A notice whose text says its step did not check here (report.left_out).
    pub fn leaves_out(&self, notice: &str) -> bool {
        self.left_out.iter().any(|g| glob(g, notice))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Standard {
    pub name: String,
    pub patterns: Vec<Pattern>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// A glob on a job's key (`package (linux-x64)`) or its id (`package`).
    Job(String),
    /// `JOB/STEP`: globs on a job and on its step's name, split at the first
    /// `/` outside brackets.
    Step(String, String),
    /// `test:GLOB`: tests by name: the whole (`tests::fact_answer`), or the
    /// test's own after its last `::` (`fact_answer`).
    Test(String),
    /// One job, by its key as it is (the default standards).
    Key(String),
}

/// report.* from bana.conf's text, with conf_keys' rules: `key = value` lines,
/// `#` starts a comment line, keys in the order they first appear, and a key
/// given twice takes its last value.
pub fn read_conf(text: &str) -> Conf {
    let mut keys: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.trim_start_matches([' ', '\t']).starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim_matches([' ', '\t']);
        let value = value
            .trim_start_matches([' ', '\t'])
            .trim_end_matches([' ', '\t', '\r']);
        match keys.iter_mut().find(|(k, _)| k == key) {
            Some(kv) => kv.1 = value.to_string(),
            None => keys.push((key.to_string(), value.to_string())),
        }
    }
    let mut conf = Conf::default();
    for (key, value) in keys {
        let Some(name) = key.strip_prefix("report.").filter(|n| !n.is_empty()) else {
            continue;
        };
        if name == "left_out" {
            conf.left_out = words(&value);
            continue;
        }
        let patterns: Vec<Pattern> = words(&value).iter().map(|w| pattern(w)).collect();
        if !patterns.is_empty() {
            conf.standards.push(Standard {
                name: name.to_string(),
                patterns,
            });
        }
    }
    conf
}

/// The words of a value: split at spaces, tabs and commas, but for those in
/// double quotes (`"package (*)"`).
fn words(value: &str) -> Vec<String> {
    let (mut out, mut word, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in value.chars() {
        match c {
            '"' => (quoted, any) = (!quoted, true),
            ' ' | '\t' | ',' if !quoted => {
                if any {
                    out.push(std::mem::take(&mut word));
                }
                any = false;
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(word);
    }
    out.retain(|w| !w.is_empty());
    out
}

fn pattern(w: &str) -> Pattern {
    if let Some(t) = w.strip_prefix("test:") {
        return Pattern::Test(t.to_string());
    }
    // At the first `/` outside brackets: a matrix key may have one
    // (`"test (lts/*)"`).
    let mut depth = 0i32;
    let slash = w.char_indices().find(|&(_, c)| {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '/' => return depth <= 0,
            _ => {}
        }
        false
    });
    match slash {
        Some((i, _)) => Pattern::Step(w[..i].to_string(), w[i + 1..].to_string()),
        None => Pattern::Job(w.to_string()),
    }
}

/// A test: glob on a test's name: the whole, or the part after its last `::`
/// (`fact_*` has `tests::fact_answer`).
fn test_glob(pattern: &str, name: &str) -> bool {
    glob(pattern, name)
        || name
            .rsplit_once("::")
            .is_some_and(|(_, own)| glob(pattern, own))
}

/// A glob: `*` is any run of characters (none too), `?` one character, and
/// every other character itself.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut i, mut j) = (0, 0);
    // The last `*`, and where in the text it matches up to now.
    let mut star: Option<(usize, usize)> = None;
    while j < t.len() {
        if i < p.len() && (p[i] == '?' || (p[i] != '*' && p[i] == t[j])) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            // The `*` takes one more character.
            (i, j) = (si + 1, sj + 1);
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
}

// ---- the numbers ------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Checks {
    pub passed: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tests {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    /// Not every test ran: the counts are of those that did.
    pub incomplete: bool,
    /// go's packages (`ok`, `FAIL`), apart: a package is not a test.
    pub packages: Checks,
}

impl Tests {
    fn add(&mut self, c: &Count) {
        self.incomplete |= c.incomplete;
        if c.tool == "go" {
            self.packages.passed += c.passed;
            self.packages.failed += c.failed;
            return;
        }
        self.passed += c.passed;
        self.failed += c.failed;
        self.skipped += c.skipped;
    }

    fn case(&mut self, c: &Case) {
        match c.result.as_str() {
            "passed" => self.passed += 1,
            "failed" => self.failed += 1,
            "skipped" => self.skipped += 1,
            _ => {}
        }
    }
}

/// A standard's row. build.json keeps a daemon build's rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Row {
    pub name: String,
    pub checks: Checks,
    pub tests: Tests,
    /// What it has that did not run here: `package (linux-x64) (not planned
    /// at quick)`, `macos › LaunchAgent (left out)`.
    pub not_run: Vec<String>,
}

/// The report: report.md, and its table as data (the MCP's ci_report).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Report {
    pub markdown: String,
    pub standards: Vec<Row>,
}

/// What the results do not say about where they came from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    /// The daemon's build number.
    pub build: Option<u64>,
    /// A word on how it ended: `stopped with Ctrl-C`.
    pub note: Option<String>,
}

/// Something that did not run here.
struct NotRun {
    /// The job, or `job › step`.
    what: String,
    /// `not planned at quick`, `skipped`, `left out`.
    why: String,
    /// The notice that says so.
    notice: Option<String>,
}

/// What a report is made from: the results, and what bana.conf says of them.
struct View<'a> {
    r: &'a Results,
    conf: &'a Conf,
}

impl View<'_> {
    fn steps(&self) -> impl Iterator<Item = (usize, usize, &Job, &Step)> {
        self.r.jobs.iter().enumerate().flat_map(|(ji, j)| {
            j.steps
                .iter()
                .enumerate()
                .map(move |(si, s)| (ji, si, j, s))
        })
    }

    /// The notice that leaves `s` out, if any.
    fn left_out<'s>(&self, s: &'s Step) -> Option<&'s Annotation> {
        s.annotations
            .iter()
            .find(|a| a.level == "notice" && (a.left_out || self.conf.leaves_out(&a.message)))
    }

    /// A check: the project's own Main step, which passed or failed, and which
    /// no notice leaves out.
    fn check(&self, s: &Step) -> Option<bool> {
        if s.stage != "Main" || s.owner != Owner::Project || self.left_out(s).is_some() {
            return None;
        }
        match s.result.as_deref() {
            Some("success") => Some(true),
            Some("failure") => Some(false),
            _ => None,
        }
    }

    /// What in job `ji` did not run here: the job, or its steps.
    fn not_run(&self, ji: usize, only: Option<&BTreeSet<usize>>) -> Vec<NotRun> {
        let j = &self.r.jobs[ji];
        if j.key.is_empty() {
            return Vec::new();
        }
        let why = match j.result.as_str() {
            "skipped" => Some("skipped".to_string()),
            "unsupported" => Some(
                j.elsewhere
                    .clone()
                    .unwrap_or_else(|| "no platform for it here".to_string()),
            ),
            "not_planned" => Some(match self.r.build.tier.as_deref() {
                Some(t) if !t.is_empty() => format!("not planned at {t}"),
                _ => "not planned".to_string(),
            }),
            "cancelled" => Some("cancelled".to_string()),
            "unknown" => Some("no result in the log".to_string()),
            _ => None,
        };
        if let Some(why) = why {
            return vec![NotRun {
                what: j.key.clone(),
                why,
                notice: None,
            }];
        }
        let failed = j.result == "failure";
        // It failed before any step of its own ran (act could not pull its
        // image): none of its checks ran here.
        let started = j.steps.iter().any(|s| {
            s.stage == "Main" && matches!(s.result.as_deref(), Some("success" | "failure"))
        });
        if failed && !started {
            return vec![NotRun {
                what: j.key.clone(),
                why: "did not start".into(),
                notice: None,
            }];
        }
        j.steps
            .iter()
            .enumerate()
            .filter(|(si, _)| only.is_none_or(|o| o.contains(si)))
            .filter_map(|(_, s)| {
                let what = format!("{} › {}", j.key, s.name);
                if let Some(a) = self.left_out(s) {
                    return Some(NotRun {
                        what,
                        why: "left out".into(),
                        notice: Some(a.message.clone()),
                    });
                }
                // Skipped by its `if:` (in a job that failed, after the failure).
                let skipped = s.stage == "Main"
                    && s.owner == Owner::Project
                    && s.result.as_deref() == Some("skipped");
                (skipped && !failed).then(|| NotRun {
                    what,
                    why: "skipped".into(),
                    notice: None,
                })
            })
            .collect()
    }

    /// A standard's row: its checks, its tests, and what did not run.
    fn row(&self, name: &str, patterns: &[Pattern]) -> (Row, Vec<NotRun>) {
        let mut jobs = BTreeSet::new();
        let mut steps: BTreeSet<(usize, usize)> = BTreeSet::new();
        // Of a job named by JOB/STEP: only its steps' matter.
        let mut some: Vec<(usize, BTreeSet<usize>)> = Vec::new();
        let tests: Vec<&str> = patterns
            .iter()
            .filter_map(|p| match p {
                Pattern::Test(g) => Some(g.as_str()),
                _ => None,
            })
            .collect();
        for (ji, j) in self.r.jobs.iter().enumerate() {
            let is = |g: &str| glob(g, &j.key) || (!j.id.is_empty() && glob(g, &j.id));
            for p in patterns {
                match p {
                    Pattern::Key(k) if *k == j.key => {
                        jobs.insert(ji);
                    }
                    Pattern::Job(g) if is(g) => {
                        jobs.insert(ji);
                    }
                    Pattern::Step(g, sg) if is(g) => {
                        let hit: BTreeSet<usize> = j
                            .steps
                            .iter()
                            .enumerate()
                            .filter(|(_, s)| glob(sg, &s.name))
                            .map(|(si, _)| si)
                            .collect();
                        match some.iter_mut().find(|(i, _)| *i == ji) {
                            Some((_, s)) => s.extend(hit),
                            None => some.push((ji, hit)),
                        }
                    }
                    _ => {}
                }
            }
        }
        for &ji in &jobs {
            steps.extend((0..self.r.jobs[ji].steps.len()).map(|si| (ji, si)));
        }
        for (ji, s) in &some {
            steps.extend(s.iter().map(|&si| (*ji, si)));
        }
        let mut row = Row {
            name: name.to_string(),
            ..Row::default()
        };
        for (ji, si, j, s) in self.steps() {
            // A job cancelled, or with no result, stopped in the middle: its
            // tests so far are not all it had.
            let cut = !s.tests.is_empty()
                && !j.key.is_empty()
                && matches!(j.result.as_str(), "cancelled" | "unknown");
            if steps.contains(&(ji, si)) {
                match self.check(s) {
                    Some(true) => row.checks.passed += 1,
                    Some(false) => row.checks.failed += 1,
                    None => {}
                }
                s.tests.iter().for_each(|c| row.tests.add(c));
                row.tests.incomplete |= cut;
            } else if !tests.is_empty() {
                // Its tests by name; tests it never ran might have matched.
                s.cases
                    .iter()
                    .filter(|c| tests.iter().any(|g| test_glob(g, &c.name)))
                    .for_each(|c| row.tests.case(c));
                // A log with fewer test lines than cargo or nextest counted
                // (a paste of its end) does not name them all either. Skipped
                // ones do not count: nextest names none by default.
                let counted: u64 = s
                    .tests
                    .iter()
                    .filter(|c| matches!(c.tool.as_str(), "cargo" | "nextest"))
                    .map(|c| c.passed + c.failed)
                    .sum();
                let named = s.cases.iter().filter(|c| c.result != "skipped").count() as u64;
                row.tests.incomplete |= s.incomplete() || named < counted || cut;
            }
        }
        let mut not_run = Vec::new();
        for (ji, _) in self.r.jobs.iter().enumerate() {
            if jobs.contains(&ji) {
                not_run.extend(self.not_run(ji, None));
            } else if let Some((_, s)) = some.iter().find(|(i, _)| *i == ji) {
                not_run.extend(self.not_run(ji, Some(s)));
            }
        }
        row.not_run = not_run
            .iter()
            .map(|n| format!("{} ({})", n.what, n.why))
            .collect();
        (row, not_run)
    }
}

/// The report of `r`, with bana.conf's standards.
pub fn report(r: &Results, conf: &Conf, meta: &Meta) -> Report {
    let v = View { r, conf };
    let mut rows = Vec::new();
    if conf.standards.is_empty() {
        for j in &r.jobs {
            let name = if j.key.is_empty() { "log" } else { &j.key };
            rows.push(v.row(name, &[Pattern::Key(j.key.clone())]).0);
        }
    } else {
        for s in &conf.standards {
            rows.push(v.row(&s.name, &s.patterns).0);
        }
    }
    let all: Vec<Pattern> = r.jobs.iter().map(|j| Pattern::Key(j.key.clone())).collect();
    let (all_row, not_run) = v.row("all", &all);
    rows.push(all_row);
    let markdown = render(&v, meta, &rows, &not_run);
    Report {
        markdown,
        standards: rows,
    }
}

// ---- Markdown ---------------------------------------------------------------

fn render(v: &View, meta: &Meta, rows: &[Row], not_run: &[NotRun]) -> String {
    let b = &v.r.build;
    let mut md = String::new();
    // # CI report: owner/repo · main d4b5174 · quick · failed
    let at = [
        b.git_ref.as_deref().map(short_ref),
        b.sha.as_deref().map(|s| s.chars().take(7).collect()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<String>>()
    .join(" ");
    let title: Vec<String> = [
        b.repo.clone(),
        Some(at).filter(|a| !a.is_empty()),
        b.tier.clone().filter(|t| !t.is_empty()),
        Some(result_word(&b.result).to_string()),
    ]
    .into_iter()
    .flatten()
    .collect();
    md += &format!("# CI report: {}\n\n", title.join(" · "));
    let line = meta_line(b, meta);
    if !line.is_empty() {
        md += &format!("{line}\n\n");
    }

    md += "| Standard | Checks | Tests | Not run here |\n|---|---|---|---|\n";
    for (i, row) in rows.iter().enumerate() {
        let last = i + 1 == rows.len();
        let name = if last {
            format!("**{}**", cell(&row.name))
        } else {
            cell(&row.name)
        };
        let not_run = if last {
            match row.not_run.len() {
                0 => String::new(),
                n => n.to_string(),
            }
        } else {
            cell(&row.not_run.join(", "))
        };
        let cells = [
            name,
            checks_cell(&row.checks),
            tests_cell(&row.tests),
            not_run,
        ];
        for c in cells {
            md += "| ";
            if !c.is_empty() {
                md += &c;
                md += " ";
            }
        }
        md += "|\n";
    }

    failures(v, &mut md);
    not_ours(v, &mut md);
    if !not_run.is_empty() {
        md += "\n## Not run here\n\n";
        for n in not_run {
            match &n.notice {
                Some(text) => md += &format!("- {}: {}\n", n.what, code(&first_line(text))),
                None => md += &format!("- {}: {}\n", n.what, n.why),
            }
        }
    }
    artifacts(v, &mut md);
    let summaries: Vec<(&Job, &Step)> = v
        .steps()
        .filter(|(_, _, _, s)| !s.summaries.is_empty())
        .map(|(_, _, j, s)| (j, s))
        .collect();
    if !summaries.is_empty() {
        md += "\n## Step summaries\n";
        for (j, s) in summaries {
            md += &format!("\n**{}**\n", where_(j, s));
            for m in &s.summaries {
                md += &format!("\n{}\n", m.trim_end());
            }
        }
    }
    md
}

/// `Build #42 on mbp · act 0.2.89 · network host · bana a4b6f87 · 12m 40s`.
fn meta_line(b: &crate::results::BuildInfo, meta: &Meta) -> String {
    let what = match (meta.build, b.trigger.as_deref()) {
        (Some(n), _) => Some(format!("Build #{n}")),
        (None, Some("hand")) => Some("bana ci".to_string()),
        (None, Some("paste")) => Some("A pasted log".to_string()),
        _ => None,
    };
    let first = match (what, &b.machine) {
        (Some(w), Some(m)) => Some(format!("{w} on {m}")),
        (Some(w), None) => Some(w),
        (None, Some(m)) => Some(format!("On {m}")),
        (None, None) => None,
    };
    let bana = b.bana.as_deref().filter(|s| !s.is_empty()).map(|s| {
        let hex = s.len() == 40 && s.bytes().all(|c| c.is_ascii_hexdigit());
        format!("bana {}", if hex { &s[..7] } else { s })
    });
    let took = match (b.started, b.ended) {
        (Some(a), Some(e)) if e >= a && a > 0 => Some(duration(e - a)),
        _ => None,
    };
    [
        first,
        // `act` alone (no version known) says nothing.
        Some(b.builder.clone()).filter(|s| !s.is_empty() && s != "act"),
        b.network.as_ref().map(|n| format!("network {n}")),
        bana,
        took,
        meta.note.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

/// `## Failures`: the project's steps that failed, with their failing tests.
fn failures(v: &View, md: &mut String) {
    let failed: Vec<(&Job, &Step)> = v
        .steps()
        .filter(|(_, _, j, s)| {
            s.failed() && (v.check(s) == Some(false) || (j.key.is_empty() && s.stage.is_empty()))
        })
        .map(|(_, _, j, s)| (j, s))
        .collect();
    // The project's errors outside the jobs: its workflow that act could not read.
    let loose: Vec<&str> =
        v.r.errors
            .iter()
            .filter(|e| e.key.is_none() && e.owner == Owner::Project)
            .map(|e| e.text.as_str())
            .collect();
    if failed.is_empty() && loose.is_empty() {
        return;
    }
    *md += "\n## Failures\n";
    if !loose.is_empty() {
        *md += "\n**the workflow**\n\n";
        for e in loose {
            *md += &format!("- {}\n", code(&actlog_cut(e)));
        }
    }
    for (j, s) in failed {
        *md += &format!("\n**{}**", where_(j, s));
        if s.continued {
            *md += " (continue-on-error: its job went on)";
        }
        *md += "\n";
        let mut items = Vec::new();
        for c in s.failed_cases() {
            let mut item = code(&c.name);
            if let Some(at) = &c.at {
                item += &format!(" at {at}");
            }
            if let Some(m) = &c.message {
                item += &format!(": {}", code(&first_line(m)));
            }
            items.push(item);
        }
        if items.is_empty() {
            for a in s.annotations.iter().filter(|a| a.level == "error") {
                let mut item = a.title.as_deref().map(code).unwrap_or_default();
                let place = [
                    a.file.clone(),
                    a.line.map(|l| l.to_string()),
                    a.col.map(|c| c.to_string()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(":");
                if !place.is_empty() {
                    item += &format!("{}at {place}", if item.is_empty() { "" } else { " " });
                }
                if !item.is_empty() {
                    item += ": ";
                }
                item += &code(&first_line(&a.message));
                items.push(item);
            }
        }
        if items.is_empty() {
            for c in s.tests.iter().filter(|c| c.failed > 0) {
                let what = match (c.tool.as_str(), c.failed) {
                    ("go", 1) => " package",
                    ("go", _) => " packages",
                    _ => "",
                };
                items.push(format!("{}: {}{what} failed", c.tool, c.failed));
            }
        }
        for t in &s.reruns {
            items.push(format!("Rerun: {}", code(&format!("cargo test {t}"))));
        }
        if s.incomplete() {
            items.push(
                "Incomplete: cargo stopped at the first failing test binary (add `--no-fail-fast`)."
                    .into(),
            );
        }
        if !items.is_empty() {
            *md += "\n";
            for i in items {
                *md += &format!("- {i}\n");
            }
        }
        let tail = &s.tail[s.tail.len().saturating_sub(TAIL)..];
        let text = tail.join("\n");
        let text = text.trim_matches('\n');
        if !text.trim().is_empty() {
            let fence = "`".repeat(3.max(longest_run(text, '`') + 1));
            *md += &format!("\n{fence}\n{text}\n{fence}\n");
        }
    }
}

/// `## Not the project's`: bana's and act's failures, which the checks leave out.
fn not_ours(v: &View, md: &mut String) {
    let mut items = Vec::new();
    for (_, _, j, s) in v.steps() {
        if !s.failed() || v.check(s).is_some() || (j.key.is_empty() && s.stage.is_empty()) {
            continue;
        }
        let who = match (s.owner, s.stage.as_str()) {
            (Owner::Bana, _) => "bana",
            (_, "") => "act",
            _ => "a Pre or Post stage",
        };
        let error =
            v.r.errors
                .iter()
                .find(|e| e.key.as_deref() == Some(&j.key) && e.step.as_deref() == Some(&s.name));
        let why = match error {
            Some(e) => code(&actlog_cut(&e.text)),
            None => "failed".into(),
        };
        items.push(format!("{who}: {}: {why}", where_(j, s)));
    }
    for e in v.r.errors.iter().filter(|e| e.key.is_none()) {
        let who = match e.owner {
            Owner::Bana => "bana",
            Owner::Act => "act",
            Owner::Project => continue,
        };
        items.push(format!("{who}: {}", code(&actlog_cut(&e.text))));
    }
    if items.is_empty() {
        return;
    }
    *md += "\n## Not the project's\n\n";
    for i in items {
        *md += &format!("- {i}\n");
    }
}

/// `## Artifacts`: what the jobs uploaded, and the files each gave the
/// build's dist/, or why it gave none.
fn artifacts(v: &View, md: &mut String) {
    if v.r.artifacts.is_empty() {
        return;
    }
    *md += "\n## Artifacts\n\n";
    for a in &v.r.artifacts {
        let from = [a.key.clone(), (a.bytes > 0).then(|| size(a.bytes))]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        let from = if from.is_empty() {
            String::new()
        } else {
            format!(" ({from})")
        };
        let what = match &a.problem {
            Some(p) => first_line(p),
            None => a
                .files
                .iter()
                .map(|f| code(f))
                .collect::<Vec<_>>()
                .join(", "),
        };
        *md += &format!("- {}{from}: {what}\n", code(&a.name));
    }
}

/// `830 B`, `1.2 KB`, `14.0 MB`.
fn size(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n if n < 1024 * 1024 * 1024 => format!("{:.1} MB", n as f64 / 1048576.0),
        n => format!("{:.1} GB", n as f64 / 1073741824.0),
    }
}

fn actlog_cut(s: &str) -> String {
    crate::actlog::cut(&first_line(&tidy(s)), TEXT_MAX)
}

/// act's error once: it wraps it again at each stage it unwinds (`Error
/// occurred running finally: ` twice and more, ` (original error: <nil>)`
/// as often).
fn tidy(s: &str) -> String {
    const AGAIN: &str = "Error occurred running finally: ";
    const NONE: &str = " (original error: <nil>)";
    let mut t = s.trim();
    while t.starts_with(AGAIN) && t[AGAIN.len()..].starts_with(AGAIN) {
        t = &t[AGAIN.len()..];
    }
    while let Some(rest) = t.strip_suffix(NONE) {
        t = rest;
    }
    t.to_string()
}

/// `rust › cargo test`, or the log itself for output outside any job.
fn where_(j: &Job, s: &Step) -> String {
    match (j.key.is_empty(), s.name.is_empty()) {
        (true, true) => "the log".into(),
        (true, false) => s.name.clone(),
        (false, true) => j.key.clone(),
        (false, false) => format!("{} › {}", j.key, s.name),
    }
}

fn result_word(r: &str) -> &str {
    match r {
        "success" => "passed",
        "failure" => "failed",
        "unknown" => "no result",
        r => r,
    }
}

fn short_ref(r: &str) -> String {
    r.strip_prefix("refs/heads/")
        .or_else(|| r.strip_prefix("refs/tags/"))
        .unwrap_or(r)
        .to_string()
}

/// `12m 40s`, `45s`, `1h 3m`.
fn duration(s: i64) -> String {
    match s {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

/// A share, rounded down (a failure keeps it under 100%), and never 0% when
/// something passed.
fn percent(passed: u64, of: u64) -> String {
    let p = passed * 100 / of;
    if p == 0 && passed > 0 {
        "<1%".into()
    } else {
        format!("{p}%")
    }
}

fn checks_cell(c: &Checks) -> String {
    let n = c.passed + c.failed;
    if n == 0 {
        return "—".into();
    }
    format!("{} ({}/{n})", percent(c.passed, n), c.passed)
}

fn tests_cell(t: &Tests) -> String {
    let n = t.passed + t.failed;
    let p = &t.packages;
    let packages = p.passed + p.failed;
    let mut s = match (n, t.incomplete) {
        (0, false) if packages > 0 => String::new(),
        (0, false) => "—".to_string(),
        (0, true) => "— (incomplete)".to_string(),
        (n, true) => format!("{} of {n} run (incomplete)", percent(t.passed, n)),
        (n, false) => format!("{} ({}/{n})", percent(t.passed, n), t.passed),
    };
    if t.skipped > 0 {
        s += &format!(", {} skipped", t.skipped);
    }
    if packages > 0 {
        if !s.is_empty() {
            s += ", ";
        }
        s += &format!(
            "{} ({}/{packages} go packages)",
            percent(p.passed, packages),
            p.passed
        );
    }
    s
}

/// The history's chip for a build's rows: its tests, as the `all` row
/// counts them (`tests 95%`); none when nothing was counted.
pub fn chip(rows: &[Row]) -> Option<String> {
    let t = &rows.last()?.tests;
    let n = t.passed + t.failed;
    (n > 0).then(|| {
        let more = if t.incomplete { " (incomplete)" } else { "" };
        format!("tests {}{more}", percent(t.passed, n))
    })
}

/// Text for a table cell.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Inline code that holds any backticks in `s`.
fn code(s: &str) -> String {
    let n = longest_run(s, '`');
    let ticks = "`".repeat(n + 1);
    let pad = if s.starts_with('`') || s.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{s}{pad}{ticks}")
}

fn longest_run(s: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for x in s.chars() {
        run = if x == c { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

/// A message's first line, cut; `…` says there is more.
fn first_line(s: &str) -> String {
    let mut lines = s.trim().lines();
    let first = lines.next().unwrap_or("").trim_end();
    let more = lines.next().is_some();
    let cut = crate::actlog::cut(first, TEXT_MAX);
    if more && !cut.ends_with('…') {
        format!("{cut} …")
    } else {
        cut
    }
}

// ---- where results come from ------------------------------------------------

/// A daemon build's results, from its directory (`builds/<id>`): its
/// results.jsonl, else act.jsonl with jobs.txt and build.json.
pub fn read_build(dir: &Path) -> Result<Results, String> {
    if let Ok(text) = std::fs::read_to_string(dir.join("results.jsonl")) {
        return Ok(Results::from_jsonl(&text));
    }
    fold_build(dir)
}

/// A daemon build's results from what act printed (act.jsonl), the jobs
/// `act -l` listed (jobs.txt) and its build.json.
fn fold_build(dir: &Path) -> Result<Results, String> {
    let log = std::fs::read(dir.join("act.jsonl")).unwrap_or_default();
    let list = std::fs::read_to_string(dir.join("jobs.txt")).unwrap_or_default();
    let mut r = crate::results::fold_json_listed(
        &String::from_utf8_lossy(&log),
        &crate::actlog::parse_list(&list),
    );
    with_record(dir, &mut r)?;
    Ok(r)
}

/// What the daemon's build.json knows of the build, into `r`.
fn with_record(dir: &Path, r: &mut Results) -> Result<(), String> {
    let bytes = std::fs::read(dir.join("build.json"))
        .map_err(|_| format!("no build in {}", dir.display()))?;
    let rec: crate::daemon::Record = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{}: {e}", dir.join("build.json").display()))?;
    let (req, b) = (&rec.request, &mut r.build);
    b.git_ref = Some(req.git_ref.clone()).filter(|r| !r.is_empty());
    b.sha = Some(req.sha.clone()).filter(|s| !s.is_empty());
    if !req.tier.is_empty() {
        b.tier = Some(req.tier.clone());
    }
    b.trigger = Some(req.trigger.as_str().into());
    b.started = rec.build.started_at.or(b.started);
    b.ended = rec.build.ended_at.or(b.ended);
    // A build stopped, or in error outside the jobs.
    if rec.build.state == crate::actlog::BuildState::Error && b.result != "failure" {
        b.result = "error".into();
    }
    Ok(())
}

/// At a daemon build's end: its results.jsonl and report.md, in its
/// directory, with the built commit's bana.conf (`conf`). The results are
/// the results.jsonl a builder other than act wrote there, else act.jsonl's;
/// build.json has the last word on the build's ref, commit, tier and times.
/// `fill` adds what neither says (the repo, the machine, act's version,
/// bana's commit). Gives the report's rows, for build.json.
pub fn write_build(
    dir: &Path,
    id: u64,
    conf: &str,
    fill: impl FnOnce(&mut crate::results::BuildInfo),
) -> Result<Vec<Row>, String> {
    let mut r = match std::fs::read_to_string(dir.join("results.jsonl")) {
        Ok(text) => {
            let mut r = Results::from_jsonl(&text);
            with_record(dir, &mut r)?;
            r
        }
        Err(_) => fold_build(dir)?,
    };
    fill(&mut r.build);
    let conf = read_conf(conf);
    let rep = report(
        &r,
        &conf,
        &Meta {
            build: Some(id),
            note: None,
        },
    );
    let jsonl = r.to_jsonl_with(|notice| conf.leaves_out(notice));
    for (name, text) in [("results.jsonl", &jsonl), ("report.md", &rep.markdown)] {
        let (path, part) = (dir.join(name), dir.join(format!("{name}.part")));
        std::fs::write(&part, text)
            .and_then(|_| std::fs::rename(&part, &path))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(rep.standards)
}

/// The report a daemon build's end wrote: report.md, with the rows its
/// build.json keeps; none before then.
pub fn read_written(dir: &Path) -> Option<Report> {
    let markdown = std::fs::read_to_string(dir.join("report.md")).ok()?;
    let rec: serde_json::Value = std::fs::read(dir.join("build.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let standards = serde_json::from_value(rec["standards"].clone()).unwrap_or_default();
    Some(Report {
        markdown,
        standards,
    })
}

/// A hand run's last.env (`KEY=VALUE` lines, as bana ci writes them) into its
/// results; a word on how it ended, when it was stopped.
pub fn hand_run(r: &mut Results, env: &str) -> Option<String> {
    let get = |k: &str| {
        env.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(key, _)| key.trim() == k)
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let b = &mut r.build;
    b.sha = get("sha").or(b.sha.take());
    b.git_ref = get("ref").or(b.git_ref.take());
    b.tier = get("tier").or(b.tier.take());
    b.network = get("network").or(b.network.take());
    if let Some(v) = get("act") {
        b.builder = format!("act {v}");
    }
    b.bana = get("bana").or(b.bana.take());
    b.trigger = Some("hand".into());
    b.started = get("started").and_then(|t| t.parse().ok()).or(b.started);
    b.ended = get("ended").and_then(|t| t.parse().ok()).or(b.ended);
    (get("stopped").as_deref() == Some("1")).then(|| "stopped with Ctrl-C".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::{fold_json, fold_json_listed, fold_text};

    /// A golden file: `BANA_BLESS=1 cargo test` writes it anew.
    fn golden(name: &str, got: &str) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/report")
            .join(name);
        if std::env::var_os("BANA_BLESS").is_some() {
            std::fs::write(&path, got).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            got == want,
            "{name} differs (BANA_BLESS=1 writes it):\n{got}"
        );
    }

    fn row<'a>(rep: &'a Report, name: &str) -> &'a Row {
        rep.standards
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no standard {name}"))
    }

    fn line_of<'a>(md: &'a str, start: &str) -> &'a str {
        md.lines()
            .find(|l| l.starts_with(start))
            .unwrap_or_else(|| panic!("no line {start}:\n{md}"))
    }

    const PASTE: &str = include_str!("../tests/fixtures/results/example-paste.txt");
    const RUN1: &str = include_str!("../tests/fixtures/results/act-run1.jsonl");
    const EXAMPLE: &str = include_str!("../tests/fixtures/report/example.jsonl");
    const EXAMPLE_CONF: &str = include_str!("../tests/fixtures/report/example.conf");

    #[test]
    fn globs() {
        for (p, t) in [
            ("*", ""),
            ("*", "anything"),
            ("rust", "rust"),
            ("package (*)", "package (linux-x64)"),
            ("real_*", "real_c3_the_engine"),
            ("*left out*", "macOS: left out here"),
            ("a*b*c", "a-b-b-c"),
            ("?", "é"),
            ("a?c", "abc"),
            ("**x", "x"),
            ("*.rs", "a.b.rs"),
            ("[x]", "[x]"),
        ] {
            assert!(glob(p, t), "{p} matches {t}");
        }
        for (p, t) in [
            ("", "x"),
            ("rust", "rust-2"),
            ("rust", "Rust"),
            ("package (*)", "package"),
            ("?", ""),
            ("a*c", "abcd"),
            ("*.rs", "a.rsx"),
            ("[x]", "x"),
        ] {
            assert!(!glob(p, t), "{p} does not match {t}");
        }
        assert!(glob("", ""));
        // test: globs: the whole name, or the test's own.
        assert!(test_glob("fact_*", "tests::fact_answer"));
        assert!(test_glob("tests::*", "tests::fact_answer"));
        assert!(test_glob("real_*", "real_c3"));
        assert!(!test_glob("fact_*", "fact::other"));
        assert!(!test_glob("tests", "tests::fact_answer"));
    }

    #[test]
    fn standards_as_bana_conf_has_them() {
        let conf = read_conf(
            "# report.no = x\n  # report.no2 = y\nreport.rust = rust \"package (*)\"\n\
             report.web=web/pnpm*,e2e/*\r\nrepo = a/b\nreport.engine = test:real_*\n\
             report.rust = rust\nreport. = x\nreport.empty =\nreport.left_out = \"*left out*\" *Not a*\n",
        );
        assert_eq!(
            conf.standards,
            [
                Standard {
                    name: "rust".into(),
                    patterns: vec![Pattern::Job("rust".into())],
                },
                Standard {
                    name: "web".into(),
                    patterns: vec![
                        Pattern::Step("web".into(), "pnpm*".into()),
                        Pattern::Step("e2e".into(), "*".into())
                    ],
                },
                Standard {
                    name: "engine".into(),
                    patterns: vec![Pattern::Test("real_*".into())],
                },
            ],
            "file order, the last value, no comments, no empty ones"
        );
        assert_eq!(conf.left_out, ["*left out*", "*Not", "a*"]);
        assert_eq!(words(r#" a  "b c",d "" "e"f "#), ["a", "b c", "d", "ef"]);
        assert_eq!(read_conf(""), Conf::default());
    }

    #[test]
    fn the_owners_log() {
        let mut r = fold_text(PASTE);
        r.build.repo = Some("tjrb-xyz/example".into());
        r.build.trigger = Some("paste".into());
        let rep = report(&r, &Conf::default(), &Meta::default());
        assert_eq!(
            line_of(&rep.markdown, "| rust |"),
            "| rust | 0% (0/1) | 95% of 22 run (incomplete) | |"
        );
        let rust = row(&rep, "rust");
        assert_eq!(
            (rust.checks, rust.tests),
            (
                Checks {
                    passed: 0,
                    failed: 1
                },
                Tests {
                    passed: 21,
                    failed: 1,
                    skipped: 0,
                    incomplete: true,
                    packages: Checks::default(),
                }
            )
        );
        golden("paste.md", &rep.markdown);
    }

    #[test]
    fn one_standard_per_job_by_default() {
        let r = fold_json(RUN1);
        let rep = report(&r, &Conf::default(), &Meta::default());
        let names: Vec<&str> = rep.standards.iter().map(|r| r.name.as_str()).collect();
        let keys: Vec<&str> = r.jobs.iter().map(|j| j.key.as_str()).collect();
        assert_eq!(names[..names.len() - 1], keys[..]);
        assert_eq!(names.last(), Some(&"all"));
        golden("default.md", &rep.markdown);
    }

    #[test]
    fn jobs_not_run_here() {
        let (jsonl, list) = (
            include_str!("../tests/fixtures/act/skip.jsonl"),
            include_str!("../tests/fixtures/act/skip.list"),
        );
        let mut r = fold_json_listed(jsonl, &crate::actlog::parse_list(list));
        r.build.tier = Some("quick".into());
        let rep = report(&r, &Conf::default(), &Meta::default());
        assert_eq!(row(&rep, "web").not_run, ["web (not planned at quick)"]);
        assert_eq!(row(&rep, "nightly").not_run, ["nightly (skipped)"]);
        assert_eq!(
            line_of(&rep.markdown, "| web |"),
            "| web | — | — | web (not planned at quick) |"
        );
        assert_eq!(row(&rep, "all").not_run.len(), 2);
        golden("not-run.md", &rep.markdown);
    }

    #[test]
    fn a_job_that_needs_a_systemd_job_says_why_it_did_not_run() {
        // tests/fixtures/act/systemd: sd ran next, in its systemd container;
        // after, which needs it, did not run (act runs a job with its needs).
        let (jsonl, list) = (
            include_str!("../tests/fixtures/act/systemd.jsonl"),
            include_str!("../tests/fixtures/act/systemd.list"),
        );
        let r = fold_json_listed(jsonl, &crate::actlog::parse_list(list));
        let rep = report(&r, &Conf::default(), &Meta::default());
        assert_eq!(checks_cell(&row(&rep, "sd").checks), "100% (1/1)");
        assert!(row(&rep, "sd").not_run.is_empty());
        assert_eq!(
            row(&rep, "after").not_run,
            ["after (needs sd, a systemd job)"]
        );
        assert!(
            rep.markdown
                .contains("\n## Not run here\n\n- after: needs sd, a systemd job\n"),
            "{}",
            rep.markdown
        );
    }

    #[test]
    fn the_example_with_its_standards() {
        // results.jsonl as a builder writes it: the example's jobs by hand.
        let r = Results::from_jsonl(EXAMPLE);
        let conf = read_conf(EXAMPLE_CONF);
        let rep = report(
            &r,
            &conf,
            &Meta {
                build: Some(42),
                note: None,
            },
        );
        let cells = |name: &str| {
            let r = row(&rep, name);
            (checks_cell(&r.checks), tests_cell(&r.tests))
        };
        assert_eq!(cells("toolchain"), ("100% (2/2)".into(), "—".into()));
        assert_eq!(
            cells("rust"),
            ("0% (0/1)".into(), "95% of 22 run (incomplete)".into())
        );
        assert_eq!(
            cells("engine"),
            ("—".into(), "90% of 10 run (incomplete)".into()),
            "by name: from the step cargo stopped in"
        );
        assert_eq!(
            cells("web"),
            ("100% (2/2)".into(), "100% (32/32), 2 skipped".into())
        );
        let macos = row(&rep, "macos");
        assert_eq!(
            (checks_cell(&macos.checks), &macos.not_run[..]),
            (
                "100% (2/2)".into(),
                &["macos › On a dedicated CI Mac only (left out)".to_string()][..]
            ),
            "a left-out step is no pass"
        );
        assert_eq!(
            row(&rep, "packaging").not_run,
            [
                "package (linux-arm64) (not planned at quick)",
                "package (macos-arm64) (no platform for it here)"
            ]
        );
        assert_eq!(
            cells("all"),
            (
                "90% (10/11)".into(),
                "98% of 72 run (incomplete), 2 skipped".into()
            )
        );
        let md = &rep.markdown;
        assert!(md.contains(
            "- bana: macos › Post tjrb-xyz/bana/actions/keep-builds@a4b6f87: `symlink log-only"
        ));
        assert!(md.contains("\n**web › pnpm test**\n\n## Vitest Test Report\n"));
        golden("example.md", md);
        // results.jsonl as the daemon writes it: left-out notices say so.
        let jsonl = r.to_jsonl_with(|n| conf.leaves_out(n));
        let notices: Vec<serde_json::Value> = jsonl
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["kind"] == "notice")
            .collect();
        assert!(notices.iter().any(|v| v["left_out"] == true), "{jsonl}");
        let back = Results::from_jsonl(&jsonl);
        assert_eq!(back.to_jsonl(), jsonl, "results.jsonl's left_out stays");
        // A builder's own left_out leaves its step out, with no report.left_out.
        let rep = report(&back, &Conf::default(), &Meta::default());
        assert_eq!(
            row(&rep, "macos").not_run,
            ["macos › On a dedicated CI Mac only (left out)"]
        );

        // The JSON the MCP gives: the same rows.
        let v = serde_json::to_value(&rep).unwrap();
        assert_eq!(v["standards"][1]["tests"]["incomplete"], true);
        assert_eq!(
            v["standards"].as_array().unwrap().len(),
            conf.standards.len() + 1
        );
    }

    #[test]
    fn the_examples_bana_conf_has_the_owners_standards() {
        let conf = read_conf(include_str!("../../examples/example/bana.conf"));
        let names: Vec<&str> = conf.standards.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "toolchain",
                "rust",
                "engine",
                "web",
                "macos",
                "streaming",
                "sdk",
                "linux_service",
                "packaging"
            ]
        );
        assert_eq!(conf.standards[2].patterns, [Pattern::Test("real_*".into())]);
        assert!(conf.leaves_out("macOS: Not a dedicated CI Mac, so the driver is left out"));
        // Its standards on the example's results: every row there, all last.
        let rep = report(&Results::from_jsonl(EXAMPLE), &conf, &Meta::default());
        assert_eq!(rep.standards.len(), names.len() + 1);
        assert_eq!(
            line_of(&rep.markdown, "| engine |"),
            "| engine | — | 90% of 10 run (incomplete) | |"
        );
    }

    #[test]
    fn a_matrix_entry_that_did_not_start() {
        // Real act 0.2.89 (bana ci -- --json): lint (x64)'s image pull got 429.
        let r = fold_text(include_str!("../tests/fixtures/report/setup-failed.txt"));
        let rep = report(&r, &read_conf("report.lint = lint\n"), &Meta::default());
        assert_eq!(
            line_of(&rep.markdown, "| lint |"),
            "| lint | 100% (2/2) | — | lint (x64) (did not start) |"
        );
        assert!(rep
            .markdown
            .contains("- act: lint (x64) › Set up job: failed\n"));
        assert!(rep.markdown.contains("\n- lint (x64): did not start\n"));
        // The same in plain text: act never named that entry's matrix.
        let r = fold_text(include_str!(
            "../tests/fixtures/report/setup-failed-text.txt"
        ));
        let rep = report(&r, &read_conf("report.lint = lint\n"), &Meta::default());
        assert_eq!(
            line_of(&rep.markdown, "| lint |"),
            "| lint | 100% (2/2) | — | lint-2 (did not start) |"
        );
    }

    #[test]
    fn a_log_that_names_some_of_its_tests() {
        // The end of cargo's output: one test line, and three counted.
        let r = fold_text(
            "test tests::fact_a ... ok\n\n\
             test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
        );
        let rep = report(
            &r,
            &read_conf("report.facts = test:fact_*\n"),
            &Meta::default(),
        );
        assert_eq!(
            line_of(&rep.markdown, "| facts |"),
            "| facts | — | 100% of 1 run (incomplete) | |"
        );
    }

    #[test]
    fn nextest_names_no_skipped_test() {
        // Its Summary counts 1 skipped, and no line names it: both facts ran.
        let r = fold_text(include_str!("../tests/fixtures/results/nextest.txt"));
        let rep = report(
            &r,
            &read_conf("report.facts = test:fact_*\n"),
            &Meta::default(),
        );
        assert_eq!(
            line_of(&rep.markdown, "| facts |"),
            "| facts | — | 100% (2/2) | |"
        );
    }

    #[test]
    fn go_counts_packages_apart() {
        let mut r = fold_text(include_str!("../tests/fixtures/results/go-test.txt"));
        let rep = report(&r, &read_conf("report.g = *\n"), &Meta::default());
        assert_eq!(
            line_of(&rep.markdown, "| g |"),
            "| g | — | 50% (1/2 go packages) | |"
        );
        assert!(
            rep.markdown.contains("- go: 1 package failed\n"),
            "{}",
            rep.markdown
        );
        // With other tools' tests: apart from them, and from the chip.
        r.jobs[0].steps[0].tests.push(Count {
            tool: "cargo".into(),
            passed: 4,
            ..Count::default()
        });
        let rep = report(&r, &Conf::default(), &Meta::default());
        assert_eq!(
            tests_cell(&row(&rep, "all").tests),
            "100% (4/4), 50% (1/2 go packages)"
        );
        assert_eq!(chip(&rep.standards).as_deref(), Some("tests 100%"));
    }

    #[test]
    fn a_job_that_did_not_finish() {
        // Its log cut in a test step: those tests are not all it had.
        let log = "[ci/t] ⭐ Run Set up job\n[ci/t]   ✅  Success - Set up job\n\
                   [ci/t] ⭐ Run Main cargo test\n[ci/t]   | running 2 tests\n\
                   [ci/t]   | test fact_a ... ok\n[ci/t]   | test fact_b ... ok\n\
                   [ci/t]   | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let r = fold_text(log);
        let rep = report(
            &r,
            &read_conf("report.t = t\nreport.facts = test:fact_*\n"),
            &Meta::default(),
        );
        assert_eq!(
            line_of(&rep.markdown, "| t |"),
            "| t | — | 100% of 2 run (incomplete) | t (cancelled) |"
        );
        assert_eq!(
            line_of(&rep.markdown, "| facts |"),
            "| facts | — | 100% of 2 run (incomplete) | |"
        );
        assert_eq!(
            chip(&rep.standards).as_deref(),
            Some("tests 100% (incomplete)")
        );
    }

    #[test]
    fn a_matrix_key_with_a_slash() {
        let conf = read_conf(
            "report.lts = \"test (lts/*)\" \"test (lts/*)/npm test\" \"web/pnpm test\"\n",
        );
        assert_eq!(
            conf.standards[0].patterns,
            [
                Pattern::Job("test (lts/*)".into()),
                Pattern::Step("test (lts/*)".into(), "npm test".into()),
                Pattern::Step("web".into(), "pnpm test".into()),
            ]
        );
        assert!(glob("test (lts/*)", "test (lts/iron)"));
    }

    #[test]
    fn a_workflow_act_could_not_read() {
        let r = fold_text(
            "time=\"2026-09-30T13:28:24Z\" level=info msg=\"Using docker host\"\n\
             Error: workflow is not valid. 'ci.yml': yaml: line 38: mapping values are not allowed in this context\n",
        );
        let md = report(&r, &Conf::default(), &Meta::default()).markdown;
        assert!(
            md.contains("## Failures\n\n**the workflow**\n\n- `workflow is not valid. 'ci.yml': yaml: line 38:"),
            "the project's own, not act's:\n{md}"
        );
        assert!(!md.contains("Not the project's"), "{md}");
    }

    #[test]
    fn artifacts_with_their_files_or_why_not() {
        let mut r = fold_text(PASTE);
        r.artifacts = vec![
            crate::results::Artifact {
                name: "demo-nightly-linux-x64".into(),
                key: Some("package (linux-x64)".into()),
                bytes: 1160,
                files: vec!["demo-linux-x64.tar.gz".into(), "demo_1.0.deb".into()],
                ..Default::default()
            },
            crate::results::Artifact {
                name: "old-style".into(),
                problem: Some("an upload-artifact@v3 layout (no zip): not collected".into()),
                ..Default::default()
            },
        ];
        let md = report(&r, &Conf::default(), &Meta::default()).markdown;
        assert!(
            md.contains(
                "\n## Artifacts\n\n\
                 - `demo-nightly-linux-x64` (package (linux-x64), 1.1 KB): `demo-linux-x64.tar.gz`, `demo_1.0.deb`\n\
                 - `old-style`: an upload-artifact@v3 layout (no zip): not collected\n"
            ),
            "{md}"
        );
        assert_eq!(
            (size(830), size(14 << 20)),
            ("830 B".into(), "14.0 MB".into())
        );
        r.artifacts.clear();
        let md = report(&r, &Conf::default(), &Meta::default()).markdown;
        assert!(!md.contains("## Artifacts"), "{md}");
    }

    #[test]
    fn a_hand_runs_env() {
        let mut r = fold_text(PASTE);
        let note = hand_run(
            &mut r,
            "sha=d4b5174aa0f1d2c3b4a5968778695a4b3c2d1e0f\nref=refs/heads/speaker-check\ntier=quick\n\
             network=host\nact=0.2.89\nbana=a4b6f87212d190304c530041b9bbd5fed72f0dd3\n\
             started=1790000000\nended=1790000760\nexit=1\nstopped=1\n",
        );
        r.build.machine = Some("mbp".into());
        r.build.repo = Some("tjrb-xyz/example".into());
        let rep = report(&r, &Conf::default(), &Meta { build: None, note });
        let mut lines = rep.markdown.lines();
        assert_eq!(
            lines.next(),
            Some("# CI report: tjrb-xyz/example · speaker-check d4b5174 · quick · failed")
        );
        assert_eq!(lines.nth(1), Some("bana ci on mbp · act 0.2.89 · network host · bana a4b6f87 · 12m 40s · stopped with Ctrl-C"));
    }

    #[test]
    fn cells_say_what_they_counted() {
        let t = |passed, failed, skipped, incomplete| {
            tests_cell(&Tests {
                passed,
                failed,
                skipped,
                incomplete,
                ..Tests::default()
            })
        };
        assert_eq!(t(0, 0, 0, false), "—");
        assert_eq!(t(0, 0, 3, false), "—, 3 skipped");
        assert_eq!(t(0, 0, 0, true), "— (incomplete)");
        assert_eq!(
            t(999, 1, 0, false),
            "99% (999/1000)",
            "a failure is never 100%"
        );
        assert_eq!(t(1, 999, 0, false), "<1% (1/1000)", "a pass is never 0%");
        assert_eq!(t(0, 4, 0, false), "0% (0/4)");
        assert_eq!(t(21, 1, 1, true), "95% of 22 run (incomplete), 1 skipped");
        assert_eq!(checks_cell(&Checks::default()), "—");
        assert_eq!(code("a`b"), "``a`b``");
        assert_eq!(code("`a"), "`` `a ``");
        assert_eq!(cell("a|b\nc"), "a\\|b c");
        assert_eq!(duration(760), "12m 40s");
        assert_eq!(duration(3720), "1h 2m");
        assert_eq!(first_line("one\ntwo"), "one …");
        assert_eq!(
            tidy("Error occurred running finally: Error occurred running finally: a: file exists (original error: <nil>) (original error: <nil>)"),
            "Error occurred running finally: a: file exists"
        );
        assert_eq!(tidy("a (original error: b)"), "a (original error: b)");
        // The history's chip: the all row's tests.
        let rows = |passed, failed, incomplete| {
            vec![Row {
                tests: Tests {
                    passed,
                    failed,
                    skipped: 2,
                    incomplete,
                    ..Tests::default()
                },
                ..Row::default()
            }]
        };
        assert_eq!(
            chip(&rows(21, 1, true)).as_deref(),
            Some("tests 95% (incomplete)")
        );
        assert_eq!(chip(&rows(4, 0, false)).as_deref(), Some("tests 100%"));
        assert_eq!(chip(&rows(0, 0, false)), None);
        assert_eq!(chip(&[]), None);
    }
}

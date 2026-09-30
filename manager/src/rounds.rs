//! A fix's rounds: its failed jobs run again by the daemon, at the failing
//! commit (round 0, the recheck) or at a snapshot of its worktree (run_jobs).
//!
//! Pure, like [`crate::watch`], but for [`load`] and [`save`]. The daemon owns
//! `fix/<sha7>.d/rounds.json` ([`Rounds`]) and writes it whole; the Stop gate
//! and the MCP only read it. A round is one build per job
//! ([`crate::watch::Trigger::Fix`]); a build retried after a restart counts by
//! its last attempt ([`state_of`]).
//!
//! The limits ([`Rounds::ask`]):
//! - `fix.rounds` rounds after round 0 ([`Rounds::limit`]; More rounds on the
//!   fix card raises it);
//! - one round at a time;
//! - a tree equal to the last finished round's gets that round back, unless
//!   `repeat`, if that round passed or failed with the jobs asked for;
//! - none while the daemon is paused.

use crate::actlog::BuildState;
use crate::results::Results;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// rounds.json's version.
const VERSION: u32 = 1;

/// rounds.json.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rounds {
    pub version: u32,
    /// Rounds allowed after round 0.
    pub limit: u32,
    /// In the order asked for: round 0 first, when there is one.
    pub rounds: Vec<Round>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Round {
    pub n: u32,
    /// What it built: the failing commit (round 0), or a snapshot of the
    /// worktree (`refs/bana/fix/<sha7>/<snap7>` in the daemon's clone).
    pub sha: String,
    /// Its tree: the gate and commit_fix compare the worktree's with it.
    pub tree: String,
    /// The jobs it runs (`bana ci -j`), one build each.
    pub jobs: Vec<String>,
    /// Its builds, in the order queued: a retry adds one for its job.
    pub builds: Vec<RoundBuild>,
    /// Queued, running, or how it ended ([`state_of`]).
    pub state: BuildState,
    /// Asked for again on a tree an earlier round ran (a flaky check).
    pub repeat: bool,
    /// Unix seconds.
    pub queued_at: i64,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoundBuild {
    pub id: u64,
    pub job: String,
    pub state: BuildState,
}

/// What a round asked for gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// A new round, numbered so.
    New(u32),
    /// This finished round ran the same tree: its result stands.
    Same(u32),
}

/// Why a round is refused; `running` names the round that runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub why: String,
    pub running: Option<u32>,
}

impl Rounds {
    pub fn new(limit: u32) -> Self {
        Self {
            version: VERSION,
            limit,
            rounds: Vec::new(),
        }
    }

    /// Rounds run or running, round 0 not counted.
    pub fn used(&self) -> u32 {
        self.rounds.iter().filter(|r| r.n > 0).count() as u32
    }

    pub fn left(&self) -> u32 {
        self.limit.saturating_sub(self.used())
    }

    pub fn get(&self, n: u32) -> Option<&Round> {
        self.rounds.iter().find(|r| r.n == n)
    }

    pub fn get_mut(&mut self, n: u32) -> Option<&mut Round> {
        self.rounds.iter_mut().find(|r| r.n == n)
    }

    /// The round that runs (or waits to), if any.
    pub fn running(&self) -> Option<&Round> {
        self.rounds.iter().find(|r| !r.state.finished())
    }

    /// Whether a round of `jobs` on `tree` may run now, in the limits' order:
    /// one at a time; the same tree again gets its result ([`Round::answers`]);
    /// not while `paused`; not past the limit.
    pub fn ask(
        &self,
        tree: &str,
        jobs: &[String],
        repeat: bool,
        paused: bool,
    ) -> Result<Ask, Refused> {
        if let Some(r) = self.running() {
            let what = if r.n == 0 {
                "round 0 (the recheck at the failing commit)".to_string()
            } else {
                format!("round {}", r.n)
            };
            return Err(Refused {
                why: format!("{what} still runs: one round at a time, so wait for it"),
                running: Some(r.n),
            });
        }
        if !repeat {
            let last = self.rounds.iter().rev().find(|r| r.state.finished());
            if let Some(r) = last.filter(|r| r.tree == tree && r.answers(jobs)) {
                return Ok(Ask::Same(r.n));
            }
        }
        if paused {
            return Err(Refused {
                why: "the bana daemon is paused: no round runs until the owner resumes it".into(),
                running: None,
            });
        }
        if self.left() == 0 {
            return Err(Refused {
                why: format!(
                    "all {} rounds of this fix are used: stop, and sum up what you found and what you would try next (the owner can give it more rounds)",
                    self.limit
                ),
                running: None,
            });
        }
        Ok(Ask::New(self.rounds.last().map_or(1, |r| r.n + 1).max(1)))
    }
}

impl Round {
    /// Whether it ran each of `jobs`: a round of other jobs says nothing of
    /// them (commit_fix wants the failed ones green).
    pub fn covers(&self, jobs: &[String]) -> bool {
        jobs.iter().all(|j| self.jobs.contains(j))
    }

    /// Whether its result answers a round of `jobs` on its tree: it ran those
    /// jobs (more of them only if all passed), and passed or failed. One that
    /// ended in error (cancelled, timed out, could not start) says nothing.
    pub fn answers(&self, jobs: &[String]) -> bool {
        match self.state {
            BuildState::Success => self.covers(jobs),
            BuildState::Failure => self.covers(jobs) && self.jobs.len() == jobs.len(),
            _ => false,
        }
    }
}

/// A round's state from its builds' (in the order queued): each job counts by
/// its last build. It runs while any does; then it passed if all passed, and
/// failed if a job failed; otherwise it ended in error (cancelled, timed out,
/// interrupted twice, could not start).
pub fn state_of(builds: &[RoundBuild]) -> BuildState {
    let mut last: BTreeMap<&str, BuildState> = BTreeMap::new();
    for b in builds {
        last.insert(&b.job, b.state);
    }
    let states: Vec<BuildState> = last.into_values().collect();
    if states.is_empty() {
        BuildState::Error
    } else if states.contains(&BuildState::Running) {
        BuildState::Running
    } else if states.iter().any(|s| !s.finished()) {
        BuildState::Queued
    } else if states.iter().all(|s| *s == BuildState::Success) {
        BuildState::Success
    } else if states.contains(&BuildState::Failure) {
        BuildState::Failure
    } else {
        BuildState::Error
    }
}

/// What failed in one build of a round, in the brief's shape: each failed step
/// (or failed job with none) with its failing tests, cargo's rerun target and
/// its last lines; and the errors outside the jobs, with whose they are.
pub fn failures(r: &Results) -> (Vec<Value>, Vec<Value>) {
    let mut out: Vec<Value> = r
        .failures()
        .into_iter()
        .map(|(job, s)| {
            json!({
                "job": job.key,
                "id": job.id,
                "step": s.name,
                "owner": s.owner,
                "tests": s.failed_cases().map(|c| json!({"name": c.name, "at": c.at, "message": c.message})).collect::<Vec<_>>(),
                "rerun": s.reruns.first(),
                "incomplete": s.incomplete(),
                "annotations": s.annotations.iter().filter(|a| a.level == "error").collect::<Vec<_>>(),
                "log_tail": s.tail,
            })
        })
        .collect();
    for job in r.jobs.iter().filter(|j| j.result == "failure") {
        if !out.iter().any(|f| f["job"] == job.key.as_str()) {
            out.push(json!({"job": job.key, "id": job.id, "step": null, "owner": "project"}));
        }
    }
    let errors = r
        .errors
        .iter()
        .map(|e| json!({"text": e.text, "owner": e.owner}))
        .collect();
    (out, errors)
}

/// A job id as `bana ci -j` takes it, and a workflow names it.
pub fn valid_job(j: &str) -> bool {
    !j.is_empty()
        && j.len() <= 100
        && !j.starts_with('-')
        && j.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
}

/// `<dir>/fix/<sha7>.d/rounds.json`.
pub fn path(dir: &Path, fix: &str) -> PathBuf {
    dir.join("fix").join(format!("{fix}.d")).join("rounds.json")
}

/// A fix's rounds; none before its first.
pub fn load(path: &Path) -> Result<Option<Rounds>, String> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Whole or not at all (a temporary file, synced, then renamed).
pub fn save(path: &Path, rounds: &Rounds) -> Result<(), String> {
    crate::daemon::write_json(path, rounds).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(n: u32, tree: &str, state: BuildState) -> Round {
        Round {
            n,
            tree: tree.into(),
            jobs: vec!["rust".into()],
            state,
            ..Round::default()
        }
    }

    fn jobs(names: &[&str]) -> Vec<String> {
        names.iter().map(|j| j.to_string()).collect()
    }

    fn build(id: u64, job: &str, state: BuildState) -> RoundBuild {
        RoundBuild {
            id,
            job: job.into(),
            state,
        }
    }

    #[test]
    fn the_limits_in_their_order() {
        use BuildState::*;
        let rust = jobs(&["rust"]);
        let mut rs = Rounds::new(2);
        assert_eq!(
            rs.ask("t0", &rust, false, false),
            Ok(Ask::New(1)),
            "no round 0"
        );
        rs.rounds.push(round(0, "t0", Running));
        let e = rs.ask("t1", &rust, false, false).unwrap_err();
        assert_eq!(e.running, Some(0));
        assert!(e.why.starts_with("round 0 (the recheck"), "{e:?}");
        rs.rounds[0].state = Failure;
        assert_eq!(
            rs.ask("t0", &rust, false, false),
            Ok(Ask::Same(0)),
            "the base again"
        );
        assert_eq!(rs.ask("t0", &rust, true, false), Ok(Ask::New(1)), "repeat");
        assert_eq!(
            rs.ask("t0", &rust, false, true),
            Ok(Ask::Same(0)),
            "even paused"
        );
        assert_eq!(
            rs.ask("t0", &jobs(&["web"]), false, false),
            Ok(Ask::New(1)),
            "other jobs: that round never ran them"
        );
        assert_eq!(
            rs.ask("t0", &jobs(&["rust", "web"]), false, false),
            Ok(Ask::New(1)),
            "nor all of these"
        );
        let e = rs.ask("t1", &rust, false, true).unwrap_err();
        assert!(e.why.contains("paused") && e.running.is_none(), "{e:?}");
        assert_eq!(rs.ask("t1", &rust, false, false), Ok(Ask::New(1)));

        rs.rounds.push(Round {
            jobs: jobs(&["rust", "web"]),
            ..round(1, "t1", Success)
        });
        assert_eq!(
            rs.ask("t1", &rust, false, false),
            Ok(Ask::Same(1)),
            "a green round of more jobs answers for fewer"
        );
        rs.rounds.push(round(2, "t2", Queued));
        assert_eq!((rs.used(), rs.left()), (2, 0));
        assert_eq!(
            rs.ask("t3", &rust, false, false).unwrap_err().running,
            Some(2)
        );
        rs.rounds[2].state = Error;
        assert_eq!(
            rs.ask("t2", &rust, false, false).unwrap_err().why,
            "all 2 rounds of this fix are used: stop, and sum up what you found and what you would try next (the owner can give it more rounds)",
            "a round that ended in error answers nothing; only the last finished round's tree is given back"
        );
        rs.limit += 2;
        assert_eq!(rs.ask("t2", &rust, false, false), Ok(Ask::New(3)));
        assert_eq!(rs.left(), 2);
    }

    #[test]
    fn a_round_counts_each_jobs_last_build() {
        use BuildState::*;
        assert_eq!(state_of(&[]), Error);
        assert_eq!(state_of(&[build(1, "a", Queued)]), Queued);
        assert_eq!(
            state_of(&[build(1, "a", Success), build(2, "b", Running)]),
            Running
        );
        assert_eq!(
            state_of(&[build(1, "a", Success), build(2, "b", Success)]),
            Success
        );
        assert_eq!(
            state_of(&[build(1, "a", Error), build(2, "b", Failure)]),
            Failure
        );
        assert_eq!(
            state_of(&[build(1, "a", Success), build(2, "b", Error)]),
            Error
        );
        // Interrupted, then retried: the retry says how it went.
        let retried = [build(1, "a", Error), build(2, "b", Success)];
        assert_eq!(state_of(&retried), Error);
        let retried = [
            build(1, "a", Error),
            build(2, "b", Success),
            build(3, "a", Queued),
        ];
        assert_eq!(state_of(&retried), Queued);
        let mut retried = retried.to_vec();
        retried[2].state = Success;
        assert_eq!(state_of(&retried), Success);
    }

    #[test]
    fn rounds_json_round_trips_and_job_ids_are_checked() {
        let dir = std::env::temp_dir().join(format!("bana-rounds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = path(&dir, "d4b5174");
        assert!(file.ends_with("fix/d4b5174.d/rounds.json"));
        assert_eq!(load(&file), Ok(None));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut rs = Rounds::new(5);
        rs.rounds.push(Round {
            n: 0,
            sha: "a".repeat(40),
            tree: "b".repeat(40),
            jobs: vec!["rust".into()],
            builds: vec![build(7, "rust", BuildState::Failure)],
            state: BuildState::Failure,
            queued_at: 1_790_600_000,
            ended_at: Some(1_790_600_300),
            ..Round::default()
        });
        save(&file, &rs).unwrap();
        assert_eq!(load(&file), Ok(Some(rs.clone())));
        let v: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(v["rounds"][0]["state"], "failure");
        assert_eq!(
            v["rounds"][0]["builds"][0],
            json!({"id": 7, "job": "rust", "state": "failure"})
        );
        std::fs::write(&file, "{").unwrap();
        assert!(load(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);

        for j in ["rust", "package_linux", "a-b", "B2"] {
            assert!(valid_job(j), "{j}");
        }
        for j in ["", "-j", "a b", "a/b", "x;y", &"a".repeat(101)] {
            assert!(!valid_job(j), "{j}");
        }
    }

    #[test]
    fn failures_in_the_briefs_shape() {
        let log = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/act/fail.jsonl"
        ))
        .unwrap();
        let (failed, _) = failures(&crate::results::fold_json(&log));
        assert!(!failed.is_empty());
        let f = &failed[0];
        assert_eq!(
            (&f["job"], &f["owner"]),
            (&json!("lint"), &json!("project"))
        );
        assert!(f["step"].as_str().unwrap().contains("cargo clippy"), "{f}");
        assert!(f["log_tail"].is_array(), "{f}");
        let (none, errors) = failures(&Results::default());
        assert!(none.is_empty() && errors.is_empty());
    }
}

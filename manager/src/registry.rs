//! The one daemon's projects. A project is added while
//! `~/.bana/<prefix>/daemon/settings` is there (`bana add` writes it, `bana
//! remove` removes it): each is a [`Daemon`], with its own routes
//! ([`crate::server::project_router`]), watcher and poster. One runner runs
//! their builds, one at a time on the machine, each project in turn.
//!
//! The files are the truth. A scan reads them at start, on `POST
//! /ci/v1/projects` (bana add, remove, pause and resume send it) and every
//! minute: it starts a project added, starts again one whose settings
//! changed (or that could not start), stops one removed, and reads each
//! project's `daemon/paused` again.
//!
//! A project that cannot start (no clone, bad settings, its daemon.lock held
//! by an older bana's daemon) is kept as its error, shown by `bana list`, and
//! the others run on.

use crate::daemon::{Daemon, Machine, Settings, MACHINE_DIR};
use axum::Router;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

/// How often the files are read again, without a poke.
const RESCAN: Duration = Duration::from_secs(60);
/// The projects' first fetches at start are this far apart.
const STAGGER: Duration = Duration::from_secs(3);
/// Why a build stops when its project is removed: never run again.
pub const REMOVED: &str = "project removed";

/// One project: its daemon, or why it could not start.
struct Entry {
    daemon: Result<Daemon, String>,
    router: Option<Router>,
    /// Its settings file as the daemon started with it.
    stamp: String,
}

pub struct Registry {
    /// bana's home: `<home>/<prefix>` are the projects.
    home: PathBuf,
    machine: Machine,
    /// Wakes the runner; every project's daemon has it.
    run: Arc<Notify>,
    /// Counts the changes: any project's summary, or the projects.
    changed: watch::Sender<u64>,
    entries: Mutex<BTreeMap<String, Entry>>,
    /// The project whose build ran last: the next turn starts after it.
    last: Mutex<Option<String>>,
    /// One scan at a time.
    scanning: tokio::sync::Mutex<()>,
    stop: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

/// A prefix, as bana.conf takes it: `^[a-z0-9][a-z0-9-]*$`.
pub fn valid_prefix(p: &str) -> bool {
    p.len() <= 40
        && p.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && p.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Registry {
    fn new(home: &Path, machine: Machine) -> Self {
        Self {
            home: home.to_path_buf(),
            machine,
            run: Arc::new(Notify::new()),
            changed: watch::Sender::new(0),
            entries: Mutex::new(BTreeMap::new()),
            last: Mutex::new(None),
            scanning: tokio::sync::Mutex::new(()),
            stop: watch::Sender::new(false),
            tasks: Mutex::new(Vec::new()),
        }
    }

    /// Starts every project added in `home` (each recovers what the last
    /// daemon left), then the runner and the minute's rescan.
    pub async fn start(home: &Path, machine: Machine) -> Arc<Self> {
        let r = Arc::new(Self::new(home, machine));
        r.scan_with(STAGGER).await;
        let tasks = vec![
            tokio::spawn(r.clone().runner()),
            tokio::spawn(r.clone().rescan()),
        ];
        *locked(&r.tasks) = tasks;
        r
    }

    /// The daemons given, each running its own builds (tests).
    #[cfg(test)]
    pub(crate) fn of(daemons: Vec<Daemon>) -> Arc<Self> {
        let s = daemons[0].settings();
        let home = s.dir.parent().unwrap_or(&s.dir).to_path_buf();
        let machine = Machine {
            port: s.port,
            machine: s.machine.clone(),
            tray: false,
            home: s.home.clone(),
            home_set: s.home_set,
            recheck: s.recheck,
        };
        let r = Self::new(&home, machine);
        for d in daemons {
            let name = d.settings().dir.file_name().unwrap_or_default();
            let name = name.to_string_lossy().into_owned();
            let entry = Entry {
                router: Some(crate::server::project_router(d.clone())),
                daemon: Ok(d),
                stamp: String::new(),
            };
            locked(&r.entries).insert(name, entry);
        }
        Arc::new(r)
    }

    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Changes as they come: any project's summary, or the projects.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// The projects' prefixes.
    pub fn prefixes(&self) -> Vec<String> {
        locked(&self.entries).keys().cloned().collect()
    }

    /// The projects that run, in order.
    pub fn daemons(&self) -> Vec<(String, Daemon)> {
        locked(&self.entries)
            .iter()
            .filter_map(|(p, e)| Some((p.clone(), e.daemon.as_ref().ok()?.clone())))
            .collect()
    }

    /// The projects that could not start (or start again now), and why.
    pub fn errors(&self) -> Vec<(String, String)> {
        locked(&self.entries)
            .iter()
            .filter_map(|(p, e)| Some((p.clone(), e.daemon.as_ref().err()?.clone())))
            .collect()
    }

    pub fn daemon(&self, prefix: &str) -> Option<Daemon> {
        locked(&self.entries)
            .get(prefix)?
            .daemon
            .as_ref()
            .ok()
            .cloned()
    }

    /// Project `prefix`'s routes: none for a project not added, its error
    /// for one that could not start.
    pub fn routes(&self, prefix: &str) -> Option<Result<Router, String>> {
        let entries = locked(&self.entries);
        let e = entries.get(prefix)?;
        Some(match (&e.daemon, &e.router) {
            (Ok(_), Some(r)) => Ok(r.clone()),
            (Err(why), _) => Err(why.clone()),
            (Ok(_), None) => Err("starting".into()),
        })
    }

    /// One row per project: what `bana list` and the page's picker show.
    pub fn rows(&self) -> Vec<Value> {
        locked(&self.entries)
            .iter()
            .map(|(prefix, e)| match &e.daemon {
                Ok(d) => {
                    let (s, sum) = (d.settings(), d.published());
                    json!({
                        "prefix": prefix, "repo": s.repo, "checkout": s.checkout,
                        "paused": sum.watcher.paused, "error": null, "queue": sum.queue.len(),
                        "running": sum.running.map(|b| json!({"id": b.id, "ref": b.git_ref})),
                        "last": sum.last.map(|b| json!({"id": b.id, "state": b.state, "ended_at": b.ended_at})),
                    })
                }
                Err(why) => {
                    let dir = self.home.join(prefix);
                    let kv = crate::fix::daemon_settings(&dir);
                    let get = |k: &str| kv.get(k).filter(|v| !v.is_empty()).cloned();
                    json!({
                        "prefix": prefix, "repo": get("repo"), "checkout": get("checkout"),
                        "paused": dir.join("daemon/paused").exists(), "error": why, "queue": 0,
                        "running": null, "last": null,
                    })
                }
            })
            .collect()
    }

    /// Reads the files again: starts a project added, starts again one whose
    /// settings changed or that could not start, stops one removed (its
    /// running build cancelled, not to run again), and reads each pause.
    pub async fn scan(&self) {
        self.scan_with(Duration::ZERO).await
    }

    /// `stagger`: how far apart the new projects' first fetches are.
    async fn scan_with(&self, stagger: Duration) {
        let _one = self.scanning.lock().await;
        let found = self.found();
        let gone: Vec<(String, Entry)> = {
            let mut entries = locked(&self.entries);
            let names: Vec<String> = entries
                .keys()
                .filter(|p| !found.contains_key(*p))
                .cloned()
                .collect();
            names
                .into_iter()
                .filter_map(|p| entries.remove(&p).map(|e| (p, e)))
                .collect()
        };
        for (prefix, e) in gone {
            if let Ok(d) = e.daemon {
                remove(&d).await;
                eprintln!("bana daemon: {prefix} was removed");
            }
        }
        let mut first = Duration::ZERO;
        for (prefix, text) in found {
            let old = {
                let mut entries = locked(&self.entries);
                match entries.get(&prefix) {
                    Some(e) if e.stamp == text && e.daemon.is_ok() => continue,
                    // Still listed while it starts again: its routes say so.
                    Some(_) => entries.insert(prefix.clone(), restarting()),
                    None => None,
                }
            };
            // Its settings changed: as a restart, its build is run again once,
            // by the new daemon only once the old one's build has ended.
            let said = match old {
                Some(Entry { daemon: Ok(d), .. }) => {
                    d.stop_fully().await;
                    None
                }
                Some(Entry {
                    daemon: Err(why), ..
                }) => Some(why),
                None => None,
            };
            let entry = self.open(&prefix, text, first, said.as_deref()).await;
            first += stagger;
            locked(&self.entries).insert(prefix, entry);
        }
        for (_, d) in self.daemons() {
            d.reload_paused();
        }
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
        self.run.notify_one();
    }

    /// The projects' settings files: `<home>/<prefix>/daemon/settings`.
    fn found(&self) -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        for e in std::fs::read_dir(&self.home)
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == MACHINE_DIR || !valid_prefix(&name) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(e.path().join("daemon/settings")) {
                found.insert(name, text);
            }
        }
        found
    }

    /// `said`: why it could not start last time, not logged again.
    async fn open(
        &self,
        prefix: &str,
        stamp: String,
        first_poll: Duration,
        said: Option<&str>,
    ) -> Entry {
        let daemon = match Settings::load(&self.home, prefix) {
            Ok(s) => Daemon::start_in(s, self.run.clone(), self.changed.clone(), first_poll).await,
            Err(e) => Err(e),
        };
        match &daemon {
            Ok(d) => eprintln!(
                "bana daemon: {prefix}: CI on push for {}",
                d.settings().repo
            ),
            Err(e) if said != Some(e.as_str()) => eprintln!("bana daemon: {prefix}: {e}"),
            Err(_) => {}
        }
        Entry {
            router: daemon
                .as_ref()
                .ok()
                .map(|d| crate::server::project_router(d.clone())),
            daemon,
            stamp,
        }
    }

    /// Runs the projects' builds one at a time, each project in turn from
    /// the one after the last served: the first whose queue may start runs
    /// its next build to its end, while the others' queues say they wait
    /// for it.
    async fn runner(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        loop {
            if *stop.borrow() {
                return;
            }
            let all = self.daemons();
            let last = locked(&self.last).clone();
            let at = last
                .and_then(|l| all.iter().position(|(p, _)| *p > l))
                .unwrap_or(0);
            let mut ran = false;
            for (prefix, d) in all[at..].iter().chain(&all[..at]) {
                if *stop.borrow() {
                    return;
                }
                let Some((id, cancel)) = d.next_build().await else {
                    continue;
                };
                *locked(&self.last) = Some(prefix.clone());
                let why = format!("after {prefix} #{id}");
                let build = d.run_build(id, cancel);
                tokio::pin!(build);
                loop {
                    for (p, other) in self.daemons() {
                        if p != *prefix {
                            other.hold_for(&why);
                        }
                    }
                    tokio::select! {
                        _ = &mut build => break,
                        _ = self.run.notified() => {}
                    }
                }
                ran = true;
                break;
            }
            if !ran {
                tokio::select! {
                    _ = self.run.notified() => {}
                    _ = tokio::time::sleep(self.machine.recheck) => {}
                    _ = stop.changed() => return,
                }
            }
        }
    }

    async fn rescan(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(RESCAN) => {}
                _ = stop.changed() => return,
            }
            self.scan().await;
        }
    }

    /// Stops every project (a running build ends, and runs again once at
    /// the next start), then the runner.
    pub async fn shutdown(&self) {
        let _ = self.stop.send(true);
        let stopping: Vec<JoinHandle<()>> = self
            .daemons()
            .into_iter()
            .map(|(_, d)| tokio::spawn(async move { d.shutdown().await }))
            .collect();
        for s in stopping {
            let _ = s.await;
        }
        let tasks = std::mem::take(&mut *locked(&self.tasks));
        for t in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(10), t).await;
        }
    }
}

/// A project removed: its running build ends as removed (so a later add does
/// not run it again), then it lets go of its daemon.lock once that build has
/// ended.
async fn remove(d: &Daemon) {
    d.stop_with(REMOVED).await;
}

/// A project between its old daemon and its new one.
fn restarting() -> Entry {
    Entry {
        daemon: Err("restarting".into()),
        router: None,
        stamp: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actlog::BuildState;
    use crate::daemon::tests::{finished, poll, until, Project, FIXTURES};

    /// Stand-in `bana` for all the projects: logs each build's start and
    /// end (`<prefix>-<id>`), waits while `hold` exists, replays the fixture.
    /// Its `--list` says so in `listing`, and waits while `holdlist` exists.
    const BANA: &str = r#"ctl='CTL'; fx='FX'
[[ $1 == ci ]] || exit 0
shift
f=$(cat "$BANA_PROJECT_ROOT/fixture")
if [[ $1 == --list ]]; then
  echo "$BANA_BUILD" >>"$ctl/listing"
  while [[ -e $ctl/holdlist ]]; do sleep 0.05; done
  grep -v '^time=' "$fx/$f.list"; exit 0; fi
trap 'echo "end $BANA_BUILD" >>"$ctl/order"; exit 130' INT
echo "start $BANA_BUILD" >>"$ctl/order"
while [[ -e $ctl/hold ]]; do sleep 0.05; done
while IFS= read -r line; do
  case $line in '{'*) printf '%s\n' "$line" ;; *) printf '%s\n' "$line" >&2 ;; esac
done <"$fx/$f.jsonl"
echo "end $BANA_BUILD" >>"$ctl/order"
"#;

    /// A home with projects added, and the machine's settings: the first
    /// project's stand-ins, but `bana`, which is theirs together.
    struct Home {
        home: PathBuf,
        ctl: PathBuf,
        p: Vec<Project>,
    }

    impl Home {
        fn new(name: &str, projects: &[&str]) -> Self {
            let home =
                std::env::temp_dir().join(format!("bana-registry-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            let ctl = home.join("ctl");
            std::fs::create_dir_all(home.join(MACHINE_DIR)).unwrap();
            std::fs::create_dir_all(&ctl).unwrap();
            let p: Vec<Project> = projects.iter().map(|n| Project::new_in(n, &home)).collect();
            let script = ctl.join("bana");
            let body = BANA
                .replace("CTL", &ctl.to_string_lossy())
                .replace("FX", FIXTURES);
            std::fs::write(&script, body).unwrap();
            let bin = p[0].root.join("bin");
            let machine = format!(
                "host = t\nlogin = me\nhome = {h}\npath = {path}\ngit = {b}/git\ngh = {b}/gh\ndocker = {b}/docker\nscript = {s}\nretry = 50ms\nrecheck = 50ms\ngrace = 500ms\nladder = 400ms 400ms\nladder_short = 200ms 200ms\n",
                h = home.display(),
                path = std::env::var("PATH").unwrap(),
                b = bin.display(),
                s = script.display(),
            );
            std::fs::write(home.join(MACHINE_DIR).join("settings"), machine).unwrap();
            let h = Self { home, ctl, p };
            for i in 0..h.p.len() {
                h.add(i, "");
            }
            h
        }

        /// `bana add`: the project's settings.
        fn add(&self, i: usize, extra: &str) {
            let p = &self.p[i];
            std::fs::create_dir_all(p.dir.join("daemon")).unwrap();
            let text = format!(
                "repo = o/r\nprefix = {}\ntiers = quick nightly\ndaemon.poll = 3600\n{extra}",
                p.prefix
            );
            std::fs::write(p.dir.join("daemon/settings"), text).unwrap();
        }

        async fn start(&self) -> Arc<Registry> {
            Registry::start(&self.home, Machine::load(&self.home).unwrap()).await
        }

        fn hold(&self, on: bool) {
            let hold = self.ctl.join("hold");
            if on {
                std::fs::write(hold, "").unwrap();
            } else {
                let _ = std::fs::remove_file(hold);
            }
        }

        fn order(&self) -> Vec<String> {
            std::fs::read_to_string(self.ctl.join("order"))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        }

        fn remove(self) {
            let _ = std::fs::remove_dir_all(&self.home);
            for p in self.p {
                p.remove();
            }
        }
    }

    const TOKEN: &str = "ci0123456789abcdef0123456789abcd";

    /// One request to the daemon's router: its status and body.
    async fn call(app: &Router, method: &str, path: &str, token: bool) -> (u16, String) {
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let mut b = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:8470");
        if token {
            b = b.header("authorization", format!("Bearer {TOKEN}"));
        }
        let r = app
            .clone()
            .oneshot(b.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let code = r.status().as_u16();
        let body = r.into_body().collect().await.unwrap().to_bytes();
        (code, String::from_utf8_lossy(&body).into_owned())
    }

    fn row<'a>(rows: &'a [Value], prefix: &str) -> &'a Value {
        rows.iter().find(|r| r["prefix"] == prefix).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_build_at_a_time_each_project_in_turn() {
        let h = Home::new("turns", &["ta", "tb"]);
        let r = h.start().await;
        let (pa, pb) = (&h.p[0], &h.p[1]);
        assert_eq!(r.prefixes(), [pa.prefix.clone(), pb.prefix.clone()]);
        let (a, b) = (r.daemon(&pa.prefix).unwrap(), r.daemon(&pb.prefix).unwrap());
        // The first fetches only record the heads.
        poll(&a).await;
        poll(&b).await;

        // a's two pushes, then b's: a runs one, b waits for it.
        h.hold(true);
        pa.commit("pass", "a1");
        pa.push("main");
        pa.commit("pass", "a2");
        pa.push("side");
        poll(&a).await;
        until("a's first build", || a.running().is_some()).await;
        pb.commit("pass", "b1");
        pb.push("main");
        poll(&b).await;
        let (id, _) = a.running().unwrap();
        let after = format!("after {} #{id}", pa.prefix);
        until("b to wait for a", || {
            b.published().queue.first().and_then(|q| q.waiting.clone()) == Some(after.clone())
        })
        .await;
        let rows = r.rows();
        assert_eq!(row(&rows, &pa.prefix)["running"]["id"], id, "{rows:?}");
        assert_eq!(row(&rows, &pb.prefix)["queue"], 1, "{rows:?}");
        h.hold(false);
        for (d, id) in [(&a, 1), (&a, 2), (&b, 1)] {
            assert_eq!(finished(d, id).await.build.state, BuildState::Success);
        }
        // Never two at once, and b before a's second.
        let order = h.order();
        let who: Vec<String> = order
            .iter()
            .filter_map(|l| l.strip_prefix("start "))
            .map(|b| b.rsplit_once('-').unwrap().0.to_string())
            .collect();
        let turns = [pa.prefix.clone(), pb.prefix.clone(), pa.prefix.clone()];
        assert_eq!(who, turns, "{order:?}");
        for pair in order.chunks(2) {
            let started = pair[0].strip_prefix("start ").unwrap();
            assert_eq!(pair[1], format!("end {started}"), "{order:?}");
        }
        r.shutdown().await;
        h.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn projects_come_and_go_and_one_that_cannot_start_stops_no_other() {
        let h = Home::new("scan", &["sa", "sb"]);
        // Added, but its clone is gone.
        let broken = h.home.join("sc");
        std::fs::create_dir_all(broken.join("daemon")).unwrap();
        std::fs::write(broken.join("daemon/settings"), "repo = o/c\nprefix = sc\n").unwrap();
        std::fs::write(broken.join("daemon/paused"), "").unwrap();
        let r = h.start().await;
        let (pa, pb) = (&h.p[0], &h.p[1]);
        let rows = r.rows();
        let c = row(&rows, "sc");
        assert!(c["error"].as_str().unwrap().contains("no clone"), "{c}");
        assert_eq!((&c["repo"], &c["paused"]), (&json!("o/c"), &json!(true)));
        assert!(r.daemon("sc").is_none());
        let errors = r.errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].0 == "sc" && errors[0].1.contains("no clone"),
            "{errors:?}"
        );
        assert!(matches!(r.routes("sc"), Some(Err(_))) && r.routes("sd").is_none());
        assert_eq!(row(&rows, &pb.prefix)["error"], Value::Null);

        let b = r.daemon(&pb.prefix).unwrap();
        poll(&b).await;
        pb.commit("pass", "b1");
        pb.push("main");
        poll(&b).await;
        assert_eq!(finished(&b, 1).await.build.state, BuildState::Success);

        // A changed settings file starts its project again; the rest stay.
        let a = r.daemon(&pa.prefix).unwrap();
        h.add(0, "daemon.poll = 3599\n");
        r.scan().await;
        assert!(!r.daemon(&pa.prefix).unwrap().same(&a));
        assert!(r.daemon(&pb.prefix).unwrap().same(&b));
        // A pause from the shell is read at the scan.
        std::fs::write(pb.dir.join("daemon/paused"), "").unwrap();
        r.scan().await;
        assert!(b.paused());
        std::fs::remove_file(pb.dir.join("daemon/paused")).unwrap();
        r.scan().await;
        assert!(!b.paused());

        // Removed mid-build: cancelled, never run again, its lock let go.
        h.hold(true);
        pb.commit("pass", "b2");
        pb.push("main");
        poll(&b).await;
        let started = format!("start {}-2", pb.prefix);
        until("b's build", || h.order().contains(&started)).await;
        std::fs::remove_file(pb.dir.join("daemon/settings")).unwrap();
        r.scan().await;
        assert!(r.daemon(&pb.prefix).is_none());
        assert!(!r.prefixes().contains(&pb.prefix));
        h.hold(false);
        h.add(1, "");
        r.scan().await;
        let b = r.daemon(&pb.prefix).expect("its daemon.lock is free");
        let rec = finished(&b, 2).await;
        assert_eq!(
            (rec.build.state, rec.build.reason.as_deref()),
            (BuildState::Error, Some(REMOVED))
        );
        let s = b.published();
        assert!(s.queue.is_empty() && s.running.is_none(), "{s:?}");
        assert_eq!(s.last.map(|l| l.id), Some(2));
        r.shutdown().await;
        h.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn removed_before_act_ran_is_not_run_again() {
        let h = Home::new("prep", &["pa"]);
        let r = h.start().await;
        let p = &h.p[0];
        let d = r.daemon(&p.prefix).unwrap();
        poll(&d).await;
        let holdlist = h.ctl.join("holdlist");
        std::fs::write(&holdlist, "").unwrap();
        p.commit("pass", "a1");
        p.push("main");
        poll(&d).await;
        let listing = h.ctl.join("listing");
        until("its bana ci --list", || listing.exists()).await;
        std::fs::remove_file(p.dir.join("daemon/settings")).unwrap();
        let scan = tokio::spawn({
            let r = r.clone();
            async move { r.scan().await }
        });
        // Its cancel, then the stop, go in the same turn as it leaves the list.
        until("the removal", || r.daemon(&p.prefix).is_none()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::remove_file(&holdlist).unwrap();
        scan.await.unwrap();
        assert!(r.daemon(&p.prefix).is_none() && r.prefixes().is_empty());
        h.add(0, "");
        r.scan().await;
        let d = r.daemon(&p.prefix).unwrap();
        let rec = finished(&d, 1).await;
        assert_eq!(
            (rec.build.state, rec.build.reason.as_deref()),
            (BuildState::Error, Some(REMOVED))
        );
        let s = d.published();
        assert!(s.queue.is_empty() && s.running.is_none(), "{s:?}");
        assert!(h.order().is_empty(), "act never ran: {:?}", h.order());
        r.shutdown().await;
        h.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_project_starting_again_waits_for_its_build_and_stays_listed() {
        let h = Home::new("again", &["ga"]);
        let r = h.start().await;
        let p = &h.p[0];
        let access = Arc::new(crate::guard::Access::loopback(TOKEN, 8470, &["/ci/v1/"]));
        let app = crate::server::daemon_router(r.clone(), access);
        let d = r.daemon(&p.prefix).unwrap();
        poll(&d).await;
        let holdlist = h.ctl.join("holdlist");
        std::fs::write(&holdlist, "").unwrap();
        p.commit("pass", "a1");
        p.push("main");
        poll(&d).await;
        let listing = h.ctl.join("listing");
        until("its bana ci --list", || listing.exists()).await;
        // Its settings change while its build is checked out.
        h.add(0, "daemon.poll = 3599\n");
        let scan = tokio::spawn({
            let r = r.clone();
            async move { r.scan().await }
        });
        until("the restart", || r.daemon(&p.prefix).is_none()).await;
        let rows = r.rows();
        assert_eq!(row(&rows, &p.prefix)["error"], "restarting", "{rows:?}");
        let (code, v) = call(&app, "GET", &format!("/ci/v1/p/{}/local", p.prefix), true).await;
        assert_eq!(code, 503, "{v}");
        assert!(v.contains("restarting"), "{v}");
        // The old daemon holds on until its build has ended.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!scan.is_finished());
        assert!(d.running().is_some());
        std::fs::remove_file(&holdlist).unwrap();
        scan.await.unwrap();
        let d2 = r.daemon(&p.prefix).unwrap();
        assert!(!d2.same(&d));
        // Queued again as it was, and run once, by the new daemon.
        let rec = finished(&d2, 1).await;
        assert_eq!(rec.build.state, BuildState::Success);
        let started = format!("start {}-1", p.prefix);
        let order = h.order();
        assert_eq!(
            order.iter().filter(|l| **l == started).count(),
            1,
            "{order:?}"
        );
        r.shutdown().await;
        h.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_projects_routes_are_under_its_prefix() {
        let h = Home::new("routes", &["ra", "rb"]);
        let broken = h.home.join("rc");
        std::fs::create_dir_all(broken.join("daemon")).unwrap();
        std::fs::write(broken.join("daemon/settings"), "repo = o/c\nprefix = rc\n").unwrap();
        let r = h.start().await;
        let access = Arc::new(crate::guard::Access::loopback(TOKEN, 8470, &["/ci/v1/"]));
        let app = crate::server::daemon_router(r.clone(), access);
        let (pa, pb) = (&h.p[0].prefix, &h.p[1].prefix);
        let a = r.daemon(pa).unwrap();
        poll(&a).await;
        h.p[0].commit("pass", "a1");
        h.p[0].push("main");
        poll(&a).await;
        finished(&a, 1).await;

        let get = |path: String, token: bool| {
            let app = app.clone();
            async move { call(&app, "GET", &path, token).await }
        };
        let (code, v) = get(format!("/ci/v1/p/{pa}/builds/1"), true).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(serde_json::from_str::<Value>(&v).unwrap()["id"], 1);
        assert_eq!(get(format!("/ci/v1/p/{pb}/builds/1"), true).await.0, 404);
        let (_, v) = get(format!("/ci/v1/p/{pa}/builds?limit=0"), true).await;
        let v: Value = serde_json::from_str(&v).unwrap();
        assert_eq!(v["builds"], json!([]), "the query goes along");
        let (_, v) = get(format!("/ci/v1/p/{pb}/local"), true).await;
        let v: Value = serde_json::from_str(&v).unwrap();
        assert_eq!((&v["prefix"], &v["port"]), (&json!(pb), &json!(8470)));
        let (code, v) = get("/ci/v1/p/nope/local".into(), true).await;
        assert_eq!(code, 404);
        assert!(v.contains("no project nope"), "{v}");
        let (code, v) = get("/ci/v1/p/rc/local".into(), true).await;
        assert_eq!(code, 503);
        assert!(v.contains("rc: no clone"), "{v}");
        assert_eq!(get(format!("/ci/v1/p/{pa}/builds/1"), false).await.0, 401);
        assert_eq!(get("/ci/v1/projects".into(), false).await.0, 401);

        let (code, v) = get("/ci/v1/health".into(), false).await;
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&v).unwrap();
        assert_eq!(
            (&v["daemon"], &v["global"], &v["port"], &v["projects"]),
            (
                &json!(true),
                &json!(true),
                &json!(8470),
                &json!([pa, pb, "rc"])
            )
        );
        // One project a line, for the shell.
        let (code, text) = get("/ci/v1/projects".into(), true).await;
        assert_eq!(code, 200);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!((lines.len(), lines[0], lines[4]), (5, "[", "]"), "{text}");
        let a: Value = serde_json::from_str(lines[1].trim_end_matches(',')).unwrap();
        assert_eq!(
            (
                &a["prefix"],
                &a["repo"],
                &a["paused"],
                &a["queue"],
                &a["error"]
            ),
            (
                &json!(pa),
                &json!("o/r"),
                &json!(false),
                &json!(0),
                &Value::Null
            )
        );
        assert_eq!(
            (&a["last"]["id"], &a["last"]["state"]),
            (&json!(1), &json!("success"))
        );
        assert_eq!(a["running"], Value::Null);
        // A rescan, then the rows: rc is removed.
        std::fs::remove_dir_all(&broken).unwrap();
        let (code, text) = call(&app, "POST", "/ci/v1/projects", true).await;
        assert_eq!(code, 200);
        let rows: Vec<Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(rows.len(), 2, "{text}");
        r.shutdown().await;
        h.remove();
    }
}

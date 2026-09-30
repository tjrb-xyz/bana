//! `bana-manager`: the dev manager for a bana runner pool. `bana manager` builds
//! and starts it with the project's settings; by hand:
//!
//!   bana-manager --script PATH/bin/bana --repo OWNER/REPO [--port 8470]
//!                [--workflow ci.yml] [--tiers quick,nightly,release] [--tier-input tier]
//!                [--gh PATH] [--token T]
//!
//! Prints `http://127.0.0.1:8470/#token=…`; open it. Loopback only.
//!
//!   bana-manager daemon --dir ~/.bana/<prefix> [--no-tray]
//!
//! runs the project's CI on push (bana_manager::daemon), with the settings
//! `bana daemon install` wrote to <dir>/daemon/settings. It serves the same
//! page, with the daemon's builds, on the settings' port, and on a Mac shows
//! 🧱 in the menu bar (bana_manager::tray) unless --no-tray or `tray = no`.
//!
//!   bana-manager post-status --repo OWNER/REPO --sha SHA --context C
//!                --state pending|success|failure|error --description D
//!                [--target-url URL] [--gh PATH]
//!
//! (hidden) posts one commit status as the daemon's poster does, and says
//! GitHub's error if any: bana's own CI asks GitHub whether it takes a
//! loopback target_url.
//!
//!   bana-manager results (--json FILE | --text FILE|-)
//!
//! folds act's log (the daemon's act.jsonl, or act's plain text: a hand run's
//! ci/last.log, a pasted log) into results.jsonl on stdout
//! (bana_manager::results): what bana fix and bana report read.
//!
//!   bana-manager fix prepare --dir ~/.bana/<prefix> --checkout DIR
//!                (--build N | --run | --log FILE|- [--sha S] [--ref R] [--tier T])
//!                [--repo OWNER/REPO] [--workflow FILE] [--bana CMD] [--headless]
//!   bana-manager fix brief --dir ~/.bana/<prefix> [FIX]
//!   bana-manager fix gate --dir ~/.bana/<prefix>
//!   bana-manager fix push --dir ~/.bana/<prefix> FIX
//!   bana-manager fix drop --dir ~/.bana/<prefix> FIX [--force] [--delete-branch]
//!
//! bana fix's Rust side (bana_manager::fix): `prepare` makes (or reuses) the
//! fix branch's worktree for a daemon build, the last hand run or a pasted log,
//! writes its brief and prompt, and prints {fix, worktree, branch, link,
//! command, dir, reused} as JSON (`--headless`: fix.json says Claude runs
//! unattended, and the prompt that round 0 runs); `brief` prints a fix's brief. `gate` is the
//! fix worktree's Stop hook: it reads the hook's JSON on stdin and exits 2,
//! with what Claude should do on stderr, when Claude stops with changes bana
//! has not run. `push` and `drop` are bana fix push and drop (and the fix
//! card's Push and Discard); they print what they did as JSON.
//!
//!   bana-manager mcp --dir ~/.bana/<prefix> [--config]
//!
//! bana's MCP server for Claude Code, on stdin and stdout (bana_manager::mcp):
//! the fix loop's tools, in the fix worktree it runs in. `--config` prints the
//! --mcp-config JSON that starts it instead.

use bana_manager::actlog::{Status, StatusState};
use bana_manager::daemon::{post_status, Daemon, Settings};
use bana_manager::guard::Access;
use bana_manager::server::{daemon_router, health_at, router, Manager, Tools};
use bana_manager::{valid_repo, valid_tier, valid_workflow};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Notify;

fn usage() -> ! {
    eprintln!(
        "usage: bana-manager --script PATH --repo OWNER/REPO [--port N] [--workflow FILE] [--tiers A,B] [--tier-input NAME] [--gh PATH] [--token T]\n       bana-manager daemon --dir DIR [--no-tray]\n       bana-manager results (--json FILE | --text FILE|-)\n       bana-manager fix prepare --dir DIR --checkout DIR (--build N | --run | --log FILE|- [--sha S] [--ref R] [--tier T]) [--repo OWNER/REPO] [--workflow FILE] [--bana CMD] [--headless]\n       bana-manager fix brief --dir DIR [FIX]\n       bana-manager fix gate --dir DIR\n       bana-manager fix push --dir DIR FIX\n       bana-manager fix drop --dir DIR FIX [--force] [--delete-branch]\n       bana-manager mcp --dir DIR [--config]"
    );
    std::process::exit(2)
}

fn home() -> PathBuf {
    std::env::var_os("BANA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".bana")))
        .unwrap_or_else(|| PathBuf::from(".bana"))
}

/// A token that survives restarts, so a bookmarked link keeps working.
fn stored_token(dir: &Path) -> String {
    let path = dir.join("manager-token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        if t.trim().len() >= 16 {
            return t.trim().to_string();
        }
    }
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("read /dev/urandom");
    let t: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    let _ = std::fs::create_dir_all(dir);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        {
            let _ = f.write_all(t.as_bytes());
        }
    }
    t
}

/// `bin/bana` next to this build (manager/target/release/bana-manager).
fn find_script() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .map(|d| d.join("bin/bana"))
        .find(|p| p.is_file())
}

fn fail(msg: &str) -> ! {
    eprintln!("bana-manager: {msg}");
    std::process::exit(2)
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|a| a == "results") {
        args.next();
        results(args);
        return;
    }
    if args.peek().is_some_and(|a| a == "fix") {
        args.next();
        fix(args);
        return;
    }
    // Synchronous, with no runtime: Claude Code starts one for each session.
    if args.peek().is_some_and(|a| a == "mcp") {
        args.next();
        mcp(args);
        return;
    }
    let daemon_mode = args.peek().is_some_and(|a| a == "daemon");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(&format!("tokio: {e}")));
    if args.peek().is_some_and(|a| a == "post-status") {
        args.next();
        rt.block_on(post_one(args));
        return;
    }
    if !daemon_mode {
        rt.block_on(manager(args));
        return;
    }
    args.next();
    let (settings, no_tray) = daemon_args(args);
    let quit = Arc::new(Notify::new());
    // On a Mac the menu bar takes the main thread (AppKit wants it); the
    // daemon runs on the runtime's threads and tells the menu bar when it stops.
    #[cfg(target_os = "macos")]
    if settings.tray && !no_tray {
        let handle = rt.handle().clone();
        let q = quit.clone();
        bana_manager::tray::run(quit, move |tray| {
            let (ready, stopped) = (tray.clone(), tray);
            handle.spawn(daemon(
                settings,
                q,
                move |d, url| ready.ready(d, url),
                move || stopped.stopped(),
            ));
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = no_tray; // no menu bar here
    rt.block_on(daemon(settings, quit, |_, _| {}, || {}));
}

fn daemon_args(mut args: impl Iterator<Item = String>) -> (Settings, bool) {
    let (mut dir, mut no_tray) = (None, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dir" => dir = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
            "--no-tray" => no_tray = true,
            _ => usage(),
        }
    }
    let Some(dir) = dir else { usage() };
    (Settings::load(&dir).unwrap_or_else(|e| fail(&e)), no_tray)
}

/// `daemon --dir D`: serves the page on the settings' port and runs until
/// SIGTERM, Ctrl-C or `quit`, then stops the build it runs. `ready` gets the
/// daemon and the page's URL (with the token) once it runs; `stopped` is
/// called after it has stopped.
async fn daemon(
    settings: Settings,
    quit: Arc<Notify>,
    ready: impl FnOnce(&Daemon, String),
    stopped: impl FnOnce(),
) {
    let (repo, prefix, port) = (
        settings.repo.clone(),
        settings.prefix.clone(),
        settings.port,
    );
    let dir = settings.dir.clone();
    let token = stored_token(&settings.home);
    let url = format!("http://127.0.0.1:{port}/#token={token}");
    // The port first: a second daemon for this project leaves, with success
    // (under launchd a failure would start it again every 10 s).
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => match health_at(port).await {
            Some(h) if h["daemon"] == true && h["repo"] == repo.as_str() => {
                println!("bana-manager: the daemon for {repo} already runs: {url}");
                std::process::exit(0)
            }
            Some(h) if h["daemon"] == true => fail(&format!(
                "port {port} is taken by the bana daemon for {}; set another port",
                h["repo"].as_str().unwrap_or("another project")
            )),
            Some(_) => fail(&format!(
                "port {port} is taken by bana's manager; stop it (bana manager) or set another port"
            )),
            None => fail(&format!("{addr}: {e}")),
        },
    };
    // Caught from here: a stop during the start (its recovery takes a while)
    // waits for it, then stops cleanly.
    let mut term =
        signal(SignalKind::terminate()).unwrap_or_else(|e| fail(&format!("SIGTERM: {e}")));
    let mut int = signal(SignalKind::interrupt()).unwrap_or_else(|e| fail(&format!("SIGINT: {e}")));
    let d = Daemon::start(settings).await.unwrap_or_else(|e| fail(&e));
    let manager = Manager::new(
        Tools::from_settings(d.settings()),
        d.settings().machine.clone(),
    );
    let access = Arc::new(Access::loopback(token, port, &["/ci/v1/"]));
    let app = daemon_router(manager, d.clone(), access);
    let (stop_http, http_stopped) = tokio::sync::oneshot::channel::<()>();
    let http = tokio::spawn(async move {
        let stop = async {
            let _ = http_stopped.await;
        };
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(stop)
            .await
        {
            eprintln!("bana-manager: {addr}: {e}");
        }
    });
    println!(
        "bana-manager: CI on push for {repo} ({prefix}), in {}: {url}",
        dir.display()
    );
    ready(&d, url);
    tokio::select! {
        _ = int.recv() => {}
        _ = term.recv() => {}
        _ = quit.notified() => {}
    }
    println!("bana-manager: stopping");
    d.shutdown().await;
    let _ = stop_http.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), http).await;
    stopped();
}

/// `results`: act's log (--json: act.jsonl; --text: act's plain text, `-` for
/// stdin) as results.jsonl, on stdout.
fn results(mut args: impl Iterator<Item = String>) {
    let (json, path) = match (args.next(), args.next(), args.next()) {
        (Some(flag), Some(path), None) if flag == "--json" || flag == "--text" => {
            (flag == "--json", path)
        }
        _ => usage(),
    };
    let mut bytes = Vec::new();
    let read = match path.as_str() {
        "-" => std::io::stdin().read_to_end(&mut bytes).map(|_| ()),
        p => std::fs::read(p).map(|b| bytes = b),
    };
    if let Err(e) = read {
        fail(&format!("{path}: {e}"));
    }
    let text = String::from_utf8_lossy(&bytes);
    let r = if json {
        bana_manager::results::fold_json(&text)
    } else {
        bana_manager::results::fold_text(&text)
    };
    use std::io::Write;
    let _ = std::io::stdout().lock().write_all(r.to_jsonl().as_bytes());
}

/// `fix prepare|brief`: bana fix's side in Rust. Prints what prepare made as
/// JSON, or a fix's brief; a failure's reason goes to stderr (exit 1).
fn fix(mut args: impl Iterator<Item = String>) {
    use bana_manager::fix::{self, Prepare, Source};
    use bana_manager::valid_ref;
    let sub = args.next().unwrap_or_else(|| usage());
    let (mut dir, mut checkout, mut name) = (None, None, None);
    let (mut build, mut run, mut log) = (None, false, None);
    let (mut sha, mut git_ref, mut tier) = (None, None, None);
    let (mut repo, mut workflow, mut bana) = (None, None, None);
    let (mut force, mut delete_branch, mut headless) = (false, false, false);
    let named = ["brief", "push", "drop"].contains(&sub.as_str());
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--dir" => dir = Some(PathBuf::from(value())),
            "--checkout" => checkout = Some(PathBuf::from(value())),
            "--build" => build = Some(value().parse::<u64>().unwrap_or_else(|_| usage())),
            "--run" => run = true,
            "--log" => log = Some(value()),
            "--sha" => sha = Some(value()),
            "--ref" => git_ref = Some(value()),
            "--tier" => tier = Some(value()),
            "--repo" => repo = Some(value()),
            "--workflow" => workflow = Some(value()),
            "--bana" => bana = Some(value()),
            "--force" if sub == "drop" => force = true,
            "--delete-branch" if sub == "drop" => delete_branch = true,
            "--headless" if sub == "prepare" => headless = true,
            n if named && name.is_none() && !n.starts_with('-') => name = Some(n.to_string()),
            _ => usage(),
        }
    }
    // Absolute: git runs in the checkout, where a relative path means another place.
    let absolute = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
    let Some(dir) = dir.map(absolute) else {
        usage()
    };
    let checkout = checkout.map(absolute);
    let done = |r: Result<String, fix::Error>| match r {
        Ok(text) => {
            use std::io::Write;
            let _ = std::io::stdout().lock().write_all(text.as_bytes());
        }
        Err(e) => {
            eprintln!("bana-manager: {e}");
            std::process::exit(1)
        }
    };
    match sub.as_str() {
        "brief" => {
            let cwd = std::env::current_dir().ok();
            return done(fix::brief(&dir, name.as_deref(), cwd.as_deref()));
        }
        "gate" => gate(&dir),
        "push" | "drop" => {
            let Some(name) = name else { usage() };
            return done(if sub == "push" {
                // git talks to the terminal as it pushes.
                fix::push_fix(&dir, &name, "git", None, true).map(|p| pretty(&p))
            } else {
                fix::drop_fix(&dir, &name, "git", None, force, delete_branch).map(|d| pretty(&d))
            });
        }
        _ => {}
    }
    if sub != "prepare" {
        usage()
    }
    let Some(checkout) = checkout else { usage() };
    let with_log = sha.is_some() || git_ref.is_some() || tier.is_some();
    let source = match (build, run, log) {
        (Some(n), false, None) if !with_log => Source::Build(n),
        (None, true, None) if !with_log => Source::Run,
        (None, false, Some(path)) => {
            let mut bytes = Vec::new();
            let read = match path.as_str() {
                "-" => std::io::stdin().read_to_end(&mut bytes).map(|_| ()),
                p => std::fs::read(p).map(|b| bytes = b),
            };
            if let Err(e) = read {
                fail(&format!("{path}: {e}"));
            }
            Source::Log {
                text: String::from_utf8_lossy(&bytes).into_owned(),
                sha,
                git_ref,
                tier,
            }
        }
        _ => usage(),
    };
    if let Source::Log {
        git_ref: Some(r), ..
    } = &source
    {
        if !valid_ref(r) {
            fail("--ref: a branch or tag");
        }
    }
    if let Source::Log { tier: Some(t), .. } = &source {
        if !t.is_empty() && !valid_tier(t) {
            fail("--tier: letters, digits, '_' and '-'");
        }
    }
    let mut p = Prepare::new(&dir, &checkout, source);
    if let Some(r) = repo {
        if !valid_repo(&r) {
            fail("--repo: OWNER/REPO");
        }
        p.repo = Some(r);
    }
    if let Some(w) = workflow {
        if !valid_workflow(&w) {
            fail("--workflow: a file name in .github/workflows, like ci.yml");
        }
        p.workflow = w;
    }
    if let Some(b) = bana {
        p.bana = b;
    }
    if headless {
        // bana fix registers it with the daemon next, which queues round 0
        // unless the fix has rounds already.
        p.headless = true;
        p.recheck = p.rounds.is_some();
    }
    done(fix::prepare(&p).map(|made| pretty(&made)));
}

fn pretty<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default() + "\n"
}

/// `mcp`: bana's MCP server on stdin and stdout, for the fix whose worktree
/// this runs in, until stdin ends; `--config` prints how to start it.
fn mcp(mut args: impl Iterator<Item = String>) {
    let (mut dir, mut config) = (None, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dir" => dir = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
            "--config" => config = true,
            _ => usage(),
        }
    }
    let Some(dir) = dir else { usage() };
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    if config {
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .unwrap_or_else(|e| fail(&format!("this program's path: {e}")));
        println!("{}", bana_manager::mcp::config(&exe, &dir));
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|e| fail(&format!("the directory: {e}")));
    let mut server = bana_manager::mcp::Server::new(&dir, &cwd);
    let (stdin, stdout) = (std::io::stdin(), std::io::stdout());
    if let Err(e) = server.serve(stdin.lock(), stdout.lock()) {
        eprintln!("bana mcp: {e}");
    }
}

/// `fix gate`: Claude Code's Stop hook in a fix's worktree. The hook's JSON
/// on stdin says where Claude is (cwd); exit 2 sends stderr to Claude, and
/// keeps it going. Anything unexpected lets Claude stop (exit 0).
fn gate(dir: &Path) -> ! {
    let mut input = Vec::new();
    let _ = std::io::stdin().take(1 << 20).read_to_end(&mut input);
    let hook: serde_json::Value = serde_json::from_slice(&input).unwrap_or_default();
    let cwd = hook["cwd"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let Some(cwd) = cwd else {
        std::process::exit(0)
    };
    match bana_manager::fix::gate(dir, &cwd, "git", None) {
        bana_manager::fix::Gate::Block(why) => {
            eprintln!("{why}");
            std::process::exit(2)
        }
        bana_manager::fix::Gate::Pass(_) => std::process::exit(0),
    }
}

/// `post-status`: one status, through the poster's own code.
async fn post_one(mut args: impl Iterator<Item = String>) {
    let (mut repo, mut sha, mut context, mut state, mut description, mut url) =
        (None, None, None, None, String::new(), None);
    let mut gh = "gh".to_string();
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--repo" => repo = Some(value()),
            "--sha" => sha = Some(value()),
            "--context" => context = Some(value()),
            "--state" => {
                state = Some(match value().as_str() {
                    "pending" => StatusState::Pending,
                    "success" => StatusState::Success,
                    "failure" => StatusState::Failure,
                    "error" => StatusState::Error,
                    _ => fail("--state: pending, success, failure or error"),
                })
            }
            "--description" => description = value(),
            "--target-url" => url = Some(value()),
            "--gh" => gh = value(),
            _ => usage(),
        }
    }
    let (Some(repo), Some(sha), Some(context), Some(state)) = (repo, sha, context, state) else {
        usage()
    };
    if !valid_repo(&repo) {
        fail("--repo: OWNER/REPO");
    }
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let status = Status {
        context,
        state,
        description,
    };
    match post_status(
        &gh,
        &path,
        Path::new("."),
        &repo,
        &sha,
        &status,
        url.as_deref(),
    )
    .await
    {
        Ok(()) => println!(
            "posted {} {} to {repo}@{sha}",
            status.context,
            state.as_str()
        ),
        Err(e) => {
            eprintln!("bana-manager: not posted: {e}");
            std::process::exit(1)
        }
    }
}

async fn manager(mut args: impl Iterator<Item = String>) {
    let mut port = 8470u16;
    let (mut repo, mut script, mut token) = (None, None, None);
    let (mut gh, mut workflow, mut tier_input) =
        ("gh".to_string(), "ci.yml".to_string(), "tier".to_string());
    let mut tiers: Vec<String> = ["quick", "nightly", "release"].map(String::from).into();
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--port" => port = value().parse().unwrap_or_else(|_| usage()),
            "--repo" => repo = Some(value()),
            "--script" => script = Some(PathBuf::from(value())),
            "--gh" => gh = value(),
            "--token" => token = Some(value()),
            "--workflow" => workflow = value(),
            "--tier-input" => tier_input = value(),
            "--tiers" => {
                tiers = value()
                    .split([',', ' '])
                    .filter(|t| !t.is_empty())
                    .map(String::from)
                    .collect()
            }
            _ => usage(),
        }
    }
    let Some(repo) = repo else { usage() };
    if !valid_repo(&repo) {
        fail("--repo: OWNER/REPO");
    }
    if !valid_workflow(&workflow) {
        fail("--workflow: a file name in .github/workflows, like ci.yml");
    }
    if !tiers.iter().all(|t| valid_tier(t)) || !valid_tier(&tier_input) {
        fail("--tiers and --tier-input: letters, digits, '_' and '-'");
    }
    let Some(script) = script.or_else(find_script) else {
        fail("bin/bana not found; pass --script (or run `bana manager`)")
    };
    let token = token.unwrap_or_else(|| stored_token(&home()));
    let machine = std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this machine".into());
    let manager = Manager::new(
        Tools {
            script,
            gh,
            repo,
            workflow,
            tiers,
            tier_input,
        },
        machine,
    );
    let access = Arc::new(Access::loopback(token.clone(), port, &["/ci/v1/"]));
    let app = router(manager, access);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => fail(&format!("{addr}: {e}")),
    };
    println!("bana-manager: http://127.0.0.1:{port}/#token={token}");
    let stop = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(stop)
        .await
    {
        fail(&format!("{addr}: {e}"));
    }
}

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

use bana_manager::daemon::{Daemon, Settings};
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
        "usage: bana-manager --script PATH --repo OWNER/REPO [--port N] [--workflow FILE] [--tiers A,B] [--tier-input NAME] [--gh PATH] [--token T]\n       bana-manager daemon --dir DIR [--no-tray]"
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
    let daemon_mode = args.peek().is_some_and(|a| a == "daemon");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(&format!("tokio: {e}")));
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

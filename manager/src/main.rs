//! `bana-manager`: the dev manager for a bana runner pool. `bana manager` builds
//! and starts it with the project's settings; by hand:
//!
//!   bana-manager --script PATH/bin/bana --repo OWNER/REPO [--port 8470]
//!                [--workflow ci.yml] [--tiers quick,nightly,release] [--tier-input tier]
//!                [--gh PATH] [--token T]
//!
//! Prints `http://127.0.0.1:8470/#token=…`; open it. Loopback only.

use bana_manager::guard::Access;
use bana_manager::server::{router, Manager, Tools};
use bana_manager::{valid_repo, valid_tier, valid_workflow};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn usage() -> ! {
    eprintln!(
        "usage: bana-manager --script PATH --repo OWNER/REPO [--port N] [--workflow FILE] [--tiers A,B] [--tier-input NAME] [--gh PATH] [--token T]"
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

#[tokio::main]
async fn main() {
    let mut port = 8470u16;
    let (mut repo, mut script, mut token) = (None, None, None);
    let (mut gh, mut workflow, mut tier_input) =
        ("gh".to_string(), "ci.yml".to_string(), "tier".to_string());
    let mut tiers: Vec<String> = ["quick", "nightly", "release"].map(String::from).into();
    let mut args = std::env::args().skip(1);
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

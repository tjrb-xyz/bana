//! What a build may leave behind, found and removed: act's process tree, the
//! processes that carry the build's marker (`BANA_BUILD=<prefix>-<id>`, set in
//! act's environment and inherited by every host step), the job containers
//! with the daemon's label and their volumes, and act's host workspaces.
//!
//! The parsers are pure; each finder runs one program (or reads /proc).

use crate::daemon::LABEL;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// (pid, ppid) pairs from `ps -A -o pid=,ppid=`.
pub fn parse_ps(text: &str) -> Vec<(u32, u32)> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let pid = f.next()?.parse().ok()?;
            let ppid = f.next()?.parse().ok()?;
            Some((pid, ppid))
        })
        .collect()
}

/// Every descendant of `root` in a `ps` listing, parents first; not `root`.
pub fn tree(pairs: &[(u32, u32)], root: u32) -> Vec<u32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i];
        for (pid, ppid) in pairs {
            if *ppid == parent && *pid != root && !out.contains(pid) {
                out.push(*pid);
            }
        }
        i += 1;
    }
    out.remove(0);
    out
}

/// act's descendants now. Host steps are session leaders (act setsid's them),
/// but still act's children, so the parent links find them.
pub async fn descendants(path: &str, pid: u32) -> Vec<u32> {
    match run("ps", &["-A", "-o", "pid=,ppid="], path).await {
        Ok(text) => tree(&parse_ps(&text), pid),
        Err(e) => {
            eprintln!("bana daemon: ps: {e}");
            Vec::new()
        }
    }
}

/// A process's environment (`/proc/<pid>/environ`: NUL-separated) has `marker`
/// as BANA_BUILD.
pub fn environ_has(environ: &[u8], marker: &str) -> bool {
    let want = format!("BANA_BUILD={marker}");
    environ.split(|b| *b == 0).any(|kv| kv == want.as_bytes())
}

/// The pids in `ps -Eww -o pid=,command=` (macOS) whose line has
/// `BANA_BUILD=<marker>` as a word: ps -E puts the environment after the
/// command, separated by spaces.
pub fn parse_ps_env(text: &str, marker: &str) -> Vec<u32> {
    let want = format!("BANA_BUILD={marker}");
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let pid = f.next()?.parse().ok()?;
            f.any(|w| w == want).then_some(pid)
        })
        .collect()
}

/// The processes of `uid` that carry the marker: those a build started,
/// wherever they went (nohup, setsid, reparented to init or launchd).
pub async fn marker_pids(path: &str, uid: u32, marker: &str) -> Vec<u32> {
    if cfg!(target_os = "linux") {
        return proc_marker_pids(Path::new("/proc"), uid, marker);
    }
    let uid = uid.to_string();
    match run("ps", &["-Eww", "-o", "pid=,command=", "-U", &uid], path).await {
        Ok(text) => parse_ps_env(&text, marker),
        Err(e) => {
            eprintln!("bana daemon: ps: {e}");
            Vec::new()
        }
    }
}

/// Linux: /proc/<pid>/environ of each process `uid` owns.
fn proc_marker_pids(proc: &Path, uid: u32, marker: &str) -> Vec<u32> {
    use std::os::unix::fs::MetadataExt;
    let mut out = Vec::new();
    for e in std::fs::read_dir(proc).into_iter().flatten().flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if e.metadata().map(|m| m.uid()).ok() != Some(uid) {
            continue;
        }
        if std::fs::read(e.path().join("environ")).is_ok_and(|env| environ_has(&env, marker)) {
            out.push(pid);
        }
    }
    out
}

/// (id, name) of each container in `docker ps -a --format '{{.ID}} {{.Names}}'`.
pub fn parse_containers(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (id, name) = l.trim().split_once(' ')?;
            Some((id.to_string(), name.trim().to_string()))
        })
        .filter(|(id, name)| !id.is_empty() && !name.is_empty())
        .collect()
}

/// Removes the job containers labelled for this prefix, and their volumes
/// (act names them after the container: `<name>` for the workspace and
/// `<name>-env`; run_context.go GetBindsAndMounts). Says how many went.
pub async fn docker_sweep(docker: &str, path: &str, prefix: &str) -> Result<usize, String> {
    let filter = format!("label={LABEL}={prefix}");
    let listed = run(
        docker,
        &[
            "ps",
            "-a",
            "--filter",
            &filter,
            "--format",
            "{{.ID}} {{.Names}}",
        ],
        path,
    )
    .await?;
    let found = parse_containers(&listed);
    if found.is_empty() {
        return Ok(0);
    }
    let mut rm = vec!["rm", "-f"];
    rm.extend(found.iter().map(|(id, _)| id.as_str()));
    run(docker, &rm, path).await?;
    let volumes: Vec<String> = found
        .iter()
        .flat_map(|(_, name)| [name.clone(), format!("{name}-env")])
        .collect();
    let mut rm = vec!["volume", "rm", "-f"];
    rm.extend(volumes.iter().map(String::as_str));
    // A volume that is not there is no error with -f; one still in use is
    // left for act's own removal by name at the next run.
    if let Err(e) = run(docker, &rm, path).await {
        eprintln!("bana daemon: docker volume rm: {e}");
    }
    Ok(found.len())
}

/// act's host workspaces in its cache: 16 hex digits (8 random bytes), which
/// act removes itself unless it is killed.
pub fn is_workspace(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Removes the workspaces under act's cache; the actions it cloned stay.
/// Says which went.
pub fn act_cache_sweep(dir: &Path) -> Vec<String> {
    let mut gone = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if is_workspace(&name)
            && e.file_type().is_ok_and(|t| t.is_dir())
            && std::fs::remove_dir_all(e.path()).is_ok()
        {
            gone.push(name);
        }
    }
    gone.sort();
    gone
}

/// Sends `sig` to `pid`; false when it is not there. Never to 0, 1 or a group.
pub fn signal(pid: u32, sig: libc::c_int) -> bool {
    match libc::pid_t::try_from(pid) {
        // SAFETY: kill(2) with a positive pid signals that one process.
        Ok(p) if p > 1 => unsafe { libc::kill(p, sig) == 0 },
        _ => false,
    }
}

/// A process that has exited but not been reaped (Linux; `/proc/<pid>/stat`):
/// it holds its pid and start time, but nothing else.
pub fn zombie(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(')')
                .map(|(_, r)| r.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

async fn run(program: &str, args: &[&str], path: &str) -> Result<String, String> {
    let o = Command::new(program)
        .args(args)
        .env("PATH", path)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(60), o).await {
        Err(_) => Err(format!("{program} took longer than 60 s")),
        Ok(Err(e)) => Err(format!("{program}: {e}")),
        Ok(Ok(o)) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(Ok(o)) => {
            let e = String::from_utf8_lossy(&o.stderr);
            Err(format!(
                "{program} {}: {}",
                args.first().unwrap_or(&""),
                e.lines()
                    .rfind(|l| !l.trim().is_empty())
                    .unwrap_or("failed")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_tree_is_found_through_its_parents() {
        // act (100) runs bash (200, a setsid'd host step) that runs cargo (300)
        // and rustc (301, 302); 400 is someone else's; 500 was reparented to init.
        let text = "    1     0\n  100     1\n  200   100\n  300   200\n  301   300\n\
                    302   300\n  400     1\n  500     1\n  bad line\n  600   400\n";
        let pairs = parse_ps(text);
        assert_eq!(pairs.len(), 9);
        assert_eq!(tree(&pairs, 100), [200, 300, 301, 302]);
        assert_eq!(tree(&pairs, 300), [301, 302]);
        assert!(tree(&pairs, 301).is_empty() && tree(&pairs, 999).is_empty());
        // A loop in a listing taken while pids were reused does not hang.
        assert_eq!(tree(&[(2, 3), (3, 2)], 2), [3]);
    }

    #[test]
    fn the_marker_is_found_in_environ_and_in_ps_e() {
        let env = b"PATH=/usr/bin\0BANA_BUILD=wid-7\0HOME=/home/me\0";
        assert!(environ_has(env, "wid-7"));
        assert!(!environ_has(env, "wid-70") && !environ_has(env, "wid"));
        assert!(!environ_has(b"XBANA_BUILD=wid-7\0", "wid-7"));
        assert!(
            environ_has(b"BANA_BUILD=wid-7", "wid-7"),
            "the last one has no NUL"
        );

        // macOS `ps -Eww -o pid=,command= -U 501`: the environment follows the
        // command, space separated.
        let ps = "  311 /usr/sbin/cfprefsd agent\n\
                  4242 /bin/sleep 1000 TMPDIR=/var/folders/x/T/ BANA_BUILD=wid-7 HOME=/Users/me\n\
                  4243 /bin/sleep 1000 BANA_BUILD=wid-70 HOME=/Users/me\n\
                  4244 bash -c echo BANA_BUILD=wid-7x\n\
                  4245 /usr/bin/caffeinate -i -w 4200 PATH=/usr/bin BANA_BUILD=wid-7\n\
                  garbage BANA_BUILD=wid-7\n";
        assert_eq!(parse_ps_env(ps, "wid-7"), [4242, 4245]);
        assert_eq!(parse_ps_env(ps, "wid-70"), [4243]);
    }

    #[test]
    fn this_process_is_found_by_its_own_environ() {
        // The test binary has no marker; a child with one is found, then goes.
        let marker = format!("sweep{}-1", std::process::id());
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .env("BANA_BUILD", &marker)
            .spawn()
            .unwrap();
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        if cfg!(target_os = "linux") {
            let found = proc_marker_pids(Path::new("/proc"), uid, &marker);
            assert_eq!(found, [child.id()]);
            assert!(proc_marker_pids(Path::new("/proc"), uid.wrapping_add(1), &marker).is_empty());
        }
        assert!(signal(child.id(), libc::SIGKILL));
        child.wait().unwrap();
        assert!(!signal(0, 0) && !signal(1, 0));
    }

    #[test]
    fn containers_and_workspaces_are_told_apart() {
        let ps = "3f2a1b0c9d8e act-ci-yml-rust-5d1e0f\n\
                  77aa act-ci-yml-web-0a1b2c\n\n  \nlonely\n";
        assert_eq!(
            parse_containers(ps),
            [
                (
                    "3f2a1b0c9d8e".to_string(),
                    "act-ci-yml-rust-5d1e0f".to_string()
                ),
                ("77aa".into(), "act-ci-yml-web-0a1b2c".into())
            ]
        );
        assert!(is_workspace("0123456789abcdef"));
        for name in [
            "0123456789ABCDEF",
            "0123456789abcde",
            "0123456789abcdef0",
            "actions-checkout@v4",
            "tool_cache",
            "0123456789abcdeg",
        ] {
            assert!(!is_workspace(name), "{name}");
        }

        let dir = std::env::temp_dir().join(format!("bana-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in [
            "0123456789abcdef/hostexecutor",
            "fedcba9876543210",
            "actions-checkout@v4",
            "tool_cache",
        ] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("aaaaaaaaaaaaaaaa"), "a file, not a workspace").unwrap();
        assert_eq!(
            act_cache_sweep(&dir),
            ["0123456789abcdef", "fedcba9876543210"]
        );
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["aaaaaaaaaaaaaaaa", "actions-checkout@v4", "tool_cache"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

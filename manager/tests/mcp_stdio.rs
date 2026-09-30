//! `bana-manager mcp` over its pipes, as Claude Code drives it: the legacy
//! handshake and the stateless era, JSON-RPC's errors, the tools that answer
//! from files, and (when Claude Code is on PATH) `claude mcp get` seeing it
//! connected. No test here runs a prompt.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_bana-manager");

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bana-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(&d).unwrap()
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A session: `lines` in, stdin closed, and each line out as JSON. The
/// server must end on its own (stdin's end), with success, and write only
/// one JSON object a line.
fn session(dir: &Path, cwd: &Path, lines: &[Value]) -> Vec<Value> {
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    session_text(dir, cwd, &text)
}

fn session_text(dir: &Path, cwd: &Path, lines: &[String]) -> Vec<Value> {
    let mut child = Command::new(BIN)
        .args(["mcp", "--dir"])
        .arg(dir)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for l in lines {
        writeln!(stdin, "{l}").unwrap();
    }
    drop(stdin);
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let Ok(out) = rx.recv_timeout(Duration::from_secs(60)) else {
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        panic!("bana-manager mcp did not end with its stdin");
    };
    let out = out.unwrap();
    assert!(
        out.status.success(),
        "{:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"));
            assert!(v.is_object() && v["jsonrpc"] == "2.0", "{l}");
            v
        })
        .collect()
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn initialize(id: u64, version: &str) -> Value {
    request(
        id,
        "initialize",
        json!({"protocolVersion": version, "capabilities": {"roots": {"listChanged": true}, "elicitation": {}},
            "clientInfo": {"name": "claude-code", "version": "2.1.284"}}),
    )
}

/// The `_meta` Claude Code sends each request in the stateless era.
fn meta(version: &str) -> Value {
    json!({"io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientInfo": {"name": "claude-code", "version": "2.1.284"},
        "io.modelcontextprotocol/clientCapabilities": {"roots": {"listChanged": true}}})
}

fn call(id: u64, tool: &str, args: Value) -> Value {
    request(id, "tools/call", json!({"name": tool, "arguments": args}))
}

fn by_id(replies: &[Value], id: u64) -> &Value {
    let found: Vec<&Value> = replies.iter().filter(|r| r["id"] == id).collect();
    assert_eq!(found.len(), 1, "one reply to {id}: {replies:?}");
    found[0]
}

/// A tool's result: its structured content, which the text block says too.
fn structured(reply: &Value) -> &Value {
    let r = &reply["result"];
    assert_eq!(r["isError"], false, "{reply}");
    let text: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(r["structuredContent"].is_object(), "{reply}");
    assert_eq!(text, r["structuredContent"]);
    &r["structuredContent"]
}

fn tool_error(reply: &Value) -> String {
    let r = &reply["result"];
    assert_eq!(r["isError"], true, "{reply}");
    r["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn the_legacy_handshake_echoes_the_versions_it_knows() {
    let d = scratch("legacy");
    let mut lines = Vec::new();
    let asked = [
        "2025-11-25",
        "2025-06-18",
        "2025-03-26",
        "2024-11-05",
        "2024-10-07",
        "2031-01-01",
    ];
    for (i, v) in asked.iter().enumerate() {
        lines.push(initialize(i as u64, v));
    }
    let replies = session(&d, &d, &lines);
    assert_eq!(replies.len(), asked.len());
    for (i, v) in asked.iter().enumerate() {
        let r = &by_id(&replies, i as u64)["result"];
        // Not 2025-03-26, whose servers must take batches.
        let want = if [0, 1, 3].contains(&i) {
            v
        } else {
            "2025-11-25"
        };
        assert_eq!(r["protocolVersion"], want, "{v}: {r}");
        assert_eq!(r["serverInfo"]["name"], "bana");
        assert_eq!(r["capabilities"], json!({"tools": {}}));
        assert!(r["instructions"].as_str().unwrap().contains("run_jobs"));
        assert!(r.get("resultType").is_none(), "legacy: {r}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_session_as_claude_code_opens_it() {
    let d = scratch("session");
    let replies = session(
        &d,
        &d,
        &[
            initialize(0, "2025-11-25"),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            request(1, "ping", json!({})),
            request(2, "tools/list", json!({})),
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 9}}),
            // A client's answer to a request of the server's: nothing back.
            json!({"jsonrpc": "2.0", "id": 77, "result": {}}),
            request(3, "tools/list", Value::Null),
        ],
    );
    assert_eq!(replies.len(), 4, "{replies:?}");
    assert_eq!(by_id(&replies, 1)["result"], json!({}));
    let tools = by_id(&replies, 2)["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "fix_brief",
            "ci_log",
            "run_jobs",
            "fix_status",
            "commit_fix"
        ]
    );
    assert_eq!(by_id(&replies, 3)["result"], by_id(&replies, 2)["result"]);
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        assert_eq!(t["outputSchema"]["type"], "object", "{t}");
        assert!(!t["description"].as_str().unwrap().is_empty());
        let a = &t["annotations"];
        assert_eq!(
            (&a["destructiveHint"], &a["openWorldHint"]),
            (&json!(false), &json!(false)),
            "{t}"
        );
        let reader = ["fix_brief", "ci_log", "fix_status"].contains(&t["name"].as_str().unwrap());
        assert_eq!(a["readOnlyHint"], reader, "{t}");
    }
    let hints = |name: &str| {
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        (
            t["annotations"]["readOnlyHint"].clone(),
            t["annotations"]["idempotentHint"].clone(),
        )
    };
    assert_eq!(hints("run_jobs"), (json!(false), json!(true)));
    assert_eq!(hints("commit_fix"), (json!(false), json!(false)));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_stateless_era_discovers_and_names_its_version_each_time() {
    let d = scratch("modern");
    let m = meta("2026-07-28");
    let mut no_caps = m.clone();
    no_caps
        .as_object_mut()
        .unwrap()
        .remove("io.modelcontextprotocol/clientCapabilities");
    let replies = session(
        &d,
        &d,
        &[
            json!({"jsonrpc": "2.0", "id": "server-discover-probe-1", "method": "server/discover", "params": {"_meta": m}}),
            request(0, "tools/list", json!({"_meta": m})),
            request(1, "tools/list", json!({})),
            request(2, "tools/list", json!({"_meta": no_caps})),
            request(3, "tools/list", json!({"_meta": meta("2099-01-01")})),
            request(
                4,
                "tools/call",
                json!({"_meta": m, "name": "run_jobs", "arguments": {}}),
            ),
            // A legacy version named per request is taken too.
            request(5, "tools/list", json!({"_meta": meta("2025-06-18")})),
        ],
    );
    let discover = &replies[0];
    assert_eq!(discover["id"], "server-discover-probe-1");
    let r = &discover["result"];
    assert_eq!(r["supportedVersions"][0], "2026-07-28");
    assert!(r["supportedVersions"]
        .as_array()
        .unwrap()
        .contains(&json!("2025-11-25")));
    assert_eq!(r["capabilities"], json!({"tools": {}}));
    assert!(r["instructions"].is_string());
    for r in [r, &by_id(&replies, 0)["result"]] {
        assert_eq!(r["resultType"], "complete", "{r}");
        assert!(r["ttlMs"].is_u64() && r["cacheScope"].is_string(), "{r}");
        assert_eq!(
            r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "bana"
        );
    }
    assert_eq!(by_id(&replies, 0)["result"]["tools"][2]["name"], "run_jobs");
    for id in [1, 2] {
        let e = &by_id(&replies, id)["error"];
        assert_eq!(e["code"], -32602, "{e}");
    }
    let e = &by_id(&replies, 3)["error"];
    assert_eq!(e["code"], -32022, "{e}");
    assert_eq!(e["data"]["requested"], "2099-01-01");
    assert_eq!(e["data"]["supported"][0], "2026-07-28");
    let run = by_id(&replies, 4);
    assert_eq!(run["result"]["resultType"], "complete");
    assert!(tool_error(run).contains("works in a bana fix's worktree"));
    let legacy = &by_id(&replies, 5)["result"];
    assert!(
        legacy["tools"].is_array() && legacy.get("resultType").is_none(),
        "{legacy}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn errors_are_json_rpcs() {
    let d = scratch("errors");
    let replies = session_text(
        &d,
        &d,
        &[
            initialize(0, "2025-11-25").to_string(),
            request(1, "resources/list", json!({})).to_string(),
            call(2, "publish_release", json!({})).to_string(),
            call(3, "fix_brief", json!({"fix": 3})).to_string(),
            call(4, "ci_log", json!({"build": 1})).to_string(),
            call(5, "fix_status", json!({"verbose": true})).to_string(),
            call(6, "run_jobs", json!({"jobs": "lint"})).to_string(),
            request(7, "tools/call", json!({"arguments": {}})).to_string(),
            "{not json".into(),
            "[1, 2]".into(),
            String::new(),
            call(8, "commit_fix", json!({"message": ""})).to_string(),
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.into(),
        ],
    );
    assert_eq!(by_id(&replies, 1)["error"]["code"], -32601);
    // An unknown tool, or no name: the protocol's errors.
    for id in [2, 7] {
        let r = by_id(&replies, id);
        assert_eq!(r["error"]["code"], -32602, "{id}: {r}");
        assert!(r.get("result").is_none());
    }
    // Arguments that do not fit: the tool's, so Claude can try again.
    for (id, why) in [
        (3, "fix_brief: fix: a string"),
        (4, "ci_log: job is required"),
        (5, "fix_status: no argument \"verbose\""),
        (6, "run_jobs: jobs: a list of strings"),
        (
            8,
            "commit_fix: message: what was wrong, and why this fixes it",
        ),
    ] {
        let got = tool_error(by_id(&replies, id));
        assert_eq!(got, why, "{id}");
    }
    let nulls: Vec<&Value> = replies.iter().filter(|r| r["id"].is_null()).collect();
    assert_eq!(nulls.len(), 3, "{replies:?}");
    assert_eq!(nulls[0]["error"]["code"], -32700);
    assert_eq!(nulls[1]["error"]["code"], -32600, "a batch");
    assert_eq!(nulls[2]["error"]["code"], -32600, "an id of null");
    assert_eq!(replies.len(), 12, "the empty line has no reply");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_fixs_tools_answer_from_its_files_without_the_daemon() {
    let root = scratch("files");
    let (dir, co, wt) = (root.join("home/wid"), root.join("co"), root.join("wt"));
    std::fs::create_dir_all(dir.join("daemon")).unwrap();
    std::fs::create_dir_all(&co).unwrap();
    git(&co, &["init", "-q", "-b", "main"]);
    std::fs::write(co.join("lib.rs"), "one\n").unwrap();
    git(&co, &["add", "-A"]);
    git(&co, &["commit", "-q", "-m", "one"]);
    let sha = git(&co, &["rev-parse", "HEAD"]);
    let sha7 = &sha[..7];
    let branch = format!("bana/fix-{sha7}");
    git(
        &co,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            &wt.to_string_lossy(),
        ],
    );
    // Nothing listens on the settings' port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .unwrap()
        .port();
    std::fs::write(
        dir.join("daemon/settings"),
        format!("port = {port}\nfix.rounds = 3\n"),
    )
    .unwrap();
    let state = dir.join(format!("fix/{sha7}.d"));
    std::fs::create_dir_all(&state).unwrap();
    let fix = json!({"version": 1, "fix": sha7, "origin": "log", "sha": sha, "ref": "refs/heads/main",
        "jobs": ["rust"], "checkout": co, "worktree": wt, "branch": branch});
    std::fs::write(state.join("fix.json"), fix.to_string()).unwrap();
    // The owner's pasted log, folded as bana fix folds it.
    let folded = Command::new(BIN)
        .args(["results", "--text"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/results/example-paste.txt"))
        .output()
        .unwrap();
    assert!(folded.status.success());
    std::fs::write(state.join("results.jsonl"), folded.stdout).unwrap();

    let sub = wt.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let replies = session(
        &dir,
        &sub,
        &[
            initialize(0, "2025-11-25"),
            call(1, "fix_brief", json!({})),
            call(2, "fix_status", json!({})),
            call(3, "run_jobs", json!({})),
            call(4, "ci_log", json!({"build": 1, "job": "rust"})),
            call(5, "commit_fix", json!({"message": "why"})),
        ],
    );
    let b = structured(by_id(&replies, 1));
    assert_eq!(
        (&b["fix"], &b["base_sha"], &b["worktree"]),
        (&json!(sha7), &json!(sha), &json!(wt))
    );
    assert_eq!(b["rounds"], json!({"used": 0, "max": 3, "left": 3}));
    assert_eq!(b["origin"]["log"], json!(state.join("log.txt")));
    assert_eq!(b["failures"][0]["owner"], "project", "{b}");
    assert!(
        b["failures"][0]["tests"][0]["at"]
            .as_str()
            .unwrap()
            .ends_with("facts.rs:457:18"),
        "{b}"
    );
    assert!(b["not_project"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["owner"] == "bana"));
    assert_eq!(
        b["environment"]["parallel_jobs"],
        json!([{"job": "rust", "result": "failure"}])
    );
    let st = structured(by_id(&replies, 2));
    assert_eq!(
        (
            &st["state"],
            &st["changed_since_last_round"],
            &st["rounds_left"]
        ),
        (&json!("open"), &json!(false), &json!(3))
    );
    for id in [3, 4] {
        let why = tool_error(by_id(&replies, id));
        assert!(why.starts_with("the bana daemon is not running"), "{why}");
    }
    let why = tool_error(by_id(&replies, 5));
    assert!(why.contains("no round has run yet"), "{why}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn it_prints_the_config_that_starts_it() {
    let d = scratch("config");
    let o = Command::new(BIN)
        .args(["mcp", "--dir"])
        .arg(&d)
        .arg("--config")
        .output()
        .unwrap();
    assert!(o.status.success());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let bana = &v["mcpServers"]["bana"];
    assert_eq!(bana["type"], "stdio");
    assert_eq!(bana["command"], json!(std::fs::canonicalize(BIN).unwrap()));
    assert_eq!(bana["args"], json!(["mcp", "--dir", d]));
    let o = Command::new(BIN).args(["mcp"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2), "no --dir");
    assert!(String::from_utf8_lossy(&o.stderr).contains("bana-manager mcp --dir DIR"));
    let _ = std::fs::remove_dir_all(&d);
}

/// Claude Code's own config commands (they run no prompt): what `bana
/// daemon install` registers is connected in the checkout and in a linked
/// worktree elsewhere, in both of Claude Code's handshakes. Skipped when
/// `claude` is not on PATH (or BANA_TEST_CLAUDE=0).
#[test]
fn claude_code_connects_to_it_in_the_checkout_and_its_worktrees() {
    let Some(claude) = std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join("claude"))
            .find(|c| c.is_file())
    }) else {
        eprintln!("skipped: no claude on PATH");
        return;
    };
    if std::env::var("BANA_TEST_CLAUDE").as_deref() == Ok("0") {
        eprintln!("skipped: BANA_TEST_CLAUDE=0");
        return;
    }
    let root = scratch("claude");
    let (home, checkout, dir) = (
        root.join("home"),
        root.join("checkout"),
        root.join("bana/wid"),
    );
    for d in [&home, &checkout, &dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let wt = root.join("bana/wid/fix/abc1234");
    git(
        &checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "bana/fix-abc1234",
            &wt.to_string_lossy(),
        ],
    );
    // Only what it needs: no session of this machine's own leaks in.
    let run = |cwd: &Path, args: &[&str], auto: bool| {
        let mut c = Command::new(&claude);
        c.args(args)
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .stdin(Stdio::null());
        if auto {
            c.env("MCP_PROTOCOL_NEGOTIATION", "auto");
        }
        let o = c.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        (o.status.success(), text)
    };
    let dir_s = dir.to_string_lossy().into_owned();
    let (added, said) = run(
        &checkout,
        &[
            "mcp", "add", "-s", "local", "bana", "--", BIN, "mcp", "--dir", &dir_s,
        ],
        false,
    );
    assert!(added, "{said}");
    for cwd in [&checkout, &wt] {
        for auto in [false, true] {
            let (ok, said) = run(cwd, &["mcp", "get", "bana"], auto);
            assert!(ok, "{}: {said}", cwd.display());
            assert!(
                said.contains("Connected") && said.contains("Local config"),
                "{} (auto {auto}): {said}",
                cwd.display()
            );
        }
    }
    let (removed, said) = run(&checkout, &["mcp", "remove", "-s", "local", "bana"], false);
    assert!(removed, "{said}");
    let (found, _) = run(&wt, &["mcp", "get", "bana"], false);
    assert!(!found, "removed");
    let _ = std::fs::remove_dir_all(&root);
}

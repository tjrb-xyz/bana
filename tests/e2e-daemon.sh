#!/usr/bin/env bash
# The daemon end to end: real act, real Docker, the one daemon built from this tree
# (bana daemon run, outside any checkout), projects added with bana add, local bare origins
# over file:// in GitHub's place, and the gh stand-in (tests/stand-ins/gh), whose log is
# the statuses posted. Slow (minutes), so it runs only when asked:
#
#   BANA_E2E=1 tests/e2e-daemon.sh
#
#   BANA_E2E_ACT=PATH        the act to run (default: act on PATH)
#   BANA_DAEMON_BIN=PATH     a bana-manager already built (default: bana builds it)
#   BANA_E2E_DOCKER=0        no Docker: every job on the host (act's host mode), as on a
#                            Mac without OrbStack; docker is a stub that says it runs
#   BANA_E2E_IMAGE=IMAGE     the Linux jobs' image (default: catthehacker/ubuntu:act-24.04
#                            when Docker has it, else pulled from ghcr.io)
#   BANA_E2E_PORT=N          the daemon's port (default 18470)
#   BANA_E2E_KEEP=1          keep the scratch directory
#   BANA_E2E_SPLIT=1         also bana split's public workflow (bana.yml), run by act as GitHub
#                            would (its ubuntu-latest on this machine), on a local private origin
#
# Pushes: one that passes, one whose test fails, a [skip ci] one, one whose `sleep 600` is
# cancelled through the daemon's API, and one whose daemon is killed (-9) mid-build and
# started again (the build runs again, once). After each: no job containers, act
# workspaces, marker processes, secrets or lock left.
#
# The fix loop, with the test as Claude Code: Fix with Claude on the failed build makes its
# worktree in the checkout and runs round 0; the test then starts bana's MCP server as Claude
# Code does and calls its tools (fix_brief, run_jobs red, run_jobs green, commit_fix), and
# pushes the branch with bana fix push, which the daemon builds as any push. Then a hand
# bana ci that fails, bana fix last (the claude stand-in), and run_jobs until green. Last,
# a matrix that uploads an archive per CPU: the green build keeps them as its files with the
# project's installer, which installs demo under a scratch home, as bana install does. Then
# a release: the pushed tag v0.1.0 builds at the tag tier, bana asks, the notes are saved,
# and Publish runs gh release create --verify-tag (the stand-in's release store).
#
# Then a second project, added while the daemon runs: one build at a time on the machine
# (its push waits "after wid #N"), its push hook, bana pause (Run now still builds, the push
# waits until bana resume), and bana remove of the first, which the second's build outlives.
set -euo pipefail

if [[ ${BANA_E2E:-} != 1 ]]; then
  echo "e2e-daemon: skipped (BANA_E2E=1 runs it: real act and Docker, a few minutes)"
  exit 0
fi

here=$(cd "$(dirname "$0")" && pwd -P)
bana=$here/../bin/bana
docker_mode=${BANA_E2E_DOCKER:-1}
port=${BANA_E2E_PORT:-18470}
os=$(uname -s)
t0=$(date +%s)
T=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/bana-e2e.XXXXXX")" && pwd -P)
fails=0 n=0 daemon_pid=''

act=${BANA_E2E_ACT:-$(command -v act || true)}
[[ -x $act ]] || { echo "e2e-daemon: act is needed (BANA_E2E_ACT=PATH)" >&2; exit 1; }
if [[ $docker_mode == 1 ]] && ! docker info >/dev/null 2>&1; then
  echo "e2e-daemon: Docker does not answer (BANA_E2E_DOCKER=0 runs host jobs only)" >&2
  exit 1
fi
image=${BANA_E2E_IMAGE:-}
if [[ $docker_mode == 1 && -z $image ]]; then
  image=catthehacker/ubuntu:act-24.04
  # act's Ubuntu image: the jobs use node actions (upload-artifact), so the image needs node.
  # The builds run with --pull=false, so it is pulled here, from ghcr.io when Docker lacks it
  # (Docker Hub limits anonymous pulls).
  docker image inspect "$image" >/dev/null 2>&1 || image=ghcr.io/catthehacker/ubuntu:act-24.04
fi
if [[ $docker_mode == 1 ]] && ! docker image inspect "$image" >/dev/null 2>&1; then
  docker pull -q "$image" >/dev/null
fi
command -v python3 >/dev/null || { echo "e2e-daemon: python3 is needed (it reads the API's JSON)" >&2; exit 1; }

say() { printf '== %s (%ss)\n' "$*" "$(($(date +%s) - t0))"; }
check() { # NAME COMMAND...: passes when COMMAND succeeds
  local name=$1
  shift
  n=$((n + 1))
  if "$@"; then echo "ok   $name"; else echo "FAIL $name"; fails=$((fails + 1)); fi
}
not() { ! "$@"; }
same() { [[ $1 == "$2" ]] || { printf '  want: %s\n  got:  %s\n' "$2" "$1" >&2; return 1; }; }
has() { grep -qF -- "$2" "$1" || { echo "  $1 lacks: $2" >&2; return 1; }; }
# Waits up to SECONDS for COMMAND to succeed.
until_ok() { # SECONDS WHAT COMMAND...
  local limit=$(($(date +%s) + $1)) what=$2
  shift 2
  until "$@" 2>/dev/null; do
    if (($(date +%s) > limit)); then
      echo "e2e-daemon: timed out waiting for $what" >&2
      return 1
    fi
    sleep 0.5
  done
}

# ---- the world: a home, bin, an origin and a checkout --------------------------------------

# rustup and cargo find their toolchains through HOME: keep yours for the daemon's build.
export RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup} CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
export HOME=$T/home FAKE_LOG=$T/gh.log FAKE_STATE=$T/state FAKE_RELEASE_STORE=$T/releases
mkdir -p "$HOME" "$FAKE_STATE" "$FAKE_RELEASE_STORE" "$T/bin"
: >"$FAKE_LOG"
# Only this world's git config: no signing, no rewrites from the machine's.
unset GIT_CONFIG_COUNT GIT_CONFIG_PARAMETERS GITHUB_TOKEN GH_TOKEN BANA_HOME BANA_PROJECT_ROOT BANA_BUILD
export GIT_CONFIG_NOSYSTEM=1
git init -q --bare "$T/origin.git"
cat >"$HOME/.gitconfig" <<EOF
[user]
  name = e2e
  email = e2e@example.com
[init]
  defaultBranch = main
# GitHub is this directory: the daemon's clone fetches https://github.com/acme/wid.git.
[url "file://$T/origin.git"]
  insteadOf = https://github.com/acme/wid.git
EOF
ln -s "$here/stand-ins/gh" "$T/bin/gh"
ln -s "$act" "$T/bin/act"
if [[ $docker_mode != 1 ]]; then
  # Says Docker runs, and has no containers: host jobs need none.
  # shellcheck disable=SC2016 # the stub's own $1
  printf '#!/bin/sh\ncase $1 in info) exit 0 ;; esac\n' >"$T/bin/docker"
  chmod +x "$T/bin/docker"
fi
export PATH=$T/bin:$PATH

d=$HOME/.bana/wid
w=$T/work
# The project the API helpers ask (P=two api /local asks the second one).
P=wid
hold=$T/hold
mkdir -p "$w/.github/workflows" "$w/ci"
cd "$w"
git init -q .
git remote add origin "https://github.com/acme/wid.git"
if [[ $docker_mode == 1 ]]; then
  host_args='-P wid-host=-self-hosted'
else
  host_args='-P wid-host=-self-hosted -P wid-linux=-self-hosted'
fi
cat >.github/bana.conf <<EOF
repo = acme/wid
prefix = wid
tiers = quick nightly
daemon.poll = 10
daemon.tags = v*
# The gh stand-in's token is no token: act fetches upload-artifact from GitHub without one.
daemon.token = none
act.args = $host_args
act.image = ${image:-none}
report.work = linux/work host
report.tests = broken
report.answers = test:tests::*
EOF
cat >.github/workflows/ci.yml <<'EOF'
name: ci
on:
  workflow_dispatch:
    inputs:
      tier:
        default: quick
jobs:
  linux:
    runs-on: [self-hosted, wid-linux]
    steps:
      - uses: actions/checkout@v4
      - name: the pushed commit
        run: sh ci/check.sh "${{ github.sha }}" "${{ github.event.before }}"
      - name: work
        run: sh ci/step.sh linux
  host:
    runs-on: [self-hosted, wid-host]
    steps:
      - uses: actions/checkout@v4
      - name: work
        run: sh ci/step.sh host
  broken:
    runs-on: [self-hosted, wid-linux]
    steps:
      - uses: actions/checkout@v4
      - name: test
        run: sh ci/step.sh broken
  never:
    if: false
    runs-on: [self-hosted, wid-linux]
    steps:
      - run: echo never
EOF
cat >ci/check.sh <<'EOF'
# The checkout is the pushed commit, and the push's before is in its history.
head=$(git -c safe.directory='*' rev-parse HEAD)
echo "HEAD $head"
[ "$head" = "$1" ] || { echo "HEAD is not $1"; exit 1; }
case $2 in
*[1-9a-f]*) git -c safe.directory='*' cat-file -e "$2^{commit}" && echo "before $2 reachable" ;;
*) echo "before: none" ;;
esac
EOF
# What each job does, by ci/mode: pass, fail (broken's test fails, as cargo says it),
# sleep (a long step; the host job leaves a process behind first, as nohup would), hold
# (the host job waits while the file ci/hold names is there).
cat >ci/step.sh <<'EOF'
mode=$(cat ci/mode)
t=$(cat ci/t)
case $mode:$1 in
fail:broken) cat ci/cargo-test.txt; exit 101 ;;
sleep:host)
  (nohup sleep 900 >/dev/null 2>&1 & echo $! >"$t/survivor")
  echo "$$" >"$t/host-sleeping"
  sleep 600 ;;
sleep:linux) sleep 600 ;;
hold:host)
  echo "$$" >"$t/host-holding"
  n=0
  while [ -e "$(cat ci/hold)" ] && [ $n -lt 600 ]; do sleep 1; n=$((n + 1)); done ;;
esac
echo "$1: done ($mode)"
EOF
cat >ci/cargo-test.txt <<'EOF'
     Running unittests src/lib.rs (target/debug/deps/wid-0123456789abcdef)

running 2 tests
test tests::adds ... ok
test tests::the_answer ... FAILED

failures:

---- tests::the_answer stdout ----

thread 'tests::the_answer' panicked at src/lib.rs:9:5:
assertion `left == right` failed
  left: 41
 right: 42
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    tests::the_answer

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--lib`
EOF
echo "$T" >ci/t
echo "$hold" >ci/hold
echo pass >ci/mode
git add -A
git commit -q -m "the project"
git push -q origin HEAD:main
git -C "$T/origin.git" symbolic-ref HEAD refs/heads/main

# ---- the daemon ---------------------------------------------------------------------------

api() { # PATH [CURL-OPTIONS...]: project P's route
  local p=$1
  shift
  top "/p/$P$p" "$@"
}
top() { # PATH [CURL-OPTIONS...]: the daemon's own route (/projects)
  local p=$1
  shift
  printf 'Authorization: Bearer %s\n' "$(cat "$HOME/.bana/manager-token")" |
    curl -fsS --noproxy '*' --max-time 10 -H @- "$@" "http://127.0.0.1:$port/ci/v1$p"
}
# A value from JSON on stdin: a Python expression of j.
jq_() { python3 -c 'import json,sys; j=json.load(sys.stdin); v=eval(sys.argv[1]); print("" if v is None else v)' "$1"; }
health() { curl -fsS --noproxy '*' --max-time 3 "http://127.0.0.1:$port/ci/v1/health"; }
healthy() { health | grep -q '"global":true'; }
# Healthy, or given up: a daemon that could not start (its build failed) stops the wait.
up_or_gone() {
  healthy && return 0
  ! kill -0 "$daemon_pid" 2>/dev/null
}

start_daemon() {
  # bana daemon run, in no checkout: the machine's checks, the snapshot and its settings the
  # first time (it takes the port already in daemon.d/settings), then the one daemon in the
  # foreground, for every project added.
  (cd "$T" && exec bash "$bana" daemon run >>"$T/daemon.log" 2>&1) &
  daemon_pid=$!
  until_ok 600 "the daemon's health" up_or_gone
  healthy || { echo "e2e-daemon: the daemon exited before it was healthy" >&2; return 1; }
}
stop_daemon() {
  [[ -n $daemon_pid ]] || return 0
  kill "$daemon_pid" 2>/dev/null || true
  wait "$daemon_pid" 2>/dev/null || true
  daemon_pid=''
}

# Processes that carry a build of P's marker (BANA_BUILD=<P>-<id>), as the sweep finds them.
markers() {
  local f
  if [[ $os == Linux ]]; then
    for f in /proc/[0-9]*/environ; do
      { tr '\0' '\n' <"$f" | grep -q "^BANA_BUILD=$P-" && echo "${f//[^0-9]/}"; } 2>/dev/null
    done
    return 0
  fi
  # shellcheck disable=SC2009 # pgrep cannot see environments; ps -E can (macOS)
  ps -Eww -o pid=,command= -U "$(id -u)" | grep "[B]ANA_BUILD=$P-" | awk '{ print $1 }' || true
}
has_marker() { markers | grep -x "$1" >/dev/null; }
# PID runs (a zombie, killed but not yet reaped by whoever inherited it, does not).
alive() { # PID
  local st
  st=$(ps -o stat= -p "$1" 2>/dev/null) || return 1
  [[ $st != Z* ]]
}
containers() { [[ $docker_mode != 1 ]] || docker ps -aq --filter "label=xyz.tjrb.bana=$P"; }
workspaces() { find "$HOME/.bana/$P/act-cache" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | grep -E '/[0-9a-f]{16}$' || true; }
secrets() { find "$HOME/.bana/$P/builds" -name secrets 2>/dev/null || true; }

cleanup() {
  local code=$?
  stop_daemon
  pkill -9 -f "$T/" 2>/dev/null || true
  [[ $docker_mode != 1 ]] || docker ps -aq --filter label=xyz.tjrb.bana | xargs docker rm -f >/dev/null 2>&1 || true
  if ((code != 0 || fails > 0)); then
    echo "---- daemon log (last 60 lines)"
    tail -n 60 "$T/daemon.log" 2>/dev/null || true
    echo "---- gh log"
    grep -v '^gh auth' "$FAKE_LOG" 2>/dev/null | tail -n 60 || true
    echo "---- bana mcp's stderr (last 20 lines)"
    tail -n 20 "$T/mcp.log" 2>/dev/null || true
  fi
  if [[ ${BANA_E2E_KEEP:-} == 1 ]]; then echo "kept: $T"; else rm -rf "$T"; fi
}
trap cleanup EXIT

# Nothing a build of P left: no job container, act workspace, marker process, secrets or lock.
clean() { # WHAT
  check "$1: no job containers labelled xyz.tjrb.bana=$P" same "$(containers)" ""
  check "$1: no act workspaces in act-cache" same "$(workspaces)" ""
  check "$1: no processes with the build's marker" same "$(markers | tr '\n' ' ')" ""
  check "$1: the secrets file is gone" same "$(secrets)" ""
  check "$1: the lock is released" test ! -e "$HOME/.bana/act.lock"
}

# The statuses posted for SHA, in order: CONTEXT|STATE|DESCRIPTION.
posts() { # SHA
  grep -F "gh api -X POST repos/acme/$P/statuses/$1 " "$FAKE_LOG" |
    sed -e 's/ -f target_url=.*$//' -e 's/^.* -f state=\([a-z]*\) -f context=\(.*\) -f description=\(.*\)$/\2|\1|\3/' || true
}
# CONTEXT's last state for SHA, and its description.
last() { posts "$1" | grep "^$2|" | tail -n 1 | cut -d'|' -f2-; }
contexts() { posts "$1" | cut -d'|' -f1 | sort -u | tr '\n' ' '; }

push() { # MODE MESSAGE: commits and pushes; prints the sha
  echo "$1" >"$w/ci/mode"
  echo "$2" >"$w/ci/push"
  git -C "$w" add -A
  git -C "$w" commit -q -m "$2"
  git -C "$w" push -q origin HEAD:main
  (cd "$w" && bash "$bana" daemon poke >/dev/null)
  git -C "$w" rev-parse HEAD
}
# SHA's builds, oldest first: id|state|trigger|attempt|reason.
builds_of() {
  api /builds | python3 -c '
import json, sys
for b in reversed(json.load(sys.stdin)["builds"]):
    if b["sha"] == sys.argv[1]:
        print("%s|%s|%s|%s|%s" % (b["id"], b["state"], b["trigger"], b["attempt"], b["reason"] or ""))' "$1"
}
# The daemon is idle: nothing runs, nothing queued, every status posted.
idle() {
  local s
  s=$(api /local) || return 1
  same "$(jq_ '"%s %s %s" % (j["running"] is None, len(j["queue"]), j["watcher"]["unposted"])' <<<"$s")" "True 0 0" 2>/dev/null
}
finished() { builds_of "$1" | tail -n 1 | grep -Eq '^[0-9]+\|(success|failure|error)\|'; }
running_job() { # SHA KEY: the build of SHA runs, and so does its job KEY
  api /local | jq_ 'j["running"] and j["running"]["sha"] == "'"$1"'" and any(c["key"] == "'"$2"'" and c["state"] == "running" for c in j["running"]["jobs"]) or ""' | grep -q True
}
build_log() { cat "$d/builds/$1/act.jsonl"; }
# A standard's row of build ID's report.md.
report_row() { grep "^| $2 |" "$d/builds/$1/report.md" || true; } # ID NAME
state_of() { builds_of "$1" | tail -n 1 | cut -d'|' -f2; }

# ---- Claude Code's side of a fix ----------------------------------------------------------

# The MCP server Claude Code starts, as {command, args}: what bana add registers.
mcp_server='{"command": "'"$HOME/.bana/daemon.d/bana-manager"'", "args": ["mcp", "--dir", "'"$d"'"]}'
# Starts the server in DIR over stdio, says what Claude Code 2.1.284 says (initialize,
# notifications/initialized, tools/list), then calls TOOL with ARGS (JSON) and a progress token.
# Prints the result's structuredContent (else its text as {"text": ...}) with isError, and
# progress: how many progress notes came meanwhile; fails when a line on stdout is not one
# JSON-RPC message, an answer is missing, or the server does not exit 0 at EOF.
# Its stderr goes to mcp.log.
tool() { # DIR TOOL ARGS
  (cd "$1" && python3 -c '
import json, subprocess, sys
tool, args, server = sys.argv[1], json.loads(sys.argv[2]), json.loads(sys.argv[3])
p = subprocess.Popen([server["command"]] + server["args"], stdin=subprocess.PIPE,
                     stdout=subprocess.PIPE, stderr=open(sys.argv[4], "a"), text=True)
client = {"name": "claude-code", "title": "Claude Code", "version": "2.1.284"}
def send(m):
    p.stdin.write(json.dumps(dict(m, jsonrpc="2.0")) + "\n")
    p.stdin.flush()
notes = []
def answer(i):
    for line in p.stdout:
        m = json.loads(line)
        assert isinstance(m, dict) and m.get("jsonrpc") == "2.0", line
        if m.get("method") == "notifications/progress":
            assert m["params"]["progressToken"] == "e2e", m
            notes.append(m["params"]["message"])
            continue
        if m.get("id") == i:
            return m
    sys.exit("bana mcp: no answer to %d" % i)
send({"id": 0, "method": "initialize", "params": {"protocolVersion": "2025-11-25",
      "capabilities": {"roots": {"listChanged": True}, "elicitation": {}}, "clientInfo": client}})
init = answer(0)["result"]
assert init["serverInfo"]["name"] == "bana" and init["protocolVersion"] == "2025-11-25", init
send({"method": "notifications/initialized"})
send({"id": 1, "method": "tools/list"})
assert tool in [t["name"] for t in answer(1)["result"]["tools"]], tool
send({"id": 2, "method": "tools/call", "params": {"name": tool, "arguments": args,
      "_meta": {"claudecode/toolUseId": "toolu_e2e", "progressToken": "e2e"}}})
r = answer(2)
p.stdin.close()
assert p.wait() == 0, "bana mcp exited %s" % p.returncode
assert "result" in r, r
out = r["result"].get("structuredContent") or {"text": r["result"]["content"][0]["text"]}
out["isError"] = r["result"].get("isError", False)
out["progress"] = len(notes)
print(json.dumps(out))' "$2" "$3" "$mcp_server" "$T/mcp.log")
}
# A fix's round N ended (the daemon's answer, without waiting).
round_ended() { # FIX N
  api "/fixes/$1/rounds/$2" | jq_ 'j["state"] in ("success", "failure", "error") or ""' | grep -q True
}
# A fix's round builds, as the history lists them: ROUND:JOB:STATE, oldest first.
round_builds() { # FIX
  api /builds | python3 -c '
import json, sys
for b in reversed(json.load(sys.stdin)["builds"]):
    if b.get("fix") == sys.argv[1]:
        print("%s:%s:%s:%s" % (b["round"], b["job"], b["trigger"], b["state"]))' "$1" | tr '\n' ' '
}
# The snapshots a fix's rounds ran (round 0 is the failing commit itself).
round_shas() { # FIX
  python3 -c 'import json, sys; print(" ".join(r["sha"] for r in json.load(open(sys.argv[1]))["rounds"] if r["n"] > 0))' \
    "$d/fix/$1.d/rounds.json"
}
# Runs the Stop hook bana wrote into WT's Claude Code settings, as Claude Code runs it (sh,
# the hook's JSON on stdin); prints its exit status.
stop_hook() { # WT
  local cmd
  cmd=$(jq_ '[h["command"] for m in j["hooks"]["Stop"] for h in m["hooks"] if " fix gate " in h["command"]][0]' \
    <"$1/.claude/settings.local.json")
  printf '{"session_id":"e2e","hook_event_name":"Stop","stop_hook_active":false,"cwd":"%s"}' "$1" |
    (cd "$1" && sh -c "$cmd" >/dev/null 2>&1) && echo 0 || echo $?
}

# bana add, off a terminal: the report, nothing written in the checkout but the push hook,
# and the project added. The daemon does not run yet.
say "bana add in the checkout"
mkdir -p "$HOME/.bana/daemon.d"
echo "port = $port" >"$HOME/.bana/daemon.d/settings"
out=$(cd "$w" && bash "$bana" add </dev/null 2>&1) && code=0 || code=$?
[[ $code == 0 ]] || echo "$out" >&2
check "add: bana add, off a terminal" same "$code" 0
check "add: the project's settings, with this checkout" has "$d/daemon/settings" "checkout = $w"
check "add: they hold no machine key" not grep -q '^port' "$d/daemon/settings"
check "add: the daemon's clone" test -d "$d/src/.git"
check "add: the push hook, which reads the daemon's port when it runs" has "$w/.git/hooks/reference-transaction" "/ci/v1/p/wid/daemon/poll"
check "add: it says bana daemon install starts the daemon" has <(printf '%s\n' "$out") "bana daemon install starts CI"
check "add: nothing else in the checkout" same "$(git -C "$w" status --porcelain)" ""

say "starting the daemon (port $port, $([[ $docker_mode == 1 ]] && echo "Linux jobs in $image" || echo "host jobs only"))"
start_daemon
check "setup: the one daemon, for wid" same "$(health | jq_ 'j["projects"]')" "['wid']"
check "setup: its snapshot is the machine's" test -x "$HOME/.bana/daemon.d/bana-manager"
until_ok 60 "the first fetch" bash -c "grep -q '\"first_start_done\": true' '$d/state.json'"
check "setup: the daemon's clone fetches the origin" same "$(git -C "$d/src" rev-parse origin/main)" "$(git -C "$w" rev-parse HEAD)"
check "setup: the first fetch builds nothing" same "$(grep -c 'statuses' "$FAKE_LOG" || true)" "0"

# ---- 1. a push that passes --------------------------------------------------------------------
say "1. a push that passes"
s=$(date +%s)
a=$(push pass "passes")
until_ok 600 "build of $a" finished "$a"
until_ok 60 "the daemon idle" idle
id=$(builds_of "$a" | tail -n 1 | cut -d'|' -f1)
echo "   build #$id: $(state_of "$a") in $(($(date +%s) - s)) s"
check "pass: the build succeeded" same "$(state_of "$a")" success
check "pass: bana pending first" same "$(posts "$a" | head -n 1 | cut -d'|' -f1-2)" "bana|pending"
check "pass: then bana success" same "$(last "$a" bana | cut -d'|' -f1)" success
check "pass: per-job contexts, and none for the if: false job" same "$(contexts "$a")" "bana bana/broken bana/host bana/linux "
# A context's newest state is what goes out: a job that ends before the poster gets to its
# pending (a host job can take 0 s) posts only its success.
for j in linux host broken; do
  check "pass: bana/$j success (after pending, unless the job was quicker)" same \
    "$(posts "$a" | grep "^bana/$j|" | cut -d'|' -f2 | tr '\n' ' ' | sed 's/^pending //')" "success "
done
check "pass: statuses link to the project on the daemon's page" has "$FAKE_LOG" "target_url=http://127.0.0.1:$port/#p=wid&build=$id"
check "pass: in the job, HEAD is the pushed commit" has <(build_log "$id") "HEAD $a"
check "pass: report.md, titled with the push" same "$(head -1 "$d/builds/$id/report.md")" "# CI report: acme/wid · main ${a:0:7} · quick · passed"
check "pass: the report's meta line, with act's version" bash -c "[[ '$(sed -n 3p "$d/builds/$id/report.md")' == 'Build #$id on '*' · act '[0-9]* ]]"
check "pass: every check of work passed" same "$(report_row "$id" work)" "| work | 100% (3/3) | — | |"
check "pass: a job not run is no pass" same "$(report_row "$id" '\*\*all\*\*')" "| **all** | 100% (7/7) | — | 1 |"
check "pass: and says why" has "$d/builds/$id/report.md" "- never: skipped"
clean pass

# ---- 2. a push that fails -----------------------------------------------------------------
say "2. a push that fails"
s=$(date +%s)
b=$(push fail "fails")
until_ok 600 "build of $b" finished "$b"
until_ok 60 "the daemon idle" idle
id=$(builds_of "$b" | tail -n 1 | cut -d'|' -f1)
echo "   build #$id: $(state_of "$b") in $(($(date +%s) - s)) s"
check "fail: the build failed" same "$(state_of "$b")" failure
check "fail: bana pending first" same "$(posts "$b" | head -n 1 | cut -d'|' -f1-2)" "bana|pending"
check "fail: bana failure, naming the job and step" bash -c "[[ '$(last "$b" bana)' == 'failure|broken failed at \"test\" · '* ]]" ||
  echo "  got: $(last "$b" bana)" >&2
check "fail: bana/broken failed at test" bash -c "[[ '$(last "$b" bana/broken)' == 'failure|failed at \"test\" after '* ]]"
check "fail: bana/linux passed" same "$(last "$b" bana/linux | cut -d'|' -f1)" success
check "fail: the push's before (the last green) is in the job's history" has <(build_log "$id") "before $a reachable"
check "fail: in the job, HEAD is the pushed commit" has <(build_log "$id") "HEAD $b"
check "fail: report.md: broken's checks and cargo's count, which cargo cut short" same "$(report_row "$id" tests)" \
  "| tests | 50% (1/2) | 50% of 2 run (incomplete) | |"
check "fail: tests by name" same "$(report_row "$id" answers)" "| answers | — | 50% of 2 run (incomplete) | |"
check "fail: the failing test, and where" has "$d/builds/$id/report.md" "- \`tests::the_answer\` at src/lib.rs:9:5: "
check "fail: and cargo's rerun" has "$d/builds/$id/report.md" "- Rerun: \`cargo test --lib\`"
check "fail: the page's report is report.md" same "$(api "/builds/$id/report" | jq_ 'j["markdown"]')" "$(cat "$d/builds/$id/report.md")"
check "fail: with its rows" same "$(api "/builds/$id/report" | jq_ '[r["name"] for r in j["standards"]]')" "['work', 'tests', 'answers', 'all']"
check "fail: the history's chip" same "$(api /builds | jq_ '[b.get("tests") for b in j["builds"] if b["id"] == '"$id"'][0]')" "tests 50% (incomplete)"
check "fail: bana report $id prints it" same "$(cd "$w" && bash "$bana" report "$id" 2>&1)" "$(cat "$d/builds/$id/report.md")"
clean fail

# ---- 2b. Fix with Claude on the failed build ------------------------------------------------
say "2b. Fix with Claude on build #$id"
sha7=${b:0:7}
wt=$d/fix/$sha7
check "fix: the summary offers build #$id" same "$(api /local | jq_ 'j["failed"]')" "$id"
s=$(date +%s)
asked=$(wc -l <"$FAKE_LOG")
fixed=$(api "/builds/$id/fix" -X POST)
echo "   fix $sha7 made in $(($(date +%s) - s)) s"
fx() { jq_ "$1" <<<"$fixed"; }
check "fix: named for the failing commit" same "$(fx '"%s %s %s" % (j["fix"], j["branch"], j["reused"])')" "$sha7 bana/fix-$sha7 False"
check "fix: its worktree, in the daemon's directory" same "$(fx 'j["worktree"]')" "$wt"
check "fix: at the failing commit" same "$(git -C "$wt" rev-parse HEAD)" "$b"
check "fix: on its own branch" same "$(git -C "$wt" symbolic-ref HEAD)" "refs/heads/bana/fix-$sha7"
check "fix: a worktree of the checkout" has <(git -C "$w" worktree list --porcelain) "worktree $wt"
check "fix: its Claude Code settings deny git push" has "$wt/.claude/settings.local.json" 'Bash(git push:*)'
check "fix: which git leaves out" same "$(git -C "$wt" status --porcelain)" ""
check "fix: the checkout stays as it was" same "$(git -C "$w" symbolic-ref HEAD)|$(git -C "$w" status --porcelain)" "refs/heads/main|"
check "fix: the prompt names the failing test" has "$d/fix/$sha7.d/prompt.txt" "\`tests::the_answer\` panicked at \`src/lib.rs:9:5\`"
check "fix: and cargo's rerun" has "$d/fix/$sha7.d/prompt.txt" "Rerun: \`cargo test --lib\`"
check "fix: the link opens Claude Code in the worktree, the prompt typed" same "$(fx 'j["link"].startswith("claude-cli://open?") and (
  lambda q: q["cwd"] == [j["worktree"]] and q["q"] == [open(j["dir"] + "/prompt.txt").read()])(
  __import__("urllib.parse").parse.parse_qs(j["link"].split("?", 1)[1]))')" True
fixed=$(api "/builds/$id/fix" -X POST)
check "fix: a second click goes on with it" same "$(fx '"%s %s" % (j["worktree"], j["reused"])')" "$wt True"
check "fix: its branch has nothing on it yet" same "$(api "/fixes/$sha7" | jq_ 'j["ahead"]')" 0
# The HTTP status of a POST to project P's route (a GET: -X GET).
posted_status() { # PATH [CURL-OPTIONS...]
  local p=$1
  shift
  printf 'Authorization: Bearer %s\n' "$(cat "$HOME/.bana/manager-token")" |
    curl -sS --noproxy '*' --max-time 10 -H @- -o /dev/null -w '%{http_code}' -X POST "$@" "http://127.0.0.1:$port/ci/v1/p/$P$p"
}
check "fix: none for a build that passed (409)" same "$(posted_status "/builds/$(builds_of "$a" | tail -n 1 | cut -d'|' -f1)/fix")" 409
check "fix: none for a build that is not there (404)" same "$(posted_status /builds/9999/fix)" 404
check "fix: nothing asked of GitHub" same "$(wc -l <"$FAKE_LOG")" "$asked"
check "fix: its settings let Claude run the jobs through bana" has "$wt/.claude/settings.local.json" 'mcp__bana__run_jobs'
check "fix: and not commit without asking" not has "$wt/.claude/settings.local.json" 'mcp__bana__commit_fix' 2>/dev/null
check "fix: and hold Claude's stop with bana's gate" has "$wt/.claude/settings.local.json" " fix gate --dir "

# ---- 2c. the fix loop, the test as Claude Code ---------------------------------------------
say "2c. round 0, then run_jobs until green, commit_fix and bana fix push"
posted_b=$(posts "$b" | wc -l)
s=$(date +%s)
until_ok 600 "round 0 of fix $sha7" round_ended "$sha7" 0
echo "   round 0: $(api "/fixes/$sha7/rounds/0" | jq_ 'j["state"]') $(($(date +%s) - s)) s after the fix"
r0=$(api "/fixes/$sha7/rounds/0")
check "loop: round 0 ran the failed job at the failing commit, and failed again" same \
  "$(jq_ '"%s %s %s" % (j["sha"], j["jobs"], j["state"])' <<<"$r0")" "$b ['broken'] failure"
check "loop: round 0's failure, in the brief's shape" same \
  "$(jq_ '"%s %s" % (j["failures"][0]["job"], j["failures"][0]["step"])' <<<"$r0")" "broken test"

r=$(tool "$wt" fix_brief '{}') || r='{}'
check "loop: fix_brief names the fix, its branch and worktree" same \
  "$(jq_ '"%s %s %s %s" % (j["fix"], j["base_sha"], j["branch"], j["worktree"])' <<<"$r")" "$sha7 $b bana/fix-$sha7 $wt"
check "loop: fix_brief has the failing test" same "$(jq_ '[t["name"] for t in j["failures"][0]["tests"]]' <<<"$r")" "['tests::the_answer']"
check "loop: fix_brief has round 0's result and the rounds" same \
  "$(jq_ '"%s %s %s %s" % (j["recheck"]["state"], j["rounds"]["used"], j["rounds"]["max"], j["rounds"]["left"])' <<<"$r")" "failure 0 5 5"
r=$(tool "$wt" ci_report '{}') || r='{}'
check "loop: ci_report: the fix's build's report" same "$(jq_ '"%s %s" % (j["build"], j["markdown"] == open("'"$d/builds/$id/report.md"'").read())' <<<"$r")" "$id True"
check "loop: ci_report: and its rows" same \
  "$(jq_ '"%(passed)s %(failed)s %(skipped)s %(incomplete)s" % j["standards"][1]["tests"]' <<<"$r")" "1 1 0 True"

# Claude's first try changes the test's output but not its outcome, and adds a file.
sed -i.bak 's/left: 41/left: 40/' "$wt/ci/cargo-test.txt" && rm -f "$wt/ci/cargo-test.txt.bak"
echo "why it failed" >"$wt/notes.txt"
check "loop: the gate holds Claude's first stop on an untested change (exit 2)" same "$(stop_hook "$wt")" 2
check "loop: but only once for that tree" same "$(stop_hook "$wt")" 0
s=$(date +%s)
r=$(tool "$wt" run_jobs '{}') || r='{}'
took=$(($(date +%s) - s))
echo "   round 1: $(jq_ 'j.get("state")' <<<"$r") in $took s, $(jq_ 'j.get("progress")' <<<"$r") progress notes"
# It says how the round goes after each ask of the daemon (every 15 s).
((took < 20)) || check "loop: run_jobs says how it goes while it waits" test "$(jq_ 'j["progress"]' <<<"$r")" -ge 1
check "loop: run_jobs runs round 1, red" same \
  "$(jq_ '"%s %s %s %s" % (j["round"], j["state"], j["green"], j["isError"])' <<<"$r")" "1 failure False False"
check "loop: it says what failed" same "$(jq_ '[f["job"] for f in j["failures"]]' <<<"$r")" "['broken']"
check "loop: and which new file it took in" same "$(jq_ 'j["new_files"]' <<<"$r")" "['notes.txt']"
check "loop: 4 rounds left" same "$(jq_ 'j["rounds_left"]' <<<"$r")" 4
snap1=$(jq_ 'j["snapshot"]' <<<"$r")
round1=$(jq_ 'j["builds"][0]["build"]' <<<"$r")
check "loop: the snapshot is the worktree on the failing commit, which git leaves alone" same \
  "$(git -C "$wt" rev-parse "$snap1^")|$(git -C "$wt" status --porcelain | tr '\n' ' ')" "$b| M ci/cargo-test.txt ?? notes.txt "
r=$(tool "$wt" ci_log "{\"build\": $round1, \"job\": \"broken\", \"grep\": \"left:\"}") || r='{}'
check "loop: ci_log shows round 1 ran the edited worktree" same \
  "$(jq_ 'any("left: 40" in l for l in j["lines"]) and not any("left: 41" in l for l in j["lines"])' <<<"$r")" True
r=$(tool "$wt" fix_status '{}') || r='{}'
check "loop: fix_status: red, and the worktree is what round 1 ran" same \
  "$(jq_ '"%s %s" % (j["state"], j["changed_since_last_round"])' <<<"$r")" "red False"

# The second try fixes it.
echo pass >"$wt/ci/mode"
r=$(tool "$wt" fix_status '{}') || r='{}'
check "loop: fix_status sees the untested change" same "$(jq_ 'j["changed_since_last_round"]' <<<"$r")" True
s=$(date +%s)
r=$(tool "$wt" run_jobs '{}') || r='{}'
echo "   round 2: $(jq_ 'j.get("state")' <<<"$r") in $(($(date +%s) - s)) s"
check "loop: run_jobs runs round 2, green" same "$(jq_ '"%s %s %s" % (j["round"], j["state"], j["green"])' <<<"$r")" "2 success True"
check "loop: and says to commit" has <(jq_ 'j["next"]' <<<"$r") "commit_fix"
green_tree=$(jq_ 'j["tree"]' <<<"$r")
r=$(tool "$wt" run_jobs '{}') || r='{}'
check "loop: the same tree again gets round 2 back, without a build" same \
  "$(jq_ '"%s %s %s" % (j["round"], j["reused"], j["green"])' <<<"$r")" "2 True True"
check "loop: the gate lets Claude stop once the tree is tested" same "$(stop_hook "$wt")" 0
check "loop: the history shows the fix's rounds, per job" same "$(round_builds "$sha7")" \
  "0:broken:fix:failure 1:broken:fix:failure 2:broken:fix:success "

r=$(tool "$wt" commit_fix '{"message": "Make the answer 42\n\nci/mode said fail."}') || r='{}'
check "loop: commit_fix refuses the new file it was not told to take" same \
  "$(jq_ 'j["isError"] and "notes.txt" in j["text"]' <<<"$r")" True
r=$(tool "$wt" commit_fix '{"message": "Make the answer 42\n\nci/mode said fail.", "include_new_files": true}') || r='{}'
check "loop: commit_fix commits on the fix's branch" same \
  "$(jq_ '"%s %s %s" % (j["isError"], j["branch"], j["round"])' <<<"$r")" "False bana/fix-$sha7 2"
kept=$(jq_ 'j["commit"]' <<<"$r")
check "loop: the branch's tip is that commit, on the failing one" same \
  "$(git -C "$w" rev-parse "bana/fix-$sha7") $(git -C "$w" rev-parse "bana/fix-$sha7^")" "$kept $b"
check "loop: the tip's tree is the green round's" same "$(git -C "$w" rev-parse "bana/fix-$sha7^{tree}")" "$green_tree"
check "loop: the worktree is clean on it" same "$(git -C "$wt" status --porcelain)" ""
check "loop: the checkout stays as it was" same "$(git -C "$w" symbolic-ref HEAD)|$(git -C "$w" status --porcelain)" "refs/heads/main|"
r=$(tool "$wt" fix_status '{}') || r='{}'
check "loop: fix_status: kept" same "$(jq_ '"%s %s" % (j["state"], j["commits"])' <<<"$r")" "kept 1"
for x in $(round_shas "$sha7"); do
  check "loop: nothing posted for round snapshot ${x:0:7}" same "$(posts "$x")" ""
done
check "loop: nor for the failing commit since the fix" same "$(posts "$b" | wc -l)" "$posted_b"
check "loop: the rounds never reached the origin" same "$(git -C "$T/origin.git" for-each-ref refs/bana)" ""
check "loop: they are in the daemon's clone only, with the base round 0 ran" same \
  "$(git -C "$d/src" rev-parse "refs/bana/fix/$sha7/base" "refs/bana/fix/$sha7/${snap1:0:7}" | tr '\n' ' ')" "$b $snap1 "
clean "fix rounds"

# The owner's push: a push like any other, which the daemon builds and posts.
out=$(cd "$w" && bash "$bana" fix push "$sha7" 2>&1) || echo "$out" >&2
check "push: bana fix push reaches the origin" same "$(git -C "$T/origin.git" rev-parse "refs/heads/bana/fix-$sha7" 2>/dev/null)" "$kept"
(cd "$w" && bash "$bana" daemon poke >/dev/null)
until_ok 600 "build of $kept" finished "$kept"
until_ok 60 "the daemon idle" idle
check "push: the daemon built bana/fix-$sha7 as a push, and it passed" same "$(builds_of "$kept" | cut -d'|' -f2,3)" "success|push"
check "push: with statuses: bana pending, then success" same \
  "$(posts "$kept" | head -n 1 | cut -d'|' -f1-2) $(last "$kept" bana | cut -d'|' -f1)" "bana|pending success"
check "push: bana/broken passes" same "$(last "$kept" bana/broken | cut -d'|' -f1)" success
r=$(tool "$wt" fix_status '{}') || r='{}'
check "push: fix_status: pushed" same "$(jq_ 'j["state"]' <<<"$r")" pushed
clean "fix push"

# ---- 3. a [skip ci] push ----------------------------------------------------------------------
say "3. a [skip ci] push"
c=$(push pass "docs only [skip ci]")
until_ok 60 "the push seen" bash -c "grep -q '$c' '$d/state.json'"
sleep 3
check "skip: no build" same "$(builds_of "$c")" ""
check "skip: nothing posted" same "$(posts "$c")" ""
check "skip: remembered as skipped" same "$(api /local | jq_ '[s["why"] for s in j["skipped"] if s["sha"] == "'"$c"'"]')" "['skip marker']"
clean skip

# ---- 4. a long step, cancelled from the API -------------------------------------------------------
say "4. a sleep 600, cancelled through the API"
rm -f "$T/survivor" "$T/host-sleeping"
e=$(push sleep "sleeps")
until_ok 300 "the long steps" bash -c "[[ -s '$T/host-sleeping' && -s '$T/survivor' ]]"
until_ok 120 "the linux job running" running_job "$e" linux
id=$(builds_of "$e" | tail -n 1 | cut -d'|' -f1)
survivor=$(cat "$T/survivor")
act_pid=$(jq_ 'j["pid"]' <"$d/builds/$id/build.json")
check "cancel: a process left behind by a host step runs" alive "$survivor"
if [[ $docker_mode == 1 ]]; then
  check "cancel: the job containers carry the daemon's label" test -n "$(containers)"
fi
check "cancel: it has the build's marker" has_marker "$survivor"
if [[ $os == Darwin ]]; then
  check "cancel: caffeinate waits on act" bash -c "pgrep -f 'caffeinate -i -w $act_pid' >/dev/null"
fi
s=$(date +%s)
api "/builds/$id/cancel" -X POST >/dev/null
until_ok 200 "build $id ended" finished "$e"
until_ok 60 "the daemon idle" idle
took=$(($(date +%s) - s))
echo "   build #$id: $(state_of "$e") ($(builds_of "$e" | tail -n 1 | cut -d'|' -f5)) ${took} s after the cancel"
check "cancel: the build ended as error, cancelled" same "$(builds_of "$e" | tail -n 1 | cut -d'|' -f2,5)" "error|cancelled from the page"
check "cancel: within the ladder's first rung (60 s)" test "$took" -lt 60
check "cancel: bana says so" same "$(last "$e" bana)" "error|cancelled from the page"
check "cancel: no fix for a cancelled build (409)" same "$(posted_status "/builds/$id/fix")" 409
check "cancel: no pending left on any context" same "$(for x in $(contexts "$e"); do last "$e" "$x" | cut -d'|' -f1; done | grep -c pending || true)" 0
check "cancel: act is gone" not alive "$act_pid"
check "cancel: so is what its host step left behind (the marker sweep)" not alive "$survivor"
if [[ $os == Darwin ]]; then
  check "cancel: caffeinate exited with act" bash -c "! pgrep -f 'caffeinate -i -w $act_pid' >/dev/null"
fi
clean cancel

# ---- 5. kill -9 of the daemon mid-build, and a restart -----------------------------------------
say "5. kill -9 of the daemon mid-build, then a restart"
rm -f "$T/host-holding"
touch "$hold"
f=$(push hold "holds")
until_ok 300 "the host job holding" test -s "$T/host-holding"
id=$(builds_of "$f" | tail -n 1 | cut -d'|' -f1)
act_pid=$(jq_ 'j["pid"]' <"$d/builds/$id/build.json")
kill -9 "$daemon_pid"
wait "$daemon_pid" 2>/dev/null || true
daemon_pid=''
check "kill -9: act runs on without the daemon" alive "$act_pid"
check "kill -9: and keeps the lock" same "$(sed -n 1p "$HOME/.bana/act.lock/owner" 2>/dev/null)" "$act_pid"
s=$(date +%s)
start_daemon
until_ok 120 "the retry" bash -c "grep -q 'build $id was interrupted; retried as build' '$T/daemon.log'"
rm -f "$hold"
until_ok 600 "the retry finished" finished "$f"
until_ok 60 "the daemon idle" idle
echo "   builds of $f: $(builds_of "$f" | tr '\n' ' ')($(($(date +%s) - s)) s after the restart)"
check "kill -9: the old act was stopped at the restart" not alive "$act_pid"
check "kill -9: the interrupted build, then its retry (attempt 2), which passed" same \
  "$(builds_of "$f" | cut -d'|' -f2-)" "$(printf 'error|push|1|interrupted (bana restarted)\nsuccess|retry|2|')"
check "kill -9: bana ends as the retry's success" same "$(last "$f" bana | cut -d'|' -f1)" success
check "kill -9: no pending left on any context" same "$(for x in $(contexts "$f"); do last "$f" "$x" | cut -d'|' -f1; done | grep -c pending || true)" 0
(cd "$w" && bash "$bana" daemon poke >/dev/null)
sleep 3
until_ok 60 "the daemon idle" idle
check "kill -9: retried exactly once" same "$(builds_of "$f" | wc -l | tr -d ' ')" 2
clean "kill -9"

# ---- 6. a hand bana ci that fails, then bana fix last -----------------------------------------
say "6. a hand bana ci that fails, bana fix last, run_jobs"
# The stand-in Claude Code: bana fix ends by starting it, which runs no prompt here.
ln -s "$here/stand-ins/claude" "$T/bin/claude"
echo fail >"$w/ci/mode"
git -C "$w" commit -q -am "fails, by hand"
h=$(git -C "$w" rev-parse HEAD)
h7=${h:0:7}
s=$(date +%s)
# --pull=false as the daemon's builds: act would pull the image again, and Docker Hub
# answers repeated pulls with 429 (the run then fails in a second or two).
out=$(cd "$w" && bash "$bana" ci quick -j broken -- --pull=false 2>&1) && code=0 || code=$?
echo "   bana ci: exit $code in $(($(date +%s) - s)) s"
check "hand: bana ci failed" test "$code" -ne 0
check "hand: and points to bana fix" has <(printf '%s\n' "$out") "bana fix: hand this failure"
check "hand: it kept its log" has "$d/ci/last.log" "tests::the_answer"
check "hand: and what ran" has "$d/ci/last.env" "sha=$h"
check "hand: it ends with the CI report's table" has <(printf '%s\n' "$out") "| tests | 50% (1/2) | 50% of 2 run (incomplete) | |"
check "hand: and keeps the report" same "$(head -1 "$d/ci/last.report.md")" "# CI report: acme/wid · main ${h:0:7} · quick · failed"
out=$(cd "$w" && bash "$bana" fix last 2>&1) && code=0 || code=$?
[[ $code == 0 ]] || echo "$out" >&2
wt=$d/fix/$h7
check "hand: bana fix last makes the fix at the commit that failed" same "$(git -C "$wt" rev-parse HEAD 2>/dev/null)" "$h"
check "hand: and starts Claude Code in its worktree" same "$(cat "$FAKE_STATE/claude.cwd" 2>/dev/null)" "$wt"
# Claude Code had no bana server registered here (bana add found no claude on PATH), so bana
# fix passes it one: the test starts that one, as Claude Code would.
mcp_server=$(tr '\0' '\n' <"$FAKE_STATE/claude.args" | sed -n '/^--mcp-config$/{n;p;}' | jq_ 'json.dumps(j["mcpServers"]["bana"])')
check "hand: with bana's MCP server" same "$(jq_ '" ".join([j["command"]] + j["args"])' <<<"$mcp_server")" "$HOME/.bana/daemon.d/bana-manager mcp --dir $d"
check "hand: and the prompt, which says to test with run_jobs" has <(tr '\0' '\n' <"$FAKE_STATE/claude.args") "run_jobs"
r=$(tool "$wt" fix_brief '{}') || r='{}'
check "hand: fix_brief: a hand run of broken, with its failing test" same \
  "$(jq_ '"%s %s %s" % (j["fix"], [f["job"] for f in j["failures"]], [t["name"] for t in j["failures"][0]["tests"]])' <<<"$r")" \
  "$h7 ['broken'] ['tests::the_answer']"
echo pass >"$wt/ci/mode"
s=$(date +%s)
r=$(tool "$wt" run_jobs '{}') || r='{}'
echo "   round $(jq_ 'j.get("round")' <<<"$r"): $(jq_ 'j.get("state")' <<<"$r") in $(($(date +%s) - s)) s"
check "hand: run_jobs is green, in the daemon, for a commit it never saw pushed" same \
  "$(jq_ '"%s %s %s" % (j["isError"], j["state"], j["green"])' <<<"$r")" "False success True"
check "hand: its snapshot is on the failing commit" same "$(git -C "$wt" rev-parse "$(jq_ 'j["snapshot"]' <<<"$r")^")" "$h"
for x in $(round_shas "$h7"); do
  check "hand: nothing posted for round snapshot ${x:0:7}" same "$(posts "$x")" ""
done
check "hand: nor for the commit" same "$(posts "$h")" ""
until_ok 60 "the daemon idle" idle
clean "hand fix"

# ---- 7. a green build's files, and an install from them -------------------------------------
say "7. a package job's uploads: the build's files, and its installer"
# A two-leg matrix packs demo for each CPU (the same script: act builds both on this one) and
# uploads the archive with its .sha256 through upload-artifact@v4. Host jobs on a Mac pack
# demo-macos-*, which the Mac's installer takes.
cat >>"$w/.github/workflows/ci.yml" <<'EOF'
  package:
    runs-on: [self-hosted, wid-linux]
    strategy:
      matrix:
        arch: [x64, arm64]
    steps:
      - uses: actions/checkout@v4
      - name: pack
        run: sh ci/pack.sh ${{ matrix.arch }}
      - uses: actions/upload-artifact@v4
        with:
          name: demo-${{ runner.os == 'macOS' && 'macos' || 'linux' }}-${{ matrix.arch }}
          path: out/
EOF
cat >"$w/ci/pack.sh" <<'EOF'
# out/demo-OS-ARCH.tar.gz, one directory with bin/demo and the hook; and its .sha256.
case $(uname -s) in Darwin) d=demo-macos-$1 ;; *) d=demo-linux-$1 ;; esac
mkdir -p "out/$d/bin"
printf '#!/bin/sh\necho "demo works (%s)"\n' "$1" >"out/$d/bin/demo"
chmod +x "out/$d/bin/demo"
printf 'mkdir -p "$WID_LOG_DIR" && echo "$1 $INSTALL_TAG" >>"$WID_LOG_DIR/hook.log"\n' >"out/$d/hook.sh"
tar -C out -czf "out/$d.tar.gz" "$d"
rm -rf "out/$d"
(cd out && { sha256sum "$d.tar.gz" 2>/dev/null || shasum -a 256 "$d.tar.gz"; } >"$d.tar.gz.sha256")
EOF
cat >>"$w/.github/bana.conf" <<'EOF'
install.bins = demo
install.hook = hook.sh
install.env.WID_LOG_DIR = ~/wid-logs
EOF
g=$(push pass "packages")
until_ok 900 "build of $g" finished "$g"
until_ok 60 "the daemon idle" idle
id=$(builds_of "$g" | tail -n 1 | cut -d'|' -f1)
dist=$d/builds/$id/dist
label=quick-${g:0:10}
# What the jobs packed, and the build this machine's installer takes.
pos=linux && [[ $os == Darwin ]] && pos=macos
cpu=x64 && [[ $(uname -m) == arm64 || $(uname -m) == aarch64 ]] && cpu=arm64
names() { find "$1" -mindepth 1 -maxdepth 1 2>/dev/null | sed 's|.*/||' | LC_ALL=C sort | tr '\n' ' '; }
echo "   build #$id: $(state_of "$g"), files: $(names "$dist")"
check "files: the build passed" same "$(state_of "$g")" success
check "files: its dist: both archives, the installer and SHA256SUMS" same "$(names "$dist")" \
  "SHA256SUMS demo-$pos-arm64.tar.gz demo-$pos-x64.tar.gz install.sh "
check "files: SHA256SUMS holds for them" bash -c "cd '$dist' && sha256sum -c --quiet SHA256SUMS"
check "files: the installer is this build's (a label: --from only)" has "$dist/install.sh" "TAG='$label'"
check "files: the zips act kept are gone" test ! -e "$d/builds/$id/artifacts/$id"
check "files: the build lists them" same \
  "$(api "/builds/$id" | jq_ '" ".join("%s:%s" % (f["name"], f.get("platform") or "") for f in j["dist"]["files"])')" \
  "SHA256SUMS: demo-$pos-arm64.tar.gz:$pos-arm64 demo-$pos-x64.tar.gz:$pos-x64 install.sh:"
check "files: without a problem" same "$(api "/builds/$id" | jq_ 'j["dist"].get("problem")')" ""
check "files: the history counts them" same "$(api /builds | jq_ '[b.get("files") for b in j["builds"] if b["id"] == '"$id"'][0]')" 4
check "files: a download is the file" cmp -s <(api "/builds/$id/files/demo-$pos-x64.tar.gz") "$dist/demo-$pos-x64.tar.gz"
check "files: nothing else is served" not api "/builds/$id/files/..%2Fbuild.json" -o /dev/null
check "files: the report lists the artifacts" has "$d/builds/$id/report.md" "- \`demo-$pos-x64\` (package (x64), "
check "files: the page has its Files section" bash -c "curl -fsS --noproxy '*' 'http://127.0.0.1:$port/' | grep -q 'id=\"files-table\"'"
# Installed from the build's own files, under a home of its own.
out=$(env HOME="$T/inst" XDG_DATA_HOME= XDG_BIN_HOME= XDG_CONFIG_HOME= sh "$dist/install.sh" --from "$dist" --yes 2>&1) && code=0 || code=$?
[[ $code == 0 ]] || echo "$out" >&2
check "install: sh dist/install.sh --from dist --yes" same "$code" 0
check "install: demo in the bin it links, and it works" same "$("$T/inst/.local/bin/demo" 2>&1)" "demo works ($cpu)"
check "install: the hook ran, with install.env" same "$(tr '\n' ' ' <"$T/inst/wid-logs/hook.log" 2>/dev/null)" \
  "pre-install $label post-install $label "
check "install: a receipt" has "$T/inst/.local/share/wid/receipt" "tag=$label"
out=$(cd "$w" && bash "$bana" install "$id" --yes 2>&1) && code=0 || code=$?
[[ $code == 0 ]] || echo "$out" >&2
check "install: bana install $id, in this home" same "$code|$("$HOME/.local/bin/demo" 2>&1)" "0|demo works ($cpu)"
out=$(cd "$w" && bash "$bana" install "$id" --uninstall --yes 2>&1) && code=0 || code=$?
[[ $code == 0 ]] || echo "$out" >&2
check "install: and --uninstall takes it away" same "$code|$(ls "$HOME/.local/bin" 2>/dev/null)" "0|"
clean files

# ---- 8. a release: the tag builds, bana asks, and publishes on the owner's yes ---------------
say "8. a release: v0.1.0 is pushed, built, asked about, and published"
git -C "$w" tag -a v0.1.0 -m "wid 0.1.0"
git -C "$w" push -q origin v0.1.0
(cd "$w" && bash "$bana" daemon poke >/dev/null)
rel() { api /releases/v0.1.0 | jq_ "$1"; }
asking() { [[ $(rel 'j["state"]') == asking && $(rel 'j["seeded"]') == True ]]; }
answered() { [[ $(rel 'j["state"]') =~ ^(asking|blocked)$ && $(rel 'j["seeded"]') == True ]]; }
until_ok 900 "v0.1.0 asked about, or blocked" answered
if [[ $(rel 'j["state"]') == blocked ]]; then
  # Two legs that fetch one action at once can race in act's action cache: a re-run takes
  # the release over.
  echo "   v0.1.0 blocked: $(rel 'j["reason"]'); re-running build #$(rel 'j["build"]["id"]')"
  api "/builds/$(rel 'j["build"]["id"]')/rerun" -X POST >/dev/null
  check "release: a re-run takes it over" same "$(rel 'j["state"]')" building
  until_ok 900 "v0.1.0 asked about" asking
fi
until_ok 60 "the daemon idle" idle
id=$(rel 'j["build"]["id"]')
dist=$d/builds/$id/dist
check "release: its build is the tag's, at the tag tier" same "$(rel '"%s %s %s" % (j["build"]["ref"], j["build"]["tier"], j["build"]["state"])')" "v0.1.0 nightly success"
check "release: the installer is the tag's" has "$dist/install.sh" "TAG='v0.1.0'"
check "release: bana asks (the summary)" same "$(api /local | jq_ '"%s %s" % (j["release"]["tag"], j["release"]["state"])')" "v0.1.0 asking"
check "release: a first release, found with gh" same "$(rel '"%s|%s" % (j["previous"]["tag"] or "", j["previous"]["how"])')" "|gh release list"
check "release: git's notes list the pushes" same "$(rel '"packages" in j["notes"]["text"] and j["notes"]["source"]')" git
check "release: the report's table, for Tested" has <(rel 'j["tested"]') "| Standard |"
check "release: nothing was written on GitHub yet" same "$(grep -cE 'gh release (create|edit|upload|delete) [^-]' "$FAKE_LOG" || true)" 0
check "bana daemon status: waiting for your answer" has <(cd "$w" && bash "$bana" daemon status) "release v0.1.0: waiting for your answer"
rev=$(rel 'j["notes"]["rev"]')
code=$(api /releases/v0.1.0/notes -o /dev/null -w '%{http_code}' -X PUT -H 'content-type: application/json' \
  -d '{"notes":"The first wid.\n","rev":0}' 2>/dev/null || true)
check "release: a stale rev is refused" same "$code" 409
# Claude's side, over MCP stdio as Claude Code drives it: what bana knows, GitHub's
# pull requests and notes (reads), then the notes saved for the owner to review.
r=$(tool "$w" release_context '{}') || r='{}'
check "mcp: release_context: the release bana asks about, and its rev" same \
  "$(jq_ '"%s %s %s %s" % (j["tag"], j["state"], j["notes"]["rev"], j["changes"]["counts"]["commits"] > 0)' <<<"$r")" "v0.1.0 asking $rev True"
r=$(tool "$w" pull_requests '{"numbers": [1]}') || r='{}'
check "mcp: pull_requests: one query, and what GitHub lacks" same "$(jq_ 'j["missing"]' <<<"$r")" "[1]"
check "mcp: pull_requests: through gh api graphql" has "$FAKE_LOG" "gh api graphql -f query=query("
r=$(tool "$w" github_notes '{"tag": "v0.1.0"}') || r='{}'
check "mcp: github_notes: GitHub's, for the tested commit" has "$FAKE_LOG" \
  "gh api -X POST repos/acme/wid/releases/generate-notes -f tag_name=v0.1.0 -f target_commitish=$(rel 'j["sha"]')"
check "mcp: github_notes: a first release" same "$(jq_ '"%s %s" % (j["previous_tag"], "commits/v0.1.0" in j["body"])' <<<"$r")" "None True"
r=$(tool "$w" save_release_notes '{"tag": "v0.1.0", "notes": "The first wid.\n", "rev": '"$rev"'}') || r='{}'
saved=$(jq_ 'j["rev"]' <<<"$r")
check "mcp: save_release_notes: saved over the rev read" same "$saved" "$((rev + 1))"
check "release: the page shows them as Claude's" same "$(rel '"%s|%s" % (j["notes"]["source"], j["notes"]["text"])')" "claude|The first wid."
r=$(tool "$w" save_release_notes '{"tag": "v0.1.0", "notes": "x", "rev": '"$rev"'}') || r='{}'
check "mcp: save_release_notes: a stale rev is Claude's error" same "$(jq_ '"%s %s" % (j["isError"], "changed since rev" in j.get("text", ""))' <<<"$r")" "True True"
check "release: and no tool wrote on GitHub" same "$(grep -cE 'gh release (create|edit|upload|delete) [^-]' "$FAKE_LOG" || true)" 0
code=$(api /releases/v0.1.0/publish -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -d "{\"rev\":$saved}")
check "release: Publish, with that rev" same "$code" 202
published() { [[ $(rel 'j["state"]') == published ]]; }
until_ok 120 "v0.1.0 published" published
check "release: published, with its URL" same "$(rel 'j["url"]')" "https://github.com/acme/wid/releases/tag/v0.1.0"
check "release: gh release create --verify-tag, from dist" has "$FAKE_LOG" "gh release create v0.1.0 -R acme/wid --verify-tag --title wid v0.1.0 --notes-file $d/releases/v0.1.0.notes.md --latest "
check "release: its files are SHA256SUMS's, and SHA256SUMS" same "$(tr '\n' ' ' <"$FAKE_RELEASE_STORE/v0.1.0/assets")" \
  "$(awk '{ print $2 }' "$dist/SHA256SUMS" | tr '\n' ' ')SHA256SUMS "
check "release: its notes, then Tested and Install" same "$(grep -E '^## |^The first' "$FAKE_RELEASE_STORE/v0.1.0/notes" | tr '\n' '|')" \
  "The first wid.|## Tested|## Install|"
check "release: the ask is over" same "$(api /local | jq_ 'j["release"]')" ""
clean release

# ---- 9. a second project, added while the daemon runs ----------------------------------------
say "9. a second project: added while the daemon runs, one build at a time, pause, remove"
d2=$HOME/.bana/two
w2=$T/two
hold2=$T/hold2
git init -q --bare "$T/two.git"
cat >>"$HOME/.gitconfig" <<EOF
[url "file://$T/two.git"]
  insteadOf = https://github.com/acme/two.git
EOF
mkdir -p "$w2/.github/workflows" "$w2/ci"
git -C "$w2" init -q
git -C "$w2" remote add origin "https://github.com/acme/two.git"
# One host job, quick unless the file ci/hold names is there. A poll an hour apart: only its
# push hook (or bana daemon poke) brings its pushes to the daemon in time.
cat >"$w2/.github/bana.conf" <<EOF
repo = acme/two
prefix = two
tiers = quick nightly
daemon.poll = 3600
daemon.token = none
act.args = -P two-host=-self-hosted
act.image = ${image:-none}
EOF
cat >"$w2/.github/workflows/ci.yml" <<'EOF'
name: ci
on:
  workflow_dispatch:
    inputs:
      tier:
        default: quick
jobs:
  host:
    runs-on: [self-hosted, two-host]
    steps:
      - uses: actions/checkout@v4
      - name: work
        run: sh ci/step.sh
EOF
cat >"$w2/ci/step.sh" <<'EOF'
t=$(cat ci/t)
echo "$$" >"$t/two-running"
n=0
while [ -e "$(cat ci/hold)" ] && [ $n -lt 600 ]; do sleep 1; n=$((n + 1)); done
echo "two: done ($(cat ci/push))"
EOF
echo "$T" >"$w2/ci/t"
echo "$hold2" >"$w2/ci/hold"
echo first >"$w2/ci/push"
git -C "$w2" add -A
git -C "$w2" commit -q -m "the second project"
git -C "$w2" push -q origin HEAD:main
git -C "$T/two.git" symbolic-ref HEAD refs/heads/main
push2() { # MESSAGE: commits and pushes, with no poke (the hook pokes); prints the sha
  echo "$1" >"$w2/ci/push"
  git -C "$w2" commit -q -am "$1"
  git -C "$w2" push -q origin HEAD:main
  git -C "$w2" rev-parse HEAD
}
# SHA's builds of TRIGGER: their ids, a line each.
ids_of() { builds_of "$1" | awk -F'|' -v t="$2" '$3 == t { print $1 }'; } # SHA TRIGGER
# Build ID of project P, from the history: TRIGGER SHA STATE.
row_of() { api /builds | jq_ '[" ".join([b["trigger"], b["sha"], b["state"]]) for b in j["builds"] if b["id"] == '"$1"'][0]'; } # ID
ended() { row_of "$1" | grep -Eq ' (success|failure|error)$'; } # ID
gives() { [[ $("${@:2}") == "$1" ]]; } # WANT COMMAND...: COMMAND prints WANT
# Project P's queue head, as SHA|WAITING.
head_of() { api /local | jq_ '"%s|%s" % (j["queue"][0]["sha"], j["queue"][0]["waiting"]) if j["queue"] else ""'; }
# Every build of wid and two, by start: no two at once on the machine.
overlaps() {
  { P=wid api '/builds?limit=100'; echo; P=two api '/builds?limit=100'; } | python3 -c '
import json, sys
spans = []
for doc in sys.stdin.read().split("\n"):
    if doc.strip():
        for b in json.loads(doc)["builds"]:
            if b["started_at"] and b["ended_at"]:
                spans.append((b["started_at"], b["ended_at"], "%s #%s" % (b.get("ref"), b["id"])))
spans.sort()
for a, b in zip(spans, spans[1:]):
    if b[0] < a[1]:
        print("%s ran from %s to %s, %s from %s" % (a[2], a[0], a[1], b[2], b[0]))'
}
listed() { (cd "$T" && bash "$bana" list) | awk 'NR > 1 { print $1, $3 }' | tr '\n' ' '; }

out=$(cd "$w2" && bash "$bana" add </dev/null 2>&1) && code=0 || code=$?
[[ $code == 0 ]] || echo "$out" >&2
check "two: bana add, while the daemon runs" same "$code" 0
check "two: it links its page" has <(printf '%s\n' "$out") "Added acme/two: http://127.0.0.1:$port/#token="
check "two: the daemon was not restarted" alive "$daemon_pid"
check "two: Claude Code got bana's tools, for two" has "$FAKE_LOG" \
  "claude mcp add -s local bana -- $HOME/.bana/daemon.d/bana-manager mcp --dir $d2 (in $w2)"
check "two: the health names both" same "$(health | jq_ 'j["projects"]')" "['two', 'wid']"
check "list: both, active" same "$(listed)" "two active wid active "
check "status: a part for each" same "$(cd "$T" && bash "$bana" daemon status | grep -E '^(two|wid):$' | tr '\n' ' ')" "two: wid: "
until_ok 60 "two's first fetch" bash -c "grep -q '\"first_start_done\": true' '$d2/state.json'"
check "two: its first fetch builds nothing" same "$(P=two api /builds | jq_ 'len(j["builds"])')" 0

# One build at a time: two's push waits for wid's.
rm -f "$T/host-holding" "$T/two-running"
touch "$hold"
f=$(push hold "holds, while two waits")
until_ok 300 "wid's host job holding" test -s "$T/host-holding"
widb=$(ids_of "$f" push | tail -n 1)
t1=$(push2 "waits for wid")
until_ok 60 "two's push, through its hook" bash -c "grep -q '$t1' '$d2/state.json'"
P=two until_ok 30 "two's push held" gives "$t1|after wid #$widb" head_of
check "after: two's push waits after wid #$widb" same "$(P=two head_of)" "$t1|after wid #$widb"
sleep 3
check "after: and does not run meanwhile" same "$(P=two api /local | jq_ 'j["running"]')|$(test -e "$T/two-running" && echo ran)" "|"
rm -f "$hold"
until_ok 600 "wid's build" finished "$f"
P=two until_ok 300 "two's build" finished "$t1"
check "after: wid's build passed" same "$(state_of "$f")" success
check "after: then two's" same "$(P=two state_of "$t1")" success
check "after: two's statuses went to its own repo" same "$(P=two last "$t1" bana | cut -d'|' -f1)" success
check "after: and link to it on the page" has "$FAKE_LOG" "target_url=http://127.0.0.1:$port/#p=two&build=$(P=two ids_of "$t1" push)"
check "one at a time: no two builds of the machine ran at once" same "$(overlaps)" ""
P=two clean two

# bana pause: two's pushes wait; Run now still builds; bana resume builds them.
out=$(cd "$w2" && bash "$bana" pause 2>&1) || echo "$out" >&2
check "pause: bana pause, in two's checkout" has <(printf '%s\n' "$out") "two: automatic builds paused; Run now, fixes and releases still work"
check "pause: its flag" test -e "$d2/daemon/paused"
check "list: two paused" same "$(listed)" "two paused wid active "
t2=$(push2 "paused")
P=two until_ok 60 "the paused push queued" gives "$t2|paused" head_of
sleep 3
check "pause: the push waits, paused" same "$(P=two head_of)|$(P=two api /local | jq_ 'j["running"]')" "$t2|paused|"
check "pause: nothing posted for it" same "$(P=two posts "$t2")" ""
# Run now at nightly: at quick, the push's commit would be built, and the push dropped.
m=$(P=two api /builds -X POST -H 'content-type: application/json' -d '{"ref":"main","tier":"nightly"}' | jq_ 'j["build"]')
P=two until_ok 300 "two's Run now" ended "$m"
check "pause: Run now builds while paused" same "$(P=two row_of "$m")" "manual $t2 success"
check "pause: the push still waits" same "$(P=two head_of)" "$t2|paused"
pb=$(P=two ids_of "$t2" push)
check "pause: never built" same "$(P=two row_of "$pb")" "push $t2 queued"
out=$(cd "$T" && bash "$bana" resume two 2>&1) || echo "$out" >&2
check "resume: bana resume two, from elsewhere" has <(printf '%s\n' "$out") "two: resumed; 1 queued builds start"
check "resume: the flag is gone" test ! -e "$d2/daemon/paused"
P=two until_ok 300 "the push, resumed" ended "$pb"
check "resume: the push built" same "$(P=two row_of "$pb")" "push $t2 success"
check "list: two active again" same "$(listed)" "two active wid active "
P=two clean "pause and resume"

# bana remove wid, while two builds: two's build goes on.
rm -f "$T/two-running"
touch "$hold2"
t3=$(push2 "outlives wid")
until_ok 300 "two's job holding" test -s "$T/two-running"
out=$(cd "$T" && bash "$bana" remove wid 2>&1) || echo "$out" >&2
check "remove: bana remove wid, from elsewhere" has <(printf '%s\n' "$out") "Removed wid (acme/wid): its builds and clone stay in"
check "remove: wid's settings are gone" test ! -e "$d/daemon"
check "remove: and its push hook, in its checkout" test ! -e "$w/.git/hooks/reference-transaction"
check "remove: its builds and clone stay" test -d "$d/builds" -a -d "$d/src/.git"
check "remove: the daemon serves only two" same "$(health | jq_ 'j["projects"]')" "['two']"
check "remove: wid's routes are gone (404)" same "$(P=wid posted_status /local -X GET)" 404
check "list: two only" same "$(listed)" "two active "
check "remove: two's build runs on" same "$(P=two api /local | jq_ 'j["running"]["sha"]')" "$t3"
rm -f "$hold2"
P=two until_ok 300 "two's build" finished "$t3"
check "remove: two's build passed" same "$(P=two builds_of "$t3" | cut -d'|' -f2,3,4)" "success|push|1"
P=two clean remove

# ---- 10. bana split's public workflow, act as GitHub (BANA_E2E_SPLIT=1) -------------------------
# bana.yml as bana split renders it, run by act with ubuntu-latest on this machine (a hosted
# runner's VM): the fetch from a local bare origin over file:// (the deploy key's in GitHub's place),
# the inner act this test's, a job of each kind. Its console must hold the steps alone, and its
# sealed output must open with the seal key here. (act's own artifact server takes no upload of
# upload-artifact@v7: that step fails here, and the bundle is read where the runner sealed it.)
if [[ ${BANA_E2E_SPLIT:-} == 1 && $docker_mode == 1 ]]; then
  say "bana split: bana.yml, with act as GitHub"
  sp=$T/split
  mkdir -p "$sp/priv/.github/workflows" "$sp/pub/.github/workflows"
  (
    cd "$sp/priv"
    git init -q -b main . && git remote add origin git@github.com:acme/secret.git
    printf 'repo = acme/secret\nprefix = sec\ntiers = quick\nact.image = %s\n' "$image" >bana.conf
    cat >.github/workflows/ci.yml <<'YML'
on:
  workflow_dispatch:
    inputs:
      tier: {type: string}
jobs:
  build:
    runs-on: sec-linux
    strategy:
      matrix:
        target: [MATRIXVALUE-x64]
    steps:
      - name: compile ${{ matrix.target }}
        run: echo "PRIVATE-OUTPUT src/secret.rs:42"; mkdir -p out; echo binary >out/demo.txt
      - id: v
        run: echo "ver=PRIVATE-STEP-OUTPUT" >>"$GITHUB_OUTPUT"
      - name: version ${{ steps.v.outputs.ver }} ${{ github.workspace }}
        run: "true"
      - run: echo "PRIVATE-SCRIPT" >/dev/null
      - uses: actions/upload-artifact@v4
        with: {name: demo, path: out/demo.txt}
  test:
    runs-on: ubuntu-latest
    steps:
      - name: cargo test
        run: echo "PRIVATE-FAILURE assert_eq!(secret, 42)"; exit 1
  host:
    runs-on: sec-systemd
    steps:
      - name: on the runner's machine
        run: echo "PRIVATE-HOST"
YML
    git add -A && git -c user.name=t -c user.email=t@t commit -q -m private
    git clone -q --bare . "$sp/private.git"
    bash "$bana" split render >"$sp/pub/.github/workflows/bana.yml"
  )
  ssha=$(git -C "$sp/priv" rev-parse HEAD)
  check "split: bana.yml lints" same "$(cd "$sp/priv" && bash "$bana" split lint "$sp/pub/.github/workflows/bana.yml" 2>&1)" \
    "$sp/pub/.github/workflows/bana.yml: as bana runs it"
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out "$sp/seal.pem" 2>/dev/null
  openssl pkey -in "$sp/seal.pem" -pubout -out "$sp/seal.pub.pem" 2>/dev/null
  git -C "$sp/pub" init -q -b main . && git -C "$sp/pub" add -A && git -C "$sp/pub" -c user.name=t -c user.email=t@t commit -q -m public
  printf '{"inputs":{"id":"sec-7","sha":"%s","ref":"refs/heads/main","tier":"quick","job":"","logs":"private","nonce":"0123456789abcdef0123456789abcdef"}}\n' "$ssha" >"$sp/event.json"
  : >"$sp/started"
  # Without the stand-in gh ($T/bin), whose token GitHub would refuse for the action's clone.
  PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vx -e "$T/bin" -e "$here/stand-ins" | paste -sd: -) env -u GITHUB_TOKEN \
    "$act" workflow_dispatch -C "$sp/pub" -W .github/workflows/bana.yml -e "$sp/event.json" -P ubuntu-latest=-self-hosted \
    --artifact-server-path "$sp/art" --secret BANA_SOURCE=acme/secret --secret "BANA_SOURCE_KEY=unused over file://" \
    --var "BANA_SEAL_PUB=$(cat "$sp/seal.pub.pem")" --env "BANA_TEST_SOURCE=file://$sp/private.git" \
    --env "BANA_TEST_ACT=$act" >"$sp/console.txt" 2>&1 || true
  check "split: the public console has each step, by its id" has "$sp/console.txt" "test / step 0: failed"
  check "split: and the matrix job's" has "$sp/console.txt" "build / step v: ok"
  check "split: no step's name (a script, an output, a matrix value)" not grep -qE 'compile|cargo test|version' "$sp/console.txt"
  check "split: <prefix>-systemd's job, not on the runner's machine" not grep -qE 'host / |host: ' "$sp/console.txt"
  check "split: the job summary" has "$sp/console.txt" "steps: "
  check "split: no output, path or matrix value in public" not grep -qE 'PRIVATE|MATRIXVALUE|secret\.rs' "$sp/console.txt"
  check "split: act.image pinned by digest" grep -q "sec-linux=[^']*@sha256:[0-9a-f]\{64\}'" "$sp/pub/.github/workflows/bana.yml"
  sealed=$(find "$HOME/.cache/act" -path '*/tmp/bana/sealed' -newer "$sp/started" -type d 2>/dev/null | head -1)
  check "split: sealed" test -s "$sealed/bundle.enc" -a -s "$sealed/key.enc"
  check "split: nothing in clear in the bundle" not grep -aq PRIVATE "$sealed/bundle.enc"
  bash -c 'source "$1"; split_unseal "$2" "$3" "$4"' _ "$here/../lib/split.sh" "$sealed" "$sp/open" "$sp/seal.pem" 2>&1
  check "split: it opens here, act's whole output" has "$sp/open/out/act.jsonl" "PRIVATE-FAILURE assert_eq!(secret, 42)"
  check "split: and act's exit" same "$(cat "$sp/open/out/rc" 2>/dev/null)" 1
  check "split: and the upload, where the daemon collects it" test -n "$(find "$sp/open/art/7" -name '*.zip' 2>/dev/null)"
  check "split: the systemd job skipped, its label named for the daemon" has "$sp/open/out/act.jsonl" '-P sec-systemd=...'
  check "split: and never run" not grep -q 'PRIVATE-HOST' "$sp/open/out/act.jsonl"
else
  echo "e2e-daemon: bana split's public workflow skipped (BANA_E2E_SPLIT=1 runs it)"
fi

stop_daemon
check "stop: the daemon leaves no lock" test ! -e "$HOME/.bana/act.lock"
say "done: $((n - fails)) of $n passed"
((fails == 0))

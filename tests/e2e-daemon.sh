#!/usr/bin/env bash
# The daemon end to end: real act, real Docker, the daemon built from this tree
# (bana daemon run), a local bare origin over file:// in GitHub's place, and the gh
# stand-in (tests/stand-ins/gh), whose log is the statuses posted. Slow (minutes), so
# it runs only when asked:
#
#   BANA_E2E=1 tests/e2e-daemon.sh
#
#   BANA_E2E_ACT=PATH        the act to run (default: act on PATH)
#   BANA_DAEMON_BIN=PATH     a bana-manager already built (default: bana builds it)
#   BANA_E2E_DOCKER=0        no Docker: every job on the host (act's host mode), as on a
#                            Mac without OrbStack; docker is a stub that says it runs
#   BANA_E2E_IMAGE=IMAGE     the Linux jobs' image (default: catthehacker/ubuntu:act-24.04
#                            when Docker has it, else buildpack-deps:bookworm-scm, pulled)
#   BANA_E2E_PORT=N          the daemon's port (default 18470)
#   BANA_E2E_KEEP=1          keep the scratch directory
#
# Pushes: one that passes, one whose test fails (then Fix with Claude makes its worktree
# in the checkout), a [skip ci] one, one whose `sleep 600` is cancelled through the
# daemon's API, and one whose daemon is killed (-9) mid-build and started again (the build
# runs again, once). After each: no job containers, act workspaces, marker processes,
# secrets or lock left.
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
  # Small, with git and bash: the builds run with --pull=false, so it is pulled here.
  docker image inspect "$image" >/dev/null 2>&1 || image=buildpack-deps:bookworm-scm
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
export HOME=$T/home FAKE_LOG=$T/gh.log FAKE_STATE=$T/state
mkdir -p "$HOME" "$FAKE_STATE" "$T/bin"
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
act.args = $host_args
act.image = ${image:-none}
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

api() { # PATH [CURL-OPTIONS...]
  local p=$1
  shift
  printf 'Authorization: Bearer %s\n' "$(cat "$HOME/.bana/manager-token")" |
    curl -fsS --noproxy '*' --max-time 10 -H @- "$@" "http://127.0.0.1:$port/ci/v1$p"
}
# A value from JSON on stdin: a Python expression of j.
jq_() { python3 -c 'import json,sys; j=json.load(sys.stdin); v=eval(sys.argv[1]); print("" if v is None else v)' "$1"; }
healthy() { curl -fsS --noproxy '*' --max-time 3 "http://127.0.0.1:$port/ci/v1/health" | grep -q '"daemon":true'; }
# Healthy, or given up: a daemon that could not start (its build failed) stops the wait.
up_or_gone() {
  healthy && return 0
  ! kill -0 "$daemon_pid" 2>/dev/null
}

start_daemon() {
  # bana daemon run: the checks, the snapshot, the clone and the settings the first time
  # (it takes the port already in the settings), then the daemon in the foreground.
  (cd "$w" && exec bash "$bana" daemon run >>"$T/daemon.log" 2>&1) &
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

# Processes that carry a build's marker (BANA_BUILD=wid-<id>), as the sweep finds them.
markers() {
  local f
  if [[ $os == Linux ]]; then
    for f in /proc/[0-9]*/environ; do
      { tr '\0' '\n' <"$f" | grep -q '^BANA_BUILD=wid-' && echo "${f//[^0-9]/}"; } 2>/dev/null
    done
    return 0
  fi
  # shellcheck disable=SC2009 # pgrep cannot see environments; ps -E can (macOS)
  ps -Eww -o pid=,command= -U "$(id -u)" | grep '[B]ANA_BUILD=wid-' | awk '{ print $1 }' || true
}
has_marker() { markers | grep -x "$1" >/dev/null; }
# PID runs (a zombie, killed but not yet reaped by whoever inherited it, does not).
alive() { # PID
  local st
  st=$(ps -o stat= -p "$1" 2>/dev/null) || return 1
  [[ $st != Z* ]]
}
containers() { [[ $docker_mode != 1 ]] || docker ps -aq --filter label=xyz.tjrb.bana=wid; }
workspaces() { find "$d/act-cache" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | grep -E '/[0-9a-f]{16}$' || true; }
secrets() { find "$d/builds" -name secrets 2>/dev/null || true; }

cleanup() {
  local code=$?
  stop_daemon
  pkill -9 -f "$T/" 2>/dev/null || true
  [[ $docker_mode != 1 ]] || containers | xargs docker rm -f >/dev/null 2>&1 || true
  if ((code != 0 || fails > 0)); then
    echo "---- daemon log (last 60 lines)"
    tail -n 60 "$T/daemon.log" 2>/dev/null || true
    echo "---- gh log"
    grep -v '^gh auth' "$FAKE_LOG" 2>/dev/null | tail -n 60 || true
  fi
  if [[ ${BANA_E2E_KEEP:-} == 1 ]]; then echo "kept: $T"; else rm -rf "$T"; fi
}
trap cleanup EXIT

# Nothing a build left: no job container, act workspace, marker process, secrets or lock.
clean() { # WHAT
  check "$1: no job containers labelled xyz.tjrb.bana=wid" same "$(containers)" ""
  check "$1: no act workspaces in act-cache" same "$(workspaces)" ""
  check "$1: no processes with the build's marker" same "$(markers | tr '\n' ' ')" ""
  check "$1: the secrets file is gone" same "$(secrets)" ""
  check "$1: the lock is released" test ! -e "$HOME/.bana/act.lock"
}

# The statuses posted for SHA, in order: CONTEXT|STATE|DESCRIPTION.
posts() { # SHA
  grep -F "gh api -X POST repos/acme/wid/statuses/$1 " "$FAKE_LOG" |
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
state_of() { builds_of "$1" | tail -n 1 | cut -d'|' -f2; }

mkdir -p "$d/daemon"
echo "port = $port" >"$d/daemon/settings"
say "starting the daemon (port $port, $([[ $docker_mode == 1 ]] && echo "Linux jobs in $image" || echo "host jobs only"))"
start_daemon
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
check "pass: statuses link to the daemon's page" has "$FAKE_LOG" "target_url=http://127.0.0.1:$port/#build=$id"
check "pass: in the job, HEAD is the pushed commit" has <(build_log "$id") "HEAD $a"
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
# The HTTP status of a POST.
posted_status() { # PATH
  printf 'Authorization: Bearer %s\n' "$(cat "$HOME/.bana/manager-token")" |
    curl -sS --noproxy '*' --max-time 10 -H @- -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$port/ci/v1$1"
}
check "fix: none for a build that passed (409)" same "$(posted_status "/builds/$(builds_of "$a" | tail -n 1 | cut -d'|' -f1)/fix")" 409
check "fix: none for a build that is not there (404)" same "$(posted_status /builds/9999/fix)" 404
check "fix: nothing asked of GitHub" same "$(wc -l <"$FAKE_LOG")" "$asked"

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

stop_daemon
check "stop: the daemon leaves no lock" test ! -e "$HOME/.bana/act.lock"
say "done: $((n - fails)) of $n passed"
((fails == 0))

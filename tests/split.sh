# shellcheck shell=bash
# shellcheck disable=SC2154 # tests/run.sh sets T, here, bana, bana_root, n, fails, and has check, has, lacks, same, fresh
# shellcheck disable=SC2016 # ${{ }}: GitHub's, never the shell's
# bana split's tests, sourced by tests/run.sh: the public runner (bana.yml's runner.sh, as
# rendered, on stand-ins for act and GitHub), its lint, and the sealed output.

# A function of lib/split.sh, in the bash under test.
sp() { env bana_root="$bana_root" bash -c 'die() { printf "%s\n" "$*" >&2; exit 1; }; source "$0"; "$@"' "$bana_root/lib/split.sh" "$@"; }
# The runner.sh a rendered bana.yml writes in its first step.
runner_of() { awk '/<<.BANA_RUNNER.$/ { on = 1; next } on && /^ *BANA_RUNNER$/ { exit } on { sub(/^          /, ""); print }' "$1"; }
fx=$here/fixtures/split

# ---- split: bana.yml, the public repository's one workflow --------------------------------------
fresh
mkdir -p .github/workflows
printf 'on: workflow_dispatch\njobs:\n  rust:\n    runs-on: wid-linux\n    steps:\n      - run: cargo test\n' >.github/workflows/ci.yml
bash "$bana" split render >"$T/w/bana.yml" 2>"$T/out" || cat "$T/out"
check "split render: lints as bana runs it" same "$(bash "$bana" split lint "$T/w/bana.yml" 2>&1)" "$T/w/bana.yml: as bana runs it"
check "split render: dispatch only, and run-name bana <id>" has "$T/w/bana.yml" "run-name: bana \${{ inputs.id }}"
check "split render: the token gets nothing" has "$T/w/bana.yml" "permissions: {}"
check "split render: upload-artifact, pinned" has "$T/w/bana.yml" "uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1"
check "split render: the deploy key in the fetch step alone" same "$(grep -c 'secrets\.' "$T/w/bana.yml")" 2
runner_of "$T/w/bana.yml" >"$T/w/runner.sh"
check "split render: its runner is bash" bash -n "$T/w/runner.sh"
check "split render: the runner is lib/split.sh's own" same \
  "$(awk '/^# ---- the runner: /, /^# ---- end of the runner/' "$T/w/runner.sh")" \
  "$(awk '/^# ---- the runner: /, /^# ---- end of the runner/' "$bana_root/lib/split.sh")"
check "split render: act, pinned" has "$T/w/runner.sh" "split_act_sha256='0191d6f1f3b716b5c55820032605d05fc3c1cdbf581ebeff655019e5dd1524c0'"
check "split render: GitHub's host key, pinned" has "$T/w/runner.sh" "split_host_key='github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5"
check "split render: Linux jobs in act.image" has "$T/w/runner.sh" "'-P' 'wid-linux=catthehacker/ubuntu:act-24.04'"
check "split render: <prefix>-systemd's on the runner's machine (it has systemd)" has "$T/w/runner.sh" "'-P' 'wid-systemd=-self-hosted'"
check "split render: macOS jobs not run" has "$T/w/runner.sh" "'-P' 'wid-macos='"
check "split render: no \${{ }} in the runner" lacks "$T/w/runner.sh" '${{'
yq=${YQ:-$(command -v yq || true)}
if [[ -n $yq ]] && "$yq" --version 2>/dev/null | grep -q mikefarah; then
  check "split render: YAML, with the runner whole in its first step" same \
    "$("$yq" '.jobs.remote.steps[0].run' "$T/w/bana.yml" | awk '/<<.BANA_RUNNER.$/ { on = 1; next } /^BANA_RUNNER$/ { on = 0 } on')" \
    "$(cat "$T/w/runner.sh")"
fi
# What the lint refuses: each a line saying why.
mutant() { # NAME AWK-PROGRAM WHY
  awk "$2" "$T/w/bana.yml" >"$T/w/mutant.yml"
  bash "$bana" split lint "$T/w/mutant.yml" >"$T/out" 2>&1 && st=0 || st=$?
  check "split lint: refuses $1" same "$st" 1
  check "split lint: $1, says why" has "$T/out" "$3"
}
mutant "a pull_request trigger" '{ print } /^on:$/ { print "  pull_request:" }' "a pull_request trigger: only workflow_dispatch"
mutant "a push trigger" '{ print } /^on:$/ { print "  push:" }' "a push trigger: only workflow_dispatch"
mutant "on: push" '/^on:$/ { print "on: [push, workflow_dispatch]"; skip = 1; next } skip && /^  / { next } { skip = 0; print }' "on: must be workflow_dispatch alone"
mutant "\${{ }} in a run: script" '{ sub(/run: bash "\$RUNNER_TEMP\/bana\/runner.sh" build/, "run: echo ${{ inputs.sha }}"); print }' '${{ }} in a run: script'
mutant "\${{ }} in a run: block" '{ print } /^          bash "\$RUNNER_TEMP\/bana\/runner.sh" check$/ { print "          echo ${{ github.event.inputs.ref }}" }' '${{ }} in a run: script'
mutant "actions/checkout" '{ sub(/actions\/upload-artifact@[0-9a-f]+/, "actions/checkout@v4"); print }' "uses actions/checkout@v4: only actions/upload-artifact, pinned to a commit"
mutant "an action by tag" '{ sub(/upload-artifact@[0-9a-f]+/, "upload-artifact@v4"); print }' "uses actions/upload-artifact@v4: only"
mutant "actions/cache" '{ sub(/actions\/upload-artifact@/, "actions/cache@"); print }' "uses actions/cache@"
mutant "a cache input" '{ print } /^          retention-days: 1$/ { print "          cache: true" }' "a cache"
mutant "no permissions" '!/^permissions:/' "no permissions: {}"
mutant "a week of retention" '{ sub(/retention-days: 1$/, "retention-days: 7"); print }' "retention-days: 1, not more"

# ---- split: the runner's steps, on GitHub's side (here: a bare repository, a stand-in act) ----------
fresh
mkdir -p .github/workflows
printf 'on: workflow_dispatch\njobs:\n  rust:\n    runs-on: wid-linux\n    steps:\n      - run: cargo test\n' >.github/workflows/ci.yml
git add -A && git -c user.name=t -c user.email=t@t commit -q -m one
git clone -q --bare . "$T/w/private.git"
sha=$(git rev-parse HEAD)
bash "$bana" split render >"$T/w/bana.yml"
runner_of "$T/w/bana.yml" >"$T/w/runner.sh"
rt=$T/w/rt
mkdir -p "$rt"
run_step() { # STEP [ENV...]: the step as bana.yml runs it, with its inputs
  local s=$1
  shift
  env RUNNER_TEMP="$rt" BANA_ID=wid-7 BANA_SHA="$sha" BANA_REF=refs/heads/main BANA_TIER=quick BANA_JOB='' \
    BANA_LOGS=private GITHUB_TOKEN=ghp_leak "$@" bash "$T/w/runner.sh" "$s"
}
run_step check >"$T/out" 2>&1
check "runner check: the inputs bana sends" same "$(cat "$T/out")" "check: ok"
run_step check BANA_SHA="$sha; curl evil" >"$T/out" 2>&1 && st=0 || st=$?
check "runner check: refuses a sha that is no commit" same "$st:$(cat "$T/out")" "1:check: failed (not as bana sends them: sha)"
run_step check BANA_TIER='$(id)' BANA_LOGS=all BANA_REF='refs/heads/a b' >"$T/out" 2>&1 || true
check "runner check: and a ref, tier or logs bana would not send" has "$T/out" "not as bana sends them: ref tier logs"
run_step fetch >"$T/out" 2>&1 && st=0 || st=$?
check "runner fetch: no deploy key, no fetch" same "$st:$(cat "$T/out")" "1:fetch: failed (no deploy key here: bana split check)"
key='-----BEGIN OPENSSH PRIVATE KEY-----
FAKE-DEPLOY-KEY-MATERIAL
-----END OPENSSH PRIVATE KEY-----'
run_step fetch BANA_SOURCE=acme/widget BANA_SOURCE_KEY="$key" BANA_TEST_SOURCE="file://$T/w/private.git" >"$T/out" 2>&1 && st=0 || st=$?
check "runner fetch: the commit, and one line" same "$st:$(cat "$T/out")" "0:fetch: ok"
check "runner fetch: at the commit asked for" same "$(git -C "$rt/bana/src" rev-parse HEAD)" "$sha"
check "runner fetch: the key is gone" test ! -e "$rt/bana/key"
check "runner fetch: no key left anywhere" bash -c "! grep -rq FAKE-DEPLOY-KEY '$rt'"
run_step fetch BANA_SOURCE=acme/widget BANA_SOURCE_KEY="$key" BANA_TEST_SOURCE="file://$T/w/none.git" >"$T/out" 2>&1 && st=0 || st=$?
check "runner fetch: a fetch that fails says only that" same "$st:$(cat "$T/out")" "1:fetch: failed"
check "runner fetch: and its key is gone too" test ! -e "$rt/bana/key"
run_step fetch BANA_SOURCE=acme/widget BANA_SOURCE_KEY="$key" BANA_TEST_SOURCE="file://$T/w/private.git" >/dev/null 2>&1
# act's stand-in: what it got, then the fixture's lines, and a failure.
cat >"$T/w/act" <<EOF
#!/bin/sh
printf '%s\n' "\$@" >'$T/w/act.argv'
env >'$T/w/act.env'
cat '$fx/act.jsonl'
echo "Error: Job 'rust' failed" >&2
exit 1
EOF
chmod +x "$T/w/act"
run_step build BANA_TEST_ACT="$T/w/act" BANA_SOURCE_KEY=leaked BANA_SOURCE=acme/widget >"$T/out" 2>&1 && st=0 || st=$?
check "runner build: act's exit" same "$st:$(cat "$rt/bana/out/rc")" "1:1"
check "runner build: each step, ok or failed, and its time" has "$T/out" "rust / cargo test: failed (12s)"
check "runner build: and each job" has "$T/out" "package: ok"
check "runner build: a matrix value in a step's name is hidden" has "$T/out" "package / build *: ok (61s)"
check "runner build: no output of the build" lacks "$T/out" "PRIVATE"
check "runner build: no matrix value" lacks "$T/out" "MATRIXVALUE"
check "runner build: no path" lacks "$T/out" "/home/runner"
check "runner build: workflow commands are off while act runs" has "$T/out" "::stop-commands::bana-"
check "runner build: says it failed" has "$T/out" "build: failed (act exited with 1)"
check "runner build: act's output, whole, in out/" same "$(cat "$rt/bana/out/act.jsonl")" "$(cat "$fx/act.jsonl")"
check "runner build: and its stderr" has "$rt/bana/out/act.err" "Error: Job 'rust' failed"
check "runner build: act's environment has no key, source or token" bash -c \
  "! grep -E '^(BANA_|GITHUB_TOKEN|FAKE_)' '$T/w/act.env'"
check "runner build: only PATH, HOME and RUNNER_TEMP" same "$(cut -d= -f1 "$T/w/act.env" | grep -Evx 'PWD|SHLVL|_' | LC_ALL=C sort | tr '\n' ' ')" "HOME PATH RUNNER_TEMP "
check "runner build: no Docker socket in the job containers" has <(tr '\n' ' ' <"$T/w/act.argv") "--container-daemon-socket - "
check "runner build: no secret file, no secret" bash -c "! grep -Eqx -- '-s|--secret|--secret-file' '$T/w/act.argv'"
check "runner build: the private workflow, in the fetched commit" has <(tr '\n' ' ' <"$T/w/act.argv") "-W $rt/bana/src/.github/workflows/ci.yml "
check "runner build: the event names no repository" same "$(cat "$rt/bana/event.json")" \
  "{\"repository\":{\"full_name\":\"private/source\"},\"ref\":\"refs/heads/main\",\"after\":\"$sha\",\"inputs\":{\"tier\":\"quick\"}}"
run_step build BANA_TEST_ACT="$T/w/act" BANA_LOGS=public >"$T/out" 2>&1 || true
check "runner build, logs public: the build's output too" has "$T/out" "PRIVATE-OUTPUT /home/runner/work/private-src/src/main.rs:3"
check "runner build, logs public: commands still off" has "$T/out" "::stop-commands::bana-"
: >"$T/w/summary"
run_step summary GITHUB_STEP_SUMMARY="$T/w/summary" >/dev/null 2>&1
check "runner summary: a table of the steps" has "$T/w/summary" "| rust / cargo test | failed | 12s |"
check "runner summary: the totals" has "$T/w/summary" "4 steps: 3 ok, 1 failed. The output is with the owner."
check "runner summary: nothing of the output" lacks "$T/w/summary" "PRIVATE"
# The seal, to a key made here (bana split on makes it, in ~/.bana/<prefix>/split).
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out "$T/w/seal.pem" 2>/dev/null
openssl pkey -in "$T/w/seal.pem" -pubout -out "$T/w/seal.pub.pem" 2>/dev/null
mkdir -p "$rt/bana/art/1/package" && echo tarball >"$rt/bana/art/1/package/pkg.tar.gz.zip"
run_step seal >"$T/out" 2>&1 && st=0 || st=$?
check "runner seal: no public key, no seal" same "$st:$(cat "$T/out")" "1:seal: failed (no BANA_SEAL_PUB here: bana split check)"
run_step seal BANA_SEAL_PUB="$(cat "$T/w/seal.pub.pem")" >"$T/out" 2>&1 && st=0 || st=$?
check "runner seal: ok, and its size" same "$st:$(sed 's/([0-9]* bytes)/(N bytes)/' "$T/out")" "0:seal: ok (N bytes)"
check "runner seal: what upload-artifact takes, alone" same "$(cd "$rt/bana/sealed" && echo *)" "bundle.enc key.enc"
check "runner seal: nothing in clear" bash -c "! grep -aq 'PRIVATE\|tarball' '$rt/bana/sealed/bundle.enc' '$rt/bana/sealed/key.enc'"
check "runner seal: no clear archive left" test ! -e "$rt/bana/bundle.tgz"
s=$rt/bana/sealed o=$T/w/open
sp split_unseal "$s" "$o" "$T/w/seal.pem" 2>"$T/out" || cat "$T/out"
check "unseal: act's output, as act wrote it" same "$(cat "$o/out/act.jsonl")" "$(cat "$fx/act.jsonl")"
check "unseal: act's exit" same "$(cat "$o/out/rc")" 1
check "unseal: the jobs' uploads" same "$(cat "$o/art/1/package/pkg.tar.gz.zip")" tarball
check "unseal: no archive left" test ! -e "$o/bundle.tgz"
unsealed() { # NAME SEALED WHY [PEM]
  rm -rf "$T/w/o2"
  sp split_unseal "$2" "$T/w/o2" "${4:-$T/w/seal.pem}" >"$T/out" 2>&1 && st=0 || st=$?
  check "unseal: refuses $1" same "$st" 1
  check "unseal: $1: says why" has "$T/out" "$3"
  check "unseal: $1: opens nothing" test ! -e "$T/w/o2/out" -a ! -e "$T/w/o2/bundle.tgz"
}
cp -R "$s" "$T/w/s1" && printf 'X' | dd of="$T/w/s1/bundle.enc" bs=1 seek=40 conv=notrunc 2>/dev/null
unsealed "a changed bundle" "$T/w/s1" "the bundle is not the one sealed"
cp -R "$s" "$T/w/s2" && head -c 100 "$s/bundle.enc" >"$T/w/s2/bundle.enc"
unsealed "a cut bundle" "$T/w/s2" "the bundle is not the one sealed"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out "$T/w/other.pem" 2>/dev/null
unsealed "another key's" "$s" "the bundle's key does not open with" "$T/w/other.pem"
cp -R "$s" "$T/w/s3" && printf 'pass x\n' | openssl pkeyutl -encrypt -pubin -inkey "$T/w/seal.pub.pem" -pkeyopt rsa_padding_mode:oaep -out "$T/w/s3/key.enc"
unsealed "a key file not bana's" "$T/w/s3" "the bundle's key is not bana's"
# One sealed like bana's, holding more than out/ and art/.
mkdir -p "$T/w/s4" "$T/w/evil/out" "$T/w/evil/etc" && echo x >"$T/w/evil/etc/x" && echo y >"$T/w/evil/out/y"
tar -czf "$T/w/evil.tgz" -C "$T/w/evil" out etc
pass=$(openssl rand -hex 32)
printf '%s\n' "$pass" | openssl enc -aes-256-cbc -pbkdf2 -iter 100000 -salt -pass stdin -in "$T/w/evil.tgz" -out "$T/w/s4/bundle.enc"
printf 'bana-seal 1\npass %s\nsha256 %s\n' "$pass" "$(sp split_sha256 "$T/w/s4/bundle.enc")" |
  openssl pkeyutl -encrypt -pubin -inkey "$T/w/seal.pub.pem" -pkeyopt rsa_padding_mode:oaep -out "$T/w/s4/key.enc"
unsealed "an archive with more" "$T/w/s4" "the bundle holds more than out/ and art/"
# The step lines, from act's output alone.
check "steps: only the step lines" same "$(sp split_runner_steps <"$fx/act.jsonl" | tr '\n' '|')" \
  "rust / Set up job: ok (2s)|rust / cargo test: failed (12s)|rust: failed|package / build *: ok (61s)|package / Post upload: ok (1s)|package: ok|"
check "steps, logs public: and the output" same "$(BANA_LOGS=public sp split_runner_steps <"$fx/act.jsonl" | grep -c PRIVATE)" 2

# ---- split: bana split on, the wizard (GitHub: the gh stand-in's store) --------------------------
# A terminal for bana: SCRIPT's lines are `expect TEXT` (wait until bana printed it), `send TEXT`
# (a line typed), `run COMMAND` (in sh, meanwhile). Prints what bana printed; bana's exit.
on_tty() { # SCRIPT COMMAND...
  python3 - "$@" <<'PY'
import os, pty, select, subprocess, sys, time
script, cmd = sys.argv[1], sys.argv[2:]
pid, fd = pty.fork()
if pid == 0:
    os.execvp(cmd[0], cmd)
out = b""
def read(until=None, limit=20.0):
    global out
    end = time.time() + limit
    while time.time() < end:
        if until is not None and until.encode() in out:
            return True
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                data = os.read(fd, 4096)
            except OSError:
                return until is None
            if not data:
                return until is None
            out += data
    return until is None
for line in open(script).read().splitlines():
    what, _, arg = line.partition(" ")
    if what == "expect" and not read(arg):
        out += ("\n[on_tty: never saw %r]\n" % arg).encode()
        break
    if what == "send":
        os.write(fd, (arg + "\n").encode())
    if what == "run":
        subprocess.call(arg, shell=True)
read(None, 30.0)
_, st = os.waitpid(pid, 0)
sys.stdout.write(out.decode("utf-8", "replace").replace("\r\n", "\n"))
sys.exit(os.WEXITSTATUS(st) if os.WIFEXITED(st) else 1)
PY
}
# The world: acme/widget added here, private, its admin signed in to gh.
split_world() {
  daemon_world
  bash "$bana" add </dev/null >/dev/null 2>&1 || { echo "bana add failed" >&2; return 1; }
  export FAKE_GH_STORE=$T/w/github
  mkdir -p "$FAKE_GH_STORE/repos/acme/widget/files"
  echo PRIVATE >"$FAKE_GH_STORE/repos/acme/widget/visibility"
  echo x >"$FAKE_GH_STORE/repos/acme/widget/files/README.md"
  : >"$FAKE_LOG"
}
writes() { grep -E 'gh api -X (POST|PUT|DELETE)|gh repo create|gh (secret|variable) (set|delete)|gh workflow' "$FAKE_LOG" || true; } # gh's writes
yes_phrase="yes, build acme/widget in public"
pub=$T/w/github/repos/acme/widget-releases
s=$HOME/.bana/wid/split
sset=$HOME/.bana/wid/daemon/settings

fresh
split_world
bash "$bana" split on --repo acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split on: no terminal, no BANA_SPLIT_CONSENT: refused" same "$st" 1
check "split on: says what it needs" has "$T/out" "No terminal: BANA_SPLIT_CONSENT must hold \"$yes_phrase\""
check "split on: refused: the plan and the risks first" has "$T/out" "anyone who takes over your GitHub account or gh token"
check "split on: refused: nothing written on GitHub" same "$(writes)" ""
check "split on: refused: nothing kept here" test ! -e "$s"
BANA_SPLIT_CONSENT="yes" bash "$bana" split on --repo acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split on: another phrase: refused, nothing written" same "$st:$(writes)" "1:"
bash "$bana" split on </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split on: no terminal, no --repo: refused" same "$st" 1
check "split on: says to name it" has "$T/out" "bana split on --repo OWNER/NAME"
bash "$bana" split on --web --repo acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split on --web: needs a terminal" same "$st:$(grep -c 'needs a terminal' "$T/out")" "1:1"
bash "$bana" split plan --repo acme/widget-releases </dev/null >"$T/out" 2>&1 || cat "$T/out"
check "split plan: how it is made: gh, here" has "$T/out" "3. acme/widget-releases, public: gh repo create acme/widget-releases --public"
check "split plan: the deploy key first" has "$T/out" "2. a read-only deploy key on acme/widget, first"
check "split plan: changes nothing" same "$(writes)" ""
bash "$bana" split plan --repo acme/widget-releases --web </dev/null >"$T/out" 2>&1 || cat "$T/out"
check "split plan --web: on GitHub's page" has "$T/out" "3. acme/widget-releases, public: on GitHub's new-repository page"
echo PUBLIC >"$FAKE_GH_STORE/repos/acme/widget/visibility"
bash "$bana" split plan --repo acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split plan: a public repository's code: refused" same "$st" 1
check "split plan: says why" has "$T/out" "acme/widget is not private (PUBLIC)"
echo PRIVATE >"$FAKE_GH_STORE/repos/acme/widget/visibility"

# All of it, off a terminal with the phrase.
: >"$FAKE_LOG"
BANA_SPLIT_CONSENT=$yes_phrase bash "$bana" split on --repo acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split on: done" same "$st" 0
[[ $st == 0 ]] || sed 's/^/  | /' "$T/out"
check "split on: each step, ok" same "$(grep -c '^\[[1-9]/9\] .* ok$' "$T/out")" 9
check "split on: the deploy key before the public repository" same \
  "$(grep -oE 'gh api -X POST repos/acme/widget/keys|gh repo create acme/widget-releases' "$FAKE_LOG" | tr '\n' '|')" \
  "gh api -X POST repos/acme/widget/keys|gh repo create acme/widget-releases|"
check "split on: the deploy key, read-only" same "$(cat "$FAKE_GH_STORE"/repos/acme/widget/keys/*/read_only)" true
check "split on: titled for the public repository" same "$(cat "$FAKE_GH_STORE"/repos/acme/widget/keys/*/title)" "bana split: acme/widget-releases"
check "split on: the public repository, public" same "$(cat "$pub/visibility")" PUBLIC
check "split on: its README, with the marker" has "$pub/files/README.md" "<!-- bana split: "
check "split on: its marker names no repository" lacks "$pub/files/README.md" "acme/widget "
check "split on: its one workflow, bana's render" same "$(cat "$pub/files/.github/workflows/bana.yml")" "$(bash "$bana" split render)"
check "split on: and nothing else" same "$(cd "$pub/files" && find . -type f | LC_ALL=C sort | tr '\n' ' ')" "./.github/workflows/bana.yml ./README.md "
check "split on: bana-source's secrets" same "$(cd "$pub/envs/bana-source/secrets" && echo *)" "BANA_SOURCE BANA_SOURCE_KEY"
check "split on: the deploy key went in on stdin (its length only here)" same "$(cat "$pub/envs/bana-source/secrets/BANA_SOURCE_KEY")" 89
check "split on: no repository secret" test ! -d "$pub/secrets"
check "split on: BANA_SEAL_PUB, the seal key's public half" same "$(cat "$pub/vars/BANA_SEAL_PUB")" "$(cat "$s/seal.pub.pem")"
check "split on: bana-source takes main only" same "$(cat "$pub/envs/bana-source/branches")" main
check "split on: upload-artifact the only action" has "$pub/settings/actions.permissions.selected-actions" "patterns_allowed[]=actions/upload-artifact@*"
check "split on: a read-only token" has "$pub/settings/actions.permissions.workflow" "default_workflow_permissions=read"
check "split on: a ruleset on the default branch" has "$(ls "$pub"/rulesets/*.json)" '"include": ["~DEFAULT_BRANCH"]'
check "split on: logs and artifacts, a day" has "$pub/settings/retention" "days=1"
check "split on: its settings" same "$(grep -E '^(split|release)\.' "$sset" | sed 's/= [0-9a-f]\{40\}$/= SHA/; s/split.key = [0-9]*$/split.key = ID/' | tr '\n' '|')" \
  "split.repo = acme/widget-releases|split.ci = github|split.logs = private|split.workflow = SHA|split.key = ID|release.repo = acme/widget-releases|"
check "split on: split.workflow is bana.yml's blob" has "$sset" "split.workflow = $(git hash-object "$pub/files/.github/workflows/bana.yml")"
check "split on: the daemon's settings take them" only_keys "$project_keys" "$sset"
check "split on: the seal key, its owner's alone" same "$(find "$s/seal.pem" -perm 600)" "$s/seal.pem"
check "split on: in a directory its owner's alone" same "$(find "$s" -maxdepth 0 -perm 700)" "$s"
check "split on: no private key material on GitHub, in gh's log or here" bash -c \
  "! grep -rq 'FAKE-PRIVATE-KEY' '$FAKE_GH_STORE' '$FAKE_LOG' '$HOME/.bana' '$T/out'"
check "split on: the deploy key's public half stays, for check" test -f "$s/deploy.pub"
check "split on: checks itself" has "$T/out" "ok    acme/widget-releases's one workflow is bana.yml, as bana pushed it"
check "split on: no FAIL" lacks "$T/out" "FAIL"
check "split on: says where runs are" has "$T/out" "bana split is on: acme/widget's pushes build on https://github.com/acme/widget-releases/actions"
bash "$bana" settings >"$T/out"
check "settings: split's" has "$T/out" "split.repo = acme/widget-releases"
check "settings: the release repo" has "$T/out" "release.repo = acme/widget-releases"
: >"$FAKE_LOG"
bash "$bana" split check >"$T/out" 2>&1 && st=0 || st=$?
check "split check: all ok" same "$st:$(grep -c '^  ok ' "$T/out")" "0:14"
check "split check: writes nothing" same "$(writes)" ""
bash "$bana" split status >"$T/out" 2>&1 || true
check "split status: on, where" has "$T/out" "public repository: https://github.com/acme/widget-releases"
check "split status: the logs" has "$T/out" "logs: private"
BANA_SPLIT_CONSENT=$yes_phrase bash "$bana" split on --repo acme/widget-releases </dev/null >"$T/out" 2>&1 || true
check "split on again: on already" has "$T/out" "bana split is on already: acme/widget-releases"

# bana split check: what it fails on, and warns of (each put back after).
failed_check() { # NAME WHY
  bash "$bana" split check >"$T/out" 2>&1 && st=0 || st=$?
  check "split check: $1: exit 1" same "$st" 1
  check "split check: $1: says so" has "$T/out" "$2"
}
wfs=$pub/files/.github/workflows
echo 'on: push' >"$wfs/other.yml"
failed_check "another workflow" "FAIL  acme/widget-releases's workflows are not bana's"
bash "$bana" split check --quick >"$T/out" 2>&1 && st=0 || st=$?
check "split check --quick: catches it too" same "$st:$(grep -c '^  ok \|^  FAIL ' "$T/out")" "1:2"
rm "$wfs/other.yml"
cp "$wfs/bana.yml" "$T/w/kept.yml" && echo '# changed' >>"$wfs/bana.yml"
failed_check "an edited bana.yml" "FAIL  acme/widget-releases's workflows are not bana's"
cp "$T/w/kept.yml" "$wfs/bana.yml"
k=$(echo "$FAKE_GH_STORE"/repos/acme/widget/keys/*)
echo false >"$k/read_only"
failed_check "a writable deploy key" "FAIL  acme/widget's deploy key $(basename "$k") can write"
echo true >"$k/read_only"
echo 3 >"$pub/envs/bana-source/secrets/EXTRA"
failed_check "another secret" "FAIL  bana-source's secrets should be BANA_SOURCE and BANA_SOURCE_KEY alone"
rm "$pub/envs/bana-source/secrets/EXTRA"
echo true >"$pub/vars/ACTIONS_STEP_DEBUG"
failed_check "a debug variable" "FAIL  acme/widget-releases has a debug variable"
rm "$pub/vars/ACTIONS_STEP_DEBUG"
echo dev >>"$pub/envs/bana-source/branches"
failed_check "another branch for bana-source" "FAIL  the environment bana-source should take main only"
echo main >"$pub/envs/bana-source/branches"
git rev-parse HEAD >>"$pub/commits"
failed_check "a commit of the private repo" "FAIL  acme/widget-releases has acme/widget's commit"
sed -i.bak '$d' "$pub/commits" && rm -f "$pub/commits.bak"
git remote add public git@github.com:acme/widget-releases.git
failed_check "a remote that points at it" "FAIL  a remote of $(git rev-parse --show-toplevel) points at acme/widget-releases"
git remote remove public
printf 'octo\nmallory\n' >"$pub/collaborators"
bash "$bana" split check >"$T/out" 2>&1 && st=0 || st=$?
check "split check: another collaborator warns, and passes" same "$st" 0
check "split check: who" has "$T/out" "WARN  acme/widget-releases's other collaborators can each read acme/widget (through the deploy key): mallory"
rm "$pub/collaborators"
check "split check: never writes" same "$(writes)" ""

# The toggles.
: >"$FAKE_LOG"
bash "$bana" split logs public </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split logs public: needs its own phrase" same "$st:$(grep -c '^split.logs = private$' "$sset")" "1:1"
BANA_SPLIT_CONSENT="yes, logs are public" bash "$bana" split logs public </dev/null >"$T/out" 2>&1 || cat "$T/out"
check "split logs public: with it" has "$sset" "split.logs = public"
check "split logs public: says the old runs keep theirs" has "$T/out" "bana split purge-runs deletes them"
bash "$bana" split logs private </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split logs private: off a terminal, --yes" same "$st:$(grep -c 'split.logs = public' "$sset")" "1:1"
bash "$bana" split logs private --yes </dev/null >/dev/null 2>&1
check "split logs private: back" has "$sset" "split.logs = private"
bash "$bana" split ci local >"$T/out" 2>&1
check "split ci local: pushes build here" same "$(grep '^split.ci' "$sset")" "split.ci = local"
check "split ci local: releases stay public" has "$T/out" "releases still publish on acme/widget-releases"
bash "$bana" split ci github >/dev/null 2>&1
check "split ci github: back" same "$(grep '^split.ci' "$sset")" "split.ci = github"
check "split's toggles: nothing on GitHub" same "$(writes)" ""

# sync: bana.yml again after it changes (here: another timeout).
bash "$bana" split sync --yes >"$T/out" 2>&1
check "split sync: nothing to do" has "$T/out" "acme/widget-releases's bana.yml is this bana's already"
sed -i.bak 's/^daemon.timeout = .*/daemon.timeout = 45/' "$sset" && rm -f "$sset.bak"
bash "$bana" split check >"$T/out" 2>&1 || true
check "split check: another render warns: sync" has "$T/out" "WARN  bana.yml is another bana's render: bana split sync"
bash "$bana" split sync </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split sync: off a terminal, --yes" same "$st" 1
bash "$bana" split sync --yes >"$T/out" 2>&1 || cat "$T/out"
check "split sync: shows the change" has "$T/out" "+    timeout-minutes: 45"
check "split sync: pushes it" has "$wfs/bana.yml" "timeout-minutes: 45"
check "split sync: and records its blob" has "$sset" "split.workflow = $(git hash-object "$wfs/bana.yml")"
bash "$bana" split check >"$T/out" 2>&1 && st=0 || st=$?
check "split sync: check passes again" same "$st:$(grep -c WARN "$T/out")" "0:0"

# rekey: a new deploy key and seal key, the old ones gone.
old=$(basename "$k") oldpub=$(cat "$s/seal.pub.pem")
: >"$FAKE_LOG"
bash "$bana" split rekey --yes >"$T/out" 2>&1 || cat "$T/out"
new=$(sed -n 's/^split.key = //p' "$sset")
check "split rekey: another deploy key" bash -c "[[ '$new' != '$old' && -d '$FAKE_GH_STORE/repos/acme/widget/keys/$new' ]]"
check "split rekey: the old one gone" test ! -e "$FAKE_GH_STORE/repos/acme/widget/keys/$old"
check "split rekey: the new one in first" same "$(grep -oE 'POST repos/acme/widget/keys|secret set BANA_SOURCE_KEY|DELETE repos/acme/widget/keys/[0-9]+' "$FAKE_LOG" | tr '\n' '|')" \
  "POST repos/acme/widget/keys|secret set BANA_SOURCE_KEY|DELETE repos/acme/widget/keys/$old|"
check "split rekey: a new seal key" bash -c "[[ \"\$(cat '$pub/vars/BANA_SEAL_PUB')\" != '$oldpub' ]]"
check "split rekey: its public half on GitHub" same "$(cat "$pub/vars/BANA_SEAL_PUB")" "$(cat "$s/seal.pub.pem")"
check "split rekey: the old seal key kept, for runs sealed before" test -f "$s/seal.old.pem"
check "split rekey: no private key left here" test ! -e "$s/deploy"
bash "$bana" split check >"$T/out" 2>&1 && st=0 || st=$?
check "split rekey: check passes" same "$st" 0

# remove waits for off; off: the deploy key first; the repository stays.
bash "$bana" remove </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "remove: refused while bana split is on" same "$st" 1
check "remove: says so" has "$T/out" "bana split off first"
check "remove: removes nothing" test -f "$sset"
mkdir -p "$pub/runs/9001" "$pub/runs/9002"
echo "bana wid-1" >"$pub/runs/9001/title" && echo "someone else's" >"$pub/runs/9002/title"
: >"$FAKE_LOG"
bash "$bana" split off </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split off: off a terminal, --yes" same "$st:$(writes)" "1:"
bash "$bana" split off --yes --purge-runs </dev/null >"$T/out" 2>&1 || cat "$T/out"
check "split off: the deploy key first" same "$(writes | head -1)" "gh api -X DELETE repos/acme/widget/keys/$new"
check "split off: the key is gone" test -z "$(ls "$FAKE_GH_STORE/repos/acme/widget/keys")"
check "split off: the secrets, environment and variable" test ! -e "$pub/envs/bana-source" -a ! -e "$pub/vars/BANA_SEAL_PUB"
check "split off: the workflow disabled" test -e "$pub/settings/disabled.bana.yml"
check "split off: --purge-runs: bana's runs" test ! -e "$pub/runs/9001" -a -e "$pub/runs/9002"
check "split off: never deletes or archives the repository" bash -c "! grep -E 'gh repo (delete|archive)' '$FAKE_LOG'"
check "split off: the repository stays" test -f "$pub/files/README.md"
check "split off: says how to archive or delete it" has "$T/out" "gh auth refresh -s delete_repo && gh repo delete acme/widget-releases"
check "split off: split's settings gone" same "$(grep -c '^split\.' "$sset")" 0
check "split off: releases stay on the public repository" has "$sset" "release.repo = acme/widget-releases"
check "split off: its files here gone" test ! -e "$s"
bash "$bana" split status >"$T/out" 2>&1
check "split status: off" has "$T/out" "bana split is off for wid (acme/widget): its builds run here."
bash "$bana" remove </dev/null >/dev/null 2>&1 && st=0 || st=$?
check "remove: once off, as before" same "$st" 0

# A stop half way, then on again: it goes on, and does nothing twice.
fresh
split_world
FAKE_GH_FAIL_AT=secret BANA_SPLIT_CONSENT=$yes_phrase bash "$bana" split on --repo acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split on, gh failing: stops" same "$st" 1
check "split on, gh failing: at that step" has "$T/out" "[6/9] bana-source's secrets ... failed"
check "split on, gh failing: says what is done" has "$T/out" "Done so far: seal, key, repo, readme, actions. bana split on goes on from here; bana split off undoes it."
check "split on, gh failing: no settings yet" same "$(grep -c '^split\.' "$sset")" 0
bash "$bana" split status >"$T/out" 2>&1
check "split status: half way" has "$T/out" "bana split on stopped half way, with acme/widget-releases"
: >"$FAKE_LOG"
bash "$bana" split on </dev/null >"$T/out" 2>&1 || cat "$T/out"
check "split on again: goes on, no phrase asked again" has "$T/out" "Going on with bana split on for acme/widget-releases (done: seal, key, repo, readme, actions)"
check "split on again: the steps done before" same "$(grep -c 'done before$' "$T/out")" 5
check "split on again: no second repository, key or ruleset" same "$(grep -cE 'repo create|POST repos/acme/widget/keys|POST repos/acme/widget-releases/rulesets' "$FAKE_LOG")" 0
check "split on again: done" has "$T/out" "bana split is on: acme/widget's pushes build on"
check "split on again: one deploy key" same "$(find "$FAKE_GH_STORE/repos/acme/widget/keys" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')" 1
check "split on again: its private half gone from here" test ! -e "$s/deploy"

# An organization that forbids deploy keys: stopped before anything public is made.
fresh
split_world
FAKE_GH_DENY=deploy-key BANA_SPLIT_CONSENT=$yes_phrase bash "$bana" split on --repo acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split on, no deploy keys allowed: stops" same "$st" 1
check "split on, no deploy keys allowed: at the key" has "$T/out" "[2/9] a read-only deploy key on acme/widget ... failed"
check "split on, no deploy keys allowed: says why" has "$T/out" "An organization's policy may forbid deploy keys"
check "split on, no deploy keys allowed: nothing public made" test ! -e "$pub"
check "split on, no deploy keys allowed: no repo create" bash -c "! grep -q 'repo create' '$FAKE_LOG'"
check "split on, no deploy keys allowed: no key material left" bash -c "! grep -rq FAKE-PRIVATE-KEY '$HOME/.bana'"

# A public repository there already, not empty: refused, nothing changed.
fresh
split_world
mkdir -p "$pub/files" && echo PUBLIC >"$pub/visibility" && echo x >"$pub/files/main.c" && echo x >"$pub/files/README.md"
BANA_SPLIT_CONSENT=$yes_phrase bash "$bana" split on --repo acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split on: a repository with things in it: refused" same "$st:$(writes)" "1:"
check "split on: says what it holds" has "$T/out" "acme/widget-releases holds more than a README (README.md main.c)"
rm "$pub/files/main.c"
BANA_SPLIT_CONSENT=$yes_phrase bash "$bana" split on --repo acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "split on: an empty one but a README (GitHub's page may add one): taken" same "$st" 0
check "split on: its README replaced by bana's" has "$pub/files/README.md" "<!-- bana split: "
check "split on: the plan says it was there" bash -c "grep -q 'there already: kept as it is' '$T/out'"

# On a terminal: the name asked (Enter takes the default), then how it is made.
fresh
split_world
printf '%s\n' "expect The public repository [acme/widget-releases]:" "send " "expect Which? [1]" "send 1" \
  "expect Which? [1]" "send " "expect Type \"$yes_phrase\"" "send $yes_phrase" >"$T/w/tty"
on_tty "$T/w/tty" bash "$bana" split on >"$T/out" 2>&1 && st=0 || st=$?
check "split on, a terminal: done" same "$st" 0
check "split on, a terminal: offers both ways" has "$T/out" "2. on GitHub's new-repository page, filled in for you; then press Enter here"
check "split on, a terminal: 1 makes it with gh" has "$FAKE_LOG" "gh repo create acme/widget-releases --public --disable-wiki --disable-issues --description CI runs and releases of widget, built by bana. The source is private."
check "split on, a terminal: logs private, the default" has "$sset" "split.logs = private"

# GitHub's page: bana opens it filled in, waits, and takes the repository once it is there.
fresh
split_world
printf '%s\n' "expect The public repository [acme/widget-releases]:" "send acme/widget-ci" "expect Which? [1]" "send 2" \
  "expect Press Enter once it is made" "send " "expect is not there yet" \
  "run mkdir -p '$FAKE_GH_STORE/repos/acme/widget-ci/files' && echo PRIVATE >'$FAKE_GH_STORE/repos/acme/widget-ci/visibility'" \
  "expect Press Enter once it is made" "send " "expect is not public" \
  "run echo PUBLIC >'$FAKE_GH_STORE/repos/acme/widget-ci/visibility'" \
  "expect Press Enter once it is made" "send " "expect Which? [1]" "send 2" "expect Type \"yes, logs are public\"" \
  "send yes, logs are public" "expect Type \"$yes_phrase\"" "send $yes_phrase" >"$T/w/tty"
FAKE_OS=Darwin on_tty "$T/w/tty" bash "$bana" split on >"$T/out" 2>&1 && st=0 || st=$?
check "split on, GitHub's page: done" same "$st" 0
[[ $st == 0 ]] || tail -20 "$T/out"
check "split on, GitHub's page: opened, filled in" has "$FAKE_LOG" \
  "open https://github.com/new?owner=acme&name=widget-ci&visibility=public&description=CI%20runs%20and%20releases%20of%20widget%2C%20built%20by%20bana.%20The%20source%20is%20private."
check "split on, GitHub's page: the URL shown too" has "$T/out" "  https://github.com/new?owner=acme&name=widget-ci&visibility=public"
check "split on, GitHub's page: waits until it is there" has "$T/out" "acme/widget-ci is not there yet"
check "split on, GitHub's page: and public" has "$T/out" "acme/widget-ci is not public (PRIVATE)"
check "split on, GitHub's page: no gh repo create" bash -c "! grep -q 'repo create' '$FAKE_LOG'"
check "split on, GitHub's page: the rest as with gh" has "$sset" "split.repo = acme/widget-ci"
check "split on: logs public, with its own phrase" has "$sset" "split.logs = public"
fresh
split_world
printf '%s\n' "expect Press Enter once it is made" "send q" >"$T/w/tty"
FAKE_OS=Linux on_tty "$T/w/tty" bash "$bana" split on --repo acme/widget-ci --web >"$T/out" 2>&1 && st=0 || st=$?
check "split on --web, q: stops" same "$st" 1
check "split on --web, q: nothing changed" same "$(grep -c 'Stopped: nothing changed' "$T/out"):$(writes)" "1:"
check "split on --web, Linux without a display: the URL to open" has "$T/out" "https://github.com/new?owner=acme&name=widget-ci"

# bana add --split: the wizard after adding the project.
fresh
daemon_world
export FAKE_GH_STORE=$T/w/github
mkdir -p "$FAKE_GH_STORE/repos/acme/widget/files" && echo PRIVATE >"$FAKE_GH_STORE/repos/acme/widget/visibility"
bash "$bana" add </dev/null >"$T/out" 2>&1 || cat "$T/out"
check "add: no --split, no bana split" bash -c "! grep -qE 'keys|repo (create|view)' '$FAKE_LOG'"
bash "$bana" remove </dev/null >/dev/null 2>&1
bash "$bana" add --split=acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "add --split, no phrase: the project is added" test -f "$sset"
check "add --split, no phrase: bana split refused, nothing on GitHub" same "$st:$(writes)" "1:"
BANA_SPLIT_CONSENT=$yes_phrase bash "$bana" add --split=acme/widget-releases </dev/null >"$T/out" 2>&1 && st=0 || st=$?
check "add --split: added and split" same "$st:$(grep '^split.repo' "$sset")" "0:split.repo = acme/widget-releases"
bash "$bana" add </dev/null >"$T/out" 2>&1 || true
check "add again: the doctor checks bana split's side" has "$T/out" "bana split: acme/widget-releases's workflow and acme/widget's deploy key are as bana left them"
check "add again: keeps bana split's settings" has "$sset" "split.repo = acme/widget-releases"

# The risks, word for word: what the wizard says is docs/SPLIT.md's.
check "split: the risks in docs/SPLIT.md are the wizard's" same \
  "$(awk '/^```text$/ { n++; if (n == 2) { on = 1; next } } on && /^```$/ { exit } on' "$bana_root/docs/SPLIT.md")" "$(sp split_risk)"
check "split help: every command" same "$(bash "$bana" split help 2>&1 | grep -c '^  bana split')" 12

# ---- split: the remote build, as the daemon runs it (bana ci, with BANA_SPLIT_*) ----------------
fresh
split_world
BANA_SPLIT_CONSENT=$yes_phrase bash "$bana" split on --repo acme/widget-releases </dev/null >"$T/out" 2>&1 || cat "$T/out"
# What the public run seals: act's lines (the fixture), its exit, an upload; sealed by the runner.
rb=$T/w/runner && mkdir -p "$rb/out" "$rb/art/7/package"
cp "$fx/act.jsonl" "$rb/out/act.jsonl" && echo 1 >"$rb/out/rc" && echo "Error: Job 'rust' failed" >"$rb/out/act.err"
echo tarball >"$rb/art/7/package/pkg.tar.gz.zip"
BANA_SEAL_PUB=$(cat "$s/seal.pub.pem") sp split_runner_seal "$rb" "$T/w/bundle" >/dev/null
b7=$T/w/builds/7 && mkdir -p "$b7"
sha=$(git rev-parse HEAD)
remote_ci() { # [ENV...]: bana ci as the daemon runs a remote build (Settings::split_env)
  (cd "$b7" && env BANA_PROJECT_ROOT="$T/w/project" BANA_ACT_LOCKED=1 BANA_BUILD=wid-7 BANA_SPLIT_POLL=0 \
    BANA_SPLIT_REPO=acme/widget-releases BANA_SPLIT_PRIVATE=acme/widget BANA_SPLIT_LOGS=private \
    BANA_SPLIT_WORKFLOW="$(sed -n 's/^split.workflow = //p' "$sset")" BANA_SPLIT_KEY="$(sed -n 's/^split.key = //p' "$sset")" \
    BANA_SPLIT_HOME="$s" BANA_SPLIT_SHA="$sha" BANA_SPLIT_REF=refs/heads/main FAKE_RUN_BUNDLE="$T/w/bundle" FAKE_RUN_CONCLUSION=failure "$@" \
    bash "$bana" ci quick --event event.json -- --json --artifact-server-path artifacts)
}
echo '{}' >"$b7/event.json"
: >"$FAKE_LOG"
remote_ci >"$T/out" 2>"$T/err" && st=0 || st=$?
run=$(sed -n 's/.*"run": \([0-9]*\),.*/\1/p' "$b7/remote.json")
check "remote build: exits as act did" same "$st" 1
check "remote build: act's lines, as act printed them" same "$(grep -v '^{"bana":' "$T/out")" "$(cat "$fx/act.jsonl")"
check "remote build: a line names its run, first" same "$(grep '^{"bana":"remote"' "$T/out" | sed -n 2p)" \
  '{"bana":"remote","msg":"remote run in public repo acme/widget-releases: https://github.com/acme/widget-releases/actions/runs/'"$run"' (logs: private)"}'
check "remote build: and its progress" has "$T/out" '{"bana":"remote","msg":"acme/widget-releases run '"$run"': completed failure"}'
check "remote build: act's stderr" has "$T/err" "Error: Job 'rust' failed"
check "remote build: remote.json" same "$(cat "$b7/remote.json")" \
  "{\"repo\": \"acme/widget-releases\", \"run\": $run, \"url\": \"https://github.com/acme/widget-releases/actions/runs/$run\", \"logs\": \"private\"}"
check "remote build: the uploads where the daemon collects them" same "$(cat "$b7/artifacts/7/package/pkg.tar.gz.zip")" tarball
check "remote build: the sealed bundle deleted from GitHub" test ! -e "$pub/runs/$run/artifact"
check "remote build: dispatched with the build's inputs" same "$(tr '\n' ' ' <"$pub/runs/$run/inputs")" \
  "id=wid-7 sha=$sha ref=refs/heads/main tier=quick job= logs=private "
check "remote build: on the public repo, the dispatch and the bundle's delete alone" same \
  "$(writes | sed 's/artifacts\/[0-9]*/artifacts\/N/' | tr '\n' '|')" \
  "gh workflow run bana.yml -R acme/widget-releases -f id=wid-7 -f sha=$sha -f ref=refs/heads/main -f tier=quick -f job= -f logs=private|gh api -X DELETE repos/acme/widget-releases/actions/artifacts/N|"
check "remote build: no act, no Docker here" bash -c "! grep -qE '^act |^docker ' '$FAKE_LOG'"
check "remote build: nothing decrypted left behind" bash -c "! ls '${TMPDIR:-/tmp}' | grep -q bana-remote"
# The guard: a public side not as bana left it dispatches nothing.
: >"$FAKE_LOG"
echo 'on: push' >"$pub/files/.github/workflows/evil.yml"
remote_ci >"$T/out" 2>"$T/err" && st=0 || st=$?
check "remote build, the guard: fails" same "$st" 1
check "remote build, the guard: says why" has "$T/err" "Error: acme/widget-releases is not as bana left it, so nothing was dispatched: acme/widget-releases's workflows are not bana's"
check "remote build, the guard: no dispatch" bash -c "! grep -q 'workflow run' '$FAKE_LOG'"
rm "$pub/files/.github/workflows/evil.yml"
remote_ci BANA_SPLIT_KEY=999 >"$T/out" 2>"$T/err" || true
check "remote build, the guard: another deploy key" has "$T/err" "acme/widget has no deploy key 999"
# Nothing sealed: the run failed before act (the fetch), or a bundle this machine cannot open.
remote_ci FAKE_RUN_NOBUNDLE=1 FAKE_RUN_CONCLUSION=failure >"$T/out" 2>"$T/err" && st=0 || st=$?
check "remote build, no bundle: one failure, where" same "$st:$(sed 's/runs\/[0-9]*/runs\/N/' "$T/err")" \
  "1:Error: the run on acme/widget-releases failure at its fetch step, with no output sealed: https://github.com/acme/widget-releases/actions/runs/N"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out "$T/w/other.pem" 2>/dev/null
openssl pkey -in "$T/w/other.pem" -pubout -out "$T/w/other.pub" 2>/dev/null
BANA_SEAL_PUB=$(cat "$T/w/other.pub") sp split_runner_seal "$rb" "$T/w/bundle2" >/dev/null
remote_ci FAKE_RUN_BUNDLE="$T/w/bundle2" >"$T/out" 2>"$T/err" && st=0 || st=$?
check "remote build, another seal key: refused" same "$st" 1
check "remote build, another seal key: says so" has "$T/err" "left a bundle this machine cannot open: the bundle's key does not open with $s/seal.pem"
# A cancel (the daemon's SIGINT, to the process group) cancels the run there too.
: >"$FAKE_LOG"
# As the daemon starts it: a process group of its own, SIGINT as the default.
: >"$T/out" && rm -f "$b7/remote.json"
(cd "$b7" && exec env BANA_PROJECT_ROOT="$T/w/project" BANA_ACT_LOCKED=1 BANA_BUILD=wid-7 BANA_SPLIT_POLL=0.2 \
  BANA_SPLIT_REPO=acme/widget-releases BANA_SPLIT_PRIVATE=acme/widget BANA_SPLIT_HOME="$s" BANA_SPLIT_SHA="$sha" \
  BANA_SPLIT_WORKFLOW="$(sed -n 's/^split.workflow = //p' "$sset")" BANA_SPLIT_KEY="$(sed -n 's/^split.key = //p' "$sset")" \
  FAKE_RUN_VIEWS=300 python3 -c 'import os, signal, sys; signal.signal(signal.SIGINT, signal.SIG_DFL); os.setpgrp(); os.execvp(sys.argv[1], sys.argv[1:])' \
  bash "$bana" ci quick --event event.json >"$T/out" 2>"$T/err") &
cpid=$!
for _ in $(seq 100); do grep -q 'remote run in public repo' "$T/out" 2>/dev/null && break; sleep 0.1; done
run=$(sed -n 's/.*"run": \([0-9]*\),.*/\1/p' "$b7/remote.json")
kill -INT -- "-$cpid" 2>/dev/null || true
wait "$cpid" && st=0 || st=$?
check "remote build, cancelled: exit 130" same "$st" 130
check "remote build, cancelled: the run there too" has "$FAKE_LOG" "gh run cancel $run -R acme/widget-releases"
check "remote build, cancelled: says so" has "$T/err" "Error: cancelled, and its run on acme/widget-releases too"
# bana ci --list stays act -l, here; a fix round (no BANA_SPLIT_*) runs act here.
: >"$FAKE_LOG"
(cd "$b7" && env BANA_PROJECT_ROOT="$T/w/project" BANA_SPLIT_REPO=acme/widget-releases bash "$bana" ci --list) >/dev/null 2>&1 || true
check "remote build: --list is act's, here" has "$FAKE_LOG" "act -l -C"
check "remote build: --list dispatches nothing" bash -c "! grep -q 'workflow run' '$FAKE_LOG'"
# bana ci --remote, by hand: the checkout's HEAD there, its steps here.
: >"$FAKE_LOG"
FAKE_RUN_BUNDLE=$T/w/bundle BANA_SPLIT_POLL=0 bash "$bana" ci quick --remote >"$T/out" 2>&1 && st=0 || st=$?
check "ci --remote: exits as act did" same "$st" 1
check "ci --remote: the steps" has "$T/out" "rust / cargo test: failed (12s)"
check "ci --remote: where it ran" has "$T/out" "remote run in public repo acme/widget-releases: https://github.com/acme/widget-releases/actions/runs/"
check "ci --remote: act's lines kept" same "$(grep -v '^{"bana":' "$HOME/.bana/wid/ci/remote.jsonl")" "$(cat "$fx/act.jsonl")"
check "ci --remote: its build name" has "$FAKE_LOG" "-f id=wid-ci-"

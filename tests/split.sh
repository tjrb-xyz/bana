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

#!/usr/bin/env bash
# bana's tests: every command on stand-ins for the programs it drives (uname, orb,
# tart, gh, ioreg, sudo, apt-get, launchctl, systemctl, curl as a daemon's API, the runner's
# config.sh and svc.sh), so the macOS paths run on Linux too. BASH=/path/to/bash-3.2 tests with macOS's stock shell;
# AWK=original-awk with its BSD awk.
#
#   tests/run.sh            all of them (as root, it also tests a Proxmox container's root path)
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
bana=$here/../bin/bana
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
fails=0 n=0

# PATH: the stand-ins, and the bash and awk under test.
mkdir -p "$T/path"
ln -s "${BASH_UNDER_TEST:-$(command -v bash)}" "$T/path/bash"
[[ -z ${AWK:-} ]] || ln -s "$(command -v "$AWK")" "$T/path/awk"
export PATH=$T/path:$here/stand-ins:$PATH
echo "bash: $(bash -c 'echo $BASH_VERSION'), awk: $(awk --version 2>&1 | head -1)"

tar czf "$T/runner.tar.gz" -C "$here/fixtures/runner" .
export FAKE_TARBALL=$T/runner.tar.gz BANA_RUNNER_VERSION=2.999.0

# A fresh world for each test: a consuming project with a bana.conf, and a home.
fresh() {
  rm -rf "$T/w"
  mkdir -p "$T/w/project/.github" "$T/w/home" "$T/w/state/vmroot/run/systemd/system"
  export HOME=$T/w/home FAKE_STATE=$T/w/state FAKE_LOG=$T/w/log
  unset FAKE_OS FAKE_ARCH FAKE_UID FAKE_IOREG BANA_SYS_ROOT BANA_TOKEN FAKE_GH FAKE_POOL FAKE_SVC_FAIL GITHUB_TOKEN \
    FAKE_HEALTH FAKE_LINGER FAKE_GH_SCOPES BANA_DAEMON_BIN BANA_DAEMON_STEP CARGO_TARGET_DIR
  # What the host (GitHub's runners, act, a daemon's build) may have set, which bana reads.
  unset XDG_CONFIG_HOME BANA_HOME BANA_CONFIG BANA_PROJECT_ROOT BANA_ACT_LOCKED BANA_DAEMON ACT \
    RUNNER_ENVIRONMENT GITHUB_WORKSPACE DOCKER_HOST DISPLAY WAYLAND_DISPLAY
  : >"$FAKE_LOG"
  cd "$T/w/project"
  git init -q . && git remote add origin git@github.com:acme/widget.git
  cat >.github/bana.conf <<'CONF'
# acme's widget
repo = acme/widget
prefix = wid
labels = big-disk
packages.linux = libasound2-dev scons
path = ~/.cargo/bin
hook.mac = mac-hook.sh
hook.linux = linux-hook.sh
keep = /target/ node_modules/
keep_max_gb = 1
tiers = quick nightly release
plan.everything = ^(\.github/workflows/|Cargo\.lock$)
plan.path.rust = ^(crates/|Cargo\.toml$)
plan.path.web = ^web/
plan.tier.release = release
plan.tier.package = nightly, release
CONF
  # shellcheck disable=SC2016 # the hooks expand these when they run
  echo 'echo "mac hook dedicated=$BANA_DEDICATED prefix=$BANA_PREFIX" >>"$FAKE_LOG"' >.github/mac-hook.sh
  # shellcheck disable=SC2016
  echo 'echo "linux hook as $(id -u) on $(uname -m)" >>"$FAKE_LOG"' >.github/linux-hook.sh
}

check() { # NAME COMMAND...: passes when COMMAND succeeds
  local name=$1
  shift
  n=$((n + 1))
  if "$@"; then echo "ok   $name"; else echo "FAIL $name"; fails=$((fails + 1)); fi
}
has() { grep -qF -- "$2" "$1" || { echo "  $1 lacks: $2" >&2; sed 's/^/  | /' "$1" >&2; return 1; }; }
lacks() { ! grep -qF -- "$2" "$1" || { echo "  $1 has: $2" >&2; return 1; }; }
same() { [[ $1 == "$2" ]] || { printf '  want: %s\n  got:  %s\n' "$2" "$1" >&2; return 1; }; }
json_lines() { python3 -c 'import json,sys; [json.loads(l) for l in sys.stdin if l.strip()]'; }

# ---- settings -------------------------------------------------------------------
fresh
bash "$bana" settings >"$T/out"
check "settings come from .github/bana.conf" has "$T/out" "repo = acme/widget"
check "hooks are relative to bana.conf" has "$T/out" "hook.mac = $(pwd -P)/.github/mac-hook.sh"
BANA_REPO=o/other bash "$bana" settings >"$T/out"
check "BANA_* overrides bana.conf" has "$T/out" "repo = o/other"
rm .github/bana.conf
bash "$bana" settings >"$T/out"
check "without bana.conf, the repository comes from git's origin" has "$T/out" "repo = acme/widget"
check "and the prefix from its name" has "$T/out" "prefix = widget"
(cd "$T" && BANA_PROJECT_ROOT=$T/w/project bash "$bana" settings) >"$T/out"
check "BANA_PROJECT_ROOT names the checkout from elsewhere" has "$T/out" "repo = acme/widget"

# ---- plan, changed, keep-builds -------------------------------------------------
fresh
check "plan: a crate change runs rust only" same "$(printf 'crates/a.rs\nREADME.md\n' | bash "$bana" plan quick | tr '\n' ' ')" \
  "tier=quick rust=true web=false release=false package=false "
check "plan: only docs run nothing" same "$(echo README.md | bash "$bana" plan quick | tr '\n' ' ')" \
  "tier=quick rust=false web=false release=false package=false "
check "plan: plan.everything runs every path job" same "$(echo Cargo.lock | bash "$bana" plan quick | tr '\n' ' ')" \
  "tier=quick rust=true web=true release=false package=false "
check "plan: '*' (unknown changes) runs everything" same "$(echo '*' | bash "$bana" plan quick | grep web)" "web=true"
check "plan: nightly runs everything, and its tier keys" same "$(bash "$bana" plan nightly </dev/null | tr '\n' ' ')" \
  "tier=nightly rust=true web=true release=false package=true "
check "plan --json" same "$(echo web/x | bash "$bana" plan release --json)" \
  '{"tier":"release","rust":true,"web":true,"release":true,"package":true}'
check "plan refuses an unknown tier" bash -c "! bash '$bana' plan weekly </dev/null 2>/dev/null"

git add .github && git -c user.name=t -c user.email=t@t commit -q -m one
first=$(git rev-parse HEAD)
mkdir -p crates && echo x >crates/a.rs && git add crates && git -c user.name=t -c user.email=t@t commit -q -m two
check "changed: the files since the previous push" same "$(bash "$bana" changed "$first" main)" "crates/a.rs"
check "changed: '*' when it cannot tell" same "$(bash "$bana" changed 0000000 main)" "*"
# In act's containers the fetch has no credentials: the clone's own origin/main stands in.
git remote set-url origin "$T/nowhere.git"
git update-ref refs/remotes/origin/main "$first"
check "changed: an unreachable remote falls back to origin/BRANCH" same "$(bash "$bana" changed 0000000 main)" "crates/a.rs"

mkdir -p target/debug web/node_modules scratch && echo b >target/debug/big && echo m >web/node_modules/m && echo s >scratch/s
RUNNER_ENVIRONMENT=github-hosted bash "$bana" keep-builds >/dev/null
check "keep-builds does nothing on GitHub's runners" test -e scratch/s
RUNNER_ENVIRONMENT=self-hosted bash "$bana" keep-builds
check "keep-builds cleans the checkout" test ! -e scratch/s
check "keep-builds keeps the build caches" test -e target/debug/big -a -e web/node_modules/m
BANA_KEEP_MAX_GB=x RUNNER_ENVIRONMENT=self-hosted bash "$bana" keep-builds 2>"$T/err" || true
check "keep-builds checks keep_max_gb" has "$T/err" "keep_max_gb"

mkdir -p scratch && echo s >scratch/s
RUNNER_ENVIRONMENT=self-hosted ACT=true bash "$bana" keep-builds >/dev/null
check "keep-builds never cleans under bana ci (its options may bind your working tree)" test -e scratch/s
(cd crates && ACT=true BANA_DAEMON=1 GITHUB_WORKSPACE=$(pwd) bash "$bana" keep-builds >/dev/null)
check "keep-builds under the daemon's act cleans only the job's checkout (not one above it)" test -e scratch/s
(cd "$HOME" && ACT=true BANA_DAEMON=1 GITHUB_WORKSPACE=$HOME bash "$bana" keep-builds) >"$T/out" 2>&1
check "keep-builds under the daemon's act, in a job with no checkout: nothing to keep" has "$T/out" "nothing to keep"
ACT=true BANA_DAEMON=1 GITHUB_WORKSPACE=$(pwd) bash "$bana" keep-builds >/dev/null
check "keep-builds cleans under the daemon's act (a reused container keeps deleted files)" test ! -e scratch/s
check "keep-builds under the daemon's act keeps the build caches" test -e target/debug/big -a -e web/node_modules/m

# ---- bana ci: the workflow here, with act ------------------------------------------
fresh
mkdir -p .github/workflows && echo 'on: workflow_dispatch' >.github/workflows/ci.yml
FAKE_OS=Darwin FAKE_ARCH=arm64 bash "$bana" ci -j plan >/dev/null
check "ci: the workflow, dispatched with the first tier" has "$FAKE_LOG" \
  "act workflow_dispatch -C $(pwd -P) -W $(pwd -P)/.github/workflows/ci.yml --artifact-server-path $HOME/.bana/act/artifacts"
check "ci: Linux jobs in act's Ubuntu image" has "$FAKE_LOG" "-P wid-linux=catthehacker/ubuntu:act-24.04"
check "ci: on a Mac, macOS jobs on the Mac itself" has "$FAKE_LOG" "-P wid-macos=-self-hosted"
check "ci: arm64 containers on Apple silicon" has "$FAKE_LOG" "--container-architecture linux/arm64 --input tier=quick"
check "ci: the token from gh, by name only" has "$FAKE_LOG" "-s GITHUB_TOKEN -j plan"
check "ci: the token itself is not on act's command line" lacks "$FAKE_LOG" "FAKE-GH-TOKEN"
: >"$FAKE_LOG"
FAKE_OS=Darwin FAKE_ARCH=arm64 BANA_ACT_IMAGE=my/image bash "$bana" ci nightly --x64 -- --reuse >/dev/null
check "ci: --x64, a tier, act.image, and act's own options" has "$FAKE_LOG" "-P wid-linux=my/image"
check "ci: x86_64 containers" has "$FAKE_LOG" "--container-architecture linux/amd64 --input tier=nightly"
check "ci: after --, act's own options" has "$FAKE_LOG" "-s GITHUB_TOKEN --reuse"
: >"$FAKE_LOG"
FAKE_OS=Linux FAKE_ARCH=x86_64 bash "$bana" ci >/dev/null
check "ci: on Linux, no macOS jobs" lacks "$FAKE_LOG" "-self-hosted"
check "ci: refuses an unknown tier" bash -c "! bash '$bana' ci weekly 2>/dev/null"
FAKE_DOCKER=0 bash "$bana" ci >"$T/out" 2>&1 || true
check "ci: says to start OrbStack when Docker is not running" has "$T/out" "start OrbStack"
check "ci: and, act never started, leaves no lock" test ! -e "$HOME/.bana/act.lock"
: >"$FAKE_LOG"
(cd .github && bash "$bana" ci >/dev/null)
check "ci: from a subdirectory, act still runs the whole checkout" has "$FAKE_LOG" "act workflow_dispatch -C $(pwd -P) -W"
echo 'act.args = --reuse --pull=false' >>.github/bana.conf
: >"$FAKE_LOG"
bash "$bana" ci -- --secret-file my.secrets --rm >/dev/null
check "ci: bana.conf's act.args, then act's own (their files relative to here)" has "$FAKE_LOG" \
  "--reuse --pull=false --secret-file $(pwd -P)/my.secrets --rm"
check "ci: a --secret-file brings the token, so none from gh" lacks "$FAKE_LOG" "GITHUB_TOKEN"

# ---- bana ci for the daemon: its own checkout, an event, a secret file -----------------------
fresh
mkdir -p .github/workflows && echo 'on: workflow_dispatch' >.github/workflows/ci.yml
echo 'act.args = --reuse' >>.github/bana.conf
src=$(pwd -P)
mkdir -p "$T/w/build" && cd "$T/w/build"
build=$(pwd -P)
echo '{"inputs":{"tier":"quick"}}' >event.json
BANA_PROJECT_ROOT=$src BANA_ACT_LOCKED=1 bash "$bana" ci quick --event event.json -- --secret-file secrets --json >/dev/null
check "daemon ci: act runs the checkout BANA_PROJECT_ROOT names" has "$FAKE_LOG" \
  "act workflow_dispatch -C $src -W $src/.github/workflows/ci.yml"
check "daemon ci: with its bana.conf" has "$FAKE_LOG" "-P wid-linux=catthehacker/ubuntu:act-24.04"
check "daemon ci: the event, by its full path" has "$FAKE_LOG" "-e $build/event.json"
check "daemon ci: no --input (act ignores it with an event)" lacks "$FAKE_LOG" "--input"
check "daemon ci: the secret file has the token, not gh" lacks "$FAKE_LOG" "-s GITHUB_TOKEN"
check "daemon ci: gh is not asked" lacks "$FAKE_LOG" "gh auth"
check "daemon ci: act.args, then the daemon's options" has "$FAKE_LOG" "--reuse --secret-file $build/secrets --json"
check "daemon ci: BANA_PROJECT_ROOT does not reach act's jobs" lacks "$FAKE_STATE/act.env" "BANA_PROJECT_ROOT="
check "daemon ci: nor BANA_ACT_LOCKED" lacks "$FAKE_STATE/act.env" "BANA_ACT_LOCKED="
check "daemon ci: BANA_ACT_LOCKED=1 leaves the lock to the daemon" test ! -e "$HOME/.bana/act.lock"
check "daemon ci: the tier is still checked" bash -c "! BANA_PROJECT_ROOT='$src' bash '$bana' ci weekly --event event.json 2>/dev/null"
BANA_PROJECT_ROOT=$src bash "$bana" ci --event nothing.json >"$T/out" 2>&1 || true
check "daemon ci: a missing event file" has "$T/out" "--event: no file nothing.json"
: >"$FAKE_LOG"
BANA_PROJECT_ROOT=$src bash "$bana" ci --list >/dev/null
check "daemon ci: --list, of that checkout" has "$FAKE_LOG" "act -l -C $src -W $src/.github/workflows/ci.yml"
cd "$src"

# ---- one act at a time on this machine ----------------------------------------------------
fresh
mkdir -p .github/workflows && echo 'on: workflow_dispatch' >.github/workflows/ci.yml
lock=$HOME/.bana/act.lock
FAKE_ACT_SLEEP=30 bash "$bana" ci >/dev/null 2>&1 &
running=$!
i=0
while [[ ! -s $FAKE_STATE/act.pid ]] && ((i++ < 100)); do sleep 0.1; done
check "lock: act holds it, with bana ci's pid (exec keeps it)" same "$(sed -n 1p "$lock/owner")" "$running"
check "lock: that pid is act's" same "$(cat "$FAKE_STATE/act.pid")" "$running"
check "lock: its label" same "$(sed -n 3p "$lock/owner")" "bana ci quick (wid)"
bash "$bana" ci nightly >"$T/out" 2>&1 || true
check "lock: another bana ci is refused" has "$T/out" "act is busy here: bana ci quick (wid)"
: >"$FAKE_LOG"
BANA_ACT_LOCKED=1 bash "$bana" ci >/dev/null
check "lock: BANA_ACT_LOCKED=1 (the daemon has it) runs anyway" has "$FAKE_LOG" "act workflow_dispatch"
check "lock: and leaves it as it was" same "$(sed -n 1p "$lock/owner")" "$running"
kill "$running"
wait "$running" 2>/dev/null || true
: >"$FAKE_LOG"
bash "$bana" ci >/dev/null
check "lock: act gone, the next bana ci takes it over" has "$FAKE_LOG" "act workflow_dispatch"
check "lock: as its own" same "$(sed -n 1p "$lock/owner")" "$(cat "$FAKE_STATE/act.pid")"
# The daemon's: a live owner; then the same pid with another start time (reused).
sleep 30 &
sleeper=$!
printf '%s\n' "$sleeper" "$(LC_ALL=C ps -o lstart= -p "$sleeper" | awk '{ $1 = $1; print }')" "build 7 of wid" >"$lock/owner"
bash "$bana" ci >"$T/out" 2>&1 || true
check "lock: a live owner (a daemon's build) refuses bana ci" has "$T/out" "act is busy here: build 7 of wid"
printf '%s\n' "$sleeper" "Thu Jan 1 00:00:00 1970" "build 7 of wid" >"$lock/owner"
: >"$FAKE_LOG"
bash "$bana" ci >/dev/null
check "lock: a pid that started at another time is someone else's: taken over" has "$FAKE_LOG" "act workflow_dispatch"
kill "$sleeper"
wait "$sleeper" 2>/dev/null || true
# An act that cannot start: its pid is gone, so its lock is stale.
mkdir -p "$T/w/badact" && printf '#!/nonexistent/interpreter\n' >"$T/w/badact/act" && chmod +x "$T/w/badact/act"
PATH=$T/w/badact:$PATH bash "$bana" ci >/dev/null 2>&1 || true
: >"$FAKE_LOG"
bash "$bana" ci >/dev/null
check "lock: an act that never started holds nothing" has "$FAKE_LOG" "act workflow_dispatch"

# ---- USB audio --------------------------------------------------------------------
fresh
BANA_SYS_ROOT=$here/fixtures/linux-sys bash "$bana" usb >"$T/out"
check "usb (Linux): a USB card with its device node" has "$T/out" "1c75:af70  MiniFuse 2  (label usb-1c75-af70)"
check "usb (Linux): not a card whose /dev/snd node is missing (a container)" lacks "$T/out" "0d8c"
check "usb (Linux): not a built-in card" lacks "$T/out" "PCH"
check "usb (Linux): the labels" has "$T/out" "Runner labels: usb-audio,usb-1c75-af70"
FAKE_OS=Darwin FAKE_ARCH=arm64 FAKE_IOREG=$here/fixtures/ioreg-mac.txt bash "$bana" usb >"$T/out"
check "usb (macOS): an audio interface" has "$T/out" "1c75:af70  MiniFuse 2"
check "usb (macOS): an audio device behind a hub" has "$T/out" "0d8c:0014  USB Audio Device"
check "usb (macOS): not a keyboard or a hub" bash -c "! grep -Eq '05ac|0c45' '$T/out'"
bash "$bana" usb >"$T/out"
check "usb: none" has "$T/out" "No USB audio devices here."

# ---- a Mac joins: macOS runner, and Linux runners in two OrbStack machines ----------
fresh
export FAKE_OS=Darwin FAKE_ARCH=arm64 FAKE_HOST=MBP FAKE_IOREG=$here/fixtures/ioreg-mac.txt
bash "$bana" up --dedicated --label gpu >"$T/out" 2>&1 || { cat "$T/out"; false; }
mac=$HOME/.bana/wid/runners/wid-mbp-macos
check "mac: the project's mac hook ran, told it is dedicated" has "$FAKE_LOG" "mac hook dedicated=1 prefix=wid"
check "mac: the macOS runner registered with its labels and USB devices" has "$FAKE_LOG" \
  "--name wid-mbp-macos --labels wid-macos,osx-arm64,mbp,big-disk,gpu,usb-audio,usb-0d8c-0014,usb-1c75-af70"
check "mac: registered at the repository" has "$FAKE_LOG" "--url https://github.com/acme/widget --token REG-TOKEN"
check "mac: a LaunchAgent (svc.sh without sudo)" has "$FAKE_LOG" "svc.sh install (in $mac)"
check "mac: jobs see BANA_DEDICATED" has "$mac/.env" "BANA_DEDICATED=1"
check "mac: bana.conf's path leads the runner's PATH" has "$FAKE_LOG" "runner PATH starts $HOME/.cargo/bin"
check "mac: two OrbStack machines, one x86_64" has "$FAKE_LOG" "orb create --arch amd64 ubuntu:noble bana-x64"
vmhome=$FAKE_STATE/orb/bana/home
check "vm: the linux hook ran in it, as a user, on arm64" has "$FAKE_LOG" "linux hook as 1000 on aarch64"
check "vm: the x86_64 one on x86_64" has "$FAKE_LOG" "linux hook as 1000 on x86_64"
check "vm: its runner is named after the Mac, without USB labels" has "$FAKE_LOG" \
  "--name wid-mbp-linux-arm64-1 --labels wid-linux,linux-arm64,mbp,big-disk,gpu --work"
check "vm: the x86_64 runner" has "$FAKE_LOG" "--name wid-mbp-linux-x64-1 --labels wid-linux,linux-x64,mbp,big-disk,gpu --work"
check "vm: a systemd service (sudo svc.sh install USER)" has "$FAKE_LOG" "sudo ./svc.sh install"
check "vm: its runners live in the machine's own home" test -e "$vmhome/.bana/wid/runners/wid-mbp-linux-arm64-1/.runner"
check "vm: bana.conf's path leads its runner's PATH" has "$FAKE_LOG" "runner PATH starts $vmhome/.cargo/bin"

bash "$bana" status-json >"$T/out"
check "status-json: valid JSON lines" json_lines <"$T/out"
check "status-json: the macOS runner" has "$T/out" '"name":"wid-mbp-macos","machine":"mbp"'
check "status-json: the USB devices" has "$T/out" '{"kind":"usb","machine":"mbp","id":"1c75:af70","name":"MiniFuse 2","label":"usb-1c75-af70"}'
check "status-json: the VM's runners" has "$T/out" '"name":"wid-mbp-linux-x64-1","machine":"mbp (bana-x64)"'
check "status-json: dedicated" has "$T/out" '"dedicated":true'

: >"$FAKE_LOG"
bash "$bana" up --dedicated --label gpu >/dev/null 2>&1
check "up again: nothing registers twice" lacks "$FAKE_LOG" "config.sh --unattended"

: >"$FAKE_LOG"
FAKE_IOREG='' bash "$bana" relabel >/dev/null
check "relabel: an unplugged device leaves the labels (the API, by runner id)" has "$FAKE_LOG" \
  "gh api -X PUT repos/acme/widget/actions/runners/42/labels -f labels[]=wid-macos -f labels[]=osx-arm64 -f labels[]=mbp -f labels[]=big-disk"
check "relabel: remembered" same "$(cat "$mac/.bana-labels")" "self-hosted,wid-macos,osx-arm64,mbp,big-disk"

: >"$FAKE_LOG"
bash "$bana" stop wid-mbp-linux-x64-1
check "stop: a VM's runner, in the right machine" has "$FAKE_LOG" "orb -m bana-x64 env"
check "stop: its service" has "$FAKE_LOG" "sudo ./svc.sh stop"
check "stop: refuses a path" bash -c "! bash '$bana' stop ../x 2>/dev/null"

: >"$FAKE_LOG"
bash "$bana" down >/dev/null
check "down: removes each runner from GitHub" has "$FAKE_LOG" "config.sh remove --token REMOVE-TOKEN (in $mac)"
check "down: and in the VMs" has "$FAKE_LOG" "config.sh remove --token REMOVE-TOKEN (in $vmhome/.bana/wid/runners/wid-mbp-linux-arm64-1)"
check "down: nothing left" test ! -e "$mac"
unset FAKE_OS FAKE_ARCH FAKE_HOST FAKE_IOREG

# ---- failures leave nothing behind ------------------------------------------------------
fresh
export FAKE_OS=Darwin FAKE_ARCH=arm64 FAKE_HOST=mbp
bash "$bana" up --linux 0 --x64 0 --token WRONG >"$T/out" 2>&1 || true
check "a refused token: says so" has "$T/out" "GitHub refused to register wid-mbp-macos"
check "a refused token: no runner directory left" test ! -e "$HOME/.bana/wid/runners/wid-mbp-macos"
FAKE_GH=0 bash "$bana" up --linux 0 --x64 0 >"$T/out" 2>&1 || true
check "no gh and no token: says how to get one" has "$T/out" "gh auth login"
FAKE_SVC_FAIL=1 bash "$bana" up --linux 0 --x64 0 >"$T/out" 2>&1 || true
check "a service that does not start: unregistered again" has "$FAKE_LOG" "config.sh remove"
check "a service that does not start: nothing left" test ! -e "$HOME/.bana/wid/runners/wid-mbp-macos"
unset FAKE_OS FAKE_ARCH FAKE_HOST

# ---- a Linux machine (a Proxmox VM) with a USB interface passed through -------------------
fresh
export FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_HOST=pve-ci FAKE_UID=1000 BANA_SYS_ROOT=$here/fixtures/linux-sys
FAKE_MISSING="scons git" bash "$bana" up --linux 2 >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "linux: installs only the missing packages" has "$FAKE_LOG" "apt-get install -y -q git scons"
check "linux: the first runner holds the USB device" has "$FAKE_LOG" \
  "--name wid-pve-ci-linux-x64-1 --labels wid-linux,linux-x64,pve-ci,big-disk,usb-audio,usb-1c75-af70 --work"
check "linux: the second does not (two jobs never share a device)" has "$FAKE_LOG" \
  "--name wid-pve-ci-linux-x64-2 --labels wid-linux,linux-x64,pve-ci,big-disk --work"
check "linux: its kernel has snd-usb-audio, so no kernel packages" lacks "$FAKE_LOG" "linux-image"

# A Debian cloud kernel (no sound drivers), and Ubuntu's virtual one.
cp -R "$here/fixtures/linux-sys" "$T/w/sys" && rm -rf "$T/w/sys/lib"
: >"$FAKE_LOG"
BANA_SYS_ROOT=$T/w/sys FAKE_KERNEL=6.12.48-cloud-amd64 FAKE_MISSING=linux-image-amd64 bash "$bana" up --linux 1 >"$T/out" 2>&1
check "linux: a cloud kernel gets the standard one" has "$FAKE_LOG" "apt-get install -y -q linux-image-amd64"
check "linux: and says to reboot" has "$T/out" "Reboot into the new kernel"
: >"$FAKE_LOG"
BANA_SYS_ROOT=$T/w/sys FAKE_KERNEL=6.8.0-1015-kvm FAKE_MISSING=linux-modules-extra-6.8.0-1015-kvm bash "$bana" up --linux 1 >"$T/out" 2>&1
check "linux: Ubuntu's virtual kernel gets its extra modules" has "$FAKE_LOG" "apt-get install -y -q linux-modules-extra-6.8.0-1015-kvm"
check "linux: and loads the driver" has "$FAKE_LOG" "sudo modprobe snd-usb-audio"
: >"$FAKE_LOG"
touch "$T/w/sys/run/systemd/container"
BANA_SYS_ROOT=$T/w/sys FAKE_KERNEL=6.12.48-cloud-amd64 bash "$bana" up --linux 1 >/dev/null 2>&1
check "linux: a container uses its host's kernel" lacks "$FAKE_LOG" "linux-image"
: >"$FAKE_LOG"
rm "$T/w/sys/run/systemd/container"
BANA_SYS_ROOT=$T/w/sys FAKE_KERNEL=6.12.48-cloud-amd64 bash "$bana" up --linux 1 --no-usb >/dev/null 2>&1
check "linux: --no-usb leaves the kernel alone" lacks "$FAKE_LOG" "linux-image"
unset BANA_SYS_ROOT
bash -c "BANA_SYS_ROOT=/nonexistent bash '$bana' up" >"$T/out" 2>&1 || true
check "linux: without systemd, says what to do" has "$T/out" "nesting"
unset FAKE_OS FAKE_ARCH FAKE_HOST FAKE_UID

# ---- entered as root (a Proxmox container) ---------------------------------------------------
if [[ $(/usr/bin/id -u) == 0 ]]; then
  fresh
  /usr/bin/id bana-test >/dev/null 2>&1 || /usr/sbin/useradd -m -s /bin/bash bana-test
  export FAKE_OS=Linux FAKE_ARCH=aarch64 FAKE_HOST=ct FAKE_UID=0 BANA_SYS_ROOT=$FAKE_STATE/vmroot BANA_LINUX_USER=bana-test
  bash "$bana" up --linux 1 --token GIVEN >"$T/out" 2>&1 || { cat "$T/out"; false; }
  check "root: runs bana again as the runner user" has "$FAKE_LOG" "sudo -iu bana-test env BANA_CONFIG= BANA_REPO=acme/widget BANA_PREFIX=wid"
  check "root: the user may sudo" test -e /etc/sudoers.d/bana-test
  check "root: its copy of the hook ran" has "$FAKE_LOG" "linux hook as 1000 on aarch64"
  check "root: the runner is the user's" test -e /home/bana-test/.bana/wid/runners/wid-ct-linux-arm64-1/.runner
  rm -rf /home/bana-test/.bana /etc/sudoers.d/bana-test
  unset FAKE_OS FAKE_ARCH FAKE_HOST FAKE_UID BANA_SYS_ROOT BANA_LINUX_USER
fi

# ---- bana daemon: CI on push, on this machine ---------------------------------------------------
# A project whose workflow needs the doctor's three fixes, its GitHub (a bare repository
# git reaches for https://github.com/acme/widget.git), a daemon binary and its token.
daemon_world() {
  mkdir -p .github/workflows
  cat >.github/workflows/ci.yml <<'YML'
on:
  push:
    branches: [main]
  workflow_dispatch:
    inputs:
      tier: {type: string}
jobs:
  mac:
    runs-on: [self-hosted, wid-macos]
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ vars.NIGHTLY_REF }}
      - if: runner.environment == 'self-hosted'
        run: ./device-test
      - if: runner.environment == 'self-hosted' || env.ACT == 'true'
        run: ./mix-test
YML
  git add -A && git -c user.name=t -c user.email=t@t commit -q -m one
  git clone -q --bare . "$T/w/origin.git"
  git branch wip # only here, not on GitHub
  git config --global url."file://$T/w/origin.git".insteadOf https://github.com/acme/widget.git
  # shellcheck disable=SC2016 # the stand-in expands these when it runs
  printf '#!/bin/sh\necho "bana-manager $*" >>"$FAKE_LOG"\n' >"$T/w/bana-manager"
  chmod +x "$T/w/bana-manager"
  mkdir -p "$HOME/.bana" && echo 0123456789abcdef0123 >"$HOME/.bana/manager-token"
  export BANA_DAEMON_BIN=$T/w/bana-manager BANA_DAEMON_STEP=0
}
# A property list's key (or, without one, its keys), as JSON.
plist() {
  python3 -c 'import json, plistlib, sys
d = plistlib.load(open(sys.argv[1], "rb"))
print(json.dumps(d[sys.argv[2]] if len(sys.argv) > 2 else sorted(d), separators=(",", ":")))' "$@"
}
# The keys the daemon takes, from its source (any other key stops it).
daemon_keys=$(sed -n '/^const KEYS/,/^];/p' "$here/../manager/src/daemon.rs" | grep -o '"[^"]*"' | tr -d '"')
only_daemon_keys() { # SETTINGS
  local k ok=0
  while read -r k; do
    grep -qx -- "$k" <<<"$daemon_keys" || { echo "  not a daemon key: $k" >&2; ok=1; }
  done < <(awk '!/^#/ { k = substr($0, 1, index($0, "=") - 1); gsub(/[ \t]/, "", k); print k }' "$1")
  return $ok
}
bana_root=$(cd "$here/.." && pwd)

fresh
daemon_world
d=$HOME/.bana/wid
export FAKE_OS=Darwin FAKE_ARCH=arm64 FAKE_HOST=MBP
bash "$bana" daemon install --port 8471 --no-open >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "daemon: the doctor reads act's version" has "$T/out" "act: act version 0.2.89"
hook=$(git rev-parse --git-path hooks)/reference-transaction
check "daemon: a push hook in this checkout" test -x "$hook"
check "daemon: it pokes this daemon's port" has "$hook" "http://127.0.0.1:8471/ci/v1/daemon/poll"
check "daemon: only after a push (origin/ refs, committed)" has "$hook" 'refs/remotes/origin/'
check "daemon: with the token from its file, not in the hook" lacks "$hook" "$(cat "$HOME/.bana/manager-token" 2>/dev/null || echo no-token)"
check "daemon: the doctor warns of a push trigger (no pool here)" has "$T/out" "ci.yml:2: a push trigger"
check "daemon: of a checkout ref:" has "$T/out" "ci.yml:13: a checkout ref:"
check "daemon: of a runner.environment gate without env.ACT" has "$T/out" "ci.yml:14: act never sets runner.environment"
check "daemon: not of one with env.ACT" lacks "$T/out" "ci.yml:16"
check "daemon: git reads the repository through gh" has "$T/out" "git: reads acme/widget through gh"
check "daemon: the snapshot's binary" cmp -s "$T/w/bana-manager" "$d/daemon/bana-manager"
check "daemon: the snapshot's bana" cmp -s "$bana" "$d/daemon/bin/bana"
check "daemon: the snapshot's lib" cmp -s "$here/../lib/daemon.sh" "$d/daemon/lib/daemon.sh"
check "daemon: its clone's origin is GitHub" same "$(git -C "$d/src" config remote.origin.url)" "https://github.com/acme/widget.git"
check "daemon: its clone knows GitHub's default branch" same "$(git -C "$d/src" symbolic-ref --short refs/remotes/origin/HEAD)" \
  "origin/$(git symbolic-ref --short HEAD)"
check "daemon: its clone has GitHub's branches, not your local ones" bash -c "! git -C '$d/src' rev-parse -q --verify refs/remotes/origin/wip"
check "daemon: settings has only the keys the daemon takes" only_daemon_keys "$d/daemon/settings"
cat >"$T/want" <<EOF
repo = acme/widget
prefix = wid
workflow = ci.yml
tiers = quick nightly release
tier_input = tier
daemon.branches = * !dependabot/* !renovate/*
daemon.tags =
daemon.tier = quick
daemon.tag_tier = release
daemon.poll = 30
daemon.timeout = 120
daemon.supersede = queued
daemon.token = gh
port = 8471
host = mbp
login = octo
path = $HOME/.cargo/bin:$PATH
tray = yes
gh = $here/stand-ins/gh
git = $(command -v git)
docker = $here/stand-ins/docker
bash = $T/path/bash
caffeinate = $here/stand-ins/caffeinate
script = $d/daemon/bin/bana
EOF
check "daemon: settings, resolved (bana.conf's path first, absolute programs)" same "$(grep -v '^#' "$d/daemon/settings")" "$(cat "$T/want")"
p=$HOME/Library/LaunchAgents/xyz.tjrb.bana.wid.plist
check "daemon: the LaunchAgent lints" has "$FAKE_LOG" "plutil -lint $p.new."
check "daemon: the LaunchAgent's keys, and no others" same "$(plist "$p")" \
  '["EnvironmentVariables","ExitTimeOut","KeepAlive","Label","LimitLoadToSessionType","ProcessType","ProgramArguments","RunAtLoad","StandardErrorPath","StandardOutPath","ThrottleInterval"]'
check "daemon: its label" same "$(plist "$p" Label)" '"xyz.tjrb.bana.wid"'
check "daemon: it runs the snapshot, daemon --dir" same "$(plist "$p" ProgramArguments)" "[\"$d/daemon/bana-manager\",\"daemon\",\"--dir\",\"$d\"]"
check "daemon: with the captured PATH" same "$(plist "$p" EnvironmentVariables)" "{\"PATH\":\"$HOME/.cargo/bin:$PATH\"}"
check "daemon: RunAtLoad" same "$(plist "$p" RunAtLoad)" true
check "daemon: KeepAlive after a crash, not after Quit" same "$(plist "$p" KeepAlive)" '{"SuccessfulExit":false}'
check "daemon: ThrottleInterval 10" same "$(plist "$p" ThrottleInterval)" 10
check "daemon: ProcessType Interactive" same "$(plist "$p" ProcessType)" '"Interactive"'
check "daemon: in the login session (Aqua)" same "$(plist "$p" LimitLoadToSessionType)" '"Aqua"'
check "daemon: ExitTimeOut 60, for the shutdown ladder" same "$(plist "$p" ExitTimeOut)" 60
check "daemon: its log" same "$(plist "$p" StandardOutPath)$(plist "$p" StandardErrorPath)" \
  "\"$HOME/Library/Logs/bana/wid.log\"\"$HOME/Library/Logs/bana/wid.log\""
check "daemon: launchctl bootout first (errors ignored)" has "$FAKE_LOG" "launchctl bootout gui/1000/xyz.tjrb.bana.wid"
check "daemon: then bootstrap in the login session" has "$FAKE_LOG" "launchctl bootstrap gui/1000 $p"
check "daemon: waits for its health, past any proxy" has "$FAKE_LOG" "--noproxy * --max-time 3 http://127.0.0.1:8471/ci/v1/health"
check "daemon: says where its page is" has "$T/out" "Its page: http://127.0.0.1:8471/#token=0123456789abcdef0123"
check "daemon: --no-open" lacks "$FAKE_LOG" "open http"

# Again, while it builds: it waits for the build, keeps the port, and does not clone again.
echo '{"now":100,"watcher":{},"running":{"id":7,"ref":"main","tier":"quick"},"queue":[],"last":null}' >"$FAKE_STATE/local.json"
echo '{"now":100,"watcher":{},"running":null,"queue":[],"last":null}' >"$FAKE_STATE/local.next"
: >"$FAKE_LOG"
bash "$bana" daemon install --no-tray >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "daemon again: waits for the running build" has "$T/out" "Build #7 (main) runs: restarting the daemon when it ends"
check "daemon again: asked until it ended" test ! -e "$FAKE_STATE/local.next"
check "daemon again: asks with the token, on stdin (not in ps)" has "$FAKE_STATE/curl.stdin" "Authorization: Bearer 0123456789abcdef0123"
check "daemon again: the token is on no curl command line" bash -c "! grep -q '^curl .*0123456789abcdef0123' '$FAKE_LOG'"
check "daemon again: keeps the installed port" has "$d/daemon/settings" "port = 8471"
check "daemon again: does not clone again" lacks "$T/out" "Cloning"
check "daemon again: --no-tray" same "$(plist "$p" ProgramArguments)" "[\"$d/daemon/bana-manager\",\"daemon\",\"--dir\",\"$d\",\"--no-tray\"]"
check "daemon again: and tray = no" has "$d/daemon/settings" "tray = no"
check "daemon again: opens the page" has "$FAKE_LOG" "open http://127.0.0.1:8471/#token="
echo '{"now":100,"watcher":{},"running":{"id":8,"ref":"main"},"queue":[],"last":null}' >"$FAKE_STATE/local.json"
: >"$FAKE_LOG"
bash "$bana" daemon install --now --no-open >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "daemon --now: restarts without waiting" lacks "$FAKE_LOG" "ci/v1/local"
check "daemon --now: restarted" has "$FAKE_LOG" "launchctl bootstrap gui/1000 $p"
: >"$FAKE_LOG"
FAKE_BOOTOUT_SLOW=3 bash "$bana" daemon install --now --no-open >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "daemon: bootstrap waits until launchd has let the old one go" \
  same "$(grep -o '^launchctl [a-z]*' "$FAKE_LOG" | tr '\n' ' ')" \
  "launchctl bootout launchctl print launchctl print launchctl print launchctl print launchctl bootstrap "

cat >"$FAKE_STATE/local.json" <<'JSON'
{"repo":"acme/widget","prefix":"wid","machine":"mbp","now":1000,
 "watcher":{"fetched_at":980,"fetch_error":null,"paused":true,"docker":false,"lock_holder":null,"unposted":2,"post_error":"gh is signed out"},
 "running":{"id":12,"ref":"main","sha":"abc","tier":"quick","trigger":"push","attempt":1,"state":"running","reason":null,
   "started_at":800,"ended_at":null,"elapsed":190,"description":"running on mbp: linux","jobs":[{"key":"linux","state":"running"}]},
 "queue":[{"id":13,"ref":"feat/x","sha":"def","tier":"quick","trigger":"push","queued_at":990,"waiting":null},{"id":14,"ref":"main"}],
 "last":{"id":11,"ref":"main","state":"failure","description":"failed on mbp: mac (test)","jobs":[]},
 "refs":["refs/heads/main"],"tiers":["quick"],"skipped":[],"port":8471}
JSON
bash "$bana" daemon status >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "daemon status: runs, and where" has "$T/out" "The daemon for acme/widget (launchd: loaded): http://127.0.0.1:8471/"
check "daemon status: when it fetched" has "$T/out" "fetched 20 s ago"
check "daemon status: paused" has "$T/out" "paused: new builds wait"
check "daemon status: no Docker" has "$T/out" "Docker does not answer"
check "daemon status: statuses not posted" has "$T/out" "statuses: gh is signed out (2 not posted)"
check "daemon status: the running build" has "$T/out" "running: #12 main (quick, 3 min): running on mbp: linux"
check "daemon status: the queue" has "$T/out" "queued: 2 (next: #13 feat/x)"
check "daemon status: the last build" has "$T/out" "last: #11 main failure: failed on mbp: mac (test)"
: >"$FAKE_LOG"
bash "$bana" daemon poke >/dev/null
check "daemon poke: asks it to fetch" has "$FAKE_LOG" "-X POST http://127.0.0.1:8471/ci/v1/daemon/poll"
bash "$bana" daemon open
check "daemon open: its page, with the token" has "$FAKE_LOG" "open http://127.0.0.1:8471/#token=0123456789abcdef0123"
: >"$FAKE_LOG"
bash "$bana" daemon run
check "daemon run: the snapshot, in the foreground, without the menu bar" has "$FAKE_LOG" "bana-manager daemon --dir $d --no-tray"
check "daemon run: builds nothing when installed" lacks "$FAKE_LOG" "cargo"

: >"$FAKE_LOG"
bash "$bana" manager >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "manager: this project's daemon runs: its page instead" has "$T/out" "acme/widget's daemon serves the page: http://127.0.0.1:8471/#token="
check "manager: opens it" has "$FAKE_LOG" "open http://127.0.0.1:8471/#token="
check "manager: builds and starts nothing" lacks "$FAKE_LOG" "cargo"
FAKE_HEALTH='{"ok":true,"daemon":true,"repo":"o/other","prefix":"other"}' bash "$bana" manager >"$T/out" 2>&1 || true
check "manager: another project's daemon on the port" has "$T/out" "Another project's bana daemon serves port 8471"
FAKE_HEALTH='{"ok":true,"service":"ci","api":1}' bash "$bana" daemon install --no-open >"$T/out" 2>&1 || true
check "daemon install: refuses a port that bana manager has" has "$T/out" "Something else serves port 8471"

mkdir -p "$d/builds/1" "$d/act-cache" && echo '{}' >"$d/state.json" && echo 'K=v' >"$d/vars"
: >"$FAKE_LOG"
bash "$bana" daemon uninstall >"$T/out"
check "daemon uninstall: launchd stops it" has "$FAKE_LOG" "launchctl bootout gui/1000/xyz.tjrb.bana.wid"
check "daemon uninstall: the LaunchAgent goes" test ! -e "$p"
check "daemon uninstall: the push hook goes" test ! -e "$hook"
check "daemon uninstall: the snapshot goes" test ! -e "$d/daemon"
check "daemon uninstall: builds and clone stay" test -e "$d/builds/1" -a -e "$d/src/.git" -a -e "$d/state.json"
bash "$bana" daemon uninstall --purge >"$T/out"
check "daemon uninstall --purge: clone, builds, cache and state go" \
  bash -c "! ls -d '$d/src' '$d/builds' '$d/act-cache' '$d/state.json' 2>/dev/null | grep -q ."
check "daemon uninstall --purge: your vars file stays" test -e "$d/vars"
bash "$bana" daemon status >"$T/out"
check "daemon status: none installed" has "$T/out" "No daemon for wid here"
unset FAKE_OS FAKE_ARCH FAKE_HOST

# Linux: a systemd user service, built with cargo, a workflow the doctor has nothing to say about.
fresh
daemon_world
d=$HOME/.bana/wid
printf 'on:\n  workflow_dispatch:\n' >.github/workflows/ci.yml
git -c user.name=t -c user.email=t@t commit -qam two
unset BANA_DAEMON_BIN
export CARGO_TARGET_DIR=$T/w/target
bash "$bana" daemon install >"$T/out" 2>&1 || { cat "$T/out"; false; }
u=$HOME/.config/systemd/user/bana-wid.service
check "daemon (Linux): nothing to change in the workflow" has "$T/out" "ci.yml: runs as workflow_dispatch, nothing to change"
check "daemon (Linux): built with cargo, locked" has "$FAKE_LOG" "cargo build -q --release --locked --manifest-path $bana_root/manager/Cargo.toml"
check "daemon (Linux): that build is the snapshot" cmp -s "$CARGO_TARGET_DIR/release/bana-manager" "$d/daemon/bana-manager"
check "daemon (Linux): the unit runs the snapshot without a tray" has "$u" \
  "ExecStart=\"$d/daemon/bana-manager\" \"daemon\" \"--dir\" \"$d\" \"--no-tray\""
check "daemon (Linux): with the captured PATH" has "$u" "Environment=\"PATH=$HOME/.cargo/bin:$PATH\""
check "daemon (Linux): restarted after a crash" has "$u" "Restart=on-failure"
check "daemon (Linux): RestartSec" has "$u" "RestartSec=5"
check "daemon (Linux): SIGTERM to the daemon only" has "$u" "KillMode=mixed"
check "daemon (Linux): time for the shutdown ladder" has "$u" "TimeoutStopSec=60"
check "daemon (Linux): started with the session" has "$u" "WantedBy=default.target"
check "daemon (Linux): systemd reads it" has "$FAKE_LOG" "systemctl --user daemon-reload"
check "daemon (Linux): enable --now" has "$FAKE_LOG" "systemctl --user enable --now bana-wid.service"
check "daemon (Linux): the linger hint, when lingering is off" has "$T/out" "sudo loginctl enable-linger"
check "daemon (Linux): no menu bar, no caffeinate" bash -c "grep -qx 'tray = no' '$d/daemon/settings' && ! grep -q caffeinate '$d/daemon/settings'"
check "daemon (Linux): settings has only the keys the daemon takes" only_daemon_keys "$d/daemon/settings"
check "daemon (Linux): the default port" has "$d/daemon/settings" "port = 8470"
: >"$FAKE_LOG"
FAKE_LINGER=yes bash "$bana" daemon install >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "daemon (Linux) again: restarts the running service" has "$FAKE_LOG" "systemctl --user restart bana-wid.service"
check "daemon (Linux) again: lingering: no hint" lacks "$T/out" "enable-linger"
echo '{"now":100,"watcher":{},"running":null,"queue":[],"last":null}' >"$FAKE_STATE/local.json"
bash "$bana" daemon status >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "daemon status (Linux): systemd's view" has "$T/out" "(systemd: active)"
check "daemon status (Linux): idle" has "$T/out" "running: nothing"
: >"$FAKE_LOG"
bash "$bana" daemon log
check "daemon log (Linux): the journal, followed" has "$FAKE_LOG" "journalctl --user -u bana-wid.service -n 200 -f"
: >"$FAKE_LOG"
bash "$bana" daemon run --build >/dev/null 2>&1
check "daemon run --build: builds from this checkout first" has "$FAKE_LOG" "cargo build"
check "daemon run --build: then runs it" has "$FAKE_LOG" "built bana-manager daemon --dir $d --no-tray"
: >"$FAKE_LOG"
bash "$bana" daemon uninstall >/dev/null
check "daemon uninstall (Linux): stops and disables it" has "$FAKE_LOG" "systemctl --user disable --now bana-wid.service"
check "daemon uninstall (Linux): the unit goes" test ! -e "$u"
unset CARGO_TARGET_DIR

# What the doctor stops at, and the settings it checks.
fresh
daemon_world
d=$HOME/.bana/wid
mkdir -p "$d/runners/wid-box-linux-x64-1" && touch "$d/runners/wid-box-linux-x64-1/.runner"
FAKE_GH_SCOPES="'gist'" bash "$bana" daemon install >"$T/out" 2>&1 || true
check "doctor: pool runners here" has "$T/out" "This machine has runners in acme/widget's pool (wid-box-linux-x64-1)"
check "doctor: then no separate push warning" lacks "$T/out" "a push trigger"
check "doctor: a token without the repo scope" has "$T/out" "lacks the repo scope"
bash "$bana" daemon uninstall --purge >/dev/null
FAKE_GH=0 bash "$bana" daemon install >"$T/out" 2>&1 || true
check "doctor: gh signed out" has "$T/out" "gh auth login"
echo 'on: push' >.github/workflows/ci.yml
bash "$bana" daemon install >"$T/out" 2>&1 || true
check "doctor: no workflow_dispatch, no daemon" has "$T/out" "ci.yml has no workflow_dispatch trigger"
check "doctor: and nothing installed" test ! -e "$d/daemon/settings" -a ! -e "$HOME/.config/systemd/user/bana-wid.service"
git checkout -q .github/workflows/ci.yml
BANA_DAEMON_POLL=5 bash "$bana" daemon install >"$T/out" 2>&1 || true
check "daemon install: checks daemon.poll" has "$T/out" "daemon.poll: seconds, at least 10"
BANA_DAEMON_TIER=weekly bash "$bana" daemon install >"$T/out" 2>&1 || true
check "daemon install: checks daemon.tier" has "$T/out" "daemon.tier: one of quick nightly release"
printf 'daemon.tags = v*\ndaemon.tag_tier = nightly\n' >>.github/bana.conf
bash "$bana" settings >"$T/out"
check "settings: daemon.branches' default" has "$T/out" "daemon.branches = * !dependabot/* !renovate/*"
check "settings: daemon.tier, the first tier" has "$T/out" "daemon.tier = quick"
check "settings: daemon.tag_tier from bana.conf" has "$T/out" "daemon.tag_tier = nightly"
check "settings: daemon.tags" has "$T/out" "daemon.tags = v*"
check "settings: daemon.poll" has "$T/out" "daemon.poll = 30"
check "settings: daemon.timeout" has "$T/out" "daemon.timeout = 120"
check "settings: daemon.supersede" has "$T/out" "daemon.supersede = queued"
check "settings: daemon.token" has "$T/out" "daemon.token = gh"
unset BANA_DAEMON_BIN BANA_DAEMON_STEP

# ---- Tart --------------------------------------------------------------------------------------
fresh
export FAKE_OS=Darwin FAKE_ARCH=arm64 FAKE_HOST=mbp
bash "$bana" tart up --cpus 6 >"$T/out" 2>&1 || { cat "$T/out"; false; }
check "tart: clones Cirrus Labs' Debian" has "$FAKE_LOG" "tart clone ghcr.io/cirruslabs/debian:latest bana-tart"
check "tart: sizes it" has "$FAKE_LOG" "tart set bana-tart --cpu 6 --memory 8192 --disk-size 64"
check "tart: a LaunchAgent keeps it running" has "$FAKE_LOG" "launchctl bootstrap"
check "tart: bana goes into the VM" cmp -s "$bana" "$FAKE_STATE/tartfs/usr/local/sbin/bana"
check "tart: so does the linux hook" cmp -s .github/linux-hook.sh "$FAKE_STATE/tartfs/usr/local/share/bana/wid-hook-linux.sh"
check "tart: bana up runs in it as root, with the settings" has "$FAKE_LOG" "BANA_REPO=acme/widget BANA_PREFIX=wid"
check "tart: named after the Mac" has "$FAKE_LOG" "BANA_HOST=mbp-tart BANA_HOOK_LINUX=/usr/local/share/bana/wid-hook-linux.sh"
check "tart: with a token" has "$FAKE_LOG" "BANA_TOKEN=REG-TOKEN"
: >"$FAKE_LOG"
FAKE_POOL="7 wid-mbp-tart-linux-arm64-1 offline" bash "$bana" tart delete >/dev/null
check "tart delete: the VM goes" has "$FAKE_LOG" "tart delete bana-tart"
check "tart delete: its runners leave the pool" has "$FAKE_LOG" "gh api -X DELETE repos/acme/widget/actions/runners/7"
unset FAKE_OS FAKE_ARCH FAKE_HOST

echo "$((n - fails)) of $n passed"
((fails == 0))

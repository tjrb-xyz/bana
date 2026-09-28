#!/usr/bin/env bash
# bana's tests: every command on stand-ins for the programs it drives (uname, orb,
# tart, gh, ioreg, sudo, apt-get, the runner's config.sh and svc.sh), so the macOS
# paths run on Linux too. BASH=/path/to/bash-3.2 tests with macOS's stock shell;
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
  unset FAKE_OS FAKE_ARCH FAKE_UID FAKE_IOREG BANA_SYS_ROOT BANA_TOKEN FAKE_GH FAKE_POOL FAKE_SVC_FAIL
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
check "hooks are relative to bana.conf" has "$T/out" "hook.mac = $T/w/project/.github/mac-hook.sh"
BANA_REPO=o/other bash "$bana" settings >"$T/out"
check "BANA_* overrides bana.conf" has "$T/out" "repo = o/other"
rm .github/bana.conf
bash "$bana" settings >"$T/out"
check "without bana.conf, the repository comes from git's origin" has "$T/out" "repo = acme/widget"
check "and the prefix from its name" has "$T/out" "prefix = widget"

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

mkdir -p target/debug web/node_modules scratch && echo b >target/debug/big && echo m >web/node_modules/m && echo s >scratch/s
RUNNER_ENVIRONMENT=github-hosted bash "$bana" keep-builds >/dev/null
check "keep-builds does nothing on GitHub's runners" test -e scratch/s
RUNNER_ENVIRONMENT=self-hosted bash "$bana" keep-builds
check "keep-builds cleans the checkout" test ! -e scratch/s
check "keep-builds keeps the build caches" test -e target/debug/big -a -e web/node_modules/m
BANA_KEEP_MAX_GB=x RUNNER_ENVIRONMENT=self-hosted bash "$bana" keep-builds 2>"$T/err" || true
check "keep-builds checks keep_max_gb" has "$T/err" "keep_max_gb"

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

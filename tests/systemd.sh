#!/usr/bin/env bash
# Jobs that need systemd, on real Docker and real act: lib/systemd.sh's container (systemd as
# PID 1, unprivileged) on act.image, and act in it running a job in host mode as a user with
# sudo: linger, a user unit that serves, the boundary seen from inside, Ctrl-C, a killed bash,
# the uploads back; then a run as bana ci makes it (the probe, act, sd_after), bana ci itself by
# hand (its view, its log, Ctrl-C), and bana split's runner (bana.yml's build step). It needs
# Docker 28 or later and the image, so it runs only when asked, and skips (saying why) without
# them:
#
#   BANA_SYSTEMD=1 tests/systemd.sh
#
#   BANA_SYSTEMD_ACT=PATH      act, a Linux build (default: act on PATH): it runs here and, mounted
#                              read-only, in the containers
#   BANA_SYSTEMD_IMAGE=IMAGE   the image (default: catthehacker/ubuntu:act-24.04, else ghcr.io's)
#   BANA_SYSTEMD_PULL=1        pull the image when Docker lacks it (CI); it is never built
#   BASH_UNDER_TEST=PATH       the bash of the run that is killed (-9) (default: bash); run this
#                              script with it too (bash 3.2: macOS's); AWK=original-awk: BSD's awk
# Every container it starts is named bana-systemd-test-*, and goes (an EXIT trap); its scratch is
# under TMPDIR. GitHub's ubuntu-latest (cgroup v2, the systemd driver, AppArmor) runs it in
# test.yml's e2e job.
set -euo pipefail

if [[ ${BANA_SYSTEMD:-} != 1 ]]; then
  echo "systemd: skipped (BANA_SYSTEMD=1 runs it: real Docker and act)"
  exit 0
fi
here=$(cd "$(dirname "$0")" && pwd -P)
skip() { echo "systemd: skipped ($*)"; exit 0; }
act=${BANA_SYSTEMD_ACT:-$(command -v act || true)}
[[ -x $act ]] || { echo "systemd: act is needed (BANA_SYSTEMD_ACT=PATH)" >&2; exit 1; }
[[ $(uname -s) == Linux ]] || skip "act runs here and in the containers: a Linux act, on Linux"
v=$(docker version --format '{{.Server.APIVersion}}' 2>/dev/null) || skip "Docker does not answer"
[[ $v =~ ^1\.([0-9]+)$ && ${BASH_REMATCH[1]} -ge 48 ]] || skip "Docker's API is $v: writable cgroups need 1.48 (Docker 28)"
image=${BANA_SYSTEMD_IMAGE:-}
if [[ -z $image ]]; then
  image=catthehacker/ubuntu:act-24.04
  docker image inspect "$image" >/dev/null 2>&1 || image=ghcr.io/catthehacker/ubuntu:act-24.04
fi
if ! docker image inspect "$image" >/dev/null 2>&1; then
  [[ ${BANA_SYSTEMD_PULL:-} == 1 ]] || skip "no $image here (BANA_SYSTEMD_PULL=1 pulls it)"
  docker pull -q "$image" >/dev/null
fi
case $(uname -m) in x86_64 | amd64) arch=linux/amd64 ;; *) arch=linux/arm64 ;; esac

T=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/bana-systemd.XXXXXX")" && pwd -P)
images=$(docker images -q --no-trunc | sort)
# AWK=original-awk: the awk lib/systemd.sh runs (BSD's, as on a Mac), as tests/run.sh has it.
if [[ -n ${AWK:-} ]]; then
  mkdir -p "$T/path"
  ln -s "$(command -v "$AWK")" "$T/path/awk"
  export PATH=$T/path:$PATH
fi
mark=bana-host-marker-$$-$RANDOM
ctr=bana-systemd-test-$$
cleanup() {
  local n
  for n in $(docker ps -a --format '{{.Names}}' | grep '^bana-systemd-test' || true); do docker rm -f "$n" >/dev/null 2>&1 || true; done
  if [[ -n ${marker_pid:-} ]]; then
    kill "$marker_pid" 2>/dev/null || true
    wait "$marker_pid" 2>/dev/null || true
  fi
  rm -rf "$T"
}
trap cleanup EXIT
fails=0 n=0
check() { # NAME COMMAND...: passes when COMMAND succeeds
  local name=$1
  shift
  n=$((n + 1))
  if "$@"; then echo "ok   $name"; else echo "FAIL $name"; fails=$((fails + 1)); fi
}
same() { [[ $1 == "$2" ]] || { printf '  want: %s\n  got:  %s\n' "$2" "$1" >&2; return 1; }; }
has() { grep -qF -- "$2" "$1" || { echo "  $1 lacks: $2" >&2; sed 's/^/  | /' "$1" | tail -40 >&2; return 1; }; }
lacks() { ! grep -qF -- "$2" "$1" || { echo "  $1 has: $2" >&2; return 1; }; }
gone() { # NAME SECONDS: until docker has no container NAME
  local i
  for ((i = 0; i < $2 * 10; i++)); do
    [[ -n $(docker ps -a -q --filter "name=^$1\$") ]] || return 0
    sleep 0.1
  done
  return 1
}
echo "systemd: $image ($arch), Docker API $v, $("$act" --version), bash $BASH_VERSION"

# A host process the containers must not see.
bash -c "exec -a $mark sleep 600" &
marker_pid=$!

# lib/systemd.sh, with lib/split.sh's pins, as bana ci has them.
# shellcheck disable=SC2034 # lib/systemd.sh reads these
{
  base_home=$T/home/.bana
  # shellcheck source=lib/split.sh
  source "$here/../lib/split.sh"
  # shellcheck source=lib/systemd.sh
  source "$here/../lib/systemd.sh"
  sysd_docker=(docker) sysd_act=("$act")
  sd_labels=" wid-systemd " sd_image=$image sd_arch=$arch sd_net='' sd_label='' sd_bin=$act sd_cache=$T/cache
  sd_name=$ctr sd_life='' sd_log=$T/systemd.log sd_art=$T/art sd_probe_file=$T/probe
}

# The project: plan on wid-linux; sd on wid-systemd (needs plan): systemd as a user, the
# boundary, an upload; after (needs sd). int.yml: a job that sleeps, and a step that always runs.
p=$T/p
sd_root=$p sd_wf=$p/.github/workflows/ci.yml
mkdir -p "$p/.github/workflows" "$T/cache" "$T/art" "$T/home"
cd "$p"
git init -q .
cat >.github/workflows/ci.yml <<EOF
name: sdtest
on:
  workflow_dispatch:
    inputs:
      tier: { type: string, default: none }
jobs:
  plan:
    runs-on: wid-linux
    steps:
      - run: echo plan >plan.txt
      - uses: actions/upload-artifact@v4
        with: { name: plan-up, path: plan.txt }
  sd:
    needs: plan
    runs-on: wid-systemd
    steps:
      - name: systemd, as a user with sudo
        run: |
          sudo loginctl enable-linger "\$USER"
          echo "user manager \$(systemctl --user is-system-running)"
          mkdir -p ~/.config/systemd/user
          printf '[Service]\nExecStart=/usr/bin/python3 -m http.server 18480 --bind 127.0.0.1\n' >~/.config/systemd/user/web.service
          systemctl --user daemon-reload
          systemctl --user start web
          code=000
          for i in \$(seq 100); do code=\$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18480/ || true); [ "\$code" = 200 ] && break; sleep 0.1; done
          echo "web answered \$code"
          systemctl --user stop web
          rm ~/.config/systemd/user/web.service
          systemctl --user daemon-reload
          echo "web unit \$(systemctl --user is-active web || true)"
      - name: the run's event and vars, read from their copies (the vars file is root's, mode 600)
        run: echo "tier \${{ inputs.tier }}, var \${{ vars.GREETING }}"
      - name: the boundary
        run: |
          echo "uid \$(id -u) \$(id -un)"
          echo "root's CapEff \$(sudo awk '/^CapEff/ { print \$2 }' /proc/self/status)"
          echo "Seccomp \$(awk '/^Seccomp:/ { print \$2 }' /proc/self/status)"
          [ -e /var/run/docker.sock ] || [ -e /run/docker.sock ] && echo "a docker socket" || echo "no docker socket"
          echo "pid 1: \$(tr '\0' ' ' </proc/1/cmdline)"
          sudo ps -eo args | grep -q '[b]ana-host-marker' && echo "the host's marker seen" || echo "no host process seen"
          [ -n "\$(find /sys/fs/cgroup -maxdepth 2 -name docker 2>/dev/null)" ] && echo "the host's docker cgroup seen" || echo "no host cgroup seen"
          echo sd >sd.txt
      - uses: actions/upload-artifact@v4
        with: { name: sd-up, path: sd.txt }
  after:
    needs: sd
    runs-on: wid-linux
    steps:
      - run: echo after
EOF
cat >.github/workflows/int.yml <<'EOF'
name: sdint
on: workflow_dispatch
jobs:
  sdint:
    runs-on: wid-systemd
    steps:
      - run: echo sleeping; sleep 30
      - if: always()
        run: echo always ran
EOF
git add -A
git -c user.name=t -c user.email=t@t commit -q -m one
# act's actions, fetched here (as bana ci's dry run does), for the containers' copies.
"$act" workflow_dispatch -C "$p" -W "$sd_wf" -P "wid-linux=$image" -P "wid-systemd=$image" --action-cache-path "$T/cache" \
  -n --concurrent-jobs 1 >/dev/null 2>&1 || true

# ---- the container ----------------------------------------------------------------------------
t0=$SECONDS
sd_up "$ctr" "$image" "$arch" "" "" "$act" "$T/cache" "$p" || { echo "sd_up: $sd_err" >&2; exit 1; }
check "container: systemd up in under 30 s" test $((SECONDS - t0)) -lt 30
check "container: no failed unit" same "$(docker exec "$ctr" systemctl --failed --no-legend --plain)" ""
check "container: Docker, containerd and ssh masked" same \
  "$(docker exec "$ctr" systemctl is-enabled docker.service docker.socket containerd.service ssh.service ssh.socket 2>&1 | sort -u)" masked-runtime
check "container: so are the timers" same \
  "$(docker exec "$ctr" systemctl is-enabled apt-daily.timer apt-daily-upgrade.timer motd-news.timer dpkg-db-backup.timer e2scrub_all.timer fstrim.timer 2>&1 | sort -u)" masked-runtime
check "container: nothing listens on :22" same "$(docker exec "$ctr" sh -c 'cat /proc/net/tcp /proc/net/tcp6 2>/dev/null' | awk '$4 == "0A" && $2 ~ /:0016$/')" ""
check "container: not privileged, no capability added, its own cgroup namespace, no host PID or IPC namespace" same \
  "$(docker inspect -f '{{.HostConfig.Privileged}} {{json .HostConfig.CapAdd}} {{.HostConfig.CgroupnsMode}} [{{.HostConfig.PidMode}}] [{{.HostConfig.IpcMode}}] {{json .HostConfig.Devices}}' "$ctr")" \
  "false null private [] [private] []"
check "container: writable cgroups, the one security option" same "$(docker inspect -f '{{json .HostConfig.SecurityOpt}}' "$ctr")" '["writable-cgroups=true"]'
check "container: every mount read-only (act, the cache, the checkout)" same \
  "$(docker inspect -f '{{range .Mounts}}{{.Destination}}={{.RW}}{{println}}{{end}}' "$ctr" | sed '/^$/d' | sort)" \
  "$(printf '%s\n' /bana/bin/act=false /bana/in/cache=false "$p=false" | sort)"
check "container: runner, the uid here (1001 for root)" same "$(docker exec "$ctr" id -u runner)" "$(($(id -u) ? $(id -u) : 1001))"
check "container: runner's user manager runs (lingering)" same "$(docker exec "$ctr" systemctl is-active "user@$sd_uid.service")" active
check "container: act's actions, runner's copy" same "$(docker exec "$ctr" stat -c %U /home/runner/.cache/act/actions-upload-artifact@v4)" runner

# act inside: the job in host mode, as runner.
(sd_act "$ctr" "$sd_uid" workflow_dispatch -C "$p" -W "$sd_wf" -P wid-linux=-self-hosted -P wid-systemd=-self-hosted -j sd \
  --concurrent-jobs 1 --action-offline-mode --artifact-server-path /home/runner/.bana/artifacts \
  --action-cache-path /home/runner/.cache/act) >"$T/act.out" 2>&1 && st=0 || st=$?
check "act inside: the job passes" same "$st" 0
check "act inside: linger, and runner's user manager runs" has "$T/act.out" "| user manager running"
check "act inside: a user unit serves" has "$T/act.out" "| web answered 200"
check "act inside: then stops, and goes" has "$T/act.out" "| web unit inactive"
check "act inside: as runner" has "$T/act.out" "| uid $sd_uid runner"
check "act inside: root's capabilities, Docker's default (no SYS_ADMIN, no SYS_PTRACE)" has "$T/act.out" "| root's CapEff 00000000a80425fb"
check "act inside: seccomp's filter" has "$T/act.out" "| Seccomp 2"
check "act inside: no docker socket" has "$T/act.out" "| no docker socket"
check "act inside: systemd is pid 1" has "$T/act.out" "| pid 1: /sbin/init "
check "act inside: no host process seen" has "$T/act.out" "| no host process seen"
check "act inside: the cgroups are its own" has "$T/act.out" "| no host cgroup seen"
# Its uploads come back: sd's, and plan's (run again inside) not, as it is here already.
mkdir -p "$T/art/1/plan-up"
echo "the run's own" >"$T/art/1/plan-up/kept"
sd_copy_out "$ctr" "$T/art" || { echo "sd_copy_out: $sd_err" >&2; false; }
check "copy-out: the job's upload" test -n "$(find "$T/art/1/sd-up" -type f 2>/dev/null)"
check "copy-out: this user's" same "$(find "$T/art/1/sd-up" ! -user "$(id -u)" | wc -l | tr -d ' ')" 0
check "copy-out: an upload here already stays as it is" same "$(ls "$T/art/1/plan-up")" kept
sd_down "$ctr"
check "container: gone" gone "$ctr" 5

# act's status comes back as it is: a stand-in act that exits 3.
printf '#!/bin/sh\nexit 3\n' >"$T/exit3"
chmod 755 "$T/exit3"
sd_up "$ctr-3" "$image" "$arch" "" "" "$T/exit3" "" "$p" || { echo "sd_up: $sd_err" >&2; exit 1; }
(sd_act "$ctr-3" "$sd_uid" anything) && st=0 || st=$?
check "act inside: its exit status (3) comes back" same "$st" 3
sd_down "$ctr-3"

# Ctrl-C, through sd_int: act stops the job, its always() step runs, act ends 1.
sd_up "$ctr" "$image" "$arch" "" "" "$act" "$T/cache" "$p" || { echo "sd_up: $sd_err" >&2; exit 1; }
set -m
(sd_act "$ctr" "$sd_uid" workflow_dispatch -C "$p" -W "$p/.github/workflows/int.yml" -P wid-systemd=-self-hosted) >"$T/int.out" 2>&1 &
pid=$!
set +m
for ((i = 0; i < 300; i++)); do ! grep -q '| sleeping' "$T/int.out" || break; sleep 0.1; done
sleep 1
sd_live=$ctr
t0=$SECONDS
sd_int
sd_wait "$pid" && st=0 || st=$?
sd_live=''
check "sd_int: act ends, as Ctrl-C ends it" same "$st" 1
check "sd_int: before the sleep did" test $((SECONDS - t0)) -lt 20
check "sd_int: the always() step ran" has "$T/int.out" "| always ran"
sd_down "$ctr"

# A bash killed (-9) with the container up: its lifeline powers it off, and --rm removes it.
# shellcheck disable=SC2016 # that bash's
"${BASH_UNDER_TEST:-bash}" -c 'set -euo pipefail
  base_home=$1/home/.bana
  source "$2/../lib/split.sh"
  source "$2/../lib/systemd.sh"
  sysd_docker=(docker) sd_root=$3
  sd_up "$4" "$5" "$6" "" "" "$7" "" "$3"
  sd_lifeline "$4"
  : >"$1/ready"
  sleep 600' killed "$T" "$here" "$p" "$ctr-k" "$image" "$arch" "$act" &
kpid=$!
for ((i = 0; i < 600; i++)); do [[ ! -e $T/ready ]] || break; sleep 0.1; done
check "lifeline: the container is up" test -n "$(docker ps -q --filter "name=^$ctr-k\$")"
kill -KILL "$kpid"
wait "$kpid" 2>/dev/null || true
check "lifeline: bash killed, the container gone within 5 s" gone "$ctr-k" 5

# ---- a run, as bana ci makes it: the probe, bana's lines, act, then sd_after (JSON) -----------
printf '{"inputs":{"tier":"quick"}}\n' >"$T/event.json"
printf 'GREETING=hello\n' >"$T/vars"
chmod 600 "$T/vars"
sd_args=(workflow_dispatch -C "$p" -W "$sd_wf" -P "wid-linux=$image" -P wid-systemd= --pull=false -e "$T/event.json"
  --var-file "$T/vars" --container-architecture "$arch" --artifact-server-path "$T/art2" --action-cache-path "$T/cache" --json)
mkdir -p "$T/art2"
sd_art=$T/art2
sd_probe "$sd_probe_file" "${sd_args[@]}" || { echo "sd_probe failed" >&2; exit 1; }
sd_args+=(--action-offline-mode)
check "run: the probe finds sd" same "$(awk -F'\t' '$4 == 1 { print $1 }' "$sd_probe_file")" sd
sd_next json >"$T/run.out"
"$act" "${sd_args[@]}" </dev/null >"$T/t.1" 2>"$T/t.2" && st=0 || st=$?
check "run: act passes, sd skipped" same "$st $(grep -c 'Skipping unsupported platform' "$T/t.1")" "0 1"
sd_after json "$T/t.1" "$T/t.2" >>"$T/run.out" 2>"$T/run.err" && st=0 || st=$?
check "run: sd_after passes" same "$st" 0
check "run: JSON lines" python3 -c 'import json,sys; [json.loads(l) for l in open(sys.argv[1]) if l.strip()]' "$T/run.out"
check "run: bana's next line, then sd's success, once" same \
  "$(grep -o '"jobID":"sd","jobResult":"success"\|"msg":"bana: next, in a systemd container"' "$T/run.out" | tr '\n' ' ')" \
  '"msg":"bana: next, in a systemd container" "jobID":"sd","jobResult":"success" '
check "run: plan's lines, run again in the container, left out" same "$(grep -c '"jobID":"plan"' "$T/run.out")" 0
check "run: after is not run here, and says why" has "$T/run.out" '"jobID":"after","matrix":{},"msg":"bana: not run here: needs sd, a systemd job"'
check "run: the event and the vars, from their copies in the container" has "$T/run.out" '"msg":"tier quick, var hello\n"'
check "run: the boundary held there too" same "$(grep -cF -e '"msg":"no host process seen\n"' -e '"msg":"no docker socket\n"' -e '"msg":"web answered 200\n"' "$T/run.out")" 3
check "run: all of the container's act in sd_log" has "$sd_log" '"jobID":"plan","jobResult":"success"'
check "run: both uploads here (plan's from act's run, sd's from its container)" same \
  "$(cd "$T/art2" && find . -mindepth 2 -maxdepth 2 | sort | tr '\n' ' ')" "./1/plan-up ./1/sd-up "
check "run: nothing on stderr" same "$(cat "$T/run.err")" ""

# ---- bana ci by hand: the view, the log, Ctrl-C ------------------------------------------------
# A project of its own (prefix test-ci: its containers are bana-systemd-test-ci, which the EXIT
# trap knows): plan; sd and sd2 (it needs sd) on test-ci-systemd, as a user with systemd; after
# (it needs sd). slow.yml: a systemd job that sleeps, and a step that always runs.
q=$T/q
mkdir -p "$q/.github/workflows" "$T/hand/bin" "$T/hand/home"
ln -s "$act" "$T/hand/bin/act"
printf '#!/bin/sh\nexit 1\n' >"$T/hand/bin/gh"
chmod 755 "$T/hand/bin/gh"
cd "$q"
git init -q .
printf 'repo = acme/sdtest\nprefix = test-ci\ntiers =\nact.image = %s\nact.args = --pull=false\n' "$image" >.github/bana.conf
cat >.github/workflows/ci.yml <<'EOF2'
name: hand
on: workflow_dispatch
jobs:
  plan:
    runs-on: [self-hosted, test-ci-linux]
    steps:
      - run: echo plan
  sd:
    needs: plan
    runs-on: [self-hosted, test-ci-systemd]
    steps:
      - run: |
          sudo loginctl enable-linger "$USER"
          echo "sd: user manager $(systemctl --user is-system-running)"
  sd2:
    needs: sd
    runs-on: [self-hosted, test-ci-systemd]
    steps:
      - run: |
          echo "sd2: user manager $(systemctl --user is-system-running)"
  after:
    needs: sd
    runs-on: [self-hosted, test-ci-linux]
    steps:
      - run: echo after
EOF2
cat >.github/workflows/slow.yml <<'EOF2'
name: slow
on: workflow_dispatch
jobs:
  slow:
    runs-on: [self-hosted, test-ci-systemd]
    steps:
      - run: echo sleeping; sleep 30
      - if: always()
        run: echo always ran
EOF2
git add -A
git -c user.name=t -c user.email=t@t commit -q -m one
hand() { # bana ci here, by hand, with the real act (a Linux one, for the containers too)
  env HOME="$T/hand/home" PATH="$T/hand/bin:$PATH" BANA_SYSTEMD_ACT="$act" CARGO_TARGET_DIR="$T/none" \
    "${BASH_UNDER_TEST:-bash}" "$here/../bin/bana" ci
}
ci=$T/hand/home/.bana/test-ci/ci
hand >"$T/hand.out" 2>&1 && st=0 || st=$?
check "hand: passes" same "$st $(sed -n 's/^exit=//p' "$ci/last.env")" "0 0"
check "hand: the view: sd next" has "$T/hand.out" "→ sd: next, in a systemd container"
check "hand: the view: sd passed" grep -qx '✓ sd' "$T/hand.out"
check "hand: the view: sd2 passed" grep -qx '✓ sd2' "$T/hand.out"
check "hand: the view: after is not run here, and why" has "$T/hand.out" "– after: not run here (needs sd, a systemd job)"
check "hand: the view's count" grep -q '^3 passed, 1 not run here · ' "$T/hand.out"
check "hand: systemd in sd, as a user" has "$ci/last.log" "| sd: user manager running"
check "hand: and in sd2" has "$ci/last.log" "| sd2: user manager running"
check "hand: plan's end once in last.log (its runs again inside are in last.systemd.log)" same \
  "$(grep -c '^\[hand/plan *\] 🏁  Job succeeded' "$ci/last.log") $(grep -c '^\[hand/plan *\] 🏁  Job succeeded' "$ci/last.systemd.log")" "1 2"
check "hand: no container left" same "$(docker ps -a --format '{{.Names}}' | grep '^bana-systemd-test-ci' || true)" ""
check "hand: no lock left" test ! -e "$T/hand/home/.bana/act.lock"
# Ctrl-C (SIGINT to the group, as a terminal sends it) while the systemd job sleeps.
own_group=(python3 -c 'import os, signal, sys
os.setpgid(0, 0)
signal.signal(signal.SIGINT, signal.SIG_DFL)
os.execvp(sys.argv[1], sys.argv[1:])')
env HOME="$T/hand/home" PATH="$T/hand/bin:$PATH" BANA_SYSTEMD_ACT="$act" CARGO_TARGET_DIR="$T/none" BANA_WORKFLOW=slow.yml \
  "${own_group[@]}" "${BASH_UNDER_TEST:-bash}" "$here/../bin/bana" ci >"$T/slow.out" 2>&1 &
hp=$!
for ((i = 0; i < 600; i++)); do ! grep -qs '| sleeping' "$ci/last.systemd.log" || break; sleep 0.1; done
sleep 1
t0=$SECONDS
kill -INT -- "-$hp"
wait "$hp" && st=0 || st=$?
check "hand: Ctrl-C: act in the container stops the job, before its sleep would have" test $((SECONDS - t0)) -lt 20
check "hand: Ctrl-C: its always() step ran" has "$ci/last.log" "| always ran"
check "hand: Ctrl-C: stopped" same "$st $(sed -n 's/^stopped=//p' "$ci/last.env")" "1 1"
check "hand: Ctrl-C: no container left" same "$(docker ps -a --format '{{.Names}}' | grep '^bana-systemd-test-ci' || true)" ""
check "hand: Ctrl-C: no lock left" test ! -e "$T/hand/home/.bana/act.lock"
cd "$p"

# ---- bana split's runner: the same, on GitHub's side (bana.yml's runner.sh, as rendered) ---------
# A private project (plan; sd on wid-systemd, it needs plan: systemd as a user, the host's marker
# looked for, an upload; after, it needs sd), its bana.yml rendered here, and the runner's build
# step on its checkout, with RUNNER_TEMP in scratch and act this test's: as on GitHub, but here.
s=$T/split
mkdir -p "$s/p/.github/workflows" "$s/home" "$s/rt/bana"
cd "$s/p"
git init -q .
printf 'repo = acme/sdsplit\nprefix = wid\ntiers = quick\nact.image = %s\n' "$image" >.github/bana.conf
cat >.github/workflows/ci.yml <<EOF
name: ci
on:
  workflow_dispatch:
    inputs:
      tier: {type: string}
jobs:
  plan:
    runs-on: [self-hosted, wid-linux]
    steps:
      - run: echo PRIVATE-PLAN
  sd:
    needs: plan
    runs-on: [self-hosted, wid-systemd]
    steps:
      - name: PRIVATE-NAME
        run: |
          sudo loginctl enable-linger "\$USER"
          echo "PRIVATE-HOST user manager \$(systemctl --user is-system-running)"
          sudo ps -eo args | grep -q '[b]ana-host-marker' && echo "the host's marker seen" || echo "no host process seen"
          [ -e /var/run/docker.sock ] || [ -e /run/docker.sock ] && echo "a docker socket" || echo "no docker socket"
          [ -e "\$RUNNER_TEMP/bana/key" ] && echo "the key's path seen" || echo "no key path"
          echo up >up.txt
      - uses: actions/upload-artifact@v4
        with: {name: sd-up, path: up.txt}
  after:
    needs: sd
    runs-on: [self-hosted, wid-linux]
    steps:
      - run: echo after
EOF
git add -A
git -c user.name=t -c user.email=t@t commit -q -m one
env HOME="$s/home" PATH="$T/hand/bin:$PATH" "${BASH_UNDER_TEST:-bash}" "$here/../bin/bana" split render >"$s/bana.yml" 2>"$s/err" ||
  { cat "$s/err" >&2; exit 1; }
check "split: bana.yml lints" same "$(env HOME="$s/home" "${BASH_UNDER_TEST:-bash}" "$here/../bin/bana" split lint "$s/bana.yml" 2>&1)" "$s/bana.yml: as bana runs it"
awk '/<<.BANA_RUNNER.$/ { on = 1; next } on && /^ *BANA_RUNNER$/ { exit } on { sub(/^          /, ""); print }' "$s/bana.yml" >"$s/runner.sh"
git clone -q "$s/p" "$s/rt/bana/src"
env RUNNER_TEMP="$s/rt" BANA_ID=test-split-1 BANA_SHA="$(git rev-parse HEAD)" BANA_TIER=quick BANA_JOB='' BANA_LOGS=private \
  BANA_TEST_ACT="$act" GITHUB_TOKEN=ghp_not_for_the_container "${BASH_UNDER_TEST:-bash}" "$s/runner.sh" build >"$s/console.txt" 2>&1 && st=0 || st=$?
o=$s/rt/bana/out
check "split: the build passes" same "$st $(cat "$o/rc")" "0 0"
check "split: the console has sd, by its id" grep -qx 'sd: ok' "$s/console.txt"
check "split: and its steps, by their ids" grep -qx 'sd / step 0: ok ([0-9]*s)' "$s/console.txt"
check "split: no name, output or bana line in public" bash -c "! grep -qE 'PRIVATE|bana: |systemd container' '$s/console.txt'"
check "split: act.jsonl: sd's success once, plan's once" same \
  "$(grep -c '"jobID":"sd","jobResult":"success"' "$o/act.jsonl") $(grep -c '"jobID":"plan","jobResult":"success"' "$o/act.jsonl")" "1 1"
check "split: systemd ran for its user" has "$o/act.jsonl" '"msg":"PRIVATE-HOST user manager running\n"'
check "split: the boundary held: no host process, no docker socket, no key path" same \
  "$(grep -cF -e '"msg":"no host process seen\n"' -e '"msg":"no docker socket\n"' -e '"msg":"no key path\n"' "$o/act.jsonl")" 3
check "split: after is not run here, and why" has "$o/act.jsonl" '"jobID":"after","matrix":{},"msg":"bana: not run here: needs sd, a systemd job"'
check "split: all the container's act said, in out/systemd.log" has "$o/systemd.log" '"jobID":"plan","jobResult":"success"'
check "split: sd's upload, in art/" test -n "$(find "$s/rt/bana/art" -path '*/sd-up/*' -type f 2>/dev/null)"
check "split: no container left" same "$(docker ps -a --format '{{.Names}}' | grep '^bana-systemd-test-split' || true)" ""
cd "$p"

# ---- nothing left ------------------------------------------------------------------------------
check "after: no bana-systemd-test container" same "$(docker ps -a --format '{{.Names}}' | grep '^bana-systemd-test' || true)" ""
check "after: no act container of these workflows" same "$(docker ps -a --format '{{.Names}}' | grep 'act-sd\(test\|int\)' || true)" ""
check "after: Docker's images as before (none built, none pulled)" same "$(docker images -q --no-trunc | sort)" "$images"
echo "$((n - fails)) of $n passed"
((fails == 0))

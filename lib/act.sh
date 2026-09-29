# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets os, prefix, base_home
# bana ci: the project's workflow on this machine with act (https://github.com/nektos/act),
# in OrbStack's Docker (or any Docker). No runners, no pool: a check before you push.
# Sourced by bin/bana.
#
#   bana ci [TIER] [options] [-- ACT-OPTIONS]
#     TIER             the workflow's tier input (default: the first of bana.conf's tiers)
#     -j JOB           only this job and the jobs it needs
#     --x64            Linux containers as x86_64 (on Apple silicon, through OrbStack's Rosetta)
#     --list           the jobs, without running them
#     -n, --dry-run    what would run
#     --event FILE     run with this event (a push's payload), whose inputs carry the tier
#   Linux jobs (<prefix>-linux, ubuntu-*) run in containers from bana.conf's act.image; on a
#   Mac, macOS jobs (<prefix>-macos) run on the Mac itself, with its CoreAudio and USB devices.
#   One act runs at a time on this machine: bana ci refuses while another one runs.
#   bana.conf's act.args go to act too (e.g. --reuse to keep containers, and their builds).
#   Anything after -- goes to act, and wins over act.args.

act_usage() { awk '/^#   bana ci/, /^#   Anything/ { sub(/^# ?/, ""); print }' "$bana_root/lib/act.sh" >&2; exit 2; }

# act talks to Docker's socket; OrbStack's is found through the docker CLI's context.
act_docker() {
  command -v docker >/dev/null || die "Docker is needed: install OrbStack (https://orbstack.dev)"
  if [[ -z ${DOCKER_HOST:-} && ! -S /var/run/docker.sock ]]; then
    local h
    h=$(docker context inspect --format '{{.Endpoints.docker.Host}}' 2>/dev/null || true)
    [[ -z $h ]] || export DOCKER_HOST=$h
  fi
  docker info >/dev/null 2>&1 || die "Docker is not running: start OrbStack (or Docker), then try again"
}

# When PID started, as the lock's owner file has it (empty: no such process).
act_started() { LC_ALL=C ps -o lstart= -p "$1" 2>/dev/null | awk '{ $1 = $1; print }'; }

# One act at a time on this machine, for bana ci and the daemon's builds: act names its
# containers without a run id (a second run removes the first one's), and its artifact
# server's port is fixed. The lock is ~/.bana/act.lock, a directory whose owner file has
# three lines: a pid, when that pid started, and a label. bana ci's pid becomes act's
# (exec), so the lock holds while act runs. Once the pid is gone, or is another process
# (another start time), the lock is stale and the next taker removes it.
act_lock() { # LABEL
  local lock=$base_home/act.lock owner pid start tries=0
  mkdir -p "$base_home"
  until mkdir "$lock" 2>/dev/null; do
    tries=$((tries + 1))
    ((tries < 5)) || die "Cannot take $lock"
    # No owner file yet: its taker may be writing it.
    owner=$(cat "$lock/owner" 2>/dev/null) || { sleep 1; owner=$(cat "$lock/owner" 2>/dev/null || true); }
    pid=$(sed -n 1p <<<"$owner")
    start=$(sed -n 2p <<<"$owner")
    if [[ $pid =~ ^[0-9]+$ ]] && kill -0 "$pid" 2>/dev/null && [[ $(act_started "$pid") == "$start" ]]; then
      die "act is busy here: $(sed -n 3p <<<"$owner")"
    fi
    # Stale: it goes, unless someone took it over meanwhile.
    [[ $(cat "$lock/owner" 2>/dev/null || true) != "$owner" ]] || rm -rf "$lock"
  done
  # Until act starts; if it cannot start, its pid is gone and the lock stale anyway.
  trap 'rm -rf "$base_home/act.lock"' EXIT
  printf '%s\n' "$$" "$(act_started $$)" "$1" >"$lock/owner.$$"
  mv "$lock/owner.$$" "$lock/owner"
}

act_main() {
  local tier='' x64='' list='' dry='' event='' secrets='' locked jobs=() pass=() args=() extra=()
  local wf root here tiers image arch i o dc
  while (($#)); do
    case $1 in
    -j | --job) jobs=(-j "${2:?-j JOB}"); shift ;;
    --x64) x64=1 ;;
    --list | -l) list=1 ;;
    -n | --dry-run) dry=1 ;;
    --event) event=${2:?--event FILE}; shift ;;
    --) shift; pass=("$@"); break ;;
    -h | --help | help) act_usage ;;
    -*) act_usage ;;
    *) [[ -z $tier ]] || act_usage; tier=$1 ;;
    esac
    shift
  done
  command -v act >/dev/null || die "act is needed: brew install act (https://nektosact.com)"
  # BANA_PROJECT_ROOT: the checkout to run, when not the one here (the daemon's).
  if [[ -n ${BANA_PROJECT_ROOT:-} ]]; then
    root=$(cd "$BANA_PROJECT_ROOT" 2>/dev/null && pwd -P) || die "BANA_PROJECT_ROOT: no directory $BANA_PROJECT_ROOT"
  else
    root=$(git rev-parse --show-toplevel 2>/dev/null) || die "Run bana ci in the project's checkout"
  fi
  # Neither reaches act's jobs: a job's own bana works on the job's checkout.
  locked=${BANA_ACT_LOCKED:-}
  unset BANA_PROJECT_ROOT BANA_ACT_LOCKED
  wf=$root/.github/workflows/$(conf workflow ci.yml)
  [[ -f $wf ]] || die "No workflow $wf (bana.conf: workflow)"
  if [[ -n $list ]]; then
    exec act -l -C "$root" -W "$wf"
  fi
  tiers=$(words "$(conf tiers "quick nightly release")")
  if [[ -n ${tiers// /} ]]; then
    tier=${tier:-${tiers%% *}}
    case " $tiers " in *" $tier "*) ;; *) die "tier: one of $tiers, not '$tier'" ;; esac
  elif [[ -n $tier ]]; then
    die "This workflow takes no tier (bana.conf: tiers)"
  fi
  [[ -z $event || -f $event ]] || die "--event: no file $event"
  [[ $locked == 1 ]] || act_lock "bana ci${tier:+ $tier} ($prefix)"
  act_docker

  image=$(conf act.image catthehacker/ubuntu:act-24.04)
  args=(-C "$root" -W "$wf" --artifact-server-path "$base_home/act/artifacts")
  for l in "$prefix-linux" ubuntu-latest ubuntu-24.04 ubuntu-22.04; do args+=(-P "$l=$image"); done
  # A macOS job runs here on a Mac (act's host mode); elsewhere act skips it.
  [[ $os != Darwin ]] || args+=(-P "$prefix-macos=-self-hosted" -P macos-latest=-self-hosted)
  if [[ -n $x64 ]]; then arch=linux/amd64
  elif [[ $(cpu) == arm64 ]]; then arch=linux/arm64
  else arch=linux/amd64; fi
  args+=(--container-architecture "$arch")
  # Each job gets a localhost of its own, as on GitHub, where each job has its own machine:
  # act's default (host) gives all of a run's Linux jobs the Docker host's, so the servers
  # of jobs running side by side answer each other (act.network = host for that).
  args+=(--network "$(conf act.network bridge)")

  # act reads its files relative to -C: relative paths stay relative to where bana ci runs.
  here=$(pwd -P)
  if [[ -n $event ]]; then
    # act ignores --input once it has an event: the event's inputs carry the tier.
    [[ $event == /* ]] || event=$here/$event
    args+=(-e "$event")
  elif [[ -n $tier ]]; then
    args+=(--input "$(conf tier_input tier)=$tier")
  fi
  for ((i = 0; i < ${#pass[@]}; i++)); do
    o=${pass[i]}
    case $o in
    -e | --eventpath | -W | --workflows | --secret-file | --env-file | --var-file | --input-file)
      [[ ${pass[i + 1]:-/} == /* ]] || pass[i + 1]=$here/${pass[i + 1]} ;;
    --eventpath=* | --workflows=* | --secret-file=* | --env-file=* | --var-file=* | --input-file=*)
      [[ ${o#*=} == /* ]] || pass[i]=${o%%=*}=$here/${o#*=} ;;
    esac
    case $o in --secret-file | --secret-file=*) secrets=1 ;; esac
  done
  [[ -z $dry ]] || args+=(-n)
  # The token some jobs use (to read the repository's artifacts), from the GitHub CLI,
  # unless a --secret-file brings the secrets.
  if [[ -z $secrets ]]; then
    if [[ -z ${GITHUB_TOKEN:-} ]] && gh_ok; then
      GITHUB_TOKEN=$(gh auth token 2>/dev/null || true)
      export GITHUB_TOKEN
    fi
    [[ -z ${GITHUB_TOKEN:-} ]] || args+=(-s GITHUB_TOKEN)
  fi
  # act pulls with Docker's logins, which on a Mac sit in the Keychain: macOS then asks for
  # your password whenever act reads them. The images CI needs are public, so act gets a
  # Docker config of its own, without logins (act.docker_config = ~/.docker for yours).
  dc=$(conf act.docker_config)
  case $dc in
  '') dc=$base_home/docker; mkdir -p "$dc"; [[ -s $dc/config.json ]] || echo '{}' >"$dc/config.json" ;;
  "~"/*) dc=$HOME/${dc#\~/} ;;
  esac
  export DOCKER_CONFIG=$dc
  # The commit's own act options; those after -- come later, so they win.
  read -r -a extra <<<"$(conf act.args)" || true
  say "act: ${tier:-the workflow} from $(basename "$wf"), Linux jobs in $image ($arch)$([[ $os == Darwin ]] && echo ", macOS jobs on this Mac")"
  exec act workflow_dispatch "${args[@]}" ${extra[@]+"${extra[@]}"} ${jobs[@]+"${jobs[@]}"} ${pass[@]+"${pass[@]}"}
}

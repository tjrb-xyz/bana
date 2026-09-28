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
#   Linux jobs (<prefix>-linux, ubuntu-*) run in containers from bana.conf's act.image; on a
#   Mac, macOS jobs (<prefix>-macos) run on the Mac itself, with its CoreAudio and USB devices.
#   Anything after -- goes to act (e.g. -- --reuse to keep containers, and their builds, between runs).

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

act_main() {
  local tier='' x64='' list='' dry='' jobs=() pass=() args=() wf root tiers input image arch
  while (($#)); do
    case $1 in
    -j | --job) jobs=(-j "${2:?-j JOB}"); shift ;;
    --x64) x64=1 ;;
    --list | -l) list=1 ;;
    -n | --dry-run) dry=1 ;;
    --) shift; pass=("$@"); break ;;
    -h | --help | help) act_usage ;;
    -*) act_usage ;;
    *) [[ -z $tier ]] || act_usage; tier=$1 ;;
    esac
    shift
  done
  command -v act >/dev/null || die "act is needed: brew install act (https://nektosact.com)"
  root=$(git rev-parse --show-toplevel 2>/dev/null) || die "Run bana ci in the project's checkout"
  wf=$root/.github/workflows/$(conf workflow ci.yml)
  [[ -f $wf ]] || die "No workflow $wf (bana.conf: workflow)"
  if [[ -n $list ]]; then
    exec act -l -W "$wf"
  fi
  act_docker

  image=$(conf act.image catthehacker/ubuntu:act-24.04)
  args=(-W "$wf" --artifact-server-path "$base_home/act/artifacts")
  for l in "$prefix-linux" ubuntu-latest ubuntu-24.04 ubuntu-22.04; do args+=(-P "$l=$image"); done
  # A macOS job runs here on a Mac (act's host mode); elsewhere act skips it.
  [[ $os != Darwin ]] || args+=(-P "$prefix-macos=-self-hosted" -P macos-latest=-self-hosted)
  if [[ -n $x64 ]]; then arch=linux/amd64
  elif [[ $(cpu) == arm64 ]]; then arch=linux/arm64
  else arch=linux/amd64; fi
  args+=(--container-architecture "$arch")

  tiers=$(words "$(conf tiers "quick nightly release")")
  if [[ -n ${tiers// /} ]]; then
    tier=${tier:-${tiers%% *}}
    input=$(conf tier_input tier)
    case " $tiers " in *" $tier "*) ;; *) die "tier: one of $tiers, not '$tier'" ;; esac
    args+=(--input "$input=$tier")
  elif [[ -n $tier ]]; then
    die "This workflow takes no tier (bana.conf: tiers)"
  fi
  [[ -z $dry ]] || args+=(-n)
  # The token some jobs use (to read the repository's artifacts), from the GitHub CLI.
  if [[ -z ${GITHUB_TOKEN:-} ]] && gh_ok; then
    GITHUB_TOKEN=$(gh auth token 2>/dev/null || true)
    export GITHUB_TOKEN
  fi
  [[ -z ${GITHUB_TOKEN:-} ]] || args+=(-s GITHUB_TOKEN)
  say "act: ${tier:-the workflow} from $(basename "$wf"), Linux jobs in $image ($arch)$([[ $os == Darwin ]] && echo ", macOS jobs on this Mac")"
  exec act workflow_dispatch "${args[@]}" ${jobs[@]+"${jobs[@]}"} ${pass[@]+"${pass[@]}"}
}

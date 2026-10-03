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
#     -v, --verbose    act's own output as it comes (by default: a line as each job starts and
#                      ends, a failed step's last lines, and why a job did not run)
#     --event FILE     run with this event (a push's payload), whose inputs carry the tier
#     --remote         HEAD, pushed, on the project's bana split repository (docs/SPLIT.md)
#   Linux jobs (<prefix>-linux, ubuntu-*) run in containers from bana.conf's act.image; on a
#   Mac, macOS jobs (<prefix>-macos) run on the Mac itself, with its CoreAudio and USB devices,
#   and jobs that need systemd (<prefix>-systemd) run next, in the Mac's Linux machine (vm).
#   One act runs at a time on this machine: bana ci refuses while another one runs.
#   A run keeps act's output in ~/.bana/<prefix>/ci/last.log, and what ran in last.env, for
#   bana fix (bana.conf: ci.log = no runs act as before, which keeps its colours in containers).
#   With bana-manager here, it ends with the CI report's table (all of it in last.report.md).
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
# act's own settings (act.*). A fix round runs Claude's snapshot, so they come from the
# failing commit's bana.conf, which the daemon saves and names in BANA_ROUND_CONF: the
# snapshot cannot loosen how act isolates its own jobs.
act_conf() { # KEY [DEFAULT]
  [[ -n ${round_conf:-} ]] || { conf "$@"; return; }
  # shellcheck disable=SC2034 # conf_lookup reads it
  local conf_file=$round_conf v
  if v=$(conf_lookup "$1"); then printf '%s\n' "$v"; else printf '%s\n' "${2:-}"; fi
}
act_conf_keys() { # PREFIX.
  [[ -n ${round_conf:-} ]] || { conf_keys "$1"; return; }
  # shellcheck disable=SC2034 # conf_keys reads it
  local conf_file=$round_conf
  conf_keys "$1"
}

# Where a job runs, by the first of its runs-on labels that has a place (act's rule): bana.conf's
# act.platform.<label> (lowercase) is linux (a container of act.image), mac (this Mac, in act's
# host mode; elsewhere the job is not run), machine (the Mac's Linux machine, where systemd runs,
# in act's host mode; elsewhere not run), skip [reason] (not run), or an image of its own.
# LABEL<TAB>VALUE lines: the built-in ones, then bana.conf's, which win (but for self-hosted,
# which would take every self-hosted job, and a label with '=', which act cannot map).
act_platform_table() {
  local l v e
  {
    printf '%s\t%s\n' "$prefix-linux" linux ubuntu-latest linux ubuntu-24.04 linux ubuntu-22.04 linux \
      ubuntu-20.04 catthehacker/ubuntu:act-20.04 ubuntu-18.04 "skip no act image for 18.04" \
      "$prefix-macos" mac macos-latest mac "$prefix-systemd" machine
    for l in $(act_conf_keys act.platform.); do printf '%s\t%s\n' "$l" "$(act_conf "act.platform.$l")"; done
  } | awk -F'\t' '{ l = tolower($1) } l == "self-hosted" || index(l, "=") { next } !(l in v) { o[++n] = l } { v[l] = $2 }
    END { for (i = 1; i <= n; i++) printf "%s\t%s\n", o[i], v[o[i]] }' |
    while IFS=$'\t' read -r l v; do
      # BANA_ACT_PLATFORM_<LABEL> overrides a built-in one too.
      e=$(env_name "act.platform.$l")
      [[ ! $e =~ ^[A-Za-z0-9_]+$ || -z ${!e+x} ]] || v=${!e}
      printf '%s\t%s\n' "$l" "$v"
    done
}

# The -P options for act_platform_table. A skipped label gets an empty image, so that act
# tries the job's next label, and its own defaults (node:16 for ubuntu-20.04) take nothing.
act_platforms() { # IMAGE
  local l v
  for l in $(act_conf_keys act.platform.); do
    case $(lower <<<"$l") in
    self-hosted) warn "act.platform.$l: left out: act would run every self-hosted job there" ;;
    *=*) warn "act.platform.$l: left out: act cannot map a label with '='" ;;
    esac
  done
  while IFS=$'\t' read -r l v; do
    case $v in
    linux) v=$1 ;;
    mac) if [[ $os == Darwin ]]; then v=-self-hosted; else v=; fi ;;
    # A second act runs these jobs, in the Linux machine (act_machine): this one skips them.
    machine | skip | "skip "*) v= ;;
    esac
    printf '%s\n' -P "$l=$v"
  done < <(act_platform_table)
}

# The second act's -P options, in the Linux machine: its jobs, and the Linux jobs they need
# (plan and the like), in act's host mode there; the rest are not run there.
act_platforms_machine() {
  local l v
  while IFS=$'\t' read -r l v; do
    case $v in machine | linux) v=-self-hosted ;; *) v= ;; esac
    printf '%s\n' -P "$l=$v"
  done < <(act_platform_table)
}

# The labels whose jobs run in the Linux machine, lowercase and space-separated, when they can:
# on a Mac with OrbStack or Lima. Elsewhere none, and act skips those jobs ("not run here").
act_machine_labels() {
  [[ $os == Darwin ]] || return 0
  command -v orb >/dev/null || command -v limactl >/dev/null || return 0
  act_platform_table | awk -F'\t' '$2 == "machine" { printf "%s ", $1 }'
}
act_started() { LC_ALL=C ps -o lstart= -p "$1" 2>/dev/null | awk '{ $1 = $1; print }'; }

# One act at a time on this machine, for bana ci and the daemon's builds: act names its
# containers without a run id (a second run removes the first one's), and its artifact
# server's port is fixed. The lock is ~/.bana/act.lock, a directory whose owner file has
# three lines: a pid, when that pid started, and a label. bana ci holds it until act ends,
# then frees it; where bana ci execs act, its pid becomes act's, so the lock holds while act
# runs. Once the pid is gone, or is another process (another start time), the lock is stale
# and the next taker removes it.
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
  # Until act ends, or starts through exec; if it cannot start, its pid is gone and the lock
  # stale anyway.
  trap 'rm -rf "$base_home/act.lock"' EXIT
  printf '%s\n' "$$" "$(act_started $$)" "$1" >"$lock/owner.$$"
  mv "$lock/owner.$$" "$lock/owner"
}

act_main() {
  local tier='' x64='' list='' dry='' event='' secrets='' remote='' locked jobs=() pass=() args=() extra=() verbose=''
  local wf root here tiers image arch net i o dc all=() mlabels='' act_fetched=''
  while (($#)); do
    case $1 in
    -j | --job) jobs=(-j "${2:?-j JOB}"); shift ;;
    --x64) x64=1 ;;
    --list | -l) list=1 ;;
    -n | --dry-run) dry=1 ;;
    -v | --verbose) verbose=1 ;;
    --event) event=${2:?--event FILE}; shift ;;
    --remote) remote=1 ;;
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
  round_conf=${BANA_ROUND_CONF:-}
  unset BANA_PROJECT_ROOT BANA_ACT_LOCKED BANA_ROUND_CONF
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
  # bana split: the daemon's build on the public repository's GitHub Actions (BANA_SPLIT_*), or
  # --remote's, by hand. act is not run here; act's lines come from there.
  if [[ -n ${BANA_SPLIT_REPO:-} || -n $remote ]]; then
    # shellcheck source=/dev/null # lib/split.sh, checked on its own (it sources this file)
    source "$bana_root/lib/split.sh"
    if [[ -n $remote ]]; then
      # shellcheck source=/dev/null # lib/daemon.sh, checked on its own
      source "$bana_root/lib/daemon.sh"
      split_ci_remote "$root" "$tier" "${jobs[1]:-}"
      exit
    fi
    for ((i = 0; i < ${#pass[@]}; i++)); do [[ ${pass[i]} != --artifact-server-path ]] || o=${pass[i + 1]:-}; done
    split_remote "$tier" "${jobs[1]:-}" "${o:-artifacts}"
    exit
  fi
  if [[ $locked != 1 ]]; then
    act_lock "bana ci${tier:+ $tier} ($prefix)"
    # Every run by hand is act's run 1, and download-artifact hands a run every artifact of
    # its run id: the last run's go, so a run gets only its own. (A dry run uploads nothing.)
    [[ -n $dry ]] || rm -rf "$base_home/act/artifacts"
  fi
  act_docker

  image=$(act_conf act.image catthehacker/ubuntu:act-24.04)
  args=(-C "$root" -W "$wf" --artifact-server-path "$base_home/act/artifacts")
  # Linux jobs in act.image, macOS jobs on this Mac (act's host mode; elsewhere act skips
  # them), and the rest as act.platform.* says.
  while IFS= read -r l; do args+=("$l"); done < <(act_platforms "$image")
  # vars.* as the daemon has them (its own --var-file comes later, and wins).
  [[ ! -f $home/vars ]] || args+=(--var-file "$home/vars")
  if [[ -n $x64 ]]; then arch=linux/amd64
  elif [[ $(cpu) == arm64 ]]; then arch=linux/arm64
  else arch=linux/amd64; fi
  args+=(--container-architecture "$arch")
  # Each job gets a localhost of its own, as on GitHub, where each job has its own machine:
  # act's default (host) gives all of a run's Linux jobs the Docker host's, so the servers
  # of jobs running side by side answer each other (act.network = host for that).
  net=$(act_conf act.network bridge)
  args+=(--network "$net")

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
  dc=$(act_conf act.docker_config)
  case $dc in
  '') dc=$base_home/docker; mkdir -p "$dc"; [[ -s $dc/config.json ]] || echo '{}' >"$dc/config.json" ;;
  "~"/*) dc=$HOME/${dc#\~/} ;;
  esac
  export DOCKER_CONFIG=$dc
  # The commit's own act options; those after -- come later, so they win.
  read -r -a extra <<<"$(act_conf act.args)" || true
  # They may name another network: act takes the last.
  all=(${extra[@]+"${extra[@]}"} ${pass[@]+"${pass[@]}"})
  for ((i = 0; i < ${#all[@]}; i++)); do
    case ${all[i]} in --network) net=${all[i + 1]:-$net} ;; --network=*) net=${all[i]#*=} ;; esac
  done
  say "act: ${tier:-the workflow} from $(basename "$wf"), Linux jobs in $image ($arch, network $net)$([[ $os == Darwin ]] && echo ", macOS jobs on this Mac")"
  args=(workflow_dispatch "${args[@]}" ${extra[@]+"${extra[@]}"} ${jobs[@]+"${jobs[@]}"} ${pass[@]+"${pass[@]}"})
  [[ -n $dry ]] || mlabels=$(act_machine_labels)
  [[ -n $dry ]] || act_actions
  # The daemon reads act's output itself, through relays that outlive it (act_relay): the
  # machine's act and bash's own lines (act_both) as well.
  if [[ $locked == 1 ]]; then
    [[ -n $mlabels ]] || exec act "${args[@]}" > >(act_relay) 2> >(act_relay >&2)
    act_both "${args[@]}" > >(act_relay) 2> >(act_relay >&2)
    exit
  fi
  # A dry run leaves the last run's log.
  if [[ -n $dry || $(conf ci.log yes) == no ]]; then
    [[ -n $mlabels ]] || exec act "${args[@]}"
    act_both "${args[@]}"
    exit
  fi
  act_logged "${args[@]}"
}

# The workflow's actions, fetched before it runs, one job at a time (a dry run), and then
# used as they are (--action-offline-mode). Jobs that run together and use one action (a
# matrix) would each fetch it into act's one cache: one job's fetch rewrote the files another
# was copying into its container, and the job failed. A fix round (offline already) fetches
# nothing; what this cannot fetch, the run fetches as before.
# act's dry run still runs a host job's steps (-self-hosted): for it, every label goes to
# the image, where a dry run runs nothing (the last -P wins). The Linux machine's act (act_machine)
# fetches its own the same way, into its own cache.
act_actions() {
  local a i dry=()
  for a in "${args[@]}"; do [[ $a != --action-offline-mode ]] || return 0; done
  for ((i = 0; i < ${#args[@]}; i++)); do
    case ${args[i]} in
    -P | --platform) a=${args[i + 1]:-} ;;
    -P=* | --platform=*) a=${args[i]#*=} ;;
    -P?*) a=${args[i]#-P} ;;
    *) continue ;;
    esac
    [[ $a != *=* ]] || dry+=(-P "${a%%=*}=$image")
  done
  act "${args[@]}" ${dry[@]+"${dry[@]}"} -n --concurrent-jobs 1 >/dev/null 2>&1 || true
  args+=(--action-offline-mode)
  act_fetched=1
}

# act's output on its way to the daemon. If the daemon dies, its pipes close: act, a Go
# program, would die of SIGPIPE at its next line, and must run on (the daemon's next start
# stops it). This passes the lines on while it can and drops them after; SIGPIPE is ignored
# here only, so act and its steps keep the default, as on GitHub. A cancel's SIGINT (to the
# process group) leaves it running, for act's last lines.
act_relay() {
  trap '' PIPE INT
  local l
  while IFS= read -r l || [[ -n $l ]]; do printf '%s\n' "$l" 2>/dev/null || :; done
}

# act, then a second act in the Linux machine for the jobs the first skipped for a machine
# label (act_machine_labels), and act's exit status: the first failure's. Without such labels,
# act itself (exec: act_both then runs as a pipeline's command, or as bana ci's last). The
# daemon (BANA_ACT_LOCKED) signals bash only: an INT or TERM goes on to the act running then,
# and a stopped run starts no second act. By hand, Ctrl-C reaches act itself.
# Uses act_main's locals.
act_both() { # ACT-ARGUMENT...
  local out st=0 ids m
  [[ -n $mlabels ]] || exec act "$@"
  out=$(mktemp "${TMPDIR:-/tmp}/bana-act.XXXXXX")
  act_stop='' act_child='' act_vm_pid=''
  if [[ $locked == 1 ]]; then trap act_forward INT TERM; else trap 'act_stop=1' INT; fi
  # What act says also goes to OUT, where its skipped jobs are found: .done marks each copy's end.
  # bash starts a background command with SIGINT ignored: trap - gives act its SIGINT back.
  (trap - INT; exec act "$@") > >(tee "$out.1"; : >"$out.1.done") 2> >(tee "$out.2" >&2; : >"$out.2.done") &
  act_child=$!
  act_wait "$act_child" || st=$?
  act_child=''
  act_copied "$out.1" "$out.2"
  if [[ -z $act_stop ]]; then
    ids=$(act_machine_jobs "$out.1" "$out.2")
    [[ -z $ids ]] || act_machine "$ids" "$@" || { m=$?; ((st)) || st=$m; }
  fi
  trap - INT TERM
  rm -f "$out" "$out".*
  return "$st"
}

# The daemon's INT or TERM, on to the act running now: here, or in the Linux machine (whose
# pid is in a file there, as orb or limactl may not pass signals on).
act_forward() {
  act_stop=1
  [[ -z $act_child ]] || kill -INT "$act_child" 2>/dev/null || true
  # shellcheck disable=SC2016 # expanded in the machine
  [[ -z $act_vm_pid ]] || vm_run "$vm" sh -c 'kill -INT "$(cat "$1")"' _ "$act_vm_pid" 2>/dev/null || true
}

# Waits for PID, also through the signals bash traps meanwhile; its exit status.
act_wait() { # PID
  local st=0
  wait "$1" || st=$?
  while kill -0 "$1" 2>/dev/null; do st=0; wait "$1" || st=$?; done
  return "$st"
}

# Until each FILE's copy has ended (FILE.done), for a few seconds at most.
act_copied() { # FILE...
  local f n
  for f in "$@"; do
    for ((n = 0; n < 100; n++)); do [[ ! -e $f.done ]] || break; sleep 0.05; done
  done
}

# The jobs act skipped for a machine label, by id, one a line. act's JSON lines carry the id; its
# text names the job ([workflow/name]), whose id act -l has.
act_machine_jobs() { # FILE...
  local list
  list=$(act -l -C "$root" -W "$wf" 2>/dev/null) || true
  # shellcheck disable=SC2016 # act's backquotes
  {
    printf '%s\n' "$list" | sed 's/^/L	/'
    sed -n 's/.*"jobID":"\([^"]*\)".*Skipping unsupported platform -- Try running with `-P \([^`]*\)=\.\.\.`.*/I	\1	\2/p' "$@"
    sed -n 's/^[^[{]*\[\(.*\)\].*Skipping unsupported platform -- Try running with `-P \([^`]*\)=\.\.\.`.*/N	\1	\2/p' "$@"
  } | awk -F'\t' -v labels=" $mlabels " '
    $1 == "L" { if (!a) { a = index($2, "Job ID"); b = index($2, "Job name"); c = index($2, "Workflow name"); next }
      id = substr($2, a, b - a); nm = substr($2, b, c - b); gsub(/^ +| +$/, "", id); gsub(/^ +| +$/, "", nm)
      if (nm != "") byname[nm] = id; next }
    { if (!index(labels, " " tolower($3) " ")) next
      if ($1 == "I") id = $2
      else { j = $2; sub(/.*\//, "", j); sub(/ +$/, "", j); id = (j in byname) ? byname[j] : j }
      if (!(id in seen)) { seen[id] = 1; print id } }'
}

# The second act: IDS (one a line) in the Linux machine, in act's host mode, where systemd runs.
# The machine is made and made ready first (bana linux-prepare: packages.linux, hook.linux,
# and act, at this act's version), then act runs there on the same checkout, event and
# options, with the jobs' platforms (act_platforms_machine) and a cache on the machine's own
# disk. When the first act's actions were fetched first (act_actions), the machine's are too,
# into that cache, with every label to the image (a dry run of a host job runs its steps).
# Under --json only the jobs' own lines (and errors) come out: the jobs they need ran in the
# first act already. Uses act_main's locals.
act_machine() { # IDS ACT-ARGUMENT...
  local ids=$1 a=() fetch=() vc=() sc j l json='' v pf tf st=0 i names
  shift
  for ((i = 1; i <= $#; i++)); do
    case ${!i} in
    -P | -j | --job) i=$((i + 1)) ;;
    *) [[ ${!i} != --json ]] || json=1; a+=("${!i}") ;;
    esac
  done
  while IFS= read -r j; do a+=("$j"); done < <(act_platforms_machine)
  for j in $ids; do a+=(-j "$j"); done
  if [[ -n $act_fetched ]]; then
    for j in "${a[@]}"; do [[ $j == --action-offline-mode ]] || fetch+=("$j"); done
    while IFS=$'\t' read -r l _; do fetch+=(-P "$l=$image"); done < <(act_platform_table)
    fetch+=(-n --concurrent-jobs 1)
  fi
  v=$(act --version 2>/dev/null | awk 'NR == 1 { print $NF }')
  [[ -n $v ]] || { warn "act has no version to install in the Linux machine"; return 1; }
  # The jobs' names, as act's text shows them.
  names=$(act -l -C "$root" -W "$wf" 2>/dev/null | awk -v ids=" $(printf '%s' "$ids" | tr '\n' ' ') " '
    NR == 1 { b = index($0, "Job name"); c = index($0, "Workflow name"); a = index($0, "Job ID"); next }
    { id = substr($0, a, b - a); nm = substr($0, b, c - b); gsub(/^ +| +$/, "", id); gsub(/^ +| +$/, "", nm)
      if (index(ids, " " id " ")) print nm }') || true
  say "act: $(printf '%s' "$ids" | tr '\n' ' ')in the Linux machine $vm (host mode, with systemd)"
  { vm_up "$vm" native && vm_bana "$vm" linux-prepare "$v"; } >&2 ||
    { warn "The Linux machine $vm is not ready: its jobs did not run"; return 1; }
  pf=/tmp/bana-act-$prefix.pid
  # The token reaches the machine in a file, not on a command line.
  tf=$(mktemp "${TMPDIR:-/tmp}/bana-token.XXXXXX")
  chmod 600 "$tf"
  printf '%s' "${GITHUB_TOKEN:-}" >"$tf"
  # vm_run's command itself, so that act_child is orb's (or limactl's) pid, not a subshell's.
  case $(vm_kind) in orbstack) vc=(orb -m "$vm") ;; *) vc=(limactl shell "$vm") ;; esac
  # act in the machine, in the checkout; its pid in PF (- for none).
  # shellcheck disable=SC2016 # expanded in the machine
  sc='cd "$1" || exit 1
    pf=$2 v=$3 tf=$4 c=$5; shift 5
    GITHUB_TOKEN=$(cat "$tf"); export GITHUB_TOKEN
    [ "$pf" = - ] || echo $$ >"$pf"
    exec "$HOME/.local/bin/act-$v" "$@" --action-cache-path "$HOME/.cache/bana/$c"'
  # The fetch: a cancel meanwhile (act_forward) stops the run before it starts.
  ((${#fetch[@]} == 0)) || "${vc[@]}" bash -c "$sc" bana-act "$here" - "$v" "$tf" "act-$prefix" "${fetch[@]}" >/dev/null 2>&1 || true
  if [[ -n $act_stop ]]; then rm -f "$tf"; return 130; fi
  act_vm_pid=$pf
  (trap - INT; exec "${vc[@]}" bash -c "$sc" bana-act "$here" "$pf" "$v" "$tf" "act-$prefix" "${a[@]}") > >(act_only "$json" "$ids" "$names") &
  act_child=$!
  act_wait "$act_child" || st=$?
  act_child='' act_vm_pid=''
  rm -f "$tf"
  return "$st"
}

# The second act's output, of only its own jobs, IDS (by NAMES in act's text): the jobs they need
# ran in the first act. Lines of no job (act's own, and errors) stay.
act_only() { # JSON(1|'') IDS NAMES
  if [[ -z $1 ]]; then
    awk -v names="$3" 'BEGIN { n = split(names, a, "\n"); for (i = 1; i <= n; i++) keep[a[i]] = 1 }
      !/^\[[^]]*\]/ { print; fflush(); next }
      { j = substr($0, 2, index($0, "]") - 2); sub(/^[^\/]*\//, "", j); sub(/ +$/, "", j); if (j in keep) { print; fflush() } }'
    return
  fi
  awk -v ids=" $(printf '%s' "$2" | tr '\n' ' ') " '
    !/^\{/ { print; fflush(); next }
    match($0, /"jobID":"[^"]*"/) { id = substr($0, RSTART + 9, RLENGTH - 10); if (index(ids, " " id " ")) { print; fflush() } next }
    /"level":"(error|fatal|panic)"/ { print; fflush() }'
}

# What bana ci shows by hand, from act's text (all of which goes to last.log): a line as each
# job starts and ends, a failed step's last 20 lines, the plan's choice, why a job did not run
# (its labels' place here, or its if:), bana's own lines and act's errors; then a count.
# -v shows act's output itself. It ignores Ctrl-C, so tee and act still end the log.
# Uses act_main's locals.
act_view() {
  local lf tf
  [[ -z $verbose ]] || { cat; return; }
  lf=$(mktemp "${TMPDIR:-/tmp}/bana-view.XXXXXX")
  tf=$lf.table
  act -l -C "$root" -W "$wf" >"$lf" 2>/dev/null || true
  act_platform_table >"$tf"
  (
    trap '' INT
    exec awk -v list="$lf" -v table="$tf" -v machine="$mlabels" -v vm="$vm" -v os="$os" -v only="${jobs[1]:-}" \
      -v logfile="$home/ci/last.log" -v tty="$([[ -t 1 ]] && echo 1)" -f "$bana_root/lib/view.awk"
  )
  rm -f "$lf" "$tf"
}

# A run by hand keeps act's output, and what ran, for bana fix: in ~/.bana/<prefix>/ci,
# last.log and last.env. last.env's lines are KEY=VALUE (the rest of the line): sha, ref,
# dirty (the changed files, as git status names them: space-separated, C-quoted when a name
# has a space), tier, job, event (its file), network, act and bana (their versions), started
# and ended (Unix seconds), exit (act's), and stopped (1: Ctrl-C stopped it, so it did not
# fail). Both take their names when act ends (.part until then). act's output goes through
# tee, so bash stays, holding the lock, until act ends; its EXIT trap then frees the lock.
# Uses act_main's locals.
act_logged() { # ACT-ARGUMENT...
  local dir=$home/ci sha ref dirty v b='' started status stopped=''
  mkdir -p "$dir"
  rm -f "$dir/last.report.md"
  sha=$(git -C "$root" rev-parse -q --verify HEAD 2>/dev/null) || true
  ref=$(git -C "$root" symbolic-ref -q HEAD 2>/dev/null) || true
  dirty=$(git -C "$root" -c core.quotePath=false status --porcelain 2>/dev/null |
    awk '{ p = substr($0, 4); i = index(p, " -> "); if (i) p = substr(p, i + 4); printf "%s%s", s, p; s = " " }') || true
  v=$(act --version 2>/dev/null | awk 'NR == 1 { print $NF }') || true
  # bana's own commit (a checkout's or a release's; none for a copy).
  b=$(bana_commit)
  started=$(date +%s)
  # Ctrl-C reaches act, which stops its jobs and ends; tee -i and bash (trapping it) wait
  # for that, so the log ends as act's output does.
  trap 'stopped=1' INT
  if act_both "$@" 2>&1 | tee -i "$dir/last.log.part" | act_view; then status=0; else status=${PIPESTATUS[0]}; fi
  trap - INT
  printf '%s\n' "sha=$sha" "ref=$ref" "dirty=$dirty" "tier=$tier" "job=${jobs[1]:-}" "event=$event" \
    "network=$net" "act=$v" "bana=$b" "started=$started" "ended=$(date +%s)" "exit=$status" \
    "stopped=${stopped:-0}" >"$dir/last.env.part"
  mv -f "$dir/last.log.part" "$dir/last.log"
  mv -f "$dir/last.env.part" "$dir/last.env"
  act_unmapped "$dir/last.log"
  act_report "$dir"
  if ((status)) && [[ -z $stopped ]]; then
    echo "act's output: $dir/last.log"
    say "bana fix: hand this failure to Claude Code on a fix branch"
  fi
  exit "$status"
}

# The CI report of the run (bana report last), when a bana-manager that makes one is here
# (none is built for it): last.report.md, and its table on the terminal.
act_report() { # DIR
  local b m='' opts=()
  for b in "$bana_root/bin/bana-manager" "$machine_dir/bana-manager" "${CARGO_TARGET_DIR:-$bana_root/manager/target}/release/bana-manager"; do
    if bm_has "$b" report; then m=$b; break; fi
  done
  [[ -n $m ]] || return 0
  [[ -z $conf_file ]] || opts+=(--conf "$conf_file")
  [[ -z $repo ]] || opts+=(--repo "$repo")
  if "$m" report --text "$1/last.log" --env "$1/last.env" ${opts[@]+"${opts[@]}"} --machine "$host" \
    >"$1/last.report.md.part" 2>/dev/null; then
    mv -f "$1/last.report.md.part" "$1/last.report.md"
    echo
    sed -n '/^| Standard |/,/^$/p' "$1/last.report.md"
    echo "The CI report: $1/last.report.md (bana report)"
  else
    rm -f "$1/last.report.md.part"
  fi
}

# The jobs act skipped for want of a place, when no act.platform key says so (a label bana
# does not know): a warning each, since the run still passes without them.
act_unmapped() { # LOG
  local keys job labels l known
  keys=" $(act_platform_table | cut -f1 | tr '\n' ' ') "
  # [workflow/job] ... Skipping unsupported platform -- Try running with `-P LABEL=...`
  # shellcheck disable=SC2016 # act's backquotes
  sed -n 's/^[^[]*\[\(.*\)\].*Skipping unsupported platform -- Try running with `-P \(.*\)=\.\.\.`.*/\1	\2/p' "$1" |
    awk -F'\t' '{ j = $1; sub(/.*\//, "", j); sub(/ +$/, "", j) } !(j in l) { o[++n] = j }
      !((j, $2) in s) { s[j, $2]; l[j] = l[j] (l[j] == "" ? "" : " ") $2 }
      END { for (i = 1; i <= n; i++) printf "%s\t%s\n", o[i], l[o[i]] }' |
    while IFS=$'\t' read -r job labels; do
      known=
      for l in $labels; do
        case $keys in *" $(lower <<<"$l") "*) known=1 ;; esac
      done
      [[ -n $known ]] || warn "not run here: $job (runs-on: $labels): see bana add"
    done
}

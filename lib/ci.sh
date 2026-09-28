# shellcheck shell=bash
# bana's workflow helpers: changed, plan, keep-builds. Sourced by bin/bana, which
# gives them conf, conf_keys and die; they work on the repository at the current
# directory (a job's checkout), not on bana's own.

ci_root=${BANA_PROJECT_ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}

# The files changed since BEFORE (a push's previous head), else since where HEAD
# left BRANCH; '*' when that cannot be told.
ci_changed() { # BEFORE [BRANCH]
  local before=${1:-} branch=${2:-main} base=
  if [[ -n $before && ! $before =~ ^0+$ ]] && git -C "$ci_root" cat-file -e "$before^{commit}" 2>/dev/null; then
    base=$before
  else
    # A new branch, or history rewritten: compare with where it left BRANCH.
    git -C "$ci_root" fetch -q --depth=200 origin "$branch" 2>/dev/null || true
    base=$(git -C "$ci_root" merge-base HEAD FETCH_HEAD 2>/dev/null || true)
  fi
  if [[ -n $base ]]; then
    git -C "$ci_root" diff --name-only "$base" HEAD
  else
    echo '*'
  fi
}

in_words() { # WORD LIST: whether WORD is one of LIST's words (spaces or commas)
  local x
  for x in $(words "$2"); do [[ $x == "$1" ]] && return 0; done
  return 1
}

# What a run covers, from the tier and the changed files on stdin, as key=true|false
# lines (for $GITHUB_OUTPUT), or one JSON object with --json. bana.conf says:
#   tiers = quick nightly release      the tiers; runs choose one
#   plan.full_tiers = nightly release  tiers that run every path job (default: all but the first)
#   plan.everything = REGEX            a changed file matching it runs every path job
#   plan.path.JOB = REGEX              JOB runs when a changed file matches
#   plan.tier.KEY = release            KEY is true on these tiers only
ci_plan() { # TIER [--json]
  local tier=${1:-} json=${2:-} tiers full files all=0 k re v out=()
  tiers=$(words "$(conf tiers "quick nightly release")" | tr -s ' ' | sed 's/^ //; s/ $//')
  in_words "$tier" "$tiers" || die "tier: one of $tiers, not '$tier'"
  full=
  [[ $tiers != *' '* ]] || full=${tiers#* }
  full=$(conf plan.full_tiers "$full")
  files=$(cat)
  in_words "$tier" "$full" && all=1
  grep -qx '\*' <<<"$files" && all=1
  re=$(conf plan.everything)
  [[ -n $re ]] && grep -Eq -- "$re" <<<"$files" && all=1
  out+=("tier=$tier")
  for k in $(conf_keys plan.path.); do
    re=$(conf "plan.path.$k")
    if ((all)) || grep -Eq -- "$re" <<<"$files"; then v=true; else v=false; fi
    out+=("$k=$v")
  done
  for k in $(conf_keys plan.tier.); do
    if in_words "$tier" "$(conf "plan.tier.$k")"; then v=true; else v=false; fi
    out+=("$k=$v")
  done
  if [[ $json == --json ]]; then
    local sep='' line
    printf '{'
    for line in "${out[@]}"; do
      k=${line%%=*} v=${line#*=}
      [[ $k == tier ]] && v="\"$v\""
      printf '%s"%s":%s' "$sep" "$k" "$v"
      sep=,
    done
    printf '}\n'
  else
    printf '%s\n' "${out[@]}"
  fi
}

# On a self-hosted runner: everything untracked goes, as with a fresh checkout, except
# the build caches bana.conf's keep names (git clean -e patterns: /target/, node_modules/).
# A kept top-level directory past keep_max_gb starts over.
ci_keep_builds() {
  if [[ ${RUNNER_ENVIRONMENT:-} != self-hosted ]]; then
    echo "Not a self-hosted runner: nothing to keep."
    return 0
  fi
  local keep=() args=() k max kb d
  read -r -a keep <<<"$(words "$(conf keep)")" || true
  max=$(conf keep_max_gb 0)
  [[ $max =~ ^[0-9]+$ ]] || die "keep_max_gb: a number of GiB, not '$max'"
  for k in ${keep[@]+"${keep[@]}"}; do args+=(-e "$k"); done
  git -C "$ci_root" clean -ffdxq ${args[@]+"${args[@]}"}
  ((max > 0)) || return 0
  for k in ${keep[@]+"${keep[@]}"}; do
    [[ $k == /* ]] || continue
    d=$ci_root/${k#/}
    d=${d%/}
    [[ -d $d ]] || continue
    kb=$(du -sk "$d" | cut -f1)
    if ((kb > max * 1024 * 1024)); then
      echo "${k} is over $max GiB: starting it over"
      rm -rf "$d"
    fi
  done
}

ci_main() {
  local cmd=$1
  shift
  case $cmd in
  changed) ci_changed "$@" ;;
  plan) ci_plan "$@" ;;
  keep-builds) ci_keep_builds ;;
  esac
}

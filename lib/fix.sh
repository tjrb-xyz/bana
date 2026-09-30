# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets os, repo, home, self, bana_root
# bana fix: a failed CI run handed to Claude Code, on a branch of its own. bana-manager does
# the work (the worktree, the brief, the prompt); docs/FIX.md has the rest. Sourced by bin/bana.
#
#   bana fix [BUILD | last | --log FILE|-] [--open | --headless]
#     makes bana/fix-<sha7> at the failing commit, a worktree of this checkout in
#     ~/.bana/<prefix>/fix/<sha7>, with the brief (what failed, and how it ran) and the
#     prompt, and starts Claude Code there
#     BUILD            a daemon build, by its number
#     last             the last bana ci here
#     --log FILE       act's output from elsewhere (-: pasted on stdin); the fix starts at HEAD
#     (none)           the newer of the last bana ci here, if it failed, and the newest
#                      failed daemon build of this branch
#     --open           Claude Code in a new terminal instead, through its claude-cli:// link
#     --headless       Claude Code unattended, here: it may read, edit the worktree, and use
#                      bana's tools (fix.allow adds rules), within fix.turns and
#                      fix.budget_usd; it needs the daemon, whose rounds test its changes
#   bana fix brief [FIX]   what failed, where, and how it ran
#   bana fix list          the fixes: their branches, commits and worktrees
#   bana fix push [FIX] [--pr]   push the branch to origin (the daemon then builds it); --pr
#                          also opens a pull request against the branch that failed
#   bana fix drop [FIX] [--force] [--delete-branch]   remove the worktree (--force: with its
#                          changes); the branch stays while it has commits
#   FIX: the commit's first digits (4 or more); by default the fix you are in, else the newest.
#   Claude Code works in the worktree with your own settings, plus rules against git push:
#   a guard for Claude, not a lock (docs/FIX.md), and bana's tools (bana mcp): run_jobs runs
#   the failed jobs through the daemon, commit_fix commits a green round. bana pushes
#   nothing: bana fix push is yours.

fix_usage() { awk '/^#   bana fix \[/, /^#   nothing: bana fix push/ { sub(/^# ?/, ""); print }' "$bana_root/lib/fix.sh" >&2; exit 2; }

# A value in bana-manager's JSON: the first "KEY": string or number (its files put their
# top-level keys first), a string's escapes undone; null is empty.
fix_json() { # KEY < JSON
  awk -v k="$1" '
    BEGIN { re = "\"" k "\": *(\"([^\"\\\\]|\\\\.)*\"|[-+.0-9A-Za-z]+)" }
    match($0, re) {
      v = substr($0, RSTART, RLENGTH); sub(/^"[^"]*": */, "", v)
      if (v == "null") v = ""
      else if (v ~ /^"/) {
        v = substr(v, 2, length(v) - 2); o = ""
        while ((i = index(v, "\\")) > 0) { o = o substr(v, 1, i - 1) substr(v, i + 1, 1); v = substr(v, i + 2) }
        v = o v
      }
      print v; exit
    }'
}
fx() { fix_json "$2" <"$home/fix/$1.d/fix.json"; } # FIX KEY: from its fix.json

# A key of the last bana ci's last.env.
fix_env() { # KEY
  awk -v k="$1" 'index($0, k "=") == 1 { print substr($0, length(k) + 2); exit }' "$home/ci/last.env" 2>/dev/null || true
}

# The checkout fixes are made in: the main worktree of the one here (not a fix's own).
fix_checkout() {
  local top main
  top=$(git -C "${BANA_PROJECT_ROOT:-.}" rev-parse --show-toplevel 2>/dev/null) || die "Run bana fix in the project's checkout"
  # git lists the main worktree first; a bare repository has none.
  main=$(git -C "$top" worktree list --porcelain 2>/dev/null |
    awk 'NR == 1 { w = substr($0, 10) } NR == 2 && $0 == "bare" { w = "" } END { print w }') || true
  if [[ -n $main && -d $main ]]; then echo "$main"; else echo "$top"; fi
}

# The newest failed daemon build of REF, and when it ended: ID ENDED. From its build.json
# (the daemon need not run); a fix's own round builds do not count.
fix_failed_build() { # REF
  local f
  for f in "$home"/builds/*/build.json; do
    [[ -f $f ]] || return 0
    break
  done
  awk -v want="$1" '
    function value(k,   v) {
      if (!match($0, "\"" k "\": *(\"[^\"]*\"|[0-9]+)")) return ""
      v = substr($0, RSTART, RLENGTH); sub(/^"[^"]*": *"?/, "", v); sub(/"$/, "", v); return v
    }
    function done() {
      if (id ~ /^[0-9]+$/ && st == "failure" && rf == want && tr != "fix" && (best == "" || id + 0 > best + 0)) { best = id; at = en }
    }
    FNR == 1 { done(); id = FILENAME; sub(/\/build\.json$/, "", id); sub(/.*\//, "", id); st = rf = en = tr = "" }
    st == "" { st = value("state") }
    rf == "" { rf = value("ref") }
    en == "" { en = value("ended_at") }
    tr == "" { tr = value("trigger") }
    END { done(); if (best != "") print best, at + 0 }' "$home"/builds/*/build.json
}

# With no source named: the newer of the last bana ci here, if it failed, and the newest
# failed daemon build of this branch. Sets fix_start's src.
fix_default() { # BRANCH (refs/heads/…, or empty)
  local e ended stopped b='' at=0 why='no failed bana ci'
  e=$(fix_env exit)
  ended=$(fix_env ended)
  stopped=$(fix_env stopped)
  [[ $ended =~ ^[0-9]+$ ]] || ended=0
  # A run Ctrl-C stopped did not fail (as a cancelled daemon build).
  [[ $stopped != 1 ]] || { e=0 why='the last bana ci was stopped (Ctrl-C)'; }
  [[ -z $1 ]] || read -r b at < <(fix_failed_build "$1") || true
  if [[ -n $e && $e != 0 ]] && { [[ -z $b ]] || ((ended >= at)); }; then
    say "The newest failure here is the last bana ci (bana fix last)"
    src=(--run)
  elif [[ -n $b ]]; then
    say "The newest failure here is daemon build $b of ${1#refs/heads/} (bana fix $b)"
    src=(--build "$b")
  else
    die "Nothing here failed: $why${1:+, and no failed daemon build of ${1#refs/heads/}}. bana fix --log FILE takes act's output from elsewhere"
  fi
}

# bana's tools for Claude Code in worktree WT: none when Claude Code has them there already
# (bana daemon install registers them in your checkout, and its worktrees see them), else
# --mcp-config with bana's MCP server. `claude mcp get` is a config command: no prompt runs.
fix_mcp() { # BANA-MANAGER WT
  (cd "$2" && claude mcp get bana) >/dev/null 2>&1 && return 0
  if ! bm_has "$1" mcp; then
    warn "Claude Code gets no bana tools (run_jobs, commit_fix): this bana-manager predates them (bana daemon install)"
    return 0
  fi
  printf '%s\n' --mcp-config "$("$1" mcp --dir "$home" --config)"
}

# bana fix --headless: the settings it runs with (checked first), and the daemon it needs.
fix_headless_check() {
  local r bare='(^|[ ,])Bash([ ,]|$)' any='Bash\((:?\*)?\)' git='Bash\(git[ :*)]'
  r=$(conf fix.turns 60)
  [[ $r =~ ^[1-9][0-9]{0,3}$ ]] || die "fix.turns: a number from 1 to 9999, not '$r'"
  r=$(conf fix.budget_usd 5)
  [[ $r =~ ^[0-9]{1,4}(\.[0-9]{1,2})?$ && ! $r =~ ^0+(\.0*)?$ ]] || die "fix.budget_usd: dollars, like 5 or 2.50, not '$r'"
  r=$(conf fix.allow)
  if [[ $r =~ $bare || $r =~ $any || $r =~ $git ]]; then
    die "fix.allow: narrow rules only, like 'Bash(cargo test:*)': not all of Bash, nor git (its --output writes files; commit_fix commits)"
  fi
  command -v claude >/dev/null || die "Claude Code (claude) is not on PATH: https://claude.com/claude-code"
  # shellcheck source=SCRIPTDIR/daemon.sh
  source "$bana_root/lib/daemon.sh"
  fix_port=$(d_port)
  if ! r=$(d_health "$fix_port") || ! d_is_ours "$r"; then
    die "bana fix --headless needs the daemon, whose rounds test Claude's changes: bana daemon install (or status)"
  fi
}

# Claude Code on fix FIX, unattended: -p in its worktree, with only the tools listed (dontAsk
# denies the rest without asking), bana's MCP server alone, and its stream in claude.jsonl.
fix_headless() { # BANA-MANAGER FIX WT DIR
  local m=$1 fix=$2 wt=$3 dir=$4 log=$4/claude.jsonl sha out mc allow extra turns budget r rc=0 n
  local sub cost sid
  sha=$(fx "$fix" sha)
  bm_has "$m" mcp || die "bana fix --headless needs bana's tools, which this bana-manager predates: bana daemon install"
  mc=$("$m" mcp --dir "$home" --config) || die "bana-manager gave no MCP config"
  # Registered, the daemon runs the failed jobs again at the failing commit (round 0), which
  # its clone may not have yet (a hand run's, a paste's): bana pushes it there, hooks off.
  if out=$(git -C "$wt" -c core.hooksPath=/dev/null push -q "$home/src" "$sha:refs/bana/fix/$fix/base" 2>&1) &&
    out=$(d_curl "$fix_port" /ci/v1/fixes -X POST -H 'Content-Type: application/json' --data "{\"fix\":\"$fix\"}" 2>&1); then
    [[ $(fix_json recheck <<<"$out") != 0 ]] || echo "  round 0: the daemon runs the failed jobs again at $fix"
  else
    warn "The daemon did not take fix $fix, so no round 0: ${out:-no answer}"
  fi
  allow="Read Grep Glob Edit(/$wt/**) Write(/$wt/**) mcp__bana__fix_brief mcp__bana__ci_log mcp__bana__run_jobs"
  allow+=" mcp__bana__fix_status mcp__bana__commit_fix"
  extra=$(conf fix.allow) turns=$(conf fix.turns 60) budget=$(conf fix.budget_usd 5)
  [[ -z $extra ]] || allow+=" $extra"
  say "Claude Code works on fix $fix unattended (at most $turns turns and \$$budget): tail -f $log"
  (cd "$wt" && exec claude -p "$(cat "$dir/prompt.txt")" -n "bana fix $fix" --permission-mode dontAsk \
    --allowedTools "$allow" --max-turns "$turns" --max-budget-usd "$budget" \
    --strict-mcp-config --mcp-config "$mc" --output-format stream-json --verbose) </dev/null >"$log" || rc=$?
  r=$(grep -E '^\{"type": *"result"' "$log" | tail -1) || true
  [[ -n $r ]] || die "Claude Code ended (exit $rc) without a result: $log"
  sub=$(fix_json subtype <<<"$r") n=$(fix_json num_turns <<<"$r")
  cost=$(fix_json total_cost_usd <<<"$r") sid=$(fix_json session_id <<<"$r")
  if [[ $sub == success ]]; then say "Claude Code is done with fix $fix"; else warn "Claude Code stopped: $sub"; fi
  echo "  $sub, $(plural "${n:-0}" turn), \$${cost:-?}"
  if n=$(fix_ahead "$(fx "$fix" checkout)" "$sha" "$(fx "$fix" branch)") && ((n > 0)); then
    echo "  $(fx "$fix" branch): $(plural "$n" commit), not pushed (bana fix push $fix)"
  else
    echo "  $(fx "$fix" branch): no commit"
  fi
  [[ -z $sid ]] || printf '  take over: cd %q && claude --resume %s\n' "$wt" "$sid"
  [[ $sub == success ]]
}

fix_start() {
  local src=() open='' headless='' fix_port='' m checkout out fix wt dir branch link cmd v mcp=() a
  while (($#)); do
    case $1 in
    --open) open=1 ;;
    --headless) headless=1 ;;
    --log) ((${#src[@]} == 0)) || fix_usage; src=(--log "${2:?--log FILE|-}"); shift ;;
    last) ((${#src[@]} == 0)) || fix_usage; src=(--run) ;;
    -h | --help | help) fix_usage ;;
    *) [[ $1 =~ ^[0-9]+$ && ${#src[@]} == 0 ]] || fix_usage; src=(--build "$1") ;;
    esac
    shift
  done
  [[ -z $open || -z $headless ]] || fix_usage
  [[ -z $headless ]] || fix_headless_check
  m=$(bm fix)
  checkout=$(fix_checkout)
  ((${#src[@]})) || fix_default "$(git -C "${BANA_PROJECT_ROOT:-.}" symbolic-ref -q HEAD 2>/dev/null || true)"
  [[ ${src[*]} != "--log -" || ! -t 0 ]] || echo "Paste act's output, then Ctrl-D:" >&2
  local args=(fix prepare --dir "$home" --checkout "$checkout" "${src[@]}" --workflow "$(conf workflow ci.yml)" --bana "$self")
  [[ ! $repo =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || args+=(--repo "$repo")
  [[ -z $headless ]] || args+=(--headless)
  out=$("$m" "${args[@]}" 2>&1) || die "${out#bana-manager: }"
  fix=$(fix_json fix <<<"$out") wt=$(fix_json worktree <<<"$out") dir=$(fix_json dir <<<"$out")
  branch=$(fix_json branch <<<"$out") link=$(fix_json link <<<"$out") cmd=$(fix_json command <<<"$out")
  [[ -n $fix && -d $wt && -f $dir/prompt.txt ]] || die "bana-manager made no fix: $out"
  if [[ $(fix_json reused <<<"$out") == true ]]; then say "Fix $fix goes on: $branch, in $wt"
  else say "Fix $fix: $branch, in $wt"; fi
  echo "  what failed, and how it ran: $dir/brief.md (bana fix brief)"

  if ! command -v claude >/dev/null; then
    warn "Claude Code (claude) is not on PATH: https://claude.com/claude-code"
    echo "The fix is ready. Start Claude Code in its worktree, with its prompt:"
    echo "  worktree: $wt"
    echo "  prompt:   $dir/prompt.txt"
    echo "  $cmd"
    return 0
  fi
  # The brief says which Claude Code ran: its flags and settings change.
  v=$(claude --version 2>/dev/null | awk 'NR == 1') || true
  [[ -z $v ]] || printf -- '- Claude Code: %s\n' "$v" >>"$dir/brief.md"
  if [[ -n $headless ]]; then fix_headless "$m" "$fix" "$wt" "$dir"; return; fi
  while IFS= read -r a; do mcp+=("$a"); done < <(fix_mcp "$m" "$wt")
  if [[ -n $open ]]; then
    ((${#mcp[@]} == 0)) || warn "Claude Code has no bana tools in $wt (bana daemon install registers them): without them, bana fix $fix in a terminal"
    # Claude Code's own handler opens a terminal in the worktree, with the prompt typed.
    if [[ $os == Darwin ]]; then open "$link"
    elif command -v xdg-open >/dev/null; then xdg-open "$link"
    else die "--open needs xdg-open (a desktop). Without it: $cmd"; fi ||
      die "Nothing opened Claude Code's link (claude registers it when it first runs). Without it: $cmd"
    say "Claude Code opens in a new terminal, in the worktree, with the prompt typed: press Enter there."
    return 0
  fi
  # A log pasted on stdin used it up: Claude Code gets the terminal back.
  if [[ ! -t 0 ]] && (: </dev/tty) 2>/dev/null; then exec </dev/tty; fi
  cd "$wt" || die "No worktree $wt"
  # --mcp-config takes several values: -n ends them, before the prompt.
  exec claude ${mcp[@]+"${mcp[@]}"} -n "bana fix $fix" "$(cat "$dir/prompt.txt")"
}

# ---- the fixes ---------------------------------------------------------------------------

# The fixes, newest first: UPDATED FIX lines, from fix/<sha7>.d/fix.json.
fix_all() {
  local f x
  for f in "$home"/fix/*.d/fix.json; do
    x=${f%.d/fix.json} x=${x##*/}
    if [[ -f $f && $x =~ ^[0-9a-f]{7}$ ]]; then printf '%s %s\n' "$(fix_json updated <"$f")" "$x"; fi
  done | sort -k1,1nr -k2,2r
}

# The fix FIX names (its commit's first digits), else the one whose worktree this is, else
# the newest: as bana fix brief finds it.
fix_find() { # [FIX]
  local n all x here wt hits=()
  n=$(printf '%s' "${1:-}" | lower)
  all=$(fix_all)
  [[ -n $all ]] || die "No fix yet: bana fix makes one"
  if [[ -n $n ]]; then
    [[ $n =~ ^[0-9a-f]{4,64}$ ]] || die "No fix $1"
    while read -r _ x; do
      [[ $(fx "$x" sha) != "$n"* && $n != "$x"* ]] || hits+=("$x")
    done <<<"$all"
    ((${#hits[@]} < 2)) || die "$1 names more than one fix: ${hits[*]}"
    ((${#hits[@]})) || die "No fix $1 (bana fix list)"
    echo "${hits[0]}"
    return
  fi
  here=$(pwd -P)
  while read -r _ x; do
    wt=$(fx "$x" worktree)
    if [[ -n $wt && ($here == "$wt" || $here == "$wt"/*) ]]; then echo "$x"; return; fi
  done <<<"$all"
  x=${all%%$'\n'*}
  echo "${x#* }"
}

# How many commits BRANCH has on SHA (fails when the branch is gone).
fix_ahead() { git -C "$1" rev-list --count "$2..refs/heads/$3" 2>/dev/null; } # CHECKOUT SHA BRANCH
plural() { if (($1 == 1)); then echo "1 $2"; else echo "$1 $2s"; fi; } # N WORD

fix_brief() { # [FIX]
  local m out
  (($# <= 1)) || fix_usage
  case ${1:-} in -*) fix_usage ;; esac
  m=$(bm fix)
  out=$("$m" fix brief --dir "$home" "$@" 2>&1) || die "${out#bana-manager: }"
  printf '%s\n' "$out"
}

fix_list() {
  local all x checkout wt branch sha ref n what from
  all=$(fix_all)
  [[ -n $all ]] || { echo "No fixes yet: bana fix makes one."; return 0; }
  while read -r _ x; do
    checkout=$(fx "$x" checkout) wt=$(fx "$x" worktree) branch=$(fx "$x" branch) sha=$(fx "$x" sha) ref=$(fx "$x" ref)
    if ! n=$(fix_ahead "$checkout" "$sha" "$branch"); then what="$branch is gone"
    elif ((n == 0)); then what="$branch: no commits yet"
    else what="$branch: $(plural "$n" commit)"; fi
    if [[ ! -d $wt ]]; then what+=", worktree removed"
    elif [[ -n $(git -C "$wt" status --porcelain 2>/dev/null) ]]; then what+=", changes not committed"; fi
    case $(fx "$x" origin) in
    build) from="daemon build $(fx "$x" build)" ;;
    run) from="bana ci here" ;;
    *) from="a pasted log" ;;
    esac
    [[ $ref != refs/heads/* ]] || from+=", on ${ref#refs/heads/}"
    printf '%s  %s (%s)\n' "$x" "$what" "$from"
    [[ ! -d $wt ]] || printf '         %s\n' "$wt"
  done <<<"$all"
}

fix_push() { # [FIX] [--pr]
  local name='' pr='' x m out branch sha ref r n args=()
  while (($#)); do
    case $1 in
    --pr) pr=1 ;;
    -*) fix_usage ;;
    *) [[ -z $name ]] || fix_usage; name=$1 ;;
    esac
    shift
  done
  x=$(fix_find "$name")
  sha=$(fx "$x" sha) ref=$(fx "$x" ref)
  m=$(bm "fix push")
  # Your push, as any other: your hooks run, and bana's tells the daemon, which builds it.
  # bana-manager does it (as the fix card's Push); git talks here as it pushes.
  out=$("$m" fix push --dir "$home" "$x") || exit 1
  branch=$(fix_json branch <<<"$out") n=$(fix_json commits <<<"$out")
  [[ $(fix_json dirty <<<"$out") != true ]] || warn "$(fx "$x" worktree) has changes not committed: they stayed out"
  say "Pushed $branch ($(plural "$n" commit) on ${sha:0:7})"
  [[ -n $pr ]] || return 0
  command -v gh >/dev/null || die "--pr needs the GitHub CLI: brew install gh, then gh auth login"
  args=(pr create --fill)
  if [[ $ref == refs/heads/* ]]; then args+=(--base "${ref#refs/heads/}")
  else warn "The failure was on no branch: the pull request goes to the default branch"; fi
  args+=(--head "$branch")
  r=$(fx "$x" repo)
  [[ -n $r ]] || r=$repo
  [[ ! $r =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || args+=(--repo "$r")
  (cd "$(fx "$x" checkout)" && gh "${args[@]}")
}

fix_drop() { # [FIX] [--force] [--delete-branch]
  local name='' x m out n flags=()
  while (($#)); do
    case $1 in
    --force | --delete-branch) flags+=("$1") ;;
    -*) fix_usage ;;
    *) [[ -z $name ]] || fix_usage; name=$1 ;;
    esac
    shift
  done
  x=$(fix_find "$name")
  m=$(bm "fix drop")
  # bana-manager does it, as the fix card's Discard: never with changes not committed or
  # submodule commits no remote has (they would go), unless --force.
  out=$("$m" fix drop --dir "$home" "$x" ${flags[@]+"${flags[@]}"} 2>&1) || die "${out#bana-manager: }"
  [[ $(fix_json removed <<<"$out") != true ]] || say "Removed the worktree $(fix_json worktree <<<"$out")"
  n=$(fix_json kept <<<"$out")
  if [[ -n $n ]]; then
    say "$(fix_json branch <<<"$out") stays, with its $(plural "$n" commit): bana fix push $x, or bana fix drop $x --delete-branch"
  elif [[ $(fix_json deleted <<<"$out") == true ]]; then
    say "Deleted $(fix_json branch <<<"$out")"
  fi
}

fix_main() {
  case ${1:-} in
  brief) shift; fix_brief "$@" ;;
  list) shift; (($# == 0)) || fix_usage; fix_list ;;
  push) shift; fix_push "$@" ;;
  drop) shift; fix_drop "$@" ;;
  *) fix_start "$@" ;;
  esac
}

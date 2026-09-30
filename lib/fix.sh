# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets os, repo, home, self, bana_root
# bana fix: a failed CI run handed to Claude Code, on a branch of its own. bana-manager does
# the work (the worktree, the brief, the prompt); docs/FIX.md has the rest. Sourced by bin/bana.
#
#   bana fix [BUILD | last | --log FILE|-] [--open]
#     makes bana/fix-<sha7> at the failing commit, a worktree of this checkout in
#     ~/.bana/<prefix>/fix/<sha7>, with the brief (what failed, and how it ran) and the
#     prompt, and starts Claude Code there
#     BUILD            a daemon build, by its number
#     last             the last bana ci here
#     --log FILE       act's output from elsewhere (-: pasted on stdin); the fix starts at HEAD
#     (none)           the newer of the last bana ci here, if it failed, and the newest
#                      failed daemon build of this branch
#     --open           Claude Code in a new terminal instead, through its claude-cli:// link
#   bana fix brief [FIX]   what failed, where, and how it ran
#   bana fix list          the fixes: their branches, commits and worktrees
#   bana fix push [FIX] [--pr]   push the branch to origin (the daemon then builds it); --pr
#                          also opens a pull request against the branch that failed
#   bana fix drop [FIX] [--force] [--delete-branch]   remove the worktree (--force: with its
#                          changes); the branch stays while it has commits
#   FIX: the commit's first digits (4 or more); by default the fix you are in, else the newest.
#   Claude Code works in the worktree with your own settings, plus rules against git push:
#   a guard for Claude, not a lock (docs/FIX.md). bana pushes nothing: bana fix push is yours.

fix_usage() { awk '/^#   bana fix \[/, /^#   a guard for Claude/ { sub(/^# ?/, ""); print }' "$bana_root/lib/fix.sh" >&2; exit 2; }

# git in the owner's checkout without their hooks, as bana-manager runs it there.
fix_git() { # DIR GIT-ARGS...
  local dir=$1
  shift
  git -C "$dir" -c core.hooksPath=/dev/null "$@"
}

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

fix_start() {
  local src=() open='' m checkout out fix wt dir branch link cmd v
  while (($#)); do
    case $1 in
    --open) open=1 ;;
    --log) ((${#src[@]} == 0)) || fix_usage; src=(--log "${2:?--log FILE|-}"); shift ;;
    last) ((${#src[@]} == 0)) || fix_usage; src=(--run) ;;
    -h | --help | help) fix_usage ;;
    *) [[ $1 =~ ^[0-9]+$ && ${#src[@]} == 0 ]] || fix_usage; src=(--build "$1") ;;
    esac
    shift
  done
  m=$(bm fix)
  checkout=$(fix_checkout)
  ((${#src[@]})) || fix_default "$(git -C "${BANA_PROJECT_ROOT:-.}" symbolic-ref -q HEAD 2>/dev/null || true)"
  [[ ${src[*]} != "--log -" || ! -t 0 ]] || echo "Paste act's output, then Ctrl-D:" >&2
  local args=(fix prepare --dir "$home" --checkout "$checkout" "${src[@]}" --workflow "$(conf workflow ci.yml)" --bana "$self")
  [[ ! $repo =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || args+=(--repo "$repo")
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
  if [[ -n $open ]]; then
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
  exec claude -n "bana fix $fix" "$(cat "$dir/prompt.txt")"
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

# A path as it is on disk; a missing one through its directory's.
fix_real() { # PATH
  local d
  if d=$(cd "$1" 2>/dev/null && pwd -P); then echo "$d"
  elif d=$(cd "$(dirname "$1")" 2>/dev/null && pwd -P); then echo "$d/$(basename "$1")"
  else echo "$1"; fi
}

# WORKTREE as CHECKOUT's git lists it, its directory there or not (fails when git has none).
fix_listed() { # CHECKOUT WORKTREE
  local w want
  want=$(fix_real "$2")
  while IFS= read -r w; do
    [[ $w == "worktree "* && $(fix_real "${w#worktree }") == "$want" ]] || continue
    echo "${w#worktree }"
    return 0
  done < <(git -C "$1" worktree list --porcelain 2>/dev/null)
  return 1
}

# The worktree's submodules with commits that no remote branch or tag has, a path a line: a
# linked worktree keeps its submodules' repositories in its own git directory, so they go
# with it.
fix_lost() { # WORKTREE
  # shellcheck disable=SC2016 # git's submodule foreach expands them
  git -C "$1" submodule foreach --quiet --recursive \
    'n=$(git rev-list --count HEAD --branches --not --remotes --tags 2>/dev/null) || n=1; [ "$n" = 0 ] || echo "$displaypath"' \
    2>/dev/null || true
}

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
  local name='' pr='' x checkout wt branch sha ref r n args=()
  while (($#)); do
    case $1 in
    --pr) pr=1 ;;
    -*) fix_usage ;;
    *) [[ -z $name ]] || fix_usage; name=$1 ;;
    esac
    shift
  done
  x=$(fix_find "$name")
  checkout=$(fx "$x" checkout) wt=$(fx "$x" worktree) branch=$(fx "$x" branch) sha=$(fx "$x" sha) ref=$(fx "$x" ref)
  n=$(fix_ahead "$checkout" "$sha" "$branch") || die "$branch is gone from $checkout"
  ((n > 0)) || die "$branch has no commits on ${sha:0:7} yet: nothing to push"
  [[ ! -d $wt || -z $(git -C "$wt" status --porcelain 2>/dev/null) ]] || warn "$wt has changes not committed: they stay out"
  # Your push, as any other: your hooks run, and bana's tells the daemon, which builds it.
  git -C "$checkout" push -u origin "$branch" || die "git could not push $branch to origin"
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
  (cd "$checkout" && gh "${args[@]}")
}

fix_drop() { # [FIX] [--force] [--delete-branch]
  local name='' force='' delete='' x checkout wt branch sha n changes lost listed
  while (($#)); do
    case $1 in
    --force) force=1 ;;
    --delete-branch) delete=1 ;;
    -*) fix_usage ;;
    *) [[ -z $name ]] || fix_usage; name=$1 ;;
    esac
    shift
  done
  x=$(fix_find "$name")
  checkout=$(fx "$x" checkout) wt=$(fx "$x" worktree) branch=$(fx "$x" branch) sha=$(fx "$x" sha)
  if [[ -d $wt && -z $force ]]; then
    # Submodules' changes too, whatever .gitmodules says to ignore.
    changes=$(git -C "$wt" status --porcelain --ignore-submodules=none 2>/dev/null) || true
    if [[ -n $changes ]]; then
      printf '%s\n' "$changes" | sed 's/^/  /' >&2
      die "$wt has changes not committed: commit them, or bana fix drop $x --force (they go)"
    fi
    lost=$(fix_lost "$wt")
    if [[ -n $lost ]]; then
      printf '%s\n' "$lost" | sed 's/^/  /' >&2
      die "$wt has submodule commits that no remote has, and they go with it: push them, or bana fix drop $x --force"
    fi
  fi
  # git forgets this worktree only, its directory there or not. (A prune would forget any
  # missing worktree of yours too, on a volume not mounted now, with its index and HEAD.)
  if listed=$(fix_listed "$checkout" "$wt"); then
    # --force for a clean one too: git keeps a worktree with submodules otherwise.
    fix_git "$checkout" worktree remove --force "$listed" || die "git could not remove $wt"
    say "Removed the worktree $wt"
  elif [[ -d $wt ]]; then
    die "$wt is not a worktree of $checkout: remove it yourself, if nothing in it is yours"
  fi
  if n=$(fix_ahead "$checkout" "$sha" "$branch") && ((n > 0)) && [[ -z $delete ]]; then
    say "$branch stays, with its $(plural "$n" commit): bana fix push $x, or bana fix drop $x --delete-branch"
    return 0
  fi
  if git -C "$checkout" rev-parse -q --verify "refs/heads/$branch" >/dev/null; then
    fix_git "$checkout" branch -q -D "$branch"
    say "Deleted $branch"
  fi
  rm -rf "${home:?}/fix/$x.d"
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

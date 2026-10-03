# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets os, prefix, repo, home, host, base_home, extra_labels, self
# bana add: this project's CI, on the daemon here. It has act evaluate the runs-on of every
# job in the workflow, says where each job would run (here, under bana ci and the daemon;
# and in a pool of bana up runners), and proposes bana.conf's settings and changes to the
# workflow, written only if you say so. Then it adds the project to the daemon (bana daemon
# install starts it, once a machine). Sourced by bin/bana.
#
#   bana add [--check] [--diff] [--workflow FILE] [--no-hook] [--no-claude] [--split[=OWNER/NAME]]
#     --workflow FILE  the workflow in .github/workflows (default: bana.conf's workflow, else
#                      ci.yml, else the only one with jobs and workflow_dispatch)
#     --check          ask, write and add nothing; exit 1 when a job has no place here, a
#                      matrix splits over runners, or the workflow has no workflow_dispatch
#     --diff           only the proposed workflow changes, as a patch for git apply
#     --no-hook        no git hook that tells the daemon about your pushes at once
#     --no-claude      don't register bana's tools (its MCP server) with Claude Code here
#     --split[=R]      then bana split on: builds and releases on a public repository's
#                      GitHub Actions, the code private (docs/SPLIT.md; R: bana split on --repo R)
#   It needs act, and mikefarah's yq (else the one in act.image, through Docker). On a
#   terminal it asks where a label bana does not know runs (bana.conf keeps the answer),
#   then whether to write bana.conf and to apply the workflow changes (git apply: no
#   branch, no commit). Run it again after changing bana.conf: the daemon reads it then.
#
#   Adding the project: its settings in ~/.bana/<prefix>/daemon/settings, the daemon's own
#   clone (~/.bana/<prefix>/src: your checkout stays yours), a reference-transaction hook here
#   that asks the daemon to fetch when a push updates origin/<branch>, and bana's MCP server
#   for Claude Code, in this checkout's local scope (the tools Claude uses in a fix:
#   fix_brief, run_jobs, commit_fix...). bana remove undoes it.

add_usage() { awk '/^#   bana add \[/, /^#   branch, no commit/ { sub(/^# ?/, ""); print }' "$bana_root/lib/add.sh" >&2; exit 2; }
add_missing() { printf '\033[31m%s\033[0m\n' "$*" >&2; exit 2; } # a tool: --check's exit 2

# The workflow, read with yq: a wf line, then a job line each; "-" is none. Fields:
# wf, workflow_dispatch, push or schedule, on's tag, its key's line, its value's line, the
# first choice input, its options. job, id, line, runs-on's tag, key line, value line, JSON,
# needs, if's tag, line, JSON, uses, container image, services, strategy.matrix, whether the
# matrix reads needs.
# shellcheck disable=SC2016 # yq's
add_meta='(.on | tag) as $t |
(["wf",
  (($t == "!!map" and (.on | has("workflow_dispatch"))) or ($t == "!!str" and .on == "workflow_dispatch") or
    ($t == "!!seq" and (.on | any_c(. == "workflow_dispatch")))),
  (($t == "!!map" and ((.on | has("push")) or (.on | has("schedule")))) or ($t == "!!str" and (.on == "push" or .on == "schedule")) or
    ($t == "!!seq" and (.on | any_c(. == "push" or . == "schedule")))),
  $t, (.on | key | line), (.on | line),
  (((.on | select(tag == "!!map") | .workflow_dispatch.inputs // {}) | to_entries | map(select(.value.type == "choice")) |
    .[0] | [.key, (((.value.options // []) | join(" ")) // "-")] | join("\t")) // "-\t-")
] | map(tostring) | join("\t")),
(.jobs | to_entries | .[] | ["job", .key, (.key | line), (.value."runs-on" | tag), (.value."runs-on" | key | line),
  (.value."runs-on" | line), (.value."runs-on" | to_json(0)), (.value | has("needs")), (.value.if | tag), (.value.if | line),
  ((.value.if // "") | to_json(0)), (.value.uses // "-"),
  ((.value.container | select(tag == "!!map") | .image) // (.value.container | select(tag == "!!str")) // "-"),
  (.value | has("services")), (.value.strategy.matrix != null),
  ((.value.strategy.matrix // "") | to_json(0) | test("needs[.]"))] | map(tostring) | join("\t"))'
# The copy act lists the labels from: only workflow_dispatch, and of each job only its
# runs-on and strategy (no needs, no if: every job on its own), with one step.
add_strip='{"on": {"workflow_dispatch": ((.on | select(tag == "!!map") | .workflow_dispatch) // {})},
  "jobs": (.jobs | with_entries(select((.value | has("uses") or has("container")) | not)) |
    map_values(pick(["runs-on", "strategy"]) | .steps = [{"run": "true"}]))}'

add_main() {
  local check='' diff='' wf='' hook=1 claude=1 rc counts unmapped='' split='' to_split='' split_args=()
  i_counts=''
  while (($#)); do
    case $1 in
    --check) check=1 ;;
    --diff) diff=1 ;;
    --workflow) wf=${2:?--workflow FILE}; shift ;;
    --no-hook) hook='' ;;
    --no-claude) claude='' ;;
    --split) to_split=1 ;;
    --split=*) to_split=1 split_args=(--repo "${1#--split=}") ;;
    *) add_usage ;;
    esac
    shift
  done
  if [[ -n $check$diff ]]; then
    add_look "$check" "$diff" "$wf"
    return
  fi
  # The report and the proposals, in a shell of their own: one that stops (no act or yq)
  # stops only them, and bana.conf is read again afterwards, as written.
  counts=$(mktemp "${TMPDIR:-/tmp}/bana-add.XXXXXX")
  i_counts=$counts
  set +e
  (
    set -e
    add_look '' '' "$wf"
  )
  rc=$?
  set -e
  read -r unmapped split <"$counts" || true
  case $rc in
  0) ;;
  2) warn "So where each job runs here is not checked; bana add goes on." ;;
  *) rm -f "$counts"; exit "$rc" ;;
  esac
  load_project
  [[ -s $counts ]] || echo >"$counts"
  [[ -z $wf ]] || echo "workflow ${wf##*/}" >>"$counts"
  add_agrees "$counts" || { rm -f "$counts"; exit 1; }
  rm -f "$counts"
  add_register "$hook" "$claude" "$unmapped" "$split"
  [[ -n $to_split ]] || return 0
  echo
  # shellcheck source=SCRIPTDIR/split.sh
  source "$bana_root/lib/split.sh"
  split_main on ${split_args[@]+"${split_args[@]}"}
}

# The daemon builds as bana.conf says, as bana ci does. A workflow or prefix the report
# found and bana.conf does not say yet (it was not written) stops bana add; tiers only warn.
add_agrees() { # COUNTS: "KEY VALUE" lines after the first
  local k want have missing='' differ=''
  while read -r k want; do
    have=$(conf_lookup "$k" 2>/dev/null) || have=$(add_default "$k")
    have=$(words "$have" | tr -s ' ' | sed 's/^ //; s/ $//')
    [[ $have != "$want" ]] || continue
    case $k in
    workflow | prefix) missing+="  $k =${want:+ $want}"$'\n' ;;
    *) differ+=" $k = $have (the workflow says${want:+ $want}${want:- none})," ;;
    esac
  done < <(sed 1d "$1")
  [[ -z $differ ]] || warn "The daemon builds with${differ%,}: bana.conf says so."
  [[ -n $missing ]] || return 0
  printf 'bana.conf does not say what this project needs yet:\n%s' "$missing" >&2
  echo "Add these lines to ${conf_file:-.github/bana.conf} (or rerun bana add on a terminal and say yes), then bana add again." >&2
  return 1
}

# The project, added to the daemon here: its settings, its clone, the push hook, bana's
# tools for Claude Code, and a word to the daemon, if it runs, which starts it.
add_register() { # HOOK CLAUDE UNMAPPED SPLIT
  local root gh old row v
  need_repo
  root=$(d_root)
  echo
  say "Adding $repo to the daemon here, as $prefix"
  old=$(d_setting repo "$(d_project "$prefix")") || old=''
  [[ -z $old || $old == "$repo" ]] || die "$prefix is $old's here already (bana list): set another prefix in bana.conf"
  ! d_olds | cut -f1 | grep -qx "$prefix" ||
    die "$prefix still has its own daemon of before: bana daemon install moves it to the one daemon (keeping its port), then bana add"
  gh=$(command -v gh) || die "The GitHub CLI is needed: brew install gh, then gh auth login"
  d_doctor_project "$root" "$gh" "$3" "$4"
  d_write_project "$root"
  echo "  settings: $(d_project "$prefix")"
  d_clone "$root" "$gh"
  if [[ -n $1 ]]; then d_hook_install "$root" "$prefix"; else d_hook_remove "$root"; fi
  [[ -z $2 ]] || d_claude_add "$root" "$home"
  if ! d_up; then
    say "Added $repo: bana daemon install starts CI on this machine (once, for every project)."
    return 0
  fi
  row=$(d_rescan | d_row "$prefix") || row=''
  v=$(d_val "$(d_flat <<<"$row")" error)
  if [[ -z $row ]]; then warn "The daemon did not list $prefix: bana daemon status"
  elif [[ $v != null ]]; then warn "The daemon could not start $prefix: $v"; fi
  say "Added $repo: $(d_url "$(d_port)" "p=$prefix")"
}

# The report and the proposals (written on a terminal, if you say so).
add_look() { # CHECK DIFF WORKFLOW
  local check=$1 diff=$2 wf=$3 v i job labels entry e keys args=() rc=0
  # shellcheck source=SCRIPTDIR/act.sh
  source "$bana_root/lib/act.sh"
  if ! i_root=$(git rev-parse --show-toplevel 2>/dev/null) || ! git -C "$i_root" remote get-url origin >/dev/null 2>&1; then
    die "Run bana add in the project's checkout (a git repository with an origin)"
  fi
  i_ask=
  [[ -n $check$diff || ! -t 0 ]] || i_ask=1
  command -v act >/dev/null || add_missing "act is needed: brew install act (https://nektosact.com)"
  v=$(act --version 2>/dev/null | awk 'NR == 1 { print $NF }')
  [[ $v == 0.2.89 ]] || warn "act ${v:-of an unknown version}: bana add was checked with act 0.2.89"
  add_yq
  mkdir -p "$base_home/init"
  i_tmp=$(mktemp -d "$base_home/init/XXXXXX")
  trap 'rm -rf "$i_tmp"' EXIT
  : >"$i_tmp/notes"
  add_workflow "$wf"
  add_read
  i_varfile=() i_vars="their defaults"
  [[ ! -f $home/vars ]] || i_varfile=(--var-file "$home/vars") i_vars=$home/vars

  # Each job's labels, as act evaluates them; a matrix whose runs-on reads it, an entry at a time.
  add_scratch
  mkdir -p "$i_tmp/check/.github/workflows"
  cp "$i_wf" "$i_tmp/check/.github/workflows/$i_name"
  i_listed=$(add_list)
  i_actok=1
  if ! (cd "$i_tmp/check" && HOME=$i_tmp/copy/home act -l -W ".github/workflows/$i_name" >/dev/null 2>"$i_tmp/act.err"); then
    i_actok=
    printf '0\tact cannot read the workflow as it is, so bana ci cannot run it: %s\n' \
      "$(grep -v '^time=' "$i_tmp/act.err" | head -2 | tr '\n' ' ' | sed 's/ *$//')" >>"$i_tmp/notes"
  fi
  : >"$i_tmp/rows"
  for ((i = 0; i < ${#j_id[@]}; i++)); do
    job=${j_id[i]}
    if [[ ${j_uses[i]} != - ]]; then add_row "$job" - uses - "${j_uses[i]}"; continue; fi
    if [[ ${j_cont[i]} != - ]]; then add_row "$job" - container - "${j_cont[i]}"; continue; fi
    v=-
    [[ ${j_ro[i]} != *needs.* && ${j_mxneeds[i]} != true ]] || v=needs
    # A matrix from needs outputs: act cannot build it with the needs stripped.
    if [[ ${j_mx[i]} == true && ${j_ro[i]} == *matrix.* && $v != needs ]]; then
      keys=$(grep -o 'matrix\.[A-Za-z0-9_-]*' <<<"${j_ro[i]}" | sed 's/^matrix\.//' | sort -u | tr '\n' ' ')
      add_act -v -j "$job" >"$i_tmp/act.out" || true
      sed -n "s/.*Final matrix after applying user inclusions '\[\(.*\)\]'.*/\1/p" "$i_tmp/act.out" | head -1 |
        add_entries "$keys" >"$i_tmp/entries"
      if [[ -s $i_tmp/entries && $(cat "$i_tmp/entries") != "?" ]]; then
        cp "$i_tmp/entries" "$i_tmp/entries.$job"
        while IFS= read -r entry <&3; do
          args=()
          while IFS= read -r e; do args+=(--matrix "$e"); done < <(tr '\t' '\n' <<<"$entry")
          add_row "$job" "$(awk -F'\t' -v OFS=' ' '{ for (i = 1; i <= NF; i++) sub(/:/, "=", $i); $1 = $1; print }' <<<"$entry")" \
            run "$(add_labels "$job" "${args[@]}")" "$v"
        done 3<"$i_tmp/entries"
        continue
      fi
      warn "$job: bana add cannot read its matrix entries apart; its labels are the first entry's"
    fi
    add_row "$job" - run "$(add_labels "$job")" "$v"
  done

  add_prefix
  prefix=$i_prefix # the built-in act.platform keys are the proposed prefix's
  act_platform_table >"$i_tmp/table"
  add_sort
  if [[ -n $diff ]]; then
    add_edits
    cat "$i_tmp/patch"
    return 0
  fi
  add_report
  add_edits
  add_notes
  add_conf
  add_next
  if [[ -n $check ]]; then
    [[ $i_unmapped == 0 && $i_split == 0 && $i_dispatch == true && -n $i_actok ]] || rc=1
    echo "bana add --check: $i_unmapped jobs with no place here, $i_split split matrices, workflow_dispatch: $([[ $i_dispatch == true ]] && echo yes || echo no)"
    return "$rc"
  fi
  [[ -z ${i_counts:-} ]] || {
    echo "$i_unmapped $i_split"
    echo "prefix ${i_prefix_read:-$i_prefix}"
    echo "workflow $i_name"
    echo "tiers $i_tiers"
    [[ -z $i_tier_input ]] || echo "tier_input $i_tier_input"
  } >"$i_counts"
  add_write
}

# yq: mikefarah's here, else the one act.image has (act's Linux jobs run in it anyway).
add_yq() {
  local image
  if yq --version 2>/dev/null | grep -q mikefarah; then i_yq=(yq); i_yq_from=yq; return; fi
  image=$(conf act.image catthehacker/ubuntu:act-24.04)
  if ! command -v docker >/dev/null || ! (act_docker) >/dev/null 2>&1; then
    add_missing "yq is needed (mikefarah's: brew install yq), or Docker running, for the yq in $image"
  fi
  act_docker
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    say "Pulling $image, for its yq (brew install yq skips this)" >&2
    docker pull "$image" >&2 || add_missing "docker pull $image failed: brew install yq (mikefarah's)"
  fi
  i_yq=(docker run --rm -i --entrypoint yq "$image")
  i_yq_from="yq in $image"
  "${i_yq[@]}" --version 2>/dev/null | grep -q mikefarah || add_missing "$image has no yq: brew install yq (mikefarah's)"
}
add_q() { "${i_yq[@]}" "$1" -; } # EXPRESSION: on the YAML on stdin

# The workflow: --workflow, bana.conf's, ci.yml (bana ci's default), the only one with jobs
# and workflow_dispatch, the only one with jobs, or the one you pick. i_why says which.
add_workflow() { # [NAME]
  local n=${1##*/} d=$i_root/.github/workflows f c=() w=() k
  i_why="bana add --workflow"
  [[ -n $n ]] || { n=$(conf workflow) && i_why="bana.conf's workflow"; }
  if [[ -z $n ]]; then
    for f in "$d"/*.yml "$d"/*.yaml; do [[ -f $f ]] && grep -q '^jobs:' "$f" && c+=("${f##*/}"); done
    ((${#c[@]})) || die "No workflow with jobs in $d"
    if [[ -f $d/ci.yml ]]; then
      n=ci.yml i_why="bana's default"
      ((${#c[@]} == 1)) || i_why="$i_why; bana add --workflow FILE for another: $(printf '%s\n' "${c[@]}" | grep -vx ci.yml | tr '\n' ' ' | sed 's/ $//')"
    elif ((${#c[@]} == 1)); then
      n=${c[0]} i_why="the only workflow with jobs"
    else
      for f in "${c[@]}"; do
        grep -qE '^[[:space:]]*(- )?workflow_dispatch([: ]|$)|^["'"'"']?on["'"'"']?:.*workflow_dispatch' "$d/$f" && w+=("$f")
      done
      if ((${#w[@]} == 1)); then
        n=${w[0]} i_why="the only workflow with jobs and workflow_dispatch; bana add --workflow FILE for another: $(printf '%s\n' "${c[@]}" | grep -vx "$n" | tr '\n' ' ' | sed 's/ $//')"
      else
        [[ -n $i_ask ]] || die "Which workflow is the project's CI? bana add --workflow FILE, one of: ${c[*]}"
        for ((k = 0; k < ${#c[@]}; k++)); do echo "  $((k + 1)) ${c[k]}"; done
        read -r -p "Which workflow is the project's CI? [1-${#c[@]}] " k
        [[ $k =~ ^[0-9]+$ ]] || die "No workflow chosen"
        ((k >= 1 && k <= ${#c[@]})) || die "No workflow chosen"
        n=${c[k - 1]} i_why="your pick"
      fi
    fi
  fi
  i_name=$n
  i_wf=$d/$n
  [[ -f $i_wf ]] || die "No workflow $i_wf"
}

# What yq reads: the triggers, the tier input, and each job.
add_read() {
  local kind a b c d e f g h k l m n o p q
  j_id=() j_line=() j_rotag=() j_rokey=() j_roline=() j_ro=() j_needs=() j_iftag=() j_ifline=() j_if=() j_uses=()
  j_cont=() j_svc=() j_mx=() j_mxneeds=()
  add_q "$add_meta" <"$i_wf" >"$i_tmp/meta" 2>"$i_tmp/err" ||
    die "yq cannot read $i_name: $(head -3 "$i_tmp/err")"
  while IFS=$'\t' read -r kind a b c d e f g h k l m n o p q _; do
    if [[ $kind == wf ]]; then
      i_dispatch=$a i_pushsched=$b i_ontag=$c i_onkey=$d i_online=$e i_tier_input=$f i_tiers=$g
      [[ $i_tier_input != - ]] || i_tier_input=
      [[ $i_tiers != - ]] || i_tiers=
      continue
    fi
    j_id+=("$a") j_line+=("$b") j_rotag+=("$c") j_rokey+=("$d") j_roline+=("$e") j_ro+=("$f") j_needs+=("$g")
    j_iftag+=("$h") j_ifline+=("$k") j_if+=("$l") j_uses+=("$m") j_cont+=("$n") j_svc+=("$o") j_mx+=("$p")
    j_mxneeds+=("$q")
  done <"$i_tmp/meta"
  ((${#j_id[@]})) || die "$i_name has no jobs"
}

# A copy of the workflow for act: stripped, in a repository with the project's origin and
# branch (so owner and branch conditions evaluate as they do there), and a HOME without
# your .actrc.
add_scratch() {
  local s=$i_tmp/copy br
  mkdir -p "$s/.github/workflows" "$s/home"
  add_q "$add_strip" <"$i_wf" >"$s/.github/workflows/$i_name"
  br=$(git -C "$i_root" symbolic-ref -q --short HEAD 2>/dev/null) || br=main
  git -C "$s" init -q
  git -C "$s" symbolic-ref HEAD "refs/heads/$br"
  git -C "$s" remote add origin "$(git -C "$i_root" remote get-url origin)"
  # Your git config's signing and hooks stay out of it.
  git -C "$s" -c user.name=bana -c user.email=bana@localhost -c commit.gpgsign=false -c core.hooksPath=/dev/null \
    commit -q --no-verify --allow-empty -m "bana add"
}

# act on the copy, dry, with no label mapped (act's own ubuntu defaults emptied too); -P
# once at least, or act asks for an image.
add_act() { # ACT-ARGUMENT...
  (
    unset XDG_CONFIG_HOME
    cd "$i_tmp/copy" && HOME=$i_tmp/copy/home act workflow_dispatch -n --pull=false -W ".github/workflows/$i_name" \
      -P bana-none=x -P ubuntu-latest= -P ubuntu-22.04= -P ubuntu-20.04= -P ubuntu-18.04= \
      ${i_varfile[@]+"${i_varfile[@]}"} "$@" 2>&1
  )
}

# A job's labels, in order: act names each one it has no image for.
add_labels() { # JOB [--matrix K:V]...
  # shellcheck disable=SC2016 # act's backquotes
  add_act -j "$@" | sed -n 's/.*Skipping unsupported platform -- Try running with `-P \(.*\)=\.\.\.`.*/\1/p' |
    awk '!s[$0]++' | tr '\n' ' ' | sed 's/ $//'
}

# act -v's matrix ([map[k:v k2:v2] map[...]]) as K:V<TAB>K:V lines, only KEYS (those runs-on
# reads), each set once; ? when it does not read. A value that is a map or a list stays as act
# prints it (map[os:macos-14 target:x]): --matrix K:V takes it so.
add_entries() { # KEYS
  awk -v want=" $1" '
    function flush() {
      if (k != "" && (want == " " || index(want, " " k " "))) out = out (out == "" ? "" : "\t") k ":" v
      k = ""
    }
    function word(t) {
      if (t ~ /^[A-Za-z_][A-Za-z0-9_-]*:/) { flush(); c = index(t, ":"); k = substr(t, 1, c - 1); v = substr(t, c + 1) }
      else if (k != "") v = v " " t
      else if (t != "") bad = 1
    }
    {
      s = $0; n = length(s); d = 0
      for (i = 1; i <= n && !bad; i++) {
        ch = substr(s, i, 1)
        if (d == 0) {
          if (substr(s, i, 4) == "map[") { d = 1; i += 3; t = ""; out = ""; k = "" }
          else if (ch != " ") bad = 1
        } else if (ch == "]" && d == 1) {
          word(t); flush(); d = 0
          if (out != "" && !(out in seen)) { seen[out]; lines[++L] = out }
        } else if (ch == " " && d == 1) { word(t); t = "" }
        else { if (ch == "[") d++; else if (ch == "]") d--; t = t ch }
      }
      if (d) bad = 1
    }
    END { if (bad) print "?"; else for (i = 1; i <= L; i++) print lines[i] }'
}

add_row() { printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "${4:--}" "$5" >>"$i_tmp/rows"; } # JOB ENTRY KIND LABELS INFO

# The prefix: bana.conf's, else X when the labels use one X-linux or X-macos, else the default.
add_prefix() {
  i_prefix_read=$(awk -F'\t' '{ n = split($4, a, " ")
      for (i = 1; i <= n; i++) { l = tolower(a[i])
        if (l ~ /^[a-z0-9][a-z0-9-]*-(linux|macos)$/) { sub(/-(linux|macos)$/, "", l); if (!(l in s)) { s[l]; c++; p = l } } } }
    END { if (c == 1) print p }' "$i_tmp/rows")
  i_prefix=$(conf_lookup prefix 2>/dev/null) || i_prefix=${i_prefix_read:-$prefix}
}

# Where act.platform puts LABEL (lowercase): VALUE<TAB>HOW. HOW is set (bana.conf or built
# in), new (to propose), unknown (not asked: skip, and --check fails), never (not a key:
# act would map every job that has it) or eq (a label with '=', which act cannot map).
add_class() { # LABEL
  local l=$1 v
  if v=$(awk -F'\t' -v l="$l" '$1 == l { print $2; f = 1; exit } END { exit !f }' "$i_tmp/table"); then
    printf '%s\tset\n' "$v"
    return
  fi
  if v=$(awk -F'\t' -v l="$l" '$1 == l { print $2; f = 1; exit } END { exit !f }' "$i_tmp/answers" 2>/dev/null); then
    printf '%s\t%s\n' "$v" "$([[ $v == "skip unknown label" ]] && echo unknown || echo new)"
    return
  fi
  case $l in
  self-hosted | x64 | arm64 | x86_64 | amd64 | aarch64 | arm | linux-x64 | linux-arm64 | linux-arm | osx-x64 | osx-arm64) printf -- '-\tnever\n' ;;
  *=*) printf -- '-\teq\n' ;;
  linux | ubuntu-*) printf 'linux\tnew\n' ;;
  macos | macos-*) printf 'mac\tnew\n' ;;
  windows | windows-*) printf 'skip no Windows under bana\tnew\n' ;;
  *) printf -- '-\task\n' ;;
  esac
}

# A label's place, asked on a terminal (the default comes from its name), else skipped.
add_answer() { # LABEL JOBS
  local d a
  case $1 in *ubuntu* | *linux* | *debian*) d=linux ;; *mac* | *osx* | *darwin*) d=mac ;; *) d=skip ;; esac
  if [[ -z $i_ask ]]; then printf '%s\tskip unknown label\n' "$1" >>"$i_tmp/answers"; return; fi
  while :; do
    read -r -p "Label $1 (jobs $2): [l]inux / [m]ac / s[y]stemd / [s]kip / an image [$d] " a || a=
    case ${a:-$d} in
    l | linux) a=linux ;;
    m | mac) a=mac ;;
    y | systemd) a=systemd ;;
    s | skip) a=skip ;;
    */* | *:*) [[ $a =~ ^[A-Za-z0-9._/:@-]+$ ]] || continue ;;
    *) continue ;;
    esac
    printf '%s\t%s\n' "$1" "$a" >>"$i_tmp/answers"
    return
  done
}

# Each row's place here and in a pool, by act's rule: the first label with a place.
add_sort() {
  local job entry kind labels info l v how here cls mac reason un pool asks
  : >"$i_tmp/answers"
  # Labels bana does not know, with the jobs that reach them: asked once each.
  while IFS=$'\t' read -r job entry kind labels info; do
    [[ $kind == run && $labels != - ]] || continue
    for l in $labels; do
      l=$(lower <<<"$l")
      IFS=$'\t' read -r v how <<<"$(add_class "$l")"
      case $how in never | eq) continue ;; ask) printf '%s\t%s\n' "$l" "$job" ;; esac
      case $v in skip | "skip "* | mac) ;; *) break ;; esac
    done
  done <"$i_tmp/rows" | awk -F'\t' '!($1 in j) { o[++n] = $1 } index(" " j[$1] " ", " " $2 " ") == 0 { j[$1] = j[$1] (j[$1] == "" ? "" : " ") $2 }
    END { for (i = 1; i <= n; i++) printf "%s\t%s\n", o[i], j[o[i]] }' >"$i_tmp/asks"
  while IFS=$'\t' read -r l asks <&3; do add_answer "$l" "$asks"; done 3<"$i_tmp/asks"

  : >"$i_tmp/sorted"
  : >"$i_tmp/keys"
  while IFS=$'\t' read -r job entry kind labels info; do
    here='' cls='' mac='' reason='' un=0
    case $kind in
    uses) here="calls ${info}: its jobs are not listed" cls=uses ;;
    container) here="its container $info" cls=container ;;
    run)
      [[ $labels != - ]] || labels=
      for l in $labels; do
        l=$(lower <<<"$l")
        IFS=$'\t' read -r v how <<<"$(add_class "$l")"
        case $how in never | eq) continue ;; new | unknown) printf '%s\t%s\t%s\n' "$l" "$v" "$job" >>"$i_tmp/keys" ;; esac
        [[ $how != unknown ]] || un=1
        case $v in
        skip | "skip "*) reason=${reason:-${v#skip}} reason=${reason:- skipped} ;;
        mac) if [[ $os == Darwin ]]; then here="this Mac (host mode)" cls=mac; break; fi; mac=1 ;;
        linux) here="Linux container $(conf act.image catthehacker/ubuntu:act-24.04)" cls=linux; break ;;
        systemd) here="a systemd container ($(conf act.image catthehacker/ubuntu:act-24.04), host mode)" cls=systemd; break ;;
        *) here="Linux container $v" cls=image; break ;;
        esac
      done
      if [[ -z $here ]]; then
        cls=none
        if [[ -n $mac ]]; then here="a Mac's job, not run on $os" cls=mac
        elif [[ -z $labels && $info == needs ]]; then here="decided at run time (its runs-on or matrix reads needs.)" labels=-
        elif [[ -z $labels ]]; then here="not run: act gives it no labels" un=1 labels=-
        elif [[ -n $reason ]]; then here="not run:$reason"
        else here="not run: no label has a place" un=1; fi
      fi
      [[ $info != needs || $here == decided* ]] || here="$here (runs-on reads needs.: decided at run time)"
      ;;
    esac
    pool=-
    [[ $kind != run || $labels == - ]] || pool=$(add_pool "$labels")
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$job" "$entry" "$labels" "$here" "$pool" "$cls" "$un" >>"$i_tmp/sorted"
  done <"$i_tmp/rows"
  # label, value, the jobs
  awk -F'\t' '!($1 in v) { o[++n] = $1; v[$1] = $2 } index(" " j[$1] " ", " " $3 " ") == 0 { j[$1] = j[$1] (j[$1] == "" ? "" : " ") $3 }
    END { for (i = 1; i <= n; i++) printf "%s\t%s\t%s\n", o[i], v[o[i]], j[o[i]] }' "$i_tmp/keys" >"$i_tmp/keys.new"
  i_unmapped=$(awk -F'\t' '$7 == 1 { n++ } END { print n + 0 }' "$i_tmp/sorted")
  # A matrix whose entries go to other places here: act runs all of them on the first one's.
  i_split=$(add_splits | awk 'END { print NR }')
}

# Where GitHub would send a job: bana up's runners (their labels: self-hosted, Linux or macOS,
# X64 or ARM64, <prefix>-linux|macos, linux-<cpu> or osx-<cpu>, the host, bana.conf's labels
# and usb-*), GitHub's own runners, or nowhere yet.
add_pool() { # LABELS
  local l ml='' mm='' self='' gh='' first=''
  for l in $1; do
    l=$(lower <<<"$l")
    first=${first:-$l}
    [[ $l != self-hosted ]] || self=1
    case $l in ubuntu-* | macos-* | windows-*) gh=1 ;; esac
    add_has "$l" linux || ml=${ml:-$l}
    add_has "$l" macos || mm=${mm:-$l}
  done
  if [[ -z $ml || -z $mm ]]; then echo "bana up runners"
  elif [[ -z $self && -n $gh ]]; then echo "GitHub-hosted"
  elif [[ -z $self ]]; then echo "another service's ($first)"
  elif [[ " $(lower <<<"$1") " == *-macos\ * || " $(lower <<<"$1") " == *\ osx-* ]]; then echo "waits: no runner has $mm"
  else echo "waits: no runner has $ml"; fi
}
add_has() { # LABEL linux|macos
  case $1 in
  self-hosted | x64 | arm64 | "$i_prefix-$2" | "$host" | usb-*) return 0 ;;
  "$i_prefix-systemd") [[ $2 == linux ]] ;;
  linux | linux-x64 | linux-arm64) [[ $2 == linux ]] ;;
  macos | osx-x64 | osx-arm64) [[ $2 == macos ]] ;;
  *) [[ ,$(lower <<<"$extra_labels"), == *,$1,* ]] ;;
  esac
}

add_report() {
  local n
  say "bana add: $repo, .github/workflows/$i_name (vars: $i_vars; $i_yq_from)"
  echo "The workflow: $i_why."
  echo "Where each job runs: here (bana ci and the daemon), and in a pool (bana up):"
  awk -F'\t' 'BEGIN { r[0] = "JOB\tRUNS-ON\tHERE\tPOOL" }
    { r[NR] = ($2 == "-" ? $1 : $1 " " $2) "\t" ($3 == "-" ? "" : $3) "\t" $4 "\t" ($5 == "-" ? "" : $5) }
    END {
      for (i = 0; i <= NR; i++) { split(r[i], c, "\t"); for (k = 1; k <= 3; k++) if (length(c[k]) > w[k]) w[k] = length(c[k]) }
      for (i = 0; i <= NR; i++) { split(r[i], c, "\t"); line = sprintf("  %-" w[1] "s  %-" w[2] "s  %-" w[3] "s  %s", c[1], c[2], c[3], c[4]); sub(/ +$/, "", line); print line }
    }' "$i_tmp/sorted"
  n=$(awk -F'\t' '$4 ~ /^(not run|a Mac.s job, not run)/ { n++ } END { print n + 0 }' "$i_tmp/sorted")
  if ((n)); then
    echo
    echo "Not run under bana here: $n jobs ($(awk -F'\t' '$4 ~ /^(not run|a Mac.s job, not run)/ { printf "%s%s", s, ($2 == "-" ? $1 : $1 " " $2); s = ", " }' "$i_tmp/sorted"))."
    echo "The daemon still posts a green build for a push without them, with \"not run here\" in its description."
  fi
}

# ---- the workflow changes ----------------------------------------------------------------

# An edit script: N<TAB>r<TAB>TEXT replaces line N, N<TAB>a<TAB>TEXT adds a line after it.
add_apply() { # SCRIPT FILE
  awk 'NR == FNR { n = $0; sub(/\t.*/, "", n); t = $0; sub(/^[^\t]*\t[^\t]*\t/, "", t)
      if (index($0, "\tr\t")) r[n] = t; else a[n] = a[n] t "\n"; next }
    { print ((FNR in r) ? r[FNR] : $0) } (FNR in a) { printf "%s", a[FNR] }' "$1" "$2"
}
add_line() { sed -n "${1}p" "$i_wf"; } # N
add_indent() { sed -n "${1}p" "$i_wf" | sed 's/[^ ].*//'; } # N: its leading spaces
# The indentation of a job's keys (the first line after its own that says something).
add_job_indent() { # LINE
  awk -v n="$1" 'NR > n && $0 !~ /^[ \t]*(#.*)?$/ { sub(/[^ ].*/, ""); print; exit }' "$i_wf"
}
add_edit() { # N r|a TEXT: a line of the edit being made (a line replaced once only)
  if [[ $2 == r ]] && awk -F'\t' -v n="$1" '$1 == n && $2 == "r" { f = 1 } END { exit !f }' "$i_tmp/edits" "$i_tmp/edit"; then
    return 0
  fi
  printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$i_tmp/edit"
}

# Keeps the edit made (in $i_tmp/edit) when the workflow still reads, and act still lists
# every job; else it becomes a note.
add_keep() { # WHAT
  local try=$i_tmp/check/.github/workflows/$i_name
  [[ -s $i_tmp/edit ]] || return 0
  cat "$i_tmp/edits" "$i_tmp/edit" >"$i_tmp/try"
  add_apply "$i_tmp/try" "$i_wf" >"$try"
  if add_q . <"$try" >/dev/null 2>&1 && [[ $(add_list) == "$i_listed" ]]; then
    cat "$i_tmp/edit" >>"$i_tmp/edits"
  else
    printf '%s\tnot proposed, since the workflow did not read right with it: %s\n' "$(cut -f1 "$i_tmp/edit" | head -1)" "$1" >>"$i_tmp/notes"
  fi
  : >"$i_tmp/edit"
}
add_list() { (cd "$i_tmp/check" || exit; HOME=$i_tmp/copy/home act -l -W ".github/workflows/$i_name" 2>/dev/null | grep -c .) || true; }

add_edits() {
  local i n t json p=$i_prefix line gate cls
  local re_on=$'^(["\']?on["\']?:[[:space:]]*)([A-Za-z_]+)(.*)$'
  local re_if=$'^([[:space:]]*(-[[:space:]]+)?if:[[:space:]]*)([^"\'>|{[!&*$#[:space:]].*[^[:space:]])[[:space:]]*$'
  local re_after='(always|failure|cancelled)\(\)'
  local re_ifx=$'^([[:space:]]*(-[[:space:]]+)?if:[[:space:]]*)\\$\\{\\{[[:space:]]*(.*[^[:space:]])[[:space:]]*\\}\\}[[:space:]]*$'
  p=$(printf '%s' "$p" | tr 'a-z-' 'A-Z_')
  : >"$i_tmp/edits"
  : >"$i_tmp/edit"
  # (a) workflow_dispatch, which bana ci and the daemon run the workflow as.
  if [[ $i_dispatch != true ]]; then
    t=$(add_line "$i_onkey")
    case $i_ontag in
    '!!map') ((i_online > i_onkey)) && add_edit "$i_onkey" a "$(add_indent "$i_online")workflow_dispatch:" ;;
    '!!str') [[ $t =~ $re_on ]] &&
      add_edit "$i_onkey" r "${BASH_REMATCH[1]}[${BASH_REMATCH[2]}, workflow_dispatch]${BASH_REMATCH[3]}" ;;
    '!!seq')
      if ((i_online > i_onkey)); then add_edit "$i_onkey" a "$(sed -n "${i_online}p" "$i_wf" | sed 's/-.*//')- workflow_dispatch"
      elif [[ $t == *']'* ]]; then add_edit "$i_onkey" r "${t%%]*}, workflow_dispatch]${t#*]}"; fi ;;
    esac
    [[ -s $i_tmp/edit ]] || printf '%s\tno workflow_dispatch trigger: add one by hand (bana ci and the daemon run the workflow as one)\n' "$i_onkey" >>"$i_tmp/notes"
    add_keep "workflow_dispatch"
  fi
  # (b) a runner.environment gate for self-hosted runners: act's jobs pass it too.
  while IFS=: read -r n t; do
    [[ $t =~ $re_if ]] || continue
    line=${BASH_REMATCH[1]} t=${BASH_REMATCH[3]}
    [[ $t != *'#'* && ( $t == *"runner.environment == 'self-hosted'"* || $t == *'runner.environment == "self-hosted"'* ) ]] || continue
    add_edit "$n" r "$line($t) || env.ACT == 'true'"
  done < <(grep -n 'runner\.environment' "$i_wf" | grep -v 'env\.ACT' || true)
  add_keep "env.ACT in runner.environment gates"
  # (c) the shell's [[ $RUNNER_ENVIRONMENT == self-hosted ]], likewise.
  # shellcheck disable=SC2016 # the workflow's shell expands them
  while IFS=: read -r n t; do
    add_edit "$n" r "$(awk -v f='[[ $RUNNER_ENVIRONMENT == self-hosted ]]' -v r='[[ $RUNNER_ENVIRONMENT == self-hosted || -n ${ACT:-} ]]' \
      '{ i = index($0, f); print substr($0, 1, i - 1) r substr($0, i + length(f)) }' <<<"$t")"
  done < <(grep -nF '[[ $RUNNER_ENVIRONMENT == self-hosted ]]' "$i_wf" || true)
  add_keep "ACT in RUNNER_ENVIRONMENT checks"
  # (d) pushes and nightlies on GitHub only while vars.<P>_CI_AUTO is not 'false' (with the
  # variable unset, as before): set it to false once the daemon builds the pushes. Every
  # other event (pull requests, workflow_dispatch, releases) runs as before.
  i_gate=
  if [[ $i_pushsched == true ]] && ! grep -q '_CI_AUTO' "$i_wf"; then
    gate="(github.event_name != 'push' && github.event_name != 'schedule') || vars.${p}_CI_AUTO != 'false'"
    for ((i = 0; i < ${#j_id[@]}; i++)); do
      # A root job; or one that runs after skipped needs too (always(), failure(), cancelled()).
      [[ ${j_needs[i]} == false || ${j_if[i]} =~ $re_after ]] || continue
      if [[ ${j_iftag[i]} == '!!null' ]]; then
        add_edit "${j_line[i]}" a "$(add_job_indent "${j_line[i]}")if: $gate"
      else
        n=${j_ifline[i]} t=$(add_line "${j_ifline[i]}")
        if [[ $t =~ $re_if && -z ${BASH_REMATCH[2]} && ${BASH_REMATCH[3]} != *'#'* ]]; then
          add_edit "$n" r "${BASH_REMATCH[1]}(${BASH_REMATCH[3]}) && ($gate)"
        elif [[ $t =~ $re_ifx && -z ${BASH_REMATCH[2]} && ${BASH_REMATCH[3]} != *'#'* && ${BASH_REMATCH[3]} != *'}}'* ]]; then
          add_edit "$n" r "${BASH_REMATCH[1]}\${{ (${BASH_REMATCH[3]}) && ($gate) }}"
        else
          printf '%s\tadd the push gate to its if: by hand (if: ... && (%s))\n' "$n" "$gate" >>"$i_tmp/notes"
        fi
      fi
    done
    [[ ! -s $i_tmp/edit ]] || i_gate=${p}_CI_AUTO
    add_keep "the ${p}_CI_AUTO push gate"
    grep -q "_CI_AUTO" "$i_tmp/edits" || i_gate=
  fi
  # (e) a literal runs-on of a Linux or macOS job, overridable by vars.<P>_RUNNER_LINUX|MACOS
  # (JSON label lists): the variable moves the jobs to a pool; unset, nothing changes.
  for ((i = 0; i < ${#j_id[@]}; i++)); do
    # shellcheck disable=SC2016 # the workflow's
    [[ ${j_rokey[i]} == "${j_roline[i]}" && ${j_ro[i]} != *'${{'* && ${j_rotag[i]} =~ ^!!(str|seq)$ ]] || continue
    cls=$(awk -F'\t' -v j="${j_id[i]}" '$1 == j && !($6 in c) { c[$6]; n++; k = $6 } END { if (n == 1) print k }' "$i_tmp/sorted")
    case $cls in linux) t=LINUX ;; mac) t=MACOS ;; *) continue ;; esac
    json=${j_ro[i]}
    [[ ${j_rotag[i]} == '!!seq' ]] || json="[$json]"
    [[ $json != *"'"* ]] || continue
    line=$(add_line "${j_rokey[i]}")
    add_edit "${j_rokey[i]}" r "${line%%runs-on:*}runs-on: \${{ fromJSON(vars.${p}_RUNNER_$t || '$json') }}"
  done
  add_keep "vars.${p}_RUNNER_LINUX and _MACOS runs-on"
  if [[ -s $i_tmp/edits ]]; then
    mkdir -p "$i_tmp/d/a/.github/workflows" "$i_tmp/d/b/.github/workflows"
    cp "$i_wf" "$i_tmp/d/a/.github/workflows/$i_name"
    add_apply "$i_tmp/edits" "$i_wf" >"$i_tmp/d/b/.github/workflows/$i_name"
    (cd "$i_tmp/d" && git -c core.quotepath=false diff --no-index --no-color --no-ext-diff --no-prefix \
      "a/.github/workflows/$i_name" "b/.github/workflows/$i_name") >"$i_tmp/patch" || true
  else
    : >"$i_tmp/patch"
  fi
}

# ---- notes ------------------------------------------------------------------------------------

# What bana reports and leaves to you, as FILE:LINE: JOB: TEXT.
add_notes() {
  local n t job e cpu i l v
  e=" $(cut -f1 "$i_tmp/edits" | tr '\n' ' ') "
  cpu=$(cpu)
  # shellcheck disable=SC2016 # the workflow's
  {
    cat "$i_tmp/notes"
    { grep -n 'RUNNER_ENVIRONMENT' "$i_wf" | grep -v '\${ACT' || true; } | while IFS=: read -r n t; do
      [[ $e == *" $n "* ]] || printf '%s\t%s\n' "$n" '$RUNNER_ENVIRONMENT is empty under act (neither self-hosted nor github-hosted); ${ACT:-} is true there'
    done
    { grep -n 'runner\.environment' "$i_wf" | grep -v 'env\.ACT' || true; } | while IFS=: read -r n t; do
      [[ $e == *" $n "* ]] || printf '%s\tact never sets runner.environment, so this gate skips under bana: add || env.ACT == '"'true'"'\n' "$n"
    done
    # A job on a systemd label runs where systemd is; any other is told to move there.
    { grep -nE 'systemctl --user|loginctl' "$i_wf" || true; } | while IFS=: read -r n t; do
      job=$(add_job_at "$n")
      [[ -z $job || -z $(awk -F'\t' -v j="$job" '$1 == j && $6 == "systemd"' "$i_tmp/sorted") ]] || continue
      printf '%s\tsystemctl --user and loginctl need systemd: give the job runs-on %s (bana ci and bana split run it in a systemd container)\n' "$n" "$i_prefix-systemd"
    done
    add_sysneeds
    { grep -nE '^[[:space:]]*ref:' "$i_wf" || true; } | while IFS=: read -r n t; do
      printf '%s\ta checkout ref: makes act clone from GitHub rather than build the pushed commit; remove it\n' "$n"
    done
    awk '/uses:[ \t]*pnpm\/action-setup/ { if (p) print p; p = NR; d = ""; ind = match($0, /[^ -]/); next }
      p && /^[ \t]*-/ && match($0, /[^ ]/) <= ind - 2 { print p; p = 0 }
      p && /dest:.*runner\.temp/ { p = 0 }
      END { if (p) print p }' "$i_wf" | while read -r n; do
      printf '%s\t%s\n' "$n" 'pnpm/action-setup without dest: ${{ runner.temp }}/setup-pnpm: runners sharing a machine race on ~/setup-pnpm'
    done
    for ((i = 0; i < ${#j_id[@]}; i++)); do
      [[ ${j_svc[i]} != true ]] ||
        printf '%s\tservices: act 0.2.89 panicked on them in a dry run: bana ci -j %s once, to see\n' "${j_line[i]}" "${j_id[i]}"
    done
    # SPLIT matrices, CPUs, macOS versions and labels with '=', at the runs-on line.
    add_splits | while IFS= read -r job; do
        printf '%s\tSPLIT: its entries go to different runners, and act 0.2.89 runs them all on the first one'"'"'s: bana ci -j %s --%s, and keep it out of daemon.tags until then\n' \
          "$(add_roline "$job")" "$job" "$(head -1 "$i_tmp/entries.$job" | tr '\t' '\n' |
            awk -v q="'" '/[^A-Za-z0-9_.:\/=@+-]/ { $0 = q $0 q } { printf " --matrix %s", $0 }')"
      done
    while IFS=$'\t' read -r job t l v _ _ _; do
      for i in $l; do
        i=$(lower <<<"$i")
        case $i in
        linux-x64 | linux-arm64 | ubuntu-*-arm)
          [[ $v == "Linux container"* && ${i#linux-} != "$cpu" && ($i != ubuntu-*-arm || $cpu != arm64) ]] || continue
          add_job_lines "$job" | grep -q 'uname -m' && continue # it checks the CPU already
          printf '%s\t%s asks for %s: act runs every Linux container at one CPU (%s here; bana ci --x64 for x64 on Apple silicon), so check the CPU in a step, as bana'"'"'s examples/example/ci.yml package job does\n' \
            "$(add_roline "$job")" "$([[ $t == - ]] && echo "it" || echo "$t")" "$i" "$cpu" ;;
        macos-latest) ;;
        macos-*) printf '%s\t%s runs on this Mac as it is (its macOS version, not %s)\n' "$(add_roline "$job")" "$i" "${i#macos-}" ;;
        *=*) printf '%s\tact cannot map %s (a label with '"'='"'); the job needs another label with a place\n' "$(add_roline "$job")" "$i" ;;
        esac
      done
    done <"$i_tmp/sorted"
  } | sort -n -k1,1 | awk -F'\t' '!s[$0]++' >"$i_tmp/notes.all"
  [[ -s $i_tmp/notes.all ]] || return 0
  echo
  echo "Notes (bana add leaves these to you):"
  local seen='|'
  while IFS=$'\t' read -r n t; do
    job=$(add_job_at "$n")
    # Once a job (systemctl on three lines is one note).
    [[ $seen != *"|$job:$t|"* ]] || continue
    seen+="$job:$t|"
    if ((n)); then echo "  $i_name:$n: ${job:+$job: }$t"; else echo "  $i_name: $t"; fi
  done <"$i_tmp/notes.all"
}
# The jobs that need a job that runs in a systemd container (all the way down): act runs a job
# with the jobs it needs, and bana runs a systemd job in a container of its own, so they do not
# run here. And why systemd jobs do not run here at all, when they do not (lib/systemd.sh).
add_sysneeds() {
  local sys g i
  sys=" $(awk -F'\t' '$6 == "systemd" && !($1 in s) { s[$1]; printf "%s ", $1 }' "$i_tmp/sorted")"
  [[ -n ${sys// /} ]] || return 0
  # shellcheck disable=SC2016 # yq's
  add_q '.jobs | to_entries | .[] | [.key, ([.value.needs] | flatten | map(select(. != null)) | join(" "))] | join("\t")' \
    <"$i_wf" 2>/dev/null | awk -F'\t' -v sys="$sys" '
    { id[++n] = $1; need[$1] = $2 }
    function first(j,   k, m, a, x) {
      m = split(need[j], a, " ")
      for (k = 1; k <= m; k++) {
        if (a[k] in done) continue
        done[a[k]]
        if (index(sys, " " a[k] " ")) return a[k]
        if ((x = first(a[k])) != "") return x
      }
      return ""
    }
    END {
      for (i = 1; i <= n; i++) {
        if (index(sys, " " id[i] " ")) continue
        split("", done); x = first(id[i])
        if (x != "") printf "%s\t%s\n", id[i], x
      }
    }' | while IFS=$'\t' read -r i g; do
    printf '%s\tit needs %s, a systemd job: bana does not run it (act runs a job with the jobs it needs, and bana runs %s in a container of its own): drop the need, or accept that\n' \
      "$(add_line_of "$i")" "$g" "$g"
  done
  # shellcheck disable=SC2034 # sd_why's settings (lib/systemd.sh)
  g=$(sysd_docker=(docker) sd_bin=''; sd_why 2>/dev/null) || g=
  [[ -z $g || $g == "no act for its container" ]] ||
    printf '0\tjobs that need systemd (%s) do not run here: %s\n' "$(printf '%s' "${sys# }" | sed 's/ $//')" "$g"
}
add_line_of() { # JOB: its line
  local i
  for ((i = 0; i < ${#j_id[@]}; i++)); do [[ ${j_id[i]} != "$1" ]] || { echo "${j_line[i]}"; return; }; done
  echo 0
}
# The jobs whose matrix entries go to more than one place here (the same place under other
# labels is no split: act's shared runs-on puts them there anyway).
add_splits() {
  awk -F'\t' '$2 != "-" && !(($1, $4) in s) { s[$1, $4]; if (++c[$1] == 2) print $1 }' "$i_tmp/sorted"
}
add_job_lines() { # JOB: its lines of the workflow
  local i b='' e=''
  for ((i = 0; i < ${#j_id[@]}; i++)); do
    if [[ -n $b ]]; then e=$((j_line[i] - 1)); break; fi
    [[ ${j_id[i]} != "$1" ]] || b=${j_line[i]}
  done
  [[ -z $b ]] || sed -n "$b,${e:-\$}p" "$i_wf"
}
add_roline() { # JOB: its runs-on line
  local i
  for ((i = 0; i < ${#j_id[@]}; i++)); do [[ ${j_id[i]} != "$1" ]] || { echo "${j_rokey[i]}"; return; }; done
}
add_job_at() { # LINE: the job it is in
  local i j=''
  for ((i = 0; i < ${#j_id[@]}; i++)); do ((j_line[i] > $1)) || j=${j_id[i]}; done
  echo "$j"
}

# ---- bana.conf -----------------------------------------------------------------------------------

# The proposal: a new .github/bana.conf, or the keys bana.conf lacks, appended.
add_conf() {
  local f=${conf_file:-$i_root/.github/bana.conf} k v want l jobs
  i_conf=$f
  : >"$i_tmp/conf"
  if [[ -z $conf_file ]]; then
    {
      echo "# bana's settings for $repo (bana add, $(date +%Y-%m-%d)). bana's README has every key;"
      echo "# its examples/example/bana.conf is a project's, for bana up's machines, plan.* and daemon.*."
      echo "repo = $repo"
      echo "prefix = $i_prefix"
      echo "workflow = $i_name"
      echo "tiers =${i_tiers:+ $i_tiers}"
      [[ -z $i_tier_input ]] || echo "tier_input = $i_tier_input"
    } >"$i_tmp/conf"
  else
    for k in prefix workflow tiers tier_input; do
      case $k in
      prefix) want=${i_prefix_read:-$i_prefix} ;;
      workflow) want=$i_name ;;
      tiers) want=$i_tiers ;;
      tier_input) want=$i_tier_input; [[ -n $want ]] || continue ;;
      esac
      if v=$(conf_lookup "$k"); then
        [[ $(words "$v" | tr -s ' ' | sed 's/^ //; s/ $//') == "$want" ]] ||
          echo "Note: bana.conf has $k = $v, and the workflow says $k =${want:+ $want}" >>"$i_tmp/differs"
      elif [[ $want != "$(add_default "$k")" ]]; then
        echo "$k =${want:+ $want}" >>"$i_tmp/conf"
      fi
    done
  fi
  while IFS=$'\t' read -r l v jobs; do
    if [[ $v == "skip unknown label" ]]; then
      echo "# $l (jobs $jobs): a label bana does not know: linux, mac, systemd, skip or an image"
    else
      echo "# $l: jobs $jobs"
    fi
    echo "act.platform.$l = $v"
  done <"$i_tmp/keys.new" >>"$i_tmp/conf"
  if [[ -n $conf_file && -s $i_tmp/conf ]]; then
    { echo; echo "# bana add $(date +%Y-%m-%d)"; cat "$i_tmp/conf"; } >"$i_tmp/conf.block"
    mv "$i_tmp/conf.block" "$i_tmp/conf"
  fi
  echo
  if [[ -s $i_tmp/conf ]]; then
    echo "Proposed $(add_rel "$f")$([[ -n $conf_file ]] && echo ", appended" || echo ", a new file"):"
    sed 's/^/  | /' "$i_tmp/conf"
  else
    echo "$(add_rel "$f"): nothing to add"
  fi
  [[ ! -s $i_tmp/differs ]] || cat "$i_tmp/differs"
  echo
  if [[ -s $i_tmp/patch ]]; then
    echo "Proposed changes to .github/workflows/$i_name (bana add --diff prints them alone):"
    sed 's/^/  /' "$i_tmp/patch"
  else
    echo ".github/workflows/$i_name: no changes to propose"
  fi
}
add_default() { # KEY: what bana uses without it
  case $1 in
  prefix) printf '%s' "${repo##*/}" | lower | tr -c 'a-z0-9-\n' '-' ;;
  workflow) echo ci.yml ;;
  tiers) echo "quick nightly release" ;;
  tier_input) echo tier ;;
  esac
}
add_rel() { echo "${1#"$i_root"/}"; } # PATH: relative to the checkout

add_next() {
  local t=${i_tiers%% *} l
  echo
  echo "Next:"
  printf '  %-22s  %s\n' "bana ci -n" "what would run here" "bana ci${t:+ $t}" "the workflow${t:+ at its first tier}, here"
  l=$(awk -F'\t' '$5 ~ /^waits: no runner has / { l = $5; sub(/.* has /, "", l); if (l !~ /=/ && !s[l]++) printf " --label %s", l }' "$i_tmp/sorted")
  [[ ! -s $i_tmp/conf && ! -s $i_tmp/patch ]] ||
    printf '  %-22s  %s\n' "git commit, git push" "bana.conf and the workflow changes: the daemon builds pushed commits, with theirs"
  [[ -z $l ]] || printf '  %-22s  %s\n' "bana up$l" "a pool of runners, for the jobs that wait for one"
  printf '  %-22s  %s\n' "bana split plan" "or builds and releases on a public repository's GitHub Actions, the code private"
  [[ -z $i_gate ]] || printf '  %-22s  %s\n' "gh variable set $i_gate --body false" "" "" "once the daemon builds the pushes: GitHub then runs no push or nightly"
}

# ---- writing -------------------------------------------------------------------------------------

add_write() {
  local a w=()
  [[ -s $i_tmp/conf || -s $i_tmp/patch ]] || return 0
  if [[ -z $i_ask ]]; then
    echo
    echo "Nothing written: rerun on a terminal to write."
    return 0
  fi
  echo
  if [[ -s $i_tmp/conf ]]; then
    read -r -p "Write $(add_rel "$i_conf")? [y/N] " a || a=
    if [[ $a == [yY]* ]]; then
      mkdir -p "$(dirname "$i_conf")"
      cat "$i_tmp/conf" >>"$i_conf"
      say "Wrote $(add_rel "$i_conf")"
      w+=("$(add_rel "$i_conf")")
    fi
  fi
  if [[ -s $i_tmp/patch ]]; then
    read -r -p "Apply these changes to .github/workflows/$i_name? [y/N] " a || a=
    if [[ $a != [yY]* ]]; then
      :
    elif [[ -n $(git -C "$i_root" status --porcelain -- ".github/workflows/$i_name") ]]; then
      warn ".github/workflows/$i_name has changes not committed: commit them, then run bana add again (or bana add --diff | git apply)"
    elif (cd "$i_root" && git apply "$i_tmp/patch"); then
      say "Changed .github/workflows/$i_name (not committed: git diff shows it)"
      w+=(".github/workflows/$i_name")
    fi
  fi
  ((${#w[@]})) || return 0
  echo "Next: git add ${w[*]} && git commit, and push (the daemon builds pushed commits)"
}

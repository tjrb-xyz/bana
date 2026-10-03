# shellcheck shell=bash
# shellcheck disable=SC2154 # the callers set sysd_docker, sysd_act and the sd_* settings (below)
# Jobs that need systemd (systemctl --user, loginctl), for bana ci and for bana split's public
# runner: a job whose runs-on label has the place systemd (<prefix>-systemd, or
# act.platform.<label> = systemd) runs in a container of act.image whose PID 1 is systemd, in
# act's host mode inside it, as a user (runner) with sudo. The container is unprivileged:
# Docker's default capabilities, seccomp and AppArmor; a cgroup namespace of its own, writable
# in its own subtree only (Docker 28 or later, not rootless); no device, no Docker socket, no
# namespace of the host's; every bind mount read-only. Its root cannot see the processes of the
# machine outside (on bana split's runner: Runner.Worker, which holds the deploy key).
#
# A run (lib/act.sh's act_both; on the runner, split_runner_build):
#   sd_probe   a dry run of the workflow, with the systemd labels on an image no one has: each
#              job entry act would start (id, act's name, matrix), and which run in a systemd
#              container. None: the run is as before, act alone.
#   sd_next    bana's line for each of those before act starts: next, or not run here (why)
#   then act, with the systemd labels on no place (act skips their jobs), and then
#   sd_after   a job that needs a systemd job is not run here (act runs a job with the jobs it
#              needs); each systemd job runs in a fresh container of its own, one after the
#              other (on GitHub each job has a machine of its own): its needs run again inside,
#              in act's host mode too, and only its own lines come out (sd_only). Its uploads
#              come back to the artifact server's directory here (sd_copy_out).
# Every line act inside says goes to sd_log, for when it fails.
#
# Sourced by lib/act.sh. bana split copies the part between the two markers below into
# bana.yml, as it is: that part uses nothing of bin/bana's, and must lint as bana.yml does.

# ---- systemd: jobs that need systemd, each in a container whose PID 1 is systemd ---------
# Its callers set:
#   sysd_docker   the docker command, an array: (docker); on the runner, under env -i
#   sysd_act      the act of the run, for the probe and act -l: (act)
#   sd_args       act's arguments for the run (an array)
#   sd_labels     the systemd labels, lowercase, each between spaces: " wid-systemd "
#   sd_image      the containers' image                  sd_arch   linux/arm64 or linux/amd64
#   sd_net        their network (none: Docker's bridge)  sd_label  the daemon's prefix, or none
#   sd_bin        the Linux act each container runs      sd_cache  act's action cache here
#   sd_root       the checkout                           sd_wf     the workflow
#   sd_probe_file the probe's entries (sd_probe)         sd_log    what act said in each
#   sd_art        where the uploads go here (act's artifact server's directory)
#   sd_name       the containers' name                   sd_life   1: sd_lifeline
# sd_stop, once set (Ctrl-C, a cancel), starts no other container; sd_live is the container
# whose act runs now (sd_int), sd_child that act's local pid (the docker CLI's subshell).

# The container: systemd as PID 1 (container=docker tells it where it is, SIGRTMIN+3 halts it),
# a cgroup namespace of its own, writable in its own subtree, /run and /run/lock in memory.
sd_run_flags=(--cgroupns=private --security-opt writable-cgroups=true --tmpfs /run --tmpfs /run/lock --stop-signal SIGRTMIN+3 -e container=docker --entrypoint /sbin/init)
# What the image would start that no job needs: Docker, ssh, and the timers.
sd_masks=(systemd.mask=docker.service systemd.mask=docker.socket systemd.mask=containerd.service systemd.mask=ssh.socket systemd.mask=ssh.service systemd.mask=apt-daily.timer systemd.mask=apt-daily-upgrade.timer systemd.mask=motd-news.timer systemd.mask=dpkg-db-backup.timer systemd.mask=e2scrub_all.timer systemd.mask=fstrim.timer)

# Why systemd jobs cannot run here, a line (nothing: they can).
sd_why() {
  local v
  v=$("${sysd_docker[@]}" version --format '{{.Server.APIVersion}}' </dev/null 2>/dev/null 9>&-) || v=
  if [[ ! $v =~ ^[0-9]+\.[0-9]+$ ]]; then
    echo "Docker is not running"
  elif ((${v%%.*} < 1 || (${v%%.*} == 1 && 10#${v#*.} < 48))); then
    echo "needs Docker 28 or later (writable cgroups)"
  elif [[ $("${sysd_docker[@]}" info --format '{{json .SecurityOptions}}' </dev/null 2>/dev/null 9>&-) == *rootless* ]]; then
    echo "rootless Docker refuses writable cgroups"
  elif [[ ! -x ${sd_bin:-} ]]; then
    echo "no act for its container"
  fi
}

# PROBE: a dry run of act's run (ACT-ARGUMENT...), every label it maps on act.image (a dry run
# pulls nothing, and on a host label runs a host job's steps) and each systemd label on
# bana-systemd-probe, an image no one has. A line each job entry act would start:
# ID<TAB>NAME<TAB>MATRIX<TAB>SYS, NAME as act's JSON has it ("ci/sd2-1  "), MATRIX the JSON act
# wrote ({"n":1}), SYS 1 when it would run in a systemd container. Fails when act does.
sd_probe() { # PROBE ACT-ARGUMENT...
  local out=$1 i a l seen=' ' p=() args
  shift
  args=("$@")
  for ((i = 0; i < ${#args[@]}; i++)); do
    case ${args[i]} in
    -P | --platform) i=$((i + 1)); a=${args[i]:-} ;;
    -P=* | --platform=*) a=${args[i]#*=} ;;
    -P?*) a=${args[i]#-P} ;;
    *) continue ;;
    esac
    [[ $a == *=* ]] || continue
    l=$(printf '%s' "${a%%=*}" | tr '[:upper:]' '[:lower:]')
    [[ $seen == *" $l "* ]] || seen="$seen$l "
  done
  for l in $seen; do
    if [[ $sd_labels == *" $l "* ]]; then p+=(-P "$l=bana-systemd-probe"); else p+=(-P "$l=$sd_image"); fi
  done
  for l in $sd_labels; do [[ $seen == *" $l "* ]] || p+=(-P "$l=bana-systemd-probe"); done
  if ! "${sysd_act[@]}" "$@" ${p[@]+"${p[@]}"} -n --json --concurrent-jobs 1 </dev/null 2>/dev/null 9>&- >"$out.raw"; then
    rm -f "$out.raw"
    return 1
  fi
  awk -v q='"' '
    function str(k,   i, v) { i = index($0, q k q ":" q); if (!i) return ""; v = substr($0, i + length(k) + 4); return substr(v, 1, index(v, q) - 1) }
    {
      img = str("msg"); if (img !~ /Start image=/) next
      sub(/.*Start image=/, "", img); id = str("jobID")
      i = index($0, q "job" q ":" q); j = index($0, q "," q "jobID" q ":" q)
      m = index($0, q "matrix" q ":"); k = index($0, "," q "msg" q ":" q)
      if (!i || j <= i || !m || k <= m) next
      if (id !~ /^[A-Za-z0-9_.-]+$/) { print "bana: left out, an id bana cannot use: " id | "cat 1>&2"; next }
      nm = substr($0, i + 7, j - i - 7); mx = substr($0, m + 9, k - m - 9)
      if ((id, nm) in seen) next
      seen[id, nm]
      print id "\t" nm "\t" mx "\t" (img == "bana-systemd-probe" ? 1 : 0)
    }' "$out.raw" >"$out"
  rm -f "$out.raw"
}

# Before act starts: bana's line for each systemd job's entry, next, or not run here and why.
sd_next() { # MODE(json|text)
  local e
  sd_gate=$(sd_why)
  while IFS= read -r e; do
    if [[ -z $sd_gate ]]; then sd_say "$1" "$e" "next, in a systemd container"; else sd_say "$1" "$e" "not run here: $sd_gate"; fi
  done < <(awk -F'\t' '$4 == 1' "$sd_probe_file")
}

# bana's line for a job's entry (a line of the probe's), in act's own shape: JSON, with the
# job's name, id and matrix as act wrote them, or [NAME] TEXT.
sd_say() { # MODE ENTRY TEXT
  local id nm mx
  IFS=$'\t' read -r id nm mx _ <<<"$2"
  if [[ $1 == json ]]; then
    printf '{"level":"info","job":"%s","jobID":"%s","matrix":%s,"msg":"bana: %s","time":"%s"}\n' \
      "$nm" "$id" "$mx" "$(printf '%s' "$3" | sed 's/[\\"]/\\&/g' | tr -d '\000-\037')" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  else
    printf '[%s] bana: %s\n' "$(sd_text "$nm")" "$3"
  fi
}
sd_say_id() { # MODE ID TEXT: sd_say for each of ID's entries
  local e
  while IFS= read -r e; do sd_say "$1" "$e" "$3"; done < <(awk -F'\t' -v id="$2" '$1 == id' "$sd_probe_file")
}
# A red line on stderr, as act's own errors: the run is an error, never a silent pass.
sd_red() { printf '\033[31mError: %s\033[0m\n' "$(printf '%s' "$1" | tr -d '\000-\037')" >&2; } # TEXT

# A name as act's JSON has it, as its text does.
sd_text() { printf '%s\n' "$1" | awk "$sd_unjson"' { print unjson($0) }'; } # NAME
# shellcheck disable=SC2016 # awk's
sd_unjson='function unjson(s,   o, i, c, h) {
  o = ""
  while ((i = index(s, "\\")) > 0) {
    o = o substr(s, 1, i - 1); c = substr(s, i + 1, 1)
    if (c == "u") { h = substr(s, i + 2, 4); o = o (h == "003c" ? "<" : (h == "003e" ? ">" : (h == "0026" ? "&" : "?"))); s = substr(s, i + 6) }
    else { o = o (c == "n" || c == "t" || c == "r" ? " " : (c == "b" || c == "f" ? "" : c)); s = substr(s, i + 2) }
  }
  return o s
}'
# ID's entries' names, as act's text has them (trimmed), a line each.
sd_names() { awk -F'\t' -v id="$1" "$sd_unjson"' $1 == id { n = unjson($2); gsub(/^ +| +$/, "", n); print n }' "$sd_probe_file"; } # ID

# The jobs ID needs, all the way down, and ID, as act -l lists them (needs first), a line each.
sd_closure() { # ID
  "${sysd_act[@]}" -l -C "$sd_root" -W "$sd_wf" -j "$1" </dev/null 2>/dev/null 9>&- |
    awk '/^Stage  *Job ID/ { on = 1; next } on && $1 ~ /^[0-9]+$/ { print $2 }' || true
}

# What act's first run (its output in OUT...) said of each job, a line each:
# P ID (it said something of ID), F ID (ID failed), S ID LABEL NAME MATRIX (act skipped it, no
# place for LABEL here). Text names are matched by the probe's names and act -l's.
sd_first() { # MODE OUT...
  local mode=$1 list=/dev/null
  shift
  if [[ $mode != json ]]; then
    list=$sd_probe_file.list
    "${sysd_act[@]}" -l -C "$sd_root" -W "$sd_wf" </dev/null 2>/dev/null 9>&- >"$list" || true
  fi
  awk -v mode="$mode" -v pf="$sd_probe_file" -v lf="$list" -v q='"' "$sd_unjson"'
    function trim(s) { gsub(/^ +| +$/, "", s); return s }
    function str(k,   i, v) {
      i = index($0, q k q ":" q); if (!i) return ""
      v = substr($0, i + length(k) + 4); match(v, /^([^"\\]|\\.)*/); return substr(v, 1, RLENGTH)
    }
    function clean(s) {
      while (s ~ /\r$/) sub(/\r$/, "", s)
      sub(/.*\r/, "", s); gsub(/\033\[[^@-~]*[@-~]/, "", s); gsub(/[\001-\010\013-\037]/, "", s)
      sub(/^\*DRYRUN\* /, "", s); return s
    }
    function skip(m, id, nm, mx,   l) {
      if (m !~ /Skipping unsupported platform -- Try running with `-P [^`]*=\.\.\.`/) return
      l = m; sub(/.*Try running with `-P /, "", l); sub(/=\.\.\.`.*/, "", l)
      print "S\t" id "\t" tolower(l) "\t" nm "\t" mx
    }
    FILENAME == pf { split($0, f, "\t"); byname[trim(unjson(f[2]))] = f[1]; next }
    FILENAME == lf {
      if (!a) { a = index($0, "Job ID"); b = index($0, "Job name"); c = index($0, "Workflow name"); d = index($0, "Workflow file"); if (!(a && b > a && c > b && d > c)) a = 0; next }
      listed[trim(substr($0, c, d - c)) "/" trim(substr($0, b, c - b))] = trim(substr($0, a, b - a)); next
    }
    mode == "json" {
      if (substr($0, 1, 1) != "{" || (id = str("jobID")) == "") next
      m = str("msg"); if (m ~ /^bana: /) next
      print "P\t" id
      if (str("jobResult") == "failure") print "F\t" id
      i = index($0, q "job" q ":" q); j = index($0, q "," q "jobID" q ":" q)
      k = index($0, q "matrix" q ":"); n = index($0, "," q "msg" q ":" q)
      if (i && j > i && k && n > k) skip(m, id, substr($0, i + 7, j - i - 7), substr($0, k + 9, n - k - 9))
      next
    }
    {
      s = clean($0); if (substr(s, 1, 1) != "[" || !(j = index(s, "]"))) next
      nm = trim(substr(s, 2, j - 2)); rest = substr(s, j + 1)
      if (nm in byname) id = byname[nm]
      else if (nm in listed) id = listed[nm]
      else { b2 = nm; sub(/-[0-9]+$/, "", b2); id = (b2 in listed) ? listed[b2] : "" }
      if (id == "" || rest ~ /^ bana: /) next
      print "P\t" id
      if (index(rest, "🏁  Job failed")) print "F\t" id
      if (nm !~ /[\\"]/) skip(rest, id, nm, "{}")
    }' "$sd_probe_file" "$list" "$@"
  [[ $list == /dev/null ]] || rm -f "$list"
}

# After act's first run, whose output is in OUT... (its stdout and stderr, as copied): a job
# that needs a systemd job is not run (act runs a job with the jobs it needs); each systemd job,
# in the probe's order, in a container of its own, but one whose need failed, or was not run
# here, or that act would not have run (its if:), and none once sd_stop is set. A systemd job
# act skipped for its label that the probe missed (an if: on its needs' outputs, which a dry
# run does not have) runs too. Its status: the first failure's.
sd_after() { # MODE OUT...
  local mode=$1 st=0 rc f id e x cl ids printed failed sysskip unplaced started=' '
  shift
  f=$sd_probe_file
  [[ -n ${sd_gate+x} ]] || sd_gate=$(sd_why)
  sd_first "$mode" "$@" >"$f.first"
  printed=" $(awk -F'\t' '$1 == "P" { printf "%s ", $2 }' "$f.first")"
  failed=" $(awk -F'\t' '$1 == "F" { printf "%s ", $2 }' "$f.first")"
  sysskip=" $(awk -F'\t' -v s="$sd_labels" '$1 == "S" && index(s, " " $3 " ") { printf "%s ", $2 }' "$f.first")"
  # Not placed here: skipped for a label, none of whose labels is a systemd one (act names each
  # label of a job it skips: [self-hosted, wid-systemd] says both).
  unplaced=" $(awk -F'\t' -v s="$sd_labels" '$1 == "S" { if (index(s, " " $3 " ")) sys[$2]; else if (!($2 in o)) o[$2] = ++n }
    END { for (i in o) if (!(i in sys)) printf "%s ", i }' "$f.first")"
  # The systemd jobs the probe missed, from act's skips: their entries, and bana's line.
  while IFS= read -r e; do
    printf '%s\n' "$e" >>"$f"
    if [[ -z $sd_gate ]]; then sd_say "$mode" "$e" "next, in a systemd container"; else sd_say "$mode" "$e" "not run here: $sd_gate"; fi
  done < <(awk -F'\t' -v s="$sd_labels" 'FILENAME == ARGV[1] { if ($4 == 1) sys[$1]; next }
    $1 == "S" && index(s, " " $3 " ") && !($2 in sys) && !(($2, $4) in seen) { seen[$2, $4]; print $2 "\t" $4 "\t" $5 "\t1" }' "$f" "$f.first")
  ids=" $(awk -F'\t' '$4 == 1 && !($1 in s) { s[$1]; printf "%s ", $1 }' "$f")"
  # Jobs that need a systemd job: act did not run them, and bana does not.
  e=$(awk -F'\t' -v ids="$ids" '$4 == 0 && !index(ids, " " $1 " ") && !($1 in s) { s[$1]; printf "%s ", $1 }' "$f")
  for id in $e; do
    [[ $printed != *" $id "* ]] || continue
    x=$(sd_first_of "$(sd_closure "$id")" "$id" "$ids")
    [[ -z $x ]] || sd_say_id "$mode" "$id" "not run here: needs $x, a systemd job"
  done
  for id in $ids; do
    [[ -z ${sd_stop:-} ]] || break
    [[ -z $sd_gate ]] || continue
    if [[ $sysskip != *" $id "* ]]; then
      cl=$(sd_closure "$id")
      x=$(sd_first_of "$cl" "$id" "$failed")
      if [[ -n $x ]]; then sd_say_id "$mode" "$id" "not run here: needs $x, which failed"; failed="$failed$id "; continue; fi
      x=$(sd_first_of "$cl" "$id" "$unplaced")
      if [[ -n $x ]]; then sd_say_id "$mode" "$id" "not run here: needs $x, which is not run here"; continue; fi
      # act said nothing of it: it skipped it for its if:, or for a need's, unless a need runs
      # in a container of its own.
      [[ -n $(sd_first_of "$cl" "$id" "$started") ]] || continue
    fi
    started="$started$id "
    rc=0
    sd_one "$mode" "$id" || rc=$?
    if ((rc)); then failed="$failed$id "; ((st)) || st=$rc; fi
  done
  rm -f "$f.first"
  return "$st"
}
# The first line of LINES, but ID, in SET (" a b "), or nothing.
sd_first_of() { # LINES ID SET
  local j
  for j in $1; do
    [[ $j == "$2" || $3 != *" $j "* ]] || { printf '%s\n' "$j"; return 0; }
  done
  return 0
}

# ID in a systemd container of its own: up, its act (its lines through sd_only, all of them to
# sd_log), its uploads back (unless stopped), down. Its status: act's, or 1 when the container
# did not start or its uploads were refused, or 130 when a stop came before its act started.
sd_one() { # MODE ID
  local mode=$1 id=$2 rc=0 t m e names files=()
  t=$(mktemp -d "${TMPDIR:-/tmp}/bana-sd.XXXXXX")
  names=$(sd_names "$id")
  sd_inner "$id"
  while IFS= read -r e; do files+=("$e"); done < <(sd_files)
  printf '# bana: %s, in a systemd container of %s\n' "$id" "$sd_image" >>"$sd_log"
  if ! sd_up "$sd_name" "$sd_image" "$sd_arch" "${sd_net:-}" "${sd_label:-}" "$sd_bin" "${sd_cache:-}" "$sd_root" ${files[@]+"${files[@]}"}; then
    rm -rf "$t"
    [[ -z ${sd_stop:-} ]] || return 130
    sd_say_id "$mode" "$id" "not run here: its systemd container did not start"
    sd_red "systemd job $id did not run: $sd_err"
    return 1
  fi
  [[ -z ${sd_life:-} ]] || sd_lifeline "$sd_name" || true
  # A stop from here on reaches its act (sd_int), or comes before it, and then it does not start.
  sd_live=$sd_name
  if [[ -n ${sd_stop:-} ]]; then
    sd_live=''
    sd_down "$sd_name"
    rm -rf "$t"
    return 130
  fi
  # Its own process group: a terminal's Ctrl-C reaches bash, which passes it on once (sd_int).
  m=$-
  set -m
  (sd_act "$sd_name" "$sd_uid" "${sd_in[@]}") 9>&- \
    > >(exec 9>&-; trap '' INT; tee -a "$sd_log" | sd_only "$mode" "$id" "$names" "$t/res"; : >"$t/o.done") \
    2> >(exec 9>&-; trap '' INT; tee -a "$sd_log" >"$t/err"; : >"$t/e.done") &
  sd_child=$!
  [[ $m == *m* ]] || set +m
  # (bash 3.2 says so on stderr when a job of set -m's dies of a signal: not here.)
  sd_wait "$sd_child" 2>/dev/null || rc=$?
  sd_child='' sd_live=''
  sd_copied "$t/o" "$t/e"
  if ((rc)) && [[ -z ${sd_stop:-} && ! -e $t/res ]]; then
    sd_say_id "$mode" "$id" "not run here: a job it needs failed in its systemd container (its output: $sd_log)"
    e=$(awk '{ gsub(/\033\[[0-9;]*[A-Za-z]/, "") } /^Error: / { sub(/^Error: /, ""); print; exit }' "$t/err")
    sd_red "systemd job $id did not run: ${e:-its act ended with $rc}"
  fi
  if [[ -n ${sd_art:-} && -z ${sd_stop:-} ]] && ! sd_copy_out "$sd_name" "$sd_art"; then
    sd_red "systemd job $id: its uploads were refused: $sd_err"
    ((rc)) || rc=1
  fi
  sd_down "$sd_name"
  rm -rf "$t"
  return "$rc"
}

# sd_in: the arguments of ID's act in its container: sd_args, but for its own platforms, its job,
# and its own artifact server's directory and action cache. A label whose jobs run in a
# container (a systemd one, act.image, an image of its own) goes to act's host mode there, as the
# jobs it needs run again inside; any other (a Mac's, skip) to no place.
sd_inner() { # ID
  local i a l v pl=()
  sd_in=()
  for ((i = 0; i < ${#sd_args[@]}; i++)); do
    a=${sd_args[i]}
    case $a in
    -P | --platform) i=$((i + 1)); pl+=("${sd_args[i]:-}"); continue ;;
    -P=* | --platform=*) pl+=("${a#*=}"); continue ;;
    -P?*) pl+=("${a#-P}"); continue ;;
    -j | --job | --container-options | --network | --artifact-server-path | --action-cache-path) i=$((i + 1)); continue ;;
    -j?* | --job=* | --container-options=* | --network=* | --artifact-server-path=* | --action-cache-path=* | --rm | --rm=*) continue ;;
    esac
    sd_in+=("$a")
  done
  # Each label once, at its last -P (act's rule), and the systemd ones.
  while IFS='=' read -r l v; do
    if [[ $sd_labels == *" $l "* || ( -n $v && $v != -* ) ]]; then sd_in+=(-P "$l=-self-hosted"); else sd_in+=(-P "$l="); fi
  done < <({ printf '%s\n' ${pl[@]+"${pl[@]}"}; printf '\n'; for l in $sd_labels; do printf '%s=\n' "$l"; done; } | awk '
    $0 == "" { more = 1; next }
    index($0, "=") { l = tolower(substr($0, 1, index($0, "=") - 1)); if (more && l in val) next
      if (!(l in val)) o[++n] = l; val[l] = substr($0, index($0, "=") + 1) }
    END { for (i = 1; i <= n; i++) print o[i] "=" val[o[i]] }')
  sd_in+=(-j "$1" --concurrent-jobs 1 --artifact-server-path /home/runner/.bana/artifacts --action-cache-path /home/runner/.cache/act)
}

# The files sd_args names (its event, secrets, env, vars and inputs), a line each.
sd_files() {
  local i a
  for ((i = 0; i < ${#sd_args[@]}; i++)); do
    a=${sd_args[i]}
    case $a in
    -e | --eventpath | --secret-file | --env-file | --var-file | --input-file) a=${sd_args[i + 1]:-} ;;
    --eventpath=* | --secret-file=* | --env-file=* | --var-file=* | --input-file=*) a=${a#*=} ;;
    *) continue ;;
    esac
    [[ $a != /* || $a == /dev/null || ! -f $a ]] || printf '%s\n' "$a"
  done
  return 0
}

# NAME, a fresh systemd container of IMAGE for ARCH, on NET (none: Docker's bridge), labelled
# for the daemon's PREFIX (none: not labelled), ready: act (ACT, read-only, at /bana/bin/act)
# and act's actions from CACHE (a copy), and a user, runner, uid as here (1001 for root, as on
# GitHub's runners), in groups sudo, adm and systemd-journal, with sudo, lingering: its systemd
# user manager runs. Each RO-PATH as here, read-only: a directory, mounted (the checkout, which
# runner must read); a file, a copy that runner owns (the root here may own it, mode 600).
# Fails, saying why (sd_err), with the container gone.
sd_up() { # NAME IMAGE ARCH NET PREFIX ACT CACHE [RO-PATH...]
  local n=$1 img=$2 arch=$3 net=$4 lbl=$5 bin=$6 cache=$7 x p e s w end in=() files=() opts=() c=()
  shift 7
  sd_err='' sd_mounts=()
  for x in "$@"; do
    [[ $x == /* && -e $x ]] || continue
    case $x in *:*) sd_err="a path Docker cannot mount: $x"; return 1 ;; esac
    for p in ${in[@]+"${in[@]}"}; do [[ $x != "$p" && $x != "$p"/* ]] || continue 2; done
    in+=("$x")
    if [[ -d $x ]]; then sd_ro "$x"; elif [[ -f $x ]]; then files+=("$x"); fi
  done
  [[ -z $lbl ]] || opts+=(--label "xyz.tjrb.bana=$lbl")
  case $net in '' | host | none | container:*) ;; *) opts+=(--network "$net") ;; esac
  [[ ! -d $cache ]] || c=(-v "$cache:/bana/in/cache:ro")
  # A container of a run before (bana ci's runs take turns: act.lock).
  "${sysd_docker[@]}" rm -f "$n" </dev/null >/dev/null 2>&1 9>&- || true
  if ! e=$("${sysd_docker[@]}" run -d --rm --name "$n" ${opts[@]+"${opts[@]}"} --platform "$arch" "${sd_run_flags[@]}" \
    -v "$bin:/bana/bin/act:ro" ${c[@]+"${c[@]}"} ${sd_mounts[@]+"${sd_mounts[@]}"} "$img" "${sd_masks[@]}" </dev/null 2>&1 9>&-); then
    case $e in
    *"/sbin/init"*) sd_err="act.image $img cannot run systemd jobs (no /sbin/init or setpriv)" ;;
    *) sd_err="docker run: $(printf '%s\n' "$e" | awk 'NF { l = $0 } END { print l }')" ;;
    esac
    sd_down "$n"
    return 1
  fi
  # Up: systemd says running (or degraded: a unit failed), once its bus answers.
  w=${BANA_SYSTEMD_WAIT:-30}
  [[ $w =~ ^[0-9]+$ ]] || w=30
  end=$((SECONDS + w))
  while :; do
    s=$("${sysd_docker[@]}" exec "$n" systemctl is-system-running </dev/null 2>&1 9>&-) || true
    case $s in
    running | degraded) break ;;
    *"No such container"* | *"is not running"*) sd_err="its container stopped as systemd started"; sd_down "$n"; return 1 ;;
    esac
    [[ -z ${sd_stop:-} ]] || { sd_err="stopped"; sd_down "$n"; return 1; }
    if ((SECONDS >= end)); then
      sd_err="systemd was not up in ${w}s ($(printf '%s' "$s" | tr -d '\000-\037'))"
      sd_down "$n"
      return 1
    fi
    sleep 0.2
  done
  sd_uid=$(id -u)
  ((sd_uid)) || sd_uid=1001
  for x in ${files[@]+"${files[@]}"}; do
    if ! "${sysd_docker[@]}" exec "$n" mkdir -p "${x%/*}" </dev/null >/dev/null 2>&1 9>&- ||
      ! "${sysd_docker[@]}" cp -L "$x" "$n:$x" </dev/null >/dev/null 2>&1 9>&-; then
      sd_err="$x did not reach its container"
      sd_down "$n"
      return 1
    fi
  done
  # As root: runner (the uid's holder goes, if a user's), sudo, act's actions, its user manager
  # (start blocks until it runs), the copies runner's, the paths to them open to it; then the
  # checkout read as runner.
  # shellcheck disable=SC2016 # the container's sh expands these
  e=$("${sysd_docker[@]}" exec "$n" sh -c 'set -e
    u=$1 r=$2; shift 2
    command -v setpriv >/dev/null || exit 3
    if id runner >/dev/null 2>&1 && [ "$(id -u runner)" != "$u" ]; then userdel runner; fi
    o=$(getent passwd "$u" | cut -d: -f1) || o=
    if [ -n "$o" ] && [ "$o" != runner ]; then
      [ "$u" -ge 1000 ] || { echo "$o"; exit 4; }
      userdel "$o"
    fi
    g=
    for x in sudo adm systemd-journal; do getent group "$x" >/dev/null && g=$g${g:+,}$x; done
    id runner >/dev/null 2>&1 || useradd -m -u "$u" -s /bin/bash ${g:+-G "$g"} runner
    echo "runner ALL=(ALL) NOPASSWD:ALL" >/etc/sudoers.d/runner
    chmod 440 /etc/sudoers.d/runner
    mkdir -p /home/runner/.cache/act /home/runner/.bana/artifacts
    for a in /bana/in/cache/*@*; do [ ! -d "$a" ] || cp -R "$a" /home/runner/.cache/act/; done
    chown -R runner: /home/runner
    loginctl enable-linger runner
    systemctl start "user@$u.service"
    for x in "$r" "$@"; do
      p=$x
      while p=${p%/*}; [ -n "$p" ]; do chmod o+x "$p" 2>/dev/null || :; done
    done
    for x in "$@"; do chown runner: "$x"; chmod 400 "$x"; done
    h=$r/.git/HEAD
    [ ! -f "$r/.git" ] || h=$r/.git
    setpriv --reuid="$u" --regid="$u" --init-groups cat "$h" >/dev/null 2>&1 || exit 5' \
    bana-up "$sd_uid" "$sd_root" ${files[@]+"${files[@]}"} </dev/null 2>&1 9>&-) || {
    case $? in
    3) sd_err="act.image $img cannot run systemd jobs (no /sbin/init or setpriv)" ;;
    4) sd_err="uid $sd_uid is $(printf '%s' "$e" | tail -1 | tr -d '\000-\037')'s in act.image, a system account" ;;
    5) sd_err="the systemd container cannot read the checkout as uid $sd_uid" ;;
    *) sd_err="its user did not start: $(printf '%s\n' "$e" | awk 'NF { l = $0 } END { print l }' | tr -d '\000-\037')" ;;
    esac
    sd_down "$n"
    return 1
  }
}
# A directory as here, read-only: the one way a path of this machine's reaches a container.
sd_ro() { # PATH
  sd_mounts+=(-v "$1:$1:ro")
}

# If bash dies (SIGKILL), its fd 9 closes, the container reads its end, and powers off (--rm then
# removes it); a killed docker CLI ends the same way. Every command bash starts after this one
# goes without fd 9 (9>&-), so that it is bash's alone. sd_down closes it.
sd_lifeline() { # NAME
  local f m=$-
  f=$(mktemp -u "${TMPDIR:-/tmp}/bana-life.XXXXXX") && mkfifo -m 600 "$f" || return 1
  set -m
  "${sysd_docker[@]}" exec -i "$1" sh -c 'cat >/dev/null; systemctl poweroff || kill -37 1' <"$f" >/dev/null 2>&1 &
  sd_life_pid=$!
  [[ $m == *m* ]] || set +m
  exec 9>"$f"
  rm -f "$f"
}

# act in container NAME, as runner (uid U), in the checkout, with ACT-ARGUMENT...: the
# environment its own, but for runner's, and GITHUB_TOKEN by its name (its value in no argv).
# Its pid in /run/bana-act.pid, root's, for sd_int (a job could change it only through sudo, and
# then hold up only its own cancel, until the daemon's last rung); a stop asked before act
# started stops it.
sd_act() { # NAME U ACT-ARGUMENT...
  local n=$1 u=$2 t=()
  shift 2
  [[ -z ${GITHUB_TOKEN:-} ]] || t=(-e GITHUB_TOKEN)
  # shellcheck disable=SC2016 # the container's sh expands these
  "${sysd_docker[@]}" exec -u root -w "$sd_root" -e USER=runner -e LOGNAME=runner -e RUNNER_USER=runner \
    -e HOME=/home/runner -e "XDG_RUNTIME_DIR=/run/user/$u" -e "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$u/bus" \
    ${t[@]+"${t[@]}"} "$n" sh -c 'echo $$ >/run/bana-act.pid
      [ ! -e /run/bana-stop ] || exit 130
      u=$1; shift
      exec setpriv --reuid="$u" --regid="$u" --init-groups /bana/bin/act "$@"' bana-act "$u" "$@" </dev/null 9>&-
}

# Ctrl-C for the act running in a container (sd_live), as act takes it: once stops its jobs,
# twice ends it.
sd_int() {
  [[ -n ${sd_live:-} ]] || return 0
  # shellcheck disable=SC2016 # the container's sh expands these
  "${sysd_docker[@]}" exec "$sd_live" sh -c 'touch /run/bana-stop; kill -INT "$(cat /run/bana-act.pid)"' \
    </dev/null >/dev/null 2>&1 9>&- || true
}

# act's output in container ID: its lines, and nothing else (the needs' run again, act's
# warnings, a line a job wrote as act's). JSON: an object, with exactly one jobID, ID, and no
# key bana (which the daemon's own lines have), no \u escape but those Go writes (no key in
# disguise), no control character. Text: the lines of ID's names (NAMES, a line each), as a
# terminal shows them, and the lines that go on one of them. FLAG: made when ID's result came.
sd_only() { # MODE ID NAMES FLAG
  SD_NAMES=$3 sd_awk -v mode="$1" -v id="$2" -v flag="$4" -v q='"' '
    BEGIN { n = split(ENVIRON["SD_NAMES"], a, "\n"); for (i = 1; i <= n; i++) if (a[i] != "") want[a[i]] }
    function keep() { print; fflush() }
    mode == "json" {
      s = $0
      if (substr(s, 1, 1) != "{" || substr(s, length(s)) != "}" || s ~ /[\001-\037]/) next
      gsub(/\\\\/, "", s)
      t = s
      gsub(/\\u(00[01][0-9a-f]|003c|003e|0026|2028|2029|fffd)/, "", t)
      if (index(t, "\\u")) next
      gsub(/\\"/, "", s)
      if (s ~ /"bana"[ \t]*:/) next
      k = s
      if (gsub(/"jobID"[ \t]*:/, "", k) != 1 || !index(s, q "jobID" q ":" q id q)) next
      if (s ~ /"jobResult"[ \t]*:/) { printf "" >flag; close(flag) }
      keep(); next
    }
    {
      s = $0
      while (s ~ /\r$/) sub(/\r$/, "", s)
      sub(/.*\r/, "", s); gsub(/\033\[[^@-~]*[@-~]/, "", s); gsub(/[\001-\010\013-\037]/, "", s)
      sub(/^\*DRYRUN\* /, "", s)
      if (substr(s, 1, 1) == "[" && (j = index(s, "]"))) {
        nm = substr(s, 2, j - 2); gsub(/^ +| +$/, "", nm)
        on = (nm in want)
        if (on) { keep(); if (index(s, "🏁  Job ")) { printf "" >flag; close(flag) } }
        next
      }
      if (on && s !~ /^Error/) keep()
    }'
}

# awk, reading a pipe a line at a time. mawk (Debian's and Ubuntu's awk) waits for a full buffer
# otherwise, and a job's lines would come late (a running job would look still); with -W
# interactive it reads a line at a time, but a line longer than its buffer (4 KiB) ends its input
# there: bash hands it the lines, each cut at 4000 bytes (such a line of act's JSON is then no
# object, and dropped; all of it is in sd_log).
sd_awk() {
  if awk -W version </dev/null 2>/dev/null | grep -q mawk; then
    (
      LC_ALL=C
      while IFS= read -r l || [[ -n $l ]]; do printf '%s\n' "${l:0:4000}"; done
    ) | awk -W interactive "$@"
  else
    awk "$@"
  fi
}

# Its uploads (/home/runner/.bana/artifacts), out of container NAME as an archive, checked as
# bana split's are (plain files and directories, no path out, BANA_SYSTEMD_MAX bytes at most, 8
# GiB), opened as this user's, without setuid: each RUN/NAME that DIR lacks goes there (the
# jobs it needs uploaded theirs here already). Refused (sd_err): nothing moves.
sd_copy_out() { # NAME DIR
  local n=$1 h=$2 t max=${BANA_SYSTEMD_MAX:-8589934592} s l v e r
  [[ $max =~ ^[0-9]+$ ]] || max=8589934592
  sd_err=''
  t=$(mktemp -d "${TMPDIR:-/tmp}/bana-art.XXXXXX") || { sd_err="no room here"; return 1; }
  "${sysd_docker[@]}" cp "$n:/home/runner/.bana/artifacts" - </dev/null 2>/dev/null 9>&- | head -c "$((max + 1))" >"$t/a.tar" || true
  s=$(wc -c <"$t/a.tar" | tr -d ' ')
  if ((s > max)); then
    sd_err="more than $max bytes"
  elif ((s > 0)); then
    if ! l=$(tar -tf "$t/a.tar" 2>/dev/null) || ! v=$(tar -tvf "$t/a.tar" 2>/dev/null); then
      sd_err="not an archive"
    elif printf '%s\n' "$l" | awk '{ p = "/" $0 "/" } !/^artifacts(\/|$)/ || index(p, "/../") { bad = 1 } END { exit !bad }'; then
      sd_err="a path out of its uploads"
    elif printf '%s\n' "$v" | awk 'NF && !/^[-d]/ { bad = 1 } END { exit !bad }'; then
      sd_err="a link or a special file"
    elif ! mkdir "$t/x" || ! tar --no-same-owner --no-same-permissions -xf "$t/a.tar" -C "$t/x" 2>/dev/null; then
      sd_err="its archive did not open"
    else
      chmod -R u-s,g-s "$t/x" 2>/dev/null || true
      for e in "$t/x/artifacts"/*/*; do
        [[ -e $e ]] || continue
        r=${e#"$t/x/artifacts/"}
        [[ ! -e $h/$r ]] || continue
        mkdir -p "$h/${r%/*}" && mv "$e" "$h/$r"
      done
    fi
  fi
  rm -rf "$t"
  [[ -z $sd_err ]]
}

# The container goes, its lifeline first.
sd_down() { # NAME
  if [[ -n ${sd_life_pid:-} ]]; then
    exec 9>&-
    sd_life_pid=''
  fi
  "${sysd_docker[@]}" rm -f "$1" </dev/null >/dev/null 2>&1 9>&- || true
}

# PID's exit status, also when a signal bash traps ends wait early (bash 3.2 has no wait -n).
sd_wait() { # PID
  local st=0
  wait "$1" || st=$?
  while kill -0 "$1" 2>/dev/null; do
    st=0
    wait "$1" || st=$?
  done
  return "$st"
}
# Until each FILE's reader has ended (FILE.done), 5 seconds at most.
sd_copied() { # FILE...
  local f i
  for f in "$@"; do
    for ((i = 0; i < 100; i++)); do [[ ! -e $f.done ]] || break; sleep 0.05; done
  done
}
# ---- end of systemd ------------------------------------------------------------------------

# The Linux act a container runs, for its CPU (ARCH: arm64, else x86_64): act
# $split_act_version, as lib/split.sh pins it (sourced first), downloaded once into
# ~/.bana/act-linux/VERSION-CPU and checked against its sha256 there; BANA_SYSTEMD_ACT: another
# (tests, offline). Sets sd_bin, or says why not (sd_err) and fails.
act_linux() { # ARCH
  local a sum dir t
  sd_err=''
  if [[ -n ${BANA_SYSTEMD_ACT:-} ]]; then
    [[ -x $BANA_SYSTEMD_ACT ]] || { sd_err="BANA_SYSTEMD_ACT: no program $BANA_SYSTEMD_ACT"; return 1; }
    sd_bin=$BANA_SYSTEMD_ACT
    return 0
  fi
  case $1 in *arm64 | *aarch64) a=arm64 sum=$split_act_sha256_arm64 ;; *) a=x86_64 sum=$split_act_sha256 ;; esac
  dir=$base_home/act-linux/$split_act_version-$a
  if [[ ! -x $dir/act ]]; then
    mkdir -p "$dir"
    t=$(mktemp -d "$dir/.get.XXXXXX") || { sd_err="cannot write $dir"; return 1; }
    if ! curl -fsSL --retry 3 -o "$t/act.tar.gz" \
      "https://github.com/nektos/act/releases/download/v$split_act_version/act_Linux_$a.tar.gz" >/dev/null 2>&1 ||
      [[ $(split_sha256 "$t/act.tar.gz") != "$sum" ]] || ! tar -xzf "$t/act.tar.gz" -C "$t" act 2>/dev/null; then
      rm -rf "$t"
      sd_err="act $split_act_version for Linux ($a) did not download, or is not the one pinned"
      return 1
    fi
    chmod 755 "$t/act"
    mv -f "$t/act" "$dir/act"
    rm -rf "$t"
  fi
  sd_bin=$dir/act
}

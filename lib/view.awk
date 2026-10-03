# bana ci's view of act's text output, by hand (lib/act.sh act_view): a line as each job starts
# and ends, a failed step's last 20 lines, the plan's choice, why a job did not run, bana's own
# lines and act's errors, then a count. BSD awk (macOS) and gawk alike.
# A job that needs systemd: bana's [NAME] bana: next, in a systemd container (→, before act
# starts), or bana: not run here: WHY (said once, counted as not run here); act's skip of it
# (its label's place is systemd) says nothing, as bana's lines say it.
#   -v list=FILE     act -l's table, for the jobs act ran nothing of (skipped)
#      table=FILE    LABEL<TAB>PLACE lines (act_platform_table), for why a job was not run here
#      only=JOB      bana ci -j's job: then the jobs not asked for are not called skipped
#      logfile=FILE  where act's output is (last.log)
#      tty=1         colours
function out(s) { print s; fflush() }
function paint(c, s) { return tty ? "\033[" c "m" s "\033[0m" : s }
function trim(s) { sub(/^ +/, "", s); sub(/ +$/, "", s); return s }
function stepname(s) { sub(/^(Main|Pre|Post) /, "", s); sub(/ \[[0-9.]+[a-zµ]*s\]$/, "", s); return s }
# Why a job act skipped for want of LABEL was not run here, by LABEL's place ('': not said, as
# for self-hosted, which every self-hosted job has, or a label that has a place).
function reason(label, l, v, r) {
  l = tolower(label)
  if (l == "self-hosted") return ""
  v = (l in place) ? place[l] : ""
  if (v == "mac") return label ": on a Mac only"
  if (v ~ /^skip/) { r = v; sub(/^skip ?/, "", r); return label ": " (r != "" ? r : "act.platform." l " = skip") }
  if (v == "") return "no place here for " label ": see bana add"
  if (v == "systemd") sysskip[job] = label
  return ""
}
# Whether act's text showed job NAME (act -l's): itself, a matrix's NAME-1, NAME-2…, or, for a
# name with an expression (act shows it worked out), any line at all.
function ran(nm, j) {
  if (nm in seen || index(nm, "${{")) return 1
  for (j in seen) if (index(j, nm "-") == 1 && substr(j, length(nm) + 2) ~ /^[0-9]+$/) return 1
  return 0
}
function plan(o, n, i, kv, k, v, yes, no, tier) {
  gsub(/[{}"]/, "", o)
  n = split(o, kv, ",")
  for (i = 1; i <= n; i++) {
    k = kv[i]; v = k; sub(/:.*/, "", k); sub(/^[^:]*:/, "", v)
    if (k == "tier") tier = v
    else if (v == "true") yes = yes " " k
    else if (v == "false") no = no " " k
  }
  out("  plan" (tier != "" ? " (" tier ")" : "") ": runs" (yes != "" ? yes : " nothing") (no != "" ? " · skips" no : ""))
}
BEGIN {
  while ((getline l < list) > 0) {
    if (!a) { if (index(l, "Job ID")) { a = index(l, "Job ID"); b = index(l, "Job name"); c = index(l, "Workflow name") } continue }
    nm = trim(substr(l, b, c - b))
    if (nm != "") order[++jobs] = nm
  }
  while ((getline l < table) > 0) { k = l; sub(/\t.*/, "", k); v = l; sub(/^[^\t]*\t/, "", v); place[tolower(k)] = v }
}
# [workflow/job   ] what act says of it
/^\[[^]]*\]/ {
  e = index($0, "]")
  job = substr($0, 2, e - 2); sub(/^[^\/]*\//, "", job); job = trim(job)
  rest = substr($0, e + 2)
  seen[job] = 1
  # bana's word on a job that needs systemd (lib/systemd.sh's sd_say).
  if (index(rest, "bana: ") == 1) {
    if (rest == "bana: next, in a systemd container") {
      if (!(job in apart)) out("→ " job ": next, in a systemd container")
      apart[job] = 1
    } else if (index(rest, "bana: not run here: ") == 1 && !(job in skip)) {
      skip[job] = 1
      out(paint(2, "– " job ": not run here (" substr(rest, 21) ")"))
      notrun++
    }
    next
  }
  if (index(rest, "Skipping unsupported platform")) {
    if (job in skip) next
    label = rest; sub(/.*-P /, "", label); sub(/=\.\.\..*/, "", label)
    r = reason(label)
    if (r == "") next
    skip[job] = 1
    out(paint(2, "– " job ": not run here (" r ")"))
    notrun++
    next
  }
  if (index(rest, "⭐ Run ") == 1) {
    if (!(job in started)) { started[job] = 1; out("▶ " job) }
    lines[job] = 0
    next
  }
  if (substr(rest, 1, 4) == "  | ") {
    o = substr(rest, 5)
    n = ++lines[job]
    buf[job, n % 20] = o
    if (index(o, "{\"tier\":") == 1) plan(o)
    next
  }
  if (index(rest, "Failure - ")) {
    s = rest; sub(/.*Failure - /, "", s)
    if (job in failed) next
    failed[job] = stepname(s)
    # Its last lines, kept: act's Post and Complete job steps come before the job's end.
    n = lines[job]; k = 0
    for (i = (n > 20 ? n - 19 : 1); i <= n; i++) kept[job, ++k] = buf[job, i % 20]
    nkept[job] = k
    next
  }
  # A job ends once: a later end of it (a line of its again) changes nothing.
  if (index(rest, "Job succeeded")) {
    if (job in ended) next
    ended[job] = 1; passed++
    out(paint(32, "✓") " " job)
    next
  }
  if (index(rest, "Job failed")) {
    if (job in ended) next
    ended[job] = 1; fails++
    out(paint(31, "✗") " " job (job in failed ? ": " failed[job] : ""))
    for (i = 1; i <= nkept[job]; i++) out("    " kept[job, i])
    next
  }
  next
}
# act's last line for a failed job: the ✗ said it.
/^Error: Job '.*' failed$/ { next }
/^Error: / { out(paint(31, $0)); next }
# bana's own lines (say, warn, die).
/^\033\[(1|31|33)m/ { out($0); next }
END {
  # act skipped it for a systemd label, and bana said nothing of it (its dry run failed).
  for (j in sysskip)
    if (!(j in apart) && !(j in skip) && !(j in started)) { out(paint(2, "– " j ": not run here (" sysskip[j] ": a job that needs systemd)")); notrun++ }
  if (only == "")
    for (i = 1; i <= jobs; i++)
      if (!ran(order[i])) { out(paint(2, "– " order[i] ": skipped (its if: was false, or a job it needs did not pass)")); skipped++ }
  s = ""
  if (passed) s = s ", " passed " passed"
  if (fails) s = s ", " fails " failed"
  if (skipped) s = s ", " skipped " skipped"
  if (notrun) s = s ", " notrun " not run here"
  sub(/^, /, "", s)
  out((s != "" ? s : "no job ran") " · act's output: " logfile)
}

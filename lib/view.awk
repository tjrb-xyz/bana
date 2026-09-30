# bana ci's view of act's text output, by hand (lib/act.sh act_view): a line as each job starts
# and ends, a failed step's last 20 lines, the plan's choice, why a job did not run, bana's own
# lines and act's errors, then a count. BSD awk (macOS) and gawk alike.
# -v list=FILE (act -l) table=FILE (label TAB place, act_platform_table) machine="LABELS "
#    vm=NAME os=Darwin|Linux only=JOB (bana ci -j) logfile=FILE tty=1|''
function out(s) { print s; fflush() }
function paint(c, s) { return tty ? "\033[" c "m" s "\033[0m" : s }
function trim(s) { sub(/^ +/, "", s); sub(/ +$/, "", s); return s }
function stepname(s) { sub(/^(Main|Pre|Post) /, "", s); sub(/ \[[0-9.]+[a-zµ]*s\]$/, "", s); return s }
function reason(job, label, l, v, r) {
  l = tolower(label)
  if (l == "self-hosted") return ""
  v = (l in place) ? place[l] : ""
  if (v == "machine") {
    if (index(" " machine, " " l " ")) return "machine"
    if (os == "Darwin") return label ": needs OrbStack or Lima, and neither orb nor limactl is on PATH"
    return label ": needs a Mac's Linux machine"
  }
  if (v == "mac") return label ": on a Mac only"
  if (v ~ /^skip/) { r = v; sub(/^skip ?/, "", r); return label ": " (r != "" ? r : "act.platform." l " = skip") }
  if (v == "") return "no place here for " label ": see bana init"
  return ""
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
  while ((getline l < table) > 0) { k = l; sub(/\t.*/, "", k); v = l; sub(/^[^\t]*\t/, "", v); place[k] = v }
}
/^\[[^]]*\]/ {
  e = index($0, "]")
  job = substr($0, 2, e - 2); sub(/^[^\/]*\//, "", job); job = trim(job)
  rest = substr($0, e + 2)
  seen[job] = 1
  if (index(rest, "Skipping unsupported platform")) {
    if (job in skip) next
    label = rest; sub(/.*-P /, "", label); sub(/=\.\.\..*/, "", label)
    r = reason(job, label)
    if (r == "") next
    skip[job] = 1
    if (r == "machine") { out(paint(36, "→") " " job ": next, in the Linux machine " vm " (with systemd)"); next }
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
  if (index(rest, "Job succeeded")) { passed++; out(paint(32, "✓") " " job); next }
  if (index(rest, "Job failed")) {
    fails++
    out(paint(31, "✗") " " job (job in failed ? ": " failed[job] : ""))
    for (i = 1; i <= nkept[job]; i++) out("    " kept[job, i])
    next
  }
  next
}
/^Error: Job '.*' failed$/ { next }
/^Error: / { out(paint(31, $0)); next }
/^\033\[(1|31|33)m/ { out($0); next }
END {
  if (only == "")
    for (i = 1; i <= jobs; i++)
      if (!(order[i] in seen)) { out(paint(2, "– " order[i] ": skipped (its if: was false, or a job it needs did not pass)")); skipped++ }
  s = ""
  if (passed) s = s ", " passed " passed"
  if (fails) s = s ", " fails " failed"
  if (skipped) s = s ", " skipped " skipped"
  if (notrun) s = s ", " notrun " not run here"
  sub(/^, /, "", s)
  out((s != "" ? s : "no job ran") " · act's output: " logfile)
}

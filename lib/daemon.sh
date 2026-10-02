# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets os, host, prefix, repo, home, base_home, machine_dir, bana_root, self
# bana daemon: CI on push, on this machine. One daemon (bana-manager daemon) serves every
# project bana add added here: it fetches their pushes, runs each through `bana ci` (act),
# one build at a time on the machine, and posts commit statuses with the GitHub CLI. It runs
# as a LaunchAgent on a Mac (in your login session, with a menu bar item) or as a systemd
# user service on Linux. Sourced by bin/bana.
#
#   bana daemon install [options]   check this machine, build, and start the daemon (again)
#     --port N         the page's port (default 8470, or the one installed)
#     --no-tray        on a Mac: no menu bar item (kept when you install again; --tray: back)
#     --no-open        on a Mac: don't open the page afterwards
#     --now            restart at once, even while a build runs (it runs again once)
#                    A new snapshot runs only once it answers as this bana; else the one before
#                    runs again (the new one stays in ~/.bana/daemon.d.bad).
#   bana daemon uninstall [--purge]  stop and remove it; the projects stay added (--purge:
#                    bana remove --purge each one)
#   bana daemon run [--build]   in the foreground, for debugging (--build: from this checkout first)
#   bana daemon status          whether it runs, and what each project builds
#   bana daemon log             its log, followed
#   bana daemon open [PROJECT]  its page
#   bana daemon poke [PROJECT]  fetch now, rather than at the next poll (every project, outside one)
#
# Install once a machine, anywhere; it moves the projects of the daemons bana had before
# (one a project) to this one. bana add, in a project's checkout, adds a project; bana list,
# remove, pause and resume do the rest. The daemon runs a snapshot of bana in
# ~/.bana/daemon.d, and builds each project in its own clone, ~/.bana/<prefix>/src: your
# checkout stays yours. Only Fix with Claude on a failed build (the page, 🧱) writes there: a
# bana/fix-<sha7> branch and its worktree under ~/.bana/<prefix>/fix.
#
#   bana list                    the projects added here, and what each does
#   bana remove [PROJECT] [--purge]   the project's CI here goes: its settings, push hook and
#                    Claude Code's tools (--purge: its clone, builds and state too)
#   bana pause [PROJECT]         no automatic builds of its pushes; Run now, fixes and releases still work
#   bana resume [PROJECT]        the pushes that waited build
#   PROJECT: a prefix bana list shows; by default this checkout's.

daemon_usage() { awk '/^#   bana daemon install/, /^#   bana daemon poke/ { sub(/^# ?/, ""); print }' "$bana_root/lib/daemon.sh" >&2; exit 2; }
project_usage() { awk '/^#   bana list/, /^#   PROJECT:/ { sub(/^# ?/, ""); print }' "$bana_root/lib/daemon.sh" >&2; exit 2; }

d_dir=$machine_dir
d_settings=$d_dir/settings
d_label=xyz.tjrb.bana
d_plist=$HOME/Library/LaunchAgents/$d_label.plist
d_logfile=$HOME/Library/Logs/bana/bana.log
d_unit=bana.service
d_unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
d_unit_file=$d_unit_dir/$d_unit
# The keys only daemon.d/settings has (manager/src/daemon.rs's MACHINE_KEYS, but path).
d_machine_keys=" port host login tray home git gh docker caffeinate bash script bana_commit retry recheck ladder ladder_short grace publish_timeout "
# The project's keys only bana split writes (bana add keeps them).
d_split_keys="release.repo split.repo split.ci split.logs split.workflow split.key"
# Seconds between asks while install waits for the daemon to answer (60 asks), and a
# tenth of those while it waits for a build to end. Tests make it 0.
d_step=${BANA_DAEMON_STEP:-1}

# A key of a settings file (the machine's by default).
d_setting() { # KEY [FILE]
  local f=${2:-$d_settings}
  [[ -f $f ]] || return 1
  awk -v k="$1" '
    /^[ \t]*#/ { next }
    { i = index($0, "=") } i == 0 { next }
    { key = substr($0, 1, i - 1); gsub(/^[ \t]+|[ \t]+$/, "", key) }
    key == k { v = substr($0, i + 1); gsub(/^[ \t]+|[ \t\r]+$/, "", v); found = 1 }
    END { if (found) print v; exit !found }' "$f"
}
d_port() { d_setting port || echo 8470; }
d_token() { cat "$base_home/manager-token" 2>/dev/null || true; }
d_url() { # PORT [FRAGMENT]
  local t
  t=$(d_token)
  echo "http://127.0.0.1:$1/${t:+#token=$t}${2:+${t:+&}$2}"
}
d_installed() { [[ -f $d_settings ]] && [[ -f $d_plist || -f $d_unit_file ]]; }
# A project's settings: the file that makes it added.
d_project() { echo "$(project_home "$1")/daemon/settings"; } # PREFIX
d_added() { [[ -n $repo && -f $(d_project "$prefix") ]]; } # this checkout's project
# The projects added here, from their files (the daemon need not run).
d_prefixes() {
  local f p
  for f in "$base_home"/*/daemon/settings; do
    [[ -f $f ]] || continue
    p=${f%/daemon/settings} p=${p##*/}
    [[ ! $p =~ ^[a-z0-9][a-z0-9-]*$ ]] || echo "$p"
  done
}
# shellcheck disable=SC2088 # shown, not expanded
d_tilde() { case $1 in "$HOME"/*) echo "~/${1#"$HOME"/}" ;; *) echo "$1" ;; esac; } # PATH

# The daemon's HTTP API on loopback, never through a proxy. The token goes in on stdin,
# not on the command line, where ps would show it.
d_curl() { # PORT PATH [CURL-OPTIONS...]
  local port=$1 path=$2
  shift 2
  printf 'Authorization: Bearer %s\n' "$(d_token)" |
    curl -fsS --noproxy '*' --max-time 10 -H @- "$@" "http://127.0.0.1:$port$path"
}
# The health answer of a daemon on PORT (fails when none, or no daemon, answers there).
d_health() { # PORT
  local h
  h=$(curl -fsS --noproxy '*' --max-time 3 "http://127.0.0.1:$1/ci/v1/health" 2>/dev/null) || return 1
  printf '%s\n' "$h"
  grep -Eq '"daemon": *true' <<<"$h"
}
d_is_ours() { grep -Eq '"global": *true' <<<"$1"; } # the one daemon, not a project's of before
# The bana version and the process a health answer names (none: a daemon from before they did).
d_health_version() { sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' <<<"$1"; } # HEALTH
d_health_pid() { sed -n 's/.*"pid": *\([0-9][0-9]*\).*/\1/p' <<<"$1"; }        # HEALTH
d_health_latest() { sed -n 's/.*"latest": *"\(v[0-9A-Za-z.-]*\)".*/\1/p' <<<"$1"; } # HEALTH
# A newer bana is out (the daemon checks once a day): a line that says so.
d_latest_line() { local l; l=$(d_health_latest "$1"); [[ -z $l ]] || echo "bana $l is out: bana upgrade"; } # HEALTH
d_up() { local h; h=$(d_health "$(d_port)") && d_is_ours "$h"; }
# The daemon reads the projects' files again, and says how each stands: one JSON line a
# project. A project that starts again waits for its build's checkout first.
d_rescan() { d_curl "$(d_port)" /ci/v1/projects -X POST --max-time 900; }
d_row() { grep -E "^\{.*\"prefix\": *\"$1\"" || true; } # PREFIX < ROWS

# JSON as path=value lines (running.jobs.0.key=linux, queue.#=2), for the few values
# status needs; strings keep their escapes. Portable awk, no jq.
d_flat() {
  awk '
    function path(   k, o) { o = ""; for (k = 1; k <= d; k++) o = o (k > 1 ? "." : "") cur[k]; return o }
    function start() { if (d > 0 && typ[d] == "[") cur[d] = idx[d]++ }
    { s = s $0 "\n" }
    END {
      n = length(s); i = 1; d = 0
      while (i <= n) {
        c = substr(s, i, 1)
        if (c == "{" || c == "[") { start(); d++; typ[d] = c; idx[d] = 0; cur[d] = ""; want[d] = (c == "{"); i++ }
        else if (c == "}" || c == "]") { if (c == "]") { m = idx[d]; d--; print path() ".#=" m } else d--; i++ }
        else if (c == ",") { if (typ[d] == "{") want[d] = 1; i++ }
        else if (c == ":") { want[d] = 0; i++ }
        else if (c == "\"") {
          j = i + 1
          while (j <= n && substr(s, j, 1) != "\"") j += (substr(s, j, 1) == "\\") ? 2 : 1
          v = substr(s, i + 1, j - i - 1); i = j + 1
          if (want[d]) cur[d] = v; else { start(); print path() "=" v }
        } else if (c ~ /[ \t\r\n]/) i++
        else {
          j = i
          while (j <= n && substr(s, j, 1) !~ /[],} \t\r\n]/) j++
          v = substr(s, i, j - i); i = j
          start(); print path() "=" v
        }
      }
    }'
}
# The value at PATH (or at a path ending in .PATH) in d_flat's lines.
d_val() { # FLAT PATH
  awk -v k="$2" '{ i = index($0, "="); p = substr($0, 1, i - 1) }
    p == k || substr(p, length(p) - length(k)) == "." k { print substr($0, i + 1); exit }' <<<"$1"
}
# A string value as text: its \" and \\ undone.
d_text() { d_val "$@" | sed 's/\\"/"/g; s/\\\\/\\/g'; } # FLAT PATH

d_ago() { # SECONDS
  local s=$1
  if ((s < 90)); then echo "${s} s"
  elif ((s < 5400)); then echo "$((s / 60)) min"
  else echo "$((s / 3600)) h $((s % 3600 / 60)) min"; fi
}

# ---- the doctor ------------------------------------------------------------------------

# The project's checkout: where bana.conf and the workflow are.
d_root() {
  if [[ -n ${BANA_PROJECT_ROOT:-} ]]; then
    (cd "$BANA_PROJECT_ROOT" 2>/dev/null && pwd -P) || die "BANA_PROJECT_ROOT: no directory $BANA_PROJECT_ROOT"
  else
    git rev-parse --show-toplevel 2>/dev/null || die "Run bana add in the project's checkout"
  fi
}

# git with the GitHub CLI as its only credential helper, as the daemon fetches.
d_git() { # GH GIT-ARGS...
  local gh=$1
  shift
  GIT_TERMINAL_PROMPT=0 git -c credential.helper= -c "credential.helper=!'$gh' auth git-credential" "$@"
}

# What the daemon needs on this machine.
d_doctor_machine() {
  local v
  say "Checking this machine for the bana daemon"
  command -v act >/dev/null || die "act is needed: brew install act (https://nektosact.com)"
  v=$(act --version 2>/dev/null | head -1)
  echo "  act: ${v:-unknown version} ($(command -v act))"
  command -v docker >/dev/null || die "Docker is needed: install OrbStack (https://orbstack.dev)"
  if docker info >/dev/null 2>&1; then echo "  Docker: running"
  else warn "  Docker is not running: builds wait for it (start OrbStack)"; fi
  # A green build's uploads: unzip, then a sha256 of each file (act's digest, the installer's).
  if command -v unzip >/dev/null && { command -v sha256sum || command -v shasum; } >/dev/null; then
    echo "  unzip and sha256: a green build's uploads are kept as its files"
  else
    warn "  No unzip, or no sha256sum or shasum: a green build's uploads are not collected (bana install needs them)"
  fi
  command -v gh >/dev/null || die "The GitHub CLI is needed: brew install gh, then gh auth login"
  v=$(gh auth status 2>&1) || die "The GitHub CLI is not signed in: gh auth login"
  # A classic token lists its scopes; statuses need repo (a fine-grained one lists none).
  if grep -q 'Token scopes:' <<<"$v" && ! grep -Eq "Token scopes:.*'repo'" <<<"$v"; then
    warn "  gh's token lacks the repo scope, which posting statuses needs: gh auth refresh -s repo"
  fi
  echo "  GitHub CLI: signed in"
  # Publish runs gh release create --verify-tag (never a new tag) with --latest or --latest=false.
  v=$(gh release create --help 2>/dev/null || true)
  if ! grep -q -- --verify-tag <<<"$v" || ! grep -q -- --latest <<<"$v"; then
    warn "  gh release create has no --verify-tag or --latest: releases cannot be published from bana until gh is newer (brew upgrade gh)"
  fi
}

# What the daemon needs of the project, and what in the workflow would go wrong under it.
# UNMAPPED and SPLIT: bana add's counts (empty: not counted, without act or yq).
d_doctor_project() { # ROOT GH UNMAPPED SPLIT
  local root=$1 gh=$2 wf name v runners=() d n line
  d_git "$gh" ls-remote --quiet "https://github.com/$repo.git" HEAD >/dev/null ||
    die "git cannot read https://github.com/$repo.git through the GitHub CLI: gh auth status, and check the repo setting"
  echo "  git: reads $repo through gh"
  name=$(conf workflow ci.yml)
  wf=$root/.github/workflows/$name
  [[ -f $wf ]] || die "No workflow $wf (bana.conf: workflow)"
  grep -q workflow_dispatch "$wf" || die "$name has no workflow_dispatch trigger: the daemon runs it as one"
  for d in "$home"/runners/*/; do [[ -e $d/.runner ]] && runners+=("$(basename "$d")"); done
  n=0
  if ((${#runners[@]})); then
    warn "  This machine has runners in $repo's pool (${runners[*]}): with the daemon too, pushes run twice. 'bana down' first."
    n=$((n + 1))
  elif v=$(grep -o 'vars\.[A-Za-z0-9_]*_CI_AUTO' "$wf" | head -1) &&
    grep -qE "github\.event_name (== 'workflow_dispatch'|!= 'push')" "$wf"; then
    # The jobs run on push only while the variable is not 'false' (bana add proposes it).
    echo "  $name: pushes are gated by ${v#vars.}: gh variable set ${v#vars.} --body false, once the daemon runs"
  else
    # grep -n: LINE:TEXT.
    while IFS= read -r line; do
      [[ -n $line ]] || continue
      warn "  $name:${line%%:*}: a push trigger, and no pool runner here: GitHub queues self-hosted jobs that no runner takes. Keep only workflow_dispatch (the daemon builds the pushes)."
      n=$((n + 1))
    done < <(grep -nE '^[[:space:]]*push:|^[[:space:]]*-[[:space:]]*push[[:space:]]*$|^"?on"?:.*[^a-z_]push([^a-z_]|$)' "$wf" || true)
  fi
  while IFS= read -r line; do
    [[ -n $line ]] || continue
    warn "  $name:${line%%:*}: a checkout ref: makes act clone from GitHub rather than build the pushed commit; remove it"
    n=$((n + 1))
  done < <(grep -nE '^[[:space:]]*ref:' "$wf" || true)
  while IFS= read -r line; do
    [[ -n $line ]] || continue
    warn "  $name:${line%%:*}: act never sets runner.environment, so this gate skips under the daemon: add || env.ACT == 'true'"
    n=$((n + 1))
  done < <(grep -n 'runner\.environment' "$wf" | grep -v 'env\.ACT' || true)
  # shellcheck disable=SC2016 # the workflow's
  while IFS= read -r line; do
    [[ -n $line ]] || continue
    warn "  $name:${line%%:*}: \$RUNNER_ENVIRONMENT is empty under act (neither self-hosted nor github-hosted); \${ACT:-} is true there"
    n=$((n + 1))
  done < <(grep -nE '\$\{?RUNNER_ENVIRONMENT' "$wf" | grep -v '\${ACT' || true)
  # Where each job runs here (bana add's report): a job with no place is left out, and the build passes.
  if [[ -n $3 && $3 != 0 ]]; then
    warn "  $name: $3 jobs would not run here, and the build would still pass: see where each job runs, above"
    n=$((n + 1))
  fi
  if [[ -n $4 && $4 != 0 ]]; then
    warn "  $name: $4 matrices put entries on other runners, and act runs them all on the first one's: see SPLIT, above"
    n=$((n + 1))
  fi
  ((n)) || echo "  $name: runs as workflow_dispatch, nothing to change"
  # bana split's public side, as bana left it (bana split check has all of it).
  if d_setting split.repo "$(d_project "$prefix")" | grep -q .; then
    # shellcheck source=SCRIPTDIR/split.sh
    source "$bana_root/lib/split.sh"
    split_doctor
  fi
}

# ---- install -----------------------------------------------------------------------------

# bana-manager: BANA_DAEMON_BIN (a given one), else a release's own (of this very version),
# else built from this checkout.
d_build() {
  local b=$bana_root/bin/bana-manager v
  if [[ -n ${BANA_DAEMON_BIN:-} ]]; then
    [[ -x $BANA_DAEMON_BIN ]] || die "BANA_DAEMON_BIN: no program $BANA_DAEMON_BIN"
    echo "$BANA_DAEMON_BIN"
    return
  fi
  if [[ -x $b ]]; then
    if [[ $(bana_kind) == release ]]; then
      v=$("$b" version 2>/dev/null) || v=
      [[ $v == "$BANA_VERSION" ]] || die "$b is not bana $BANA_VERSION's bana-manager (it says '$v'): install this release again (its install.sh --force)"
    fi
    echo "$b"
    return
  fi
  command -v cargo >/dev/null || die "cargo is needed to build the daemon: https://rustup.rs"
  say "Building bana-manager (the first time takes a minute)" >&2
  cargo build -q --release --locked --manifest-path "$bana_root/manager/Cargo.toml" >&2 || die "The build failed"
  echo "${CARGO_TARGET_DIR:-$bana_root/manager/target}/release/bana-manager"
}

# Copies FROM to TO by a rename, so a running daemon (or build) keeps its old file.
d_put() { # FROM TO MODE
  mkdir -p "$(dirname "$2")"
  cp "$1" "$2.new.$$"
  chmod "$3" "$2.new.$$"
  mv -f "$2.new.$$" "$2"
}

# The snapshot the daemon runs, in DIR (a new one): a branch switch in your checkout never
# changes it, and a file bana no longer has is not in it.
d_snapshot() { # DIR BIN
  local f
  d_put "$2" "$1/bana-manager" 755
  d_put "$bana_root/bin/bana" "$1/bin/bana" 755
  for f in "$bana_root"/lib/*.sh "$bana_root"/lib/install.*.in; do d_put "$f" "$1/lib/$(basename "$f")" 644; done
}

# The daemon's own clone of the project: made from your checkout (quick), then pointed at GitHub.
d_clone() { # ROOT GH
  local src=$home/src tmp
  [[ -d $src/.git ]] && return 0
  say "Cloning $repo into $src"
  tmp=$src.new.$$
  rm -rf "$tmp"
  mkdir -p "$home"
  if ! {
    git clone -q --no-checkout "$1" "$tmp" &&
      git -C "$tmp" remote set-url origin "https://github.com/$repo.git" &&
      d_git "$2" -C "$tmp" fetch -q --prune origin '+refs/heads/*:refs/remotes/origin/*' '+refs/tags/*:refs/tags/*'
  }; then
    rm -rf "$tmp"
    die "Could not clone $repo into $src"
  fi
  # The default branch, for a push's event; an origin whose HEAD names no branch has none.
  d_git "$2" -C "$tmp" remote set-head origin --auto >/dev/null 2>&1 ||
    warn "$repo's default branch is unknown (its HEAD names no branch): push events carry none, and bana changed compares with main"
  mv "$tmp" "$src"
}

# The PATH a project builds with, when bana.conf has a path: it goes first, then the daemon's.
d_path() {
  local d p
  p=$(d_setting path) || p=$PATH
  for d in $(words "$(conf path)"); do
    case $d in "~"/*) d=$HOME/${d#\~/} ;; esac
    p=$d:$p
  done
  echo "$p"
}

# Writes FILE by a rename, from stdin.
d_write() { # FILE
  mkdir -p "$(dirname "$1")"
  cat >"$1.new.$$"
  mv -f "$1.new.$$" "$1"
}

# Writes a project's settings from stdin, unless only their comments change: the daemon
# starts a project again (its build too) when its file changes.
d_write_keys() { # FILE
  mkdir -p "$(dirname "$1")"
  cat >"$1.new.$$"
  if [[ -f $1 && $(grep -v '^#' "$1") == "$(grep -v '^#' "$1.new.$$")" ]]; then
    rm -f "$1.new.$$"
  else
    mv -f "$1.new.$$" "$1"
  fi
}

# daemon.d/settings, in DIR: the machine's keys (another is an error there). A release
# writes only keys its own bana-manager reads.
d_write_machine() { # DIR PORT TRAY GH
  local k prog login b
  login=$(gh api user --jq .login 2>/dev/null || true)
  # The bana commit the snapshot is of (a fix's brief names it): a checkout's or a
  # release's; none for a copy inside another repository.
  b=$(bana_commit)
  {
    echo "# Written by bana daemon install ($(date '+%Y-%m-%d %H:%M')); run it again to change this."
    echo "port = $2"
    echo "host = $host"
    [[ ! $login =~ ^[A-Za-z0-9-]+$ ]] || echo "login = $login"
    echo "path = $PATH"
    echo "tray = $3"
    [[ -z ${BANA_HOME:-} ]] || echo "home = $base_home"
    echo "gh = $4"
    # caffeinate keeps a Mac awake while act runs.
    for k in git docker bash $([[ $os != Darwin ]] || echo caffeinate); do
      prog=$(command -v "$k" 2>/dev/null) || prog=
      [[ $prog == /* ]] || die "$k is not on PATH ($PATH)"
      echo "$k = $prog"
    done
    echo "script = $d_dir/bin/bana"
    [[ -z $b ]] || echo "bana_commit = $b"
  } | d_write "$1/settings"
}

# <prefix>/daemon/settings: the project's keys, from bana.conf (another is an error there).
# ROOT is this checkout: the page's Fix with Claude makes its worktrees and branches in it.
d_write_project() { # ROOT
  local root=$1 tiers k v keep=''
  # bana split's keys are never bana.conf's: they stay as bana split wrote them.
  for k in $d_split_keys; do
    v=$(d_setting "$k" "$(d_project "$prefix")") || continue
    keep+="$k = $v"$'\n'
  done
  tiers=$(words "$(conf tiers "quick nightly release")" | tr -s ' ' | sed 's/^ //; s/ $//')
  for k in daemon.tier daemon.tag_tier; do
    v=$(daemon_conf "$k")
    [[ -z ${tiers// /} && -z $v ]] || case " $tiers " in *" $v "*) ;; *) die "$k: one of $tiers, not '$v'" ;; esac
  done
  v=$(daemon_conf daemon.poll)
  if ! [[ $v =~ ^[0-9]+$ ]] || ((v < 10)); then die "daemon.poll: seconds, at least 10, not '$v'"; fi
  [[ $(daemon_conf daemon.timeout) =~ ^[1-9][0-9]*s?$ ]] || die "daemon.timeout: minutes (or seconds, as 90s)"
  case $(daemon_conf daemon.supersede) in queued | running) ;; *) die "daemon.supersede: queued or running" ;; esac
  case $(daemon_conf daemon.token) in gh | none) ;; *) die "daemon.token: gh or none" ;; esac
  # A fix's rounds (run_jobs): how many, and their GITHUB_TOKEN (none: act runs offline).
  v=$(conf fix.rounds 5)
  if ! [[ $v =~ ^[0-9]+$ ]] || ((v < 1 || v > 100)); then die "fix.rounds: a number from 1 to 100, not '$v'"; fi
  case $(conf fix.token none) in gh | none) ;; *) die "fix.token: gh or none" ;; esac
  {
    echo "# Written by bana add ($(date '+%Y-%m-%d %H:%M')); bana add again changes it, bana remove removes it."
    echo "repo = $repo"
    echo "prefix = $prefix"
    echo "workflow = $(conf workflow ci.yml)"
    echo "tiers = $tiers"
    echo "tier_input = $(conf tier_input tier)"
    for k in daemon.branches daemon.tags daemon.tier daemon.tag_tier daemon.poll daemon.timeout \
      daemon.supersede daemon.token; do
      v=$(daemon_conf "$k")
      echo "$k =${v:+ $v}"
    done
    echo "fix.rounds = $(conf fix.rounds 5)"
    echo "fix.token = $(conf fix.token none)"
    [[ -z $(conf path) ]] || echo "path = $(d_path)"
    echo "checkout = $root"
    printf '%s' "$keep"
  } | d_write_keys "$(d_project "$prefix")"
}

d_xml() { printf '%s' "$1" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g'; }

d_write_plist() { # PATH TRAY
  local a args=("$d_dir/bana-manager" daemon --home "$base_home")
  [[ $2 == yes ]] || args+=(--no-tray)
  mkdir -p "$(dirname "$d_plist")" "$(dirname "$d_logfile")"
  {
    cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$d_label</string>
  <key>ProgramArguments</key>
  <array>
EOF
    for a in "${args[@]}"; do echo "    <string>$(d_xml "$a")</string>"; done
    cat <<EOF
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>$(d_xml "$1")</string>
EOF
    [[ -z ${BANA_HOME:-} ]] || echo "    <key>BANA_HOME</key><string>$(d_xml "$base_home")</string>"
    cat <<EOF
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key><false/>
  </dict>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>ExitTimeOut</key><integer>60</integer>
  <key>StandardOutPath</key><string>$(d_xml "$d_logfile")</string>
  <key>StandardErrorPath</key><string>$(d_xml "$d_logfile")</string>
</dict>
</plist>
EOF
  } >"$d_plist.new.$$"
  plutil -lint "$d_plist.new.$$" >/dev/null || { rm -f "$d_plist.new.$$"; die "The LaunchAgent does not lint: $d_plist"; }
  mv -f "$d_plist.new.$$" "$d_plist"
}

# A systemd value, quoted, with its specifiers (%) escaped; in a command line, its
# variables ($) too (Environment= expands none).
d_unit_word() { # WORD [exec]
  local w
  w=$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/%/%%/g')
  [[ -z ${2:-} ]] || w=$(printf '%s' "$w" | sed 's/\$/$$/g')
  printf '"%s"' "$w"
}

d_write_unit() { # PATH
  local a exec=''
  for a in "$d_dir/bana-manager" daemon --home "$base_home" --no-tray; do exec+="${exec:+ }$(d_unit_word "$a" exec)"; done
  mkdir -p "$d_unit_dir"
  {
    cat <<EOF
[Unit]
Description=bana: CI on push, for the projects bana add added

[Service]
Environment=$(d_unit_word "PATH=$1")
EOF
    [[ -z ${BANA_HOME:-} ]] || echo "Environment=$(d_unit_word "BANA_HOME=$base_home")"
    cat <<EOF
ExecStart=$exec
Restart=on-failure
RestartSec=5
# SIGTERM to the daemon only, so it can stop act gently; then SIGKILL to what is left.
KillMode=mixed
TimeoutStopSec=60

[Install]
WantedBy=default.target
EOF
  } >"$d_unit_file.new.$$"
  mv -f "$d_unit_file.new.$$" "$d_unit_file"
}

# Waits while the daemon runs a build (a restart would interrupt it). A queued build may
# start as the one waited for ends: it asks once more, a moment later.
d_wait_build() { # PORT
  local flat line b said='' again=''
  while :; do
    line=$(d_curl "$1" /ci/v1/projects 2>/dev/null | grep -E '"running": *\{' | head -1) || line=''
    if [[ -z $line ]]; then
      [[ -n $said && -z $again ]] || return 0
      again=1
      sleep "$d_step"
      continue
    fi
    again=''
    flat=$(d_flat <<<"$line")
    b="$(d_val "$flat" prefix)'s build #$(d_val "$flat" running.id) ($(d_val "$flat" running.ref))"
    [[ $said == "$b" ]] || say "$b runs: restarting the daemon when it ends (--now: restart now; it runs again)"
    said=$b
    sleep $((d_step * 10))
  done
}

# Stops a LaunchAgent, and waits until launchd has let go of it: bootout may return
# while the daemon still stops a build (up to ExitTimeOut). A bootstrap before then
# fails, or starts a daemon that finds the old one on its port and leaves for good.
d_bootout() { # LABEL
  local k uid
  uid=$(id -u)
  launchctl bootout "gui/$uid/$1" 2>/dev/null || true
  for ((k = 0; k < 70; k++)); do
    launchctl print "gui/$uid/$1" >/dev/null 2>&1 || return 0
    sleep "$d_step"
  done
  warn "launchd still has $1 after 70 s"
}

# Starts the service, or starts it again with the new snapshot and settings.
d_service() { # PATH TRAY
  local uid was=''
  uid=$(id -u)
  if [[ $os == Darwin ]]; then
    d_write_plist "$1" "$2"
    d_bootout "$d_label"
    launchctl bootstrap "gui/$uid" "$d_plist" || die "launchctl could not start $d_plist"
  else
    command -v systemctl >/dev/null || die "The daemon runs as a systemd user service, and there is no systemctl here"
    systemctl --user is-active --quiet "$d_unit" 2>/dev/null && was=1
    d_write_unit "$1"
    systemctl --user daemon-reload ||
      die "systemd's user manager does not answer: log in to a session (or sudo loginctl enable-linger ${USER:-$(id -un)}), then install again"
    systemctl --user enable --now "$d_unit" || die "systemd could not start $d_unit"
    [[ -z $was ]] || systemctl --user restart "$d_unit" || die "systemd could not restart $d_unit"
    local user=${USER:-$(id -un)}
    if [[ $(loginctl show-user "$user" -p Linger 2>/dev/null) == Linger=no ]]; then
      say "The daemon runs while you are logged in. On a machine you don't log in to, keep it running with:"
      echo "  sudo loginctl enable-linger $user"
    fi
  fi
}

# The push hook: git runs reference-transaction for every ref change; a push that went
# through updates refs/remotes/origin/*, "committed". It must never fail (a non-zero exit
# in "prepared" aborts the change) and never make the push wait, so it pokes in the
# background, with the token on stdin. It reads the daemon's port when it runs.
d_hook_mark='# bana: tells the daemon about your pushes'
d_hook_file() { # ROOT
  local dir
  dir=$(git -C "$1" rev-parse --git-path hooks) || return 1
  case $dir in /*) ;; *) dir=$1/$dir ;; esac
  echo "$dir/reference-transaction"
}
d_hook_install() { # ROOT PREFIX
  local f
  f=$(d_hook_file "$1") || return 0
  if [[ -f $f ]] && ! grep -qF "$d_hook_mark" "$f"; then
    warn "$f is your own hook: pushes reach the daemon at its next poll (or: bana daemon poke)"
    return 0
  fi
  mkdir -p "$(dirname "$f")"
  cat >"$f.bana" <<HOOK
#!/bin/sh
$d_hook_mark (bana add added it; bana remove removes it).
[ "\$1" = committed ] || exit 0
case "\$(cat)" in *" refs/remotes/origin/"*) ;; *) exit 0 ;; esac
port=\$(sed -n 's/^port *= *//p' '$d_settings' 2>/dev/null)
( printf 'Authorization: Bearer %s\\n' "\$(cat '$base_home/manager-token' 2>/dev/null)" |
  curl -fsS --noproxy '*' --max-time 5 -H @- -X POST "http://127.0.0.1:\${port:-8470}/ci/v1/p/$2/daemon/poll" ) >/dev/null 2>&1 &
exit 0
HOOK
  chmod 0755 "$f.bana"
  mv "$f.bana" "$f"
  echo "  push hook: $f"
}
d_hook_remove() { # ROOT
  local f
  f=$(d_hook_file "$1" 2>/dev/null) || return 0
  if [[ -f $f ]] && grep -qF "$d_hook_mark" "$f"; then rm -f "$f"; fi
}

# bana's MCP server for Claude Code, in ROOT's local scope (~/.claude.json; nothing to commit
# or approve), for the project in DIR: what an earlier one registered goes first. Claude
# Code's config commands run no prompt.
d_claude_add() { # ROOT DIR
  if ! command -v claude >/dev/null; then
    echo "  Claude Code: not on PATH; bana fix passes bana's tools to it itself"
    return 0
  fi
  (cd "$1" && claude mcp remove -s local bana) >/dev/null 2>&1 || true
  if (cd "$1" && claude mcp add -s local bana -- "$d_dir/bana-manager" mcp --dir "$2") >/dev/null 2>&1; then
    echo "  Claude Code: bana's tools, as the MCP server bana in $1 (undo: claude mcp remove -s local bana)"
  else
    warn "  Claude Code did not register bana's tools (claude mcp add): bana fix passes them to it itself"
  fi
}
d_claude_remove() { # ROOT
  command -v claude >/dev/null || return 0
  (cd "$1" && claude mcp remove -s local bana) >/dev/null 2>&1 || true
}

# ---- the daemons of before (one a project) ---------------------------------------------------

# Their projects, one a line: PREFIX<TAB>SERVICE-FILE.
d_olds() {
  local f p
  if [[ $os == Darwin ]]; then
    for f in "$HOME"/Library/LaunchAgents/xyz.tjrb.bana.*.plist; do
      [[ -f $f ]] || continue
      p=${f##*/xyz.tjrb.bana.} p=${p%.plist}
      [[ ! $p =~ ^[a-z0-9][a-z0-9-]*$ ]] || printf '%s\t%s\n' "$p" "$f"
    done
  else
    for f in "$d_unit_dir"/bana-*.service; do
      [[ -f $f ]] || continue
      p=${f##*/bana-} p=${p%.service}
      [[ ! $p =~ ^[a-z0-9][a-z0-9-]*$ ]] || printf '%s\t%s\n' "$p" "$f"
    done
  fi
}

# Moves each project of a daemon of before to this one: stops that daemon (once its build
# ends, unless NOW), removes it and its snapshot, keeps the project's settings but the
# machine's keys, and its hook and Claude Code's tools point here now. Its clone, builds,
# fixes, releases and state stay as they are.
d_migrate() { # NOW
  local p f dir s port h flat id checkout r said mpath reload=''
  mpath=$(d_setting path "$d_dir.new/settings") || mpath= # the one about to run
  while IFS=$'\t' read -r p f <&3; do
    [[ -n $p ]] || continue
    dir=$(project_home "$p") s=$(d_project "$p")
    port=$(d_setting port "$s") || port=8470
    if [[ -z $1 ]] && h=$(d_health "$port") && grep -Eq "\"prefix\": *\"$p\"" <<<"$h"; then
      said=''
      while flat=$(d_curl "$port" /ci/v1/local 2>/dev/null | d_flat) && id=$(d_val "$flat" running.id) && [[ -n $id ]]; do
        [[ $said == "$id" ]] ||
          say "$p's build #$id ($(d_val "$flat" running.ref)) runs: moving $p to the one daemon when it ends (--now: now; it runs again)"
        said=$id
        sleep $((d_step * 10))
      done
    fi
    if [[ $os == Darwin ]]; then
      d_bootout "xyz.tjrb.bana.$p"
    else
      systemctl --user disable --now "bana-$p.service" 2>/dev/null || true
      reload=1
    fi
    rm -f "$f"
    rm -rf "$dir/daemon/bana-manager" "$dir/daemon/bin" "$dir/daemon/lib"
    r=$p
    if [[ -f $s ]]; then
      # path too, when it is the machine's (no bana.conf path made it the project's own).
      awk -v drop="$d_machine_keys" -v mpath="$mpath" '{ i = index($0, "=") } /^[ \t]*#/ || i == 0 { print; next }
        { k = substr($0, 1, i - 1); gsub(/[ \t]/, "", k); v = substr($0, i + 1); gsub(/^[ \t]+|[ \t\r]+$/, "", v) }
        k == "path" && v == mpath { next }
        index(drop, " " k " ") == 0' "$s" | d_write "$s"
      r=$(d_setting repo "$s") || r=$p
      checkout=$(d_setting checkout "$s") || checkout=
      # The hook and Claude Code's MCP server point at the one daemon now: only where the
      # old install put them (--no-hook, --no-claude: still none).
      if [[ -n $checkout && -d $checkout ]]; then
        h=$(d_hook_file "$checkout") || h=
        if [[ -f $h ]] && grep -qF "$d_hook_mark" "$h"; then
          d_hook_install "$checkout" "$p" >/dev/null
        fi
        if command -v claude >/dev/null && (cd "$checkout" && claude mcp get bana) >/dev/null 2>&1; then
          d_claude_add "$checkout" "$dir" >/dev/null
        fi
      fi
    fi
    [[ $os != Darwin ]] || rm -f "$HOME/Library/Logs/bana/$p.log"
    say "Moved $p ($r) to the one daemon: its builds, fixes and releases stay"
  done 3< <(d_olds)
  [[ -z $reload ]] || systemctl --user daemon-reload 2>/dev/null || true
}

# ---- the commands ------------------------------------------------------------------------

# Everything the daemon needs but its service, in daemon.d.new: checks, build, snapshot,
# settings (the new bana writes them). The running daemon's daemon.d is not touched.
d_stage() { # PORT TRAY
  local bin
  d_doctor_machine
  bin=$(d_build)
  rm -rf "$d_dir.new"
  d_snapshot "$d_dir.new" "$bin"
  d_write_machine "$d_dir.new" "$1" "$2" "$(command -v gh)"
  echo "  settings: $d_settings"
}

# One install at a time: two would share daemon.d.new. The lock has its pid; one whose
# process is gone is stale.
d_lock() {
  local lock=$d_dir.lock pid tries=0
  mkdir -p "$base_home"
  until mkdir "$lock" 2>/dev/null; do
    tries=$((tries + 1))
    ((tries < 5)) || die "Cannot take $lock"
    # No pid yet: its taker may be writing it.
    pid=$(cat "$lock/pid" 2>/dev/null) || { sleep 1; pid=$(cat "$lock/pid" 2>/dev/null || true); }
    if [[ $pid =~ ^[0-9]+$ ]] && kill -0 "$pid" 2>/dev/null; then
      die "Another bana daemon install runs (pid $pid): try again once it ends"
    fi
    # Stale: it goes, unless someone took it over meanwhile.
    [[ $(cat "$lock/pid" 2>/dev/null || true) != "$pid" ]] || rm -rf "$lock"
  done
  trap 'rm -rf "$d_dir.lock"' EXIT
  echo "$$" >"$lock/pid.$$"
  mv "$lock/pid.$$" "$lock/pid"
}

# daemon.d.new becomes daemon.d, by two renames: the one before is daemon.d.prev (a
# daemon still running keeps its files). No Ctrl-C between the two: the caller says
# what one does after.
d_swap() {
  rm -rf "$d_dir.prev"
  trap '' INT TERM HUP
  [[ ! -d $d_dir ]] || mv "$d_dir" "$d_dir.prev"
  mv "$d_dir.new" "$d_dir"
}

# Waits for the daemon on PORT to answer as bana VERSION, from a process other than
# NOT-PID (if given). d_last: the last answer of the one daemon ('' when none did).
d_last=''
d_confirm() { # PORT VERSION [NOT-PID]
  local k h
  d_last=''
  for ((k = 0; k < 60; k++)); do
    if h=$(d_health "$1") && d_is_ours "$h"; then
      d_last=$h
      if [[ $(d_health_version "$h") == "$2" ]] && [[ -z ${3:-} || $(d_health_pid "$h") != "$3" ]]; then return 0; fi
    fi
    sleep "$d_step"
  done
  return 1
}

# The service, started again on daemon.d: its own settings' PATH and tray. A subshell, so
# that a die there returns.
d_restart() { (d_service "$(d_setting path)" "$(d_setting tray || echo no)"); }

# What the last answer says it is, for a message.
d_said() { # VERSION
  local v
  [[ -n $d_last ]] || { echo "did not answer"; return; }
  v=$(d_health_version "$d_last")
  [[ $v == "$1" ]] || { echo "answered as bana ${v:-of before}"; return; }
  echo "answered from the process before (pid $(d_health_pid "$d_last"))"
}

daemon_install() {
  local port='' tray=yes open=1 now=${BANA_UPGRADE_NOW:+1} h k olds n=0 old='' pid='' was name
  # The menu bar as installed (--no-tray stays), unless said.
  case $(d_setting tray || true) in no) tray=no ;; esac
  [[ $os == Darwin ]] || tray=no
  while (($#)); do
    case $1 in
    --port) port=${2:?--port N}; shift ;;
    --tray) [[ $os != Darwin ]] || tray=yes ;;
    --no-tray) tray=no ;;
    --no-open) open='' ;;
    --now) now=1 ;;
    --no-hook | --no-claude) die "$1 is bana add's now" ;;
    *) daemon_usage ;;
    esac
    shift
  done
  olds=$(d_olds)
  # The port: --port, else the one installed, else that of the one daemon of before.
  if [[ -z $port ]] && ! port=$(d_setting port); then
    while IFS=$'\t' read -r k _; do
      [[ -n $k ]] || continue
      n=$((n + 1))
      old=$(d_setting port "$(d_project "$k")") || old=8470
    done <<<"$olds"
    port=8470
    ((n != 1)) || port=$old
  fi
  if ! [[ $port =~ ^[0-9]+$ ]] || ((port < 1 || port > 65535)); then die "--port: a port number, not '$port'"; fi
  d_lock
  # The port must be free, or this daemon's, or a daemon's of before (it stops).
  if h=$(curl -fsS --noproxy '*' --max-time 3 "http://127.0.0.1:$port/ci/v1/health" 2>/dev/null); then
    if ! grep -Eq '"daemon": *true' <<<"$h" || { ! d_is_ours "$h" && [[ -z $olds ]]; }; then
      die "Something else serves port $port (bana manager, say): pass --port"
    fi
  fi
  d_stage "$port" "$tray"
  [[ -n $now ]] || ! d_installed || d_wait_build "$port"
  d_migrate "$now"
  # What runs now (a daemon from before says no pid or version; one may not run): the new
  # daemon is another process, of this bana. NAME: the version of the snapshot before.
  name=$(sed -n 's/^BANA_VERSION=//p' "$d_dir/bin/bana" 2>/dev/null) || name=''
  if h=$(d_health "$port") && d_is_ours "$h"; then
    pid=$(d_health_pid "$h") was=$(d_health_version "$h")
  else
    was=$name
  fi
  d_swap
  # Stopped (Ctrl-C, the terminal closed) before the new daemon answers: the one before again.
  if [[ -d $d_dir.prev ]]; then
    for k in INT TERM HUP; do
      # shellcheck disable=SC2064 # the values now
      trap "d_interrupted $k $(printf '%q ' "$port" "$was" "${was:-$name}")" "$k"
    done
  else
    trap - INT TERM HUP
  fi
  say "Waiting for the daemon on port $port"
  if d_restart && d_confirm "$port" "$BANA_VERSION" "$pid"; then
    trap - INT TERM HUP
    if [[ -n ${was:-$name} && ${was:-$name} != "$BANA_VERSION" ]]; then
      say "The daemon runs bana $BANA_VERSION (was ${was:-$name}). Its page: $(d_url "$port")"
    else
      say "The daemon runs. Its page: $(d_url "$port")"
    fi
  else
    [[ -d $d_dir.prev ]] || die "The daemon $(d_said "$BANA_VERSION") on port $port: 'bana daemon log' says why"
    d_rollback "$port" "$was" "${was:-$name}" "$(d_said "$BANA_VERSION")"
  fi
  daemon_list
  echo "bana add, in a project's checkout, adds a project."
  if [[ $os == Darwin && -n $open ]]; then open "$(d_url "$port")" || true; fi
}

# Install was stopped by SIGNAL after the swap: the one before runs again (with no
# terminal to say so after a HUP).
d_interrupted() { # SIGNAL PORT WAS NAME
  [[ $1 != HUP ]] || exec >/dev/null 2>&1
  d_rollback "$2" "$3" "$4" "install was stopped"
}

# The new snapshot's daemon did not come up as this bana (SAID): the one before
# (daemon.d.prev, bana NAME, whose health says WAS) runs again, and the new one stays in
# daemon.d.bad for its log.
d_rollback() { # PORT WAS NAME SAID
  local bad='' start
  [[ -z $d_last ]] || bad=$(d_health_pid "$d_last")
  trap '' INT TERM HUP
  rm -rf "$d_dir.bad"
  mv "$d_dir" "$d_dir.bad"
  mv "$d_dir.prev" "$d_dir"
  trap - INT TERM HUP
  warn "bana $BANA_VERSION's daemon $4: starting bana $3's again"
  if d_restart && d_confirm "$1" "$2" "$bad"; then
    die "bana $BANA_VERSION's daemon did not come up; the daemon is back on $3. Its log: bana daemon log, files in $(d_tilde "$d_dir.bad")"
  fi
  # Nothing more is tried, and nothing is deleted.
  if [[ $os == Darwin ]]; then
    start="launchctl bootout gui/$(id -u)/$d_label; launchctl bootstrap gui/$(id -u) $d_plist"
  else
    start="systemctl --user restart $d_unit"
  fi
  {
    echo "bana $BANA_VERSION's daemon did not come up, and bana $3's did not either ($(d_said "$2")). Both are kept:"
    echo "  $(d_tilde "$d_dir"): bana $3's, which the service runs"
    echo "  $(d_tilde "$d_dir.bad"): bana $BANA_VERSION's"
    echo "Why: bana daemon log. To start bana $3's again:"
    echo "  $start"
    echo "To try bana $BANA_VERSION's instead:"
    echo "  mv $d_dir $d_dir.prev && mv $d_dir.bad $d_dir && $start"
  } >&2
  exit 1
}

daemon_uninstall() {
  local purge='' p n=0
  case ${1:-} in '') ;; --purge) purge=1 ;; *) daemon_usage ;; esac
  if [[ -n $purge ]]; then
    for p in $(d_prefixes); do d_remove "$p" 1; done
  fi
  if [[ $os == Darwin ]]; then
    # launchd stops it (SIGTERM): a running build stops, and is not run again.
    d_bootout "$d_label"
    rm -f "$d_plist"
  elif [[ -f $d_unit_file ]]; then
    systemctl --user disable --now "$d_unit" 2>/dev/null || true
    rm -f "$d_unit_file"
    systemctl --user daemon-reload 2>/dev/null || true
  fi
  rm -rf "$d_dir" "$d_dir.new" "$d_dir.prev" "$d_dir.bad"
  [[ -z $purge ]] || rm -f "$d_logfile"
  for p in $(d_prefixes); do n=$((n + 1)); done
  if ((n)); then
    say "The daemon is gone. $n projects stay added (bana list): their pushes wait for the next bana daemon install."
  else
    say "The daemon is gone."
  fi
}

daemon_run() {
  local build=''
  case ${1:-} in '') ;; --build) build=1 ;; *) daemon_usage ;; esac
  if [[ -n $build || ! -x $d_dir/bana-manager || ! -f $d_settings ]]; then
    d_stage "$(d_port)" "$([[ $os == Darwin ]] && echo yes || echo no)"
    d_swap
  fi
  exec "$d_dir/bana-manager" daemon --home "$base_home" --no-tray
}

# What PROJECT's daemon does, a line each (its /local).
d_status_project() { # PORT PROJECT
  local flat v h now
  flat=$(d_curl "$1" "/ci/v1/p/$2/local" | d_flat) || { echo "  $2: it did not say what it does"; return 0; }
  echo "$2:"
  now=$(d_val "$flat" now)
  v=$(d_val "$flat" watcher.fetched_at)
  if [[ $v =~ ^[0-9]+$ && $now =~ ^[0-9]+$ ]]; then echo "  fetched $(d_ago $((now - v))) ago"; else echo "  not fetched yet"; fi
  [[ $(d_val "$flat" watcher.paused) != true ]] || echo "  paused: automatic builds wait"
  [[ $(d_val "$flat" watcher.docker) != false ]] || echo "  Docker does not answer: builds wait"
  v=$(d_val "$flat" watcher.fetch_error)
  [[ -z $v || $v == null ]] || echo "  fetch: $v"
  v=$(d_val "$flat" watcher.lock_holder)
  [[ -z $v || $v == null ]] || echo "  act is busy with $v"
  v=$(d_val "$flat" watcher.post_error)
  [[ -z $v || $v == null ]] || echo "  statuses: $v ($(d_val "$flat" watcher.unposted) not posted)"
  # A one-job build (Run JOB… on the page) says its job: " · job JOB".
  v=$(d_val "$flat" running.id)
  if [[ -n $v ]]; then
    h=$(d_val "$flat" running.job)
    echo "  running: #$v $(d_val "$flat" running.ref)${h:+ · job $h} ($(d_val "$flat" running.tier), $(d_ago "$(d_val "$flat" running.elapsed)")): $(d_text "$flat" running.description)"
  else
    echo "  running: nothing"
  fi
  v=$(d_val "$flat" 'queue.#')
  h=$(d_val "$flat" queue.0.job)
  [[ ${v:-0} == 0 ]] || echo "  queued: $v (next: #$(d_val "$flat" queue.0.id) $(d_val "$flat" queue.0.ref)${h:+ · job $h})"
  v=$(d_val "$flat" last.id)
  [[ -z $v ]] || echo "  last: #$v $(d_val "$flat" last.ref) $(d_val "$flat" last.state): $(d_text "$flat" last.description)"
  # The release bana asks about (Linux has no menu bar: this line is the ask there).
  v=$(d_val "$flat" release.tag)
  if [[ -n $v && $v != null ]]; then
    case $(d_val "$flat" release.state) in
    asking) echo "  release $v: waiting for your answer (bana daemon open $2)" ;;
    failed)
      h=$(d_val "$flat" release.reason)
      echo "  release $v: publishing failed, waiting for your answer (bana daemon open $2): ${h%%\\n*}" ;;
    publishing) echo "  release $v: publishing" ;;
    building) echo "  release $v: building (#$(d_val "$flat" release.build))" ;;
    blocked) echo "  release $v: not asked: $(d_val "$flat" release.reason)" ;;
    esac
  fi
}

daemon_status() {
  local port h v p
  port=$(d_port)
  # Not installed, but one may answer: bana daemon run.
  if ! d_installed && ! d_up; then
    echo "The daemon is not installed here: bana daemon install"
    return 0
  fi
  if [[ $os == Darwin ]]; then
    launchctl print "gui/$(id -u)/$d_label" >/dev/null 2>&1 && v="launchd: loaded" || v="launchd: not loaded"
  else
    v=$(systemctl --user is-active "$d_unit" 2>/dev/null) || true
    v="systemd: ${v:-no answer}"
  fi
  if ! h=$(d_health "$port") || ! d_is_ours "$h"; then
    echo "The daemon ($v) does not answer on port $port: bana daemon log"
    return 1
  fi
  echo "The daemon ($v): http://127.0.0.1:$port/"
  d_list 1
  while IFS= read -r p; do
    [[ -z $p ]] || d_status_project "$port" "$p"
  done < <(d_curl "$port" /ci/v1/projects 2>/dev/null | grep -E '"error": *null' | sed -n 's/.*"prefix": *"\([a-z0-9-]*\)".*/\1/p')
  d_latest_line "$h"
}

# The projects, a line each: PROJECT REPO STATE QUEUE LAST CHECKOUT FILES, tab-separated.
# From the daemon (UP), else from their files.
d_list_rows() { # UP
  local line flat p s l f
  if [[ -z $1 ]]; then
    for p in $(d_prefixes); do
      f=$(d_project "$p")
      s=down
      [[ ! -e ${f%/settings}/paused ]] || s="down, paused"
      l=$(d_setting checkout "$f") || l=-
      printf '%s\t%s\t%s\t-\t-\t%s\t%s\n' "$p" "$(d_setting repo "$f" || echo -)" "$s" "$(d_tilde "$l")" \
        "$(d_tilde "$(project_home "$p")")"
    done
    return 0
  fi
  d_curl "$(d_port)" /ci/v1/projects | while IFS= read -r line; do
    [[ $line == '{'* ]] || continue
    flat=$(d_flat <<<"$line")
    p=$(d_val "$flat" prefix)
    s=$(d_val "$flat" error)
    if [[ $s != null ]]; then s="error: $s"
    elif [[ $(d_val "$flat" paused) == true ]]; then s=paused
    else s=active; fi
    l=$(d_val "$flat" last.id)
    if [[ -n $l && $l != null ]]; then l="#$l $(d_val "$flat" last.state)"; else l=-; fi
    f=$(d_val "$flat" checkout)
    [[ -n $f && $f != null ]] || f=-
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$p" "$(d_val "$flat" repo)" "$s" "$(d_val "$flat" queue)" "$l" \
      "$(d_tilde "$f")" "$(d_tilde "$(project_home "$p")")"
  done
}

# bana list: what each project added here does; FILES is where bana keeps its clone,
# builds and state.
daemon_list() {
  local h
  if h=$(d_health "$(d_port)") && d_is_ours "$h"; then d_list 1; else h='' && d_list ''; fi
  d_latest_line "$h"
}
d_list() { # UP
  local up=$1 rows
  rows=$(d_list_rows "$up") || rows=''
  if [[ -z $rows ]]; then
    echo "No projects yet: bana add, in a project's checkout, adds one."
  else
    printf 'PROJECT\tREPO\tSTATE\tQUEUE\tLAST\tCHECKOUT\tFILES\n%s\n' "$rows" |
      awk -F'\t' '{ for (k = 1; k <= NF; k++) { c[NR, k] = $k; if (length($k) > w[k]) w[k] = length($k) } n = NF }
        END { for (i = 1; i <= NR; i++) { line = ""; for (k = 1; k < n; k++) line = line sprintf("%-" w[k] "s  ", c[i, k]); print line c[i, n] } }'
  fi
  [[ -n $up ]] || echo "(The daemon does not answer on port $(d_port): bana daemon install starts it.)"
}

# This checkout's project (else the one named), added here.
d_need_added() {
  [[ -n $repo || -n ${picked:-} ]] || die "Which project? Name one (bana list), or run this in its checkout"
  [[ -f $(d_project "$prefix") ]] || die "$prefix is not added here: bana add (in its checkout) adds it, bana list lists what is"
}

# The project's CI here goes: its settings and pause (the daemon drops it, and cancels its
# build), its push hook and Claude Code's tools in its checkout; PURGE: its clone, builds,
# fixes and state too. Never its runners (bana up's) or vars (yours).
# A project removed before (no settings) has only its files left: PURGE removes them.
d_remove() { # PREFIX [PURGE]
  local dir r='' checkout='' d gits
  dir=$(project_home "$1")
  if [[ -f $(d_project "$1") ]]; then
    r=$(d_setting repo "$(d_project "$1")") || r=''
    ! d_setting split.repo "$(d_project "$1")" | grep -q . ||
      warn "$1's bana split stays on GitHub: its deploy key on $r and $(d_setting split.repo "$(d_project "$1")")'s secrets (delete them there, or add $1 again and bana split off)"
    checkout=$(d_setting checkout "$(d_project "$1")") || checkout=
    rm -rf "$dir/daemon"
    if d_up; then d_rescan >/dev/null || warn "The daemon did not answer: it drops $1 within a minute"; fi
    if [[ -n $checkout && -d $checkout ]]; then
      d_hook_remove "$checkout"
      d_claude_remove "$checkout"
    else
      warn "$1's checkout ${checkout:-(none)} is gone: its push hook and Claude Code's bana server stay there"
    fi
  fi
  rm -rf "$dir/daemon"
  if [[ -n ${2:-} ]]; then
    # The repositories its fixes' worktrees are of (the checkout, as it was), to prune after.
    gits=$(for d in "$dir"/fix/*/; do [[ ! -e $d.git ]] || (cd "$d" && cd "$(git rev-parse --git-common-dir)" && pwd -P) 2>/dev/null; done | sort -u)
    for d in src builds fix releases act-cache state.json daemon.lock; do rm -rf "${dir:?}/$d"; done
    rmdir "$dir" 2>/dev/null || true
    [[ -z $checkout || ! -d $checkout ]] || git -C "$checkout" worktree prune 2>/dev/null || true
    while IFS= read -r d; do [[ -z $d ]] || git --git-dir="$d" worktree prune 2>/dev/null || true; done <<<"$gits"
    say "Removed $1${r:+ ($r)}, with its clone, builds, fixes and state."
  else
    say "Removed $1${r:+ ($r)}: its builds and clone stay in $(d_tilde "$dir") (bana remove $1 --purge removes them)."
  fi
}

daemon_remove() {
  local purge=''
  while (($#)); do
    case $1 in --purge) purge=1 ;; *) project_usage ;; esac
    shift
  done
  # Removed before: --purge removes the files left.
  if [[ -n $purge && ! -f $(d_project "$prefix") ]] && { [[ -n $repo || -n ${picked:-} ]] && project_left "$prefix"; }; then
    d_remove "$prefix" 1
    return
  fi
  d_need_added
  ! d_setting split.repo "$(d_project "$prefix")" | grep -q . ||
    die "$prefix builds and releases through bana split ($(d_setting split.repo "$(d_project "$prefix")")): bana split off first"
  d_remove "$prefix" "$purge"
}

daemon_pause() { # pause|resume
  local flag row q
  (($# == 1)) || project_usage
  d_need_added
  flag=$(project_home "$prefix")/daemon/paused
  if [[ $1 == pause ]]; then : >"$flag"; else rm -f "$flag"; fi
  if d_up; then
    row=$(d_rescan 2>/dev/null | d_row "$prefix") || row=''
  else
    row=''
  fi
  if [[ $1 == pause ]]; then
    say "$prefix: automatic builds paused; Run now, fixes and releases still work"
  else
    q=$(sed -n 's/.*"queue": *\([0-9]*\).*/\1/p' <<<"$row")
    if [[ ${q:-0} != 0 ]]; then say "$prefix: resumed; $q queued builds start"; else say "$prefix: resumed"; fi
  fi
  d_up || echo "(The daemon does not answer: it reads this when it starts.)"
}

daemon_main() {
  local cmd=${1:-} p
  shift || true
  case $cmd in
  install) daemon_install "$@" ;;
  uninstall) daemon_uninstall "$@" ;;
  run) daemon_run "$@" ;;
  status) daemon_status ;;
  log)
    if [[ $os == Darwin ]]; then
      [[ -f $d_logfile ]] || die "No log yet: $d_logfile"
      exec tail -n 200 -f "$d_logfile"
    fi
    exec journalctl --user -u "$d_unit" -n 200 -f
    ;;
  open)
    (($# <= 1)) || daemon_usage
    [[ -z ${1:-} ]] || pick_project "$1"
    d_health "$(d_port)" >/dev/null || die "The daemon does not answer on port $(d_port): bana daemon status"
    p=''
    [[ -z ${1:-} ]] && ! d_added || p="p=$prefix"
    if [[ $os == Darwin ]]; then open "$(d_url "$(d_port)" "$p")"
    elif command -v xdg-open >/dev/null && [[ -n ${DISPLAY:-}${WAYLAND_DISPLAY:-} ]]; then xdg-open "$(d_url "$(d_port)" "$p")"
    else d_url "$(d_port)" "$p"; fi
    ;;
  poke)
    (($# <= 1)) || daemon_usage
    [[ -z ${1:-} ]] || pick_project "$1"
    if [[ -n ${1:-} ]] || d_added; then p=$prefix; else p=$(d_prefixes); fi
    [[ -n $p ]] || die "No projects yet: bana add, in a project's checkout, adds one"
    for p in $p; do
      d_curl "$(d_port)" "/ci/v1/p/$p/daemon/poll" -X POST >/dev/null || die "The daemon does not answer on port $(d_port): bana daemon status"
      say "The daemon fetches $p now."
    done
    ;;
  *) daemon_usage ;;
  esac
}

# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets os, host, prefix, repo, home, base_home, bana_root, conf_file
# bana daemon: CI on push, on this machine. A daemon (bana-manager daemon) fetches the
# project's pushes, runs each through `bana ci` (act) and posts commit statuses with the
# GitHub CLI. It runs as a LaunchAgent on a Mac (in your login session, with a menu bar
# item) or as a systemd user service on Linux. Sourced by bin/bana.
#
#   bana daemon install [options]   check this machine, build, and start the daemon (again)
#     --port N         the page's port (default 8470, or the one installed)
#     --no-tray        on a Mac: no menu bar item
#     --no-open        on a Mac: don't open the page afterwards
#     --now            restart at once, even while a build runs (it runs again once)
#     --no-hook        don't add the git hook that tells the daemon about your pushes at once
#   bana daemon uninstall [--purge]  stop and remove it; --purge also its clone, builds and state
#   bana daemon run [--build]   in the foreground, for debugging (--build: from this checkout first)
#   bana daemon status          whether it runs, and what it builds
#   bana daemon log             its log, followed
#   bana daemon open            its page
#   bana daemon poke            fetch now, rather than at the next poll
#
# The daemon fetches every daemon.poll seconds. Pushes from this checkout reach it at once:
# install adds a reference-transaction hook here, which git runs when a push updates
# origin/<branch>, and which then asks the daemon to fetch (as bana daemon poke does).
#
# bana.conf's daemon.* keys say which pushes run (`bana settings` lists them). They and
# the rest of the settings are read at install: run install again after changing them.
# The daemon runs a snapshot of bana in ~/.bana/<prefix>/daemon, and builds in its own
# clone, ~/.bana/<prefix>/src: your checkout stays yours. Only Fix with Claude on a failed
# build (the page, 🧱) writes there: a bana/fix-<sha7> branch and its worktree under
# ~/.bana/<prefix>/fix, in the checkout install ran in.

daemon_usage() { awk '/^#   bana daemon/, /^#   bana daemon poke/ { sub(/^# ?/, ""); print }' "$bana_root/lib/daemon.sh" >&2; exit 2; }

d_snap=$home/daemon
d_settings=$d_snap/settings
d_label=xyz.tjrb.bana.$prefix
d_plist=$HOME/Library/LaunchAgents/$d_label.plist
d_logfile=$HOME/Library/Logs/bana/$prefix.log
d_unit=bana-$prefix.service
d_unit_file=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$d_unit
# Seconds between asks while install waits for the daemon to answer (60 asks), and a
# tenth of those while it waits for a build to end. Tests make it 0.
d_step=${BANA_DAEMON_STEP:-1}

# A key of the installed settings file.
d_setting() { # KEY
  [[ -f $d_settings ]] || return 1
  awk -v k="$1" '
    /^[ \t]*#/ { next }
    { i = index($0, "=") } i == 0 { next }
    { key = substr($0, 1, i - 1); gsub(/^[ \t]+|[ \t]+$/, "", key) }
    key == k { v = substr($0, i + 1); gsub(/^[ \t]+|[ \t\r]+$/, "", v); found = 1 }
    END { if (found) print v; exit !found }' "$d_settings"
}
d_port() { d_setting port || echo 8470; }
d_token() { cat "$base_home/manager-token" 2>/dev/null || true; }
d_url() { # PORT [FRAGMENT]
  local t
  t=$(d_token)
  echo "http://127.0.0.1:$1/${t:+#token=$t}${2:+${t:+&}$2}"
}
d_installed() { [[ -f $d_settings ]] && [[ -f $d_plist || -f $d_unit_file ]]; }

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
d_is_ours() { grep -Eq "\"prefix\": *\"$prefix\"" <<<"$1"; }

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
    git rev-parse --show-toplevel 2>/dev/null || die "Run bana daemon install in the project's checkout"
  fi
}

# git with the GitHub CLI as its only credential helper, as the daemon fetches.
d_git() { # GH GIT-ARGS...
  local gh=$1
  shift
  GIT_TERMINAL_PROMPT=0 git -c credential.helper= -c "credential.helper=!'$gh' auth git-credential" "$@"
}

# What the daemon needs here, and what in the workflow would go wrong under it.
d_doctor() { # ROOT
  local root=$1 wf name v gh runners=() d n line
  say "Checking this machine for $repo's daemon"
  command -v act >/dev/null || die "act is needed: brew install act (https://nektosact.com)"
  v=$(act --version 2>/dev/null | head -1)
  echo "  act: ${v:-unknown version} ($(command -v act))"
  command -v docker >/dev/null || die "Docker is needed: install OrbStack (https://orbstack.dev)"
  if docker info >/dev/null 2>&1; then echo "  Docker: running"
  else warn "  Docker is not running: builds wait for it (start OrbStack)"; fi
  gh=$(command -v gh) || die "The GitHub CLI is needed: brew install gh, then gh auth login"
  v=$(gh auth status 2>&1) || die "The GitHub CLI is not signed in: gh auth login"
  # A classic token lists its scopes; statuses need repo (a fine-grained one lists none).
  if grep -q 'Token scopes:' <<<"$v" && ! grep -Eq "Token scopes:.*'repo'" <<<"$v"; then
    warn "  gh's token lacks the repo scope, which posting statuses needs: gh auth refresh -s repo"
  fi
  echo "  GitHub CLI: signed in"
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
  ((n)) || echo "  $name: runs as workflow_dispatch, nothing to change"
}

# ---- install -----------------------------------------------------------------------------

# bana-manager, built from this checkout (BANA_DAEMON_BIN: a given one).
d_build() {
  if [[ -n ${BANA_DAEMON_BIN:-} ]]; then
    [[ -x $BANA_DAEMON_BIN ]] || die "BANA_DAEMON_BIN: no program $BANA_DAEMON_BIN"
    echo "$BANA_DAEMON_BIN"
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

# The snapshot the daemon runs: a branch switch in your checkout never changes it.
d_snapshot() { # BIN
  local f
  d_put "$1" "$d_snap/bana-manager" 755
  d_put "$bana_root/bin/bana" "$d_snap/bin/bana" 755
  for f in "$bana_root"/lib/*.sh; do d_put "$f" "$d_snap/lib/$(basename "$f")" 644; done
}

# The daemon's own clone: made from your checkout (quick), then pointed at GitHub.
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

# The PATH the daemon runs with: bana.conf's path first, then this shell's.
d_path() {
  local d p=$PATH
  for d in $(words "$(conf path)"); do
    case $d in "~"/*) d=$HOME/${d#\~/} ;; esac
    p=$d:$p
  done
  echo "$p"
}

# daemon/settings: only the keys the daemon takes (another is an error there). ROOT is this
# checkout: the page's Fix with Claude makes its worktrees and branches in it.
d_write_settings() { # PORT TRAY PATH GH ROOT
  local port=$1 tray=$2 path=$3 gh=$4 root=$5 tiers k v login p b=''
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
  login=$(gh api user --jq .login 2>/dev/null || true)
  # The bana commit the snapshot is of (a fix's brief names it), unless bana is a copy
  # inside another repository.
  if [[ $(git -C "$bana_root" rev-parse --show-toplevel 2>/dev/null) == "$(cd "$bana_root" && pwd -P)" ]]; then
    b=$(git -C "$bana_root" rev-parse HEAD 2>/dev/null) || b=''
  fi
  mkdir -p "$d_snap"
  {
    echo "# Written by bana daemon install ($(date '+%Y-%m-%d %H:%M')); run it again to change this."
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
    echo "port = $port"
    echo "host = $host"
    [[ ! $login =~ ^[A-Za-z0-9-]+$ ]] || echo "login = $login"
    echo "path = $path"
    echo "tray = $tray"
    [[ -z ${BANA_HOME:-} ]] || echo "home = $base_home"
    echo "gh = $gh"
    # caffeinate keeps a Mac awake while act runs.
    for k in git docker bash $([[ $os != Darwin ]] || echo caffeinate); do
      p=$(PATH=$path command -v "$k" 2>/dev/null) || p=
      [[ $p == /* ]] || die "$k is not on the daemon's PATH ($path)"
      echo "$k = $p"
    done
    echo "script = $d_snap/bin/bana"
    echo "checkout = $root"
    [[ -z $b ]] || echo "bana_commit = $b"
  } >"$d_settings.new.$$"
  mv -f "$d_settings.new.$$" "$d_settings"
}

d_xml() { printf '%s' "$1" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g'; }

d_write_plist() { # PATH TRAY
  local a args=("$d_snap/bana-manager" daemon --dir "$home")
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
  for a in "$d_snap/bana-manager" daemon --dir "$home" --no-tray; do exec+="${exec:+ }$(d_unit_word "$a" exec)"; done
  mkdir -p "$(dirname "$d_unit_file")"
  {
    cat <<EOF
[Unit]
Description=bana: CI on push for $repo ($prefix)

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

# Waits while the installed daemon runs a build (a restart would interrupt it).
d_wait_build() { # PORT
  local flat id said=''
  while flat=$(d_curl "$1" /ci/v1/local 2>/dev/null | d_flat) && id=$(d_val "$flat" running.id) && [[ -n $id ]]; do
    if [[ -z $said ]]; then
      say "Build #$id ($(d_val "$flat" running.ref)) runs: restarting the daemon when it ends (--now: restart now; it runs again)"
      said=1
    fi
    sleep $((d_step * 10))
  done
}

# Stops the LaunchAgent, and waits until launchd has let go of it: bootout may return
# while the daemon still stops a build (up to ExitTimeOut). A bootstrap before then
# fails, or starts a daemon that finds the old one on its port and leaves for good.
d_bootout() {
  local k uid
  uid=$(id -u)
  launchctl bootout "gui/$uid/$d_label" 2>/dev/null || true
  for ((k = 0; k < 70; k++)); do
    launchctl print "gui/$uid/$d_label" >/dev/null 2>&1 || return 0
    sleep "$d_step"
  done
  warn "launchd still has $d_label after 70 s"
}

# Starts the service, or starts it again with the new snapshot and settings.
d_service() { # PATH TRAY
  local uid was=''
  uid=$(id -u)
  if [[ $os == Darwin ]]; then
    d_write_plist "$1" "$2"
    d_bootout
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

# Everything the daemon needs but its service: checks, build, snapshot, clone, settings.
# The push hook: git runs reference-transaction for every ref change; a push that went
# through updates refs/remotes/origin/*, "committed". It must never fail (a non-zero exit
# in "prepared" aborts the change) and never make the push wait, so it pokes in the
# background, with the token on stdin.
d_hook_mark='# bana: tells the daemon about your pushes'
d_hook_file() { # ROOT
  local dir
  dir=$(git -C "$1" rev-parse --git-path hooks) || return 1
  case $dir in /*) ;; *) dir=$1/$dir ;; esac
  echo "$dir/reference-transaction"
}
d_hook_install() { # ROOT PORT
  local f
  f=$(d_hook_file "$1") || return 0
  if [[ -f $f ]] && ! grep -qF "$d_hook_mark" "$f"; then
    warn "$f is your own hook: pushes reach the daemon at its next poll (or: bana daemon poke)"
    return 0
  fi
  mkdir -p "$(dirname "$f")"
  cat >"$f.bana" <<HOOK
#!/bin/sh
$d_hook_mark (bana daemon install added it; bana daemon uninstall removes it).
[ "\$1" = committed ] || exit 0
case "\$(cat)" in *" refs/remotes/origin/"*) ;; *) exit 0 ;; esac
( printf 'Authorization: Bearer %s\\n' "\$(cat '$base_home/manager-token' 2>/dev/null)" |
  curl -fsS --noproxy '*' --max-time 5 -H @- -X POST http://127.0.0.1:$2/ci/v1/daemon/poll ) >/dev/null 2>&1 &
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

d_prepare() { # PORT TRAY
  local root bin gh path
  root=$(d_root)
  d_doctor "$root"
  bin=$(d_build)
  gh=$(command -v gh)
  d_snapshot "$bin"
  d_clone "$root" "$gh"
  path=$(d_path)
  d_write_settings "$1" "$2" "$path" "$gh" "$root"
  echo "  settings: $d_settings"
}

daemon_install() {
  local port='' tray=yes open=1 now='' hook=1 h k path
  [[ $os == Darwin ]] || tray=no
  while (($#)); do
    case $1 in
    --port) port=${2:?--port N}; shift ;;
    --no-tray) tray=no ;;
    --no-open) open='' ;;
    --now) now=1 ;;
    --no-hook) hook='' ;;
    *) daemon_usage ;;
    esac
    shift
  done
  port=${port:-$(d_port)}
  if ! [[ $port =~ ^[0-9]+$ ]] || ((port < 1 || port > 65535)); then die "--port: a port number, not '$port'"; fi
  # The port must be free, or this project's daemon's.
  if h=$(curl -fsS --noproxy '*' --max-time 3 "http://127.0.0.1:$port/ci/v1/health" 2>/dev/null); then
    if ! grep -Eq '"daemon": *true' <<<"$h" || ! d_is_ours "$h"; then
      die "Something else serves port $port (another project's daemon, or bana manager): pass --port"
    fi
  fi
  d_prepare "$port" "$tray"
  if [[ -n $hook ]]; then d_hook_install "$(d_root)" "$port"; else d_hook_remove "$(d_root)"; fi
  path=$(d_setting path)
  [[ -n $now ]] || ! d_installed || d_wait_build "$port"
  d_service "$path" "$tray"
  say "Waiting for the daemon on port $port"
  for ((k = 0; k < 60; k++)); do
    h=$(d_health "$port") && d_is_ours "$h" && break
    h=
    sleep "$d_step"
  done
  if [[ -z $h ]]; then
    die "The daemon does not answer on port $port: 'bana daemon log' says why"
  fi
  say "The daemon runs: $repo's pushes build here. Its page: $(d_url "$port")"
  if [[ $os == Darwin && -n $open ]]; then open "$(d_url "$port")" || true; fi
}

daemon_uninstall() {
  local purge='' d
  case ${1:-} in '') ;; --purge) purge=1 ;; *) daemon_usage ;; esac
  if [[ $os == Darwin ]]; then
    # launchd stops it (SIGTERM): a running build stops, and is not run again.
    d_bootout
    rm -f "$d_plist"
  elif [[ -f $d_unit_file ]]; then
    systemctl --user disable --now "$d_unit" 2>/dev/null || true
    rm -f "$d_unit_file"
    systemctl --user daemon-reload 2>/dev/null || true
  fi
  rm -rf "$d_snap"
  d_hook_remove "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
  if [[ -n $purge ]]; then
    for d in src builds act-cache state.json daemon.lock; do rm -rf "${home:?}/$d"; done
    rm -f "$d_logfile"
    say "The daemon for $repo is gone, with its clone, builds and state."
  else
    say "The daemon for $repo is gone; its builds and clone stay in $home (--purge removes them)."
  fi
}

daemon_run() {
  local build=''
  case ${1:-} in '') ;; --build) build=1 ;; *) daemon_usage ;; esac
  if [[ -n $build || ! -x $d_snap/bana-manager || ! -f $d_settings ]]; then
    d_prepare "$(d_port)" "$([[ $os == Darwin ]] && echo yes || echo no)"
  fi
  exec "$d_snap/bana-manager" daemon --dir "$home" --no-tray
}

daemon_status() {
  local port h flat v now
  port=$(d_port)
  if ! [[ -f $d_settings ]]; then
    echo "No daemon for $prefix here: bana daemon install"
    return 0
  fi
  if [[ $os == Darwin ]]; then
    launchctl print "gui/$(id -u)/$d_label" >/dev/null 2>&1 && v="launchd: loaded" || v="launchd: not loaded"
  else
    v="systemd: $(systemctl --user is-active "$d_unit" 2>/dev/null || true)"
  fi
  if ! h=$(d_health "$port") || ! d_is_ours "$h"; then
    echo "The daemon for $repo ($v) does not answer on port $port: bana daemon log"
    return 1
  fi
  echo "The daemon for $repo ($v): http://127.0.0.1:$port/"
  flat=$(d_curl "$port" /ci/v1/local | d_flat) || die "It did not say what it does (/ci/v1/local)"
  now=$(d_val "$flat" now)
  v=$(d_val "$flat" watcher.fetched_at)
  if [[ $v =~ ^[0-9]+$ && $now =~ ^[0-9]+$ ]]; then echo "  fetched $(d_ago $((now - v))) ago"; else echo "  not fetched yet"; fi
  [[ $(d_val "$flat" watcher.paused) != true ]] || echo "  paused: new builds wait"
  [[ $(d_val "$flat" watcher.docker) != false ]] || echo "  Docker does not answer: builds wait"
  v=$(d_val "$flat" watcher.fetch_error)
  [[ -z $v || $v == null ]] || echo "  fetch: $v"
  v=$(d_val "$flat" watcher.lock_holder)
  [[ -z $v || $v == null ]] || echo "  act is busy with $v"
  v=$(d_val "$flat" watcher.post_error)
  [[ -z $v || $v == null ]] || echo "  statuses: $v ($(d_val "$flat" watcher.unposted) not posted)"
  v=$(d_val "$flat" running.id)
  if [[ -n $v ]]; then
    echo "  running: #$v $(d_val "$flat" running.ref) ($(d_val "$flat" running.tier), $(d_ago "$(d_val "$flat" running.elapsed)")): $(d_val "$flat" running.description)"
  else
    echo "  running: nothing"
  fi
  v=$(d_val "$flat" 'queue.#')
  [[ ${v:-0} == 0 ]] || echo "  queued: $v (next: #$(d_val "$flat" queue.0.id) $(d_val "$flat" queue.0.ref))"
  v=$(d_val "$flat" last.id)
  [[ -z $v ]] || echo "  last: #$v $(d_val "$flat" last.ref) $(d_val "$flat" last.state): $(d_val "$flat" last.description)"
}

daemon_main() {
  local cmd=${1:-}
  shift || true
  case $cmd in install | uninstall | run | status | log | open | poke) need_repo ;; esac
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
    d_health "$(d_port)" >/dev/null || die "The daemon does not answer on port $(d_port): bana daemon status"
    if [[ $os == Darwin ]]; then open "$(d_url "$(d_port)")"
    elif command -v xdg-open >/dev/null && [[ -n ${DISPLAY:-}${WAYLAND_DISPLAY:-} ]]; then xdg-open "$(d_url "$(d_port)")"
    else d_url "$(d_port)"; fi
    ;;
  poke)
    d_curl "$(d_port)" /ci/v1/daemon/poll -X POST >/dev/null || die "The daemon does not answer on port $(d_port): bana daemon status"
    say "The daemon fetches now."
    ;;
  *) daemon_usage ;;
  esac
}

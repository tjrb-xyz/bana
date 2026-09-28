# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets bana_root, self, os, host, prefix, repo, linux_user
# bana tart: a Linux VM in Tart (https://github.com/openai/tart) on an Apple-silicon
# Mac, made, kept running and joined to the pool in one command. Sourced by bin/bana.
#
#   bana tart up [options]   make the VM (once), start it, and register this project's runners in it
#     --linux N        runners in it (default 1)
#     --cpus N         (default 4)     --memory GiB   (default 8)     --disk GiB   (default 64)
#     --name NAME      the VM's name in Tart (default: tart_name in bana.conf, else bana-tart)
#     --image IMAGE    (default ghcr.io/cirruslabs/debian:latest)
#     --dedicated      jobs in it see BANA_DEDICATED=1
#     --token T        a registration token (else the GitHub CLI fetches one)
#   bana tart status         the VM, and this project's runners from it in the pool
#   bana tart log            the runners' service log in the VM
#   bana tart shell          a shell in the VM
#   bana tart down           this project's runners in it leave the pool (the VM stays)
#   bana tart delete         the VM is deleted, and this project's runners from it leave the pool
#
# Needs macOS 14+ on Apple silicon (the VM is arm64: labels <prefix>-linux, linux-arm64),
# Homebrew (to install Tart), and the GitHub CLI signed in as a repository admin (or
# --token). A LaunchAgent keeps the VM running while you are logged in. Tart has no
# USB passthrough: hardware jobs need a runner on the Mac itself or a Proxmox machine.

tart_name=$(conf tart_name bana-tart)
tart_image=ghcr.io/cirruslabs/debian:latest

tart_usage() { awk '/^#   bana tart/, /^# --token/ { sub(/^# ?/, ""); print }' "$bana_root/lib/tart.sh" >&2; exit 2; }

# The VM's host name: its runners are named after it (<prefix>-<mac>-tart-linux-arm64-<n>).
tart_host() { echo "$host-tart"; }
tart_label() { echo "xyz.bana.tart.$tart_name"; }
tart_plist() { echo "$HOME/Library/LaunchAgents/$(tart_label).plist"; }
tart_exists() { tart list --source local --quiet 2>/dev/null | grep -qx "$tart_name"; }

# This project's runners from the VM, as GitHub sees them: "id name status" lines.
tart_pool() {
  gh_ok || return 0
  gh api "repos/$repo/actions/runners?per_page=100" --jq \
    ".runners[] | select(.name | startswith(\"$prefix-$(tart_host)-linux-\")) | \"\(.id) \(.name) \(.status)\""
}

# Keeps `tart run` going: while you are logged in, and again after a restart.
tart_keep_running() {
  local bin logs
  bin=$(command -v tart)
  logs=$HOME/Library/Logs/bana
  mkdir -p "$logs" "$(dirname "$(tart_plist)")"
  cat >"$(tart_plist)" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$(tart_label)</string>
  <key>ProgramArguments</key>
  <array><string>$bin</string><string>run</string><string>$tart_name</string><string>--no-graphics</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$logs/tart-$tart_name.log</string>
  <key>StandardErrorPath</key><string>$logs/tart-$tart_name.log</string>
</dict>
</plist>
EOF
  launchctl bootout "gui/$(id -u)/$(tart_label)" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$(tart_plist)"
}

# `tart exec` answers once the VM has booted and its guest agent runs.
tart_wait() {
  local k
  for ((k = 0; k < 90; k++)); do
    tart exec "$tart_name" true >/dev/null 2>&1 && return 0
    sleep 2
  done
  die "The VM did not answer within 3 minutes: see ~/Library/Logs/bana/tart-$tart_name.log"
}

tart_copy() { # FILE PATH-IN-VM MODE
  tart exec -i "$tart_name" sh -c 'cat >/tmp/bana-copy' <"$1"
  tart exec "$tart_name" sudo install -D -m "$3" /tmp/bana-copy "$2"
}

tart_up() {
  local n=1 cpus=4 mem=8 disk=64 token v hook envs=() line
  while (($#)); do
    case $1 in
    --linux) n=${2:?}; shift ;;
    --cpus) cpus=${2:?}; shift ;;
    --memory) mem=${2:?}; shift ;;
    --disk) disk=${2:?}; shift ;;
    --image) tart_image=${2:?}; shift ;;
    --dedicated) dedicated=1 ;;
    --token) export BANA_TOKEN=${2:?}; shift ;;
    *) tart_usage ;;
    esac
    shift
  done
  for v in "$n" "$cpus" "$mem" "$disk"; do
    [[ $v =~ ^[1-9][0-9]*$ ]] || die "--linux, --cpus, --memory and --disk take a number"
  done
  need_repo
  (($(sw_vers -productVersion | cut -d. -f1) >= 14)) || die "macOS 14 or later is needed (tart exec)"
  if ! command -v tart >/dev/null; then
    command -v brew >/dev/null || die "Tart is needed: brew install openai/tools/tart (Homebrew: https://brew.sh)"
    say "Installing Tart"
    brew install openai/tools/tart
  fi
  token=$(token_for registration) # before the long download, so a missing token stops it early

  if tart_exists; then
    say "Tart has $tart_name already: adding $prefix's runners to it"
  else
    say "Getting $tart_image (the first time downloads about 2 GB)"
    tart clone "$tart_image" "$tart_name"
    tart set "$tart_name" --cpu "$cpus" --memory "$((mem * 1024))" --disk-size "$disk"
    say "Starting $tart_name ($cpus CPUs, $mem GiB, a $disk GB disk), kept running by launchd"
  fi
  launchctl print "gui/$(id -u)/$(tart_label)" >/dev/null 2>&1 || tart_keep_running
  tart_wait

  say "Setting it up: what CI needs, then $n runner(s) (about 10 minutes the first time)"
  tart exec "$tart_name" sudo hostnamectl set-hostname "$(tart_host)"
  tart_copy "$self" /usr/local/sbin/bana 0755
  hook=$(conf_path hook.linux)
  if [[ -n $hook ]]; then
    tart_copy "$hook" "/usr/local/share/bana/$prefix-hook-linux.sh" 0644
    hook=/usr/local/share/bana/$prefix-hook-linux.sh
  fi
  while IFS= read -r line; do envs+=("$line"); done < <(BANA_TOKEN=$token no_usb=1 settings_env)
  # As root: it makes the user the runners run as (systemd services).
  tart exec "$tart_name" sudo env "${envs[@]}" BANA_HOST="$(tart_host)" BANA_HOOK_LINUX="$hook" \
    BANA_DEDICATED_UP="$dedicated" /usr/local/sbin/bana up --linux "$n"
  say "Done. $tart_name runs while you are logged in; 'bana manager' shows its runners in the pool."
  tart_pool | sed 's/^[0-9]* /  /'
}

tart_status() {
  local ip=
  if ! tart_exists; then
    say "Tart has no VM named $tart_name."
  else
    ip=$(tart ip "$tart_name" 2>/dev/null || true)
    say "Tart: $tart_name is $(tart list --source local --format json 2>/dev/null |
      tr '{' '\n' | grep "\"Name\" *: *\"$tart_name\"" | sed -n 's/.*"State" *: *"\([^"]*\)".*/\1/p' | head -1)${ip:+, at $ip}"
    if launchctl print "gui/$(id -u)/$(tart_label)" >/dev/null 2>&1; then
      say "launchd keeps it running ($(tart_label))"
    else
      say "launchd does not keep it running: 'bana tart up' again sets that up"
    fi
  fi
  need_repo
  say "$prefix's runners from it in the pool:"
  tart_pool | sed 's/^[0-9]* /  /'
}

# This project's runners leave the pool (from inside the VM, as their user).
tart_down() {
  local token envs=() line
  need_repo
  tart_exists || die "Tart has no VM named $tart_name"
  token=$(token_for remove)
  while IFS= read -r line; do envs+=("$line"); done < <(BANA_TOKEN=$token settings_env)
  tart exec "$tart_name" sudo -iu "$linux_user" env "${envs[@]}" BANA_HOST="$(tart_host)" \
    bash /usr/local/sbin/bana down-here || warn "Could not reach the runners in $tart_name"
  tart_forget
}

# Removes what is left of this project's runners from the pool (they would show as offline forever).
tart_forget() {
  local id runner
  while read -r id runner _; do
    [[ -n $id ]] || continue
    say "Removing $runner from the pool"
    gh api -X DELETE "repos/$repo/actions/runners/$id" >/dev/null
  done < <(tart_pool)
}

tart_delete() {
  launchctl bootout "gui/$(id -u)/$(tart_label)" 2>/dev/null || true
  rm -f "$(tart_plist)"
  if tart_exists; then
    say "Stopping and deleting the Tart VM $tart_name"
    tart stop "$tart_name" 2>/dev/null || true
    tart delete "$tart_name"
  fi
  if [[ -n $repo ]]; then tart_forget; fi
  say "Gone."
}

tart_main() {
  local cmd=${1:-} args=()
  shift || true
  [[ $os == Darwin && $(uname -m) == arm64 ]] || die "bana tart needs an Apple-silicon Mac; on Linux use bana up"
  # --name goes with every command.
  while (($#)); do
    case $1 in
    --name) tart_name=${2:?--name NAME}; shift 2 ;;
    *) args+=("$1"); shift ;;
    esac
  done
  [[ $tart_name =~ ^[A-Za-z0-9._-]+$ ]] || die "--name: letters, digits, '.', '_' and '-'"
  case $cmd in
  up) tart_up ${args[@]+"${args[@]}"} ;;
  status) tart_status ;;
  log) tart exec "$tart_name" sudo journalctl -u 'actions.runner.*' -n 200 --no-pager ;;
  shell) tart exec -i -t "$tart_name" bash -l ;;
  down) tart_down ;;
  delete) tart_delete ;;
  *) tart_usage ;;
  esac
}

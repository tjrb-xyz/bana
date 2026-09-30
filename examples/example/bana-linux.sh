#!/usr/bin/env bash
# example's Linux runner setup (bana.conf's hook.linux): runs in each Linux machine
# (an OrbStack machine, a Tart VM, a Proxmox VM or container), as the runners'
# user, after bana installed packages.linux and before its runners register.
set -euo pipefail
if ! command -v rustup >/dev/null && [[ ! -x $HOME/.cargo/bin/rustup ]]; then
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# Jobs install what they miss with sudo (scripts/ci.sh apt, loginctl): it must not ask.
sudo -n true 2>/dev/null ||
  { echo "$(id -un) needs passwordless sudo for example's jobs (or run 'bana up' as root, which makes a user that has it)" >&2; exit 1; }

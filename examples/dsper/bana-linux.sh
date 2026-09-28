#!/usr/bin/env bash
# dsper's Linux runner setup (bana.conf's hook.linux): runs in each Linux machine
# (an OrbStack machine, a Tart VM, a Proxmox VM or container), as the runners'
# user, after bana installed packages.linux and before its runners register.
set -euo pipefail
if ! command -v rustup >/dev/null && [[ ! -x $HOME/.cargo/bin/rustup ]]; then
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
fi

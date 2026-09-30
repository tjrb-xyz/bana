#!/usr/bin/env bash
# example's macOS runner setup (bana.conf's hook.mac): runs on the Mac before
# `bana up` registers its macOS runner, in .github/ of example's checkout.
set -euo pipefail
root=$(git rev-parse --show-toplevel)

command -v brew >/dev/null || { echo "Homebrew is needed (libroc's build tools): https://brew.sh" >&2; exit 1; }
for p in scons ragel cmake; do brew list "$p" >/dev/null 2>&1 || brew install "$p"; done
command -v rustup >/dev/null || { echo "rustup is needed (jobs pick their toolchain): https://rustup.rs" >&2; exit 1; }
if [[ ! -d /Library/Audio/Plug-Ins/HAL/examplestream16ch.driver ]]; then
  echo "Installing example's devices: the macOS job tests them (asks for your password)"
  "$root/scripts/mac.sh" driver
fi
# A dedicated Mac installs each new build of the devices from its LaunchAgent, where
# sudo cannot ask for a password.
if [[ -n ${BANA_DEDICATED:-} ]] && ! sudo -n true 2>/dev/null; then
  echo "A dedicated Mac needs passwordless sudo for $(id -un) (driver installs): sudo visudo -f /etc/sudoers.d/bana" >&2
  exit 1
fi

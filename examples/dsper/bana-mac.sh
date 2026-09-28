#!/usr/bin/env bash
# dsper's macOS runner setup (bana.conf's hook.mac): runs on the Mac before
# `bana up` registers its macOS runner, in .github/ of dsper's checkout.
set -euo pipefail
root=$(git rev-parse --show-toplevel)

command -v brew >/dev/null || { echo "Homebrew is needed (SCons, for libroc): https://brew.sh" >&2; exit 1; }
brew list scons >/dev/null 2>&1 || brew install scons
command -v rustup >/dev/null || { echo "rustup is needed (jobs pick their toolchain): https://rustup.rs" >&2; exit 1; }
if [[ ! -d /Library/Audio/Plug-Ins/HAL/dsperstream16ch.driver ]]; then
  echo "Installing dsper's devices: the macOS job tests them (asks for your password)"
  "$root/scripts/mac.sh" driver
fi

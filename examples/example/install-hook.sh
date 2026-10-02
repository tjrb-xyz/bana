#!/bin/sh
# example's install hook (bana.conf's install.hook): scripts/ci.sh package puts it at the top
# of each build's archive, and the installer bana compiles runs it as: sh install-hook.sh STAGE
#   pre-install     the new version is unpacked in INSTALL_DIR, not yet current
#   post-install    INSTALL_DIR is now current; INSTALL_PREVIOUS is set on an upgrade
#   pre-uninstall   before anything is removed; failing stops the uninstall
#   post-uninstall  after the files are gone (this is a copy of the hook)
# INSTALL_OS, INSTALL_PREFIX, INSTALL_CONFIG, INSTALL_YES and the rest come with it, and
# bana.conf's install.env.EXAMPLE_LOG_DIR. Everything is per-user but the Mac's audio
# devices, which need sudo: the hook asks first (--yes answers for it).
# Again by hand (it finds its own directory, as INSTALL_DIR):
#   sh ~/.local/share/example/current/install-hook.sh post-install
set -eu
stage=${1:?stage}
[ -n "${INSTALL_OS:-}" ] || case $(uname -s) in Darwin) INSTALL_OS=macos ;; *) INSTALL_OS=linux ;; esac
INSTALL_DIR=${INSTALL_DIR:-$(cd "$(dirname "$0")" && pwd)}
INSTALL_PREFIX=${INSTALL_PREFIX:-$(dirname "$INSTALL_DIR")}
[ "${INSTALL_OS:-}" = macos ] || exit 0 # Linux: the .deb's postinst does the system part
hal=${EXAMPLE_HAL_DIR:-/Library/Audio/Plug-Ins/HAL}
drivers='examplesystem2ch exampledaw16ch examplestream16ch examplein16ch'

ask() {
  [ "${INSTALL_YES:-0}" = 1 ] && return 0
  printf '%s [y/N] ' "$1" >&2
  read -r a || return 1
  case $a in [yY]*) return 0 ;; *) return 1 ;; esac
}
same() { # the installed devices are this build's
  for d in $drivers; do diff -r -q "$INSTALL_DIR/driver/$d.driver" "$hal/$d.driver" >/dev/null 2>&1 || return 1; done
}
stop_agent() { launchctl bootout "gui/$(id -u)/xyz.tjrb.exampled" 2>/dev/null || true; }

case $stage in
pre-install) stop_agent ;; # exampled from the old version lets go of the devices
post-install)
  [ -z "${EXAMPLE_LOG_DIR:-}" ] || mkdir -p "$EXAMPLE_LOG_DIR"
  if same; then echo "example's devices are up to date"; exit 0; fi
  if ! ask "Install example's audio devices in $hal? This needs sudo, and Core Audio restarts (sound stops for a moment)."; then
    echo "Devices not installed; later: sh $INSTALL_PREFIX/current/install-hook.sh post-install"
    exit 0
  fi
  for d in $drivers; do
    sudo rm -rf "${hal:?}/$d.driver"
    sudo cp -R "$INSTALL_DIR/driver/$d.driver" "$hal/"
    sudo chown -R root:wheel "$hal/$d.driver"
  done
  sudo killall -9 coreaudiod 2>/dev/null || true
  echo "example's devices are installed"
  ;;
pre-uninstall)
  stop_agent
  [ -d "$hal/examplesystem2ch.driver" ] || exit 0
  ask "Remove example's audio devices from $hal? Core Audio restarts." || exit 0
  for d in $drivers; do sudo rm -rf "${hal:?}/$d.driver"; done
  sudo killall -9 coreaudiod 2>/dev/null || true
  ;;
post-uninstall) echo "example is gone; its settings stay in ${INSTALL_CONFIG:-~/.config/example} (--purge removes them)" ;;
*) echo "unknown stage $stage" >&2; exit 2 ;;
esac

#!/bin/sh
# bana's own installer hook (install.hook of bana's releases; .github/release.sh dist):
# sh lib/install-hook.sh STAGE, run by install.sh with INSTALL_DIR, INSTALL_BIN and the rest.
#
#   pre-install     the daemon, if one is installed here, moves to this bana first (bana daemon
#                   install from the new files); if it does not come up, install.sh stops and
#                   nothing changes. So does a bana command in INSTALL_BIN that is not the
#                   installer's, before the daemon moves. Then a bana command linked into ~/.bana/src (the old
#                   install.sh's checkout) makes way for this one; ~/.bana/.upgrade-from says
#                   where it pointed.
#   pre-uninstall   says that the daemon stays, and how to remove it
#
# BANA_UPGRADE_NOW=1: the daemon restarts at once, without waiting for a running build.
set -eu

home=${BANA_HOME:-$HOME/.bana}

case ${1:-} in
pre-install)
  l=$INSTALL_BIN/bana
  src=$(cd "$home/src" 2>/dev/null && pwd -P) || src=$home/src
  if [ -f "$home/daemon.d/settings" ]; then
    # The command must become this bana too (the installer leaves one not its own alone),
    # or the daemon would run another bana than it: checked before the daemon moves.
    if [ -e "$l" ] || [ -L "$l" ]; then
      to=$(readlink "$l" 2>/dev/null) || to=''
      case $to in
      "$INSTALL_PREFIX/"* | "$home/src/"* | "$src/"*) ;;
      *)
        echo "$l is not this installer's: remove it, or install with --bin-dir DIR" >&2
        exit 1
        ;;
      esac
    fi
    # shellcheck disable=SC2086 # no flag, or --now
    "$INSTALL_DIR/bin/bana" daemon install --no-open ${BANA_UPGRADE_NOW:+--now} || exit 1
  fi
  if [ -L "$l" ]; then
    to=$(readlink "$l")
    case $to in
    "$home/src/"* | "$src/"*)
      printf '%s\n' "$to" >"$home/.upgrade-from"
      rm -f "$l"
      echo "$l pointed into $home/src (the old install.sh's): now this bana's"
      ;;
    esac
  fi
  ;;
pre-uninstall)
  [ ! -f "$home/daemon.d/settings" ] ||
    echo "The daemon keeps running, from $home/daemon.d: $home/daemon.d/bin/bana daemon uninstall removes it"
  ;;
esac

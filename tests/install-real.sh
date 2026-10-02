#!/usr/bin/env bash
# The compiled installer on this machine as it is, with no stand-ins: its own uname, sysctl
# (Rosetta), getconf, tar and sha256 tool (/sbin/sha256sum on macOS 14+, else shasum). On a
# Mac, the quarantine round trip too: an archive a browser downloaded (com.apple.quarantine)
# installs with --from, and nothing installed carries the mark. tests/install.sh covers the
# rest on stand-ins.
#
#   tests/install-real.sh    SH: the shell under test (default sh)
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
bana=$here/../bin/bana
SH=${SH:-sh}
T=$(cd "$(mktemp -d)" && pwd -P)
trap 'rm -rf "$T"' EXIT
n=0 fails=0
ok() { n=$((n + 1)); echo "ok   $*"; }
bad() { n=$((n + 1)); fails=$((fails + 1)); echo "FAIL $*"; sed 's/^/     | /' "$T/out" 2>/dev/null; }
check() { local what=$1; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }
fails_with() { local what=$1; shift; if "$@"; then bad "$what"; else ok "$what"; fi; }

case $(uname -s) in
Darwin) os=macos ;;
Linux) os=linux ;;
*) echo "install-real: not macOS or Linux: nothing to test"; exit 0 ;;
esac
case $(uname -m) in
x86_64 | amd64) cpu=x64 ;;
arm64 | aarch64) cpu=arm64 ;;
*) echo "install-real: no build for $(uname -m): nothing to test"; exit 0 ;;
esac
# A Rosetta shell says x86_64; the Mac is arm64.
[[ $os == macos && $(sysctl -n hw.optional.arm64 2>/dev/null) == 1 ]] && cpu=arm64
echo "shell under test: $SH; this machine: $os-$cpu; tar: $(tar --version 2>&1 | head -1)"

# A release as a project's package job makes one: an archive per CPU, one directory each,
# with the project's hook; rendered by bana installer as the daemon does (--label).
mkdir -p "$T/dist" "$T/project/.github" "$T/home"
for arch in x64 arm64; do
  s=$T/stage/demo-$os-$arch
  mkdir -p "$s/bin"
  printf '#!/bin/sh\necho "demo %s"\n' "$os-$arch" >"$s/bin/demo"
  chmod +x "$s/bin/demo"
  # shellcheck disable=SC2016 # the hook's
  printf '#!/bin/sh\necho "$1 $INSTALL_OS-$INSTALL_ARCH" >>"$DEMO_LOG"\n' >"$s/hook.sh"
  tar -C "$T/stage" -czf "$T/dist/demo-$os-$arch.tar.gz" "demo-$os-$arch"
done
printf '%s\n' 'repo = acme/demo' 'prefix = demo' 'install.hook = hook.sh' 'install.env.DEMO_LOG = ~/demo.log' \
  >"$T/project/.github/bana.conf"
(cd "$T/project" && BANA_CONFIG=$T/project/.github/bana.conf bash "$bana" installer "$T/dist" --label real-1) >"$T/out" 2>&1 ||
  { cat "$T/out"; exit 1; }

export HOME=$T/home
unset XDG_DATA_HOME XDG_BIN_HOME XDG_CONFIG_HOME INSTALL_PREFIX INSTALL_BIN INSTALL_URL INSTALL_YES INSTALL_UNINSTALL GITHUB_PATH
x() { "$SH" "$@" </dev/null >"$T/out" 2>&1; }
d=$HOME/.local/share/demo b=$HOME/.local/bin

check "installs --from its files" x "$T/dist/install.sh" --from "$T/dist" --yes
check "... this machine's build" test "$("$b/demo")" = "demo $os-$cpu"
check "... the hook's stages, with install.env (~ expanded)" test "$(tr '\n' '|' <"$HOME/demo.log")" = "pre-install $os-$cpu|post-install $os-$cpu|"
check "... current is the build" test "$(readlink "$d/current")" = real-1
cp -R "$T/dist" "$T/bad"
printf 'x' >>"$T/bad/demo-$os-$cpu.tar.gz"
fails_with "a changed archive: refused (its sha256)" x "$T/bad/install.sh" --from "$T/bad" --force --yes
check "... saying so, and the build stays" bash -c "grep -q 'but the release says' '$T/out' && test \"\$('$b/demo')\" = 'demo $os-$cpu'"

if [[ $os == macos ]]; then
  # As Safari marks a download. bsdtar hands the mark to every file it unpacks.
  q=$T/q/demo-$os-$cpu.tar.gz
  mkdir -p "$T/q" && cp "$T/dist/demo-$os-$cpu.tar.gz" "$q"
  xattr -w com.apple.quarantine "0083;$(printf %x "$(date +%s)");Safari;" "$q"
  check "the archive carries com.apple.quarantine" xattr -p com.apple.quarantine "$q"
  mkdir -p "$T/untar" && tar -xzf "$q" -C "$T/untar"
  if xattr -r -l "$T/untar" 2>/dev/null | grep -q com.apple.quarantine; then
    echo "     (tar -x passed the mark on, as it does from a browser download)"
  else
    echo "     (tar -x did not pass the mark on here)"
  fi
  check "a quarantined archive installs --from" x "$T/dist/install.sh" --from "$q" --force --yes
  check "... and nothing installed carries com.apple.quarantine" bash -c "! xattr -r -l '$d/real-1' | grep -q com.apple.quarantine"
  check "... it runs" test "$("$b/demo")" = "demo $os-$cpu"
  if [[ $cpu == arm64 ]] && arch -x86_64 /usr/bin/true 2>/dev/null; then
    # uname -m says x86_64 under Rosetta; sysctl says the Mac is arm64.
    check "under Rosetta (arch -x86_64): installs" bash -c "arch -x86_64 '$SH' '$T/dist/install.sh' --from '$T/dist' --force --yes </dev/null >'$T/out' 2>&1"
    check "... still the arm64 build" test "$("$b/demo")" = "demo macos-arm64"
  fi
fi

check "uninstall" x "$T/dist/install.sh" --uninstall --yes
check "... all gone" test ! -e "$d" -a ! -e "$b/demo"

echo "$((n - fails)) of $n passed ($SH, $os-$cpu)"
((fails == 0))

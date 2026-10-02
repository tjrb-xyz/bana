#!/usr/bin/env bash
# The installer bana compiles (lib/install.sh.in through bana installer), on stand-ins for gh,
# uname, sysctl, getconf, sudo, killall, xattr and chown; example's own hook drives the
# lifecycle, and a real program runs from current while it upgrades.
#
#   tests/install.sh [--fixtures OUT]   OUT: only the releases, for tests/install-ps1.sh
#                           SH: the shell under test (default dash; bash, /opt/bash32/bin/bash;
#                           busybox sh runs its own uname, so only the linux-x64 cases hold
#                           there); BASH_UNDER_TEST renders; AWK=original-awk: BSD's awk
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
bana=$here/../bin/bana
SH=${SH:-dash}
T=$(cd "$(mktemp -d)" && pwd -P) # macOS: /var is /private/var
trap 'rm -rf "$T"; [ -z "${srv:-}" ] || kill "$srv" 2>/dev/null; [ -z "${run:-}" ] || kill "$run" 2>/dev/null' EXIT
n=0 fails=0

# PATH: the stand-ins the installer meets, and the awk under test.
mkdir -p "$T/path"
for c in gh uname sysctl getconf sudo killall xattr chown; do ln -s "$here/stand-ins/$c" "$T/path/$c"; done
[[ -z ${AWK:-} ]] || ln -s "$(command -v "$AWK")" "$T/path/awk"
# nosid COMMAND: without a terminal (setsid, where there is one).
printf '#!/bin/sh\nif command -v setsid >/dev/null; then exec setsid "$@"; fi\nexec "$@"\n' >"$T/path/nosid"
chmod +x "$T/path/nosid"
export PATH=$T/path:$PATH FAKE_RELEASES=$T/releases
render_bash=${BASH_UNDER_TEST:-bash}
# shellcheck disable=SC2016 # that bash's
echo "shell under test: $SH; bana installer on $("$render_bash" -c 'echo $BASH_VERSION'); awk: $(awk --version 2>&1 | head -1)"

ok() { n=$((n + 1)); echo "ok   $*"; }
bad() { n=$((n + 1)); fails=$((fails + 1)); echo "FAIL $*"; sed 's/^/     | /' "$T/out"; }
check() { local what=$1; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }
fails_with() { local what=$1; shift; if "$@"; then bad "$what"; else ok "$what"; fi; }

# example's bana.conf (its install.* keys), and its hook.
mkdir -p "$T/project/.github"
cp "$here/../examples/example/bana.conf" "$T/project/.github/bana.conf"
# example builds no Windows zip; these releases have one, with a hook.
export BANA_CONFIG=$T/project/.github/bana.conf BANA_RELEASE_FILES='example-*.tar.gz example-*.zip example-*.deb' \
  BANA_INSTALL_HOOK_PS1=install-hook.ps1
render() { # DIR TAG|--label LABEL
  local dir=$1
  shift
  [[ $1 == --label ]] || set -- --tag "$1"
  "$render_bash" "$bana" installer "$dir" "$@" >"$T/out" 2>&1
}

# Releases shaped like example's scripts/ci.sh package makes them: example-release-<commit>-<os>-<arch>
# (one top directory: bin/ ui/ driver/ or linux/, the hook), a .zip for Windows.
fixtures() { # OUT
  local out=$1 i=0 tag commit rel plat name s bins b drv d
  mkdir -p "$out"
  for tag in v1.0.0 v1.1.0 v1.2.0; do
    i=$((i + 1))
    commit=$(printf '%010d' "$i")
    rel=$out/$tag && mkdir -p "$rel"
    for plat in linux-x64 linux-arm64 macos-x64 macos-arm64 windows-x64; do
      name=example-release-$commit-$plat
      s=$T/stage/$name && rm -rf "$T/stage" && mkdir -p "$s/bin" "$s/ui"
      echo "<h1>example $tag</h1>" >"$s/ui/index.html"
      bins='exampled example-mcp camilladsp'
      [[ $tag == v1.0.0 ]] || bins='exampled example-new camilladsp' # v1.1.0: example-mcp gone, example-new new
      [[ $plat == macos-* ]] && bins+=' example-coreaudio'
      for b in $bins; do
        if [[ $plat == windows-* ]]; then
          printf '@echo %s %s %s\r\n' "$b" "$tag" "$plat" >"$s/bin/$b.cmd"
        else
          printf '#!/bin/sh\necho "%s %s %s"\n' "$b" "$tag" "$plat" >"$s/bin/$b" && chmod +x "$s/bin/$b"
        fi
      done
      # A real program on this machine's CPU, to upgrade while it runs.
      [[ $plat == linux-x64 ]] && cp /bin/sleep "$s/bin/example-sleep"
      case $plat in
      macos-*)
        # v1.0.0 and v1.1.0 share a driver build; v1.2.0 has a new one.
        drv=1 && [[ $tag == v1.2.0 ]] && drv=2
        for d in examplesystem2ch exampledaw16ch examplestream16ch examplein16ch; do
          mkdir -p "$s/driver/$d.driver/Contents/MacOS" && echo "driver $drv" >"$s/driver/$d.driver/Contents/MacOS/$d"
        done ;;
      linux-*) mkdir -p "$s/linux" && echo unit >"$s/linux/exampled.service" ;;
      esac
      if [[ $plat == windows-* ]]; then
        # shellcheck disable=SC2016 # PowerShell's
        printf '%s\n' 'param([string]$Stage)' \
          'Write-Host "hook $Stage in $env:INSTALL_DIR (previous: '"'"'$env:INSTALL_PREVIOUS'"'"', yes: $env:INSTALL_YES)"' \
          'Add-Content -LiteralPath $env:FAKE_LOG -Value "ps1-hook $Stage $env:INSTALL_TAG prev=$env:INSTALL_PREVIOUS log=$env:EXAMPLE_LOG_DIR"' \
          'if ($env:FAKE_FAIL_STAGE -eq $Stage) { throw "hook refuses $Stage" }' >"$s/install-hook.ps1"
        (cd "$T/stage" && zip -qr "$rel/$name.zip" "$name")
      else
        # The test's hook: logs its stage and fails on FAKE_FAIL_STAGE, then runs example's.
        cp "$here/../examples/example/install-hook.sh" "$s/example-hook.sh"
        # shellcheck disable=SC2016 # the hook expands them
        printf '%s\n' '#!/bin/sh' \
          'echo "hook $1 tag=$INSTALL_TAG previous=${INSTALL_PREVIOUS:-} dir=$INSTALL_DIR log=${EXAMPLE_LOG_DIR:-} yes=$INSTALL_YES" >>"$FAKE_LOG"' \
          '[ "${FAKE_FAIL_STAGE:-}" != "$1" ] || { echo "hook refuses $1" >&2; exit 3; }' \
          'h=$(dirname "$0")/example-hook.sh' \
          '[ -f "$h" ] || exit 0 # post-uninstall runs a copy of this file alone' \
          'exec sh "$h" "$@"' >"$s/install-hook.sh"
        tar -C "$T/stage" -czf "$rel/$name.tar.gz" "$name"
      fi
    done
    echo "a package" >"$rel/example-release-$commit-linux-x64.deb"
    render "$rel" "$tag" || { cat "$T/out"; exit 1; }
  done
  rm -rf "$T/stage"
}
if [[ ${1:-} == --fixtures ]]; then # OUT: for tests/install-ps1.sh
  fixtures "${2:?OUT}"
  exit 0
fi
fixtures "$T/fixtures"

fresh() {
  rm -rf "${T:?}/home" "$T/releases" "$T/hal" && mkdir -p "$T/home" "$T/hal"
  cp -R "$T/fixtures" "$T/releases"
  export HOME=$T/home FAKE_LOG=$T/log EXAMPLE_HAL_DIR=$T/hal FAKE_GH=1
  unset FAKE_OS FAKE_ARCH FAKE_ARM64 FAKE_LONG_BIT FAKE_FAIL_STAGE XDG_DATA_HOME XDG_CONFIG_HOME XDG_BIN_HOME \
    INSTALL_BIN INSTALL_PREFIX INSTALL_URL INSTALL_YES INSTALL_UNINSTALL GITHUB_PATH
  : >"$FAKE_LOG"
}
x() { nosid "$SH" "$@" >"$T/out" 2>&1; } # install.sh [ARGS], no terminal
inst() { # TAG [ARGS]: as `gh release download TAG -p install.sh -O - | sh -s -- ARGS`, no terminal
  local tag=$1
  shift
  nosid "$SH" -s -- "$@" <"$T/releases/$tag/install.sh" >"$T/out" 2>&1
}
asset() { local f; for f in "$T/releases/$1/"*"-$2".tar.gz "$T/releases/$1/"*"-$2".zip; do [ -f "$f" ] && echo "${f##*/}"; done; }
d=$T/home/.local/share/example b=$T/home/.local/bin
out() { grep -q -- "$1" "$T/out"; }
log() { grep -q -- "$1" "$FAKE_LOG"; }

check "the installers are printable ASCII (no escape byte for gh's pipe guard)" bash -c "! LC_ALL=C grep -q '[^ -~]' '$T/fixtures'/*/install.*"
check "SHA256SUMS: the release files and installers (the .deb too, release.files)" bash -c \
  "cd '$T/fixtures/v1.0.0' && sha256sum -c --quiet SHA256SUMS && grep -q 'linux-x64.deb\$' SHA256SUMS && grep -q ' install.ps1\$' SHA256SUMS"

fresh
check "linux x64: installs through gh" inst v1.0.0
check "exampled is the linux-x64 build" test "$("$b/exampled")" = "exampled v1.0.0 linux-x64"
check "only install.bins are linked (camilladsp is not)" test -L "$b/example-mcp" -a ! -e "$b/camilladsp"
check "current -> v1.0.0" test "$(readlink "$d/current")" = v1.0.0
check "gh fetched this platform's archive" log "gh release download v1.0.0 -R tjrb-xyz/example -p $(asset v1.0.0 linux-x64)"
check "receipt: tag, platform, sha256, config, links" bash -c "grep -qx tag=v1.0.0 '$d/receipt' && grep -qx platform=linux-x64 '$d/receipt' && grep -q '^sha256=[0-9a-f]\{64\}$' '$d/receipt' && grep -qx 'config=$HOME/.config/example' '$d/receipt' && grep -qx 'link=$b/exampled' '$d/receipt'"
check "PATH hint when ~/.local/bin is not on it" out "Add $b to your PATH"
check "install.env: EXAMPLE_LOG_DIR reached the hook, ~ expanded" log "hook post-install tag=v1.0.0 previous= dir=$d/v1.0.0 log=$HOME/Library/Logs/example"
inst v1.0.0
check "same release again: nothing to do" out "is installed already"
"$d/current/bin/example-sleep" 30 &
run=$!
check "--force while its program runs (no 'text file busy')" inst v1.0.0 --force
kill "$run" 2>/dev/null
run=''
check "upgrade to v1.1.0" inst v1.1.0
check "upgrade: example-new linked" test "$("$b/example-new")" = "example-new v1.1.0 linux-x64"
check "upgrade: example-mcp (gone from v1.1.0) unlinked" test ! -e "$b/example-mcp" -a ! -L "$b/example-mcp"
check "upgrade: the previous version is kept" test -d "$d/v1.0.0"
inst v1.1.0 --from "$T/fixtures/v1.1.0"
check "same release again --from its files: nothing to do (no hook)" bash -c "grep -q 'is installed already' '$T/out' && [ \$(grep -c 'hook post-install tag=v1.1.0' '$FAKE_LOG') = 1 ]"
check "--force the same release" inst v1.1.0 --force
check "--force: the version before it is still kept" test -d "$d/v1.0.0"
check "upgrade to v1.2.0" inst v1.2.0
check "upgrade: older than the previous one pruned" test ! -e "$d/v1.0.0" -a -d "$d/v1.1.0"
check "no install temp left" bash -c "! ls -a '$d' | grep -q '^.install'"
mkdir -p "$HOME/.config/example" && echo '{}' >"$HOME/.config/example/settings.json"
check "uninstall" inst v1.0.0 --uninstall
check "uninstall: links gone" bash -c "! ls '$b/' | grep -q example"
check "uninstall: files gone" test ! -e "$d"
check "uninstall: settings stay" test -f "$HOME/.config/example/settings.json"
inst v1.2.0 && inst v1.2.0 --uninstall --purge
check "--purge: settings gone too" test ! -e "$HOME/.config/example"
fails_with "uninstall when not installed fails" inst v1.2.0 --uninstall

fresh
mkdir -p "$b" && echo mine >"$b/exampled"
inst v1.0.0
check "a file of yours is left alone" test "$(cat "$b/exampled")" = mine
check "... and said so" out "is not example's: left alone"

fresh
check "--prefix and --bin-dir" inst v1.0.0 --prefix "$T/home/opt/example" --bin-dir "$T/home/bin2"
check "... installed there" test "$("$T/home/bin2/exampled")" = "exampled v1.0.0 linux-x64" -a -d "$T/home/opt/example/v1.0.0"
check "an upgrade keeps the bin dir" inst v1.1.0 --prefix "$T/home/opt/example"
check "... example-new in bin2" test -L "$T/home/bin2/example-new" -a ! -e "$b/example-new"
check "INSTALL_PREFIX works as --prefix, INSTALL_UNINSTALL as --uninstall" \
  env INSTALL_PREFIX="$T/home/opt/example" INSTALL_UNINSTALL=1 nosid "$SH" -s <"$T/releases/v1.1.0/install.sh" >"$T/out" 2>&1
check "... uninstalled from there" test ! -e "$T/home/opt/example" -a ! -e "$T/home/bin2/exampled"

# PREFIX may hold other things: the installer removes only the versions it made.
fresh
o=$T/home/opt && mkdir -p "$o/mytools" && echo mine >"$o/notes.txt"
check "--prefix a directory with other things in it" inst v1.0.0 --prefix "$o"
inst v1.1.0 --prefix "$o" && inst v1.2.0 --prefix "$o"
check "... upgrades prune only its own versions" test ! -e "$o/v1.0.0" -a -d "$o/v1.1.0" -a -d "$o/mytools" -a -f "$o/notes.txt"
check "... uninstall" inst v1.2.0 --uninstall --prefix "$o"
check "... removes only its own, and the directory stays" test "$(cd "$o" && echo .[!.]* *)" = ".[!.]* mytools notes.txt"
mkdir -p "$o/v1.0.0"
fails_with "a directory named as the version, not its own: refused" inst v1.0.0 --prefix "$o"
check "... and left alone" test -d "$o/v1.0.0" -a ! -e "$o/current" -a ! -e "$o/receipt"
# A config of your home (bana installer refuses it; an edited install.sh may not): --purge keeps it.
sed "s|^CONFIG=.*|CONFIG='~'|" "$T/releases/v1.0.0/install.sh" >"$T/home.sh"
x "$T/home.sh" && x "$T/home.sh" --uninstall --purge
check "--purge with a config of ~: your home stays" bash -c "test -d '$HOME/.local' && grep -q 'not removing $HOME (your home)' '$T/out'"
rm -f "$T/home.sh"

# bana.conf's install.prefix, install.bin and install.config: the defaults baked in.
fresh
cp -R "$T/fixtures/v1.0.0" "$T/baked"
# shellcheck disable=SC2088 # as bana.conf says it
BANA_INSTALL_PREFIX='~/apps/example' BANA_INSTALL_BIN='~/bin3' BANA_INSTALL_CONFIG='~/Library/Application Support/example' \
  render "$T/baked" v1.0.0
check "install.prefix and install.bin: installs there" x "$T/baked/install.sh" --from "$T/baked"
check "... in ~/apps/example, commands in ~/bin3" test "$("$T/home/bin3/exampled")" = "exampled v1.0.0 linux-x64" -a -d "$T/home/apps/example/v1.0.0"
check "... install.config, ~ expanded, spaces kept" grep -qx "config=$HOME/Library/Application Support/example" "$T/home/apps/example/receipt"
check "INSTALL_BIN and INSTALL_PREFIX still win" env INSTALL_PREFIX="$T/home/p2" INSTALL_BIN="$T/home/b2" nosid "$SH" "$T/baked/install.sh" --from "$T/baked" >"$T/out" 2>&1
check "... there" test -L "$T/home/b2/exampled" -a -d "$T/home/p2/v1.0.0"
mkdir -p "$HOME/Library/Application Support/example"
check "--purge removes the receipt's config" x "$T/baked/install.sh" --uninstall --purge
check "... gone" test ! -e "$HOME/Library/Application Support/example" -a ! -e "$T/home/apps/example"
rm -rf "$T/baked"

fresh
export FAKE_OS=Darwin FAKE_ARCH=x86_64 FAKE_ARM64=1
check "Rosetta shell on Apple silicon: installs" inst v1.0.0 --yes
check "Rosetta: picked macos-arm64" test "$("$b/exampled")" = "exampled v1.0.0 macos-arm64"
check "stages: pre-install (from the new files), then post-install" bash -c "grep '^hook' '$FAKE_LOG' | sed -n '1p;2p' | tr '\n' '|' | grep -q '^hook pre-install tag=v1.0.0 previous= dir=$d/.install.*|hook post-install tag=v1.0.0 previous= dir=$d/v1.0.0 .*|$'"
check "--yes: the four devices copied to HAL" test "$(cat "$T/hal"/*.driver/Contents/MacOS/* | sort -u)" = "driver 1" -a "$(find "$T/hal" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')" = 4
check "--yes: through sudo, and coreaudiod restarted" bash -c "grep -q 'sudo cp -R' '$FAKE_LOG' && grep -q 'killall -9 coreaudiod' '$FAKE_LOG'"
check "install.env: the hook made EXAMPLE_LOG_DIR" test -d "$HOME/Library/Logs/example"
check "quarantine cleared on what was unpacked" log "xattr -dr com.apple.quarantine"
: >"$FAKE_LOG"
inst v1.1.0
check "upgrade: the hook sees the previous version" log "hook post-install tag=v1.1.0 previous=v1.0.0 dir=$d/v1.1.0"
check "same devices in the upgrade: no question, no restart" bash -c "grep -q 'devices are up to date' '$T/out' && ! grep -q killall '$FAKE_LOG'"
inst v1.2.0
check "new devices, no terminal, no --yes: not installed" bash -c "grep -q 'Devices not installed' '$T/out' && ! grep -q killall '$FAKE_LOG'"
check "... the rest is installed anyway" test "$("$b/exampled")" = "exampled v1.2.0 macos-arm64"
check "the hook by hand, as it says (no INSTALL_* from an installer)" bash -c \
  "INSTALL_YES=1 sh '$d/current/install-hook.sh' post-install >'$T/out' 2>&1 && grep -q 'devices are installed' '$T/out' && test \"\$(cat '$T/hal/examplein16ch.driver/Contents/MacOS/examplein16ch')\" = 'driver 2'"
rm -rf "${T:?}/hal"/* && cp -R "$d/v1.1.0/driver/"*.driver "$T/hal/" # v1.1.0's devices again, for what follows
# With a terminal: the question reads /dev/tty although stdin is the script (cat install.sh | sh).
if script -qec true /dev/null >/dev/null 2>&1; then
  printf 'y\n' | script -qec "cat '$T/releases/v1.2.0/install.sh' | $SH -s -- --force" /dev/null >"$T/out" 2>&1
  check "cat install.sh | sh with a terminal: asks, and y installs the devices" bash -c "grep -q 'devices are installed' '$T/out' && test \"\$(cat '$T/hal/examplein16ch.driver/Contents/MacOS/examplein16ch')\" = 'driver 2'"
fi
: >"$FAKE_LOG"
FAKE_FAIL_STAGE=pre-uninstall inst v1.2.0 --uninstall --yes
check "a failing pre-uninstall stops the uninstall" bash -c "grep -q 'pre-uninstall hook failed; nothing removed' '$T/out' && test -L '$b/exampled' -a -d '$T/hal/examplein16ch.driver'"
check "INSTALL_YES=1, as --yes: uninstall" env INSTALL_YES=1 nosid "$SH" -s -- --uninstall <"$T/releases/v1.2.0/install.sh" >"$T/out" 2>&1
check "... ran pre-uninstall, then post-uninstall (from a copy: the files are gone)" bash -c "grep '^hook' '$FAKE_LOG' | tail -2 | tr '\n' '|' | grep -q '^hook pre-uninstall tag=v1.2.0 .*yes=1|hook post-uninstall tag=v1.2.0 '"
check "... devices removed" test -z "$(ls "$T/hal")"
unset FAKE_ARM64
: >"$FAKE_LOG"
check "Intel Mac: installs" inst v1.0.0 --no-hook
check "Intel Mac: macos-x64" test "$("$b/exampled")" = "exampled v1.0.0 macos-x64"
check "--no-hook: no hook" bash -c "! grep -q '^hook' '$FAKE_LOG' && test -z \"\$(ls '$T/hal')\""

fresh
inst v1.0.0
fails_with "a failing pre-install stops the upgrade" env FAKE_FAIL_STAGE=pre-install nosid "$SH" -s <"$T/releases/v1.1.0/install.sh" >"$T/out" 2>&1
check "... v1.0.0 stays, untouched" test "$(readlink "$d/current")" = v1.0.0 -a ! -e "$d/v1.1.0" -a "$("$b/exampled")" = "exampled v1.0.0 linux-x64"
fails_with "a failing post-install fails the run" env FAKE_FAIL_STAGE=post-install nosid "$SH" -s <"$T/releases/v1.1.0/install.sh" >"$T/out" 2>&1
check "... but v1.1.0 is installed, and it says how to run the hook again" bash -c "test \"\$(readlink '$d/current')\" = v1.1.0 && grep -q 'Again: this installer with --force' '$T/out'"

fresh
export FAKE_ARCH=aarch64
check "linux arm64" inst v1.0.0
check "linux arm64: build" test "$("$b/exampled")" = "exampled v1.0.0 linux-arm64"
export FAKE_LONG_BIT=32
fails_with "a 32-bit userland on arm64: refuses" inst v1.1.0
check "... saying why" out "32-bit system"

fresh
inst v1.0.0
printf 'garbage' >"$T/releases/v1.1.0/$(asset v1.1.0 linux-x64)"
fails_with "a bad download fails" inst v1.1.0
check "... saying why" out "sha256 .*, but the release says"
check "... and leaves the installed version" test "$("$b/exampled")" = "exampled v1.0.0 linux-x64"
check "... and no temp" bash -c "! ls -a '$d' | grep -q '^.install'"

# Without sha256sum, shasum and openssl the install stops (it never skips the check).
fresh
mkdir -p "$T/nohash"
for f in /usr/local/bin/* /usr/bin/* /bin/*; do
  c=${f##*/}
  case $c in sha256sum | shasum | openssl) continue ;; esac
  [[ -e $T/nohash/$c ]] || ln -s "$f" "$T/nohash/$c"
done
fails_with "no sha256sum, shasum or openssl: stops" env PATH="$T/path:$T/nohash" nosid "$SH" -s <"$T/releases/v1.0.0/install.sh" >"$T/out" 2>&1
check "... saying so, nothing installed" bash -c "grep -q 'need sha256sum, shasum or openssl' '$T/out' && test ! -e '$d/current'"
# shasum alone (an older Mac)
rm -f "$T/nohash/sha256sum" && ln -s "$(command -v shasum)" "$T/nohash/shasum"
check "shasum alone: installs" env PATH="$T/path:$T/nohash" nosid "$SH" -s <"$T/releases/v1.0.0/install.sh" >"$T/out" 2>&1
rm -rf "$T/nohash"

# Archives the installer refuses, though their sha256 matches (as baked in): entries outside
# the directory, or two top directories.
fresh
evil() { # KIND: .. or two; replaces linux-x64's archive of v1.0.0, and its sha256
  local a old new
  a=$T/releases/v1.0.0/$(asset v1.0.0 linux-x64)
  old=$(sha256sum "$a" | cut -d' ' -f1)
  python3 - "$a" "$1" <<'PY'
import io, sys, tarfile
a, kind = sys.argv[1], sys.argv[2]
with tarfile.open(a, "w:gz") as t:
    def add(name, data=b"x"):
        i = tarfile.TarInfo(name); i.size = len(data); i.mode = 0o755; t.addfile(i, io.BytesIO(data))
    add("example-release/bin/exampled", b"#!/bin/sh\necho evil\n")
    if kind == "..": add("example-release/../../escaped")
    else: add("other/bin/x")
PY
  new=$(sha256sum "$a" | cut -d' ' -f1)
  sed "s/$old/$new/" "$T/releases/v1.0.0/install.sh" >"$T/evil.sh"
}
evil ..
fails_with "an entry with '..': refused" x "$T/evil.sh"
# (busybox's tar drops the ../ itself, and then there are two top entries)
check "... before anything is unpacked" bash -c "grep -Eq 'outside its directory|should hold one directory' '$T/out' && test ! -e '$T/home/.local/share/escaped' -a ! -e '$d/escaped' -a ! -e '$d/current'"
fresh
evil two
fails_with "two top directories: refused" x "$T/evil.sh"
check "... saying so" out "should hold one directory"
rm -f "$T/evil.sh"

fresh
FAKE_GH=0
check "--from a downloaded archive (no network)" inst v1.0.0 --from "$T/fixtures/v1.0.0/$(asset v1.0.0 linux-x64)"
check "--from: installed" test "$("$b/exampled")" = "exampled v1.0.0 linux-x64"
fails_with "--from another platform's archive fails (sha256)" inst v1.0.0 --force --from "$T/fixtures/v1.0.0/$(asset v1.0.0 linux-arm64)"
export FAKE_ARCH=aarch64
check "--from DIR picks this machine's archive" inst v1.1.0 --from "$T/fixtures/v1.1.0"
check "... linux-arm64" test "$("$b/exampled")" = "exampled v1.1.0 linux-arm64"
unset FAKE_ARCH
check "--from DIR, relative" bash -c "cd '$T/fixtures' && nosid $SH v1.2.0/install.sh --from v1.2.0 >'$T/out' 2>&1"
check "... installed" test "$("$b/exampled")" = "exampled v1.2.0 linux-x64"

# A daemon build (--label): no release to download from, so --from.
fresh
cp -R "$T/fixtures/v1.0.0" "$T/nightly"
rm "$T/nightly"/install.* "$T/nightly/SHA256SUMS"
render "$T/nightly" --label nightly-0000000001
check "--label: LOCAL=1" grep -qx LOCAL=1 "$T/nightly/install.sh"
fails_with "a daemon build without --from: refuses" x "$T/nightly/install.sh"
check "--from its files: installs" x "$T/nightly/install.sh" --from "$T/nightly"
check "... as nightly-0000000001" test "$(readlink "$d/current")" = nightly-0000000001 -a "$("$b/exampled")" = "exampled v1.0.0 linux-x64"
check "... and gh was not asked" bash -c "! grep -q 'gh release' '$FAKE_LOG'"
rm -rf "$T/nightly"

fresh
export FAKE_OS=MINGW64_NT-10.0
fails_with "Git Bash on Windows: refuses" inst v1.0.0
check "... pointing at install.ps1" out "install.ps1"
export FAKE_OS=Linux FAKE_ARCH=riscv64
fails_with "riscv64: refuses" inst v1.0.0
check "... saying there is no build" out "no build for riscv64"
unset FAKE_OS FAKE_ARCH

# curl from a public repository's release (here: a local HTTPS server with its own CA).
fresh
FAKE_GH=0
fails_with "not signed in to gh, and a release curl cannot reach" env INSTALL_URL=https://127.0.0.1:1/v1.0.0 nosid "$SH" -s <"$T/releases/v1.0.0/install.sh" >"$T/out" 2>&1
check "... is the release there, or private (gh)" out "(is v1.0.0 published there? A private repository's files need gh: gh auth login)"
if openssl req -x509 -newkey rsa:2048 -nodes -keyout "$T/key.pem" -out "$T/cert.pem" -days 1 -subj /CN=127.0.0.1 \
  -addext subjectAltName=IP:127.0.0.1 2>/dev/null; then
  port=$((20000 + $$ % 20000))
  python3 - "$T" "$port" <<'PY' &
import http.server, ssl, sys, os
os.chdir(sys.argv[1] + "/releases")
h = http.server.SimpleHTTPRequestHandler
h.log_message = lambda *a: None
s = http.server.HTTPServer(("127.0.0.1", int(sys.argv[2])), h)
c = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); c.load_cert_chain(sys.argv[1] + "/cert.pem", sys.argv[1] + "/key.pem")
s.socket = c.wrap_socket(s.socket, server_side=True)
s.serve_forever()
PY
  srv=$!
  for _ in 1 2 3 4 5 6 7 8 9 10; do curl -fsS --cacert "$T/cert.pem" -o /dev/null "https://127.0.0.1:$port/" 2>/dev/null && break; sleep 0.3; done
  export CURL_CA_BUNDLE=$T/cert.pem INSTALL_URL=https://127.0.0.1:$port/v1.0.0
  check "curl over https (a public release, or a mirror)" inst v1.0.0
  check "curl: installed" test "$("$b/exampled")" = "exampled v1.0.0 linux-x64"
  check "curl: gh not used" bash -c "! grep -q 'gh release' '$FAKE_LOG'"
  unset INSTALL_URL CURL_CA_BUNDLE
  kill "$srv" 2>/dev/null
  srv=''
else
  echo "(this openssl makes no certificate for an IP address: curl over https is not tested)"
fi
export GITHUB_PATH=$T/github_path FAKE_GH=1
inst v1.1.0
check "GITHUB_PATH gets ~/.local/bin (CI)" grep -qx "$b" "$T/github_path"

# bana's own release (.github/release.sh pack and dist, a fake bana-manager): its install.sh and
# its hook (lib/install-hook.sh), which makes way for the old install.sh's link into ~/.bana/src.
fresh
V=$(sed -n 's/^BANA_VERSION=//p' "$bana")
printf '#!/bin/sh\necho %s\n' "$V" >"$T/bana-manager" && chmod +x "$T/bana-manager"
if (GITHUB_SHA=0123456789abcdef0123456789abcdef01234567 "$render_bash" "$here/../.github/release.sh" pack "$T/bana-manager" "$V" linux-x64 "$T/bana" &&
  "$render_bash" "$here/../.github/release.sh" dist "$T/bana" "$V") >"$T/out" 2>&1; then ok "bana's release files"; else bad "bana's release files"; fi
mkdir -p "$HOME/.bana/src/bin" "$b" && cp "$bana" "$HOME/.bana/src/bin/bana" && ln -s "$HOME/.bana/src/bin/bana" "$b/bana"
check "bana's install.sh, over the old install.sh's link" x "$T/bana/install.sh" --from "$T/bana" --yes
check "... bana is the release's" test "$(readlink "$b/bana")" = "$HOME/.local/share/bana/current/bin/bana" -a "$("$b/bana" version)" = "bana $V (release v$V)"
check "... the old link is in ~/.bana/.upgrade-from" test "$(cat "$HOME/.bana/.upgrade-from")" = "$HOME/.bana/src/bin/bana"
check "... uninstall" x "$T/bana/install.sh" --uninstall --yes
check "... ~/.bana stays" test -f "$HOME/.bana/src/bin/bana" -a ! -e "$HOME/.local/share/bana" -a ! -e "$b/bana"
rm -rf "$T/bana" "$T/bana-manager"

echo "$((n - fails)) of $n passed ($SH)"
((fails == 0))

#!/usr/bin/env bash
# install.ps1, as bana installer compiles it, on PowerShell 7 for Linux: what does not need
# Windows (download, sha256, unzip, versions, receipt, the hook's stages, uninstall). The
# registry PATH, junctions, Unblock-File and Windows PowerShell 5.1 need a Windows runner.
#
#   tests/install-ps1.sh    PWSH: pwsh (default: the one on PATH; none: nothing to test)
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
PWSH=${PWSH:-$(command -v pwsh || true)}
if [[ -z $PWSH ]]; then
  echo "no pwsh here (PWSH=path/to/pwsh): install.ps1 is not tested"
  exit 0
fi
T=$(mktemp -d)
trap 'rm -rf "$T"; [ -z "${srv:-}" ] || kill "$srv" 2>/dev/null' EXIT
"$here/install.sh" --fixtures "$T/fixtures" >/dev/null || exit 1
mkdir -p "$T/path"
ln -s "$here/stand-ins/gh" "$T/path/gh"
export PATH=$T/path:$PATH FAKE_RELEASES=$T/releases
n=0 fails=0
ok() { n=$((n + 1)); echo "ok   $*"; }
bad() { n=$((n + 1)); fails=$((fails + 1)); echo "FAIL $*"; sed 's/^/     | /' "$T/out"; }
check() { local what=$1; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }
fails_with() { local what=$1; shift; if "$@"; then bad "$what"; else ok "$what"; fi; }
fresh() {
  rm -rf "${T:?}/home" "$T/releases" && mkdir -p "$T/home"
  cp -R "$T/fixtures" "$T/releases"
  export HOME=$T/home FAKE_LOG=$T/log FAKE_GH=1
  unset LOCALAPPDATA APPDATA INSTALL_URL INSTALL_YES INSTALL_PREFIX INSTALL_UNINSTALL FAKE_FAIL_STAGE
  : >"$FAKE_LOG"
}
ps() { local tag=$1; shift; "$PWSH" -NoLogo -NoProfile -NonInteractive -File "$T/releases/$tag/install.ps1" "$@" >"$T/out" 2>&1; }
d=$T/home/.local/share/example
zip() { local f; for f in "$T/fixtures/$1/"*-windows-x64.zip; do echo "${f##*/}"; done; }
# shellcheck disable=SC2016 # PowerShell's
echo "pwsh: $("$PWSH" -NoProfile -Command '$PSVersionTable.PSVersion.ToString()')"

check "install.ps1 is printable ASCII (Windows PowerShell 5.1 reads it as ANSI)" bash -c "! LC_ALL=C grep -q '[^ -~]' '$T/fixtures'/*/install.ps1"

fresh
check "installs through gh" ps v1.0.0
check "gh fetched the windows zip" grep -q "gh release download v1.0.0 -R tjrb-xyz/example -p $(zip v1.0.0)" "$FAKE_LOG"
check "current -> v1.0.0" test "$(readlink "$d/current")" = "$d/v1.0.0"
check "bin/exampled.cmd unpacked" test -f "$d/current/bin/exampled.cmd"
check "receipt" bash -c "grep -qx tag=v1.0.0 '$d/receipt' && grep -qx platform=windows-x64 '$d/receipt' && grep -qx hook=install-hook.ps1 '$d/receipt'"
check "stages: pre-install, then post-install, with install.env (~ expanded)" test "$(grep ps1-hook "$FAKE_LOG" | tr '\n' '|')" = \
  "ps1-hook pre-install v1.0.0 prev= log=$HOME/Library/Logs/example|ps1-hook post-install v1.0.0 prev= log=$HOME/Library/Logs/example|"
ps v1.0.0
check "same release again: nothing to do" grep -q "is installed already" "$T/out"
check "upgrade to v1.1.0" ps v1.1.0
check "upgrade: current -> v1.1.0, previous kept" test "$(readlink "$d/current")" = "$d/v1.1.0" -a -d "$d/v1.0.0"
check "upgrade: the hook saw the previous version" grep -q "^ps1-hook post-install v1.1.0 prev=v1.0.0 " "$FAKE_LOG"
ps v1.1.0 -From "$T/fixtures/v1.1.0"
check "same release again -From its files: nothing to do" grep -q "is installed already" "$T/out"
check "-Force the same release" ps v1.1.0 -Force
check "-Force: the version before it is still kept" test -d "$d/v1.0.0"
check "upgrade to v1.2.0" ps v1.2.0
check "upgrade: older pruned" test ! -e "$d/v1.0.0"
: >"$FAKE_LOG"
FAKE_FAIL_STAGE=pre-uninstall ps v1.2.0 -Uninstall
check "a failing pre-uninstall stops the uninstall" bash -c "grep -q 'nothing removed' '$T/out' && test -d '$d/v1.2.0'"
mkdir -p "$HOME/.config/example" && echo '{}' >"$HOME/.config/example/settings.json"
check "uninstall" ps v1.0.0 -Uninstall
check "uninstall: pre-uninstall, then post-uninstall (from a copy)" bash -c \
  "grep ps1-hook '$FAKE_LOG' | tail -2 | tr '\n' '|' | grep -q '^ps1-hook pre-uninstall v1.2.0 prev=v1.2.0 .*|ps1-hook post-uninstall v1.2.0 prev=v1.2.0 .*|$'"
check "uninstall: all gone, settings stay" test ! -e "$d" -a -f "$HOME/.config/example/settings.json"
ps v1.2.0 && ps v1.2.0 -Uninstall -Purge
check "-Purge: settings gone" test ! -e "$HOME/.config/example"

fresh
ps v1.0.0
fails_with "a failing pre-install stops the upgrade" env FAKE_FAIL_STAGE=pre-install "$PWSH" -NoProfile -NonInteractive -File "$T/releases/v1.1.0/install.ps1" >"$T/out" 2>&1
check "... v1.0.0 stays" test "$(readlink "$d/current")" = "$d/v1.0.0" -a ! -e "$d/v1.1.0"
fails_with "a failing post-install fails the run" env FAKE_FAIL_STAGE=post-install "$PWSH" -NoProfile -NonInteractive -File "$T/releases/v1.1.0/install.ps1" >"$T/out" 2>&1
check "... but v1.1.0 is installed" test "$(readlink "$d/current")" = "$d/v1.1.0"
printf 'garbage' >"$T/releases/v1.2.0/$(zip v1.2.0)"
fails_with "a bad download fails" ps v1.2.0
check "... saying why" grep -q "but the release says" "$T/out"
check "... leaving v1.1.0, and no temp" bash -c "test \"\$(readlink '$d/current')\" = '$d/v1.1.0' && ! ls -a '$d' | grep -q '^.install'"

fresh
check "-Prefix" ps v1.0.0 -Prefix "$T/home/progs/example"
check "... installed there" test -f "$T/home/progs/example/current/bin/exampled.cmd"
check "INSTALL_PREFIX and INSTALL_UNINSTALL (for irm | iex)" env INSTALL_PREFIX="$T/home/progs/example" INSTALL_UNINSTALL=1 "$PWSH" -NoProfile -NonInteractive -File "$T/releases/v1.0.0/install.ps1" >"$T/out" 2>&1
check "... gone" test ! -e "$T/home/progs/example"

# PREFIX may hold other things: the installer removes only the versions it made.
fresh
o=$T/home/progs && mkdir -p "$o/mytools" && echo mine >"$o/notes.txt"
check "-Prefix a directory with other things in it" ps v1.0.0 -Prefix "$o"
ps v1.1.0 -Prefix "$o" && ps v1.2.0 -Prefix "$o"
check "... upgrades prune only its own versions" test ! -e "$o/v1.0.0" -a -d "$o/v1.1.0" -a -d "$o/mytools" -a -f "$o/notes.txt"
check "... uninstall" ps v1.2.0 -Uninstall -Prefix "$o"
check "... removes only its own, and the directory stays" test "$(cd "$o" && echo .[!.]* *)" = ".[!.]* mytools notes.txt"
mkdir -p "$o/v1.0.0"
fails_with "a directory named as the version, not its own: refused" ps v1.0.0 -Prefix "$o"
check "... and left alone" test -d "$o/v1.0.0" -a ! -e "$o/current" -a ! -e "$o/receipt"

fresh
FAKE_GH=0
check "-From a local zip, -NoHook" ps v1.0.0 -From "$T/fixtures/v1.0.0/$(zip v1.0.0)" -NoHook
check "-NoHook: no hook" bash -c "! grep -q ps1-hook '$FAKE_LOG'"
check "-From DIR picks the windows zip" ps v1.1.0 -From "$T/fixtures/v1.1.0"
check "... installed" test "$(readlink "$d/current")" = "$d/v1.1.0"
fails_with "-From a DIR without it" ps v1.2.0 -From "$T/home"
check "... saying so" grep -q "has no example-release-" "$T/out"

# A daemon build (bana installer --label): -From only.
fresh
cp -R "$T/fixtures/v1.0.0" "$T/nightly"
cp "$here/../examples/example/bana.conf" "$T/bana.conf"
BANA_CONFIG=$T/bana.conf BANA_RELEASE_FILES='*' BANA_INSTALL_HOOK_PS1=install-hook.ps1 \
  bash "$here/../bin/bana" installer "$T/nightly" --label nightly-1 >"$T/out" 2>&1 || cat "$T/out"
fails_with "--label: refuses without -From" bash -c "'$PWSH' -NoProfile -NonInteractive -File '$T/nightly/install.ps1' >'$T/out' 2>&1"
check "... saying so" grep -q "a build on bana's daemon" "$T/out"
check "--label: -From its files" bash -c "'$PWSH' -NoProfile -NonInteractive -File '$T/nightly/install.ps1' -From '$T/nightly' >'$T/out' 2>&1"
check "... as nightly-1" test "$(readlink "$d/current")" = "$d/nightly-1"

fresh
FAKE_GH=0
python3 -m http.server 18080 --bind 127.0.0.1 --directory "$T/releases" >/dev/null 2>&1 &
srv=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do curl -fsS -o /dev/null http://127.0.0.1:18080/ 2>/dev/null && break; sleep 0.3; done
check "Invoke-WebRequest download (a public release, or a mirror)" env INSTALL_URL=http://127.0.0.1:18080/v1.0.0 "$PWSH" -NoProfile -NonInteractive -File "$T/releases/v1.0.0/install.ps1" >"$T/out" 2>&1
check "... installed" test -f "$d/v1.0.0/bin/exampled.cmd"
fails_with "a release it cannot reach" env INSTALL_URL=http://127.0.0.1:18080/nothing "$PWSH" -NoProfile -NonInteractive -File "$T/releases/v1.1.0/install.ps1" >"$T/out" 2>&1
check "... may be private: gh auth login" grep -q "may be private" "$T/out"
# irm | iex: no parameters; INSTALL_* from the environment.
export FAKE_GH=1 INSTALL_YES=1
"$PWSH" -NoLogo -NoProfile -NonInteractive -Command "Get-Content -Raw '$T/releases/v1.1.0/install.ps1' | Invoke-Expression" >"$T/out" 2>&1
check "as irm | iex (a string): installs" test "$(readlink "$d/current")" = "$d/v1.1.0"
check "... INSTALL_YES reached the hook" grep -q "yes: 1" "$T/out"
# gh release download -p install.ps1 -O - | Out-String | iex: a native command's lines, joined.
"$PWSH" -NoLogo -NoProfile -NonInteractive -Command "& cat '$T/releases/v1.2.0/install.ps1' | Out-String | Invoke-Expression" >"$T/out" 2>&1
check "a native command's output | Out-String | iex: installs" test "$(readlink "$d/current")" = "$d/v1.2.0"
# Under iex a failure, or nothing to do, returns to your session: exit would close it.
printf 'garbage' >"$T/releases/v1.1.0/$(zip v1.1.0)"
# shellcheck disable=SC2016 # PowerShell's
"$PWSH" -NoLogo -NoProfile -NonInteractive -Command "\$ErrorActionPreference = 'Continue'; Get-Content -Raw '$T/releases/v1.1.0/install.ps1' | Invoke-Expression; Write-Host \"after: \$LASTEXITCODE \$ErrorActionPreference [\$Tag]\"; Get-Content -Raw '$T/releases/v1.2.0/install.ps1' | Invoke-Expression; Write-Host 'after again'" >"$T/out" 2>&1
check "iex: a bad download says so, and the session goes on (\$LASTEXITCODE 1, nothing left in it)" bash -c \
  "grep -q 'but the release says' '$T/out' && grep -qx 'after: 1 Continue \[\]' '$T/out' && grep -q 'is installed already' '$T/out' && grep -qx 'after again' '$T/out'"

echo "$((n - fails)) of $n passed (pwsh)"
((fails == 0))

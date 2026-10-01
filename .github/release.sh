#!/usr/bin/env bash
# bana's own releases (for bana's developers and its release workflow; not shipped).
#
#   .github/release.sh check [X.Y.Z]   bin/bana's BANA_VERSION, manager/Cargo.toml's version and
#                                      bana-manager's in manager/Cargo.lock agree (and are X.Y.Z)
#   .github/release.sh bump X.Y.Z      sets all three to X.Y.Z
#   .github/release.sh pack MANAGER X.Y.Z PLAT OUT
#                                      OUT/bana-vX.Y.Z-PLAT.tar.gz: bana-vX.Y.Z/ with bin/bana (the
#                                      commit stamped: GITHUB_SHA, else HEAD), bin/bana-manager
#                                      (MANAGER, built for PLAT), lib/, LICENSE and README.md.
#                                      PLAT: linux-x64, linux-arm64, macos-x64 or macos-arm64
#   .github/release.sh dist DIR X.Y.Z  DIR's archives as release vX.Y.Z: install.sh (bana installer's,
#                                      with lib/install-hook.sh) and SHA256SUMS, then notes.md (the
#                                      release's text; not uploaded)
#
# A release is tag vX.Y.Z of a commit whose version is X.Y.Z; a '-' (v1.2.0-rc.1) makes a
# prerelease. README.md's "Releasing bana" has the steps.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
repo=${BANA_RELEASE_REPO:-tjrb-xyz/bana} # where bana's releases are
plats="linux-x64 linux-arm64 macos-x64 macos-arm64"
die() { printf '%s\n' "$*" >&2; exit 1; }
usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0" >&2; exit 2; }
valid() { printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; } # VERSION

# FILE's version, as each keeps it.
version_of() { # FILE
  case $1 in
  bin/bana) sed -n 's/^BANA_VERSION=//p' "$root/$1" ;;
  manager/Cargo.toml)
    awk '/^\[/ { p = ($0 == "[package]") } p && /^version *=/ { sub(/^version *= *"/, ""); sub(/".*/, ""); print; exit }' "$root/$1" ;;
  manager/Cargo.lock)
    awk '$0 == "name = \"bana-manager\"" { n = 1; next } n { sub(/^version = "/, ""); sub(/".*/, ""); print; exit }' "$root/$1" ;;
  esac
}
files="bin/bana manager/Cargo.toml manager/Cargo.lock"

check() { # [VERSION]
  local f v want=${1:-} bad=''
  [[ -z $want ]] || valid "$want" || die "not a version: '$want' (X.Y.Z, or X.Y.Z-pre for a prerelease)"
  for f in $files; do
    v=$(version_of "$f")
    [[ -n $v ]] || die "$f: no version found"
    want=${want:-$v}
    [[ $v == "$want" ]] || bad=1
  done
  if [[ -n $bad ]]; then
    for f in $files; do printf '  %s: %s\n' "$f" "$(version_of "$f")" >&2; done
    die "The versions are not all ${1:-the same}: .github/release.sh bump X.Y.Z"
  fi
  echo "bana $want"
}

# Rewrites FILE through the awk program, keeping its mode.
rewrite() { # FILE AWK-PROGRAM VERSION
  local tmp
  tmp=$(mktemp)
  awk -v v="$3" "$2" "$root/$1" >"$tmp"
  cat "$tmp" >"$root/$1"
  rm -f "$tmp"
}

# shellcheck disable=SC2016 # awk's own $0
bump() { # VERSION
  valid "${1:-}" || die "bump X.Y.Z (X.Y.Z-pre for a prerelease), not '${1:-}'"
  rewrite bin/bana '/^BANA_VERSION=/ && !d { $0 = "BANA_VERSION=" v; d = 1 } { print }' "$1"
  rewrite manager/Cargo.toml '/^\[/ { p = ($0 == "[package]") } p && !d && /^version *=/ { $0 = "version = \"" v "\""; d = 1 } { print }' "$1"
  rewrite manager/Cargo.lock 'n { $0 = "version = \"" v "\""; n = 0 } $0 == "name = \"bana-manager\"" { n = 1 } { print }' "$1"
  check "$1"
}

stage=''
trap '[[ -z $stage ]] || rm -rf "$stage"' EXIT

pack() { # MANAGER VERSION PLAT OUT
  local mgr=${1:-} v=${2:-} plat=${3:-} out=${4:-} sha d f own
  [[ -n $out ]] || usage
  valid "$v" || die "pack: X.Y.Z (X.Y.Z-pre for a prerelease), not '$v'"
  [[ " $plats " == *" $plat "* ]] || die "pack: PLAT is one of $plats, not '$plat'"
  [[ -f $mgr && -x $mgr ]] || die "pack: no bana-manager program at $mgr"
  [[ $(version_of bin/bana) == "$v" ]] || die "pack: bin/bana is bana $(version_of bin/bana), not $v"
  sha=${GITHUB_SHA:-$(git -C "$root" rev-parse HEAD 2>/dev/null || true)}
  [[ $sha =~ ^[0-9a-f]{40,64}$ ]] || die "pack: no commit to stamp (GITHUB_SHA, or a git checkout), not '$sha'"
  stage=$(mktemp -d)
  d=$stage/bana-v$v
  mkdir -p "$d/bin" "$d/lib"
  sed "s/^BANA_COMMIT=/BANA_COMMIT=$sha/" "$root/bin/bana" >"$d/bin/bana"
  grep -q "^BANA_COMMIT=$sha" "$d/bin/bana" || die "pack: bin/bana has no BANA_COMMIT= line to stamp"
  cp "$mgr" "$d/bin/bana-manager"
  cp "$root"/lib/*.sh "$root"/lib/install.sh.in "$root"/lib/install.ps1.in "$d/lib/"
  cp "$root/LICENSE" "$root/README.md" "$d/"
  chmod 755 "$d/bin/bana" "$d/bin/bana-manager"
  chmod 644 "$d"/lib/* "$d/LICENSE" "$d/README.md"
  mkdir -p "$out"
  f=$out/bana-v$v-$plat.tar.gz
  (cd "$stage" && find "bana-v$v" -type f | LC_ALL=C sort) >"$stage/list"
  # Owned by root (0:0), not the build's user: a root install's files are root's, whatever
  # the tar. COPYFILE_DISABLE: no ._ files from a Mac's tar.
  if tar --version 2>/dev/null | grep -q 'GNU tar'; then own=(--owner=0 --group=0 --numeric-owner); else own=(--uid 0 --gid 0); fi
  COPYFILE_DISABLE=1 tar "${own[@]}" -C "$stage" -czf "$f.part" -T "$stage/list"
  mv "$f.part" "$f"
  rm -rf "$stage"
  stage=''
  echo "$f"
}

dist() { # DIR VERSION
  local dir=${1:-} v=${2:-} f b n=0 got='' url fence
  [[ -n $v ]] || usage
  valid "$v" || die "dist: X.Y.Z (X.Y.Z-pre for a prerelease), not '$v'"
  [[ -d $dir ]] || die "dist: no directory $dir"
  dir=$(cd "$dir" && pwd)
  for f in "$dir"/bana-*.tar.gz; do
    [[ -f $f ]] || continue
    b=${f##*/}
    case $b in "bana-v$v-"*) ;; *) die "dist: $b is not bana v$v's" ;; esac
    n=$((n + 1))
    b=${b#"bana-v$v-"}
    got+="${got:+, }${b%.tar.gz}"
  done
  ((n)) || die "dist: no bana-v$v-PLAT.tar.gz in $dir (.github/release.sh pack)"
  # bana installer, with its settings here, not from a bana.conf (none is read).
  (cd "$root" && BANA_CONFIG='' BANA_REPO=$repo BANA_PREFIX=bana BANA_INSTALL_NAME=bana BANA_INSTALL_BINS=bana \
    BANA_INSTALL_HOOK=lib/install-hook.sh BANA_INSTALL_HOOK_PS1='' BANA_INSTALL_PREFIX='' BANA_INSTALL_BIN='' \
    BANA_INSTALL_CONFIG='' BANA_RELEASE_FILES="bana-v$v-*.tar.gz" "$BASH" "$root/bin/bana" installer "$dir" --tag "v$v")
  # A prerelease is never releases/latest: its own installer.
  url=https://github.com/$repo/releases/latest/download/install.sh
  [[ $v != *-* ]] || url=https://github.com/$repo/releases/download/v$v/install.sh
  {
    fence='```'
    printf '## Install\n\n%ssh\ncurl -fsSL %s | sh\n%s\n\n' "$fence" "$url" "$fence"
    echo "bana goes in ~/.local/share/bana and the bana command in ~/.local/bin. Already have bana: \`bana upgrade\`."
    echo "Platforms: $got. SHA256SUMS lists every file."
  } >"$dir/notes.md"
  echo "$dir/notes.md"
}

case ${1:-} in
check) check "${2:-}" ;;
bump) bump "${2:-}" ;;
pack) shift && pack "$@" ;;
dist) shift && dist "$@" ;;
*) usage ;;
esac

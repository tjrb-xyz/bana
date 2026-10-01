#!/usr/bin/env bash
# bana's own releases (for bana's developers and its release workflow; not shipped).
#
#   .github/release.sh check [X.Y.Z]   bin/bana's BANA_VERSION, manager/Cargo.toml's version and
#                                      bana-manager's in manager/Cargo.lock agree (and are X.Y.Z)
#   .github/release.sh bump X.Y.Z      sets all three to X.Y.Z
#
# A release is tag vX.Y.Z of a commit whose version is X.Y.Z; a '-' (v1.2.0-rc.1) makes a
# prerelease. README.md's "Releasing bana" has the steps.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
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

case ${1:-} in
check) check "${2:-}" ;;
bump) bump "${2:-}" ;;
*) usage ;;
esac

#!/bin/sh
# Installs bana for this user, from its releases: bana in ~/.local/share/bana, and `bana` on
# the PATH in ~/.local/bin (the release's own install.sh, checked against its SHA256SUMS).
# Run it again to update; bana upgrade does the same.
#
#   curl -fsSL https://raw.githubusercontent.com/tjrb-xyz/bana/main/install.sh | sh
#   curl ... | sh -s -- vX.Y.Z              (that release)
#   curl ... | sh -s -- --git [REF]         (a git checkout in ~/.bana/src instead; REF: a
#                                            branch, tag or commit, main by default)
#   sh install.sh [ARGS]                    (from a checkout of bana)
#
# With no release yet, it takes the git checkout, and says so. A bana linked into ~/.bana/src
# (this script's checkout of before) moves to the release; ~/.bana/src stays until you remove it.
set -eu

repo=${BANA_RELEASE_REPO:-tjrb-xyz/bana}
releases=${BANA_RELEASES:-https://github.com/$repo/releases}
url=${BANA_URL:-https://github.com/$repo}
home=${BANA_HOME:-$HOME/.bana}
src=$home/src
bin=$HOME/.local/bin

die() { echo "$*" >&2; exit 1; }
next() {
  [ -f "$home/daemon.d/settings" ] ||
    echo "Next: bana daemon install, once, then bana add in each project's checkout (README: CI on push)"
}

# The git checkout, as this script did before the releases.
git_install() { # REF
  command -v git >/dev/null || die "git is needed (on a Mac: xcode-select --install)"
  if [ -d "$src/.git" ]; then
    git -C "$src" fetch -q origin
  else
    mkdir -p "$(dirname "$src")"
    git clone -q "$url" "$src"
  fi
  git -C "$src" -c advice.detachedHead=false checkout -q "$1"
  case $1 in main | master) git -C "$src" merge -q --ff-only "origin/$1" ;; esac
  mkdir -p "$bin"
  ln -sf "$src/bin/bana" "$bin/bana"
  echo "bana $(git -C "$src" describe --always --tags) is $bin/bana"
  case ":$PATH:" in *":$bin:"*) ;; *) echo "Add $bin to your PATH (e.g. in ~/.zshrc: export PATH=\$HOME/.local/bin:\$PATH)" ;; esac
  next
}

tag=''
case ${1:-} in
--git) git_install "${2:-main}"; exit 0 ;;
'') ;;
v[0-9]*) tag=$1 ;;
-*) die "install.sh: unknown option $1 (vX.Y.Z, or --git [REF])" ;;
*) git_install "$1"; exit 0 ;; # a branch or commit, as before
esac

# The newest release: where releases/latest redirects. None yet: the git checkout.
if [ -z "$tag" ]; then
  to=$(curl -fsS --max-time 20 -o /dev/null -w '%{redirect_url}' "$releases/latest") ||
    die "could not ask $releases/latest which bana is the newest"
  case $to in
  */tag/*) tag=${to##*/tag/} ;;
  *)
    echo "No bana release yet: a git checkout in $src instead"
    git_install main
    exit 0
    ;;
  esac
fi
printf '%s\n' "$tag" | grep -Eqx 'v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?' || die "not a bana release: '$tag' (vX.Y.Z)"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1"
  elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 "$1" | awk '{ print $NF }'
  else echo none; fi | awk '{ print $1; exit }'
}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for f in install.sh SHA256SUMS; do
  if [ -n "${BANA_RELEASES:-}" ]; then
    curl -fsSL --max-time 120 -o "$tmp/$f" "$releases/download/$tag/$f"
  else
    curl -fsSL --proto '=https' --max-time 120 -o "$tmp/$f" "$releases/download/$tag/$f"
  fi || die "could not download $releases/download/$tag/$f"
done
want=$(awk '$2 == "install.sh" || $2 == "*install.sh" { print $1; exit }' "$tmp/SHA256SUMS")
got=$(sha256 "$tmp/install.sh")
[ "$got" != none ] || die "need sha256sum, shasum or openssl to check install.sh"
{ [ -n "$want" ] && [ "$got" = "$want" ]; } || die "install.sh does not match SHA256SUMS: nothing changed"
{ grep -qx "NAME='bana'" "$tmp/install.sh" && grep -qx "TAG='$tag'" "$tmp/install.sh"; } ||
  die "$releases/download/$tag/install.sh is not bana $tag's: nothing changed"

# The commands where the release has them, else in ~/.local/bin.
prefix=${XDG_DATA_HOME:-$HOME/.local/share}/bana
b=$(sed -n 's/^bin=//p' "$prefix/receipt" 2>/dev/null | head -1)
# This script's link of before is always in ~/.local/bin: the release's command takes its place.
if [ -z "$b" ] && [ -L "$bin/bana" ]; then
  case $(readlink "$bin/bana") in "$src"/*) b=$bin ;; esac
fi
bin=${b:-${XDG_BIN_HOME:-$bin}}
rm -f "$home/.upgrade-from"
st=0
INSTALL_URL=${BANA_RELEASES:+$releases/download/$tag} sh "$tmp/install.sh" --yes --prefix "$prefix" --bin-dir "$bin" || st=$?
# The hook made way for the release's bana, and the install failed after: the old one again.
if [ "$st" != 0 ] && [ -f "$home/.upgrade-from" ] && [ ! -e "$bin/bana" ] && [ ! -L "$bin/bana" ]; then
  ln -s "$(cat "$home/.upgrade-from")" "$bin/bana"
  echo "$bin/bana points to $(cat "$home/.upgrade-from") again" >&2
fi
[ "$st" = 0 ] || exit "$st"
if [ -f "$home/.upgrade-from" ]; then
  echo "$src is no longer used: rm -rf $src (to go back: ln -sfn $src/bin/bana $bin/bana)"
fi
next

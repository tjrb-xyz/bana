#!/bin/sh
# Installs bana for this user: a checkout in ~/.bana/src, and `bana` on the PATH
# in ~/.local/bin. Run it again to update; pass a tag or commit to pin one.
#
#   curl -fsSL -H "Authorization: token $(gh auth token)" \
#     https://raw.githubusercontent.com/tjrb-xyz/bana/main/install.sh | sh
#   curl ... | sh -s -- REF                 (a tag or commit)
#   sh install.sh [REF]                     (from a checkout of bana)
#
# The header is for a private bana (drop it once bana is public). With the GitHub
# CLI signed in, git fetches bana through gh's credentials, as the daemon does.
set -eu

ref=${1:-main}
url=${BANA_URL:-https://github.com/tjrb-xyz/bana}
src=${BANA_HOME:-$HOME/.bana}/src
bin=$HOME/.local/bin

command -v git >/dev/null || { echo "git is needed (on a Mac: xcode-select --install)" >&2; exit 1; }
# git through gh when it is signed in, else through your own git credentials.
g() {
  if command -v gh >/dev/null && gh auth status >/dev/null 2>&1; then
    git -c credential.helper= -c "credential.helper=!$(command -v gh) auth git-credential" "$@"
  else
    git "$@"
  fi
}

if [ -d "$src/.git" ]; then
  g -C "$src" fetch -q origin
else
  mkdir -p "$(dirname "$src")"
  g clone -q "$url" "$src"
fi
git -C "$src" -c advice.detachedHead=false checkout -q "$ref"
case $ref in main | master) git -C "$src" merge -q --ff-only "origin/$ref" ;; esac
mkdir -p "$bin"
ln -sf "$src/bin/bana" "$bin/bana"
echo "bana $(git -C "$src" describe --always --tags) is $bin/bana"
case ":$PATH:" in *":$bin:"*) ;; *) echo "Add $bin to your PATH (e.g. in ~/.zshrc: export PATH=\$HOME/.local/bin:\$PATH)" ;; esac
echo "Next, in your project's checkout: bana daemon install (README: CI on push)"

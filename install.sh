#!/bin/sh
# Installs bana for this user: a checkout in ~/.bana/src, and `bana` on the PATH
# in ~/.local/bin. Run it again to update; pass a tag or commit to pin one.
#
#   sh install.sh [REF]                     (from a checkout of bana)
#   curl -fsSL https://raw.githubusercontent.com/tjrb-xyz/bana/main/install.sh | sh -s -- [REF]
#
# A private bana: clone it yourself (gh repo clone tjrb-xyz/bana ~/.bana/src),
# then run ~/.bana/src/install.sh.
set -eu

ref=${1:-main}
url=${BANA_URL:-https://github.com/tjrb-xyz/bana}
src=${BANA_HOME:-$HOME/.bana}/src
bin=$HOME/.local/bin

if [ -d "$src/.git" ]; then
  git -C "$src" fetch -q origin
else
  mkdir -p "$(dirname "$src")"
  git clone -q "$url" "$src"
fi
git -C "$src" -c advice.detachedHead=false checkout -q "$ref"
case $ref in main | master) git -C "$src" merge -q --ff-only "origin/$ref" ;; esac
mkdir -p "$bin"
ln -sf "$src/bin/bana" "$bin/bana"
echo "bana $(git -C "$src" describe --always --tags) is $bin/bana"
case ":$PATH:" in *":$bin:"*) ;; *) echo "Add $bin to your PATH (e.g. in ~/.zshrc: export PATH=\$HOME/.local/bin:\$PATH)" ;; esac

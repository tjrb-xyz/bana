# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets bana_root, base_home and BANA_VERSION
# bana upgrade: a newer bana, from its releases. It runs that release's install.sh (its sha256
# in the release's SHA256SUMS), whose hook moves the daemon first: if the new daemon does not
# come up, the one before runs again and nothing changes. Sourced by bin/bana.
#
#   bana upgrade [vX.Y.Z] [--check] [--now] [--yes]   the newest release, or this one
#     --check          only say whether a newer one is out (exit 10 when it is)
#     --now            the daemon restarts at once, even while a build runs (it runs again once)
#     --yes            no question (an older release than this one is a downgrade: it asks)
#
# A bana from the old install.sh (a checkout in ~/.bana/src) moves to the releases: its
# command becomes the release's, and ~/.bana/src stays until you remove it. To go back to
# the release before: bana upgrade vX.Y.Z (the installer keeps it). A git checkout of bana,
# or a submodule, updates with git instead. BANA_RELEASES names another releases page.

upgrade_usage() { awk '/^#   bana upgrade/, /^#     --yes/ { sub(/^# ?/, ""); print }' "$bana_root/lib/upgrade.sh" >&2; exit 2; }

up_tag_re='^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'

# The repository bana's releases are of: a release's own (its receipt's), else bana's.
up_repo() {
  local r=''
  [[ $(bana_kind) != release ]] || r=$(sed -n 's/^repo=//p' "$bana_root/../receipt" | head -1)
  [[ $r =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || r=${BANA_RELEASE_REPO:-tjrb-xyz/bana}
  echo "$r"
}
up_releases() { echo "${BANA_RELEASES:-https://github.com/$(up_repo)/releases}"; }

# A < B, = or > (vX.Y.Z[-pre], as notes.rs's version_cmp: a prerelease is below its final).
up_vcmp() { # A B
  local a=${1#v} b=${2#v} pa='' pb='' i x y xs ys
  [[ $a != *-* ]] || { pa=${a#*-}; a=${a%%-*}; }
  [[ $b != *-* ]] || { pb=${b#*-}; b=${b%%-*}; }
  IFS=. read -r -a xs <<<"$a"
  IFS=. read -r -a ys <<<"$b"
  for i in 0 1 2; do
    x=$((10#${xs[i]:-0})) y=$((10#${ys[i]:-0}))
    ((x == y)) || { ((x < y)) && echo '<' || echo '>'; return; }
  done
  if [[ $pa == "$pb" ]]; then echo '='; return; fi
  [[ -n $pa ]] || { echo '>'; return; }
  [[ -n $pb ]] || { echo '<'; return; }
  IFS=. read -r -a xs <<<"$pa"
  IFS=. read -r -a ys <<<"$pb"
  for ((i = 0; ; i++)); do
    x=${xs[i]:-} y=${ys[i]:-}
    if [[ -z $x || -z $y ]]; then [[ -z $x ]] && echo '<' || echo '>'; return; fi
    [[ $x != "$y" ]] || continue
    if [[ $x =~ ^[0-9]+$ && $y =~ ^[0-9]+$ ]]; then ((10#$x < 10#$y)) && echo '<' || echo '>'
    elif [[ $x =~ ^[0-9]+$ ]]; then echo '<'
    elif [[ $y =~ ^[0-9]+$ ]]; then echo '>'
    else [[ $x < "$y" ]] && echo '<' || echo '>'; fi
    return
  done
}

up_sha256() { # FILE
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1"
  elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 "$1" | awk '{ print $NF }'
  else echo none; fi | awk '{ print $1; exit }'
}

# The newest release's tag (none: no release yet), from where releases/latest redirects.
up_latest() { # RELEASES
  local to
  to=$(curl -fsS --max-time 20 -o /dev/null -w '%{redirect_url}' "$1/latest") ||
    die "could not ask $1/latest which bana is the newest"
  case $to in */tag/*) echo "${to##*/tag/}" ;; esac
}

upgrade_main() {
  local tag='' named='' check='' now='' yes='' kind base was to tmp f want got prefix bindir='' l st=0 proto
  while (($#)); do
    case $1 in
    --check) check=1 ;;
    --now) now=1 ;;
    -y | --yes) yes=1 ;;
    v[0-9]*) [[ -z $tag ]] || upgrade_usage; tag=$1 named=1 ;;
    *) upgrade_usage ;;
    esac
    shift
  done
  kind=$(bana_kind)
  base=$(up_releases)
  case $kind in
  submodule)
    l=$(git -C "$bana_root" rev-parse --show-superproject-working-tree)
    to=$(cd "$bana_root" && pwd -P)
    die "bana is a submodule here: git -C $l submodule update --remote ${to#"$l"/}" ;;
  dev) die "bana is a git checkout here: git -C $bana_root pull, then bana daemon install" ;;
  copy) die "this bana is a copy ($bana_root), which does not upgrade: curl -fsSL $base/latest/download/install.sh | sh" ;;
  esac

  if [[ -z $tag ]]; then
    tag=$(up_latest "$base")
    [[ -n $tag ]] || { echo "no bana release yet ($base)"; return 0; }
  fi
  [[ $tag =~ $up_tag_re ]] || die "not a bana release: '$tag' (vX.Y.Z)"
  was=$(up_vcmp "$tag" "v$BANA_VERSION")
  if [[ -n $check ]]; then
    if [[ $was == '>' ]]; then echo "bana $BANA_VERSION ($kind); newest $tag: bana upgrade"; exit 10; fi
    echo "bana $BANA_VERSION ($kind); newest $tag"
    return 0
  fi
  if [[ $was == '=' && $kind == release ]]; then echo "bana $tag is the newest"; return 0; fi
  if [[ $was == '<' ]]; then
    [[ -n $named ]] || { echo "bana $BANA_VERSION is newer than the newest release, $tag (bana upgrade $tag takes it)"; return 0; }
    if [[ -z $yes ]]; then
      (: </dev/tty) 2>/dev/null || die "$tag is older than bana $BANA_VERSION, a downgrade: --yes"
      printf 'bana %s to %s is a downgrade. Go on? [y/N] ' "$BANA_VERSION" "$tag" >/dev/tty
      read -r l </dev/tty || l=
      [[ $l == [yY]* ]] || die "nothing changed"
    fi
  fi

  # Its install.sh, checked against its SHA256SUMS before it runs.
  tmp=$(mktemp -d)
  # shellcheck disable=SC2064 # tmp is fixed now
  trap "rm -rf '$tmp'" EXIT
  proto=(--proto '=https')
  [[ -z ${BANA_RELEASES:-} ]] || proto=()
  for f in install.sh SHA256SUMS; do
    curl -fsSL ${proto[@]+"${proto[@]}"} --max-time 120 -o "$tmp/$f" "$base/download/$tag/$f" ||
      die "could not download $base/download/$tag/$f: nothing changed"
  done
  want=$(awk '$2 == "install.sh" || $2 == "*install.sh" { print $1; exit }' "$tmp/SHA256SUMS")
  got=$(up_sha256 "$tmp/install.sh")
  [[ $got != none ]] || die "need sha256sum, shasum or openssl to check install.sh: nothing changed"
  [[ -n $want && $got == "$want" ]] || die "install.sh does not match SHA256SUMS: nothing changed"
  { grep -qx "NAME='bana'" "$tmp/install.sh" && grep -qx "TAG='$tag'" "$tmp/install.sh"; } ||
    die "$base/download/$tag/install.sh is not bana $tag's: nothing changed"

  # Where it goes: where this release is, or the installer's default (and the command
  # where the old install.sh linked it).
  if [[ $kind == release ]]; then
    prefix=$(dirname "$bana_root")
    bindir=$(sed -n 's/^bin=//p' "$prefix/receipt" | head -1)
    # The prefix as the installer's link names it: bana_root is the physical path, another
    # one when HOME is reached through a link (the installer would not know its link).
    l=$(readlink "$bindir/bana" 2>/dev/null) || l=''
    l=${l%/current/bin/bana}
    if [[ -n $l && $l != "$prefix" && -d $l && $(cd "$l" && pwd -P) == "$(cd "$prefix" && pwd -P)" ]]; then
      prefix=$l
    fi
  else
    prefix=${XDG_DATA_HOME:-$HOME/.local/share}/bana
    if l=$(command -v bana) && [[ -L $l ]]; then
      case $(readlink "$l") in "$bana_root"/* | "$(cd "$bana_root" && pwd -P)"/*) bindir=$(dirname "$l") ;; esac
    fi
  fi
  bindir=${bindir:-${XDG_BIN_HOME:-$HOME/.local/bin}}
  rm -f "$base_home/.upgrade-from"
  BANA_UPGRADE_NOW=$now INSTALL_URL=${BANA_RELEASES:+$base/download/$tag} \
    sh "$tmp/install.sh" --yes --prefix "$prefix" --bin-dir "$bindir" || st=$?
  # The hook made way for the release's command, and the install failed after: the old one again.
  if ((st)) && [[ -f $base_home/.upgrade-from && ! -e $bindir/bana && ! -L $bindir/bana ]]; then
    ln -s "$(cat "$base_home/.upgrade-from")" "$bindir/bana"
    warn "$bindir/bana points to $(cat "$base_home/.upgrade-from") again"
  fi
  ((st == 0)) || exit "$st"
  "$bindir/bana" version
  if [[ $kind == legacy ]]; then
    echo "$base_home/src is no longer used: rm -rf $base_home/src (to go back: ln -sfn $base_home/src/bin/bana $bindir/bana)"
  fi
}

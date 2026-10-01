# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets repo, prefix, home, bana_root
# bana installer and bana install: the project's own installer, compiled from bana's templates
# (lib/install.sh.in, lib/install.ps1.in) and bana.conf's install.* keys. Sourced by bin/bana.
#
#   bana installer DIST (--tag TAG | --label LABEL)
#     writes DIST/install.sh (POSIX sh: macOS and Linux), DIST/install.ps1 (Windows, when
#     DIST has a Windows zip) and DIST/SHA256SUMS, last, over them and the release files.
#     The archives are DIST's release files named NAME-...-(linux|macos)-(x64|arm64).tar.gz
#     and NAME-...-windows-(x64|arm64).zip, one per platform, each holding one directory;
#     their sha256 is baked into the installers, which refuse anything else.
#     --tag TAG        a GitHub release: the installers download from it (gh, else curl)
#     --label LABEL    a build of bana's daemon (nightly-<sha>, say): its installer needs --from
#   bana install [BUILD | --from FILE|DIR] [INSTALLER OPTIONS]
#     a daemon build's files on this machine: sh DIST/install.sh --from DIST, in your terminal
#     (the project's hook may ask, and use sudo). BUILD: its number; default the newest
#     build with files. --from: a directory with install.sh, or an archive beside one.
#     INSTALLER OPTIONS: --yes, --force, --no-hook, --prefix DIR, --bin-dir DIR, --uninstall,
#     --purge (sh install.sh --help)
# bana.conf, all optional:
#   install.name = NAME        the installed program's name (default: prefix)
#   install.bins = A B          the commands linked onto the PATH (default: every program in bin/);
#                               install.sh's: install.ps1 puts all of bin\ on the PATH
#   install.hook = FILE         the project's lifecycle script, a path inside the archive:
#                               sh FILE pre-install|post-install|pre-uninstall|post-uninstall
#   install.hook_ps1 = FILE     the same for Windows (a PowerShell script)
#   install.prefix = DIR        where versions go (default ~/.local/share/NAME)
#   install.bin = DIR           where commands are linked (default ~/.local/bin)
#   install.config = DIR        the program's settings, given to the hook; --purge removes them
#                               (default ~/.config/NAME)
#   install.env.KEY = VALUE     given to the hook as KEY (a leading ~ is your home)
#   release.files = GLOBS       the files of DIST that are the release (default *)
#   (release.platforms is the daemon's: the platforms a release's page expects)
# install.prefix, install.bin and install.config are install.sh's (prefix and config not ~
# or / themselves: uninstalling removes what is in them); install.ps1 keeps
# Windows' places (%LOCALAPPDATA%\Programs\NAME, %APPDATA%\NAME). Every value is printable
# ASCII: gh refuses to pipe an escape byte, and Windows PowerShell 5.1 reads a .ps1 as ANSI.

installer_usage() { awk '/^#   bana installer DIST/, /^#     --purge/ { sub(/^# ?/, ""); print }' "$bana_root/lib/installer.sh" >&2; exit 2; }

installer_sum() { # FILE
  if command -v sha256sum >/dev/null; then sha256sum "$1"
  elif command -v shasum >/dev/null; then shasum -a 256 "$1"
  else die "bana installer: no sha256sum or shasum here"; fi | awk '{ print $1; exit }'
}

# Single quotes, for sh and for PowerShell.
installer_q() { # VALUE [SQ_ESCAPED]
  local s=$1 out='' sq="'" esc="'\\''"
  [[ $# -lt 2 ]] || esc=$2
  while [[ $s == *"$sq"* ]]; do
    out+=${s%%"$sq"*}$esc
    s=${s#*"$sq"}
  done
  printf "'%s'" "$out$s"
}
installer_qps() { installer_q "$1" "''"; }
# VAR='VALUE' for install.sh; a ~ or $ in it is bana.conf's, for the installer (or nobody) to expand.
installer_let() { # VAR VALUE
  case $2 in *'~'* | *'$'* | *'`'*) echo "# shellcheck disable=SC2016,SC2088 # as bana.conf says it" ;; esac
  echo "$1=$(installer_q "$2")"
}

installer_printable() { # KEY VALUE
  [[ -z $(printf '%s' "$2" | LC_ALL=C tr -d ' -~') ]] ||
    die "bana installer: $1 has a character that is not printable ASCII"
}
installer_check() { # KEY VALUE REGEX
  installer_printable "$1" "$2"
  [[ -z $2 || $2 =~ $3 ]] || die "bana installer: $1: not '$2'"
}

# An archive's entries (tar -tz, unzip -Z1 on stdin): the first outside its directory, and
# its top-level names.
installer_outside() { awk '{ gsub(/\\/, "/"); p = "/" $0 "/" } substr($0, 1, 1) == "/" || index(p, "/../") { print; exit }'; }
installer_tops() {
  awk '{ gsub(/\\/, "/"); while (substr($0, 1, 2) == "./") $0 = substr($0, 3); i = index($0, "/"); if (i) $0 = substr($0, 1, i - 1) }
    $0 != "" && $0 != "." { print }' | sort -u
}
installer_archive() { # FILE: holds one directory, and nothing outside it
  local list bad tops
  case $1 in
  *.tar.gz) list=$(tar -tzf "$1" 2>/dev/null) || die "bana installer: ${1##*/} is not a tar.gz" ;;
  *) command -v unzip >/dev/null || return 0
    list=$(unzip -Z1 "$1" 2>/dev/null) || die "bana installer: ${1##*/} is not a zip" ;;
  esac
  bad=$(installer_outside <<<"$list")
  [[ -z $bad ]] || die "bana installer: ${1##*/} has an entry outside its directory: $bad"
  tops=$(installer_tops <<<"$list")
  [[ -n $tops && $tops != *$'\n'* ]] ||
    die "bana installer: ${1##*/} should hold one directory, and has: $(tr '\n' ' ' <<<"$tops")"
}

installer_main() {
  local dist='' tag='' local_=0 name bins hook hook_ps1 iprefix ibin iconfig k v envs='' envps=''
  local globs=() g f b p plat unix='' win='' files=() sums sh ps
  while (($#)); do
    case $1 in
    --tag | --label)
      [[ $# -ge 2 && -z $tag ]] || installer_usage
      tag=$2
      [[ $1 == --tag ]] || local_=1
      shift
      ;;
    -*) installer_usage ;;
    *) [[ -z $dist ]] || installer_usage; dist=$1 ;;
    esac
    shift
  done
  [[ -n $dist && -n $tag ]] || installer_usage
  [[ -d $dist ]] || die "bana installer: no directory $dist"
  ((local_)) || need_repo
  [[ -z $repo || $repo =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "repo: owner/name, not '$repo'"
  installer_check "the tag" "$tag" '^[A-Za-z0-9][A-Za-z0-9._+-]{0,99}$'
  [[ $tag != current && $tag != receipt ]] || die "bana installer: '$tag' is the installer's own name, not a tag"

  name=$(conf install.name "$prefix")
  installer_check install.name "$name" '^[A-Za-z0-9][A-Za-z0-9._-]*$'
  [[ -n $name ]] || die "bana installer: install.name is empty"
  bins=$(words "$(conf install.bins)" | tr -s ' ' ' ' | sed 's/^ //; s/ $//')
  installer_check install.bins "$bins" '^[A-Za-z0-9_-][A-Za-z0-9._-]*( [A-Za-z0-9_-][A-Za-z0-9._-]*)*$'
  hook=$(conf install.hook)
  hook_ps1=$(conf install.hook_ps1)
  iprefix=$(conf install.prefix)
  ibin=$(conf install.bin)
  iconfig=$(conf install.config)
  for k in hook hook_ps1; do
    v=$(conf "install.$k")
    installer_check "install.$k" "$v" '^[A-Za-z0-9_-][A-Za-z0-9._-]*(/[A-Za-z0-9_-][A-Za-z0-9._-]*)*$'
    [[ /$v/ != */../* ]] || die "bana installer: install.$k: a path inside the archive, not '$v'"
  done
  for k in prefix bin config; do
    v=$(conf "install.$k")
    installer_check "install.$k" "$v" '^(~|~/.*|/.*)$'
    # Uninstalling removes what is in them (--purge: all of install.config).
    [[ $k == bin || -z $v || ! $v =~ ^(~|/)/*$ ]] || die "bana installer: install.$k: a directory of its own, not '$v'"
  done
  # install.env.KEY: for the hook, as its environment.
  while IFS= read -r k; do
    [[ -n $k ]] || continue
    [[ $k =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || die "bana installer: install.env.$k: a variable name, not '$k'"
    case $k in
    INSTALL_* | PATH | HOME | IFS | ENV | BASH_ENV | CDPATH | SHELL | PS4 | LD_* | DYLD_*)
      die "bana installer: install.env.$k: the installer's own, or the shell's" ;;
    esac
    v=$(conf "install.env.$k")
    installer_printable "install.env.$k" "$v"
    envs+=$k=$v$'\n'
    envps+="  $(installer_qps "$k") = $(installer_qps "$v")"$'\n'
  done < <(conf_keys install.env.)

  read -r -a globs <<<"$(words "$(conf release.files '*')")" || true
  ((${#globs[@]})) || globs=('*')
  for f in "$dist"/*; do
    [[ -f $f && ! -L $f ]] || continue
    b=${f##*/}
    case $b in install.sh | install.ps1 | SHA256SUMS | .*) continue ;; esac # the installer's own
    for g in "${globs[@]}"; do
      # shellcheck disable=SC2053 # a glob
      [[ $b == $g ]] && break
      g=''
    done
    [[ -n $g ]] || continue
    files+=("$b")
    case $b in
    *-linux-x64.tar.gz | *-linux-arm64.tar.gz | *-macos-x64.tar.gz | *-macos-arm64.tar.gz)
      p=${b%.tar.gz}
      plat=${p%-*}
      plat=${plat##*-}-${p##*-}
      [[ $'\n'$unix != *$'\n'"$plat "* ]] || die "bana installer: two archives for $plat in $dist: $(grep "^$plat " <<<"$unix" | cut -d' ' -f2) and $b"
      installer_archive "$f"
      unix+="$plat $b $(installer_sum "$f")"$'\n'
      ;;
    *-windows-x64.zip | *-windows-arm64.zip)
      p=${b%.zip}
      p=${p##*-}
      [[ $win != *"'$p' ="* ]] || die "bana installer: two archives for windows-$p in $dist"
      installer_archive "$f"
      win+="  $(installer_qps "$p") = @($(installer_qps "$b"), '$(installer_sum "$f")')"$'\n'
      ;;
    esac
  done
  ((${#files[@]})) || die "bana installer: no release files in $dist (release.files = ${globs[*]})"
  [[ -n $unix$win ]] ||
    die "bana installer: no archives in $dist named NAME-...-(linux|macos)-(x64|arm64).tar.gz or NAME-...-windows-(x64|arm64).zip"

  sh=$dist/install.sh
  ps=$dist/install.ps1
  rm -f "$dist/SHA256SUMS" "$sh.part" "$ps.part"
  if [[ -n $unix ]]; then
    {
      echo '#!/bin/sh'
      echo "# $name $tag's installer, made by bana from bana.conf (install.*):"
      if ((local_)); then
        echo "#   sh install.sh --from DIR (DIR: this build's files; bana install on the daemon's machine)"
      else
        echo "#   curl -fsSL https://github.com/$repo/releases/download/$tag/install.sh | sh"
        echo "#   gh release download $tag -R $repo -p install.sh -O - | sh   (a private repository)"
      fi
      installer_let NAME "$name"
      installer_let REPO "$repo"
      installer_let TAG "$tag"
      echo "LOCAL=$local_"
      installer_let BINS "$bins"
      installer_let HOOK "$hook"
      installer_let PREFIX_DIR "$iprefix"
      installer_let BIN_DIR "$ibin"
      installer_let CONFIG "$iconfig"
      installer_let ENVS "${envs%$'\n'}"
      installer_let ASSETS "${unix%$'\n'}"
      cat "$bana_root/lib/install.sh.in"
    } >"$sh.part"
  else
    rm -f "$sh" # only a Windows zip: install.ps1 alone
  fi
  if [[ -n $win ]]; then
    {
      echo "# $name $tag's installer, made by bana from bana.conf (install.*)."
      sed '/^#@BUILD@$/,$d' "$bana_root/lib/install.ps1.in"
      echo "\$Name = $(installer_qps "$name")"
      echo "\$Repo = $(installer_qps "$repo")"
      echo "\$Tag = $(installer_qps "$tag")"
      echo "\$Local = \$$( ((local_)) && echo true || echo false)"
      echo "\$HookPs1 = $(installer_qps "$hook_ps1")"
      echo "\$HookEnv = @{"
      printf '%s' "$envps"
      echo "}"
      echo "\$Assets = @{"
      printf '%s' "$win"
      echo "}"
      sed '1,/^#@BUILD@$/d' "$bana_root/lib/install.ps1.in"
    } >"$ps.part"
  fi
  # Printable ASCII only: gh will not write an asset with an escape byte to a pipe, and
  # Windows PowerShell 5.1 reads a .ps1 without a BOM as ANSI.
  for f in "$sh.part" "$ps.part"; do
    [[ -f $f ]] || continue
    if LC_ALL=C grep -q '[^ -~]' "$f"; then
      rm -f "$sh.part" "$ps.part"
      die "bana installer: ${f##*/} would have a byte that is not printable ASCII"
    fi
  done
  if [[ -n $unix ]]; then chmod 755 "$sh.part"; mv "$sh.part" "$sh"; files+=(install.sh); fi
  if [[ -n $win ]]; then mv "$ps.part" "$ps"; files+=(install.ps1); else rm -f "$ps"; fi
  # SHA256SUMS, last: the release's files and its installers, for sha256sum -c.
  sums=''
  for b in "${files[@]}"; do sums+="$(installer_sum "$dist/$b")  $b"$'\n'; done
  printf '%s' "$sums" | LC_ALL=C sort -k2 >"$dist/SHA256SUMS.part"
  mv "$dist/SHA256SUMS.part" "$dist/SHA256SUMS"
  [[ -z $unix ]] || echo "$sh"
  [[ -z $win ]] || echo "$ps"
  echo "$dist/SHA256SUMS"
}

# bana install: a daemon build's files, installed here by their own install.sh.
install_main() {
  local dir='' from='' f b=''
  case ${1:-} in
  --from)
    [[ $# -ge 2 ]] || installer_usage
    from=$2
    shift 2
    if [[ -d $from ]]; then dir=$from; else dir=$(dirname "$from"); fi
    ;;
  [0-9]*)
    [[ $1 =~ ^[0-9]+$ ]] || installer_usage
    dir=$home/builds/$1/dist
    [[ -f $dir/install.sh || -f $dir/install.ps1 ]] || die "Build $1 has no files to install ($dir)"
    shift
    ;;
  esac
  if [[ -z $dir ]]; then
    for f in "$home"/builds/*/dist/install.sh; do
      [[ -f $f ]] || continue
      f=${f%/dist/install.sh} && f=${f##*/}
      [[ $f =~ ^[0-9]+$ ]] && { [[ -z $b ]] || ((f > b)); } && b=$f
    done
    [[ -n $b ]] || die "No daemon build here has files to install (a green build of a job that uploads archives)"
    dir=$home/builds/$b/dist
    say "Build $b"
  fi
  [[ ! -f $dir/install.ps1 || -f $dir/install.sh ]] || die "$dir has only a Windows build: install.ps1, in PowerShell there"
  [[ -f $dir/install.sh ]] || die "No install.sh in $dir (bana installer $dir --label L writes one)"
  exec sh "$dir/install.sh" --from "${from:-$dir}" "$@"
}

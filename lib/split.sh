# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets bana_root, os, prefix, repo, home, conf_file
# bana split: a private repository's CI and releases on a public one's GitHub Actions
# (docs/SPLIT.md). The public repository holds a README and one workflow, bana.yml, which
# bana renders from lib/split.yml.in; a run there fetches the private commit with a
# read-only deploy key, builds it with act, prints only its steps, and encrypts act's
# output to a key that stays on this machine. The daemon dispatches the runs, and writes
# what each did to the private repository (a commit comment) and to its page. Sourced by
# bin/bana; PROJECT: this checkout's, added here (bana add).
#
#   bana split [status]           on or off, the public repository, logs, the last remote run
#   bana split plan [--repo OWNER/NAME] [--web]   what bana split on would do; changes nothing
#   bana split on [--repo OWNER/NAME] [--web] [--releases-only]
#                                 the wizard: checks, the plan, the risks, your typed yes, then
#                                 the public repository (default OWNER/<name>-releases): made
#                                 here with gh, or on GitHub's page (--web, or answer 2), with
#                                 its workflow, deploy key and settings. Run again, it resumes.
#                                 --releases-only: releases go there, builds stay here
#   bana split check [--quick]    the public side as bana left it: ok, WARN or FAIL (exit 1)
#   bana split ci github|local    where pushes build (fix rounds and bana ci: always here)
#   bana split logs private|public   what a run prints on the public repository: its steps,
#                                 or its whole output too (a typed yes)
#   bana split sync [--yes]       bana.yml again, rendered by this bana (after bana upgrade)
#   bana split rekey [--yes]      a new deploy key and seal key; the old ones go
#   bana split purge-runs [--yes] deletes the public repository's runs
#   bana split off [--yes] [--purge-runs]   undo: the deploy key first; the repository stays
#   bana split render             the bana.yml this project's public repository gets
#   bana split lint [FILE]        checks a bana.yml (default: the render): only
#                                 workflow_dispatch, no permissions, no ${{ }} in run:, no
#                                 action but upload-artifact pinned, no cache, 1-day retention,
#                                 images pinned by digest, no job on the runner's machine
# Off a terminal, on takes --repo and BANA_SPLIT_CONSENT (the exact phrase it asks for).

split_usage() { awk '/^#   bana split \[status/, /^# Off a terminal/ { sub(/^# ?/, ""); print }' "$bana_root/lib/split.sh" >&2; exit 2; }

# Pins, each to check against GitHub when it changes (docs/SPLIT.md: LIVE-CHECK):
# act in the public runner (its Linux x86_64 tarball's sha256, from the release's
# checksums.txt), and in a systemd container (lib/systemd.sh's act_linux: x86_64, or arm64
# on Apple silicon); actions/upload-artifact's commit (v7.0.1); act.image's default, by the
# digest Docker Hub gives its tag (docker buildx imagetools inspect); GitHub's ssh host key
# (docs.github.com: GitHub's SSH key fingerprints).
split_act_version=0.2.89
split_act_sha256=0191d6f1f3b716b5c55820032605d05fc3c1cdbf581ebeff655019e5dd1524c0
# shellcheck disable=SC2034 # lib/systemd.sh's act_linux reads it
split_act_sha256_arm64=daa8679ba9615a74d2d0cec321dc593f21948a2a11bb65862b063d8b930f4bcb
split_upload=043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
split_upload_tag=v7.0.1
split_image_tag=catthehacker/ubuntu:act-24.04
split_image_digest=sha256:c58e2b364da03b0c804c7d660f2ecbedf2f221a382b9baa0b344b0144780ff43
split_host_key='github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl'

split_q() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; } # VALUE, single-quoted for sh

# ---- the runner: bana.yml's steps on GitHub (split_render copies this part) ----------
# Each step runs bash runner.sh STEP: check, fetch, build, summary, seal. Its console
# gets a line a step (`job / step: ok (12s)`); with BANA_LOGS=public, act's output too.

split_runner_main() { # STEP
  local d=${RUNNER_TEMP:?}/bana
  case ${1:-} in
  check) split_runner_check ;;
  fetch) split_runner_fetch "$d" ;;
  build) split_runner_build "$d" ;;
  summary) split_runner_summary "$d" ;;
  seal) split_runner_seal "$d" "$d/sealed" ;;
  *) echo "bana: no step ${1:-}"; return 2 ;;
  esac
}

# The run's inputs, as the workflow passes them (through env, never into the script).
split_runner_check() {
  local bad=''
  [[ ${BANA_SHA:-} =~ ^[0-9a-f]{40}$ ]] || bad+=" sha"
  [[ ${BANA_ID:-} =~ ^[a-z0-9-]+$ ]] || bad+=" id"
  [[ ${BANA_NONCE:-} =~ ^[0-9a-f]{32}$ ]] || bad+=" nonce"
  [[ -z ${BANA_REF:-} || $BANA_REF =~ ^refs/(heads|tags)/[A-Za-z0-9._/+-]+$ ]] || bad+=" ref"
  [[ ${BANA_TIER:-} =~ ^[A-Za-z0-9_.-]*$ ]] || bad+=" tier"
  [[ ${BANA_JOB:-} =~ ^[A-Za-z0-9_.-]*$ ]] || bad+=" job"
  case ${BANA_LOGS:-private} in private | public) ;; *) bad+=" logs" ;; esac
  if [[ -n $bad ]]; then
    echo "check: failed (not as bana sends them:$bad)"
    return 1
  fi
  echo "check: ok"
}

# The commit, with the deploy key: on disk only while git fetches it, and gone before any
# of the project's code runs. Plain git and ssh with GitHub's pinned host key; nothing
# they print reaches the log. BANA_TEST_SOURCE: another URL (bana's tests).
split_runner_fetch() { # DIR
  local d=$1 rc=0
  rm -rf "$d/src" "$d/key"
  mkdir -p "$d/src"
  if [[ -z ${BANA_SOURCE:-} || -z ${BANA_SOURCE_KEY:-} ]]; then
    echo "fetch: failed (no deploy key here: bana split check)"
    return 1
  fi
  (umask 077 && printf '%s\n' "$BANA_SOURCE_KEY" >"$d/key")
  printf '%s\n' "$split_host_key" >"$d/known_hosts"
  (
    export GIT_TERMINAL_PROMPT=0
    export GIT_SSH_COMMAND="ssh -i $d/key -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$d/known_hosts"
    cd "$d/src" &&
      git init -q . &&
      git fetch -q --depth 1 "${BANA_TEST_SOURCE:-git@github.com:$BANA_SOURCE.git}" "$BANA_SHA" &&
      git -c advice.detachedHead=false checkout -q FETCH_HEAD
  ) >/dev/null 2>&1 || rc=1
  rm -f "$d/key"
  if ((rc)); then
    echo "fetch: failed"
    return 1
  fi
  echo "fetch: ok"
}

# act, pinned, on the commit (images pulled when missing): no secrets, no GITHUB_TOKEN, no
# Docker socket in the job containers, an environment of three variables. Its output goes to
# out/act.jsonl, and only its steps reach the log as they end. BANA_TEST_ACT: an act already
# here (tests).
split_runner_build() { # DIR
  local d=$1 act=${BANA_TEST_ACT:-} rc=0 pid args=() envs=() tier=''
  mkdir -p "$d/out" "$d/art" "$d/bin"
  echo 1 >"$d/out/rc"
  if [[ ! -d $d/src/.git ]]; then
    echo "build: failed (nothing was fetched)"
    return 1
  fi
  if [[ -z $act ]]; then
    act=$d/bin/act
    if ! curl -fsSL --retry 3 -o "$d/act.tar.gz" \
      "https://github.com/nektos/act/releases/download/v$split_act_version/act_Linux_x86_64.tar.gz" >/dev/null 2>&1 ||
      [[ $(split_sha256 "$d/act.tar.gz") != "$split_act_sha256" ]] || ! tar -xzf "$d/act.tar.gz" -C "$d/bin" act; then
      echo "build: failed (act $split_act_version did not download, or is not the one pinned)"
      return 1
    fi
  fi
  [[ -z ${BANA_TIER:-} ]] || tier=",\"inputs\":{\"$split_tier_input\":\"$BANA_TIER\"}"
  printf '{"repository":{"full_name":"private/source"},"ref":"%s","after":"%s"%s}\n' \
    "${BANA_REF:-}" "$BANA_SHA" "$tier" >"$d/event.json"
  args=(workflow_dispatch --json --rm --pull=false --container-daemon-socket - -C "$d/src"
    -W "$d/src/.github/workflows/$split_workflow" -e "$d/event.json" --artifact-server-path "$d/art"
    --artifact-server-port $((20000 + RANDOM % 20000)) --container-architecture linux/amd64
    --env "GITHUB_RUN_ID=${BANA_ID##*-}" --env "GITHUB_RUN_NUMBER=${BANA_ID##*-}"
    "${split_platforms[@]}")
  [[ -z ${BANA_JOB:-} ]] || args+=(-j "$BANA_JOB")
  echo "build: act $split_act_version, the workflow $split_workflow${BANA_TIER:+ at $BANA_TIER}${BANA_JOB:+, job $BANA_JOB}"
  envs=(PATH="$PATH" HOME="$HOME" RUNNER_TEMP="$RUNNER_TEMP")
  [[ -z ${DOCKER_HOST:-} ]] || envs+=(DOCKER_HOST="$DOCKER_HOST")
  env -i "${envs[@]}" "$act" "${args[@]}" >"$d/out/act.jsonl" 2>"$d/out/act.err" </dev/null &
  pid=$!
  split_runner_follow "$d/out/act.jsonl" "$pid"
  wait "$pid" || rc=$?
  echo "$rc" >"$d/out/rc"
  if ((rc)); then echo "build: failed (act exited with $rc)"; else echo "build: ok"; fi
  return "$rc"
}

# act's lines, as they come, through split_runner_steps, until act ends. Workflow commands
# (::error:: and the like) are off meanwhile: no line of the build makes an annotation.
split_runner_follow() { # FILE PID
  local tok n=0 all alive=1
  tok=bana-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')
  echo "::stop-commands::$tok"
  while ((alive)); do
    kill -0 "$2" 2>/dev/null || alive=0
    all=$(wc -l <"$1" | tr -d ' ')
    if ((all > n)); then
      sed -n "$((n + 1)),${all}p" "$1" | split_runner_steps
      n=$all
    fi
    ((!alive)) || sleep 2
  done
  echo "::$tok::"
}

# act's JSON lines in, a line a finished step or job out: `job / step 0: ok (12s)`, by act's
# ids alone. Never a step's name (act names an unnamed step after its script, and expands the
# expressions in a name: an output, a path, a matrix value), its output or act's words. With
# BANA_LOGS=public, the steps' output as well; BANA_LOGS=here (on bana's machine): their names.
split_runner_steps() {
  # shellcheck disable=SC2016 # jq's
  jq -rR --unbuffered --arg logs "${BANA_LOGS:-private}" '
    def word: if . == "success" then "ok" elif . == "failure" then "failed" else . end;
    def safe: tostring | if test("^[A-Za-z0-9_.-]{1,64}$") then . else "?" end;
    def step: if $logs == "here" then (.step // "?" | tostring)
      else ((.stepID // .stepid // ["?"])[0] | safe) | if startswith("--") then .[2:] | gsub("-"; " ") else "step " + . end end;
    fromjson? // empty | select(type == "object")
    | ((.jobID // "?") | safe) as $j
    | if .stepResult != null then
        "\($j) / \(if (.stage // "Main") == "Main" then "" else "\(.stage | safe) " end)\(step): \(.stepResult | word) (\(((.executionTime // 0) / 1000000000) | floor)s)"
      elif .jobResult != null then "\($j): \(.jobResult | word)"
      elif $logs == "public" and .raw_output == true then (.msg // "" | tostring | sub("\n$"; ""))
      else empty end'
}

# The job summary: a table of the steps, their results and times, and the totals.
split_runner_summary() { # DIR
  local f=$1/out/act.jsonl
  [[ -n ${GITHUB_STEP_SUMMARY:-} ]] || return 0
  {
    echo "### bana ${BANA_ID:-}"
    echo
    if [[ ! -s $f ]]; then
      echo "Nothing was built."
    else
      echo "| Step | Result | Time |"
      echo "|---|---|---|"
      BANA_LOGS=private split_runner_steps <"$f" | sed -n 's/^\(.*\): \([a-z]*\) (\([0-9]*s\))$/| \1 | \2 | \3 |/p'
      echo
      BANA_LOGS=private split_runner_steps <"$f" | awk '/ \(([0-9]+)s\)$/ { n++; if ($0 ~ /: ok \(/) ok++; else if ($0 ~ /: failed \(/) bad++ }
        END { printf "%d steps: %d ok, %d failed. The output is with the owner.\n", n, ok, bad }'
    fi
  } >>"$GITHUB_STEP_SUMMARY"
  echo "summary: ok"
}

# out/ and art/ (act's output and the jobs' uploads), encrypted: AES-256-CBC with a fresh
# key, which goes in key.enc with the ciphertext's sha256 and what the run was (its run id,
# bana's build, commit and nonce), encrypted with RSA-OAEP to BANA_SEAL_PUB, whose private
# half only bana's machine has (split_unseal).
split_runner_seal() { # DIR SEALED
  local d=$1 o=$2 pass sum
  rm -rf "$o"
  mkdir -p "$o" "$d/out" "$d/art"
  if [[ -z ${BANA_SEAL_PUB:-} ]]; then
    echo "seal: failed (no BANA_SEAL_PUB here: bana split check)"
    return 1
  fi
  if [[ ! ${GITHUB_RUN_ID:-} =~ ^[0-9]+$ ]]; then
    echo "seal: failed (no run id)"
    return 1
  fi
  printf '%s\n' "$BANA_SEAL_PUB" >"$d/seal.pub.pem"
  tar -czf "$d/bundle.tgz" -C "$d" out art
  pass=$(openssl rand -hex 32)
  printf '%s\n' "$pass" | openssl enc -aes-256-cbc -pbkdf2 -iter 100000 -salt -pass stdin \
    -in "$d/bundle.tgz" -out "$o/bundle.enc"
  rm -f "$d/bundle.tgz"
  sum=$(split_sha256 "$o/bundle.enc")
  printf 'bana-seal 2\npass %s\nsha256 %s\nrun %s\nid %s\nsha %s\nnonce %s\n' "$pass" "$sum" "$GITHUB_RUN_ID" \
    "${BANA_ID:-}" "${BANA_SHA:-}" "${BANA_NONCE:-}" |
    openssl pkeyutl -encrypt -pubin -inkey "$d/seal.pub.pem" -pkeyopt rsa_padding_mode:oaep -out "$o/key.enc"
  echo "seal: ok ($(wc -c <"$o/bundle.enc" | tr -d ' ') bytes)"
}

split_sha256() { # FILE
  if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{ print $1; exit }'
}
# ---- end of the runner --------------------------------------------------------------

# The runner's part of this file, as text: what bana.yml carries.
split_runner_text() {
  awk '/^# ---- the runner: /, /^# ---- end of the runner/' "$bana_root/lib/split.sh"
}

# DIR/sealed (key.enc, bundle.enc) opened with PEM into OUT: out/ and art/. Refused, with
# nothing in OUT, unless the key opens, the ciphertext is the one sealed, it was sealed by
# the run asked for (RUN, bana's build ID, commit SHA and NONCE, when given), and the archive
# holds only plain files and directories in out/ and art/, at most BANA_SPLIT_MAX bytes (8 GiB).
split_unseal() { # SEALED OUT PEM [RUN ID SHA NONCE]
  local s=$1 o=$2 info pass sum list k v max=${BANA_SPLIT_MAX:-8589934592} n
  [[ -f $s/key.enc && -f $s/bundle.enc ]] || { echo "no sealed bundle in $s" >&2; return 1; }
  [[ ! -L $s/key.enc && ! -L $s/bundle.enc ]] || { echo "the bundle is a link" >&2; return 1; }
  info=$(openssl pkeyutl -decrypt -inkey "$3" -pkeyopt rsa_padding_mode:oaep -in "$s/key.enc" 2>/dev/null) ||
    { echo "the bundle's key does not open with $3" >&2; return 1; }
  pass=$(printf '%s\n' "$info" | sed -n 's/^pass \([0-9a-f]\{64\}\)$/\1/p')
  sum=$(printf '%s\n' "$info" | sed -n 's/^sha256 \([0-9a-f]\{64\}\)$/\1/p')
  if [[ $(printf '%s\n' "$info" | head -1) != "bana-seal 2" || -z $pass || -z $sum ]]; then
    echo "the bundle's key is not bana's (an older bana.yml's: bana split sync)" >&2
    return 1
  fi
  if (($# > 3)); then
    for k in run id sha nonce; do
      case $k in run) v=$4 ;; id) v=$5 ;; sha) v=$6 ;; nonce) v=$7 ;; esac
      [[ $(printf '%s\n' "$info" | sed -n "s/^$k //p") == "$v" ]] ||
        { echo "the bundle was sealed for another run (its $k is not $v)" >&2; return 1; }
    done
  fi
  [[ $(split_sha256 "$s/bundle.enc") == "$sum" ]] || { echo "the bundle is not the one sealed" >&2; return 1; }
  mkdir -p "$o"
  printf '%s\n' "$pass" | openssl enc -d -aes-256-cbc -pbkdf2 -iter 100000 -pass stdin \
    -in "$s/bundle.enc" -out "$o/bundle.tgz" 2>/dev/null || { rm -f "$o/bundle.tgz"; echo "the bundle does not decrypt" >&2; return 1; }
  n=$(gzip -dc <"$o/bundle.tgz" 2>/dev/null | head -c "$((max + 1))" | wc -c | tr -d ' ') || true
  if ((n > max)); then
    rm -f "$o/bundle.tgz"
    echo "the bundle holds more than $max bytes" >&2
    return 1
  fi
  if ! list=$(tar -tzf "$o/bundle.tgz" 2>/dev/null) || ! v=$(tar -tvzf "$o/bundle.tgz" 2>/dev/null); then
    rm -f "$o/bundle.tgz"
    echo "the bundle is not an archive" >&2
    return 1
  fi
  if printf '%s\n' "$list" | awk '{ p = "/" $0 "/" } !/^(out|art)(\/|$)/ || index(p, "/../") { bad = 1 } END { exit !bad }'; then
    rm -f "$o/bundle.tgz"
    echo "the bundle holds more than out/ and art/" >&2
    return 1
  fi
  # A link (symbolic or hard), a device or a fifo: refused (tar -tv's first letter).
  if printf '%s\n' "$v" | awk 'NF && !/^[-d]/ { bad = 1 } END { exit !bad }'; then
    rm -f "$o/bundle.tgz"
    echo "the bundle holds a link or a special file" >&2
    return 1
  fi
  tar --no-same-owner --no-same-permissions -xzf "$o/bundle.tgz" -C "$o" && rm -f "$o/bundle.tgz"
}

# ---- the public repository's workflow ---------------------------------------------------

# bana.yml for this project: lib/split.yml.in with the runner, its pins, and the project's
# workflow, tier input, timeout and platforms: Linux jobs in act.image, pinned by digest; macOS
# and <prefix>-systemd jobs not run (no job runs on the runner's own machine, which holds the
# deploy key in its memory: a systemd job runs on bana's machine).
split_render() {
  local wf tin t image runner l v p=()
  # shellcheck source=SCRIPTDIR/act.sh
  source "$bana_root/lib/act.sh"
  wf=$(conf workflow ci.yml)
  tin=$(conf tier_input tier)
  t=$(split_project daemon.timeout || daemon_conf daemon.timeout)
  case $t in *s) t=$(((${t%s} + 59) / 60)) ;; esac
  [[ $t =~ ^[1-9][0-9]*$ ]] || die "daemon.timeout: minutes, not '$t'"
  image=$(split_image "$(conf act.image "$split_image_tag")") || exit 1
  while IFS=$'\t' read -r l v; do
    case $v in
    linux) v=$image ;;
    mac | skip | "skip "* | -*) v= ;;
    *@sha256:*) ;;
    *) v= ;; # an image of its own, unless pinned by digest: not run
    esac
    p+=("-P" "$l=$v")
  done < <(act_platform_table)
  p+=("-P" "$prefix-systemd=")
  runner=$(
    echo "set -euo pipefail"
    printf 'split_act_version=%s\nsplit_act_sha256=%s\nsplit_host_key=%s\n' "$(split_q "$split_act_version")" \
      "$(split_q "$split_act_sha256")" "$(split_q "$split_host_key")"
    printf 'split_workflow=%s\nsplit_tier_input=%s\n' "$(split_q "$wf")" "$(split_q "$tin")"
    printf 'split_platforms=('
    for v in "${p[@]}"; do printf ' %s' "$(split_q "$v")"; done
    printf ' )\n'
    split_runner_text
    echo 'split_runner_main "$@"'
  )
  BANA_T=$t BANA_R=$runner BANA_U=$split_upload BANA_UT=$split_upload_tag awk '
    function swap(s, a, b,   i, o) { o = ""; while ((i = index(s, a)) > 0) { o = o substr(s, 1, i - 1) b; s = substr(s, i + length(a)) } return o s }
    /^ *__BANA_RUNNER__$/ {
      ind = $0; sub(/__BANA_RUNNER__$/, "", ind)
      n = split(ENVIRON["BANA_R"], r, "\n")
      for (i = 1; i <= n; i++) print (r[i] == "" ? "" : ind r[i])
      next
    }
    { $0 = swap($0, "__BANA_TIMEOUT__", ENVIRON["BANA_T"]); $0 = swap($0, "__BANA_UPLOAD_TAG__", ENVIRON["BANA_UT"])
      print swap($0, "__BANA_UPLOAD__", ENVIRON["BANA_U"]) }' "$bana_root/lib/split.yml.in"
}

# A daemon setting of this project (bana split's keys live there), else fails.
split_project() { project_setting "$1"; } # KEY

# IMAGE pinned by its digest: as it is when it has one; the default, at split_image_digest;
# another, at the digest Docker has here for it (fails, saying so, without one).
split_image() { # IMAGE
  local repo d
  case $1 in
  *@sha256:*) echo "$1"; return 0 ;;
  "$split_image_tag") echo "$1@$split_image_digest"; return 0 ;;
  esac
  repo=$1
  case ${1##*/} in *:*) repo=${1%:*} ;; esac
  d=$(docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$1" 2>/dev/null | grep -m1 "^$repo@sha256:") || d=''
  if [[ -z $d ]]; then
    echo "bana split runs images pinned by digest: $1 has none here (docker pull $1, or set it as NAME@sha256:DIGEST)" >&2
    return 1
  fi
  echo "$1@${d#*@}"
}

# Checks a bana.yml: why it is not one bana runs, a line each (nothing: it is).
split_lint() { # FILE
  awk -v q="'" '
    function bad(why) { print why; n++ }
    function ind(s) { match(s, /^ */); return RLENGTH }
    { line = $0; i = ind(line) }
    # The platforms of the runner: each image pinned by digest, none on its own machine.
    /split_platforms=\(/ {
      rest = line; pre = q "-P" q " " q
      while ((j = index(rest, pre)) > 0) {
        rest = substr(rest, j + length(pre)); v = substr(rest, 1, index(rest, q) - 1)
        l = v; sub(/=.*/, "", l); sub(/^[^=]*=/, "", v)
        if (v == "") continue
        h = v; sub(/^.*@sha256:/, "", h)
        if (v ~ /^-/) bad("line " NR ": " l " runs on the machine of the runner")
        else if (index(v, "@sha256:") == 0 || length(h) != 64 || h !~ /^[0-9a-f]+$/) bad("line " NR ": " l "=" v ": an image pinned by digest only")
      }
    }
    # A run: block: its lines, deeper than its key.
    inrun && line !~ /^ *$/ && i <= runind { inrun = 0 }
    inrun && index(line, "${{") { bad("line " NR ": ${{ }} in a run: script"); next }
    inrun { next }
    /^ *(- )?run: *[|>][-+]? *$/ { inrun = 1; runind = ind(line); if (line ~ /^ *- /) runind += 2; next }
    /^ *(- )?run: / && index(line, "${{") { bad("line " NR ": ${{ }} in a run: script") }
    /^[^ #][^:]*:/ { top = line; sub(/:.*/, "", top); ontop = (top == "on" || top == "\"on\"") }
    ontop && /^on: *[^ #]/ { bad("line " NR ": on: must be workflow_dispatch alone") }
    ontop && /^  [^ #]/ { k = line; sub(/^  /, "", k); sub(/:.*/, "", k); if (k != "workflow_dispatch") bad("line " NR ": a " k " trigger: only workflow_dispatch") }
    /^permissions: *\{\} *$/ { perms = 1 }
    /^ *(- )?uses: / {
      u = line; sub(/^ *(- )?uses: */, "", u); sub(/ *#.*$/, "", u)
      # 24 + 40: actions/upload-artifact@ and a commit (no {40}: BSD awk has no intervals).
      if (u !~ /^actions\/upload-artifact@[0-9a-f]+$/ || length(u) != 64) bad("line " NR ": uses " u ": only actions/upload-artifact, pinned to a commit")
    }
    /^ *(- )?[a-z-]*cache[a-z-]*:/ { bad("line " NR ": a cache") }
    /^ *retention-days: *1 *$/ { ret = 1 }
    /^ *retention-days:/ && !/^ *retention-days: *1 *$/ { bad("line " NR ": retention-days: 1, not more") }
    END {
      if (!perms) bad("no permissions: {} (the token would get the default scopes)")
      if (!ret) bad("no retention-days: 1 on the upload")
      exit n > 0
    }' "$1"
}

# ---- the wizard and its toggles --------------------------------------------------------

# What bana asks gh for, each its own way (tests/stand-ins/gh answers these).
split_jq_view='"\(.visibility) \(.viewerPermission) \(.isEmpty)"'
split_jq_keys='.[] | "\(.id) \(.read_only) \(.key) \(.title)"'
split_jq_files='.[] | .name + " " + .sha'
split_jq_runs='.[] | "\(.databaseId) \(.url) \(.displayTitle)"'
split_jq_run='"\(.status) \(.conclusion)", (.jobs[0].steps[]? | "\(.conclusion) \(.name)")'
split_jq_artifact='.artifacts[] | select(.name == "bana-sealed") | .id'
split_jq_ruleset='.[] | select(.name == "bana split") | .id'
split_jq_wf='.state'
split_jq_found='.[] | "\(.databaseId) \(.event) \(.headBranch) \(.headSha) \(.createdAt) \(.url) \(.displayTitle)"'

# The project's files for bana split: ~/.bana/<prefix>/split (0700): seal.pem (the seal key,
# 0600, never leaves this machine) and seal.pub.pem, deploy.pub (the deploy key's public
# half), bana.yml (the last render pushed), and state (what bana split on has done).
split_init() {
  d_need_added
  s_dir=$home/split
  s_state=$s_dir/state
  s_settings=$(d_project "$prefix")
}
# A key of the project's settings, when set and not empty.
split_get() { local v; v=$(d_setting "$1" "$s_settings") && [[ -n $v ]] && printf '%s\n' "$v"; } # KEY
# Sets KEY=VALUE pairs in the project's settings (an empty VALUE removes KEY).
split_set() { # KEY=VALUE...
  local kv keys=' ' add=''
  for kv in "$@"; do
    keys+="${kv%%=*} "
    [[ -z ${kv#*=} ]] || add+="${kv%%=*} = ${kv#*=}"$'\n'
  done
  {
    awk -v drop="$keys" '{ i = index($0, "="); k = substr($0, 1, i - 1); gsub(/[ \t]/, "", k) }
      i && index(drop, " " k " ") { next } { print }' "$s_settings"
    printf '%s' "$add"
  } | d_write_keys "$s_settings"
}
# The daemon reads the settings again (it starts the project again).
split_rescan() { if d_up; then d_rescan >/dev/null || warn "The daemon did not answer: it reads this when it starts"; fi; }
# state's lines: KEY VALUE (pub, how, logs, key, workflow, done STEP), the last of a key winning.
split_state() { [[ -f $s_state ]] && awk -v k="$1" '$1 == k { v = substr($0, length(k) + 2) } END { if (v == "") exit 1; print v }' "$s_state"; } # KEY
split_state_add() { mkdir -p "$s_dir" && chmod 700 "$s_dir" && printf '%s %s\n' "$1" "$2" >>"$s_state"; } # KEY VALUE
split_did() { [[ -f $s_state ]] && grep -qx "done $1" "$s_state"; } # STEP

split_tty() { [[ -t 0 ]]; }
split_ask() { local a; read -r -p "$1" a || a=''; printf '%s\n' "${a:-$2}"; } # PROMPT DEFAULT
split_valid() { # OWNER/NAME
  [[ $1 =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "The public repository: owner/name, not '$1'"
  [[ $(lower <<<"$1") != "$(lower <<<"$repo")" ]] || die "The public repository must be another than $repo"
}
split_default() { echo "${repo%%/*}/${repo##*/}-releases"; }
split_description() { echo "CI runs and releases, built by bana. The source is private."; }
# The README's marker: a random id kept here ($home/split.mark, which bana split off keeps), so
# bana knows its own public repository again and the README names nothing. Fails with none.
split_mark() { local m; m=$(cat "$home/split.mark" 2>/dev/null) && [[ -n $m ]] && printf '<!-- bana split: %s -->' "$m"; }
split_mark_new() { [[ -s $home/split.mark ]] || { mkdir -p "$home" && od -An -N16 -tx1 /dev/urandom | tr -d ' \n' >"$home/split.mark"; }; }
# Warnings when the public names say the private one: PUB's name, or the prefix (in every run's
# name and labels on PUB).
split_names() { # PUB
  local n p
  n=$(lower <<<"${repo##*/}") p=$(lower <<<"${1##*/}")
  [[ $p != *"$n"* ]] || warn "$1 names $repo's name in public: another name (bana split on --repo OWNER/NAME) keeps it out"
  [[ ${#prefix} -lt 3 || ( $n != *"$(lower <<<"$prefix")"* && $(lower <<<"$prefix") != *"$n"* ) ]] ||
    warn "The prefix $prefix, in every run's name and labels on $1, is public and says $repo's name: a neutral bana.conf prefix keeps it out"
}
split_url() { # TEXT, percent-encoded but for letters, digits and -._~
  printf '%s' "$1" | od -An -v -tx1 | tr ' ' '\n' | awk 'NF {
    n = (index("0123456789abcdef", substr($1, 1, 1)) - 1) * 16 + index("0123456789abcdef", substr($1, 2, 1)) - 1
    if ((n >= 48 && n <= 57) || (n >= 65 && n <= 90) || (n >= 97 && n <= 122) || n == 45 || n == 46 || n == 95 || n == 126) printf "%c", n
    else printf "%%%s", toupper($1) }'
}
# VISIBILITY PERMISSION EMPTY of a repository (fails when gh cannot see it).
split_view() { gh repo view "$1" --json visibility,viewerPermission,isEmpty --jq "$split_jq_view" 2>/dev/null; } # OWNER/NAME
# Why PUB cannot be the public repository (VIEW: split_view's); nothing when it can: public,
# and empty, or a README alone (GitHub's page may add one), or bana's own (its marker).
split_fresh() { # PUB VIEW
  local files m
  case $2 in PUBLIC\ *) ;; *) echo "$1 is not public (${2%% *})"; return 1 ;; esac
  [[ $2 != *" true" ]] || return 0
  files=$(gh api "repos/$1/contents" --jq '.[].path' 2>/dev/null | LC_ALL=C sort | tr '\n' ' ')
  case $files in
  "README.md ") return 0 ;;
  ".github README.md ") m=$(split_mark) && gh api "repos/$1/contents/README.md" -H "Accept: application/vnd.github.raw" 2>/dev/null | grep -qF "$m" && return 0 ;;
  esac
  echo "$1 holds more than a README (${files% }): bana split takes an empty public repository"
  return 1
}

# The public repository: named (--repo), else asked for (a terminal), and, when it is not
# there yet, how it is made: gh (here), or GitHub's page (--web, or the answer 2), which
# bana opens filled in and waits for at its step, after the typed yes and the deploy key.
# Sets s_pub and s_how (gh, web, or exists). PLAN: say only.
split_choose() { # NAME WEB PLAN
  local pub=$1 how=gh v a why
  if [[ -z $pub ]]; then
    split_tty || die "Which public repository? bana split ${3:+plan}${3:-on} --repo OWNER/NAME (gh makes it when it is not there)"
    pub=$(split_ask "The public repository [$(split_default)]: " "$(split_default)")
  fi
  split_valid "$pub"
  s_pub=$pub
  if v=$(split_view "$pub"); then
    why=$(split_fresh "$pub" "$v") || die "$why"
    s_how=exists
    return 0
  fi
  if [[ -n $2 ]]; then
    how=web
  elif split_tty && [[ -z $3 ]]; then
    echo "$pub does not exist yet. Make it:"
    echo "  1. here, with gh (gh repo create $pub --public)"
    echo "  2. on GitHub's new-repository page, filled in for you; then press Enter here"
    a=$(split_ask "Which? [1] " 1)
    case $a in 1) ;; 2) how=web ;; *) die "1 or 2, not '$a': nothing changed" ;; esac
  fi
  s_how=$how
  [[ $how == web && -z $3 ]] || return 0
  split_tty || die "--web needs a terminal: you make the repository on GitHub's page, then press Enter here"
}

# GitHub's new-repository page for PUB, filled in, then a wait until PUB is there, public
# and empty. q stops (bana split on goes on from there).
split_web() { # PUB
  local url a v why
  url="https://github.com/new?owner=$(split_url "${1%%/*}")&name=$(split_url "${1#*/}")&visibility=public&description=$(split_url "$(split_description)")"
  say "Make $1 on GitHub's page: public, with nothing in it (no README, license or .gitignore):"
  echo "  $url"
  if [[ $os == Darwin ]]; then open "$url" 2>/dev/null || true
  elif command -v xdg-open >/dev/null && [[ -n ${DISPLAY:-}${WAYLAND_DISPLAY:-} ]]; then xdg-open "$url" >/dev/null 2>&1 || true; fi
  while :; do
    read -r -p "Press Enter once it is made (q: stop) " a || a=q
    [[ $a != [qQ]* ]] || die "Stopped: bana split on goes on from here; bana split off undoes it"
    if ! v=$(split_view "$1"); then
      warn "$1 is not there yet (gh repo view $1)"
    elif why=$(split_fresh "$1" "$v"); then
      say "$1 is there, public and empty"
      return 0
    else
      warn "$why"
    fi
  done
}

# What must hold before anything changes, all said at once: problems stop bana split on.
# The workflow's jobs that cannot run on GitHub's Linux machine, or need secrets, warn.
split_preflight() { # PUB HOW
  local pub=$1 v root wf probs=() line
  command -v openssl >/dev/null || probs+=("openssl is needed: it seals each run's output")
  command -v ssh-keygen >/dev/null || probs+=("ssh-keygen is needed: it makes the deploy key")
  if v=$(gh auth status 2>&1); then
    if grep -q 'Token scopes:' <<<"$v"; then
      grep -q "'repo'" <<<"$v" || probs+=("gh's token lacks the repo scope: gh auth refresh -s repo")
      grep -q "'workflow'" <<<"$v" || probs+=("gh's token lacks the workflow scope (bana.yml is a workflow): gh auth refresh -s workflow")
    fi
  else
    probs+=("The GitHub CLI is not signed in: gh auth login")
  fi
  v=$(split_view "$repo") || v=''
  case $v in
  "PRIVATE ADMIN "*) ;;
  '') probs+=("gh cannot read $repo") ;;
  PRIVATE\ *) probs+=("you are not an admin of $repo: adding a deploy key needs one") ;;
  *) probs+=("$repo is not private (${v%% *}): bana split keeps a private repository's code private") ;;
  esac
  root=$(split_get checkout) || root=$(git rev-parse --show-toplevel 2>/dev/null) || root=''
  if [[ -n $root ]]; then
    git -C "$root" remote -v 2>/dev/null | grep -Eiq "github\.com[:/]$pub(\.git)?([[:space:]]|$)" &&
      probs+=("a remote of $root points at $pub: one push would publish $repo's history")
    if [[ $2 == exists ]]; then
      while IFS= read -r line; do
        [[ -n $line ]] || continue
        ! git -C "$root" cat-file -e "$line^{commit}" 2>/dev/null || { probs+=("$pub has commits of $repo: one with no shared history only"); break; }
      done < <(gh api "repos/$pub/commits" --jq '.[].sha' 2>/dev/null || true)
    fi
    wf=$root/.github/workflows/$(conf workflow ci.yml)
    if [[ -f $wf ]]; then
      while IFS= read -r line; do
        [[ -n $line ]] && warn "  ${wf##*/}:${line%%:*}: macOS and Windows jobs do not run on $pub (act runs Linux jobs only): bana split ci local builds them here"
      done < <(grep -nE '^[^#]*(runs-on|os):.*(macos|windows|osx|'"$prefix"'-macos)' "$wf" || true)
      line=$(grep -nE '\$\{\{ *secrets\.[A-Za-z_]' "$wf" | grep -v 'secrets\.GITHUB_TOKEN' | head -1 | cut -d: -f1) || line=''
      [[ -z $line ]] || warn "  ${wf##*/}:$line: jobs that read secrets get none on $pub"
    fi
    [[ ! -f $root/.gitmodules ]] || warn "  .gitmodules: the deploy key reads $repo alone; private submodules are not fetched on $pub"
  fi
  ((${#probs[@]})) || return 0
  printf '%s\n' "bana split cannot go on:" >&2
  printf '  %s\n' "${probs[@]}" >&2
  echo "Nothing changed." >&2
  return 1
}

# The plan: what bana split on does, in order, each with what does it.
split_plan_print() { # PUB HOW RELEASES-ONLY
  local made
  case $2 in
  gh) made="gh repo create $1 --public --disable-wiki --disable-issues" ;;
  web) made="on GitHub's new-repository page, which bana opens filled in, then waits" ;;
  *) made="there already: kept as it is" ;;
  esac
  say "The plan (bana split off deletes 2, 6, 7 and the environment, disables 8 and clears 9; the"
  echo "  repository, its README, bana.yml, its Actions settings and ruleset stay, yours to archive or delete):"
  printf '  %s\n' \
    "1. a seal key here, in $(d_tilde "$s_dir"): each run's output is encrypted to it" \
    "2. a read-only deploy key on $repo, first (an organization that forbids them stops it here):" \
    "     gh api -X POST repos/$repo/keys -F read_only=true" \
    "3. $1, public: $made" \
    "4. its README.md: what it is, and a random marker (gh api -X PUT repos/$1/contents/README.md)" \
    "5. its Actions: a read-only token, upload-artifact as the only action, the environment" \
    "     bana-source (its default branch only), a ruleset that keeps that branch" \
    "6. bana-source's secrets: the deploy key and $repo's name (gh secret set, on stdin)" \
    "7. the variable BANA_SEAL_PUB: the seal key's public half (gh variable set)" \
    "8. .github/workflows/bana.yml: bana split render (gh api -X PUT)" \
    "9. this project's settings: split.repo, split.ci = $([[ -n $3 ]] && echo local || echo github), split.logs, release.repo = $1"
  echo "  Where GitHub's API allows: fork pull requests' runs wait for approval, logs and artifacts are kept a day."
}

# The risks, as docs/SPLIT.md has them: <private> and <public> are the two repositories.
split_risk() {
  cat <<'RISK'
bana will build <private> on GitHub's machines from the PUBLIC repository <public>. The
workflow there is bana's own. With logs set to private it prints only job ids, step
numbers, ok or failed, and durations: no step's name, which may hold its script. Your
full output is encrypted to a key on this machine, then written to <private> (as a
commit comment) and to bana's page. Even so:
- Anyone can see that <public> exists, its workflow file, each run's inputs (commit sha,
  branch or tag name, tier, job), job ids, step numbers, timings, results and the
  encrypted bundle's size.
- Names are public: <public>'s, which you choose, and the prefix <prefix>, in each run's
  name and labels (unless bana.conf sets a neutral one). Neither need say <private>.
- Anything printed before bana's redirect (a runner, Docker or download failure), or
  written to the runner's log some other way, is public, and copies cannot be taken back.
- The deploy key in <public> reads ALL of <private>: every branch and its history. Anyone
  with write access to <public>, or anyone who takes over your GitHub account or gh token,
  can use it to copy your code. Deleting the key stops future reads, not past copies.
- Your build's own code and dependencies run on GitHub's machines with network access,
  in act's containers. The key is off disk by then, but GitHub's runner keeps it in its
  memory: code that escapes its container could read it, and a malicious dependency
  could send your code anywhere.
- Jobs that need secrets get none. macOS, Windows and <prefix>-systemd jobs do not run
  there: systemd jobs are for this machine.
- Release notes, release files and the installer are public on purpose. Binaries can be
  reverse-engineered.
- If you turn logs to public, compiler errors, test output, paths and source lines become
  world-readable.
- GitHub says standard GitHub-hosted runners are free in public repositories, and bana
  uses ubuntu-latest only. Larger runners are billed, and job and concurrency limits
  apply. GitHub's terms limit Actions to building, testing and publishing the
  repository's project, and using <public> for <private> is your call (see
  docs.github.com, About billing for GitHub Actions).
If any of this is unacceptable, answer no and keep CI on this machine.
RISK
}
split_risk_print() { # PUB
  echo
  split_risk | awk -v priv="$repo" -v pub="$1" -v pre="$prefix" '{ while ((i = index($0, "<private>")) > 0) $0 = substr($0, 1, i - 1) priv substr($0, i + 9)
    while ((i = index($0, "<public>")) > 0) $0 = substr($0, 1, i - 1) pub substr($0, i + 8)
    while ((i = index($0, "<prefix>")) > 0) $0 = substr($0, 1, i - 1) pre substr($0, i + 8); print }'
  echo
}

# The typed yes: on a terminal, PHRASE; off one, BANA_SPLIT_CONSENT holding it.
split_consent() { # PHRASE
  local a
  if split_tty; then
    read -r -p "Type \"$1\" to go on: " a || a=''
  else
    a=${BANA_SPLIT_CONSENT:-}
    [[ -n $a ]] || warn "No terminal: BANA_SPLIT_CONSENT must hold \"$1\""
  fi
  [[ $a == "$1" ]]
}
# A y/N on a terminal; --yes (YES) off one.
split_yes() { # PROMPT YES
  local a
  [[ -z $2 ]] || return 0
  split_tty || die "No terminal: pass --yes"
  read -r -p "$1 [y/N] " a || a=''
  [[ $a == [yY]* ]]
}

split_plan() { # [--repo R] [--web] [--releases-only]
  local pub='' web='' rel='' v
  while (($#)); do
    case $1 in
    --repo) pub=${2:?--repo OWNER/NAME}; shift ;;
    --web) web=1 ;;
    --releases-only) rel=1 ;;
    -*) split_usage ;;
    *) [[ -z $pub ]] || split_usage; pub=$1 ;;
    esac
    shift
  done
  if v=$(split_get split.repo); then say "bana split is on: $v (bana split status)"; return 0; fi
  [[ -z $(split_state pub) ]] || { pub=$(split_state pub); s_pub=$pub s_how=$(split_state how || echo exists); }
  [[ -n ${s_pub:-} ]] || split_choose "$pub" "$web" plan
  split_preflight "$s_pub" "$s_how" || exit 1
  split_plan_print "$s_pub" "$s_how" "$rel"
  split_names "$s_pub"
  echo "Nothing changed: bana split on${pub:+ --repo $pub}${web:+ --web} does it, after a typed yes."
}

split_on() { # [--repo R] [--web] [--releases-only]
  local pub='' web='' rel='' a logs=private n=0 step what v
  while (($#)); do
    case $1 in
    --repo) pub=${2:?--repo OWNER/NAME}; shift ;;
    --web) web=1 ;;
    --releases-only) rel=1 ;;
    -*) split_usage ;;
    *) [[ -z $pub ]] || split_usage; pub=$1 ;;
    esac
    shift
  done
  if v=$(split_get split.repo); then say "bana split is on already: $v (bana split status)"; return 0; fi
  if v=$(split_state pub); then
    [[ -z $pub || $pub == "$v" ]] || die "bana split on was started with $v: bana split on goes on with it (bana split off undoes it)"
    s_pub=$v s_how=$(split_state how || echo exists)
    logs=$(split_state logs || echo private)
    rel=$(split_state releases || true)
    say "Going on with bana split on for $s_pub (done: $(awk '$1 == "done" { printf "%s%s", s, $2; s = ", " }' "$s_state"))"
  else
    split_choose "$pub" "$web" ''
    split_preflight "$s_pub" "$s_how" || exit 1
    split_plan_print "$s_pub" "$s_how" "$rel"
    split_names "$s_pub"
    split_risk_print "$s_pub"
    if split_tty; then
      echo "Detailed logs (bana split logs changes it later):"
      echo "  1. private: to $repo (a commit comment) and bana's page only (recommended)"
      echo "  2. public: also in $s_pub's public console"
      a=$(split_ask "Which? [1] " 1)
      case $a in
      1) ;;
      2)
        echo "Compiler errors, test output, paths and source lines will be world-readable in $s_pub's run logs."
        split_consent "yes, logs are public" || die "Not that: nothing changed"
        logs=public
        ;;
      *) die "1 or 2, not '$a': nothing changed" ;;
      esac
    fi
    split_consent "yes, build $repo in public" || die "Not that: nothing changed"
    split_state_add pub "$s_pub"
    split_state_add how "$s_how"
    split_state_add logs "$logs"
    [[ -z $rel ]] || split_state_add releases 1
  fi
  for step in seal key repo readme actions secrets variable workflow save; do
    n=$((n + 1))
    case $step in
    seal) what="the seal key" ;;
    key) what="a read-only deploy key on $repo" ;;
    repo) what="$s_pub, public" ;;
    readme) what="its README.md" ;;
    actions) what="its Actions settings, the environment bana-source, a ruleset" ;;
    secrets) what="bana-source's secrets" ;;
    variable) what="BANA_SEAL_PUB" ;;
    workflow) what=".github/workflows/bana.yml" ;;
    save) what="this project's settings" ;;
    esac
    printf '[%d/9] %s ... ' "$n" "$what"
    if split_did "$step"; then
      echo "done before"
      continue
    fi
    # GitHub's page: bana waits on a terminal for the repository, now that the typed yes and
    # the deploy key are done.
    if [[ $step == repo && $s_how == web ]] && ! split_view "$s_pub" >/dev/null; then
      echo "on GitHub's page"
      split_tty || die "--web needs a terminal: you make the repository on GitHub's page, then press Enter here. bana split on goes on from here; bana split off undoes it."
      split_web "$s_pub"
      printf '[%d/9] %s ... ' "$n" "$what"
    fi
    if "split_do_$step" "$s_pub" "$logs" "$rel" >"$s_dir.out" 2>&1; then
      rm -f "$s_dir.out"
      split_state_add "done" "$step"
      echo ok
    else
      echo failed
      sed 's/^/  /' "$s_dir.out" >&2
      rm -f "$s_dir.out"
      die "Done so far: $(awk '$1 == "done" { printf "%s%s", s, $2; s = ", " } END { if (!s) printf "nothing" }' "$s_state" 2>/dev/null). bana split on goes on from here; bana split off undoes it."
    fi
  done
  split_extras "$s_pub"
  echo
  split_check || warn "bana split check found the above: bana split on is done, but look at it"
  echo
  say "bana split is on: $repo's pushes build on https://github.com/$s_pub/actions$([[ -n $rel ]] && echo " (not yet: split.ci = local; bana split ci github)")."
  echo "  Each run's details: commit comments on $repo, bana's page, bana report. Releases publish on $s_pub."
  echo "  bana split ci local: build here again. bana split off: undo."
  echo "  On GitHub by hand, if you want them: $s_pub's Settings → General → Pull requests off;"
  echo "  and stop $repo's own workflow on pushes (Actions → $(conf workflow ci.yml) → Disable workflow), so pushes use no private minutes."
}

# ---- bana split on's steps: each does what it says, or nothing it cannot undo, and fails ----

split_do_seal() {
  mkdir -p "$s_dir" && chmod 700 "$s_dir"
  (umask 077 && openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out "$s_dir/seal.pem.new" 2>/dev/null) &&
    openssl pkey -in "$s_dir/seal.pem.new" -pubout -out "$s_dir/seal.pub.pem" 2>/dev/null &&
    mv -f "$s_dir/seal.pem.new" "$s_dir/seal.pem"
}

# A fresh ed25519 key, added to the private repository read-only, its private half kept in
# split/ (0600) only until the secrets step sets it on the public repository. KEEP: the key
# in use (bana split rekey), which stays until the new one works.
split_do_key() { # PUB [KEEP]
  local f=$s_dir/deploy.new id ro kt kb title k='' out
  rm -f "$f" "$f.pub"
  (umask 077 && ssh-keygen -q -t ed25519 -N '' -C "bana split $1" -f "$f" >/dev/null) || return 1
  # A key an earlier, stopped bana split on added: its private half is gone with it.
  while read -r id ro kt kb title; do
    [[ $title == "bana split: $1" && $id != "${2:-}" ]] && gh api -X DELETE "repos/$repo/keys/$id" >/dev/null
  done < <(gh api "repos/$repo/keys" --jq "$split_jq_keys")
  if ! out=$(gh api -X POST "repos/$repo/keys" -f "title=bana split: $1" -f "key=$(cat "$f.pub")" -F read_only=true 2>&1); then
    rm -f "$f" "$f.pub"
    echo "GitHub took no deploy key on $repo: $(tail -1 <<<"$out")"
    echo "An organization's policy may forbid deploy keys, or you are not $repo's admin. bana split needs one (no token stands in): nothing public was made."
    return 1
  fi
  while read -r id ro kt kb title; do
    [[ "$kt $kb" == "$(cut -d' ' -f1-2 "$f.pub")" ]] && k=$id && break
  done < <(gh api "repos/$repo/keys" --jq "$split_jq_keys")
  [[ -n $k ]] || { rm -f "$f" "$f.pub"; echo "GitHub does not list the deploy key it took"; return 1; }
  if [[ $ro != true ]]; then
    gh api -X DELETE "repos/$repo/keys/$k" >/dev/null
    rm -f "$f" "$f.pub"
    echo "GitHub made the deploy key writable: it is deleted"
    return 1
  fi
  mv -f "$f.pub" "$s_dir/deploy.pub" && mv -f "$f" "$s_dir/deploy" && split_state_add key "$k"
}

split_do_repo() { # PUB
  local v why
  if v=$(split_view "$1"); then
    why=$(split_fresh "$1" "$v") || { echo "$why"; return 1; }
    return 0
  fi
  gh repo create "$1" --public --disable-wiki --disable-issues --description "$(split_description)" >/dev/null
}

split_readme() { # PUB
  cat <<EOF
# ${1#*/}

CI runs and releases of a private project, built by [bana](https://github.com/tjrb-xyz/bana).
The source stays private: this repository holds this README and bana's workflow
(.github/workflows/bana.yml), which bana renders and checks before every run.

A run prints its jobs and steps by their ids, ok or failed, and their times; its output is
encrypted to the project's owner. The releases are here.

$(split_mark)
EOF
}
# FILE as the content of PUB's PATH, with a commit (none when it is that already).
split_put() { # PUB PATH FILE MESSAGE
  local old
  old=$(gh api "repos/$1/contents/$2" --jq .sha 2>/dev/null) || old=''
  [[ $old != "$(git hash-object "$3")" ]] || return 0
  gh api -X PUT "repos/$1/contents/$2" -f "message=$4" -f "content=$(base64 <"$3" | tr -d '\n')" ${old:+-f "sha=$old"} >/dev/null
}
split_do_readme() { # PUB
  split_mark_new && split_readme "$1" >"$s_dir/README.md" || return 1
  split_put "$1" README.md "$s_dir/README.md" "bana split: README" && rm -f "$s_dir/README.md"
}

split_do_actions() { # PUB
  local b id
  b=$(gh api "repos/$1" --jq .default_branch) || return 1
  gh api -X PUT "repos/$1/actions/permissions" -F enabled=true -f allowed_actions=selected >/dev/null &&
    gh api -X PUT "repos/$1/actions/permissions/selected-actions" -F github_owned_allowed=false -F verified_allowed=false \
      -f 'patterns_allowed[]=actions/upload-artifact@*' >/dev/null &&
    gh api -X PUT "repos/$1/actions/permissions/workflow" -f default_workflow_permissions=read \
      -F can_approve_pull_request_reviews=false >/dev/null &&
    gh api -X PUT "repos/$1/environments/bana-source" -F 'deployment_branch_policy[protected_branches]=false' \
      -F 'deployment_branch_policy[custom_branch_policies]=true' >/dev/null || return 1
  if ! gh api "repos/$1/environments/bana-source/deployment-branch-policies" --jq '.branch_policies[].name' | grep -qx "$b"; then
    gh api -X POST "repos/$1/environments/bana-source/deployment-branch-policies" -f "name=$b" >/dev/null || return 1
  fi
  printf '%s\n' '{"name": "bana split", "target": "branch", "enforcement": "active",' \
    '"conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},' \
    '"rules": [{"type": "deletion"}, {"type": "non_fast_forward"}]}' >"$s_dir/ruleset.json"
  # bana's ruleset, made once (a run again updates it).
  if id=$(gh api "repos/$1/rulesets" --jq "$split_jq_ruleset") && id=$(head -1 <<<"$id") &&
    if [[ $id =~ ^[0-9]+$ ]]; then gh api -X PUT "repos/$1/rulesets/$id" --input "$s_dir/ruleset.json" >/dev/null
    else gh api -X POST "repos/$1/rulesets" --input "$s_dir/ruleset.json" >/dev/null; fi; then
    rm -f "$s_dir/ruleset.json"
  else
    rm -f "$s_dir/ruleset.json"
    return 1
  fi
}

split_do_secrets() { # PUB
  local k
  if [[ ! -f $s_dir/deploy ]]; then
    # Its private half is gone (bana split on stopped in between): a new key, next time.
    k=$(split_state key) && gh api -X DELETE "repos/$repo/keys/$k" >/dev/null 2>&1
    grep -vx 'done key' "$s_state" >"$s_state.new" && mv -f "$s_state.new" "$s_state"
    echo "The deploy key's private half is not here any more: bana split on makes a new one"
    return 1
  fi
  printf '%s' "$repo" | gh secret set BANA_SOURCE -R "$1" --env bana-source >/dev/null &&
    gh secret set BANA_SOURCE_KEY -R "$1" --env bana-source <"$s_dir/deploy" >/dev/null &&
    rm -f "$s_dir/deploy"
}

split_do_variable() { gh variable set BANA_SEAL_PUB -R "$1" <"$s_dir/seal.pub.pem" >/dev/null; } # PUB

split_do_workflow() { # PUB
  split_render >"$s_dir/bana.yml.new" || return 1
  split_lint "$s_dir/bana.yml.new" || return 1
  split_put "$1" .github/workflows/bana.yml "$s_dir/bana.yml.new" "bana split: the workflow, rendered by bana $BANA_VERSION" || return 1
  split_enable "$1" || return 1
  split_state_add workflow "$(git hash-object "$s_dir/bana.yml.new")"
  mv -f "$s_dir/bana.yml.new" "$s_dir/bana.yml"
}

# bana.yml enabled on PUB (bana split off disables it). A workflow GitHub does not list yet is
# a new one, and enabled.
split_wf_state() { gh api "repos/$1/actions/workflows/bana.yml" --jq "$split_jq_wf" 2>/dev/null; } # PUB
split_enable() { # PUB
  case $(split_wf_state "$1") in
  disabled*) gh workflow enable bana.yml -R "$1" >/dev/null ;;
  esac
}

split_do_save() { # PUB LOGS RELEASES-ONLY
  split_set "split.repo=$1" "split.ci=$([[ -n $3 ]] && echo local || echo github)" "split.logs=$2" \
    "split.workflow=$(split_state workflow)" "split.key=$(split_state key)" "release.repo=$1"
  split_rescan
}

# What GitHub's API may not have yet (docs/SPLIT.md: LIVE-CHECK): a warning with the place
# in Settings when it does not.
split_extras() { # PUB
  gh api -X PUT "repos/$1/actions/permissions/fork-pr-contributor-approval" -f approval_policy=all_external_contributors >/dev/null 2>&1 ||
    warn "  By hand: $1's Settings → Actions → General → approval for all outside collaborators"
  gh api -X PUT "repos/$1/actions/permissions/artifact-and-log-retention" -F days=1 >/dev/null 2>&1 ||
    warn "  By hand: $1's Settings → Actions → General → artifact and log retention: 1 day"
}

# ---- check, status and the toggles -----------------------------------------------------------

split_ok() { printf '  ok    %s\n' "$*"; }
split_warn() { printf '  WARN  %s\n' "$*"; s_warns=$((s_warns + 1)); }
split_fail() { printf '  FAIL  %s\n' "$*"; s_fails=$((s_fails + 1)); }

# The things every remote build needs as bana left them: PUB's one workflow, bana.yml at blob
# WORKFLOW (in commit AT, else on its default branch), enabled, and PRIV's deploy key KEY,
# read-only and the one in DIR/deploy.pub.
split_quick() { # PUB PRIV WORKFLOW KEY DIR [AT]
  local v line found=''
  v=$(gh api "repos/$1/contents/.github/workflows${6:+?ref=$6}" --jq "$split_jq_files" 2>/dev/null) || v='(none)'
  if [[ -n $3 && $v == "bana.yml $3" ]]; then split_ok "$1's one workflow is bana.yml, as bana pushed it"
  else split_fail "$1's workflows are not bana's (changed outside bana: bana split sync puts bana's back): $(tr '\n' ' ' <<<"$v")"; fi
  case $(split_wf_state "$1") in
  active) split_ok "bana.yml is enabled on $1" ;;
  disabled*) split_fail "bana.yml is disabled on $1: bana split sync enables it" ;;
  *) split_warn "GitHub does not list bana.yml as a workflow of $1 (yet)" ;;
  esac
  while read -r line; do
    [[ ${line%% *} == "$4" ]] && found=$line
  done < <(gh api "repos/$2/keys" --jq "$split_jq_keys" 2>/dev/null || true)
  if [[ -z $4 || -z $found ]]; then split_fail "$2 has no deploy key ${4:-(none set)}: bana split rekey"
  elif [[ $(cut -d' ' -f2 <<<"$found") != true ]]; then split_fail "$2's deploy key $4 can write: bana split rekey"
  elif [[ -f $5/deploy.pub && $(cut -d' ' -f3-4 <<<"$found") != "$(cut -d' ' -f1-2 "$5/deploy.pub")" ]]; then
    split_fail "$2's deploy key $4 is not the one bana made: bana split rekey"
  else split_ok "$2's deploy key $4 is read-only"; fi
}

# The public side as bana left it. --quick: split_quick alone (bana add's doctor). Read-only;
# exit 1 on a FAIL.
split_check() { # [--quick]
  local pub key wf v line root b
  s_fails=0 s_warns=0
  pub=$(split_get split.repo) || { echo "bana split is off for $prefix (bana split on)"; return 1; }
  wf=$(split_get split.workflow) || wf=''
  key=$(split_get split.key) || key=''
  split_quick "$pub" "$repo" "$wf" "$key" "$s_dir"
  if [[ ${1:-} != --quick ]]; then
    v=$(split_view "$pub") || v=''
    case $v in PUBLIC\ *) split_ok "$pub is public" ;; *) split_fail "$pub is not public, or gh cannot see it (${v:-gh repo view $pub})" ;; esac
    v=$(split_render 2>/dev/null | git hash-object --stdin)
    if [[ $v == "$wf" ]]; then split_ok "bana.yml is this bana's render"
    else split_warn "bana.yml is another bana's render: bana split sync"; fi
    v=$(gh secret list -R "$pub" --env bana-source --json name --jq '.[].name' 2>/dev/null | LC_ALL=C sort | tr '\n' ' ') || v=''
    if [[ $v == "BANA_SOURCE BANA_SOURCE_KEY " ]]; then split_ok "bana-source's secrets: BANA_SOURCE and BANA_SOURCE_KEY alone"
    else split_fail "bana-source's secrets should be BANA_SOURCE and BANA_SOURCE_KEY alone: ${v:-none}"; fi
    v=$(gh secret list -R "$pub" --json name --jq '.[].name' 2>/dev/null | tr '\n' ' ') || v=''
    if [[ -z $v ]]; then split_ok "no repository secrets"
    else split_fail "$pub has repository secrets (any workflow there could read them): $v"; fi
    v=$(gh variable list -R "$pub" --json name --jq '.[].name' 2>/dev/null | tr '\n' ' ') || v=''
    case " $v" in *" ACTIONS_STEP_DEBUG "* | *" ACTIONS_RUNNER_DEBUG "*) split_fail "$pub has a debug variable (runs would log more): $v" ;; esac
    if [[ $v == "BANA_SEAL_PUB " ]]; then split_ok "the variable BANA_SEAL_PUB alone"
    else split_warn "$pub's variables should be BANA_SEAL_PUB alone: ${v:-none}"; fi
    if [[ -f $s_dir/seal.pub.pem && $(gh variable get BANA_SEAL_PUB -R "$pub" 2>/dev/null) == "$(cat "$s_dir/seal.pub.pem")" ]]; then
      split_ok "BANA_SEAL_PUB is this machine's seal key"
    else split_fail "BANA_SEAL_PUB is not this machine's seal key: runs could not be opened here (bana split rekey)"; fi
    b=$(gh api "repos/$pub" --jq .default_branch 2>/dev/null) || b=main
    v=$(gh api "repos/$pub/environments/bana-source/deployment-branch-policies" --jq '.branch_policies[].name' 2>/dev/null | tr '\n' ' ') || v=''
    if [[ $v == "$b " ]]; then split_ok "bana-source: $b only"
    else split_fail "the environment bana-source should take $b only: ${v:-none}"; fi
    v=$(gh api "repos/$pub/actions/permissions" --jq .allowed_actions 2>/dev/null) || v=''
    if [[ $v == selected ]]; then split_ok "Actions: upload-artifact alone"
    else split_fail "$pub allows ${v:-?} actions, not upload-artifact alone"; fi
    v=$(gh api "repos/$pub/actions/permissions/workflow" --jq .default_workflow_permissions 2>/dev/null) || v=''
    if [[ $v == read ]]; then split_ok "the token: read only"
    else split_fail "$pub's token defaults to ${v:-?}, not read"; fi
    v=$(gh api "repos/$pub/collaborators" --jq '.[].login' 2>/dev/null | grep -vx "$(gh api user --jq .login 2>/dev/null || echo -)" | tr '\n' ' ') || v=''
    if [[ -z $v ]]; then split_ok "no other collaborator"
    else split_warn "$pub's other collaborators can each read $repo (through the deploy key): $v"; fi
    root=$(split_get checkout) || root=''
    if [[ -n $root ]] && git -C "$root" remote -v 2>/dev/null | grep -Eiq "github\.com[:/]$pub(\.git)?([[:space:]]|$)"; then
      split_fail "a remote of $root points at $pub: one push would publish $repo's history"
    else split_ok "no remote here points at $pub"; fi
    v=''
    if [[ -n $root ]]; then
      while IFS= read -r line; do
        [[ -n $line ]] && git -C "$root" cat-file -e "$line^{commit}" 2>/dev/null && v=$line && break
      done < <(gh api "repos/$pub/commits" --jq '.[].sha' 2>/dev/null || true)
    fi
    if [[ -z $v ]]; then split_ok "$pub shares no commit with $repo"
    else split_fail "$pub has $repo's commit ${v:0:7}: its history is public"; fi
  fi
  if ((s_fails)); then echo "$s_fails FAIL, $s_warns WARN"; return 1; fi
  ((s_warns == 0)) || echo "$s_warns WARN"
  return 0
}

# bana add's doctor: the quick check, as warnings.
split_doctor() {
  s_dir=$home/split s_state=$home/split/state s_settings=$(d_project "$prefix")
  local out
  if out=$(split_check --quick 2>&1); then
    echo "  bana split: $(split_get split.repo)'s workflow and $repo's deploy key are as bana left them"
  else
    warn "  bana split: $(grep FAIL <<<"$out" | sed 's/^ *FAIL *//' | head -3 | tr '\n' ' ')(bana split check)"
  fi
}

# The newest remote run of a build here: its URL.
split_last_run() {
  local f b='' a
  for f in "$home"/builds/*/remote.json; do
    [[ -f $f ]] || continue
    a=${f%/remote.json} && a=${a##*/}
    [[ $a =~ ^[0-9]+$ ]] && { [[ -z $b ]] || ((a > b)); } && b=$a
  done
  [[ -n $b ]] && sed -n 's/.*"url": *"\([^"]*\)".*/\1/p' "$home/builds/$b/remote.json" | head -1
}

split_status() {
  local pub v
  if ! pub=$(split_get split.repo); then
    echo "bana split is off for $prefix ($repo): its builds run here."
    if v=$(split_state pub); then echo "  bana split on stopped half way, with $v: bana split on goes on, bana split off undoes it"; fi
    v=$(split_get release.repo) && echo "  releases: $v"
    echo "  bana split plan: what bana split on would do"
    return 0
  fi
  echo "bana split is on for $prefix ($repo):"
  echo "  public repository: https://github.com/$pub"
  if [[ $(split_get split.ci) == github ]]; then echo "  builds: on $pub's GitHub Actions (fix rounds and bana ci: here)"
  else echo "  builds: here (bana split ci github: on $pub)"; fi
  echo "  logs: $(split_get split.logs || echo private)$([[ $(split_get split.logs) == public ]] && echo ": each run's whole output is public")"
  echo "  releases: $(split_get release.repo || echo "$repo")"
  echo "  deploy key: $(split_get split.key || echo none) on $repo"
  v=$(split_last_run) && echo "  last remote run: $v"
  split_check --quick
}

split_ci_mode() { # github|local
  case ${1:-} in
  github | local) ;;
  *) split_usage ;;
  esac
  [[ $1 == local ]] || split_get split.repo >/dev/null || die "bana split is off for $prefix: bana split on first"
  split_set "split.ci=$1"
  split_rescan
  if [[ $1 == github ]]; then say "$prefix's pushes build on $(split_get split.repo)'s GitHub Actions"
  else say "$prefix's pushes build here$(v=$(split_get release.repo) && echo "; releases still publish on $v")"; fi
}

split_logs() { # private|public [--yes]
  local to=${1:-} yes=''
  [[ ${2:-} != --yes ]] || yes=1
  local pub
  pub=$(split_get split.repo) || die "bana split is off for $prefix: bana split on first"
  case $to in
  public)
    echo "Each run's whole output goes to $pub's public run logs: compiler errors, test output,"
    echo "paths and source lines become world-readable, and copies cannot be taken back."
    split_consent "yes, logs are public" || die "Not that: the logs stay private"
    ;;
  private) split_yes "Runs on $pub print their steps only from now on?" "$yes" || die "Nothing changed" ;;
  *) split_usage ;;
  esac
  split_set "split.logs=$to"
  split_rescan
  say "$prefix's runs on $pub: logs $to from the next one."
  echo "  The runs there already keep what they printed: bana split purge-runs deletes them."
}

split_sync() { # [--yes]
  local pub wf new
  pub=$(split_get split.repo) || die "bana split is off for $prefix: bana split on first"
  wf=$(split_get split.workflow) || wf=''
  mkdir -p "$s_dir"
  split_render >"$s_dir/bana.yml.new"
  split_lint "$s_dir/bana.yml.new" || die "this bana's bana.yml does not lint"
  new=$(git hash-object "$s_dir/bana.yml.new")
  if [[ $new == "$wf" ]]; then
    rm -f "$s_dir/bana.yml.new"
    split_enable "$pub" || die "gh did not enable bana.yml on $pub"
    say "$pub's bana.yml is this bana's already"
    return 0
  fi
  diff -u "$s_dir/bana.yml" "$s_dir/bana.yml.new" 2>/dev/null | sed 1,2d | head -200 || true
  split_yes "Push this bana.yml to $pub?" "${1:-}" || { rm -f "$s_dir/bana.yml.new"; die "Nothing changed"; }
  split_put "$pub" .github/workflows/bana.yml "$s_dir/bana.yml.new" "bana split: the workflow, rendered by bana $BANA_VERSION" ||
    die "gh did not take the new bana.yml"
  split_enable "$pub" || die "gh did not enable bana.yml on $pub"
  mv -f "$s_dir/bana.yml.new" "$s_dir/bana.yml"
  split_set "split.workflow=$new"
  split_rescan
  say "$pub's bana.yml is this bana's now"
}

# A new deploy key and seal key, each working before the old one goes. A run sealed to the
# old key still opens (seal.old.pem).
split_rekey() { # [--yes]
  local pub old k=''
  pub=$(split_get split.repo) || die "bana split is off for $prefix: bana split on first"
  split_yes "A new deploy key on $repo and a new seal key for $pub?" "${1:-}" || die "Nothing changed"
  old=$(split_get split.key) || old=''
  [[ ! -f $s_dir/deploy.pub ]] || cp "$s_dir/deploy.pub" "$s_dir/deploy.old.pub"
  sed -i.bak '/^key /d; /^done key$/d' "$s_state" 2>/dev/null && rm -f "$s_state.bak"
  printf 'the deploy key ... '
  if split_do_key "$pub" "$old" && k=$(split_state key) &&
    gh secret set BANA_SOURCE_KEY -R "$pub" --env bana-source <"$s_dir/deploy" >/dev/null; then
    # The new key is the one in use from here on, whatever fails next.
    rm -f "$s_dir/deploy" "$s_dir/deploy.old.pub"
    split_set "split.key=$k"
    split_rescan
  else
    [[ -z $k ]] || gh api -X DELETE "repos/$repo/keys/$k" >/dev/null 2>&1 || warn "the new deploy key $k: delete it on GitHub ($repo's Settings → Deploy keys)"
    rm -f "$s_dir/deploy"
    [[ ! -f $s_dir/deploy.old.pub ]] || mv -f "$s_dir/deploy.old.pub" "$s_dir/deploy.pub"
    sed -i.bak '/^key /d; /^done key$/d' "$s_state" 2>/dev/null && rm -f "$s_state.bak"
    [[ -z $old ]] || { split_state_add key "$old" && split_state_add "done" key; }
    die "failed: the old key ($old) stays"
  fi
  [[ -z $old || $old == "$k" ]] || gh api -X DELETE "repos/$repo/keys/$old" >/dev/null 2>&1 || warn "the old deploy key $old: delete it on GitHub ($repo's Settings → Deploy keys)"
  echo ok
  printf 'the seal key ... '
  mv -f "$s_dir/seal.pem" "$s_dir/seal.old.pem" && mv -f "$s_dir/seal.pub.pem" "$s_dir/seal.old.pub.pem"
  if split_do_seal && split_do_variable "$pub"; then
    echo ok
  else
    mv -f "$s_dir/seal.old.pem" "$s_dir/seal.pem" && mv -f "$s_dir/seal.old.pub.pem" "$s_dir/seal.pub.pem"
    die "failed: the old seal key stays"
  fi
  rm -f "$s_dir/seal.old.pub.pem"
  split_check --quick
}

split_purge() { # [--yes]
  local pub id n=0
  pub=$(split_get split.repo || split_state pub) || die "bana split is off for $prefix"
  split_yes "Delete $pub's bana runs (their logs and step lines)?" "${1:-}" || die "Nothing changed"
  while read -r id _ _; do
    [[ $id =~ ^[0-9]+$ ]] || continue
    gh run delete "$id" -R "$pub" >/dev/null 2>&1 && n=$((n + 1))
  done < <(gh run list -R "$pub" --workflow bana.yml -L 1000 --json databaseId,url,displayTitle --jq "$split_jq_runs" 2>/dev/null | awk '$3 == "bana"')
  say "Deleted $n runs of $pub"
}

# NAME is not there: LIST-COMMAND prints names without it, or GitHub says Not Found.
split_absent() { # NAME LIST-COMMAND...
  local l
  if l=$("${@:2}" 2>&1); then ! grep -qxF -- "$1" <<<"$l"; else grep -q 'HTTP 404' <<<"$l"; fi
}
split_key_ids() { gh api "repos/$repo/keys" --jq "$split_jq_keys" | cut -d' ' -f1; }
split_envs() { gh api "repos/$1/environments" --jq '.environments[].name'; } # PUB
split_wf_names() { gh api "repos/$1/contents/.github/workflows" --jq "$split_jq_files" | cut -d' ' -f1; } # PUB

# Undo: the deploy key first (the public side reads nothing more from then on), then the
# secrets, the environment and the variable, and the workflow is disabled. The repository,
# and its releases, stay: archiving or deleting it is yours.
split_off() { # [--yes] [--purge-runs]
  local pub yes='' purge='' k keep=1 a='' v left=()
  while (($#)); do
    case $1 in --yes) yes=1 ;; --purge-runs) purge=1 ;; *) split_usage ;; esac
    shift
  done
  pub=$(split_get split.repo || split_state pub) || die "bana split is off for $prefix"
  split_yes "Turn bana split off for $prefix ($pub's deploy key, secrets and workflow go; the repository stays)?" "$yes" || die "Nothing changed"
  k=$(split_get split.key || split_state key) || k=''
  # The key first: gone when GitHub deletes it, or lists the keys without it.
  printf 'the deploy key on %s ... ' "$repo"
  if [[ -z $k ]] || gh api -X DELETE "repos/$repo/keys/$k" >/dev/null 2>&1 || split_absent "$k" split_key_ids; then
    echo gone
  else
    echo "failed"
    die "bana split off stops: $repo's deploy key $k may still be there (gh api repos/$repo/keys; delete it in $repo's Settings → Deploy keys, then bana split off again)"
  fi
  printf '%s ... ' "$pub's secrets, environment and variable"
  # The secrets, then their environment: gone with it, or each on its own.
  for v in BANA_SOURCE_KEY BANA_SOURCE; do
    gh secret delete "$v" -R "$pub" --env bana-source >/dev/null 2>&1 ||
      split_absent "$v" gh secret list -R "$pub" --env bana-source --json name --jq '.[].name' || a+=" $v"
  done
  if ! gh api -X DELETE "repos/$pub/environments/bana-source" >/dev/null 2>&1 && ! split_absent bana-source split_envs "$pub"; then
    left+=("the environment bana-source")
    for v in $a; do left+=("the secret $v"); done
  fi
  gh variable delete BANA_SEAL_PUB -R "$pub" >/dev/null 2>&1 ||
    split_absent BANA_SEAL_PUB gh variable list -R "$pub" --json name --jq '.[].name' || left+=("the variable BANA_SEAL_PUB")
  if ((${#left[@]} == 0)); then echo gone; else echo failed; fi
  printf '%s ... ' "its workflow"
  if gh workflow disable bana.yml -R "$pub" >/dev/null 2>&1; then echo disabled
  else
    case $(split_wf_state "$pub") in
    disabled*) echo disabled ;;
    *) if split_absent bana.yml split_wf_names "$pub"; then echo "not there"; else echo failed; left+=("bana.yml, enabled"); fi ;;
    esac
  fi
  if ((${#left[@]})); then
    { printf '  still on %s:' "$pub"; printf ' %s;' "${left[@]}"; echo; } >&2
    die "bana split off stops: the deploy key is gone, but not all of the above (gh failed): bana split off again; bana split's settings and files here stay until then"
  fi
  [[ -z $purge ]] || split_purge --yes
  if [[ -n $(split_get release.repo) ]] && split_tty && [[ -z $yes ]]; then
    a=$(split_ask "Keep publishing releases on $pub (installers download from there)? [Y/n] " y)
    [[ $a == [nN]* ]] && keep=''
  fi
  if [[ -n $keep ]]; then split_set split.repo= split.ci= split.logs= split.workflow= split.key=
  else split_set split.repo= split.ci= split.logs= split.workflow= split.key= release.repo=; fi
  rm -rf "$s_dir" "$s_dir.out"
  split_rescan
  say "bana split is off for $prefix: its builds run here$([[ -n $keep ]] && v=$(split_get release.repo) && echo "; releases still publish on $v")."
  echo "  $pub stays. To archive it: gh repo archive $pub"
  echo "  To delete it (and its releases): gh auth refresh -s delete_repo && gh repo delete $pub"
}

# ---- the remote build: bana ci, for the daemon, when split.ci = github --------------------------

# A build of BANA_SPLIT_SHA on BANA_SPLIT_REPO's GitHub Actions, printed as act prints it: the
# daemon's statuses, page, report and bana fix take it as a build here. The guard first
# (split_quick on the default branch's head: nothing is dispatched unless PUB is as bana left
# it); then the dispatch there, with a nonce; the run found by its name (bana BANA_BUILD NONCE),
# made since, by that dispatch, at that commit; remote.json and a line naming it; its progress
# while it runs (a SIGINT or TERM cancels it there too, found first if need be); then its
# sealed output, opened with this machine's seal key, and sealed by that run: act's lines on
# stdout, its stderr on stderr, the jobs' uploads in ARTIFACTS, and the bundle deleted from
# GitHub. The daemon writes the details to the private repository (a commit comment). Exits as
# act did. BANA_SPLIT_POLL: seconds between asks (10).
split_remote() { # TIER JOB ARTIFACTS
  local tier=$1 job=$2 art=$3 pub=${BANA_SPLIT_REPO:-} priv=${BANA_SPLIT_PRIVATE:-} id=${BANA_BUILD:-}
  local logs=${BANA_SPLIT_LOGS:-private} dir=${BANA_SPLIT_HOME:-} sha=${BANA_SPLIT_SHA:-} ref=${BANA_SPLIT_REF:-}
  local poll=${BANA_SPLIT_POLL:-10} out line='' run='' url='' st='' last='' k tmp rc=1 failed pem
  local b at nonce since sent=''
  if [[ -z $pub || -z $priv || ! $id =~ ^[a-z0-9-]+$ || ! $sha =~ ^[0-9a-f]{40}$ || ! -d $dir ]]; then
    echo "Error: bana split: the daemon gave no repository, build, commit or $dir" >&2
    return 1
  fi
  if ! b=$(gh api "repos/$pub" --jq .default_branch 2>/dev/null) || ! at=$(gh api "repos/$pub/commits/$b" --jq .sha 2>/dev/null) ||
    [[ ! $at =~ ^[0-9a-f]{40}$ ]]; then
    echo "Error: gh cannot read $pub's default branch, so nothing was dispatched (bana split check)" >&2
    return 1
  fi
  if ! out=$(
    s_fails=0 s_warns=0
    split_quick "$pub" "$priv" "${BANA_SPLIT_WORKFLOW:-}" "${BANA_SPLIT_KEY:-}" "$dir" "$at"
    ((s_fails == 0))
  ); then
    echo "Error: $pub is not as bana left it, so nothing was dispatched: $(grep FAIL <<<"$out" | head -1 | sed 's/^ *FAIL *//') (bana split check)" >&2
    return 1
  fi
  nonce=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
  since=$(split_iso "$(($(date -u +%s) - 120))")
  trap 'split_remote_cancel' INT TERM
  sent=1
  if ! out=$(gh workflow run bana.yml -R "$pub" --ref "$b" -f "id=$id" -f "sha=$sha" -f "ref=$ref" -f "tier=$tier" -f "job=$job" \
    -f "logs=$logs" -f "nonce=$nonce" 2>&1); then
    trap - INT TERM
    echo "Error: gh workflow run bana.yml -R $pub: $(tail -1 <<<"$out")" >&2
    return 1
  fi
  echo "{\"bana\":\"remote\",\"msg\":\"dispatched to $pub: waiting for its run\"}"
  for ((k = 0; k < 24; k++)); do
    line=$(split_remote_find) || line=''
    [[ -z $line ]] || break
    split_sleep "$poll"
  done
  if [[ -z $line ]]; then
    trap - INT TERM
    echo "Error: the run on $pub did not show up (gh run list -R $pub --workflow bana.yml)" >&2
    return 1
  fi
  run=${line%% *} url=${line#* }
  printf '{"repo": "%s", "run": %s, "url": "%s", "logs": "%s"}\n' "$pub" "$run" "$url" "$logs" >remote.json
  echo "{\"bana\":\"remote\",\"msg\":\"remote run in public repo $pub: $url (logs: $logs)\"}"
  while :; do
    out=$(gh run view "$run" -R "$pub" --json status,conclusion,jobs --jq "$split_jq_run" 2>/dev/null) || out=''
    st=$(head -1 <<<"$out")
    if [[ -n $st && $st != "$last" ]]; then
      echo "{\"bana\":\"remote\",\"msg\":\"$pub run $run: ${st% }\"}"
      last=$st
    fi
    case $st in completed\ *) break ;; esac
    split_sleep "$poll"
  done
  trap - INT TERM
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/bana-remote.XXXXXX")
  if gh run download "$run" -R "$pub" -n bana-sealed -D "$tmp/sealed" >/dev/null 2>&1; then
    for pem in "$dir/seal.pem" "$dir/seal.old.pem"; do
      [[ -f $pem ]] || continue
      split_unseal "$tmp/sealed" "$tmp/open" "$pem" "$run" "$id" "$sha" "$nonce" 2>"$tmp/why" && break
      rm -rf "$tmp/open"
    done
  fi
  if [[ -f $tmp/open/out/act.jsonl ]]; then
    cat "$tmp/open/out/act.jsonl"
    cat "$tmp/open/out/act.err" >&2 2>/dev/null || true
    rc=$(cat "$tmp/open/out/rc" 2>/dev/null) || rc=1
    [[ $rc =~ ^[0-9]+$ ]] || rc=1
    if [[ -n $(ls -A "$tmp/open/art" 2>/dev/null) ]]; then
      mkdir -p "$art" && cp -R "$tmp/open/art/." "$art/"
    fi
    k=$(gh api "repos/$pub/actions/runs/$run/artifacts" --jq "$split_jq_artifact" 2>/dev/null) || k=''
    [[ ! $k =~ ^[0-9]+$ ]] || gh api -X DELETE "repos/$pub/actions/artifacts/$k" >/dev/null 2>&1 || true
  else
    # Nothing sealed: it failed before act ran (the fetch, the runner), or the bundle does not open.
    failed=$(sed 1d <<<"$out" | awk '$1 == "failure" { $1 = ""; sub(/^ /, ""); print; exit }')
    if [[ -s $tmp/why ]]; then
      echo "Error: the run on $pub ($url) left a bundle this machine cannot open: $(tail -1 "$tmp/why") (bana split check)" >&2
    else
      echo "Error: the run on $pub ${st#completed }${failed:+ at its $failed step}, with no output sealed: $url" >&2
    fi
    rc=1
  fi
  rm -rf "$tmp"
  return "$rc"
}

# split_remote's run: "RUN URL" of the one named bana ID NONCE, dispatched on branch b at commit
# at, made since (split_remote's locals).
split_remote_find() {
  gh run list -R "$pub" --workflow bana.yml --event workflow_dispatch -L 50 \
    --json databaseId,event,headBranch,headSha,createdAt,url,displayTitle --jq "$split_jq_found" 2>/dev/null |
    awk -v t="bana $id $nonce" -v b="$b" -v at="$at" -v since="$since" '{ n = $1; e = $2; hb = $3; hs = $4; c = $5; u = $6
      $1 = ""; $2 = ""; $3 = ""; $4 = ""; $5 = ""; $6 = ""; sub(/^ +/, "") }
      $0 == t && e == "workflow_dispatch" && hb == b && hs == at && c >= since && n ~ /^[0-9]+$/ && u ~ /^https:\/\// { print n " " u; exit }'
}
# A cancel (split_remote's trap): its run there cancelled too, looked for if not found yet.
split_remote_cancel() {
  trap - INT TERM
  [[ -n $run || -z $sent ]] || { line=$(split_remote_find) || line=''; run=${line%% *}; }
  if [[ -n $run ]]; then
    gh run cancel "$run" -R "$pub" >/dev/null 2>&1
    echo "Error: cancelled, and its run on $pub too" >&2
  elif [[ -n $sent ]]; then
    echo "Error: cancelled; its run on $pub did not show up to be cancelled (gh run list -R $pub --workflow bana.yml)" >&2
  fi
  exit 130
}
# SECONDS, in the background: a signal's trap runs at once.
split_sleep() { sleep "$1" & wait $! || true; }
# Unix time T as GitHub writes times (UTC, ISO 8601).
split_iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -r "$1" +%Y-%m-%dT%H:%M:%SZ; } # T

# bana ci --remote: this checkout's HEAD (pushed: the public run fetches it from GitHub) built on
# the public repository, as the daemon builds it, by hand: its steps on the terminal, act's
# lines in ~/.bana/<prefix>/ci/remote.jsonl, the uploads in ci/remote/artifacts. No comment.
split_ci_remote() { # ROOT TIER JOB
  local pub b rc
  s_settings=$(d_project "$prefix")
  [[ -f $s_settings ]] || die "$prefix is not added here (bana add): bana ci --remote builds on its bana split repository"
  pub=$(d_setting split.repo "$s_settings") || pub=''
  [[ -n $pub ]] || die "bana split is off for $prefix: bana split on first"
  b=$(git -C "$1" rev-parse HEAD) || die "No commit in $1"
  git -C "$1" branch -r --contains "$b" 2>/dev/null | grep -q . ||
    warn "$(git -C "$1" rev-parse --short HEAD) may not be on GitHub yet: the run on $pub fetches it from $repo"
  mkdir -p "$home/ci/remote"
  say "bana ci --remote: $(git -C "$1" rev-parse --short HEAD)${2:+ at $2}${3:+, job $3}, on $pub's GitHub Actions"
  (
    cd "$home/ci/remote" || exit 1
    BANA_SPLIT_REPO=$pub BANA_SPLIT_PRIVATE=$repo BANA_BUILD=$prefix-ci-$(date +%s) \
      BANA_SPLIT_LOGS=$(d_setting split.logs "$s_settings" || echo private) BANA_SPLIT_HOME=$home/split \
      BANA_SPLIT_WORKFLOW=$(d_setting split.workflow "$s_settings" || true) BANA_SPLIT_KEY=$(d_setting split.key "$s_settings" || true) \
      BANA_SPLIT_SHA=$b BANA_SPLIT_REF=$(git -C "$1" symbolic-ref -q HEAD || true) \
      split_remote "$2" "$3" "$home/ci/remote/artifacts"
  ) >"$home/ci/remote.jsonl" && rc=0 || rc=$?
  if command -v jq >/dev/null; then BANA_LOGS=here split_runner_steps <"$home/ci/remote.jsonl"; fi
  sed -n 's/^{"bana":"remote","msg":"\(remote run in [^"]*\)"}$/\1/p' "$home/ci/remote.jsonl"
  echo "act's lines: $home/ci/remote.jsonl"
  return "$rc"
}

split_main() {
  local cmd=${1:-status} f
  shift || true
  case $cmd in
  render) split_render; return ;;
  lint)
    f=${1:-}
    if [[ -z $f ]]; then
      f=$(mktemp "${TMPDIR:-/tmp}/bana-split.XXXXXX")
      split_render >"$f"
    fi
    if split_lint "$f"; then echo "$f: as bana runs it"; else die "$f: not a bana.yml bana runs"; fi
    return
    ;;
  help | -h | --help) split_usage ;;
  esac
  split_init
  case $cmd in
  status) (($# == 0)) || split_usage; split_status ;;
  plan) split_plan "$@" ;;
  on) split_on "$@" ;;
  check) split_check "$@" ;;
  ci) split_ci_mode "$@" ;;
  logs) split_logs "$@" ;;
  sync) split_sync "$@" ;;
  rekey) split_rekey "$@" ;;
  purge-runs) split_purge "$@" ;;
  off) split_off "$@" ;;
  *) split_usage ;;
  esac
}

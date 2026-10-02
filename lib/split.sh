# shellcheck shell=bash
# shellcheck disable=SC2154 # bin/bana sets bana_root, prefix, repo, home, conf_file
# bana split: a private repository's CI and releases on a public one's GitHub Actions
# (docs/SPLIT.md). The public repository holds a README and one workflow, bana.yml, which
# bana renders from lib/split.yml.in; a run there fetches the private commit with a
# read-only deploy key, builds it with act, prints only its steps, and encrypts act's
# output to a key that stays on this machine. Sourced by bin/bana.
#
#   bana split render             the bana.yml this project's public repository would get
#   bana split lint [FILE]        checks a bana.yml (default: the render): only
#                                 workflow_dispatch, no permissions, no ${{ }} in run:, no
#                                 action but upload-artifact pinned, no cache, 1-day retention

split_usage() { awk '/^#   bana split/, /^#                                 workflow_dispatch/ { sub(/^# ?/, ""); print }' "$bana_root/lib/split.sh" >&2; exit 2; }

# Pins, each to check against GitHub when it changes (docs/SPLIT.md: LIVE-CHECK):
# act in the public runner (its Linux x86_64 tarball's sha256, from the release's
# checksums.txt); actions/upload-artifact's commit (v7.0.1); GitHub's ssh host key
# (docs.github.com: GitHub's SSH key fingerprints).
split_act_version=0.2.89
split_act_sha256=0191d6f1f3b716b5c55820032605d05fc3c1cdbf581ebeff655019e5dd1524c0
split_upload=043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
split_upload_tag=v7.0.1
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

# act, pinned, on the commit: no secrets, no GITHUB_TOKEN, no Docker socket in the job
# containers, an environment of four variables. Its output goes to out/act.jsonl, and
# only its steps reach the log as they end. BANA_TEST_ACT: an act already here (tests).
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
  args=(workflow_dispatch --json --rm --container-daemon-socket - -C "$d/src"
    -W "$d/src/.github/workflows/$split_workflow" -e "$d/event.json" --artifact-server-path "$d/art"
    --container-architecture linux/amd64 "${split_platforms[@]}")
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

# act's JSON lines in, a line a finished step or job out: `job / step: ok (12s)`. Never a
# step's output, act's words or a matrix value (those in a step's name become *); with
# BANA_LOGS=public, the steps' output as well.
split_runner_steps() {
  # shellcheck disable=SC2016 # jq's
  jq -rR --unbuffered --arg logs "${BANA_LOGS:-private}" '
    def hide($m): reduce ($m[] | select(length > 0)) as $v (.; split($v) | join("*"));
    def word: if . == "success" then "ok" elif . == "failure" then "failed" else . end;
    fromjson? // empty | select(type == "object")
    | ([(.matrix // {})[] | tostring]) as $m
    | ((.jobID // "?") | tostring) as $j
    | if .stepResult != null then
        "\($j) / \(((if (.stage // "Main") == "Main" then "" else "\(.stage) " end) + ((.step // "?") | tostring)) | hide($m)): \(.stepResult | word) (\(((.executionTime // 0) / 1000000000) | floor)s)"
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
# key, which goes in key.enc with the ciphertext's sha256, encrypted with RSA-OAEP to
# BANA_SEAL_PUB, whose private half only bana's machine has (split_unseal).
split_runner_seal() { # DIR SEALED
  local d=$1 o=$2 pass sum
  rm -rf "$o"
  mkdir -p "$o" "$d/out" "$d/art"
  if [[ -z ${BANA_SEAL_PUB:-} ]]; then
    echo "seal: failed (no BANA_SEAL_PUB here: bana split check)"
    return 1
  fi
  printf '%s\n' "$BANA_SEAL_PUB" >"$d/seal.pub.pem"
  tar -czf "$d/bundle.tgz" -C "$d" out art
  pass=$(openssl rand -hex 32)
  printf '%s\n' "$pass" | openssl enc -aes-256-cbc -pbkdf2 -iter 100000 -salt -pass stdin \
    -in "$d/bundle.tgz" -out "$o/bundle.enc"
  rm -f "$d/bundle.tgz"
  sum=$(split_sha256 "$o/bundle.enc")
  printf 'bana-seal 1\npass %s\nsha256 %s\n' "$pass" "$sum" |
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
# nothing in OUT, unless the key opens, the ciphertext is the one sealed, and the archive
# holds only out/ and art/.
split_unseal() { # SEALED OUT PEM
  local s=$1 o=$2 info pass sum list
  [[ -f $s/key.enc && -f $s/bundle.enc ]] || { echo "no sealed bundle in $s" >&2; return 1; }
  info=$(openssl pkeyutl -decrypt -inkey "$3" -pkeyopt rsa_padding_mode:oaep -in "$s/key.enc" 2>/dev/null) ||
    { echo "the bundle's key does not open with $3" >&2; return 1; }
  pass=$(printf '%s\n' "$info" | sed -n 's/^pass \([0-9a-f]\{64\}\)$/\1/p')
  sum=$(printf '%s\n' "$info" | sed -n 's/^sha256 \([0-9a-f]\{64\}\)$/\1/p')
  if [[ $(printf '%s\n' "$info" | head -1) != "bana-seal 1" || -z $pass || -z $sum ]]; then
    echo "the bundle's key is not bana's" >&2
    return 1
  fi
  [[ $(split_sha256 "$s/bundle.enc") == "$sum" ]] || { echo "the bundle is not the one sealed" >&2; return 1; }
  mkdir -p "$o"
  printf '%s\n' "$pass" | openssl enc -d -aes-256-cbc -pbkdf2 -iter 100000 -pass stdin \
    -in "$s/bundle.enc" -out "$o/bundle.tgz" 2>/dev/null || { rm -f "$o/bundle.tgz"; echo "the bundle does not decrypt" >&2; return 1; }
  list=$(tar -tzf "$o/bundle.tgz" 2>/dev/null) || { rm -f "$o/bundle.tgz"; echo "the bundle is not an archive" >&2; return 1; }
  if printf '%s\n' "$list" | awk '{ p = "/" $0 "/" } !/^(out|art)(\/|$)/ || index(p, "/../") { bad = 1 } END { exit !bad }'; then
    rm -f "$o/bundle.tgz"
    echo "the bundle holds more than out/ and art/" >&2
    return 1
  fi
  tar -xzf "$o/bundle.tgz" -C "$o" && rm -f "$o/bundle.tgz"
}

# ---- the public repository's workflow ---------------------------------------------------

# bana.yml for this project: lib/split.yml.in with the runner, its pins, and the project's
# workflow, tier input, timeout and platforms (Linux jobs in act.image, <prefix>-systemd's on
# the runner's own machine, which has systemd; macOS jobs not run).
split_render() {
  local wf tin t image runner l v p=()
  # shellcheck source=SCRIPTDIR/act.sh
  source "$bana_root/lib/act.sh"
  wf=$(conf workflow ci.yml)
  tin=$(conf tier_input tier)
  t=$(split_project daemon.timeout || daemon_conf daemon.timeout)
  case $t in *s) t=$(((${t%s} + 59) / 60)) ;; esac
  [[ $t =~ ^[1-9][0-9]*$ ]] || die "daemon.timeout: minutes, not '$t'"
  image=$(conf act.image catthehacker/ubuntu:act-24.04)
  while IFS=$'\t' read -r l v; do
    case $v in
    linux) v=$image ;;
    mac | skip | "skip "*) v= ;;
    esac
    p+=("-P" "$l=$v")
  done < <(act_platform_table)
  p+=("-P" "$prefix-systemd=-self-hosted")
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

# Checks a bana.yml: why it is not one bana runs, a line each (nothing: it is).
split_lint() { # FILE
  awk '
    function bad(why) { print why; n++ }
    function ind(s) { match(s, /^ */); return RLENGTH }
    { line = $0; i = ind(line) }
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

split_main() {
  local cmd=${1:-status} f
  shift || true
  case $cmd in
  render) split_render ;;
  lint)
    f=${1:-}
    if [[ -z $f ]]; then
      f=$(mktemp "${TMPDIR:-/tmp}/bana-split.XXXXXX")
      split_render >"$f"
    fi
    if split_lint "$f"; then echo "$f: as bana runs it"; else die "$f: not a bana.yml bana runs"; fi
    ;;
  help | -h | --help) split_usage ;;
  *) split_usage ;;
  esac
}

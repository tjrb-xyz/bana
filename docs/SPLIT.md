# bana split: private code, public CI and releases

`bana split` keeps a project's code in its private repository and builds it on a second, public
repository's GitHub Actions, where standard GitHub-hosted runners are free, and publishes its releases
there. The public repository holds a README and one workflow, `bana.yml`, which bana renders and checks.
The daemon on your machine still watches the private repository; for each push it dispatches a run on
the public one, which fetches the commit with a read-only deploy key, builds it with act (the same
workflow `bana ci` runs), and prints only its steps. The full output is encrypted to a key on your
machine: the daemon decrypts it, and writes it to the private repository (a commit comment) and to its
page, labelled as a remote run with a link to the public one. Statuses, the CI report, the log viewer
and `bana fix` work as for a build here.

```text
push to PRIVATE ─▶ bana daemon (your machine) ─▶ gh workflow run bana.yml -R PUBLIC
                                                     │  fetch (deploy key, then deleted) · act · seal
PUBLIC's run log: "rust / cargo test: ok (12s)"  ◀───┘  artifact bana-sealed (1 day, encrypted)
bana daemon ◀─ gh run download · decrypt ─▶ statuses, page, report, bana fix · commit comment on PRIVATE
```

It suits a project whose code must stay private and whose CI should not spend private Actions minutes
or wait for your machine's Docker. Read [what is public](#what-is-public) first.

## Turn it on

In the project's checkout, once `bana add` added it (or `bana add --split` does both):

```sh
bana split plan                         # what it would do, and what it checks first; changes nothing
bana split on                           # the wizard: asks the name, how to make it, the logs; a typed yes
bana split on --repo acme/widget-ci     # off a terminal (with BANA_SPLIT_CONSENT, below)
bana split on --web                     # make the public repository on GitHub's page
bana split on --releases-only           # releases public, builds still here (split.ci = local)
bana add --split[=OWNER/NAME]           # add the project, then bana split on
```

The public repository's default name is `OWNER/<name>-releases`. When it does not exist yet, the wizard
asks how to make it:

1. **here, with gh**: `gh repo create OWNER/NAME --public --disable-wiki --disable-issues`, after your yes;
2. **on GitHub's new-repository page**: bana opens it filled in (`open` on a Mac, `xdg-open` on Linux with a
   display, else it prints the link), you make the repository there, public and with nothing in it, and
   press Enter. bana checks it is there, public and empty (a README alone is taken: GitHub's page may add
   one, and bana's replaces it), and asks again until it is; `q` stops with nothing changed. `--web` picks
   this way, and needs a terminal.

A public repository there already is taken only when it is empty, holds a README alone, or is bana's own
(its README's marker). Off a terminal, `--repo` names it, gh makes it, and `BANA_SPLIT_CONSENT` must hold
the phrase the wizard would ask for (`yes, build OWNER/PRIVATE in public`).

Before anything changes, `on` checks all of this and lists every problem at once: gh signed in with the
`repo` and `workflow` scopes; you are an admin of the private repository, and it is private; no remote of
your checkout points at the public repository, which shares no commit with the private one; openssl and
ssh-keygen are here. It warns of the workflow's macOS and Windows jobs (they do not run there), of jobs
that read secrets (they get none), and of submodules (not fetched). Then it shows the plan, the risks
(below, word for word), and asks where the detailed logs go (private, the default, or public, which needs a
second typed phrase), then for the typed yes. Then, each step `[n/9] … ok`:

1. a seal key here, `~/.bana/<prefix>/split/seal.pem` (RSA-3072, 0600; never leaves this machine);
2. a fresh ed25519 deploy key on the private repository, read-only (checked; a writable one is deleted).
   It comes first, so an organization whose policy forbids deploy keys stops the wizard before anything
   public exists. bana has no token fallback;
3. the public repository (unless you made it on GitHub's page);
4. its README, with a marker: a hash of the private repository's name;
5. its Actions: a read-only default token, `actions/upload-artifact@*` as the only allowed action, the
   environment `bana-source` limited to the default branch, and a ruleset that keeps that branch from
   deletion and force pushes;
6. `bana-source`'s two secrets, on gh's stdin: `BANA_SOURCE_KEY` (the deploy key's private half, then
   deleted here) and `BANA_SOURCE` (the private repository's name, so the logs mask it);
7. the variable `BANA_SEAL_PUB`: the seal key's public half;
8. `.github/workflows/bana.yml`, `bana split render`'s, checked by `bana split lint`;
9. the project's settings: `split.repo`, `split.ci`, `split.logs`, `split.workflow`, `split.key`, and
   `release.repo` (the public repository).

Where GitHub's API allows it, fork pull requests' runs wait for approval and logs and artifacts are kept a
day; where it does not, the wizard says which setting to change by hand. Each step is recorded in
`~/.bana/<prefix>/split/state`: after a failure, `bana split on` goes on from there, and does nothing twice.
Then it runs `bana split check`.

By hand, if you want them: turn pull requests off on the public repository (Settings → General), and stop
the private repository's own workflow on pushes (Actions → the workflow → Disable workflow, or bana add's
`<P>_CI_AUTO` variable), so pushes spend no private minutes.

## The toggles

```sh
bana split                     # status: on or off, the public repository, logs, the last remote run
bana split ci github|local     # where pushes build; releases stay on the public repository either way
bana split logs private|public # what a run prints publicly: its steps, or its whole output too
```

`split.logs = private` (the default) prints a line a step and job, `rust / cargo test: failed (12s)`, and a
job summary table of the same. Matrix values in a step's name become `*`. `public` prints each step's output
too, and needs the typed phrase `yes, logs are public`; back to private needs a yes. Runs already on the
public repository keep what they printed: `bana split purge-runs` deletes them.

Fix rounds always build here, with act: Claude's snapshots never leave your machine. `bana ci` by hand
builds here too.

## What a run does

The public workflow takes `workflow_dispatch` alone, with the inputs id, sha, ref, tier, job and logs, and
`permissions: {}`. Its inputs reach the script through `env:` only, and are checked first. Its steps:

- **fetch**: plain git and ssh with GitHub's pinned host key fetch the commit with the deploy key, which is
  deleted before any of the project's code runs. Nothing git prints reaches the log. No third-party action
  runs while the key is on disk;
- **build**: act, pinned by its tarball's sha256, runs the private workflow under `env -i` (PATH, HOME and
  RUNNER_TEMP), with no secrets, no GITHUB_TOKEN and no Docker socket in the job containers. Its output goes
  to a file; workflow commands are off while it runs, so nothing in it makes an annotation. Linux jobs run in
  bana.conf's `act.image`; `<prefix>-systemd` jobs run on the runner's own machine (act's host mode), which
  boots systemd; macOS jobs do not run;
- **summary**: the table of steps;
- **seal**: act's output and the jobs' uploads, as a tar, encrypted with AES-256-CBC (pbkdf2) under a fresh
  key; that key and the ciphertext's sha256 are encrypted with RSA-OAEP to `BANA_SEAL_PUB`;
- **upload**: the artifact `bana-sealed`, kept a day.

`bana split lint` refuses any other trigger, a token with scopes, `${{ }}` in a `run:` script, any action
but upload-artifact pinned to a commit, a cache, and retention over a day.

## Releases

Releases go to `release.repo`, the public repository: `bana installer --tag` and the daemon's installer
point the installers there (public assets download with curl, no sign-in), and Publish lists, views, deletes
and creates releases there. That repository has none of the private commits, so gh makes the tag on its
default branch (no `--verify-tag`), and GitHub's automatic *Source code* archives hold only its README and
`bana.yml`. bana's notes there have the pull requests' titles and the commits' subjects, without `#N`, commit
or changelog links. The release's page says the notes and files become public. Release files are built by the
release tier (on the public repository inside the sealed bundle, or here), and uploaded from your machine.

Before Publish uploads anything to the public repository, it reads every file (inside each `.tar.gz` and
`.zip`) for the private repository's name, `/home/runner/work/` and the build's checkout paths. One found stops
the publish, naming the file and what it holds. A Rust binary carries its source paths in panic messages and
debug info: build release files with `RUSTFLAGS="--remap-path-prefix=$PWD=."` and stripped
(`[profile.release] strip = true`), or the files of a remote build are refused.

`bana split ci local` with `release.repo` set is "private code, public releases" with no public CI.

## Undo, and the rest

```sh
bana split check [--quick]   # ok, WARN or FAIL, read-only; exit 1 on a FAIL
bana split sync [--yes]      # bana.yml again, as this bana renders it (after bana upgrade)
bana split rekey [--yes]     # a new deploy key and seal key; the old ones go once the new ones work
bana split purge-runs [--yes]
bana split off [--yes] [--purge-runs]
```

`check` fails when the public repository has another workflow, or `bana.yml` is not the one bana pushed, or
the deploy key can write or is gone, or the environment, its secrets, the variables, the allowed actions or
the token's default are not as bana set them, or a debug variable is there, or a remote of your checkout
points at the public repository, or it shares a commit with the private one. It warns of other collaborators
(each can read your code through the deploy key) and of an older render. Before every remote build, the
daemon runs `check --quick` (the workflow and the deploy key): on a FAIL it dispatches nothing, and the
build fails saying why.

`off` deletes the deploy key first, which cuts the public side's access at once, then the secrets, the
environment and the variable, disables `bana.yml`, and with `--purge-runs` deletes the runs. It keeps
`release.repo` (your installers point there) unless you say otherwise on a terminal, and removes
`~/.bana/<prefix>/split`. It never archives or deletes the repository: `gh repo archive OWNER/NAME`, or
`gh auth refresh -s delete_repo && gh repo delete OWNER/NAME`. `bana remove` refuses while bana split is on.

The settings are the daemon's (`~/.bana/<prefix>/daemon/settings`), written by bana split alone and kept by
`bana add`; a commit's bana.conf cannot move CI, logs or releases.

## What is public

The wizard shows this before your yes, with your repositories' names:

```text
bana will build <private> on GitHub's machines from the PUBLIC repository <public>. The
workflow there is bana's own. With logs set to private it prints only job and step names,
ok or failed, and durations. Your full output is encrypted to a key on this machine, then
written to <private> (as a commit comment) and to bana's page. Even so:
- Anyone can see that <public> exists, its workflow file, each run's inputs (commit sha,
  branch or tag name, tier), job and step names, timings, results and the encrypted
  bundle's size.
- Anything printed before bana's redirect (a runner, Docker or download failure), or
  written to the runner's log some other way, is public, and copies cannot be taken back.
- The deploy key in <public> reads ALL of <private>: every branch and its history. Anyone
  with write access to <public>, or anyone who takes over your GitHub account or gh token,
  can use it to copy your code. Deleting the key stops future reads, not past copies.
- Your build's own code and dependencies run on GitHub's machines with network access.
  They cannot reach the key, but a malicious dependency could send your code anywhere.
- Jobs that need secrets get none. macOS and Windows jobs do not run there.
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
```

## What a remote build looks like

On the public repository, a run's log is its steps (`fetch: ok`, `rust / cargo test: failed (12s)`) and a job
summary of them; in the private repository, a comment on the commit:

```text
### bana: remote run 9182736 in public repo acme/widget-ci, failed

Build #57 of main at `1a2b3c4d5e`, built on GitHub's machines from acme/widget-ci ([run 9182736](…)),
dispatched by bana on mbp.

# CI report: acme/widget · main 1a2b3c4 · quick · failed
…
All of its output: bana's page on mbp (build #57), or `bana report 57` there.
```

and on bana's page, the build's *remote: acme/widget-ci #9182736* chip and link; its statuses say
`remote acme/widget-ci: failed at "cargo test"` and link the run.

## Check on GitHub before relying on it (LIVE-CHECK)

bana's tests run on stand-ins for GitHub and gh; these can only be checked against GitHub itself, once, with a
scratch pair of repositories (a private one with a small workflow, and `bana split on` for it):

1. The public log of a passing and a failing run holds only the step lines and the summary: no output,
   annotation or path (and with `bana split logs public`, the output too). Download the log archive as well.
2. The fetch: GitHub serves a commit by sha to a read-only deploy key (`git fetch --depth 1 … SHA`), and the
   pinned host key (`split_host_key` in lib/split.sh) is GitHub's ed25519 key in
   [GitHub's SSH key fingerprints](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints).
3. The pins in lib/split.sh: act's version and its `act_Linux_x86_64.tar.gz` sha256 (the release's
   `checksums.txt`), and actions/upload-artifact's commit for its tag.
4. The seal: macOS's LibreSSL and the runner's OpenSSL 3 agree on `openssl enc -aes-256-cbc -pbkdf2 -iter
   100000` and `openssl pkeyutl -pkeyopt rsa_padding_mode:oaep` (a run sealed there opens here).
5. The REST endpoints the wizard uses and `bana split check` reads: deploy keys (`repos/O/R/keys`),
   environments and their deployment branch policies, `actions/permissions` and `selected-actions`, the
   workflow permissions, rulesets, and the two it tries: `actions/permissions/fork-pr-contributor-approval` and
   `actions/permissions/artifact-and-log-retention` (where missing, the wizard says what to set by hand). And
   gh's flags: `gh secret set --env` on stdin, `gh variable get`, `gh workflow run -f`, `gh run list
   --json databaseId,url,displayTitle`, `gh run download -n`.
6. An organization's policy that forbids deploy keys: the wizard stops at step 2, with nothing public made.
7. A release on the public repository: gh makes the tag on its default branch, the installer downloads with
   curl and no sign-in, and the notes link nothing private.
8. The billing and terms wording in the risks: [About billing for GitHub Actions](https://docs.github.com/en/billing/managing-billing-for-your-products/managing-billing-for-github-actions/about-billing-for-github-actions)
   and GitHub's terms for Actions.
9. A `<prefix>-systemd` job: act's host mode on the runner's machine, which boots systemd.

## Limits

Linux jobs only (act inside `ubuntu-latest`); jobs that need secrets, private submodules or private git
dependencies stay with `bana split ci local`. act on a hosted runner pulls its images each run, so it is
slower than act here, and behaves as act does, not as GitHub's own runner. A remote build holds bana's one
build slot while it waits on GitHub, so other projects' builds here wait too. With your machine off, nothing
is dispatched and statuses stay pending: no token that could post them is in the public repository.

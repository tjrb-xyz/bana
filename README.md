# bana

bana runs a project's GitHub Actions workflow on your own machines, for CI that is too long, too big or too
hardware-bound for GitHub's runners. It is small on purpose, and it works in three ways:

- **`bana daemon`: CI on push, on your Mac.** One daemon a machine serves every project you add with
  `bana add`: it fetches each repository, runs the workflow with [act](https://github.com/nektos/act) for each
  eligible push, one build at a time, and posts commit statuses to GitHub. 🧱 in the menu bar shows what it is
  doing, and a click opens its page with the build's live log. It keeps nothing awake while idle, and a push
  made while the Mac sleeps builds when it wakes.
- **`bana ci`: the same, by hand.** It runs the jobs with act in OrbStack's Docker: Linux jobs in containers,
  and on a Mac the macOS jobs on the Mac itself, with its real CoreAudio and USB devices. Nothing stays running.
  Use it before you push, and before you add the project to the daemon.
- **`bana up`: a runner pool, if you want it.** Your machines register as self-hosted runners and GitHub gives
  them the jobs from each push. That needs them awake and online, so it is opt-in, and `bana down` undoes it.
  A project uses the daemon or the pool, not both.

It runs on macOS (Apple silicon, the stock bash 3.2 and BSD tools, Xcode's command line tools) and on Debian or
Ubuntu. It was extracted from its first user, a project kept here as [the worked example](#the-worked-example),
`example`.

## CI on push: bana daemon

With act, OrbStack (running) and the GitHub CLI signed in (and Rust in a git checkout of bana, where cargo builds
the daemon the first time; a release has it built):

```sh
brew install act gh yq           # and OrbStack, for Docker
gh auth login
bana daemon install              # once a machine, anywhere: check, build, start; on a Mac it opens the page
cd ~/src/myproj                  # then in each project's checkout:
bana add                         # where each job runs here; bana.conf and workflow changes, if you say y; CI here
git add .github && git commit -m "bana as CI"   # the daemon builds commits, with their bana.conf
bana ci                          # once by hand: the workflow works under act
git push                         # builds on this Mac
```

Install checks the machine and starts the one daemon; `bana upgrade` takes a newer bana, the daemon first. `bana add` checks the
project and warns about what in the workflow would go wrong under the daemon
([below](#what-the-workflow-needs)), then adds it: the daemon builds it in its own clone,
`~/.bana/<prefix>/src`, never in your checkout, and starts from the branches as they are: the next push builds.
Its settings (bana.conf's `daemon.*` keys) are read at `bana add`, so run it again after changing them.

```sh
bana list                        # the projects added here: state, queue, last build, checkout, files
bana pause [PROJECT]             # no automatic builds of its pushes; Run now, fixes and releases still work
bana resume [PROJECT]            # the pushes that waited build
bana remove [PROJECT] [--purge]  # its CI here goes; --purge also its clone, builds and state
```

PROJECT is a prefix `bana list` shows, by default the checkout's. A project's files (its clone, builds, fixes
and releases) are in `~/.bana/<prefix>/`, which `bana list` shows.

**How a push reaches it.** The daemon fetches every 30 seconds; that is one small git request, and it catches up
by itself after the Mac sleeps. A push from your checkout arrives at once: `bana add` adds a git hook there
(`reference-transaction`) that, when a push goes through, asks the daemon to fetch now. A push from elsewhere (a
merge on GitHub, another machine) waits for the next fetch. No webhook is needed, so nothing on your Mac is
reachable from the internet and no GitHub Actions minutes are spent. `bana add --no-hook` leaves the hook out,
and a hook of your own by that name is left alone.

**On GitHub** each build posts the statuses `bana` for the build and `bana/<job>` for each job that starts:
`running on mbp (quick)`, then `passed on mbp in 12m · 5 jobs` or `rust failed at "cargo clippy" · 3m40s on
mbp`. A nightly run by hand posts `bana nightly` instead. The *Details* link opens the build on the daemon's
page, at `http://127.0.0.1:8470/`, so it works only on the machine that ran it.

**In the menu bar**, 🧱 alone is idle; `🧱 4m +2` is building for 4 minutes with 2 queued; `🧱 !` means the
last build failed; `🧱 paused`, `🧱 no Docker` (start OrbStack), `🧱 busy` (waiting for your `bana ci`) and
`🧱 !gh` (statuses not posted) say why builds wait; `🧱 v0.1.0?` asks whether to publish a release. With more
than one project the title names the one it is about (`🧱 wid 4m +2`). A left click opens the page; a right
click has a submenu for each project (*Cancel #N*, *Fix #N with Claude…*, *Publish v0.1.0…*, *Pause automatic
builds*) and *Quit bana*, which stops local CI until the next login.

**The page**, *Local CI*, has a project picker (with two or more projects), the queue, the running build with
its jobs and live log, and the history. *Run now* builds a branch at any tier, which is how a nightly runs.
*Pause automatic builds* holds the pushes (the running build finishes, and Run now still builds); *Cancel* stops
one; *Re-run* builds the same commit again.

**Which pushes run:** branches matching `daemon.branches` (all but `dependabot/*` and `renovate/*`), tags
matching `daemon.tags` (none by default), and not a head commit with `[skip ci]` or another of GitHub's skip
markers. The rules come from the install, not from the pushed commit. A commit already built at that tier does
not run again, unless a tag names it: a tag's build is its release ([Releases](#releases)). A branch keeps one queued build, and a newer push replaces it; a running build finishes.

**A green build's files.** What a green build's jobs upload with `actions/upload-artifact@v4` becomes its files,
in `~/.bana/<prefix>/builds/<id>/dist/`: each zip is checked against the digest its upload step printed, then
unpacked, and a `NAME.sha256` beside a file must match it. When they include archives
(`NAME-...-linux-x64.tar.gz`, [below](#the-projects-installer)), bana compiles the project's installer beside
them. The page lists them with *Download* and the command that installs the build here, `bana install <id>`,
in a terminal, where the project's hook can ask. They are kept a week, and the newest of each branch and tier
while the branch is there, so the latest nightly of main can always be installed. A failed build's uploads stay
as act left them, for a week; a problem with the files never changes a build's result.

**One build at a time.** The daemon builds one project's build at a time, each project in turn; another
project's queue says *after wid #12*. The daemon and `bana ci` share a lock: while the daemon builds, `bana ci`
stops with *act is busy here*, and while your `bana ci` runs, the daemon's builds wait.

**Sleep.** Nothing keeps the Mac awake while idle. While act runs, `caffeinate` stops idle sleep; closing the lid
still sleeps the Mac, and the build pauses until it wakes. After a wake the next fetch comes within 30 seconds
and queues one build for each branch that moved. A build cut short by a crash or a reboot runs again once, if
its branch has not moved.

**Trust.** A push runs its workflow as you on your Mac, macOS jobs outside any container, with your home, SSH
keys and Keychain in reach. Use the daemon only on a private repository, where write access is the gate. Jobs
get the GitHub CLI's token as `GITHUB_TOKEN` (`daemon.token = none`: an empty one).

```sh
bana daemon status               # whether it runs, and what each project builds and waits for
bana daemon log                  # its log (~/Library/Logs/bana/bana.log on a Mac)
bana daemon open [PROJECT]       # the page
bana daemon poke [PROJECT]       # fetch now (every project, outside a checkout)
bana daemon uninstall [--purge]  # stop it; the projects stay added (--purge: bana remove --purge each)
```

[docs/DAEMON.md](docs/DAEMON.md) has the rest: every setting, how cancels and timeouts work, the files it keeps,
running it on Linux, moving from the daemon a project of before, and the trust boundary in full.

### What the workflow needs

The daemon runs the workflow as `workflow_dispatch`, with an event shaped like the push (`github.ref`,
`github.sha`, `github.event.before` and the tier input as a push would have them). `bana add` warns about each
of these, and proposes the changes:

- GitHub itself runs nothing on a push: `on:` keeps only `workflow_dispatch`, or its root jobs are gated on a
  variable, `if: (github.event_name != 'push' && github.event_name != 'schedule') || vars.<PREFIX>_CI_AUTO != 'false'`,
  set to `false` (`gh variable set <PREFIX>_CI_AUTO --body false`); pull requests still run on GitHub. Otherwise,
  with no runner pool, GitHub queues self-hosted jobs that no runner ever takes.
- No checkout `ref:`: act builds the pushed commit only for a checkout without one.
- A step gated on `runner.environment == 'self-hosted'` also needs `|| env.ACT == 'true'`: act never sets
  `runner.environment`, so the step would be skipped. `$RUNNER_ENVIRONMENT` in a script is empty too.
- Every job has a place here: a runs-on label bana does not know is not run, and the build still passes
  (`act.platform.*`, below).

macOS jobs run in a fresh copy of the commit, without `target/` or other gitignored files, so they build from
scratch. Linux jobs run in fresh containers unless bana.conf has `act.args = --reuse`.

## bana ci: the workflow on this machine

```sh
brew install act                 # and OrbStack, for Docker
bana ci                          # every job, with the first tier (quick)
bana ci nightly                  # another tier
bana ci -j rust                  # one job, and the jobs it needs
bana ci --x64                    # Linux containers as x86_64 (Rosetta, on Apple silicon)
bana ci --list                   # the jobs
bana ci -- --reuse               # anything after -- goes to act; --reuse keeps containers, and their builds
```

| Job's `runs-on` | Where it runs |
|---|---|
| `<prefix>-linux`, `ubuntu-latest`, `ubuntu-24.04`, `ubuntu-22.04` | a container from `act.image` (default `catthehacker/ubuntu:act-24.04`) |
| `ubuntu-20.04` | a container from `catthehacker/ubuntu:act-20.04` |
| `<prefix>-macos`, `macos-latest` | on a Mac, on the Mac itself (act's host mode, in a copy of your working tree); elsewhere skipped |
| `<prefix>-systemd` | on a Mac, in its Linux machine (`vm`, an OrbStack or Lima Ubuntu with systemd), in act's host mode, after the rest; elsewhere skipped |
| `ubuntu-18.04`, and any other label | not run (act has no image for 18.04), unless an `act.platform.<label>` key gives it a place |

A job takes the first of its labels that has a place, as act does. `act.platform.<label>` (lowercase) in
bana.conf is `linux` (a container from `act.image`), `mac` (the Mac itself; elsewhere not run), `machine` (the
Mac's Linux machine; elsewhere not run), `skip [reason]` (not run) or an image of its own; `bana add` asks and
writes these. `self-hosted` and a label with `=` cannot be keys. After a run, bana names the jobs that had no
place: *not run here: JOB (runs-on: ...): see bana add*.

**Jobs that need systemd** (a user service, `loginctl`) cannot run in act's containers: act starts each one with
its own entrypoint, so systemd never boots there. Give such a job `runs-on: [self-hosted, <prefix>-systemd]`
(not `<prefix>-linux` too: a job takes its first label with a place). On a Mac, `bana ci` then runs a second
act once the first has ended, in the Linux machine `vm` (made if missing, with `packages.linux`, `hook.linux`
and act at the Mac's version, as `bana linux-prepare`): the skipped jobs, and the Linux jobs they need, run in
act's host mode there, as the machine's user (who has passwordless sudo), on the same checkout and event.
Under the daemon only those jobs' lines reach the build, so a job they need is not reported twice. A job that
needs a macOS job cannot run there. `bana up`'s Linux runners are systemd services, and have the label too.

The run uses your working tree, uncommitted changes included, and the workflow's tier input (`tiers`,
`tier_input` in bana.conf). Artifacts land in `~/.bana/act/artifacts/1/<name>/<name>.zip` (upload-artifact@v4:
one zip each), the last run's only: a run first removes the previous run's, which act would otherwise hand
to its download-artifact steps (every run is act's run 1). With the GitHub CLI signed in,
jobs get its token as `GITHUB_TOKEN`.

act's output also goes to `~/.bana/<prefix>/ci/last.log`, and what ran to `last.env` (commit, changed files,
tier, job, network, versions, exit status, and whether Ctrl-C stopped it), for `bana fix`, which a failed run
points to. `ci.log = no` skips it. The workflow's `vars.*` come from `~/.bana/<prefix>/vars` (`KEY=value`
lines) if you write one, as under the daemon.

Limits worth knowing:
- act uses your working tree only for a checkout step without `ref:` (or with `ref:` equal to the current ref).
  A checkout with any other `ref:` clones from GitHub instead.
- act runs every Linux container at one architecture per run, so a matrix over CPUs needs `bana ci` and
  `bana ci --x64`.
- act reimplements GitHub's runner. Most actions work, but it is not bit-for-bit GitHub.
- On a Mac, macOS jobs run as you, but not in your working tree: the checkout step copies it into a fresh
  directory under act's cache (`~/.cache/act/<random>/hostexecutor`), leaving out gitignored files such as
  `target/`, so they build from scratch. act deletes the copy after the job (after a failed one only with
  `-- --rm`). bana's `keep-builds` does nothing under `bana ci`.

## Fix a failure with Claude Code

`bana fix` hands a failed run to [Claude Code](https://claude.com/claude-code), on a branch of its own:

```sh
bana fix                     # the newest failure here: the last bana ci, or the daemon's last failed build of this branch
bana fix 41                  # daemon build 41 (or Fix with Claude on the daemon's page, or in 🧱)
bana fix last                # the last bana ci here
pbpaste | bana fix --log -   # act's output from elsewhere (or --log FILE); the fix starts at HEAD
bana fix --open              # in a new terminal, through Claude Code's claude-cli:// link
bana fix --headless          # unattended, within fix.turns and fix.budget_usd
```

It makes `bana/fix-<sha7>` at the failing commit, as a git worktree of your checkout in
`~/.bana/<prefix>/fix/<sha7>`, writes a brief of what failed and how it ran (the failing tests with their file
and line, cargo's rerun command, each failed step's last lines, and what was bana's or act's rather than the
project's), and starts Claude Code there with a prompt that says so. Your working tree and branches stay as they
are, and a commit on the fix branch shows up in your checkout at once.

With the daemon, Claude tests through bana's tools (an MCP server that `bana add` registers with Claude Code
in your checkout; `--no-claude` leaves it out). `run_jobs` snapshots the worktree and has the daemon
run the failed jobs on it under act, the way CI ran them, at the front of its queue and without posting
statuses. A fix gets `fix.rounds` (5) such rounds, one at a time, plus round 0: the failed jobs again at the
unchanged commit, to tell a real failure from an environmental one. When a round is green, `commit_fix` (which
asks you) or *Keep* on the fix card commits exactly the tree that passed. A Stop hook holds Claude once when it
stops with changes no round tested. Claude works with your own Claude Code settings plus rules against
`git push` (a guard for Claude, not a lock). Pushing is yours:

```sh
bana fix list                              # the fixes: where each stands, rounds, branch, worktree
bana fix brief [FIX]                       # what failed, where and how it ran
bana fix push [FIX] [--pr]                 # push the branch (the daemon builds it); --pr opens a pull request
bana fix drop [FIX] [--force] [--delete-branch]   # remove the worktree and round builds; the branch stays while it has commits
```

The page's fix card has the same: the rounds, *Keep*, *Push*, *More rounds* and *Discard*. It needs
bana-manager: the daemon's (`bana daemon install`), else one bana builds with cargo the first time.
[docs/FIX.md](docs/FIX.md) has the rest, and a checklist to run once on a Mac.

## The CI report

A report of how a run went, as percentages per standard, in Markdown:

```sh
bana report                  # the newer of the last bana ci here and the daemon's last build
bana report 41               # daemon build 41 (the build page's Report tab has it too)
bana report last             # the last bana ci here
pbpaste | bana report --log -   # act's output from elsewhere (or --log FILE)
bana report --json           # {markdown, standards}
```

A standard is a group of checks, named in bana.conf, in the order you want them; an `all` row comes last:

```
report.rust = rust                 # a job (its id, or a matrix entry's name: "package (*)")
report.web = "web/pnpm test" e2e   # JOB/STEP: some steps of a job
report.engine = test:real_*        # tests by name, wherever they ran (cargo's and nextest's; a::real_x too)
report.left_out = "*left out*"     # ::notice:: text that means a step did not check here
```

With no `report.*` key, each job is a standard. *Checks* are the project's own steps that passed or failed (not
bana's, not act's, not Pre or Post). *Tests* are the counts the test tools printed (cargo, nextest, vitest, jest,
node, pytest, unittest; go's packages apart): passed of those run, skipped apart. A run that stopped early (cargo at
its first failing binary, pytest `-x`, a bail, a step that never ended) reads `95% of 22 run (incomplete)`, nothing
counted reads `—`, and a job that did not run here (skipped, no platform, not planned at the tier, left out, or failed
before its first step) is listed as not run, never as a pass or a failure. Below the table come the failures (tests,
where they failed, the rerun command), what was bana's or act's, what did not run, and the steps' own summaries.
Each daemon build writes one when it ends, with its commit's standards (a fix round's: its failing commit's);
`bana ci` ends with the table and keeps the report in `~/.bana/<prefix>/ci/last.report.md`. Nothing goes to GitHub.
[docs/DAEMON.md](docs/DAEMON.md#the-ci-report) has results.jsonl, what the report is made from, for a builder other
than act.

## The project's installer

bana compiles an installer for the project's builds from its templates and bana.conf's `install.*` keys:
`install.sh` (POSIX sh: macOS and Linux, under dash, bash 3.2 and busybox) and `install.ps1` (Windows PowerShell
5.1 and PowerShell 7), with each archive's sha256 baked in, and `SHA256SUMS`, written last.

```sh
bana installer DIST --tag v1.2.0         # a release: the installers download from it (gh, else curl)
bana installer DIST --label nightly-abc  # a daemon build: its installer takes --from
bana install [BUILD]                     # a daemon build's files, here: sh DIST/install.sh --from DIST
sh install.sh [--yes] [--prefix DIR] [--bin-dir DIR] [--from FILE|DIR] [--force] [--no-hook]
sh install.sh --uninstall [--purge]
```

The archives are the build's files named `NAME-...-(linux|macos)-(x64|arm64).tar.gz` and
`NAME-...-windows-(x64|arm64).zip`, one per platform, each holding one directory (only a Windows zip: no
install.sh). A Linux archive is used on any libc, so build it static (a musl target, say) if it should run on
Alpine or other busybox systems. The installer picks this
machine's (arm64 under Rosetta too), checks its sha256 (no sha256sum, shasum or openssl: it stops), refuses an
archive with entries outside its directory, and unpacks it into `PREFIX/TAG` beside the installed version; then
`PREFIX/current` switches to it and the commands are linked into the bin directory. The previous version stays
(older ones go), a receipt in `PREFIX/receipt` says what is installed, and `--uninstall` removes only what it made:
the versions the receipt lists, `current` and the links, so PREFIX may be a directory with other things in it.
It never uses sudo and never edits your shell's rc files.

```
install.name = example                 # its directories and messages (default: prefix)
install.bins = exampled example-new    # the commands linked (default: every program in the archive's bin/)
install.hook = install-hook.sh         # the lifecycle hook, a path inside the archive
install.hook_ps1 = install-hook.ps1    # the same for Windows
install.prefix = ~/.local/share/example   # where versions go (--prefix, INSTALL_PREFIX)
install.bin = ~/.local/bin             # where commands are linked (--bin-dir, INSTALL_BIN)
install.config = ~/.config/example     # the settings: given to the hook, removed only by --purge
install.env.EXAMPLE_LOG_DIR = ~/Library/Logs/example   # given to the hook (~ is the home)
release.files = example-*.tar.gz example-*.deb         # the files that are the release (default *)
release.platforms = linux-arm64 linux-x64 macos-arm64  # the release's page lists those with no archive
```

The hook runs as `sh HOOK STAGE` from the archive: `pre-install` from the unpacked files before they are current
(failing: nothing changes), `post-install` once they are (failing: installed, and `--force` runs it again),
`pre-uninstall` (failing: nothing is removed) and `post-uninstall` (from a copy). An upgrade is pre-install and
post-install with `INSTALL_PREVIOUS` set. It gets `INSTALL_STAGE`, `DIR`, `TAG`, `PREVIOUS`, `PREVIOUS_DIR`,
`PREFIX`, `BIN`, `CONFIG`, `NAME`, `YES`, `OS` and `ARCH`, and the `install.env.*` keys; its stdin is the terminal,
even under `curl | sh`, so it can ask before it uses sudo (`--yes` answers for it). The example's
[install-hook.sh](examples/example/install-hook.sh) puts its audio devices in /Library/Audio/Plug-Ins/HAL that way.
The daemon runs `bana installer dist --label <tier>-<sha10>` (`--tag` for a tag) after each green build with
archives, from the built commit's bana.conf, and `bana install [BUILD]` runs that installer here.
`install.prefix`, `install.bin`, `install.config` and `install.bins` are install.sh's (install.prefix and
install.config are never `~` or `/` themselves); install.ps1 uses `%LOCALAPPDATA%\Programs\NAME`, puts all of
`current\bin` on your user PATH, and keeps settings in `%APPDATA%\NAME`. It also works as `irm URL | iex`: a
failure then returns to your prompt with `$LASTEXITCODE` 1.
Every value must be printable ASCII.

## Releases

With the daemon and `daemon.tags = v*`, a tag you push is a release candidate: bana builds it at
`daemon.tag_tier`, and when that build passes with its files, asks whether to publish it. You choose the version
and make the tag; bana never makes, moves or deletes one.

```sh
gh auth refresh -h github.com -s workflow  # once, before the first release: gh may need the workflow scope
git tag -a v0.1.0 -m "example 0.1.0" && git push origin v0.1.0
bana daemon poke                 # fetch now: the push hook covers branches, not tags
bana daemon status               # release v0.1.0: building (#57), then: waiting for your answer
bana daemon open                 # the Release card: files, Tested, the notes; Publish v0.1.0 or Not now
```

While it builds, bana writes notes from git: every commit on the first-parent line since the previous release,
as a pull request (a merge or a squash, `Title (#12)`) or as another change. You edit them on the page, or ask
Claude Code in the checkout, where `bana add` registered bana's tools: *write the release notes for v0.1.0*. Claude reads the release and the pull requests, writes notes for the project's users and saves them
for you to review; it cannot publish.

*Publish v0.1.0* checks the files against `SHA256SUMS`, that the tag is still the commit built and that the
previous release is still the one the notes start from, then runs
`gh release create v0.1.0 --verify-tag` with the notes, a `## Tested` section (the CI report's table) and an
`## Install` section, and uploads the files `release.files` names, the installers and `SHA256SUMS`. A tag with a
`-` after its version (`v0.2.0-rc1`) is a prerelease; a hotfix of an older line never becomes Latest. Nothing is
published without your click: not from the menu bar, not by Claude.

People install a release with its installer, from GitHub:

```sh
curl -fsSL https://github.com/OWNER/REPO/releases/download/v0.1.0/install.sh | sh   # macOS, Linux
gh release download v0.1.0 -R OWNER/REPO -p install.sh -O - | sh            # a private repository
irm https://github.com/OWNER/REPO/releases/download/v0.1.0/install.ps1 | iex       # Windows
gh release download v0.1.0 -R OWNER/REPO -p install.ps1 -O - | Out-String | iex    # Windows, a private repository
```

[docs/DAEMON.md](docs/DAEMON.md#releases) has the rest: how the previous release is found, the checks before a
publish, Claude's tools, and the settings that keep Claude from publishing around bana.

## Private code, public CI and releases: bana split

`bana split on`, in the checkout of a project bana added, keeps its code in its private repository and builds it
on a second, public repository's GitHub Actions (free standard runners), where its releases are published too.
The wizard checks first, shows the plan and what becomes public, and takes your typed yes; it makes the public
repository with gh, or opens GitHub's new-repository page filled in. The public repository holds a README and
bana's own workflow, which fetches each pushed commit with a read-only deploy key, builds it with act, and prints
only its steps; the full output is encrypted to a key on your machine, and the daemon writes it to the code's
repository (a commit comment) and to its page, labelled as a remote run with a link to it.

```sh
bana split plan                  # what it would do; changes nothing
bana split on                    # the wizard (bana add --split adds the project and runs it)
bana split ci github|local       # where pushes build; fix rounds always build here
bana split logs private|public   # the public log: steps only, or the whole output too
bana split check                 # the public side as bana left it
bana split off                   # undo: the deploy key first; the repository stays
```

[docs/SPLIT.md](docs/SPLIT.md) has how it works, what is public, and what to check on GitHub before relying on it.

## bana up: a runner pool (optional)

| On | `bana up` makes | Labels |
|---|---|---|
| a Mac | a macOS runner as a LaunchAgent, so it runs in your login session: real CoreAudio, real USB devices | `<prefix>-macos`, `osx-arm64` |
| a Mac | Linux runners in two OrbStack machines: `bana` on the Mac's CPU, `bana-x64` (x86_64 through Rosetta) | `<prefix>-linux`, `<prefix>-systemd`, `linux-arm64` or `linux-x64` |
| Linux | Linux runners as systemd services (a Proxmox VM or container, any Debian/Ubuntu) | `<prefix>-linux`, `<prefix>-systemd`, `linux-<cpu>` |

Every runner also gets `self-hosted` and the machine's name, plus any `--label`. A machine with USB audio devices
adds `usb-audio` and `usb-<vid>-<pid>` for each (see [USB audio](#usb-audio)). `<prefix>` comes from your
bana.conf, so several projects can share one machine without their runners mixing.

A runner runs one job at a time. `--linux N` gives a machine more runners, and each one keeps its own build caches.
Jobs wait in GitHub's queue while no runner is online, so leave the pool (`bana down`) when you stop using it.

On a Mac, OrbStack's command line (`orb`) makes and runs the Linux machines. Without OrbStack, bana uses Lima
for the arm64 machine and makes no x86_64 one. A [Tart VM](#more-machines) is an optional extra.

## bana add: bana as the project's CI

```sh
bana add                         # the report, what it proposes (asked about on a terminal), then CI here
bana add --check                 # the report only, nothing added; exit 1 when a job would have no place here
bana add --diff | git apply      # only the workflow changes, as a patch
bana add --workflow test.yml     # another workflow in .github/workflows
bana add --no-hook --no-claude   # no push hook; bana's tools not registered with Claude Code
```

(`bana add` was `bana init`, which now says so and stops.) In the project's checkout, `bana add` has act
evaluate every job's `runs-on` (matrix entries one by one) and
says where each would run: here, under `bana ci` and the daemon, and in a pool of `bana up` runners. The
workflow is bana.conf's `workflow`, else `ci.yml`, else the only one with `workflow_dispatch`. A job whose
`runs-on` or matrix reads `needs.` is decided at run time. A label
bana does not know is asked about on a terminal (`linux`, `mac`, `skip` or an image) and becomes an
`act.platform.*` key; without a terminal it is proposed as `skip unknown label`. Notes follow, with the file and
line: a matrix whose entries go to different places here, a step that needs systemd, a CPU a container cannot give, `$RUNNER_ENVIRONMENT`
in a script, a checkout `ref:`.

It then proposes `.github/bana.conf` (a new file, or an appended `# bana add DATE` block of the keys it lacks)
and a patch to the workflow: `workflow_dispatch:` in `on:`, the `env.ACT` and `RUNNER_ENVIRONMENT` gates, the
`vars.<PREFIX>_CI_AUTO` gate on pushes and nightlies of root jobs (and of jobs that run after skipped needs,
`always()`), and `runs-on`
a variable can move to a pool, `${{ fromJSON(vars.<PREFIX>_RUNNER_LINUX || '["self-hosted","<prefix>-linux"]') }}`.
It writes bana.conf and runs `git apply` only after you say y, and never on a workflow with uncommitted changes.
No branch, no commit: review with `git diff`. A second run asks only about what is new. It needs act, and
mikefarah's yq (else the one in `act.image`, through Docker); without them it only warns, and goes on.

Then, even off a terminal, it adds the project to the daemon here: it checks what the daemon needs of it (git
reads the repository through gh, `workflow_dispatch`, no pool runners here), writes its settings to
`~/.bana/<prefix>/daemon/settings`, clones it into `~/.bana/<prefix>/src`, adds the push hook and registers
bana's MCP server with Claude Code in this checkout. If the daemon runs, it starts the project at once and
prints its page; otherwise `bana daemon install` starts it. A prefix another repository has here already is
refused. `bana remove` undoes it all but the clone and builds (`--purge`: those too).

## Adopt bana in a project

`bana add` does steps 2 to 4 for you, or shows what they are.

**1. Add bana.** Pin it as a git submodule, so each checkout of your project has the bana it was tested with:

```sh
git submodule add https://github.com/tjrb-xyz/bana tools/bana
git -C tools/bana checkout <commit or tag>
```

Or install it per machine: a release in `~/.local/share/bana`, `bana` on your PATH in `~/.local/bin`, and
`bana upgrade` to update (it checks the release's install.sh against its SHA256SUMS, and moves the daemon first):

```sh
curl -fsSL https://github.com/tjrb-xyz/bana/releases/latest/download/install.sh | sh
curl -fsSL https://raw.githubusercontent.com/tjrb-xyz/bana/main/install.sh | sh -s -- --git   # a git checkout instead
```

The second, bana's own install.sh, takes the newest release too (`sh -s -- vX.Y.Z`: that one), and a git checkout in
`~/.bana/src` with `--git [REF]` or while there is no release yet. A bana it installed in `~/.bana/src` before the
releases moves to them with `bana upgrade`; `~/.bana/src` stays until you remove it.

**2. Write `.github/bana.conf`** (or `bana.conf` at the root). `key = value` lines; `#` starts a comment line.
Every key can be overridden by `BANA_<KEY>` in the environment (`plan.path.rust` is `BANA_PLAN_PATH_RUST`).

| Key | Default | What |
|---|---|---|
| `repo` | git's `origin` | `owner/name`: where runners register, and what the daemon fetches |
| `prefix` | the repository's name | runner names and the `<prefix>-linux` / `<prefix>-macos` labels |
| `labels` | | more labels for every runner |
| `packages.linux` | | Debian packages every Linux machine gets (bana installs the runner's own) |
| `path` | | directories put first on the runners' and the daemon's PATH (`~/.cargo/bin`) |
| `hook.mac` | | a script run on a Mac before its macOS runner registers |
| `hook.linux` | | a script run in each Linux machine, as the runners' user, before they register |
| `vm`, `vm_x64` | `bana`, `bana-x64` | the OrbStack machines on a Mac (shared by projects) |
| `linux_user` | `bana` | who runners run as when `bana up` starts as root |
| `runner_version` | the newest | an actions/runner version to pin |
| `workflow`, `tiers`, `tier_input` | `ci.yml`, `quick nightly release`, `tier` | the workflow `bana ci`, the daemon and the manager's *Start a run* run, and its tier input |
| `act.image` | `catthehacker/ubuntu:act-24.04` | the image `bana ci` runs Linux jobs in |
| `act.network` | `bridge` | the Docker network of Linux jobs: `bridge` gives each job a localhost of its own, as on GitHub; `host` (act's default) shares the Docker host's between all of them |
| `act.args` | | more act options for `bana ci` and the daemon's builds (`--reuse`) |
| `act.platform.<label>` | see [bana ci](#bana-ci-the-workflow-on-this-machine) | where `bana ci` and the daemon run jobs with this runs-on label: `linux`, `mac`, `machine`, `skip [reason]` or an image (`bana add` writes these) |
| `act.docker_config` | `~/.bana/docker` | the Docker config act pulls with: bana's own, without your logins, so macOS never asks for your Keychain password; `~/.docker` for private images |
| `ci.log` | `yes` | `bana ci` keeps act's output in `~/.bana/<prefix>/ci/last.log` for `bana fix`, through a pipe, so Linux jobs print without colours; `no` gives act your terminal, and keeps nothing |
| `daemon.*` | | which pushes the daemon builds, and how ([docs/DAEMON.md](docs/DAEMON.md#settings)) |
| `fix.*` | | a fix's rounds and their token ([docs/DAEMON.md](docs/DAEMON.md#settings)), and `bana fix --headless`'s limits ([docs/FIX.md](docs/FIX.md#headless)) |
| `keep`, `keep_max_gb` | , `0` | what `keep-builds` keeps (git clean `-e` patterns), and the size that starts one over |
| `plan.*` | | how `plan` picks jobs ([Tiers and plan](#tiers-and-plan)) |
| `tart_name` | `bana-tart` | the Tart VM `bana tart up` makes ([More machines](#more-machines)) |

Hooks are paths relative to bana.conf. They get `BANA_DEDICATED`, `BANA_PREFIX`, `BANA_REPO` and
`BANA_MACHINE`, and a failing hook stops `up`.

**3. Point your jobs at your machines:**

```yaml
jobs:
  test:
    runs-on: [self-hosted, myproj-linux]         # any Linux runner
  mac:
    runs-on: [self-hosted, myproj-macos]
  build-x64:
    runs-on: [self-hosted, myproj-linux, linux-x64]
  hardware:
    runs-on: [self-hosted, myproj-linux, usb-1c75-af70]   # the machine that holds this device
```

Jobs see `BANA_MACHINE` and, on a machine joined with `--dedicated`, `BANA_DEDICATED=1`: use it to guard
invasive tests (installing drivers, restarting services) that should not run on someone's laptop.

Under `bana ci` and the daemon, act maps `<prefix>-linux` to a container and `<prefix>-macos` to the Mac, and a
second act runs `<prefix>-systemd` jobs in the Mac's Linux machine; the other labels matter only to a pool. To move jobs between the daemon's machines and a pool without editing the
workflow, take `runs-on` from a variable, as the worked example does:

```yaml
    runs-on: ${{ fromJSON(vars.MYPROJ_RUNNER_LINUX || '["self-hosted","myproj-linux"]') }}
```

**4. Run it:** `tools/bana/bin/bana ci` in the project's checkout. For CI on push, `bana daemon install` once a
machine, then `tools/bana/bin/bana add` in the checkout. For a pool instead, `tools/bana/bin/bana up` on each
machine ([Commands](#commands)).

### Reusable workflow pieces

Two composite actions, for workflows that want them:

```yaml
    steps:
      - uses: actions/checkout@v4
        with:
          clean: ${{ runner.environment != 'self-hosted' }}   # keep-builds cleans instead
      - uses: tjrb-xyz/bana/actions/keep-builds@<commit>      # keeps bana.conf's `keep`
      - id: plan
        uses: tjrb-xyz/bana/actions/plan@<commit>
        with:
          tier: ${{ inputs.tier || 'quick' }}
    # a job output: ${{ fromJSON(steps.plan.outputs.json).rust }}
```

Without actions, run the same commands in a step: `tools/bana/bin/bana changed "$BEFORE" "$BRANCH" |
tools/bana/bin/bana plan "$TIER" >> "$GITHUB_OUTPUT"`. That needs the submodule checked out in the job.

### Tiers and plan

`bana plan TIER` reads the files a push changed (from `bana changed`) and prints `key=true|false` lines for
`$GITHUB_OUTPUT` (or one JSON object with `--json`):

```ini
tiers = quick nightly release
plan.full_tiers = nightly release        # these run every path job (default: all tiers but the first)
plan.everything = ^(\.github/workflows/|Cargo\.lock$)   # a change here runs every path job
plan.path.rust = ^(crates/|Cargo\.toml$) # rust=true when a changed file matches
plan.path.web = ^web/
plan.tier.package = nightly release      # package=true on these tiers only
```

When the changes cannot be told (a new branch with no common history), every path job runs.

## The worked example

`example` is bana's first user: a Rust and web project with a macOS audio driver, Linux services and packages
for several CPUs. [examples/example](examples/example) has its `.github/bana.conf` and the two hooks beside it,
and `ci.yml`, its workflow trimmed to where the jobs run (`bana add`'s tests run on it). The mac hook installs
Homebrew's SCons, ragel and CMake and the project's audio driver, and checks for rustup. The linux hook installs
rustup and checks that sudo does not ask. Its `plan.*` keys are its path rules. Its `daemon.*` keys build every
branch but the bots', at `quick`, and `v*` tags at `release`, which it publishes from the daemon's page. Its `report.*` keys are its CI report's standards: toolchain,
rust, engine (the `real_*` tests), web, macos, streaming, sdk, linux_service and packaging. Its `install.*` keys
make its installer per-user, with `install-hook.sh` (shipped in each archive) asking before it installs the Mac's
audio devices with sudo.

On a Mac, `bana add --check` finds a place for every job of it, and still exits 1: `package`'s matrix splits
over machines (the Linux targets in containers, `osx-arm64` on the Mac itself), and act runs every Linux
container at one CPU, so a run there builds only the arm64 targets. Its notes also name `background-linux`,
which needs systemd that act's containers do not have, and a `$RUNNER_ENVIRONMENT` check in a script.

### CI on push for the example

The daemon on a Mac is the example's push CI, and the pool is one variable away. These are the changes its
workflow made:

1. `on:` keeps `push`, `schedule` and tags, and the `plan` job, which every other job needs, runs only
   `if: vars.EXAMPLE_CI_AUTO != 'false' || github.event_name == 'workflow_dispatch'`. With
   `EXAMPLE_CI_AUTO=false` set on GitHub, GitHub queues nothing; the daemon's builds are `workflow_dispatch`
   under act, so they always run.
2. No checkout has a `ref:`: every job tests the run's own commit. Any `ref:` makes act clone from GitHub
   instead of building the pushed commit.
3. The three `if: runner.environment == 'self-hosted' || ...` gates in the macos job get `|| env.ACT == 'true'`,
   or the real-CoreAudio device, mix and cpal checks are skipped under bana. Every checkout has
   `clean: ${{ runner.environment != 'self-hosted' && env.ACT != 'true' }}`. `BANA_DEDICATED` stays unset on
   the laptop, so the driver install and LaunchAgent tests stay off.
4. The macos job's last step, `if: always()`, removes the `example mix · CI` aggregate device, so a cancel or a
   failure does not leave it on the Mac:
   `[[ -x target/release/example-coreaudio ]] && printf '{"id":1,"op":"destroy_aggregate","uid":"xyz.tjrb.example.agg.mix.ci"}\n' | target/release/example-coreaudio || true`.
5. `plan` uses bana's plan action (`tjrb-xyz/bana/actions/plan@<commit>`), whose `changed` falls back to
   `refs/remotes/origin/<branch>` when act's containers cannot fetch, and the jobs start with `keep-builds`.
6. Jobs that share a machine keep apart: pnpm/action-setup installs into `${{ runner.temp }}`, the end-to-end
   tests take a free port, and `background-linux`, which installs the user's one service, takes a per-user
   `flock`.
7. `package` checks the machine's CPU is its target's. Under act a target of another CPU is left out with a
   notice rather than built mislabelled; on a pool a mismatch fails the job.
8. `.github/bana.conf` has the `daemon.*` keys, with `daemon.tags = v*`, and `install.*` and `release.files`
   for its releases.

On the MacBook, in the example's checkout:

```sh
brew install act gh yq                       # and OrbStack, running; rustup is there already
gh auth login
curl -fsSL https://github.com/tjrb-xyz/bana/releases/latest/download/install.sh | sh   # bana on the PATH
bana add --check                             # every job has a place; exits 1 for package's SPLIT matrix
bana ci nightly                              # every job once, by hand; fix what fails
bana daemon install                          # once on the MacBook
bana add                                     # its warnings name what is left in ci.yml
gh variable set EXAMPLE_CI_AUTO --body false # GitHub queues nothing for a pool not there
git push                                     # 🧱 4m in the menu bar, then bana on the commit
```

A pull request's checks then show `bana` and `bana/<job>` from the MacBook. A nightly runs from the page:
*Run now*, a branch at `nightly`, which posts `bana nightly`.

Its first release, as a prerelease to try the whole path once:

```sh
gh auth refresh -h github.com -s workflow
git tag -a v0.1.0-rc1 -m "example 0.1.0-rc1" && git push origin v0.1.0-rc1 && bana daemon poke
# 🧱 v0.1.0-rc1? when the release build passes: the notes (or ask Claude Code for better ones), then Publish
gh release view v0.1.0-rc1 -R tjrb-xyz/example   # its files, the installers and SHA256SUMS
gh release download v0.1.0-rc1 -R tjrb-xyz/example -p install.sh -O - | sh   # on the Mac, and on a Linux arm64 machine
```

### A runner pool for the example instead

If the example goes back to a pool (`bana up` on each machine), `EXAMPLE_CI_AUTO` goes (`gh variable delete
EXAMPLE_CI_AUTO`) and so does its CI on the daemon (`bana remove example`): the two must not both take its
pushes. The workflow stays as it is. Its jobs run on

```yaml
    runs-on: ${{ fromJSON(vars.EXAMPLE_RUNNER_LINUX || '["self-hosted","example-linux"]') }}
```

and `[self-hosted, example-macos]` likewise, and packaging per CPU on `[self-hosted, example-linux, linux-x64]`
and so on: bana makes the same labels. `EXAMPLE_RUNNER_LINUX` and `EXAMPLE_RUNNER_MACOS` (JSON label lists)
send the jobs elsewhere without a change to the workflow.

## Commands

```sh
bana ci [TIER] [-j JOB] [--x64] [--list] [--event FILE] [-- ACT-OPTIONS]
bana fix [BUILD | last | --log FILE|-] [--open | --headless]   # and brief, list, push, drop: docs/FIX.md
bana report [BUILD | last | --log FILE|-] [--json]   # the CI report, per standard
bana installer DIST (--tag T | --label L)   # the project's install.sh, install.ps1 and SHA256SUMS
bana install [BUILD | --from FILE|DIR]     # a daemon build's files, on this machine
bana mcp             # bana's tools for Claude Code, by hand (an MCP server on stdio)
bana daemon install [--port N] [--no-tray | --tray] [--no-open] [--now]   # once a machine
bana daemon status|log|open|poke|run|uninstall   # docs/DAEMON.md
bana add [--check] [--diff] [--workflow F] [--no-hook] [--no-claude] [--split[=R]]   # a project's CI here
bana list            # the projects added here
bana remove [PROJECT] [--purge]   # a project's CI here goes
bana pause|resume [PROJECT]       # hold its automatic builds, or build them again
bana split [on | off | plan | check | ci github|local | logs private|public | sync | rekey]   # docs/SPLIT.md
bana up [--linux N] [--x64 N] [--no-mac] [--dedicated] [--label L] [--no-usb] [--token T]
bana status          # this machine's runners and USB audio devices, and the pool
bana usb             # the USB audio devices here, and the labels they give
bana relabel         # update runner labels after plugging a device in or out
bana start|stop NAME # one runner
bana down            # this machine's runners leave the pool
bana manager         # the page, at http://127.0.0.1:8470/#token=… (the daemon's, when it runs)
bana settings        # the settings in effect
bana version         # this bana (a release's, or a checkout's commit), and the daemon's if other
bana upgrade [vX.Y.Z] [--check] [--now] [--yes]   # the newest release, the daemon first (a downgrade asks)
```

Run them from the project's checkout: that is where bana finds bana.conf. `bana daemon`, `bana list`, and
`remove`, `pause` and `resume` given a PROJECT run anywhere. `bana up` again is safe: registered
runners stay, and their labels are brought up to date.

**Tokens.** With the GitHub CLI signed in as a repository admin (`gh auth login`) nothing else is needed.
Otherwise pass `--token`: *Settings → Actions → Runners → New self-hosted runner*, the value after `--token`
(it is valid for an hour).

## The manager

The manager is the pool's page. While the daemon runs, `bana manager` opens the daemon's page, which
has the same sections under *Runner pool (optional)*. Otherwise it starts the release's manager (in a git
checkout of bana, cargo builds it: a minute the first time) and a page on `http://127.0.0.1:8470`:

- **this machine's runners**: *waiting for a job* (the listener runs), *running a job* (a worker runs too) or
  *stopped*, with Start and Stop; and its USB audio devices with their labels;
- **the pool**: every runner GitHub knows, online or not, and the job each busy one runs;
- **runs**: the latest ten, each job with the runner it landed on; *Start a run* and *Cancel*;
- **tasks**: what the page started, with its output.

*Join the pool* runs `bana up` without a terminal, so a hook that asks for a password (the example's driver install)
has to run once from a terminal first. It serves loopback only, behind a token kept in `~/.bana/manager-token`, and runs only `bana` and `gh`, with
arguments it checks, one runner change at a time. To see another machine's page:
`ssh -L 8471:127.0.0.1:8470 mac-mini.local`, then open `http://127.0.0.1:8471/#token=…` with that machine's token.

## More machines

- **A Mac mini for CI:** `bana up --dedicated`.
- **Proxmox:** a VM, or a container with *nesting* on (runners are systemd services), from Debian 13 or Ubuntu
  24.04. In it, clone your project and run `bana up`. Entered as root, bana makes a `bana` user for the runners
  (in the `audio` group). An x86_64 PC gives native `linux-x64` runners.
- **A Tart VM on a Mac** (optional; OrbStack is the default): `bana tart up` makes a Debian VM with
  [Tart](https://github.com/openai/tart), keeps it running with a LaunchAgent and joins it. `bana tart help` has
  the rest. Tart has no USB passthrough.

A Mac's Linux machines stop with the Mac. OrbStack → Settings → *Start at login* brings them back, and the
runners start with them.

## USB audio

`bana up` finds USB audio devices and labels the machine's first runner with `usb-audio` and
`usb-<vid>-<pid>` for each one (only the first runner, so two jobs never use a device at once). On a Mac it
reads the I/O Registry; on Linux, ALSA's USB cards whose `/dev/snd` node is present, so a container claims only
the devices it was given. On a Linux machine, `up` also makes sure the kernel has the USB audio driver (Debian's
cloud kernels and Ubuntu's virtual ones do not). [docs/USB.md](docs/USB.md) covers passing devices through on Proxmox, and why a Mac's
own devices reach only its macOS runner.

## Worth knowing

- **A runner runs whatever the workflow says, as its user.** So does the daemon, as you. Use bana with private
  repositories, or at least never let pull requests from forks run on these runners (the daemon never builds
  them).
- **Billing.** The daemon and self-hosted runners use no Actions minutes. An account GitHub has locked for a
  payment problem runs no Actions at all, self-hosted included.
- **Leave the pool** with `bana down`, or *Leave the pool…* in the manager. The OrbStack machines stay for
  other projects; `orb delete -f bana bana-x64` removes them.

## bana's own tests

`tests/run.sh` runs every command against stand-ins for the programs bana drives (`uname`, `orb`, `tart`,
`gh`, `ioreg`, `sudo`, `apt-get`, and the runner's own scripts), so the macOS paths run on Linux too.
`BASH_UNDER_TEST=/bin/bash` picks the shell under test. `tests/install.sh` runs the compiled installer (`SH=dash`,
bash, bash 3.2) through every stage of example's hook, `tests/install-ps1.sh` runs install.ps1 on pwsh, and
`tests/install-real.sh` runs install.sh on the machine's own tools (on a Mac: sysctl, BSD tar, /sbin/sha256sum and a
quarantined download). `.github/workflows/test.yml` runs them on Ubuntu and on
macOS (stock bash 3.2), plus shellcheck and the manager's tests. `.github/workflows/installer.yml` runs install.ps1 on Windows (PowerShell 5.1 and 7:
the junction, the user PATH, Unblock-File, the hook under a Restricted policy), only when the installer changes.
`BANA_E2E=1 tests/e2e-daemon.sh` runs the daemon with real act: a push uploads an archive per CPU, and the
green build's installer installs one under a scratch home; then a second project is added while it runs, paused,
resumed, and outlives the removal of the first.

## Releasing bana

1. `.github/release.sh bump X.Y.Z` sets the version in bin/bana and the manager; a pull request, merged.
2. *Actions → release → Run workflow* with `X.Y.Z`: a dry run. It runs the tests, builds bana-manager for
   linux-x64, linux-arm64, macos-arm64 and macos-x64, packs them, installs the release on Linux and a Mac, and
   leaves the files as the artifact `bana-vX.Y.Z-dist`. It never publishes.
3. `git tag vX.Y.Z && git push origin vX.Y.Z` (a commit on main) runs the same and publishes, through the
   `release` environment (*Settings → Environments* can require a reviewer). A `-` makes a prerelease.
4. A bad release: `gh release delete vX.Y.Z`, and people go back with `bana upgrade vPREV`.

License: GPL-3.0-only.

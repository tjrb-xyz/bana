# bana

bana runs a project's GitHub Actions workflow on your own machines, for CI that is too long, too big or too
hardware-bound for GitHub's runners. It is small on purpose, and it works in three ways:

- **`bana daemon`: CI on push, on your Mac.** A daemon fetches the repository, runs the workflow with
  [act](https://github.com/nektos/act) for each eligible push, and posts commit statuses to GitHub. 🧱 in the
  menu bar shows what it is doing, and a click opens its page with the build's live log. It keeps nothing
  awake while idle, and a push made while the Mac sleeps builds when it wakes.
- **`bana ci`: the same, by hand.** It runs the jobs with act in OrbStack's Docker: Linux jobs in containers,
  and on a Mac the macOS jobs on the Mac itself, with its real CoreAudio and USB devices. Nothing stays running.
  Use it before you push, and before you install the daemon.
- **`bana up`: a runner pool, if you want it.** Your machines register as self-hosted runners and GitHub gives
  them the jobs from each push. That needs them awake and online, so it is opt-in, and `bana down` undoes it.
  A project uses the daemon or the pool, not both.

It runs on macOS (Apple silicon, the stock bash 3.2 and BSD tools, Xcode's command line tools) and on Debian or
Ubuntu. It was extracted from [dsper](https://github.com/tjrb-xyz/dsper), which is its first user and the
worked example below.

## CI on push: bana daemon

In the project's checkout, with act, OrbStack (running), the GitHub CLI signed in and Rust (cargo builds the
daemon the first time):

```sh
brew install act gh              # and OrbStack, for Docker
gh auth login
bana ci                          # once by hand: the workflow works under act
bana daemon install              # check, build, start; on a Mac it opens the page
git push                         # builds on this Mac
```

Install checks the machine and warns about what in the workflow would go wrong under the daemon
([below](#what-the-workflow-needs)). It builds in its own clone, `~/.bana/<prefix>/src`, never in your
checkout. It starts from the branches as they are: the next push builds. Its settings (bana.conf's `daemon.*`
keys) are read at install, so run it again after changing them.

**On GitHub** each build posts the statuses `bana` for the build and `bana/<job>` for each job that starts:
`running on mbp (quick)`, then `passed on mbp in 12m · 5 jobs` or `rust failed at "cargo clippy" · 3m40s on
mbp`. A nightly run by hand posts `bana nightly` instead. The *Details* link opens the build on the daemon's
page, at `http://127.0.0.1:8470/`, so it works only on the machine that ran it.

**In the menu bar**, 🧱 alone is idle; `🧱 4m +2` is building for 4 minutes with 2 queued; `🧱 !` means the
last build failed; `🧱 paused`, `🧱 no Docker` (start OrbStack), `🧱 busy` (waiting for your `bana ci`) and
`🧱 !gh` (statuses not posted) say why builds wait. A left click opens the page; a right click has *Cancel build*,
*Pause new builds* and *Quit bana*, which stops local CI until the next login.

**The page**, *Local CI*, has the queue, the running build with its jobs and live log, and the history. *Run now*
builds a branch at any tier, which is how a nightly runs. *Pause new builds* lets the running build finish;
*Cancel* stops one; *Re-run* builds the same commit again.

**Which pushes run:** branches matching `daemon.branches` (all but `dependabot/*` and `renovate/*`), tags
matching `daemon.tags` (none by default), and not a head commit with `[skip ci]` or another of GitHub's skip
markers. The rules come from the install, not from the pushed commit. A commit already built at that tier does
not run again. A branch keeps one queued build, and a newer push replaces it; a running build finishes.

**One build at a time.** The daemon and `bana ci` share a lock: while the daemon builds, `bana ci` stops with
*act is busy here*, and while your `bana ci` runs, the daemon's builds wait.

**Sleep.** Nothing keeps the Mac awake while idle. While act runs, `caffeinate` stops idle sleep; closing the lid
still sleeps the Mac, and the build pauses until it wakes. After a wake the next fetch comes within 30 seconds
and queues one build for each branch that moved. A build cut short by a crash or a reboot runs again once, if
its branch has not moved.

**Trust.** A push runs its workflow as you on your Mac, macOS jobs outside any container, with your home, SSH
keys and Keychain in reach. Use the daemon only on a private repository, where write access is the gate. Jobs
get the GitHub CLI's token as `GITHUB_TOKEN` (`daemon.token = none`: an empty one).

```sh
bana daemon status               # whether it runs, what it builds, what waits
bana daemon log                  # its log (~/Library/Logs/bana/<prefix>.log on a Mac)
bana daemon open                 # the page
bana daemon poke                 # fetch now
bana daemon uninstall [--purge]  # stop it; --purge also removes its clone and builds
```

[docs/DAEMON.md](docs/DAEMON.md) has the rest: every setting, how cancels and timeouts work, the files it keeps,
running it on Linux, and the trust boundary in full.

### What the workflow needs

The daemon runs the workflow as `workflow_dispatch`, with an event shaped like the push (`github.ref`,
`github.sha`, `github.event.before` and the tier input as a push would have them). Install warns about each of
these:

- `on:` keeps only `workflow_dispatch`, so GitHub itself runs nothing on a push. With `push` too and no runner
  pool, GitHub queues self-hosted jobs that no runner ever takes.
- No checkout `ref:`: act builds the pushed commit only for a checkout without one.
- A step gated on `runner.environment == 'self-hosted'` also needs `|| env.ACT == 'true'`: act never sets
  `runner.environment`, so the step would be skipped.

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
| `<prefix>-linux`, `ubuntu-*` | a container from `act.image` (default `catthehacker/ubuntu:act-24.04`) |
| `<prefix>-macos`, `macos-latest` | on a Mac, on the Mac itself (act's host mode, in a copy of your working tree); elsewhere skipped |

The run uses your working tree, uncommitted changes included, and the workflow's tier input (`tiers`,
`tier_input` in bana.conf). Artifacts land in `~/.bana/act/artifacts`. With the GitHub CLI signed in, jobs get
its token as `GITHUB_TOKEN`.

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

## bana up: a runner pool (optional)

| On | `bana up` makes | Labels |
|---|---|---|
| a Mac | a macOS runner as a LaunchAgent, so it runs in your login session: real CoreAudio, real USB devices | `<prefix>-macos`, `osx-arm64` |
| a Mac | Linux runners in two OrbStack machines: `bana` on the Mac's CPU, `bana-x64` (x86_64 through Rosetta) | `<prefix>-linux`, `linux-arm64` or `linux-x64` |
| Linux | Linux runners as systemd services (a Proxmox VM or container, any Debian/Ubuntu) | `<prefix>-linux`, `linux-<cpu>` |

Every runner also gets `self-hosted` and the machine's name, plus any `--label`. A machine with USB audio devices
adds `usb-audio` and `usb-<vid>-<pid>` for each (see [USB audio](#usb-audio)). `<prefix>` comes from your
bana.conf, so several projects can share one machine without their runners mixing.

A runner runs one job at a time. `--linux N` gives a machine more runners, and each one keeps its own build caches.
Jobs wait in GitHub's queue while no runner is online, so leave the pool (`bana down`) when you stop using it.

On a Mac, OrbStack's command line (`orb`) makes and runs the Linux machines. Without OrbStack, bana uses Lima
for the arm64 machine and makes no x86_64 one. A [Tart VM](#more-machines) is an optional extra.

## Adopt bana in a project

**1. Add bana.** Pin it as a git submodule, so each checkout of your project has the bana it was tested with:

```sh
git submodule add https://github.com/tjrb-xyz/bana tools/bana
git -C tools/bana checkout <commit or tag>
```

Other ways: `sh install.sh [REF]` puts a pinned checkout in `~/.bana/src` and `bana` on your PATH, per machine.
`install.sh` also works as `curl … | sh` once bana is public; while it is private, clone it with
`gh repo clone tjrb-xyz/bana ~/.bana/src` and run `~/.bana/src/install.sh`.

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
| `act.args` | | more act options for `bana ci` and the daemon's builds (`--reuse`) |
| `daemon.*` | | which pushes the daemon builds, and how ([docs/DAEMON.md](docs/DAEMON.md#settings)) |
| `keep`, `keep_max_gb` | , `0` | what `keep-builds` keeps (git clean `-e` patterns), and the size that starts one over |
| `plan.*` | | how `plan` picks jobs ([Tiers and plan](#tiers-and-plan)) |

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

Under `bana ci` and the daemon, act maps `<prefix>-linux` to a container and `<prefix>-macos` to the Mac; the
other labels matter only to a pool.

**4. Run it:** `tools/bana/bin/bana ci` in the project's checkout, then `tools/bana/bin/bana daemon install` for CI
on push. For a pool instead, `tools/bana/bin/bana up` on each machine ([Commands](#commands)).

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

bana is a private repository, so its actions work in other repositories only after bana's
**Settings → Actions → General → Access** allows repositories owned by `tjrb-xyz`. Then no token is needed.
Without actions, run the same commands in a step: `tools/bana/bin/bana changed "$BEFORE" "$BRANCH" |
tools/bana/bin/bana plan "$TIER" >> "$GITHUB_OUTPUT"`. That needs the submodule checked out in the job, which a
private submodule does not allow with the default token.

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

## dsper, the worked example

dsper keeps [examples/dsper/bana.conf](examples/dsper/bana.conf) as `.github/bana.conf`, with its two hooks
beside it. The mac hook installs Homebrew's scons and dsper's audio driver and checks for rustup. The linux hook
installs rustup. Its `plan.*` keys are the path rules that were in `scripts/ci.sh`, and `plan` gives the same
outputs as before. Its `daemon.*` keys build every branch but the bots', at `quick`, and no tags yet.

### CI on push for dsper

bana does not push to dsper. These are the changes dsper's maintainer makes, in a dsper branch, for the daemon
to be dsper's push CI:

1. `.github/workflows/ci.yml`: `on:` keeps only `workflow_dispatch` with its `tier` input; `push` and
   `schedule` go, and so does the `concurrency:` block (act ignores it; the daemon builds one at a time and
   replaces queued pushes). Nightly runs from the page's *Run now* for now.
2. Every checkout `ref:` goes: the eight `vars.DSPER_NIGHTLY_REF` lines, and `package`'s
   `ref: ${{ needs.plan.outputs.commit }}`. Any `ref:` makes act clone from GitHub instead of building the
   pushed commit.
3. The three `if: runner.environment == 'self-hosted' || ...` gates in the macos job get `|| env.ACT == 'true'`,
   or the real-CoreAudio device, mix and cpal checks are skipped under bana. `DSPER_CI_DEDICATED` stays unset on
   the laptop, so the driver install and LaunchAgent tests stay off.
4. The macos job gets a last step, `if: always() && env.ACT == 'true'`, that removes the `dsper mix · CI`
   aggregate device, so a cancel or a failure does not leave it on the Mac:
   `[[ -x target/release/dsper-coreaudio ]] && printf '{"id":1,"op":"destroy_aggregate","uid":"xyz.tjrb.dsper.agg.mix.ci"}\n' | target/release/dsper-coreaudio || true`.
5. `scripts/ci.sh changed`: when `git fetch origin "$branch"` fails (act's containers have no credentials), it
   falls back to `refs/remotes/origin/$branch`, as bana's `changed` does. Otherwise a new branch plans every job.
6. If Linux builds turn out too slow and `act.args = --reuse` goes in: `scripts/ci.sh keep-builds` cleans under
   act when `BANA_DAEMON=1`, as bana's does.
7. `.github/bana.conf` gets the example's `daemon.*` keys.
8. `docs/CI.md`'s "CI on your Mac" becomes `bana daemon install`, then push: where the statuses and logs are,
   what the menu bar shows, and that the pool is the optional path.

Only dsper's plan job has run under act so far. The first full run will likely need fixes in the rust, web
(pnpm, setup-node's cache, Playwright), streaming and sdk jobs, which is why `bana ci nightly` runs by hand once
before the daemon is installed.

On the MacBook, in dsper's checkout, on that branch:

```sh
brew install act gh                          # and OrbStack, running; rustup is there already
gh auth login
gh repo clone tjrb-xyz/bana ~/.bana/src && ~/.bana/src/install.sh   # bana on the PATH
cp ~/.bana/src/examples/dsper/bana.conf .github/bana.conf
bana ci nightly                              # every job once, by hand; fix what fails
bana daemon install                          # its warnings name what is left in ci.yml
git push                                     # 🧱 4m in the menu bar, then bana on the commit
```

A pull request's checks then show `bana` and `bana/<job>` from the MacBook. A nightly runs from the page:
*Run now*, a branch at `nightly`, which posts `bana nightly`.

### A runner pool for dsper instead

If dsper goes back to a pool (`bana up`), `on:` gets its `push` trigger back and the daemon goes
(`bana daemon uninstall`): the two must not both take its pushes. Its workflow then changes little:

- `runs-on` stays `[self-hosted, dsper-linux]` and `[self-hosted, dsper-macos]`, and packaging per CPU stays
  `[self-hosted, dsper-linux, linux-x64]` and so on: bana makes the same labels.
- `scripts/ci.sh keep-builds` becomes `tjrb-xyz/bana/actions/keep-builds@<commit>`, and `plan`'s step pipes
  `changed` into `plan` as before (or uses the plan action, with `fromJSON` for each output).
- `DSPER_CI_DEDICATED` becomes `BANA_DEDICATED`.
- `camilladsp`, `package` and `nightly-done` stay in dsper's `scripts/ci.sh`.

Moving a machine over: `scripts/ci-runner.sh down` with the old script (it removes the old runners and their
`~/.dsper-ci`), then `git submodule update --init` and `tools/bana/bin/bana up`. The old OrbStack machines
`dsper-ci` and `dsper-ci-x64` can go (`orb delete -f dsper-ci dsper-ci-x64`), or be kept with
`vm = dsper-ci` and `vm_x64 = dsper-ci-x64` in bana.conf.

## Commands

```sh
bana ci [TIER] [-j JOB] [--x64] [--list] [--event FILE] [-- ACT-OPTIONS]
bana daemon install [--port N] [--no-tray] [--no-open] [--now]
bana daemon status|log|open|poke|run|uninstall   # docs/DAEMON.md
bana up [--linux N] [--x64 N] [--no-mac] [--dedicated] [--label L] [--no-usb] [--token T]
bana status          # this machine's runners and USB audio devices, and the pool
bana usb             # the USB audio devices here, and the labels they give
bana relabel         # update runner labels after plugging a device in or out
bana start|stop NAME # one runner
bana down            # this machine's runners leave the pool
bana manager         # the page, at http://127.0.0.1:8470/#token=… (the daemon's, when it runs)
bana settings        # the settings in effect
```

Run them from the project's checkout: that is where bana finds bana.conf. `bana up` again is safe: registered
runners stay, and their labels are brought up to date.

**Tokens.** With the GitHub CLI signed in as a repository admin (`gh auth login`) nothing else is needed.
Otherwise pass `--token`: *Settings → Actions → Runners → New self-hosted runner*, the value after `--token`
(it is valid for an hour).

## The manager

The manager is the pool's page. While the project's daemon runs, `bana manager` opens the daemon's page, which
has the same sections under *Runner pool (optional)*. Otherwise it builds the manager with cargo (a minute the
first time) and starts a page on `http://127.0.0.1:8470`:

- **this machine's runners**: *waiting for a job* (the listener runs), *running a job* (a worker runs too) or
  *stopped*, with Start and Stop; and its USB audio devices with their labels;
- **the pool**: every runner GitHub knows, online or not, and the job each busy one runs;
- **runs**: the latest ten, each job with the runner it landed on; *Start a run* and *Cancel*;
- **tasks**: what the page started, with its output.

*Join the pool* runs `bana up` without a terminal, so a hook that asks for a password (dsper's driver install)
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
`BASH_UNDER_TEST=/bin/bash` picks the shell under test. `.github/workflows/test.yml` runs it on Ubuntu and on
macOS (stock bash 3.2), plus shellcheck and the manager's tests. On a private repository the macOS job's
minutes count ten times.

License: GPL-3.0-only, as dsper.

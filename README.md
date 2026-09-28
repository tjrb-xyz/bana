# bana

bana runs a project's GitHub Actions workflow on your own machines, for CI that is too long, too big or too
hardware-bound for GitHub's runners. It is small on purpose, and it works in two ways:

- **`bana ci`: the workflow here, when you ask for it.** It runs the jobs with
  [act](https://github.com/nektos/act) in OrbStack's Docker: Linux jobs in containers, and on a Mac the macOS jobs
  on the Mac itself, with its real CoreAudio and USB devices. Nothing stays running, and no machine has to stay
  awake. Use it before you push.
- **`bana up`: a runner pool, if you want it.** Your machines register as self-hosted runners and GitHub gives
  them the jobs from each push. That needs them awake and online, so it is opt-in, and `bana down` undoes it.

It runs on macOS (Apple silicon, the stock bash 3.2 and BSD tools, Xcode's command line tools) and on Debian or
Ubuntu. It was extracted from [dsper](https://github.com/tjrb-xyz/dsper), which is its first user and the
worked example below.

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
| `<prefix>-macos`, `macos-latest` | on a Mac, on the Mac itself (act's host mode, in your working tree); elsewhere skipped |

The run uses your working tree, uncommitted changes included, and the workflow's tier input (`tiers`,
`tier_input` in bana.conf). Artifacts land in `~/.bana/act/artifacts`. With the GitHub CLI signed in, jobs get
its token as `GITHUB_TOKEN`.

Limits worth knowing:
- act uses your working tree only for a checkout step without `ref:` (or with `ref:` equal to the current ref).
  A checkout with any other `ref:` clones from GitHub instead.
- act runs every Linux container at one architecture per run, so a matrix over CPUs needs `bana ci` and
  `bana ci --x64`.
- act reimplements GitHub's runner. Most actions work, but it is not bit-for-bit GitHub.
- On a Mac, macOS jobs run in your working tree, as you. bana's `keep-builds` does nothing under act, so it never
  cleans your checkout.

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
| `repo` | git's `origin` | `owner/name`: where runners register |
| `prefix` | the repository's name | runner names and the `<prefix>-linux` / `<prefix>-macos` labels |
| `labels` | | more labels for every runner |
| `packages.linux` | | Debian packages every Linux machine gets (bana installs the runner's own) |
| `path` | | directories put first on the runners' PATH (`~/.cargo/bin`) |
| `hook.mac` | | a script run on a Mac before its macOS runner registers |
| `hook.linux` | | a script run in each Linux machine, as the runners' user, before they register |
| `vm`, `vm_x64` | `bana`, `bana-x64` | the OrbStack machines on a Mac (shared by projects) |
| `linux_user` | `bana` | who runners run as when `bana up` starts as root |
| `runner_version` | the newest | an actions/runner version to pin |
| `workflow`, `tiers`, `tier_input` | `ci.yml`, `quick nightly release`, `tier` | the workflow `bana ci` and the manager's *Start a run* run, and its tier input |
| `act.image` | `catthehacker/ubuntu:act-24.04` | the image `bana ci` runs Linux jobs in |
| `keep`, `keep_max_gb` | , `0` | what `keep-builds` keeps (git clean `-e` patterns), and the size that starts one over |
| `plan.*` | | how `plan` picks jobs ([Tiers and plan](#tiers-and-plan)) |

Hooks are paths relative to bana.conf. They get `BANA_DEDICATED`, `BANA_PREFIX`, `BANA_REPO` and
`BANA_MACHINE`, and a failing hook stops `up`.

**3. Point your jobs at the pool:**

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

**4. Run it:** `tools/bana/bin/bana ci` in the project's checkout. For a pool, `tools/bana/bin/bana up` on each
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
outputs as before.

To run dsper's CI locally with `bana ci`, its workflow needs one change: its checkouts pass
`ref: ${{ github.event_name == 'schedule' && vars.DSPER_NIGHTLY_REF || '' }}`, and act clones from GitHub for
any checkout with a `ref:`. Dropping those lines (and with them `DSPER_NIGHTLY_REF`) lets act use the working
tree; dsper's `plan` job then runs under `bana ci` as it does on GitHub. If dsper uses no runner pool, its
push trigger only queues jobs that no runner takes, so `on:` could keep just `workflow_dispatch`.

For the runner pool, its workflow changes little:

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
bana ci [TIER] [-j JOB] [--x64] [--list] [-- ACT-OPTIONS]
bana up [--linux N] [--x64 N] [--no-mac] [--dedicated] [--label L] [--no-usb] [--token T]
bana status          # this machine's runners and USB audio devices, and the pool
bana usb             # the USB audio devices here, and the labels they give
bana relabel         # update runner labels after plugging a device in or out
bana start|stop NAME # one runner
bana down            # this machine's runners leave the pool
bana manager         # the page, at http://127.0.0.1:8470/#token=…
bana settings        # the settings in effect
```

Run them from the project's checkout: that is where bana finds bana.conf. `bana up` again is safe: registered
runners stay, and their labels are brought up to date.

**Tokens.** With the GitHub CLI signed in as a repository admin (`gh auth login`) nothing else is needed.
Otherwise pass `--token`: *Settings → Actions → Runners → New self-hosted runner*, the value after `--token`
(it is valid for an hour).

## The manager

`bana manager` builds it with cargo (a minute the first time) and starts a page on `http://127.0.0.1:8470`:

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

- **A runner runs whatever the workflow says, as its user.** Use bana with private repositories, or at least
  never let pull requests from forks run on these runners.
- **Billing.** Self-hosted runners use no Actions minutes. An account GitHub has locked for a payment problem
  runs no Actions at all, self-hosted included.
- **Leave the pool** with `bana down`, or *Leave the pool…* in the manager. The OrbStack machines stay for
  other projects; `orb delete -f bana bana-x64` removes them.

## bana's own tests

`tests/run.sh` runs every command against stand-ins for the programs bana drives (`uname`, `orb`, `tart`,
`gh`, `ioreg`, `sudo`, `apt-get`, and the runner's own scripts), so the macOS paths run on Linux too.
`BASH_UNDER_TEST=/bin/bash` picks the shell under test. `.github/workflows/test.yml` runs it on Ubuntu and on
macOS (stock bash 3.2), plus shellcheck and the manager's tests. On a private repository the macOS job's
minutes count ten times.

License: GPL-3.0-only, as dsper.

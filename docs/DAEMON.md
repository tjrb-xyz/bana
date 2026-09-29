# CI on push: bana daemon

`bana daemon install` makes a machine a project's CI. A daemon fetches the repository every 30 seconds (and at
once after a push from your checkout, through a git hook install adds; `--no-hook` leaves it out), runs each
eligible push through `bana ci` (act, in OrbStack's Docker) and posts commit statuses to GitHub with the GitHub
CLI. On a Mac it is a LaunchAgent in your login session, with 🧱 in the menu bar; on Linux it is a systemd user
service with no menu bar. It builds in its own clone, `~/.bana/<prefix>/src`, so your checkout stays yours.

The [README](../README.md#ci-on-push-bana-daemon) has the short version. This page has the rest.

## Install

What it needs: act (`brew install act`), Docker (OrbStack, running), the GitHub CLI signed in with the `repo`
scope (`gh auth login`), and cargo (rustup), which builds the daemon the first time. In the project's checkout:

```sh
bana daemon install             # check, build, clone, start; on a Mac it opens the page
bana daemon install --port 8471 # another port (the default is 8470, or the one installed)
bana daemon install --no-tray   # on a Mac: no menu bar item (a Mac mini nobody looks at)
bana daemon install --no-open   # don't open the page afterwards
bana daemon install --now       # restart at once, even while a build runs
```

Install first checks the machine and the workflow (the doctor): act, Docker, gh and its token's scope, that git
can read the repository through gh, and the workflow's `workflow_dispatch` trigger. It warns about what would go
wrong in the workflow ([What the workflow needs](#what-the-workflow-needs)), and about pool runners on this
machine, since with both every push would run twice. Then it builds `bana-manager`, puts a snapshot of bana in
`~/.bana/<prefix>/daemon`, clones the repository into `~/.bana/<prefix>/src` and writes the daemon's settings
from bana.conf.

The settings are read at install, so run install again after changing bana.conf's `daemon.*` keys, or to take a
newer bana. If a build is running, install waits for it to end; with `--now` it restarts at once and the build
runs again.

The first start only records the branches and tags as they are. Nothing is built until the next push, or until
you use *Run now*.

## On GitHub

Each build posts commit statuses: `bana` for the build and `bana/<job>` for each job that starts (a matrix job
adds its values, as `bana/package (linux-arm64)`). Jobs skipped by `if:` get none. `bana` says what happened and
where: `running on mbp (quick)`, `passed on mbp in 12m · 5 jobs, 3 skipped`,
`rust failed at "cargo clippy" · 3m40s on mbp`, or why it stopped (`cancelled from the menu bar`,
`timed out after 120m`, `interrupted (bana restarted)`).

A build at another tier than pushes run (a nightly by hand, a tag) posts `bana <tier>` and `bana <tier>/<job>`
instead, so it never overwrites a push's result. A queued build posts nothing, so a push replaced while it waited
shows no status at all.

Each status's *Details* link is `http://127.0.0.1:<port>/#build=<id>`: it opens the build on the page, and works
only on the machine that ran it, which is why the descriptions name the machine. If GitHub refuses that link, the
daemon posts without one from then on. Statuses that cannot be posted (offline, gh signed out) are tried again,
at most five minutes apart.

Pending statuses stay pending while the Mac is off in the middle of a build, so don't make `bana` a required check
yet.

## The menu bar

🧱, then what the daemon is doing:

| Title | |
|---|---|
| `🧱` | idle |
| `🧱 4m`, `🧱 4m +2` | building for 4 minutes of awake time, with 2 builds queued |
| `🧱 !` | idle, and the last build failed |
| `🧱 paused` | new builds are paused |
| `🧱 no Docker` | builds wait for Docker (start OrbStack) |
| `🧱 busy` | builds wait for your `bana ci` |
| `🧱 !gh` | statuses are not being posted; the tooltip says why |

A left click opens the page on the running build, else the latest one. A right click shows the menu: what it is
doing, the last result (it opens that build), *Open bana*, *Cancel build*, *Pause new builds* and *Quit bana*.
Quit stops local CI until your next login or `bana daemon install`.

## The page

`bana daemon open`, or the menu bar, opens it at `http://127.0.0.1:8470/`. Its *Local CI* section has:

- a line with the repository, when it last fetched, and whatever is holding builds back;
- *Pause new builds* (fetching goes on, and a running build finishes), *Check now* (fetch at once) and
  *Clear queue* (drops a backlog after a long time away);
- *Run now*: a branch or tag at a tier, at the front of the queue. That is how a nightly runs;
- the queue, each build with *Remove*;
- the build: its jobs, the live log of the one you pick (a failed step opens by itself), *Cancel* and *Re-run*
  (the same commit and tier again, even if it was built);
- the history (the last 100 builds), and the pushes not built.

The runner pool's sections follow, under *Runner pool (optional)*. `bana manager` opens this page too while the
project's daemon runs.

The page takes the token from the address the first time (the menu bar and `bana daemon open` pass it) and keeps
it in the browser's local storage, so GitHub's *Details* links open builds directly after that. It serves
loopback only, behind the token in `~/.bana/manager-token`. To see it from another machine:
`ssh -L 8471:127.0.0.1:8470 macbook.local`, then `http://127.0.0.1:8471/#token=…` with that machine's token.

## Which pushes run

A push runs when:

1. its branch matches `daemon.branches` (all but `dependabot/*` and `renovate/*`), or its tag matches
   `daemon.tags` (none). `*` also matches `/`, and a `!` pattern leaves names out;
2. its head commit's message has none of GitHub's skip markers: `[skip ci]`, `[ci skip]`, `[no ci]`,
   `[skip actions]`, `[actions skip]`;
3. the workflow is in that commit;
4. that commit has not been built at that tier already, so the same commit on a new branch, or pushed again, does
   not run twice (*Re-run* does).

The rules come from the install, never from the pushed commit, so a branch cannot widen them. Pull requests from
forks never reach the daemon: it fetches only branches and tags.

Branch pushes run `daemon.tier` (the first of `tiers`), tags `daemon.tag_tier` (the last). A branch keeps at most
one queued build: a newer push replaces its commit. A running build finishes (`daemon.supersede = queued`); with
`running`, a newer push to the same branch cancels it. A tag's builds are never replaced. A deleted branch drops
its queued build.

The workflow's plan job sees the push's `before` as the branch's last green commit, so the changes of pushes that
failed, were replaced or were cancelled stay in its diff. A branch with no green build yet compares with the
default branch.

## One build at a time

The daemon runs one build at a time, and act runs a build's jobs in parallel as it does under `bana ci`. The lock
`~/.bana/act.lock` is shared with `bana ci`: while the daemon builds, `bana ci` says `act is busy here` and stops;
while your `bana ci` runs, the queue waits (`🧱 busy`). Builds also wait while paused, and while Docker does not
answer (OrbStack not started yet after login).

A build that takes longer than `daemon.timeout` (120 minutes of awake time) is cancelled. A cancel sends act a
SIGINT, so its cleanup and `always()` steps run; a second one after 60 s, and after 30 s more act and everything
it started are killed. After every build the daemon ends any process the build left behind and removes act's
workspaces; after one that did not end on its own, its job containers too.

## Sleep, wake and restarts

The daemon keeps nothing awake while idle. While act runs it holds `caffeinate -i`, so the Mac does not sleep on
its own in the middle of a build. Closing the lid still sleeps it: the build pauses and goes on after wake, and a
step that needed the network may fail then. Those failures look real and are not retried.

The first fetch after the Mac wakes comes within one poll (`daemon.poll`, 30 seconds). It sees every branch that
moved meanwhile and queues one build each, for its newest commit.

A build the daemon did not finish (a crash, a reboot, a logout) is ended at the next start and run again once, if
its commit is still its branch's head (a tag's, or one started by hand, always). `bana daemon uninstall` stops a
running build and does not run it again.

## What the workflow needs

The daemon runs the workflow as `workflow_dispatch`, with an event shaped like the push (`github.ref`,
`github.sha`, `github.event.before`, and the tier in `inputs`). The doctor warns about each of these:

- **`on:` keeps only `workflow_dispatch`.** With `push` too and no runner pool, GitHub queues self-hosted jobs
  that no runner takes. With only `workflow_dispatch`, GitHub runs nothing itself on a push. A workflow without
  `workflow_dispatch` cannot be installed.
- **No checkout `ref:`.** act builds the pushed commit only for a checkout without `ref:`; with one it clones
  from GitHub instead.
- **`runner.environment == 'self-hosted'` gates need `|| env.ACT == 'true'`.** act never sets
  `runner.environment`, so such a step is skipped under the daemon.

act ignores `on.push.branches`, `paths` and `concurrency`: the daemon's own rules and its one-build-at-a-time
replace them. act enforces only a step's `timeout-minutes`; `daemon.timeout` guards the build.

Caches: macOS jobs run on the Mac in a fresh copy of the commit under `~/.bana/<prefix>/act-cache`, without
gitignored files such as `target/`, so they build from scratch every time; the daemon removes the copy after the
build. Linux jobs run in fresh containers, unless bana.conf has `act.args = --reuse`. Reused containers keep
their builds, and also the files deleted since, so a workflow that uses it should run `keep-builds`, which under
the daemon cleans the job's copy but for bana.conf's `keep`.

A job can tell it runs under the daemon by `BANA_DAEMON=1` (and under any act by `ACT=true`). The workflow's
`vars.*` come from `~/.bana/<prefix>/vars` (`KEY=value` lines) if you write one.

## Settings

In bana.conf, read at install (`bana settings` shows them):

| Key | Default | What |
|---|---|---|
| `daemon.branches` | `* !dependabot/* !renovate/*` | the branches whose pushes run; empty: none |
| `daemon.tags` | | the tags whose pushes run (`v*`); empty: none |
| `daemon.tier` | the first of `tiers` | the tier branch pushes run, under the plain `bana` statuses |
| `daemon.tag_tier` | the last of `tiers` | the tier tag pushes run, under `bana <tier>` |
| `daemon.poll` | `30` | seconds between fetches (at least 10) |
| `daemon.timeout` | `120` | minutes of awake time a build may take (`90s`: seconds) |
| `daemon.supersede` | `queued` | `queued`: a newer push replaces only a queued build of its branch; `running`: it also cancels the running one |
| `daemon.token` | `gh` | the jobs' `GITHUB_TOKEN`: `gh` (the GitHub CLI's token, as `bana ci` gives it) or `none` (empty) |

`act.args` (act options for `bana ci` and the daemon's builds, such as `--reuse`), `act.image` and `act.network` are read from
the commit being built, like the workflow. `repo`, `prefix`, `workflow`, `tiers`, `tier_input` and `path` are
taken at install; `path` goes first on the daemon's PATH.

## Trust

**A push runs as you on your Mac.** An eligible push runs that commit's workflow and scripts with your user:
macOS jobs outside any container, with your home directory, SSH keys and Keychain in reach. Linux containers are
no boundary either, since OrbStack shares `/Users` into its VM. That is acceptable only because the repository
is private and write access is the gate: use the daemon only on a private repository whose writers you trust.
Bot branches are left out by default, and pull requests from forks never run.

Tokens: statuses and fetches go through gh, whose token stays in the Keychain. Jobs get `GITHUB_TOKEN` only
through a per-build secrets file (0600), removed when act exits; it is never on a command line. With
`daemon.token = gh` it is gh's token, which has every scope you gave gh and does not expire, unlike GitHub's
per-run token. `none` gives jobs an empty one, but a host job can still run `gh auth token` itself, so the real
limit is the trust above.

What the pushed commit cannot change: act runs in the build's directory, so an `.actrc` in the tree is not read
(yours in `~` still is), and the tree's `.env`, `.input`, `.vars` and `.secrets` are ignored. Job containers
get no Docker socket, and act gets only a short list of the daemon's environment (`PATH`, `HOME`, `USER`,
`LOGNAME`, `SHELL`, `LANG`, `LC_ALL`, `TMPDIR`, `DOCKER_HOST`). `act.args` is read from the commit, which is the
same trust as the workflow.

While a build runs, act's artifact and cache servers listen without a password on the Mac's network address, so
on a shared network (café Wi-Fi) a neighbour could reach them. The macOS firewall may ask once about act.

Logs stay on the machine.

## Files and logs

| | |
|---|---|
| `~/.bana/<prefix>/daemon/` | the snapshot: `bana-manager`, bana, and `settings` |
| `~/.bana/<prefix>/src/` | the daemon's clone |
| `~/.bana/<prefix>/builds/<id>/` | each build: `build.json`, `act.jsonl` (act's log), `event.json`, `artifacts/` |
| `~/.bana/<prefix>/act-cache/` | act's actions, and the macOS jobs' copies while they run |
| `~/.bana/<prefix>/state.json` | paused, the queue, the heads seen, each branch's last green commit |
| `~/.bana/<prefix>/vars` | optional, yours: `KEY=value` lines for `vars.*` |
| `~/.bana/act.lock` | the lock shared with `bana ci` |
| `~/Library/Logs/bana/<prefix>.log` | the daemon's log on a Mac (`journalctl --user -u bana-<prefix>` on Linux) |

The last 100 builds are kept, and their artifacts for 7 days.

## Commands

```sh
bana daemon install [--port N] [--no-tray] [--no-open] [--now]
bana daemon status             # whether it runs, what it builds, what waits
bana daemon log                # its log, followed
bana daemon open               # the page
bana daemon poke               # fetch now, rather than at the next poll
bana daemon run [--build]      # in the foreground, for debugging (--build: from this checkout first)
bana daemon uninstall [--purge] # stop and remove it; --purge also its clone, builds and state
```

Run them in the project's checkout. On Linux the daemon runs while you are logged in; on a machine you don't log
in to, `sudo loginctl enable-linger $USER` keeps it running.

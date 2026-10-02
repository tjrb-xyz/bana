# CI on push: bana daemon

One daemon a machine is the CI of every project you add to it. `bana daemon install` starts it, once, anywhere;
`bana add`, in a project's checkout, adds the project. The daemon fetches each repository every 30 seconds (and
at once after a push from a checkout, through a git hook `bana add` adds), runs each eligible push through
`bana ci` (act, in OrbStack's Docker), one build at a time on the machine, and posts commit statuses to GitHub
with the GitHub CLI. On a Mac it is a LaunchAgent in your login session, with 🧱 in the menu bar; on Linux it is
a systemd user service with no menu bar. It builds each project in its own clone, `~/.bana/<prefix>/src`, so
your checkout stays yours.

The [README](../README.md#ci-on-push-bana-daemon) has the short version. This page has the rest.

## Install

What it needs: act (`brew install act`), Docker (OrbStack, running), the GitHub CLI signed in with the `repo`
scope (`gh auth login`), and, in a git checkout of bana, cargo (rustup), which builds the daemon the first time (a
release has it built). Anywhere, once a machine:

```sh
bana daemon install             # check, build, start; on a Mac it opens the page
bana daemon install --port 8471 # another port (the default is 8470, or the one installed)
bana daemon install --no-tray   # on a Mac: no menu bar item (a Mac mini nobody looks at; kept, --tray: back)
bana daemon install --no-open   # don't open the page afterwards
bana daemon install --now       # restart at once, even while a build runs
```

Install first checks the machine (the doctor): act, Docker, unzip and sha256, gh and its token's scope, and
whether gh can publish releases. Then it takes the release's `bana-manager` (or builds one), puts a snapshot of bana in `~/.bana/daemon.d`,
writes the machine's settings there, and starts the daemon: the LaunchAgent `xyz.tjrb.bana` on a Mac, the user
unit `bana.service` on Linux. It lists the projects it serves (none, the first time).

`bana upgrade` takes a newer bana: the new release's install.sh runs install from the new files before it
switches bana over. Run install again for another port. If a build is running, install waits for it to end;
with `--now` it restarts at once and the build runs again. The push hooks read the port when they run, so a new
port needs nothing else. If the new daemon does not come up, the one before runs again ([Upgrades](#upgrades)).

## Projects

```sh
bana add [--check] [--diff] [--workflow F] [--no-hook] [--no-claude]   # in its checkout
bana list                        # the projects added here
bana pause [PROJECT]             # hold its automatic builds
bana resume [PROJECT]            # build what waited
bana remove [PROJECT] [--purge]  # its CI here goes
```

PROJECT is a prefix `bana list` shows, by default the checkout's.

**`bana add`** first says where each of the workflow's jobs runs here and proposes bana.conf and workflow changes
([README](../README.md#bana-add-bana-as-the-projects-ci)), written only if you say so on a terminal. Then it
checks what the daemon needs of the project: that git can read the repository through gh, the workflow's
`workflow_dispatch` trigger, and what would go wrong in the workflow
([What the workflow needs](#what-the-workflow-needs)), and pool runners on this machine, since with both every
push would run twice. It writes the project's settings, `~/.bana/<prefix>/daemon/settings`, from bana.conf (that
file is the project being added: the daemon serves every project that has one), clones the repository into
`~/.bana/<prefix>/src`, adds the push hook (`--no-hook` leaves it out) and registers bana's tools with Claude
Code (below). If the daemon runs, it starts the project at once and prints its page; otherwise the next
`bana daemon install` does. A prefix another repository has here already is refused: set another in bana.conf.

The settings are read at `bana add`, so run it again after changing bana.conf's `daemon.*` keys. When they
changed, the daemon starts the project again; a build it was running ends interrupted and runs again once.

When Claude Code (`claude`) is on the PATH, `bana add` registers bana's MCP server with it, in this checkout at
local scope (private to you; the fix worktrees see it too): `claude mcp remove -s local bana`, then `claude mcp
add -s local bana -- ~/.bana/daemon.d/bana-manager mcp --dir ~/.bana/<prefix>`. Those are the tools Claude uses
in a fix ([FIX.md](FIX.md)). `--no-claude` leaves it out; `bana remove` removes it.

The first start of a project only records its branches and tags as they are. Nothing is built until the next
push, or until you use *Run now*.

**`bana list`** has a line a project: PROJECT, REPO, STATE (`active`, `paused`, or `error: …` for a project the
daemon cannot start, such as one with no clone), QUEUE, LAST (the last build and how it ended), CHECKOUT and
FILES, where its clone, builds, fixes and releases are (`~/.bana/<prefix>`). While the daemon is down it reads
the files instead, and STATE is `down` (or `down, paused`).

**`bana pause`** holds the project's automatic builds: its pushes, and the retries of builds a restart cut
short. The daemon still fetches, and the pushes queue (a newer push still replaces a queued one of its branch);
*Run now*, *Re-run*, a fix's rounds and *Publish* still work. A tag's build is a push, so it waits too. A
running build finishes. **`bana resume`** builds what waited. Both work while the daemon is down: the pause is
the file `~/.bana/<prefix>/daemon/paused`, which the daemon reads when it starts, and *Pause automatic builds*
on the page and in 🧱 is the same file.

**`bana remove`** takes the project's settings and pause away; the daemon cancels its running build (`project
removed`) and drops it. Then it removes the push hook and Claude Code's registration in the checkout the
settings name (from any directory: `bana remove wid`). Its clone, builds, fixes and releases stay in
`~/.bana/<prefix>`, so `bana add` again goes on from them; `--purge` removes those too (also later:
`bana remove wid --purge` after a `bana remove wid`). Neither touches the project's pool runners (`bana up`) or
your `vars`.

**`bana daemon uninstall`** stops the daemon and removes `~/.bana/daemon.d` (and `.new`, `.prev`, `.bad`) and the LaunchAgent or unit. The
projects stay added: their pushes wait for the next install. `--purge` runs `bana remove --purge` for each first.

## On GitHub

Each build posts commit statuses: `bana` for the build and `bana/<job>` for each job that starts (a matrix job
adds its values, as `bana/package (linux-arm64)`). Jobs skipped by `if:` get none. `bana` says what happened and
where: `running on mbp (quick)`, `passed on mbp in 12m · 5 jobs, 3 skipped`,
`rust failed at "cargo clippy" · 3m40s on mbp`, or why it stopped (`cancelled from the menu bar`,
`timed out after 120m`, `interrupted (bana restarted)`).

A build at another tier than pushes run (a nightly by hand, a tag) posts `bana <tier>` and `bana <tier>/<job>`
instead, so it never overwrites a push's result. A queued build posts nothing, so a push replaced while it waited
shows no status at all.

Each status's *Details* link is `http://127.0.0.1:<port>/#p=<prefix>&build=<id>`: it opens the build on the page, and works
only on the machine that ran it, which is why the descriptions name the machine. If GitHub refuses that link, the
daemon posts without one from then on. Statuses that cannot be posted (offline, gh signed out) are tried again,
at most five minutes apart.

Pending statuses stay pending while the Mac is off in the middle of a build, so don't make `bana` a required check
yet.

## The menu bar

🧱, then what the daemon is doing. With more than one project, the title names the project it is about: the
one building (`🧱 wid 4m +3`, every project's queue counted), else the one that most needs you (a release to
publish, then one that cannot start, a failure, a wait, a pause), else `🧱 +3` for what is queued.

| Title | |
|---|---|
| `🧱` | idle |
| `🧱 4m`, `🧱 4m +2` | building for 4 minutes of awake time, with 2 builds queued |
| `🧱 !` | idle, and the last build failed |
| `🧱 paused` | automatic builds are paused |
| `🧱 no Docker` | builds wait for Docker (start OrbStack) |
| `🧱 busy` | builds wait for your `bana ci` |
| `🧱 publishing` | a release is being published |
| `🧱 v0.1.0?` | v0.1.0's build passed with its files: publish it? ([Releases](#releases)) |
| `🧱 !gh` | statuses are not being posted; the tooltip says why |

A left click opens the page on the running build, else the latest one. A right click shows the menu: what the
machine is doing (`bana: building wid #12`, `bana: 2 queued`, `bana: idle`, and how many projects cannot
start), *Open bana*, a submenu for each project, and *Quit bana*. A project's submenu is labelled with what it
does, and has the last result (it opens that build), *Publish v0.1.0…* while bana asks (it opens the page on
the release, where you read the notes first), *Fix #41 with Claude…* while its last build failed
([FIX.md](FIX.md)), *Open*, *Cancel #12* while it builds, and *Pause automatic builds*. A project the daemon
cannot start shows why, and what to run. Quit stops local CI until your next login or `bana daemon install`.

## The page

`bana daemon open [PROJECT]`, or the menu bar, opens it at `http://127.0.0.1:8470/`. With two projects or
more, its header has a picker, each with its state and queue; the address says which project it shows
(`#p=<prefix>`), and without one it shows the project building, else the first. Its *Local CI* section has, for
that project:

- a line with the repository, when it last fetched, and whatever is holding builds back (`after wid #12`: another
  project's build runs first);
- the release bana asks about, or `#p=<prefix>&release=<tag>` ([Releases](#releases));
- *Pause automatic builds* ([Projects](#projects)), *Check now* (fetch at once) and *Clear queue* (drops a
  backlog after a long time away);
- *Run now…*: a branch or tag at a tier, all jobs or one (with the jobs it needs), at the front of the
  queue, once you say yes. That is how a nightly runs;
- the queue, each build with *Remove*;
- the build: its jobs, the live log of the one you pick (a failed step opens by itself), *Run JOB…* for the
  job picked (`bana ci -j`: it posts that job's statuses, and those of the jobs it needs, but not the build's
  `bana`, moves no green and leaves the commit's own build to come), *Cancel* and *Re-run*
  (the same commit and tier again, even if it was built), and on a failed build *Fix with Claude*
  ([FIX.md](FIX.md)), then the fix's card: its rounds, *Keep*, *Push*, *Compare on GitHub*, *More rounds* and
  *Discard*;
- the build's *Report* (once it ended): the CI report's table, the rest as text, with *Copy* and *Download*;
- the build's *Files* ([below](#a-builds-files)): each with its size, platform and *Download*, and the command
  that installs them here;
- the history (the last 100 builds, each with its tests' share, `tests 95%`, and its files, `4 files`), and the
  pushes not built.

The runner pool's sections follow, under *Runner pool (optional)*, for the project shown. `bana manager` opens
this page too while the daemon runs. With no project yet the page says to run `bana add`; a project the daemon
cannot start shows why.

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
   not run twice (*Re-run* does). A tag always runs, even on a commit built already: its build is the release.

The rules come from `bana add`, never from the pushed commit, so a branch cannot widen them. Pull requests from
forks never reach the daemon: it fetches only branches and tags.

Branch pushes run `daemon.tier` (the first of `tiers`), tags `daemon.tag_tier` (the last). A branch keeps at most
one queued build: a newer push replaces its commit. A running build finishes (`daemon.supersede = queued`); with
`running`, a newer push to the same branch cancels it. A tag's builds are never replaced. A deleted branch drops
its queued build.

A pushed tag is seen at the next poll, or at once with `bana daemon poke`. The push hook covers branches only.

The workflow's plan job sees the push's `before` as the branch's last green commit, so the changes of pushes that
failed, were replaced or were cancelled stay in its diff. A branch with no green build yet compares with the
default branch.

## One build at a time

The daemon runs one build at a time on the machine, whatever its project, and act runs a build's jobs in
parallel as it does under `bana ci`. The projects take turns: after a build of one, the next project with a
queue goes first, so a long queue in one project delays another by one build at most. Meanwhile another
project's queue says `after wid #12`. The lock `~/.bana/act.lock` is shared with `bana ci`: while the daemon
builds, `bana ci` says `act is busy here` and stops; while your `bana ci` runs, the queue waits (`🧱 busy`).
Builds also wait while Docker does not answer (OrbStack not started yet after login), and a paused project's
pushes wait.

A build that takes longer than `daemon.timeout` (120 minutes of awake time) is cancelled. A cancel sends act a
SIGINT, so its cleanup and `always()` steps run; a second one after 60 s, and after 30 s more act and everything
it started are killed. After every build the daemon ends any process the build left behind and removes act's
workspaces; after one that did not end on its own, its job containers too.

## Fix rounds

A fix's rounds ([FIX.md](FIX.md#the-loop)) are builds too, one per failed job: `bana ci <tier> -j <job>` on a
snapshot of Claude's worktree, which the MCP server pushes into the daemon's clone under `refs/bana/fix/`
(the daemon's fetch never touches those, and they never reach GitHub). They run with the failing build's ref,
tier and before, at the front of the queue, behind the running build: they never cancel one.

They differ from pushes: they post no statuses, move no branch's last green commit, never count as built and are
never replaced. Their `GITHUB_TOKEN` is empty and act gets `--action-offline-mode`, so they use the actions the
daemon already has; `fix.token = gh` gives them gh's token (a fix that moves an action pin to a commit the daemon
never ran needs it). act's own settings (`act.*`) come from the failing commit's bana.conf, which the daemon
saves as the build's `round.conf`, not from the snapshot. The history shows them as `fix d4b5174 · round 2 · job`.
A round cut short by a restart runs again once, like any build.

A fix gets `fix.rounds` rounds, one at a time; a pause does not hold them. Round 0, the failed jobs at
the failing commit, runs when a fix is made or registered: from the page or 🧱, or `bana fix` while the daemon
runs. When a fix's builds leave the history (pruned, or its fix dropped), its refs in the daemon's clone go too. The daemon touches your checkout only when you click:
*Fix with Claude* (the worktree), *Keep*, *Push* and *Discard*.

## The CI report

When a build ends, the daemon writes `results.jsonl` and `report.md` beside its log: the report of
[README.md](../README.md#the-ci-report), with the standards (`report.*`) of the built commit's bana.conf. The
page's *Report* tab, `bana report N`, `GET /ci/v1/p/<prefix>/builds/N/report` and the MCP server's `ci_report`
show it.
The statuses stay as they were.

The report is made from `results.jsonl` alone, one JSON object per line, `schema` 1. A builder other than act
can print act's `--json` lines (below) and have the daemon's fold, statuses, page and log work unchanged, or
write this file itself into the build's directory before the build ends: the daemon then takes it as it is, and
adds only the build's ref, commit, tier and times from `build.json`, and the repo, machine and bana commit where
it has none. An ended build without a report (one from before bana wrote them) gets it when it is asked for.

| `kind` | Fields |
|---|---|
| `build` | `schema`, `builder` (`act 0.2.89`), `bana`, `repo`, `ref`, `sha`, `tier`, `machine`, `network`, `trigger`, `started`, `ended` (Unix seconds), `result`: `success`, `failure`, `error` or `unknown` |
| `job` | `key` (`package (linux-arm64)`), `job` (its id), `matrix`, `result`: `success`, `failure`, `skipped`, `unsupported` (no platform here), `not_planned` (the plan left it out), `cancelled` or `unknown`; `ms` |
| `step` | `key`, `step` (its name), `stage` (`Pre`, `Main`, `Post`; empty for Set up job and Complete job), `result`, `ms`, `owner`: `project`, `bana` or `act`, `continued` (continue-on-error) |
| `tests` | `key`, `step`, `tool` (`cargo`, `nextest`, `vitest`, `jest`, `node`, `pytest`, `unittest`, `go`, whose counts are packages and are shown apart), `passed`, `failed`, `skipped`, `incomplete` (not every test ran: the tool stopped early, or the step never ended) |
| `test` | `key`, `step`, `name`, `result` (`passed`, `failed`, `skipped`), `binary`, `at` (FILE:LINE:COL), `message` |
| `rerun` | `key`, `step`, `target` (cargo's `--lib`, `-p crate --test facts`) |
| `annotation` | `key`, `step`, `level`, `message`, `title`, `file`, `line`, `col` |
| `notice` | `key`, `step`, `text`, `left_out` (true: the step did not check here; bana sets it when report.left_out matches, and a builder may set it itself), `title`, `file`, `line`, `col` |
| `summary` | `key`, `step`, `markdown` (the step's GITHUB_STEP_SUMMARY) |
| `tail` | `key`, `step`, `lines`: a failed step's last lines |
| `artifact` | `name`, `key` and `step` (the upload), `bytes`, `sha256` (the zip's), `files` (their names in `dist/`), `problem`: what a green build's jobs uploaded ([below](#a-builds-files)) |
| `error` | `owner`, `text`, and `key` and `step` when it names one: act's or bana's errors |

Lines come in that order, a job's steps after it, a step's lines after it; unknown kinds and fields are skipped.
act's `--json` lines that the daemon reads: `jobID`, `matrix`, `step`, `stepID` (`stepid` for Set up job),
`stage`, `msg` with `raw_output` (a step's output), `stepResult` with `executionTime` (ns), `jobResult`, and
optionally `command` (`summary`, `error`, `warning`, `notice`); `actlog.rs` has the details.

## A build's files

After a green build (not a fix round), the daemon collects what its jobs uploaded with
`actions/upload-artifact@v4` into `builds/<id>/dist/`. act keeps each upload as one zip,
`artifacts/<id>/<name>/<name>.zip`, from anyone who reaches its artifact server, and keeps only the last of two
uploads of one name. So a zip is taken only when exactly one upload step says it uploaded it: its `artifact-id`
output is act's id for the name (FNV-1a) and its `artifact-digest` is the zip's sha256. A 0-byte zip (a failed
upload), entries that are absolute, `..` or symlinks, a `NAME.sha256` that does not match NAME, or two files of
one name with different content (a matrix whose legs each built "their" CPU under act) refuse the whole
collection, with the reasons on the build. Names become `[A-Za-z0-9._-]`. An upload-artifact@v3 layout is left
as it is, with a note.

When the files include archives, `bana installer dist --label <tier>-<sha10>` (`--tag <tag>` for a tag) compiles
the project's installer beside them, from the built commit's bana.conf, in at most 120 seconds. Then the zips
go, so each file is kept once. The build's `dist` lists each file with its size, platform, and whether it is a
release's (in `SHA256SUMS`: `release.files` and the installer), or its problem. The CI report's *Artifacts*
section lists what each upload gave. A problem never changes the build's result or its statuses.

`GET /ci/v1/p/<prefix>/builds/<id>/files/<name>` serves a listed file, and nothing else. Install one on this machine with
`bana install <id>` (or `sh <dist>/install.sh --from <dist>`), in a terminal: the project's hook may ask, and
use sudo. The page does not install anything itself.

Kept: the newest build with files of each branch and tier while the branch exists, which the 100 builds do not
count. The rest go a week after the build: its `artifacts/` and `dist/`. A failed build's uploads stay where act
put them, for that week; a fix round's go when it ends.

## Releases

You choose the version and push the tag; bana never makes, moves or deletes one. With `daemon.tags = v*`:

```sh
gh auth refresh -h github.com -s workflow   # once: publishing may need gh's workflow scope
git tag -a v0.1.0 -m "example 0.1.0" && git push origin v0.1.0
bana daemon poke                            # or wait for the next fetch: the push hook covers branches only
```

The pushed tag builds at `daemon.tag_tier` (or *Run now* the tag at that tier), even on a commit built already,
and that build makes the tag's release, `releases/<tag>.json`. While it builds, bana finds the previous release
(the nearest published release that is an ancestor, from one `gh release list`, leaving out prereleases for a
final tag; local `v*` tags when gh fails, and it says so) and writes notes from git: each first-parent commit
since then, as a pull request (`Merge pull request #12 …`, `Merge #12: …`, or a squash, `Title (#12)`) or as
another change, so a direct push is never left out.

When the build passes with files for a release (its `SHA256SUMS`), bana asks: the page's card, `🧱 v0.1.0?` and
*Publish v0.1.0…*, and `release v0.1.0: waiting for your answer` in `bana daemon status`. A failed build, or one
with no files, is blocked, and the card says why; a re-run takes the release over. The card has the build, the
previous release and how it was found, the files and the platforms not built (those `release.platforms` names
with no archive), the CI report's table, and the title and notes to edit, with who saved them (git, Claude,
you) and which pull requests of the range they leave out, name from outside it, or name twice. That check warns;
it never stops a publish. The notes carry a rev: a save over newer notes, or a Publish of another rev than the
page shows, is refused, so you and Claude never write over each other. Notes saved while you edit are offered
(*Load rev 3*, your text kept below to copy, or *Save over rev 3*), never swapped in; notes saved while the page
sat untouched are swapped in and flagged, and Publish's question names their rev, who saved them, and the title.

*Publish v0.1.0*:

1. checks every file against `SHA256SUMS`, and that the tag on origin (`git ls-remote`, through gh's sign-in) is
   still the commit built: otherwise `v0.1.0 moved since build #57`;
2. asks gh again for the previous release: when it is another than the notes started from (one published since,
   or local tags stood in while gh failed), it stops, and git's notes are written again from it;
3. asks `gh release view v0.1.0`: if GitHub has the release already with these files (each asset's digest the
   sha256 in `SHA256SUMS`), it is recorded; with other files, it stops. A draft a killed publish left (its body
   the notes bana sent, only bana's files) is deleted, never the tag; any other draft, written on GitHub or by
   release-drafter, stays, and it stops: `a draft v0.1.0 is on GitHub (not bana's)`;
4. runs `gh release create v0.1.0 -R OWNER/REPO --verify-tag --title "<install.name> v0.1.0" --notes-file …`
   from `dist/`, with the files `SHA256SUMS` lists (`release.files`, and the installers) and `SHA256SUMS`
   itself. The notes get `## Tested` (the report's table) and `## Install` (the one-liners), which no edit can
   drop. `v0.1.0-rc1` gets `--prerelease`; a final version gets `--latest` only above every published final
   version, else `--latest=false`, so a hotfix of an older line never becomes Latest. It may take 30 minutes,
   under `caffeinate` on a Mac, and `releases/v0.1.0.log` keeps what gh said;
5. records the release's URL, or gh's last lines, and bana asks again.

One publish runs at a time. A restart in the middle of one leaves it failed (`interrupted`), and the next
Publish cleans up whatever gh left. *Not now* stops the ask; the card stays open, and the tag build's row in
the history links to it (`v0.1.0 dismissed`, `v0.1.0 published`), where *Publish* stays while the files are
kept. Nothing is published without your click: the menu bar only opens the page, and no tool of Claude's
publishes.

### Notes with Claude

In the checkout where `bana add` registered bana's MCP server, ask Claude Code: *write the release
notes for v0.1.0*. The server's instructions tell it how: read the release, then its pull requests, write for
the project's users one line per pull request ending `(#N)`, grouped by theme, never invent changes, and leave
Tested and Install to bana.

| Tool | Reaches | What |
|---|---|---|
| `release_context` | the daemon | the release bana asks about (or a tag): its build, previous release, files, Tested table, the changes from git (at most 300 other commits listed), and the notes with their rev |
| `pull_requests` | GitHub | up to 50 pull requests (or issues) by number, in one GraphQL query: title, author, labels, merged, base, the body's first 2,000 characters, the issues it closes; the numbers GitHub lacks are `missing` |
| `github_notes` | GitHub | GitHub's own generate-notes, from bana's previous release (always passed as `previous_tag_name`) to the tested commit; it follows `.github/release.yml`, saves nothing, and lists pull requests only |
| `save_release_notes` | the daemon | saves notes over the rev read, as Claude's; says what they leave out. Local only |

The GitHub tools run the daemon's gh with your sign-in and only read. Pull request bodies are anyone's text:
the tools and the instructions say they are data, not instructions. To keep Claude from publishing around bana,
the project's `.claude/settings.json` can allow the readers, deny gh's release writes, and deny the page's token
(`~/.bana/manager-token`, which the Publish route takes like every other) and curl to the daemon:

```json
{
  "permissions": {
    "allow": ["mcp__bana__release_context", "mcp__bana__pull_requests", "mcp__bana__github_notes"],
    "deny": ["Bash(gh release create:*)", "Bash(gh release edit:*)", "Bash(gh release upload:*)",
             "Bash(gh release delete:*)", "Bash(gh api *releases*)",
             "Read(~/.bana/manager-token)", "Bash(cat ~/.bana/manager-token:*)",
             "Bash(curl *127.0.0.1:847*)", "Bash(curl *localhost:847*)"]
  }
}
```

`save_release_notes` then asks you each time, which is a second look before the page's. The rules are a guard,
not a lock: a script Claude writes and runs could still read the token and call gh or the route, as any program
you run as you can.

### The API

The page and the MCP server use these, on loopback behind the token like the rest of a project's routes,
`/ci/v1/p/<prefix>/` ([The daemon's API](#the-daemons-api)):

| | |
|---|---|
| `GET …/releases` | the releases, the newest first |
| `GET …/releases/<tag>` | one release: its state, build, previous release, files, not built, changes, title (and the default one), notes, their check, Tested |
| `PUT …/releases/<tag>/notes` | `{notes, title?, rev, source?}` (`you` or `claude`): the new rev and the check; 409 for another rev, or once it publishes |
| `POST …/releases/<tag>/publish` | `{rev}`: 202, and the publish runs; 409 unless bana asks (or it failed, or was dismissed), the rev is the notes' now, and no other publish runs |
| `POST …/releases/<tag>/dismiss` | *Not now* |

`GET …/local`'s `release` is the one the menu bar and `bana daemon status` show.

## Sleep, wake and restarts

The daemon keeps nothing awake while idle. While act runs it holds `caffeinate -i`, so the Mac does not sleep on
its own in the middle of a build. Closing the lid still sleeps it: the build pauses and goes on after wake, and a
step that needed the network may fail then. Those failures look real and are not retried.

The first fetch after the Mac wakes comes within one poll (`daemon.poll`, 30 seconds). It sees every branch that
moved meanwhile and queues one build each, for its newest commit.

A build the daemon did not finish (a crash, a reboot, a logout) is ended at the next start and run again once, if
its commit is still its branch's head (a tag's, or one started by hand, always). With one build at a time, a
crash cuts short one build at most. `bana daemon uninstall` stops a running build and does not run it again.
While a project is paused, such a retry of a push waits for `bana resume`, and so does one of a *Run now* or a
*Re-run* (a retry no longer knows what started it); a fix round's retry runs.

## Upgrades

`bana daemon install`, run again (by hand, or by an upgrade), hands the daemon over to the bana it runs from:

1. It puts the new snapshot and settings in `~/.bana/daemon.d.new`; the running daemon's `daemon.d` is not
   touched, so a build running meanwhile keeps its bana.
2. It waits for the running build to end (and for one that starts as it ends), unless `--now`.
3. It renames `daemon.d` to `daemon.d.prev` and `daemon.d.new` to `daemon.d`, and restarts the service.
4. It waits up to a minute for the daemon to answer its health as this bana's version, from a new process.
5. If it does not, `daemon.d` becomes `daemon.d.bad` (its files, for `bana daemon log`), `daemon.d.prev` becomes
   `daemon.d` again, and the daemon before restarts; install fails, naming both versions. If that one does not
   come up either, both stay as they are and install prints the commands to start either. Install stopped
   meanwhile (Ctrl-C, kill, the terminal closed) does the same.

One install runs at a time: another one meanwhile is refused (`~/.bana/daemon.d.lock` has its pid).

One `daemon.d.prev` is kept. The settings are written by the new bana and go back with the snapshot, so a
release writes only the settings keys its own `bana-manager` reads. A change to `state.json` comes with a new
`version` in it, which the new daemon upgrades itself; an older daemon refuses a newer `state.json` (that project
does not run, and its log says `bana upgrade`) rather than drop what it does not know.

`bana upgrade` runs the newest release's install.sh, once its sha256 matches the release's SHA256SUMS. Its hook
runs the steps above from the new files first: if the new daemon does not come up, the install stops and bana
stays as it was. With `--now`, the daemon restarts at once. `bana upgrade vX.Y.Z` takes that release; an older
one is a downgrade, which asks (`--yes`). The installer keeps the release before, so going back is quick.

**The release check.** Two minutes after it starts, then once a day (by the clock, so a Mac's sleep counts; a
failure asks again an hour later), the daemon asks `https://github.com/tjrb-xyz/bana/releases/latest` where it
redirects (curl, with the settings' `path`); no API, no token. When that release is newer than the daemon's bana, the health says `"latest"`, and the page, the menu
bar (*bana vX.Y.Z is out…*), `bana list`, `bana daemon status` and the fix tools' status say so. Nothing is
installed by itself. To turn it off: `touch ~/.bana/.no-upgrade-check`.

## What the workflow needs

The daemon runs the workflow as `workflow_dispatch`, with an event shaped like the push (`github.ref`,
`github.sha`, `github.event.before`, and the tier in `inputs`). `bana add` warns about each of these:

- **`on:` keeps only `workflow_dispatch`, or pushes are gated.** With `push` too and no runner pool, GitHub
  queues self-hosted jobs that no runner takes. With only `workflow_dispatch`, GitHub runs nothing itself on a
  push. A `push` is fine too when the root jobs run only
  `if: (github.event_name != 'push' && github.event_name != 'schedule') || vars.<PREFIX>_CI_AUTO != 'false'`
  (bana add's gate; `vars.<PREFIX>_CI_AUTO != 'false' || github.event_name == 'workflow_dispatch'` does too,
  for a workflow with no other trigger) and that variable is `false` on GitHub. A workflow without
  `workflow_dispatch` cannot be added.
- **No checkout `ref:`.** act builds the pushed commit only for a checkout without `ref:`; with one it clones
  from GitHub instead.
- **`runner.environment == 'self-hosted'` gates need `|| env.ACT == 'true'`.** act never sets
  `runner.environment`, so such a step is skipped under the daemon.
  `$RUNNER_ENVIRONMENT` in a script is empty for the same reason.
- **Every job has a place.** `bana add`'s report says where each job runs: a job whose runs-on label has no
  place here is not run, and the build still passes. The `act.platform.*` keys it writes come through
  `bana ci`, so each build takes them from the commit it builds.

act ignores `on.push.branches`, `paths` and `concurrency`: the daemon's own rules and its one-build-at-a-time
replace them. act enforces only a step's `timeout-minutes`; `daemon.timeout` guards the build.

Caches: macOS jobs run on the Mac in a fresh copy of the commit under `~/.bana/<prefix>/act-cache`, without
gitignored files such as `target/`, so they build from scratch every time; the daemon removes the copy after the
build. Linux jobs run in fresh containers, unless bana.conf has `act.args = --reuse`. Jobs that need systemd
(`<prefix>-systemd`, `act.platform.<label> = machine`) run after the rest, in the Mac's Linux machine (`vm`), in a
copy under `~/.cache/bana/act-<prefix>` there, which act removes after each job. Reused containers keep
their builds, and also the files deleted since, so a workflow that uses it should run `keep-builds`, which under
the daemon cleans the job's copy but for bana.conf's `keep`.

A job can tell it runs under the daemon by `BANA_DAEMON=1` (and under any act by `ACT=true`). The workflow's
`vars.*` come from `~/.bana/<prefix>/vars` (`KEY=value` lines) if you write one.

## Settings

In bana.conf, read at `bana add` (`bana settings` shows them):

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
| `fix.rounds` | `5` | the rounds a fix may run after round 0 (1 to 100); *More rounds* adds as many again |
| `fix.token` | `none` | a fix round's `GITHUB_TOKEN`: `none` (empty, and act runs with `--action-offline-mode`) or `gh` |

`act.args` (act options for `bana ci` and the daemon's builds, such as `--reuse`), `act.image` and `act.network` are read from
the commit being built, like the workflow. `repo`, `prefix`, `workflow`, `tiers`, `tier_input` and `path` are
taken at `bana add`; `path` goes first on the project's PATH. `bana add` also records the checkout it runs in,
where *Fix with Claude* makes its branches and worktrees.

The machine's settings, in `~/.bana/daemon.d/settings`, are written at `bana daemon install`: the port, the
machine's name, whether there is a menu bar, the PATH and the programs (gh, git, docker, bash) found then, and
bana's commit, which a fix's brief names. A project's settings never change them.

## Trust

**A push runs as you on your Mac.** An eligible push runs that commit's workflow and scripts with your user:
macOS jobs outside any container, with your home directory, SSH keys and Keychain in reach, and systemd's jobs as
the Linux machine's user, who has passwordless sudo there. Linux containers are
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
same trust as the workflow; a fix round reads it from the failing commit, not from Claude's snapshot.

While a build runs, act's artifact and cache servers listen without a password on the Mac's network address, so
on a shared network (café Wi-Fi) a neighbour could reach them. A file planted or replaced there is refused (its
digest is not the one the upload step printed), but a neighbour could read unreleased packages while the build
runs. The macOS firewall may ask once about act.

A release publishes only the files collected that way: each bound to the digest its upload step printed, then
to `SHA256SUMS`, which Publish checks again, with the tag on origin, just before `gh release create`. Neither a
file changed in `dist/` afterwards nor a tag moved to another commit reaches GitHub. Publishing uses gh's token
with its `repo` (and `workflow`) scope; Claude's release tools only read GitHub, and write the notes on this
machine.

A fix round runs the code Claude wrote the same way, with the same reach as a push, but with no token by
default: nobody reviews it before it runs. Claude works in its worktree with your own Claude Code permission
settings.

Logs stay on the machine.

## Files and logs

| | |
|---|---|
| `~/.bana/daemon.d/` | the machine's: the snapshot (`bana-manager`, `bin/bana`, `lib/`), `settings`, and `daemon.lock` (one daemon a machine) |
| `~/.bana/daemon.d.new`, `.prev`, `.bad` | an install's: the snapshot it stages, the one before, and one that did not come up ([Upgrades](#upgrades)) |
| `~/.bana/.upgrade-from` | where `bana` pointed into `~/.bana/src` before a release took its place |
| `~/.bana/.no-upgrade-check` | there: the daemon does not ask for a newer bana ([Upgrades](#upgrades)) |
| `~/.bana/<prefix>/daemon/settings` | the project's settings, from bana.conf: while it is there, the project is added |
| `~/.bana/<prefix>/daemon/paused` | there while the project is paused |
| `~/.bana/<prefix>/src/` | the daemon's clone |
| `~/.bana/<prefix>/builds/<id>/` | each build: `build.json`, `act.jsonl` (act's log), `event.json`, `results.jsonl` and `report.md` (the CI report), `artifacts/` (act's), `dist/` (a green build's files and installer) |
| `~/.bana/<prefix>/act-cache/` | act's actions, and the macOS jobs' copies while they run |
| `~/.bana/<prefix>/releases/` | each release: `<tag>.json`, `<tag>.log` (what gh said when it was published) and `<tag>.notes.md` (what went on GitHub) |
| `~/.bana/<prefix>/fix/` | *Fix with Claude* and `bana fix`: each fix's worktree, and beside it `<sha7>.d/` with its brief and `rounds.json` ([FIX.md](FIX.md)) |
| `~/.bana/<prefix>/state.json` | the queue, the heads seen, each branch's last green commit |
| `~/.bana/<prefix>/vars` | optional, yours: `KEY=value` lines for `vars.*` |
| `~/.bana/act.lock` | the lock shared with `bana ci` |
| `~/Library/Logs/bana/bana.log` | the daemon's log on a Mac (`journalctl --user -u bana` on Linux) |

The last 100 builds are kept, and their artifacts and files for 7 days, but for the newest files of each branch
and tier ([above](#a-builds-files)), and a release's build while bana asks about it, and for 7 days after it was
published or dismissed. A fix's rounds are kept apart, until the fix is discarded or 14 days after
its last round.

## The daemon's API

On loopback, behind the token in `~/.bana/manager-token` but for the health:

| | |
|---|---|
| `GET /ci/v1/health` | `{daemon, global: true, port, machine, projects}` |
| `GET /ci/v1/projects` | a line a project: `{prefix, repo, checkout, paused, error, queue, running, last}` |
| `POST /ci/v1/projects` | read the projects' settings again (`bana add`, `remove`, `pause` and `resume` send it): start the new ones, start the changed ones again, drop the removed ones; the same answer |
| `/ci/v1/p/<prefix>/…` | the project's routes: `local`, `builds…`, `fixes…`, `releases…`, `daemon` (`{paused}`), `daemon/poll`, `queue/clear`, and the runner pool's. 404 for a project not added, 503 for one the daemon cannot start |

The push hook posts `/ci/v1/p/<prefix>/daemon/poll`, with the port from `~/.bana/daemon.d/settings`.

## From a daemon a project

bana had one daemon a project before: the LaunchAgent `xyz.tjrb.bana.<prefix>` or the unit
`bana-<prefix>.service`, each on its own port. `bana daemon install` moves them to the one daemon: it waits for
each one's running build (not with `--now`), stops it and removes its LaunchAgent or unit and its snapshot,
keeps only the project's keys in its settings, and puts its push hook and Claude Code's registration in its
checkout again. Its clone, builds, fixes, releases and state stay, and a pause carries over. The port is the old
daemon's when there was one, else 8470.

Old links do not follow: the statuses already on GitHub, and bookmarks, point at the old ports, or at
`#build=<id>` with no project. On the daemon's port such a link opens the project building, else the
first.

## Commands

```sh
bana daemon install [--port N] [--no-tray | --tray] [--no-open] [--now]
bana daemon status             # whether it runs, and what each project builds and waits for
bana daemon log                # its log, followed
bana daemon open [PROJECT]     # the page
bana daemon poke [PROJECT]     # fetch now, rather than at the next poll (every project, outside a checkout)
bana daemon run [--build]      # in the foreground, for debugging (--build: from this checkout first)
bana daemon uninstall [--purge] # stop and remove it; the projects stay (--purge: bana remove --purge each)
bana add | list | remove | pause | resume   # the projects (above)
```

`bana daemon` runs anywhere; `open` and `poke` take this checkout's project by default. On Linux the daemon runs
while you are logged in; on a machine you don't log in to, `sudo loginctl enable-linger $USER` keeps it
running.

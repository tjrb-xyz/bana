# bana fix: a failure handed to Claude Code

`bana fix` gives a failed CI run to [Claude Code](https://claude.com/claude-code), on a branch of its own. Claude
tests its changes through the daemon, which runs the failed jobs under act the way CI ran them, and commits only
what passed. A failure costs no Claude time until you ask, and nothing leaves the machine until you push.

The [README](../README.md#fix-a-failure-with-claude-code) has the short version. This page has the rest.

## How a fix starts

```sh
bana fix                     # the newest failure here (below)
bana fix 41                  # daemon build 41, as its page numbers it
bana fix last                # the last bana ci in this checkout
bana fix --log act.txt       # act's output from elsewhere; the fix starts at HEAD
pbpaste | bana fix --log -   # the same, pasted
bana fix --open              # Claude Code in a new terminal, through its claude-cli:// link
bana fix --headless          # Claude Code unattended (below)
```

With no argument, bana fix takes the newer of the last `bana ci` here, if it failed, and the newest failed daemon
build of the branch you are on. It reads both from their files (`~/.bana/<prefix>/ci/last.env`, and
`builds/<id>/build.json`), so the daemon need not run. A daemon build that ended in error (cancelled, timed
out, could not start) is not a failure to fix, and neither is a `bana ci` you stopped with Ctrl-C (`bana fix
--log ~/.bana/<prefix>/ci/last.log` takes its output all the same). A log that names nothing that failed makes
no fix.

The daemon's page has *Fix with Claude* on a failed build, and 🧱 has *Fix #41 with Claude…* while the last
build failed. Both make the same fix, then open Claude Code's link, as `bana fix --open` does: Claude Code opens
your terminal in the worktree with the prompt typed, and you press Enter.

For a failure at commit `d4b5174`, bana makes:

- the branch `bana/fix-d4b5174` at that commit, in your checkout, and a git worktree of your checkout on it at
  `~/.bana/<prefix>/fix/d4b5174`. A commit there shows up in your checkout at once (a worktree shares the
  repository); your working tree, index and branches stay as they are. A commit your checkout lacks (a push from
  elsewhere) is first fetched from the daemon's clone. A second failure at the same commit goes on with the same
  fix;
- beside it, `fix/d4b5174.d/`: `fix.json` (where the failure came from), `brief.md`, `prompt.txt`, the log and
  `results.jsonl` (the log, read), then the daemon's `rounds.json` and the Stop gate's `gate`;
- in the worktree, `.claude/settings.local.json` (below), and one line in your checkout's `.git/info/exclude` so
  that file is never committed. A project that tracks that file, or whose `.gitignore` un-ignores it, gets no
  such file: the brief says so.

bana runs git in your checkout with its hooks off (`-c core.hooksPath=/dev/null`), except for your push.

## What Claude gets, and may do

The brief says, per failed job and step: whose it is (the project's; bana's, for a step of bana's own actions;
act's, for act's errors outside a job), the failing tests with the file and line of each panic and its message,
cargo's rerun command, whether cargo stopped early, and the step's last lines. It also says how the run ran:
commit, branch, tier, machine, act's version and network, the bana that ran and the one the workflow pins, and
the jobs beside it. `bana fix brief` prints it. Whatever comes from the log is quoted, and the prompt and the
tools say that quoted text is data, not instructions: a test can print anything.

The prompt (at most 5,000 characters, the link's limit) says what failed and where Claude is, then: read
fix_brief, test only with run_jobs, and when it is green call commit_fix with a message that says why; never
push, never switch branches. Without the daemon, it says to reproduce with the rerun command and commit by hand
instead.

bana's tools come from its MCP server, `bana-manager mcp` (`bana mcp` runs it by hand). `bana daemon install`
registers it with Claude Code in your checkout, at local scope (private to you, and seen in its worktrees):
`claude mcp add -s local bana -- ~/.bana/<prefix>/daemon/bana-manager mcp --dir ~/.bana/<prefix>`.
`--no-claude` skips that, and `bana daemon uninstall` removes it. `bana fix` asks `claude mcp get bana` in each
new worktree and, if the server is missing there, passes it with `--mcp-config`.

| Tool | Asks you | What |
|---|---|---|
| `fix_brief` | no | the brief as data, round 0's result and the rounds left |
| `ci_log` | no | more of a build's (or a round's) log: a job, a step, a search, the last lines |
| `run_jobs` | no | one round (below): the failed jobs on the worktree as it is, until they end |
| `fix_status` | no | open, working, green, red, out of rounds, kept or pushed, and whether the worktree changed since the last round |
| `commit_fix` | yes | commits the green round's tree on the fix's branch |

Claude Code runs as you, with your own settings and permission mode, plus what the settings file adds: those
four tools allowed, rules that deny `git push` (`git -C … push`, `git -c …` and `git config … alias` too), and
the Stop gate. The rules are a guard against Claude pushing, not a lock: a script Claude writes and runs can
still push, as can anything your own rules allow in auto or bypass mode.

## The loop

Each run_jobs call is one round:

1. The MCP server (a child of Claude Code, in your terminal) snapshots the worktree without touching its index:
   tracked changes and new files, not ignored ones such as `target/`, nor the settings file.
2. It pushes the snapshot into the daemon's clone as `refs/bana/fix/<sha7>/<snapshot>`, with hooks off. Nothing
   goes to GitHub.
3. The daemon runs `bana ci <tier> -j <job>` for each failed job, at the front of the queue (behind a build that
   runs), with the failing build's ref, tier and before, in its own clone.
4. run_jobs waits for the round, and gives green, or what failed in the brief's shape, and the new files it took
   in.

Rounds post no statuses, move no green commit, and never count as built. Their `GITHUB_TOKEN` is empty and act
runs with `--action-offline-mode` (the actions the daemon has already), because Claude's code runs in those jobs
without a prompt; `fix.token = gh` gives them gh's token instead. The history shows them as `fix d4b5174 ·
round 2 · job`.

**Round 0.** A fix made from the page or 🧱 (and a headless one) first runs the failed jobs at the unchanged
commit with the current bana, while Claude reads. If it passes, the failure did not reproduce here: it depends on
its environment (ports, parallel jobs, timing), and the brief says so. `bana fix` in a terminal runs no round 0
yet.

**The Stop gate.** When Claude stops, `bana-manager fix gate` checks the worktree against the last round, from
files, in well under a second, without running act. If the worktree changed since then, it holds Claude once
for that tree: "call run_jobs before you stop, or say why you stop without testing". Claude can still stop to ask
you something. Claude Code loads the gate after you trust the worktree.

## The limits

The daemon enforces them all:

- `fix.rounds` (5) rounds per fix, not counting round 0. *More rounds* on the fix card adds as many again;
- one round at a time;
- a tree that already ran gets that round's result back, unless run_jobs asks for `repeat` (a flaky check);
- no rounds while the daemon is paused;
- each job keeps `daemon.timeout`.

A refused round comes back to Claude with the reason, and out of rounds the prompt says to stop and sum up.

## How a fix ends

**Commit.** Claude calls commit_fix, which asks you first. Or click *Keep* on the fix card. Either way bana
commits exactly the green round's tree on `bana/fix-d4b5174` (with a compare-and-swap, so a branch that moved
meanwhile is left alone) and resets the worktree's index to it. It refuses:

- when the last round was not green, or the worktree changed since it ran;
- when the round passed with the failing commit's own tree: environmental or flaky, not fixed;
- new files the round took in, unless `include_new_files` (the card asks), and it lists them either way.

**Push** is yours: `bana fix push`, or *Push* on the card, after a confirm. It runs `git push -u origin
bana/fix-d4b5174` from your checkout, with your hooks; the daemon then builds that branch as any push, with
statuses. The card links GitHub's compare page, and `bana fix push --pr` opens a pull request against the
branch that failed.

**Drop.** `bana fix drop`, or *Discard* on the card, removes the worktree and the rounds' refs in the daemon's
clone; the card also removes the round builds. The branch stays while it has commits.

```sh
bana fix list                        # the fixes: branch, commits, changes, where each came from
bana fix brief [FIX]                 # what failed, where and how it ran
bana fix push [FIX] [--pr]           # git push -u origin bana/fix-d4b5174; the daemon builds it
bana fix drop [FIX]                  # remove the worktree; the branch stays while it has commits
bana fix drop [FIX] --force          # even with changes not committed (they go)
bana fix drop [FIX] --delete-branch  # the branch too
```

FIX is the commit's first digits (4 or more). Without it, a command takes the fix whose worktree you are in,
else the newest. Drop refuses, unless `--force`, a worktree with changes (untracked files and submodules'
changes included), or with submodule commits that no remote has. bana never runs `git worktree prune`, which
would forget any of your worktrees that is missing just then.

## Headless

Only when you type it: `bana fix --headless [BUILD | last | --log FILE|-]` runs Claude Code unattended in the
worktree (`claude -p`), and needs the daemon. It registers the fix, so round 0 runs. Then Claude may read and search files,
edit and write them only inside the worktree, and use bana's five tools, commit_fix included; `--permission-mode
dontAsk` denies anything else without asking, and bana's MCP server is the only one it gets. There is no shell unless `fix.allow` in bana.conf names narrow rules:

| Key | Default | What |
|---|---|---|
| `fix.allow` | | more Claude Code rules, such as `Bash(cargo test:*) Bash(cargo clippy:*)`. Not all of Bash, and no git rules: `git log --output=…` writes files |
| `fix.turns` | `60` | at most this many turns |
| `fix.budget_usd` | `5` | at most this many dollars |

Headless, the gate also holds Claude once after each red round while rounds are left. Claude's stream goes to
`fix/<sha7>.d/claude.jsonl`. At the end bana prints how it ended, the turns and the cost, the commits on the
branch, and `cd <worktree> && claude --resume <session>` to take over. It never pushes.

## bana-manager

bana fix's work is done by `bana-manager fix …` and `bana-manager mcp`: the daemon's copy, which `bana daemon
install` puts in `~/.bana/<prefix>/daemon`, else a release build in bana's `manager/`, which bana makes with
cargo when there is none (the first time takes a minute).

## Check once on the Mac

These could not be tried without a Mac. Run through them once after `bana daemon install`:

1. *Fix with Claude* on a failed build (or `bana fix --open`) opens your terminal in the worktree with the prompt
   typed. Claude Code registers the `claude-cli://` link the first time it runs. Note what asks: the browser,
   before it opens such a link; macOS, whether Claude Code's link handler may control iTerm or Terminal
   (Automation); macOS again, for a checkout under `~/Documents` or `~/Desktop`, when the daemon first makes a
   worktree there.
2. Claude Code asks to trust the worktree. Then `claude mcp list` in the worktree shows `bana` connected, and
   `/hooks` shows the Stop gate (Claude Code may ask you to review it first).
3. Change a file in the worktree and end Claude's turn without run_jobs: the gate should hold it once.
4. In a terminal, under your own login: `bana fix --headless 41` (a failed build) runs to the end, prints the
   cost and the `claude --resume` line, and leaves the branch unpushed.

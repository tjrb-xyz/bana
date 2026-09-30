# bana fix: a failure handed to Claude Code

`bana fix` gives a failed CI run to [Claude Code](https://claude.com/claude-code), on a branch of its own. A
failure costs no Claude time until you ask: nothing starts by itself.

The [README](../README.md#fix-a-failure-with-claude-code) has the short version. This page has the rest.

## Where a fix starts

```sh
bana fix                     # the newest failure here (below)
bana fix 41                  # daemon build 41, as its page numbers it
bana fix last                # the last bana ci in this checkout
bana fix --log act.txt       # act's output from elsewhere; the fix starts at HEAD
pbpaste | bana fix --log -   # the same, pasted
bana fix --open              # Claude Code in a new terminal, through its claude-cli:// link
```

With no argument, bana fix takes the newer of the last `bana ci` here, if it failed, and the newest failed daemon
build of the branch you are on. It reads both from their files (`~/.bana/<prefix>/ci/last.env`, and
`builds/<id>/build.json`), so the daemon need not run. A daemon build that ended in error (cancelled, timed
out, could not start) is not a failure to fix.

The daemon's page has *Fix with Claude* on a failed build, and 🧱 has *Fix #41 with Claude…* while the last
build failed. Both make the same fix, then open Claude Code's link, as `bana fix --open` does.

## What bana makes

For a failure at commit `d4b5174`:

- the branch `bana/fix-d4b5174` at that commit, in your checkout;
- a git worktree of your checkout on it, at `~/.bana/<prefix>/fix/d4b5174`. A commit Claude makes there shows
  up in your checkout at once (a worktree shares the repository); your working tree, index and branches stay as
  they are. A commit your checkout lacks (a push from elsewhere) is first fetched from the daemon's clone;
- beside it, `fix/d4b5174.d/`: `fix.json` (where the failure came from), `brief.md`, `prompt.txt`, the log
  (`log.txt`, for a hand run or a paste) and `results.jsonl` (the log, read);
- in the worktree, `.claude/settings.local.json`, which denies Claude `git push`, and one line in your
  checkout's `.git/info/exclude` so that file is never committed. A project that tracks that file gets neither.

bana runs git in your checkout with its hooks off (`-c core.hooksPath=/dev/null`), except for `bana fix push`,
which is your push. A second failure at the same commit reuses the worktree and its branch: the fix goes on.

## What Claude gets

The brief says, per failed job and step: whose it is (the project's; bana's, for a step of bana's own actions;
act's, for act's errors outside a job), the failing tests with the file and line of each panic and its message,
cargo's rerun command, whether cargo stopped early, and the step's last lines (as data, fenced). It also says
how the run ran: commit, branch, tier, machine, act's version and network, the bana that ran and the one the
workflow pins, the jobs beside it, and, for a hand run, the changes it had that the branch lacks. `bana fix
brief` prints it.

The prompt (at most 5,000 characters, the link's limit) says what failed and where Claude is, then: read the
brief, reproduce with the rerun command in the worktree, and commit on the branch with a message that says why,
never pushing and never switching branches.

Claude Code runs as you, with your own settings and permission mode, plus that one rule. It asks once to trust
each new worktree.

## How it ends

```sh
bana fix list                        # the fixes: branch, commits, changes, where it came from
bana fix push [FIX]                  # git push -u origin bana/fix-d4b5174; the daemon builds it
bana fix push [FIX] --pr             # and gh pr create --fill against the branch that failed
bana fix drop [FIX]                  # remove the worktree; the branch stays while it has commits
bana fix drop [FIX] --force          # even with changes not committed (they go)
bana fix drop [FIX] --delete-branch  # the branch too
```

FIX is the commit's first digits (4 or more). Without it, a command takes the fix whose worktree you are in,
else the newest. Push refuses a branch with no commits. Drop refuses a worktree with changes, untracked files
included, unless `--force`. Nothing leaves the machine unless you push.

## bana-manager

bana fix's work is done by `bana-manager fix prepare` (and `fix brief`): the daemon's copy, which `bana daemon
install` puts in `~/.bana/<prefix>/daemon`, else a `cargo build --release` in bana's `manager/`. An older copy
without `fix` does not count: `bana daemon install` again brings a new one.

## Check once on the Mac

These could not be tried without a Mac:

- `bana fix --open` (and the page's button) opens a terminal in the worktree with the prompt typed. Claude Code
  registers the `claude-cli://` link the first time it runs; the browser may ask once before it opens such a
  link.
- macOS may ask once whether Claude Code's link handler may control iTerm or Terminal (Automation).
- Claude Code asks to trust each new worktree; its settings file loads after that.
- A checkout under `~/Documents` or `~/Desktop`: the daemon's first fix from the page may make macOS ask for
  that folder. `bana fix` in a terminal does not.

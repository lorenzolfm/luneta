---
name: lune
description: Drive lune for the user with the `lune` CLI - start work in a worktree of one of lune's projects from a folder, a branch, an issue or a PR, with an agent on each issue (one per issue, in parallel), take the user to a worktree, list the agents that run, delete the worktrees of merged pull requests, rebase a worktree's branch onto upstream, have lune watch another machine, and reset lune on every machine. Use when the user asks in words to start work on something, tackle one or several issues, check out a PR, go to a folder or a worktree, see what the agents are doing, clean up merged worktrees, rebase a branch onto main, add a machine, or reset lune and start from scratch, and `lune` is on the PATH.
---

# Drive lune

lune shows a sidebar of the user's projects, on this machine and on the others
it watches, beside the views of the worktree they are on. A project is a folder:
the root of a clone `wt clone` made, which holds each of its worktrees, or a plain
folder, whose one worktree is the folder itself. A worktree has a row in the
sidebar once it is opened. Its views are terminals of lune-mux, lune's own
multiplexer, each running a shell or an agent. luned, lune's daemon, keeps
the projects, their worktrees and their views, and the commands below ask it.

You run in one of those terminals, where `$LUNE_MUX_TERMINAL` holds its id, in
lune's ask box, or in a terminal of the user's own. `lune` alone, or with a
project's name, is lune itself, and takes over the terminal: don't run it.

## Find what the words point at

```sh
lune agents       # each agent of this machine: its terminal, status (busy/idle/waiting/shell), pid, and name or directory
zoxide query -l     # the directories the user works in, most used first; `zoxide query <words>` picks one
git worktree list   # the worktrees of the repository you run in, and their branches
gh pr view          # the pull request of the branch you run on
```

- "X" is a directory or a repository: match it against the zoxide list. Ask
  when two fit.
- "here", "this worktree", "this branch": the worktree you run in. "that PR":
  its pull request, or the one you were just talking about.
- `lune agents` writes `terminal <id>` for an agent in a terminal of
  lune-mux, and `-` for one in no terminal of lune's. No command takes the
  id: it tells the agents apart, and yours is `$LUNE_MUX_TERMINAL`. It lists
  this machine's agents alone: the sidebar shows the other machines'.
- `lune agents` needs luned, which lune starts. Where none runs, it says
  so and exits 5.

## Commands

```sh
lune start [--ship] [--flow <name>] [--interactive] <folder|branch|'#issue'|issue or PR link>
lune flow [status|list|start|advance|auto|pause|resume|retry|skip|takeover|abort] [--dir <dir>] [--prompt <text>] [--flow <name>] [--interactive] [--harness <claude|pi|agy>]
lune browse [<url>]
lune agents
lune prune [--repo <dir>] [--yes <plan>]
lune rebase [<dir>]
lune ssh <destination>
lune say [--now] [<text>]
lune say --stop
lune resume [--rules] [--reload] [--terminal <id>]
lune terminate [--yes <plan>]
lune info
```

- `start` starts a worktree, with an agent on an issue (or a worktree flow with
  `--ship`), and takes the user there: see [Start a worktree](#start-a-worktree).
- `flow` starts, inspects, or controls worktree flows (implement, review skills,
  CI fixes, rebases, and PR merge). Without an action it prints the flow on the
  current worktree or lists all active flows; it exits 5 where no luned runs.
- `browse` opens an embedded browser view beside your view (or in the current
  worktree), on `<url>` or the start page when omitted.
- `agents` lists the agents that run on this machine.
- `prune` deletes the worktrees of a repository's merged pull requests: see
  [Merged worktrees](#merged-worktrees).
- `rebase` rebases the branch of a worktree onto upstream: see
  [Rebase onto upstream](#rebase-onto-upstream).
- `ssh` has luned watch another machine, as the sidebar's `n m` does: it adds
  the machine to lune's hosts file, and says how the first ssh to it went.
  luned's ssh asks nothing, so a machine whose host key ssh has yet to know is
  reached once the user has run `ssh` to it themselves. Tell them so.
- `say` reads text aloud on the machine the user sits at: see the `say` skill.
- `resume` inspects per-terminal program resume records and trust rules, validates
  local resume config with `--rules`, or reloads `config.toml` in luned with
  `--reload`.
- `terminate` ends lune on every machine and forgets every project: see
  [Reset lune](#reset-lune).
- `info` says which luned runs, for how long, and whether it is another build
  than this lune. `lune watch [--after <pid>:<seq>] [--changes]` is for
  programs, not for you.

In lune's ask box, only the commands its prompt lists run, and Claude Code
denies the others. Where one you need is denied there, tell the user to run it
in a terminal instead.

## What the user does in lune

Past what `start`, `browse` and `rebase` open, the worktrees `prune` deletes, and what
`terminate` ends and forgets, no command opens, closes, moves or renames a view,
a worktree's row or a project, and only `start` takes the user anywhere.
The user does those in lune, whose menu (`Alt a`) and palette (`Super k`) list
each action by name. When they ask you for one, tell them which:

- "Open a project" (`Super o`) lists their projects, used last first, then
  the clones `wt clone` made that are no project yet, and takes a folder's
  path typed in it. Enter goes to a project, or makes one of the clone or the
  folder.
- "new view" opens a shell beside the view they are on.
- "new browser view…" (`n b`) opens an embedded browser view on a URL (or its
  start page) beside the view they are on.
- "close the view", "close the worktree" and "close the project" end the
  shells and agents in their views.
- "/thermo-nuclear-review", "/tigerstyle" and
  "/make-illegal-states-unrepresentable" start an agent on that skill beside
  the view they are on. "Search skills" finds any other skill.
- "start PR flow (auto)" (`f a`) and "start prompt flow (auto)…" (`f p`) start
  a worktree flow from the sidebar; when a flow is running or paused, "advance
  flow" (`f n`), "auto the rest" (`f u`), "take over" (`f o`), "resume flow"
  (`f r`), "retry step" (`f t`), "skip step" (`f s`), "pause flow" (`f z`) and
  "abort flow" (`f x`) control it.
- A link pasted in lune's sidebar offers `t` to tackle an issue or `r` for
  /thermo-nuclear-review on a pull request, `f` to ship it with a worktree
  flow, and `w` for its worktree. A pull request's link goes straight to its
  worktree when the sidebar has one row of it. Pasted in the palette, a link
  lists the same choices, and Enter starts the agent.

## Start a worktree

`lune start` starts work in one of lune's projects on this machine, opens
the worktree, and prints its folder.

```sh
lune start ~/code/app                        # a folder: the worktree it is, or is in
lune start feat/sidebar                      # a branch: a worktree of it, as `wt add` makes
lune start '#42'                             # an issue: worktree issue-42, and an agent told to tackle it
lune start https://github.com/o/app/pull/7   # a pull request, checked out in a worktree
```

- A folder of the name goes before a branch of it. `~` is the user's home,
  quoted or not. Quote `'#42'`, or the shell drops it as a comment.
- A folder inside a worktree starts that worktree. A clone's root starts the
  worktree used last there. Where it has no worktree, it opens nothing, prints
  the root, and says on stderr to start a branch in it.
- A branch needs a clone `wt clone` made, and you in it: in a plain clone it
  makes nothing and says why. Tell the user.
- `'#42'` asks gh in the directory you run in, so run it in the issue's
  repository. A link of an issue or a pull request goes to a clone `wt clone`
  made of its repository: the one you run in, else one of lune's projects,
  else one zoxide knows. Where none is, it makes nothing and says why.
- An issue's agent starts in the harness the user picked last in lune's menu
  (`Tab` there picks another), told `Tackle <the issue's link>`. Don't start
  another agent on it: `start` already has. Started again while its worktree
  has views, it starts no second agent; with none, the agent starts in it. If
  the worktree is on another branch than the issue's, no agent starts there. A
  pull request gets no agent; where its worktree is on another branch than its
  head's, as up a stack of pull requests, stderr names the branch.
- It exits 4 when the worktree started but its agent did not, or taking the
  user there failed: the folder is printed all the same, and stderr says what
  failed. Tell the user. Starting it again is safe.

### Which project and worktree work goes in

A worktree goes in the project lune keeps for its folder: the project of its
clone's root, or the project of the folder itself. Where lune has none, luned
makes one there, named after that folder: the clone's root, or the folder. A
name another project has gets a `-2`, as no two projects share one. luned
makes none at `~/lune`, where lune keeps clones, or in a folder holding it.
The worktree gets a row in that project, and a view, which runs the issue's agent
where `start` starts one, and a shell otherwise.

Run in a terminal of lune-mux, `start` takes each lune whose focus is on
your view to the worktree. If none's is, it exits 0, and stderr says no lune
is on this terminal: the user stays where they are. Elsewhere it takes nobody
anywhere.

### Several at once, one per issue

Agents working in parallel on one repository each need a worktree of their own,
or they edit the same files. Run `lune start '#<n>'`, or `lune start` with
the issue's link, once per issue: each gets its own worktree `issue-<n>` and its
own agent. The user lands on the first, as they leave your terminal with it.
Report each issue and the folder `start` printed for it.

### A skill on a pull request

`lune start <PR link>` checks the pull request out in a worktree and opens it,
with no agent. Then tell the user to start the skill from lune's menu on a view
of that worktree. For /thermo-nuclear-review, the PR's link pasted in lune's
palette and its /thermo-nuclear-review row does both, on the pull request's
head. So does the link pasted in the sidebar and `r`, unless the sidebar has
one row of the PR's worktree.

## Merged worktrees

When the user asks to clean up merged worktrees, run `lune prune`, with `--repo
<dir>` for a repository other than the one you run in. It deletes nothing, prints
each worktree that would go and each that stays with why, and exits 3. Its last
line, on stderr, names the plan:

```text
delete issue-282  #297  4.5 GB  branch issue-282, and issue-282 on origin
keep issue-308 #310: it has a view
lune: deleted nothing; run it again with --yes 9c41d07e3b2a5f18 to delete what it says
```

Show that to the user and ask. Run it again with that `--yes <plan>` only after a
clear yes, in a later turn, with the same arguments. If the plan changed, it
deletes nothing, prints the new one and exits 3: show that and ask again.

Exit 0 without `--yes` means there is nothing to delete. Exit 1 means something
failed: after `--yes`, it says which worktree stayed and why, and the others went.
Tell the user. If it says it cannot tell where the views are, it deleted nothing.
Don't delete worktrees or branches yourself instead: it has checked what git
can't, and the files git ignores go with a worktree, build output included.

Never run it unless the user asked.

## Rebase onto upstream

When the user asks to rebase a branch onto main, or to bring it up to date,
run `lune rebase`, with the worktree's directory for one other than the one
you run in. A pushed branch with an open pull request goes onto that pull
request's base, fetched from the remote of the pull request's repository, so a
stacked branch stays on the branch beneath it. Any other branch goes onto the default
branch of the repository's `upstream`, `github` or `origin` remote (else its own
`main` or `master`), fetched first. It pushes nothing: push only when the user
asks.

- It exits 0 once the branch is on top of it, and prints a line of where it
  is now.
- It exits 1 and rebases nothing when the worktree has changes not committed,
  is on no branch, or is already in the middle of a rebase, a merge or a
  cherry-pick. It does too when gh can't say whether the branch has a pull
  request open, or no remote fetches the pull request's repository. Tell the
  user why: don't commit or stash for them.
- It exits 4 when the rebase stopped on conflicts. The rebase is left stopped,
  and an agent is started on it in a new view of the worktree, in the harness
  the user picked last. Don't resolve the conflicts yourself, and don't run
  `git rebase --abort`: that agent has them. stderr says if no agent started;
  then tell the user the rebase waits for them.
- It also exits 4, with `no telling how the rebase ended` on stderr, when git
  took too long and was stopped, or what it left could not be read. The
  rebase may be part of the way through: don't resolve or abort anything, and
  tell the user to look at `git status` in the worktree.

## Reset lune

When the user asks to reset lune, or start it from scratch, run `lune
terminate`. It ends nothing, prints what would end on each machine, and exits 3.
Its last line, on stderr, names the plan:

```text
mac (this machine): ends 1 lune, luned and lune-mux, and 3 agents with them, 1 busy; forgets 4 projects and 9 worktrees
pi: skipped, as ssh does not reach it, and so lune runs on there and keeps its projects: ssh: connect to host pi port 22: Operation timed out
lune: ended nothing; run it again with --yes 5f0c2a9e81d4b37a to end what it says
```

Show that to the user and ask. Say that every agent on those machines ends, busy
ones too, and, if you run in a terminal of lune-mux, that you end with them.
Run it again with that `--yes <plan>` only after a clear yes, in a later turn. If
the plan changed, it ends nothing, prints the new one and exits 3: show that and
ask again. Once it runs, whatever it says shows nowhere if you end with it, so
tell the user before you run it that they can run `lune terminate` from a
terminal of their own to see what is left.

In lune's ask box, you cannot run it, as it would end the luned you run in:
tell the user to run `lune terminate` in a terminal instead.

Never run it unless the user asked for exactly that. Closing a view or a
worktree is theirs to do in lune.

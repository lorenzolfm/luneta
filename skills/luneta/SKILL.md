---
name: luneta
description: Drive luneta for the user with the `luneta` CLI - start work in a worktree of one of luneta's projects from a folder, a branch, an issue or a PR, with an agent on each issue (one per issue, in parallel), take the user to a worktree, list the agents that run, delete the worktrees of merged pull requests, rebase a worktree's branch onto upstream, have luneta watch another machine, and reset luneta on every machine. Use when the user asks in words to start work on something, tackle one or several issues, check out a PR, go to a folder or a worktree, see what the agents are doing, clean up merged worktrees, rebase a branch onto main, add a machine, or reset luneta and start from scratch, and `luneta` is on the PATH.
---

# Drive luneta

luneta shows a sidebar of the user's projects, on this machine and on the others
it watches, beside the views of the worktree they are on. A project is a folder:
the root of a clone `wt clone` made, which holds each of its worktrees, or a plain
folder, whose one worktree is the folder itself. A worktree has a row in the
sidebar once it is opened. Its views are terminals of luneta-mux, luneta's own
multiplexer, each running a shell or an agent. lunetad, luneta's daemon, keeps
the projects, their worktrees and their views, and the commands below ask it.

You run in one of those terminals, where `$LUNETA_MUX_TERMINAL` holds its id, in
luneta's ask box, or in a terminal of the user's own. `luneta` alone, or with a
project's name, is luneta itself, and takes over the terminal: don't run it.

## Find what the words point at

```sh
luneta agents       # each agent of this machine: its terminal, status (busy/idle/waiting/shell), pid, and name or directory
zoxide query -l     # the directories the user works in, most used first; `zoxide query <words>` picks one
git worktree list   # the worktrees of the repository you run in, and their branches
gh pr view          # the pull request of the branch you run on
```

- "X" is a directory or a repository: match it against the zoxide list. Ask
  when two fit.
- "here", "this worktree", "this branch": the worktree you run in. "that PR":
  its pull request, or the one you were just talking about.
- `luneta agents` writes `terminal <id>` for an agent in a terminal of
  luneta-mux, and `-` for one in no terminal of luneta's. No command takes the
  id: it tells the agents apart, and yours is `$LUNETA_MUX_TERMINAL`. It lists
  this machine's agents alone: the sidebar shows the other machines'.
- `luneta agents` needs lunetad, which luneta starts. Where none runs, it says
  so and exits 5.

## Commands

```sh
luneta start <folder|branch|'#issue'|issue or PR link>
luneta agents
luneta prune [--repo <dir>] [--yes <plan>]
luneta rebase [<dir>]
luneta ssh <destination>
luneta say [--now] [<text>]
luneta say --stop
luneta terminate [--yes <plan>]
luneta info
```

- `start` starts a worktree, with an agent on an issue, and takes the user
  there: see [Start a worktree](#start-a-worktree).
- `agents` lists the agents that run on this machine.
- `prune` deletes the worktrees of a repository's merged pull requests: see
  [Merged worktrees](#merged-worktrees).
- `rebase` rebases the branch of a worktree onto upstream: see
  [Rebase onto upstream](#rebase-onto-upstream).
- `ssh` has lunetad watch another machine, as the sidebar's `n m` does: it adds
  the machine to luneta's hosts file, and says how the first ssh to it went.
  lunetad's ssh asks nothing, so a machine whose host key ssh has yet to know is
  reached once the user has run `ssh` to it themselves. Tell them so.
- `say` reads text aloud on the machine the user sits at: see the `say` skill.
- `terminate` ends luneta on every machine and forgets every project: see
  [Reset luneta](#reset-luneta).
- `info` says which lunetad runs, for how long, and whether it is another build
  than this luneta. `luneta watch [--after <pid>:<seq>]` is for programs, not
  for you.

In luneta's ask box, only the commands its prompt lists run, and Claude Code
denies the others. Where one you need is denied there, tell the user to run it
in a terminal instead.

## What the user does in luneta

Past what `start` and `rebase` open, the worktrees `prune` deletes, and what
`terminate` ends and forgets, no command opens, closes, moves or renames a view,
a worktree's row or a project, and only `start` takes the user anywhere.
The user does those in luneta, whose menu (`Alt a`) and palette (`Super k`) list
each action by name. When they ask you for one, tell them which:

- "new view" opens a shell beside the view they are on.
- "close the view", "close the worktree" and "close the project" end the
  shells and agents in their views.
- "/thermo-nuclear-review", "/tigerstyle" and
  "/make-illegal-states-unrepresentable" start an agent on that skill beside
  the view they are on. "Search skills" finds any other skill.
- A link pasted in luneta's sidebar offers `w` for its worktree, and `t` to
  tackle an issue or `r` for /thermo-nuclear-review on a pull request. A pull
  request's link goes straight to its worktree when the sidebar has one row of
  it. Pasted in the palette, a link lists the same choices.

## Start a worktree

`luneta start` starts work in one of luneta's projects on this machine, opens
the worktree, and prints its folder.

```sh
luneta start ~/code/app                        # a folder: the worktree it is, or is in
luneta start feat/sidebar                      # a branch: a worktree of it, as `wt add` makes
luneta start '#42'                             # an issue: worktree issue-42, and an agent told to tackle it
luneta start https://github.com/o/app/pull/7   # a pull request, checked out in a worktree
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
  made of its repository: the one you run in, else one of luneta's projects,
  else one zoxide knows. Where none is, it makes nothing and says why.
- An issue's agent starts in the harness the user picked last in luneta's menu
  (`Tab` there picks another), told `Tackle <the issue's link>`. Don't start
  another agent on it: `start` already has. Started again while its worktree
  has views, it starts no second agent; with none, the agent starts in it. If
  the worktree is on another branch than the issue's, no agent starts there. A
  pull request gets no agent.
- It exits 4 when the worktree started but its agent did not, or taking the
  user there failed: the folder is printed all the same, and stderr says what
  failed. Tell the user. Starting it again is safe.

### Which project and worktree work goes in

A worktree goes in the project luneta keeps for its folder: the project of its
clone's root, or the project of the folder itself. Where luneta has none, lunetad
makes one there, named after that folder: the clone's root, or the folder. The
worktree gets a row in that project, and a view, which runs the issue's agent
where `start` starts one, and a shell otherwise.

Run in a terminal of luneta-mux, `start` takes each luneta whose focus is on
your view to the worktree. If none's is, it exits 0, and stderr says no luneta
is on this terminal: the user stays where they are. Elsewhere it takes nobody
anywhere.

### Several at once, one per issue

Agents working in parallel on one repository each need a worktree of their own,
or they edit the same files. Run `luneta start '#<n>'`, or `luneta start` with
the issue's link, once per issue: each gets its own worktree `issue-<n>` and its
own agent. The user lands on the first, as they leave your terminal with it.
Report each issue and the folder `start` printed for it.

### A skill on a pull request

`luneta start <PR link>` checks the pull request out in a worktree and opens it,
with no agent. Then tell the user to start the skill from luneta's menu on a view
of that worktree. For /thermo-nuclear-review, the PR's link pasted in luneta's
palette and its /thermo-nuclear-review row does both, on the pull request's
head. So does the link pasted in the sidebar and `r`, unless the sidebar has
one row of the PR's worktree.

## Merged worktrees

When the user asks to clean up merged worktrees, run `luneta prune`, with `--repo
<dir>` for a repository other than the one you run in. It deletes nothing, prints
each worktree that would go and each that stays with why, and exits 3. Its last
line, on stderr, names the plan:

```text
delete issue-282  #297  4.5 GB  branch issue-282, and issue-282 on origin
keep issue-308 #310: it has a view
luneta: deleted nothing; run it again with --yes 9c41d07e3b2a5f18 to delete what it says
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
run `luneta rebase`, with the worktree's directory for one other than the one
you run in. A branch with an open pull request goes onto that pull request's
base, fetched from the remote of the pull request's repository, so a stacked
branch stays on the branch beneath it. Any other branch goes onto the default
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

## Reset luneta

When the user asks to reset luneta, or start it from scratch, run `luneta
terminate`. It ends nothing, prints what would end on each machine, and exits 3.
Its last line, on stderr, names the plan:

```text
mac (this machine): ends 1 luneta, lunetad and luneta-mux, and 3 agents with them, 1 busy; forgets 4 projects and 9 worktrees
pi: skipped, as ssh does not reach it, and so luneta runs on there and keeps its projects: ssh: connect to host pi port 22: Operation timed out
luneta: ended nothing; run it again with --yes 5f0c2a9e81d4b37a to end what it says
```

Show that to the user and ask. Say that every agent on those machines ends, busy
ones too, and, if you run in a terminal of luneta-mux, that you end with them.
Run it again with that `--yes <plan>` only after a clear yes, in a later turn. If
the plan changed, it ends nothing, prints the new one and exits 3: show that and
ask again. Once it runs, whatever it says shows nowhere if you end with it, so
tell the user before you run it that they can run `luneta terminate` from a
terminal of their own to see what is left.

In luneta's ask box, you cannot run it, as it would end the lunetad you run in:
tell the user to run `luneta terminate` in a terminal instead.

Never run it unless the user asked for exactly that. Closing a view or a
worktree is theirs to do in luneta.

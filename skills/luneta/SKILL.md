---
name: luneta
description: Drive luneta and zellij for the user with the `luneta` CLI - start a worktree in a folder, a branch, an issue or a PR, open a session, a tab or a pane, start coding agents (Claude Code or pi) on work (one per issue, in parallel), take the user to a session or pane, turn a pane into a tab or a tab into panes, close panes or delete sessions, delete the worktrees of merged pull requests, rebase a worktree's branch onto upstream, and reset luneta on every machine. Use when the user asks in words to make a session to work on something, spawn an agent or a skill on a PR or issue, tackle several issues at once, go somewhere, move a pane to a tab of its own or a tab's panes into another tab, close sessions, tabs or panes, clean up merged worktrees, rebase a branch onto main, or reset luneta and start from scratch, and `luneta` is on the PATH.
---

# Drive luneta

luneta runs zellij beside a sidebar of every session on the host, and what
the agent in each pane is doing. Its CLI reads what lunetad, its daemon, knows,
and has zellij open, move and close things. You run in a pane of zellij, or in
a terminal of one of luneta's projects (`luneta-mux`'s), which the verbs don't
reach yet. In zellij, `luneta here` says which pane, as `<session>:<pane>`. Don't read it from `$ZELLIJ_SESSION_NAME`: that keeps the
name the session had when your pane opened, and a rename leaves it behind.

A pane is named `<session>:<pane>`, as `luneta:11`; a session by its name alone.

## Find what the words point at

```sh
luneta here       # the pane you run in, as <session>:<pane>, by its session's name now
luneta sessions   # each session, its panes as <tab>:<pane>, their directory, branch, changes, PR (with its checks) and repo
luneta agents     # each agent: <session>:<pane>, status (busy/idle/waiting/shell), pid, name
zoxide query -l   # the directories the user works in, most used first; `zoxide query <words>` picks one
```

`luneta agents` writes `terminal <id>` for an agent in a project's terminal.
That is no pane address: no verb takes it.

- "X" is a directory, a repository or a session: match it against the zoxide
  list and the session names. Ask when two fit.
- "that PR", "this branch": the pane you run in, or the PR you were just
  talking about. `luneta sessions` shows which pane has which branch and PR.
- "here", "this session": the session `luneta here` names.
- `luneta sessions` lists the user's other machines' sessions after a
  `<machine>/` line, indented under it. The verbs don't reach them yet: a
  session there is none of the ones they act on, even under the same name.

`sessions` and `agents` need lunetad, which luneta starts. Where none runs, they
say so and exit 5. The verbs below ask zellij themselves and work without it. Only `close` then can't tell which
agents end, and `prune` deletes nothing and says so.

`here`, `go`, `open session`, `close`, and `open pane`/`open tab` without
`--beside`/`--session` need to know which session you run in. If your session
was renamed and luneta can't tell which it is now (no lunetad, or one that
can't say), they fail and say so. Tell the user. Don't guess a session.

In a project's terminal, `luneta here` says you run in no pane of zellij, and
the verbs that need your session fail. Tell the user; they open, close and
move a project's terminals in luneta itself.

## Verbs

```sh
luneta open pane [--beside <session>:<pane>] [--cwd <dir>] [-- <command>...]
luneta open tab [--session <session>] [--cwd <dir>] [--name <name>] [-- <command>...]
luneta open session [--cwd <dir>] [--name <name>]
luneta go <session>[:<pane>]
luneta turn pane <session>:<pane>
luneta turn tab <session>:<pane> --into <session>:<pane>
luneta close <session>[:<pane>]... [--yes <plan>]
luneta prune [--repo <dir>] [--yes <plan>]
luneta rebase [<dir>]
luneta terminate [--yes <plan>]
luneta start <folder|branch|'#issue'|issue or PR link>
```

- `open pane` opens beside your pane by default, in its tab. `open tab` opens
  in your session by default. Both move nobody, and print the new pane as
  `<session>:<pane>`. `--cwd` defaults to your directory. Without a command,
  the pane runs the user's shell.
- `open tab` needs a client in that session: zellij can't size a tab
  otherwise, so in a session nobody is in it opens nothing and says so. See
  [Which session a tab opens in](#which-session-a-tab-opens-in). `open pane
  --beside` one of its panes needs no client, for when the user asked for a pane.
- `open session` makes a session in `--cwd`, named for it as luneta's picker
  names one (`luneta-main` for a worktree `main` of `luneta`, `-2` when taken),
  and takes the user there. It prints the name.
- `go` takes the user to a session, or a pane of one.
- `go` and `open session` move the client that last typed in your session,
  which is the user who just typed to you. If the user has left your session
  since, they fail with "zellij moved nobody". Tell the user where things are
  instead.
- `turn` moves panes that are running: see [Turn panes and tabs](#turn-panes-and-tabs).
- `close` takes a pane's tab along when nothing else holds the tab open, and
  the session when that was its only tab. The panes of a session given
  together close together: give every pane of a tab and the tab closes. It
  deletes a session given by name: kills it, and zellij keeps nothing of it to
  resurrect. It closes running sessions only. What holds your pane goes last,
  and not at all if anything before it failed to close, so that you're still
  there to tell the user.
- `prune` deletes the worktrees of a repository's merged pull requests: see
  [Merged worktrees](#merged-worktrees).
- `rebase` rebases the branch of a worktree onto upstream: see
  [Rebase onto upstream](#rebase-onto-upstream).
- `terminate` ends luneta on every machine and forgets every project: see
  [Reset luneta](#reset-luneta).
- `start` starts a worktree of one of luneta's projects: see
  [Start a worktree](#start-a-worktree).

## Which session a tab opens in

A tab of work in a repository goes in the session that works in that
repository, as a pasted link's does. Being asked from a session doesn't make it
the right one. Take the first of these that fits, and name it with `--session`
on every `open tab`:

1. The session the user named, or the session `luneta here` names if they said
   "here".
2. The session `luneta here` names, if a pane of it is in that repository.
3. Another session of this machine's with a pane in that repository. Of several,
   the first `luneta sessions` lists.
4. A session made for it: `luneta open session --cwd <the clone>`, with the
   directory `zoxide query` says. It takes the user there, and prints the name.

A pane that lists a `repo <dir>` in `luneta sessions` is in a repository if that
`repo` is the directory `zoxide query` says of the repository, or holds it, or if
the pane's own directory is that directory or in it. A pane that lists neither a
`repo` nor `no repository` says nothing of its repository: if its directory is in
the one `zoxide query` says, tell the user instead of making a session.

If `open tab` says no client is in the session, run `luneta go <session>` and
open the tab again, as a pasted link does. Do this without asking, and say in
your report that you took the user there. Don't open panes instead, unless the
user asked for panes. If `go` fails, or `open tab` still says no client, tell the
user the session, and what you made for it.

`go` and step 4 take the user from what they are doing, and what they type next
lands in another pane. Do either only while the user is waiting on you: they
asked for this just now and you went straight to it, as in luneta's ask box they
always have. If you have been working on your own since, leave them where they
are: tell them the session or the directory, and what you made for it.

A tab in no repository opens in the user's session: leave `--session` out.

## Start an agent

Run a coding agent as the pane's command, in a harness luneta knows: Claude
Code (`claude`) or pi (`pi`). Start the one the user names. Otherwise, in
luneta's ask box, the one its line names, which the user picked last in
luneta's menu; anywhere else, the one you run in. In the box, luneta opens pi
only where the line names pi: where it names Claude Code and the user asks for
pi, tell them to pick pi with `Tab` in luneta's menu. Don't give it a name
(`--name`) unless the user asks for one: in Claude Code, a name holds the
agent's row in the sidebar for good, where an unnamed agent's row follows what
it works on.

```sh
luneta open tab --session app --cwd ~/code/app --name issue-42 -- claude "let's tackle this one: https://github.com/o/app/issues/42"
luneta open pane --beside luneta:11 -- claude "/thermo-nuclear-review"
luneta open pane --beside luneta:11 -- pi "/skill:thermo-nuclear-review"
```

- A skill runs as the prompt: `/skill-name args` in Claude Code, and
  `/skill:skill-name args` in pi.
- Put the prompt in one argument. Quote it for the shell, not for zellij.
- The `<session>:<pane>` that `open` prints is where the agent runs.
  `luneta agents` lists it once it has started.

### Several at once, one per issue

Agents working in parallel on one repository each need a worktree of their
own, or they edit the same files:

1. Read each issue (`gh issue view <n>`) for a short branch name, such as `issue-42`.
2. Make a worktree per branch with `luneta worktree --repo <dir> <branch>`, where
   `<dir>` is any directory of the repository. It puts the worktree beside the
   others and prints where. It refuses a clone that git made; there, if you run
   in that repository, use `git worktree add ../<branch> -b <branch>`, and
   otherwise tell the user.
3. Open a tab per worktree in the session
   [Which session a tab opens in](#which-session-a-tab-opens-in) picks, named for
   the branch, with `claude "<prompt>"` in it, or `pi` in place of `claude`.
4. Report the tabs you opened (`<session>:<pane>` and the issue each has).
   Don't `go` anywhere unless asked, or that session has nobody in it and the
   user is waiting on you.

### A skill on a PR

Find the pane whose branch is the PR's in `luneta sessions` and open a pane
beside it. If no pane has the branch, make a worktree for the PR (`luneta
worktree --repo <dir> <PR link>` takes a link), then open a tab there, in the
session [Which session a tab opens in](#which-session-a-tab-opens-in) picks.

## Start a worktree

`luneta start` starts work in one of luneta's own projects, and prints the
worktree's folder. It works from a pane of zellij too, as it asks lunetad,
not zellij.

```sh
luneta start ~/code/app              # a folder: its project's, or a project made of it
luneta start feat/sidebar            # a branch: a worktree of it, as `wt add` makes
luneta start '#42'                   # an issue: worktree issue-42, and an agent told to tackle it
luneta start https://github.com/o/app/pull/7   # a pull request, checked out in a worktree
```

- Run it in the repository the branch, issue or pull request is of, or from
  anywhere for a link: luneta finds a clone of the link's repository. Quote
  `'#42'`, or the shell drops it as a comment.
- A project is the root of a clone `wt clone` made, holding each of its
  worktrees, or a plain folder, whose one worktree is the folder itself. A
  folder inside a project starts the worktree it is in.
- A branch needs a clone `wt clone` made: in a plain clone it makes nothing
  and says why. Tell the user. An issue or a pull request goes to a clone
  `wt clone` made of its repository: the one it runs in, else one of
  luneta's projects, else one zoxide knows.
- The issue's agent starts in the harness the user picked last in luneta's
  menu. Don't open another agent for it: `start` already has. Started again
  while its worktree has terminals, it goes there and starts no second
  agent. If the worktree has no terminal, the agent starts in it.
- A folder of the name goes before a branch of it. `~` is the user's home,
  quoted or not.
- It exits 4 when the worktree started but its agent did not, or taking
  the user there failed: the folder is printed all the same, and stderr says
  what failed. Tell the user. Starting it again is safe: it starts the agent
  only where no terminal is.
- Run in a terminal of luneta's, it takes the user there if they are on your
  terminal. If they are not, it exits 0 and stderr says no luneta is on this
  terminal: the user stays where they are. For several issues at once, run it
  once per issue: each gets its own worktree and agent, and the user lands on
  the first, as they leave your terminal with it.

## New session to work on X

```sh
zoxide query X                        # the directory X is
luneta open session --cwd <that directory>
```

If a session for X already runs (`luneta sessions`), say so, and `luneta go`
there instead, unless the user asked for a new one.

## Turn panes and tabs

Name panes as `luneta here` and `luneta sessions` print them, one command at a
time:

```sh
luneta here                                 # luneta:11
luneta turn pane luneta:11                  # that pane, into a tab of its own
luneta turn tab luneta:11 --into luneta:4   # the panes of its tab, into the tab of pane 4
```

- `turn tab` moves every pane of the tab, floating ones too, and the tab they
  left goes. `--into` takes a pane of the same session, because two tabs can
  share a name: find it in `luneta sessions`.
- What runs in the panes keeps running, your own pane too, and each keeps its
  `<session>:<pane>`. Both print the pane they were given.
- `turn` moves nobody. A user on a pane that moves goes with it. If `turn` adds
  that the user may not be on the pane they were on, tell them.
- `turn` fails and says why for a pane alone in its tab, a pane that alone holds
  a tab with floating panes open, or no room in the tab. Tell the user: don't
  work around it with `zellij action`, where the wrong order ends floating panes.
- If `turn tab` says it turned only some of the panes, those stay moved. Tell the
  user how many, and where.
- `turn pane` needs a client in that session, as `open tab` does, and moves
  nothing without one. Tell the user: `go` there and try again only if they want
  that. `turn tab` needs no client.

## Closing

Always ask before closing anything. First run `luneta close` without `--yes`.
It closes nothing, prints what would go and which agents end with it, and
exits 3. Its last line, on stderr, names the plan:

```text
delete session bipa: 1 agent, 1 busy
  bipa:0  busy     3602051  cipher-6d
close pane luneta:12: no agent
luneta: closed nothing; run it again with --yes 9c41d07e3b2a5f18 to close what it says
```

Show that to the user and ask. Run it again with that `--yes <plan>` only
after a clear yes, with the same arguments. If what would go has changed
since, say an agent went busy or a pane closed, it closes nothing, prints
what it would close now and a new plan, and exits 3 again: show the user the
new plan and ask again. Exit 1 means something failed.

- Name busy, waiting and unknown agents in the question. Their work is lost.
  An unknown agent may be at work: luneta can't tell what it is doing. A `?`
  line is one whose session file lunetad can't read, named by its pid.
- If the plan says `luneta cannot tell which agents it ends`, tell the user
  that and why, as it says: any agent could end with it.
- "All sessions" means the running ones in `luneta sessions`. Leave out the
  session `luneta here` names unless the user says so. Deleting it ends you
  too: `(this runs here)` marks your own line in the plan, and luneta closes
  it last.
- Close only what the user named or plainly meant. Never sweep up "idle"
  sessions on your own.

## Merged worktrees

When the user asks to clean up merged worktrees, run `luneta prune`, with `--repo
<dir>` for a repository other than the one you run in. It deletes nothing, prints
each worktree that would go and each that stays with why, and exits 3. Its last
line, on stderr, names the plan:

```text
delete issue-282  #297  4.5 GB  branch issue-282, and issue-282 on origin
keep issue-308 #310: it has a pane
luneta: deleted nothing; run it again with --yes 9c41d07e3b2a5f18 to delete what it says
```

Show that to the user and ask. Run it again with that `--yes <plan>` only after a
clear yes, in a later turn, with the same arguments, as for `close`. If the plan
changed, it deletes nothing, prints the new one and exits 3: show that and ask again.

Exit 0 without `--yes` means there is nothing to delete. Exit 1 means something
failed: after `--yes`, it says which worktree stayed and why, and the others went.
Tell the user. If it says it can't tell where the panes are, it deleted nothing. Don't
delete worktrees or branches yourself instead: it has checked what git can't, and the
files git ignores go with a worktree, build output included.

Never run it unless the user asked.

## Rebase onto upstream

When the user asks to rebase a branch onto main, or to bring it up to date,
run `luneta rebase`, with the worktree's directory for one other than the one
you run in. It fetches the default branch of the repository's `upstream`,
`github` or `origin` remote (else its own `main` or `master`), and rebases the
worktree's branch onto it. It pushes nothing: push only when the user asks.

- It exits 0 once the branch is on top of it, and prints a line of where it
  is now.
- It exits 1 and rebases nothing when the worktree has changes not committed,
  is on no branch, or is already in the middle of a rebase, a merge or a
  cherry-pick. Tell the user why: don't commit or stash for them.
- It exits 4 when the rebase stopped on conflicts. The rebase is left stopped,
  and an agent is started on it in a new view of the worktree, in the harness
  the user picked last. Don't resolve the conflicts yourself, and don't run
  `git rebase --abort`: that agent has them. stderr says if no agent started;
  then tell the user the rebase waits for them.


When the user asks to reset luneta, or start it from scratch, run `luneta
terminate`. It ends nothing, prints what would end on each machine, and exits 3.
Its last line, on stderr, names the plan:

```text
mac (this machine): ends 1 luneta, lunetad and luneta-mux, and 3 agents with them, 1 busy; forgets 4 projects and 9 worktrees
pi: skipped, as ssh does not reach it, and so luneta runs on there and keeps its projects: ssh: connect to host pi port 22: Operation timed out
luneta: ended nothing; run it again with --yes 5f0c2a9e81d4b37a to end what it says
```

Show that to the user and ask, as for `close`. Say that every agent on those
machines ends, busy ones too, and that you end with them: you run in a terminal
of luneta-mux. Run it again with that `--yes <plan>` only after a clear yes, in a
later turn. If the plan changed, it ends nothing, prints the new one and exits 3:
show that and ask again. Once it runs, whatever it says shows nowhere, so tell
the user before you run it that they can run `luneta terminate` from a terminal
of their own to see what is left.

In luneta's ask box, you cannot run it, as it would end the lunetad you run in:
tell the user to run `luneta terminate` in a terminal instead.

Never run it unless the user asked for exactly that. Closing sessions or panes is
`close`.

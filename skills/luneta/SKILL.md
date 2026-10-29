---
name: luneta
description: Drive luneta and zellij for the user with the `luneta` CLI - open a session, a tab or a pane, start Claude Code agents on work (one per issue, in parallel), take the user to a session or pane, and close panes or delete sessions. Use when the user asks in words to make a session to work on something, spawn an agent or a skill on a PR or issue, tackle several issues at once, go somewhere, or close sessions, tabs or panes, and `luneta` is on the PATH.
---

# Drive luneta

luneta runs zellij beside a sidebar of every session on the host, and what
the agent in each pane is doing. Its CLI reads what lunetad, its daemon, knows,
and has zellij open, move and close things. You run in a pane of zellij:
`luneta here` says which, as `<session>:<pane>`. Don't read it from
`$ZELLIJ_SESSION_NAME`: that keeps the name the session had when your pane
opened, and a rename leaves it behind.

A pane is named `<session>:<pane>`, as `luneta:11`; a session by its name alone.

## Find what the words point at

```sh
luneta here       # the pane you run in, as <session>:<pane>, by its session's name now
luneta sessions   # each session, its panes as <tab>:<pane>, their directory, branch, changes and PR
luneta agents     # each agent: <session>:<pane>, status (busy/idle/waiting/shell), pid, name
zoxide query -l   # the directories the user works in, most used first; `zoxide query <words>` picks one
```

- "X" is a directory, a repository or a session: match it against the zoxide
  list and the session names. Ask when two fit.
- "that PR", "this branch": the pane you run in, or the PR you were just
  talking about. `luneta sessions` shows which pane has which branch and PR.
- "here", "this session": the session `luneta here` names.

`sessions` and `agents` need lunetad, which luneta starts. The verbs below ask
zellij themselves and work without it. Only `close` then can't tell which
agents end.

`here`, `go`, `open session`, `close`, and `open pane`/`open tab` without
`--beside`/`--session` need to know which session you run in. If your session
was renamed and luneta can't tell which it is now (no lunetad, or one that
can't say), they fail and say so. Tell the user. Don't guess a session.

## Verbs

```sh
luneta open pane [--beside <session>:<pane>] [--cwd <dir>] [-- <command>...]
luneta open tab [--session <session>] [--cwd <dir>] [--name <name>] [-- <command>...]
luneta open session [--cwd <dir>] [--name <name>]
luneta go <session>[:<pane>]
luneta close <session>[:<pane>]... [--yes <plan>]
```

- `open pane` opens beside your pane by default, in its tab. `open tab` opens
  in your session by default. Both move nobody, and print the new pane as
  `<session>:<pane>`. `--cwd` defaults to your directory. Without a command,
  the pane runs the user's shell.
- `open tab` needs a client in that session: zellij can't size a tab
  otherwise. In a session nobody is in, use `open pane --beside` one of its panes.
- `open session` makes a session in `--cwd`, named for it as luneta's picker
  names one (`luneta-main` for a worktree `main` of `luneta`, `-2` when taken),
  and takes the user there. It prints the name.
- `go` takes the user to a session, or a pane of one.
- `go` and `open session` move the client that last typed in your session,
  which is the user who just typed to you. If the user has left your session
  since, they fail with "zellij moved nobody". Tell the user where things are
  instead.
- `close` takes a pane's tab along when nothing else holds the tab open, and
  the session when that was its only tab. The panes of a session given
  together close together: give every pane of a tab and the tab closes. It
  deletes a session given by name: kills it, and zellij keeps nothing of it to
  resurrect. It closes running sessions only. What holds your pane goes last,
  and not at all if anything before it failed to close, so that you're still
  there to tell the user.

## Start an agent

Run Claude Code as the pane's command, naming it for what it does, so the
sidebar shows it from the start:

```sh
luneta open tab --cwd ~/code/app --name issue-42 -- claude --name "issue-42" "let's tackle this one: https://github.com/o/app/issues/42"
luneta open pane --beside luneta:11 -- claude --name "thermo-nuclear-review: fix-auth" "/thermo-nuclear-review"
```

- A skill runs as the prompt: `/skill-name args`. Name it `<skill>: <branch>`.
- Put the prompt in one argument. Quote it for the shell, not for zellij.
- The `<session>:<pane>` that `open` prints is where the agent runs.
  `luneta agents` lists it once Claude Code has started.

### Several at once, one per issue

Agents working in parallel on one repository each need a worktree of their
own, or they edit the same files:

1. Read each issue (`gh issue view <n>`) for a short branch name, such as `issue-42`.
2. Make a worktree per branch with the repository's own tool. Use `wt add <branch>`
   when `wt` is on the PATH, which puts it beside the others. Otherwise use
   `git worktree add ../<branch> -b <branch>`.
3. Open a tab per worktree in the user's session, named for the branch, with
   `claude --name <branch> "<prompt>"` in it.
4. Report the tabs you opened (`<session>:<pane>` and the issue each has).
   Don't `go` anywhere unless asked.

### A skill on a PR

Find the pane whose branch is the PR's in `luneta sessions` and open a pane
beside it. If no pane has the branch, make a worktree for the PR (`wt add <PR
link>` takes a link), then open a tab there.

## New session to work on X

```sh
zoxide query X                        # the directory X is
luneta open session --cwd <that directory>
```

If a session for X already runs (`luneta sessions`), say so, and `luneta go`
there instead, unless the user asked for a new one.

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

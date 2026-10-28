<h1 align="center">luneta</h1>

<p align="center">
  <em>A telescope-style picker for <a href="https://zellij.dev">zellij</a>.<br>
  Type a few characters, press <code>Enter</code>, and you are there.</em>
</p>

<p align="center">
  <a href="https://github.com/lorenzolfm/luneta/releases/latest"><img alt="release" src="https://img.shields.io/github/v/release/lorenzolfm/luneta?style=flat-square"></a>
  <a href="#license"><img alt="license" src="https://img.shields.io/badge/license-MIT-blue?style=flat-square"></a>
  <img alt="zellij 0.45.0" src="https://img.shields.io/badge/zellij-0.45.0-brightgreen?style=flat-square">
</p>

<!--
  MEDIA SLOT — hero.gif
  Record with `vhs docs/media/hero.tape` (nix shell nixpkgs#vhs), after a
  `dev install`. The tapes press real keys against the installed .wasm, so what
  lands in a GIF is the plugin and not a mock: they assume zoxide, eza, luneta
  and lunetad on the PATH, a populated zoxide database, a running Claude Code
  agent, and a Nerd Font.
  Then point the <img> below at docs/media/hero.gif.
-->
<p align="center"><img src="docs/media/sessions.png" alt="the session picker: live and dead sessions on the left, the highlighted session's focused pane on the right" width="900"></p>

---

## What is luneta

A replacement for zellij's default session picker, with sessions, agents and directories tabs.

* Create, hop between and delete sessions
* Jump to the agent that is waiting for you
* Create a session in a directory you work in
* Keep every session and agent in sight, in a [sidebar](#luneta-in-your-terminal) beside zellij
* Start an agent on a skill with a few keys, from a [menu](#menu) of your own, and
  open a pane, a tab or a session, or close a pane or kill a session, with two
* Come back from a break to a [brief](#brief) of what your agents are doing
* [Tell an agent](#tell-an-agent) what you want in words: a session to work on
  something, an agent on each of four issues, every session closed

| `Tab` | list | `Enter` |
|---|---|---|
| 1 | **Sessions** — live and resurrectable, newest first | attach, resurrect, or create |
| 2 | **Agents** — the agents running and their status | jump to that agent's pane |
| 3 | **Directories** — where you work, in frecency order | create a session there |

Select an item to see a live preview of it.

### Agents

<!--
  MEDIA SLOT — agents.gif
  Record with `vhs docs/media/agents.tape`; swap the <img> src for docs/media/agents.gif once it exists.
-->
<p align="center"><img src="docs/media/agents.png" alt="the agent picker: the agents and their status on the left, the highlighted agent's question on the right" width="900"></p>

Know which agents need your attention, and jump straight to them.

| | status | meaning |
|---|---|---|
| 🙋 | `waiting` | it asked you something and stopped |
| ☕ | `idle` | it finished and is waiting for you |
| ⠋⠙⠹⠸ | `busy` | a spinner — the one status that ends on its own |
| 🐚 | `shell` | it is a shell |
| 🛸 | anything else | unknown, and visibly so |

Agents keep their place, as sessions do: in the order of their sessions in the
session list, the one you are in first and then the others newest first, and in
the order their panes were opened within each. Which agent wants you is the job
of the glyph, not of the order. The agent in the pane you came from is pinned
above the list.

An agent goes by the name you gave it with `/rename`, or else the title Claude
Code gives its pane, its tab or its directory. Two agents named alike are told apart by their panes, as
`luneta/main:4`, and by their sessions too when they sit in two, as
`luneta/main (review:4)`. A search finds an agent by what its row shows, the
pane that tells it apart as well, or by its session's name.

A pane holds one agent. A `claude -p` an agent runs
has its pane's ids, and luneta finds it in that pane too: the list shows the
agent you started at a prompt, or with a Claude Code that does not say how an
agent started, the one that started first, and says below it how many it left
out. An agent only guessed into a pane gives way to the one whose session
names that pane, and counts among those luneta cannot find.

`Del` closes the pane the highlighted agent is in, and its tab with it when
nothing else holds the tab open, as a floating pane does not. When the pane is a
guess, `Del` leaves it alone and
says so: the agent names a session that is still running, but its pane turned
up in another, matched by its id and directory alone.

Needs `luneta` and `lunetad` on the server's `PATH`
([building from source](#building-from-source)): the list is what lunetad,
luneta's daemon, finds. Without them, the list says why it has no agents, and
the other two lists work exactly as before.

Agent support, today:

| agent | status |
|---|---|
| [Claude Code](https://claude.com/claude-code) | supported, through the file it keeps for each agent in `~/.claude/sessions` |
| anything else | not yet |

### Directories

<!--
  MEDIA SLOT — dirs.gif
  Record with `vhs docs/media/dirs.tape`; swap the <img> src for docs/media/dirs.gif once it exists.
-->
<p align="center"><img src="docs/media/dirs.png" alt="the directory picker: zoxide directories on the left, the highlighted directory's contents on the right" width="900"></p>

Lists the directories you work in, in frecency order, and lets you create a new session right where you want to be.

A session is named for its directory. A git worktree is named for its repository too, wherever it lives, as `luneta-main`, where `main` alone would name a worktree of every repository. A bare repository's `.git` is left off, so `infra.git/master` is `infra-master`, and a worktree whose name already starts with its repository's, as `app-hotfix` beside `app`, keeps it. So does one whose name would come out too long for zellij to make a session of it.

---

### Menu

Keys, pressed one after another, that start an agent on a skill, beside the
agent whose work it is for, open a pane, a tab or a session where it works, or
delete what you are done with. The menu shows the keys of the group you are in
and what each does, as which-key does in neovim.

```text
╭─ Claude ─────────────── luneta/sidebar ─╮
│ t  /thermo-nuclear-review               │
│ m  /make-illegal-states-unrepresentable │
│ s  /tigerstyle                          │
│                                         │
│ <BS> - Back, <ESC> - Close              │
╰─────────────────────────────────────────╯
```

Your key opens the menu floating at the bottom right of the tab, for the pane
you are in. `c` opens the group you named Claude, and `t` starts Claude Code on
`/thermo-nuclear-review` in a pane of its own, beside that pane in its tab, and
in the directory it works in, which the border over the keys names. For
`/thermo-nuclear-review` that is the point: it reviews the branch checked out
there. In a pane that is no terminal, the menu has no pane to start an agent
beside, and says so in place of its keys. So it does for a pane whose directory
zellij has yet to say: an agent starts where its pane works, or not at all.
[luneta's sidebar](#luneta-in-your-terminal) has the menu too, for the row it
highlights.

An agent started on a skill goes by the skill and the branch, as
`thermo-nuclear-review: feat/sidebar`, as though you had named it with
`/rename`, so the sidebar and the picker say what it is doing from the start.

What a key starts on is the first message of the session, as
`claude "/tigerstyle"` would take it: a skill, a skill and its arguments, as
`/code-review since main`, or words. `Backspace` goes back a group, and out of
the menu from the first; `Esc` closes it.

Your keys are for the few skills you run all the time. For any other, `/` at
the top of the menu searches every skill the pane's directory can run: what you
type narrows the list, fuzzily, by a skill's name and by what it says it does,
the skills whose names match first, and `Enter` starts an agent on the one
highlighted, beside the pane, as a key does. `↓` and `↑` move; `Esc` goes back
to the keys, and so does `Backspace` once there is nothing to take back.

```text
╭─ Skills ─────────────────────────────────────── luneta/main ─╮
│ /_                                                           │
│ /code-review  Review the changes since a fixed point, on tw… │
│ /diagnosing-bugs  Diagnosis loop for hard bugs and performa… │
│ /handoff  Hand the work off to the next agent, with what it… │
│ /make-illegal-states-unrepresentable  Find the illegal stat… │
│ /prototype  Build a throwaway prototype to answer a design…  │
│ /research  Research a question against primary sources.      │
│ /thermo-nuclear-review  Review a branch as hard as it can b… │
│ /tigerstyle  Apply TigerStyle when writing or reviewing cod… │
│                                                              │
│ <↓↑> - Move, <ENTER> - Start, <ESC> - Back                   │
╰──────────────────────────────────────────────────────────────╯
```

The list shows eight skills at most, and scrolls under the cursor. It keeps
the height of the skills found as you type, so the menu keeps its size.

The skills are the ones Claude Code finds for that directory, as slash
commands, where it finds them:

- yours, in `~/.claude`, or in `CLAUDE_CONFIG_DIR`
- the project's, in the `.claude` of the directory and of each one above it,
  up to the top of its repository
- each plugin's that `claude plugin list` says is on there, as
  `/plugin:skill`, where the plugin's manifest says it keeps them

A skill is a `skills/<name>/SKILL.md`, a command a `commands/<name>.md`, and a
workflow a `workflows/<file>.js`, named as its `meta` names it. A skill whose
frontmatter says `user-invocable: false` is left out. The skills built into
Claude Code, and the ones it brings from claude.ai, are not where luneta looks,
and the list leaves them out. It is made again each time `/` opens it, which
takes a moment, mostly `claude plugin list`'s.

Every menu has luneta's own New group too, under `n`, with nothing to
configure, which opens where the pane you are in works: `n p` your shell in a
pane of its own beside that pane, `n t` a tab of its session, and `n s` a
session, which you go to. The session is named as the picker's directories
name that directory, `luneta-main` for a worktree of `luneta`, with a `-2`
when a session has that name.

```text
╭─ New ─── luneta/sidebar ─╮
│ p  new pane              │
│ t  new tab               │
│ s  new session           │
│                          │
│ <BS> Back  <ESC> Close   │
╰──────────────────────────╯
```

Every menu has luneta's own Delete group too, under `d`, with nothing to
configure: `d d` closes the pane you are in, and `d s` kills its session. In a
tab with a floating pane, the menu takes the floating focus from the pane you
were in as it opens, and zellij then says the pane under it has the focus, so
luneta cannot tell which you came from. There `d d` closes neither and says
so, and goes on doing so once the floating pane closes; `d s` still kills the
session. A pane that is the last in its tab to hold it open closes the tab
with it, floating panes and all; a pane another was opened in place of keeps
the tab open, since zellij brings that pane back. The session you are in is
killed once you are in another, the newest, which the picker lists first; with
no other session to go to, the menu says so and kills nothing. Closing the
last pane that holds a session's only tab open would end the session, so
`d d` there deletes the session, as `d s` does: it asks as for the session,
and the session you are in goes once you are in another.

When what a key deletes would end an agent that is busy or waiting on you, the
menu asks first, and says what goes:

```text
╭─ Delete ────────── luneta/main ─╮
│ Close the pane? 1 agent, 1 busy │
│                                 │
│ <y> Yes  <BS> Back  <ESC> Close │
╰─────────────────────────────────╯
```

`y`, or the key again, goes ahead; `n` or `Backspace` goes back to the keys. An
idle agent or a shell goes without asking. An agent whose status luneta does
not know, or that reports none, may be at work, and asks. The menu counts every
agent of the session, those the list leaves out too: a `claude -p` in the pane
of the agent it runs from, and one luneta cannot find a pane for, which may be
in any pane of the session that names it, and asks while it is at work. While
the daemon does not answer, luneta cannot tell, and asks every time, saying
why.

Needs `claude` on the server's `PATH`, and `git` for the branch.

### Brief

A few lines on what your agents are doing, for coming back to them after a
break, written by a fast model from what each one's transcript says last.

```text
┌─ Brief of every session ───────────── at 18:42 ─┐
│ You have 3 sessions open:                       │
│ - cipher: an agent waits for CI to finish       │
│ - luneta: the sidebar fold is done              │
│ - dotfiles: your nvim config is fixed           │
│                                                 │
│ <any key> - Close                               │
└─────────────────────────────────────────────────┘
```

In [luneta's sidebar](#luneta-in-your-terminal), `b` opens the brief's keys,
for the highlighted row, and `Backspace` goes back to the rest of the
sidebar's keys. A folded session's agents are among them.

| keys | brief |
|---|---|
| `b` `z` | every session, a line each |
| `b` `s` | the row's session, a line for each agent in it |
| `b` `h` | the agent in the row's pane, in a few lines; on a session's row, its session |

The brief shows in a box over zellij, which has the keys and the mouse until
it closes: while the model writes, `Esc` gives it up, `claude` with it, and any
other key waits, and once it shows, any key or a click closes it, leaving the
sidebar the keys and the highlight where it was. The rule along the box's top
says when you asked. Nothing is kept: each key asks again, so a brief costs
only when you ask for one. With no agent to brief you on, the sidebar says so
at its bottom.

For each agent, the model is told its session, its row's name, its status and
for how long, and its branch, as the sidebar knows them, and from the end of
its transcript: the title Claude Code gave it, what you last asked of it, and
its last few steps, what it said and what it ran. A brief reads 512 KiB of
those records at most, shared among the agents it covers: one of many agents
may tell fewer steps of each. It is asked to use those alone. luneta reads the
transcripts in the folder lunetad reads Claude Code's sessions in, where the
agents it lists come from, whatever `CLAUDE_CONFIG_DIR` luneta has, by the
session id Claude Code gives; the model is told when one was not found there.
Two agents a session names alike are told apart by their panes, as
`luneta/main:4`, and a brief tells of 32 agents at most, and says how many it
leaves out.

The model is `claude -p --model haiku`, on your Claude Code login, with no
tools, and in `--safe-mode`, so that none of your hooks, `CLAUDE.md` or MCP
servers come into it. A brief takes a few seconds. While it is asked, the
`claude` it runs has no zellij in its environment, and luneta places it in no
pane. luneta runs `claude` itself, so it needs `claude` on its own `PATH`.

### luneta in your terminal

luneta is turning inside out: from a plugin in zellij to a program that runs
zellij. Type `luneta` in a terminal, and zellij runs in it beside luneta's
sidebar: every live session, the panes in it, and what the agent in each pane
is doing.

```text
─ 2 sessions ───────────── ●1 ─  Zellij (luneta-main)  Tab #1
                                ╭ Pane #1 ────────────────────
 ▾ luneta-main ──────────── ●1  │~/P/m/luneta/main ➜
   ● fold                   3m  │
     feat/sidebar            !  │
▌    luneta/main                │
▌    main                       │
 ▾ elo ───────────────────────  │
     elo/main                   │
```

`luneta` attaches to the session named for the directory you are in, as the
picker's [directories](#directories) name it, `luneta-main` in a worktree of
`luneta`, and makes it if none runs; `luneta <name>` attaches to that one.
Run in a pane of zellij, luneta will not attach to the session that pane is
in, as zellij will not: name another. When zellij detaches, luneta ends. When
the session zellij is in ends, as zellij quits, its last pane exits, or it is
killed, luneta attaches zellij to the newest session that runs, which the
picker lists first, but never to the one luneta's own pane is in, and ends
where none is left. When zellij fails, luneta ends and says what zellij said.
zellij's keys are yours as they were, but three, which luneta takes before
zellij sees them, whichever of the two has the keys:

- `Ctrl b` goes back and forth between zellij and the sidebar. A click goes to
  whichever you click.
- `Alt Tab` goes to the next pane the sidebar lists, across sessions, and from
  the last back to the first. A folded session lists none of its panes, so it
  goes past them.
- `Ctrl q` collapses the sidebar to a strip four columns wide, zellij taking
  the rest of its columns, and opens it again, as a click on the strip does.
  `Ctrl b` opens it too, and goes into it.

Claude Code binds `Ctrl b` to send a task to the background, and neither of
the other two. zellij's default config binds `Ctrl b` to tmux mode and `Ctrl q`
to quit. luneta takes all three first: if you use those, bind them to other
keys, in zellij's config or in Claude Code's `~/.claude/keybindings.json`.

The sidebar lists every session, oldest first until you move them, the panes
in each, in the order of their tabs, and what the agent in each pane is doing,
with the branch under it, all as [`lunetad`](#building-from-source) says:
luneta starts the daemon when none runs, and the sidebar says so at its bottom
while it does not answer. `j` and `k` move, `gg` and `Home` go to the first
row, `G` and `End` to the last,
`h`, `l` and `Space` fold, `Enter` or a click goes to the pane, across sessions
too, `u` marks a pane read or unread, and `Esc` goes back to zellij. The session
zellij is in has its rule in
the accent, and `▌` marks the pane it is on. The highlight rests there while
the sidebar does not have the focus; once it has, the highlight goes with the
keys, and comes back as the sidebar gives the focus back. A move you make in
zellij, as with its keys, shows within two seconds, as the daemon asks zellij
that often; one the sidebar makes shows at once. Another client of the
session, as a plain `zellij attach`, moves no mark. A pane with no mark, of an
agent or of a command, is dimmed.

A dot one column wide marks an agent's status, where the picker has an emoji,
so that every label starts at the same column and the dot can take a colour:
`●` in the accent for `waiting`, `✓` for `idle`, the spinner for `busy`, and
`$`, dimmed, for `shell`. Any other status shows its initial.

After the dot, an icon says what runs in the pane: Claude's for an agent, a
terminal for a shell, neovim's for nvim. It has a column of its own, so every
label still starts at the same column, and a program with no icon leaves the
column blank. What runs is the agent the daemon finds in the pane, or else
what zellij says the pane's terminal has in front, by its binary's name: the
program you started, or the shell itself at its prompt. `sudo`, `env` and
`nix run` give way to what they run. lunetad asks zellij every few seconds, so
a program shows its icon a few seconds after it starts.

| icon | Nerd Fonts name | for |
|---|---|---|
| Claude | `cod-claude` | an agent, and `claude` |
| a terminal | `dev-terminal` | `bash`, `dash`, `elvish`, `fish`, `nu`, `sh`, `zsh` |
| neovim | `custom-neovim` | `nvim`, `vim` |
| rust | `dev-rust` | `cargo` |
| python | `dev-python` | `python`, `python3` |
| docker | `dev-docker` | `docker` |
| ssh | `dev-ssh` | `ssh` |
| git | `dev-git` | `git`, `lazygit` |
| a pulse | `cod-pulse` | `htop` |

The icons are a [Nerd Font](https://www.nerdfonts.com)'s, as the directory
preview's are. Claude's came with Nerd Fonts 3.5.0, and ssh's with 3.3.0: an
older font has neither, and the terminal draws a box or nothing for them, on
every agent's row for Claude's. In a font whose name ends in "Mono", an icon
takes one column; in the others it can spill into the space after it. The
sidebars drawn on this page leave the icons out. luneta has no configuration
yet to change them.

`/` searches the rows, fuzzily, as the picker does: a session by its name, a
pane by its row, its branch, its tab or its directory. What you type shows at
the bottom where the keys were. The rows left keep their order, and the
highlight lands on the best match, so `Ctrl b` `/` *a few characters* `Enter`
takes you anywhere. A folded session opens to show the panes found in it, and
a session found by its name shows all of its panes, the highlight on the one
that matches best, or else on the first. There `Enter` goes to the highlighted
row, and `Esc` clears the search. Giving zellij the keys clears it too.

The sidebar has the [menu](#menu) too, at its bottom, for the highlighted row,
with luneta's own keys but none of yours: luneta has no configuration to read
your groups from yet. While the sidebar has the keys and no menu is open, its
bottom lists what you can press there:

```text
─ luneta ──────────────────────
 /  Search
 u  Unread
 v  Reorder
 n  New ▸
 d  Delete ▸
 b  Brief ▸
```

`n p` opens your shell in a pane of its own beside the highlighted pane, in its
tab and in the directory it works in, `n t` a tab of its session there, and
`n s` a session there, named as the picker's directories name the directory,
with a `-2` when a session runs by that name. Each takes zellij there, and
gives it the keys. A tab of another session opens once zellij is there: zellij
makes a tab as big as a client of its session, and most have none attached.
`m` opens the menu on its first group, where `/` searches the skills the pane's
directory can run, and `Enter` starts Claude Code on the one highlighted,
beside the pane, named for the skill and the branch.

`d d` closes the highlighted pane, or its tab once nothing else holds the tab
open, floating panes and all. `d s` kills its session. On the last
pane that holds a session's only tab open, or on a session's row, `d d` kills
the session too: zellij would leave an empty tab in a session with no client,
and end the one zellij is in. The session zellij is in is killed once zellij
is in another, the newest, so that zellij stays on screen throughout; with no
other, it is killed with zellij in it, and luneta ends. When that would end an
agent that is busy, waiting on you, or that says nothing luneta knows, the
menu asks first, and so does every delete while lunetad
does not answer, as what it said last of the agents may be out of date: `y`,
or the key again, goes ahead, and `n` or `Backspace` goes back.
luneta asks zellij what a pane takes along as you press the key, and again as
you go ahead, so a pane that opened in the tab meanwhile keeps the tab open,
and is not closed with it. A delete leaves you in the sidebar, the highlight on
the row after once what it took is gone.

`b` opens the [brief](#brief)'s keys: `b z` briefs you on every session, `b s`
on the highlighted row's session, and `b h` on the agent in its pane, or on a
session's row, on its session.

An agent's row is named for the name you gave it with `/rename`, or else for
its topic, the title Claude Code gives its pane. A bare `Claude Code` is no
topic, and such a row goes by its tab, if you named it, or else by the
directory the agent works in. The daemon has the title without the `✳` Claude
Code leads it with, or the frame it turns there while it works, so the title
changes only as the topic does. A pane you renamed in zellij has your name for
its title, and no topic: its row goes by its tab or its directory. A pane with
no agent in it goes by what runs in it, and a shell at its prompt by its tab,
if you named it, as an agent does, or else by its directory's last two parts,
as `luneta/main`, where fish would title it `~/P/m/l/main`.

A command in a shell with the [hook](#hook-the-shell) has the marks an agent
has, once it has run for a second: the spinner while it works, `●` while it
waits for you to type, and once it ends, `✓` in green for an exit status of 0,
`✗` in red for any other, or `$` where the shell does not say, its row named
for the command and its age counting up. A program that takes the terminal
over, as an editor does, and a job stopped with `Ctrl z` have none, and nor
does a command that ends within a second. A command that ended keeps its mark
until you have seen it, as the `•` below says, and its row is then the plain
row it was. A shell without the hook has no marks.

lunetad reads what the hook writes, and the terminal of each command that
runs. The terminal says whether the command works or waits for you: how it
takes keys, and whether a process in front of it reads it. A command that
waits on the network with a blocking read looks, to Linux, like one that reads
the terminal, and has the `●` too. `sudo` and `script` run the command on a
terminal of their own, and keep yours a key at a time to pass the keys on: the
command goes by theirs. sudo's is root's, which says neither how it takes keys
nor where its processes sleep, so there a shell, an editor, a pager or `ssh` in
front has no mark, and anything else has the spinner, even as it asks you
something.

Under a pane's row, dimmed, is the branch checked out where it works, and
under the age, how its work tree stands, in the glyphs
[starship](https://starship.rs/config/#git-status) shows by default:

| | means |
|---|---|
| `=` | a conflict |
| `$` | a stash |
| `✘` | a deleted file |
| `»` | a renamed file |
| `!` | a modified file |
| `+` | a staged change |
| `?` | an untracked file |
| `⇡` `⇣` `⇕` | commits ahead of the upstream, behind it, or both |

A pane outside a repository, or on a detached `HEAD`, has no branch, and its
row stays one line. After the branch is the number of its pull request on
GitHub, as `fold #126`, in the colour `gh` lists it in: green while it is
open, grey while it is a draft, and magenta once it is merged, which says the
worktree can go. A closed one shows nothing, and neither does a branch with
none, or one `gh` cannot ask about, as where it is not installed or not
logged in. A branch too long for the line is cut short, and the number kept.
lunetad runs `gh pr view` in the directory once for a branch of a repository,
however many panes are on it, as a pane comes to the branch and then every
minute: each ask counts against GitHub's hourly limit, which every other `gh`
you run shares. A pull request opened or merged shows within a minute.

A `•` before a pane's mark says that something finished there since you were
last in it: an agent, its `✓` in green, or a
command, after its `✓`, `✗` or `$`. You were in a pane once luneta's zellij
stayed on it a second while luneta's window had the focus. So one you only
pass through keeps its `•`, and so does one luneta's zellij is on in a window
you are not using, and the pane it is on has none, as you see it. Once you
have been there, the agent keeps its `✓`, dimmed. `u` marks the highlighted
pane read or unread, as an email is: a row with the `•` is unread, whatever
gave it the `•`. On one, `u` is as going there: the `•` goes, a command's row
goes back to its shell, and an agent's `✓` is dimmed. On a row without it,
`u` gives it the `•`, which it keeps until you go to the pane again and stay
a second. On the pane you are in, that is once you leave it, or luneta's
window, and come back. A folded session shows the `•` when any of its panes
has one, unless one waits for you. Each luneta sees you only on the
pane its own zellij is on, and every luneta of one daemon shares what they
saw and marked: a file for each pane in `seen` and `unread` in luneta's
runtime folder, `$XDG_RUNTIME_DIR/luneta` or `$TMPDIR/luneta`, beside the
daemon's socket, which every luneta watches. A pane's file keeps to the
session's socket, by when it was made and its inode, so a session renamed
keeps them, and one started again under its name has none. They go once the
daemon has not listed the session for ten seconds, counted only while it
answers and says it has read all of zellij's folder, and with the runtime
folder, as the socket does.

The rule along the top counts the sessions, the agents and commands that wait
for you, the panes with a `•`, those at work, and the agents in `shell`, as
`$2`, dimmed as their `$` is, a folded session's too. A count of none is left
out. Collapsed, the
sidebar keeps three of those counts, one to a line under a short rule: the
panes with a `•`, those at work, and the agents in `shell`. The short rule is
red while lunetad does not answer, and the counts under it are the last it
gave. A `│` down the sidebar's last column, open or collapsed, parts it from
zellij: in the accent while the sidebar has the keys, and dim while it does
not.

A fold is the same in every luneta on the host, and lasts until you unfold it:
each folded session is an empty file named for it in
`$XDG_STATE_HOME/luneta/folded`, or `~/.local/state/luneta/folded`, which every
luneta watches. A fold keeps to the session's name, so a session started again
under it comes back folded.

The sidebar is 32 columns wide until you drag its edge, that `│`, with the
mouse: from 16 columns to half the terminal, zellij taking the rest. A
press on the edge leaves the keys where they are. The width is kept as you let
go, in `$XDG_STATE_HOME/luneta/width`, or `~/.local/state/luneta/width`, and
every luneta opens that wide from its next start: one already running keeps
its own. Collapsed, the strip stays four columns wide, and opens to the width
you dragged.

You can move the sessions; their panes stay in the order of their tabs. `v`
selects the session of the highlighted row, `j` and `k` grow the selection
from session to session, folded or not, and `J` and `K` move the sessions it
holds, each with its panes, one down or one up, as a visual selection and
`:m` do in vim. They stay selected, so `JJJ` goes on. `v` or `Esc` ends the
selection, `Esc` doing nothing else; any other key ends it and does what it
does.

With the mouse, a press on a session's row lifts the session once the mouse
moves a line, and the sidebar takes the keys. It shows where it lands, lit,
with its panes, the sessions around it making room. Held on the first or last
line of the rows, it scrolls them. Let go, it stays there. Let go off the
sidebar, or press `Esc`, and it goes back. A press on a pane's row lifts
nothing. What the mouse holds stays with the sidebar until it lets go, over
zellij too, and while the button is down the sidebar does nothing with a key
but `Esc`, which lets go. So a click is a press let go on the line it went
down on, and acts as it lets go.
While the search has something typed, no session lifts.

The order is the same in every luneta on the host, and lasts: it is kept in
`$XDG_STATE_HOME/luneta/order`, or `~/.local/state/luneta/order`, which every
luneta watches. A session keeps its place by its name, as a fold does, so one
started again under it comes back there, and two that share one, as zellij
versions with sockets of their own can each run, move together. A move is made
to the order as it is kept then, so it undoes none that another luneta made,
heard or not. A session the order does not have
goes after those it has, as a new session always went last. The order is the
sidebar's only: zellij's tabs keep theirs.

The picker and the floating menu are still the plugin's, and work in the
zellij luneta runs as in any other.

luneta's terminal tells zellij what it is told: the kitty keyboard protocol,
where the terminal you run luneta in speaks it, so that `Shift Enter` is not
`Enter`; what zellij copies, as OSC 52; and the session zellij is in, by the
title it gives the terminal, or else the session luneta sent it to: zellij
gives no title to a client that comes to a tab it last gave the same one. To
take zellij's client to a pane, luneta types a
`ScrollLock` let go into it first, which nothing binds, and which a shell, or
nvim as you type in it, passes over: zellij moves the client of a session that
last typed in it. With
`support_kitty_keyboard_protocol false` in zellij's config, the client cannot
read that key, and the sidebar takes it somewhere only once you have typed in
the session it is in.

### Tell an agent

`luneta`'s commands open, move and close what the menu does, so the agent you
are talking to can do it for you when you ask in words: "make me a session to
work on floresta", "tackle these four issues in parallel", "run
`/thermo-nuclear-review` on that PR", "close every session but this one". The
skill in [`skills/luneta`](skills/luneta/SKILL.md) tells Claude Code, or any
agent that reads skills, how it goes about it: what it reads to work out what
your words point at, a worktree for each agent it starts side by side, and
asking you before it closes anything.

```sh
npx skills add lorenzolfm/luneta          # or: cp -r skills/luneta ~/.claude/skills/
```

| command | does |
|---|---|
| `luneta open pane [--beside S:P] [--cwd D] [-- CMD…]` | opens a pane beside the one it runs in, or beside pane `P` of session `S`, in its tab |
| `luneta open tab [--session S] [--cwd D] [--name N] [-- CMD…]` | opens a tab of the session it runs in, or of `S` |
| `luneta open session [--cwd D] [--name N]` | makes a session and takes you there |
| `luneta go S[:P]` | takes you to a session, or to a pane of one |
| `luneta close S[:P]… [--yes PLAN]` | closes panes and kills sessions |

They are for a pane of zellij, where an agent runs, and take what it runs in
for the pane, the session and the directory you leave out. A pane or a tab
opens with `CMD` running in it, or your shell, and nobody moved, and `open`
says which it is, as `luneta:12`, as `luneta agents` names an agent's pane. A
tab opens only in a session someone is in: zellij makes a tab as big as a
client of its session, and one with no room for its panes in a session with
none. A session is named as the picker names its directory, with a `-2` when
a session runs by that name.

`go` and `open session` move the client that last typed in the session they
run in, which is you, where you just asked an agent there for something:
zellij moves no other, as it does for luneta's own sidebar. They wait to see
you there, and say that zellij moved nobody where you have left that session
since. zellij's client makes the session, so what runs in it has your
environment, not the agent's.

`close` takes a pane's tab along once nothing else holds the tab open, and the
session where that is its only tab, as `d d` does. What it is given in a
session closes together, so the panes that are all that holds a tab open close
the tab. Without `--yes`, it says what each would close and which agents would
end with it, as lunetad lists them, and the plan to tell `--yes`, closes
nothing, and exits 3, so that an agent shows you that and asks before it goes
ahead:

```text
kill session bipa: 1 agent, 1 busy
  bipa:0  busy     3602051  cipher-6d
close pane luneta:12: no agent
luneta: closed nothing; run it again with --yes 9c41d07e3b2a5f18 to close what it says
```

`--yes` closes only that plan, as the menu's `y` does only what it asked about:
where an agent has gone to work since, a pane has closed, or a session was
made again by the same name, it closes nothing, and says what it would close
now, and its plan.

What holds the pane it runs in goes last, and its agent's line says `(this
runs here)`: the agent ends with it. It kills only sessions that run, not
those zellij keeps to resurrect.

## Getting started

### Requirements

| | | needed for |
|---|---|---|
| **zellij 0.45.0** | | everything |
| [`claude`](https://claude.com/claude-code) | optional | the menu and the brief |
| `git` | optional | the branch in the name of an agent the menu starts |
| [`zoxide`](https://github.com/ajeetdsouza/zoxide) | optional | the directory list |
| [`eza`](https://github.com/eza-community/eza) | optional | the directory preview |
| [`git`](https://git-scm.com) | optional | the branch and its status under a pane in [luneta's sidebar](#luneta-in-your-terminal) |
| [`gh`](https://cli.github.com), logged in | optional | the number of a branch's pull request under a pane in [luneta's sidebar](#luneta-in-your-terminal) |
| a [Nerd Font](https://www.nerdfonts.com), 3.5.0 or later | optional | the icons in luneta's sidebar, and in the directory preview |
| `luneta` and [`lunetad`](#building-from-source), side by side | optional | the agents, in the agent list, the menu, luneta's sidebar and the brief |
| the [hook](#hook-the-shell) in fish | optional | a command's spinner in luneta's sidebar, and how it ended |

The session list needs nothing but zellij. The optional tools are looked up by
name on the `PATH` of the zellij **server**, which is inherited from whatever
started it — on a long-lived session that can be older than your shell profile.
The font is not looked up: it is the terminal's you attach from, which over SSH
is the one on your own machine, not the server.

### Install

```kdl
plugins {
    // ...
    luneta location="https://github.com/lorenzolfm/luneta/releases/download/v0.2.2/luneta-0.2.2.wasm"
}
```

Zellij will prompt once for `RunCommands`, `ReadApplicationState`,
`ChangeApplicationState` and `OpenTerminalsOrPlugins`. `RunCommands` is how
luneta reaches `zoxide`, `eza`, `git`, `zellij` and `luneta`, and how
the [menu](#menu) starts `claude`; `OpenTerminalsOrPlugins` is how the menu
opens your shell beside a pane.
Every mode asks for all four, since zellij keeps one grant for a plugin.

<details>
<summary><strong>Prefer to verify the bytes first?</strong></summary>

Zellij does not check a checksum, a signature or an attestation on a plugin it
downloads — it fetches the URL and runs the bytes. Every release ships a
`SHA256SUMS` and a keyless build provenance attestation, so you can download,
check, and install from disk instead:

```sh
v=0.2.2
gh release download "v$v" -R lorenzolfm/luneta -p 'luneta-*.wasm' -p SHA256SUMS
sha256sum -c SHA256SUMS
gh attestation verify "luneta-$v.wasm" -R lorenzolfm/luneta
install -m 0644 "luneta-$v.wasm" ~/.local/share/zellij/plugins/luneta.wasm
```

`sha256sum -c` proves the bytes are complete. `gh attestation verify` proves where
they came from: built by `ci.yml`, at that commit, in this repo, on a GitHub
runner. Then point `config.kdl` at the `file:` path instead of the URL.

</details>

### Bind keys

Bind one key to open luneta and use `Tab` to cycle between the tabs, or bind separate keys that open the agents and directories tabs directly.

```kdl
keybinds {
    normal {
        bind "Ctrl j" {
            LaunchOrFocusPlugin "luneta" {
                floating true
                move_to_focused_tab true
            }
            SwitchToMode "normal"
        }
        bind "Ctrl a" {
            LaunchOrFocusPlugin "https://github.com/lorenzolfm/luneta/releases/download/v0.2.2/luneta-0.2.2.wasm" {
                floating true
                move_to_focused_tab true
            }
            MessagePlugin "https://github.com/lorenzolfm/luneta/releases/download/v0.2.2/luneta-0.2.2.wasm" {
                name "screen"
                payload "agents"
            }
            SwitchToMode "normal"
        }
        bind "Ctrl d" {
            LaunchOrFocusPlugin "https://github.com/lorenzolfm/luneta/releases/download/v0.2.2/luneta-0.2.2.wasm" {
                floating true
                move_to_focused_tab true
            }
            MessagePlugin "https://github.com/lorenzolfm/luneta/releases/download/v0.2.2/luneta-0.2.2.wasm" {
                name "screen"
                payload "dirs"
            }
            SwitchToMode "normal"
        }
    }
}
```

### Open the menu

The menu is the plugin in `mode "menu"`. Its keys are a `menu`, a node to a
key: a key's one string is what it starts an agent on, or, with keys inside
it, the name of the group they make. At the top of the menu, `/`, `n` and `d`
are luneta's, for its search, its New group and its Delete group, and a menu
that gives one of them a key of yours says so. Give them to the alias:

```kdl
plugins {
    luneta location="https://github.com/lorenzolfm/luneta/releases/download/v0.2.2/luneta-0.2.2.wasm" {
        menu {
            c "Claude" {
                t "/thermo-nuclear-review"
                m "/make-illegal-states-unrepresentable"
                s "/tigerstyle"
            }
        }
    }
}

keybinds {
    normal {
        bind "Alt m" {
            LaunchOrFocusPlugin "luneta" {
                floating true
                move_to_focused_tab true
                mode "menu"
            }
            SwitchToMode "normal"
        }
    }
}
```

A key is one character, pressed bare. The key that opens the menu needs a
modifier: zellij hands a bare `m` to the pane you are in. A menu luneta cannot
read says what is wrong with it where its keys would be, and closes on any key.

A `mode` luneta does not know does nothing but say so, in its pane if it has
one and in zellij's log.

### Hook the shell

A command in a shell has its [marks in the sidebar](#luneta-in-your-terminal) once the shell
says what it runs and how it ended. luneta cannot hook your shell for you: put
this in `~/.config/fish/conf.d/luneta.fish`.

```fish
# luneta: say what runs in each zellij pane, and how it ended.
if status is-interactive; and set -q ZELLIJ_PANE_ID; and test -t 0
    and test -n "$XDG_RUNTIME_DIR$TMPDIR"
    # A window opened from a pane has the pane's variables, and a terminal of
    # its own: a shell there is not the pane's.
    and not begin
        test "$__luneta_pane" = "$ZELLIJ_SESSION_NAME $ZELLIJ_PANE_ID"
        and test "$__luneta_pane_tty" != (tty)
    end
    set -gx __luneta_pane "$ZELLIJ_SESSION_NAME $ZELLIJ_PANE_ID"
    set -gx __luneta_pane_tty (tty)
    set -l root $XDG_RUNTIME_DIR
    test -n "$root"; or set root $TMPDIR
    set -g __luneta_root $root/luneta
    set -g __luneta_dir $__luneta_root/commands/$ZELLIJ_SESSION_NAME
    # The shell's own file, for its pane and when it started.
    set -q __luneta_file; or set -g __luneta_file $__luneta_dir/$ZELLIJ_PANE_ID.(date +%s)

    function __luneta_say
        # Yours alone, and no link to another: what runs says what you type.
        test -d $__luneta_root; or mkdir -m 700 $__luneta_root
        test -O $__luneta_root; and not test -L $__luneta_root; or return
        test -d $__luneta_dir; or mkdir -p $__luneta_dir; or return
        # Moved into place whole, as the sidebar reads it.
        printf '%s\n' "$argv" >$__luneta_file.new; and mv -f $__luneta_file.new $__luneta_file
    end

    function __luneta_going --on-event fish_preexec
        set -g __luneta_jobs (jobs --group)
        set -g __luneta_command (string replace -a \n ' ' -- $argv[1])
        # `exec` gives the pane to another program, and nothing would say how
        # this ended.
        if string match -qr '(^|[;&|]|\b(and|or|not))\s*exec(\s|$)' -- $argv[1]
            rm -f $__luneta_file
            return
        end
        # `fg` brings back the last job, which is what runs.
        if string match -qr '^\s*fg\s*$' -- $argv[1]
            set -l job (jobs --last | string split \t)
            if set -q job[1]
                set __luneta_command $job[-1]
                set __luneta_jobs (string match -v -- $job[2] $__luneta_jobs)
            end
        end
        __luneta_say going (date +%s) $__luneta_pane_tty $__luneta_command
    end

    function __luneta_ended --on-event fish_postexec
        set -l s $status
        # `fg` says 0 however the job it brought back ended.
        string match -qr '^\s*fg(\s|$)' -- $argv[1]; and set s -
        # `Ctrl z` stops the job, and gives the prompt back with a status of 0.
        set -l job (jobs --last --group)
        if set -q job[1]; and not contains -- $job $__luneta_jobs
            and ps -o stat= -p $job | string match -q 'T*'
            __luneta_say stopped (date +%s) $__luneta_command
        else
            __luneta_say ended (date +%s) $s $CMD_DURATION $__luneta_command
        end
    end

    function __luneta_gone --on-event fish_exit
        rm -f $__luneta_file
    end
end
```

A shell started before the hook was there has none: start it again, or
`source` the file in it.

The hook keeps a file for each shell, of one line, at
`$XDG_RUNTIME_DIR/luneta/commands/<session>/<pane>.<started>`, or under
`$TMPDIR` where that is not set, and takes it away as the shell exits. With
neither, it keeps none. The folder `luneta` is yours alone: the hook makes it
so, and neither it nor lunetad uses one that another user made, or a link,
as anyone could make in a `TMPDIR` of `/tmp`. It writes the line beside it first and moves it into place, so that
lunetad never reads half of one. Another shell can be hooked to write the same
line, which is one of

```text
going <since> <tty> <command>
ended <at> <status> <took> <command>
stopped <at> <command>
```

with `<started>`, `<since>` and `<at>` in seconds since the epoch, `<took>` in
milliseconds, `<status>` the exit status, or `-` where the shell cannot say it,
as fish cannot after `fg`, `<tty>` the path `tty` prints, the command on one
line, and `stopped` for a job stopped with `Ctrl z`. The sidebar has them from
lunetad, which keeps its socket in the same folder: it reads a file as it
changes, and the terminal of each command that runs every second, through `ps`
on its `PATH`.
A pane's shell is the one that started last on its terminal, and one that
started before its session was made is another session's. A shell in a window
opened from the pane has the pane's variables, which the hook passes on, and a
terminal of its own: it says nothing.

A shell started before its session was renamed goes on writing under the old
name, and so does one started again in a pane the session had then, as the
pane's variables still say that name; a pane opened since says the new one.
lunetad follows the session to its new name, as it does an agent, when it saw
the rename: a session renamed before lunetad started leaves those shells
unmarked. A session made later under the old name has the shells started in it
since, and so does a shell started since in an old pane of the one renamed,
which says the old name too.

A shell killed with `kill -9`, or by the kernel when memory runs out, cannot
take its file away, and the file stays until you log out. One that ran in a
pane's shell stands in front of the pane's own, in the sidebar, until you
take its file away with `rm`. lunetad drops a command going on a terminal that
is gone, but only until another terminal is opened, which takes the gone
one's number.

---

## Usage

Press your key, type, press `Enter`.

| key | does |
|---|---|
| *any character* | filter, fuzzily |
| `↓` / `↑` | move the highlight |
| `Enter` | act on the highlighted row — the prompt says which action |
| `Tab` / `Shift-Tab` | next / previous list, keeping the search term |
| `Ctrl-r` | rename the session you are in |
| `Del` | kill a live session, or delete a dead one; close an agent's pane, and its tab once nothing else holds it open |
| `Backspace` | delete a character |
| `Esc` / `Ctrl-c` | close |

In the menu, for the pane you opened it from:

| key | does |
|---|---|
| *a key it shows* | open its group, or start its agent |
| `n p` | open your shell in a pane beside the pane |
| `n t` | open a tab of the pane's session |
| `n s` | start a session where the pane works, and go there |
| `d d` | close the pane |
| `d s` | kill the pane's session |
| `y`, or the key again | go ahead with a delete that asked first |
| `n` | back to the keys, from a delete that asked first |
| `Backspace` | back a group, or close from the first; back to the keys from a delete that asked |
| `Esc` / `Ctrl-c` | close |

---

## Building from source

Only if you would rather not install a downloaded `.wasm`.

```sh
nix develop          # or: direnv allow, once
dev install          # builds and installs to ~/.local/share/zellij/plugins/luneta.wasm
```

The flake pins the rust toolchain with `wasm32-wasip1` added; a system rustc
without that target fails with `error[E0463]: can't find crate for 'std'`.

For the artifact a release ships — every dependency at the revision `Cargo.lock`
names, built in a sandbox, no devshell and no checkout needed:

```sh
nix build github:lorenzolfm/luneta   # -> result/luneta.wasm
```

The flake builds on `x86_64-linux`, `aarch64-linux` and `aarch64-darwin`. A
release is the `x86_64-linux` build; the others work the same, but are not the
same bytes, as the paths a build runs in are written into the `.wasm`. On any
other system, build with cargo instead, on the toolchain `rust-toolchain.toml`
names and with `protoc` on the `PATH`, for the daemon's protocol the plugin
reads: `cargo build --release --target wasm32-wasip1` writes
`target/wasm32-wasip1/release/luneta-plugin.wasm`, which you install as
`~/.local/share/zellij/plugins/luneta.wasm`.

Then point `config.kdl` at `file:` instead of the release URL:

```kdl
plugins {
    luneta location="file:~/.local/share/zellij/plugins/luneta.wasm"
}
```

`nix build .#lunetad` builds `lunetad`, the daemon that holds what luneta
knows of a host, and `luneta`: zellij beside a sidebar, as
[luneta in your terminal](#luneta-in-your-terminal) has it, and with a command,
the CLI that talks to the daemon
([#95](https://github.com/lorenzolfm/luneta/issues/95)). With `luneta` on the
zellij server's `PATH`, the picker takes where each pane works and the agents
from the daemon, through `luneta watch`, as they change, and the menu asks the
daemon once as it opens. Without it, the picker has no agents, and says why; it
asks the daemon again every few seconds.

Nothing has to start the daemon, or run it as a service: `luneta` and `luneta
watch` start `lunetad` when none answers, as zellij and tmux start their
servers. They start the `lunetad` beside them, which the same build made, or
else the first on the `PATH`, detached, with what it says in `lunetad.log`
beside its socket, in `$XDG_RUNTIME_DIR/luneta` (`$TMPDIR/luneta` on macOS).
It lives as long as zellij does, not as long as a client is attached: once
zellij has had no session for 30 seconds, it ends. A daemon older than them,
as an upgrade leaves running, they stop, and start their own in its place. A
newer one they keep, so that two versions in use at once never take turns at
it: after a rollback, the newer daemon runs on until zellij's last session ends.
One that reads zellij's sessions in another folder than their zellij keeps
them in, as one started with another `ZELLIJ_SOCKET_DIR` does, they neither
ask nor stop, and say so: one of theirs in its place would leave out the
sessions of every luneta that reaches it.
`luneta info` says which daemon runs, `luneta agents` lists the agents it
finds, and `luneta sessions` the sessions, their panes and the branch each
works on, with its pull request; none of the three starts one, and nor does
`luneta close`, which says it cannot tell the agents it would end while none
runs. The commands an agent [drives luneta](#tell-an-agent) with ask zellij
itself. The daemon asks
`zellij`, `git` and `gh` on its `PATH`, which is the zellij server's when the
plugin started it.

## License

MIT. Zellij is MIT-licensed too.

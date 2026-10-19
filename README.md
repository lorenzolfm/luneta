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
  lands in a GIF is the plugin and not a mock: they assume zoxide, eza and
  claude-ps on the PATH, a populated zoxide database, a running Claude Code
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
* Keep every session and agent in sight, in a [sidebar](#sidebar) pinned to each tab
* Start an agent on a skill with a few keys, from a [menu](#menu) of your own, and
  open a pane, a tab or a session, or close a pane or kill a session, with two
* Come back from a break to a [brief](#brief) of what your agents are doing

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

`Del` closes the pane the highlighted agent is in, and its tab with it when
nothing else holds the tab open: neither the sidebar's column nor a floating
pane does. That goes for a session nobody is attached to as well, where no dock
is there to close the tab. When the pane is a guess, `Del` leaves it alone and
says so: the agent names a session that is still running, but its pane turned
up in another, matched by its id and directory alone.

Needs [`claude-ps`](https://github.com/lorenzolfm/claude-ps) on the server's
`PATH`. Without it, the other two lists work exactly as before.

Agent support, today:

| agent | status |
|---|---|
| [Claude Code](https://claude.com/claude-code) | supported, through [`claude-ps`](https://github.com/lorenzolfm/claude-ps) |
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

### Sidebar

The same plugin, docked to the left of the tab you are in instead of floating
over it: every live session, the panes in it, and what the agent in each pane
is doing. One sidebar for the whole session, which follows you from tab to tab.

```text
─ 3 sessions ────── ●1 ✓1 ⠋3 ─

 ▾ bipa ────────────────── ●1
   ✓ handoff-eng-11259-p… 13h
     eng-11259-pr4b         ⇡
   ● dda-bank-slip-paid-…  3m
     dda-bank-slip-paid    !?
   ⠋ Handoff pix outflow… 12s
     pix-outflow-readback !+?
 ▸ floresta ─────────────── ⠋
 ▾ unnamed ──────────────────
▌  ⠋ orca-copy            12s
▌    main
     misc/unnamed
```

Sessions keep their place, oldest first, so a row never moves under your eye
or your mouse. Which agent wants you is the job of the dot, the count on its
session's row, and the `next` key — not of the order.

The sidebar marks a status with a dot one column wide where the picker has an
emoji, so that every label starts at the same column and the dot can take a
colour: `●` in the accent for `waiting`, `✓` in green for `idle`, the spinner
for `busy`, and `$`, dimmed, for `shell`. Any other status shows its initial.
The rule along the top counts the live sessions, and the agents across all of
them in each of the first three, a folded session's too. A count of none is
left out.

An agent's row is named for what it is doing: the name you gave it with
`/rename`, or else the title Claude Code gives its pane. That title is a bare
`Claude Code` until the agent has a topic, and some never get one; such a row
goes by its tab, if you named it, or else by the directory the agent works in.
A pane with no agent in it is dimmed. `▌` marks the pane you are in, and the
highlight stays on it while the sidebar does not have the focus; once it has,
the highlight goes with the cursor. The session you are in has its rule in the
accent. A shell at its prompt is named for its directory's last two parts, as
`luneta/main`, where fish would title it `~/P/m/l/main`.

Under a pane's row, dimmed, is the branch checked out in the directory the pane
works in, and under the age, how its work tree stands, in the glyphs
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

A pane outside a repository, or on a detached `HEAD`, has no branch, and its row
stays one line. `git` is asked again every five seconds, so a change or a
checkout shows a few seconds later.

Click a pane's row, or focus the sidebar and press `Enter`, to go there — across
tabs and sessions alike. One click does it, whether the sidebar has the focus or
not. A session's row heads its panes, and is no place to go: the highlight
passes over it, from the last pane of one session to the first of the next, and
a click on its name does nothing, but bring the focus into the sidebar, as a
click anywhere on it does. On a session's row, the `▾` or `▸` folds or
unfolds the session. A folded session stands in for its panes: the highlight
stops on its row, and it switches to the session, on the pane it last had the
focus on; the session you are in takes you back to your pane. It keeps its
count and shows the dot of the agent that wants you most. A fold holds in the
sidebar of every session, and after zellij restarts: the sidebars keep their
folds in the cache zellij gives the plugin, one for each location it loads
from, so every session's `load_plugins` names the same one. A session the
picker deletes takes its fold with it: one made later by the same name starts
unfolded.

The wheel scrolls the list and leaves the highlight where it is, until you go
somewhere else or press a key. A click on a row it scrolled to lights the row
where it stands, and the list stays put. Leaving a session from the sidebar
hands its focus back to the pane you came from first, so the session has you
back there and not in the sidebar.

Press `/` to search, as in less and vim. The tree narrows to the rows that
match what you type, fuzzily, as the picker does: a session by its name, a pane
by its row, its branch, its tab or its directory. The rows left keep their order, and the
highlight lands on the best match, so `Ctrl b` `/` *a few characters* `Enter`
takes you anywhere. A folded session opens to show the panes found in it, and
a session found by its name shows all of its panes, the highlight on the one
that matches best, or else on the first.

One key goes to the next pane listed, and from the last back to the first, so
pressing it again and again takes you down the sidebar, across sessions. A
folded session's panes are left out.

The sidebar and the picker keep your place as you go from one to the other.
A key that opens the picker from the sidebar opens it on the row you had
highlighted: Sessions on its session, Agents on the agent in its pane, or else
the first agent in its session. `Ctrl b` from the picker closes it and
highlights, in the sidebar, the agent the picker had, unfolding its session, or
the first pane of the session it had, or that session's row while it is folded.

One key hides the sidebar, the panes beside it taking its columns in every tab,
and brings it back. Either goes for every session, within a second, and for a
session started later: the docks keep it in the plugin's cache, beside the
folds.

### Menu

Keys, pressed one after another, that start an agent on a skill, beside the
agent whose work it is for, open a pane, a tab or a session where it works, or
delete what you are done with. The menu shows the keys of the group you are in
and what each does, as which-key does in neovim.

```text
─ 2 sessions ──────────── ⠋1 ─

 ▾ luneta ───────────────────
▌  ⠋ fold                 12s
▌    feat/sidebar           !
     luneta/main
     main
 ▸ elo ──────────────────────
─ Claude ──── luneta/sidebar ─
 t  /thermo-nuclear-review
 m  /make-illegal-states-unr…
 s  /tigerstyle
 <BS> - Back, <ESC> - Close
```

In the sidebar, the menu's keys are the sidebar's. On the row of a pane, here
the agent `fold`, `c` opens the group you named Claude at the bottom of the
sidebar, and `t` starts Claude Code on `/thermo-nuclear-review` in a pane of its
own, beside that pane in its tab, and in the directory it works in, which the
rule over the keys names. For `/thermo-nuclear-review` that is the point: it
reviews the branch checked out there. A row of another tab takes you to that
tab, and a row of another session starts the agent in that session and takes
you there, handing the session you leave the pane you came from. A key the
sidebar has a use of its own for, as `j`, `/` or `b`, stays the sidebar's: `m`
opens the menu on its first group, where every key is the menu's. The menu
closes when the sidebar loses the focus, and with a click.

`?` opens it on its first group too, and under its keys, the sidebar's own
commands: `/` to search, and the [brief](#brief)'s keys, in a group `b` opens.
A key of the menu that a command has stays the command's, as it does in the
sidebar.

```text
─ luneta ──── luneta/sidebar ─
 c  Claude ▸
 n  New ▸
 d  Delete ▸
 /  Search
 b  Brief ▸
 <ESC> - Close
```

Your key opens the same menu floating at the bottom right of the tab, for the
pane you are in. In the sidebar, or any pane that is no terminal, it has no
pane to start an agent beside, and says so in place of its keys. So it does for
a pane whose directory zellij has yet to say: an agent starts where its pane
works, or not at all.

An agent started on a skill goes by the skill and the branch, as
`thermo-nuclear-review: feat/sidebar`, as though you had named it with
`/rename`, so the sidebar and the picker say what it is doing from the start.

What a key starts on is the first message of the session, as
`claude "/tigerstyle"` would take it: a skill, a skill and its arguments, as
`/code-review since main`, or words. `Backspace` goes back a group, and out of
the menu from the first; `Esc` closes it.

Every menu has luneta's own New group too, under `n`, with nothing to
configure, which opens where the highlighted pane works: `n p` your shell in a
pane of its own beside that pane, `n t` a tab of its session, and `n s` a
session, which you go to. The session is named as the picker's directories
name that directory, `luneta-main` for a worktree of `luneta`, with a `-2`
when a session has that name. The floating menu does the same for the pane you
are in. A pane of another session opens there, and you go to it, and so does a
tab of a session with a client. A tab of a session with none, as most are,
opens once you are there: zellij makes a tab as big as a client of its
session, and one with none would have no room for its panes. A session's row
has no pane, nor a directory to open in, and its `n` keys say so.

```text
─ New ─────────── luneta/main ─
 p  new pane
 t  new tab
 s  new session
 <BS> - Back, <ESC> - Close
```

Every menu has luneta's own Delete group too, under `d`, with nothing to
configure: `d d` deletes the highlighted row, and `d s` kills its session. On a
pane's row, `d d` closes the pane; on the row of a session that lists none of
its panes, folded or with none to list, it kills the session. The
floating menu does the same for the pane you are in, except in a tab with a
floating pane: the menu takes the floating focus from the pane you were in as
it opens, and zellij then says the pane under it has the focus, so luneta cannot
tell which you came from. There `d d` closes neither and says so, and goes on
doing so once the floating pane closes; `d s` still kills the session. A pane that is the last in
its tab to hold it open closes the tab with it, so no tab is left with nothing
in it but the sidebar's column; a pane another was opened in place of keeps the
tab open, since zellij brings that pane back. A pane of another session closes
through `zellij --session`, and the sidebar's highlight moves to the row after
it once it is gone. The session you are in is killed once you are in another,
the newest, which the picker lists first; with no other session to go to, the
menu says so and kills nothing. Closing the last pane that holds a session's
only tab open would end the session, so `d d` there deletes the session, as
`d s` does: it asks as for the session, and the session you are in goes once
you are in another.

When what a key deletes would end an agent that is busy or waiting on you, the
menu asks first, and says what goes:

```text
─ Delete ──────── luneta/main ─
 Close the pane? 1 agent, 1
 busy
 <y> Yes <BS> Back <ESC> Close
```

`y`, or the key again, goes ahead; `n` or `Backspace` goes back to the keys. An
idle agent or a shell goes without asking. An agent whose status luneta does
not know, or that reports none, may be at work, and asks. Without `claude-ps`,
luneta cannot tell, and asks every time.

Needs `claude` on the server's `PATH`, and `git` for the branch.

### Brief

A few lines on what your agents are doing, for coming back to them after a
break, written by a fast model from what each one's transcript says last.

```text
┌ luneta: brief of every session ──────────────────────────────────┐
│Brief of every session, at 18:42                                  │
│                                                                  │
│You have 3 sessions open:                                         │
│- cipher: an agent waits for CI to finish, to report how it went  │
│- luneta: the sidebar fold is done and waits for your review      │
│- dotfiles: your nvim config is fixed                             │
│                                                                  │
│<any key> - Close                                                 │
└──────────────────────────────────────────────────────────────────┘
```

In the sidebar, `b` opens `?` on the brief's keys, for the row the cursor is
on, and `Backspace` goes back to the rest of `?`:

| keys | brief |
|---|---|
| `b` `z` | every session, a line each |
| `b` `s` | the row's session, a line for each agent in it |
| `b` `h` | the agent in the row's pane, in a few lines; on a session's row, its session |

The brief opens floating over the tab, and any key closes it. It says when it
was made, and nothing is kept: each key asks again, so a brief costs only when
you ask for one. With no agent to brief you on, or transcripts that could not
be read, the sidebar says so where the keys were.

For each agent, the model is told its session, its row's name, its status and
for how long, and its branch, as the sidebar knows them, and from the last
256 KiB of its transcript: the title Claude Code gave it, what you last asked
of it, and its last few steps, what it said and what it ran. It is asked to
use those alone. The transcript is the one Claude Code keeps in
`~/.claude/projects`, or under `CLAUDE_CONFIG_DIR`, by the session id
claude-ps gives; the model is told when it was not found there. Two agents a
session names alike are told apart by their panes, as `luneta/main:4`, and a
brief tells of 32 agents at most, and says how many it leaves out.

The model is `claude -p --model haiku`, on your Claude Code login, with no
tools, and in `--safe-mode`, so that none of your hooks, `CLAUDE.md` or MCP
servers come into it. A brief takes a few seconds. While it is asked, the
`claude` it runs has no zellij in its environment, and claude-ps places it in
no pane.

Needs `claude` and `claude-ps` on the server's `PATH`.

## Getting started

### Requirements

| | | needed for |
|---|---|---|
| **zellij 0.45.0** | | everything |
| [`claude-ps`](https://github.com/lorenzolfm/claude-ps) | optional | the agent list and the brief |
| [`claude`](https://claude.com/claude-code) | optional | the menu and the brief |
| `git` | optional | the branch in the name of an agent the menu starts |
| [`zoxide`](https://github.com/ajeetdsouza/zoxide) | optional | the directory list |
| [`eza`](https://github.com/eza-community/eza) | optional | the directory preview |
| [`git`](https://git-scm.com) | optional | the branch and its status under a pane in the sidebar |

The session list needs nothing but zellij. The optional tools are looked up by
name on the `PATH` of the zellij **server**, which is inherited from whatever
started it — on a long-lived session that can be older than your shell profile.

### Install

```kdl
plugins {
    // ...
    luneta location="https://github.com/lorenzolfm/luneta/releases/download/v0.2.2/luneta-0.2.2.wasm"
}
```

Zellij will prompt once for `RunCommands`, `ReadApplicationState`,
`ChangeApplicationState` and `OpenTerminalsOrPlugins`. `RunCommands` is how
luneta reaches `zoxide`, `eza`, `claude-ps`, `git` and `zellij` itself, and how
the [menu](#menu) starts `claude`; `OpenTerminalsOrPlugins` is how the
[dock](#dock-the-sidebar) opens the sidebar.
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
it, the name of the group they make. `d` is luneta's, for its Delete group, and
a menu that gives it a key of yours says so. Give them to the alias, so that
the key that opens the menu and the sidebar show the same keys:

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

The sidebar has the keys of the dock that opened it, so load the dock by the
alias too, as [below](#dock-the-sidebar).

A key is one character, pressed bare. The key that opens the menu needs a
modifier: zellij hands a bare `m` to the pane you are in. A menu luneta cannot
read says what is wrong with it where its keys would be, and closes on any key.
A sidebar from a dock loaded by its location has none of the alias's keys, and
its `m` says to load the dock by the alias.

### Dock the sidebar

The sidebar is one pane for the whole session. The dock keeps it in the tab on
screen: the same plugin, in the background, loaded with every session in
`config.kdl`:

```kdl
load_plugins {
    luneta {
        mode "dock"
    }
}
```

`luneta` is the alias from [Install](#install), so the dock, and the sidebar it
opens, have whatever the alias is given: the [menu](#open-the-menu)'s keys, for
one.

A `mode` luneta does not know does nothing but say so, in its pane if it has
one and in zellij's log.

Every tab keeps a column for the sidebar to dock into: a placeholder, a pane that
runs `tail -f /dev/null`. Give it to every tab in a layout:

```kdl
// ~/.config/zellij/layouts/sidebar.kdl
layout {
    pane size=1 borderless=true {
        plugin location="zellij:tab-bar"
    }
    pane split_direction="vertical" {
        pane size=32 command="tail" {
            args "-f" "/dev/null"
        }
        pane focus=true
    }
    pane size=2 borderless=true {
        plugin location="zellij:status-bar"
    }
}
```

Leave the placeholder its border. Each time zellij arranges a tab, the pane in
the placeholder's place takes its `borderless`, the sidebar too, and zellij takes
a borderless plugin on the edge of the tab for part of its own frame, like the
tab bar, and draws no line between it and the panes beside it.

When another tab comes on screen, the sidebar hands its tab back the placeholder
and takes the new tab's, in its place, so the panes beside it never change size.
The dock opens the sidebar when the session starts, and again if the tab that
held it closes. Closing the sidebar hides it, in every session. A tab closes
with the last pane beside its column, as it would without one, and the session
ends with its last tab.

zellij opens every new tab of a session from the layout the session was made
with, so `default_layout "sidebar"` in `config.kdl` gives every tab of every new
session its column. Leave the `pane` in: a tab of nothing but the placeholder
closes as soon as it opens, so a session from a layout of nothing but a
`default_tab_template` ends as soon as it starts. A session that is already
running keeps its own layout, with no column to dock into; start it again.

A layout of your own leaves zellij's swap layouts behind: the ones `Alt [` and
`Alt ]` step through, and that arrange a tab as you open panes in it. Give the
layout its own, with the placeholder in a column of its own, in a file beside it.
zellij's, with every pane count one higher for the column:

```kdl
// ~/.config/zellij/layouts/sidebar.swap.kdl
tab_template name="ui" {
    pane size=1 borderless=true {
        plugin location="zellij:tab-bar"
    }
    pane split_direction="vertical" {
        pane size=32 command="tail" {
            args "-f" "/dev/null"
        }
        pane {
            children
        }
    }
    pane size=2 borderless=true {
        plugin location="zellij:status-bar"
    }
}

swap_tiled_layout name="vertical" {
    ui max_panes=6 {
        pane split_direction="vertical" {
            pane
            pane { children; }
        }
    }
    ui max_panes=9 {
        pane split_direction="vertical" {
            pane { children; }
            pane { pane; pane; pane; pane; }
        }
    }
    ui max_panes=13 {
        pane split_direction="vertical" {
            pane { children; }
            pane { pane; pane; pane; pane; }
            pane { pane; pane; pane; pane; }
        }
    }
}

swap_tiled_layout name="horizontal" {
    ui max_panes=5 {
        pane
        pane
    }
    ui max_panes=9 {
        pane {
            pane split_direction="vertical" { children; }
            pane split_direction="vertical" { pane; pane; pane; pane; }
        }
    }
    ui max_panes=13 {
        pane {
            pane split_direction="vertical" { children; }
            pane split_direction="vertical" { pane; pane; pane; pane; }
            pane split_direction="vertical" { pane; pane; pane; pane; }
        }
    }
}

swap_tiled_layout name="stacked" {
    ui min_panes=5 {
        pane stacked=true { children; }
    }
}

swap_tiled_layout name="half-stacked" {
    ui min_panes=6 {
        pane split_direction="vertical" {
            pane
            pane stacked=true { children; }
        }
    }
}
```

The floating ones, which do not touch the column, are the
`swap_floating_layout` blocks of `zellij setup --dump-swap-layout default`. The
sidebar needs the tiled ones to come back to its column after hiding, in a tab
of more than one terminal.

Key bindings drive it without moving your hands to it. A `MessagePlugin` with
no location goes to every plugin in the session: the dock answers `toggle`, the
sidebar `focus`, `cycle` and `next`, and the picker `focus` too, handing the
sidebar its row. Bind the ones you want:

```kdl
keybinds {
    normal {
        // Into the sidebar, and back out to the pane you came from.
        bind "Ctrl b" {
            MessagePlugin {
                name "sidebar"
                payload "focus"
            }
        }
        // The next pane the sidebar lists, and from the last back to the
        // first.
        bind "Ctrl f" {
            MessagePlugin {
                name "sidebar"
                payload "cycle"
            }
        }
        // The next agent that wants you: the waiting first, then the idle,
        // the most recent first.
        bind "Alt a" {
            MessagePlugin {
                name "sidebar"
                payload "next"
            }
        }
        // Hide the sidebar, or bring it back.
        bind "Alt b" {
            MessagePlugin {
                name "sidebar"
                payload "toggle"
            }
        }
    }
}
```

zellij's own keys take `Ctrl b` into tmux mode: unbind it there, or pick
another key.

`Ctrl b` on a hidden sidebar brings it back and moves into it. A tab opened
while it is hidden opens with its column hidden too.

The keys that move the focus pass over the sidebar, as they pass over the tab
bar: `Alt h`, which zellij binds to move the focus left, goes on from the pane
beside the column to the tab before, and `Alt l` into a tab lands in the pane
beside its column. `Ctrl b` and a click are the ways in. zellij does not say
which key moved the focus, so one that moves it only within a tab, `h` in pane
mode, goes on to the tab before too.

zellij hands the sidebar no click that only focuses it, but it ends the hover
over the sidebar first, where the click was, and the sidebar acts on that. With
`mouse_hover_effects false` there is no hover: such a click only focuses the
sidebar, and from the pane beside the column reads as `Alt h`.
`mouse_click_through true` hands the sidebar the click itself. zellij hands a
plugin no right click, and no drag.

Two clients on one session — a terminal and `zellij web`, say — share the one
sidebar. It goes with whoever switched tabs last, and comes back from hidden to
the tab of the client that attached first. A key reaches the plugins of every
client, and zellij does not say whose it was, so `Ctrl b`, `Ctrl f` and
`Alt a` act for both.

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

In the sidebar, once it has the focus:

| key | does |
|---|---|
| `↓` `↑` / `j` `k` | move the highlight, from pane to pane |
| `Enter` | go to the highlighted pane, or switch to the highlighted folded session |
| `←` `→` / `h` `l` | fold / unfold the session |
| `Space` | fold or unfold |
| *a key of the [menu](#menu)* | open its group, or start its agent, for the highlighted pane |
| `m` | open the menu on its first group, for the highlighted pane |
| `?` | open the menu on its first group, with the sidebar's commands under its keys |
| `b` | open `?` on the [brief](#brief)'s keys, for the highlighted row |
| `/` | search |
| `Esc` | back to the pane you came from |

With the mouse, focused or not:

| mouse | does |
|---|---|
| click a pane, or a folded session | go there |
| click `▾` / `▸` | fold / unfold the session |
| wheel | scroll the list |

While the search is open, every key types into it but these:

| key | does |
|---|---|
| `↓` / `↑` | move the highlight |
| `Enter` | go to the highlighted row, and close the search |
| `Esc` | close the search, and clear it |
| `Backspace` | delete a character, or close the search once it is empty |

In the menu:

| key | does |
|---|---|
| *a key it shows* | open its group, start its agent, or run the sidebar's command |
| `n p` | open your shell in a pane beside the highlighted pane |
| `n t` | open a tab of the highlighted pane's session |
| `n s` | start a session where the highlighted pane works, and go there |
| `d d` | close the highlighted pane, or kill the highlighted session |
| `d s` | kill the highlighted row's session |
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

The flake names one system, `x86_64-linux`. On anything else that command and the
`nix develop` above both fail to evaluate; build with cargo instead, on the
toolchain `rust-toolchain.toml` names.

Then point `config.kdl` at `file:` instead of the release URL:

```kdl
plugins {
    luneta location="file:~/.local/share/zellij/plugins/luneta.wasm"
}
```

## License

MIT. Zellij is MIT-licensed too.

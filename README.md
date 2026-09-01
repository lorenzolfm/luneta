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
  Record with `make media` (needs charmbracelet/vhs); see docs/media/README.md.
  Then replace the code block below with:
  <p align="center"><img src="docs/media/hero.gif" alt="luneta demo" width="900"></p>
-->

```
╭─ luneta ───────────────────────── 2/4 ─╮╭─ dotfiles ─────────────────── 3 panes ─╮
│                                        ││ editor · nvim                          │
│                                        ││                                        │
│                                        ││                                        │
│                                        ││                                        │
│   🏠 notes ─────────────────── current ││                                        │
│   luneta                        2h ago ││   1 //! luneta: a personal zellij ses… │
│ > dotfiles                      5h ago ││   2                                    │
│   🪦 Dead sessions ─────────────────── ││   3 mod agents;                        │
│   despesas-old                  1w ago ││                                        │
│   api-spike                     5w ago ││ "src/main.rs" 1005L, 41k               │
╰────────────────────────────────────────╯╰────────────────────────────────────────╯
╭─ Sessions ───────────────────────────────────────────────────────────────────────╮
│ > _                                                               <ENTER> Attach │
╰──────────────────────────────────────────────────────────────────────────────────╯
  <↓↑> Nav <ENTER> Select <TAB> Agents <Ctrl r> Rename <Del> Delete <ESC> Close
```

---

## What is luneta

One floating pane, one search box, three lists. `Tab` moves between them, and the
search term follows you across, so `Tab` asks the next list the same question.

| `Tab` | list | `Enter` |
|---|---|---|
| 1 | **Sessions** — live and resurrectable, newest first | attach, resurrect, or create |
| 2 | **Agents** — the Claude Code agents running right now, sorted by which one wants you | jump to that agent's pane |
| 3 | **Directories** — where you work, in [zoxide](https://github.com/ajeetdsouza/zoxide) frecency order | create a session there |

The picker answers three questions: *which session am I in*, *which agent waits
for me*, and *where do I want to be*.

### Every row previews itself

A session name does not say what runs in it. A directory name does not say what is
in it. An agent label does not say what it is waiting for. No row is wide enough
for those answers — so the box on the right shows the thing itself, live, and
follows the highlight.

- **A session** shows the focused pane of its active tab — the exact screen an
  attach puts you in front of — in that pane's own colours, with the pane name
  above and the pane count in the border.
- **A directory** shows its contents as [`eza`](https://github.com/eza-community/eza)
  draws them: icons, colours, directories first.
- **An agent** shows its status, how long it has held it, and its pane, so you can
  read the question before you go answer it.

Previews are debounced and cached, so holding an arrow key down through a hundred
directories costs nothing.

### Find the agent that is waiting for you

<!--
  MEDIA SLOT — agents.gif
  <p align="center"><img src="docs/media/agents.gif" alt="agent picker" width="900"></p>
-->

```
╭─ luneta ───────────────────────── 1/3 ─╮╭─ luneta ───────────────────────────────╮
│                                        ││ waiting · 18m                          │
│                                        ││                                        │
│                                        ││                                        │
│                                        ││                                        │
│                                        ││ > read the docs?                       │
│                                        ││                                        │
│                                        ││   1. yes                               │
│ > luneta  🙋  18m          misc/luneta ││   2. no                                │
│   notes   ☕  5m     lorenzo/Documents ││                                        │
│   bipa    ⠋   31m            Work/bipa ││                                        │
╰────────────────────────────────────────╯╰────────────────────────────────────────╯
╭─ Agents ─────────────────────────────────────────────────────────────────────────╮
│ > _                                                       <ENTER> Go to "luneta" │
╰──────────────────────────────────────────────────────────────────────────────────╯
  <↓↑> - Navigate, <ENTER> - Go to agent, <TAB> - Directories, <ESC> - Close
```

Run several agents at once and the problem is not starting them, it is knowing
which one stopped and needs you. The list is **sorted by attention** — 🙋 `waiting`
first, then ☕ `idle`, then ⠋ `busy`, most recently changed first within each
group — and that order holds as you type, so the boundary between the agents that
want you and the rest never moves under your cursor.

| | status | meaning |
|---|---|---|
| 🙋 | `waiting` | it asked you something and stopped |
| ☕ | `idle` | it finished and is waiting for you |
| ⠋⠙⠹⠸ | `busy` | a spinner — the one status that ends on its own |
| 🐚 | `shell` | it is a shell |
| 🛸 | anything else | unknown, and visibly so |

The age is a **duration, not a timestamp**: `35m` means it has been waiting
thirty-five minutes, and the number keeps climbing while you look at it.

Needs [`claude-ps`](https://github.com/lorenzolfm/claude-ps) on the server's
`PATH`. Without it, the other two lists work exactly as before.

### Jump to a project, not to a path

<!--
  MEDIA SLOT — dirs.gif
  <p align="center"><img src="docs/media/dirs.gif" alt="directory picker" width="900"></p>
-->

```
╭─ luneta ───────────────────────── 1/4 ─╮╭─ luneta-2 ─────────────────── 5 items ─╮
│                                        ││ /home/lorenzo/Projects/misc/luneta     │
│                                        ││                                        │
│                                        ││  src/                                  │
│                                        ││  target/                               │
│                                        ││  Cargo.toml                            │
│                                        ││  Makefile                              │
│ > luneta-2  …enzo/Projects/misc/luneta ││ 󰂺 README.md                            │
│   homelab   …nzo/Projects/misc/homelab ││                                        │
│   bipa      …orenzo/Projects/Work/bipa ││                                        │
│   bin         /home/lorenzo/.local/bin ││                                        │
╰────────────────────────────────────────╯╰────────────────────────────────────────╯
╭─ Directories ────────────────────────────────────────────────────────────────────╮
│ > _                                                    <ENTER> Create "luneta-2" │
╰──────────────────────────────────────────────────────────────────────────────────╯
  <↓↑> - Navigate, <ENTER> - Go there, <TAB> - Sessions, <ESC> - Close
```

Every row is a directory plus a session name **nothing else is holding**. The name
is the directory itself, and if it is taken it steps to `luneta-2`, `luneta-3`,
counting past every live session and every saved layout. The row shows the name you
will get before you press anything, so `Create "luneta-2"` is the whole warning
that `luneta` was already open.

That is not cosmetic: hand zellij a name that already exists and it silently
attaches you to that session, wherever it is, and drops your cwd. luneta never
hands it one.

The filter searches the **path**, because the path is what you would type at `z`.

### And the small things

- **It follows your theme.** There is no palette to configure and nothing to keep
  in sync — zellij colours the picker from your active theme. The only other
  colours on screen are the ones inside a preview, which belong to the program
  that drew them.
- **The prompt tells you what `Enter` does** — `<ENTER> Attach`,
  `<ENTER> Resurrect`, `<ENTER> Create "desp"` — and shows refusals in place, so
  no error overlay ever eats your next keystroke.
- **Names are validated as you type**, in both the search box and the rename
  screen, so a name zellij would refuse never gets that far.
- **The current session is never a row**, because you are already in it. It sits on
  the first line as a banner instead, whatever you have typed: the count in the
  border ignores it, `Up` and `Down` cannot reach it, and `Del` cannot act on it.
- **`Del` cleans up** — kill a live session, delete a dead one, without leaving the
  picker.
- **`Ctrl-r` renames** the session you are in.
- **It degrades instead of breaking.** No zoxide, no eza, no claude-ps? The screen
  says which one is missing, on the note line, and everything else keeps working.
- **It fits a narrow pane.** Below 52 columns the preview goes and the list takes
  the full width.

---

## Getting started

### Requirements

| | | needed for |
|---|---|---|
| **zellij 0.45.0** | | everything |
| [`zoxide`](https://github.com/ajeetdsouza/zoxide) | optional | the directory list |
| [`eza`](https://github.com/eza-community/eza) | optional | the directory preview |
| [`claude-ps`](https://github.com/lorenzolfm/claude-ps) | optional | the agent list |
| a [Nerd Font](https://www.nerdfonts.com) | optional | the icons in the directory preview |

The session list needs nothing but zellij. The optional tools are looked up by
name on the `PATH` of the zellij **server**, which is inherited from whatever
started it — on a long-lived session that can be older than your shell profile.

### Install

One config line. Zellij fetches the plugin itself — no clone, no rust toolchain,
no `make`. In `~/.config/zellij/config.kdl`:

```kdl
plugins {
    // ...
    luneta location="https://github.com/lorenzolfm/luneta/releases/download/v0.1.0/luneta-0.1.0.wasm"
}
```

Zellij will prompt once for `RunCommands`, `ReadApplicationState` and
`ChangeApplicationState`. `RunCommands` is how luneta reaches `zoxide`, `eza`,
`claude-ps` and `zellij` itself.

> **Upgrading is a config edit.** Zellij caches a downloaded plugin by the last
> segment of its URL and never re-fetches, which is why the version is in the
> filename. Point the line at the new release to move.

<details>
<summary><strong>Prefer to verify the bytes first?</strong></summary>

Zellij does not check a checksum, a signature or an attestation on a plugin it
downloads — it fetches the URL and runs the bytes. Every release ships a
`SHA256SUMS` and a keyless build provenance attestation, so you can download,
check, and install from disk instead:

```sh
v=0.1.0
gh release download "v$v" -R lorenzolfm/luneta -p 'luneta-*.wasm' -p SHA256SUMS
sha256sum -c SHA256SUMS
gh attestation verify "luneta-$v.wasm" -R lorenzolfm/luneta
install -m 0644 "luneta-$v.wasm" ~/.local/share/zellij/plugins/luneta.wasm
```

`sha256sum -c` proves the bytes are complete. `gh attestation verify` proves where
they came from: built by `ci.yml`, at that commit, in this repo, on a GitHub
runner. Then point `config.kdl` at the `file:` path instead of the URL.

</details>

### Bind a key

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
    }
}
```

Restart zellij once after adding the `plugins` block — a change to the alias needs
it. A change to the `.wasm` behind it does not.

> **Write the location the same way everywhere.** Zellij identifies a plugin
> instance by its location string, so a `file:` binding and an `https:` binding are
> two different plugins, and you will get two floating panes stacked on each other.

---

## Usage

Press your key, type, press `Enter`.

| key | does |
|---|---|
| *any character* | filter, fuzzily |
| `↓` / `↑` | move the highlight (it does not wrap) |
| `Enter` | act on the highlighted row — the prompt says which action |
| `Tab` / `Shift-Tab` | next / previous list, keeping the search term |
| `Ctrl-r` | rename the session you are in |
| `Del` | kill a live session, or delete a dead one |
| `Backspace` | delete a character |
| `Esc` / `Ctrl-c` | close |

### Sessions

`Enter` attaches to the highlighted session, or resurrects it if it is a dead row.
With nothing highlighted, `Enter` **creates** a session under the name you typed.
There is no layout menu and no confirmation step.

Live sessions always sort above resurrectable ones, newest first within each
group.

`Del` acts immediately, with no confirmation. A live row is killed; a dead row is
deleted for good. To remove a running session permanently, press `Del` on the live
row, then `Del` again on the dead row that replaces it.

### Agents

`Enter` takes you to the agent's pane — attaching to its session and focusing that
pane, or just focusing the pane if it is in the session you are already in.

An agent that has not been given a name goes by the name of the session it sits in,
plus its pane, if another agent in the same session would answer to the same name.
That is the name the session goes by **now**. An agent reports the session it
started in — the old name, after a rename, and nothing can correct it, because the
name lives in the environment of a process that is already running. So luneta never
uses that name as an address. It asks every live session which panes it has, and
takes the one session holding a pane with the agent's pane id and the directory the
agent works in. Only when several sessions could answer for that pane does the
reported name get a say, as a tiebreaker: the holder it names wins, and failing
that, the holder whose pane is running claude. An agent no live pane answers for is
left out of the list and counted in a note under it, rather than offering you a jump
to a session that no longer exists.

The list is snapshotted when the screen opens, so no row moves while you read it.
The ages keep climbing. The agent you opened the picker from is not listed; another
agent in the same session still is.

### Directories

`Enter` creates a session in that directory, named after it. This screen never
attaches — the session list beside it is where you go to a session that exists.

`Del` does nothing here; removing a directory from zoxide is a different action on
a different store.

---

## Configuration

### Open straight onto a screen

Pair `LaunchOrFocusPlugin` with a `MessagePlugin` naming the screen — `sessions`,
`agents` or `dirs`:

```kdl
bind "Ctrl a" {
    LaunchOrFocusPlugin "file:/home/you/.local/share/zellij/plugins/luneta.wasm" {
        floating true
        move_to_focused_tab true
    }
    MessagePlugin "file:/home/you/.local/share/zellij/plugins/luneta.wasm" {
        name "screen"
        payload "agents"
    }
    SwitchToMode "normal"
}
```

Three rules for this one:

- **Use the full location, not the `luneta` alias, in both actions.**
  `MessagePlugin` does not resolve an alias to the running instance — it starts a
  second, hidden one and talks to that.
- **`MessagePlugin` alone is not enough.** It starts the plugin hidden and
  unfocused. `LaunchOrFocusPlugin` first is what makes it visible.
- **Give every such binding its own `MessagePlugin`**, including the plain one,
  or a key that opened the sessions will keep landing wherever the last key left
  the picker.

### `agents_command`

If `claude-ps` is not on the server's `PATH`, name the program to run instead:

```kdl
LaunchOrFocusPlugin "file:/home/you/.local/share/zellij/plugins/luneta.wasm" {
    floating true
    move_to_focused_tab true
    agents_command "/opt/tools/claude-ps"
}
```

It is a program, not a command line — arguments are not split out of it, because a
path can contain a space. Wrap it in a script if it needs any. And **every binding
must pass the same value**, or none: zellij identifies a plugin instance partly by
its configuration, so two different values give you two pickers.

---

## Building from source

Only if you would rather not install a downloaded `.wasm`.

```sh
nix develop          # or: direnv allow, once
make install         # builds and installs to ~/.local/share/zellij/plugins/luneta.wasm
```

The flake pins the rust toolchain with `wasm32-wasip1` added; a system rustc
without that target fails with `error[E0463]: can't find crate for 'std'`.

Then point `config.kdl` at `file:` instead of the release URL:

```kdl
plugins {
    luneta location="file:~/.local/share/zellij/plugins/luneta.wasm"
}
```

## Why another session picker

luneta started as a personal tool and stayed opinionated. It diverges from the
built-in session manager on purpose: no tab list, no pane drill-down, one search
box shared by three lists, and a live preview on every row.

Issues and pull requests are welcome.

## License

MIT. Zellij is MIT-licensed too.

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
<p align="center"><img src="docs/media/agents.png" alt="the agent picker: agents sorted by attention on the left, the highlighted agent's question on the right" width="900"></p>

Know which agents need your attention, and jump straight to them.

| | status | meaning |
|---|---|---|
| 🙋 | `waiting` | it asked you something and stopped |
| ☕ | `idle` | it finished and is waiting for you |
| ⠋⠙⠹⠸ | `busy` | a spinner — the one status that ends on its own |
| 🐚 | `shell` | it is a shell |
| 🛸 | anything else | unknown, and visibly so |

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

---

### Sidebar

The same plugin, pinned to the left of every tab instead of floating over it:
every live session, the panes in it, and what the agent in each pane is doing.

```text
▾ bipa                     🙋1
  ☕ handoff-eng-11259-pr… 13h
  🙋 dda-bank-slip-paid-fe… 3m
  ⠋  Handoff pix outflow … 12s
▸ floresta                   ⠋
▾ unnamed
› ⠋  orca-copy             12s
     ~/P/m/unnamed
```

Sessions keep their place, oldest first, so a row never moves under your eye
or your mouse. Which agent wants you is the job of the glyph, the count on its
session's row, and the `next` key — not of the order.

An agent's row is named for what it is doing: the name you gave it, or else
the title Claude Code gives its pane. A pane with no agent in it is dimmed, and
`›` marks the pane you are in.

Click a row, or focus the sidebar and press `Enter`, to go there — across tabs
and sessions alike. A folded session keeps its count and shows the glyph of
the agent that wants you most.

One key hides the sidebar in every tab of the session, the panes beside it
taking its columns, and brings it back where it stood.

## Getting started

### Requirements

| | | needed for |
|---|---|---|
| **zellij 0.45.0** | | everything |
| [`claude-ps`](https://github.com/lorenzolfm/claude-ps) | optional | the agent list |
| [`zoxide`](https://github.com/ajeetdsouza/zoxide) | optional | the directory list |
| [`eza`](https://github.com/eza-community/eza) | optional | the directory preview |

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

Zellij will prompt once for `RunCommands`, `ReadApplicationState` and
`ChangeApplicationState`. `RunCommands` is how luneta reaches `zoxide`, `eza`,
`claude-ps` and `zellij` itself.

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

### Pin the sidebar

Give the plugin `mode "sidebar"` in a layout, beside the pane you work in:

```kdl
// ~/.config/zellij/layouts/sidebar.kdl
layout {
    pane size=1 borderless=true {
        plugin location="zellij:tab-bar"
    }
    pane split_direction="vertical" {
        pane size=32 borderless=true {
            plugin location="https://github.com/lorenzolfm/luneta/releases/download/v0.2.2/luneta-0.2.2.wasm" {
                mode "sidebar"
            }
        }
        pane focus=true
    }
    pane size=2 borderless=true {
        plugin location="zellij:status-bar"
    }
}
```

zellij opens every new tab of a session from the layout the session was made
with, so `default_layout "sidebar"` in `config.kdl` gives every tab of every
new session a sidebar. Leave the `pane` in: a layout of nothing but a
`default_tab_template` opens tabs that hold the sidebar and no terminal.

A session that is already running keeps its own layout.
`zellij action new-tab --layout ~/.config/zellij/layouts/sidebar.kdl` opens one
tab with a sidebar in it, and the tabs after it follow the session's layout
again.

A layout of your own leaves zellij's swap layouts behind: the ones `Alt [` and
`Alt ]` step through, and that arrange a tab as you open panes in it. Give the
layout its own, with the sidebar in a column of its own, in a file beside it.
zellij's, with every pane count one higher for the sidebar:

```kdl
// ~/.config/zellij/layouts/sidebar.swap.kdl
tab_template name="ui" {
    pane size=1 borderless=true {
        plugin location="zellij:tab-bar"
    }
    pane split_direction="vertical" {
        pane size=32 borderless=true {
            plugin location="https://github.com/lorenzolfm/luneta/releases/download/v0.2.2/luneta-0.2.2.wasm" {
                mode "sidebar"
            }
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

The floating ones, which do not touch the sidebar, are the
`swap_floating_layout` blocks of `zellij setup --dump-swap-layout default`.
The sidebar needs the tiled ones to come back to its column after hiding, in a
tab of more than one terminal.

Three key bindings drive it without moving your hands to it. A `MessagePlugin`
with no location goes to every plugin in the session. Every sidebar hides and
shows; the rest only the sidebar on screen acts on:

```kdl
keybinds {
    normal {
        // Into the sidebar, and back out to the pane you came from.
        bind "Alt s" {
            MessagePlugin {
                name "sidebar"
                payload "focus"
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
        // Hide the sidebar in every tab, or bring it back.
        bind "Alt b" {
            MessagePlugin {
                name "sidebar"
                payload "toggle"
            }
        }
    }
}
```

`Alt s` on hidden sidebars brings them back and moves into the one on screen.
A tab opened while they are hidden opens with its sidebar hidden too.

`Alt h`, which zellij binds to move the focus left, reaches the sidebar from
the leftmost pane as well.

Only the sidebar in the tab on screen does any work, hidden or not. The others
wake once a second to keep the clock, and ask `claude-ps` nothing.

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
| `Del` | kill a live session, or delete a dead one |
| `Backspace` | delete a character |
| `Esc` / `Ctrl-c` | close |

In the sidebar, once it has the focus:

| key | does |
|---|---|
| `↓` `↑` / `j` `k` | move the highlight |
| `Enter` | go to the highlighted pane, or switch to the highlighted session |
| `←` `→` / `h` `l` | fold / unfold the session |
| `Space` | fold or unfold |
| `n` | go to the next agent that wants you |
| `Esc` | back to the pane you came from |

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

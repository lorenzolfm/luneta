# zj-picker

A personal [zellij](https://zellij.dev) plugin: a session picker with telescope
semantics — type a few characters, press `Enter`, you're there.

It lists live and resurrectable sessions, filters them as you type, and attaches
on `Enter` to the highlighted row — creating a session only when you ask for one.

Built against **zellij 0.45.0** (`zellij-tile = "=0.45.0"`).

## What it does

```
Session: dep_

you are in my-current-session (not listed)

  api-deploy         [ATTACH]     2h ago
  deploy-scripts     [ATTACH]     3d ago
  old-deploy         [RESURRECT]  1w ago
```

- **Live sessions always sort above resurrectable ones**, at every stage —
  before the search term, and after it. Upstream sorts score-first with type
  only as a tiebreak, so its live and dead rows interleave as you type. Here,
  filtering only ever *removes* rows; the live/dead boundary never moves.
- Within a group, newest first. `creation_time` arrives from the host as an
  elapsed age truncated to whole seconds, so ties are common and harmless.
- **The current session is not listed** — you are already in it. It is dropped
  where the match set is built, not in the renderer, so the rendered list *is*
  the match set and row indices can never drift from match indices. The hint
  line above the list says which session is being hidden.
- The details column is the **age**, for both kinds of row. It is the sort key,
  so showing it is what makes the order look deliberate rather than arbitrary.

### What `Enter` does

> **The highlighted row tells you what `Enter` does, and its tag says which. With
> nothing highlighted, `Enter` hands your typed text to the host, which attaches,
> resurrects, or creates.**

The second half is safe because session names are unique across live *and*
resurrectable sessions, so the plugin never decides attach-vs-create. It hands
the host **one name**, and the host resolves it: live → attach, has a saved
layout → resurrect, neither → create. One call, three outcomes.

- Something is always highlighted while the list is non-empty, starting at the
  top match. Typing or `Backspace` snaps back to the top; `Up`/`Down` move and
  **stop** at the ends rather than wrapping.
- **`Enter` never creates a session in one keystroke.** With no match it opens a
  layout screen, which creates nothing — `Enter` there creates with the selected
  layout, `Esc` backs out with your typed name intact.
- **`Esc` means "I mean the literal text I typed."** It drops the highlight (and
  does not take it back until you type again), which is how you create `infra`
  while `infra-staging` is live: type, `Esc`, `Enter`. A second `Esc` — or `Esc`
  when nothing is highlighted — closes the picker. `Ctrl-c` always closes.
- **An empty name is a feature**: `Esc` `Enter` `Enter` on an empty prompt gives
  a host-named scratch session.
- Typing your own session's name is a **no-op**, not an error. The hint line says
  so rather than an overlay, because an error modal here would eat your next
  keystroke.

### The hint line

Dim, above the list, never a row and never selectable. It says the two things
the list cannot:

- `you are in "despesas" (not listed)` whenever your search reaches for the
  session you are already in — which the list deliberately omits, and would
  otherwise omit silently.
- What `Enter` will do when nothing is highlighted: `Enter to create "desp"`, or
  why the name is refused (`invalid · name cannot contain '/'`).

Name validation runs live, here, rather than as an error when you press `Enter`.
🔴 The host does **not** validate names on this path — `validate_session_name` is
wired only to the CLI and the web client — so the plugin is the last line of
defence: length, `/`, `.`/`..` and whitespace-only.

Keys: type to filter, `Up`/`Down` to move (no wrap), `Backspace`, `Enter` to act,
`Esc` to drop the highlight then close, `Ctrl-c` to dismiss.

### The renderer is a rewrite, not a port

Upstream's `ui/components.rs` is 1847 lines, and most of it serves the tab and
pane drill-down this picker cuts — the four-column layout and the five-tier
width-reduction algorithm that fed it. With three fixed columns the reduction is
a three-step ladder: full tags and age → `[A]`/`[R]` and age → `[A]`/`[R]` only.
Column widths are measured over the **visible window**, not the whole list, so
one very long name cannot cost every other row its age column.

Styling is plain SGR (bold, dim, reverse video). That takes the terminal's own
theme and avoids subscribing to `ModeUpdate` just to read a palette.

## Why this repo exists standalone

Vendored rather than kept as a patch against a zellij checkout. The plugin only
needs `zellij-tile` from crates.io — not the zellij source tree — so carrying the
whole workspace bought nothing. The picker deliberately diverges from upstream
(tabs and pane drill-down are cut), so inheriting upstream's session-manager
changes was never going to be useful.

The cost of that choice: upstream fixes never arrive, and a zellij upgrade means
bumping the `zellij-tile` pin by hand and rebuilding.

## Building

The **system rustc cannot build this.** Nix ships `std` for the host triple only
and there is no `rustup` to add targets with, so `wasm32-wasip1` fails with
`error[E0463]: can't find crate for 'std'`. The flake solves it: it takes the
`rust-overlay` toolchain pinned to rust 1.95.0 — the channel zellij's own
`rust-toolchain.toml` names — with `wasm32-wasip1` added.

```sh
nix develop          # or: direnv allow, once
make install
```

`make install` builds and copies the `.wasm` to
`~/.local/share/zellij/plugins/zj-picker.wasm`.

⚠️ **Keep that path stable.** Zellij caches granted permissions against the
**absolute path** of the `.wasm` (`~/.cache/zellij/permissions.kdl`), so moving
or renaming it makes zellij prompt for permissions again.

## The edit → see-it loop

Measured at **~4.5s for an incremental rebuild**, plus a plugin reload of ~4ms.
No zellij restart and no permission re-prompt.

```sh
make dev             # build, install, and reload the plugin pane
```

or by hand:

```sh
make install
zellij action close-pane
zellij action launch-or-focus-plugin --skip-plugin-cache --floating \
    file:$HOME/.local/share/zellij/plugins/zj-picker.wasm
```

`--skip-plugin-cache` is the load-bearing flag. Without it zellij reuses the
compiled module it already has in memory for that path and your new bytes are
ignored — the module is re-inserted into the cache after every load
(`plugin_loader.rs:306`), so simply reopening the pane is not enough.

Add `SESSION=<name>` to any recipe to drive a session other than the current one.

## Installing it as `Ctrl-j`

`config.kdl` needs a plugin alias plus the bindings re-pointed:

```kdl
plugins {
    // ...
    zj-picker location="file:~/.local/share/zellij/plugins/zj-picker.wasm"
}
```

A `file:~` URL is fine — the tilde is preserved and shell-expanded
(`layout.rs:605-607,619`), so an absolute path is not required.

Then point the bindings at `"zj-picker"` instead of `"session-manager"`.
Changing the alias block **requires a zellij restart**; changing the `.wasm`
behind it does not.

## Notes for the next change

- **A `file:` plugin is not a builtin, so it gets no free permissions.** Builtins
  short-circuit every permission check (`zellij_exports.rs:5428`); this plugin
  must call `request_permission` itself. If it doesn't, commands are **silently
  denied** — the only trace is a `log::error!` in
  `/tmp/zellij-1000/zellij-log/zellij.log`, with nothing shown on screen.
- **`launch-or-focus-plugin` needs a connected client.** Against a detached
  session it logs `No connected clients found - cannot load or focus plugin`
  and the CLI still exits 0. Attach a client (a pty via `script` works) before
  driving it headlessly.
- `get_session_list()` returns a `SessionListSnapshot { live_sessions,
  resurrectable_sessions }` — both halves in one call. The picker polls it once
  a second and takes the whole list from that single snapshot, rather than
  subscribing to `SessionUpdate`. The pushed path refreshes only the *current*
  session's age and leaves its peers frozen, so ages taken from it agree only by
  accident.
- **A plugin pane has no scrollback of its own.** Printing `rows` lines each
  terminated by `\n` into a `rows`-tall pane scrolls the first line off the top,
  silently. Build the whole frame and emit it with one `print!` and no trailing
  newline (upstream instead positions the cursor absolutely, `\e[{y};{x}H`).
- **A background poll must not reset the cursor.** Rebuilding the result list
  once a second is correct; snapping the selection to row 0 while doing it drags
  the cursor out from under the user every second. Snap on a *search-term*
  change; hold the selected session by name on a poll.

## Driving it headlessly

`zellij action` makes the picker's output a diffable artifact, no interaction
needed. The gotchas, all of which **exit 0 while doing nothing**:

- `launch-or-focus-plugin` needs a **connected client** (see above).
- `dump-screen` takes `--path`, and that path must be **absolute**.
- 🔴 **`dump-screen --ansi` produces nothing at all** in 0.45.0 — an empty file
  with `--path`, empty output without it. So styling (the selection highlight,
  the match-character bolding) cannot be verified this way. Verify the selection
  through a side effect that shows in plain text instead: move it past the
  bottom of a short pane and diff the rows that scroll into view.
- Arrow keys go through `action write` as raw bytes — `write 27 91 66` for
  `Down`, `write 27 91 65` for `Up`, `write 127` for `Backspace`, `write 27` for
  `Esc`. `write-chars` is for literal text.
- A session sizes itself to its **smallest** attached client, so an extra 80x24
  client pins the pane narrow no matter how wide the one you care about is.
  Drive a session that has exactly one client of the size you want.
- `dump-screen` dumps the **focused pane**, not the whole screen — so it cannot
  see the status bar, and "did this session get the layout I picked?" is not a
  question it can answer.

### Seeing where `Enter` landed

`Enter` moves the *client*, which no `zellij action` reports. Attach the test
client through a pty you control and log it: after a switch the client
re-attaches in the same process, and the new session's tab bar carries its name,
so `grep -ao 'Zellij ([a-z0-9-]*)'` over that log says where you ended up. Fork
the pty with `pty.fork()` and set its size with `TIOCSWINSZ` — `script` inherits
whatever size the calling terminal had, which is how you get a surprise 80x24
client pinning the pane.

⚠️ `pkill -f drive.py` **kills the shell running it**: `pkill -f` matches its own
invoker's command line. Use `pgrep -f 'drive[.]py' | xargs -r kill`.

## License

MIT. Zellij itself is MIT-licensed too.

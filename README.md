# zj-picker

A personal [zellij](https://zellij.dev) plugin: a session picker with telescope
semantics — type a few characters, press `Enter`, you're there.

It lists live and resurrectable sessions, filters them as you type, and attaches
on `Enter` to the highlighted row — creating a session only when you ask for one.

`Tab` swaps that list for the directories you actually work in, ranked by
zoxide's frecency, so the picker answers *"where do I want to be?"* as well as
*"which session am I already in?"*.

Built against **zellij 0.45.0** (`zellij-tile = "=0.45.0"`).

## What it does

```
            Session: dep_ <ENTER> - Attach

         api-deploy     [ATTACH]     2h ago
         deploy-scripts [ATTACH]     3d ago
         old-deploy     [RESURRECT]  1w ago

       you are in "my-current-session" — not listed
  <↓↑> Nav  <ENTER> Select  <TAB> Dirs  <Ctrl r> Rename  <Del> Delete  <ESC> Close
```

and, one `Tab` away:

```
         Directory: home_ <ENTER> - Create "misc-homelab"

  misc-homelab        [CREATE]   …renzo/Projects/misc/homelab
  homelab-infra       [CREATE]   …Projects/misc/homelab/infra
  Work-bipa           [ATTACH]   …me/lorenzo/Projects/Work/bipa
  .local-bin          [CREATE]   /home/lorenzo/.local/bin
                          +8 more

      <↓↑> Nav  <ENTER> Go  <TAB> Sessions  <ESC> Back
```

- **Live sessions always sort above resurrectable ones**, at every stage —
  before the search term, and after it. Upstream sorts score-first with type
  only as a tiebreak, so its live and dead rows interleave as you type. Here,
  filtering only ever *removes* rows; the live/dead boundary never moves.
- Within a group, newest first. `creation_time` arrives from the host as an
  elapsed age truncated to whole seconds, so ties are common and harmless.
- **The current session is not listed** — you are already in it. It is dropped
  where the match set is built, not in the renderer, so the rendered list *is*
  the match set and row indices can never drift from match indices. The note
  line below the list says which session is being hidden.
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
- **`Enter` creates immediately, with the host's default layout.** There is no
  layout picker: it was a menu answered the same way every time. The cost is that
  the create path has no confirm step — `Enter` on a name that matches nothing
  makes the session then and there.
- **`Esc` means "I mean the literal text I typed."** It drops the highlight (and
  does not take it back until you type again), which is how you create `infra`
  while `infra-staging` is live: type, `Esc`, `Enter`. A second `Esc` — or `Esc`
  when nothing is highlighted — closes the picker. `Ctrl-c` always closes.
- **An empty name is a feature**: `Esc` `Enter` on an empty prompt gives a
  host-named scratch session.
- Typing your own session's name is a **no-op**, not an error. The prompt says
  so rather than an overlay, because an error modal here would eat your next
  keystroke.

### The prompt says what `Enter` does

The outcome lives *in* the prompt, beside the term — `<ENTER> - Attach`,
`<ENTER> - Resurrect`, `<ENTER> - Create "desp"` — so the two states of the
contract are told by one sentence that cannot contradict itself. Both refusals
show up there too, and in the theme's error colour: `already attached` when the
term is the session you are sitting in, and the reason when the name is not a
legal one (`name cannot contain '/'`).

Below the list is the note line — dim, never a row, never selectable — for the
one thing the list cannot say: `you are in "despesas" — not listed`, whenever
your search reaches for the session the list deliberately omits and would
otherwise omit silently.

Name validation runs live, in the prompt, rather than as an error when you press
`Enter` — and the rename screen shares the same validator.
🔴 The host does **not** validate names on this path — `validate_session_name` is
wired only to the CLI and the web client — so the plugin is the last line of
defence: length, `/`, `.`/`..` and whitespace-only.

Keys: type to filter, `Up`/`Down` to move (no wrap), `Backspace`, `Enter` to act,
`Tab` to swap lists, `Ctrl-r` to rename, `Del` to kill or delete, `Esc` to drop the
highlight then close, `Ctrl-c` to dismiss.

### `Del` — kill a session, or delete a dead one

`Del` aims at the highlight and opens a confirm screen; `Enter` there does it, `Esc`
backs out. Nothing happens on the first keystroke — the one place in the picker
that still asks twice, because these are the only keys you cannot take back.

The two cases wear the same key and are not the same thing, so the screen names
which one you are in:

- `[ATTACH]` → **Kill**. Stops the running session. Whether it comes back as
  `[RESURRECT]` depends on whether the host had serialized it yet, which the
  plugin cannot see — so the screen says "it comes back only if it was saved"
  rather than promising a resurrection the host may not be able to deliver.
- `[RESURRECT]` → **Delete**. Throws the saved layout away. Irreversible, and
  said so in the error colour.

**`Del` cannot kill the session you are in** — not by a guard, but because the
current session left the match set at the source and can never be the highlight.

Kill-all and disconnect-others stay cut. Both act on sessions you cannot see from
here, which is the one thing this picker refuses to do.

### `Ctrl-r` — rename the session you are in

`rename_session` takes no target: it renames the current session and nothing
else. Upstream's session manager is doing the same thing behind a UI that implies
it is renaming your selection. Here the current session is never a row, so the
screen can say plainly whose name is changing — `renaming "despesas" — the
session you are in`.

The name is checked on every keystroke, not on `Enter`: empty, unchanged,
colliding with a live or resurrectable session, or failing the same rules the
search prompt applies. The reason sits where the outcome would be, so `Enter` on
a refused name is a no-op you already saw coming. Collisions are checked against
the whole snapshot, not the filtered list — a name colliding with a session the
search term happens to exclude would otherwise sail through.

### `Tab` — the places you work

The second screen lists directories out of **zoxide**, in zoxide's own frecency
order, and `Enter` puts you in one. The search term is shared, so `Tab` asks the
other list the same question you were already asking.

**A directory row is a proposed session name plus a cwd, and the cwd only takes
effect if that name is free.** That is not a rule the plugin chose — it is what
the host does. `switch_session_with_cwd` carries the cwd as far as
`ClientInfo::set_cwd`, which matches `New` and `Resurrect` and drops everything
else through a `_ => {}`. Hand it the name of a live session and you attach to
that session, wherever it happens to be, with no error and no cwd.

So the row's tag says which of the host's outcomes your `Enter` will get, exactly
as a session row's tag does — and it is recomputed against the session snapshot
every poll, so a session created elsewhere turns a `[CREATE]` row into an
`[ATTACH]` one under the cursor:

- `[CREATE]` — the name is free. The session is made, **in that directory**.
- `[ATTACH]` / `[RESURRECT]` — the name is taken. The name goes to the host
  **alone**: it would accept a cwd there and throw it away, and an argument that
  is silently discarded is how you end up believing a session is somewhere it is
  not.
- `[HERE]` — the name is the session you are in, and `Enter` is refused. Not a
  courtesy: asking the host to attach to the session you are running in does not
  decline, it **panics the client**.

🔴 The plugin cannot check the `[ATTACH]` claim. `SessionInfo` has no cwd and
neither does `PaneInfo`, so there is no way to ask a live session which directory
it is in. `[ATTACH]` means *"a session by this name exists"*, not *"that session
is in this directory"* — which is only ever as good as the name is unique.

### Which is why the name is not the basename

A directory's session name is its **last two path components**, joined with `-`:
`~/Projects/Work/bipa.git/master` becomes `bipa.git-master`.

Two components, not one, and the reason is measurable. Across a real 136-path
zoxide database the bare basename collides **nine** ways — `master`, `backend`,
`frontend`, `bin`, `nixos`, `skills`, `.claude`, `.config`, `ldk-server` — and
`bipa.git/master` and `infra.git/master` are both perfectly ordinary things to
have visited. The two-component form collides **zero** times.

`/` is impossible by construction, which matters twice: the host refuses a
session name containing one (and only *logs* the refusal), and so does the
plugin's own validator.

### What the directory screen costs

- **A new permission.** `RunCommands`, because zoxide is reached with
  `run_command(["zoxide", "query", "--list", "--score"])` — the plugin's wasi
  sandbox preopens only `/host`, `/data`, `/cache` and `/tmp`, so `db.zo` is
  both unreachable and binary. ⚠️ The host grants the *set*, so adding it
  re-prompts once, and denying it takes the session list down with it.
- **Not a poll.** zoxide is asked on the permission grant and again on
  `Visible`, not on the 1s timer: that would fork a process a second to re-read
  a database that only changes when you `cd`. `launch-or-focus` means one
  instance outlives many openings, which is what `Visible` is there for.
- **Nothing, when it is missing.** No zoxide, or a denied grant, and the screen
  says which — `zoxide is not available` on the note line. The three ways to be
  empty (waiting, failed, nothing to show) are not collapsed into a blank list.
- **No `-a`.** Without it zoxide omits directories that no longer exist, which
  is the one bit of staleness filtering the plugin could not do for itself.
- **`Del` does nothing here.** Dropping a directory out of zoxide is a different
  verb against a different store, and it does not belong on a key whose confirm
  screen talks about killing processes and throwing away saved layouts.

### The renderer is a rewrite, not a port

It draws through zellij's own UI components — `Text`, `Table`, and the
`print_*_with_coordinates` family — exactly as the built-in session manager
does. Those are serialized to the host as a DCS payload and coloured *there*,
from the active theme, so the picker follows your theme and its selection
colours with no palette to carry and no `ModeUpdate` subscription to keep
current. `Table` also pads the columns, which is most of the layout code gone.

What is left is the reduction ladder. Upstream's `ui/components.rs` is 1847
lines, and most of it serves the tab and pane drill-down this picker cuts — the
four-column layout and the five-tier width-reduction algorithm that fed it. With
three fixed columns the reduction is a three-step ladder: full tags and age →
`[A]`/`[R]` and age → `[A]`/`[R]` only. Column widths are measured over the
**visible window**, not the whole list, so one very long name cannot cost every
other row its age column.

### Centring is per element, not per screen

Every line is centred on the width it actually renders to; the table is centred
as a unit on the width the host will pad its columns to. Upstream instead centres
a fixed `min(cols, 90)` block, which only looks centred because four columns of
session detail fill it — three narrow columns leave the text parked at that
block's left edge with a ragged right side, centred on neither.

The cost is that a line re-centres when its own width changes, so the prompt
drifts by half a column per character as you type. That is what centred input
does everywhere, and it is the prompt moving rather than the whole screen
shifting under it.

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

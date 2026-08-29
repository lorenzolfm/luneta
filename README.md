# zj-picker

A personal [zellij](https://zellij.dev) plugin: a session picker with telescope
semantics — type a few characters, press `Enter`, you're there.

It lists live and resurrectable sessions, filters them as you type, and attaches
on `Enter` to the highlighted row — creating a session only when you ask for one.

`Tab` cycles through three lists: sessions, the Claude Code agents that are
running, and the directories you actually work in (ranked by zoxide's
frecency). So the picker answers *"which agent wants me?"* and *"where do I
want to be?"* as well as *"which session am I already in?"*.

Built against **zellij 0.45.0** (`zellij-tile = "=0.45.0"`).

## What it does

```
            Session: dep_ <ENTER> - Attach

         api-deploy     [ATTACH]     2h ago
         deploy-scripts [ATTACH]     3d ago
         old-deploy     [RESURRECT]  1w ago

       you are in "my-current-session" — not listed
  <↓↑> Nav  <ENTER> Select  <TAB> Agents  <Ctrl r> Rename  <Del> Delete  <ESC> Close
```

and, one `Tab` away:

```
              Agent: _ <ENTER> - Go to "proper-airpods-nixos"

  proper-airpods-nixos   🙋 WAITING 18m misc/proper-airpods-nixos
  claude-code-status-bar 🙋 WAITING 17m misc/claude-code-status-bar
  wt                     ☕ IDLE    2h  misc/wt
  scratch:0              ☕ IDLE    9m  misc/zj-picker
  scratch:1              🐚 SHELL   6m  lorenzo/Documents
  zellij                 ⠹ BUSY    31m misc/zellij

                    1 agent not in zellij — not listed
  <↓↑> Nav  <ENTER> Go  <TAB> Directories  <ESC> Back
```

and one more `Tab` on from there:

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

### The agent screen

The list comes from `claude-agents`, a separate tool on `$PATH` that joins what
Claude Code publishes about itself (`~/.claude/sessions/<pid>.json`) to the pane
each agent is running in. The plugin cannot do that join itself: its wasi
sandbox preopens only `/host`, `/data`, `/cache` and `/tmp`, so neither
`~/.claude/sessions` nor `/proc` is readable from inside it.

- **Sorted attention-first** — `waiting`, then `idle`, then `busy`, then
  anything else — with the longest-waiting first inside each group. The status
  rank sits *above* the fuzzy score, so the boundary between "wants you" and
  "does not" stays a fixed landmark while you narrow instead of shuffling on
  every keystroke.
- **Statuses are passed through verbatim**, uppercased, behind a glyph:

  | | status | why |
  |---|---|---|
  | 🙋 | `waiting` | hand up — it asked you something and stopped |
  | ☕ | `idle` | *finished*, waiting on you. Not asleep: an idle agent outranks a busy one in the sort, so it gets a cup rather than a 💤 |
  | ⠋⠙⠹⠸ | `busy` | a **spinner**, turning once a second. The one glyph that moves, because it is the one status that is going somewhere on its own |
  | 🐚 | `shell` | it is a shell |
  | 🛸 | anything else | unidentified, and visibly not one of the four |

  Those four are the *whole* vocabulary of Claude Code 2.1.251 — read out of
  the binary (`"busy","shell","idle","waiting"`) rather than inferred from
  whatever happened to be running. That makes the table complete **today**, and
  is not a reason to drop 🛸: the set grew by one (`shell`) between two releases
  already.

  The busy spinner is the same claim as the rest of the table, animated: every
  frame is one column wide, so it turns without resizing the tag column and
  shoving the two columns to its right back and forth. It is what the plugin's
  timer runs at 10Hz for — the session poll behind it still runs once a second,
  and a tick that is neither a poll nor a spinner frame redraws nothing.

  So **the table is decoration and nothing else**: it never decides whether a
  row is shown or where it sorts, and it never replaces the word. A status
  invented after this was written keeps its own word, takes 🛸, and ranks last.
  The table going stale costs a picture, never a row. In a pane too narrow for
  the word the glyph stands alone; an unrecognised status falls back to its
  first letter instead, because 🛸 on every unknown row would render two
  different unknown statuses identically.
- **The age is a duration, not a timestamp.** `35m` means "has been idle for
  thirty-five minutes", which is the routing decision; `35m ago` would be a
  different claim.
- **The cwd is its last two components**, so `misc/zj-picker` rather than
  `/home/you/Projects/misc/zj-picker`. Down a column of agents the leading
  components are the same on every row and separate nothing. Two rather than
  one for the reason the directory screen derives names from two: measured
  across a real 136-path zoxide database, the last-two form collided zero
  times where the bare basename collided nine ways.
- **A `:pane` suffix appears only when two rows share a session name**, and it
  is never what the search term matches — you type the bare name. It shows up
  exactly when the session name stops identifying one target.
- **A glance, not a watch.** The snapshot is taken when the screen opens and
  frozen while it is up. That is what makes attention-first ordering safe:
  nothing reorders while you read it.
- **The agent whose pane you opened the picker from is not listed**, dropped by
  `(session, pane)` rather than by session — a *sibling* agent in the same
  session is a legitimate target and stays. Agents running outside zellij are
  dropped too, and counted on the note line so they are never silently absent.
- 🔴 **The column count is checked exactly, and a mismatch is loud.** The wire is
  nine tab-separated fields — `status age session pane name pid session_id
  started_at cwd`. Anything else stops the parse and puts the count on the note
  line (`claude-agents line 1: 10 columns, expected 9`), because a picker that
  tolerated an extra column would fold it into `cwd` and go on rendering rows
  off a schema it no longer understands. So a `claude-agents` newer or older
  than the picker says so instead of looking like "no agents are running".

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
`[A]`/`[R]` and age → `[A]`/`[R]` only. The agent screen has four columns and so
one rung more — abbreviate the tag → drop the cwd → drop the age; age outranks
cwd because the session name usually already names the project, while nothing
else says how long an agent has been stuck. Column widths are measured over the
**visible window**, not the whole list, so one very long name cannot cost every
other row its age column.

### `Enter` on an agent is two calls, not one

Every other row in this picker ends in `switch_session`. An agent row cannot,
because agents are reachable at *pane* granularity and one of the panes may be
in the session you are already sitting in — and asking zellij to attach to your
own session does not decline, it reaches a bare
`panic!("You are trying to attach to the current session")`
(`src/commands.rs:793`) and takes the client down.

So the row carries which call applies:

- a **different** session → `switch_session_with_focus(name, None, Some((pane, false)))`,
  which attaches *and* lands on the pane rather than the session's default one;
- **our own** session → `focus_terminal_pane(pane, …)`, a plain pane focus.

That split also removes the need for a refusal. A directory row that resolves to
the current session is `[HERE]` and does nothing, because there is nothing safe
for it to do; an agent in the current session has a call that works, so it stays
a live target.

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

That `.wasm` is the whole install. The other two screens each want one ordinary
program on the zellij **server's** `PATH`, and neither is required:

| screen | tool | without it |
|---|---|---|
| sessions | — | always works |
| directories | [`zoxide`](https://github.com/ajeetdsouza/zoxide) | `zoxide is not available` on the note line |
| agents | [`claude-agents`](https://github.com/lorenzolfm/claude-agents) | `claude-agents is not available` on the note line |

Both are looked up **by name**. Nothing in the source names an install path, so
where you keep them is your business — but note it is the *server's* `PATH`, not
your shell's: the server inherits it from whatever launched it, which on a
long-lived session may be older than your current profile.

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
zellij action launch-or-focus-plugin --skip-plugin-cache --floating \
    file:$HOME/.local/share/zellij/plugins/zj-picker.wasm
zellij action start-or-reload-plugin \
    file:$HOME/.local/share/zellij/plugins/zj-picker.wasm
```

**Nothing is closed**, and the two calls are there because neither covers both
states alone. `launch-or-focus-plugin` is the only one that can create the
picker when it is not running, and the only one that can say `--floating` — the
geometry the keybinding gives it; on a picker that is already up it just takes
focus. `start-or-reload-plugin` re-reads the `.wasm` from disk and swaps it into
the pane that is already there, which is what actually picks up new bytes when
the pane survived from the last loop. Focus-or-create first, so there is
something to reload.

`--skip-plugin-cache` is the load-bearing flag on the *creating* path. Without
it zellij reuses the compiled module it already has in memory for that path and
your new bytes are ignored — the module is re-inserted into the cache after
every load (`plugin_loader.rs:306`), so simply reopening the pane is not enough.

⚠️ Do not go back to close-and-relaunch. That version had to name the pane it
was closing and could not: zellij documents a `plugin_<id>` on
`launch-or-focus-plugin`'s stdout ("Returns: Plugin pane ID") but prints nothing
at all on 0.45.1, so the guard saw an empty id and refused to run — correctly,
since `close-pane` without an id closes whatever is *focused*, which during a
dev loop is usually your editor. Reloading in place needs no pane id.

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

### Opening straight onto a screen

A key can open the picker *on* a chosen screen by pairing `LaunchOrFocusPlugin`
with a `MessagePlugin` naming it — `sessions`, `agents` or `dirs`:

```kdl
bind "Ctrl a" {
    LaunchOrFocusPlugin "file:/home/you/.local/share/zellij/plugins/zj-picker.wasm" {
        floating true
        move_to_focused_tab true
        skip_plugin_cache true
    }
    MessagePlugin "file:/home/you/.local/share/zellij/plugins/zj-picker.wasm" {
        name "screen"
        payload "agents"
    }
    SwitchToMode "normal"
}
```

Three things about that, each found the hard way:

- 🔴 **Both actions need the full `file:` path, not the `zj-picker` alias.**
  `MessagePlugin` does not resolve the alias to the running instance: it
  launches a *second*, hidden one and messages that, leaving the visible picker
  on whatever screen it was already showing. Mixing the two forms across
  bindings has the same effect. Use the path everywhere, including in the
  binding that opens the picker normally.
- 🔴 **`MessagePlugin` alone will not do.** It launches the plugin if it is not
  running, but launches it *hidden* and unfocused — the pane exists and you
  cannot see or type into it. `LaunchOrFocusPlugin` first is what makes it
  visible.
- ⚠️ **Give every such binding its own `MessagePlugin`.** A plain
  `LaunchOrFocusPlugin` only focuses, so once one key has moved the picker to
  the agents it stays there, and the key that used to open the sessions no
  longer does. When each key names its screen, each one lands where it says.

### When `claude-agents` is not on the server's `PATH`

The escape hatch is one plugin configuration key, `agents_command`, naming the
executable to run instead:

```kdl
bind "Ctrl a" {
    LaunchOrFocusPlugin "file:/home/you/.local/share/zellij/plugins/zj-picker.wasm" {
        floating true
        move_to_focused_tab true
        skip_plugin_cache true
        agents_command "/opt/tools/claude-agents"
    }
    // ... the MessagePlugin, unchanged
}
```

- **It is an executable, not a command line.** Arguments are not split out of
  it, because a path may contain a space and the split would be ambiguous. Wrap
  the tool in a script if you need arguments.
- 🔴 **Every binding must pass the same value, or none of them may.** Zellij keys
  a plugin instance partly on its configuration, so two keys disagreeing about
  this mint *two* pickers and leave two floating panes stacked over each other —
  the same trap that put the screen selection on a `MessagePlugin` rather than
  on configuration. Leaving the key out everywhere is the ordinary case.
- There is deliberately no equivalent for `zoxide`. It is a packaged program on
  everyone's `PATH`; `claude-agents` is one you build yourself.

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

# luneta

A personal [zellij](https://zellij.dev) plugin: a session picker with telescope
semantics. Type a few characters, press `Enter`, and you are there.

It lists live and resurrectable sessions, filters them as you type, and attaches
to the highlighted row on `Enter`. It creates a session only when you ask for one.

`Tab` moves through three lists: sessions, the Claude Code agents that run now,
and the directories you work in, in the frecency order of zoxide. The picker thus
answers three questions: which session am I in, which agent waits for me, and
where do I want to be.

Built against **zellij 0.45.0** (`zellij-tile = "=0.45.0"`).

## What it does

```
╭─ luneta ───────────────────────── 2/4 ─╮╭─ dotfiles ─────────────────── 3 panes ─╮
│                                        ││ editor · nvim                          │
│                                        ││                                        │
│                                        ││                                        │
│                                        ││                                        │
│ you are in "notes" — not listed        ││                                        │
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

One `Tab` away:

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
│   bipa    ⠋   31m            Work/bipa ││ > _                                    │
╰────────────────────────────────────────╯╰────────────────────────────────────────╯
╭─ Agents ─────────────────────────────────────────────────────────────────────────╮
│ > _                                                       <ENTER> Go to "luneta" │
╰──────────────────────────────────────────────────────────────────────────────────╯
  <↓↑> - Navigate, <ENTER> - Go to agent, <TAB> - Directories, <ESC> - Close
```

And one more `Tab`:

```
╭─ luneta ───────────────────────── 1/4 ─╮╭─ luneta ───────────────────── 5 items ─╮
│                                        ││ /home/lorenzo/Projects/misc/luneta     │
│                                        ││                                        │
│                                        ││ src/                                   │
│                                        ││ target/                                │
│                                        ││ Cargo.toml                             │
│                                        ││ Makefile                               │
│ > luneta   …renzo/Projects/misc/luneta ││ README.md                              │
│   homelab  …enzo/Projects/misc/homelab ││                                        │
│   bipa     …lorenzo/Projects/Work/bipa ││                                        │
│   bin         /home/lorenzo/.local/bin ││                                        │
╰────────────────────────────────────────╯╰────────────────────────────────────────╯
╭─ Directories ────────────────────────────────────────────────────────────────────╮
│ > _                                                      <ENTER> Create "luneta" │
╰──────────────────────────────────────────────────────────────────────────────────╯
  <↓↑> - Navigate, <ENTER> - Go there, <TAB> - Sessions, <ESC> - Close
```

The renderer draws those three pictures itself. To print them, run
`cargo test -- --ignored --nocapture print_the_screens`. A terminal shows the
directory listing in the colours and the icons of eza, which a page of text
cannot; the picture above has neither.

The session list follows four rules:

- **A live session always sorts above a resurrectable one**, before the search
  term and after it. A filter only removes rows, so the boundary between the two
  groups never moves.
- **In each group, the newest is first.** The host reports `creation_time` as an
  elapsed age in whole seconds, so equal ages are common and harmless.
- **The current session is not in the list**, because you are in it. It is
  removed where the match set is built, not in the renderer, so the rendered list
  is the match set and the indexes of the two cannot differ. The note line below
  the list names the session the picker hides.
- **The second column is the age**, for both kinds of row. It is the sort key, so
  it shows that the order is deliberate.

## The preview box

The box on the right shows the pane itself, live. The name of a session does not
say what runs in it, the name of a directory does not say what is in it, and the
label of an agent does not say what it waits for. No row has the width for those
answers. The box follows the highlight.

**A live session** shows the focused pane of its active tab, which is the screen
an attach puts you in front of. Above it is a line that names that pane, and the
border holds the pane count. The box shows the end of the screen, at the bottom of
the box: you read a terminal from the bottom, so the newest line is in the same
place on every row you move to.

**In the colours of the pane**, which is the one place the picker paints in
colours it did not choose. Everything else is a zellij `Text`, which the host
colours from your theme. A pane line has no emphasis level it could use, because it
is `nvim` syntax or the red and green of a diff, in truecolour, so the dump keeps
its `--ansi` styling and the renderer prints it unchanged.

**Through the CLI, not the plugin API.** `get_pane_scrollback` can answer only for
the current session, because a plugin runs in one server and a `PaneId` has no
meaning in another. Every session in the list is a different session, so the plugin
runs `zellij --session NAME action dump-screen`, which connects to the server of
that session over its socket. It also needs no new permission, because
`RunCommands` is already granted for zoxide.

**A resurrectable session** has no process, so it has no screen and no panes to
count. The box says `not running` and what there is instead. `0 panes` would say
that it has no panes, and not that nothing runs to hold any.

**An agent** shows its status and its time in that status on one line, which is
the decision you make, and then its own pane.

**A directory** shows its path and its contents, as `eza` draws them: its
colours, its icons, and its directories first. The count is in the border. The
picker prints those bytes and does not re-colour them, for the reason it prints a
pane unchanged — the colours belong to the program that chose them. `--icons`
needs a Nerd Font, and it is one flag to remove in `dirs.rs` if your terminal has
none.

Both commands cost one process for each highlighted row, so both follow the same
rules:

- **Delayed.** The cursor must stay still for two animation ticks, a fifth of a
  second, before anything is asked. A held `↓` key over a hundred zoxide entries
  would otherwise start a process for every entry it passed.
- **Cached**, by path for a directory and by session and pane for a screen. The
  cache is cleared when it fills or when the picker opens again.
- **Held.** A pane screen is the fastest-moving thing the picker reads, and it is
  not read again while you stay on the row. A new read for each tick would start
  one process a second for each row you look at.
- **Filed by what the command asked about**, never by the position of the cursor
  when the reply lands. eza and `dump-screen` answer at any time, and the cursor
  has usually moved. Without the key on the reply, a slow answer would show the
  contents of another place.
- **Only colour passes**, of everything either command can write. `SGR`
  (`ESC [ … m`) is kept, and every other escape is dropped complete, with the
  control characters. The terminal that draws this plugin would obey a cursor move,
  a screen clear, a scroll region, or an `OSC` that renames the tab. Each of those
  lets a previewed pane redraw the picker that only looks at it. The escape must go
  complete, because a sequence that is one character short leaves its end on the
  screen as text.

The box takes half the pane, and it goes when the pane is narrow. Below 52 columns
there is no room for two boxes that can say anything, so the list takes the full
width. The rename screen keeps the full width, because it asks one question and
has nothing to preview.

The preview cannot scroll. The cursor is in the list beside it, and every key that
could move it has a meaning there.

## The agent screen

The list comes from `claude-ps`, a separate tool on `$PATH` that joins what Claude
Code publishes about itself (`~/.claude/sessions/<pid>.json`) to the pane each
agent runs in. The plugin cannot do that join: its wasi sandbox opens only
`/host`, `/data`, `/cache` and `/tmp`, so it can read neither `~/.claude/sessions`
nor `/proc`.

- **Sorted by attention**: `waiting`, then `idle`, then `busy`, then all else,
  with the most recent change first in each group. The status rank sorts above the
  fuzzy score, so the boundary between the agents that want you and the rest stays
  in one place as you type. The age sorts above the score too, because in one
  status the agent that changed a moment ago is the one you worked with.
- **A status passes through unchanged**, in capitals, behind a glyph:

  | | status | meaning |
  |---|---|---|
  | 🙋 | `waiting` | it asked you something and stopped |
  | ☕ | `idle` | it finished and waits for you. It is not asleep, and it sorts above a busy agent, so it takes a cup and not a 💤 |
  | ⠋⠙⠹⠸ | `busy` | a spinner. It is the one glyph that moves, because it is the one status that ends on its own |
  | 🐚 | `shell` | it is a shell |
  | 🛸 | all else | unknown, and visibly not one of the four |

  Those four are the full set of Claude Code 2.1.251, read from the binary
  (`"busy","shell","idle","waiting"`) and not from what happened to run. The table
  is thus complete today, which is not a reason to remove 🛸: the set grew by one
  (`shell`) between two releases.

  Every spinner frame is one column wide, so the spinner turns without a change of
  the tag column and no column to its right moves. The timer of the plugin runs at
  10 Hz for the spinner. The session poll behind it still runs once a second, and a
  tick that is neither a poll nor a spinner frame redraws nothing.

  **The table is decoration only.** It never decides whether a row shows or where
  it sorts, and it never replaces the word. A status added after this was written
  keeps its word, takes 🛸, and sorts last. A stale table thus costs a picture and
  not a row. In a pane too narrow for the word, the glyph stands alone, and an
  unknown status becomes its first letter instead, because 🛸 would draw two
  different unknown statuses in the same way.
- **The age is a duration, not a time, and it continues while you watch.** `35m`
  means that the agent has been idle for thirty-five minutes, which is the decision
  you make. `35m ago` would be a different statement. `claude-ps` is asked once and
  its answer is held, so the column adds the age of that answer instead of asking
  again. Without that, an agent that has waited three minutes would show `4s` for as
  long as the picker is open.
- **The cwd is its last two components**, so `misc/luneta` and not
  `/home/you/Projects/misc/luneta`. In a column of agents the first components are
  the same on every row and separate nothing. Two components, not one: in a
  136-path zoxide database, the last two collided zero times and the last one alone
  collided nine times.
- **A row is called what somebody called it**, and takes the zellij session name
  only when nobody did. `claude-ps` reports the name and the source of the name,
  and the source decides. A `derived` name is the basename of the cwd plus a
  suffix, so it would put the cwd on the row twice. Only `user` and `peer` are
  names that a person or another agent chose.

  A source this build does not know is rejected, which is the opposite of what an
  unknown status gets. Every status is a real state, so a hidden status hides a
  live agent. The sources that carry a chosen name are a short list, but Claude
  Code already writes `derived`, `collision`, `auto` and `hook`, so a new source is
  more probably another generated name. An absent source is accepted, because that
  is the state from before the key existed.
- **A `:pane` suffix appears only when two rows have the same label**, and the
  search term never matches it: you type the bare name. It appears exactly when the
  label stops identifying one target. Two agents in one session with different
  chosen names take no suffix.
- **You type what you see.** The fuzzy term matches the label of the row, so an
  agent that shows a chosen name answers to that name and not to the zellij session
  under it. That is one string: the highlight is a list of character offsets into
  whatever the matcher ran on, so a label from another source would paint hits onto
  characters the term never touched. `Enter` is not affected, because it goes to
  the `(session, pane)` of the row.
- **The list is held while the screen is up.** The snapshot is taken when the
  screen opens, which makes the attention-first order safe: no row moves while you
  read it. The ages on it still move, and that costs nothing, because the offset
  added to them is the same number on every row and cannot change a comparison. A
  new opening takes a new snapshot.
- **The agent whose pane you opened the picker from is not listed.** It is removed
  by `(session, pane)` and not by session, so another agent in the same session
  stays a valid target. An agent this screen cannot address is dropped as well,
  and silently: a row is a pane `Enter` puts you in, and an agent with no pane to
  go to is not a row that is missing — it is not a row.
- **Keys are read by name, and a document that does not parse is loud.** The wire
  is a JSON array. This screen names `status`, `status_age`, `zellij`, `cwd`,
  `name` and `name_source`, and ignores all else, so a new key costs nothing. A key
  this screen depends on stops the parse when it is absent, and the reason goes on
  the note line, because a partial list looks the same as no agents. `src/agents.rs`
  records what happened when `status_age` was tolerant instead.
- **`zellij` is one object or `null`.** A session and its pane arrive together or
  not at all, so `Enter` needs no guard against half an address. `null` means that
  the agent runs outside zellij, and this screen has nothing to offer it.

## What `Enter` does

> The highlighted row says what `Enter` does. With no highlight, `Enter` gives the
> text you typed to the host, which attaches, resurrects, or creates.

The second sentence is safe because a session name is unique across live and
resurrectable sessions, so the plugin never chooses between attach and create. It
gives the host one name, and the host resolves it: a live session gives an attach,
a saved layout gives a resurrect, and neither gives a create.

- A row is always highlighted while the list is not empty, starting at the top
  match. A keystroke or a `Backspace` returns the cursor to the top match. `Up`
  and `Down` move the cursor and stop at the ends. They do not wrap.
- **`Enter` creates immediately, with the default layout of the host.** There is
  no layout list, because it was a menu with the same answer every time. The
  create path thus has no confirmation: `Enter` on a name that matches nothing
  makes the session.
- **`Esc` closes the picker.** One press, from any of the three screens, at any
  highlight. `Ctrl-c` does the same. There is no state to back out of, so a name
  that matches a session that exists, such as `infra` while `infra-staging` is
  live, is reachable only when no row matches it.
- **An empty name is still valid.** `Enter` on an empty prompt with an empty list
  gives a session that the host names.
- Your own session name does nothing. The prompt gives the reason, because an
  error overlay would take your next keystroke.

### The prompt says what `Enter` does

The result is in the prompt, beside the term: `<ENTER> - Attach`,
`<ENTER> - Resurrect`, `<ENTER> - Create "desp"`. One sentence thus covers both
states of the rule and cannot contradict itself. Both refusals appear there too,
in the error colour of the theme: `already attached` when the term is the current
session, and the reason when the name is not legal, such as
`name cannot contain '/'`.

Below the list is the note line, which is dim, never a row and never selectable.
It says the one thing the list cannot: `you are in "despesas" — not listed`,
whenever your search reaches for the session the list omits.

Name validation runs in the prompt as you type, and the rename screen uses the
same validator. The host does not validate names on this path, because
`validate_session_name` is connected only to the CLI and the web client. The
plugin is thus the last check: length, `/`, `.`, `..` and whitespace only.

Keys: type to filter, `Up` and `Down` to move, `Backspace`, `Enter` to act, `Tab`
to change list, `Ctrl-r` to rename, `Del` to kill or delete, and `Esc` or `Ctrl-c`
to close.

### `Del` — kill a session, or delete a dead one

`Del` acts on the highlight immediately. There is no confirmation.

- A live row is **killed**. The session stops. It returns as a dead row only if
  the host had serialized it, which depends on `serialization_interval` (10s by
  default, and off when `session_serialization false`) and which the plugin cannot
  read.
- A dead row is **deleted**. The saved layout goes, and that is permanent.

To remove a running session for good, press `Del` on the live row and then `Del`
again on the dead row that follows.

The search term is cleared afterwards. It searched for a session that no longer
exists, and it would otherwise filter the list by a name with nothing behind it.

**`Del` cannot kill the session you are in.** That is not a guard: the current
session left the match set at the source and can never be the highlight.

There is no kill-all and no disconnect-others. Both act on sessions the picker
does not show.

### `Ctrl-r` — rename the session you are in

`rename_session` takes no target: it renames the current session and nothing else.
The current session is never a row here, so the screen can name it plainly:
`renaming "despesas" — the session you are in`.

The name is checked on each keystroke, not on `Enter`: empty, unchanged, in
collision with a live or resurrectable session, or against the rules the search
prompt applies. The reason sits where the result would be, so `Enter` on a refused
name does nothing that you did not expect. Collisions are checked against the whole
snapshot and not the filtered list, because a name that collides with a session the
term excludes would otherwise pass.

### `Tab` — the places you work

The directory screen lists directories from **zoxide**, in the frecency order of
zoxide, and `Enter` puts you in one. The search term is shared, so `Tab` asks the
other list the same question.

**A directory row is a proposed session name and a cwd, and the cwd applies only
if the name is free.** The host makes that rule. `switch_session_with_cwd` carries
the cwd to `ClientInfo::set_cwd`, which matches `New` and `Resurrect` and discards
all else through a `_ => {}`. Give it the name of a live session and you attach to
that session, wherever it is, with no error and no cwd.

The prompt therefore names the outcome the host will choose, and it is computed
against the session snapshot on every poll, so a session created elsewhere changes
a create row into an attach row under the cursor:

- **Create** — the name is free. The session is made, in that directory.
- **Attach to** / **Resurrect** — the name is taken. The name goes to the host
  alone. The host would accept a cwd and discard it, and a discarded argument makes
  you believe a session is somewhere it is not.
- **already in this session** — the name is the current session, and `Enter` is
  refused. That is not a courtesy: an attach to the current session does not fail,
  it panics the client.

The plugin cannot verify an attach. Neither `SessionInfo` nor `PaneInfo` has a cwd,
so nothing can ask a live session which directory it is in. An attach row means
that a session of this name exists, not that it is in this directory.

**The session name is the directory itself**, so `~/Projects/Work/bipa.git/master`
gives `master`. Two directories can end in the same component, and the plugin
cannot tell that the existing session of that name is somewhere else. In a
136-path zoxide database, nine names collided that way: `master`, `backend`,
`frontend`, `bin`, `nixos`, `skills`, `.claude`, `.config` and `ldk-server`. On
those rows, read the path beside the name before you press `Enter`.

A name cannot contain a `/`, which matters twice: the host refuses such a name,
and only logs the refusal, and so does the validator of the plugin.

### What the directory screen costs

- **A new permission.** `RunCommands`, because zoxide is reached with
  `run_command(["zoxide", "query", "--list", "--score"])`. The wasi sandbox of the
  plugin opens only `/host`, `/data`, `/cache` and `/tmp`, so `db.zo` is both out
  of reach and binary. The host grants the set and not the item, so this prompts
  once more, and a refusal also stops the session list.
- **Not a poll.** zoxide is asked on the permission grant and again on `Visible`,
  and not on the timer. A timer would start one process a second to read a database
  that changes only when you `cd`. `launch-or-focus` means that one instance serves
  many openings, which is what `Visible` is for.
- **A second command, for the preview box.** `eza` on the directory the cursor
  stopped on, delayed, cached by path, and asked at most once for each directory
  for each opening. The same permission covers it. The session and agent screens
  spend that permission on `zellij action dump-screen`. See
  [the preview box](#the-preview-box).
- **Nothing, when either is missing.** Without zoxide, or with a refused grant, the
  screen says which: `zoxide is not available` on the note line. The three ways to
  be empty (waiting, failed, and nothing to show) are three different messages.
  Without eza, the list still works and the preview box says `eza is not
  available`.
- **No `-a`.** Without it, zoxide removes directories that no longer exist, which
  is the one check for stale data that the plugin cannot do itself.

  eza is not asked to filter anything. It reports a directory it may not open on
  stderr and still exits 0, so an empty listing with a message beside it is read as
  a failure and not as an empty directory.
- **`Del` does nothing here.** To remove a directory from zoxide is a different
  action on a different store.

## The renderer

The renderer draws through the `Text` component of zellij and the
`print_*_with_coordinates` family, as the built-in session manager does. A `Text`
goes to the host as a DCS payload, and the host colours it from the active theme.
The picker thus follows your theme and its selection colours, with no palette to
carry and no `ModeUpdate` subscription to keep current. Colour is expressed as
emphasis levels and not as colours. Weight has one user: the host draws every
character of a `Text` bold unless it is told otherwise, so the row content is made
not bold and the box titles are the only bold text left.

A row is one `Text` across the width, measured here, and not a set of `Table`
cells. A `Table` cannot express a bordered row, a last column cannot sit against
the right border in one, and an empty cell crossed the wire as a cell that the host
dropped, which moved every later cell one place left.

One reduction ladder remains, on the agent screen: shorten the tag, remove the
cwd, remove the age. The age stays longer than the cwd, because the session name
usually names the project, and nothing else says how long an agent has waited.
Column widths are measured over the visible window and not the whole list, so one
long name cannot cost every other row its age column.

## `Enter` on an agent is two calls, not one

Every other row in this picker ends in `switch_session`. An agent row cannot,
because agents are reachable at pane granularity and one of those panes can be in
the session you are in. An attach to your own session does not fail: it reaches a
bare `panic!("You are trying to attach to the current session")`
(`src/commands.rs:793`) and stops the client.

The row therefore carries the call that applies:

- a **different** session: `switch_session_with_focus(name, None, Some((pane, false)))`,
  which attaches and lands on the pane, and not on the default pane of the session;
- **our own** session: `focus_terminal_pane(pane, …)`, a pane focus.

That division also removes the need for a refusal. A directory row for the current
session can do nothing safely, but an agent row for the current session has a call
that works.

## The geometry is arithmetic, and it is tested

Where things go lives in `src/layout.rs` and calls nothing on the host: the boxes,
the division down the middle, the list at the bottom, and the truncation. It was
once inline in five render functions, where only an installed plugin could show
whether it was correct. `cargo test` now renders whole panes to strings and asserts
on the picture, at every width and height the pane can take. The rule it checks is
that every line of a box is as wide as the box and closed at both ends.

The pane draws no frame but the boxes of the picker. Zellij frames a floating pane
by default, so the picker sat inside a third box that it did not draw and could not
style. The fix is the `borderless` flag on `FloatingPaneCoordinates`, which rides
on the `change_floating_panes_coordinates` call the plugin already makes to set its
size. `set_pane_frame_style` is a session setting and would have removed the frame
from every other pane.

## Why this repo is standalone

The plugin is vendored and not kept as a patch against a zellij checkout. It needs
only `zellij-tile` from crates.io, so the whole workspace bought nothing. The
picker also diverges from the upstream session manager, which has tabs and pane
drill-down that this one cuts, so upstream changes were never going to apply.

The cost: no upstream fix arrives on its own, and a zellij upgrade means a change
to the `zellij-tile` pin by hand and a rebuild.

## Installing

One config line. Zellij fetches the plugin itself, so there is no clone, no rust
toolchain and no `make`:

```kdl
plugins {
    // ...
    luneta location="https://github.com/lorenzolfm/luneta/releases/download/v0.1.0/luneta-0.1.0.wasm"
}
```

An `http` or `https` location parses to `RunPluginLocation::Remote`
(`layout.rs:629`), and the downloader follows redirects (`downloader.rs:59`), which
is what makes a GitHub release asset work: those redirect to
`objects.githubusercontent.com`.

Zellij prompts once for `RunCommands`, `ReadApplicationState` and
`ChangeApplicationState`. Read the first one twice: it is how the picker runs
`zoxide`, `claude-ps`, `eza` and `zellij` itself.

### Verifying what you got

**Zellij does not verify a plugin it downloads.** Its downloader checks no
checksum, no signature and no attestation. It fetches the URL and runs the bytes.
The config line above thus gives you TLS and the word of GitHub. Everything in this
section is something you run.

Every release ships a `SHA256SUMS` and a build provenance attestation. To get the
guarantee, download first, check, and install from disk:

```sh
v=0.1.0
gh release download "v$v" -R lorenzolfm/luneta -p 'luneta-*.wasm' -p SHA256SUMS
sha256sum -c SHA256SUMS
gh attestation verify "luneta-$v.wasm" -R lorenzolfm/luneta
install -m 0644 "luneta-$v.wasm" ~/.local/share/zellij/plugins/luneta.wasm
```

Then point `config.kdl` at the `file:` path and not at the URL.

The checksum of the current release, so that the usual case is one command and no
second file to fetch:

```sh
echo '70f1ecf01907b96b8371abec12b079e49e637271a4a361d5d3cb1618074137ca  luneta-0.1.0.wasm' \
    | sha256sum -c
```

The two checks make different claims:

- `sha256sum -c` proves that the bytes are complete: not truncated and not
  corrupted in transit. It proves nothing about their origin, because anyone who
  could replace the `.wasm` could replace the `SHA256SUMS` beside it.
- `gh attestation verify` says where the bytes came from: that this artifact was
  built by `ci.yml`, at this commit, in this repo, on a GitHub runner. It is
  keyless, signed against the OIDC identity of the runner, so there is no
  maintainer key to guard, to rotate, or to lose. There is no PGP signature, which
  would make a weaker claim and add a key to guard.

**An upgrade is a config edit, and the version in the filename is why it works.**
The downloader caches by the last path segment of the URL and returns immediately
if that file exists. It reads no ETag and no timestamp, and never fetches again
(`downloader.rs:88-92`, cached under `~/.cache/zellij/<zellij-version>/`). An asset
named `luneta.wasm` would hold you at the build you downloaded first. Point the
line at the new release to move, and expect one new permission prompt, because the
cache keys on the location string and that string changed.

That `.wasm` is the whole install. The other two screens each want one program on
the `PATH` of the zellij **server**, and neither is required:

| screen | tool | without it |
|---|---|---|
| sessions | — | always works |
| directories | [`zoxide`](https://github.com/ajeetdsouza/zoxide) | `zoxide is not available` on the note line |
| directories | [`eza`](https://github.com/eza-community/eza) | the list works; the preview box says `eza is not available` |
| agents | [`claude-ps`](https://github.com/lorenzolfm/claude-ps) | `claude-ps is not available` on the note line |

The icons in the directory preview want a [Nerd Font](https://www.nerdfonts.com).
Without one they draw as empty boxes, and `--icons=always` is the one flag to
remove from `LIST` in `src/dirs.rs`.

Both are found by name. Nothing in the source names an install path. Note that it
is the `PATH` of the server and not of your shell: the server takes it from
whatever started it, which on a long session can be older than your profile.

## Building it yourself

You need this only to work on the plugin. To use it, see
[Installing](#installing).

**The system rustc cannot build this.** Nix ships `std` for the host triple only,
and there is no `rustup` to add a target with, so `wasm32-wasip1` fails with
`error[E0463]: can't find crate for 'std'`. The flake solves it: it takes the
`rust-overlay` toolchain pinned to rust 1.95.0, which is the channel the
`rust-toolchain.toml` of zellij names, with `wasm32-wasip1` added.

```sh
nix develop          # or: direnv allow, once
make install
cargo test           # the unit tests, on the host target
```

`make install` builds and copies the `.wasm` to
`~/.local/share/zellij/plugins/luneta.wasm`, which is the `file:` path the rest of
this README uses. A local build and a release asset are the same bytes by two
routes. Only the location string differs, and it must be spelled the same way in
every binding.

**Keep that path stable.** Zellij caches granted permissions against the absolute
path of the `.wasm` (`~/.cache/zellij/permissions.kdl`), so a move or a rename
makes zellij prompt again.

### Cutting a release

CI builds through the flake, so the published `.wasm` comes off the same pinned
toolchain as a local `make build`. The rust version is written down once, in
`flake.nix`.

```sh
# Change `version` in Cargo.toml first. The workflow refuses a tag that disagrees.
git tag v0.2.0 && git push origin v0.2.0
```

`.github/workflows/ci.yml` builds on every PR and releases on a `v*` tag. The
`release` job does not build. It takes the artifact the `build` job produced, so
the bytes in a release are the bytes a PR check passed, and the build is written
down once.

It renames that artifact to `luneta-<version>.wasm`, writes a `SHA256SUMS` over it,
and cuts a keyless build provenance attestation. The rename matters for the caching
reason above.

Then update the checksum in
[Verifying what you got](#verifying-what-you-got). It is the one part of a release
that cannot be automatic, because the hash does not exist until the tag is built.

**Actions are pinned by commit SHA, not by tag.** `@v7` is a ref that the repo of
the action can move at any time, so a tag pin trusts every future commit of that
repo. The threat model for a plugin distributed this way is mostly this pipeline:
whatever runs here can put bytes in front of users under our name. The comment
after each SHA is a label for a person, and the SHA is the pin. Change both
together.

CI builds for `wasm32-wasip1`, which a build for the host target does not check.
The unit tests run on the host target with `cargo test`.

## The edit and see-it loop

An incremental rebuild takes about 4.5s, and a plugin reload takes about 4ms. There
is no zellij restart and no new permission prompt.

```sh
make dev             # build, install, and reload the plugin pane
```

Or by hand:

```sh
make install
zellij action launch-or-focus-plugin --skip-plugin-cache --floating \
    file:$HOME/.local/share/zellij/plugins/luneta.wasm
zellij action start-or-reload-plugin \
    file:$HOME/.local/share/zellij/plugins/luneta.wasm
```

**Nothing is closed.** The two calls are both necessary, because neither covers
both states. `launch-or-focus-plugin` is the only one that can create the picker
when it does not run, and the only one that can pass `--floating`, which is the
geometry the key binding gives it. On a picker that runs, it takes focus.
`start-or-reload-plugin` reads the `.wasm` from disk again and puts it into the
pane that is already there, which is what loads the new bytes. Focus or create
first, so that there is something to reload.

`--skip-plugin-cache` is necessary on the creating path. Without it, zellij reuses
the module it holds in memory for that path and ignores your new bytes. The module
is inserted into the cache after every load (`plugin_loader.rs:306`), so a new pane
is not enough.

Do not return to a close-and-relaunch pair. That version had to name the pane it
closed, and it could not: zellij documents a `plugin_<id>` on the stdout of
`launch-or-focus-plugin` ("Returns: Plugin pane ID"), but 0.45.1 prints nothing.
The guard thus saw an empty id and refused to run, which was correct: `close-pane`
without an id closes the focused pane, which during development is usually your
editor. A reload needs no pane id.

Add `SESSION=<name>` to any recipe to drive a session other than the current one.

## Binding it to `Ctrl-j`

The alias from [Installing](#installing) is half of it. The bindings must point at
`"luneta"` and not at `"session-manager"`.

A change to the alias block **needs a zellij restart**. A change to the `.wasm`
behind it does not.

### Your location string

Everything below writes the location in full and does not use the alias. **Write
the form you installed with.** The two forms are interchangeable wherever a
location appears, and nothing else in the binding changes:

```kdl
"https://github.com/lorenzolfm/luneta/releases/download/v0.1.0/luneta-0.1.0.wasm"
"file:/home/you/.local/share/zellij/plugins/luneta.wasm"
```

A `file:~` URL also works. The tilde is preserved and expanded by the shell
(`layout.rs:605-607,619`), so an absolute path is not required.

**Do not mix the two forms.** Zellij identifies a plugin instance by its location,
so a `file:` binding and an `https:` binding are two different plugins: you get two
floating panes, one over the other.

### Opening straight onto a screen

A key can open the picker on a chosen screen. Pair `LaunchOrFocusPlugin` with a
`MessagePlugin` that names the screen: `sessions`, `agents` or `dirs`.

```kdl
bind "Ctrl a" {
    LaunchOrFocusPlugin "file:/home/you/.local/share/zellij/plugins/luneta.wasm" {
        floating true
        move_to_focused_tab true
        skip_plugin_cache true
    }
    MessagePlugin "file:/home/you/.local/share/zellij/plugins/luneta.wasm" {
        name "screen"
        payload "agents"
    }
    SwitchToMode "normal"
}
```

Three things about that:

- **Both actions need the full `file:` path, not the `luneta` alias.**
  `MessagePlugin` does not resolve the alias to the running instance. It starts a
  second, hidden instance and sends the message to that one, and the visible picker
  stays on the screen it showed. Mixing the two forms across bindings has the same
  result. Use the path everywhere, including in the binding that opens the picker
  normally.
- **`MessagePlugin` alone is not enough.** It starts the plugin if it does not run,
  but it starts it hidden and without focus: the pane exists and you can neither
  see it nor type into it. `LaunchOrFocusPlugin` first is what makes it visible.
- **Give every such binding its own `MessagePlugin`.** A plain
  `LaunchOrFocusPlugin` only takes focus, so after one key moves the picker to the
  agents it stays there, and the key that opened the sessions no longer does. When
  each key names its screen, each key lands where it says.

### When `claude-ps` is not on the `PATH` of the server

The alternative is one plugin configuration key, `agents_command`, which names the
program to run instead:

```kdl
bind "Ctrl a" {
    LaunchOrFocusPlugin "file:/home/you/.local/share/zellij/plugins/luneta.wasm" {
        floating true
        move_to_focused_tab true
        skip_plugin_cache true
        agents_command "/opt/tools/claude-ps"
    }
    // ... the MessagePlugin, unchanged
}
```

- **It is a program, not a command line.** Arguments are not split out of it,
  because a path can contain a space. Put the tool in a script if it needs
  arguments.
- **Every binding must pass the same value, or no binding must pass one.** Zellij
  identifies a plugin instance partly by its configuration, so two keys with
  different values give two picker panes, one over the other. To leave the key out
  everywhere is the usual case.
- There is no equivalent for `zoxide`. It is a packaged program on every `PATH`,
  and `claude-ps` is one you build yourself.

## Notes for the next change

- **A `file:` plugin is not a builtin, so it gets no free permissions.** A builtin
  passes every permission check (`zellij_exports.rs:5428`). This plugin must call
  `request_permission` itself. Without that call, a command is denied silently, and
  the only record is a `log::error!` in
  `/tmp/zellij-1000/zellij-log/zellij.log`.
- **`launch-or-focus-plugin` needs a connected client.** Against a detached session
  it logs `No connected clients found - cannot load or focus plugin`, and the CLI
  still exits 0. Attach a client, such as a pty through `script`, before you drive
  it without a terminal.
- `get_session_list()` returns a `SessionListSnapshot { live_sessions,
  resurrectable_sessions }`, which holds both lists in one call. The picker polls
  it once a second and takes the whole list from that one snapshot. It does not
  subscribe to `SessionUpdate`, which refreshes the age of the current session only
  and leaves the others unchanged.
- **Absolute coordinates are safe, and newlines are not.** The host clears the
  viewport of the plugin pane before each render (`plugin_pane.rs:243`), so
  `print_text_with_coordinates` can place anything anywhere and nothing
  accumulates. To print `rows` lines each ending in `\n` into a pane of `rows`
  rows, on the other hand, scrolls the first line off the top without a message. A
  plugin pane has no scrollback.
- **A plugin sees one server, so anything across sessions goes through the CLI.**
  `get_pane_scrollback` and every `PaneId` in the API belong to this session, and
  the whole list of the picker is other sessions. `zellij --session NAME action ...`
  connects to the socket of that session and is the only route. Its replies are
  asynchronous: `dump-screen --path` returns before the server writes the file, and
  without `--path` the promised STDOUT is empty because the CLI exits first.
- **A reply must carry what it was about.** A `RunCommandResult` arrives with the
  context map the command carried, which is the only link to the question. The
  directory preview puts the path in that map: eza answers at any time, the cursor
  has usually moved, and an answer filed under the current cursor shows the contents
  of another place with no sign of an error.
- **A background poll must not move the cursor.** To rebuild the list once a second
  is correct. To move the selection to row 0 while doing it takes the cursor away
  from the user every second. Move to the top match when the term changes, and hold
  the selected session by name on a poll.
- **The plugin has no clock.** A wasi sandbox with `/host`, `/data`, `/cache` and
  `/tmp` open is not a clock, so "how long ago" is answered by a count of the 10 Hz
  animation ticks of the plugin. That is what lets the ages on the agent screen move
  without a new call to `claude-ps`: the frame that received the snapshot is the
  origin, and the difference divided by ten is the offset. It drifts with what the
  host does to the timer, which a column rounded to the second absorbs.

## Driving it without a terminal

`zellij action` makes the output of the picker an artifact you can diff, with no
interaction. Every one of these problems exits 0 and does nothing:

- `launch-or-focus-plugin` needs a connected client. See above.
- `dump-screen` takes `--path`, and that path must be absolute.
- **`dump-screen --ansi` produces nothing at all** in 0.45.0: an empty file with
  `--path`, and empty output without it. Styling, such as the selection highlight
  and the match characters, thus cannot be verified this way. Verify the selection
  through an effect that shows in plain text: move it past the bottom of a short
  pane and diff the rows that scroll into view.
- Arrow keys go through `action write` as raw bytes: `write 27 91 66` for `Down`,
  `write 27 91 65` for `Up`, `write 127` for `Backspace`, and `write 27` for `Esc`.
  `write-chars` is for literal text.
- A session takes the size of its **smallest** attached client, so one extra 80x24
  client holds the pane narrow however wide the client you care about is. Drive a
  session with exactly one client of the size you want.
- `dump-screen` dumps the **focused pane** and not the whole screen, so it cannot
  see the status bar.

### Seeing where `Enter` landed

`Enter` moves the client, which no `zellij action` reports. Attach the test client
through a pty you control and log it. After a switch, the client attaches again in
the same process, and the tab bar of the new session carries its name, so
`grep -ao 'Zellij ([a-z0-9-]*)'` over that log says where you ended up. Fork the
pty with `pty.fork()` and set its size with `TIOCSWINSZ`. `script` takes the size
of the calling terminal, which is how you get a surprise 80x24 client that holds
the pane narrow.

`pkill -f drive.py` **kills the shell that runs it**, because `pkill -f` matches
the command line of its own caller. Use `pgrep -f 'drive[.]py' | xargs -r kill`.

## License

MIT. Zellij is MIT-licensed too.

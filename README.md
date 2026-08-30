# luneta

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
╭─ luneta ───────────────────────── 2/4 ─╮╭─ dotfiles ─────────────────────────────╮
│                                        ││ 2 tabs, 3 panes, 1 client              │
│                                        ││                                        │
│                                        ││ 1  editor                              │
│                                        ││    · nvim                              │
│ you are in "notes" — not listed        ││    · fish                              │
│   luneta                        2h ago ││ 2  server                              │
│ > dotfiles                      5h ago ││    · cargo watch -x test               │
│   🪦 Dead sessions ─────────────────── ││                                        │
│   despesas-old                  1w ago ││                                        │
│   api-spike                     5w ago ││                                        │
╰────────────────────────────────────────╯╰────────────────────────────────────────╯
╭─ Sessions ───────────────────────────────────────────────────────────────────────╮
│ > _                                                               <ENTER> Attach │
╰──────────────────────────────────────────────────────────────────────────────────╯
  <↓↑> Nav <ENTER> Select <TAB> Agents <Ctrl r> Rename <Del> Delete <ESC> Close
```

and, one `Tab` away:

```
╭─ luneta ───────────────────────── 1/3 ─╮╭─ luneta ───────────────────────────────╮
│                                        ││ waiting                                │
│                                        ││ 18m in this status                     │
│                                        ││                                        │
│                                        ││ session  misc                          │
│                                        ││ pane     12                            │
│                                        ││ cwd      …lorenzo/Projects/misc/luneta │
│ 1 agent not in zellij — not listed     ││                                        │
│ > luneta  🙋  18m          misc/luneta ││                                        │
│   notes   ☕  5m     lorenzo/Documents ││                                        │
│   bipa    ⠋   31m            Work/bipa ││                                        │
╰────────────────────────────────────────╯╰────────────────────────────────────────╯
╭─ Agents ─────────────────────────────────────────────────────────────────────────╮
│ > _                                                       <ENTER> Go to "luneta" │
╰──────────────────────────────────────────────────────────────────────────────────╯
  <↓↑> - Navigate, <ENTER> - Go to agent, <TAB> - Directories, <ESC> - Close
```

and one more `Tab` on from there:

```
╭─ luneta ───────────────────────── 1/4 ─╮╭─ misc-luneta ────────────────────── 5 ─╮
│                                        ││ /home/lorenzo/Projects/misc/luneta     │
│                                        ││                                        │
│                                        ││ src/                                   │
│                                        ││ target/                                │
│                                        ││ Cargo.toml                             │
│                                        ││ Makefile                               │
│ > misc-luneta   …/Projects/misc/luneta ││ README.md                              │
│   misc-homelab  …Projects/misc/homelab ││                                        │
│   Work-bipa     …zo/Projects/Work/bipa ││                                        │
│   .local-bin    …me/lorenzo/.local/bin ││                                        │
╰────────────────────────────────────────╯╰────────────────────────────────────────╯
╭─ Directories ────────────────────────────────────────────────────────────────────╮
│ > _                                                 <ENTER> Create "misc-luneta" │
╰──────────────────────────────────────────────────────────────────────────────────╯
  <↓↑> - Navigate, <ENTER> - Go there, <TAB> - Sessions, <ESC> - Close
```

Those three are drawn by the renderer itself, not by hand:
`cargo test -- --ignored --nocapture print_the_screens` prints them.

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

### The preview box

The box on the right answers the question every row of every screen begs and no
row has the width to answer: a session's name does not say what is running in
it, a directory's name does not say what is in it, and an agent's label does not
say what it is stuck on. It follows the highlight, and each screen fills it from
a different place.

**A live session** shows what it is made of, then its tabs with their panes
under them, the tab you would land on in the accent colour.

🔴 That is real data, not an estimate. Every zellij server writes its own tabs
and panes to `session-metadata.kdl` about once a second, and every other server
reads them back (`zellij-utils/src/sessions.rs`, `read_live_session_states`), so
the picker can say what is inside a session it is not attached to — and say it
from the same one-second snapshot the ages beside it come from. The panes are
filtered to the **selectable, unsuppressed** ones: zellij's own tab bar and
status bar are panes in that manifest like any other, and counting them would
report every tab as holding two panes it does not have.

**A resurrectable session** has none of that — its layout is on disk in a form
the host will not hand a plugin — so the box says `not running` and what there
is instead, rather than showing `0 panes` and implying it has none.

**A directory** shows its path and an `ls`, directories first, with the entry
count in the border. That is one host command per directory, so:

- **It is debounced.** The cursor has to sit still for two animation ticks (a
  fifth of a second) before anything is asked. Holding `↓` down a hundred-odd
  zoxide entries would otherwise fork a process for every one it passed over, to
  show you the last.
- **It is cached by path**, and the cache is dropped whole when it fills or when
  the picker is opened again — a listing from the last time you looked is a
  claim about how a directory *was*.
- **Replies are filed by the path they went out with**, never by the cursor's
  position when they land. `ls` answers whenever it answers and the cursor has
  usually moved on; without the path on the reply, a slow one would confidently
  show you somewhere else's contents.
- A directory that cannot be read says why, in the error colour.

**An agent** shows its status and how long it has been in it — the routing
decision, at the top — then the session and pane `Enter` would take you to, and
its cwd in full rather than the two components the row has room for.

The box is **half the pane, and it goes when the pane is narrow**: below 52
columns there is no room for two boxes that can say anything, so the list takes
the width back. Same ladder as the borders and the help row. The confirm and
rename screens keep the whole width — they are one question each and have
nothing to preview.

⚠️ The preview does not scroll and cannot: the cursor is in the list beside it,
and every key that could move it means something there. Content that overruns
loses its tail and the last row says how much (`… 2 more`).

### The agent screen

The list comes from `claude-ps`, a separate tool on `$PATH` that joins what
Claude Code publishes about itself (`~/.claude/sessions/<pid>.json`) to the pane
each agent is running in. The plugin cannot do that join itself: its wasi
sandbox preopens only `/host`, `/data`, `/cache` and `/tmp`, so neither
`~/.claude/sessions` nor `/proc` is readable from inside it.

- **Sorted attention-first** — `waiting`, then `idle`, then `busy`, then
  anything else — with the most recently changed first inside each group. The
  status rank sits *above* the fuzzy score, so the boundary between "wants you"
  and "does not" stays a fixed landmark while you narrow instead of shuffling on
  every keystroke, and age sits above it too: within one status the agent that
  changed a moment ago is the one you were just working with.
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
- **The age is a duration, not a timestamp**, and it **counts on while you
  watch**. `35m` means "has been idle for thirty-five minutes", which is the
  routing decision; `35m ago` would be a different claim. `claude-ps` is asked
  once and its answer frozen (below), so the column adds how long ago that was
  rather than re-asking — otherwise an agent that has been waiting three minutes
  reads `4s` for as long as you leave the picker open, on the one column the
  decision is made on.
- **The cwd is its last two components**, so `misc/luneta` rather than
  `/home/you/Projects/misc/luneta`. Down a column of agents the leading
  components are the same on every row and separate nothing. Two rather than
  one for the reason the directory screen derives names from two: measured
  across a real 136-path zoxide database, the last-two form collided zero
  times where the bare basename collided nine ways.
- **A row is called what someone called it**, and the zellij session name only
  when nobody did. `claude-ps` reports the name *and who chose it*, and the
  second half is the load-bearing one: a `derived` name is the basename of the
  cwd plus a suffix, so showing it would put the cwd on the row twice — once as
  a name that looks chosen, once as the cwd it was copied from. Only `user` and
  `peer` are a name a person or another agent picked.

  ⚠️ A source this build does not recognise is **suppressed**, which is the exact
  opposite of what an unrecognised *status* does. The asymmetry is deliberate on
  both sides. Every value in the status vocabulary is a real state, so hiding one
  hides a live agent. The name sources that carry a chosen name are a short list
  and the machinery is the long one — Claude Code already writes `derived`,
  `collision`, `auto` and `hook` — so a source invented tomorrow is far likelier
  to be more machinery, and trusting it would put a generated name where a chosen
  one belongs. An *absent* source is trusted, because that is the state before
  the key existed rather than a word this build failed to place.
- **A `:pane` suffix appears only when two rows are called the same thing**, and
  it is never what the search term matches — you type the bare name. It shows up
  exactly when the label stops identifying one target: two agents in one session
  that carry different chosen names take no suffix, and two that fall back to the
  session name collide exactly as they did before names were read at all.
- **You type what you see.** The fuzzy term is matched against the row's label,
  so an agent shown by its chosen name is reached by that name and *not* by the
  zellij session underneath it. That is one string, deliberately: the highlight
  is a list of character offsets into whatever the matcher ran on, so a label
  that came from somewhere else would paint hits onto characters the term never
  touched. `Enter` is unaffected either way — it goes to the row's own
  `(session, pane)`, whatever the row is called.
- **A glance, not a watch** — a frozen *list*, though, not a frozen clock. The
  snapshot is taken when the screen opens and held while it is up, which is what
  makes attention-first ordering safe: nothing reorders while you read it. The
  ages on it still move, and that costs the guarantee nothing, because the offset
  added to them is the same number on every row — a uniform offset cannot flip a
  comparison between two of them. Reopening takes a fresh snapshot and the clock
  restarts from it.
- **The agent whose pane you opened the picker from is not listed**, dropped by
  `(session, pane)` rather than by session — a *sibling* agent in the same
  session is a legitimate target and stays. Agents running outside zellij are
  dropped too, and counted on the note line so they are never silently absent.
- **There is no token count.** There was one, between the age and the cwd, and
  `claude-ps` withdrew the `context` key it came from: the transcript path was
  built from the cwd, and Claude Code writes that path with a `-` for each `/`
  and each `.`, so `/home/x/.config` and `/home/x-config` slug the same. The
  count was probable rather than certain, and it was that tool's only unbounded
  read — a 256 KiB tail of a file with no size limit, per agent, per call, from
  a consumer that polls. The column went with the key rather than staying on as
  a rung of the width ladder no producer can reach.
- 🔴 **Keys are read by name, and a document that will not deserialise is loud.**
  The wire is a JSON array; this screen names only `status`, `status_age`,
  `zellij`, `cwd`, `name` and `name_source` and ignores the rest. A key it has never heard of costs
  nothing — which is the point, because the count used to be checked *exactly*
  and `claude-ps` gaining `started_at` was a hard failure on a screen that
  understood every other field. A key it *depends* on going missing still stops
  the parse and puts the reason on the note line, because a picker rendering half
  a list it cannot vouch for looks exactly like "no agents are running".

  ⚠️ That last sentence is a promise every depended-on key has to actually keep,
  and `status_age` did not. It was read as `age` behind a `#[serde(default)]`,
  so when `claude-ps` renamed the key the parse did not stop — every row got a
  default `0` and the column read `0s` for every agent, in every status, forever.
  A silent zero is worse than a blank: it is a confident answer that happens to
  be wrong, on the column the routing decision is made on. Tolerance is for keys
  this screen does *not* read; the ones it does are strict, and `age` survives as
  a serde alias so an older `claude-ps` still parses.
- **`zellij` is one object or `null`.** A session and its pane arrive together or
  not at all, so there is no half-address for `Enter` to guard against; `null`
  means the agent is outside zellij and is counted on the note line.

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
- **`Esc` closes.** One press, from any of the three screens, whatever is
  highlighted. `Ctrl-c` does the same.

  ⚠️ It used to do two other things first — back out of a secondary screen, and
  then drop the highlight — and the second of those was load-bearing. With the
  selection always on, `Enter` always takes a row, so a dropped highlight was the
  only way to ask for a *name* that fuzzy-matches a session that already exists:
  `infra` while `infra-staging` is live. That is now out of reach unless nothing
  matches. Deliberate: a key that dismisses on the third press is a key you press
  three times.
- **An empty name is still a feature**: `Enter` on an empty prompt with nothing
  in the list gives a host-named scratch session.
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
`Tab` to swap lists, `Ctrl-r` to rename, `Del` to kill or delete (and `Del` again to
do both), `Esc` or `Ctrl-c` to close.

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

A **second `Del`, on the confirm screen**, escalates a live session's kill into a
kill *and* a delete. That is the whole point of it: without it, getting rid of a
running session for good meant killing it, waiting for it to come back into the
list as a dead row, finding it again, and deleting *that*. The escalation changes
the question rather than answering it — the verb becomes `Delete` and the
consequence line becomes the irreversible one — so `Enter` is still what commits,
and the screen you commit from is the screen that told you what it costs. On a
row that is already dead there is nothing to escalate, so the key is not offered.

The kill and the delete are two blocking host calls issued back to back, and the
delete is skipped if the kill came back `Err` — `delete_dead_session` would
otherwise throw away the saved layout of a session that is still running.

**Either way the search term is cleared afterwards.** It was searching for a
session that no longer exists, and leaving it in place left the list filtered by
a name with nothing behind it.

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
- **A second command, for the preview box.** `ls -1Ap -- <path>`, on the
  directory the cursor has settled on — debounced, cached by path, and asked at
  most once per directory per opening. Same permission, no new prompt. See
  [the preview box](#the-preview-box).
- **Nothing, when it is missing.** No zoxide, or a denied grant, and the screen
  says which — `zoxide is not available` on the note line. The three ways to be
  empty (waiting, failed, nothing to show) are not collapsed into a blank list.
- **No `-a`.** Without it zoxide omits directories that no longer exist, which
  is the one bit of staleness filtering the plugin could not do for itself.
- **`Del` does nothing here.** Dropping a directory out of zoxide is a different
  verb against a different store, and it does not belong on a key whose confirm
  screen talks about killing processes and throwing away saved layouts.

### The renderer is a rewrite, not a port

It draws through zellij's own `Text` component and the
`print_*_with_coordinates` family, exactly as the built-in session manager does.
A `Text` is serialized to the host as a DCS payload and coloured *there*, from
the active theme, so the picker follows your theme and its selection colours
with no palette to carry and no `ModeUpdate` subscription to keep current.
Colour is expressed as emphasis *levels* rather than colours, and weight has
exactly one user: the host draws every character of a `Text` bold unless told
otherwise, so bold said nothing until the row content was unbolded — which
leaves the box titles as the only bold text on the screen.

`Table` is not one of the components used, though it was. A row is one `Text`
spanning the width, measured here, because a bordered row is not a thing a
`Table` can express, because a trailing column cannot be pushed flush against
the right border in one, and because an empty cell used to cross the wire as a
cell that was *dropped* — sliding every cell after it one place left and letting
a row eat the first cell of the row below.

What is left of upstream's 1847-line `ui/components.rs` is the reduction ladder,
and only the agent screen still needs one: abbreviate the tag → drop the cwd →
drop the age. Age outranks cwd because the session name usually already names
the project, while nothing else says how long an agent has been stuck. Column
widths are measured over the **visible window**, not the whole list, so one very
long name cannot cost every other row its age column.

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

### The geometry is arithmetic, and it is tested

Where things go lives in `src/layout.rs` and touches no host call: boxes, the
split down the middle, the bottom-anchored list, the truncation. It used to live
inline in five render functions, where the only way to check it was to install
the plugin and look at a floating pane. Now `cargo test` renders whole panes to
strings and asserts on the picture — including the invariant a bordered layout
lives or dies by, which is that *every* line of a box is exactly as wide as the
box and closed at both ends, at every width and height the pane can be.

The pane draws no frame of its own but the picker's. Zellij frames a floating
pane by default, so the picker used to sit inside a third box it did not draw
and could not style; the fix is the per-pane `borderless` flag on
`FloatingPaneCoordinates`, which rides on the `change_floating_panes_coordinates`
call the plugin was already making to fix its size. The obvious lever,
`set_pane_frame_style`, is a *session* setting and would have unframed every
pane behind it too.

## Why this repo exists standalone

Vendored rather than kept as a patch against a zellij checkout. The plugin only
needs `zellij-tile` from crates.io — not the zellij source tree — so carrying the
whole workspace bought nothing. The picker deliberately diverges from upstream
(tabs and pane drill-down are cut), so inheriting upstream's session-manager
changes was never going to be useful.

The cost of that choice: upstream fixes never arrive, and a zellij upgrade means
bumping the `zellij-tile` pin by hand and rebuilding.

## Installing

One config line. Zellij fetches the plugin itself — no clone, no rust
toolchain, no `make`:

```kdl
plugins {
    // ...
    luneta location="https://github.com/lorenzolfm/luneta/releases/download/v0.1.0/luneta-0.1.0.wasm"
}
```

`http` and `https` locations parse to `RunPluginLocation::Remote`
(`layout.rs:629`) and the downloader follows redirects (`downloader.rs:59`),
which is what makes a GitHub release asset work at all — those 302 to
`objects.githubusercontent.com`.

Zellij will prompt once for `RunCommands`, `ReadApplicationState` and
`ChangeApplicationState`. The first is the one worth reading twice: it is how
the directory and agent screens shell out to `zoxide`, `claude-ps` and `ls`.

### Verifying what you got

🔴 **Zellij does not verify plugins it downloads.** There is no checksum,
signature or attestation check anywhere in its downloader — it fetches the URL
and runs the bytes. So the config line above gives you TLS and GitHub's word,
and nothing else. Everything in this section is something *you* run; none of it
happens on its own.

Every release ships a `SHA256SUMS` and a build provenance attestation. To
actually get the guarantee, download first, check, and install from disk:

```sh
v=0.1.0
gh release download "v$v" -R lorenzolfm/luneta -p 'luneta-*.wasm' -p SHA256SUMS
sha256sum -c SHA256SUMS
gh attestation verify "luneta-$v.wasm" -R lorenzolfm/luneta
install -m 0644 "luneta-$v.wasm" ~/.local/share/zellij/plugins/luneta.wasm
```

then point `config.kdl` at the `file:` path instead of the URL.

The published checksum for the current release, so the common case is one
command and no second file to fetch:

```sh
echo '70f1ecf01907b96b8371abec12b079e49e637271a4a361d5d3cb1618074137ca  luneta-0.1.0.wasm' \
    | sha256sum -c
```

**What each half proves**, because they are not the same claim and the
difference is the whole point:

- `sha256sum -c` proves the bytes are **intact** — not truncated, not corrupted
  in transit. It proves nothing about **origin**: anyone who could replace the
  `.wasm` could replace the `SHA256SUMS` sitting beside it.
- `gh attestation verify` is the one that says where the bytes came from — not
  "someone with a key signed this" but "this exact artifact was built by
  `ci.yml`, at this commit, in this repo, on a GitHub-hosted runner". It is
  keyless: signed against the runner's OIDC identity and recorded in a public
  transparency log, so there is no maintainer key to guard, rotate, or lose.

There is deliberately no PGP signature. For a CI-built artifact it would make a
weaker claim than the attestation already does, while adding a private key to
guard — and it would be guarding an artifact that, per the 🔴 above, the default
install path never checks anyway.

One consolation for the plain URL install: zellij caches by filename and never
re-fetches (see the ⚠️ below), so a version you verified once cannot be quietly
swapped underneath you later.

⚠️ **Upgrading is a config edit, and the version in the filename is why it
works.** The downloader caches by the *last path segment of the URL* and
returns early if that file already exists — no ETag, no timestamp, no re-fetch,
ever (`downloader.rs:88-92`, cached under `~/.cache/zellij/<zellij-version>/`).
An asset named plainly `luneta.wasm` would pin you forever to whichever build
you downloaded first. Point the line at the new release to move; expect one
fresh permission prompt, since the cache keys on the location string and that
string just changed.

That `.wasm` is the whole install. The other two screens each want one ordinary
program on the zellij **server's** `PATH`, and neither is required:

| screen | tool | without it |
|---|---|---|
| sessions | — | always works |
| directories | [`zoxide`](https://github.com/ajeetdsouza/zoxide) | `zoxide is not available` on the note line |
| agents | [`claude-ps`](https://github.com/lorenzolfm/claude-ps) | `claude-ps is not available` on the note line |

Both are looked up **by name**. Nothing in the source names an install path, so
where you keep them is your business — but note it is the *server's* `PATH`, not
your shell's: the server inherits it from whatever launched it, which on a
long-lived session may be older than your current profile.

## Building it yourself

Only needed to hack on the plugin — to *use* it, see [Installing](#installing).

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
`~/.local/share/zellij/plugins/luneta.wasm`, which is the `file:` path the rest
of this README uses. A local build and a release asset are the same bytes
reached two ways; the location string is the only thing that differs, and it
has to be spelled the same way in every binding either way.

⚠️ **Keep that path stable.** Zellij caches granted permissions against the
**absolute path** of the `.wasm` (`~/.cache/zellij/permissions.kdl`), so moving
or renaming it makes zellij prompt for permissions again.

### Cutting a release

CI builds through the flake, so the published `.wasm` comes off the same pinned
toolchain as a local `make build` — the rust version is written down once, in
`flake.nix`.

```sh
# bump `version` in Cargo.toml first; the workflow refuses a tag that disagrees
git tag v0.2.0 && git push origin v0.2.0
```

`.github/workflows/ci.yml` builds on every PR and releases on a `v*` tag. The
`release` job does **not** build: it takes the artifact the `build` job
produced, so the bytes attached to a release are the bytes a PR check went green
on, and the build is written down once rather than drifting between two
pipelines.

It renames that artifact to `luneta-<version>.wasm` on the way out, writes a
`SHA256SUMS` over it, and cuts a keyless build provenance attestation. The
rename is load-bearing for the caching reason above, not cosmetic.

Then update the checksum printed in [Verifying what you got](#verifying-what-you-got).
It is the one thing in the release that is not automatic, because it cannot be:
the hash does not exist until the tag has been built.

🔴 **Actions are pinned by commit SHA, not by tag.** `@v7` is a ref the action's
own repo can repoint at any time, so pinning to it trusts every commit that repo
will ever make. The threat model for a plugin distributed this way is mostly
this pipeline: whatever runs here can put bytes in front of users under our
name. The trailing `# v7` is a label for humans; the SHA is the pin. Bump both
together, deliberately.

There is no `cargo test` step, because there is nothing to run — the crate has
no test modules, and a green step asserting nothing is worse than no step. What
CI does check is that the code still compiles for `wasm32-wasip1`, which is the
failure a host-target build would miss.

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
    file:$HOME/.local/share/zellij/plugins/luneta.wasm
zellij action start-or-reload-plugin \
    file:$HOME/.local/share/zellij/plugins/luneta.wasm
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

## Binding it to `Ctrl-j`

The alias from [Installing](#installing) is half of it; the bindings have to be
re-pointed at `"luneta"` instead of `"session-manager"`.

Changing the alias block **requires a zellij restart**; changing the `.wasm`
behind it does not.

### Your location string

Everything below writes the location out in full rather than using the alias,
for the reason in the first 🔴 note. **Whichever form you installed with is the
form to write** — they are interchangeable everywhere a location appears, and
nothing else in the binding changes:

```kdl
"https://github.com/lorenzolfm/luneta/releases/download/v0.1.0/luneta-0.1.0.wasm"
"file:/home/you/.local/share/zellij/plugins/luneta.wasm"
```

A `file:~` URL is fine too — the tilde is preserved and shell-expanded
(`layout.rs:605-607,619`), so an absolute path is not required.

🔴 **Do not mix the two forms.** Zellij keys a plugin instance on its location,
so a `file:` binding and an `https:` binding are two different plugins: you get
two floating panes stacked over each other, the same trap as the
`agents_command` mismatch further down.

### Opening straight onto a screen

A key can open the picker *on* a chosen screen by pairing `LaunchOrFocusPlugin`
with a `MessagePlugin` naming it — `sessions`, `agents` or `dirs`:

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

Three things about that, each found the hard way:

- 🔴 **Both actions need the full `file:` path, not the `luneta` alias.**
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

### When `claude-ps` is not on the server's `PATH`

The escape hatch is one plugin configuration key, `agents_command`, naming the
executable to run instead:

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

- **It is an executable, not a command line.** Arguments are not split out of
  it, because a path may contain a space and the split would be ambiguous. Wrap
  the tool in a script if you need arguments.
- 🔴 **Every binding must pass the same value, or none of them may.** Zellij keys
  a plugin instance partly on its configuration, so two keys disagreeing about
  this mint *two* pickers and leave two floating panes stacked over each other —
  the same trap that put the screen selection on a `MessagePlugin` rather than
  on configuration. Leaving the key out everywhere is the ordinary case.
- There is deliberately no equivalent for `zoxide`. It is a packaged program on
  everyone's `PATH`; `claude-ps` is one you build yourself.

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
- **Absolute coordinates are safe, and newlines are not.** The host deletes the
  plugin pane's viewport before feeding it each render
  (`plugin_pane.rs:243`), so `print_text_with_coordinates` can place anything
  anywhere and nothing accumulates. Printing `rows` lines each terminated by
  `\n` into a `rows`-tall pane, on the other hand, scrolls the first line off
  the top, silently — a plugin pane has no scrollback of its own.
- **A reply must carry what it was about.** `RunCommandResult` arrives with the
  context map the command went out with, and that is the only thing tying it to
  the question. The directory preview puts the *path* in there: `ls` answers
  whenever it answers, the cursor has usually moved on by then, and filing the
  answer under wherever the cursor is now shows you somewhere else's contents
  with no sign anything went wrong.
- **A background poll must not reset the cursor.** Rebuilding the result list
  once a second is correct; snapping the selection to row 0 while doing it drags
  the cursor out from under the user every second. Snap on a *search-term*
  change; hold the selected session by name on a poll.
- **The plugin has no wall clock.** A wasi sandbox with `/host`, `/data`,
  `/cache` and `/tmp` preopened is not a clock, so "how long ago did that
  happen?" is answered by counting the plugin's own 10Hz animation ticks. That
  is what lets the agent screen's ages move without re-running `claude-ps`: the
  frame the snapshot landed on is the anchor, and the difference divided by ten
  is the offset. It drifts with whatever the host does to the timer, which a
  column rounded to the second absorbs.

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

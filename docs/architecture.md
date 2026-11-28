# Architecture

lune is four programs. Two Rust crates hold the wire protocol and the model
they share. This note says which program does what, which crate holds it, who
owns which state inside the daemon, and how a request flows through them. A
crate's paths are relative to `crates/`; `proto/`, `ios/` and `skills/` sit at
the root of the repository.

## The processes

```text
  lune (TUI)     lune <verb>               phone                luned of another machine
      │  │            │                      │                          ▲
      │  │            │ luned.sock           │ tailnet :7473            │ ssh: lune bridge
      │  │            ▼                      ▼                          │
      │  └─────────▶ luned ◀─────────────────┴──────────────────────────┘
      │               │
      │ mux.sock      │ mux.sock
      ▼               ▼
  lune-mux: the pseudoterminals, and what each one's program shows
```

- **`lune`** (`lune/`) is the terminal UI: the sidebar of hosts, projects,
  worktrees and views, and the views of the worktree you are on, drawn with
  ratatui. With a subcommand (`lune start`, `lune prune`, `lune agents`, ...)
  it asks luned one thing and exits instead. It links `luned` as a library, for
  the client, the folders and some of the work it does in its own process.
- **`luned`** (`luned/`) is the daemon, one per user on each machine. It keeps
  the projects and their views, follows the terminals of lune-mux and the
  agents in them, polls git and GitHub, watches other machines, and serves all
  of that to lune and the phone.
- **`lune-mux`** (`mux/`) is lune's multiplexer. It holds the pseudoterminals
  and reads each one's program with libghostty-vt, as Ghostty does. It knows
  terminals, not projects: which view a terminal belongs to is luned's to know.
- **The phone** is an iPhone app (`ios/`) on a Rust core (`phone/`). It is a
  client of luned over the tailnet, as lune is over the socket, with fewer
  verbs.

### Who starts whom

- `lune` finds luned's socket, asks its `Info`, and starts `luned --detached`
  when none holds the lock, or ends an older build for its own
  (`lune/src/daemon.rs`). `start`, `watch` and `bridge` start luned when needed;
  the other subcommands only reach a running one, with `info`, `agents` and
  `say` exiting 5 when none runs.
- `luned` starts `lune-mux` the first time it needs a terminal
  (`luned/src/mux/own/server.rs`). `lune` never starts lune-mux.
- `luned` ends when its socket is gone or replaced, or after 30s with no
  lune-mux terminal and no watcher (`luned/src/daemon.rs`). lune-mux ends when
  its last terminal closes (`mux/src/mux.rs`), when its socket is no longer the
  one it bound, or after 30s if it never made a terminal (`mux/src/serve.rs`).
  Closing `lune` ends neither, so the shells and agents in its views run on.
- A lock beside each socket keeps it to one luned and one lune-mux per runtime
  folder. `projects.lock` in the state folder refuses a second luned that runs
  from another runtime folder.

### Where they talk

Each socket speaks gRPC (tonic, over HTTP/2), with the messages in `proto/`:

| Socket | Server | Service | Clients |
| --- | --- | --- | --- |
| `luned.sock` | luned | `lune.v1.Daemon` (`proto/lune/v1/daemon.proto`) | lune, `lune <verb>`, the luned of another machine through `lune bridge` |
| `<tailnet address>:7473` | luned | `lune.v1.Daemon`, narrowed (`luned/src/daemon/over_tailnet.rs`) | the phone |
| `mux.sock` | lune-mux | `lune.mux.v1.Mux` (`proto/lune/mux/v1/mux.proto`) | luned; lune, for the frames of a terminal on this machine |
| `alerts.sock` | the lune holding `alerts.lock` | none: lines of text | the other lunes open on this machine (`lune/src/peers.rs`) |

## The crates

| Crate | Path | What it holds |
| --- | --- | --- |
| `lune` | `lune/` | The TUI and the CLI. `host.rs` runs the UI, `sidebar.rs` and `viewed.rs` draw it, `commands.rs` holds the subcommands. |
| `luned` | `luned/` | The daemon, and the library lune links for its client, its folders and its helpers. |
| `lune-mux` | `mux/` | The multiplexer: ptys (`pty.rs`), each terminal's screen through libghostty-vt (`screen.rs`), its foreground program (`front.rs`), and the server (`serve.rs`). |
| `lune-domain` | `domain/` | What projects, worktrees, views and layouts are, apart from any IO: how a change to them is checked, how they are kept as text, and the rows a client reads them into. |
| `lune-protocol` | `protocol/` | The code generated from `proto/`, and what every side reads the same way: agent and program statuses, git's status, the ask box's turns, the limits on the wire. |
| `lune-phone` | `phone/` | The phone's core: a luned client that keeps a model of the hosts it watches, called from Swift through UniFFI. |
| `lune-phone-bindgen` | `phone-bindgen/` | UniFFI's Swift generator, pinned to the version lune-phone builds with. |
| `lune-waits` | `waits/` | The one way the tests wait: on an event, never on the clock. |

`skills/lune/SKILL.md` is built into luned, as what an agent in lune's ask box
reads about the `lune` commands.

## Inside luned

luned is a few actors, each owning some state. Two of them, core and layouts,
are threads that take commands on a bounded channel, and the state a client
sees lives in them. The others are threads or tokio tasks that watch something,
a file, a stream or a socket, and report what they see to those two. The gRPC
handlers own nothing. They ask core or layouts and wait for the answer, on the
one shared tokio runtime (`luned/src/tasks.rs`).

| Actor | Where | Owns | Hears from |
| --- | --- | --- | --- |
| **core** | `luned/src/core.rs` | Everything a client watches: the agents found and their statuses, lune-mux's terminals and which view each is in, git statuses, pull requests, the other machines, a copy of the projects, and the `Host` it builds of them, with its version and the watches parked on it | the other actors, and the handlers |
| **layouts** | `luned/src/layouts.rs` | The projects file and its writes, the clients showing a worktree, and which terminal of lune-mux each view has. The only actor that makes or closes a terminal. | the handlers, `owns`, `folders` |
| **owns** | `luned/src/owns.rs` | lune-mux's `Watch` stream: the terminals and their program statuses | lune-mux |
| **folders** | `luned/src/projects.rs` | Which project folders are missing, and each clone's worktrees | the file system |
| **git**, **gh**, **fetches**, **topics** | `git.rs`, `github.rs`, `fetches.rs`, `topics.rs` | Their own polling: git status, pull requests and checks, fetches, what each agent's session is about | core, which they ask what to poll |
| agent watchers | `watch.rs`, `claude.rs`, `pi.rs`, `agy.rs`, `codex.rs` | The agents' session files on disk, and the thread each Codex holds a writer lock on | the file system; `codex.rs` asks core which agents are Codex's |
| **remote** | `luned/src/remote.rs` | The `hosts` file, and one ssh bridge per machine in it | the other machines' luned |
| **tailnet** | `luned/src/tailnet.rs` | The tailnet listener, and which devices it admits | tailscaled |

core is where the state a client sees comes together, and layouts is where the
projects change. Every other actor reports to one of the two.

### How a request flows

`lune start feat/sidebar`, in a clone `wt clone` made:

1. lune reaches luned on `luned.sock`, starting it if need be, and asks its
   `Watch` which worktrees the projects already hold
   (`worktrees::holders` in `lune/src/worktrees.rs`).
2. `lune/src/start.rs` makes the worktree with wt's code, in lune's own process,
   unless one of the branch is there already. It builds a `Change::Join` for it
   and calls `ChangeProjects`.
3. luned's handler (`Host::change_projects` in `luned/src/daemon.rs`) forwards
   the change over the bridge if it names another machine. Here, it hands the
   change to layouts.
4. layouts checks the change against the projects (`lune-domain`), writes the
   projects file, tells core the new projects, and asks lune-mux for a
   `NewTerminal` for the view.
5. layouts answers with what it made and opened, and the handler returns that
   to lune.
6. lune-mux lists the new terminal on its `Watch` stream. `owns` hands the
   listing to core and layouts, and core rebuilds its `Host`.
7. Run in a terminal of lune-mux, lune then sends a `Change::Bring`, which
   takes each lune whose focus is on that terminal to the new worktree.
8. The sidebar of every lune open, and the phone, get the new `Host` on their
   next `Watch`, and show the new worktree.

### How state reaches clients

`Watch` sends the whole `Host` each time, never a delta. Each `Host` carries a
version, luned's pid and a sequence number that core bumps when the `Host`
changes. A client sends the version it has. core answers at once if it differs,
which covers a luned that restarted, and otherwise parks the call until the
`Host` changes. The handler waits on core for 4s at most, and then answers with
the `Host` as it is. `lune watch --after <pid>:<seq>` is the same call
from the shell.

The frames of a terminal come another way. For one on this machine, lune asks
luned which terminals the worktree's views are (`Show`) and then reads their
frames from lune-mux itself. For one on another machine, and for the phone,
luned relays them (`Frames`).

### Other machines

`lune ssh <destination>` adds a line to the `hosts` file. luned runs
`ssh -T <destination> lune bridge` for each line, and `lune bridge` pipes that
ssh to the luned on the other side. luned watches the other luned's `Host`
through the bridge, and core folds it into its own, so the sidebar shows every
machine. A verb on another machine's project goes over the same bridge.

### The phone

luned listens on its tailnet address, port 7473, when it runs from your own
home. It admits a device only when Tailscale says it is another machine of the
same user, and its ID is in the `devices` file. Over the tailnet, luned serves
only what `luned/src/daemon/over_tailnet.rs` lets through, and refuses the rest
as not served:

- `Watch`, `Waiting` and `Answer`: what the hosts hold, and the agents waiting
  on you.
- `Frames`: a terminal's frames, with keys, pastes and scrolls typed into it,
  but no mouse or focus.
- `Ask`, `ListWork`, `FileIssue`, `IssueRepositories`, `Prunes`, `Prune` and
  `Rebase`.
- `MakeWorktree`, only when it opens an agent.
- `ChangeProjects`, only to close a view or a worktree, rename a project, a
  worktree or a terminal, or reorder projects or worktrees.

## Files

| Folder | Holds |
| --- | --- |
| `$XDG_RUNTIME_DIR/lune`, else `/tmp/lune-<uid>` (the user's temp folder on macOS) | `luned.sock`, `luned.lock`, `luned.log`, `mux.sock`, `mux.lock`, `alerts.sock`, ssh's control sockets |
| `$XDG_STATE_HOME/lune`, else `~/.local/state/lune` | `projects`, `hosts`, `devices`, `lune.log`, and what the TUI remembers: the sidebar's width, what is folded, the harness picked last |
| `~/lune` | clones lune made, always listed beside what zoxide knows |

# PROTOTYPE findings: a server that emulates, clients that get the grid (#331 step 2)

Throwaway. This branch is never merged. Its question: can one server own the
pseudoterminals, emulate each pane with alacritty_terminal, and send its clients
only the grid, and still keep Claude Code, nvim and fish working?

## Shape

- `proto-mux server`: one process for every session (the user chose this on
  2026-11-11), with one pane per session. It is started on demand by the first
  client, runs `setsid`, and exits when its last session ends.
- Each session runs alacritty's `EventLoop`, `Term` (10k lines of scrollback,
  kitty keyboard on), and the far end held open, as `hosted.rs` does.
- Each client gets a writer thread. A `Wakeup` marks the client dirty, and the
  marks merge into one. The writer then reads the grid under the lock, unlocks,
  and sends the rows that changed. Frames are at least 4ms apart. Each client
  has its own scrollback offset.
- The client asks its terminal for OSC 10/11 and passes them to the server,
  which answers the panes' colour queries with them. It encodes keys, mouse,
  paste and focus with luneta's own `keys.rs` (included by path) for the pane's
  `TermMode`, and draws with ratatui inside a synchronized update.

## Measured (`drive/`, pyte as the outer terminal, 2026-11-11)

| | direct | proto-mux (release) | zellij 0.45.1 |
|---|---|---|---|
| key echo, median | 0.07ms | **0.9ms** | 11.5ms |
| key echo, p90 | 0.10ms | 1.2ms | 11.7–13.3ms |
| `seq 1 200000` | 1.3s (pyte parses it all) | 105–117ms | 53–74ms |

- A debug build echoed in 2.7ms, and reading the grid took ~460µs. Release
  fixes both.
- fish (`--no-config`) and nvim (`--clean`): after scripted steps, the screen
  text was the same as a direct run's, line for line. The cursor matched at
  each step. Style differences came from pyte misreading `CSI > 4;1 m` as
  bold. Through proto-mux, pyte only sees ratatui's plain SGR, so the proxied
  screen was the correct one.
- Claude Code 2.1.322: the same through typing, kitty `Shift Enter` (it asks
  for kitty flags 5), `Ctrl U` and `Esc`. The only lines that differed were a
  random tip and timed hints.
- Two clients on one session, scrollback in one while the other stays put,
  detach and reattach (the screen comes back), resize (`stty size` follows), the
  program's exit reaching every client, and the server outliving a crashed
  client all work.

## Gaps the real server has to own

Each is from the server's log of what alacritty_terminal 0.26 left unhandled:

- **XTVERSION** (`CSI > q`): fish and Claude Code ask, and nothing answers.
  The server should answer with its own name.
- **Mode 2031** (dark/light reports): fish and nvim ask, and alacritty does
  not know it. luneta already hears 2031 from its own terminal (`answers.rs`),
  so the server has to keep the mode per pane and pass the reports on.
- **Mode 2027** (grapheme clustering): reported as unknown. 👍🏽 becomes two
  wide cells, and 👨‍👩‍👧 three. This is no regression. zellij drops the
  combining accent and the ZWJs, and luneta's `hosted.rs` splits clusters
  today.
- **XTGETTCAP** (`DCS + q`, from fish) and **DECRQSS** (`DCS $ q`, from nvim):
  unanswered, and both programs fall back without them.
- **OSC 8 links and undercurl** reach the client, but ratatui can draw neither.
  `hosted.rs` loses them today too. Fixing this means writing those cells
  outside ratatui.
- **OSC 133** prompt marks (fish) are unhandled, and they are free events:
  "prompt shown", "command started", "command ended with status". That is the
  events-and-latency win the user cares about most.
- Unknown: **OSC 7501 ?** from Claude Code.
- The last client's size wins for everyone. Clients of different sizes need a
  rule, such as the smallest wins, as tmux and zellij do.

## Not tested here

A real terminal (Ghostty) as the outer one: real colours, kitty keys from a
real keyboard, the mouse in nvim, image protocols, and long Claude Code turns.

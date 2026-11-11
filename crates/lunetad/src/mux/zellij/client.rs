//! What luneta, which hosts zellij's client beside its sidebar, makes of that
//! client's ways: what it types into it for zellij to move it, and what the
//! client says as zellij drops it.

use std::time::Duration;

/// What luneta types into zellij's client before it asks zellij to move it,
/// and ahead of that, as the keys or a press of the mouse come to luneta,
/// where the client typed nothing in its session yet: `ScrollLock` let go,
/// as the kitty keyboard protocol spells it, with no modifiers and the event
/// of a release, `1:3`. zellij moves the client of a session that last sent
/// it a key, and a client that has sent none since it came to the session,
/// as luneta's has not once the sidebar took it there, moves for no action.
/// The client counts a key let go as a key. No one binds `ScrollLock`, and
/// zellij writes nothing of it to a pane that does not read keys as kitty
/// spells them. One that does gets it as it came, a release it
/// never asked to hear of, and passes it over: nvim does, where the key
/// pressed would have it insert a character of the protocol's own in the
/// text. The client reads it as kitty spells it unless its config turns the
/// protocol off: then zellij takes it for no key, and hands the pane
/// nothing. So `nudge` gives it only to a client that asks its terminal for
/// kitty's keys, and zellij moves one that does not only once you type in
/// it. As you rename a tab or a pane, zellij takes a key it binds nothing to
/// for text, and writes the nudge's, `[57359;1:3u`, into the name: only a
/// client that typed in its session can be renaming there.
const NUDGE: &[u8] = b"\x1b[57359;1:3u";

/// `NUDGE`, where zellij's client reads it as a key: as `kitty_keys` says,
/// it asked its terminal, luneta's, for keys as kitty spells them.
pub fn nudge(kitty_keys: bool) -> Option<&'static [u8]> {
    kitty_keys.then_some(NUDGE)
}

/// How long an action waits, after the client typed `NUDGE` or another key,
/// for zellij to take it in, as it goes to zellij through its client while
/// the action goes straight to its server.
pub const NUDGED: Duration = Duration::from_millis(30);

/// What zellij's client writes as its server drops it, with no white space: on
/// a terminal as narrow as luneta's zellij's, a line wraps at any character.
/// One of its first lines, and one of its last, for a terminal too short to
/// hold them all, which scrolls the first away.
const DROPPED: [&str; 2] = ["lostconnectiontothezellijserver", "Reattachtoyourprevioussession"];

/// Whether `said`, what zellij's client left on its terminal as it ended well,
/// is what it writes as its server drops it, as the server does one that falls
/// behind, and the session runs on. Nothing else tells that from a client that
/// detached: zellij 0.45.1's ends well for both, measured, and takes its screen
/// down. Where a later zellij words it otherwise, the client counts as one that
/// detached, and ends luneta, whose log has what it said: better that than that
/// luneta takes back a client you detached. It can still err that way, by a
/// hair: zellij's own word to a client goes unsent where its queue is full, so
/// that one you detach, or whose session is killed, just as that fills hears
/// this. A second detach then ends luneta.
pub fn dropped(said: &str) -> bool {
    let said: String = said.split_whitespace().collect();
    DROPPED.iter().any(|words| said.contains(words))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What zellij's client writes as its server drops it reads however its
    /// terminal cuts it: one as narrow as luneta's zellij's can be wraps its
    /// lines at any character, and one too short to hold them scrolls the first
    /// away. Nothing else a client of zellij's says as it ends well does, with
    /// a session running on.
    #[test]
    fn a_dropped_client_is_told_by_what_it_says_however_its_terminal_cuts_it() {
        const DROPPED_SAYS: &str = "
Your zellij client lost connection to the zellij server.

As a safety measure, you have been disconnected from the current zellij session.
However, the session should still exist and none of your data should be lost.

This usually means that your terminal didn't process server messages quick
enough. Maybe your system is currently under high load, or your terminal
isn't performant enough.

There are a few things you can try now:
    - Reattach to your previous session and see if it works out better this
      time: `zellij attach luneta`
    - Try using a faster (maybe GPU-accelerated) terminal emulator
    ";
        let on_terminal = |columns: usize, rows: usize| {
            let chars = |line: &str| line.chars().collect::<Vec<_>>();
            let cut: Vec<String> = DROPPED_SAYS
                .lines()
                .flat_map(|line| {
                    let line = chars(line);
                    if line.is_empty() {
                        return vec![String::new()];
                    }
                    line.chunks(columns).map(|row| row.iter().collect()).collect()
                })
                .collect();
            cut[cut.len().saturating_sub(rows)..].join("\n")
        };
        for (columns, rows) in [(120, 40), (90, 40), (50, 24), (20, 40), (90, 8), (40, 8)] {
            let said = on_terminal(columns, rows);
            assert!(dropped(&said), "not told on {columns}x{rows}: {said}");
        }
        let others = [
            "Bye from Zellij!",
            "Session detached",
            "Session was detached from this client (possibly because another client connected)",
            "Disconnected by host",
            "",
        ];
        for said in others {
            assert!(!dropped(said), "told of {said}");
        }
    }
}

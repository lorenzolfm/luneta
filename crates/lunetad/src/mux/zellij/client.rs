use std::time::Duration;

const NUDGE: &[u8] = b"\x1b[57359;1:3u";

pub fn nudge(kitty_keys: bool) -> Option<&'static [u8]> {
    kitty_keys.then_some(NUDGE)
}

pub const NUDGED: Duration = Duration::from_millis(30);

const DROPPED: [&str; 2] = ["lostconnectiontothezellijserver", "Reattachtoyourprevioussession"];

pub fn dropped(said: &str) -> bool {
    let said: String = said.split_whitespace().collect();
    DROPPED.iter().any(|words| said.contains(words))
}

#[cfg(test)]
mod tests {
    use super::*;

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

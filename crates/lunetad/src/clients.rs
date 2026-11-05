//! The clients attached to a zellij session, as `zellij action list-clients`
//! lists them: each by its id, and the pane it has the focus on. zellij does
//! not say whose a client is, and luneta finds its own by its id.
//!
//! zellij lists a client that is attached, once, in the tab it is on: one
//! that left is gone from the list, where `list-panes` keeps the focus it had
//! (`list_clients_metadata` in zellij-server 0.45.1's
//! `session_layout_metadata.rs`).

use std::collections::HashSet;
use std::process::Command;
use std::time::Duration;

use crate::api;
use crate::cli::{answer_of, heard, PaneId};
use crate::program;

/// How long zellij has to list a session's clients: a server that runs
/// answers at once.
const LIMIT: Duration = Duration::from_secs(2);

/// The line zellij 0.45 starts its list with, which names its columns.
const HEADER: &str = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND";

/// A client attached to a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Client {
    /// Its id, which it keeps while it stays in the session.
    pub id: u32,
    /// The pane it has the focus on.
    pub pane: PaneId,
}

impl Client {
    /// As lunetad says it: by the terminal it is on, none on a plugin's pane.
    #[must_use]
    pub fn to_api(self) -> api::Client {
        api::Client { id: self.id, pane: self.pane.terminal() }
    }
}

/// The clients in what zellij listed, by their ids, or why it does not read.
/// After the line that names the columns, zellij writes one for each client:
/// its id, the pane it has the focus on, as `terminal_3` or `plugin_2`, and
/// what runs there, which can hold spaces. zellij lists each client once.
pub fn parse(said: &str) -> Result<Vec<Client>, String> {
    let mut lines = said.lines().filter(|line| !line.trim().is_empty());
    if lines.next().map(str::trim_end) != Some(HEADER) {
        return Err(format!("it did not start with {HEADER:?}"));
    }
    let mut clients = Vec::new();
    for line in lines {
        let mut words = line.split_whitespace();
        let (Some(id), Some(pane)) = (words.next(), words.next()) else {
            return Err(format!("a line with no pane: {line:?}"));
        };
        let id = id.parse().map_err(|_| format!("no client's id: {id:?}"))?;
        let number = |prefix| pane.strip_prefix(prefix).and_then(|n| n.parse::<u32>().ok());
        let pane = match (number("terminal_"), number("plugin_")) {
            (Some(terminal), _) => PaneId::Terminal(terminal),
            (None, Some(plugin)) => PaneId::Plugin(plugin),
            (None, None) => return Err(format!("no pane's id: {pane:?}")),
        };
        clients.push(Client { id, pane });
    }
    let mut ids = HashSet::new();
    if let Some(twice) = clients.iter().find(|client| !ids.insert(client.id)) {
        return Err(format!("client {} comes twice", twice.id));
    }
    clients.sort_unstable_by_key(|client| client.id);
    Ok(clients)
}

/// The clients of `session`, as `zellij` lists them, or why it did not in
/// `LIMIT`: a command of zellij's, set up as its caller needs, which this
/// asks for them.
pub fn list(mut zellij: Command, session: &str) -> Result<Vec<Client>, String> {
    zellij.args(["--session", session, "action", "list-clients"]);
    let ask = || {
        let ran = program::run(&mut zellij, LIMIT)
            .map_err(|error| format!("zellij could not be run: {error}"))?;
        let said = answer_of(ran)?;
        String::from_utf8(said).map_err(|_| "zellij listed what is not UTF-8".to_owned())
    };
    let read = |said: &str| {
        parse(said).map_err(|why| format!("zellij listed what does not read: {why}"))
    };
    heard(ask, read)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As zellij 0.45.1 pads its columns, a plugin's pane with its URL for
    /// what runs, and a command with spaces in it.
    #[test]
    fn each_client_is_read_by_its_id_and_its_terminal() {
        let said = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\
                    2         plugin_4       file:/x/luneta.wasm\n\
                    1         terminal_3     tail -f /dev/null\n\
                    3         terminal_0     N/A            \n";
        let client = |id, pane| Client { id, pane };
        let (on, plugin) = (PaneId::Terminal, PaneId::Plugin);
        assert_eq!(parse(said), Ok(vec![client(1, on(3)), client(2, plugin(4)), client(3, on(0))]));
        assert_eq!(parse("CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n"), Ok(Vec::new()));
    }

    /// lunetad says no plugin's pane to what reads it, which goes by terminals.
    #[test]
    fn lunetad_says_the_terminal_a_client_is_on_and_no_plugin() {
        let said = |pane| Client { id: 1, pane }.to_api().pane;
        assert_eq!(said(PaneId::Terminal(3)), Some(3));
        assert_eq!(said(PaneId::Plugin(3)), None);
    }

    #[test]
    fn a_list_that_does_not_read_says_why() {
        let header = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n";
        assert!(parse("").is_err());
        assert!(parse("1 terminal_3 fish\n").is_err());
        assert!(parse(&format!("{header}one terminal_3 fish\n")).is_err());
        assert!(parse(&format!("{header}1 window_3 fish\n")).is_err());
        assert!(parse(&format!("{header}1\n")).is_err());
        let twice = parse(&format!("{header}1 terminal_3 fish\n1 terminal_4 fish\n"));
        assert_eq!(twice, Err("client 1 comes twice".to_owned()));
    }
}

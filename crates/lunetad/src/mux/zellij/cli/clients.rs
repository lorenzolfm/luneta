//! The clients attached to a zellij session, as `zellij action list-clients`
//! lists them: each by its id, and the pane it has the focus on. zellij does
//! not say whose a client is, and luneta finds its own by its id.
//!
//! zellij lists a client that is attached, once, in the tab it is on: one
//! that left is gone from the list, where `list-panes` keeps the focus it had
//! (`list_clients_metadata` in zellij-server 0.45.1's
//! `session_layout_metadata.rs`).

use std::ops::Deref;

use super::{laid, Failure, Laid, PaneId};
use crate::api;
use crate::mux::SessionName;

/// The line zellij 0.45 starts its list with, which names its columns.
const HEADER: &str = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND";

const ID_COLUMNS: usize = 9;
const PANE_COLUMNS: usize = 14;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    clients: Vec<Client>,
    set_aside: Vec<Client>,
}

impl Listed {
    #[must_use]
    pub fn set_aside(&self) -> &[Client] {
        &self.set_aside
    }
}

impl Deref for Listed {
    type Target = [Client];

    fn deref(&self) -> &[Client] {
        &self.clients
    }
}

/// The clients in what zellij listed, by their ids, or why it does not read.
/// After the line that names the columns, zellij writes one for each client,
/// by its id: that id, the pane it has the focus on, as `terminal_3` or
/// `plugin_2`, and what runs there, which can hold spaces and lines of its own.
fn parse(said: &str, commands: &[(PaneId, String)]) -> Result<Listed, String> {
    let said = said.trim_start();
    let (header, mut rest) = said.split_once('\n').unwrap_or((said, ""));
    if header.trim_end() != HEADER {
        return Err(format!("it did not start with {HEADER:?}"));
    }
    let mut listed = Listed { clients: Vec::new(), set_aside: Vec::new() };
    while !rest.is_empty() {
        let text = rest;
        let (line, next) = text.split_once('\n').unwrap_or((text, ""));
        rest = next;
        let Some((client, command_at)) = row(line) else {
            if listed.is_empty() && !line.trim().is_empty() {
                let columns: Vec<&str> = line.split_whitespace().take(2).collect();
                return Err(format!("no client's row: {columns:?}"));
            }
            continue;
        };
        if listed.last().is_some_and(|last| client.id <= last.id) {
            listed.set_aside.push(client);
            continue;
        }
        listed.clients.push(client);
        let known = commands.iter().find(|(pane, _)| *pane == client.pane);
        if let Some(past) = known.and_then(|(_, known)| text[command_at..].strip_prefix(known)) {
            rest = past.split_once('\n').map_or("", |(_, next)| next);
        }
    }
    Ok(listed)
}

fn row(line: &str) -> Option<(Client, usize)> {
    let id: u16 = line.get(..ID_COLUMNS)?.trim_end().parse().ok()?;
    let columns = line.get(ID_COLUMNS..)?.strip_prefix(' ')?;
    let width = columns.find(' ')?;
    let pane = pane_named(&columns[..width])?;
    let padded = width.max(PANE_COLUMNS) + 1;
    columns.get(width..padded)?.trim_matches(' ').is_empty().then_some(())?;
    Some((Client { id: id.into(), pane }, ID_COLUMNS + 1 + padded))
}

fn pane_named(name: &str) -> Option<PaneId> {
    let number = |prefix| name.strip_prefix(prefix).and_then(|n| n.parse().ok());
    number("terminal_").map(PaneId::Terminal).or_else(|| number("plugin_").map(PaneId::Plugin))
}

/// What zellij said as it listed a session's clients, where it reads as such
/// a list: its rows, as many as zellij wrote. A row may be one that a command
/// of many lines quotes, which only the session's panes tell.
pub(super) struct Said {
    said: String,
    rows: Listed,
}

/// What zellij `said` as it listed clients, or why it is no such list.
pub(super) fn said(said: &str) -> Result<Said, String> {
    let rows = parse(said, &[]).map_err(|why| unreadable(&why))?;
    Ok(Said { said: said.to_owned(), rows })
}

impl Said {
    /// Whether it lists a client: its first row is one, as a command can
    /// quote only rows after its own.
    pub(super) fn any(&self) -> bool {
        !self.rows.is_empty()
    }

    /// Every row it lists: each client, and each row a command quotes.
    pub(super) fn rows(&self) -> Vec<Client> {
        self.rows.iter().chain(self.rows.set_aside()).copied().collect()
    }

    /// The clients it lists, a row that a command of many lines quotes
    /// skipped by that command, as zellij lists the panes of `session` now;
    /// or why it did not list them. Of one row, none is quoted.
    pub(super) fn clients(self, session: &SessionName) -> Result<Listed, Failure> {
        if self.rows.len() + self.rows.set_aside().len() < 2 {
            return Ok(self.rows);
        }
        let many_lines = |pane: Laid| {
            let id = pane.pane_id();
            Some((id, pane.pane_command.filter(|command| command.contains('\n'))?))
        };
        let commands: Vec<_> = laid(session)?.into_iter().filter_map(many_lines).collect();
        parse(&self.said, &commands).map_err(|why| Failure::Unread(unreadable(&why)))
    }
}

fn unreadable(why: &str) -> String {
    format!("zellij listed what does not read: {why}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(said: &str) -> Result<Vec<Client>, String> {
        parse(said, &[]).map(|listed| listed.to_vec())
    }

    /// As zellij 0.45.1 pads its columns, a plugin's pane with its URL for
    /// what runs, and a command with spaces in it.
    #[test]
    fn each_client_is_read_by_its_id_and_its_terminal() {
        let said = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\
                    1         terminal_3     tail -f /dev/null\n\
                    2         plugin_4       file:/x/luneta.wasm\n\
                    3         terminal_0     N/A            \n";
        let client = |id, pane| Client { id, pane };
        let (on, plugin) = (PaneId::Terminal, PaneId::Plugin);
        assert_eq!(read(said), Ok(vec![client(1, on(3)), client(2, plugin(4)), client(3, on(0))]));
        assert_eq!(read("CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n"), Ok(Vec::new()));
    }

    #[test]
    fn a_command_with_lines_of_its_own_leaves_the_clients_as_zellij_listed_them() {
        let (client, on) = (|id, pane| Client { id, pane }, PaneId::Terminal);
        let header = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n";
        let one = format!(
            "{header}1         terminal_0     claude --name x Fix this.\n\
             \n\
             Where things stand: it merged.\n\
             7 lines down\n"
        );
        assert_eq!(read(&one), Ok(vec![client(1, on(0))]));
        let two = format!("{one}2         plugin_4       N/A            \n");
        assert_eq!(read(&two), Ok(vec![client(1, on(0)), client(2, PaneId::Plugin(4))]));
    }

    #[test]
    fn a_command_that_quotes_a_client_no_higher_than_its_own_adds_none() {
        let header = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n";
        let said = format!(
            "{header}5         terminal_0     claude Why does this read so?\n\
             1         terminal_3     fish\n\
             5         terminal_4     fish\n"
        );
        let listed = parse(&said, &[]).unwrap();
        assert_eq!(listed.to_vec(), [Client { id: 5, pane: PaneId::Terminal(0) }]);
        let (client, on) = (|id, pane| Client { id, pane }, PaneId::Terminal);
        assert_eq!(listed.set_aside(), [client(1, on(3)), client(5, on(4))]);
    }

    #[test]
    fn a_row_a_command_quotes_is_skipped_by_the_text_zellij_says_runs_there() {
        let (client, on) = (|id, pane| Client { id, pane }, PaneId::Terminal);
        for quoted in ["2         terminal_4     N/A", "7         terminal_4     N/A"] {
            let command = format!("claude Fix this.\n\nzellij lists:\n{quoted}\nThanks");
            let said = format!(
                "{HEADER}\n1         terminal_0     {command}\n\
                 2         terminal_1     fish\n\
                 3         terminal_0     {command}\n"
            );
            let listed = parse(&said, &[(on(0), command)]).unwrap();
            assert_eq!(listed.to_vec(), [client(1, on(0)), client(2, on(1)), client(3, on(0))]);
            assert_eq!(listed.set_aside(), []);
        }
    }

    #[test]
    fn a_row_quoted_where_zellij_says_otherwise_of_what_runs_goes_by_the_order_of_the_ids() {
        let (client, on) = (|id, pane| Client { id, pane }, PaneId::Terminal);
        let said = format!(
            "{HEADER}\n1         terminal_0     claude Fix this.\n\
             7         terminal_4     N/A            \n\
             2         terminal_1     fish\n"
        );
        let listed = parse(&said, &[(on(0), "claude Other.\nThanks".to_owned())]).unwrap();
        assert_eq!(listed.to_vec(), [client(1, on(0)), client(7, on(4))]);
        assert_eq!(listed.set_aside(), [client(2, on(1))]);
    }

    #[test]
    fn only_a_row_as_zellij_writes_it_reads_as_a_client() {
        let first = "1         terminal_0     claude x       ";
        let client = |id| Client { id, pane: PaneId::Terminal(0) };
        for line in [
            "  2         terminal_4     N/A            ",
            "2 terminal_4 fish",
            "2 terminal_4",
            "2\tterminal_4\tfish",
            "70000     terminal_4     fish",
            "2         terminal_4 fish with room",
            "2         terminal_4",
        ] {
            let said = format!("{HEADER}\n{first}\n{line}\n");
            assert_eq!(read(&said), Ok(vec![client(1)]), "{line:?}");
        }
        let wide = format!("{HEADER}\n1         terminal_1234567 fish           \n");
        assert_eq!(read(&wide), Ok(vec![Client { id: 1, pane: PaneId::Terminal(1_234_567) }]));
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
        assert!(read("").is_err());
        assert!(read("1 terminal_3 fish\n").is_err());
        assert!(read(&format!("{header}one terminal_3 fish\n")).is_err());
        assert!(read(&format!("{header}1 window_3 fish\n")).is_err());
        assert!(read(&format!("{header}1\n")).is_err());
        assert!(read(&format!("{header}1 terminal_3 fish\n")).is_err());
    }
}

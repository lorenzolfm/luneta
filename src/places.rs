use std::collections::{BTreeMap, HashMap, HashSet};

use luneta_protocol::said::Said;
use luneta_protocol::v1;

struct Pane {
    id: u32,
    cwd: Said,
    /// Whether claude runs in it, as far as zellij can tell: what runs in front
    /// says so, or an agent titled it last. zellij keeps the last title a
    /// program gave, so a pane where claude ended still counts, under a shell
    /// that sets no title of its own.
    claude: bool,
    /// What its terminal runs in the foreground, when zellij could say: the
    /// command line of the process group in front, or of the shell itself at
    /// its prompt.
    command: Option<String>,
}

/// One session that holds a pane of a given id. `find` asks about a pane many
/// times per frame — once per agent — so the answers are indexed by pane id
/// instead of being searched for across every session's pane list.
struct Holder {
    session: String,
    cwd: Said,
    claude: bool,
}

/// Where the panes of every session work, and what runs in each, as the
/// daemon last said.
#[derive(Default)]
pub struct Places {
    sessions: BTreeMap<String, Vec<Pane>>,
    holders: HashMap<u32, Vec<Holder>>,
}

impl Places {
    /// What the daemon says of every session's panes, in the place of all it
    /// said before. A session whose panes it has not listed holds none.
    pub fn heard(&mut self, sessions: &[v1::Session]) {
        let panes = |session: &v1::Session| {
            let listed = session.panes.as_ref().map_or(&[][..], |panes| panes.panes.as_slice());
            let pane = |pane: &v1::Pane| {
                let command = pane.command.clone().filter(|command| !command.trim().is_empty());
                Some(Pane {
                    id: pane.id,
                    cwd: Said::new(pane.cwd.clone()?)?,
                    // The daemon says what an agent said in a title only where
                    // an agent's glyph led it, as the `✳` does.
                    claude: command.as_deref().is_some_and(|command| command.contains("claude"))
                        || pane.titled.is_some(),
                    command,
                })
            };
            listed.iter().filter_map(pane).collect()
        };
        self.sessions =
            sessions.iter().map(|session| (session.name.clone(), panes(session))).collect();
        self.reindex();
    }

    /// Rebuilds the pane index from the sessions. Keeps the first pane of each
    /// (id, cwd) within a session, and visits sessions in name order, so a tie
    /// between two sessions is broken the same way every time.
    fn reindex(&mut self) {
        let mut holders: HashMap<u32, Vec<Holder>> = HashMap::new();
        for (name, panes) in &self.sessions {
            let mut seen: HashSet<(u32, &str)> = HashSet::new();
            for pane in panes {
                if !seen.insert((pane.id, pane.cwd.as_str())) {
                    continue;
                }
                holders.entry(pane.id).or_default().push(Holder {
                    session: name.clone(),
                    cwd: pane.cwd.clone(),
                    claude: pane.claude,
                });
            }
        }
        self.holders = holders;
    }

    /// The directory a pane works in, as its session last said.
    pub fn cwd(&self, session: &str, pane: u32) -> Option<&str> {
        let panes = self.sessions.get(session)?;
        panes.iter().find(|held| held.id == pane).map(|held| held.cwd.as_str())
    }

    /// What a pane's terminal runs in the foreground, as its session last
    /// said, as zellij spells a command: its path, then each argument after a
    /// space.
    pub fn command(&self, session: &str, pane: u32) -> Option<&str> {
        let panes = self.sessions.get(session)?;
        panes.iter().find(|held| held.id == pane)?.command.as_deref()
    }

    pub fn find(&self, pane: u32, cwd: &str, reported: &str) -> Option<&str> {
        let holders = self.holders.get(&pane)?;
        let held = || holders.iter().filter(|held| held.cwd.as_str() == cwd);
        let mut only = held();
        match (only.next(), only.next()) {
            (Some(only), None) => return Some(only.session.as_str()),
            (None, _) => return None,
            _ => {},
        }
        if let Some(named) = held().find(|held| held.session == reported) {
            return Some(named.session.as_str());
        }
        let mut claudes = held().filter(|held| held.claude);
        match (claudes.next(), claudes.next()) {
            (Some(one), None) => Some(one.session.as_str()),
            _ => None,
        }
    }
}

#[cfg(test)]
impl Places {
    pub fn of(sessions: &[(&str, &[(u32, &str)])]) -> Self {
        let mut places = Places {
            sessions: sessions
                .iter()
                .map(|(name, panes)| {
                    let panes = panes
                        .iter()
                        .map(|(id, cwd)| Pane {
                            id: *id,
                            cwd: Said::new((*cwd).to_owned()).expect("a pane works somewhere"),
                            claude: true,
                            command: None,
                        })
                        .collect();
                    (name.to_string(), panes)
                })
                .collect(),
            ..Places::default()
        };
        places.reindex();
        places
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal as the daemon lists it: pane `id`, in `cwd`, running
    /// `command` in front, with what an agent said in its title, if one did.
    fn pane(id: u32, cwd: Option<&str>, command: Option<&str>, titled: bool) -> v1::Pane {
        v1::Pane {
            id,
            tab: 0,
            tab_name: "Tab #1".to_owned(),
            cwd: cwd.map(str::to_owned),
            command: command.map(str::to_owned),
            titled: titled.then_some(v1::Titled { topic: None }),
            shell: None,
        }
    }

    /// Session `name`, with `panes` once the daemon has listed them.
    fn session(name: &str, panes: Option<Vec<v1::Pane>>) -> v1::Session {
        v1::Session {
            name: name.to_owned(),
            started_at: None,
            socket: 0,
            panes: panes.map(|panes| v1::Panes { panes }),
            clients: None,
        }
    }

    fn ghostty() -> Places {
        let mut places = Places::default();
        places.heard(&[session(
            "ghostty",
            Some(vec![
                pane(0, Some("/home/lorenzo/Projects/misc"), Some("claude"), true),
                pane(1, Some("/home/lorenzo/Projects/misc/ghostty"), Some("fish"), false),
            ]),
        )]);
        places
    }

    /// What the daemon says takes the place of all it said: where each pane
    /// works, and what runs in it. A session whose panes it has yet to list
    /// holds none, and a session it no longer lists, none either.
    #[test]
    fn what_the_daemon_says_stands_in_the_place_of_what_it_said() {
        let mut places = ghostty();
        let listed = vec![
            pane(0, Some("/w/repo"), Some("claude"), false),
            pane(1, Some("/w/repo/src"), None, false),
            pane(2, None, Some("fish"), false),
        ];
        places.heard(&[session("luneta", Some(listed)), session("new", None)]);
        assert_eq!(places.find(0, "/w/repo", ""), Some("luneta"));
        assert_eq!(places.command("luneta", 0), Some("claude"));
        assert_eq!(places.cwd("luneta", 1), Some("/w/repo/src"));
        assert_eq!(places.command("luneta", 1), None);
        assert_eq!(places.cwd("luneta", 2), None);
        assert_eq!(places.find(0, "/home/lorenzo/Projects/misc", ""), None);
    }

    #[test]
    fn a_pane_is_found_by_the_id_and_the_cwd_together() {
        let places = ghostty();
        assert_eq!(places.find(0, "/home/lorenzo/Projects/misc", ""), Some("ghostty"));
        assert_eq!(places.find(1, "/home/lorenzo/Projects/misc/ghostty", ""), Some("ghostty"));
        assert_eq!(places.find(1, "/home/lorenzo/Projects/misc", ""), None);
        assert_eq!(places.find(9, "/home/lorenzo/Projects/misc", ""), None);
    }

    /// A pane the daemon says no directory for has none, rather than one
    /// spelled `""` for an agent with none to be found at.
    #[test]
    fn a_pane_with_no_cwd_has_none() {
        let mut places = Places::default();
        places.heard(&[session("s", Some(vec![pane(0, Some(""), Some("claude"), false)]))]);
        assert_eq!(places.cwd("s", 0), None);
        assert_eq!(places.find(0, "", ""), None);
    }

    #[test]
    fn two_sessions_that_could_both_answer_answer_neither() {
        let places = Places::of(&[("one", &[(0, "/w/repo")]), ("two", &[(0, "/w/repo")])]);
        assert_eq!(places.find(0, "/w/repo", ""), None);
    }

    /// Two sessions with a pane 0 in `/w/repo`: a shell in `shell`, and in
    /// `agent` what `command` runs in front, titled by an agent if `titled`.
    fn shell_and(command: &str, titled: bool) -> Places {
        let mut places = Places::default();
        places.heard(&[
            session("shell", Some(vec![pane(0, Some("/w/repo"), Some("fish"), false)])),
            session("agent", Some(vec![pane(0, Some("/w/repo"), Some(command), titled)])),
        ]);
        places
    }

    #[test]
    fn the_pane_that_runs_claude_wins_a_tie() {
        assert_eq!(shell_and("claude", false).find(0, "/w/repo", ""), Some("agent"));
    }

    /// The daemon says what an agent said in a pane's title only where an
    /// agent's glyph led it, a bare `Claude Code` too.
    #[test]
    fn a_claude_started_inside_a_shell_is_known_by_its_title() {
        assert_eq!(shell_and("fish", true).find(0, "/w/repo", ""), Some("agent"));
        assert_eq!(shell_and("fish", false).find(0, "/w/repo", ""), None);
    }

    #[test]
    fn the_reported_name_breaks_a_tie_the_panes_alone_cannot() {
        let places = Places::of(&[("one", &[(0, "/w/repo")]), ("two", &[(0, "/w/repo")])]);
        assert_eq!(places.find(0, "/w/repo", "two"), Some("two"));
        assert_eq!(places.find(0, "/w/repo", "gone"), None);
    }

    #[test]
    fn a_name_that_still_holds_the_pane_outranks_the_claude_guess() {
        assert_eq!(shell_and("claude", true).find(0, "/w/repo", "shell"), Some("shell"));
    }

    #[test]
    fn a_lone_holder_is_taken_whatever_the_reported_name_says() {
        let places = ghostty();
        assert_eq!(places.find(0, "/home/lorenzo/Projects/misc", "misc"), Some("ghostty"));
    }
}

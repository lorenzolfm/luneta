use std::collections::BTreeMap;

use serde::Deserialize;

pub const CONTEXT_VALUE: &str = "places";
pub const SESSION_KEY: &str = "luneta_session";

pub fn query(session: &str) -> [&str; 7] {
    ["zellij", "--session", session, "action", "list-panes", "--all", "--json"]
}

#[derive(Deserialize)]
struct Wire {
    id: u32,
    #[serde(default)]
    is_plugin: bool,
    #[serde(default)]
    is_suppressed: bool,
    #[serde(default)]
    pane_cwd: Option<String>,
    #[serde(default)]
    pane_command: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

struct Pane {
    id: u32,
    cwd: String,
    claude: bool,
}

enum Reply {
    Panes(Vec<Pane>),
    Failed(String),
}

const MARK: char = '✳';

#[derive(Default)]
pub struct Places {
    sessions: BTreeMap<String, Reply>,
    asked: Option<Vec<String>>,
}

impl Places {
    pub fn ask(&mut self, live: &[String]) -> Vec<String> {
        if self.asked.as_deref() == Some(live) {
            return Vec::new();
        }
        self.sessions.retain(|name, _| live.contains(name));
        self.asked = Some(live.to_vec());
        live.to_vec()
    }

    pub fn ingest(
        &mut self,
        session: String,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
    ) {
        if !self.asked.as_ref().is_some_and(|asked| asked.contains(&session)) {
            return;
        }
        let reply = match exit_code {
            Some(0) => Reply::Panes(parse(&String::from_utf8_lossy(stdout))),
            _ => Reply::Failed(reason(&session, stderr)),
        };
        self.sessions.insert(session, reply);
    }

    pub fn trouble(&self) -> Option<String> {
        let mut failed = self.sessions.values().filter_map(|reply| match reply {
            Reply::Failed(reason) => Some(reason.as_str()),
            Reply::Panes(_) => None,
        });
        let first = failed.next()?;
        match failed.count() {
            0 => Some(first.to_string()),
            rest => Some(format!("{} sessions could not be read: {}", rest + 1, first)),
        }
    }

    pub fn forget(&mut self) {
        self.asked = None;
    }

    pub fn find(&self, pane: u32, cwd: &str, reported: &str) -> Option<&str> {
        if cwd.is_empty() {
            return None;
        }
        let held: Vec<(&str, bool)> = self
            .sessions
            .iter()
            .filter_map(|(name, reply)| match reply {
                Reply::Panes(panes) => Some((name, panes)),
                Reply::Failed(_) => None,
            })
            .filter_map(|(name, panes)| {
                panes
                    .iter()
                    .find(|held| held.id == pane && held.cwd == cwd)
                    .map(|held| (name.as_str(), held.claude))
            })
            .collect();
        if let [(only, _)] = held.as_slice() {
            return Some(only);
        }
        if let Some((named, _)) = held.iter().find(|(name, _)| *name == reported) {
            return Some(named);
        }
        let mut claudes = held.iter().filter(|(_, claude)| *claude);
        match (claudes.next(), claudes.next()) {
            (Some((name, _)), None) => Some(name),
            _ => None,
        }
    }
}

fn reason(session: &str, stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().next().unwrap_or("").trim();
    match line.is_empty() {
        true => format!("the panes of {} could not be read", session),
        false => line.to_string(),
    }
}

fn parse(stdout: &str) -> Vec<Pane> {
    let wire: Vec<Wire> = serde_json::from_str(stdout).unwrap_or_default();
    wire.into_iter()
        .filter(|pane| !pane.is_plugin && !pane.is_suppressed)
        .filter_map(|pane| {
            Some(Pane {
                id: pane.id,
                cwd: pane.pane_cwd?,
                claude: is_claude(pane.pane_command.as_deref(), pane.title.as_deref()),
            })
        })
        .collect()
}

fn is_claude(command: Option<&str>, title: Option<&str>) -> bool {
    command.is_some_and(|command| command.contains("claude"))
        || title.is_some_and(|title| title.trim_start().starts_with(MARK))
}

#[cfg(test)]
impl Places {
    pub fn of(sessions: &[(&str, &[(u32, &str)])]) -> Self {
        Places {
            asked: Some(sessions.iter().map(|(name, _)| name.to_string()).collect()),
            sessions: sessions
                .iter()
                .map(|(name, panes)| {
                    let panes = panes
                        .iter()
                        .map(|(id, cwd)| Pane { id: *id, cwd: cwd.to_string(), claude: true })
                        .collect();
                    (name.to_string(), Reply::Panes(panes))
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(sessions: &[(&str, &[u8])]) -> Places {
        let mut places = Places::default();
        places.ask(&sessions.iter().map(|(name, _)| name.to_string()).collect::<Vec<_>>());
        for (name, stdout) in sessions {
            places.ingest(name.to_string(), Some(0), stdout, b"");
        }
        places
    }

    const GHOSTTY: &str = r#"[
        {"id":0,"is_plugin":true,"is_selectable":false,"is_suppressed":false,
         "title":"zellij:tab-bar","pane_cwd":null,"pane_command":null},
        {"id":0,"is_plugin":false,"is_selectable":true,"is_suppressed":false,
         "title":"✳ Ghostty OSC 94 progress bar","pane_cwd":"/home/lorenzo/Projects/misc",
         "pane_command":"claude"},
        {"id":1,"is_plugin":false,"is_selectable":true,"is_suppressed":false,
         "title":"~/P/m/ghostty","pane_cwd":"/home/lorenzo/Projects/misc/ghostty",
         "pane_command":"/run/current-system/sw/bin/fish"}]"#;

    fn ghostty() -> Places {
        asked(&[("ghostty", GHOSTTY.as_bytes())])
    }

    #[test]
    fn a_pane_is_found_by_the_id_and_the_cwd_together() {
        let places = ghostty();
        assert_eq!(places.find(0, "/home/lorenzo/Projects/misc", ""), Some("ghostty"));
        assert_eq!(places.find(1, "/home/lorenzo/Projects/misc/ghostty", ""), Some("ghostty"));
        assert_eq!(places.find(1, "/home/lorenzo/Projects/misc", ""), None);
        assert_eq!(places.find(9, "/home/lorenzo/Projects/misc", ""), None);
    }

    #[test]
    fn a_plugin_pane_never_answers_for_a_terminal_of_the_same_id() {
        let places = ghostty();
        assert_eq!(places.find(0, "", ""), None);
        match &places.sessions["ghostty"] {
            Reply::Panes(panes) => assert_eq!(panes.len(), 2),
            Reply::Failed(reason) => panic!("{}", reason),
        }
    }

    #[test]
    fn an_agent_with_no_cwd_is_not_guessed_at() {
        let places =
            asked(&[("s", br#"[{"id":0,"is_plugin":false,"pane_cwd":"","pane_command":"claude"}]"#)]);
        assert_eq!(places.find(0, "", ""), None);
    }

    #[test]
    fn two_sessions_that_could_both_answer_answer_neither() {
        let places = Places::of(&[("one", &[(0, "/w/repo")]), ("two", &[(0, "/w/repo")])]);
        assert_eq!(places.find(0, "/w/repo", ""), None);
    }

    #[test]
    fn the_pane_that_runs_claude_wins_a_tie() {
        let places = asked(&[
            ("shell", br#"[{"id":0,"is_plugin":false,"pane_cwd":"/w/repo","pane_command":"fish",
                 "title":"~/w/repo"}]"#),
            (
                "agent",
                r#"[{"id":0,"is_plugin":false,"pane_cwd":"/w/repo","pane_command":"claude",
                 "title":"✳ Claude Code"}]"#
                    .as_bytes(),
            ),
        ]);
        assert_eq!(places.find(0, "/w/repo", ""), Some("agent"));
    }

    #[test]
    fn a_claude_started_inside_a_shell_is_known_by_its_title() {
        let places = asked(&[
            ("shell", br#"[{"id":0,"is_plugin":false,"pane_cwd":"/w/repo","pane_command":"fish",
                 "title":"~/w/repo"}]"#),
            (
                "agent",
                r#"[{"id":0,"is_plugin":false,"pane_cwd":"/w/repo","pane_command":"fish",
                 "title":"✳ Claude Code"}]"#
                    .as_bytes(),
            ),
        ]);
        assert_eq!(places.find(0, "/w/repo", ""), Some("agent"));
    }

    #[test]
    fn the_reported_name_breaks_a_tie_the_panes_alone_cannot() {
        let places = Places::of(&[("one", &[(0, "/w/repo")]), ("two", &[(0, "/w/repo")])]);
        assert_eq!(places.find(0, "/w/repo", "two"), Some("two"));
        assert_eq!(places.find(0, "/w/repo", "gone"), None);
    }

    #[test]
    fn a_name_that_still_holds_the_pane_outranks_the_claude_guess() {
        let places = asked(&[
            ("shell", br#"[{"id":0,"is_plugin":false,"pane_cwd":"/w/repo","pane_command":"fish",
                 "title":"~/w/repo"}]"#),
            (
                "agent",
                r#"[{"id":0,"is_plugin":false,"pane_cwd":"/w/repo","pane_command":"claude",
                 "title":"✳ Claude Code"}]"#
                    .as_bytes(),
            ),
        ]);
        assert_eq!(places.find(0, "/w/repo", "shell"), Some("shell"));
    }

    #[test]
    fn a_lone_holder_is_taken_whatever_the_reported_name_says() {
        let places = ghostty();
        assert_eq!(places.find(0, "/home/lorenzo/Projects/misc", "misc"), Some("ghostty"));
    }

    #[test]
    fn a_session_that_could_not_be_read_holds_nothing_and_says_why() {
        let mut places = ghostty();
        places.ingest("ghostty".to_string(), Some(1), b"", b"zellij: no session ghostty\n");
        assert_eq!(places.find(0, "/home/lorenzo/Projects/misc", ""), None);
        assert_eq!(places.trouble().as_deref(), Some("zellij: no session ghostty"));
    }

    #[test]
    fn a_session_that_answered_is_not_a_session_that_could_not_be_read() {
        assert_eq!(ghostty().trouble(), None);
        assert_eq!(Places::default().trouble(), None);
    }

    #[test]
    fn a_failure_that_said_nothing_still_names_the_session_it_lost() {
        let mut places = Places::default();
        places.ask(&["misc".to_string()]);
        places.ingest("misc".to_string(), None, b"", b"");
        assert_eq!(places.trouble().as_deref(), Some("the panes of misc could not be read"));
    }

    #[test]
    fn a_failure_that_took_every_session_is_counted_once() {
        let mut places = Places::default();
        places.ask(&["a".to_string(), "b".to_string(), "c".to_string()]);
        for session in ["a", "b", "c"] {
            places.ingest(session.to_string(), Some(127), b"", b"zellij: command not found\n");
        }
        assert_eq!(
            places.trouble().as_deref(),
            Some("3 sessions could not be read: zellij: command not found")
        );
    }

    #[test]
    fn a_reply_that_outlived_the_session_it_names_is_dropped() {
        let mut places = Places::default();
        assert_eq!(places.ask(&["ghostty".to_string()]), ["ghostty"]);
        assert_eq!(places.ask(&["luneta".to_string()]), ["luneta"]);
        places.ingest("ghostty".to_string(), Some(0), GHOSTTY.as_bytes(), b"");
        assert_eq!(places.find(0, "/home/lorenzo/Projects/misc", "ghostty"), None);
        assert_eq!(places.trouble(), None);
    }

    #[test]
    fn the_same_set_of_sessions_is_only_asked_about_once() {
        let live = vec!["a".to_string(), "b".to_string()];
        let mut places = Places::default();
        assert_eq!(places.ask(&live), live);
        assert!(places.ask(&live).is_empty());
        places.forget();
        assert_eq!(places.ask(&live), live);
    }

    #[test]
    fn a_session_that_is_gone_is_forgotten_when_the_rest_are_asked_again() {
        let mut places = ghostty();
        assert_eq!(places.find(0, "/home/lorenzo/Projects/misc", ""), Some("ghostty"));
        assert_eq!(places.ask(&["luneta".to_string()]), ["luneta"]);
        assert_eq!(places.find(0, "/home/lorenzo/Projects/misc", ""), None);
    }

    #[test]
    fn the_query_names_the_session_it_asks_about() {
        assert_eq!(
            query("misc-luneta"),
            ["zellij", "--session", "misc-luneta", "action", "list-panes", "--all", "--json"]
        );
    }
}

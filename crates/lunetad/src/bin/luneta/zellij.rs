//! What luneta asks of zellij: a client to run beside the sidebar, to take
//! that client to a pane, and the clients of a session. All luneta knows of
//! zellij's ways is here and in `own`, which finds luneta's client among
//! those, so that a multiplexer of luneta's own can take zellij's place
//! behind it.

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::Client;
use lunetad::clients;

use crate::host::Event;
use crate::own::{Look, Looked};

/// The longest name zellij makes a session of, in bytes.
const NAME_BYTES_MAX: usize = 60;

// `SessionName::parse` refuses a longer name in zellij's words, which count the
// bytes a name must be shorter than.
const _: () = assert!(NAME_BYTES_MAX + 1 == 61);

/// What names the session of a zellij pane, in the variables of what runs in
/// it.
const SESSION_VAR: &str = "ZELLIJ_SESSION_NAME";

/// Variables that zellij's client must not have. With `ZELLIJ` set, it takes
/// itself for one started in a pane of that session, and will not attach, as
/// when luneta itself runs in a pane: `refusal` makes the one check it skips
/// then. The window ids are of the terminal luneta's own is made by, and name
/// no window.
const LEFT_OUT: [&str; 5] =
    ["ZELLIJ", SESSION_VAR, "ZELLIJ_PANE_ID", "WINDOWID", "ALACRITTY_WINDOW_ID"];

/// A name zellij can make a session of. `parse` is the one way to make one, so
/// a name that reaches zellij as one has been held to what zellij takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionName(String);

impl SessionName {
    pub fn parse(name: &str) -> Result<Self, &'static str> {
        if name.is_empty() {
            return Err("name must not be empty");
        }
        if name.trim().is_empty() {
            return Err("name cannot be only whitespace");
        }
        if name == "." || name == ".." {
            return Err("name cannot be '.' or '..'");
        }
        if name.contains('/') {
            return Err("name cannot contain '/'");
        }
        if name.len() > NAME_BYTES_MAX {
            return Err("name must be shorter than 61 bytes");
        }
        Ok(SessionName(name.to_owned()))
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why zellij's client must not attach to `session` from the terminal luneta
/// runs in, where it must not: the terminal is a pane of that session, which
/// the client would show inside itself, with the focus on it, so that a key
/// typed there would come round to it again for good. zellij refuses this by
/// `SESSION_VAR`, which the client runs without. A pane of a session renamed
/// since it opened holds the name it had then, and zellij's check goes by it.
pub fn refusal(session: &SessionName) -> Option<String> {
    refused(session, env::var(SESSION_VAR).ok().as_deref())
}

/// `refusal`, for a terminal in the session `current`, if it is a pane's.
fn refused(session: &SessionName, current: Option<&str>) -> Option<String> {
    (current == Some(session.0.as_str())).then(|| {
        format!(
            "this terminal is a pane of \"{session}\", which zellij would show inside itself: \
             name another session, or run luneta outside zellij"
        )
    })
}

/// The program luneta runs beside the sidebar, and its arguments: zellij's
/// client, attached to `session` and making it if none runs, or without one
/// in a session of a name zellij makes up. It runs through `env`, to leave out
/// what `LEFT_OUT` names, which luneta cannot take out of its own variables.
pub fn client(session: Option<&SessionName>) -> (String, Vec<String>) {
    let mut args: Vec<String> =
        LEFT_OUT.iter().flat_map(|name| ["-u".to_owned(), (*name).to_owned()]).collect();
    args.push("zellij".to_owned());
    if let Some(session) = session {
        args.extend(["attach".to_owned(), "--create".to_owned(), session.0.clone()]);
    }
    ("env".to_owned(), args)
}

/// The session a directory is for, as the picker's directories name it: for
/// the directory itself, or for its repository too when it is a worktree of
/// one, as `luneta-main`. None for a directory no session can be named for,
/// as `/`.
pub fn session_for(dir: &Path) -> Option<SessionName> {
    let path = dir.to_str()?;
    common_dir(dir)
        .and_then(|common| worktree_name(path, common.to_str()?))
        .or_else(|| SessionName::parse(path.split('/').rfind(|part| !part.is_empty())?).ok())
}

/// The repository a linked worktree is of: its git directory, which every
/// worktree of it shares. A worktree's `.git` is a file that names its own git
/// directory, and that names the shared one in `commondir`. None for a
/// directory that is no linked worktree, as a repository's main one is not.
fn common_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = fs::read_to_string(dir.join(".git")).ok()?;
    let own = dir.join(dot_git.lines().next()?.strip_prefix("gitdir: ")?);
    let common = fs::read_to_string(own.join("commondir")).ok()?;
    fs::canonicalize(own.join(common.lines().next()?)).ok()
}

/// A worktree is named for its repository too, `luneta-main`, where `main`
/// alone would name one of every repository, wherever the worktree lives. The
/// repository is its git directory, less a bare repository's `.git`, or, when
/// that directory is hidden, as a `.git` or a `.bare`, the directory that
/// holds it. A worktree whose own name starts with its repository's, as
/// `app-hotfix` beside `app`, keeps it, and so does one whose name would come
/// out too long for zellij to make a session of.
fn worktree_name(path: &str, common: &str) -> Option<SessionName> {
    let base = path.split('/').rfind(|part| !part.is_empty())?;
    let mut parts = common.split('/').filter(|part| !part.is_empty());
    let mut repository = parts.next_back()?;
    if repository.starts_with('.') {
        repository = parts.next_back()?;
    }
    let repository =
        repository.strip_suffix(".git").filter(|bare| !bare.is_empty()).unwrap_or(repository);
    if base.strip_prefix(repository).is_some_and(|rest| !rest.starts_with(char::is_alphanumeric)) {
        return None;
    }
    SessionName::parse(&format!("{repository}-{base}")).ok()
}

/// What luneta types into zellij's client before it asks zellij to move it:
/// `ScrollLock` let go, as the kitty keyboard protocol spells it, with no
/// modifiers and the event of a release, `1:3`. zellij moves the client of a
/// session that last sent it a key, and a client that has sent none since it
/// came to the session, as luneta's has not once the sidebar took it there,
/// moves for no action. The client counts a key let go as a key. No one binds
/// `ScrollLock`, and zellij writes nothing of it to a pane that does not read
/// keys as kitty spells them. One that does gets it as it came, a release it
/// never asked to hear of, and passes it over: nvim does, where the key
/// pressed would have it insert a character of the protocol's own in the
/// text. The client reads it as kitty spells it unless its config turns the
/// protocol off, and then zellij moves the client only once you type in it.
pub const NUDGE: &[u8] = b"\x1b[57359;1:3u";

/// How long an action waits for zellij to take in `NUDGE`, which goes to it
/// through its client while the action goes straight to its server.
const NUDGED: Duration = Duration::from_millis(30);

/// How long luneta waits for its client to come to a session it sent it to
/// from another, after zellij took the action, before it takes the client
/// for one that stayed. It comes within milliseconds.
const ARRIVAL: Duration = Duration::from_secs(2);

/// How long luneta waits for its client to come to the session it attaches
/// to as it starts, which zellij can take a while to make.
const START: Duration = Duration::from_secs(10);

/// How often luneta asks zellij for the clients of a session its client is
/// on its way to.
const ARRIVING: Duration = Duration::from_millis(20);

/// Takes the client from the session `at` to the pane `pane` of `to`, or to
/// the session `to` alone, on the pane it last had the focus on, and then
/// takes the look `then`, if any: of `at`, for the pane, or of `to`, for the
/// client's arrival there. The client must have been sent `NUDGE` first. What
/// zellij says of a failure comes back as a notice.
pub fn go(at: &str, to: &str, pane: Option<u32>, then: Option<Look>, events: &Sender<Event>) {
    let mut command = Command::new("zellij");
    let pane = pane.map(|id| format!("terminal_{id}"));
    match pane {
        Some(pane) if at == to => {
            command.args(["--session", at, "action", "focus-pane-id", &pane]);
        },
        None if at == to => return,
        pane => {
            command.args(["--session", at, "action", "switch-session", to]);
            command.args(pane.iter().flat_map(|pane| ["--pane-id", pane.as_str()]));
        },
    }
    run(command, then.map(|look| if at == to { Then::Look(look) } else { Then::Arrive(look) }), events);
}

/// Asks zellij for the clients of the session `look` is of, on a thread of
/// its own, and hands back what it answers.
pub fn look(look: Look, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || looking(&look, &events));
}

/// Asks zellij for the clients of the session `look` is of, on a thread of
/// its own, until it lists one that `before` does not, which is luneta's own
/// client as it attaches, and hands back what it answers then.
pub fn arrive(look: Look, before: Vec<Client>, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || arriving(&look, before, START, &events));
}

/// The clients of `session`, as zellij lists them now, by their ids, or why
/// zellij did not list them, as of a session that does not run.
pub fn clients(session: &str) -> Result<Vec<Client>, String> {
    let mut command = Command::new("zellij");
    command.args(["--session", session, "action", "list-clients"]);
    quiet(&mut command);
    match command.output() {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .map_err(|_| "zellij listed clients in what is not UTF-8".to_owned())
            .and_then(|said| clients::parse(&said).map_err(|why| format!("zellij: {why}")))
            .map(|clients| clients.into_iter().map(clients::Client::to_api).collect()),
        Ok(output) => Err(failure(&output)),
        Err(error) => Err(unrun(&error)),
    }
}

/// What luneta asks of zellij once an action has run.
enum Then {
    /// The clients of the session the client is in, for the pane the action
    /// took it to.
    Look(Look),
    /// The client's arrival in the session of the look, from another.
    Arrive(Look),
}

/// Runs a zellij action on a thread of its own, so that the sidebar goes on
/// drawing while zellij answers, says why when it fails, and then does as
/// `then` says. A client on its way to another session comes there once
/// zellij lists one more client of it than before the action. A pane
/// already focused is where the action would take the client, and no failure.
fn run(mut command: Command, then: Option<Then>, events: &Sender<Event>) {
    quiet(&mut command);
    let events = events.clone();
    thread::spawn(move || {
        let before = match &then {
            Some(Then::Arrive(look)) => clients(&look.session).unwrap_or_default(),
            _ => Vec::new(),
        };
        thread::sleep(NUDGED);
        let failure = match command.output() {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(failure(&output)).filter(|said| !said.contains("is already focused")),
            Err(error) => Some(unrun(&error)),
        };
        if let Some(said) = &failure {
            // luneta is on its way out.
            let _ = events.send(Event::Notice(said.clone()));
        }
        match then {
            None => {},
            Some(Then::Look(look)) => {
                let went = if failure.is_none() { look.went } else { None };
                looking(&Look { went, ..look }, &events);
            },
            Some(Then::Arrive(look)) => {
                let came = failure.is_none() && arriving(&look, before, ARRIVAL, &events);
                if failure.is_none() && !came {
                    let said = "zellij did not take its client there".to_owned();
                    let _ = events.send(Event::Notice(said));
                }
                if !came {
                    let _ = events.send(Event::Stayed);
                }
            },
        }
    });
}

/// Asks zellij for the clients of the session `look` is of, and hands back
/// what it answers, or why it did not.
fn looking(look: &Look, events: &Sender<Event>) {
    // luneta is on its way out.
    let _ = events.send(match clients(&look.session) {
        Ok(clients) => {
            Event::Looked(Looked { number: look.number, went: look.went, clients, before: None })
        },
        Err(why) => Event::Notice(why),
    });
}

/// Asks zellij for the clients of the session `look` is of until it lists
/// one that `before` does not, for as long as `patience`, and hands back what
/// it answers then. Whether it did.
fn arriving(look: &Look, before: Vec<Client>, patience: Duration, events: &Sender<Event>) -> bool {
    let deadline = Instant::now() + patience;
    loop {
        let came = clients(&look.session).ok().filter(|clients| {
            clients.iter().any(|client| before.iter().all(|was| was.id != client.id))
        });
        if let Some(clients) = came {
            let (number, went) = (look.number, look.went);
            // luneta is on its way out.
            let _ = events.send(Event::Looked(Looked { number, went, clients, before: Some(before) }));
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(ARRIVING);
    }
}

/// Has `command` run without what `LEFT_OUT` names, and with nothing to read.
fn quiet(command: &mut Command) {
    for name in LEFT_OUT {
        command.env_remove(name);
    }
    command.stdin(Stdio::null());
}

/// What zellij said as it failed: the last line it wrote on stderr, and how
/// it ended.
fn failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr.lines().rfind(|line| !line.trim().is_empty()).unwrap_or("");
    format!("zellij: {} ({})", line.trim(), output.status)
}

/// Why zellij could not be run.
fn unrun(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        return "zellij is not on the PATH".to_owned();
    }
    format!("zellij: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_the_directory_itself() {
        let named = |dir: &str| session_for(Path::new(dir)).map(|name| name.0);
        assert_eq!(named("/nowhere/at/all/luneta"), Some("luneta".to_owned()));
        assert_eq!(named("/nowhere/at/all/luneta/"), Some("luneta".to_owned()));
        assert_eq!(named("/"), None);
    }

    #[test]
    fn a_worktree_is_named_for_its_repository_too() {
        let named = |path: &str, common: &str| worktree_name(path, common).map(|name| name.0);
        assert_eq!(named("/w/luneta/main", "/w/luneta/.bare"), Some("luneta-main".to_owned()));
        assert_eq!(named("/w/trees/main", "/w/luneta/.git"), Some("luneta-main".to_owned()));
        assert_eq!(named("/w/infra.git/master", "/w/infra.git"), Some("infra-master".to_owned()));
        assert_eq!(named("/w/app-hotfix", "/w/app/.git"), None);
        assert_eq!(named("/w/application", "/w/app/.git"), Some("app-application".to_owned()));
        let long = "x".repeat(NAME_BYTES_MAX);
        assert_eq!(named(&format!("/w/{long}"), "/w/repo/.git"), None);
    }

    /// A worktree made by git is found one, by the files git leaves in it.
    #[test]
    fn a_linked_worktree_is_found_by_its_git_files() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let root = scratch.path();
        let common = root.join("luneta").join(".bare");
        let own = common.join("worktrees").join("main");
        let tree = root.join("luneta").join("main");
        for dir in [&own, &tree] {
            assert!(fs::create_dir_all(dir).is_ok());
        }
        let wrote = [
            fs::write(tree.join(".git"), format!("gitdir: {}\n", own.display())),
            fs::write(own.join("commondir"), "../..\n"),
        ];
        assert!(wrote.iter().all(Result::is_ok));
        assert_eq!(session_for(&tree).map(|name| name.0), Some("luneta-main".to_owned()));
        let repository = root.join("luneta");
        assert_eq!(session_for(&repository).map(|name| name.0), Some("luneta".to_owned()));
    }

    #[test]
    fn the_client_leaves_zellij_s_variables_out() {
        let Ok(session) = SessionName::parse("luneta-main") else {
            panic!("a name zellij takes");
        };
        let (program, args) = client(Some(&session));
        assert_eq!(program, "env");
        assert!(args.windows(2).any(|pair| pair == ["-u", "ZELLIJ"]));
        assert_eq!(args[args.len() - 4..], ["zellij", "attach", "--create", "luneta-main"]);
    }

    /// The client leaves out the variable zellij refuses by to attach a pane's
    /// terminal to its own session, and luneta refuses in its place.
    #[test]
    fn a_pane_does_not_attach_to_its_own_session() {
        let Ok(session) = SessionName::parse("outer") else {
            panic!("a name zellij takes");
        };
        assert!(refused(&session, Some("outer")).is_some());
        assert_eq!(refused(&session, Some("other")), None);
        assert_eq!(refused(&session, None), None);
    }
}

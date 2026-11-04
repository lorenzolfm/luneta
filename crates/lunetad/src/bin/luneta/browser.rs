//! The browser of the machine luneta runs on, which the desktop picks: as
//! `xdg-open` asks it on Linux, and `open` on macOS. Each hands the page over,
//! and may stay until the browser it started ends, which outlives luneta.

use std::io::ErrorKind;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use crate::desktop;
use crate::host::Event;

/// What asks the desktop to open a page.
const OPENER: &str = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };

/// Opens `url` in your browser. Why it could not start goes back at once, and
/// why it could not go on to `events`. luneta over ssh opens nothing: the
/// desktop of the machine it runs on is not the one you sit at.
pub fn open(url: &str, events: &Sender<Event>) -> Result<(), String> {
    if desktop::elsewhere() {
        return Err("luneta runs over ssh, far from your browser: `y p` copies the link".to_owned());
    }
    open_by(Command::new(OPENER), url, events)
}

/// Has `opener` open `url`, as `open` says. It is no terminal's, and no group's
/// but its own, for what it starts to draw nothing on luneta's terminal and to
/// outlive its window. Only a web page goes: the desktop would run what a
/// daemon of another machine could name otherwise.
fn open_by(mut opener: Command, url: &str, events: &Sender<Event>) -> Result<(), String> {
    if !["https://", "http://"].into_iter().any(|scheme| url.starts_with(scheme)) {
        return Err(format!("{url} is no web page to open"));
    }
    let program = opener.get_program().to_string_lossy().into_owned();
    opener.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut opening = opener.process_group(0).spawn().map_err(|error| match error.kind() {
        ErrorKind::NotFound => format!("{program} is not on the PATH"),
        _ => format!("{program}: {error}"),
    })?;
    let events = events.clone();
    thread::spawn(move || {
        let why = match opening.wait() {
            Ok(status) if status.success() => return,
            Ok(status) => format!("{program} did not open the link: {status}"),
            Err(error) => format!("{program}: {error}"),
        };
        // luneta is on its way out.
        let _ = events.send(Event::Notice(why));
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    const URL: &str = "https://github.com/owner/repo/pull/249";

    /// What `opener` said of `url`: whether it started, and what it told
    /// luneta once it ended.
    fn said_by(opener: Command, url: &str) -> (Result<(), String>, Vec<String>) {
        let (events, heard) = channel();
        let started = open_by(opener, url, &events);
        drop(events);
        let notice = |event| match event {
            Event::Notice(notice) => notice,
            _ => panic!("an event other than a notice"),
        };
        (started, heard.into_iter().map(notice).collect())
    }

    /// A shell, as what opens the page: it takes `script`, and the page as its
    /// first argument.
    fn shell(script: &str) -> Command {
        let mut shell = Command::new("sh");
        shell.args(["-c", script, "opener"]);
        shell
    }

    /// The page goes to what opens it as its last argument, and what ends well
    /// says nothing.
    #[test]
    fn what_opens_a_page_is_handed_it_and_says_nothing_of_it() {
        let script = format!("test \"$1\" = {URL}");
        assert_eq!(said_by(shell(&script), URL), (Ok(()), Vec::new()));
    }

    /// One that fails says how it ended, as it has no terminal to say why on.
    #[test]
    fn what_fails_to_open_a_page_says_so() {
        let (started, said) = said_by(shell("exit 3"), URL);
        assert_eq!(started, Ok(()));
        assert_eq!(said, ["sh did not open the link: exit status: 3"]);
    }

    /// One that is not there says so at once, by its name.
    #[test]
    fn what_is_not_there_to_open_a_page_says_so() {
        let (started, said) = said_by(Command::new("luneta-no-such-opener"), URL);
        assert_eq!(started, Err("luneta-no-such-opener is not on the PATH".to_owned()));
        assert_eq!(said, Vec::<String>::new());
    }

    /// Only a web page goes to the desktop: another machine's daemon says the
    /// link, and the desktop runs a file it is handed by what opens that.
    #[test]
    fn only_a_web_page_is_opened() {
        for url in ["file:///tmp/run.desktop", "-x", "", "javascript:alert(1)"] {
            let (started, said) = said_by(shell(":"), url);
            assert_eq!(started, Err(format!("{url} is no web page to open")));
            assert_eq!(said, Vec::<String>::new());
        }
        assert_eq!(said_by(shell(":"), "http://ghe.example/o/r/pull/1").0, Ok(()));
    }
}

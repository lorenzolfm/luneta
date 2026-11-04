//! The browser of the machine luneta runs on, which the desktop picks: as
//! `xdg-open` asks it on Linux, and `open` on macOS. Each hands the page over,
//! and may stay until the browser it started ends, which outlives luneta. A
//! key held down opens its page once.

use std::io::ErrorKind;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use crate::desktop;
use crate::host::Event;
use crate::pull::Link;

/// What asks the desktop to open a page.
const OPENER: &str = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };

/// How soon after it was last asked for a page asked for again is the key held
/// down: a terminal can send a key held as if typed again and again, and the
/// first repeat comes only after the keyboard's wait, which a desktop lets you
/// set to nearly two seconds.
const HELD: Duration = Duration::from_secs(2);

/// What opens the pages you ask for.
#[derive(Debug, Default)]
pub struct Browser {
    /// The page that started last, and when it was last asked for.
    asked: Option<(Link, Instant)>,
}

impl Browser {
    /// Opens the page of `link` in your browser, asked for at `now`, but for a
    /// key held down. Why it could not start goes back at once, and why it
    /// could not go on to `events`. luneta over ssh opens nothing: the desktop
    /// of the machine it runs on is not the one you sit at.
    pub fn open(
        &mut self,
        link: &Link,
        events: &Sender<Event>,
        now: Instant,
    ) -> Result<(), String> {
        if desktop::elsewhere() {
            let why = "luneta runs over ssh, far from your browser: `y p` copies the link";
            return Err(why.to_owned());
        }
        self.open_with(Command::new(OPENER), link, events, now)
    }

    /// Has `opener` open the page of `link`, unless that is the key held down,
    /// which then lasts from `now`. What could not start is no page asked for,
    /// for a repeat to say so again in place of the note it takes away.
    fn open_with(
        &mut self,
        opener: Command,
        link: &Link,
        events: &Sender<Event>,
        now: Instant,
    ) -> Result<(), String> {
        let held = self.asked.as_ref().is_some_and(|(page, then)| {
            page == link && now.saturating_duration_since(*then) < HELD
        });
        if !held {
            open_by(opener, link, events)?;
        }
        self.asked = Some((link.clone(), now));
        Ok(())
    }
}

/// Has `opener` open the page of `link`, as `Browser::open` says. It is no
/// terminal's, and no group's but its own, for what it starts to draw nothing
/// on luneta's terminal and to outlive its window. It takes a `Link`, which
/// spells the page again, for what a daemon of another machine says never to
/// reach the desktop, which runs a file it is handed.
fn open_by(mut opener: Command, link: &Link, events: &Sender<Event>) -> Result<(), String> {
    let program = opener.get_program().to_string_lossy().into_owned();
    opener.arg(link.url()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
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

    /// What `opener` said of the page of `URL`: whether it started, and what
    /// it told luneta once it ended.
    fn said_by(opener: Command) -> (Result<(), String>, Vec<String>) {
        let (events, heard) = channel();
        let started = open_by(opener, &Link::parse(URL).expect("a link"), &events);
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
        assert_eq!(said_by(shell(&script)), (Ok(()), Vec::new()));
    }

    /// One that fails says how it ended, as it has no terminal to say why on.
    #[test]
    fn what_fails_to_open_a_page_says_so() {
        let (started, said) = said_by(shell("exit 3"));
        assert_eq!(started, Ok(()));
        assert_eq!(said, ["sh did not open the link: exit status: 3"]);
    }

    /// One that is not there says so at once, by its name.
    #[test]
    fn what_is_not_there_to_open_a_page_says_so() {
        let (started, said) = said_by(Command::new("luneta-no-such-opener"));
        assert_eq!(started, Err("luneta-no-such-opener is not on the PATH".to_owned()));
        assert_eq!(said, Vec::<String>::new());
    }

    /// A key held opens its page once: neither its first repeat, after the
    /// keyboard's wait, nor one after it, counted from the one before, asks
    /// anything of the desktop. Another page opens at once, and the same one
    /// once the key has been let go a while. What could not start is tried
    /// again.
    #[test]
    fn a_key_held_opens_its_page_once() {
        let (events, _heard) = channel();
        let page = Link::parse(URL).expect("a link");
        let other = Link::parse("https://github.com/owner/repo/pull/250").expect("a link");
        let mut browser = Browser::default();
        let start = Instant::now();
        assert_eq!(browser.open_with(shell("exit 0"), &page, &events, start), Ok(()));

        // What is not there says so, where the desktop is asked at all.
        let mut asks = |link: &Link, at: Instant| {
            browser.open_with(Command::new("luneta-no-such-opener"), link, &events, at).is_err()
        };
        let just_short = HELD.saturating_sub(Duration::from_millis(1));
        let first_repeat = start + just_short;
        let next_repeat = first_repeat + just_short;
        assert!(!asks(&page, first_repeat), "the first repeat");
        assert!(!asks(&page, next_repeat), "a repeat after it");
        assert!(asks(&other, next_repeat), "another page");
        let pressed_again = next_repeat + HELD;
        assert!(asks(&page, pressed_again), "the same page, later");
        assert!(asks(&page, pressed_again), "what could not start");
    }
}

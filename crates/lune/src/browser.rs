use std::io::ErrorKind;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use lune_domain::projects::BrowserUrl;

use crate::desktop;
use crate::host::Event;
use crate::pull::Link;

const OPENER: &str = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };

const HELD: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub struct Browser {
    asked: Option<(String, Instant)>,
}

impl Browser {
    pub fn open(
        &mut self,
        link: &Link,
        events: &Sender<Event>,
        now: Instant,
    ) -> Result<(), String> {
        self.open_str(&link.url(), events, now)
    }

    pub fn open_url(
        &mut self,
        url: &BrowserUrl,
        events: &Sender<Event>,
        now: Instant,
    ) -> Result<(), String> {
        self.open_str(url.as_str(), events, now)
    }

    fn open_str(
        &mut self,
        url: &str,
        events: &Sender<Event>,
        now: Instant,
    ) -> Result<(), String> {
        if desktop::elsewhere() {
            let why = "lune runs over ssh, far from your browser: `y p` copies the link";
            return Err(why.to_owned());
        }
        self.open_with(Command::new(OPENER), url, events, now)
    }

    fn open_with(
        &mut self,
        opener: Command,
        url: &str,
        events: &Sender<Event>,
        now: Instant,
    ) -> Result<(), String> {
        let held = self.asked.as_ref().is_some_and(|(page, then)| {
            page == url && now.saturating_duration_since(*then) < HELD
        });
        if !held {
            open_by(opener, url, events)?;
        }
        self.asked = Some((url.to_owned(), now));
        Ok(())
    }
}

fn open_by(mut opener: Command, url: &str, events: &Sender<Event>) -> Result<(), String> {
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
        let _ = events.send(Event::Notice(why));
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    const URL: &str = "https://github.com/owner/repo/pull/249";

    fn said_by(opener: Command) -> (Result<(), String>, Vec<String>) {
        let (events, heard) = channel();
        let started = open_by(opener, &Link::parse(URL).expect("a link").url(), &events);
        drop(events);
        let notice = |event| match event {
            Event::Notice(notice) => notice,
            _ => panic!("an event other than a notice"),
        };
        (started, heard.into_iter().map(notice).collect())
    }

    fn shell(script: &str) -> Command {
        let mut shell = Command::new("sh");
        shell.args(["-c", script, "opener"]);
        shell
    }

    #[test]
    fn what_opens_a_page_is_handed_it_and_says_nothing_of_it() {
        let script = format!("test \"$1\" = {URL}");
        assert_eq!(said_by(shell(&script)), (Ok(()), Vec::new()));
    }

    #[test]
    fn what_fails_to_open_a_page_says_so() {
        let (started, said) = said_by(shell("exit 3"));
        assert_eq!(started, Ok(()));
        assert_eq!(said, ["sh did not open the link: exit status: 3"]);
    }

    #[test]
    fn what_is_not_there_to_open_a_page_says_so() {
        let (started, said) = said_by(Command::new("lune-no-such-opener"));
        assert_eq!(started, Err("lune-no-such-opener is not on the PATH".to_owned()));
        assert_eq!(said, Vec::<String>::new());
    }

    #[test]
    fn a_key_held_opens_its_page_once() {
        let (events, _heard) = channel();
        let page = Link::parse(URL).expect("a link");
        let other = Link::parse("https://github.com/owner/repo/pull/250").expect("a link");
        let mut browser = Browser::default();
        let start = Instant::now();
        assert_eq!(browser.open_with(shell("exit 0"), &page.url(), &events, start), Ok(()));

        let mut asks = |link: &Link, at: Instant| {
            browser
                .open_with(Command::new("lune-no-such-opener"), &link.url(), &events, at)
                .is_err()
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

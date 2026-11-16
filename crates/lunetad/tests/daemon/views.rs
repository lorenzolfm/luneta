use std::fs;
use std::time::Duration;

use lunetad::api::show_request::{self, focus, open, Event, Hello};
use lunetad::api::show_response::{self, Laid};
use lunetad::api::{ShowRequest, ShowResponse};
use lunetad::mux::own::api::{self as mux_api, Listing};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Streaming;

use crate::host::{eventually, folder_in};
use crate::own::Owned;
use crate::projects::{kept, project, workstream};

const WAITS: [&str; 2] = ["sleep", "600"];

const HEARD_IN: Duration = Duration::from_secs(10);

struct Shown<'a> {
    owned: &'a Owned,
    asks: mpsc::Sender<ShowRequest>,
    heard: Streaming<ShowResponse>,
    laid: Option<Laid>,
    refused: Vec<String>,
}

fn size(columns: u32, rows: u32) -> show_request::Size {
    show_request::Size { columns, rows }
}

fn rect(column: u32, row: u32, columns: u32, rows: u32) -> show_response::Rect {
    show_response::Rect { column, row, columns, rows }
}

impl<'a> Shown<'a> {
    fn new(owned: &'a Owned, workstream: u64, columns: u32, rows: u32) -> Shown<'a> {
        let folder = folder_in(owned.host.runtime.path()).unwrap();
        let (asks, asked) = mpsc::channel(16);
        let hello = Hello { workstream, size: Some(size(columns, rows)) };
        asks.try_send(ShowRequest { event: Some(Event::Hello(hello)) }).unwrap();
        let heard = owned.block_on(async move {
            let mut daemon = lunetad::connect(&folder).await.unwrap();
            daemon.show(ReceiverStream::new(asked)).await.unwrap().into_inner()
        });
        let mut shown = Shown { owned, asks, heard, laid: None, refused: Vec::new() };
        shown.until("the first view", |_| true);
        shown
    }

    fn ask(&self, event: Event) {
        self.asks.try_send(ShowRequest { event: Some(event) }).unwrap();
    }

    fn open(&mut self) -> u64 {
        let before = self.terminals();
        let argv = WAITS.map(str::to_owned).to_vec();
        let largest = open::Beside::Largest(());
        self.ask(Event::Open(show_request::Open { beside: Some(largest), argv }));
        let laid = self.until("a terminal more", |laid| laid.panes.len() == before.len() + 1);
        let new = laid.panes.iter().find(|pane| !before.contains(&pane.terminal));
        let new = new.expect("the view holds a new terminal").terminal;
        assert_eq!(laid.focused, Some(new), "a client focuses the terminal it opens");
        new
    }

    fn terminals(&self) -> Vec<u64> {
        let laid = self.laid.as_ref().expect("a view was heard");
        laid.panes.iter().map(|pane| pane.terminal).collect()
    }

    fn until(&mut self, what: &str, mut done: impl FnMut(&Laid) -> bool) -> Laid {
        if let Some(laid) = &self.laid
            && done(laid)
        {
            return laid.clone();
        }
        loop {
            let heard = self.heard_in_time();
            let last = &self.laid;
            let heard = heard.unwrap_or_else(|_| panic!("never heard: {what}, last {last:?}"));
            let word = heard.unwrap().expect("the view goes on");
            match word.event.expect("a word says something") {
                show_response::Event::Laid(laid) => {
                    let found = done(&laid);
                    self.laid = Some(laid.clone());
                    if found {
                        return laid;
                    }
                },
                show_response::Event::Refused(why) => self.refused.push(why),
            }
        }
    }

    fn heard_in_time(
        &mut self,
    ) -> Result<Result<Option<ShowResponse>, tonic::Status>, tokio::time::error::Elapsed> {
        let heard = &mut self.heard;
        self.owned.block_on(async { tokio::time::timeout(HEARD_IN, heard.message()).await })
    }

    fn until_refused(&mut self) -> String {
        loop {
            if let Some(why) = self.refused.pop() {
                return why;
            }
            let heard = self.heard_in_time();
            let word = heard.expect("a refusal comes").unwrap().expect("the view goes on");
            match word.event.expect("a word says something") {
                show_response::Event::Laid(laid) => self.laid = Some(laid),
                show_response::Event::Refused(why) => return why,
            }
        }
    }
}

fn tagged(listing: &Listing, terminal: u64) -> Option<&mux_api::Terminal> {
    let tag = format!("luneta terminal {terminal:016x}");
    listing.terminals.iter().find(|listed| listed.tag == tag.as_bytes())
}

fn sized(owned: &Owned, terminal: u64) -> Option<(u32, u32)> {
    let size = tagged(&owned.listing(), terminal)?.size?;
    Some((size.columns, size.rows))
}

fn workstream_in_home(owned: &Owned) -> u64 {
    let home = fs::canonicalize(owned.host.home.path()).unwrap();
    let made = project(&owned.host, &home, None);
    workstream(&owned.host, made, &home)
}

#[test]
fn a_view_opens_terminals_the_mux_holds_tagged_and_sized_to_their_place() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let empty = shown.laid.clone().unwrap();
    assert_eq!((empty.workstream, empty.panes.len(), empty.focused), (workstream, 0, None));

    let first = shown.open();
    let laid = shown.laid.clone().unwrap();
    assert_eq!(laid.panes[0].rect, Some(rect(0, 0, 80, 24)));
    let key = laid.panes[0].key.expect("an open terminal has its key in the mux");
    let listing = owned.listing();
    let listed = tagged(&listing, first).expect("the mux holds the terminal by its tag");
    assert_eq!(listed.key.map(|it| (it.id, it.made_at)), Some((key.id, key.made_at)));
    assert_eq!(sized(&owned, first), Some((78, 22)));

    let second = shown.open();
    let laid = shown.laid.clone().unwrap();
    let rects: Vec<_> = laid.panes.iter().map(|pane| (pane.terminal, pane.rect)).collect();
    assert_eq!(rects, [(first, Some(rect(0, 0, 40, 24))), (second, Some(rect(40, 0, 40, 24)))]);
    eventually("both terminals take the size of their place", || {
        (sized(&owned, first) == Some((38, 22)) && sized(&owned, second) == Some((38, 22)))
            .then_some(())
    });
}

#[test]
fn the_smallest_client_sizes_the_terminals_it_shares_with_others() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut wide = Shown::new(&owned, workstream, 80, 24);
    let terminal = wide.open();
    eventually("the terminal fills the wide client", || {
        (sized(&owned, terminal) == Some((78, 22))).then_some(())
    });
    let mut narrow = Shown::new(&owned, workstream, 60, 20);
    narrow.until("the narrow client's view", |laid| laid.panes.len() == 1);
    wide.until("the narrow size", |laid| laid.size == Some(size(60, 20)));
    eventually("the terminal fits the narrow client", || {
        (sized(&owned, terminal) == Some((58, 18))).then_some(())
    });
    drop(narrow);
    wide.until("the wide size again", |laid| laid.size == Some(size(80, 24)));
    eventually("the terminal fills the wide client again", || {
        (sized(&owned, terminal) == Some((78, 22))).then_some(())
    });
}

#[test]
fn focus_is_each_client_s_own_and_fullscreen_is_the_view_s() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut one = Shown::new(&owned, workstream, 80, 24);
    let left = one.open();
    let right = one.open();
    let mut other = Shown::new(&owned, workstream, 80, 24);
    other.until("the view where the last client was", |laid| laid.focused == Some(right));
    let towards_left = focus::To::Towards(show_request::Direction::Left.into());
    other.ask(Event::Focus(show_request::Focus { to: Some(towards_left) }));
    other.until("the other client on the left", |laid| laid.focused == Some(left));
    assert_eq!(one.laid.as_ref().unwrap().focused, Some(right));

    one.ask(Event::Fullscreen(()));
    let full = |laid: &Laid| laid.full == Some(right) && laid.focused == Some(right);
    one.until("the right terminal fills the view", full);
    other.until("the other client on the full terminal", full);
    eventually("the full terminal takes the whole view", || {
        (sized(&owned, right) == Some((78, 22)) && sized(&owned, left) == Some((38, 22)))
            .then_some(())
    });

    other.ask(Event::Focus(show_request::Focus { to: Some(focus::To::Terminal(left)) }));
    other.until("the view tiled again", |laid| laid.full.is_none() && laid.focused == Some(left));
    one.until("the view tiled again", |laid| laid.full.is_none() && laid.focused == Some(right));
    other.ask(Event::Go(workstream + 1));
    assert!(other.until_refused().contains("no workstream"), "a go to nowhere is refused");
}

fn closed_outside(owned: &Owned, laid: &Laid, terminal: u64) {
    let pane = laid.panes.iter().find(|pane| pane.terminal == terminal).unwrap();
    let key = pane.key.unwrap();
    let mux_key = mux_api::TerminalKey { id: key.id, made_at: key.made_at };
    owned.close_terminal(mux_api::CloseTerminalRequest { terminal: Some(mux_key) });
}

#[test]
fn a_closed_terminal_leaves_the_mux_and_its_view_to_the_terminal_beside_it() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let first = shown.open();
    let second = shown.open();
    let third = shown.open();
    shown.ask(Event::Close(()));
    let laid = shown.until("two terminals left", |laid| laid.panes.len() == 2);
    let left: Vec<u64> = laid.panes.iter().map(|pane| pane.terminal).collect();
    assert_eq!(left, [first, second]);
    assert_eq!(laid.focused, Some(first), "the terminal the closed one split takes the focus");
    eventually("the closed terminal leaves the mux", || {
        tagged(&owned.listing(), third).is_none().then_some(())
    });

    closed_outside(&owned, &laid, second);
    let laid = shown.until("the terminal closed outside gone", |laid| laid.panes.len() == 1);
    assert_eq!((laid.panes[0].terminal, laid.focused), (first, Some(first)));
    closed_outside(&owned, &laid, first);
    let laid = shown.until("the view empty", |laid| laid.panes.is_empty());
    assert_eq!(laid.focused, None);
    let file = fs::read_to_string(kept(&owned.host)).unwrap();
    assert!(!file.contains(r#""view""#), "an empty view is kept as no view: {file}");
}

#[test]
fn a_view_comes_back_with_its_terminals_when_lunetad_starts_again() {
    let mut owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    shown.open();
    shown.open();
    shown.ask(Event::Fullscreen(()));
    let before = shown.until("the view full", |laid| laid.full.is_some());
    drop(shown);
    let file = fs::read_to_string(kept(&owned.host)).unwrap();
    assert!(file.starts_with("{\n  \"version\": 2,"), "{file}");
    assert!(file.contains(r#""command": "sleep 600""#), "{file}");

    owned.stop_daemon();
    owned.start_daemon();
    let mut again = Shown::new(&owned, workstream, 80, 24);
    let after = again.until("the terminals found again", |laid| {
        laid.panes.iter().all(|pane| pane.key.is_some())
    });
    assert_eq!(after, before);
    assert_eq!(fs::read_to_string(kept(&owned.host)).unwrap(), file);
}

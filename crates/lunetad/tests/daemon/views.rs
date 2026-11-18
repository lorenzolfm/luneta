use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::show_request::{self, focus, open, r#move, Event, Hello};
use lunetad::api::show_response::{self, pane::Found, Laid, Pane};
use lunetad::api::change_projects_request::{
    Change, CloseWorkstream, ForgetWorkstream, RenameTerminal,
};
use lunetad::api::projects::Held;
use lunetad::api::{agent, terminal, ShowRequest, ShowResponse, Terminal, TerminalKey, Workstream};
use lunetad::mux::own::api::{self as mux_api, typed, Listing};
use lunetad::mux::own::frames::{Line, Rows, Shows};
use lunetad::mux::own::viewer::{view, Greeting, Seen, FRAMES_MAX};
use lunetad::projects::{Size, WorkstreamId};
use lunetad::{Asked, Beside, Heard};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Status, Streaming};

use crate::agents::file_of_pid;
use crate::host::{eventually, folder_in, script};
use crate::own::Owned;
use crate::projects::{asked, kept, project, workstream};

const WAITS: [&str; 2] = ["sleep", "600"];

const HEARD_IN: Duration = Duration::from_secs(10);

const RESTORE_PAUSE: Duration = Duration::from_secs(10);

const RESTORE_ATTEMPTS_MAX: u32 = 3;

const READ_RUNS_SWEEPS: Duration = Duration::from_secs(5);

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
        Shown::running_in(owned, workstream, (columns, rows), None)
    }

    fn running_in(
        owned: &'a Owned,
        workstream: u64,
        (columns, rows): (u32, u32),
        runs_in: Option<TerminalKey>,
    ) -> Shown<'a> {
        let folder = folder_in(owned.host.runtime.path()).unwrap();
        let (asks, asked) = mpsc::channel(16);
        let hello = Hello { workstream, size: Some(size(columns, rows)), runs_in };
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
        self.open_running(&WAITS)
    }

    fn open_running(&mut self, words: &[&str]) -> u64 {
        self.open_beside(open::Beside::Largest(()), words)
    }

    fn open_beside(&mut self, beside: open::Beside, words: &[&str]) -> u64 {
        let before = self.terminals();
        let argv = words.iter().map(|word| (*word).to_owned()).collect();
        self.ask(Event::Open(show_request::Open { beside: Some(beside), argv }));
        let laid = self.until("a terminal more", |laid| laid.panes.len() == before.len() + 1);
        let new = laid.panes.iter().find(|pane| !before.contains(&pane.terminal));
        let new = new.expect("the view holds a new terminal").terminal;
        assert_eq!(focused_of(&laid), Some(new), "a client focuses the terminal it opens");
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
            let (last, refused) = (&self.laid, &self.refused);
            let never = |_| panic!("never heard: {what}, last {last:?}, refused {refused:?}");
            let heard = heard.unwrap_or_else(never);
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

    fn until_ended(&mut self) -> Status {
        loop {
            let heard = self.heard_in_time().expect("the view ends in time");
            match heard {
                Ok(Some(_)) => {},
                Ok(None) => panic!("the view ends with no word of why"),
                Err(status) => return status,
            }
        }
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

fn focused_of(laid: &Laid) -> Option<u64> {
    match laid.shown? {
        show_response::laid::Shown::Tiled(it) | show_response::laid::Shown::Full(it) => Some(it),
    }
}

fn full_of(laid: &Laid) -> Option<u64> {
    match laid.shown? {
        show_response::laid::Shown::Full(it) => Some(it),
        show_response::laid::Shown::Tiled(_) => None,
    }
}

fn key(pane: &Pane) -> Option<TerminalKey> {
    match &pane.found {
        Some(Found::Key(key)) => Some(*key),
        Some(Found::Lost(()) | Found::Looking(())) | None => None,
    }
}

fn lost(pane: &Pane) -> bool {
    matches!(pane.found, Some(Found::Lost(())))
}

fn tagged(listing: &Listing, terminal: u64) -> Option<&mux_api::Terminal> {
    let tag = format!("luneta terminal {terminal:016x}");
    listing.terminals.iter().find(|listed| listed.tag == tag.as_bytes())
}

fn sized(owned: &Owned, terminal: u64) -> Option<(u32, u32)> {
    let size = tagged(&owned.listing(), terminal)?.size?;
    Some((size.columns, size.rows))
}

fn watched_workstream(owned: &Owned, workstream: u64) -> Option<Workstream> {
    let host = owned.host.host();
    let Some(Held::Kept(kept)) = host.projects.and_then(|projects| projects.held) else {
        panic!("lunetad keeps no projects");
    };
    let workstreams = kept.projects.into_iter().flat_map(|project| project.workstreams);
    let mut workstreams = workstreams.filter(|it| it.id == workstream);
    workstreams.next()
}

fn watched(owned: &Owned, workstream: u64) -> Vec<Terminal> {
    watched_workstream(owned, workstream).expect("lunetad watches the workstream").terminals
}

fn root_of(owned: &Owned, terminal: u64) -> u32 {
    let listing = owned.listing();
    let listed = tagged(&listing, terminal).expect("the mux holds the terminal");
    let Some(mux_api::terminal::Program::Runs(runs)) = &listed.program else {
        panic!("terminal {terminal} runs nothing: {listed:?}");
    };
    runs.pid
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
    let laid = (empty.workstream, empty.panes.len(), focused_of(&empty));
    assert_eq!(laid, (workstream, 0, None));

    let first = shown.open();
    let laid = shown.laid.clone().unwrap();
    assert_eq!(laid.panes[0].rect, Some(rect(0, 0, 80, 24)));
    let key = key(&laid.panes[0]).expect("an open terminal has its key in the mux");
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
fn a_client_that_runs_in_a_terminal_of_the_view_it_shows_sizes_none_of_it() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut outer = Shown::new(&owned, workstream, 80, 24);
    let terminal = outer.open();
    let runs_in = key(&outer.laid.clone().unwrap().panes[0]);
    let mut inner = Shown::running_in(&owned, workstream, (50, 16), runs_in);
    inner.until("the outer client's size", |laid| laid.size == Some(size(80, 24)));
    let other = inner.open();
    outer.until("the outer size with both", |laid| laid.panes.len() == 2);
    assert_eq!(outer.laid.clone().unwrap().size, Some(size(80, 24)));
    eventually("both terminals fit the outer client", || {
        (sized(&owned, terminal) == Some((38, 22)) && sized(&owned, other) == Some((38, 22)))
            .then_some(())
    });
    drop(outer);
    let laid = inner.until("the inner size, alone", |laid| laid.size == Some(size(50, 16)));
    let pane = laid.panes.iter().find(|pane| pane.terminal == other).unwrap();
    let rect = pane.rect.unwrap();
    eventually("the other terminal fits the inner client", || {
        (sized(&owned, other) == Some((rect.columns - 2, rect.rows - 2))).then_some(())
    });
    assert_eq!(sized(&owned, terminal), Some((38, 22)), "the inner client's own keeps its size");
}

#[test]
fn focus_is_each_client_s_own_and_fullscreen_is_the_view_s() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut one = Shown::new(&owned, workstream, 80, 24);
    let left = one.open();
    let right = one.open();
    let mut other = Shown::new(&owned, workstream, 80, 24);
    other.until("the view where the last client was", |laid| focused_of(laid) == Some(right));
    let towards_left = focus::To::Direction(show_request::Direction::Left.into());
    other.ask(Event::Focus(show_request::Focus { to: Some(towards_left) }));
    other.until("the other client on the left", |laid| focused_of(laid) == Some(left));
    assert_eq!(one.laid.as_ref().and_then(focused_of), Some(right));

    one.ask(Event::Fullscreen(()));
    let full = |laid: &Laid| full_of(laid) == Some(right) && focused_of(laid) == Some(right);
    one.until("the right terminal fills the view", full);
    other.until("the other client on the full terminal", full);
    eventually("the full terminal takes the whole view", || {
        (sized(&owned, right) == Some((78, 22)) && sized(&owned, left) == Some((38, 22)))
            .then_some(())
    });

    other.ask(Event::Focus(show_request::Focus { to: Some(focus::To::Terminal(left)) }));
    let tiled = |on| move |laid: &Laid| full_of(laid).is_none() && focused_of(laid) == Some(on);
    other.until("the view tiled again", tiled(left));
    one.until("the view tiled again", tiled(right));
    other.ask(Event::Go(workstream + 1));
    assert!(other.until_refused().contains("no workstream"), "a go to nowhere is refused");
}

fn closed_outside(owned: &Owned, laid: &Laid, terminal: u64) {
    let pane = laid.panes.iter().find(|pane| pane.terminal == terminal).unwrap();
    let key = key(pane).unwrap();
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
    let took = "the terminal the closed one split takes the focus";
    assert_eq!(focused_of(&laid), Some(first), "{took}");
    eventually("the closed terminal leaves the mux", || {
        tagged(&owned.listing(), third).is_none().then_some(())
    });

    closed_outside(&owned, &laid, second);
    let laid = shown.until("the terminal closed outside gone", |laid| laid.panes.len() == 1);
    assert_eq!((laid.panes[0].terminal, focused_of(&laid)), (first, Some(first)));
    closed_outside(&owned, &laid, first);
    let laid = shown.until("the view empty", |laid| laid.panes.is_empty());
    assert_eq!(focused_of(&laid), None);
    let file = fs::read_to_string(kept(&owned.host)).unwrap();
    assert!(!file.contains(r#""view""#), "an empty view is kept as no view: {file}");
    let again = shown.open();
    let made = "the mux that ended with its last terminal gives way to the next";
    assert!(tagged(&owned.listing(), again).is_some(), "{made}");
}

#[test]
fn a_view_comes_back_with_its_terminals_when_lunetad_starts_again() {
    let mut owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    shown.open();
    shown.open();
    shown.ask(Event::Fullscreen(()));
    let before = shown.until("the view full", |laid| full_of(laid).is_some());
    drop(shown);
    let full = format!(r#""full": "{:016x}""#, full_of(&before).unwrap());
    eventually("the view full is kept within a sweep", || {
        fs::read_to_string(kept(&owned.host)).unwrap().contains(&full).then_some(())
    });
    let file = fs::read_to_string(kept(&owned.host)).unwrap();
    assert!(file.starts_with("{\n  \"version\": 4,"), "{file}");
    assert!(file.contains(r#""command": "sleep 600""#), "{file}");

    owned.stop_daemon();
    owned.start_daemon();
    let mut again = Shown::new(&owned, workstream, 80, 24);
    let after = again.until("the terminals found again", |laid| {
        assert!(!laid.panes.iter().any(lost), "no terminal is lost before a look: {laid:?}");
        laid.panes.iter().all(|pane| key(pane).is_some())
    });
    assert_eq!(after, before);
    assert_eq!(fs::read_to_string(kept(&owned.host)).unwrap(), file);
}

#[test]
fn two_views_that_each_open_a_terminal_alone_in_one_empty_workstream_open_one() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut one = Shown::new(&owned, workstream, 80, 24);
    let mut other = Shown::new(&owned, workstream, 80, 24);
    let alone = || {
        let argv = WAITS.iter().map(|word| (*word).to_owned()).collect();
        Event::Open(show_request::Open { beside: Some(open::Beside::Alone(())), argv })
    };
    one.ask(alone());
    other.ask(alone());
    let opened = one.until("a terminal alone", |laid| !laid.panes.is_empty());
    other.until("the other sees it", |laid| !laid.panes.is_empty());
    one.ask(Event::Size(size(79, 24)));
    let resized = one.until("the view again", |laid| laid.size != opened.size);
    assert_eq!(resized.panes.len(), 1, "the second open alone opens nothing");
    assert!(other.refused.is_empty() && one.refused.is_empty(), "{:?}", other.refused);
    assert_eq!(watched(&owned, workstream).len(), 1);
}

#[test]
fn an_open_beside_the_focused_terminal_splits_it_on_its_longer_side() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let first = shown.open();
    let second = shown.open();
    shown.open();
    let to = Some(show_request::focus::To::Terminal(first));
    shown.ask(Event::Focus(show_request::Focus { to }));
    shown.until("the first terminal focused", |laid| focused_of(laid) == Some(first));
    let new = shown.open_beside(open::Beside::Focused(()), &WAITS);
    let laid = shown.laid.clone().unwrap();
    let rect_of = |terminal| laid.panes.iter().find(|pane| pane.terminal == terminal).unwrap().rect;
    assert_eq!(rect_of(first), Some(rect(0, 0, 20, 12)));
    assert_eq!(rect_of(new), Some(rect(20, 0, 20, 12)));
    assert_eq!(rect_of(second), Some(rect(40, 0, 40, 24)), "the largest terminal stays whole");
}

#[test]
fn an_open_the_view_refuses_makes_no_terminal_in_the_mux() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 1000, 1000);
    let opened: Vec<u64> = (0..64).map(|_| shown.open()).collect();
    let largest = open::Beside::Largest(());
    let argv = WAITS.iter().map(|word| (*word).to_owned()).collect();
    shown.ask(Event::Open(show_request::Open { beside: Some(largest), argv }));
    assert!(shown.until_refused().contains("no terminal has room"));
    let listing = owned.listing();
    assert_eq!(listing.terminals.len(), opened.len(), "the mux holds only the views' terminals");
    assert!(opened.iter().all(|terminal| tagged(&listing, *terminal).is_some()));
    shown.ask(Event::Close(()));
    shown.until("a terminal fewer", |laid| laid.panes.len() == opened.len() - 1);
}

#[test]
fn a_resize_moves_a_border_and_a_move_trades_two_terminals_places() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let left = shown.open();
    let right = shown.open();
    let towards_left: i32 = show_request::Direction::Left.into();
    let resize = |grow| show_request::Resize { direction: Some(towards_left), grow };
    shown.ask(Event::Resize(resize(true)));
    let laid = shown.until("the border moved", |laid| {
        laid.panes[0].rect == Some(rect(0, 0, 36, 24))
    });
    assert_eq!(laid.panes[1].rect, Some(rect(36, 0, 44, 24)));
    eventually("the grown terminal takes its place's size", || {
        (sized(&owned, right) == Some((42, 22))).then_some(())
    });
    let to = Some(r#move::To::Direction(towards_left));
    shown.ask(Event::Move(show_request::Move { to }));
    let laid = shown.until("the terminals traded", |laid| laid.panes[0].terminal == right);
    assert_eq!((laid.panes[1].terminal, focused_of(&laid)), (left, Some(right)));
    shown.ask(Event::Fullscreen(()));
    shown.until("the view full", |laid| full_of(laid) == Some(right));
    shown.ask(Event::Resize(resize(true)));
    assert!(shown.until_refused().contains("resized by none"));
    let to = Some(r#move::To::Direction(show_request::Direction::Right.into()));
    shown.ask(Event::Move(show_request::Move { to }));
    assert!(shown.until_refused().contains("moves none"));
}

#[test]
fn a_resize_that_names_no_direction_moves_every_border_of_the_focused_terminal() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    shown.open();
    shown.open();
    let around = |grow| Event::Resize(show_request::Resize { direction: None, grow });
    shown.ask(around(true));
    let laid = shown.until("the border moved", |laid| {
        laid.panes[1].rect == Some(rect(36, 0, 44, 24))
    });
    assert_eq!(laid.panes[0].rect, Some(rect(0, 0, 36, 24)));
    shown.ask(around(false));
    shown.until("the border back", |laid| laid.panes[1].rect == Some(rect(40, 0, 40, 24)));
    let unknown = Event::Resize(show_request::Resize { direction: Some(0), grow: true });
    shown.ask(unknown);
    assert!(shown.until_refused().contains("no direction it knows"));
}

#[test]
fn a_workstream_closes_with_its_terminals_and_ends_the_views_of_it() {
    let owned = Owned::new();
    let home = fs::canonicalize(owned.host.home.path()).unwrap();
    let made = project(&owned.host, &home, None);
    let add = || workstream(&owned.host, made, &home);
    let (closing, other) = (add(), add());
    let mut shown = Shown::new(&owned, closing, 80, 24);
    let terminals = [shown.open(), shown.open()];
    let mut beside = Shown::new(&owned, other, 80, 24);
    let stays = beside.open();
    let forget = Change::ForgetWorkstream(ForgetWorkstream { workstream: closing });
    let refused = asked(&owned.host, forget).unwrap_err();
    assert_eq!(refused.code(), Code::InvalidArgument);
    assert!(refused.message().contains("holds 2 terminals still"), "{}", refused.message());

    let close = || Change::CloseWorkstream(CloseWorkstream { workstream: closing });
    let stalled = owned.stall_mux();
    let unclosed = asked(&owned.host, close()).unwrap_err();
    assert_eq!(unclosed.code(), Code::Unavailable, "{unclosed:?}");
    assert_eq!(watched(&owned, closing).len(), 2, "no terminal closed, so none is forgotten");
    drop(stalled);
    assert_eq!(asked(&owned.host, close()).unwrap(), None);
    eventually("the workstream's terminals leave the mux", || {
        let listing = owned.listing();
        terminals.iter().all(|terminal| tagged(&listing, *terminal).is_none()).then_some(())
    });
    assert!(tagged(&owned.listing(), stays).is_some(), "another workstream's terminal stays");
    let ended = shown.until_ended();
    assert_eq!(ended.code(), Code::NotFound, "{ended:?}");
    assert_eq!(watched_workstream(&owned, closing), None);
    let file = fs::read_to_string(kept(&owned.host)).unwrap();
    assert!(!file.contains(&format!("{closing:016x}")), "{file}");
    let again = asked(&owned.host, close()).unwrap_err();
    assert_eq!(again.code(), Code::InvalidArgument);
    assert!(again.message().contains("no workstream has the id"), "{}", again.message());
}

#[test]
fn a_terminal_s_name_and_the_last_use_of_a_workstream_reach_watch_and_the_file() {
    let owned = Owned::new();
    let home = fs::canonicalize(owned.host.home.path()).unwrap();
    let made = project(&owned.host, &home, None);
    let [first, second] = [(); 2].map(|()| workstream(&owned.host, made, &home));
    let used = |workstream| watched_workstream(&owned, workstream).unwrap().used_at;
    assert_eq!((used(first), used(second)), (None, None));
    let mut shown = Shown::new(&owned, first, 80, 24);
    let first_used = eventually("a client's arrival is a use", || used(first));
    assert_eq!(used(second), None);
    let terminal = shown.open();
    let to = Some("tests".to_owned());
    let rename = Change::RenameTerminal(RenameTerminal { terminal, to });
    assert_eq!(asked(&owned.host, rename).unwrap(), None);
    let named = watched(&owned, first).into_iter().find(|it| it.id == terminal).unwrap().name;
    assert_eq!(named.as_deref(), Some("tests"));
    let blank = RenameTerminal { terminal, to: Some(String::new()) };
    let refused = asked(&owned.host, Change::RenameTerminal(blank)).unwrap_err();
    assert_eq!(refused.code(), Code::InvalidArgument);

    shown.ask(Event::Go(second));
    shown.until("the second workstream", |laid| laid.workstream == second);
    let second_used = eventually("a go is a use", || used(second));
    let millis = |at: prost_types::Timestamp| at.seconds * 1000 + i64::from(at.nanos) / 1_000_000;
    assert!(millis(second_used) > millis(first_used), "{second_used:?} {first_used:?}");
    eventually("the uses reach the file within a sweep", || {
        let file = fs::read_to_string(kept(&owned.host)).ok()?;
        let named = file.contains(r#""name": "tests""#);
        (named && file.matches(r#""used_millis": "#).count() == 2).then_some(())
    });
}

fn mux_key(key: TerminalKey) -> mux_api::TerminalKey {
    mux_api::TerminalKey { id: key.id, made_at: key.made_at }
}

fn front_of(owned: &Owned, terminal: u64) -> Option<Vec<String>> {
    let listing = owned.listing();
    let Some(mux_api::terminal::Program::Runs(runs)) = &tagged(&listing, terminal)?.program else {
        return None;
    };
    runs.foreground.as_ref().map(|front| front.argv.clone())
}

fn logged(owned: &Owned) -> String {
    let log = owned.host.home.path().join(".local/state/luneta/luneta.log");
    fs::read_to_string(log).unwrap_or_default()
}

#[test]
fn a_view_restores_its_terminals_when_the_mux_dies() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let terminals = [
        heard_in_below(&owned, &mut shown, "sleep 602"),
        heard_in_below(&owned, &mut shown, "sleep 606"),
    ];
    let laid = restored_after_kill(&owned, &mut shown);
    assert_eq!(laid.panes.iter().map(|pane| pane.terminal).collect::<Vec<_>>(), terminals);
    assert_eq!(shown.refused, Vec::<String>::new());
    let below = fs::canonicalize(owned.host.home.path()).unwrap().join("below");
    let listing = owned.listing();
    for (pane, command) in laid.panes.iter().zip(["sleep 602", "sleep 606"]) {
        let listed = tagged(&listing, pane.terminal).expect("the new mux holds the terminal");
        assert_eq!(listed.cwd, below.to_str().unwrap(), "a terminal comes back in its folder");
        let restored = mux_key(key(pane).unwrap());
        eventually("its last command is typed at the prompt", || {
            owned.dumped(restored).contains(command).then_some(())
        });
        let front = front_of(&owned, pane.terminal).expect("a shell runs in the terminal");
        assert_ne!(front.first().map(String::as_str), Some("sleep"), "the command is not run");
    }
    let log = logged(&owned);
    for trouble in ["panicked", "not typed", "not restored"] {
        assert!(!log.contains(trouble), "{log}");
    }
}

#[test]
fn a_restored_terminal_that_ran_nothing_closes_and_comes_back_the_same_after_a_second_crash() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let stays = heard_in_below(&owned, &mut shown, "sleep 602");
    let closes = heard_in_below(&owned, &mut shown, "sleep 606");
    let crashed = Instant::now();
    let laid = restored_after_kill(&owned, &mut shown);
    for pane in &laid.panes {
        let restored = mux_key(key(pane).unwrap());
        eventually("its last command is typed at the prompt", || {
            owned.dumped(restored).contains("sleep 60").then_some(())
        });
    }
    shown.ask(Event::Focus(show_request::Focus { to: Some(focus::To::Terminal(closes)) }));
    shown.until("the terminal to close focused", |laid| focused_of(laid) == Some(closes));
    shown.ask(Event::Close(()));
    let terminals = |laid: &Laid| laid.panes.iter().map(|pane| pane.terminal).collect::<Vec<_>>();
    shown.until("the restored terminal closed", |laid| terminals(laid) == [stays]);
    eventually("the closed terminal leaves the mux", || {
        tagged(&owned.listing(), closes).is_none().then_some(())
    });
    thread::sleep(RESTORE_PAUSE.saturating_sub(crashed.elapsed()));
    let laid = restored_after_kill(&owned, &mut shown);
    assert_eq!(terminals(&laid), [stays], "a closed terminal stays closed");
    let again = mux_key(key(&laid.panes[0]).unwrap());
    eventually("its last command is typed again", || {
        owned.dumped(again).contains("sleep 602").then_some(())
    });
    let front = front_of(&owned, stays).expect("a shell runs in the terminal");
    assert_ne!(front.first().map(String::as_str), Some("sleep"), "the command is not run");
    let log = logged(&owned);
    for trouble in ["panicked", "not typed", "not restored"] {
        assert!(!log.contains(trouble), "{log}");
    }
}

#[test]
fn a_terminal_that_luneta_mux_keeps_ending_after_is_given_up_and_left_lost() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    heard_in_below(&owned, &mut shown, "sleep 607");
    let mut crashed = Instant::now();
    restored_after_kill(&owned, &mut shown);
    for _ in 1..RESTORE_ATTEMPTS_MAX {
        thread::sleep(RESTORE_PAUSE.saturating_sub(crashed.elapsed()));
        crashed = Instant::now();
        restored_after_kill(&owned, &mut shown);
    }
    owned.kill_mux();
    shown.until("the terminal left lost", |laid| lost(&laid.panes[0]));
    eventually("the terminal is given up", || {
        logged(&owned).contains("given up after 3 attempts").then_some(())
    });
    thread::sleep(RESTORE_PAUSE.saturating_sub(crashed.elapsed()) + Duration::from_secs(3));
    let unrun = std::os::unix::net::UnixStream::connect(owned.socket()).is_err();
    assert!(unrun, "no luneta-mux is started again for a terminal given up");
    assert!(lost(&shown.until("the terminal still lost", |_| true).panes[0]));
}

#[test]
fn a_line_typed_by_a_restore_outlives_a_restart_of_lunetad() {
    let mut owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let terminal = heard_in_below(&owned, &mut shown, "sleep 602");
    let laid = restored_after_kill(&owned, &mut shown);
    let restored = mux_key(key(&laid.panes[0]).unwrap());
    eventually("its last command is typed at the prompt", || {
        owned.dumped(restored).contains("sleep 602").then_some(())
    });
    let unrun = r#""unrun": "sleep 602""#;
    eventually("the line is kept, left unrun", || {
        fs::read_to_string(kept(&owned.host)).ok()?.contains(unrun).then_some(())
    });
    drop(shown);
    owned.stop_daemon();
    owned.start_daemon();
    let mut again = Shown::new(&owned, workstream, 80, 24);
    again.until("the terminal found", |laid| laid.panes.iter().all(|pane| key(pane).is_some()));
    thread::sleep(READ_RUNS_SWEEPS);
    let file = fs::read_to_string(kept(&owned.host)).unwrap();
    assert!(file.contains(unrun), "the line is still kept, left unrun: {file}");
    let laid = restored_after_kill(&owned, &mut again);
    assert_eq!(laid.panes.iter().map(|pane| pane.terminal).collect::<Vec<_>>(), [terminal]);
    let typed = mux_key(key(&laid.panes[0]).unwrap());
    eventually("its last command is typed again", || {
        owned.dumped(typed).contains("sleep 602").then_some(())
    });
    let log = logged(&owned);
    for trouble in ["panicked", "not typed", "not restored", "not kept"] {
        assert!(!log.contains(trouble), "{log}");
    }
}

#[test]
fn a_command_that_spans_lines_is_kept_untyped_when_its_terminal_is_restored() {
    let mut owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    heard_in_below(&owned, &mut shown, "sleep 603");
    drop(shown);
    owned.stop_daemon();
    owned.kill_mux();
    let home = fs::canonicalize(owned.host.home.path()).unwrap();
    fs::remove_dir_all(home.join("below")).unwrap();
    let file = fs::read_to_string(kept(&owned.host)).unwrap();
    let spans = r#""command": "sleep 603\necho done""#;
    fs::write(kept(&owned.host), file.replace(r#""command": "sleep 603""#, spans)).unwrap();
    owned.start_daemon();
    let mut again = Shown::new(&owned, workstream, 80, 24);
    again.until("the terminal restored", |laid| laid.panes.iter().all(|pane| key(pane).is_some()));
    let folder = format!(r#""folder": "{}""#, home.display());
    let file = eventually("what the terminal restored runs is read", || {
        let file = fs::read_to_string(kept(&owned.host)).ok()?;
        file.contains(&folder).then_some(file)
    });
    let unrun = r#""unrun": "sleep 603\necho done""#;
    assert!(file.contains(unrun), "a command that spans lines is kept, left unrun: {file}");
    let log = logged(&owned);
    assert!(log.contains("spans lines, and is kept, not typed"), "{log}");
}

#[test]
fn an_agent_resumes_when_lunetad_starts_with_no_mux() {
    let mut owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let terminal = shown.open();
    let pid = root_of(&owned, terminal);
    let sessions = owned.host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let file = file_of_pid("resumed-7c22", pid, "idle");
    fs::write(sessions.join(format!("{pid}.json")), file.to_string()).unwrap();
    let agent = r#""agent": {"harness": "claude-code", "session": "resumed-7c22"}"#;
    eventually("the agent is kept as what the terminal runs", || {
        let file = fs::read_to_string(kept(&owned.host)).ok()?;
        file.replace(['\n', ' '], "").contains(&agent.replace(' ', "")).then_some(())
    });
    let asked = owned.host.home.path().join("claude-asked");
    let claude = owned.host.zellij.bin().join("claude");
    script(&claude, &format!("echo \"$*\" > '{}'\nexec sleep 600\n", asked.display()));
    drop(shown);
    owned.stop_daemon();
    owned.kill_mux();
    owned.start_daemon();
    let mut again = Shown::new(&owned, workstream, 80, 24);
    again.until("the terminal restored", |laid| laid.panes.iter().all(|pane| key(pane).is_some()));
    let said = eventually("the agent is started again", || fs::read_to_string(&asked).ok());
    assert_eq!(said, "--resume resumed-7c22\n");
}

#[test]
fn where_a_terminal_is_and_what_it_runs_reach_the_file() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    heard_in_below(&owned, &mut shown, "sleep 601");
}

fn restored_after_kill(owned: &Owned, shown: &mut Shown) -> Laid {
    let laid = shown.laid.clone().expect("a view was heard");
    let before: Vec<Option<TerminalKey>> = laid.panes.iter().map(key).collect();
    owned.kill_mux();
    shown.until("the terminals restored", |laid| {
        let found: Vec<Option<TerminalKey>> = laid.panes.iter().map(key).collect();
        found.len() == before.len()
            && found.iter().zip(&before).all(|(found, was)| found.is_some() && found != was)
    })
}

fn heard_in_below(owned: &Owned, shown: &mut Shown, runs: &str) -> u64 {
    let below = fs::canonicalize(owned.host.home.path()).unwrap().join("below");
    fs::create_dir_all(&below).unwrap();
    let said = r#"(sleep 0.3; printf '\033]7;file://localhost%s\007' "$PWD") &"#;
    let line = format!("cd '{}' && {said} exec {runs}", below.display());
    let terminal = shown.open_running(&["sh", "-c", &line]);
    let folder = format!(r#""folder": "{}""#, below.display());
    let command = format!(r#""command": "{runs}""#);
    eventually("the folder and command it runs are kept", || {
        let file = fs::read_to_string(kept(&owned.host)).ok()?;
        (file.contains(&folder) && file.contains(&command)).then_some(())
    });
    terminal
}

#[test]
fn a_workstream_lists_its_view_s_terminals_on_watch_in_their_order() {
    let owned = Owned::new();
    let home = fs::canonicalize(owned.host.home.path()).unwrap();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let plain = shown.open();
    let titled = r"printf '\033]0;\342\234\263 Fixing the tests\007'; exec sleep 604";
    let topical = shown.open_running(&["sh", "-c", titled]);
    let laid = shown.laid.clone().unwrap();
    let keys: Vec<Option<TerminalKey>> = laid.panes.iter().map(key).collect();
    let (plain_key, topical_key) = (keys[0], keys[1]);
    let told = eventually("the terminals and what they run are watched", || {
        let terminals = watched(&owned, workstream);
        let ran = terminals.iter().map(|it| it.command.as_deref()).collect::<Vec<_>>();
        let titled = terminals.get(1).and_then(|it| it.titled.as_ref());
        (ran == [Some("sleep 600"), Some("sleep 604")] && titled.is_some()).then_some(terminals)
    });
    let ids: Vec<u64> = told.iter().map(|it| it.id).collect();
    assert_eq!(ids, [plain, topical]);
    let found: Vec<Option<terminal::Found>> = told.iter().map(|it| it.found).collect();
    let said = |key: Option<TerminalKey>| Some(terminal::Found::Key(key.unwrap()));
    assert_eq!(found, [said(plain_key), said(topical_key)]);
    let topic = told[1].titled.as_ref().and_then(|titled| titled.topic.as_deref());
    assert_eq!(topic, Some("Fixing the tests"));
    for terminal in &told {
        assert_eq!((terminal.folder.as_str(), terminal.held), (home.to_str().unwrap(), false));
    }

    owned.kill_mux();
    eventually("the terminals are watched restored, by new keys", || {
        let terminals = watched(&owned, workstream);
        let new = |(it, was): (&Terminal, &Option<terminal::Found>)| {
            matches!(it.found, Some(terminal::Found::Key(_))) && it.found != *was
        };
        (terminals.len() == 2 && terminals.iter().zip(&found).all(new)).then_some(())
    });
}

#[test]
fn a_terminal_s_shell_marks_say_what_it_ran_on_watch() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let mark = "m() { printf '\\033]133;%s\\007' \"$1\"; }";
    let said = format!("{mark}; m A; m 'C;cmdline_url=make%20test'; m 'D;3'; m A; exec sleep 600");
    let unsaid = format!("{mark}; m A; m C; m 'D;4'; m A; exec sleep 604");
    shown.open_running(&["sh", "-c", &said]);
    shown.open_running(&["sh", "-c", &unsaid]);
    let ended = |terminal: &Terminal| match terminal.shell.as_ref()?.doing.as_ref()? {
        lunetad::api::shell::Doing::Ended(ended) => Some(ended.clone()),
        _ => None,
    };
    let told = eventually("the shells say their commands ended", || {
        let terminals = watched(&owned, workstream);
        let ended: Option<Vec<_>> = terminals.iter().map(ended).collect();
        ended.filter(|ended| ended.len() == 2)
    });
    let status = lunetad::api::shell::ended::Ending::Status;
    assert_eq!(told[0].command.as_deref(), Some("make test"));
    assert_eq!((told[0].ending, told[1].ending), (Some(status(3)), Some(status(4))));
    assert!(told[0].at.is_some() && told[0].took.is_some(), "{told:?}");
    assert_eq!(told[1].command, None, "a shell that marks no line names no command");
}

#[test]
fn an_agent_in_a_view_s_terminal_is_there_by_the_terminal_s_key() {
    let owned = Owned::new();
    let workstream = workstream_in_home(&owned);
    let mut shown = Shown::new(&owned, workstream, 80, 24);
    let terminal = shown.open();
    let laid = shown.laid.clone().unwrap();
    let key = key(&laid.panes[0]).unwrap();
    let pid = root_of(&owned, terminal);
    let sessions = owned.host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let file = file_of_pid("in-a-view", pid, "busy");
    fs::write(sessions.join(format!("{pid}.json")), file.to_string()).unwrap();

    let agent = eventually("the agent is on the host", || {
        owned.host.host().agents.into_iter().find(|agent| agent.pid == pid)
    });
    assert_eq!(agent.pane, Some(agent::Pane::Terminal(key)));
    assert_eq!(agent.zellij(), None);
    let host = owned.host.host();
    let own_session = |session: &lunetad::api::Session| {
        matches!(session.mux, Some(lunetad::api::session::Mux::Own(_)))
    };
    assert!(!host.sessions.iter().any(own_session), "{:?}", host.sessions);
    let own_agent = |agent: &lunetad::api::Agent| matches!(agent.pane, Some(agent::Pane::Own(_)));
    assert!(!host.agents.iter().any(own_agent), "{:?}", host.agents);
    assert!(host.unread_files.iter().all(|file| file.own.is_none()), "{:?}", host.unread_files);
}

fn text_of(lines: &[Line]) -> Vec<String> {
    let shown = |glyph: &lunetad::mux::own::frames::Glyph| match &glyph.shows {
        Shows::Blank => " ".to_owned(),
        Shows::Narrow(grapheme) | Shows::Wide(grapheme) => grapheme.as_str().to_owned(),
    };
    let line = |line: &Line| {
        let text: String = line.glyphs().iter().map(shown).collect();
        text.trim_end().to_owned()
    };
    lines.iter().map(line).collect()
}

#[test]
fn a_view_laid_out_by_lunetad_is_seen_and_typed_into_through_the_mux() {
    let owned = Owned::new();
    let workstream = WorkstreamId::of(workstream_in_home(&owned)).unwrap();
    let folder = folder_in(owned.host.runtime.path()).unwrap();
    let (heard, hearing) = std::sync::mpsc::channel();
    let size = Size::of(80, 24).unwrap();
    let show = lunetad::show(folder, workstream, size, None, move |it| drop(heard.send(it)));
    let laid = || match hearing.recv_timeout(HEARD_IN).expect("lunetad lays the view out") {
        Heard::Laid(laid) => laid,
        Heard::Refused(why) | Heard::Forgotten(why) | Heard::Lost(why) => {
            panic!("the view is not laid out: {why}")
        },
    };
    assert!(laid().is_empty());
    show.ask(Asked::Open { beside: Beside::Largest, argv: vec!["sh".to_owned()] });
    let opened = laid();
    let pane = *opened.panes().next().expect("the view lays out the terminal opened");
    let key = pane.key().expect("lunetad says the key of a terminal it opened");
    assert_eq!(opened.focused(), Some(&pane));

    let (seen, seeing) = std::sync::mpsc::channel();
    let greeting = Greeting { runs_in: None, foreground: None, background: None };
    let viewer = view(owned.socket(), greeting, move |it| drop(seen.send(it)));
    viewer.show(opened.keys());
    let paste = |text: &str| typed::Event::Paste(mux_api::Paste { text: text.to_owned() });
    viewer.typed(key, paste("echo seen-$((40 + 2))"));
    let enter = mux_api::Key {
        press: mux_api::Press::Down.into(),
        modifiers: Some(mux_api::Modifiers::default()),
        key: Some(mux_api::key::Key::Named(mux_api::Named::Enter.into())),
        text: String::new(),
    };
    viewer.typed(key, typed::Event::Key(enter));
    let mut lines = Vec::new();
    let deadline = std::time::Instant::now() + 3 * HEARD_IN;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let Ok(seen) = seeing.recv_timeout(left) else {
            panic!("the terminal never showed seen-42: {:?}", text_of(&lines));
        };
        let framed = match seen {
            Seen::Frame(framed) => framed,
            Seen::Refused(why) | Seen::Lost(why) => panic!("the terminal is not seen: {why}"),
        };
        let frame = framed.frame();
        assert_eq!(frame.terminal(), key, "the mux shows only the terminal asked for");
        assert_eq!((frame.size().columns.get(), frame.size().rows.get()), (78, 22));
        match frame.rows() {
            Rows::Whole(whole) => lines.clone_from(whole),
            Rows::Changed(rows) => {
                for (row, line) in rows {
                    lines[usize::from(*row)] = line.clone();
                }
            },
        }
        if text_of(&lines).iter().any(|line| line.ends_with("seen-42")) {
            break;
        }
    }
}

#[test]
fn a_viewer_reads_no_more_frames_than_luneta_has_taken() {
    let owned = Owned::new();
    let workstream = WorkstreamId::of(workstream_in_home(&owned)).unwrap();
    let folder = folder_in(owned.host.runtime.path()).unwrap();
    let (heard, hearing) = std::sync::mpsc::channel();
    let size = Size::of(80, 24).unwrap();
    let show = lunetad::show(folder, workstream, size, None, move |it| drop(heard.send(it)));
    let laid = || match hearing.recv_timeout(HEARD_IN).expect("lunetad lays the view out") {
        Heard::Laid(laid) => laid,
        Heard::Refused(why) | Heard::Forgotten(why) | Heard::Lost(why) => {
            panic!("the view is not laid out: {why}")
        },
    };
    assert!(laid().is_empty());
    let ticking = ["sh", "-c", "while :; do date +%N; sleep 0.005; done"];
    let argv = ticking.iter().map(|word| (*word).to_owned()).collect();
    show.ask(Asked::Open { beside: Beside::Largest, argv });
    let opened = laid();

    let (seen, seeing) = std::sync::mpsc::channel();
    let greeting = Greeting { runs_in: None, foreground: None, background: None };
    let viewer = view(owned.socket(), greeting, move |it| drop(seen.send(it)));
    viewer.show(opened.keys());
    let frame = |within: Duration| match seeing.recv_timeout(within) {
        Ok(Seen::Frame(framed)) => Some(framed),
        Ok(Seen::Refused(why) | Seen::Lost(why)) => panic!("the terminal is not seen: {why}"),
        Err(_) => None,
    };
    let mut held = Vec::new();
    for told in 0..FRAMES_MAX {
        let framed = frame(HEARD_IN);
        held.push(framed.unwrap_or_else(|| panic!("frame {told} of {FRAMES_MAX} came")));
    }
    let still = Duration::from_millis(500);
    assert!(frame(still).is_none(), "no frame comes past {FRAMES_MAX} that luneta still holds");
    drop(held.pop());
    let again = frame(HEARD_IN);
    assert!(again.is_some(), "a frame comes once luneta lets one go");
    assert!(frame(still).is_none(), "only one frame comes for the one let go");
}

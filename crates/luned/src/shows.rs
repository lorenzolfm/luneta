use std::collections::BTreeSet;
use std::iter;
use std::num::NonZeroU64;
use std::pin::pin;

use lune_domain::mux::TerminalKey;
use lune_domain::projects::{Arranged, BrowserUrl, Opens, Size, ViewId, WorktreeId};
use lune_domain::projects::{FRAME_CELLS, LAYOUT_VIEWS_MAX};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Status};

use crate::api::{self, show_request, show_response};
use crate::folder::Folder;
use crate::remote::Destination;
use crate::streams::{self, Dropped, Inbox, Next, Outbox};
use crate::tasks;

const SAID_MAX: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AskNumber(NonZeroU64);

impl AskNumber {
    pub const HELLO: AskNumber = AskNumber(NonZeroU64::new(1).expect("hello ask number is 1"));

    #[must_use]
    pub const fn of(number: u64) -> Option<AskNumber> {
        match NonZeroU64::new(number) {
            Some(number) => Some(AskNumber(number)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    #[must_use]
    pub const fn wire(number: Option<AskNumber>) -> u64 {
        match number {
            Some(number) => number.get(),
            None => 0,
        }
    }

    fn next(self) -> AskNumber {
        let next = self.0.checked_add(1).expect("asks fit in u64");
        assert!(
            next.get() > Self::HELLO.get(),
            "awaited asks count up past hello"
        );
        AskNumber(next)
    }
}

const _: () = assert!(AskNumber::HELLO.get() == 1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Echoed {
    Unnumbered,
    Through(AskNumber),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerEcho {
    Unheard,
    Unnumbered,
    Numbered(AskNumber),
}

impl PeerEcho {
    fn hear(&mut self, wire: u64, sent_max: AskNumber) -> Result<Echoed, String> {
        match (*self, AskNumber::of(wire)) {
            (PeerEcho::Unheard, None) => {
                *self = PeerEcho::Unnumbered;
                Ok(Echoed::Unnumbered)
            }
            (PeerEcho::Unheard, Some(number)) if number <= sent_max => {
                *self = PeerEcho::Numbered(number);
                Ok(Echoed::Through(number))
            }
            (PeerEcho::Unheard, Some(number)) => Err(format!(
                "luned echoed heard {} past {}",
                number.get(),
                sent_max.get()
            )),
            (PeerEcho::Unnumbered, None) => Ok(Echoed::Unnumbered),
            (PeerEcho::Unnumbered, Some(number)) => Err(format!(
                "luned echoed heard {} after unnumbered response",
                number.get()
            )),
            (PeerEcho::Numbered(last), None) => {
                Err(format!("luned echoed heard 0 after {}", last.get()))
            }
            (PeerEcho::Numbered(last), Some(number)) if number < last => Err(format!(
                "luned echoed heard {} after {}",
                number.get(),
                last.get()
            )),
            (PeerEcho::Numbered(_), Some(number)) if number > sent_max => Err(format!(
                "luned echoed heard {} past {}",
                number.get(),
                sent_max.get()
            )),
            (PeerEcho::Numbered(_), Some(number)) => {
                *self = PeerEcho::Numbered(number);
                Ok(Echoed::Through(number))
            }
        }
    }
}

pub use lune_domain::projects::layout::{Direction, Growth, Rect};

fn right(rect: Rect) -> u32 {
    u32::from(rect.column) + u32::from(rect.columns)
}

fn bottom(rect: Rect) -> u32 {
    u32::from(rect.row) + u32::from(rect.rows)
}

fn overlaps(one: Rect, other: Rect) -> bool {
    u32::from(one.column) < right(other)
        && u32::from(other.column) < right(one)
        && u32::from(one.row) < bottom(other)
        && u32::from(other.row) < bottom(one)
}

fn cells(rect: Rect) -> u64 {
    u64::from(rect.columns) * u64::from(rect.rows)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    Key(TerminalKey),
    Lost,
    Looking,
    Welcome,
    Browser { url: Option<BrowserUrl> },
    Review,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub id: ViewId,
    pub rect: Rect,
    pub found: Found,
}

impl View {
    #[must_use]
    pub fn key(&self) -> Option<TerminalKey> {
        match &self.found {
            Found::Key(key) => Some(*key),
            Found::Lost
            | Found::Looking
            | Found::Welcome
            | Found::Browser { .. }
            | Found::Review => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shows {
    Tiled,
    Full,
    Tabs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Showing {
    before: Vec<View>,
    focused: View,
    after: Vec<View>,
    shows: Shows,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    worktree: WorktreeId,
    size: Size,
    shown: Option<Showing>,
    version: Option<api::Version>,
}

impl Laid {
    #[must_use]
    pub fn worktree(&self) -> WorktreeId {
        self.worktree
    }

    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    #[must_use]
    pub fn version(&self) -> Option<api::Version> {
        self.version
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shown.is_none()
    }

    pub fn views(&self) -> impl Iterator<Item = &View> {
        self.shown.iter().flat_map(|shown| {
            iter::once(&shown.focused)
                .chain(&shown.before)
                .chain(&shown.after)
        })
    }

    pub fn in_order(&self) -> impl Iterator<Item = &View> {
        self.shown.iter().flat_map(|shown| {
            shown
                .before
                .iter()
                .chain(iter::once(&shown.focused))
                .chain(&shown.after)
        })
    }

    #[must_use]
    pub fn focused(&self) -> Option<&View> {
        self.shown.as_ref().map(|shown| &shown.focused)
    }

    #[must_use]
    pub fn shows(&self) -> Option<Shows> {
        self.shown.as_ref().map(|shown| shown.shows)
    }

    #[must_use]
    pub fn focused_at(&self) -> Option<(&View, Rect)> {
        let shown = self.shown.as_ref()?;
        let rect = match shown.shows {
            Shows::Full | Shows::Tabs => self.whole(),
            Shows::Tiled => shown.focused.rect,
        };
        Some((&shown.focused, rect))
    }

    #[must_use]
    pub fn drawn(&self) -> Vec<(&View, Rect)> {
        match self.shows() {
            Some(Shows::Full | Shows::Tabs) => self.focused_at().into_iter().collect(),
            Some(Shows::Tiled) | None => self.views().map(|view| (view, view.rect)).collect(),
        }
    }

    fn whole(&self) -> Rect {
        Rect::whole(self.size)
    }

    #[must_use]
    pub fn keys(&self) -> BTreeSet<TerminalKey> {
        self.views().filter_map(View::key).collect()
    }

    pub fn heard(laid: show_response::Laid) -> Result<Laid, String> {
        use show_response::laid::Shown as Said;
        let worktree = WorktreeId::of(laid.worktree).ok_or("a layout of worktree 0")?;
        let size = laid.size.ok_or("a layout of no size")?;
        let size = Size::of(size.columns, size.rows).map_err(|why| format!("a layout's {why}"))?;
        if laid.views.len() > LAYOUT_VIEWS_MAX {
            let count = laid.views.len();
            return Err(format!(
                "a layout of {count} views, over {LAYOUT_VIEWS_MAX}"
            ));
        }
        let version = laid.version;
        let mut before = laid
            .views
            .into_iter()
            .map(heard_view)
            .collect::<Result<Vec<View>, String>>()?;
        let (focused, shows) = match laid.shown {
            None if before.is_empty() => {
                return Ok(Laid {
                    worktree,
                    size,
                    shown: None,
                    version,
                });
            }
            None => return Err("a layout of terminals focuses none".to_owned()),
            Some(Said::Tiled(focused)) => (focused, Shows::Tiled),
            Some(Said::Full(_)) if before.len() == 1 => {
                return Err("a layout of one terminal fills itself with it".to_owned());
            }
            Some(Said::Full(full)) => (full, Shows::Full),
            Some(Said::Tabbed(focused)) => (focused, Shows::Tabs),
        };
        tiles(&before, size, shows)?;
        let at = before.iter().position(|view| view.id.get() == focused);
        let unheld = || format!("a layout focuses terminal {focused:016x}, which it lays out not");
        let at = at.ok_or_else(unheld)?;
        let after = before.split_off(at + 1);
        let focused = before
            .pop()
            .expect("the focused view is the last before those after it");
        Ok(Laid {
            worktree,
            size,
            shown: Some(Showing {
                before,
                focused,
                after,
                shows,
            }),
            version,
        })
    }
}

fn heard_view(view: show_response::View) -> Result<View, String> {
    use show_response::view::Found as Said;
    let id = ViewId::of(view.id).ok_or("a layout lays out view 0")?;
    let rect = view.rect.ok_or_else(|| format!("view {id} has no place"))?;
    let side = |cells: u32| u16::try_from(cells).ok();
    let sides = (
        side(rect.column),
        side(rect.row),
        side(rect.columns),
        side(rect.rows),
    );
    let (Some(column), Some(row), Some(columns), Some(rows)) = sides else {
        return Err(format!("view {id} is placed off any screen"));
    };
    let rect = Rect {
        column,
        row,
        columns,
        rows,
    };
    if columns <= 2 * FRAME_CELLS || rows <= 2 * FRAME_CELLS {
        return Err(format!("view {id} has no room inside its frame"));
    }
    let found = match view
        .found
        .ok_or_else(|| format!("view {id} is found nowhere"))?
    {
        Said::Key(key) => Found::Key(TerminalKey::of(&key)?),
        Said::Lost(()) => Found::Lost,
        Said::Looking(()) => Found::Looking,
        Said::Welcome(()) => Found::Welcome,
        Said::Browser(browser) => {
            let url = browser
                .url
                .as_deref()
                .map(BrowserUrl::parse)
                .transpose()
                .map_err(|why| format!("view {id}: {why}"))?;
            Found::Browser { url }
        }
        Said::Review(()) => Found::Review,
    };
    Ok(View { id, rect, found })
}

fn tiles(views: &[View], size: Size, shows: Shows) -> Result<(), String> {
    let (columns, rows) = size.get();
    let tabbed = shows == Shows::Tabs;
    for (at, one) in views.iter().enumerate() {
        if right(one.rect) > u32::from(columns) || bottom(one.rect) > u32::from(rows) {
            return Err(format!("view {} is placed off its layout", one.id));
        }
        if tabbed && one.rect != Rect::whole(size) {
            return Err(format!(
                "view {} is a tab that fills not its layout",
                one.id
            ));
        }
        for other in &views[at + 1..] {
            if one.id == other.id {
                return Err(format!("a layout lays out view {} twice", one.id));
            }
            if let Some(key) = one.key()
                && other.key() == Some(key)
            {
                let (one, other, id) = (one.id, other.id, key.id);
                return Err(format!(
                    "terminals {one} and {other} share lune-mux's terminal {id}"
                ));
            }
            if !tabbed && overlaps(one.rect, other.rect) {
                return Err(format!("view {} is laid over {}", other.id, one.id));
            }
        }
    }
    if tabbed {
        return Ok(());
    }
    let covered: u64 = views.iter().map(|view| cells(view.rect)).sum();
    let whole = u64::from(columns) * u64::from(rows);
    if !views.is_empty() && covered != whole {
        return Err(format!(
            "a layout's terminals cover {covered} of its {whole} cells"
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Laid {
        laid: Laid,
        heard: Echoed,
    },
    Edged {
        worktree: WorktreeId,
        direction: Direction,
        step: NonZeroU64,
        heard: Echoed,
    },
    Sent(WorktreeId),
    Left(WorktreeId),
    Refused {
        why: String,
        heard: Echoed,
    },
    Forgotten(String),
    Lost(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beside {
    Alone,
    Largest,
    Focused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutAsked {
    Size(Size),
    Navigate {
        view: ViewId,
        url: Option<BrowserUrl>,
    },
    BrowserTty {
        view: ViewId,
        tty: Option<String>,
    },
    Resize(Growth),
    Fullscreen,
    Arrange(Arranged),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FocusAsked {
    Go(WorktreeId),
    Open {
        beside: Beside,
        opens: Opens,
    },
    Welcome,
    Browser(Option<BrowserUrl>),
    Review,
    Close(ViewId),
    Focus(ViewId),
    Step {
        direction: Direction,
        step: NonZeroU64,
    },
}

enum Outgoing {
    Layout(LayoutAsked),
    Focus(AskNumber, FocusAsked),
}

impl Outgoing {
    fn said(self) -> api::ShowRequest {
        use show_request::Event;
        let (asked_number, event) = match self {
            Outgoing::Layout(asked) => {
                let event = match asked {
                    LayoutAsked::Size(size) => Event::Size(size_said(size)),
                    LayoutAsked::Navigate { view, url } => {
                        Event::Navigate(show_request::Navigate {
                            view: view.get(),
                            url: url.map(|url| url.as_str().to_owned()),
                        })
                    }
                    LayoutAsked::BrowserTty { view, tty } => {
                        Event::BrowserTty(show_request::BrowserTty {
                            view: view.get(),
                            tty,
                        })
                    }
                    LayoutAsked::Resize(growth) => {
                        let sides = Some(show_request::resize::Sides::Every(()));
                        let growth = match growth {
                            Growth::Grow => show_request::Growth::Grow,
                            Growth::Shrink => show_request::Growth::Shrink,
                        };
                        Event::Resize(show_request::Resize {
                            sides,
                            growth: growth.into(),
                        })
                    }
                    LayoutAsked::Fullscreen => Event::Fullscreen(()),
                    LayoutAsked::Arrange(arranged) => Event::Arrange(arranged.to_api().into()),
                };
                (None, event)
            }
            Outgoing::Focus(asked_number, asked) => {
                let event = match asked {
                    FocusAsked::Go(worktree) => Event::Go(worktree.get()),
                    FocusAsked::Open { beside, opens } => {
                        use show_request::open::Beside as Said;
                        let beside = match beside {
                            Beside::Alone => Said::Alone(()),
                            Beside::Largest => Said::Largest(()),
                            Beside::Focused => Said::Focused(()),
                        };
                        let (beside, opens) = (Some(beside), Some(opens.to_api()));
                        Event::Open(show_request::Open { beside, opens })
                    }
                    FocusAsked::Welcome => Event::Welcome(()),
                    FocusAsked::Browser(url) => Event::Browser(show_request::Browser {
                        url: url.map(|url| url.as_str().to_owned()),
                    }),
                    FocusAsked::Review => Event::Review(()),
                    FocusAsked::Close(view) => Event::Close(view.get()),
                    FocusAsked::Focus(view) => {
                        let to = show_request::focus::To::View(view.get());
                        Event::Focus(show_request::Focus {
                            to: Some(to),
                            step: 0,
                        })
                    }
                    FocusAsked::Step { direction, step } => {
                        let to =
                            show_request::focus::To::Direction(direction_said(direction).into());
                        Event::Focus(show_request::Focus {
                            to: Some(to),
                            step: step.get(),
                        })
                    }
                };
                (Some(asked_number), event)
            }
        };
        api::ShowRequest {
            event: Some(event),
            asked: AskNumber::wire(asked_number),
        }
    }
}

fn edged_heard(edged: &show_response::Edged, heard: Echoed) -> Result<Heard, String> {
    let worktree = WorktreeId::of(edged.worktree).ok_or("an edge of worktree 0")?;
    let direction = crate::layouts::direction_of(edged.direction);
    let direction = direction.map_err(|why| format!("an edge: {why}"))?;
    let step = NonZeroU64::new(edged.step).ok_or("an edge of step 0")?;
    Ok(Heard::Edged {
        worktree,
        direction,
        step,
        heard,
    })
}

pub(crate) fn direction_said(direction: Direction) -> show_request::Direction {
    use show_request::Direction as Said;
    match direction {
        Direction::Left => Said::Left,
        Direction::Right => Said::Right,
        Direction::Up => Said::Up,
        Direction::Down => Said::Down,
    }
}

fn size_said(size: Size) -> show_request::Size {
    let (columns, rows) = size.get();
    show_request::Size {
        columns: columns.into(),
        rows: rows.into(),
    }
}

pub struct Show {
    outbox: Outbox<Outgoing>,
    asked_last: AskNumber,
}

impl Show {
    pub fn ask(&self, asked: LayoutAsked) {
        self.outbox.say(Outgoing::Layout(asked));
    }

    pub fn ask_focus(&mut self, asked: FocusAsked) -> AskNumber {
        let next = self.asked_last.next();
        assert!(next > self.asked_last, "awaited asks climb strictly");
        self.asked_last = next;
        self.outbox.say(Outgoing::Focus(next, asked));
        next
    }
}

fn untaken() -> String {
    format!("luned has yet to take the last {SAID_MAX} words sent its layout")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Where {
    Here { runs_in: Option<TerminalKey> },
    On(Destination),
}

impl Where {
    #[must_use]
    pub fn runs_in(&self) -> Option<TerminalKey> {
        match self {
            Where::Here { runs_in } => *runs_in,
            Where::On(_) => None,
        }
    }
}

pub fn show<Told: FnMut(Heard) + Send + 'static>(
    folder: Folder,
    at: Where,
    worktree: WorktreeId,
    size: Size,
    again: bool,
    mut told: Told,
) -> Show {
    let (outbox, inbox) = streams::boxes(SAID_MAX);
    let size = Some(size_said(size));
    let said = |key: TerminalKey| api::TerminalKey {
        id: key.id.get(),
        made_at: Some(key.made.into()),
    };
    let (runs_in, on) = match at {
        Where::Here { runs_in } => (runs_in.map(said), None),
        Where::On(on) => (
            None,
            Some(api::On {
                destination: on.to_string(),
            }),
        ),
    };
    let hello = show_request::Hello {
        worktree: worktree.get(),
        size,
        runs_in,
        again,
        on,
    };
    drop(tasks::shared().spawn(async move {
        let mut told_aside = |heard| tokio::task::block_in_place(|| told(heard));
        if let Ok(last) = stream(&folder, hello, inbox, &mut told_aside).await {
            told_aside(last);
        }
    }));
    Show {
        outbox,
        asked_last: AskNumber::HELLO,
    }
}

async fn stream(
    folder: &Folder,
    hello: show_request::Hello,
    inbox: Inbox<Outgoing>,
    told: &mut impl FnMut(Heard),
) -> Result<Heard, Dropped> {
    let daemon = match crate::reach(folder).await {
        Ok(crate::Reached::Matched(daemon)) => daemon,
        Ok(crate::Reached::Unmatched(unmatched)) => return Ok(Heard::Lost(unmatched.to_string())),
        Err(error) => return Ok(Heard::Lost(format!("luned is not reached: {error}"))),
    };
    let (sending, sent) = mpsc::channel(SAID_MAX);
    let mut sent_max = AskNumber::HELLO;
    let mut peer_echo = PeerEcho::Unheard;
    let first = api::ShowRequest {
        event: Some(show_request::Event::Hello(hello)),
        asked: AskNumber::HELLO.get(),
    };
    let said = tokio_stream::once(first).chain(ReceiverStream::new(sent));
    let hearing = match daemon.show(crate::ANSWER, said).await {
        Ok(hearing) => hearing,
        Err(crate::Unasked::Unsent(unsent)) => return Ok(Heard::Lost(unsent.to_string())),
        Err(crate::Unasked::Asked(status)) => return Ok(ended(&status)),
    };
    let mut next = pin!(inbox.merged(hearing));
    loop {
        let said = match next.next().await {
            Some(Next::Heard(Ok(said))) => said,
            Some(Next::Heard(Err(status))) => return Ok(ended(&status)),
            Some(Next::LetGo) | None => {
                return Ok(Heard::Lost("luned let go of lune's layout".to_owned()));
            }
            Some(Next::Untaken) => return Ok(Heard::Lost(untaken())),
            Some(Next::Dropped) => return Err(Dropped),
            Some(Next::Said(outgoing)) => {
                if let Outgoing::Focus(asked_number, _) = &outgoing {
                    assert!(
                        *asked_number > sent_max,
                        "awaited asks climb strictly on one stream",
                    );
                    sent_max = *asked_number;
                }
                if let Err(TrySendError::Full(_)) = sending.try_send(outgoing.said()) {
                    return Ok(Heard::Lost(untaken()));
                }
                continue;
            }
        };
        let heard = match peer_echo.hear(said.heard, sent_max) {
            Ok(heard) => heard,
            Err(why) => {
                return Ok(Heard::Lost(format!(
                    "luned said what lune cannot follow: {why}"
                )));
            }
        };
        match said.event {
            Some(show_response::Event::Laid(laid)) => match Laid::heard(laid) {
                Ok(laid) => told(Heard::Laid { laid, heard }),
                Err(why) => {
                    return Ok(Heard::Lost(format!(
                        "luned said what lune cannot follow: {why}"
                    )));
                }
            },
            Some(show_response::Event::Edged(edged)) => match edged_heard(&edged, heard) {
                Ok(heard) => told(heard),
                Err(why) => {
                    return Ok(Heard::Lost(format!(
                        "luned said what lune cannot follow: {why}"
                    )));
                }
            },
            Some(show_response::Event::Sent(worktree)) => match WorktreeId::of(worktree) {
                Some(worktree) => told(Heard::Sent(worktree)),
                None => {
                    return Ok(Heard::Lost(
                        "luned sent lune's layout to worktree 0".to_owned(),
                    ));
                }
            },
            Some(show_response::Event::Left(worktree)) => match WorktreeId::of(worktree) {
                Some(worktree) => told(Heard::Left(worktree)),
                None => {
                    return Ok(Heard::Lost(
                        "luned said lune's worktree 0 is gone".to_owned(),
                    ));
                }
            },
            Some(show_response::Event::Refused(why)) => told(Heard::Refused { why, heard }),
            None => {
                return Ok(Heard::Lost(
                    "luned said nothing to lune's layout".to_owned(),
                ));
            }
        }
    }
}

fn ended(status: &Status) -> Heard {
    match status.code() {
        Code::NotFound => Heard::Forgotten(refusal(status)),
        _ => Heard::Lost(refusal(status)),
    }
}

fn refusal(status: &Status) -> String {
    match status.message() {
        "" => status.code().description().to_owned(),
        said => said.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use show_response::laid::Shown as Said;

    use super::*;

    fn placed(terminal: u64, rect: (u32, u32, u32, u32), key: Option<u32>) -> show_response::View {
        use show_response::view::Found as Said;
        let (column, row, columns, rows) = rect;
        let found = match key {
            Some(id) => Said::Key(api::TerminalKey {
                id,
                made_at: Some(prost_types::Timestamp::default()),
            }),
            None => Said::Lost(()),
        };
        show_response::View {
            id: terminal,
            rect: Some(show_response::Rect {
                column,
                row,
                columns,
                rows,
            }),
            found: Some(found),
        }
    }

    fn laid(views: Vec<show_response::View>, shown: Option<Said>) -> show_response::Laid {
        show_response::Laid {
            worktree: 9,
            size: Some(show_request::Size {
                columns: 80,
                rows: 24,
            }),
            views,
            shown,
            version: None,
        }
    }

    fn tiled(terminal: u64) -> Said {
        Said::Tiled(terminal)
    }

    fn full(terminal: u64) -> Said {
        Said::Full(terminal)
    }

    fn halves() -> Vec<show_response::View> {
        vec![
            placed(1, (0, 0, 40, 24), Some(3)),
            placed(2, (40, 0, 40, 24), None),
        ]
    }

    #[test]
    fn a_layout_reads_where_its_terminals_tile_it_and_one_is_focused() {
        let read = Laid::heard(laid(halves(), Some(tiled(2)))).unwrap();
        let terminals: Vec<u64> = read.views().map(|view| view.id.get()).collect();
        assert_eq!(terminals, [2, 1], "the focused terminal first");
        let focused = read.focused().map(|view| (view.id.get(), &view.found));
        assert_eq!(focused, Some((2, &Found::Lost)));
        assert_eq!(read.keys().len(), 1);
        let drawn: Vec<(u64, u16)> = read
            .drawn()
            .iter()
            .map(|(view, rect)| (view.id.get(), rect.columns))
            .collect();
        assert_eq!(
            (read.shows(), drawn),
            (Some(Shows::Tiled), vec![(2, 40), (1, 40)])
        );
        let full = Laid::heard(laid(halves(), Some(full(1)))).unwrap();
        let drawn: Vec<(u64, u16)> = full
            .drawn()
            .iter()
            .map(|(view, rect)| (view.id.get(), rect.columns))
            .collect();
        assert_eq!((full.shows(), drawn), (Some(Shows::Full), vec![(1, 80)]));
        let empty = Laid::heard(laid(Vec::new(), None)).unwrap();
        assert!(empty.is_empty() && empty.focused().is_none() && empty.drawn().is_empty());
    }

    #[test]
    fn tabs_each_fill_the_layout_keep_their_order_and_draw_the_focused_one_alone() {
        let tab = |terminal, key| placed(terminal, (0, 0, 80, 24), key);
        let tabs = vec![tab(1, Some(3)), tab(2, None), tab(5, Some(4))];
        let read = Laid::heard(laid(tabs.clone(), Some(Said::Tabbed(2)))).unwrap();
        let order: Vec<u64> = read.in_order().map(|view| view.id.get()).collect();
        assert_eq!(order, [1, 2, 5], "tabs keep the layout's order");
        let drawn: Vec<(u64, u16)> = read
            .drawn()
            .iter()
            .map(|(view, rect)| (view.id.get(), rect.columns))
            .collect();
        assert_eq!((read.shows(), drawn), (Some(Shows::Tabs), vec![(2, 80)]));
        let lone = Laid::heard(laid(vec![tab(1, Some(3))], Some(Said::Tabbed(1)))).unwrap();
        assert_eq!(lone.in_order().count(), 1, "one view is one tab");
        let unfilled = Laid::heard(laid(halves(), Some(Said::Tabbed(1)))).unwrap_err();
        assert!(unfilled.contains("fills not its layout"), "{unfilled}");
        let twice = vec![tab(1, Some(3)), tab(2, Some(3))];
        let shared = Laid::heard(laid(twice, Some(Said::Tabbed(1)))).unwrap_err();
        assert!(shared.contains("share lune-mux's terminal"), "{shared}");
        let astray = Laid::heard(laid(tabs, Some(Said::Tabbed(9)))).unwrap_err();
        assert!(astray.contains("lays out not"), "{astray}");
    }

    #[test]
    fn an_edge_reads_with_its_worktree_and_a_direction_it_knows() {
        let edged = |worktree, direction, step| show_response::Edged {
            worktree,
            direction,
            step,
        };
        let left = show_request::Direction::Left.into();
        let ask = Echoed::Through(AskNumber::of(3).unwrap());
        let heard = edged_heard(&edged(2, left, 5), ask);
        let (worktree, step) = (WorktreeId::of(2).unwrap(), NonZeroU64::new(5).unwrap());
        let expected = Heard::Edged {
            worktree,
            direction: Direction::Left,
            step,
            heard: ask,
        };
        assert_eq!(heard, Ok(expected));
        assert!(edged_heard(&edged(0, left, 5), ask).is_err(), "worktree 0");
        assert!(edged_heard(&edged(2, 0, 5), ask).is_err(), "no direction");
        assert!(
            edged_heard(&edged(2, 9, 5), ask).is_err(),
            "an unknown direction"
        );
        assert!(edged_heard(&edged(2, left, 0), ask).is_err(), "step 0");
    }

    #[test]
    fn heard_sequences_stay_monotonic_keep_their_mode_and_never_pass_what_was_sent() {
        let one = AskNumber::HELLO;
        let two = one.next();
        assert_eq!(one.get(), 1);
        assert_eq!(two.get(), 2);
        assert_eq!(AskNumber::wire(None), 0);
        assert_eq!(AskNumber::wire(Some(two)), 2);

        let mut legacy = PeerEcho::Unheard;
        assert_eq!(
            legacy.hear(0, one),
            Ok(Echoed::Unnumbered),
            "older daemon sends 0"
        );
        assert_eq!(
            legacy.hear(0, two),
            Ok(Echoed::Unnumbered),
            "older daemon stays unnumbered"
        );
        assert!(
            legacy.hear(2, two).is_err(),
            "unnumbered peer cannot switch to numbered"
        );

        let mut numbered = PeerEcho::Unheard;
        assert!(
            numbered.hear(2, one).is_err(),
            "first heard cannot pass what was sent"
        );
        assert_eq!(numbered.hear(1, one), Ok(Echoed::Through(one)));
        assert_eq!(
            numbered.hear(1, two),
            Ok(Echoed::Through(one)),
            "unchanged heard is valid"
        );
        assert_eq!(numbered.hear(2, two), Ok(Echoed::Through(two)));
        assert!(numbered.hear(1, two).is_err(), "heard cannot go backward");
        assert!(
            numbered.hear(0, two).is_err(),
            "heard cannot drop back to 0"
        );
        assert!(
            numbered.hear(3, two).is_err(),
            "heard cannot pass what was sent"
        );
    }

    #[test]
    fn a_layout_ends_forgotten_only_when_luned_finds_no_worktree() {
        let forgotten = Status::not_found("worktree 9 is forgotten");
        assert_eq!(
            ended(&forgotten),
            Heard::Forgotten("worktree 9 is forgotten".to_owned())
        );
        let gone = Status::unavailable("the layouts have stopped");
        assert_eq!(
            ended(&gone),
            Heard::Lost("the layouts have stopped".to_owned())
        );
        assert_eq!(
            ended(&Status::internal("")),
            Heard::Lost("Internal error".to_owned())
        );
    }

    #[test]
    fn a_layout_that_does_not_tile_or_focus_as_one_does_is_not_read() {
        let gap = vec![
            placed(1, (0, 0, 40, 24), None),
            placed(2, (41, 0, 39, 24), None),
        ];
        let over = vec![
            placed(1, (0, 0, 41, 24), None),
            placed(2, (40, 0, 40, 24), None),
        ];
        let off = vec![
            placed(1, (0, 0, 40, 24), None),
            placed(2, (40, 0, 41, 24), None),
        ];
        let twice = vec![
            placed(1, (0, 0, 40, 24), None),
            placed(1, (40, 0, 40, 24), None),
        ];
        let shared = vec![
            placed(1, (0, 0, 40, 24), Some(3)),
            placed(2, (40, 0, 40, 24), Some(3)),
        ];
        let unframed = vec![
            placed(1, (0, 0, 2, 24), None),
            placed(2, (2, 0, 78, 24), None),
        ];
        let one = vec![placed(1, (0, 0, 80, 24), None)];
        for refused in [
            laid(gap, Some(tiled(1))),
            laid(over, Some(tiled(1))),
            laid(off, Some(tiled(1))),
            laid(twice, Some(tiled(1))),
            laid(shared, Some(tiled(1))),
            laid(unframed, Some(tiled(1))),
            laid(halves(), None),
            laid(halves(), Some(tiled(3))),
            laid(halves(), Some(full(3))),
            laid(one, Some(full(1))),
            laid(Vec::new(), Some(tiled(1))),
            show_response::Laid {
                worktree: 0,
                ..laid(halves(), Some(tiled(1)))
            },
            show_response::Laid {
                size: None,
                ..laid(halves(), Some(tiled(1)))
            },
        ] {
            assert!(Laid::heard(refused.clone()).is_err(), "{refused:?}");
        }
        let mut remade = placed(2, (40, 0, 40, 24), Some(3));
        if let Some(show_response::view::Found::Key(key)) = &mut remade.found {
            key.made_at = Some(prost_types::Timestamp {
                seconds: 1,
                nanos: 0,
            });
        }
        let one_id = vec![placed(1, (0, 0, 40, 24), Some(3)), remade];
        assert!(
            Laid::heard(laid(one_id, Some(tiled(1)))).is_ok(),
            "frames name the whole key"
        );
    }
}

use std::collections::BTreeSet;
use std::iter;
use std::pin::pin;

use tokio::runtime;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;
use tonic::{Code, Status};

use crate::api::{self, show_request, show_response};
use crate::folder::Folder;
use crate::mux::TerminalKey;
use crate::projects::{Size, TerminalId, WorkstreamId, FRAME_CELLS, PANES_MAX};
use crate::streams::{self, Inbox, Next, Outbox};

const SAID_MAX: usize = 64;

pub use crate::projects::layout::Rect;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    Key(TerminalKey),
    Lost,
    Looking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pane {
    pub terminal: TerminalId,
    pub rect: Rect,
    pub found: Found,
}

impl Pane {
    #[must_use]
    pub fn key(&self) -> Option<TerminalKey> {
        match self.found {
            Found::Key(key) => Some(key),
            Found::Lost | Found::Looking => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Showing {
    focused: Pane,
    others: Vec<Pane>,
    full: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    workstream: WorkstreamId,
    size: Size,
    shown: Option<Showing>,
}

impl Laid {
    #[must_use]
    pub fn workstream(&self) -> WorkstreamId {
        self.workstream
    }

    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shown.is_none()
    }

    pub fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.shown.iter().flat_map(|shown| iter::once(&shown.focused).chain(&shown.others))
    }

    #[must_use]
    pub fn focused(&self) -> Option<&Pane> {
        self.shown.as_ref().map(|shown| &shown.focused)
    }

    #[must_use]
    pub fn full(&self) -> bool {
        self.shown.as_ref().is_some_and(|shown| shown.full)
    }

    #[must_use]
    pub fn focused_at(&self) -> Option<(&Pane, Rect)> {
        let shown = self.shown.as_ref()?;
        let rect = if shown.full { self.whole() } else { shown.focused.rect };
        Some((&shown.focused, rect))
    }

    #[must_use]
    pub fn drawn(&self) -> Vec<(&Pane, Rect)> {
        match &self.shown {
            Some(Showing { full: true, .. }) => self.focused_at().into_iter().collect(),
            Some(Showing { full: false, .. }) | None => {
                self.panes().map(|pane| (pane, pane.rect)).collect()
            },
        }
    }

    fn whole(&self) -> Rect {
        Rect::whole(self.size)
    }

    #[must_use]
    pub fn keys(&self) -> BTreeSet<TerminalKey> {
        self.panes().filter_map(Pane::key).collect()
    }

    pub fn heard(laid: show_response::Laid) -> Result<Laid, String> {
        use show_response::laid::Shown as Said;
        let workstream = WorkstreamId::of(laid.workstream).ok_or("a view of workstream 0")?;
        let size = laid.size.ok_or("a view of no size")?;
        let size = Size::of(size.columns, size.rows).map_err(|why| format!("a view's {why}"))?;
        if laid.panes.len() > PANES_MAX {
            return Err(format!("a view of {} terminals, over {PANES_MAX}", laid.panes.len()));
        }
        let mut others = laid.panes.into_iter().map(pane).collect::<Result<Vec<Pane>, String>>()?;
        tiles(&others, size)?;
        let (focused, full) = match laid.shown {
            None if others.is_empty() => return Ok(Laid { workstream, size, shown: None }),
            None => return Err("a view of terminals focuses none".to_owned()),
            Some(Said::Tiled(focused)) => (focused, false),
            Some(Said::Full(_)) if others.len() == 1 => {
                return Err("a view of one terminal fills itself with it".to_owned());
            },
            Some(Said::Full(full)) => (full, true),
        };
        let at = others.iter().position(|pane| pane.terminal.get() == focused);
        let unheld = || format!("a view focuses terminal {focused:016x}, which it lays out not");
        let focused = others.remove(at.ok_or_else(unheld)?);
        Ok(Laid { workstream, size, shown: Some(Showing { focused, others, full }) })
    }
}

fn pane(pane: show_response::Pane) -> Result<Pane, String> {
    use show_response::pane::Found as Said;
    let terminal = TerminalId::of(pane.terminal).ok_or("a view lays out terminal 0")?;
    let rect = pane.rect.ok_or_else(|| format!("terminal {terminal} has no place"))?;
    let side = |cells: u32| u16::try_from(cells).ok();
    let sides = (side(rect.column), side(rect.row), side(rect.columns), side(rect.rows));
    let (Some(column), Some(row), Some(columns), Some(rows)) = sides else {
        return Err(format!("terminal {terminal} is placed off any screen"));
    };
    let rect = Rect { column, row, columns, rows };
    if columns <= 2 * FRAME_CELLS || rows <= 2 * FRAME_CELLS {
        return Err(format!("terminal {terminal} has no room inside its frame"));
    }
    let found = match pane.found.ok_or_else(|| format!("terminal {terminal} is found nowhere"))? {
        Said::Key(key) => Found::Key(TerminalKey::of(&key)?),
        Said::Lost(()) => Found::Lost,
        Said::Looking(()) => Found::Looking,
    };
    Ok(Pane { terminal, rect, found })
}

fn tiles(panes: &[Pane], size: Size) -> Result<(), String> {
    let (columns, rows) = size.get();
    for (at, one) in panes.iter().enumerate() {
        if right(one.rect) > u32::from(columns) || bottom(one.rect) > u32::from(rows) {
            return Err(format!("terminal {} is placed off its view", one.terminal));
        }
        for other in &panes[at + 1..] {
            if one.terminal == other.terminal {
                return Err(format!("a view lays out terminal {} twice", one.terminal));
            }
            if let Some(key) = one.key()
                && other.key() == Some(key)
            {
                let (one, other, id) = (one.terminal, other.terminal, key.id);
                return Err(format!("terminals {one} and {other} share luneta-mux's terminal {id}"));
            }
            if overlaps(one.rect, other.rect) {
                return Err(format!("terminal {} is laid over {}", other.terminal, one.terminal));
            }
        }
    }
    let covered: u64 = panes.iter().map(|pane| cells(pane.rect)).sum();
    let whole = u64::from(columns) * u64::from(rows);
    if !panes.is_empty() && covered != whole {
        return Err(format!("a view's terminals cover {covered} of its {whole} cells"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Laid(Laid),
    Refused(String),
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
pub enum Asked {
    Size(Size),
    Go(WorkstreamId),
    Open { beside: Beside, argv: Vec<String> },
    Close,
    Focus(TerminalId),
    Fullscreen,
}

impl Asked {
    fn said(self) -> api::ShowRequest {
        use show_request::Event;
        let event = match self {
            Asked::Size(size) => Event::Size(size_said(size)),
            Asked::Go(workstream) => Event::Go(workstream.get()),
            Asked::Open { beside, argv } => {
                use show_request::open::Beside as Said;
                let beside = match beside {
                    Beside::Alone => Said::Alone(()),
                    Beside::Largest => Said::Largest(()),
                    Beside::Focused => Said::Focused(()),
                };
                Event::Open(show_request::Open { beside: Some(beside), argv })
            },
            Asked::Close => Event::Close(()),
            Asked::Focus(terminal) => {
                let to = show_request::focus::To::Terminal(terminal.get());
                Event::Focus(show_request::Focus { to: Some(to) })
            },
            Asked::Fullscreen => Event::Fullscreen(()),
        };
        api::ShowRequest { event: Some(event) }
    }
}

fn size_said(size: Size) -> show_request::Size {
    let (columns, rows) = size.get();
    show_request::Size { columns: columns.into(), rows: rows.into() }
}

pub struct Show {
    outbox: Outbox<Asked>,
}

impl Show {
    pub fn ask(&self, asked: Asked) {
        self.outbox.say(asked);
    }

    pub fn give_up(&self, why: String) {
        self.outbox.give_up(why);
    }
}

fn untaken() -> String {
    format!("lunetad has yet to take the last {SAID_MAX} words sent its view")
}

pub fn show<Told: FnMut(Heard) + Send + 'static>(
    folder: Folder,
    workstream: WorkstreamId,
    size: Size,
    runs_in: Option<TerminalKey>,
    told: Told,
) -> Show {
    let (outbox, inbox) = streams::boxes(SAID_MAX, untaken);
    let showing = move |mut told: Told| {
        let built = runtime::Builder::new_current_thread().enable_all().build();
        let size = Some(size_said(size));
        let runs_in = runs_in.map(|key| api::TerminalKey {
            id: key.id.get(),
            made_at: Some(key.made.into()),
        });
        let hello = show_request::Hello { workstream: workstream.get(), size, runs_in };
        let last = match built {
            Ok(runtime) => runtime.block_on(stream(&folder, hello, inbox, &mut told)),
            Err(error) => Heard::Lost(format!("no runtime: {error}")),
        };
        told(last);
    };
    if let Err((mut told, error)) = streams::on_thread("lunetad view", told, showing) {
        told(Heard::Lost(format!("no thread for a view of lunetad's: {error}")));
    }
    Show { outbox }
}

async fn stream(
    folder: &Folder,
    hello: show_request::Hello,
    inbox: Inbox<Asked>,
    told: &mut impl FnMut(Heard),
) -> Heard {
    let mut daemon = match crate::connect(folder).await {
        Ok(daemon) => daemon,
        Err(error) => return Heard::Lost(format!("lunetad is not reached: {error}")),
    };
    let (sending, sent) = mpsc::channel(SAID_MAX);
    let first = api::ShowRequest { event: Some(show_request::Event::Hello(hello)) };
    let said = tokio_stream::once(first).chain(ReceiverStream::new(sent));
    let hearing = match daemon.show(said).await {
        Ok(hearing) => hearing.into_inner(),
        Err(status) => return ended(&status),
    };
    let mut next = pin!(inbox.merged(hearing));
    loop {
        let said = match next.next().await {
            Some(Next::Heard(Ok(said))) => said,
            Some(Next::Heard(Err(status))) => return ended(&status),
            Some(Next::LetGo) | None => {
                return Heard::Lost("lunetad let go of luneta's view".to_owned());
            },
            Some(Next::GaveUp(why)) => return Heard::Lost(why),
            Some(Next::Said(asked)) => {
                if let Err(TrySendError::Full(_)) = sending.try_send(asked.said()) {
                    return Heard::Lost(untaken());
                }
                continue;
            },
        };
        match said.event {
            Some(show_response::Event::Laid(laid)) => match Laid::heard(laid) {
                Ok(laid) => told(Heard::Laid(laid)),
                Err(why) => {
                    return Heard::Lost(format!("lunetad said what luneta cannot follow: {why}"));
                },
            },
            Some(show_response::Event::Refused(why)) => told(Heard::Refused(why)),
            None => return Heard::Lost("lunetad said nothing to luneta's view".to_owned()),
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

    fn placed(terminal: u64, rect: (u32, u32, u32, u32), key: Option<u32>) -> show_response::Pane {
        use show_response::pane::Found as Said;
        let (column, row, columns, rows) = rect;
        let found = match key {
            Some(id) => Said::Key(api::TerminalKey {
                id,
                made_at: Some(prost_types::Timestamp::default()),
            }),
            None => Said::Lost(()),
        };
        show_response::Pane {
            terminal,
            rect: Some(show_response::Rect { column, row, columns, rows }),
            found: Some(found),
        }
    }

    fn laid(panes: Vec<show_response::Pane>, shown: Option<Said>) -> show_response::Laid {
        show_response::Laid {
            workstream: 9,
            size: Some(show_request::Size { columns: 80, rows: 24 }),
            panes,
            shown,
        }
    }

    fn tiled(terminal: u64) -> Said {
        Said::Tiled(terminal)
    }

    fn full(terminal: u64) -> Said {
        Said::Full(terminal)
    }

    fn halves() -> Vec<show_response::Pane> {
        vec![placed(1, (0, 0, 40, 24), Some(3)), placed(2, (40, 0, 40, 24), None)]
    }

    #[test]
    fn a_view_reads_where_its_terminals_tile_it_and_one_is_focused() {
        let read = Laid::heard(laid(halves(), Some(tiled(2)))).unwrap();
        let terminals: Vec<u64> = read.panes().map(|pane| pane.terminal.get()).collect();
        assert_eq!(terminals, [2, 1], "the focused terminal first");
        let focused = read.focused().map(|pane| (pane.terminal.get(), pane.found));
        assert_eq!(focused, Some((2, Found::Lost)));
        assert_eq!(read.keys().len(), 1);
        let drawn: Vec<(u64, u16)> =
            read.drawn().iter().map(|(pane, rect)| (pane.terminal.get(), rect.columns)).collect();
        assert_eq!((read.full(), drawn), (false, vec![(2, 40), (1, 40)]));
        let full = Laid::heard(laid(halves(), Some(full(1)))).unwrap();
        let drawn: Vec<(u64, u16)> =
            full.drawn().iter().map(|(pane, rect)| (pane.terminal.get(), rect.columns)).collect();
        assert_eq!((full.full(), drawn), (true, vec![(1, 80)]));
        let empty = Laid::heard(laid(Vec::new(), None)).unwrap();
        assert!(empty.is_empty() && empty.focused().is_none() && empty.drawn().is_empty());
    }

    #[test]
    fn a_view_ends_forgotten_only_when_lunetad_finds_no_workstream() {
        let forgotten = Status::not_found("workstream 9 is forgotten");
        assert_eq!(ended(&forgotten), Heard::Forgotten("workstream 9 is forgotten".to_owned()));
        let gone = Status::unavailable("the views have stopped");
        assert_eq!(ended(&gone), Heard::Lost("the views have stopped".to_owned()));
        assert_eq!(ended(&Status::internal("")), Heard::Lost("Internal error".to_owned()));
    }

    #[test]
    fn a_view_that_does_not_tile_or_focus_as_one_does_is_not_read() {
        let gap = vec![placed(1, (0, 0, 40, 24), None), placed(2, (41, 0, 39, 24), None)];
        let over = vec![placed(1, (0, 0, 41, 24), None), placed(2, (40, 0, 40, 24), None)];
        let off = vec![placed(1, (0, 0, 40, 24), None), placed(2, (40, 0, 41, 24), None)];
        let twice = vec![placed(1, (0, 0, 40, 24), None), placed(1, (40, 0, 40, 24), None)];
        let shared = vec![placed(1, (0, 0, 40, 24), Some(3)), placed(2, (40, 0, 40, 24), Some(3))];
        let unframed = vec![placed(1, (0, 0, 2, 24), None), placed(2, (2, 0, 78, 24), None)];
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
            show_response::Laid { workstream: 0, ..laid(halves(), Some(tiled(1))) },
            show_response::Laid { size: None, ..laid(halves(), Some(tiled(1))) },
        ] {
            assert!(Laid::heard(refused.clone()).is_err(), "{refused:?}");
        }
        let mut remade = placed(2, (40, 0, 40, 24), Some(3));
        if let Some(show_response::pane::Found::Key(key)) = &mut remade.found {
            key.made_at = Some(prost_types::Timestamp { seconds: 1, nanos: 0 });
        }
        let one_id = vec![placed(1, (0, 0, 40, 24), Some(3)), remade];
        assert!(Laid::heard(laid(one_id, Some(tiled(1)))).is_ok(), "frames name the whole key");
    }
}

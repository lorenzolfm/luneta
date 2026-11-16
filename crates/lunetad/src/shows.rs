use std::collections::BTreeSet;
use std::pin::pin;
use std::thread;

use tokio::runtime;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;
use tonic::Status;

use crate::api::{self, show_request, show_response};
use crate::folder::Folder;
use crate::mux::TerminalKey;
use crate::projects::{Size, TerminalId, WorkstreamId, FRAME_CELLS, PANES_MAX};

const SAID_MAX: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub column: u16,
    pub row: u16,
    pub columns: u16,
    pub rows: u16,
}

impl Rect {
    fn right(self) -> u32 {
        u32::from(self.column) + u32::from(self.columns)
    }

    fn bottom(self) -> u32 {
        u32::from(self.row) + u32::from(self.rows)
    }

    fn overlaps(self, other: Rect) -> bool {
        u32::from(self.column) < other.right()
            && u32::from(other.column) < self.right()
            && u32::from(self.row) < other.bottom()
            && u32::from(other.row) < self.bottom()
    }

    fn cells(self) -> u64 {
        u64::from(self.columns) * u64::from(self.rows)
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Tiled { focused: TerminalId },
    Full(TerminalId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    workstream: WorkstreamId,
    size: Size,
    panes: Vec<Pane>,
    shown: Option<Shown>,
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
    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    #[must_use]
    pub fn shown(&self) -> Option<Shown> {
        self.shown
    }

    #[must_use]
    pub fn focused(&self) -> Option<&Pane> {
        let focused = match self.shown? {
            Shown::Tiled { focused } | Shown::Full(focused) => focused,
        };
        self.panes.iter().find(|pane| pane.terminal == focused)
    }

    #[must_use]
    pub fn whole(&self) -> Rect {
        let (columns, rows) = self.size.get();
        Rect { column: 0, row: 0, columns, rows }
    }

    #[must_use]
    pub fn keys(&self) -> BTreeSet<TerminalKey> {
        self.panes.iter().filter_map(Pane::key).collect()
    }

    pub fn heard(laid: show_response::Laid) -> Result<Laid, String> {
        let workstream = WorkstreamId::of(laid.workstream).ok_or("a view of workstream 0")?;
        let size = laid.size.ok_or("a view of no size")?;
        let size = Size::of(size.columns, size.rows).map_err(|why| format!("a view's {why}"))?;
        if laid.panes.len() > PANES_MAX {
            return Err(format!("a view of {} terminals, over {PANES_MAX}", laid.panes.len()));
        }
        let panes = laid.panes.into_iter().map(pane).collect::<Result<Vec<Pane>, String>>()?;
        let terminal = |id: Option<u64>, what: &str| -> Result<Option<TerminalId>, String> {
            let Some(id) = id else {
                return Ok(None);
            };
            let held = panes.iter().find(|pane| pane.terminal.get() == id);
            let unheld = || format!("a view {what} terminal {id:016x}, which it does not lay out");
            held.map(|pane| Some(pane.terminal)).ok_or_else(unheld)
        };
        let focused = terminal(laid.focused, "focuses")?;
        let full = terminal(laid.full, "fills with")?;
        let shown = match (focused, full) {
            (None, None) if panes.is_empty() => None,
            (None, _) => return Err("a view of terminals focuses none".to_owned()),
            (Some(focused), None) => Some(Shown::Tiled { focused }),
            (Some(focused), Some(full)) if focused == full && panes.len() > 1 => {
                Some(Shown::Full(full))
            },
            (Some(_), Some(_)) if panes.len() == 1 => {
                return Err("a view of one terminal fills itself with it".to_owned());
            },
            (Some(_), Some(_)) => return Err("a full view focuses another terminal".to_owned()),
        };
        tiles(&panes, size)?;
        Ok(Laid { workstream, size, panes, shown })
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
        if one.rect.right() > u32::from(columns) || one.rect.bottom() > u32::from(rows) {
            return Err(format!("terminal {} is placed off its view", one.terminal));
        }
        for other in &panes[at + 1..] {
            if one.terminal == other.terminal {
                return Err(format!("a view lays out terminal {} twice", one.terminal));
            }
            if one.key().is_some() && one.key() == other.key() {
                let (one, other) = (one.terminal, other.terminal);
                return Err(format!("terminals {one} and {other} share a key"));
            }
            if one.rect.overlaps(other.rect) {
                return Err(format!("terminal {} is laid over {}", other.terminal, one.terminal));
            }
        }
    }
    let covered: u64 = panes.iter().map(|pane| pane.rect.cells()).sum();
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
    Lost(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    Size(Size),
    Go(WorkstreamId),
    Open { towards: Option<show_request::Towards>, argv: Vec<String> },
    Close,
    Focus(show_request::focus::To),
    Fullscreen,
}

impl Asked {
    fn said(self) -> api::ShowRequest {
        use show_request::Event;
        let event = match self {
            Asked::Size(size) => Event::Size(size_said(size)),
            Asked::Go(workstream) => Event::Go(workstream.get()),
            Asked::Open { towards, argv } => {
                use show_request::open::Beside;
                let beside = match towards {
                    None => Beside::Largest(()),
                    Some(towards) => Beside::Towards(towards.into()),
                };
                Event::Open(show_request::Open { beside: Some(beside), argv })
            },
            Asked::Close => Event::Close(()),
            Asked::Focus(to) => Event::Focus(show_request::Focus { to: Some(to) }),
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
    said: mpsc::Sender<Asked>,
    giving_up: mpsc::Sender<String>,
}

impl Show {
    pub fn ask(&self, asked: Asked) {
        match self.said.try_send(asked) {
            Ok(()) | Err(TrySendError::Closed(_)) => {},
            Err(TrySendError::Full(_)) => self.give_up(untaken()),
        }
    }

    pub fn give_up(&self, why: String) {
        let _ = self.giving_up.try_send(why);
    }
}

fn untaken() -> String {
    format!("lunetad has yet to take the last {SAID_MAX} words sent its view")
}

pub fn show<Told: FnMut(Heard) + Send + 'static>(
    folder: Folder,
    workstream: WorkstreamId,
    size: Size,
    mut told: Told,
) -> Show {
    let (said, saying) = mpsc::channel(SAID_MAX);
    let (giving_up, given_up) = mpsc::channel(1);
    let (handing, handed) = std::sync::mpsc::sync_channel::<Told>(1);
    let showing = move || {
        let mut told = handed.recv().expect("`show` hands its thread `told` as it starts");
        let built = runtime::Builder::new_current_thread().enable_all().build();
        let size = Some(size_said(size));
        let hello = show_request::Hello { workstream: workstream.get(), size };
        let last = match built {
            Ok(runtime) => runtime.block_on(stream(&folder, hello, saying, given_up, &mut told)),
            Err(error) => format!("no runtime: {error}"),
        };
        told(Heard::Lost(last));
    };
    match thread::Builder::new().name("lunetad view".to_owned()).spawn(showing) {
        Ok(_) => handing.send(told).expect("the thread waits on `told` before all else"),
        Err(error) => told(Heard::Lost(format!("no thread for a view of lunetad's: {error}"))),
    }
    Show { said, giving_up }
}

enum Next {
    Said(Result<api::ShowResponse, Status>),
    LetGo,
    Asked(Asked),
    GaveUp(String),
}

async fn stream(
    folder: &Folder,
    hello: show_request::Hello,
    asked: mpsc::Receiver<Asked>,
    given_up: mpsc::Receiver<String>,
    told: &mut impl FnMut(Heard),
) -> String {
    let mut daemon = match crate::connect(folder).await {
        Ok(daemon) => daemon,
        Err(error) => return format!("lunetad is not reached: {error}"),
    };
    let (sending, sent) = mpsc::channel(SAID_MAX);
    let first = api::ShowRequest { event: Some(show_request::Event::Hello(hello)) };
    let said = tokio_stream::once(first).chain(ReceiverStream::new(sent));
    let hearing = match daemon.show(said).await {
        Ok(hearing) => hearing.into_inner(),
        Err(status) => return refusal(&status),
    };
    let hearing = hearing.map(Next::Said).chain(tokio_stream::once(Next::LetGo));
    let asked = ReceiverStream::new(asked).map(Next::Asked);
    let given_up = ReceiverStream::new(given_up).map(Next::GaveUp);
    let mut next = pin!(hearing.merge(asked).merge(given_up));
    loop {
        let said = match next.next().await {
            Some(Next::Said(Ok(said))) => said,
            Some(Next::Said(Err(status))) => return refusal(&status),
            Some(Next::LetGo) | None => return "lunetad let go of luneta's view".to_owned(),
            Some(Next::GaveUp(why)) => return why,
            Some(Next::Asked(asked)) => {
                if let Err(TrySendError::Full(_)) = sending.try_send(asked.said()) {
                    return untaken();
                }
                continue;
            },
        };
        match said.event {
            Some(show_response::Event::Laid(laid)) => match Laid::heard(laid) {
                Ok(laid) => told(Heard::Laid(laid)),
                Err(why) => return format!("lunetad said what luneta cannot follow: {why}"),
            },
            Some(show_response::Event::Refused(why)) => told(Heard::Refused(why)),
            None => return "lunetad said nothing to luneta's view".to_owned(),
        }
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

    fn laid(
        panes: Vec<show_response::Pane>,
        focused: Option<u64>,
        full: Option<u64>,
    ) -> show_response::Laid {
        show_response::Laid {
            workstream: 9,
            size: Some(show_request::Size { columns: 80, rows: 24 }),
            panes,
            focused,
            full,
        }
    }

    fn halves() -> Vec<show_response::Pane> {
        vec![placed(1, (0, 0, 40, 24), Some(3)), placed(2, (40, 0, 40, 24), None)]
    }

    #[test]
    fn a_view_reads_where_its_terminals_tile_it_and_one_is_focused() {
        let read = Laid::heard(laid(halves(), Some(2), None)).unwrap();
        let terminals: Vec<u64> = read.panes().iter().map(|pane| pane.terminal.get()).collect();
        assert_eq!(terminals, [1, 2]);
        assert_eq!(read.focused().map(|pane| pane.terminal.get()), Some(2));
        assert_eq!(read.keys().len(), 1);
        assert_eq!(read.panes()[1].found, Found::Lost);
        let full = Laid::heard(laid(halves(), Some(1), Some(1))).unwrap();
        assert_eq!(full.shown(), Some(Shown::Full(full.panes()[0].terminal)));
        let empty = Laid::heard(laid(Vec::new(), None, None)).unwrap();
        assert_eq!((empty.panes().len(), empty.shown()), (0, None));
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
            laid(gap, Some(1), None),
            laid(over, Some(1), None),
            laid(off, Some(1), None),
            laid(twice, Some(1), None),
            laid(shared, Some(1), None),
            laid(unframed, Some(1), None),
            laid(halves(), None, None),
            laid(halves(), Some(3), None),
            laid(halves(), Some(1), Some(2)),
            laid(one, Some(1), Some(1)),
            laid(Vec::new(), Some(1), None),
            show_response::Laid { workstream: 0, ..laid(halves(), Some(1), None) },
            show_response::Laid { size: None, ..laid(halves(), Some(1), None) },
        ] {
            assert!(Laid::heard(refused.clone()).is_err(), "{refused:?}");
        }
    }
}

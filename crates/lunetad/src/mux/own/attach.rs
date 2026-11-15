use std::num::{NonZeroU16, NonZeroU32};
use std::pin::pin;
use std::thread;

use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;
use tonic::transport::Channel;
use tonic::Status;

use super::api::{self, mux_client::MuxClient, to_client, to_server};
use super::server::{connect, refused, runtime, socket};
use super::{OwnClientId, OwnId, OwnKey, OwnMade, OwnPane};

pub const SIDE_MAX: u16 = 1000;

const SAID_MAX: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum To {
    Session(OwnKey),
    Tab { session: OwnKey, tab: NonZeroU32 },
    Pane(OwnPane),
}

impl To {
    #[must_use]
    pub fn session(self) -> OwnKey {
        match self {
            To::Session(session) | To::Tab { session, .. } => session,
            To::Pane(pane) => pane.session,
        }
    }

    #[must_use]
    pub fn reached(self, went: &Went) -> bool {
        match self {
            To::Session(session) => went.pane.session == session,
            To::Tab { session, tab } => went.pane.session == session && went.tab == tab,
            To::Pane(pane) => went.pane == pane,
        }
    }

    fn said(self) -> api::Place {
        let is = match self {
            To::Session(session) => api::place::Is::Session(session.id.get()),
            To::Tab { tab, .. } => api::place::Is::Tab(tab.get()),
            To::Pane(pane) => api::place::Is::Pane(pane.id.get()),
        };
        api::Place { is: Some(is), made_at: Some(self.session().made.into()) }
    }

    fn found_in(self, sessions: &[OwnKey], listing: &[api::Session]) -> Result<(), String> {
        let session = self.session();
        if !sessions.contains(&session) {
            return Err(format!("own session {} no longer runs", session.id));
        }
        let listed = listing.iter().find(|listed| listed.id == session.id.get());
        let tabs = listed.map_or(&[][..], |listed| &listed.tabs);
        let has_pane =
            |pane: NonZeroU32| tabs.iter().flat_map(|tab| &tab.panes).any(|it| it.id == pane.get());
        match self {
            To::Session(_) => Ok(()),
            To::Tab { tab, .. } if tabs.iter().any(|it| it.id == tab.get()) => Ok(()),
            To::Tab { tab, .. } => Err(format!("own session {} has no tab {tab}", session.id)),
            To::Pane(pane) if has_pane(pane.id) => Ok(()),
            To::Pane(pane) => Err(format!("own session {} has no pane {}", session.id, pane.id)),
        }
    }
}

fn keys(listing: &[api::Session]) -> Vec<OwnKey> {
    let key = |session: &api::Session| {
        let id = OwnId::new(session.id)?;
        let made = OwnMade::read(session.made_at?).ok()?;
        Some(OwnKey { id, made })
    };
    listing.iter().filter_map(key).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub columns: NonZeroU16,
    pub rows: NonZeroU16,
}

impl Size {
    #[must_use]
    pub fn within(columns: NonZeroU16, rows: NonZeroU16) -> Size {
        let max = NonZeroU16::new(SIDE_MAX).expect("the most is not 0");
        Size { columns: columns.min(max), rows: rows.min(max) }
    }

    #[must_use]
    pub fn said(self) -> api::Size {
        api::Size { columns: self.columns.get().into(), rows: self.rows.get().into() }
    }

    fn heard(what: &str, size: Option<api::Size>) -> Result<Size, String> {
        let size = size.ok_or_else(|| format!("{what} has no size"))?;
        let cells = |name: &str, count: u32| {
            let cells = u16::try_from(count).ok().filter(|cells| *cells <= SIDE_MAX);
            let cells = cells.and_then(NonZeroU16::new);
            cells.ok_or_else(|| format!("{what}'s {name} are {count}"))
        };
        Ok(Size { columns: cells("columns", size.columns)?, rows: cells("rows", size.rows)? })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Hello {
    pub to: To,
    pub size: Size,
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
}

impl Hello {
    fn said(self) -> api::ToServer {
        let rgb = |[red, green, blue]: [u8; 3]| api::Rgb {
            red: red.into(),
            green: green.into(),
            blue: blue.into(),
        };
        let colours = api::Colours {
            foreground: self.foreground.map(rgb),
            background: self.background.map(rgb),
        };
        let hello = api::Hello {
            to: Some(self.to.said()),
            size: Some(self.size.said()),
            colours: Some(colours),
        };
        api::ToServer { event: Some(to_server::Event::Hello(hello)) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Went {
    pub pane: OwnPane,
    pub tab: NonZeroU32,
    pub client: OwnClientId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grapheme(String);

impl Grapheme {
    pub fn new(grapheme: String) -> Result<Grapheme, String> {
        if grapheme.is_empty() {
            return Err("a glyph has no grapheme".to_owned());
        }
        if grapheme.chars().any(char::is_control) {
            return Err(format!("the glyph {grapheme:?} has a control character"));
        }
        Ok(Grapheme(grapheme))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shows {
    Blank,
    Narrow(Grapheme),
    Wide(Grapheme),
}

impl Shows {
    #[must_use]
    pub fn columns(&self) -> u16 {
        match self {
            Shows::Blank | Shows::Narrow(_) => 1,
            Shows::Wide(_) => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub shows: Shows,
    pub style: Style,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Palette(u8),
    Rgb([u8; 3]),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an attribute a terminal sets apart from the rest, in any combination"
)]
pub struct Style {
    pub foreground: Option<Colour>,
    pub background: Option<Colour>,
    pub underline_colour: Option<Colour>,
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub underlined: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    glyphs: Vec<Glyph>,
    wraps_wide: bool,
}

impl Line {
    #[must_use]
    pub fn glyphs(&self) -> &[Glyph] {
        &self.glyphs
    }

    #[must_use]
    pub fn wraps_wide(&self) -> bool {
        self.wraps_wide
    }

    pub fn new(glyphs: Vec<Glyph>, wraps_wide: bool, columns: NonZeroU16) -> Result<Line, String> {
        let line = Line { glyphs, wraps_wide };
        let taken = line.columns();
        if taken != u32::from(columns.get()) {
            return Err(format!("a row of {columns} columns has glyphs of {taken}"));
        }
        Ok(line)
    }

    fn columns(&self) -> u32 {
        let taken: u32 = self.glyphs.iter().map(|glyph| u32::from(glyph.shows.columns())).sum();
        taken + u32::from(self.wraps_wide)
    }

    fn heard(line: Option<api::Line>, columns: NonZeroU16) -> Result<Line, String> {
        let line = line.ok_or("a row of a frame has no line")?;
        let glyphs = line.glyphs.into_iter().map(glyph).collect::<Result<Vec<_>, _>>()?;
        Line::new(glyphs, line.wraps_wide, columns)
    }
}

fn glyph(glyph: api::Glyph) -> Result<Glyph, String> {
    let shows = match glyph.shows.ok_or("a glyph shows nothing")? {
        api::glyph::Shows::Blank(api::Blank {}) => Shows::Blank,
        api::glyph::Shows::Narrow(api::Narrow { grapheme }) => {
            Shows::Narrow(Grapheme::new(grapheme)?)
        },
        api::glyph::Shows::Wide(api::Wide { grapheme }) => Shows::Wide(Grapheme::new(grapheme)?),
    };
    Ok(Glyph { shows, style: style_heard(glyph.style)? })
}

fn style_heard(style: Option<api::Style>) -> Result<Style, String> {
    let Some(style) = style else {
        return Ok(Style::default());
    };
    let underlined = !matches!(
        api::Underline::try_from(style.underline),
        Ok(api::Underline::None | api::Underline::Unspecified) | Err(_)
    );
    Ok(Style {
        foreground: colour_heard(style.foreground)?,
        background: colour_heard(style.background)?,
        underline_colour: colour_heard(style.underline_colour)?,
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        underlined,
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
    })
}

fn colour_heard(colour: Option<api::Colour>) -> Result<Option<Colour>, String> {
    let Some(colour) = colour else {
        return Ok(None);
    };
    let byte = |value: u32| u8::try_from(value).ok();
    let read = match colour.is.as_ref().ok_or("a colour is none")? {
        api::colour::Is::Palette(index) => byte(*index).map(Colour::Palette),
        api::colour::Is::Rgb(rgb) => match (byte(rgb.red), byte(rgb.green), byte(rgb.blue)) {
            (Some(red), Some(green), Some(blue)) => Some(Colour::Rgb([red, green, blue])),
            _ => None,
        },
    };
    let none = || format!("the colour {colour:?} is of none a terminal has");
    read.map(Some).ok_or_else(none)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rows {
    Whole(Vec<Line>),
    Changed(Vec<(u16, Line)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub column: u16,
    pub shape: CursorShape,
    pub blinking: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pane: NonZeroU32,
    size: Size,
    rows: Rows,
    cursor: Option<Cursor>,
    tracks_mouse: bool,
}

impl Frame {
    pub fn new(
        pane: NonZeroU32,
        size: Size,
        rows: Rows,
        cursor: Option<Cursor>,
        tracks_mouse: bool,
    ) -> Result<Frame, String> {
        let columns = u32::from(size.columns.get());
        let lines: Vec<&Line> = match &rows {
            Rows::Whole(lines) => {
                if lines.len() != usize::from(size.rows.get()) {
                    return Err(format!("a frame of {} rows has {}", size.rows, lines.len()));
                }
                lines.iter().collect()
            },
            Rows::Changed(rows) => {
                let mut changed = vec![false; usize::from(size.rows.get())];
                for (index, _) in rows {
                    let Some(seen) = changed.get_mut(usize::from(*index)) else {
                        return Err(format!("a frame has no row {index}"));
                    };
                    if std::mem::replace(seen, true) {
                        return Err(format!("a frame changes row {index} twice"));
                    }
                }
                rows.iter().map(|(_, line)| line).collect()
            },
        };
        if let Some(line) = lines.into_iter().find(|line| line.columns() != columns) {
            let taken = line.columns();
            return Err(format!("a row of {columns} columns has glyphs of {taken}"));
        }
        if let Some(cursor) = cursor
            && (cursor.row >= size.rows.get() || cursor.column >= size.columns.get())
        {
            let at = format!("{}:{}", cursor.row, cursor.column);
            return Err(format!("the cursor is off the frame, at {at}"));
        }
        Ok(Frame { pane, size, rows, cursor, tracks_mouse })
    }

    #[must_use]
    pub fn pane(&self) -> NonZeroU32 {
        self.pane
    }

    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    #[must_use]
    pub fn rows(&self) -> &Rows {
        &self.rows
    }

    #[must_use]
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    #[must_use]
    pub fn tracks_mouse(&self) -> bool {
        self.tracks_mouse
    }

    fn heard(frame: api::Frame) -> Result<Frame, String> {
        let pane = NonZeroU32::new(frame.pane).ok_or("a frame is of pane 0")?;
        let size = Size::heard("a frame", frame.size)?;
        let rows = match frame.rows.ok_or("a frame has no rows")? {
            api::frame::Rows::Whole(api::Whole { lines }) => {
                if lines.len() != usize::from(size.rows.get()) {
                    return Err(format!("a frame of {} rows has {}", size.rows, lines.len()));
                }
                let lines = lines.into_iter().map(|line| Line::heard(Some(line), size.columns));
                Rows::Whole(lines.collect::<Result<_, _>>()?)
            },
            api::frame::Rows::Changed(api::Changed { rows }) => {
                let row = |row: api::Row| {
                    let index = u16::try_from(row.index).ok().filter(|it| *it < size.rows.get());
                    let index = index.ok_or_else(|| format!("a frame has no row {}", row.index))?;
                    Ok((index, Line::heard(row.line, size.columns)?))
                };
                Rows::Changed(rows.into_iter().map(row).collect::<Result<_, String>>()?)
            },
        };
        let cursor = frame.cursor.map(|cursor| cursor_heard(&cursor)).transpose()?;
        Frame::new(pane, size, rows, cursor, frame.mouse_tracking)
    }
}

fn cursor_heard(cursor: &api::Cursor) -> Result<Cursor, String> {
    let off = || format!("the cursor is off the frame, at {}:{}", cursor.row, cursor.column);
    let row = u16::try_from(cursor.row).map_err(|_| off())?;
    let column = u16::try_from(cursor.column).map_err(|_| off())?;
    let shape = match api::CursorShape::try_from(cursor.shape) {
        Ok(api::CursorShape::Bar) => CursorShape::Bar,
        Ok(api::CursorShape::Underline) => CursorShape::Underline,
        Ok(api::CursorShape::BlockHollow) => CursorShape::BlockHollow,
        Ok(api::CursorShape::Block | api::CursorShape::Unspecified) | Err(_) => CursorShape::Block,
    };
    Ok(Cursor { row, column, shape, blinking: cursor.blinking })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub column: u16,
    pub row: u16,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub pane: NonZeroU32,
    pub rect: Rect,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tabbed {
    pub id: NonZeroU32,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    tab: NonZeroU32,
    size: Size,
    shown: Shown,
    tabs: Vec<Tabbed>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Tiled(Tiled),
    Full(Full),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tiled {
    panes: Vec<Placed>,
    focused: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Full {
    pane: Placed,
    hidden: Vec<NonZeroU32>,
}

impl Layout {
    #[must_use]
    pub fn tab(&self) -> NonZeroU32 {
        self.tab
    }

    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    #[must_use]
    pub fn shown(&self) -> &Shown {
        &self.shown
    }

    #[must_use]
    pub fn tabs(&self) -> &[Tabbed] {
        &self.tabs
    }

    #[must_use]
    pub fn holds(&self, pane: NonZeroU32) -> bool {
        let shows = self.shown.panes().iter().any(|placed| placed.pane == pane);
        shows || matches!(&self.shown, Shown::Full(full) if full.hidden.contains(&pane))
    }

    pub fn heard(layout: api::Layout) -> Result<Layout, String> {
        let size = Size::heard("a layout", layout.size)?;
        let tab = NonZeroU32::new(layout.tab).ok_or("a layout is of tab 0")?;
        let (columns, rows) = (size.columns.get(), size.rows.get());
        let placed = |placed: api::Placed| -> Result<Placed, String> {
            let pane = NonZeroU32::new(placed.pane).ok_or("a layout places pane 0")?;
            let rect = placed.rect.ok_or_else(|| format!("pane {pane} is laid nowhere"))?;
            let off = || format!("pane {pane} is laid off the tab of {columns}x{rows}");
            let cell = |count: u32| u16::try_from(count).map_err(|_| off());
            let rect = Rect {
                column: cell(rect.column)?,
                row: cell(rect.row)?,
                columns: cell(rect.columns)?,
                rows: cell(rect.rows)?,
            };
            if rect.columns == 0 || rect.rows == 0 {
                return Err(format!("pane {pane} is laid on no cell"));
            }
            if rect.right() > u32::from(columns) || rect.bottom() > u32::from(rows) {
                return Err(off());
            }
            Ok(Placed { pane, rect, title: placed.title })
        };
        let shown = match layout.shown.ok_or("a layout shows nothing")? {
            api::layout::Shown::Tiled(api::Tiled { panes, focused }) => {
                let panes = panes.into_iter().map(placed).collect::<Result<Vec<_>, _>>()?;
                let wanted = NonZeroU32::new(focused).ok_or("a layout focuses pane 0")?;
                let focused = panes.iter().position(|placed| placed.pane == wanted);
                let focused = focused.ok_or_else(|| {
                    format!("a layout focuses pane {wanted}, which it does not lay out")
                })?;
                Shown::Tiled(Tiled { panes, focused })
            },
            api::layout::Shown::Full(api::Full { pane, hidden }) => {
                let pane = placed(pane.ok_or("a full layout places no pane")?)?;
                let hidden = hidden_heard(pane.pane, hidden, columns, rows)?;
                Shown::Full(Full { pane, hidden })
            },
        };
        tiles(shown.panes(), columns, rows)?;
        let tabbed = |tabbed: api::Tabbed| {
            let id = NonZeroU32::new(tabbed.id).ok_or("a layout names tab 0")?;
            Ok::<_, String>(Tabbed { id, name: tabbed.name })
        };
        let tabs = layout.tabs.into_iter().map(tabbed).collect::<Result<Vec<_>, _>>()?;
        if !tabs.iter().any(|tabbed| tabbed.id == tab) {
            return Err(format!("a layout of tab {tab} does not name it"));
        }
        Ok(Layout { tab, size, shown, tabs })
    }
}

impl Shown {
    #[must_use]
    pub fn panes(&self) -> &[Placed] {
        match self {
            Shown::Tiled(tiled) => &tiled.panes,
            Shown::Full(full) => std::slice::from_ref(&full.pane),
        }
    }

    #[must_use]
    pub fn focused(&self) -> &Placed {
        match self {
            Shown::Tiled(tiled) => &tiled.panes[tiled.focused],
            Shown::Full(full) => &full.pane,
        }
    }
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
}

fn hidden_heard(
    full: NonZeroU32,
    hidden: Vec<u32>,
    columns: u16,
    rows: u16,
) -> Result<Vec<NonZeroU32>, String> {
    let cells = u64::from(columns) * u64::from(rows);
    let count = u64::try_from(hidden.len()).unwrap_or(u64::MAX);
    if count >= cells {
        return Err(format!("a layout hides {count} panes on {cells} cells"));
    }
    let mut read: Vec<NonZeroU32> = Vec::with_capacity(hidden.len());
    for pane in hidden {
        let pane = NonZeroU32::new(pane).ok_or("a layout hides pane 0")?;
        if pane == full {
            return Err(format!("a layout hides pane {pane}, which fills its tab"));
        }
        read.push(pane);
    }
    let mut sorted = read.clone();
    sorted.sort_unstable();
    let twice = sorted.windows(2).find_map(|pair| match pair {
        [one, other] if one == other => Some(*one),
        _ => None,
    });
    if let Some(pane) = twice {
        return Err(format!("a layout hides pane {pane} twice"));
    }
    Ok(read)
}

fn tiles(panes: &[Placed], columns: u16, rows: u16) -> Result<(), String> {
    for (place, one) in panes.iter().enumerate() {
        for other in &panes[place + 1..] {
            if one.pane == other.pane {
                return Err(format!("a layout lays pane {} out twice", one.pane));
            }
            if one.rect.overlaps(other.rect) {
                return Err(format!("a layout lays pane {} over pane {}", other.pane, one.pane));
            }
        }
    }
    let cells = |columns: u16, rows: u16| u64::from(columns) * u64::from(rows);
    let covered = panes.iter().map(|placed| cells(placed.rect.columns, placed.rect.rows));
    let covered: u64 = covered.sum();
    let whole = cells(columns, rows);
    if covered != whole {
        return Err(format!(
            "a layout's panes cover {covered} of the {whole} cells of a tab of {columns}x{rows}"
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Went(Went),
    Frame(Frame),
    Layout(Layout),
    Stayed(String),
    Refused(String),
    Ended,
    Lost(String),
}

#[derive(Debug, PartialEq, Eq)]
enum Read {
    TakenIn(OwnClientId),
    Heard(Heard),
}

fn heard(
    said: api::ToClient,
    sessions: &[OwnKey],
    taken: Option<OwnClientId>,
) -> Result<Read, String> {
    let event = said.event.ok_or("the server said nothing")?;
    Ok(Read::Heard(match (taken, event) {
        (None, to_client::Event::Attached(api::Attached { client })) => {
            let client = OwnClientId::new(client).ok_or("the server named its client 0")?;
            return Ok(Read::TakenIn(client));
        },
        (Some(_), to_client::Event::Attached(_)) => {
            return Err("the server took the client in twice".to_owned());
        },
        (None, _) => return Err("the server spoke to the client before it took it in".to_owned()),
        (Some(client), to_client::Event::Went(api::Went { session, tab, pane })) => {
            let id = OwnId::new(session).ok_or("the client went to session 0")?;
            let session = sessions.iter().find(|key| key.id == id);
            let unlisted = || format!("the client went to session {id}, which it does not list");
            let session = *session.ok_or_else(unlisted)?;
            let tab = NonZeroU32::new(tab).ok_or("the client went to tab 0")?;
            let pane = OwnPane::new(session, pane).ok_or("the client went to pane 0")?;
            Heard::Went(Went { pane, tab, client })
        },
        (Some(_), to_client::Event::Frame(frame)) => Heard::Frame(Frame::heard(frame)?),
        (Some(_), to_client::Event::Ended(api::Ended {})) => Heard::Ended,
        (Some(_), to_client::Event::Stayed(api::Stayed { reason })) => Heard::Stayed(reason),
        (Some(_), to_client::Event::Layout(layout)) => Heard::Layout(Layout::heard(layout)?),
        (Some(_), to_client::Event::Refused(api::Refused { reason })) => Heard::Refused(reason),
    }))
}

enum Saying {
    Event(to_server::Event),
    Go(To),
}

pub struct Attach {
    said: mpsc::Sender<Saying>,
    giving_up: mpsc::Sender<String>,
}

impl Attach {
    pub fn send(&self, event: to_server::Event) {
        self.say(Saying::Event(event));
    }

    pub fn go(&self, to: To) {
        self.say(Saying::Go(to));
    }

    fn say(&self, saying: Saying) {
        match self.said.try_send(saying) {
            Ok(()) | Err(TrySendError::Closed(_)) => {},
            Err(TrySendError::Full(_)) => self.give_up(untaken()),
        }
    }

    pub fn give_up(&self, why: String) {
        let _ = self.giving_up.try_send(why);
    }
}

pub fn attach<Told: FnMut(Heard) + Send + 'static>(hello: Hello, mut told: Told) -> Attach {
    let (said, saying) = mpsc::channel(SAID_MAX);
    let (giving_up, given_up) = mpsc::channel(1);
    let (handing, handed) = std::sync::mpsc::sync_channel::<Told>(1);
    let attaching = move || {
        let mut told = handed.recv().expect("`attach` hands its thread `told` as it starts");
        let last = match runtime() {
            Ok(runtime) => runtime.block_on(stream(hello, saying, given_up, &mut told)),
            Err(failure) => Heard::Lost(failure.to_string()),
        };
        told(last);
    };
    match thread::Builder::new().name("luneta-mux client".to_owned()).spawn(attaching) {
        Ok(_) => handing.send(told).expect("the thread waits on `told` before all else"),
        Err(error) => told(Heard::Lost(format!("no thread for a client of luneta-mux: {error}"))),
    }
    Attach { said, giving_up }
}

fn untaken() -> String {
    format!("luneta-mux has yet to take the last {SAID_MAX} events sent it")
}

enum Next {
    Said(Result<api::ToClient, Status>),
    LetGo,
    Saying(Saying),
    Unsaid,
    GaveUp(String),
}

async fn stream(
    hello: Hello,
    saying: mpsc::Receiver<Saying>,
    given_up: mpsc::Receiver<String>,
    told: &mut impl FnMut(Heard),
) -> Heard {
    let socket = match socket() {
        Ok(socket) => socket,
        Err(failure) => return Heard::Lost(failure.to_string()),
    };
    let mut client = match connect(&socket, None).await {
        Ok(Some(client)) => client,
        Ok(None) => return Heard::Lost("luneta-mux does not run, nor any own session".to_owned()),
        Err(error) => return Heard::Lost(format!("{}: {error}", socket.display())),
    };
    let mut sessions = match listed(&mut client).await {
        Ok((sessions, listing)) => match hello.to.found_in(&sessions, &listing) {
            Ok(()) => sessions,
            Err(why) => return Heard::Lost(why),
        },
        Err(lost) => return lost,
    };
    let (sending, sent) = mpsc::channel(SAID_MAX);
    let mut sending = Some(sending);
    let said = tokio_stream::once(hello.said()).chain(ReceiverStream::new(sent));
    let hearing = match client.attach(said).await {
        Ok(hearing) => hearing.into_inner(),
        Err(status) => return Heard::Lost(refused(&status).to_string()),
    };
    let hearing = hearing.map(Next::Said).chain(tokio_stream::once(Next::LetGo));
    let saying = ReceiverStream::new(saying).map(Next::Saying);
    let saying = saying.chain(tokio_stream::once(Next::Unsaid));
    let given_up = ReceiverStream::new(given_up).map(Next::GaveUp);
    let mut next = pin!(hearing.merge(saying).merge(given_up));
    let mut taken = None;
    loop {
        let said = match next.next().await {
            Some(Next::Said(Ok(said))) => said,
            Some(Next::Said(Err(status))) => return Heard::Lost(refused(&status).to_string()),
            Some(Next::LetGo) | None => {
                return Heard::Lost("luneta-mux let go of luneta's client".to_owned());
            },
            Some(Next::GaveUp(why)) => return Heard::Lost(why),
            Some(Next::Unsaid) => {
                sending = None;
                continue;
            },
            Some(Next::Saying(saying)) => {
                let event = match saying {
                    Saying::Event(event) => event,
                    Saying::Go(to) => {
                        let listing = match listed(&mut client).await {
                            Ok((listed, listing)) => {
                                sessions = listed;
                                listing
                            },
                            Err(lost) => return lost,
                        };
                        if let Err(why) = to.found_in(&sessions, &listing) {
                            told(Heard::Stayed(why));
                            continue;
                        }
                        to_server::Event::Go(to.said())
                    },
                };
                let said = api::ToServer { event: Some(event) };
                if let Some(Err(TrySendError::Full(_))) =
                    sending.as_ref().map(|sending| sending.try_send(said))
                {
                    return Heard::Lost(untaken());
                }
                continue;
            },
        };
        if let Some(to_client::Event::Went(went)) = &said.event
            && !sessions.iter().any(|key| key.id.get() == went.session)
        {
            sessions = match listed(&mut client).await {
                Ok((sessions, _)) => sessions,
                Err(lost) => return lost,
            };
        }
        match heard(said, &sessions, taken) {
            Ok(Read::TakenIn(client)) => taken = Some(client),
            Ok(Read::Heard(Heard::Ended)) => return Heard::Ended,
            Ok(Read::Heard(heard)) => told(heard),
            Err(why) => {
                return Heard::Lost(format!("luneta-mux said what luneta cannot read: {why}"));
            },
        }
    }
}

async fn listed(
    client: &mut MuxClient<Channel>,
) -> Result<(Vec<OwnKey>, Vec<api::Session>), Heard> {
    match client.list(api::ListRequest {}).await {
        Ok(listing) => {
            let listing = listing.into_inner().sessions;
            Ok((keys(&listing), listing))
        },
        Err(status) => Err(Heard::Lost(refused(&status).to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn narrow(grapheme: &str) -> api::Glyph {
        let shows = api::glyph::Shows::Narrow(api::Narrow { grapheme: grapheme.to_owned() });
        api::Glyph { shows: Some(shows), style: None }
    }

    fn wide(grapheme: &str) -> api::Glyph {
        let shows = api::glyph::Shows::Wide(api::Wide { grapheme: grapheme.to_owned() });
        api::Glyph { shows: Some(shows), style: None }
    }

    fn frame(columns: u32, lines: Vec<api::Line>) -> api::Frame {
        let rows = u32::try_from(lines.len()).unwrap();
        api::Frame {
            pane: 2,
            size: Some(api::Size { columns, rows }),
            rows: Some(api::frame::Rows::Whole(api::Whole { lines })),
            cursor: None,
            ..api::Frame::default()
        }
    }

    #[test]
    fn a_frame_reads_where_its_rows_add_up_to_its_size() {
        let line = |glyphs: Vec<api::Glyph>, wraps_wide| api::Line { glyphs, wraps_wide };
        let read = Frame::heard(frame(
            3,
            vec![
                line(vec![narrow("a"), wide("日")], false),
                line(vec![narrow("a"), narrow("b")], true),
            ],
        ))
        .unwrap();
        let Rows::Whole(lines) = &read.rows else { panic!("whole rows") };
        let two = Shows::Wide(Grapheme::new("日".to_owned()).unwrap());
        assert_eq!(lines[0].glyphs()[1].shows, two);
        assert!(lines[1].wraps_wide());
        let short = Frame::heard(frame(3, vec![line(vec![narrow("a")], false)]));
        assert_eq!(short.unwrap_err(), "a row of 3 columns has glyphs of 1");
        let long = Frame::heard(frame(2, vec![line(vec![narrow("a"), wide("日")], false)]));
        assert_eq!(long.unwrap_err(), "a row of 2 columns has glyphs of 3");
    }

    #[test]
    fn a_frame_that_does_not_fit_its_size_does_not_read() {
        let row = api::Line { glyphs: vec![narrow("a")], wraps_wide: false };
        let mut two = frame(1, vec![row.clone(), row.clone()]);
        two.size = Some(api::Size { columns: 1, rows: 1 });
        assert_eq!(Frame::heard(two).unwrap_err(), "a frame of 1 rows has 2");
        let nobody = api::Frame { pane: 0, ..frame(1, vec![row.clone()]) };
        assert_eq!(Frame::heard(nobody).unwrap_err(), "a frame is of pane 0");
        let changed = api::Changed { rows: vec![api::Row { index: 1, line: Some(row.clone()) }] };
        let past = api::Frame {
            rows: Some(api::frame::Rows::Changed(changed)),
            ..frame(1, vec![row.clone()])
        };
        assert_eq!(Frame::heard(past).unwrap_err(), "a frame has no row 1");
        let twice = |index| api::Row { index, line: Some(row.clone()) };
        let changed = api::Changed { rows: vec![twice(0), twice(0)] };
        let again = api::Frame {
            rows: Some(api::frame::Rows::Changed(changed)),
            ..frame(1, vec![row.clone()])
        };
        assert_eq!(Frame::heard(again).unwrap_err(), "a frame changes row 0 twice");
        let cursor = Some(api::Cursor { row: 0, column: 1, ..api::Cursor::default() });
        let off = api::Frame { cursor, ..frame(1, vec![row.clone()]) };
        assert_eq!(Frame::heard(off).unwrap_err(), "the cursor is off the frame, at 0:1");
        let none =
            api::Frame { size: Some(api::Size { columns: 0, rows: 1 }), ..frame(1, vec![row]) };
        assert_eq!(Frame::heard(none).unwrap_err(), "a frame's columns are 0");
        let escape = api::Line { glyphs: vec![narrow("\u{1b}")], wraps_wide: false };
        assert!(Frame::heard(frame(1, vec![escape])).unwrap_err().contains("control character"));
    }

    #[test]
    fn a_glyph_of_a_colour_no_terminal_has_does_not_read() {
        let coloured = |is| {
            let foreground = Some(api::Colour { is });
            let style = Some(api::Style { foreground, ..api::Style::default() });
            glyph(api::Glyph { style, ..narrow("a") })
        };
        let foreground = |glyph: Result<Glyph, String>| glyph.unwrap().style.foreground;
        let rgb = |red| Some(api::colour::Is::Rgb(api::Rgb { red, green: 0, blue: 255 }));
        let palette = foreground(coloured(Some(api::colour::Is::Palette(255))));
        assert_eq!(palette, Some(Colour::Palette(255)));
        assert_eq!(foreground(coloured(rgb(255))), Some(Colour::Rgb([255, 0, 255])));
        assert!(coloured(Some(api::colour::Is::Palette(256))).unwrap_err().contains("none a"));
        assert!(coloured(rgb(256)).unwrap_err().contains("is of none a terminal has"));
        assert_eq!(coloured(None).unwrap_err(), "a colour is none");
        let plain = glyph(api::Glyph { style: Some(api::Style::default()), ..narrow("a") });
        assert_eq!(plain, glyph(narrow("a")));
        assert_eq!(glyph(narrow("a")).unwrap().style, Style::default());
    }

    #[test]
    fn a_frame_is_made_only_of_its_size() {
        let size = Size { columns: NonZeroU16::new(2).unwrap(), rows: NonZeroU16::MIN };
        let line = |columns: u16| {
            let glyph = Glyph { shows: Shows::Blank, style: Style::default() };
            let glyphs = vec![glyph; usize::from(columns)];
            Line::new(glyphs, false, NonZeroU16::new(columns).unwrap()).unwrap()
        };
        let two = line(2);
        let cursor = |column| {
            Some(Cursor { row: 0, column, shape: CursorShape::Bar, blinking: false })
        };
        let made = |rows, cursor| Frame::new(NonZeroU32::MIN, size, rows, cursor, false);
        assert!(made(Rows::Whole(vec![two.clone()]), cursor(1)).is_ok());
        let none = made(Rows::Whole(Vec::new()), None);
        assert_eq!(none.unwrap_err(), "a frame of 1 rows has 0");
        let past = made(Rows::Changed(vec![(1, two.clone())]), None);
        assert_eq!(past.unwrap_err(), "a frame has no row 1");
        let twice = made(Rows::Changed(vec![(0, two.clone()), (0, two.clone())]), None);
        assert_eq!(twice.unwrap_err(), "a frame changes row 0 twice");
        let off = made(Rows::Whole(vec![two]), cursor(2));
        assert_eq!(off.unwrap_err(), "the cursor is off the frame, at 0:2");
        let narrow = made(Rows::Whole(vec![line(1)]), None);
        assert_eq!(narrow.unwrap_err(), "a row of 2 columns has glyphs of 1");
        assert!(Grapheme::new("\u{1b}[2J".to_owned()).unwrap_err().contains("control character"));
        assert_eq!(Grapheme::new(String::new()).unwrap_err(), "a glyph has no grapheme");
    }

    fn key(id: u64, made: u64) -> OwnKey {
        OwnKey { id: OwnId::new(id).unwrap(), made: OwnMade::from_nanos(made * 1_000_000_000) }
    }

    #[test]
    fn a_went_names_a_listed_session_a_tab_and_a_pane() {
        let sessions = [key(3, 20)];
        let client = OwnClientId::new(7).unwrap();
        let went = |session, tab, pane| {
            let event = to_client::Event::Went(api::Went { session, tab, pane });
            heard(api::ToClient { event: Some(event) }, &sessions, Some(client))
        };
        let pane = OwnPane::new(key(3, 20), 4).unwrap();
        let tab = NonZeroU32::MIN;
        assert_eq!(went(3, 1, 4), Ok(Read::Heard(Heard::Went(Went { pane, tab, client }))));
        assert_eq!(went(0, 1, 4).unwrap_err(), "the client went to session 0");
        let unlisted = "the client went to session 5, which it does not list";
        assert_eq!(went(5, 1, 4).unwrap_err(), unlisted);
        assert_eq!(went(3, 0, 4).unwrap_err(), "the client went to tab 0");
        assert_eq!(went(3, 1, 0).unwrap_err(), "the client went to pane 0");
    }

    #[test]
    fn a_client_is_taken_in_first_and_once() {
        let sessions = [key(3, 20)];
        let said = |event, taken| heard(api::ToClient { event: Some(event) }, &sessions, taken);
        let attached = |client| to_client::Event::Attached(api::Attached { client });
        let client = OwnClientId::new(7).unwrap();
        assert_eq!(said(attached(7), None), Ok(Read::TakenIn(client)));
        assert_eq!(said(attached(0), None).unwrap_err(), "the server named its client 0");
        let twice = "the server took the client in twice";
        assert_eq!(said(attached(7), Some(client)).unwrap_err(), twice);
        let went = || to_client::Event::Went(api::Went { session: 3, tab: 1, pane: 4 });
        let early = "the server spoke to the client before it took it in";
        assert_eq!(said(went(), None).unwrap_err(), early);
        let ended = to_client::Event::Ended(api::Ended {});
        assert_eq!(said(ended, None).unwrap_err(), early);
        let Ok(Read::Heard(Heard::Went(went))) = said(went(), Some(client)) else {
            panic!("a went after the client was taken in reads");
        };
        assert_eq!(went.client, client);
    }

    #[test]
    fn a_client_goes_only_to_a_session_its_server_holds() {
        let pane = api::Pane { id: 3, ..api::Pane::default() };
        let tab = |id| api::Tab { id, panes: vec![pane.clone()], ..api::Tab::default() };
        let listed = |id, made: u64, tabs| api::Session {
            id,
            tabs,
            made_at: Some(OwnMade::from_nanos(made * 1_000_000_000).into()),
            ..api::Session::default()
        };
        let listing = [listed(1, 30, vec![tab(7)]), listed(0, 10, Vec::new())];
        let sessions = keys(&listing);
        assert_eq!(sessions, [key(1, 30)]);
        assert_eq!(To::Session(key(1, 30)).found_in(&sessions, &listing), Ok(()));
        let since = To::Session(key(1, 10)).found_in(&sessions, &listing);
        assert_eq!(since.unwrap_err(), "own session 1 no longer runs");
        let tab = |tab| To::Tab { session: key(1, 30), tab: NonZeroU32::new(tab).unwrap() };
        assert_eq!(tab(7).found_in(&sessions, &listing), Ok(()));
        assert_eq!(tab(8).found_in(&sessions, &listing).unwrap_err(), "own session 1 has no tab 8");
        let pane = |pane| To::Pane(OwnPane::new(key(1, 30), pane).unwrap());
        assert_eq!(pane(3).found_in(&sessions, &listing), Ok(()));
        let none = pane(4).found_in(&sessions, &listing).unwrap_err();
        assert_eq!(none, "own session 1 has no pane 4");
    }

    #[test]
    fn a_layout_reads_where_it_lays_its_panes_on_its_tab() {
        let rect = |column, columns| Some(api::Rect { column, row: 0, columns, rows: 24 });
        let placed = |pane, rect| api::Placed { pane, rect, title: "fish".to_owned() };
        let tabbed = |id| api::Tabbed { id, name: format!("Tab #{id}") };
        let shown = |shown, tabs| api::Layout {
            session: 3,
            tab: 1,
            size: Some(api::Size { columns: 80, rows: 24 }),
            shown: Some(shown),
            tabs,
        };
        let tiled = |panes, focused| api::layout::Shown::Tiled(api::Tiled { panes, focused });
        let layout = |panes, focused, tabs| shown(tiled(panes, focused), tabs);
        let two = || vec![placed(4, rect(0, 40)), placed(5, rect(40, 40))];
        let read = Layout::heard(layout(two(), 5, vec![tabbed(1), tabbed(2)])).unwrap();
        assert_eq!(read.shown().focused().pane.get(), 5);
        let second = Rect { column: 40, row: 0, columns: 40, rows: 24 };
        assert_eq!(read.shown().panes()[1].rect, second);
        assert_eq!(read.tabs().len(), 2);
        let over = vec![placed(4, rect(41, 40))];
        let over = Layout::heard(layout(over, 4, vec![tabbed(1)])).unwrap_err();
        assert_eq!(over, "pane 4 is laid off the tab of 80x24");
        let away = Layout::heard(layout(two(), 6, vec![tabbed(1)])).unwrap_err();
        assert_eq!(away, "a layout focuses pane 6, which it does not lay out");
        let unnamed = Layout::heard(layout(two(), 4, vec![tabbed(2)])).unwrap_err();
        assert_eq!(unnamed, "a layout of tab 1 does not name it");
        let nowhere = Layout::heard(layout(vec![placed(4, None)], 4, vec![tabbed(1)]));
        assert_eq!(nowhere.unwrap_err(), "pane 4 is laid nowhere");
        let twice = vec![placed(4, rect(0, 40)), placed(4, rect(40, 40))];
        let twice = Layout::heard(layout(twice, 4, vec![tabbed(1)])).unwrap_err();
        assert_eq!(twice, "a layout lays pane 4 out twice");
        let half = Layout::heard(layout(vec![placed(4, rect(0, 40))], 4, vec![tabbed(1)]));
        let uncovered = "a layout's panes cover 960 of the 1920 cells of a tab of 80x24";
        assert_eq!(half.unwrap_err(), uncovered);
        let top = |pane| placed(pane, Some(api::Rect { column: 0, row: 0, columns: 80, rows: 12 }));
        let stacked = Layout::heard(layout(vec![top(4), top(5)], 4, vec![tabbed(1)]));
        assert_eq!(stacked.unwrap_err(), "a layout lays pane 5 over pane 4");
        let thin = vec![placed(4, rect(0, 80)), placed(5, rect(80, 0))];
        let thin = Layout::heard(layout(thin, 4, vec![tabbed(1)])).unwrap_err();
        assert_eq!(thin, "pane 5 is laid on no cell");
        let full = |pane, hidden| api::layout::Shown::Full(api::Full { pane: Some(pane), hidden });
        let hides = |hidden| shown(full(placed(5, rect(0, 80)), hidden), vec![tabbed(1)]);
        let alone = Layout::heard(hides(vec![])).unwrap();
        assert_eq!(alone.shown().focused().pane.get(), 5);
        assert!(matches!(alone.shown(), Shown::Full(_)));
        let hiding = Layout::heard(hides(vec![4, 6])).unwrap();
        let (four, six) = (NonZeroU32::new(4).unwrap(), NonZeroU32::new(6).unwrap());
        let Shown::Full(hid) = hiding.shown() else { panic!("a full layout reads as tiled") };
        assert_eq!(hid.hidden, [four, six]);
        assert!(hiding.holds(four) && hiding.holds(NonZeroU32::new(5).unwrap()));
        assert!(!hiding.holds(NonZeroU32::new(7).unwrap()));
        assert_eq!(Layout::heard(hides(vec![0])).unwrap_err(), "a layout hides pane 0");
        let under = Layout::heard(hides(vec![5])).unwrap_err();
        assert_eq!(under, "a layout hides pane 5, which fills its tab");
        let twice = Layout::heard(hides(vec![4, 6, 4])).unwrap_err();
        assert_eq!(twice, "a layout hides pane 4 twice");
        let many = Layout::heard(hides((1..=1921).filter(|it| *it != 5).collect())).unwrap_err();
        assert_eq!(many, "a layout hides 1920 panes on 1920 cells");
        let part = shown(full(placed(5, rect(0, 40)), vec![]), vec![tabbed(1)]);
        assert_eq!(Layout::heard(part).unwrap_err(), uncovered);
        let unplaced = shown(api::layout::Shown::Full(api::Full::default()), vec![tabbed(1)]);
        assert_eq!(Layout::heard(unplaced).unwrap_err(), "a full layout places no pane");
        let empty = api::Layout { shown: None, ..layout(two(), 4, vec![tabbed(1)]) };
        assert_eq!(Layout::heard(empty).unwrap_err(), "a layout shows nothing");
    }

    #[test]
    fn a_size_is_held_to_what_the_server_takes() {
        let cells = |count| NonZeroU16::new(count).unwrap();
        let size = Size::within(cells(4000), cells(30));
        assert_eq!((size.columns.get(), size.rows.get()), (SIDE_MAX, 30));
    }
}

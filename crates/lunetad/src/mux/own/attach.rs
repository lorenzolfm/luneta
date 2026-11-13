//! A client of an own session, as luneta attaches to one: on a stream of its
//! own to the server, both ways, which says where the client is and what the
//! pane it is on shows, and takes what you do there. A thread of its own
//! waits on the stream, and hands on what the server says as it comes.
//!
//! What the server says is read here, at the edge: a frame whose rows do not
//! add up to its size, a colour no terminal has, or a word that names no
//! session or pane, ends the client, as a server that says them is not one
//! luneta can follow. The client hears one last word, however it ends.
//!
//! A client goes to an own session as no other is, by its id and when its
//! server made it, as the next server counts its ids from 1 again: it asks the
//! server it reached for its sessions first, on the one connection it keeps,
//! and goes only where that server holds the session it names, as it comes
//! and on each go after.

use std::num::NonZeroU16;
use std::pin::pin;
use std::thread;
use std::time::SystemTime;

use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;
use tonic::transport::Channel;
use tonic::Status;

use super::api::{self, mux_client::MuxClient, to_client, to_server};
use super::server::{connect, refused, runtime, socket};
use super::{OwnId, OwnKey, OwnPane};

/// The most the server lets a side of a pane be, in cells: more is refused.
pub const SIDE_MAX: u16 = 1000;

/// How many events a client holds for the server at once, while the server
/// has yet to take them: a key repeat or the mouse moving sends hundreds a
/// second, and the server takes each at once.
const SAID_MAX: usize = 1024;

/// Where a client goes among the own sessions: to a session as no other is,
/// and to a tab of it by the id its server gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum To {
    /// A session, on the tab a client coming there goes to.
    Session(OwnKey),
    /// A tab of a session.
    Tab { session: OwnKey, tab: u32 },
}

impl To {
    /// The session it goes to.
    #[must_use]
    pub fn session(self) -> OwnKey {
        match self {
            To::Session(session) | To::Tab { session, .. } => session,
        }
    }

    /// Whether `went` is there.
    #[must_use]
    pub fn reached(self, went: &Went) -> bool {
        match self {
            To::Session(session) => went.pane.session == session,
            To::Tab { session, tab } => went.pane.session == session && went.tab == tab,
        }
    }

    /// It, by the ids alone, as the server takes it: the server it goes to
    /// is the one `found_in` found it held by.
    fn said(self) -> api::Place {
        let is = match self {
            To::Session(session) => api::place::Is::Session(session.id.get()),
            To::Tab { tab, .. } => api::place::Is::Tab(tab),
        };
        api::Place { is: Some(is) }
    }

    /// Why `sessions`, as a server lists them, do not hold where it goes, if
    /// they do not: no session of its id, or one of its id that this server
    /// made at another time, or none of its tab.
    fn found_in(self, sessions: &[OwnKey], listing: &[api::Session]) -> Result<(), String> {
        let session = self.session();
        if !sessions.contains(&session) {
            return Err(format!("own session {} no longer runs", session.id));
        }
        let To::Tab { tab, .. } = self else {
            return Ok(());
        };
        let listed = listing.iter().find(|listed| listed.id == session.id.get());
        if listed.is_some_and(|listed| listed.tabs.iter().any(|it| it.id == tab)) {
            Ok(())
        } else {
            Err(format!("own session {} has no tab {tab}", session.id))
        }
    }
}

/// The sessions of `listing`, as no other is: one whose id is 0, or that says
/// no time it was made, is left out, as none can go there.
fn keys(listing: &[api::Session]) -> Vec<OwnKey> {
    let key = |session: &api::Session| {
        let id = OwnId::new(session.id)?;
        let made = SystemTime::try_from(session.made_at?).ok()?;
        Some(OwnKey { id, made })
    };
    listing.iter().filter_map(key).collect()
}

/// A size in cells, of 1 to `SIDE_MAX` each way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub columns: NonZeroU16,
    pub rows: NonZeroU16,
}

impl Size {
    /// `columns` by `rows`, each made `SIDE_MAX` where it is more.
    #[must_use]
    pub fn within(columns: NonZeroU16, rows: NonZeroU16) -> Size {
        let max = NonZeroU16::new(SIDE_MAX).expect("the most is not 0");
        Size { columns: columns.min(max), rows: rows.min(max) }
    }

    #[must_use]
    pub fn said(self) -> api::Size {
        api::Size { columns: self.columns.get().into(), rows: self.rows.get().into() }
    }

    /// `size`, or why it does not read: a side of 0, or past `SIDE_MAX`.
    fn heard(size: Option<api::Size>) -> Result<Size, String> {
        let size = size.ok_or("a frame has no size")?;
        let cells = |name: &str, count: u32| {
            let cells = u16::try_from(count).ok().filter(|cells| *cells <= SIDE_MAX);
            let cells = cells.and_then(NonZeroU16::new);
            cells.ok_or_else(|| format!("a frame's {name} are {count}"))
        };
        Ok(Size { columns: cells("columns", size.columns)?, rows: cells("rows", size.rows)? })
    }
}

/// What a client says first: where it goes, how big it is, and the colours
/// of its terminal, which the panes it goes to answer their programs with,
/// where it knows them.
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

/// Where the client is now, as the server says each time it moves: on a pane,
/// in a session as no other is, by the server the client keeps to, and on
/// the pane's tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Went {
    pub pane: OwnPane,
    pub tab: u32,
}

/// A grapheme a glyph shows: never none, and with no control character in
/// it, which would write more than itself to a terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grapheme(String);

impl Grapheme {
    /// `grapheme`, or why it is none a glyph shows.
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

/// What a column of a row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shows {
    /// Nothing was written to it.
    Blank,
    /// A grapheme of one column.
    Narrow(Grapheme),
    /// A grapheme of two columns, this one and the next, which shows nothing
    /// of its own.
    Wide(Grapheme),
}

impl Shows {
    /// How many columns it takes.
    #[must_use]
    pub fn columns(&self) -> u16 {
        match self {
            Shows::Blank | Shows::Narrow(_) => 1,
            Shows::Wide(_) => 2,
        }
    }
}

/// A glyph, and the style it shows in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub shows: Shows,
    pub style: Style,
}

/// A colour a terminal has: of the 256 of its palette, or of red, green and
/// blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Palette(u8),
    Rgb([u8; 3]),
}

/// How a glyph shows: in the terminal's own colours where they are none, and
/// as the terminal's own text where all is none.
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
    /// In any of the ways a terminal underlines.
    pub underlined: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
}

/// A row's glyphs, from its first column, whose columns add up to the row's,
/// less one where it `wraps_wide`: its last column shows nothing, as the wide
/// glyph that did not fit there went on to the next row.
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

    /// `glyphs`, a row of `columns` that `wraps_wide` or not, or why they
    /// are none: they do not add up to the row's columns.
    pub fn new(glyphs: Vec<Glyph>, wraps_wide: bool, columns: NonZeroU16) -> Result<Line, String> {
        let line = Line { glyphs, wraps_wide };
        let taken = line.columns();
        if taken != u32::from(columns.get()) {
            return Err(format!("a row of {columns} columns has glyphs of {taken}"));
        }
        Ok(line)
    }

    /// The columns of the row it is: its glyphs', and the one it leaves
    /// blank where it `wraps_wide`.
    fn columns(&self) -> u32 {
        let taken: u32 = self.glyphs.iter().map(|glyph| u32::from(glyph.shows.columns())).sum();
        taken + u32::from(self.wraps_wide)
    }

    /// `line`, for a row of `columns`, or why it does not read: its glyphs
    /// do not add up to the row's columns, or one has no grapheme, or a
    /// grapheme with a control character in it, which would write more than
    /// itself to a terminal.
    fn heard(line: Option<api::Line>, columns: NonZeroU16) -> Result<Line, String> {
        let line = line.ok_or("a row of a frame has no line")?;
        let glyphs = line.glyphs.into_iter().map(glyph).collect::<Result<Vec<_>, _>>()?;
        Line::new(glyphs, line.wraps_wide, columns)
    }
}

/// `glyph`, or why it does not read: no grapheme, one that writes more than
/// itself, or a style of a colour no terminal has.
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

/// `style`, or why it does not read: of a colour no terminal has. None is the
/// terminal's own, as one of no colours and nothing set is. An underline of
/// a kind this luneta does not know of is none.
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

/// `colour`, where there is one, or why it does not read: it is of nothing,
/// or of a palette entry past the 256 a terminal has, or of a part past 255.
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

/// The rows a frame has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rows {
    /// Each of the pane's, from the top.
    Whole(Vec<Line>),
    /// Those that changed since the last frame of the pane, each by its
    /// index, from 0 at the top, and each once.
    Changed(Vec<(u16, Line)>),
}

/// The cursor's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

/// Where the cursor is, on the frame's rows, and how it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub column: u16,
    pub shape: CursorShape,
    pub blinking: bool,
}

/// What a pane shows, to this client: rows of its size, each of its columns,
/// and a cursor on them, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pane: u32,
    size: Size,
    rows: Rows,
    cursor: Option<Cursor>,
    tracks_mouse: bool,
}

impl Frame {
    /// A frame of `pane`, of `rows` of `size`, with `cursor` on them, and
    /// whether the pane's program `tracks_mouse`; or why it is none: all its
    /// rows are not the size's, or a row is of other columns, or it changes
    /// a row past its last or twice, or the cursor is off it.
    pub fn new(
        pane: u32,
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

    /// The pane: a frame of one the client is no longer on is to be dropped.
    #[must_use]
    pub fn pane(&self) -> u32 {
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

    /// None where it is hidden, or out of the view.
    #[must_use]
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    /// Whether the pane's program takes the mouse.
    #[must_use]
    pub fn tracks_mouse(&self) -> bool {
        self.tracks_mouse
    }

    /// `frame`, or why it does not read: a size out of bounds, rows that are
    /// not the size's, a row changed twice, or a cursor off them.
    fn heard(frame: api::Frame) -> Result<Frame, String> {
        let size = Size::heard(frame.size)?;
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
        Frame::new(frame.pane, size, rows, cursor, frame.mouse_tracking)
    }
}

/// `cursor`, or why it does not read: at a row or a column past any a frame
/// has. `Frame::new` holds it to its frame.
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

/// What the server said to a client, as it comes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    /// It took the client in: what it says next is where the client is.
    Attached,
    Went(Went),
    Frame(Frame),
    /// A go the client sent was refused, for this reason, and it stays where
    /// it is: by the server, or by the client, as the server holds none of
    /// where it goes, as `To::found_in` says.
    Stayed(String),
    /// The client's session ended. The server says nothing more.
    Ended,
    /// The server is gone, or let go of the client, or said what does not
    /// read, as this says, and says nothing more.
    Lost(String),
}

/// `said`, the server's word to a client, with `sessions` the sessions it
/// lists, or why it does not read.
fn heard(said: api::ToClient, sessions: &[OwnKey]) -> Result<Heard, String> {
    Ok(match said.event.ok_or("the server said nothing")? {
        to_client::Event::Attached(_) => Heard::Attached,
        to_client::Event::Went(api::Went { session, tab, pane }) => {
            let id = OwnId::new(session).ok_or("the client went to session 0")?;
            let session = sessions.iter().find(|key| key.id == id);
            let unlisted = || format!("the client went to session {id}, which it does not list");
            let session = *session.ok_or_else(unlisted)?;
            if tab == 0 {
                return Err("the client went to tab 0".to_owned());
            }
            let pane = OwnPane::new(session, pane).ok_or("the client went to pane 0")?;
            Heard::Went(Went { pane, tab })
        },
        to_client::Event::Frame(frame) => Heard::Frame(Frame::heard(frame)?),
        to_client::Event::Ended(api::Ended {}) => Heard::Ended,
        to_client::Event::Stayed(api::Stayed { reason }) => Heard::Stayed(reason),
    })
}

/// What luneta has a client say to the server, in the order it says it: an
/// event, which goes as it is, or a go, which goes where the server holds
/// where it goes, as `To::found_in` says, and is refused where not.
enum Saying {
    Event(to_server::Event),
    Go(To),
}

/// A client attached to an own session, which takes what you do there. It
/// lets go of the session as it drops.
pub struct Attach {
    said: mpsc::Sender<Saying>,
    /// Why luneta gave up on the client, which ends it: the first why alone.
    giving_up: mpsc::Sender<String>,
}

impl Attach {
    /// Sends the server `event`. Where it has yet to take the `SAID_MAX` sent
    /// before, luneta gives up on the client, as `give_up` says, rather than
    /// drop what you did there and go on. Where the client is lost already,
    /// it goes nowhere, as `Heard::Lost` says.
    pub fn send(&self, event: to_server::Event) {
        self.say(Saying::Event(event));
    }

    /// Takes the client `to` a session or a tab, as `send` sends an event:
    /// where the server it reached holds none there, the client hears that
    /// it stayed, as `Heard::Stayed` says.
    pub fn go(&self, to: To) {
        self.say(Saying::Go(to));
    }

    fn say(&self, saying: Saying) {
        match self.said.try_send(saying) {
            Ok(()) | Err(TrySendError::Closed(_)) => {},
            Err(TrySendError::Full(_)) => self.give_up(untaken()),
        }
    }

    /// Ends the client, for `why`, which it hears last, as lost. Of two whys,
    /// the first stands.
    pub fn give_up(&self, why: String) {
        // Full, as luneta gave up already; or closed, as the client ended.
        let _ = self.giving_up.try_send(why);
    }
}

/// A client of the own sessions, which says `hello` first: a thread of its
/// own reaches the server, and has `told` hear what the server says, as it
/// comes, until it says nothing more, as the last it hears says. Where no
/// thread starts, `told` hears at once that the client is lost.
pub fn attach<Told: FnMut(Heard) + Send + 'static>(hello: Hello, mut told: Told) -> Attach {
    let (said, saying) = mpsc::channel(SAID_MAX);
    let (giving_up, given_up) = mpsc::channel(1);
    // `told` goes to the thread once it runs, for luneta to have it still to
    // tell where none does.
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

/// Why luneta gives up on a client whose server has yet to take what it sent.
fn untaken() -> String {
    format!("luneta-mux has yet to take the last {SAID_MAX} events sent it")
}

/// What a client hears next: what the server said, or that it let go; what
/// luneta has it say, or that luneta says no more, as it let go of the
/// client; or why luneta gave up on it.
enum Next {
    Said(Result<api::ToClient, Status>),
    LetGo,
    Saying(Saying),
    Unsaid,
    GaveUp(String),
}

/// The client: `hello`, then what comes on `saying`, to the server, in turn,
/// and what the server says to `told`, but the last, which it gives back,
/// until the server says no more or a why comes on `given_up`. It goes only
/// where the server it reached holds the session `hello` goes to, or a go
/// after, which it asks that server for its sessions for, and `told` hears
/// it stayed where not. It asks for them again too where the client went to
/// one it did not list before.
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
    // What luneta has the client say goes on to the server through this, in
    // turn, once each go is checked; and as luneta lets go of the client, so
    // does this, which ends what the client says, and the server lets go.
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
                // Closed, as the server let go, which the client hears next.
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
        match heard(said, &sessions) {
            Ok(Heard::Ended) => return Heard::Ended,
            Ok(heard) => told(heard),
            Err(why) => {
                return Heard::Lost(format!("luneta-mux said what luneta cannot read: {why}"));
            },
        }
    }
}

/// The sessions the server `client` reached lists now, as no other is, and
/// as it lists them; or the client lost, where it does not answer.
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

    /// A row's glyphs add up to its columns, a wide one taking two, and one
    /// left blank where a wide glyph went on to the next row.
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

    /// A frame of more rows than its size, a row past its last, a cursor off
    /// it, a side of 0 and a grapheme that writes more than itself do not read.
    #[test]
    fn a_frame_that_does_not_fit_its_size_does_not_read() {
        let row = api::Line { glyphs: vec![narrow("a")], wraps_wide: false };
        let mut two = frame(1, vec![row.clone(), row.clone()]);
        two.size = Some(api::Size { columns: 1, rows: 1 });
        assert_eq!(Frame::heard(two).unwrap_err(), "a frame of 1 rows has 2");
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

    /// A glyph's colours are of a palette entry of the 256, or of red, green
    /// and blue of a byte each: any other does not read. A glyph of no style
    /// shows as one of no colours and nothing set does.
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

    /// A frame is made only of rows of its size, each of its columns, each
    /// changed once, and a cursor on them, however it is made.
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
        let made = |rows, cursor| Frame::new(1, size, rows, cursor, false);
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
        let made = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(made);
        OwnKey { id: OwnId::new(id).unwrap(), made }
    }

    /// A client went to a session as no other is, of those its server
    /// lists, and never to session 0, tab 0 or pane 0, which name none.
    #[test]
    fn a_went_names_a_listed_session_a_tab_and_a_pane() {
        let sessions = [key(3, 20)];
        let went = |session, tab, pane| {
            let event = to_client::Event::Went(api::Went { session, tab, pane });
            heard(api::ToClient { event: Some(event) }, &sessions)
        };
        let pane = OwnPane::new(key(3, 20), 4).unwrap();
        assert_eq!(went(3, 1, 4), Ok(Heard::Went(Went { pane, tab: 1 })));
        assert_eq!(went(0, 1, 4).unwrap_err(), "the client went to session 0");
        let unlisted = "the client went to session 5, which it does not list";
        assert_eq!(went(5, 1, 4).unwrap_err(), unlisted);
        assert_eq!(went(3, 0, 4).unwrap_err(), "the client went to tab 0");
        assert_eq!(went(3, 1, 0).unwrap_err(), "the client went to pane 0");
    }

    /// A client goes only where the server lists its session, of the id it
    /// names and made when it says: a server started since gives the id to
    /// another. A tab is one of that session's.
    #[test]
    fn a_client_goes_only_to_a_session_its_server_holds() {
        let tab = |id| api::Tab { id, ..api::Tab::default() };
        let listed = |id, made: u64, tabs| api::Session {
            id,
            tabs,
            made_at: Some((SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(made)).into()),
            ..api::Session::default()
        };
        let listing = [listed(1, 30, vec![tab(7)]), listed(0, 10, Vec::new())];
        let sessions = keys(&listing);
        assert_eq!(sessions, [key(1, 30)]);
        assert_eq!(To::Session(key(1, 30)).found_in(&sessions, &listing), Ok(()));
        let since = To::Session(key(1, 10)).found_in(&sessions, &listing);
        assert_eq!(since.unwrap_err(), "own session 1 no longer runs");
        let tab = |tab| To::Tab { session: key(1, 30), tab };
        assert_eq!(tab(7).found_in(&sessions, &listing), Ok(()));
        assert_eq!(tab(8).found_in(&sessions, &listing).unwrap_err(), "own session 1 has no tab 8");
    }

    /// A side past what the server takes is made what it takes.
    #[test]
    fn a_size_is_held_to_what_the_server_takes() {
        let cells = |count| NonZeroU16::new(count).unwrap();
        let size = Size::within(cells(4000), cells(30));
        assert_eq!((size.columns.get(), size.rows.get()), (SIDE_MAX, 30));
    }
}

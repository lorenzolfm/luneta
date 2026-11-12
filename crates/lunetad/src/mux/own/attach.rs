//! A client of an own session, as luneta attaches to one: on a stream of its
//! own to the server, both ways, which says where the client is and what the
//! pane it is on shows, and takes what you do there. A thread of its own
//! waits on the stream, and hands on what the server says as it comes.
//!
//! What the server says is read here, at the edge: a frame whose rows do not
//! add up to its size, or a word that names no session or pane, ends the
//! client, as a server that says them is not one luneta can follow.

use std::num::NonZeroU16;
use std::thread;

use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;

use super::api::{self, to_client, to_server};
use super::server::{connect, refused, runtime, socket};
use super::OwnId;

/// The most the server lets a side of a pane be, in cells: more is refused.
pub const SIDE_MAX: u16 = 1000;

/// How many events a client holds for the server at once, while the server
/// has yet to take them: a key repeat or the mouse moving sends hundreds a
/// second, and the server takes each at once.
const SAID_MAX: usize = 1024;

/// Where a client goes among the own sessions, by the ids its server gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum To {
    /// A session, on the tab a client coming there goes to.
    Session(OwnId),
    /// A tab of a session.
    Tab(u32),
}

impl To {
    #[must_use]
    pub fn said(self) -> api::Place {
        let is = match self {
            To::Session(session) => api::place::Is::Session(session.get()),
            To::Tab(tab) => api::place::Is::Tab(tab),
        };
        api::Place { is: Some(is) }
    }
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

/// Where the client is now, as the server says each time it moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Went {
    pub session: OwnId,
    pub tab: u32,
    pub pane: u32,
}

/// What a column of a row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shows {
    /// Nothing was written to it.
    Blank,
    /// A grapheme of one column.
    Narrow(String),
    /// A grapheme of two columns, this one and the next, which shows nothing
    /// of its own.
    Wide(String),
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

/// A glyph, and the style it shows in: none for the terminal's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub shows: Shows,
    pub style: Option<api::Style>,
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
        let taken: u32 = glyphs.iter().map(|glyph| u32::from(glyph.shows.columns())).sum();
        let taken = taken + u32::from(wraps_wide);
        if taken != u32::from(columns.get()) {
            return Err(format!("a row of {columns} columns has glyphs of {taken}"));
        }
        Ok(Line { glyphs, wraps_wide })
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

/// `glyph`, or why it does not read.
fn glyph(glyph: api::Glyph) -> Result<Glyph, String> {
    let grapheme = |grapheme: String| {
        if grapheme.is_empty() {
            return Err("a glyph has no grapheme".to_owned());
        }
        if grapheme.chars().any(char::is_control) {
            return Err(format!("the glyph {grapheme:?} has a control character"));
        }
        Ok(grapheme)
    };
    let shows = match glyph.shows.ok_or("a glyph shows nothing")? {
        api::glyph::Shows::Blank(api::Blank {}) => Shows::Blank,
        api::glyph::Shows::Narrow(api::Narrow { grapheme: it }) => Shows::Narrow(grapheme(it)?),
        api::glyph::Shows::Wide(api::Wide { grapheme: it }) => Shows::Wide(grapheme(it)?),
    };
    Ok(Glyph { shows, style: glyph.style })
}

/// The rows a frame has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rows {
    /// Each of the pane's, from the top.
    Whole(Vec<Line>),
    /// Those that changed since the last frame of the pane, each by its
    /// index, from 0 at the top.
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

/// What a pane shows, to this client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The pane: a frame of one the client is no longer on is to be dropped.
    pub pane: u32,
    pub size: Size,
    pub rows: Rows,
    /// None where it is hidden, or out of the view.
    pub cursor: Option<Cursor>,
    /// Whether the pane's program takes the mouse.
    pub tracks_mouse: bool,
}

impl Frame {
    /// `frame`, or why it does not read: a size out of bounds, rows that are
    /// not the size's, or a cursor off them.
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
        let cursor = frame.cursor.map(|cursor| cursor_heard(cursor, size)).transpose()?;
        Ok(Frame { pane: frame.pane, size, rows, cursor, tracks_mouse: frame.mouse_tracking })
    }
}

/// `cursor`, on a frame of `size`, or why it does not read.
fn cursor_heard(cursor: api::Cursor, size: Size) -> Result<Cursor, String> {
    let on = |at: u32, side: NonZeroU16| u16::try_from(at).ok().filter(|at| *at < side.get());
    let off = || format!("the cursor is off the frame, at {}:{}", cursor.row, cursor.column);
    let row = on(cursor.row, size.rows).ok_or_else(off)?;
    let column = on(cursor.column, size.columns).ok_or_else(off)?;
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
    /// A `Go` the client sent was refused, for this reason, and it stays where
    /// it is.
    Stayed(String),
    /// The client's session ended. The server says nothing more.
    Ended,
    /// The server is gone, or let go of the client, or said what does not
    /// read, as this says, and says nothing more.
    Lost(String),
}

/// `said`, the server's word to a client, or why it does not read.
fn heard(said: api::ToClient) -> Result<Heard, String> {
    Ok(match said.event.ok_or("the server said nothing")? {
        to_client::Event::Attached(_) => Heard::Attached,
        to_client::Event::Went(api::Went { session, tab, pane }) => {
            let session = OwnId::new(session).ok_or("the client went to session 0")?;
            if pane == 0 {
                return Err("the client went to pane 0".to_owned());
            }
            Heard::Went(Went { session, tab, pane })
        },
        to_client::Event::Frame(frame) => Heard::Frame(Frame::heard(frame)?),
        to_client::Event::Ended(api::Ended {}) => Heard::Ended,
        to_client::Event::Stayed(api::Stayed { reason }) => Heard::Stayed(reason),
    })
}

/// A client attached to an own session, which takes what you do there. It
/// lets go of the session as it drops.
pub struct Attach {
    said: mpsc::Sender<api::ToServer>,
}

impl Attach {
    /// Sends the server `event`, or says why it did not: it has yet to take
    /// the many sent before. Where the client is lost already, it goes
    /// nowhere, as `Heard::Lost` says.
    pub fn send(&self, event: to_server::Event) -> Result<(), String> {
        match self.said.try_send(api::ToServer { event: Some(event) }) {
            Ok(()) | Err(TrySendError::Closed(_)) => Ok(()),
            Err(TrySendError::Full(_)) => {
                Err(format!("luneta-mux has yet to take the last {SAID_MAX} events sent it"))
            },
        }
    }
}

/// A client of the own sessions, which says `hello` first: a thread of its
/// own reaches the server, and has `told` hear what the server says, as it
/// comes, until it says nothing more, as the last it hears says.
pub fn attach(hello: Hello, mut told: impl FnMut(Heard) + Send + 'static) -> Attach {
    let (said, saying) = mpsc::channel(SAID_MAX);
    let attaching = move || {
        let last = match runtime() {
            Ok(runtime) => runtime.block_on(attached(hello, saying, &mut told)),
            Err(failure) => Heard::Lost(failure.to_string()),
        };
        told(last);
    };
    let spawned = thread::Builder::new().name("luneta-mux client".to_owned()).spawn(attaching);
    if let Err(error) = spawned {
        // The sender drops with it, and the client never sends.
        crate::log::say(&format!("no thread for a client of luneta-mux: {error}"));
    }
    Attach { said }
}

/// The client: `hello`, then what comes on `saying`, to the server, and what
/// the server says to `told`, but the last, which it gives back.
async fn attached(
    hello: Hello,
    saying: mpsc::Receiver<api::ToServer>,
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
    let said = tokio_stream::once(hello.said()).chain(ReceiverStream::new(saying));
    let mut hearing = match client.attach(said).await {
        Ok(hearing) => hearing.into_inner(),
        Err(status) => return Heard::Lost(refused(&status).to_string()),
    };
    loop {
        let said = match hearing.message().await {
            Ok(Some(said)) => said,
            Ok(None) => return Heard::Lost("luneta-mux let go of luneta's client".to_owned()),
            Err(status) => return Heard::Lost(refused(&status).to_string()),
        };
        match heard(said) {
            Ok(Heard::Ended) => return Heard::Ended,
            Ok(heard) => told(heard),
            Err(why) => {
                return Heard::Lost(format!("luneta-mux said what luneta cannot read: {why}"));
            },
        }
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
        assert_eq!(lines[0].glyphs()[1].shows, Shows::Wide("日".to_owned()));
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
        let cursor = Some(api::Cursor { row: 0, column: 1, ..api::Cursor::default() });
        let off = api::Frame { cursor, ..frame(1, vec![row.clone()]) };
        assert_eq!(Frame::heard(off).unwrap_err(), "the cursor is off the frame, at 0:1");
        let none =
            api::Frame { size: Some(api::Size { columns: 0, rows: 1 }), ..frame(1, vec![row]) };
        assert_eq!(Frame::heard(none).unwrap_err(), "a frame's columns are 0");
        let escape = api::Line { glyphs: vec![narrow("\u{1b}")], wraps_wide: false };
        assert!(Frame::heard(frame(1, vec![escape])).unwrap_err().contains("control character"));
    }

    /// A client is never told it went to session 0 or pane 0, which name none.
    #[test]
    fn a_went_names_a_session_and_a_pane() {
        let went = |session, pane| {
            let event = to_client::Event::Went(api::Went { session, tab: 1, pane });
            heard(api::ToClient { event: Some(event) })
        };
        let session = OwnId::new(3).unwrap();
        assert_eq!(went(3, 4), Ok(Heard::Went(Went { session, tab: 1, pane: 4 })));
        assert_eq!(went(0, 4).unwrap_err(), "the client went to session 0");
        assert_eq!(went(3, 0).unwrap_err(), "the client went to pane 0");
    }

    /// A side past what the server takes is made what it takes.
    #[test]
    fn a_size_is_held_to_what_the_server_takes() {
        let cells = |count| NonZeroU16::new(count).unwrap();
        let size = Size::within(cells(4000), cells(30));
        assert_eq!((size.columns.get(), size.rows.get()), (SIDE_MAX, 30));
    }
}

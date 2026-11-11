//! PROTOTYPE. The messages between the server and its clients: each a u32
//! length, a tag byte, and its fields, little endian.

use std::io::{self, Read, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Size {
    pub columns: u16,
    pub lines: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A colour as the client's terminal is told it: its own default, one of its
/// 256, or an exact one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Reset,
    Indexed(u8),
    Rgb(Rgb),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub c: char,
    pub zerowidth: Vec<char>,
    pub fg: Colour,
    pub bg: Colour,
    pub underline: Option<Colour>,
    /// alacritty's `Flags`, plus `DIM_FG` for a dim one of the sixteen.
    pub flags: u16,
    /// An OSC 8 link, if the cell is in one.
    pub link: Option<String>,
}

pub const DIM_FG: u16 = 1 << 15;

impl Default for Cell {
    fn default() -> Self {
        Cell {
            c: ' ',
            zerowidth: Vec::new(),
            fg: Colour::Reset,
            bg: Colour::Reset,
            underline: None,
            flags: 0,
            link: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub columns: u16,
    pub lines: u16,
    /// alacritty's `TermMode` bits.
    pub mode: u32,
    /// Line and column, or none for a hidden cursor.
    pub cursor: Option<(u16, u16)>,
    /// 0 block, 1 underline, 2 beam, 3 hollow block (never set), 4 hidden.
    pub shape: u8,
    pub blinking: bool,
    /// How far back into the scrollback this client looks, and how much there is.
    pub offset: u32,
    pub history: u32,
    /// Whether every line is in `rows`, as after a resize.
    pub whole: bool,
    pub rows: Vec<(u16, Vec<Cell>)>,
    /// The server's time spent locking and reading the grid, in microseconds.
    pub read_us: u32,
}

pub enum ToServer {
    /// Attach to the session, making it with `command` in `dir` if there is none.
    Hello { session: String, command: Vec<String>, dir: String, size: Size, fg: Option<Rgb>, bg: Option<Rgb> },
    Input(Vec<u8>),
    Resize(Size),
    /// Lines further back (positive) or forward (negative).
    Scroll(i32),
    List,
    Kill(String),
}

pub enum ToClient {
    Frame(Frame),
    Title(String),
    Bell,
    Clipboard(String),
    /// The session ended, with the program's exit code where it had one.
    Ended(Option<i32>),
    Sessions(Vec<String>),
}

#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) { self.0.push(v); }
    fn u16(&mut self, v: u16) { self.0.extend_from_slice(&v.to_le_bytes()); }
    fn u32(&mut self, v: u32) { self.0.extend_from_slice(&v.to_le_bytes()); }
    fn i32(&mut self, v: i32) { self.0.extend_from_slice(&v.to_le_bytes()); }
    fn bytes(&mut self, v: &[u8]) { self.u32(v.len() as u32); self.0.extend_from_slice(v); }
    fn str(&mut self, v: &str) { self.bytes(v.as_bytes()); }
    fn char(&mut self, c: char) { self.u32(c as u32); }
    fn size(&mut self, s: Size) { self.u16(s.columns); self.u16(s.lines); self.u16(s.cell_width); self.u16(s.cell_height); }
    fn rgb(&mut self, c: Option<Rgb>) {
        match c {
            None => self.u8(0),
            Some(Rgb(r, g, b)) => { self.u8(1); self.u8(r); self.u8(g); self.u8(b); },
        }
    }
    fn colour(&mut self, c: Colour) {
        match c {
            Colour::Reset => self.u8(0),
            Colour::Indexed(i) => { self.u8(1); self.u8(i); },
            Colour::Rgb(Rgb(r, g, b)) => { self.u8(2); self.u8(r); self.u8(g); self.u8(b); },
        }
    }
    fn cell(&mut self, c: &Cell) {
        self.char(c.c);
        self.u8(c.zerowidth.len() as u8);
        for z in &c.zerowidth { self.char(*z); }
        self.colour(c.fg);
        self.colour(c.bg);
        match c.underline { None => self.u8(9), Some(u) => self.colour(u) }
        self.u16(c.flags);
        match &c.link { None => self.u8(0), Some(l) => { self.u8(1); self.str(l); } }
    }
}

struct In<'a>(&'a [u8]);

fn short() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "short message") }

impl In<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n { return Err(short()); }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> io::Result<u8> { Ok(self.take(1)?[0]) }
    fn u16(&mut self) -> io::Result<u16> { Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap())) }
    fn u32(&mut self) -> io::Result<u32> { Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
    fn i32(&mut self) -> io::Result<i32> { Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
    fn bytes(&mut self) -> io::Result<Vec<u8>> { let n = self.u32()? as usize; Ok(self.take(n)?.to_vec()) }
    fn str(&mut self) -> io::Result<String> { String::from_utf8(self.bytes()?).map_err(|_| short()) }
    fn char(&mut self) -> io::Result<char> { char::from_u32(self.u32()?).ok_or_else(short) }
    fn size(&mut self) -> io::Result<Size> {
        Ok(Size { columns: self.u16()?, lines: self.u16()?, cell_width: self.u16()?, cell_height: self.u16()? })
    }
    fn rgb(&mut self) -> io::Result<Option<Rgb>> {
        Ok(match self.u8()? { 0 => None, _ => Some(Rgb(self.u8()?, self.u8()?, self.u8()?)) })
    }
    fn colour_tagged(&mut self, tag: u8) -> io::Result<Colour> {
        Ok(match tag {
            0 => Colour::Reset,
            1 => Colour::Indexed(self.u8()?),
            2 => Colour::Rgb(Rgb(self.u8()?, self.u8()?, self.u8()?)),
            _ => return Err(short()),
        })
    }
    fn colour(&mut self) -> io::Result<Colour> { let t = self.u8()?; self.colour_tagged(t) }
    fn cell(&mut self) -> io::Result<Cell> {
        let c = self.char()?;
        let n = self.u8()?;
        let zerowidth = (0..n).map(|_| self.char()).collect::<io::Result<_>>()?;
        let fg = self.colour()?;
        let bg = self.colour()?;
        let underline = match self.u8()? { 9 => None, t => Some(self.colour_tagged(t)?) };
        let flags = self.u16()?;
        let link = match self.u8()? { 0 => None, _ => Some(self.str()?) };
        Ok(Cell { c, zerowidth, fg, bg, underline, flags, link })
    }
}

fn send(to: &mut impl Write, tag: u8, body: Out) -> io::Result<usize> {
    let mut whole = Vec::with_capacity(body.0.len() + 5);
    whole.extend_from_slice(&(body.0.len() as u32 + 1).to_le_bytes());
    whole.push(tag);
    whole.extend_from_slice(&body.0);
    to.write_all(&whole)?;
    Ok(whole.len())
}

fn receive(from: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut length = [0; 4];
    from.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    let mut body = vec![0; length];
    from.read_exact(&mut body)?;
    let tag = *body.first().ok_or_else(short)?;
    body.remove(0);
    Ok((tag, body))
}

impl ToServer {
    pub fn send(&self, to: &mut impl Write) -> io::Result<usize> {
        let mut o = Out::default();
        let tag = match self {
            ToServer::Hello { session, command, dir, size, fg, bg } => {
                o.str(session);
                o.u32(command.len() as u32);
                for a in command { o.str(a); }
                o.str(dir);
                o.size(*size);
                o.rgb(*fg);
                o.rgb(*bg);
                0
            },
            ToServer::Input(b) => { o.bytes(b); 1 },
            ToServer::Resize(s) => { o.size(*s); 2 },
            ToServer::Scroll(d) => { o.i32(*d); 3 },
            ToServer::List => 4,
            ToServer::Kill(s) => { o.str(s); 5 },
        };
        send(to, tag, o)
    }

    pub fn receive(from: &mut impl Read) -> io::Result<Self> {
        let (tag, body) = receive(from)?;
        let mut i = In(&body);
        Ok(match tag {
            0 => {
                let session = i.str()?;
                let n = i.u32()?;
                let command = (0..n).map(|_| i.str()).collect::<io::Result<_>>()?;
                ToServer::Hello { session, command, dir: i.str()?, size: i.size()?, fg: i.rgb()?, bg: i.rgb()? }
            },
            1 => ToServer::Input(i.bytes()?),
            2 => ToServer::Resize(i.size()?),
            3 => ToServer::Scroll(i.i32()?),
            4 => ToServer::List,
            5 => ToServer::Kill(i.str()?),
            _ => return Err(short()),
        })
    }
}

impl ToClient {
    pub fn send(&self, to: &mut impl Write) -> io::Result<usize> {
        let mut o = Out::default();
        let tag = match self {
            ToClient::Frame(f) => {
                o.u16(f.columns);
                o.u16(f.lines);
                o.u32(f.mode);
                match f.cursor { None => o.u8(0), Some((l, c)) => { o.u8(1); o.u16(l); o.u16(c); } }
                o.u8(f.shape);
                o.u8(u8::from(f.blinking));
                o.u32(f.offset);
                o.u32(f.history);
                o.u8(u8::from(f.whole));
                o.u32(f.read_us);
                o.u32(f.rows.len() as u32);
                for (line, cells) in &f.rows {
                    o.u16(*line);
                    o.u32(cells.len() as u32);
                    for c in cells { o.cell(c); }
                }
                0
            },
            ToClient::Title(t) => { o.str(t); 1 },
            ToClient::Bell => 2,
            ToClient::Clipboard(t) => { o.str(t); 3 },
            ToClient::Ended(code) => { match code { None => o.u8(0), Some(c) => { o.u8(1); o.i32(*c); } } 4 },
            ToClient::Sessions(names) => { o.u32(names.len() as u32); for n in names { o.str(n); } 5 },
        };
        send(to, tag, o)
    }

    /// The message, and how many bytes it took on the wire.
    pub fn receive(from: &mut impl Read) -> io::Result<(Self, usize)> {
        let (tag, body) = receive(from)?;
        let wire = body.len() + 5;
        let mut i = In(&body);
        let message = match tag {
            0 => {
                let columns = i.u16()?;
                let lines = i.u16()?;
                let mode = i.u32()?;
                let cursor = match i.u8()? { 0 => None, _ => Some((i.u16()?, i.u16()?)) };
                let shape = i.u8()?;
                let blinking = i.u8()? != 0;
                let offset = i.u32()?;
                let history = i.u32()?;
                let whole = i.u8()? != 0;
                let read_us = i.u32()?;
                let n = i.u32()?;
                let mut rows = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let line = i.u16()?;
                    let m = i.u32()?;
                    let cells = (0..m).map(|_| i.cell()).collect::<io::Result<_>>()?;
                    rows.push((line, cells));
                }
                ToClient::Frame(Frame { columns, lines, mode, cursor, shape, blinking, offset, history, whole, rows, read_us })
            },
            1 => ToClient::Title(i.str()?),
            2 => ToClient::Bell,
            3 => ToClient::Clipboard(i.str()?),
            4 => ToClient::Ended(match i.u8()? { 0 => None, _ => Some(i.i32()?) }),
            5 => {
                let n = i.u32()?;
                ToClient::Sessions((0..n).map(|_| i.str()).collect::<io::Result<_>>()?)
            },
            _ => return Err(short()),
        };
        Ok((message, wire))
    }
}

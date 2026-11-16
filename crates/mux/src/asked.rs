use std::fmt;
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::time::SystemTime;

use libghostty_vt::key::{self as keys, Mods};
use libghostty_vt::mouse;
use tonic::Status;

use crate::api;
use crate::id::{ClientId, PaneId, SessionId, TabId};
use crate::layout::{Change, Direction, Order, Towards};
use crate::screen::{Click, RgbColor, Wheel};

pub const NAME_BYTES_MAX: usize = 255;

pub const SIDE_MAX: u16 = 1000;

pub const TAG_BYTES_MAX: usize = 256;

pub const SHOWN_MAX: usize = 1024;

pub(crate) fn refused(field: &str, why: impl fmt::Display) -> Status {
    Status::invalid_argument(format!("{field} {why}"))
}

fn present<T>(field: &str, value: Option<T>) -> Result<T, Status> {
    value.ok_or_else(|| refused(field, "is missing"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub columns: NonZeroU16,
    pub rows: NonZeroU16,
}

impl Size {
    pub fn parse(field: &str, size: Option<api::Size>) -> Result<Size, Status> {
        let size = present(field, size)?;
        let count = |name: &str, count: u32| {
            let field = format!("{field}.{name}");
            let count = u16::try_from(count)
                .ok()
                .filter(|count| *count <= SIDE_MAX)
                .ok_or_else(|| refused(&field, format_args!("is over {SIDE_MAX}")))?;
            NonZeroU16::new(count).ok_or_else(|| refused(&field, "is 0"))
        };
        Ok(Size { columns: count("columns", size.columns)?, rows: count("rows", size.rows)? })
    }

    #[must_use]
    pub fn said(self) -> api::Size {
        api::Size { columns: self.columns.get().into(), rows: self.rows.get().into() }
    }

    #[must_use]
    pub fn get(self) -> (u16, u16) {
        (self.columns.get(), self.rows.get())
    }

    #[must_use]
    pub fn holds(self, column: u16, row: u16) -> bool {
        let (columns, rows) = self.get();
        column < columns && row < rows
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name(String);

impl Name {
    pub fn parse(field: &str, name: String) -> Result<Name, Status> {
        if name.is_empty() {
            return Err(refused(field, "is empty"));
        }
        if name.len() > NAME_BYTES_MAX {
            return Err(refused(field, format_args!("is over {NAME_BYTES_MAX} bytes")));
        }
        if name.chars().any(char::is_control) {
            return Err(refused(field, "has a control character"));
        }
        Ok(Name(name))
    }

    #[must_use]
    pub fn numbered(place: usize) -> Name {
        Name(format!("Tab #{place}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionName(Name);

impl SessionName {
    pub fn parse(field: &str, name: String) -> Result<SessionName, Status> {
        let name = Name::parse(field, name)?;
        if name.as_str().trim().is_empty() {
            return Err(refused(field, "is only whitespace"));
        }
        if name.as_str() == "." || name.as_str() == ".." {
            return Err(refused(field, "is '.' or '..'"));
        }
        if name.as_str().contains('/') {
            return Err(refused(field, "has a '/'"));
        }
        Ok(SessionName(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opens {
    pub cwd: PathBuf,
    pub runs: Runs,
    pub ends: Ends,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runs {
    Shell,
    Program { program: String, args: Vec<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ends {
    Closes,
    Holds,
}

impl Opens {
    pub fn parse(field: &str, opens: Option<api::Opens>) -> Result<Opens, Status> {
        let opens = present(field, opens)?;
        if opens.cwd.contains('\0') {
            return Err(refused(&format!("{field}.cwd"), "has a NUL"));
        }
        let cwd = PathBuf::from(opens.cwd);
        if !cwd.is_absolute() {
            return Err(refused(&format!("{field}.cwd"), "is not an absolute path"));
        }
        let runs = match present(&format!("{field}.runs"), opens.runs)? {
            api::opens::Runs::Shell(api::Shell {}) => Runs::Shell,
            api::opens::Runs::Program(api::Program { mut argv }) => {
                let argv_field = format!("{field}.program.argv");
                if argv.is_empty() {
                    return Err(refused(&argv_field, "is empty"));
                }
                if argv[0].is_empty() {
                    return Err(refused(&argv_field, "names no program"));
                }
                if argv.iter().any(|arg| arg.contains('\0')) {
                    return Err(refused(&argv_field, "has a NUL"));
                }
                let program = argv.remove(0);
                Runs::Program { program, args: argv }
            },
        };
        let ends = match api::Ends::try_from(opens.ends) {
            Ok(api::Ends::Closes) => Ends::Closes,
            Ok(api::Ends::Holds) => Ends::Holds,
            Ok(api::Ends::Unspecified) | Err(_) => {
                return Err(refused(&format!("{field}.ends"), "is not one of CLOSES or HOLDS"));
            },
        };
        Ok(Opens { cwd, runs, ends })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub is: Is,
    pub made: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Is {
    Session(SessionId),
    Tab(TabId),
    Pane(PaneId),
}

impl Place {
    pub fn parse(field: &str, place: Option<api::Place>) -> Result<Place, Status> {
        let place = present(field, place)?;
        let is = match present(&format!("{field}.is"), place.is)? {
            api::place::Is::Session(session) => {
                Is::Session(SessionId::parse(&format!("{field}.session"), session)?)
            },
            api::place::Is::Tab(tab) => Is::Tab(TabId::parse(&format!("{field}.tab"), tab)?),
            api::place::Is::Pane(pane) => {
                Is::Pane(PaneId::parse(&format!("{field}.pane"), pane)?)
            },
        };
        let made_at = format!("{field}.made_at");
        let made = SystemTime::try_from(present(&made_at, place.made_at)?)
            .map_err(|why| refused(&made_at, why))?;
        Ok(Place { is, made })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    Beside { pane: PaneId, towards: Towards },
    Tab(TabId),
}

impl At {
    pub fn parse(field: &str, at: Option<api::new_pane_request::At>) -> Result<At, Status> {
        Ok(match present(field, at)? {
            api::new_pane_request::At::Beside(api::Beside { pane, towards }) => At::Beside {
                pane: PaneId::parse(&format!("{field}.beside.pane"), pane)?,
                towards: towards_heard(&format!("{field}.beside.towards"), towards)?,
            },
            api::new_pane_request::At::Tab(tab) => {
                At::Tab(TabId::parse(&format!("{field}.tab"), tab)?)
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnTo {
    NewTab(Option<Name>),
    Tab(TabId),
}

impl TurnTo {
    pub fn parse(field: &str, into: Option<api::Into>) -> Result<TurnTo, Status> {
        let into = present(field, into)?;
        Ok(match present(&format!("{field}.is"), into.is)? {
            api::into::Is::NewTab(api::NewTab { name }) => {
                let field = format!("{field}.new_tab.name");
                TurnTo::NewTab(name.map(|name| Name::parse(&field, name)).transpose()?)
            },
            api::into::Is::Tab(tab) => TurnTo::Tab(TabId::parse(&format!("{field}.tab"), tab)?),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    Largest,
    Beside(Towards),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTo {
    Towards(Direction),
    TowardsOrTab(Order),
    InOrder(Order),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveTo {
    Towards(Direction),
    InOrder(Order),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Open(Opening),
    ClosePane,
    Focus(FocusTo),
    Resize { change: Change, towards: Option<Direction> },
    Move(MoveTo),
    Fullscreen,
    NamePane(Option<Name>),
    NameTab(Name),
    MoveTab(Order),
    Turn(TurnTo),
}

fn towards_heard(field: &str, towards: i32) -> Result<Towards, Status> {
    match api::Towards::try_from(towards) {
        Ok(api::Towards::Right) => Ok(Towards::Right),
        Ok(api::Towards::Down) => Ok(Towards::Down),
        Ok(api::Towards::Unspecified) | Err(_) => {
            Err(refused(field, "is not one of RIGHT or DOWN"))
        },
    }
}

fn direction_heard(field: &str, direction: i32) -> Result<Direction, Status> {
    match api::Direction::try_from(direction) {
        Ok(api::Direction::Left) => Ok(Direction::Left),
        Ok(api::Direction::Right) => Ok(Direction::Right),
        Ok(api::Direction::Up) => Ok(Direction::Up),
        Ok(api::Direction::Down) => Ok(Direction::Down),
        Ok(api::Direction::Unspecified) | Err(_) => {
            Err(refused(field, "is not one of LEFT, RIGHT, UP or DOWN"))
        },
    }
}

fn order_heard(field: &str, order: i32) -> Result<Order, Status> {
    match api::Order::try_from(order) {
        Ok(api::Order::Next) => Ok(Order::Next),
        Ok(api::Order::Previous) => Ok(Order::Previous),
        Ok(api::Order::Unspecified) | Err(_) => {
            Err(refused(field, "is not one of NEXT or PREVIOUS"))
        },
    }
}

fn change_heard(field: &str, change: i32) -> Result<Change, Status> {
    match api::Change::try_from(change) {
        Ok(api::Change::Grow) => Ok(Change::Grow),
        Ok(api::Change::Shrink) => Ok(Change::Shrink),
        Ok(api::Change::Unspecified) | Err(_) => {
            Err(refused(field, "is not one of GROW or SHRINK"))
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Client(ClientId),
    LastTypedIn(SessionId),
}

impl Who {
    pub fn parse(field: &str, who: Option<api::go_request::Who>) -> Result<Who, Status> {
        Ok(match present(field, who)? {
            api::go_request::Who::Client(client) => {
                Who::Client(ClientId::parse(&format!("{field}.client"), client)?)
            },
            api::go_request::Who::LastTypedIn(session) => {
                Who::LastTypedIn(SessionId::parse(&format!("{field}.last_typed_in"), session)?)
            },
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dumped {
    Screen,
    Text,
}

impl Dumped {
    pub fn parse(field: &str, dumped: i32) -> Result<Dumped, Status> {
        match api::Dumped::try_from(dumped) {
            Ok(api::Dumped::Screen) => Ok(Dumped::Screen),
            Ok(api::Dumped::Text) => Ok(Dumped::Text),
            Ok(api::Dumped::Unspecified) | Err(_) => {
                Err(refused(field, "is not one of SCREEN or TEXT"))
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Colours {
    pub foreground: Option<RgbColor>,
    pub background: Option<RgbColor>,
}

impl Colours {
    pub fn parse(field: &str, colours: Option<api::Colours>) -> Result<Colours, Status> {
        let Some(colours) = colours else {
            return Ok(Colours::default());
        };
        let rgb = |name: &str, rgb: Option<api::Rgb>| -> Result<Option<RgbColor>, Status> {
            let Some(rgb) = rgb else {
                return Ok(None);
            };
            let part = |part: &str, value: u32| {
                u8::try_from(value)
                    .map_err(|_| refused(&format!("{field}.{name}.{part}"), "is over 255"))
            };
            Ok(Some(RgbColor {
                r: part("red", rgb.red)?,
                g: part("green", rgb.green)?,
                b: part("blue", rgb.blue)?,
            }))
        };
        Ok(Colours {
            foreground: rgb("foreground", colours.foreground)?,
            background: rgb("background", colours.background)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello {
    pub to: Place,
    pub size: Size,
    pub colours: Colours,
    pub runs_in: Option<RunsIn>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunsIn {
    pub session: SessionId,
    pub made: SystemTime,
}

impl RunsIn {
    pub fn parse(field: &str, runs_in: Option<api::RunsIn>) -> Result<Option<RunsIn>, Status> {
        let Some(runs_in) = runs_in else {
            return Ok(None);
        };
        let session = SessionId::parse(&format!("{field}.session"), runs_in.session)?;
        let made_at = format!("{field}.made_at");
        let made = SystemTime::try_from(present(&made_at, runs_in.made_at)?)
            .map_err(|why| refused(&made_at, why))?;
        Ok(Some(RunsIn { session, made }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TerminalKey {
    pub id: PaneId,
    pub made: SystemTime,
}

impl TerminalKey {
    pub fn parse(field: &str, key: Option<api::TerminalKey>) -> Result<TerminalKey, Status> {
        let key = present(field, key)?;
        let id = PaneId::parse(&format!("{field}.id"), key.id)?;
        let made_at = format!("{field}.made_at");
        let made = SystemTime::try_from(present(&made_at, key.made_at)?)
            .map_err(|why| refused(&made_at, why))?;
        Ok(TerminalKey { id, made })
    }

    #[must_use]
    pub fn said(self) -> api::TerminalKey {
        api::TerminalKey { id: self.id.said(), made_at: Some(self.made.into()) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag(Vec<u8>);

impl Tag {
    pub fn parse(field: &str, tag: Vec<u8>) -> Result<Tag, Status> {
        if tag.is_empty() {
            return Err(refused(field, "is empty"));
        }
        if tag.len() > TAG_BYTES_MAX {
            return Err(refused(field, format_args!("is over {TAG_BYTES_MAX} bytes")));
        }
        Ok(Tag(tag))
    }

    #[must_use]
    pub fn said(&self) -> Vec<u8> {
        self.0.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Greeting {
    pub colours: Colours,
    pub runs_in: Option<TerminalKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shows {
    Showing(Vec<TerminalKey>),
    Typed(PaneId, Input),
}

pub fn greeting(api::FromViewer { event }: api::FromViewer) -> Result<Greeting, Status> {
    let Some(api::from_viewer::Event::Hello(hello)) = event else {
        return Err(refused("event", "is not hello, as the first must be"));
    };
    let runs_in = hello.runs_in.map(|key| TerminalKey::parse("hello.runs_in", Some(key)));
    Ok(Greeting {
        colours: Colours::parse("hello.colours", hello.colours)?,
        runs_in: runs_in.transpose()?,
    })
}

pub fn shows(message: api::FromViewer) -> Result<Shows, Status> {
    use api::from_viewer::Event;
    Ok(match present("event", message.event)? {
        Event::Hello(_) => return Err(refused("event", "is hello, which only the first may be")),
        Event::Showing(api::Showing { terminals }) => {
            if terminals.len() > SHOWN_MAX {
                return Err(refused("showing.terminals", format_args!("are over {SHOWN_MAX}")));
            }
            let mut keys: Vec<TerminalKey> = Vec::with_capacity(terminals.len());
            for key in terminals {
                let key = TerminalKey::parse("showing.terminals", Some(key))?;
                if keys.iter().any(|shown| shown.id == key.id) {
                    return Err(refused("showing.terminals", format_args!("repeat {}", key.id)));
                }
                keys.push(key);
            }
            Shows::Showing(keys)
        },
        Event::Typed(api::Typed { terminal, event }) => {
            use api::typed::Event as Typed;
            let input = match present("typed.event", event)? {
                Typed::Key(key) => key_heard(key)?,
                Typed::Mouse(mouse) => mouse_heard(mouse)?,
                Typed::Paste(api::Paste { text }) => Input::Paste(text),
                Typed::Focus(api::Focus { focused }) => Input::Focus(focused),
                Typed::Scroll(api::Scroll { back }) => Input::Scroll(back),
            };
            Shows::Typed(PaneId::parse("typed.terminal", terminal)?, input)
        },
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Input(Input),
    Resize(Size),
    Go(Place),
    Act(Act),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Key {
        action: keys::Action,
        key: keys::Key,
        mods: Mods,
        text: Option<String>,
        unshifted: Option<char>,
    },
    Mouse {
        click: Click,
        mods: Mods,
        column: u16,
        row: u16,
    },
    Wheel {
        wheel: Wheel,
        mods: Mods,
        column: u16,
        row: u16,
    },
    Paste(String),
    Focus(bool),
    Scroll(i32),
}

impl Input {
    #[must_use]
    pub fn types(&self) -> bool {
        matches!(self, Input::Key { .. } | Input::Paste(_))
    }
}

pub fn hello(api::ToServer { event }: api::ToServer) -> Result<Hello, Status> {
    let Some(api::to_server::Event::Hello(hello)) = event else {
        return Err(refused("event", "is not hello, as the first must be"));
    };
    Ok(Hello {
        to: Place::parse("hello.to", hello.to)?,
        size: Size::parse("hello.size", hello.size)?,
        colours: Colours::parse("hello.colours", hello.colours)?,
        runs_in: RunsIn::parse("hello.runs_in", hello.runs_in)?,
    })
}

pub fn heard(message: api::ToServer) -> Result<Heard, Status> {
    use api::to_server::Event;
    Ok(match present("event", message.event)? {
        Event::Hello(_) => return Err(refused("event", "is hello, which only the first may be")),
        Event::Key(key) => Heard::Input(key_heard(key)?),
        Event::Mouse(mouse) => Heard::Input(mouse_heard(mouse)?),
        Event::Paste(api::Paste { text }) => Heard::Input(Input::Paste(text)),
        Event::Focus(api::Focus { focused }) => Heard::Input(Input::Focus(focused)),
        Event::Scroll(api::Scroll { back }) => Heard::Input(Input::Scroll(back)),
        Event::Resize(size) => Heard::Resize(Size::parse("resize", Some(size))?),
        Event::Go(place) => Heard::Go(Place::parse("go", Some(place))?),
        Event::Open(open) => Heard::Act(Act::Open(match present("open.at", open.at)? {
            api::open::At::Largest(api::Largest {}) => Opening::Largest,
            api::open::At::Beside(towards) => {
                Opening::Beside(towards_heard("open.beside", towards)?)
            },
        })),
        Event::ClosePane(api::ClosePane {}) => Heard::Act(Act::ClosePane),
        Event::FocusPane(focus) => Heard::Act(Act::Focus(match present("focus_pane.to", focus.to)? {
            api::focus_pane::To::Towards(towards) => {
                FocusTo::Towards(direction_heard("focus_pane.towards", towards)?)
            },
            api::focus_pane::To::TowardsOrTab(order) => {
                FocusTo::TowardsOrTab(order_heard("focus_pane.towards_or_tab", order)?)
            },
            api::focus_pane::To::Order(order) => {
                FocusTo::InOrder(order_heard("focus_pane.order", order)?)
            },
        })),
        Event::ResizePane(resize) => Heard::Act(Act::Resize {
            change: change_heard("resize_pane.change", resize.change)?,
            towards: resize
                .towards
                .map(|towards| direction_heard("resize_pane.towards", towards))
                .transpose()?,
        }),
        Event::MovePane(moved) => Heard::Act(Act::Move(match present("move_pane.to", moved.to)? {
            api::move_pane::To::Towards(towards) => {
                MoveTo::Towards(direction_heard("move_pane.towards", towards)?)
            },
            api::move_pane::To::Order(order) => {
                MoveTo::InOrder(order_heard("move_pane.order", order)?)
            },
        })),
        Event::Fullscreen(api::Fullscreen {}) => Heard::Act(Act::Fullscreen),
        Event::NamePane(api::NamePane { name }) => Heard::Act(Act::NamePane(
            name.map(|name| Name::parse("name_pane.name", name)).transpose()?,
        )),
        Event::NameTab(api::NameTab { name }) => {
            Heard::Act(Act::NameTab(Name::parse("name_tab.name", name)?))
        },
        Event::MoveTab(api::MoveTab { order }) => {
            Heard::Act(Act::MoveTab(order_heard("move_tab.order", order)?))
        },
        Event::Turn(into) => Heard::Act(Act::Turn(TurnTo::parse("turn", Some(into))?)),
    })
}

fn mods(modifiers: Option<api::Modifiers>) -> Mods {
    let modifiers = modifiers.unwrap_or_default();
    let mut mods = Mods::empty();
    mods.set(Mods::SHIFT, modifiers.shift);
    mods.set(Mods::ALT, modifiers.alt);
    mods.set(Mods::CTRL, modifiers.ctrl);
    mods.set(Mods::SUPER, modifiers.super_key);
    mods
}

fn key_heard(key: api::Key) -> Result<Input, Status> {
    let action = match api::Press::try_from(key.press) {
        Ok(api::Press::Down) => keys::Action::Press,
        Ok(api::Press::Repeat) => keys::Action::Repeat,
        Ok(api::Press::Up) => keys::Action::Release,
        Ok(api::Press::Unspecified) | Err(_) => {
            return Err(refused("key.press", "is not one of DOWN, REPEAT or UP"));
        },
    };
    let (code, unshifted) = match present("key.key", key.key)? {
        api::key::Key::Named(named) => (named_key(named)?, None),
        api::key::Key::Character(character) => {
            let character = char::from_u32(character)
                .ok_or_else(|| refused("key.character", "is not a Unicode scalar value"))?;
            (character_key(character), Some(character))
        },
    };
    if key.text.chars().any(char::is_control) {
        return Err(refused("key.text", "has a control character"));
    }
    let text = (!key.text.is_empty()).then_some(key.text);
    Ok(Input::Key { action, key: code, mods: mods(key.modifiers), text, unshifted })
}

fn named_key(named: i32) -> Result<keys::Key, Status> {
    use api::Named;
    use keys::Key;
    let Ok(named) = Named::try_from(named) else {
        return Err(refused("key.named", "is not a key this server knows"));
    };
    Ok(match named {
        Named::Unspecified => return Err(refused("key.named", "is unspecified")),
        Named::Enter => Key::Enter,
        Named::Tab => Key::Tab,
        Named::Backspace => Key::Backspace,
        Named::Escape => Key::Escape,
        Named::Insert => Key::Insert,
        Named::Delete => Key::Delete,
        Named::Home => Key::Home,
        Named::End => Key::End,
        Named::PageUp => Key::PageUp,
        Named::PageDown => Key::PageDown,
        Named::Up => Key::ArrowUp,
        Named::Down => Key::ArrowDown,
        Named::Left => Key::ArrowLeft,
        Named::Right => Key::ArrowRight,
        Named::F1 => Key::F1,
        Named::F2 => Key::F2,
        Named::F3 => Key::F3,
        Named::F4 => Key::F4,
        Named::F5 => Key::F5,
        Named::F6 => Key::F6,
        Named::F7 => Key::F7,
        Named::F8 => Key::F8,
        Named::F9 => Key::F9,
        Named::F10 => Key::F10,
        Named::F11 => Key::F11,
        Named::F12 => Key::F12,
        Named::CapsLock => Key::CapsLock,
        Named::NumLock => Key::NumLock,
        Named::ScrollLock => Key::ScrollLock,
        Named::PrintScreen => Key::PrintScreen,
        Named::Pause => Key::Pause,
        Named::Menu => Key::ContextMenu,
        Named::ShiftLeft => Key::ShiftLeft,
        Named::ShiftRight => Key::ShiftRight,
        Named::CtrlLeft => Key::ControlLeft,
        Named::CtrlRight => Key::ControlRight,
        Named::AltLeft => Key::AltLeft,
        Named::AltRight => Key::AltRight,
        Named::SuperLeft => Key::MetaLeft,
        Named::SuperRight => Key::MetaRight,
    })
}

fn character_key(character: char) -> keys::Key {
    use keys::Key;
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] = [
        Key::Digit0,
        Key::Digit1,
        Key::Digit2,
        Key::Digit3,
        Key::Digit4,
        Key::Digit5,
        Key::Digit6,
        Key::Digit7,
        Key::Digit8,
        Key::Digit9,
    ];
    match character {
        'a'..='z' => LETTERS[usize::from(character as u8 - b'a')],
        '0'..='9' => DIGITS[usize::from(character as u8 - b'0')],
        '`' => Key::Backquote,
        '\\' => Key::Backslash,
        '[' => Key::BracketLeft,
        ']' => Key::BracketRight,
        ',' => Key::Comma,
        '=' => Key::Equal,
        '-' => Key::Minus,
        '.' => Key::Period,
        '\'' => Key::Quote,
        ';' => Key::Semicolon,
        '/' => Key::Slash,
        ' ' => Key::Space,
        _ => Key::Unidentified,
    }
}

fn button(field: &str, button: i32) -> Result<mouse::Button, Status> {
    match api::Button::try_from(button) {
        Ok(api::Button::Left) => Ok(mouse::Button::Left),
        Ok(api::Button::Middle) => Ok(mouse::Button::Middle),
        Ok(api::Button::Right) => Ok(mouse::Button::Right),
        Ok(api::Button::Unspecified) | Err(_) => {
            Err(refused(field, "is not one of LEFT, MIDDLE or RIGHT"))
        },
    }
}

fn mouse_heard(heard: api::Mouse) -> Result<Input, Status> {
    use api::mouse::Does;
    let at = |field: &str, cell: u32| {
        u16::try_from(cell).map_err(|_| refused(&format!("mouse.{field}"), "is over 65535"))
    };
    let (column, row) = (at("column", heard.column)?, at("row", heard.row)?);
    let mods = mods(heard.modifiers);
    Ok(match present("mouse.does", heard.does)? {
        Does::Press(pressed) => {
            Input::Mouse { click: Click::Press(button("mouse.press", pressed)?), mods, column, row }
        },
        Does::Release(released) => Input::Mouse {
            click: Click::Release(button("mouse.release", released)?),
            mods,
            column,
            row,
        },
        Does::Motion(api::Motion { held }) => {
            let held = held.map(|held| button("mouse.motion.held", held)).transpose()?;
            Input::Mouse { click: Click::Motion(held), mods, column, row }
        },
        Does::Wheel(wheel) => {
            let wheel = match api::Wheel::try_from(wheel) {
                Ok(api::Wheel::Up) => Wheel::Up,
                Ok(api::Wheel::Down) => Wheel::Down,
                Ok(api::Wheel::Left) => Wheel::Left,
                Ok(api::Wheel::Right) => Wheel::Right,
                Ok(api::Wheel::Unspecified) | Err(_) => {
                    return Err(refused("mouse.wheel", "is not one of UP, DOWN, LEFT or RIGHT"));
                },
            };
            Input::Wheel { wheel, mods, column, row }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> prost_types::Timestamp {
        prost_types::Timestamp { seconds: 60, nanos: 0 }
    }

    fn size(columns: u32, rows: u32) -> Result<Size, Status> {
        Size::parse("resize", Some(api::Size { columns, rows }))
    }

    #[test]
    fn a_size_is_of_1_to_1000_and_a_refusal_names_the_field() {
        assert_eq!(size(80, 24).unwrap().columns.get(), 80);
        assert_eq!(size(1000, 1000).unwrap().rows.get(), 1000);
        assert_eq!(size(80, 0).unwrap_err().message(), "resize.rows is 0");
        assert_eq!(size(1001, 1).unwrap_err().message(), "resize.columns is over 1000");
        assert_eq!(size(1, 65536).unwrap_err().message(), "resize.rows is over 1000");
        let missing = Size::parse("hello.size", None).unwrap_err();
        assert_eq!(missing.message(), "hello.size is missing");
    }

    #[test]
    fn a_name_is_held_to_its_rules() {
        assert_eq!(Name::parse("name", "work".to_owned()).unwrap().as_str(), "work");
        let refusal = |name: &str| Name::parse("name", name.to_owned()).unwrap_err();
        assert_eq!(refusal("").message(), "name is empty");
        assert_eq!(refusal("a\nb").message(), "name has a control character");
        assert_eq!(refusal(&"x".repeat(256)).message(), "name is over 255 bytes");
        assert_eq!(Name::parse("name", "src/main".to_owned()).unwrap().as_str(), "src/main");
    }

    #[test]
    fn a_session_s_name_is_one_luneta_reads() {
        let parsed = |name: &str| SessionName::parse("name", name.to_owned());
        assert_eq!(parsed("work").unwrap().as_str(), "work");
        assert_eq!(parsed("café . 2").unwrap().as_str(), "café . 2");
        let refused = [
            ("", "name is empty"),
            ("a\nb", "name has a control character"),
            (" \u{a0}", "name is only whitespace"),
            (".", "name is '.' or '..'"),
            ("..", "name is '.' or '..'"),
            ("a/b", "name has a '/'"),
        ];
        for (name, message) in refused {
            assert_eq!(parsed(name).unwrap_err().message(), message, "{name:?}");
        }
    }

    #[test]
    fn opens_needs_an_absolute_cwd_a_program_and_an_end() {
        let opens = |cwd: &str, runs, ends: api::Ends| {
            Opens::parse("opens", Some(api::Opens { cwd: cwd.to_owned(), runs, ends: ends.into() }))
        };
        let shell = Some(api::opens::Runs::Shell(api::Shell {}));
        let program = |argv: &[&str]| {
            let argv = argv.iter().map(|arg| (*arg).to_owned()).collect();
            Some(api::opens::Runs::Program(api::Program { argv }))
        };
        assert_eq!(opens("/", shell.clone(), api::Ends::Holds).unwrap().runs, Runs::Shell);
        let refused = [
            (opens("tmp", shell.clone(), api::Ends::Holds), "opens.cwd is not an absolute path"),
            (opens("/", None, api::Ends::Holds), "opens.runs is missing"),
            (opens("/", program(&[]), api::Ends::Holds), "opens.program.argv is empty"),
            (opens("/", program(&[""]), api::Ends::Holds), "opens.program.argv names no program"),
            (opens("/", program(&["ls", "a\0"]), api::Ends::Holds), "opens.program.argv has a NUL"),
            (opens("/\0", shell.clone(), api::Ends::Holds), "opens.cwd has a NUL"),
            (opens("/", shell, api::Ends::Unspecified), "opens.ends is not one of CLOSES or HOLDS"),
        ];
        for (opens, message) in refused {
            assert_eq!(opens.unwrap_err().message(), message);
        }
    }

    #[test]
    fn a_key_is_parsed_into_the_encoders_words() {
        let key = |press: api::Press, key, text: &str| {
            key_heard(api::Key {
                press: press.into(),
                modifiers: Some(api::Modifiers { ctrl: true, ..api::Modifiers::default() }),
                key,
                text: text.to_owned(),
            })
        };
        let a = Some(api::key::Key::Character('a'.into()));
        let Input::Key { action, key: code, mods, text, unshifted } =
            key(api::Press::Down, a, "").unwrap()
        else {
            panic!("not a key");
        };
        assert_eq!(
            (action, code, mods, text, unshifted),
            (keys::Action::Press, keys::Key::A, Mods::CTRL, None, Some('a'))
        );
        assert_eq!(
            key(api::Press::Unspecified, a, "").unwrap_err().message(),
            "key.press is not one of DOWN, REPEAT or UP",
        );
        assert_eq!(key(api::Press::Down, None, "").unwrap_err().message(), "key.key is missing");
        let surrogate = Some(api::key::Key::Character(0xd800));
        assert_eq!(
            key(api::Press::Down, surrogate, "").unwrap_err().message(),
            "key.character is not a Unicode scalar value",
        );
        assert_eq!(
            key(api::Press::Down, a, "\r").unwrap_err().message(),
            "key.text has a control character",
        );
    }

    #[test]
    fn only_the_first_message_is_a_hello() {
        let hello_said = api::ToServer {
            event: Some(api::to_server::Event::Hello(api::Hello {
                to: Some(api::Place { is: Some(api::place::Is::Session(1)), made_at: Some(at()) }),
                size: Some(api::Size { columns: 80, rows: 24 }),
                colours: None,
                runs_in: None,
            })),
        };
        let focus = api::ToServer {
            event: Some(api::to_server::Event::Focus(api::Focus { focused: true })),
        };
        let first = SessionId::parse("session", 1).unwrap();
        let made = SystemTime::try_from(at()).unwrap();
        let first = Place { is: Is::Session(first), made };
        assert_eq!(hello(hello_said.clone()).unwrap().to, first);
        assert_eq!(
            hello(focus.clone()).unwrap_err().message(),
            "event is not hello, as the first must be"
        );
        assert_eq!(heard(focus).unwrap(), Heard::Input(Input::Focus(true)));
        assert_eq!(
            heard(hello_said).unwrap_err().message(),
            "event is hello, which only the first may be"
        );
    }

    #[test]
    fn the_mouse_says_what_it_did() {
        let mouse = |does| mouse_heard(api::Mouse { modifiers: None, column: 3, row: 4, does });
        let wheel = mouse(Some(api::mouse::Does::Wheel(api::Wheel::Up.into()))).unwrap();
        assert_eq!(
            wheel,
            Input::Wheel { wheel: Wheel::Up, mods: Mods::empty(), column: 3, row: 4 }
        );
        let still = mouse(Some(api::mouse::Does::Motion(api::Motion { held: None }))).unwrap();
        assert_eq!(
            still,
            Input::Mouse { click: Click::Motion(None), mods: Mods::empty(), column: 3, row: 4 }
        );
        let unspecified = mouse(Some(api::mouse::Does::Press(api::Button::Unspecified.into())));
        assert_eq!(
            unspecified.unwrap_err().message(),
            "mouse.press is not one of LEFT, MIDDLE or RIGHT"
        );
    }
}

use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU16;
use std::path::Path;

use libghostty_vt::key::{self as keys, Mods};
use libghostty_vt::mouse;
use lune_protocol::made::Made;
use lune_protocol::program_resume::{ResumeArgv, ShellKind};
use tonic::Status;

use crate::api;
use crate::id::TerminalId;
use crate::screen::{Click, RgbColor, Wheel};

pub const SIDE_MAX: u16 = 1000;

pub const TAG_BYTES_MAX: usize = 256;

pub const SHOWN_MAX: usize = 1024;

pub const PRESSED_MAX: usize = 64;

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
pub struct AbsoluteCwd(String);

impl AbsoluteCwd {
    pub fn parse(field: &str, raw: String) -> Result<AbsoluteCwd, Status> {
        if raw.contains('\0') {
            return Err(refused(field, "has a NUL"));
        }
        if !Path::new(&raw).is_absolute() {
            return Err(refused(field, "is not an absolute path"));
        }
        Ok(AbsoluteCwd(raw))
    }

    #[must_use]
    pub fn of_path(raw: String) -> Option<AbsoluteCwd> {
        if raw.contains('\0') || !Path::new(&raw).is_absolute() {
            return None;
        }
        Some(AbsoluteCwd(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<Path> for AbsoluteCwd {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnSpec {
    pub cwd: AbsoluteCwd,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    Direct { program: String, args: Vec<String>, ends: Ends },
    ResumeInShell { shell: String, kind: ShellKind, resume: ResumeArgv },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opens {
    pub cwd: AbsoluteCwd,
    pub launch: Launch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ends {
    Closes,
    Holds,
}

impl Opens {
    pub fn parse(field: &str, opens: Option<api::Opens>) -> Result<Opens, Status> {
        let opens = present(field, opens)?;
        let cwd = AbsoluteCwd::parse(&format!("{field}.cwd"), opens.cwd)?;
        if let Some(resume) = opens.resume_in_shell {
            if opens.program.is_some() {
                return Err(refused(
                    &format!("{field}.program"),
                    "cannot be set with resume_in_shell",
                ));
            }
            if !matches!(api::Ends::try_from(opens.ends), Ok(api::Ends::Unspecified)) {
                return Err(refused(
                    &format!("{field}.ends"),
                    "cannot be set with resume_in_shell",
                ));
            }
            let shell_field = format!("{field}.resume_in_shell.shell");
            if resume.shell.is_empty() {
                return Err(refused(&shell_field, "is empty"));
            }
            if resume.shell.contains('\0') {
                return Err(refused(&shell_field, "has a NUL"));
            }
            let kind = ShellKind::of_path(&resume.shell)
                .ok_or_else(|| refused(&shell_field, "is not a supported shell"))?;
            let argv_field = format!("{field}.resume_in_shell.argv");
            let api::Program { argv } = present(&argv_field, resume.argv)?;
            let resume_argv =
                ResumeArgv::parse(argv).map_err(|why| refused(&argv_field, why))?;
            return Ok(Opens {
                cwd,
                launch: Launch::ResumeInShell { shell: resume.shell, kind, resume: resume_argv },
            });
        }
        let api::Program { argv } = present(&format!("{field}.program"), opens.program)?;
        let argv_field = format!("{field}.program.argv");
        let Some((program, args)) = argv.split_first() else {
            return Err(refused(&argv_field, "is empty"));
        };
        if program.is_empty() {
            return Err(refused(&argv_field, "names no program"));
        }
        if argv.iter().any(|arg| arg.contains('\0')) {
            return Err(refused(&argv_field, "has a NUL"));
        }
        let (program, args) = (program.clone(), args.to_vec());
        let ends = match api::Ends::try_from(opens.ends) {
            Ok(api::Ends::Closes) => Ends::Closes,
            Ok(api::Ends::Holds) => Ends::Holds,
            Ok(api::Ends::Unspecified) | Err(_) => {
                return Err(refused(&format!("{field}.ends"), "is not one of CLOSES or HOLDS"));
            },
        };
        Ok(Opens { cwd, launch: Launch::Direct { program, args, ends } })
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Press {
    pub terminal: TerminalKey,
    pub shown: u64,
    pub inputs: Vec<Input>,
}

impl Press {
    pub fn parse(request: api::PressRequest) -> Result<Press, Status> {
        let api::PressRequest { terminal, shown, pressed } = request;
        let terminal = TerminalKey::parse("terminal", terminal)?;
        let shown = present("shown", shown)?;
        if pressed.is_empty() {
            return Err(refused("pressed", "is empty"));
        }
        if pressed.len() > PRESSED_MAX {
            return Err(refused("pressed", format_args!("are over {PRESSED_MAX}")));
        }
        let inputs = pressed.into_iter().map(|api::Pressed { event }| {
            Ok(match present("pressed.event", event)? {
                api::pressed::Event::Key(key) => key_heard(key)?,
                api::pressed::Event::Paste(api::Paste { text }) => Input::Paste(text),
            })
        });
        let inputs = inputs.collect::<Result<Vec<Input>, Status>>()?;
        assert!((1..=PRESSED_MAX).contains(&inputs.len()), "{} inputs", inputs.len());
        Ok(Press { terminal, shown, inputs })
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TerminalKey {
    pub id: TerminalId,
    pub made: Made,
}

impl TerminalKey {
    pub fn parse(field: &str, key: Option<api::TerminalKey>) -> Result<TerminalKey, Status> {
        let key = present(field, key)?;
        let id = TerminalId::parse(&format!("{field}.id"), key.id)?;
        let made_at = format!("{field}.made_at");
        let made = Made::read(present(&made_at, key.made_at)?).map_err(|why| refused(&made_at, why))?;
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
pub enum ViewerHeard {
    Showing(BTreeSet<TerminalKey>),
    Typed(TerminalKey, Input),
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

pub fn viewer_heard(message: api::FromViewer) -> Result<ViewerHeard, Status> {
    use api::from_viewer::Event;
    Ok(match present("event", message.event)? {
        Event::Hello(_) => return Err(refused("event", "is hello, which only the first may be")),
        Event::Showing(api::Showing { terminals }) => {
            if terminals.len() > SHOWN_MAX {
                return Err(refused("showing.terminals", format_args!("are over {SHOWN_MAX}")));
            }
            let mut keys = BTreeSet::new();
            for key in terminals {
                let key = TerminalKey::parse("showing.terminals", Some(key))?;
                if !keys.insert(key) {
                    return Err(refused("showing.terminals", format_args!("repeat {}", key.id)));
                }
            }
            ViewerHeard::Showing(keys)
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
            ViewerHeard::Typed(TerminalKey::parse("typed.terminal", terminal)?, input)
        },
    })
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
    fn opens_needs_an_absolute_cwd_a_program_and_an_end() {
        let opens = |cwd: &str, program, ends: api::Ends| {
            let ends = ends.into();
            Opens::parse(
                "opens",
                Some(api::Opens { cwd: cwd.to_owned(), program, ends, resume_in_shell: None }),
            )
        };
        let program = |argv: &[&str]| {
            let argv = argv.iter().map(|arg| (*arg).to_owned()).collect();
            Some(api::Program { argv })
        };
        let shell = program(&["/bin/sh"]);
        let opened = opens("/", program(&["ls", "-l"]), api::Ends::Holds).unwrap();
        assert_eq!(
            opened.launch,
            Launch::Direct {
                program: "ls".to_owned(),
                args: vec!["-l".to_owned()],
                ends: Ends::Holds,
            }
        );
        let refused = [
            (opens("tmp", shell.clone(), api::Ends::Holds), "opens.cwd is not an absolute path"),
            (opens("/", None, api::Ends::Holds), "opens.program is missing"),
            (opens("/", program(&[]), api::Ends::Holds), "opens.program.argv is empty"),
            (opens("/", program(&[""]), api::Ends::Holds), "opens.program.argv names no program"),
            (opens("/", program(&["ls", "a\0"]), api::Ends::Holds), "opens.program.argv has a NUL"),
            (opens("/\0", shell.clone(), api::Ends::Holds), "opens.cwd has a NUL"),
            (opens("/", shell.clone(), api::Ends::Unspecified), "opens.ends is not one of CLOSES or HOLDS"),
        ];
        for (opens, message) in refused {
            assert_eq!(opens.unwrap_err().message(), message);
        }

        let resume = |shell_path: &str, argv, prog, ends: api::Ends| {
            Opens::parse(
                "opens",
                Some(api::Opens {
                    cwd: "/".to_owned(),
                    program: prog,
                    ends: ends.into(),
                    resume_in_shell: Some(api::opens::ResumeInShell {
                        shell: shell_path.to_owned(),
                        argv,
                    }),
                }),
            )
        };
        let parsed_resume =
            resume("/bin/bash", program(&["nvim"]), None, api::Ends::Unspecified).unwrap();
        assert_eq!(
            parsed_resume.launch,
            Launch::ResumeInShell {
                shell: "/bin/bash".to_owned(),
                kind: ShellKind::Bash,
                resume: ResumeArgv::parse(vec!["nvim".to_owned()]).unwrap(),
            }
        );
        assert_eq!(
            resume("/bin/bash", program(&["nvim"]), shell, api::Ends::Unspecified)
                .unwrap_err()
                .message(),
            "opens.program cannot be set with resume_in_shell"
        );
        assert_eq!(
            resume("/bin/bash", program(&["nvim"]), None, api::Ends::Holds)
                .unwrap_err()
                .message(),
            "opens.ends cannot be set with resume_in_shell"
        );
        assert_eq!(
            resume("/bin/tcsh", program(&["nvim"]), None, api::Ends::Unspecified)
                .unwrap_err()
                .message(),
            "opens.resume_in_shell.shell is not a supported shell"
        );
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
        let made_at = Some(prost_types::Timestamp { seconds: 60, nanos: 0 });
        let runs_in = Some(api::TerminalKey { id: 3, made_at });
        let hello = api::Greeting { colours: None, runs_in };
        let hello_said = api::FromViewer { event: Some(api::from_viewer::Event::Hello(hello)) };
        let showing = api::Showing { terminals: Vec::new() };
        let shows = api::FromViewer { event: Some(api::from_viewer::Event::Showing(showing)) };
        let runs_in = greeting(hello_said.clone()).unwrap().runs_in.unwrap();
        assert_eq!(runs_in.id.said(), 3);
        assert_eq!(
            greeting(shows.clone()).unwrap_err().message(),
            "event is not hello, as the first must be"
        );
        assert_eq!(viewer_heard(shows).unwrap(), ViewerHeard::Showing(BTreeSet::new()));
        assert_eq!(
            viewer_heard(hello_said).unwrap_err().message(),
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

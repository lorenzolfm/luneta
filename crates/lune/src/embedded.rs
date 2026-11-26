use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{self, BufRead as _, BufReader, ErrorKind, Read as _, Write as _};
use std::net::Shutdown;
use std::num::{NonZeroU16, NonZeroU32};
use std::os::unix::fs::symlink;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use lune_domain::projects::{BrowserUrl, ViewId};
use luned::log;
use luned::terminal_socket;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;
use serde::{Deserialize, Serialize};

use crate::answers::{CellPixels, Colours, WindowPixels};
use crate::host::Event;
use crate::machines::Machine;

pub const EMBEDDED_MAX: usize = 16;

const IMAGE_ID_BASE: u32 = 0x004c_0001;

const IMAGE_ID_SPAN: u32 = 0x0000_ffff;

const IMAGE_PIXELS_MAX: u32 = 4096;

const SHM_NAME_BYTES_MAX: usize = 256;

const POINTER_SHAPE_BYTES_MAX: usize = 32;

const CONTROL_LINE_BYTES_MAX: usize = 16 * 1024;

const GUEST_LINE_BYTES_MAX: usize = 1024 * 1024;

const GUEST_LINE_DRAIN_MAX: usize = 8 * 1024 * 1024;

const STDERR_BYTES_MAX: usize = 2048;

const STDERR_JOIN_WAIT: Duration = Duration::from_millis(500);

const TO_GUEST_QUEUE_MAX: usize = 256;

const ACCEPT_POLL: Duration = Duration::from_millis(50);

const ACCEPT_TIMEOUT: Duration = Duration::from_secs(15);

const ACCEPT_POLLS_MAX: u32 = 300;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

const HANDSHAKE_POLLS_MAX: u32 = 100;

const EXIT_WAIT: Duration = Duration::from_secs(2);

const EXIT_POLLS_MAX: u32 = 40;

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

const CONTROL_TIMEOUT: Duration = Duration::from_millis(300);

const CONTROL_SOCKETS_MAX: usize = 64;

const DIR_ENTRIES_MAX: usize = 1024;

const URL_POLL_GAP_MIN: Duration = Duration::from_millis(350);

const DIACRITICS_MAX: u16 = 297;

const PLACEHOLDER: char = '\u{10EEEE}';

const DEFAULT_CELL: CellPixels = CellPixels {
    width: NonZeroU16::new(10).expect("default cell width is non-zero"),
    height: NonZeroU16::new(20).expect("default cell height is non-zero"),
};

const MISSING_GUIDANCE: &str =
    "terminal-browser is not on PATH (install via `nix profile install` or github.com/zenbu-labs/terminal-browser)";

const ROW_COLUMN_DIACRITICS: [char; 297] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
    '\u{0659}', '\u{065A}', '\u{065B}', '\u{065D}', '\u{065E}', '\u{06D6}', '\u{06D7}', '\u{06D8}',
    '\u{06D9}', '\u{06DA}', '\u{06DB}', '\u{06DC}', '\u{06DF}', '\u{06E0}', '\u{06E1}', '\u{06E2}',
    '\u{06E4}', '\u{06E7}', '\u{06E8}', '\u{06EB}', '\u{06EC}', '\u{0730}', '\u{0732}', '\u{0733}',
    '\u{0735}', '\u{0736}', '\u{073A}', '\u{073D}', '\u{073F}', '\u{0740}', '\u{0741}', '\u{0743}',
    '\u{0745}', '\u{0747}', '\u{0749}', '\u{074A}', '\u{07EB}', '\u{07EC}', '\u{07ED}', '\u{07EE}',
    '\u{07EF}', '\u{07F0}', '\u{07F1}', '\u{07F3}', '\u{0816}', '\u{0817}', '\u{0818}', '\u{0819}',
    '\u{081B}', '\u{081C}', '\u{081D}', '\u{081E}', '\u{081F}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082A}', '\u{082B}', '\u{082C}',
    '\u{082D}', '\u{0951}', '\u{0953}', '\u{0954}', '\u{0F82}', '\u{0F83}', '\u{0F86}', '\u{0F87}',
    '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}',
    '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}', '\u{1B6B}', '\u{1B6D}',
    '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}',
    '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}',
    '\u{1DC5}', '\u{1DC6}', '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}',
    '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}', '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}',
    '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}', '\u{1DE0}', '\u{1DE1}',
    '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}', '\u{20D1}',
    '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}', '\u{20E7}',
    '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}',
    '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}', '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}',
    '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}', '\u{2DF2}',
    '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}',
    '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}',
    '\u{A6F0}', '\u{A6F1}', '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}', '\u{A8E5}',
    '\u{A8E6}', '\u{A8E7}', '\u{A8E8}', '\u{A8E9}', '\u{A8EA}', '\u{A8EB}', '\u{A8EC}', '\u{A8ED}',
    '\u{A8EE}', '\u{A8EF}', '\u{A8F0}', '\u{A8F1}', '\u{AAB0}', '\u{AAB2}', '\u{AAB3}', '\u{AAB7}',
    '\u{AAB8}', '\u{AABE}', '\u{AABF}', '\u{AAC1}', '\u{FE20}', '\u{FE21}', '\u{FE22}', '\u{FE23}',
    '\u{FE24}', '\u{FE25}', '\u{FE26}', '\u{10A0F}', '\u{10A38}', '\u{1D185}', '\u{1D186}',
    '\u{1D187}', '\u{1D188}', '\u{1D189}', '\u{1D1AA}', '\u{1D1AB}', '\u{1D1AC}', '\u{1D1AD}',
    '\u{1D242}', '\u{1D243}', '\u{1D244}',
];

const _: () = assert!(ROW_COLUMN_DIACRITICS.len() == DIACRITICS_MAX as usize);
const _: () = assert!(IMAGE_ID_BASE > 0);
const _: () = assert!(IMAGE_ID_BASE + IMAGE_ID_SPAN <= 0x00ff_ffff);
const _: () = assert!(EMBEDDED_MAX > 0);
const _: () = assert!(EMBEDDED_MAX < IMAGE_ID_SPAN as usize);
const _: () = assert!(IMAGE_PIXELS_MAX > 0);
const _: () = assert!(SHM_NAME_BYTES_MAX > 0);
const _: () = assert!(POINTER_SHAPE_BYTES_MAX > 0);
const _: () = assert!(CONTROL_LINE_BYTES_MAX > 0);
const _: () = assert!(GUEST_LINE_BYTES_MAX >= CONTROL_LINE_BYTES_MAX);
const _: () = assert!(GUEST_LINE_DRAIN_MAX >= GUEST_LINE_BYTES_MAX);
const _: () = assert!(STDERR_BYTES_MAX > 0);
const _: () = assert!(STDERR_JOIN_WAIT.as_millis() > 0);
const _: () = assert!(TO_GUEST_QUEUE_MAX > 0);
const _: () = assert!(CONTROL_SOCKETS_MAX > 0 && CONTROL_SOCKETS_MAX <= DIR_ENTRIES_MAX);
const _: () =
    assert!(ACCEPT_POLLS_MAX as u128 * ACCEPT_POLL.as_millis() == ACCEPT_TIMEOUT.as_millis());
const _: () =
    assert!(HANDSHAKE_POLLS_MAX as u128 * ACCEPT_POLL.as_millis() == HANDSHAKE_TIMEOUT.as_millis());
const _: () = assert!(EXIT_POLLS_MAX as u128 * ACCEPT_POLL.as_millis() == EXIT_WAIT.as_millis());
const _: () = assert!(WRITE_TIMEOUT.as_millis() > 0);

fn div_floor_nonzero(pixels: u16, cells: u16) -> Option<NonZeroU16> {
    if pixels == 0 || cells == 0 {
        return None;
    }
    let quotient = NonZeroU16::new(pixels / cells)?;
    assert!(u32::from(quotient.get()) * u32::from(cells) <= u32::from(pixels));
    Some(quotient)
}

#[must_use]
pub fn cell_pixels(
    cell: Option<CellPixels>,
    window_pixels: Option<WindowPixels>,
    columns: u16,
    rows: u16,
) -> CellPixels {
    if let Some(cell) = cell {
        cell
    } else if let Some(window) = window_pixels
        && let Some(width) = div_floor_nonzero(window.width.get(), columns)
        && let Some(height) = div_floor_nonzero(window.height.get(), rows)
    {
        CellPixels { width, height }
    } else if let Ok(window) = rustix::termios::tcgetwinsize(io::stdout())
        && let Some(width) = div_floor_nonzero(window.ws_xpixel, window.ws_col)
        && let Some(height) = div_floor_nonzero(window.ws_ypixel, window.ws_row)
    {
        CellPixels { width, height }
    } else {
        DEFAULT_CELL
    }
}

fn is_pointer_shape_byte(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_')
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerShape(String);

impl PointerShape {
    #[must_use]
    pub fn parse(raw: &str) -> Option<PointerShape> {
        if raw.is_empty() || raw.len() > POINTER_SHAPE_BYTES_MAX {
            return None;
        }
        if !raw.bytes().all(is_pointer_shape_byte) {
            return None;
        }
        let shape = PointerShape(raw.to_owned());
        assert_ne!(shape.0, "", "pointer shape is non-empty");
        assert!(shape.0.len() <= POINTER_SHAPE_BYTES_MAX, "pointer shape fits bound");
        Some(shape)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        assert_ne!(self.0, "", "pointer shape is non-empty");
        assert!(
            self.0.bytes().all(is_pointer_shape_byte),
            "pointer shape holds only safe OSC 22 token bytes"
        );
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    folder: PathBuf,
    terminal_socket: PathBuf,
}

impl Folder {
    #[must_use]
    pub fn new(folder: PathBuf) -> Option<Folder> {
        if folder.as_os_str().is_empty() {
            return None;
        }
        let terminal_socket = folder.join(terminal_socket::SOCKET);
        Some(Folder { folder, terminal_socket })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Unsupported,
    Supported(Folder),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub mode: Mode,
    pub cell: CellPixels,
    pub colours: Colours,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ImageId(u32);

impl ImageId {
    fn new(raw: u32) -> ImageId {
        assert!(
            (IMAGE_ID_BASE..=IMAGE_ID_BASE + IMAGE_ID_SPAN).contains(&raw),
            "image id stays within Kitty virtual placement range"
        );
        ImageId(raw)
    }

    fn get(self) -> u32 {
        self.0
    }

    fn rgb(self) -> (u8, u8, u8) {
        let [top, red, green, blue] = self.0.to_be_bytes();
        assert_eq!(top, 0, "Kitty virtual placement image id fits 24-bit RGB");
        (red, green, blue)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    shm: String,
    width: NonZeroU32,
    height: NonZeroU32,
    columns: NonZeroU16,
    rows: NonZeroU16,
    generation: u64,
}

impl Frame {
    fn parse(
        shm: String,
        width: u32,
        height: u32,
        columns: u16,
        rows: u16,
        generation: u64,
    ) -> Option<Frame> {
        if shm.is_empty() || shm.len() > SHM_NAME_BYTES_MAX || !shm.starts_with('/') {
            return None;
        }
        if shm.bytes().any(|byte| byte == 0 || byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return None;
        }
        let width = NonZeroU32::new(width).filter(|width| width.get() <= IMAGE_PIXELS_MAX)?;
        let height = NonZeroU32::new(height).filter(|height| height.get() <= IMAGE_PIXELS_MAX)?;
        let columns = NonZeroU16::new(columns)?;
        let rows = NonZeroU16::new(rows)?;
        Some(Frame { shm, width, height, columns, rows, generation })
    }

    #[must_use]
    pub fn shm(&self) -> &str {
        &self.shm
    }

    #[must_use]
    pub fn width(&self) -> NonZeroU32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> NonZeroU32 {
        self.height
    }

    #[must_use]
    pub fn columns(&self) -> NonZeroU16 {
        self.columns
    }

    #[must_use]
    pub fn rows(&self) -> NonZeroU16 {
        self.rows
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    Clean,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Frame(Frame),
    Title(Option<String>),
    Pointer(PointerShape),
    Clipboard(String),
    Url(Option<BrowserUrl>),
    Exited(Exit),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Copy(String),
    Navigated { view: ViewId, url: Option<BrowserUrl> },
    Close(ViewId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown<'a> {
    Live,
    Starting,
    AtLimit,
    Failed(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Desired<'a> {
    pub view: ViewId,
    pub url: Option<&'a BrowserUrl>,
    pub inner: Rect,
    pub visible: bool,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ToGuest {
    Init {
        #[serde(rename = "cols")]
        columns: u16,
        rows: u16,
        width: u32,
        height: u32,
        cell: [u16; 2],
        #[serde(rename = "imageId")]
        image_id: u32,
        transport: &'static str,
        focused: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        colors: Option<GuestColours>,
    },
    Size {
        #[serde(rename = "cols")]
        columns: u16,
        rows: u16,
        width: u32,
        height: u32,
        cell: [u16; 2],
    },
    Key {
        key: String,
        kind: &'static str,
        text: String,
        mods: GuestMods,
    },
    Paste {
        text: String,
    },
    Mouse {
        kind: &'static str,
        button: &'static str,
        mods: GuestMods,
        x: u32,
        y: u32,
    },
    Focus {
        focused: bool,
    },
    Visible {
        value: bool,
    },
    Colors {
        colors: GuestColours,
    },
}

#[derive(Clone, Copy, Serialize)]
struct GuestColours {
    foreground: [u8; 4],
    background: [u8; 4],
}

impl GuestColours {
    fn of(colours: Colours) -> Option<GuestColours> {
        let (foreground, background) = colours.both();
        if foreground.is_none() && background.is_none() {
            return None;
        }
        let [fg_red, fg_green, fg_blue] = foreground.unwrap_or([220, 224, 232]);
        let [bg_red, bg_green, bg_blue] = background.unwrap_or([24, 24, 34]);
        let guest = GuestColours {
            foreground: [fg_red, fg_green, fg_blue, 255],
            background: [bg_red, bg_green, bg_blue, 255],
        };
        assert_eq!(guest.foreground[3], 255);
        assert_eq!(guest.background[3], 255);
        Some(guest)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "matches terminal-browser PIXEL_EMBED JSON mods schema"
)]
struct GuestMods {
    shift: bool,
    alt: bool,
    ctrl: bool,
    #[serde(rename = "super")]
    super_key: bool,
}

impl GuestMods {
    fn of(modifiers: KeyModifiers) -> GuestMods {
        let mods = GuestMods {
            shift: modifiers.contains(KeyModifiers::SHIFT),
            alt: modifiers.contains(KeyModifiers::ALT),
            ctrl: modifiers.contains(KeyModifiers::CONTROL),
            super_key: modifiers.contains(KeyModifiers::SUPER),
        };
        assert_eq!(mods.shift, modifiers.contains(KeyModifiers::SHIFT));
        assert_eq!(mods.ctrl, modifiers.contains(KeyModifiers::CONTROL));
        mods
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum FromGuest {
    Join {},
    Frame {
        shm: String,
        width: u32,
        height: u32,
        #[serde(rename = "cols")]
        columns: u16,
        rows: u16,
        #[serde(default)]
        generation: u64,
    },
    Title {
        text: String,
    },
    Pointer {
        shape: String,
    },
    Clipboard {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    Hidden,
    Visible { focused: bool },
}

impl Presence {
    fn of(visible: bool, focused: bool) -> Presence {
        if visible { Presence::Visible { focused } } else { Presence::Hidden }
    }

    fn visible(self) -> bool {
        matches!(self, Presence::Visible { .. })
    }

    fn focused(self) -> bool {
        matches!(self, Presence::Visible { focused: true })
    }
}

struct SpawnSpec<'a> {
    view: ViewId,
    machine: &'a Machine,
    url: Option<&'a BrowserUrl>,
    columns: NonZeroU16,
    rows: NonZeroU16,
    presence: Presence,
}

struct Running {
    image_id: ImageId,
    columns: NonZeroU16,
    rows: NonZeroU16,
    presence: Presence,
    title: Option<String>,
    tty: PathBuf,
    socket: PathBuf,
    last_url_poll: Option<Instant>,
    url_poll_in_flight: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    sender: SyncSender<ToGuest>,
}

impl Running {
    fn send(&self, message: ToGuest) -> bool {
        let is_mouse = matches!(message, ToGuest::Mouse { .. });
        match self.sender.try_send(message) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                if !is_mouse {
                    log::say("embedded browser outbound queue full; dropping message");
                }
                true
            },
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        remove_if_present(&self.socket);
        remove_if_present(&self.tty);
    }
}

enum Instance {
    Starting(Running),
    Live {
        running: Running,
        frame: Frame,
        pointer: Option<PointerShape>,
    },
    Failed {
        error: String,
    },
}

impl Instance {
    fn running(&self) -> Option<&Running> {
        match self {
            Instance::Starting(running) | Instance::Live { running, .. } => Some(running),
            Instance::Failed { .. } => None,
        }
    }

    fn running_mut(&mut self) -> Option<&mut Running> {
        match self {
            Instance::Starting(running) | Instance::Live { running, .. } => Some(running),
            Instance::Failed { .. } => None,
        }
    }
}

struct FlagGuard(Arc<AtomicBool>);

impl Drop for FlagGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn remove_if_present(path: &Path) {
    if path.as_os_str().is_empty() {
        return;
    }
    if let Err(error) = fs::remove_file(path)
        && error.kind() != ErrorKind::NotFound
    {
        log::say(&format!("{}: embedded browser file not removed: {error}", path.display()));
    }
}

struct SupportedState {
    folder: Folder,
    views: BTreeMap<ViewId, Instance>,
    next_id: u32,
    next_socket: u64,
    pending_transmits: BTreeMap<ImageId, Frame>,
    pending_deletes: BTreeSet<ImageId>,
    owned_ids: BTreeSet<ImageId>,
    pending_ttys: Vec<(ViewId, Option<String>)>,
}

impl SupportedState {
    fn new(folder: Folder) -> SupportedState {
        SupportedState {
            folder,
            views: BTreeMap::new(),
            next_id: IMAGE_ID_BASE,
            next_socket: 0,
            pending_transmits: BTreeMap::new(),
            pending_deletes: BTreeSet::new(),
            owned_ids: BTreeSet::new(),
            pending_ttys: Vec::new(),
        }
    }

    fn spawn(
        &mut self,
        number: u64,
        cell: CellPixels,
        colours: Colours,
        events: &Sender<Event>,
        spec: &SpawnSpec<'_>,
    ) {
        assert!(!self.views.contains_key(&spec.view), "spawn does not overwrite an active view");
        assert!(self.views.len() < EMBEDDED_MAX, "spawn stays within EMBEDDED_MAX");
        self.next_socket = self.next_socket.wrapping_add(1);
        let pid = std::process::id();
        let socket = self.folder.folder.join(format!("b-{pid}-{}.sock", self.next_socket));
        let tty = terminal_socket::browser_client_tty_path(
            &self.folder.folder,
            spec.view,
            pid,
            self.next_socket,
        );
        remove_if_present(&socket);
        remove_if_present(&tty);
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) => {
                self.insert_failed(spec.view, format!("embed socket failed: {error}"));
                return;
            },
        };
        if let Err(error) = listener.set_nonblocking(true) {
            remove_if_present(&socket);
            self.insert_failed(spec.view, format!("embed socket setup failed: {error}"));
            return;
        }
        if let Err(error) = symlink("/dev/null", &tty) {
            remove_if_present(&socket);
            self.insert_failed(spec.view, format!("browser tty setup failed: {error}"));
            return;
        }
        let mut command =
            configure_spawn_command(spec, &socket, &tty, &self.folder.terminal_socket);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                remove_if_present(&socket);
                remove_if_present(&tty);
                self.insert_failed(spec.view, MISSING_GUIDANCE.to_owned());
                return;
            },
            Err(error) => {
                remove_if_present(&socket);
                remove_if_present(&tty);
                self.insert_failed(spec.view, format!("terminal-browser did not start: {error}"));
                return;
            },
        };
        let image_id = self.alloc_image_id();
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel(TO_GUEST_QUEUE_MAX);
        let (width, height) = pixel_size(spec.columns, spec.rows, cell);
        let init = ToGuest::Init {
            columns: spec.columns.get(),
            rows: spec.rows.get(),
            width,
            height,
            cell: [cell.width.get(), cell.height.get()],
            image_id: image_id.get(),
            transport: "host",
            focused: spec.presence.focused(),
            colors: GuestColours::of(colours),
        };
        let peer = Peer {
            number,
            view: spec.view,
            listener,
            init,
            receiver,
            events: events.clone(),
            stop: Arc::clone(&stop),
        };
        thread::spawn(move || run_peer(peer, Some(child)));
        let tty_string = tty.to_string_lossy().into_owned();
        let previous = self.views.insert(
            spec.view,
            Instance::Starting(Running {
                image_id,
                columns: spec.columns,
                rows: spec.rows,
                presence: spec.presence,
                title: None,
                tty,
                socket,
                last_url_poll: None,
                url_poll_in_flight: Arc::new(AtomicBool::new(false)),
                stop,
                sender,
            }),
        );
        assert!(previous.is_none(), "spawned view was not already present");
        self.pending_ttys.push((spec.view, Some(tty_string)));
    }

    fn insert_failed(&mut self, view: ViewId, error: String) {
        assert_ne!(error, "", "failure message is non-empty");
        assert!(self.views.len() < EMBEDDED_MAX, "failed instance stays within EMBEDDED_MAX");
        let previous = self.views.insert(view, Instance::Failed { error });
        assert!(previous.is_none(), "failed view did not overwrite an active view");
    }

    fn fail_instance(&mut self, view: ViewId, error: String) {
        assert_ne!(error, "", "failure message is non-empty");
        if let Some(previous) = self.views.insert(view, Instance::Failed { error })
            && let Some(running) = previous.running()
        {
            self.delete_image(running.image_id);
            self.pending_ttys.push((view, None));
        }
    }

    fn send_to(&mut self, view: ViewId, message: ToGuest) -> bool {
        let Some(running) = self.views.get(&view).and_then(Instance::running) else {
            return false;
        };
        if running.send(message) {
            return true;
        }
        self.fail_instance(view, "terminal-browser disconnected".to_owned());
        false
    }

    fn alloc_image_id(&mut self) -> ImageId {
        assert!(
            (IMAGE_ID_BASE..=IMAGE_ID_BASE + IMAGE_ID_SPAN).contains(&self.next_id),
            "next image id stays in range"
        );
        for _ in 0..=IMAGE_ID_SPAN {
            let candidate = ImageId::new(self.next_id);
            self.next_id = if self.next_id >= IMAGE_ID_BASE + IMAGE_ID_SPAN {
                IMAGE_ID_BASE
            } else {
                self.next_id + 1
            };
            let free = !self.owned_ids.contains(&candidate)
                && !self.pending_deletes.contains(&candidate)
                && !self
                    .views
                    .values()
                    .filter_map(Instance::running)
                    .any(|running| running.image_id == candidate);
            if free {
                return candidate;
            }
        }
        unreachable!("at most EMBEDDED_MAX views share IMAGE_ID_SPAN + 1 image ids")
    }

    fn remove(&mut self, view: ViewId) {
        if let Some(instance) = self.views.remove(&view)
            && let Some(running) = instance.running()
        {
            self.delete_image(running.image_id);
            self.pending_ttys.push((view, None));
        }
        assert!(!self.views.contains_key(&view), "removed view is gone");
    }

    fn clear(&mut self) {
        let ids: Vec<ViewId> = self.views.keys().copied().collect();
        for view in ids {
            self.remove(view);
        }
        assert!(self.views.is_empty(), "cleared browsers hold no views");
        assert!(self.owned_ids.is_empty(), "cleared browsers own no live images");
    }

    fn delete_image(&mut self, image_id: ImageId) {
        self.pending_transmits.remove(&image_id);
        if self.owned_ids.remove(&image_id) {
            self.pending_deletes.insert(image_id);
        }
        assert!(!self.pending_transmits.contains_key(&image_id));
    }
}

enum BrowsersMode {
    Unsupported,
    Supported(SupportedState),
}

pub struct Browsers {
    number: u64,
    cell: CellPixels,
    colours: Colours,
    events: Sender<Event>,
    mode: BrowsersMode,
}

impl Browsers {
    #[must_use]
    pub fn new(number: u64, config: Config, events: Sender<Event>) -> Browsers {
        let mode = match config.mode {
            Mode::Unsupported => BrowsersMode::Unsupported,
            Mode::Supported(runtime) => BrowsersMode::Supported(SupportedState::new(runtime)),
        };
        Browsers { number, cell: config.cell, colours: config.colours, events, mode }
    }

    #[must_use]
    pub fn supported(&self) -> bool {
        matches!(self.mode, BrowsersMode::Supported(_))
    }

    #[must_use]
    pub fn config(&self) -> Config {
        let mode = match &self.mode {
            BrowsersMode::Unsupported => Mode::Unsupported,
            BrowsersMode::Supported(state) => Mode::Supported(state.folder.clone()),
        };
        Config { mode, cell: self.cell, colours: self.colours }
    }

    pub fn drain_tty_changes(&mut self) -> Vec<(ViewId, Option<String>)> {
        match &mut self.mode {
            BrowsersMode::Unsupported => Vec::new(),
            BrowsersMode::Supported(state) => std::mem::take(&mut state.pending_ttys),
        }
    }

    #[must_use]
    pub fn active_ttys(&self) -> Vec<(ViewId, String)> {
        match &self.mode {
            BrowsersMode::Unsupported => Vec::new(),
            BrowsersMode::Supported(state) => state
                .views
                .iter()
                .filter_map(|(&view, instance)| {
                    let running = instance.running()?;
                    Some((view, running.tty.to_string_lossy().into_owned()))
                })
                .collect(),
        }
    }

    pub fn sync(
        &mut self,
        machine: &Machine,
        desired: &[Desired<'_>],
        focused: Option<ViewId>,
        window_focused: bool,
    ) {
        let cell = self.cell;
        let colours = self.colours;
        let number = self.number;
        let events = &self.events;
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return;
        };
        assert!(state.views.len() <= EMBEDDED_MAX, "embedded browsers stay within EMBEDDED_MAX");
        let wanted: BTreeSet<ViewId> = desired.iter().map(|item| item.view).collect();
        let removed: Vec<ViewId> =
            state.views.keys().copied().filter(|id| !wanted.contains(id)).collect();
        for view in removed {
            state.remove(view);
        }
        let mut disconnected = Vec::new();
        for item in desired {
            let columns = NonZeroU16::new(item.inner.width).unwrap_or(NonZeroU16::MIN);
            let rows = NonZeroU16::new(item.inner.height).unwrap_or(NonZeroU16::MIN);
            let presence = Presence::of(item.visible, window_focused && focused == Some(item.view));
            if let Some(instance) = state.views.get_mut(&item.view) {
                let Some(running) = instance.running_mut() else {
                    continue;
                };
                let mut alive = true;
                if running.columns != columns || running.rows != rows {
                    running.columns = columns;
                    running.rows = rows;
                    let (width, height) = pixel_size(columns, rows, cell);
                    alive &= running.send(ToGuest::Size {
                        columns: columns.get(),
                        rows: rows.get(),
                        width,
                        height,
                        cell: [cell.width.get(), cell.height.get()],
                    });
                }
                if alive && running.presence.visible() != presence.visible() {
                    alive &= running.send(ToGuest::Visible { value: presence.visible() });
                }
                if alive && running.presence.focused() != presence.focused() {
                    alive &= running.send(ToGuest::Focus { focused: presence.focused() });
                }
                running.presence = presence;
                if !alive {
                    disconnected.push(item.view);
                }
                continue;
            }
            if state.views.len() >= EMBEDDED_MAX {
                continue;
            }
            state.spawn(
                number,
                cell,
                colours,
                events,
                &SpawnSpec {
                    view: item.view,
                    machine,
                    url: item.url,
                    columns,
                    rows,
                    presence,
                },
            );
        }
        for view in disconnected {
            state.fail_instance(view, "terminal-browser disconnected".to_owned());
        }
        assert!(state.views.len() <= EMBEDDED_MAX, "embedded browsers stay within EMBEDDED_MAX");
    }

    pub fn retry(
        &mut self,
        view: ViewId,
        machine: &Machine,
        url: Option<&BrowserUrl>,
        inner: Rect,
        focused: bool,
    ) {
        let cell = self.cell;
        let colours = self.colours;
        let number = self.number;
        let events = &self.events;
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return;
        };
        let already_held = state.views.contains_key(&view);
        if !already_held && state.views.len() >= EMBEDDED_MAX {
            return;
        }
        state.remove(view);
        assert!(!state.views.contains_key(&view), "retried view was removed first");
        state.spawn(
            number,
            cell,
            colours,
            events,
            &SpawnSpec {
                view,
                machine,
                url,
                columns: NonZeroU16::new(inner.width).unwrap_or(NonZeroU16::MIN),
                rows: NonZeroU16::new(inner.height).unwrap_or(NonZeroU16::MIN),
                presence: Presence::Visible { focused },
            },
        );
        assert!(state.views.len() <= EMBEDDED_MAX, "embedded browsers stay within EMBEDDED_MAX");
    }

    pub fn clear(&mut self) {
        if let BrowsersMode::Supported(state) = &mut self.mode {
            state.clear();
        }
    }

    pub fn heard(&mut self, view: ViewId, heard: Heard) -> Option<Outcome> {
        let number = self.number;
        let events = self.events.clone();
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return None;
        };
        if !state.views.contains_key(&view) {
            return None;
        }
        match heard {
            Heard::Frame(frame) => {
                match state.views.get(&view) {
                    Some(Instance::Starting(_)) => {},
                    Some(Instance::Live { frame: previous, .. })
                        if frame.generation() >= previous.generation() => {},
                    Some(Instance::Live { .. } | Instance::Failed { .. }) | None => return None,
                }
                let previous = state.views.remove(&view).expect("checked view exists");
                let (running, pointer) = match previous {
                    Instance::Starting(running) => (running, None),
                    Instance::Live { running, pointer, .. } => (running, pointer),
                    Instance::Failed { .. } => unreachable!("failed instances return early above"),
                };
                let image_id = running.image_id;
                assert!(
                    !state.pending_deletes.contains(&image_id),
                    "active browser image id is not queued for deletion"
                );
                state.pending_transmits.insert(image_id, frame.clone());
                state.owned_ids.insert(image_id);
                state.views.insert(view, Instance::Live { running, frame, pointer });
                assert!(state.owned_ids.len() <= EMBEDDED_MAX);
                assert!(state.pending_transmits.len() <= EMBEDDED_MAX);
                None
            },
            Heard::Title(title) => {
                let running = state.views.get_mut(&view)?.running_mut()?;
                let changed = running.title != title;
                running.title = title;
                if changed {
                    let now = Instant::now();
                    let due = running.last_url_poll.is_none_or(|last| {
                        now.saturating_duration_since(last) >= URL_POLL_GAP_MIN
                    });
                    if due
                        && running
                            .url_poll_in_flight
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                    {
                        running.last_url_poll = Some(now);
                        let tty = running.tty.clone();
                        let in_flight = Arc::clone(&running.url_poll_in_flight);
                        thread::spawn(move || {
                            let _guard = FlagGuard(in_flight);
                            let navigated = match query_url_for_tty(&tty) {
                                QueriedUrl::StartPage => Some(None),
                                QueriedUrl::Url(url) => Some(Some(url)),
                                QueriedUrl::Missing | QueriedUrl::Unpersistable => None,
                            };
                            if let Some(url) = navigated
                                && events
                                    .send(Event::Browser { number, view, heard: Heard::Url(url) })
                                    .is_err()
                            {
                                log::say("embedded browser URL reply dropped on closed channel");
                            }
                        });
                    }
                }
                None
            },
            Heard::Pointer(shape) => {
                if let Some(Instance::Live { pointer, .. }) = state.views.get_mut(&view) {
                    *pointer = Some(shape);
                }
                None
            },
            Heard::Clipboard(text) => {
                state.views.get(&view)?.running()?;
                Some(Outcome::Copy(text))
            },
            Heard::Url(url) => {
                state.views.get(&view)?.running()?;
                Some(Outcome::Navigated { view, url })
            },
            Heard::Exited(Exit::Clean) => {
                state.remove(view);
                Some(Outcome::Close(view))
            },
            Heard::Exited(Exit::Failed(why)) => {
                state.fail_instance(view, why);
                None
            },
        }
    }

    pub fn key(&mut self, view: ViewId, key: &KeyEvent) -> bool {
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return false;
        };
        if state.views.get(&view).and_then(Instance::running).is_none() {
            return false;
        }
        if let Some(message) = encode_key(key) {
            let _ = state.send_to(view, message);
        }
        true
    }

    pub fn paste(&mut self, view: ViewId, text: &str) -> bool {
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return false;
        };
        state.send_to(view, ToGuest::Paste { text: text.to_owned() })
    }

    pub fn mouse(&mut self, view: ViewId, mouse: MouseEvent, column: u16, row: u16) -> bool {
        let cell = self.cell;
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return false;
        };
        state.send_to(view, encode_mouse(mouse, column, row, cell))
    }

    pub fn colours(&mut self, colours: Colours) {
        self.colours = colours;
        let Some(guest) = GuestColours::of(colours) else {
            return;
        };
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return;
        };
        let mut disconnected = Vec::new();
        for (&view, instance) in &state.views {
            if let Some(running) = instance.running()
                && !running.send(ToGuest::Colors { colors: guest })
            {
                disconnected.push(view);
            }
        }
        for view in disconnected {
            state.fail_instance(view, "terminal-browser disconnected".to_owned());
        }
    }

    pub fn cell_resized(&mut self, cell: CellPixels) {
        if self.cell == cell {
            return;
        }
        self.cell = cell;
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return;
        };
        let mut disconnected = Vec::new();
        for (&view, instance) in &state.views {
            if let Some(running) = instance.running() {
                let (width, height) = pixel_size(running.columns, running.rows, cell);
                if !running.send(ToGuest::Size {
                    columns: running.columns.get(),
                    rows: running.rows.get(),
                    width,
                    height,
                    cell: [cell.width.get(), cell.height.get()],
                }) {
                    disconnected.push(view);
                }
            }
        }
        for view in disconnected {
            state.fail_instance(view, "terminal-browser disconnected".to_owned());
        }
    }

    #[must_use]
    pub fn title(&self, view: ViewId) -> Option<&str> {
        let BrowsersMode::Supported(state) = &self.mode else {
            return None;
        };
        state.views.get(&view)?.running()?.title.as_deref()
    }

    #[must_use]
    pub fn pointer(&self, view: ViewId) -> Option<&PointerShape> {
        let BrowsersMode::Supported(state) = &self.mode else {
            return None;
        };
        match state.views.get(&view)? {
            Instance::Live { pointer, .. } => pointer.as_ref(),
            Instance::Starting(_) | Instance::Failed { .. } => None,
        }
    }

    #[must_use]
    pub fn draw(&self, view: ViewId, buffer: &mut Buffer, seen: Rect) -> Shown<'_> {
        let BrowsersMode::Supported(state) = &self.mode else {
            return Shown::Starting;
        };
        match state.views.get(&view) {
            Some(Instance::Live { running, frame, .. }) => {
                paint_placeholders(
                    buffer,
                    seen,
                    running.image_id,
                    frame.columns().get(),
                    frame.rows().get(),
                );
                Shown::Live
            },
            Some(Instance::Failed { error }) => Shown::Failed(error),
            None if state.views.len() >= EMBEDDED_MAX => Shown::AtLimit,
            Some(Instance::Starting(_)) | None => Shown::Starting,
        }
    }

    pub fn drain_graphics(&mut self) -> Vec<u8> {
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return Vec::new();
        };
        assert!(state.pending_transmits.len() <= EMBEDDED_MAX);
        let mut out = Vec::new();
        for id in std::mem::take(&mut state.pending_deletes) {
            append_kitty_delete(&mut out, id);
        }
        for (id, frame) in std::mem::take(&mut state.pending_transmits) {
            append_kitty_transmit(&mut out, id, &frame);
        }
        assert!(state.pending_deletes.is_empty() && state.pending_transmits.is_empty());
        out
    }

    pub fn drain_all_deletes(&mut self) -> Vec<u8> {
        let BrowsersMode::Supported(state) = &mut self.mode else {
            return Vec::new();
        };
        state.pending_transmits.clear();
        let mut ids = std::mem::take(&mut state.pending_deletes);
        ids.extend(std::mem::take(&mut state.owned_ids));
        let mut out = Vec::new();
        for id in ids {
            append_kitty_delete(&mut out, id);
        }
        assert!(state.pending_deletes.is_empty() && state.owned_ids.is_empty());
        out
    }
}

impl Drop for Browsers {
    fn drop(&mut self) {
        self.clear();
    }
}

fn configure_spawn_command(
    spec: &SpawnSpec<'_>,
    socket: &Path,
    tty: &Path,
    terminal_socket: &Path,
) -> Command {
    let mut command = Command::new("terminal-browser");
    command.arg("open");
    if let Machine::Remote(destination) = spec.machine {
        command.arg(format!("--ssh={destination}"));
    }
    if let Some(url) = spec.url {
        command.arg(url.as_str());
    }
    command
        .env_remove("PIXEL_PANE")
        .env("PIXEL_EMBED", socket)
        .env("PIXEL_TTY", tty)
        .env("TERMINAL_BROWSER_START_PAGE", "1")
        .env("TERMINAL_BROWSER_COPY_ON_SELECT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0);
    match spec.machine {
        Machine::Local => {
            command.env("TERMINAL_BROWSER_TERMINAL_SOCKET", terminal_socket);
        },
        Machine::Remote(_) => {
            command.env_remove("TERMINAL_BROWSER_TERMINAL_SOCKET");
        },
    }
    command
}

#[derive(Clone, Copy)]
struct PeerTimeouts {
    accept_polls_max: u32,
    accept_timeout: Duration,
    handshake_polls_max: u32,
    handshake_timeout: Duration,
    exit_polls_max: u32,
}

impl PeerTimeouts {
    const DEFAULT: PeerTimeouts = PeerTimeouts {
        accept_polls_max: ACCEPT_POLLS_MAX,
        accept_timeout: ACCEPT_TIMEOUT,
        handshake_polls_max: HANDSHAKE_POLLS_MAX,
        handshake_timeout: HANDSHAKE_TIMEOUT,
        exit_polls_max: EXIT_POLLS_MAX,
    };
}

enum Accepted {
    Connected(UnixStream),
    Stopped,
    Exited(ExitStatus),
    Failed(String),
}

#[derive(Debug)]
enum BoundedLine {
    Eof,
    Line(String),
    Oversized,
}

struct Peer {
    number: u64,
    view: ViewId,
    listener: UnixListener,
    init: ToGuest,
    receiver: mpsc::Receiver<ToGuest>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
}

fn kill_and_wait(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => return,
        Ok(None) => {},
        Err(error) => {
            log::say(&format!("terminal-browser status check failed: {error}"));
        },
    }
    if let Some(pid) = i32::try_from(child.id()).ok().and_then(rustix::process::Pid::from_raw)
        && let Err(error) = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL)
        && error != rustix::io::Errno::SRCH
    {
        log::say(&format!("terminal-browser process group kill failed: {error}"));
    }
    if let Err(error) = child.kill()
        && error.kind() != ErrorKind::InvalidInput
    {
        log::say(&format!("terminal-browser kill failed: {error}"));
    }
    if let Err(error) = child.wait() {
        log::say(&format!("terminal-browser wait failed: {error}"));
    }
}

fn collect_stderr(stderr: Option<mpsc::Receiver<String>>) -> String {
    stderr.and_then(|rx| rx.recv_timeout(STDERR_JOIN_WAIT).ok()).unwrap_or_default()
}

fn wait_for_child(
    child: &mut Child,
    stop: &AtomicBool,
    exit_polls_max: u32,
) -> io::Result<Option<ExitStatus>> {
    for poll in 0..exit_polls_max {
        assert!(poll < exit_polls_max);
        if stop.load(Ordering::Relaxed) {
            kill_and_wait(child);
            return Ok(None);
        }
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        thread::sleep(ACCEPT_POLL);
    }
    kill_and_wait(child);
    Ok(None)
}

fn run_peer(peer: Peer, child: Option<Child>) {
    run_peer_with_timeouts(peer, child, PeerTimeouts::DEFAULT);
}

fn run_peer_with_timeouts(peer: Peer, mut child: Option<Child>, timeouts: PeerTimeouts) {
    let Peer { number, view, listener, init, receiver, events, stop } = peer;
    let stderr = child.as_mut().and_then(|child| child.stderr.take()).map(|mut err| {
        let (tx, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut buffer = Vec::new();
            if let Err(error) = err.by_ref().take(STDERR_BYTES_MAX as u64).read_to_end(&mut buffer)
            {
                log::say(&format!("terminal-browser stderr read failed: {error}"));
            }
            assert!(buffer.len() <= STDERR_BYTES_MAX);
            let _ = tx.send(String::from_utf8_lossy(&buffer).trim().to_owned());
        });
        rx
    });
    match accept_peer(&listener, &stop, child.as_mut(), timeouts) {
        Accepted::Connected(stream) => {
            serve_stream(stream, number, view, &init, receiver, &events, &stop, timeouts);
        },
        Accepted::Stopped => {
            if let Some(mut child) = child {
                kill_and_wait(&mut child);
            }
            return;
        },
        Accepted::Exited(status) => {
            let stderr_text = collect_stderr(stderr);
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let exit = if status.success() {
                Exit::Clean
            } else {
                let message = if stderr_text.is_empty() {
                    format!("terminal-browser exited before connecting ({status})")
                } else {
                    stderr_text
                };
                Exit::Failed(message)
            };
            if events.send(Event::Browser { number, view, heard: Heard::Exited(exit) }).is_err() {
                log::say("embedded browser exit event dropped on closed channel");
            }
            return;
        },
        Accepted::Failed(why) => {
            if let Some(mut child) = child {
                kill_and_wait(&mut child);
            }
            let stderr_text = collect_stderr(stderr);
            let detail = if stderr_text.is_empty() { why } else { stderr_text };
            if !stop.load(Ordering::Relaxed)
                && events
                    .send(Event::Browser { number, view, heard: Heard::Exited(Exit::Failed(detail)) })
                    .is_err()
            {
                log::say("embedded browser exit event dropped on closed channel");
            }
            return;
        },
    }
    if let Some(mut child) = child {
        if stop.load(Ordering::Relaxed) {
            kill_and_wait(&mut child);
            return;
        }
        let status = wait_for_child(&mut child, &stop, timeouts.exit_polls_max);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let stderr_text = collect_stderr(stderr);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let exit = match status {
            Ok(Some(status)) if status.success() => Exit::Clean,
            Ok(Some(status)) => {
                let message = if stderr_text.is_empty() {
                    format!("terminal-browser exited ({status})")
                } else {
                    stderr_text
                };
                Exit::Failed(message)
            },
            Ok(None) => {
                let message = if stderr_text.is_empty() {
                    "terminal-browser stopped responding".to_owned()
                } else {
                    stderr_text
                };
                Exit::Failed(message)
            },
            Err(error) => {
                kill_and_wait(&mut child);
                Exit::Failed(format!("terminal-browser wait failed: {error}"))
            },
        };
        if events.send(Event::Browser { number, view, heard: Heard::Exited(exit) }).is_err() {
            log::say("embedded browser exit event dropped on closed channel");
        }
    } else if !stop.load(Ordering::Relaxed)
        && events
            .send(Event::Browser { number, view, heard: Heard::Exited(Exit::Clean) })
            .is_err()
    {
        log::say("embedded browser exit event dropped on closed channel");
    }
}

fn accept_peer(
    listener: &UnixListener,
    stop: &AtomicBool,
    mut child: Option<&mut Child>,
    timeouts: PeerTimeouts,
) -> Accepted {
    let deadline = Instant::now() + timeouts.accept_timeout;
    for poll in 0..timeouts.accept_polls_max {
        assert!(poll < timeouts.accept_polls_max);
        if stop.load(Ordering::Relaxed) {
            return Accepted::Stopped;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = stream.set_nonblocking(false) {
                    return Accepted::Failed(format!("embed stream setup failed: {error}"));
                }
                return Accepted::Connected(stream);
            },
            Err(error) if error.kind() == ErrorKind::WouldBlock => {},
            Err(error) => return Accepted::Failed(format!("embed accept failed: {error}")),
        }
        if let Some(child) = child.as_deref_mut() {
            match child.try_wait() {
                Ok(Some(status)) => return Accepted::Exited(status),
                Ok(None) => {},
                Err(error) => {
                    return Accepted::Failed(format!("terminal-browser wait check failed: {error}"));
                },
            }
        }
        if Instant::now() >= deadline {
            return Accepted::Failed("terminal-browser did not connect in time".to_owned());
        }
        thread::sleep(ACCEPT_POLL);
    }
    Accepted::Failed("terminal-browser did not connect in time".to_owned())
}

fn serve_stream(
    stream: UnixStream,
    number: u64,
    view: ViewId,
    init: &ToGuest,
    receiver: mpsc::Receiver<ToGuest>,
    events: &Sender<Event>,
    stop: &Arc<AtomicBool>,
    timeouts: PeerTimeouts,
) {
    if let Err(error) = stream.set_read_timeout(Some(ACCEPT_POLL)) {
        log::say(&format!("embedded browser handshake read timeout failed: {error}"));
        return;
    }
    if let Err(error) = stream.set_write_timeout(Some(WRITE_TIMEOUT)) {
        log::say(&format!("embedded browser write timeout failed: {error}"));
        return;
    }
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(error) => {
            log::say(&format!("embedded browser stream clone failed: {error}"));
            return;
        },
    };
    let mut reader = BufReader::new(stream);
    let first = match read_handshake_line(
        &mut reader,
        stop,
        timeouts.handshake_polls_max,
        timeouts.handshake_timeout,
    ) {
        Ok(Some(line)) => line,
        Ok(None) => return,
        Err(error) => {
            log::say(&format!("embedded browser handshake read failed: {error}"));
            return;
        },
    };
    if !matches!(serde_json::from_str::<FromGuest>(&first), Ok(FromGuest::Join {})) {
        log::say("embedded browser sent unexpected handshake");
        return;
    }
    if let Err(error) = write_json_line(&mut writer, init) {
        log::say(&format!("embedded browser init write failed: {error}"));
        return;
    }
    if let Err(error) = reader.get_ref().set_read_timeout(None) {
        log::say(&format!("embedded browser read timeout clear failed: {error}"));
        return;
    }
    let write_stop = Arc::clone(stop);
    let shutdown_stream = match writer.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            log::say(&format!("embedded browser shutdown handle clone failed: {error}"));
            return;
        },
    };
    thread::scope(|scope| {
        scope.spawn(move || {
            for message in receiver {
                if write_stop.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(error) = write_json_line(&mut writer, &message) {
                    log::say(&format!("embedded browser write failed: {error}"));
                    break;
                }
            }
            if let Err(error) = shutdown_stream.shutdown(Shutdown::Both)
                && error.kind() != ErrorKind::NotConnected
            {
                log::say(&format!("embedded browser stream shutdown failed: {error}"));
            }
        });
        loop {
            let line = match read_bounded_line(
                &mut reader,
                GUEST_LINE_BYTES_MAX,
                GUEST_LINE_DRAIN_MAX,
            ) {
                Ok(BoundedLine::Line(line)) => line,
                Ok(BoundedLine::Oversized) => {
                    log::say(&format!(
                        "embedded browser message exceeded {GUEST_LINE_BYTES_MAX} bytes; skipping"
                    ));
                    continue;
                },
                Ok(BoundedLine::Eof) => break,
                Err(error) => {
                    if !stop.load(Ordering::Relaxed) {
                        log::say(&format!("embedded browser read failed: {error}"));
                    }
                    break;
                },
            };
            if stop.load(Ordering::Relaxed) {
                break;
            }
            assert!(line.len() <= GUEST_LINE_BYTES_MAX);
            let message = match serde_json::from_str::<FromGuest>(&line) {
                Ok(message) => message,
                Err(error) => {
                    log::say(&format!("embedded browser sent invalid message: {error}"));
                    continue;
                },
            };
            let heard = match message {
                FromGuest::Frame { shm, width, height, columns, rows, generation } => {
                    let Some(frame) = Frame::parse(shm, width, height, columns, rows, generation)
                    else {
                        log::say("embedded browser sent invalid frame dimensions or shm");
                        continue;
                    };
                    Heard::Frame(frame)
                },
                FromGuest::Title { text } => Heard::Title((!text.is_empty()).then_some(text)),
                FromGuest::Pointer { shape } => {
                    let Some(shape) = PointerShape::parse(&shape) else {
                        log::say("embedded browser sent invalid pointer shape");
                        continue;
                    };
                    Heard::Pointer(shape)
                },
                FromGuest::Clipboard { text } => Heard::Clipboard(text),
                FromGuest::Join {} | FromGuest::Other => continue,
            };
            if events.send(Event::Browser { number, view, heard }).is_err() {
                break;
            }
        }
    });
}

fn read_handshake_line(
    reader: &mut BufReader<UnixStream>,
    stop: &AtomicBool,
    polls_max: u32,
    timeout: Duration,
) -> io::Result<Option<String>> {
    let mut buffer = Vec::new();
    let deadline = Instant::now() + timeout;
    let mut polls = 0u32;
    let iterations_max = polls_max.saturating_add(CONTROL_LINE_BYTES_MAX as u32);
    for _ in 0..iterations_max {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                polls += 1;
                if polls >= polls_max || Instant::now() >= deadline {
                    return Err(io::Error::new(
                        ErrorKind::TimedOut,
                        "embedded browser handshake timed out",
                    ));
                }
                continue;
            },
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            if buffer.is_empty() {
                return Ok(None);
            }
            assert!(buffer.len() <= CONTROL_LINE_BYTES_MAX);
            return Ok(Some(String::from_utf8_lossy(&buffer).into_owned()));
        }
        if let Some(newline) = available.iter().position(|&byte| byte == b'\n') {
            if buffer.len().saturating_add(newline) > CONTROL_LINE_BYTES_MAX {
                reader.consume(newline + 1);
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("embedded browser line exceeded {CONTROL_LINE_BYTES_MAX} bytes"),
                ));
            }
            buffer.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            assert!(buffer.len() <= CONTROL_LINE_BYTES_MAX);
            return Ok(Some(String::from_utf8_lossy(&buffer).into_owned()));
        }
        let chunk_len = available.len();
        if buffer.len().saturating_add(chunk_len) > CONTROL_LINE_BYTES_MAX {
            reader.consume(chunk_len);
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("embedded browser line exceeded {CONTROL_LINE_BYTES_MAX} bytes"),
            ));
        }
        buffer.extend_from_slice(available);
        reader.consume(chunk_len);
    }
    Err(io::Error::new(ErrorKind::TimedOut, "embedded browser handshake timed out"))
}

fn read_bounded_line(
    reader: &mut BufReader<UnixStream>,
    max_bytes: usize,
    drain_max: usize,
) -> io::Result<BoundedLine> {
    assert!(max_bytes > 0, "line byte limit is positive");
    assert!(drain_max >= max_bytes, "drain limit is at least line byte limit");
    let mut buffer = Vec::new();
    let mut total_read = 0usize;
    let mut oversized = false;
    while total_read <= drain_max {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if oversized {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("embedded browser line exceeded {max_bytes} bytes"),
                ));
            }
            if buffer.is_empty() {
                return Ok(BoundedLine::Eof);
            }
            assert!(buffer.len() <= max_bytes);
            return Ok(BoundedLine::Line(String::from_utf8_lossy(&buffer).into_owned()));
        }
        if let Some(newline) = available.iter().position(|&byte| byte == b'\n') {
            let line_len = total_read.saturating_add(newline);
            if !oversized && line_len <= max_bytes {
                buffer.extend_from_slice(&available[..newline]);
                reader.consume(newline + 1);
                assert!(buffer.len() <= max_bytes);
                return Ok(BoundedLine::Line(String::from_utf8_lossy(&buffer).into_owned()));
            }
            reader.consume(newline + 1);
            if line_len > drain_max {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("embedded browser line exceeded {drain_max} bytes"),
                ));
            }
            return Ok(BoundedLine::Oversized);
        }
        let chunk_len = available.len();
        total_read = total_read.saturating_add(chunk_len);
        if total_read > drain_max {
            reader.consume(chunk_len);
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("embedded browser line exceeded {drain_max} bytes"),
            ));
        }
        if total_read > max_bytes {
            oversized = true;
            buffer.clear();
        } else if !oversized {
            buffer.extend_from_slice(available);
        }
        reader.consume(chunk_len);
    }
    Err(io::Error::new(
        ErrorKind::InvalidData,
        format!("embedded browser line exceeded {drain_max} bytes"),
    ))
}

fn write_json_line(writer: &mut impl io::Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    assert!(!line.contains('\n'), "serialized JSON line holds no embedded newline");
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

fn pixel_size(columns: NonZeroU16, rows: NonZeroU16, cell: CellPixels) -> (u32, u32) {
    let width = (u32::from(columns.get()) * u32::from(cell.width.get())).min(IMAGE_PIXELS_MAX);
    let height = (u32::from(rows.get()) * u32::from(cell.height.get())).min(IMAGE_PIXELS_MAX);
    assert!((1..=IMAGE_PIXELS_MAX).contains(&width));
    assert!((1..=IMAGE_PIXELS_MAX).contains(&height));
    (width, height)
}

fn append_kitty_transmit(out: &mut Vec<u8>, image_id: ImageId, frame: &Frame) {
    let payload = base64::engine::general_purpose::STANDARD.encode(frame.shm().as_bytes());
    write!(
        out,
        "\x1b_Ga=T,f=32,s={},v={},t=s,i={},U=1,c={},r={},q=2;{}\x1b\\",
        frame.width(),
        frame.height(),
        image_id.get(),
        frame.columns(),
        frame.rows(),
        payload
    )
    .expect("writing to Vec<u8> is infallible");
}

fn append_kitty_delete(out: &mut Vec<u8>, image_id: ImageId) {
    write!(out, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", image_id.get())
        .expect("writing to Vec<u8> is infallible");
}

fn paint_placeholders(buffer: &mut Buffer, seen: Rect, image_id: ImageId, columns: u16, rows: u16) {
    let (red, green, blue) = image_id.rgb();
    let draw_columns = columns.min(seen.width).min(DIACRITICS_MAX);
    let draw_rows = rows.min(seen.height).min(DIACRITICS_MAX);
    assert!(usize::from(draw_columns) <= ROW_COLUMN_DIACRITICS.len());
    assert!(usize::from(draw_rows) <= ROW_COLUMN_DIACRITICS.len());
    let foreground = Color::Rgb(red, green, blue);
    let mut glyph = String::with_capacity(12);
    for row in 0..draw_rows {
        let row_diacritic = ROW_COLUMN_DIACRITICS[usize::from(row)];
        for column in 0..draw_columns {
            let Some(cell) = buffer.cell_mut((seen.x + column, seen.y + row)) else {
                continue;
            };
            glyph.clear();
            glyph.push(PLACEHOLDER);
            glyph.push(row_diacritic);
            glyph.push(ROW_COLUMN_DIACRITICS[usize::from(column)]);
            cell.reset();
            cell.set_symbol(&glyph);
            cell.set_fg(foreground);
            cell.set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
        }
    }
}

fn encode_key(key: &KeyEvent) -> Option<ToGuest> {
    let kind = match key.kind {
        KeyEventKind::Press => "press",
        KeyEventKind::Repeat => "repeat",
        KeyEventKind::Release => "release",
    };
    let mut mods = GuestMods::of(key.modifiers);
    let (name, text) = match key.code {
        KeyCode::Char(character) => {
            let spelled = character.to_string();
            let text = if mods.ctrl || mods.alt || mods.super_key {
                String::new()
            } else {
                spelled.clone()
            };
            (spelled, text)
        },
        KeyCode::Up => ("up".to_owned(), String::new()),
        KeyCode::Down => ("down".to_owned(), String::new()),
        KeyCode::Left => ("left".to_owned(), String::new()),
        KeyCode::Right => ("right".to_owned(), String::new()),
        KeyCode::Home => ("home".to_owned(), String::new()),
        KeyCode::End => ("end".to_owned(), String::new()),
        KeyCode::Insert => ("insert".to_owned(), String::new()),
        KeyCode::PageUp => ("pageup".to_owned(), String::new()),
        KeyCode::PageDown => ("pagedown".to_owned(), String::new()),
        KeyCode::Enter => ("enter".to_owned(), String::new()),
        KeyCode::Backspace => ("backspace".to_owned(), String::new()),
        KeyCode::Delete => ("delete".to_owned(), String::new()),
        KeyCode::Esc => ("escape".to_owned(), String::new()),
        KeyCode::Tab => ("tab".to_owned(), String::new()),
        KeyCode::BackTab => {
            mods.shift = true;
            ("tab".to_owned(), String::new())
        },
        KeyCode::F(number @ 1..=12) => (format!("f{number}"), String::new()),
        _ => return None,
    };
    assert_ne!(name, "", "encoded key carries a non-empty key name");
    Some(ToGuest::Key { key: name, kind, text, mods })
}

fn encode_mouse(mouse: MouseEvent, column: u16, row: u16, cell: CellPixels) -> ToGuest {
    let button_name = |button: MouseButton| match button {
        MouseButton::Left => "left",
        MouseButton::Middle => "middle",
        MouseButton::Right => "right",
    };
    let (kind, button) = match mouse.kind {
        MouseEventKind::Down(button) => ("down", button_name(button)),
        MouseEventKind::Up(button) => ("up", button_name(button)),
        MouseEventKind::Drag(button) => ("move", button_name(button)),
        MouseEventKind::Moved => ("move", "none"),
        MouseEventKind::ScrollUp => ("scrollup", "none"),
        MouseEventKind::ScrollDown => ("scrolldown", "none"),
        MouseEventKind::ScrollLeft => ("scrollleft", "none"),
        MouseEventKind::ScrollRight => ("scrollright", "none"),
    };
    let cell_width = u32::from(cell.width.get());
    let cell_height = u32::from(cell.height.get());
    let x = u32::from(column) * cell_width + cell_width / 2;
    let y = u32::from(row) * cell_height + cell_height / 2;
    assert!(x >= u32::from(column) * cell_width);
    assert!(y >= u32::from(row) * cell_height);
    ToGuest::Mouse { kind, button, mods: GuestMods::of(mouse.modifiers), x, y }
}

#[derive(Debug, PartialEq, Eq)]
enum QueriedUrl {
    Missing,
    Unpersistable,
    StartPage,
    Url(BrowserUrl),
}

fn classify_queried_url(url: Option<&str>) -> QueriedUrl {
    let Some(raw) = url else {
        return QueriedUrl::Unpersistable;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("about:blank") {
        return QueriedUrl::StartPage;
    }
    match BrowserUrl::parse(trimmed) {
        Ok(parsed) => QueriedUrl::Url(parsed),
        Err(why) => {
            log::say(&format!("embedded browser URL not persisted: {why}"));
            QueriedUrl::Unpersistable
        },
    }
}

#[derive(Deserialize)]
struct ControlReply {
    ok: bool,
    data: Option<ControlData>,
}

#[derive(Deserialize)]
struct ControlData {
    tty: Option<String>,
    url: Option<String>,
}

fn query_url_for_tty(tty: &Path) -> QueriedUrl {
    assert_ne!(tty.as_os_str(), "", "queried tty path is non-empty");
    let want = tty.to_string_lossy();
    let Some(runtime) = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("XDG_STATE_HOME").map(PathBuf::from))
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
    else {
        return QueriedUrl::Missing;
    };
    let dirs = match fs::read_dir(&runtime) {
        Ok(dirs) => dirs,
        Err(error) => {
            log::say(&format!("{}: browser runtime dir unread: {error}", runtime.display()));
            return QueriedUrl::Missing;
        },
    };
    let mut probed = 0usize;
    for entry in dirs.take(DIR_ENTRIES_MAX) {
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("terminal-browser-") {
            continue;
        }
        let instances = entry.path().join("instances");
        let Ok(sockets) = fs::read_dir(instances) else {
            continue;
        };
        for socket in sockets.take(DIR_ENTRIES_MAX) {
            if probed >= CONTROL_SOCKETS_MAX {
                assert!(probed <= CONTROL_SOCKETS_MAX);
                return QueriedUrl::Missing;
            }
            let Ok(socket) = socket else {
                continue;
            };
            let path = socket.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("sock") {
                continue;
            }
            probed += 1;
            if let Some(data) = ask_control_state(&path)
                && data.tty.as_deref() == Some(want.as_ref())
            {
                return classify_queried_url(data.url.as_deref());
            }
        }
    }
    assert!(probed <= CONTROL_SOCKETS_MAX);
    QueriedUrl::Missing
}

fn ask_control_state(socket: &Path) -> Option<ControlData> {
    assert_ne!(socket.as_os_str(), "", "control socket path is non-empty");
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(CONTROL_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(CONTROL_TIMEOUT)).ok()?;
    stream.write_all(b"{\"id\":\"1\",\"cmd\":\"state\"}\n").ok()?;
    stream.flush().ok()?;
    let mut reader = BufReader::new(stream);
    let BoundedLine::Line(line) =
        read_bounded_line(&mut reader, CONTROL_LINE_BYTES_MAX, CONTROL_LINE_BYTES_MAX).ok()?
    else {
        return None;
    };
    assert!(line.len() <= CONTROL_LINE_BYTES_MAX);
    let reply: ControlReply = serde_json::from_str(&line).ok()?;
    reply.ok.then_some(reply.data).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_and_mouse_events_encode_for_embedded_browser() {
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let encoded = serde_json::to_value(encode_key(&key).unwrap()).unwrap();
        assert_eq!(encoded["type"], "key");
        assert_eq!(encoded["key"], "a");
        assert_eq!(encoded["text"], "");
        assert_eq!(encoded["mods"]["ctrl"], true);

        let plain = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        let encoded = serde_json::to_value(encode_key(&plain).unwrap()).unwrap();
        assert_eq!(encoded["text"], "x");

        let backtab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE);
        let encoded = serde_json::to_value(encode_key(&backtab).unwrap()).unwrap();
        assert_eq!(encoded["key"], "tab");
        assert_eq!(encoded["mods"]["shift"], true);

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::SHIFT,
        };
        let encoded = serde_json::to_value(
            encode_mouse(mouse, 3, 2, DEFAULT_CELL),
        )
        .unwrap();
        assert_eq!(encoded["type"], "mouse");
        assert_eq!(encoded["kind"], "down");
        assert_eq!(encoded["button"], "left");
        assert_eq!(encoded["x"], 35);
        assert_eq!(encoded["y"], 50);
        assert_eq!(encoded["mods"]["shift"], true);
    }

    #[test]
    fn pointer_shape_and_frame_reject_invalid_guest_values() {
        assert_eq!(PointerShape::parse("pointer").unwrap().as_str(), "pointer");
        assert_eq!(PointerShape::parse("ew-resize").unwrap().as_str(), "ew-resize");
        for bad in [
            "",
            "pointer\x1b\\injected",
            "pointer\x07",
            "pointer;extra",
            "UPPER",
            "a b",
            &"a".repeat(POINTER_SHAPE_BYTES_MAX + 1),
        ] {
            assert_eq!(PointerShape::parse(bad), None, "{bad:?}");
        }

        assert!(Frame::parse("/px-1".to_owned(), 800, 600, 80, 24, 1).is_some());
        assert!(Frame::parse(String::new(), 800, 600, 80, 24, 1).is_none());
        assert!(Frame::parse("px-1".to_owned(), 800, 600, 80, 24, 1).is_none());
        assert!(Frame::parse("/px\x001".to_owned(), 800, 600, 80, 24, 1).is_none());
        assert!(Frame::parse("/px-1".to_owned(), 0, 600, 80, 24, 1).is_none());
        assert!(Frame::parse("/px-1".to_owned(), IMAGE_PIXELS_MAX + 1, 600, 80, 24, 1).is_none());
        assert!(Frame::parse("/px-1".to_owned(), 800, 600, 0, 24, 1).is_none());
    }

    #[test]
    fn read_bounded_line_skips_oversized_newlines_and_rejects_runaway_streams() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("line.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut client = UnixStream::connect(&socket).unwrap();
        let (server, _) = listener.accept().unwrap();

        let oversized_with_newline = format!("{}\nafter\n", "x".repeat(CONTROL_LINE_BYTES_MAX + 64));
        let runaway = vec![b'y'; CONTROL_LINE_BYTES_MAX * 2 + 128];
        let writer = thread::spawn(move || {
            client.write_all(oversized_with_newline.as_bytes()).unwrap();
            let _ = client.write_all(&runaway);
        });
        let mut reader = BufReader::new(server);
        assert!(matches!(
            read_bounded_line(&mut reader, CONTROL_LINE_BYTES_MAX, CONTROL_LINE_BYTES_MAX * 2),
            Ok(BoundedLine::Oversized)
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, CONTROL_LINE_BYTES_MAX, CONTROL_LINE_BYTES_MAX * 2),
            Ok(BoundedLine::Line(ref line)) if line == "after"
        ));
        let err = read_bounded_line(
            &mut reader,
            CONTROL_LINE_BYTES_MAX,
            CONTROL_LINE_BYTES_MAX * 2,
        )
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        writer.join().unwrap();
    }

    #[test]
    fn kitty_graphics_and_placeholder_cells_encode_properly() {
        let frame = Frame::parse("/px-12345".to_owned(), 800, 480, 80, 24, 1).unwrap();
        let image_id = ImageId::new(0x004c_0001);
        let mut out = Vec::new();
        append_kitty_transmit(&mut out, image_id, &frame);
        let text = String::from_utf8(out).unwrap();
        let expected_b64 = base64::engine::general_purpose::STANDARD.encode("/px-12345");
        assert_eq!(
            text,
            format!("\x1b_Ga=T,f=32,s=800,v=480,t=s,i=4980737,U=1,c=80,r=24,q=2;{expected_b64}\x1b\\")
        );

        let area = Rect::new(2, 3, 4, 2);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 10));
        paint_placeholders(&mut buffer, area, image_id, 4, 2);
        let cell = &buffer[(2, 3)];
        assert_eq!(
            cell.symbol(),
            format!("{PLACEHOLDER}{}{}", ROW_COLUMN_DIACRITICS[0], ROW_COLUMN_DIACRITICS[0])
        );
        assert_eq!(cell.fg, Color::Rgb(0x4c, 0x00, 0x01));
        let corner = &buffer[(5, 4)];
        assert_eq!(
            corner.symbol(),
            format!("{PLACEHOLDER}{}{}", ROW_COLUMN_DIACRITICS[1], ROW_COLUMN_DIACRITICS[3])
        );
    }

    #[test]
    fn queried_url_preserves_state_on_missing_or_unpersistable_urls() {
        assert_eq!(classify_queried_url(None), QueriedUrl::Unpersistable);
        assert_eq!(
            classify_queried_url(Some("chrome-error://chromewebdata/")),
            QueriedUrl::Unpersistable
        );
        assert_eq!(
            classify_queried_url(Some("data:text/html,<h1>hi</h1>")),
            QueriedUrl::Unpersistable
        );
        let oversized = format!("https://sso.example.com/?token={}", "a".repeat(3000));
        assert_eq!(classify_queried_url(Some(&oversized)), QueriedUrl::Unpersistable);
        assert_eq!(classify_queried_url(Some("")), QueriedUrl::StartPage);
        assert_eq!(classify_queried_url(Some("about:blank")), QueriedUrl::StartPage);
        assert_eq!(
            classify_queried_url(Some("https://example.com/docs")),
            QueriedUrl::Url(BrowserUrl::parse("https://example.com/docs").unwrap())
        );
    }

    #[test]
    fn browser_state_machine_handles_disconnect_failed_guards_empty_title_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let folder = Folder::new(dir.path().to_owned()).unwrap();
        assert!(Folder::new(PathBuf::new()).is_none(), "empty runtime folder is rejected");
        let (events_tx, _events_rx) = mpsc::channel();
        let mut browsers = Browsers::new(
            1,
            Config { mode: Mode::Supported(folder), cell: DEFAULT_CELL, colours: Colours::default() },
            events_tx,
        );
        let view = ViewId::of(1).unwrap();
        let (tx, rx) = mpsc::sync_channel(4);
        let BrowsersMode::Supported(state) = &mut browsers.mode else {
            unreachable!();
        };
        let image_id = state.alloc_image_id();
        state.views.insert(
            view,
            Instance::Starting(Running {
                image_id,
                columns: NonZeroU16::new(40).unwrap(),
                rows: NonZeroU16::new(12).unwrap(),
                presence: Presence::Visible { focused: true },
                title: None,
                tty: dir.path().join(format!("b-{view}-1-1.tty")),
                socket: dir.path().join("b-1-1.sock"),
                last_url_poll: None,
                url_poll_in_flight: Arc::new(AtomicBool::new(false)),
                stop: Arc::new(AtomicBool::new(false)),
                sender: tx,
            }),
        );

        let area = Rect::new(0, 0, 40, 12);
        let mut buffer = Buffer::empty(area);
        assert_eq!(browsers.heard(view, Heard::Title(None)), None);
        if let BrowsersMode::Supported(state) = &browsers.mode {
            assert!(state.views[&view].running().unwrap().last_url_poll.is_none());
        }
        assert_eq!(browsers.heard(view, Heard::Pointer(PointerShape::parse("pointer").unwrap())), None);
        assert_eq!(browsers.pointer(view), None);

        let frame = Frame::parse("/px-1".to_owned(), 400, 240, 40, 12, 1).unwrap();
        assert_eq!(browsers.heard(view, Heard::Frame(frame.clone())), None);
        assert_eq!(browsers.draw(view, &mut buffer, area), Shown::Live);
        assert_eq!(browsers.heard(view, Heard::Pointer(PointerShape::parse("pointer").unwrap())), None);
        assert_eq!(browsers.pointer(view).map(PointerShape::as_str), Some("pointer"));

        drop(rx);
        let press = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(browsers.key(view, &press), "disconnect-detecting keystroke is consumed");
        assert!(matches!(browsers.draw(view, &mut buffer, area), Shown::Failed(_)));
        assert_eq!(browsers.pointer(view), None);
        assert!(!browsers.key(view, &press), "subsequent keystrokes fall through to fallback UI");
        let tty_changes = browsers.drain_tty_changes();
        assert_eq!(tty_changes, vec![(view, None)]);

        assert_eq!(browsers.heard(view, Heard::Frame(frame)), None);
        assert!(matches!(browsers.draw(view, &mut buffer, area), Shown::Failed(_)));
        assert_eq!(browsers.heard(view, Heard::Pointer(PointerShape::parse("text").unwrap())), None);
        assert_eq!(browsers.pointer(view), None);

        if let BrowsersMode::Supported(state) = &mut browsers.mode {
            for index in 2..=(EMBEDDED_MAX as u64) {
                state.insert_failed(ViewId::of(index).unwrap(), "failed".to_owned());
            }
        }
        let overflow = ViewId::of((EMBEDDED_MAX as u64) + 1).unwrap();
        assert_eq!(browsers.draw(overflow, &mut buffer, area), Shown::AtLimit);
    }

    #[test]
    fn peer_socket_completes_handshake_exchanges_frames_and_forwards_input() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("embed.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (events_tx, events_rx) = mpsc::channel();
        let (to_guest_tx, to_guest_rx) = mpsc::sync_channel(TO_GUEST_QUEUE_MAX);
        let stop = Arc::new(AtomicBool::new(false));
        let view = ViewId::of(7).unwrap();
        let peer = Peer {
            number: 1,
            view,
            listener,
            init: ToGuest::Init {
                columns: 40,
                rows: 12,
                width: 400,
                height: 240,
                cell: [10, 20],
                image_id: IMAGE_ID_BASE,
                transport: "host",
                focused: true,
                colors: None,
            },
            receiver: to_guest_rx,
            events: events_tx,
            stop: Arc::clone(&stop),
        };
        let handle = thread::spawn(move || run_peer(peer, None));

        let mut client = UnixStream::connect(&socket).unwrap();
        client
            .write_all(b"{\"type\":\"join\",\"pane\":\"1\",\"name\":\"browser\",\"pid\":1}\n")
            .unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut init_line = String::new();
        reader.read_line(&mut init_line).unwrap();
        let init: serde_json::Value = serde_json::from_str(&init_line).unwrap();
        assert_eq!(init["type"], "init");
        assert_eq!(init["transport"], "host");
        assert_eq!(init["imageId"], IMAGE_ID_BASE);
        assert_eq!(init["cols"], 40);

        client
            .write_all(
                b"{\"type\":\"frame\",\"shm\":\"/px-1\",\"format\":\"rgba\",\"width\":400,\"height\":240,\"generation\":1,\"cols\":40,\"rows\":12,\"cell\":[10,20]}\n",
            )
            .unwrap();
        client.write_all(b"{\"type\":\"title\",\"text\":\"GitHub\"}\n").unwrap();
        client.write_all(b"{\"type\":\"pointer\",\"shape\":\"pointer\"}\n").unwrap();
        client.write_all(b"{\"type\":\"pointer\",\"shape\":\"bad\\u001b]52;c;\"}\n").unwrap();
        client.write_all(b"{\"type\":\"clipboard\",\"text\":\"copied\"}\n").unwrap();

        let large_clip = "c".repeat(32 * 1024);
        let clip_payload = format!("{{\"type\":\"clipboard\",\"text\":\"{large_clip}\"}}\n");
        client.write_all(clip_payload.as_bytes()).unwrap();

        let mut received = Vec::new();
        lune_waits::eventually("five browser events arrive", || {
            while let Ok(Event::Browser { heard, .. }) = events_rx.try_recv() {
                received.push(heard);
            }
            (received.len() >= 5).then_some(())
        });
        assert!(matches!(received[0], Heard::Frame(ref frame) if frame.shm() == "/px-1"));
        assert_eq!(received[1], Heard::Title(Some("GitHub".to_owned())));
        assert_eq!(received[2], Heard::Pointer(PointerShape::parse("pointer").unwrap()));
        assert_eq!(received[3], Heard::Clipboard("copied".to_owned()));
        assert_eq!(received[4], Heard::Clipboard(large_clip));

        to_guest_tx.send(ToGuest::Paste { text: "hello".to_owned() }).unwrap();
        let mut pasted_line = String::new();
        reader.read_line(&mut pasted_line).unwrap();
        let pasted: serde_json::Value = serde_json::from_str(&pasted_line).unwrap();
        assert_eq!(pasted["type"], "paste");
        assert_eq!(pasted["text"], "hello");

        stop.store(true, Ordering::Relaxed);
        drop(to_guest_tx);
        drop(client);
        handle.join().unwrap();
    }

    #[test]
    fn spawn_command_only_passes_terminal_socket_for_local_machine() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("b.sock");
        let tty = dir.path().join("b.tty");
        let terminal_socket = dir.path().join("terminal-browser.sock");
        let view = ViewId::of(1).unwrap();
        let local_spec = SpawnSpec {
            view,
            machine: &Machine::Local,
            url: None,
            columns: NonZeroU16::new(80).unwrap(),
            rows: NonZeroU16::new(24).unwrap(),
            presence: Presence::Visible { focused: true },
        };
        let local_cmd = configure_spawn_command(&local_spec, &socket, &tty, &terminal_socket);
        let local_envs: BTreeMap<_, _> = local_cmd.get_envs().collect();
        assert_eq!(
            local_envs.get(std::ffi::OsStr::new("TERMINAL_BROWSER_TERMINAL_SOCKET")),
            Some(&Some(terminal_socket.as_os_str()))
        );

        let remote_machine = Machine::Remote("box".parse().unwrap());
        let remote_spec = SpawnSpec { machine: &remote_machine, ..local_spec };
        let remote_cmd = configure_spawn_command(&remote_spec, &socket, &tty, &terminal_socket);
        let remote_envs: BTreeMap<_, _> = remote_cmd.get_envs().collect();
        assert_eq!(
            remote_envs.get(std::ffi::OsStr::new("TERMINAL_BROWSER_TERMINAL_SOCKET")),
            Some(&None),
            "remote browsers must remove inherited local terminal socket env var"
        );
        let args: Vec<_> = remote_cmd.get_args().collect();
        assert!(args.iter().any(|arg| *arg == std::ffi::OsStr::new("--ssh=box")));
    }

    #[test]
    fn run_peer_kills_hanging_child_on_accept_timeout_and_unblocks_stalled_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("timeout.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (events_tx, events_rx) = mpsc::channel();
        let (_to_guest_tx, to_guest_rx) = mpsc::sync_channel(TO_GUEST_QUEUE_MAX);
        let stop = Arc::new(AtomicBool::new(false));
        let view = ViewId::of(8).unwrap();
        let mut child_cmd = Command::new("/bin/sh");
        child_cmd
            .args(["-c", "exec sleep 60"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = child_cmd.spawn().unwrap();
        let peer = Peer {
            number: 1,
            view,
            listener,
            init: ToGuest::Init {
                columns: 40,
                rows: 12,
                width: 400,
                height: 240,
                cell: [10, 20],
                image_id: IMAGE_ID_BASE,
                transport: "host",
                focused: true,
                colors: None,
            },
            receiver: to_guest_rx,
            events: events_tx,
            stop: Arc::clone(&stop),
        };
        let short = PeerTimeouts {
            accept_polls_max: 2,
            accept_timeout: Duration::from_millis(100),
            handshake_polls_max: 2,
            handshake_timeout: Duration::from_millis(100),
            exit_polls_max: 2,
        };
        let handle = thread::spawn(move || run_peer_with_timeouts(peer, Some(child), short));
        let event = lune_waits::eventually("browser exit event arrives after accept timeout", || {
            events_rx.try_recv().ok()
        });
        let Event::Browser { heard: Heard::Exited(Exit::Failed(reason)), .. } = event else {
            panic!("expected failed exit on accept timeout");
        };
        assert!(reason.contains("did not connect in time"), "{reason}");
        handle.join().unwrap();

        let handshake_socket = dir.path().join("handshake.sock");
        let listener = UnixListener::bind(&handshake_socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (events_tx, _events_rx) = mpsc::channel();
        let (_to_guest_tx, to_guest_rx) = mpsc::sync_channel(TO_GUEST_QUEUE_MAX);
        let stop = Arc::new(AtomicBool::new(false));
        let peer = Peer {
            number: 2,
            view,
            listener,
            init: ToGuest::Init {
                columns: 40,
                rows: 12,
                width: 400,
                height: 240,
                cell: [10, 20],
                image_id: IMAGE_ID_BASE,
                transport: "host",
                focused: true,
                colors: None,
            },
            receiver: to_guest_rx,
            events: events_tx,
            stop: Arc::clone(&stop),
        };
        let handle = thread::spawn(move || run_peer_with_timeouts(peer, None, PeerTimeouts::DEFAULT));
        let _stalled = UnixStream::connect(&handshake_socket).unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{self, BufRead as _, BufReader, ErrorKind, Read as _, Write as _};
use std::net::Shutdown;
use std::num::NonZeroU16;
use std::os::unix::fs::symlink;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

const LINE_BYTES_MAX: usize = 16 * 1024;

const STDERR_BYTES_MAX: usize = 2048;

const TO_GUEST_QUEUE_MAX: usize = 256;

const ACCEPT_POLL: Duration = Duration::from_millis(50);

const ACCEPT_TIMEOUT: Duration = Duration::from_secs(15);

const ACCEPT_POLLS_MAX: u32 = 300;

const CONTROL_TIMEOUT: Duration = Duration::from_millis(300);

const CONTROL_SOCKETS_MAX: usize = 64;

const DIR_ENTRIES_MAX: usize = 1024;

const URL_POLL_GAP_MIN: Duration = Duration::from_millis(350);

const DIACRITICS_MAX: u16 = 297;

const PLACEHOLDER: char = '\u{10EEEE}';

const DEFAULT_CELL: CellPixels = CellPixels { width: 10, height: 20 };

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
const _: () = assert!(LINE_BYTES_MAX > 0);
const _: () = assert!(STDERR_BYTES_MAX > 0);
const _: () = assert!(TO_GUEST_QUEUE_MAX > 0);
const _: () = assert!(CONTROL_SOCKETS_MAX > 0 && CONTROL_SOCKETS_MAX <= DIR_ENTRIES_MAX);
const _: () =
    assert!(ACCEPT_POLLS_MAX as u128 * ACCEPT_POLL.as_millis() == ACCEPT_TIMEOUT.as_millis());
const _: () = assert!(DEFAULT_CELL.width > 0 && DEFAULT_CELL.height > 0);

fn div_floor_nonzero(pixels: u16, cells: u16) -> Option<u16> {
    if pixels == 0 || cells == 0 {
        return None;
    }
    let quotient = pixels / cells;
    if quotient == 0 {
        return None;
    }
    assert!(u32::from(quotient) * u32::from(cells) <= u32::from(pixels));
    Some(quotient)
}

#[must_use]
pub fn cell_pixels(
    cell: Option<CellPixels>,
    window_pixels: Option<WindowPixels>,
    columns: u16,
    rows: u16,
) -> CellPixels {
    let resolved = if let Some(cell) = cell
        && cell.width > 0
        && cell.height > 0
    {
        cell
    } else if let Some(window) = window_pixels
        && let Some(width) = div_floor_nonzero(window.width, columns)
        && let Some(height) = div_floor_nonzero(window.height, rows)
    {
        CellPixels { width, height }
    } else if let Ok(window) = rustix::termios::tcgetwinsize(io::stdout())
        && let Some(width) = div_floor_nonzero(window.ws_xpixel, window.ws_col)
        && let Some(height) = div_floor_nonzero(window.ws_ypixel, window.ws_row)
    {
        CellPixels { width, height }
    } else {
        DEFAULT_CELL
    };
    assert!(resolved.width > 0, "resolved cell width is positive");
    assert!(resolved.height > 0, "resolved cell height is positive");
    resolved
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

#[derive(Clone, Debug)]
pub struct Config {
    pub runtime: PathBuf,
    pub terminal_socket: PathBuf,
    pub cell: CellPixels,
    pub colours: Colours,
    pub supported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub shm: String,
    pub width: u32,
    pub height: u32,
    pub columns: u16,
    pub rows: u16,
    pub generation: u64,
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
        if width == 0 || width > IMAGE_PIXELS_MAX || height == 0 || height > IMAGE_PIXELS_MAX {
            return None;
        }
        if columns == 0 || rows == 0 {
            return None;
        }
        assert!(width <= IMAGE_PIXELS_MAX && height <= IMAGE_PIXELS_MAX);
        assert!(columns > 0 && rows > 0);
        Some(Frame { shm, width, height, columns, rows, generation })
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
    Title(String),
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

struct SpawnSpec<'a> {
    view: ViewId,
    machine: &'a Machine,
    url: Option<&'a BrowserUrl>,
    columns: u16,
    rows: u16,
    visible: bool,
    focused: bool,
}

struct InstanceMeta {
    image_id: u32,
    columns: u16,
    rows: u16,
    visible: bool,
    focused: bool,
    socket: PathBuf,
    tty: PathBuf,
    stop: Arc<AtomicBool>,
}

struct Instance {
    image_id: u32,
    columns: u16,
    rows: u16,
    visible: bool,
    focused: bool,
    frame: Option<Frame>,
    title: Option<String>,
    pointer: Option<PointerShape>,
    error: Option<String>,
    tty: PathBuf,
    socket: PathBuf,
    last_url_poll: Option<Instant>,
    url_poll_in_flight: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    sender: Option<SyncSender<ToGuest>>,
}

impl Instance {
    fn send(&mut self, message: ToGuest) -> bool {
        let Some(sender) = &self.sender else {
            return false;
        };
        let is_mouse = matches!(message, ToGuest::Mouse { .. });
        match sender.try_send(message) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                if !is_mouse {
                    log::say("embedded browser outbound queue full; dropping message");
                }
                true
            },
            Err(TrySendError::Disconnected(_)) => {
                self.sender = None;
                false
            },
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.sender = None;
        remove_if_present(&self.socket);
        remove_if_present(&self.tty);
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

pub struct Browsers {
    number: u64,
    config: Config,
    events: Sender<Event>,
    views: BTreeMap<ViewId, Instance>,
    next_id: u32,
    next_socket: u64,
    pending_transmits: BTreeMap<u32, Frame>,
    pending_deletes: BTreeSet<u32>,
    owned_ids: BTreeSet<u32>,
}

impl Browsers {
    #[must_use]
    pub fn new(number: u64, config: Config, events: Sender<Event>) -> Browsers {
        assert!(config.cell.width > 0, "browser config cell width is positive");
        assert!(config.cell.height > 0, "browser config cell height is positive");
        Browsers {
            number,
            config,
            events,
            views: BTreeMap::new(),
            next_id: IMAGE_ID_BASE,
            next_socket: 0,
            pending_transmits: BTreeMap::new(),
            pending_deletes: BTreeSet::new(),
            owned_ids: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn supported(&self) -> bool {
        self.config.supported
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        assert!(self.config.cell.width > 0 && self.config.cell.height > 0);
        &self.config
    }

    pub fn sync(
        &mut self,
        machine: &Machine,
        desired: &[Desired<'_>],
        focused: Option<ViewId>,
        window_focused: bool,
    ) {
        assert!(self.views.len() <= EMBEDDED_MAX, "embedded browsers stay within EMBEDDED_MAX");
        if !self.config.supported {
            self.clear();
            assert!(self.views.is_empty(), "unsupported browsers hold no instances");
            return;
        }
        let wanted: BTreeSet<ViewId> = desired.iter().map(|item| item.view).collect();
        let removed: Vec<ViewId> =
            self.views.keys().copied().filter(|id| !wanted.contains(id)).collect();
        for view in removed {
            self.remove(view);
        }
        for item in desired {
            let columns = item.inner.width.max(1);
            let rows = item.inner.height.max(1);
            let is_focused = window_focused && item.visible && focused == Some(item.view);
            if let Some(instance) = self.views.get_mut(&item.view) {
                if instance.columns != columns || instance.rows != rows {
                    instance.columns = columns;
                    instance.rows = rows;
                    let (width, height) = pixel_size(columns, rows, self.config.cell);
                    let _ = instance.send(ToGuest::Size {
                        columns,
                        rows,
                        width,
                        height,
                        cell: [self.config.cell.width, self.config.cell.height],
                    });
                }
                if instance.visible != item.visible {
                    instance.visible = item.visible;
                    let _ = instance.send(ToGuest::Visible { value: item.visible });
                }
                if instance.focused != is_focused {
                    instance.focused = is_focused;
                    let _ = instance.send(ToGuest::Focus { focused: is_focused });
                }
                continue;
            }
            if self.views.len() >= EMBEDDED_MAX {
                continue;
            }
            self.spawn(&SpawnSpec {
                view: item.view,
                machine,
                url: item.url,
                columns,
                rows,
                visible: item.visible,
                focused: is_focused,
            });
        }
        assert!(self.views.len() <= EMBEDDED_MAX, "embedded browsers stay within EMBEDDED_MAX");
    }

    pub fn retry(
        &mut self,
        view: ViewId,
        machine: &Machine,
        url: Option<&BrowserUrl>,
        inner: Rect,
        focused: bool,
    ) {
        if !self.config.supported {
            return;
        }
        let already_held = self.views.contains_key(&view);
        if !already_held && self.views.len() >= EMBEDDED_MAX {
            return;
        }
        self.remove(view);
        assert!(!self.views.contains_key(&view), "retried view was removed first");
        self.spawn(&SpawnSpec {
            view,
            machine,
            url,
            columns: inner.width.max(1),
            rows: inner.height.max(1),
            visible: true,
            focused,
        });
        assert!(self.views.len() <= EMBEDDED_MAX, "embedded browsers stay within EMBEDDED_MAX");
    }

    fn spawn(&mut self, spec: &SpawnSpec<'_>) {
        assert!(!self.views.contains_key(&spec.view), "spawn does not overwrite an active view");
        assert!(self.views.len() < EMBEDDED_MAX, "spawn stays within EMBEDDED_MAX");
        assert!(spec.columns > 0 && spec.rows > 0, "spawn dimensions are positive");
        let image_id = self.alloc_image_id();
        self.next_socket = self.next_socket.wrapping_add(1);
        let pid = std::process::id();
        let socket = self.config.runtime.join(format!("b-{pid}-{}.sock", self.next_socket));
        let tty = terminal_socket::browser_client_tty_path(
            &self.config.runtime,
            spec.view,
            pid,
            self.next_socket,
        );
        let stop = Arc::new(AtomicBool::new(false));
        let meta = InstanceMeta {
            image_id,
            columns: spec.columns,
            rows: spec.rows,
            visible: spec.visible,
            focused: spec.focused,
            socket: socket.clone(),
            tty: tty.clone(),
            stop: Arc::clone(&stop),
        };
        remove_if_present(&socket);
        remove_if_present(&tty);
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) => {
                self.insert_failed(spec.view, meta, format!("embed socket failed: {error}"));
                return;
            },
        };
        if let Err(error) = listener.set_nonblocking(true) {
            remove_if_present(&socket);
            self.insert_failed(spec.view, meta, format!("embed socket setup failed: {error}"));
            return;
        }
        if let Err(error) = symlink("/dev/null", &tty) {
            remove_if_present(&socket);
            self.insert_failed(spec.view, meta, format!("browser tty setup failed: {error}"));
            return;
        }
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
            .env("PIXEL_EMBED", &socket)
            .env("PIXEL_TTY", &tty)
            .env("TERMINAL_BROWSER_START_PAGE", "1")
            .env("TERMINAL_BROWSER_COPY_ON_SELECT", "1")
            .env("TERMINAL_BROWSER_TERMINAL_SOCKET", &self.config.terminal_socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                remove_if_present(&socket);
                remove_if_present(&tty);
                self.insert_failed(spec.view, meta, MISSING_GUIDANCE.to_owned());
                return;
            },
            Err(error) => {
                remove_if_present(&socket);
                remove_if_present(&tty);
                self.insert_failed(
                    spec.view,
                    meta,
                    format!("terminal-browser did not start: {error}"),
                );
                return;
            },
        };
        let (sender, receiver) = mpsc::sync_channel(TO_GUEST_QUEUE_MAX);
        let (width, height) = pixel_size(spec.columns, spec.rows, self.config.cell);
        let init = ToGuest::Init {
            columns: spec.columns,
            rows: spec.rows,
            width,
            height,
            cell: [self.config.cell.width, self.config.cell.height],
            image_id,
            transport: "host",
            focused: spec.focused,
            colors: GuestColours::of(self.config.colours),
        };
        let peer = Peer {
            number: self.number,
            view: spec.view,
            listener,
            init,
            receiver,
            events: self.events.clone(),
            stop: Arc::clone(&stop),
        };
        thread::spawn(move || run_peer(peer, Some(child)));
        let previous = self.views.insert(spec.view, Instance {
            image_id: meta.image_id,
            columns: meta.columns,
            rows: meta.rows,
            visible: meta.visible,
            focused: meta.focused,
            frame: None,
            title: None,
            pointer: None,
            error: None,
            tty: meta.tty,
            socket: meta.socket,
            last_url_poll: None,
            url_poll_in_flight: Arc::new(AtomicBool::new(false)),
            stop: meta.stop,
            sender: Some(sender),
        });
        assert!(previous.is_none(), "spawned view was not already present");
    }

    fn insert_failed(&mut self, view: ViewId, meta: InstanceMeta, error: String) {
        assert_ne!(error, "", "failure message is non-empty");
        assert!(self.views.len() < EMBEDDED_MAX, "failed instance stays within EMBEDDED_MAX");
        let previous = self.views.insert(view, Instance {
            image_id: meta.image_id,
            columns: meta.columns,
            rows: meta.rows,
            visible: meta.visible,
            focused: meta.focused,
            frame: None,
            title: None,
            pointer: None,
            error: Some(error),
            tty: meta.tty,
            socket: meta.socket,
            last_url_poll: None,
            url_poll_in_flight: Arc::new(AtomicBool::new(false)),
            stop: meta.stop,
            sender: None,
        });
        assert!(previous.is_none(), "failed view did not overwrite an active view");
    }

    fn alloc_image_id(&mut self) -> u32 {
        assert!(
            (IMAGE_ID_BASE..=IMAGE_ID_BASE + IMAGE_ID_SPAN).contains(&self.next_id),
            "next image id stays in range"
        );
        for _ in 0..=IMAGE_ID_SPAN {
            let id = self.next_id;
            self.next_id = if self.next_id >= IMAGE_ID_BASE + IMAGE_ID_SPAN {
                IMAGE_ID_BASE
            } else {
                self.next_id + 1
            };
            let free = !self.owned_ids.contains(&id)
                && !self.pending_deletes.contains(&id)
                && !self.views.values().any(|instance| instance.image_id == id);
            if free {
                assert!((IMAGE_ID_BASE..=IMAGE_ID_BASE + IMAGE_ID_SPAN).contains(&id));
                return id;
            }
        }
        unreachable!("at most EMBEDDED_MAX views share IMAGE_ID_SPAN + 1 image ids")
    }

    fn remove(&mut self, view: ViewId) {
        if let Some(instance) = self.views.remove(&view) {
            self.delete_image(instance.image_id);
        }
        assert!(!self.views.contains_key(&view), "removed view is gone");
    }

    pub fn clear(&mut self) {
        let ids: Vec<ViewId> = self.views.keys().copied().collect();
        for view in ids {
            self.remove(view);
        }
        assert!(self.views.is_empty(), "cleared browsers hold no views");
        assert!(self.owned_ids.is_empty(), "cleared browsers own no live images");
    }

    fn delete_image(&mut self, image_id: u32) {
        assert!((IMAGE_ID_BASE..=IMAGE_ID_BASE + IMAGE_ID_SPAN).contains(&image_id));
        self.pending_transmits.remove(&image_id);
        if self.owned_ids.remove(&image_id) {
            self.pending_deletes.insert(image_id);
        }
        assert!(!self.pending_transmits.contains_key(&image_id));
    }

    pub fn heard(&mut self, view: ViewId, heard: Heard) -> Option<Outcome> {
        let number = self.number;
        let events = self.events.clone();
        let instance = self.views.get_mut(&view)?;
        match heard {
            Heard::Frame(frame) => {
                if instance
                    .frame
                    .as_ref()
                    .is_some_and(|previous| frame.generation < previous.generation)
                {
                    return None;
                }
                let image_id = instance.image_id;
                assert!(
                    !self.pending_deletes.contains(&image_id),
                    "active browser image id is not queued for deletion"
                );
                self.pending_transmits.insert(image_id, frame.clone());
                self.owned_ids.insert(image_id);
                instance.frame = Some(frame);
                instance.error = None;
                assert!(self.owned_ids.len() <= EMBEDDED_MAX);
                assert!(self.pending_transmits.len() <= EMBEDDED_MAX);
                None
            },
            Heard::Title(title) => {
                let changed = instance.title.as_deref() != Some(title.as_str());
                instance.title = (!title.is_empty()).then_some(title);
                if changed {
                    let now = Instant::now();
                    let due = instance.last_url_poll.is_none_or(|last| {
                        now.saturating_duration_since(last) >= URL_POLL_GAP_MIN
                    });
                    if due
                        && instance
                            .url_poll_in_flight
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                    {
                        instance.last_url_poll = Some(now);
                        let tty = instance.tty.clone();
                        let in_flight = Arc::clone(&instance.url_poll_in_flight);
                        thread::spawn(move || {
                            let _guard = FlagGuard(in_flight);
                            if let QueriedUrl::Found(url) = query_url_for_tty(&tty)
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
                instance.pointer = Some(shape);
                None
            },
            Heard::Clipboard(text) => Some(Outcome::Copy(text)),
            Heard::Url(url) => Some(Outcome::Navigated { view, url }),
            Heard::Exited(Exit::Clean) => {
                self.remove(view);
                Some(Outcome::Close(view))
            },
            Heard::Exited(Exit::Failed(why)) => {
                let image_id = instance.image_id;
                instance.frame = None;
                instance.pointer = None;
                instance.sender = None;
                instance.error = Some(why);
                self.delete_image(image_id);
                None
            },
        }
    }

    pub fn key(&mut self, view: ViewId, key: &KeyEvent) -> bool {
        let Some(instance) = self.views.get_mut(&view) else {
            return false;
        };
        if instance.sender.is_none() {
            return false;
        }
        if let Some(message) = encode_key(key) {
            return instance.send(message);
        }
        true
    }

    pub fn paste(&mut self, view: ViewId, text: &str) -> bool {
        let Some(instance) = self.views.get_mut(&view) else {
            return false;
        };
        if instance.sender.is_none() {
            return false;
        }
        instance.send(ToGuest::Paste { text: text.to_owned() })
    }

    pub fn mouse(&mut self, view: ViewId, mouse: MouseEvent, column: u16, row: u16) -> bool {
        let Some(instance) = self.views.get_mut(&view) else {
            return false;
        };
        if instance.sender.is_none() {
            return false;
        }
        instance.send(encode_mouse(mouse, column, row, self.config.cell))
    }

    pub fn colours(&mut self, colours: Colours) {
        self.config.colours = colours;
        let Some(guest) = GuestColours::of(colours) else {
            return;
        };
        for instance in self.views.values_mut() {
            let _ = instance.send(ToGuest::Colors { colors: guest });
        }
    }

    pub fn cell_resized(&mut self, cell: CellPixels) {
        assert!(cell.width > 0 && cell.height > 0, "resized cell dimensions are positive");
        if self.config.cell == cell {
            return;
        }
        self.config.cell = cell;
        for instance in self.views.values_mut() {
            let (width, height) = pixel_size(instance.columns, instance.rows, cell);
            let _ = instance.send(ToGuest::Size {
                columns: instance.columns,
                rows: instance.rows,
                width,
                height,
                cell: [cell.width, cell.height],
            });
        }
    }

    #[must_use]
    pub fn title(&self, view: ViewId) -> Option<&str> {
        self.views.get(&view)?.title.as_deref()
    }

    #[must_use]
    pub fn pointer(&self, view: ViewId) -> Option<&PointerShape> {
        let instance = self.views.get(&view)?;
        instance.frame.as_ref()?;
        instance.pointer.as_ref()
    }

    #[must_use]
    pub fn draw(&self, view: ViewId, buffer: &mut Buffer, seen: Rect) -> Shown<'_> {
        let Some(instance) = self.views.get(&view) else {
            return Shown::Starting;
        };
        if let Some(error) = &instance.error {
            return Shown::Failed(error);
        }
        let Some(frame) = &instance.frame else {
            return Shown::Starting;
        };
        paint_placeholders(buffer, seen, instance.image_id, frame.columns, frame.rows);
        Shown::Live
    }

    pub fn drain_graphics(&mut self) -> Vec<u8> {
        assert!(self.pending_transmits.len() <= EMBEDDED_MAX);
        let mut out = Vec::new();
        for id in std::mem::take(&mut self.pending_deletes) {
            append_kitty_delete(&mut out, id);
        }
        for (id, frame) in std::mem::take(&mut self.pending_transmits) {
            append_kitty_transmit(&mut out, id, &frame);
        }
        assert!(self.pending_deletes.is_empty() && self.pending_transmits.is_empty());
        out
    }

    pub fn drain_all_deletes(&mut self) -> Vec<u8> {
        self.pending_transmits.clear();
        let mut ids = std::mem::take(&mut self.pending_deletes);
        ids.extend(std::mem::take(&mut self.owned_ids));
        let mut out = Vec::new();
        for id in ids {
            append_kitty_delete(&mut out, id);
        }
        assert!(self.pending_deletes.is_empty() && self.owned_ids.is_empty());
        out
    }
}

impl Drop for Browsers {
    fn drop(&mut self) {
        self.clear();
    }
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
    if let Err(error) = child.kill()
        && error.kind() != ErrorKind::InvalidInput
    {
        log::say(&format!("terminal-browser kill failed: {error}"));
    }
    if let Err(error) = child.wait() {
        log::say(&format!("terminal-browser wait failed: {error}"));
    }
}

fn run_peer(peer: Peer, mut child: Option<Child>) {
    let Peer { number, view, listener, init, receiver, events, stop } = peer;
    let stderr = child.as_mut().and_then(|child| child.stderr.take()).map(|mut err| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            if let Err(error) = err.by_ref().take(STDERR_BYTES_MAX as u64).read_to_end(&mut buffer)
            {
                log::say(&format!("terminal-browser stderr read failed: {error}"));
            }
            assert!(buffer.len() <= STDERR_BYTES_MAX);
            String::from_utf8_lossy(&buffer).trim().to_owned()
        })
    });
    let accepted = accept_peer(&listener, &stop, child.as_mut());
    match accepted {
        Ok(Some(stream)) => {
            serve_stream(stream, number, view, &init, receiver, &events, &stop);
        },
        Ok(None) => {},
        Err(why) => {
            let stderr_text = stderr.and_then(|handle| handle.join().ok()).unwrap_or_default();
            let detail = if stderr_text.is_empty() { why } else { stderr_text };
            if !stop.load(Ordering::Relaxed)
                && events
                    .send(Event::Browser { number, view, heard: Heard::Exited(Exit::Failed(detail)) })
                    .is_err()
            {
                log::say("embedded browser exit event dropped on closed channel");
            }
            if let Some(mut child) = child {
                kill_and_wait(&mut child);
            }
            return;
        },
    }
    if let Some(mut child) = child {
        if stop.load(Ordering::Relaxed) {
            kill_and_wait(&mut child);
            return;
        }
        let status = child.wait();
        let stderr_text = stderr.and_then(|handle| handle.join().ok()).unwrap_or_default();
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let exit = match status {
            Ok(status) if status.success() => Exit::Clean,
            Ok(status) => {
                let message = if stderr_text.is_empty() {
                    format!("terminal-browser exited ({status})")
                } else {
                    stderr_text
                };
                Exit::Failed(message)
            },
            Err(error) => Exit::Failed(format!("terminal-browser wait failed: {error}")),
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
) -> Result<Option<UnixStream>, String> {
    let deadline = Instant::now() + ACCEPT_TIMEOUT;
    for poll in 0..ACCEPT_POLLS_MAX {
        assert!(poll < ACCEPT_POLLS_MAX);
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| format!("embed stream setup failed: {error}"))?;
                return Ok(Some(stream));
            },
            Err(error) if error.kind() == ErrorKind::WouldBlock => {},
            Err(error) => return Err(format!("embed accept failed: {error}")),
        }
        if let Some(child) = child.as_deref_mut() {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(None),
                Ok(Some(status)) => {
                    return Err(format!("terminal-browser exited before connecting ({status})"));
                },
                Ok(None) => {},
                Err(error) => return Err(format!("terminal-browser wait check failed: {error}")),
            }
        }
        if Instant::now() >= deadline {
            return Err("terminal-browser did not connect in time".to_owned());
        }
        thread::sleep(ACCEPT_POLL);
    }
    Err("terminal-browser did not connect in time".to_owned())
}

fn serve_stream(
    stream: UnixStream,
    number: u64,
    view: ViewId,
    init: &ToGuest,
    receiver: mpsc::Receiver<ToGuest>,
    events: &Sender<Event>,
    stop: &Arc<AtomicBool>,
) {
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(error) => {
            log::say(&format!("embedded browser stream clone failed: {error}"));
            return;
        },
    };
    let mut reader = BufReader::new(stream);
    let first = match read_bounded_line(&mut reader) {
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
                assert!(!write_stop.load(Ordering::Relaxed));
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
            let line = match read_bounded_line(&mut reader) {
                Ok(Some(line)) => line,
                Ok(None) => break,
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
            assert!(!stop.load(Ordering::Relaxed));
            assert!(line.len() <= LINE_BYTES_MAX);
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
                FromGuest::Title { text } => Heard::Title(text),
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

fn read_bounded_line(reader: &mut BufReader<UnixStream>) -> io::Result<Option<String>> {
    let mut buffer = Vec::new();
    let mut total_read = 0usize;
    while total_read <= LINE_BYTES_MAX {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if buffer.is_empty() {
                return Ok(None);
            }
            assert!(buffer.len() <= LINE_BYTES_MAX);
            return Ok(Some(String::from_utf8_lossy(&buffer).into_owned()));
        }
        if let Some(newline) = available.iter().position(|&byte| byte == b'\n') {
            if total_read.saturating_add(newline) > LINE_BYTES_MAX {
                reader.consume(newline + 1);
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("embedded browser line exceeded {LINE_BYTES_MAX} bytes"),
                ));
            }
            buffer.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            assert!(buffer.len() <= LINE_BYTES_MAX);
            return Ok(Some(String::from_utf8_lossy(&buffer).into_owned()));
        }
        let chunk_len = available.len();
        total_read = total_read.saturating_add(chunk_len);
        if total_read > LINE_BYTES_MAX {
            reader.consume(chunk_len);
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("embedded browser line exceeded {LINE_BYTES_MAX} bytes"),
            ));
        }
        buffer.extend_from_slice(available);
        reader.consume(chunk_len);
    }
    Err(io::Error::new(
        ErrorKind::InvalidData,
        format!("embedded browser line exceeded {LINE_BYTES_MAX} bytes"),
    ))
}

fn write_json_line(writer: &mut impl io::Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    assert!(!line.contains('\n'), "serialized JSON line holds no embedded newline");
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

fn pixel_size(columns: u16, rows: u16, cell: CellPixels) -> (u32, u32) {
    assert!(cell.width > 0 && cell.height > 0, "cell pixel dimensions are positive");
    let width = (u32::from(columns.max(1)) * u32::from(cell.width)).min(IMAGE_PIXELS_MAX);
    let height = (u32::from(rows.max(1)) * u32::from(cell.height)).min(IMAGE_PIXELS_MAX);
    assert!((1..=IMAGE_PIXELS_MAX).contains(&width));
    assert!((1..=IMAGE_PIXELS_MAX).contains(&height));
    (width, height)
}

fn append_kitty_transmit(out: &mut Vec<u8>, image_id: u32, frame: &Frame) {
    assert!((IMAGE_ID_BASE..=IMAGE_ID_BASE + IMAGE_ID_SPAN).contains(&image_id));
    assert!((1..=IMAGE_PIXELS_MAX).contains(&frame.width));
    assert!((1..=IMAGE_PIXELS_MAX).contains(&frame.height));
    let payload = base64::engine::general_purpose::STANDARD.encode(frame.shm.as_bytes());
    write!(
        out,
        "\x1b_Ga=T,f=32,s={},v={},t=s,i={},U=1,c={},r={},q=2;{}\x1b\\",
        frame.width, frame.height, image_id, frame.columns, frame.rows, payload
    )
    .expect("writing to Vec<u8> is infallible");
}

fn append_kitty_delete(out: &mut Vec<u8>, image_id: u32) {
    assert!((IMAGE_ID_BASE..=IMAGE_ID_BASE + IMAGE_ID_SPAN).contains(&image_id));
    write!(out, "\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\").expect("writing to Vec<u8> is infallible");
}

fn paint_placeholders(buffer: &mut Buffer, seen: Rect, image_id: u32, columns: u16, rows: u16) {
    assert!((IMAGE_ID_BASE..=IMAGE_ID_BASE + IMAGE_ID_SPAN).contains(&image_id));
    let [top, red, green, blue] = image_id.to_be_bytes();
    assert_eq!(top, 0, "Kitty virtual placement image id fits 24-bit RGB");
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
    assert!(cell.width > 0 && cell.height > 0, "mouse cell dimensions are positive");
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
    let cell_width = u32::from(cell.width);
    let cell_height = u32::from(cell.height);
    let x = u32::from(column) * cell_width + cell_width / 2;
    let y = u32::from(row) * cell_height + cell_height / 2;
    assert!(x >= u32::from(column) * cell_width);
    assert!(y >= u32::from(row) * cell_height);
    ToGuest::Mouse { kind, button, mods: GuestMods::of(mouse.modifiers), x, y }
}

enum QueriedUrl {
    Missing,
    Found(Option<BrowserUrl>),
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
                let url = data.url.as_deref().and_then(|raw| {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("about:blank") {
                        None
                    } else {
                        BrowserUrl::parse(trimmed).ok()
                    }
                });
                return QueriedUrl::Found(url);
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
    let line = read_bounded_line(&mut reader).ok()??;
    assert!(line.len() <= LINE_BYTES_MAX);
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
            encode_mouse(mouse, 3, 2, CellPixels { width: 10, height: 20 }),
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
    fn read_bounded_line_rejects_oversized_lines_without_spinning() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("line.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut client = UnixStream::connect(&socket).unwrap();
        let (server, _) = listener.accept().unwrap();

        let junk = vec![b'x'; LINE_BYTES_MAX + 128];
        let writer = thread::spawn(move || {
            let _ = client.write_all(&junk);
        });
        let mut reader = BufReader::new(server);
        let err = read_bounded_line(&mut reader).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        writer.join().unwrap();
    }

    #[test]
    fn kitty_graphics_and_placeholder_cells_encode_properly() {
        let frame = Frame {
            shm: "/px-12345".to_owned(),
            width: 800,
            height: 480,
            columns: 80,
            rows: 24,
            generation: 1,
        };
        let mut out = Vec::new();
        append_kitty_transmit(&mut out, 0x004c_0001, &frame);
        let text = String::from_utf8(out).unwrap();
        let expected_b64 = base64::engine::general_purpose::STANDARD.encode("/px-12345");
        assert_eq!(
            text,
            format!("\x1b_Ga=T,f=32,s=800,v=480,t=s,i=4980737,U=1,c=80,r=24,q=2;{expected_b64}\x1b\\")
        );

        let area = Rect::new(2, 3, 4, 2);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 10));
        paint_placeholders(&mut buffer, area, 0x004c_0001, 4, 2);
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

        let mut received = Vec::new();
        lune_waits::eventually("four browser events arrive", || {
            while let Ok(Event::Browser { heard, .. }) = events_rx.try_recv() {
                received.push(heard);
            }
            (received.len() >= 4).then_some(())
        });
        assert!(matches!(received[0], Heard::Frame(ref frame) if frame.shm == "/px-1"));
        assert_eq!(received[1], Heard::Title("GitHub".to_owned()));
        assert_eq!(received[2], Heard::Pointer(PointerShape::parse("pointer").unwrap()));
        assert_eq!(received[3], Heard::Clipboard("copied".to_owned()));

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
}

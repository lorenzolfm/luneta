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
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lune_domain::projects::{BrowserUrl, ViewId};
use luned::log;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;
use serde::{Deserialize, Serialize};

use crate::answers::{CellPixels, Colours};
use crate::host::Event;
use crate::machines::Machine;

pub const EMBEDDED_MAX: usize = 16;

const IMAGE_ID_BASE: u32 = 0x004c_0001;

const IMAGE_ID_SPAN: u32 = 0x0000_ffff;

const MAX_IMAGE_PX: u32 = 4096;

const LINE_BYTES_MAX: usize = 16 * 1024;

const STDERR_BYTES_MAX: usize = 2048;

const ACCEPT_POLL: Duration = Duration::from_millis(50);

const ACCEPT_TIMEOUT: Duration = Duration::from_secs(15);

const CONTROL_TIMEOUT: Duration = Duration::from_millis(300);

const CONTROL_SOCKETS_MAX: usize = 64;

const URL_POLL_MIN_GAP: Duration = Duration::from_millis(350);

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

const _: () = assert!(ROW_COLUMN_DIACRITICS.len() == 297);
const _: () = assert!(IMAGE_ID_BASE + IMAGE_ID_SPAN <= 0x00ff_ffff);

#[must_use]
pub fn cell_pixels(
    cell: Option<CellPixels>,
    window_pixels: Option<CellPixels>,
    columns: u16,
    rows: u16,
) -> CellPixels {
    if let Some(cell) = cell
        && cell.width > 0
        && cell.height > 0
    {
        return cell;
    }
    if let Some(window) = window_pixels
        && columns > 0
        && rows > 0
    {
        let width = window.width / columns;
        let height = window.height / rows;
        if width > 0 && height > 0 {
            return CellPixels { width, height };
        }
    }
    if let Ok(win) = rustix::termios::tcgetwinsize(io::stdout())
        && win.ws_xpixel > 0
        && win.ws_ypixel > 0
        && win.ws_col > 0
        && win.ws_row > 0
    {
        let width = win.ws_xpixel / win.ws_col;
        let height = win.ws_ypixel / win.ws_row;
        if width > 0 && height > 0 {
            return CellPixels { width, height };
        }
    }
    DEFAULT_CELL
}

#[must_use]
pub fn tty_path(runtime: &Path, view: ViewId) -> PathBuf {
    runtime.join(format!("b-{view}.tty"))
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
    pub cols: u16,
    pub rows: u16,
    pub generation: u64,
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
    Pointer(String),
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
        cols: u16,
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
        cols: u16,
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
        let (fg, bg) = colours.both();
        let [fr, fg_g, fb] = fg.unwrap_or([220, 224, 232]);
        let [br, bg_g, bb] = bg.unwrap_or([24, 24, 34]);
        (fg.is_some() || bg.is_some()).then_some(GuestColours {
            foreground: [fr, fg_g, fb, 255],
            background: [br, bg_g, bb, 255],
        })
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
        GuestMods {
            shift: modifiers.contains(KeyModifiers::SHIFT),
            alt: modifiers.contains(KeyModifiers::ALT),
            ctrl: modifiers.contains(KeyModifiers::CONTROL),
            super_key: modifiers.contains(KeyModifiers::SUPER),
        }
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
        cols: u16,
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

struct Instance {
    image_id: u32,
    cols: u16,
    rows: u16,
    visible: bool,
    focused: bool,
    frame: Option<Frame>,
    title: Option<String>,
    pointer: Option<String>,
    error: Option<String>,
    tty: PathBuf,
    socket: PathBuf,
    last_url_poll: Option<Instant>,
    stop: Arc<AtomicBool>,
    tx: Option<Sender<ToGuest>>,
}

impl Instance {
    fn send(&self, message: ToGuest) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(message);
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.tx = None;
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_file(&self.tty);
    }
}

pub struct Browsers {
    number: u64,
    config: Config,
    events: Sender<Event>,
    views: BTreeMap<ViewId, Instance>,
    next_id: u32,
    next_socket: u64,
    pending_graphics: Vec<u8>,
    owned_ids: BTreeSet<u32>,
}

impl Browsers {
    #[must_use]
    pub fn new(number: u64, config: Config, events: Sender<Event>) -> Browsers {
        Browsers {
            number,
            config,
            events,
            views: BTreeMap::new(),
            next_id: IMAGE_ID_BASE,
            next_socket: 0,
            pending_graphics: Vec::new(),
            owned_ids: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn supported(&self) -> bool {
        self.config.supported
    }

    pub fn sync(
        &mut self,
        machine: &Machine,
        desired: &[Desired<'_>],
        focused: Option<ViewId>,
        window_focused: bool,
    ) {
        if !self.config.supported {
            self.clear();
            return;
        }
        let wanted: BTreeSet<ViewId> = desired.iter().map(|it| it.view).collect();
        let removed: Vec<ViewId> =
            self.views.keys().copied().filter(|id| !wanted.contains(id)).collect();
        for view in removed {
            self.remove(view);
        }
        for item in desired {
            let cols = item.inner.width.max(1);
            let rows = item.inner.height.max(1);
            let is_focused = window_focused && item.visible && focused == Some(item.view);
            if let Some(instance) = self.views.get_mut(&item.view) {
                if instance.cols != cols || instance.rows != rows {
                    instance.cols = cols;
                    instance.rows = rows;
                    let (width, height) = pixel_size(cols, rows, self.config.cell);
                    instance.send(ToGuest::Size {
                        cols,
                        rows,
                        width,
                        height,
                        cell: [self.config.cell.width, self.config.cell.height],
                    });
                }
                if instance.visible != item.visible {
                    instance.visible = item.visible;
                    instance.send(ToGuest::Visible { value: item.visible });
                }
                if instance.focused != is_focused {
                    instance.focused = is_focused;
                    instance.send(ToGuest::Focus { focused: is_focused });
                }
                continue;
            }
            if self.views.len() >= EMBEDDED_MAX {
                continue;
            }
            self.spawn(item.view, machine, item.url, cols, rows, item.visible, is_focused);
        }
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
        self.remove(view);
        self.spawn(view, machine, url, inner.width.max(1), inner.height.max(1), true, focused);
    }

    fn spawn(
        &mut self,
        view: ViewId,
        machine: &Machine,
        url: Option<&BrowserUrl>,
        cols: u16,
        rows: u16,
        visible: bool,
        focused: bool,
    ) {
        let image_id = self.alloc_image_id();
        self.next_socket = self.next_socket.wrapping_add(1);
        let socket = self.config.runtime.join(format!("b-{}-{}.sock", std::process::id(), self.next_socket));
        let tty = tty_path(&self.config.runtime, view);
        let stop = Arc::new(AtomicBool::new(false));
        let _ = fs::remove_file(&socket);
        let _ = fs::remove_file(&tty);
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) => {
                self.insert_failed(view, image_id, cols, rows, visible, focused, socket, tty, stop, format!("embed socket failed: {error}"));
                return;
            },
        };
        if let Err(error) = listener.set_nonblocking(true) {
            let _ = fs::remove_file(&socket);
            self.insert_failed(view, image_id, cols, rows, visible, focused, socket, tty, stop, format!("embed socket setup failed: {error}"));
            return;
        }
        if let Err(error) = symlink("/dev/null", &tty) {
            let _ = fs::remove_file(&socket);
            self.insert_failed(view, image_id, cols, rows, visible, focused, socket, tty, stop, format!("browser tty setup failed: {error}"));
            return;
        }
        let mut command = Command::new("terminal-browser");
        command.arg("open");
        if let Machine::Remote(destination) = machine {
            command.arg(format!("--ssh={destination}"));
        }
        if let Some(url) = url {
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
                let _ = fs::remove_file(&socket);
                let _ = fs::remove_file(&tty);
                self.insert_failed(
                    view,
                    image_id,
                    cols,
                    rows,
                    visible,
                    focused,
                    socket,
                    tty,
                    stop,
                    MISSING_GUIDANCE.to_owned(),
                );
                return;
            },
            Err(error) => {
                let _ = fs::remove_file(&socket);
                let _ = fs::remove_file(&tty);
                self.insert_failed(
                    view,
                    image_id,
                    cols,
                    rows,
                    visible,
                    focused,
                    socket,
                    tty,
                    stop,
                    format!("terminal-browser did not start: {error}"),
                );
                return;
            },
        };
        let (tx, rx) = mpsc::channel();
        let (width, height) = pixel_size(cols, rows, self.config.cell);
        let init = ToGuest::Init {
            cols,
            rows,
            width,
            height,
            cell: [self.config.cell.width, self.config.cell.height],
            image_id,
            transport: "host",
            focused,
            colors: GuestColours::of(self.config.colours),
        };
        let peer = Peer {
            number: self.number,
            view,
            listener,
            init,
            rx,
            events: self.events.clone(),
            stop: Arc::clone(&stop),
        };
        thread::spawn(move || run_peer(peer, Some(child)));
        self.views.insert(view, Instance {
            image_id,
            cols,
            rows,
            visible,
            focused,
            frame: None,
            title: None,
            pointer: None,
            error: None,
            tty,
            socket,
            last_url_poll: None,
            stop,
            tx: Some(tx),
        });
    }

    #[expect(clippy::too_many_arguments, reason = "internal helper constructing a failed instance")]
    fn insert_failed(
        &mut self,
        view: ViewId,
        image_id: u32,
        cols: u16,
        rows: u16,
        visible: bool,
        focused: bool,
        socket: PathBuf,
        tty: PathBuf,
        stop: Arc<AtomicBool>,
        error: String,
    ) {
        self.views.insert(view, Instance {
            image_id,
            cols,
            rows,
            visible,
            focused,
            frame: None,
            title: None,
            pointer: None,
            error: Some(error),
            tty,
            socket,
            last_url_poll: None,
            stop,
            tx: None,
        });
    }

    fn alloc_image_id(&mut self) -> u32 {
        for _ in 0..IMAGE_ID_SPAN {
            let id = self.next_id;
            self.next_id = if self.next_id >= IMAGE_ID_BASE + IMAGE_ID_SPAN {
                IMAGE_ID_BASE
            } else {
                self.next_id + 1
            };
            if !self.owned_ids.contains(&id)
                && !self.views.values().any(|instance| instance.image_id == id)
            {
                return id;
            }
        }
        IMAGE_ID_BASE
    }

    fn remove(&mut self, view: ViewId) {
        if let Some(instance) = self.views.remove(&view) {
            self.delete_image(instance.image_id);
        }
    }

    pub fn clear(&mut self) {
        let ids: Vec<ViewId> = self.views.keys().copied().collect();
        for view in ids {
            self.remove(view);
        }
    }

    fn delete_image(&mut self, image_id: u32) {
        if self.owned_ids.remove(&image_id) {
            append_kitty_delete(&mut self.pending_graphics, image_id);
        }
    }

    pub fn heard(&mut self, view: ViewId, heard: Heard) -> Option<Outcome> {
        let number = self.number;
        let events = self.events.clone();
        let instance = self.views.get_mut(&view)?;
        match heard {
            Heard::Frame(frame) => {
                let image_id = instance.image_id;
                append_kitty_transmit(&mut self.pending_graphics, image_id, &frame);
                self.owned_ids.insert(image_id);
                instance.frame = Some(frame);
                instance.error = None;
                None
            },
            Heard::Title(title) => {
                let changed = instance.title.as_deref() != Some(title.as_str());
                instance.title = (!title.is_empty()).then_some(title);
                if changed {
                    let now = Instant::now();
                    let due = instance
                        .last_url_poll
                        .is_none_or(|last| now.saturating_duration_since(last) >= URL_POLL_MIN_GAP);
                    if due {
                        instance.last_url_poll = Some(now);
                        let tty = instance.tty.clone();
                        thread::spawn(move || {
                            if let QueriedUrl::Found(url) = query_url_for_tty(&tty) {
                                let _ = events.send(Event::Browser {
                                    number,
                                    view,
                                    heard: Heard::Url(url),
                                });
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
                instance.tx = None;
                instance.error = Some(why);
                self.delete_image(image_id);
                None
            },
        }
    }

    pub fn key(&self, view: ViewId, key: &KeyEvent) -> bool {
        let Some(instance) = self.views.get(&view) else {
            return false;
        };
        if instance.tx.is_none() {
            return false;
        }
        if let Some(message) = encode_key(key) {
            instance.send(message);
        }
        true
    }

    pub fn paste(&self, view: ViewId, text: &str) -> bool {
        let Some(instance) = self.views.get(&view) else {
            return false;
        };
        if instance.tx.is_none() {
            return false;
        }
        instance.send(ToGuest::Paste { text: text.to_owned() });
        true
    }

    pub fn mouse(&self, view: ViewId, mouse: MouseEvent, rel_col: u16, rel_row: u16) -> bool {
        let Some(instance) = self.views.get(&view) else {
            return false;
        };
        if instance.tx.is_none() {
            return false;
        }
        instance.send(encode_mouse(mouse, rel_col, rel_row, self.config.cell));
        true
    }

    pub fn colours(&mut self, colours: Colours) {
        self.config.colours = colours;
        let Some(guest) = GuestColours::of(colours) else {
            return;
        };
        for instance in self.views.values() {
            instance.send(ToGuest::Colors { colors: guest });
        }
    }

    pub fn cell_resized(&mut self, cell: CellPixels) {
        if self.config.cell == cell {
            return;
        }
        self.config.cell = cell;
        for instance in self.views.values() {
            let (width, height) = pixel_size(instance.cols, instance.rows, cell);
            instance.send(ToGuest::Size {
                cols: instance.cols,
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
    pub fn pointer(&self, view: ViewId) -> Option<&str> {
        let instance = self.views.get(&view)?;
        instance.frame.as_ref()?;
        instance.pointer.as_deref()
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
        paint_placeholders(buffer, seen, instance.image_id, frame.cols, frame.rows);
        Shown::Live
    }

    pub fn drain_graphics(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_graphics)
    }

    pub fn drain_all_deletes(&mut self) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.pending_graphics);
        let ids: Vec<u32> = std::mem::take(&mut self.owned_ids).into_iter().collect();
        for id in ids {
            append_kitty_delete(&mut out, id);
        }
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
    rx: mpsc::Receiver<ToGuest>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
}

fn run_peer(peer: Peer, mut child: Option<Child>) {
    let Peer { number, view, listener, init, rx, events, stop } = peer;
    let stderr = child.as_mut().and_then(|child| child.stderr.take()).map(|mut err| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.by_ref().take(STDERR_BYTES_MAX as u64).read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).trim().to_owned()
        })
    });
    let accepted = accept_peer(&listener, &stop, child.as_mut());
    match accepted {
        Ok(Some(stream)) => {
            serve_stream(stream, number, view, &init, rx, &events, &stop);
        },
        Ok(None) => {},
        Err(why) => {
            let stderr_text = stderr.and_then(|handle| handle.join().ok()).unwrap_or_default();
            let detail = if stderr_text.is_empty() { why } else { stderr_text };
            if !stop.load(Ordering::Relaxed) {
                let _ = events.send(Event::Browser {
                    number,
                    view,
                    heard: Heard::Exited(Exit::Failed(detail)),
                });
            }
            if let Some(mut child) = child {
                let _ = child.kill();
                let _ = child.wait();
            }
            return;
        },
    }
    if let Some(mut child) = child {
        let stopped = stop.load(Ordering::Relaxed);
        if stopped {
            let _ = child.kill();
            let _ = child.wait();
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
                let msg = if stderr_text.is_empty() {
                    format!("terminal-browser exited ({status})")
                } else {
                    stderr_text
                };
                Exit::Failed(msg)
            },
            Err(error) => Exit::Failed(format!("terminal-browser wait failed: {error}")),
        };
        let _ = events.send(Event::Browser { number, view, heard: Heard::Exited(exit) });
    } else if !stop.load(Ordering::Relaxed) {
        let _ = events.send(Event::Browser { number, view, heard: Heard::Exited(Exit::Clean) });
    }
}

fn accept_peer(
    listener: &UnixListener,
    stop: &AtomicBool,
    mut child: Option<&mut Child>,
) -> Result<Option<UnixStream>, String> {
    let deadline = Instant::now() + ACCEPT_TIMEOUT;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                return Ok(Some(stream));
            },
            Err(error) if error.kind() == ErrorKind::WouldBlock => {},
            Err(error) => return Err(format!("embed accept failed: {error}")),
        }
        if let Some(child) = child.as_deref_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            if status.success() {
                return Ok(None);
            }
            return Err(format!("terminal-browser exited before connecting ({status})"));
        }
        if Instant::now() >= deadline {
            return Err("terminal-browser did not connect in time".to_owned());
        }
        thread::sleep(ACCEPT_POLL);
    }
}

fn serve_stream(
    stream: UnixStream,
    number: u64,
    view: ViewId,
    init: &ToGuest,
    rx: mpsc::Receiver<ToGuest>,
    events: &Sender<Event>,
    stop: &Arc<AtomicBool>,
) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let Ok(Some(first)) = read_bounded_line(&mut reader) else {
        return;
    };
    if !matches!(serde_json::from_str::<FromGuest>(&first), Ok(FromGuest::Join {})) {
        log::say("embedded browser sent unexpected handshake");
        return;
    }
    if write_json_line(&mut writer, init).is_err() {
        return;
    }
    let write_stop = Arc::clone(stop);
    let Ok(shutdown_stream) = writer.try_clone() else {
        return;
    };
    thread::scope(|scope| {
        scope.spawn(move || {
            for message in rx {
                if write_stop.load(Ordering::Relaxed) {
                    break;
                }
                if write_json_line(&mut writer, &message).is_err() {
                    break;
                }
            }
            let _ = shutdown_stream.shutdown(Shutdown::Both);
        });
        while let Ok(Some(line)) = read_bounded_line(&mut reader) {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(message) = serde_json::from_str::<FromGuest>(&line) else {
                continue;
            };
            let heard = match message {
                FromGuest::Frame { shm, width, height, cols, rows, generation } => {
                    Heard::Frame(Frame { shm, width, height, cols, rows, generation })
                },
                FromGuest::Title { text } => Heard::Title(text),
                FromGuest::Pointer { shape } => Heard::Pointer(shape),
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
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let take = pos.min(LINE_BYTES_MAX.saturating_sub(buf.len()));
            buf.extend_from_slice(&available[..take]);
            reader.consume(pos + 1);
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
        let len = available.len();
        let take = len.min(LINE_BYTES_MAX.saturating_sub(buf.len()));
        buf.extend_from_slice(&available[..take]);
        reader.consume(len);
    }
}

fn write_json_line(writer: &mut impl io::Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

fn pixel_size(cols: u16, rows: u16, cell: CellPixels) -> (u32, u32) {
    let width = (u32::from(cols.max(1)) * u32::from(cell.width.max(1))).min(MAX_IMAGE_PX);
    let height = (u32::from(rows.max(1)) * u32::from(cell.height.max(1))).min(MAX_IMAGE_PX);
    (width, height)
}

fn append_kitty_transmit(out: &mut Vec<u8>, image_id: u32, frame: &Frame) {
    let payload = base64::engine::general_purpose::STANDARD.encode(frame.shm.as_bytes());
    let _ = write!(
        out,
        "\x1b_Ga=T,f=32,s={},v={},t=s,i={},U=1,c={},r={},q=2;{}\x1b\\",
        frame.width, frame.height, image_id, frame.cols, frame.rows, payload
    );
}

fn append_kitty_delete(out: &mut Vec<u8>, image_id: u32) {
    let _ = write!(out, "\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\");
}

fn paint_placeholders(buffer: &mut Buffer, seen: Rect, image_id: u32, cols: u16, rows: u16) {
    let max_dim = u16::try_from(ROW_COLUMN_DIACRITICS.len()).unwrap_or(u16::MAX);
    let draw_cols = cols.min(seen.width).min(max_dim);
    let draw_rows = rows.min(seen.height).min(max_dim);
    let fg = Color::Rgb(
        ((image_id >> 16) & 0xff) as u8,
        ((image_id >> 8) & 0xff) as u8,
        (image_id & 0xff) as u8,
    );
    let mut glyph = String::with_capacity(12);
    for row in 0..draw_rows {
        let row_diacritic = ROW_COLUMN_DIACRITICS[usize::from(row)];
        for col in 0..draw_cols {
            let Some(cell) = buffer.cell_mut((seen.x + col, seen.y + row)) else {
                continue;
            };
            glyph.clear();
            glyph.push(PLACEHOLDER);
            glyph.push(row_diacritic);
            glyph.push(ROW_COLUMN_DIACRITICS[usize::from(col)]);
            cell.reset();
            cell.set_symbol(&glyph);
            cell.set_fg(fg);
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
        KeyCode::Char(c) => {
            let s = c.to_string();
            let text = if mods.ctrl || mods.alt || mods.super_key {
                String::new()
            } else {
                s.clone()
            };
            (s, text)
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
        KeyCode::F(n @ 1..=12) => (format!("f{n}"), String::new()),
        _ => return None,
    };
    Some(ToGuest::Key { key: name, kind, text, mods })
}

fn encode_mouse(mouse: MouseEvent, rel_col: u16, rel_row: u16, cell: CellPixels) -> ToGuest {
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
    let cw = u32::from(cell.width.max(1));
    let ch = u32::from(cell.height.max(1));
    let x = u32::from(rel_col) * cw + cw / 2;
    let y = u32::from(rel_row) * ch + ch / 2;
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
    let want = tty.to_string_lossy();
    let Some(runtime) = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("XDG_STATE_HOME").map(PathBuf::from))
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
    else {
        return QueriedUrl::Missing;
    };
    let Ok(dirs) = fs::read_dir(runtime) else {
        return QueriedUrl::Missing;
    };
    let mut probed = 0usize;
    for entry in dirs.flatten() {
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
        for socket in sockets.flatten() {
            if probed >= CONTROL_SOCKETS_MAX {
                return QueriedUrl::Missing;
            }
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
                    if trimmed.is_empty() || trimmed == "about:blank" {
                        None
                    } else {
                        BrowserUrl::parse(trimmed).ok()
                    }
                });
                return QueriedUrl::Found(url);
            }
        }
    }
    QueriedUrl::Missing
}

fn ask_control_state(socket: &Path) -> Option<ControlData> {
    let mut stream = UnixStream::connect(socket).ok()?;
    let _ = stream.set_read_timeout(Some(CONTROL_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONTROL_TIMEOUT));
    stream.write_all(b"{\"id\":\"1\",\"cmd\":\"state\"}\n").ok()?;
    stream.flush().ok()?;
    let mut reader = BufReader::new(stream);
    let line = read_bounded_line(&mut reader).ok()??;
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
    fn kitty_graphics_and_placeholder_cells_encode_properly() {
        let frame = Frame {
            shm: "/px-12345".to_owned(),
            width: 800,
            height: 480,
            cols: 80,
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
        assert_eq!(cell.symbol(), format!("{PLACEHOLDER}{}{}", ROW_COLUMN_DIACRITICS[0], ROW_COLUMN_DIACRITICS[0]));
        assert_eq!(cell.fg, Color::Rgb(0x4c, 0x00, 0x01));
        let corner = &buffer[(5, 4)];
        assert_eq!(corner.symbol(), format!("{PLACEHOLDER}{}{}", ROW_COLUMN_DIACRITICS[1], ROW_COLUMN_DIACRITICS[3]));
    }

    #[test]
    fn peer_socket_completes_handshake_exchanges_frames_and_forwards_input() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("embed.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (events_tx, events_rx) = mpsc::channel();
        let (to_guest_tx, to_guest_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let view = ViewId::of(7).unwrap();
        let peer = Peer {
            number: 1,
            view,
            listener,
            init: ToGuest::Init {
                cols: 40,
                rows: 12,
                width: 400,
                height: 240,
                cell: [10, 20],
                image_id: IMAGE_ID_BASE,
                transport: "host",
                focused: true,
                colors: None,
            },
            rx: to_guest_rx,
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
        client.write_all(b"{\"type\":\"clipboard\",\"text\":\"copied\"}\n").unwrap();

        let mut received = Vec::new();
        lune_waits::eventually("four browser events arrive", || {
            while let Ok(Event::Browser { heard, .. }) = events_rx.try_recv() {
                received.push(heard);
            }
            (received.len() >= 4).then_some(())
        });
        assert!(matches!(received[0], Heard::Frame(ref f) if f.shm == "/px-1"));
        assert_eq!(received[1], Heard::Title("GitHub".to_owned()));
        assert_eq!(received[2], Heard::Pointer("pointer".to_owned()));
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

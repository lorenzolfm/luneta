//! What zellij writes of each session that runs, in its cache folder, as the
//! session changes: its tabs, its panes and where its clients are.
//!
//! The server writes `session-metadata.kdl` in a folder named for the session
//! within a second of a pane or a tab opening, closing or moving, or a client
//! coming, going or moving its focus (`background_jobs.rs` in zellij-server
//! 0.45.1 writes it from a ticker, and only where it differs from the file).
//! It writes nothing as a shell changes directory or runs a command, nor as a
//! pane takes a new title: those come in only along with something else.
//!
//! Every call of zellij's CLI has the server write the file again, the
//! daemon's own asking for panes among them, with the session's age anew, the
//! title and cursor of each pane as they are by then, and the panes of its
//! tabs in another order. A `Metadata` counts only what says which panes,
//! tabs and clients there are, and the panes in no order of zellij's, or each
//! listing would have the session listed again.

use std::ffi::OsString;
use std::fs::File;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{self, ErrorKind, Read as _};
use std::path::{Path, PathBuf};

use crate::zellij;

/// Where zellij writes the metadata of each session, in its cache folder: in
/// the folder of the contract between a zellij client and its server that
/// zellij 0.45 speaks. A zellij of another contract writes it elsewhere, and
/// the panes of its sessions are listed on the sweep alone.
const SESSIONS: [&str; 2] = [zellij::SPOKEN, "session_info"];

/// The file zellij writes a session's metadata in, in the session's folder.
const FILE: &str = "session-metadata.kdl";

/// The most of a file the daemon reads. A file grows by a few hundred bytes
/// for each pane: one past this has more panes than a screen holds, and what
/// changes past it goes unseen until the sweep.
const FILE_BYTES_MAX: u64 = 4 * 1024 * 1024;

/// What a `Metadata` counts of what zellij writes, by the name a line leads
/// with: which panes there are, in which tab, and whether each runs; the tabs,
/// by their place and name; and the clients, and what each has its focus on.
/// zellij 0.45.1 writes none of these anew with a call of its CLI. The rest
/// counts for nothing: what it does write anew, the session's age and each
/// pane's title and cursor; what luneta shows nothing of, as a pane's size;
/// and what a later zellij writes that this one does not, which it could
/// write anew, and which leaves a change it alone says to the sweep.
const COUNTED: [&str; 15] = [
    // A pane.
    "id",
    "is_plugin",
    "is_suppressed",
    "tab_position",
    "is_focused",
    "exited",
    "is_held",
    // A tab.
    "position",
    "name",
    "tab_id",
    "active",
    "other_focused_clients",
    // The clients, and the tabs and panes each has had its focus on.
    "connected_clients",
    "history",
    "pane_id",
];

/// zellij's folder of the metadata of its sessions: in `zellij` in
/// `$XDG_CACHE_HOME`, or else in `~/.cache`, on Linux, and in
/// `~/Library/Caches/org.Zellij-Contributors.Zellij` on macOS, as zellij-utils'
/// `consts.rs` has the `directories` crate find it. None without a home to
/// find it in.
#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| {
        std::env::var_os(name).filter(|value: &OsString| !value.is_empty()).map(PathBuf::from)
    };
    let cache = if cfg!(target_os = "macos") {
        set("HOME")?.join("Library/Caches/org.Zellij-Contributors.Zellij")
    } else {
        let xdg = set("XDG_CACHE_HOME").filter(|dir| dir.is_absolute());
        xdg.or_else(|| Some(set("HOME")?.join(".cache")))?.join("zellij")
    };
    Some(SESSIONS.iter().fold(cache, |path, part| path.join(part)))
}

/// Whether a change at `path`, in the folder, can be to a session's metadata.
/// Only the end of the path is read, as macOS can name the folder by a path
/// of its own.
#[must_use]
pub fn is_metadata(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == FILE)
}

/// What zellij last wrote of a session, as much of it as is `COUNTED`, and the
/// panes in no order of zellij's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata(u64);

impl Metadata {
    /// zellij writes the panes of each tab in the order of a `HashMap` it makes
    /// anew for each write, so the same panes come in another order nearly
    /// every time. Each pane, `pane {` to its `}`, is taken in an order of its
    /// own, and the rest as written. The lines of every pane in one order would
    /// not do: a focus that moves from one pane to another leaves the same
    /// lines.
    fn of(written: &str) -> Self {
        let counted =
            |line: &str| line.split_whitespace().next().is_some_and(|name| COUNTED.contains(&name));
        let mut rest: Vec<&str> = Vec::new();
        let mut panes: Vec<Vec<&str>> = Vec::new();
        let mut pane: Option<Vec<&str>> = None;
        for line in written.lines() {
            match (pane.as_mut(), line.trim()) {
                (None, "pane {") => pane = Some(Vec::new()),
                // zellij 0.45.1 writes a pane with nothing in braces in it.
                (Some(_), "}") => panes.extend(pane.take()),
                (_, line) if !counted(line) => {},
                (None, line) => rest.push(line),
                (Some(lines), line) => lines.push(line),
            }
        }
        // One cut off by `FILE_BYTES_MAX` counts as far as it reads.
        panes.extend(pane);
        panes.sort_unstable();
        let mut hasher = DefaultHasher::new();
        (rest, panes).hash(&mut hasher);
        Metadata(hasher.finish())
    }
}

/// What zellij last wrote of the session `name`, in `folder`: none before it
/// has, or where it writes none.
pub fn read(folder: &Path, name: &str) -> io::Result<Option<Metadata>> {
    let file = match File::open(folder.join(name).join(FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut written = Vec::new();
    file.take(FILE_BYTES_MAX).read_to_end(&mut written)?;
    Ok(Some(Metadata::of(&String::from_utf8_lossy(&written))))
}

#[cfg(test)]
mod tests {
    use super::Metadata;

    /// The file as zellij 0.45.1 writes it, cut down: a tab, a pane, and the
    /// session's age.
    fn written(tab: &str, title: &str, cursor: (u32, u32), age: u32) -> String {
        format!(
            "name \"work\"\ntabs {{\n    tab {{\n        position 0\n        name \"{tab}\"\n    \
             }}\n}}\npanes {{\n    pane {{\n        id 1\n        title \"{title}\"\n        \
             cursor_coordinates_in_pane {} {}\n        tab_position 0\n    }}\n}}\n\
             connected_clients 1\ncreation_time {age}\n",
            cursor.0, cursor.1
        )
    }

    /// The daemon's own asking has zellij write the session's age, its panes'
    /// titles and cursors anew, which tells it nothing, and so does what
    /// luneta shows nothing of, as a pane's size, or what a later zellij
    /// could write anew: what else zellij writes does.
    #[test]
    fn metadata_changes_with_what_zellij_writes_but_for_what_it_writes_anew() {
        let was = Metadata::of(&written("Tab #1", "~/work", (2, 0), 40));
        assert_eq!(Metadata::of(&written("Tab #1", "✳ Fix it", (7, 3), 41)), was);
        let sized = written("Tab #1", "~/work", (2, 0), 40).replace("id 1", "id 1\npane_rows 40");
        assert_eq!(Metadata::of(&format!("{sized}update_time 41\n")), was);
        assert_ne!(Metadata::of(&written("review", "~/work", (2, 0), 40)), was);
        let came = written("Tab #1", "~/work", (2, 0), 40).replace("clients 1", "clients 2");
        assert_ne!(Metadata::of(&came), was);
    }

    /// zellij writes the panes of its tabs in an order of its own each time:
    /// the same panes in another order are the same, but a focus that moves
    /// from one pane to another is not, though it leaves the same lines.
    #[test]
    fn metadata_takes_the_panes_in_no_order_of_zellij_s() {
        let pane = |id: u32, tab: u32, focused: bool| {
            format!(
                "    pane {{\n        id {id}\n        is_focused {focused}\n        \
                 tab_position {tab}\n    }}\n"
            )
        };
        let written = |panes: [String; 2]| format!("panes {{\n{}}}\n", panes.concat());
        let was = Metadata::of(&written([pane(1, 0, true), pane(2, 1, false)]));
        assert_eq!(Metadata::of(&written([pane(2, 1, false), pane(1, 0, true)])), was);
        assert_ne!(Metadata::of(&written([pane(1, 0, false), pane(2, 1, true)])), was);
    }
}

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{self, ErrorKind, Read as _};
use std::path::{Path, PathBuf};

use super::{SessionName, SPOKEN};

const SESSIONS: [&str; 2] = [SPOKEN, "session_info"];

const FILE: &str = "session-metadata.kdl";

const LAYOUT: &str = "session-layout.kdl";

const FILE_BYTES_MAX: u64 = 4 * 1024 * 1024;

const COUNTED: [&str; 15] = [
    "id",
    "is_plugin",
    "is_suppressed",
    "tab_position",
    "is_focused",
    "exited",
    "is_held",
    "position",
    "name",
    "tab_id",
    "active",
    "other_focused_clients",
    "connected_clients",
    "history",
    "pane_id",
];

#[must_use]
pub fn folder() -> Option<CacheFolder> {
    let set = |name| {
        std::env::var_os(name).filter(|value: &OsString| !value.is_empty()).map(PathBuf::from)
    };
    let cache = if cfg!(target_os = "macos") {
        set("HOME")?.join("Library/Caches/org.Zellij-Contributors.Zellij")
    } else {
        let xdg = set("XDG_CACHE_HOME").filter(|dir| dir.is_absolute());
        xdg.or_else(|| Some(set("HOME")?.join(".cache")))?.join("zellij")
    };
    Some(CacheFolder(SESSIONS.iter().fold(cache, |path, part| path.join(part))))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheFolder(PathBuf);

impl CacheFolder {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[must_use]
pub fn is_metadata(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == FILE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata(u64);

impl Metadata {
    fn of(written: &str) -> Self {
        let counted =
            |line: &str| line.split_whitespace().next().is_some_and(|name| COUNTED.contains(&name));
        let mut rest: Vec<&str> = Vec::new();
        let mut panes: Vec<Vec<&str>> = Vec::new();
        let mut pane: Option<Vec<&str>> = None;
        for line in written.lines() {
            match (pane.as_mut(), line.trim()) {
                (None, "pane {") => pane = Some(Vec::new()),
                (Some(_), "}") => panes.extend(pane.take()),
                (_, line) if !counted(line) => {},
                (None, line) => rest.push(line),
                (Some(lines), line) => lines.push(line),
            }
        }
        panes.extend(pane);
        panes.sort_unstable();
        let mut hasher = DefaultHasher::new();
        (rest, panes).hash(&mut hasher);
        Metadata(hasher.finish())
    }
}

pub fn read(folder: &CacheFolder, name: &SessionName) -> io::Result<Option<Metadata>> {
    let file = match File::open(folder.0.join(name.as_str()).join(FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut written = Vec::new();
    file.take(FILE_BYTES_MAX).read_to_end(&mut written)?;
    Ok(Some(Metadata::of(&String::from_utf8_lossy(&written))))
}

#[derive(Debug)]
pub enum Unknown {
    NoHome,
    Unread { folder: PathBuf, error: io::Error },
}

impl fmt::Display for Unknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unknown::NoHome => f.write_str("no home says where zellij's cache is"),
            Unknown::Unread { folder, error } => {
                write!(f, "{} does not read: {error}", folder.display())
            },
        }
    }
}

pub fn kept() -> Result<Vec<SessionName>, Unknown> {
    let CacheFolder(folder) = folder().ok_or(Unknown::NoHome)?;
    let unread = |error| Unknown::Unread { folder: folder.clone(), error };
    let entries = match fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(unread(error)),
    };
    let mut kept = Vec::new();
    for entry in entries {
        let entry = entry.map_err(unread)?;
        let file_name = entry.file_name();
        if let Some(name) = file_name.to_str().and_then(|name| SessionName::running(name).ok())
            && entry.path().join(LAYOUT).is_file()
        {
            kept.push(name);
        }
    }
    Ok(kept)
}

pub fn cached(session: &SessionName) -> Result<bool, Unknown> {
    let folder = folder().ok_or(Unknown::NoHome)?.0.join(session.as_str());
    folder.try_exists().map_err(|error| Unknown::Unread { folder, error })
}

#[cfg(test)]
mod tests {
    use super::Metadata;

    fn written(tab: &str, title: &str, cursor: (u32, u32), age: u32) -> String {
        format!(
            "name \"work\"\ntabs {{\n    tab {{\n        position 0\n        name \"{tab}\"\n    \
             }}\n}}\npanes {{\n    pane {{\n        id 1\n        title \"{title}\"\n        \
             cursor_coordinates_in_pane {} {}\n        tab_position 0\n    }}\n}}\n\
             connected_clients 1\ncreation_time {age}\n",
            cursor.0, cursor.1
        )
    }

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

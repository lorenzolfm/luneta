use std::fmt;
use std::path::{Component, Path};

use crate::said::Said;
use crate::v1;
use crate::{
    PATCH_BYTES_MAX, UNTRACKED_BYTES_MAX, UNTRACKED_FILES_MAX, UNTRACKED_TOTAL_BYTES_MAX,
};

pub const REF_LABEL_BYTES_MAX: usize = 1024;

pub const UNTRACKED_PATH_BYTES_MAX: usize = 4096;

const _: () = assert!(REF_LABEL_BYTES_MAX > 0);
const _: () = assert!(UNTRACKED_PATH_BYTES_MAX > 0);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativePath(String);

impl RelativePath {
    pub fn parse(raw: String) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("untracked path is empty".to_owned());
        }
        if raw.len() > UNTRACKED_PATH_BYTES_MAX {
            return Err(format!(
                "untracked path exceeds {UNTRACKED_PATH_BYTES_MAX} bytes"
            ));
        }
        if raw.chars().any(char::is_control) {
            return Err("untracked path contains a control character".to_owned());
        }
        if raw.starts_with('/') || raw.ends_with('/') || raw.contains("//") {
            return Err("untracked path has an empty segment".to_owned());
        }
        let mut has_normal = false;
        for component in Path::new(&raw).components() {
            match component {
                Component::Normal(_) => has_normal = true,
                Component::CurDir
                | Component::ParentDir
                | Component::RootDir
                | Component::Prefix(_) => {
                    return Err("untracked path is not a safe relative path".to_owned());
                },
            }
        }
        if !has_normal {
            return Err("untracked path has no normal component".to_owned());
        }
        Ok(RelativePath(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl AsRef<Path> for RelativePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<RelativePath> for String {
    fn from(path: RelativePath) -> Self {
        path.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UntrackedFile {
    path: RelativePath,
    content: String,
}

impl UntrackedFile {
    pub fn new(path: RelativePath, content: String) -> Result<Self, String> {
        if content.len() > UNTRACKED_BYTES_MAX {
            return Err(format!(
                "untracked file {} exceeds {UNTRACKED_BYTES_MAX} bytes",
                path.as_str()
            ));
        }
        if content.as_bytes().contains(&0) {
            return Err(format!("untracked file {} contains a NUL byte", path.as_str()));
        }
        Ok(UntrackedFile { path, content })
    }

    #[must_use]
    pub fn path(&self) -> &RelativePath {
        &self.path
    }

    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadDiff {
    base: Said,
    head: Said,
    patch: Vec<u8>,
    truncated: bool,
    untracked: Vec<UntrackedFile>,
}

fn parse_label(field: &str, raw: String) -> Result<Said, String> {
    if raw.len() > REF_LABEL_BYTES_MAX {
        return Err(format!("diff {field} label exceeds {REF_LABEL_BYTES_MAX} bytes"));
    }
    if raw.chars().any(char::is_control) {
        return Err(format!("diff {field} label contains a control character"));
    }
    Said::new(raw).ok_or_else(|| format!("diff {field} label is empty"))
}

impl ReadDiff {
    pub fn new(
        base: Said,
        head: Said,
        patch: Vec<u8>,
        truncated: bool,
        untracked: Vec<UntrackedFile>,
    ) -> Result<Self, String> {
        let base = parse_label("base", String::from(base))?;
        let head = parse_label("head", String::from(head))?;
        if patch.len() > PATCH_BYTES_MAX {
            return Err(format!("diff patch exceeds {PATCH_BYTES_MAX} bytes"));
        }
        if untracked.len() > UNTRACKED_FILES_MAX {
            return Err(format!("diff untracked files exceed {UNTRACKED_FILES_MAX}"));
        }
        let mut total_bytes = 0usize;
        for file in &untracked {
            total_bytes = total_bytes.saturating_add(file.content.len());
            if total_bytes > UNTRACKED_TOTAL_BYTES_MAX {
                return Err(format!(
                    "diff untracked content exceeds {UNTRACKED_TOTAL_BYTES_MAX} bytes"
                ));
            }
        }
        Ok(ReadDiff { base, head, patch, truncated, untracked })
    }

    pub fn of_api(raw: v1::ReadDiffResponse) -> Result<Self, String> {
        let base = parse_label("base", raw.base)?;
        let head = parse_label("head", raw.head)?;
        if raw.patch.len() > PATCH_BYTES_MAX {
            return Err(format!("diff patch exceeds {PATCH_BYTES_MAX} bytes"));
        }
        if raw.untracked.len() > UNTRACKED_FILES_MAX {
            return Err(format!("diff untracked files exceed {UNTRACKED_FILES_MAX}"));
        }
        let mut untracked = Vec::with_capacity(raw.untracked.len());
        let mut total_bytes = 0usize;
        for entry in raw.untracked {
            let path = RelativePath::parse(entry.path)?;
            let file = UntrackedFile::new(path, entry.content)?;
            total_bytes = total_bytes.saturating_add(file.content.len());
            if total_bytes > UNTRACKED_TOTAL_BYTES_MAX {
                return Err(format!(
                    "diff untracked content exceeds {UNTRACKED_TOTAL_BYTES_MAX} bytes"
                ));
            }
            untracked.push(file);
        }
        Ok(ReadDiff {
            base,
            head,
            patch: raw.patch,
            truncated: raw.truncated,
            untracked,
        })
    }

    #[must_use]
    pub fn into_api(self) -> v1::ReadDiffResponse {
        let untracked = self
            .untracked
            .into_iter()
            .map(|file| v1::read_diff_response::Untracked {
                path: String::from(file.path),
                content: file.content,
            })
            .collect();
        v1::ReadDiffResponse {
            base: String::from(self.base),
            head: String::from(self.head),
            patch: self.patch,
            truncated: self.truncated,
            untracked,
        }
    }

    #[must_use]
    pub fn base(&self) -> &Said {
        &self.base
    }

    #[must_use]
    pub fn head(&self) -> &Said {
        &self.head
    }

    #[must_use]
    pub fn patch(&self) -> &[u8] {
        &self.patch
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub fn untracked(&self) -> &[UntrackedFile] {
        &self.untracked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_response() -> v1::ReadDiffResponse {
        v1::ReadDiffResponse {
            base: "main".to_owned(),
            head: "feat".to_owned(),
            patch: b"diff --git a/a.rs b/a.rs\n".to_vec(),
            truncated: false,
            untracked: vec![v1::read_diff_response::Untracked {
                path: "src/.gitignore".to_owned(),
                content: "target/\n".to_owned(),
            }],
        }
    }

    #[test]
    fn accepts_valid_wire_response_and_round_trips() {
        let raw = valid_response();
        let parsed = ReadDiff::of_api(raw.clone()).unwrap();
        assert_eq!(parsed.base().as_str(), "main");
        assert_eq!(parsed.head().as_str(), "feat");
        assert_eq!(parsed.untracked().len(), 1);
        assert_eq!(parsed.untracked()[0].path().as_str(), "src/.gitignore");
        assert_eq!(parsed.into_api(), raw);
    }

    #[test]
    fn rejects_empty_or_control_labels_and_oversized_patch() {
        let mut empty_base = valid_response();
        empty_base.base.clear();
        assert!(ReadDiff::of_api(empty_base).is_err());

        let mut newline_head = valid_response();
        newline_head.head = "feat\nnext".to_owned();
        assert!(ReadDiff::of_api(newline_head).is_err());

        let mut huge_patch = valid_response();
        huge_patch.patch = vec![b'a'; PATCH_BYTES_MAX + 1];
        assert!(ReadDiff::of_api(huge_patch).is_err());
    }

    #[test]
    fn rejects_unsafe_or_oversized_untracked_entries() {
        for bad_path in ["", "/", "///", "./a.rs", "../a.rs", "a/../b.rs", "/etc/passwd", "a//b.rs", "a\0b", "a\nb"] {
            assert!(RelativePath::parse(bad_path.to_owned()).is_err(), "{bad_path:?}");
        }
        for good_path in [".gitignore", "a..b.rs", "crates/diff/src/lib.rs"] {
            assert!(RelativePath::parse(good_path.to_owned()).is_ok(), "{good_path:?}");
        }

        let mut too_big = valid_response();
        too_big.untracked[0].content = "x".repeat(UNTRACKED_BYTES_MAX + 1);
        assert!(ReadDiff::of_api(too_big).is_err());

        let mut too_many = valid_response();
        too_many.untracked = (0..=UNTRACKED_FILES_MAX)
            .map(|i| v1::read_diff_response::Untracked {
                path: format!("f{i}.txt"),
                content: "ok".to_owned(),
            })
            .collect();
        assert!(ReadDiff::of_api(too_many).is_err());

        let mut total_overflow = valid_response();
        let per_file = UNTRACKED_BYTES_MAX;
        let count = UNTRACKED_TOTAL_BYTES_MAX / per_file + 1;
        total_overflow.untracked = (0..count)
            .map(|i| v1::read_diff_response::Untracked {
                path: format!("big{i}.txt"),
                content: "x".repeat(per_file),
            })
            .collect();
        assert!(ReadDiff::of_api(total_overflow).is_err());
    }
}

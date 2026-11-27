use std::collections::{BTreeMap, BTreeSet};

use crate::patch::{FILES_MAX, FileStatus, Patch};

pub const PATH_DEPTH_MAX: usize = 64;

const TREE_ENTRIES_MAX: usize = FILES_MAX * (PATH_DEPTH_MAX + 1);

const _: () = assert!(PATH_DEPTH_MAX >= 1);
const _: () = assert!(TREE_ENTRIES_MAX >= FILES_MAX);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeNode {
    Dir {
        name: String,
        path: String,
        added: u32,
        deleted: u32,
    },
    File {
        file: usize,
        name: String,
        status: FileStatus,
        added: u32,
        deleted: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub depth: usize,
    pub node: TreeNode,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileTree {
    pub entries: Vec<TreeEntry>,
}

#[derive(Default)]
struct DirBuilder {
    dirs: BTreeMap<String, DirBuilder>,
    files: Vec<(String, usize, FileStatus, u32, u32)>,
    added: u32,
    deleted: u32,
}

impl DirBuilder {
    fn insert(
        &mut self,
        parts: &[&str],
        file: usize,
        status: FileStatus,
        added: u32,
        deleted: u32,
    ) {
        assert!(!parts.is_empty(), "a tree path has at least one component");
        let dir_depth = parts.len().saturating_sub(1).min(PATH_DEPTH_MAX - 1);
        let mut current = self;
        current.added = current.added.saturating_add(added);
        current.deleted = current.deleted.saturating_add(deleted);
        for segment in &parts[..dir_depth] {
            current = current.dirs.entry((*segment).to_owned()).or_default();
            current.added = current.added.saturating_add(added);
            current.deleted = current.deleted.saturating_add(deleted);
        }
        let leaf_name = parts[dir_depth..].join("/");
        assert!(!leaf_name.is_empty(), "a leaf file name is non-empty");
        current
            .files
            .push((leaf_name, file, status, added, deleted));
    }

    fn flatten(&self, folded: &BTreeSet<String>) -> Vec<TreeEntry> {
        let mut entries = Vec::new();
        let mut stack: Vec<FlattenFrame<'_>> = Vec::new();
        push_children(&mut stack, "", 0, self);
        let mut steps = 0usize;
        while let Some(frame) = stack.pop() {
            assert!(
                steps < TREE_ENTRIES_MAX,
                "tree flattening stays within bounded entry count"
            );
            steps += 1;
            match frame {
                FlattenFrame::File {
                    depth,
                    name,
                    file,
                    status,
                    added,
                    deleted,
                } => {
                    assert!(depth <= PATH_DEPTH_MAX, "file depth stays bounded");
                    entries.push(TreeEntry {
                        depth,
                        node: TreeNode::File {
                            file,
                            name,
                            status,
                            added,
                            deleted,
                        },
                    });
                }
                FlattenFrame::Dir {
                    prefix,
                    name,
                    depth,
                    dir,
                } => {
                    assert!(depth < PATH_DEPTH_MAX, "directory depth stays bounded");
                    let (label, full_path, node) = collapse_chain(&prefix, name, dir);
                    entries.push(TreeEntry {
                        depth,
                        node: TreeNode::Dir {
                            name: label,
                            path: full_path.clone(),
                            added: node.added,
                            deleted: node.deleted,
                        },
                    });
                    if !folded.contains(&full_path) {
                        push_children(&mut stack, &full_path, depth + 1, node);
                    }
                }
            }
        }
        assert!(
            entries.len() <= TREE_ENTRIES_MAX,
            "flattened tree stays within entry bound"
        );
        entries
    }
}

enum FlattenFrame<'a> {
    Dir {
        prefix: String,
        name: &'a str,
        depth: usize,
        dir: &'a DirBuilder,
    },
    File {
        depth: usize,
        name: String,
        file: usize,
        status: FileStatus,
        added: u32,
        deleted: u32,
    },
}

fn push_children<'a>(
    stack: &mut Vec<FlattenFrame<'a>>,
    prefix: &str,
    depth: usize,
    dir: &'a DirBuilder,
) {
    assert!(
        depth <= PATH_DEPTH_MAX,
        "child depth stays within PATH_DEPTH_MAX"
    );
    let mut files = dir.files.clone();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, file, status, added, deleted) in files.into_iter().rev() {
        stack.push(FlattenFrame::File {
            depth,
            name,
            file,
            status,
            added,
            deleted,
        });
    }
    for (name, child) in dir.dirs.iter().rev() {
        stack.push(FlattenFrame::Dir {
            prefix: prefix.to_owned(),
            name: name.as_str(),
            depth,
            dir: child,
        });
    }
}

fn collapse_chain<'a>(
    prefix: &str,
    first_name: &str,
    mut current: &'a DirBuilder,
) -> (String, String, &'a DirBuilder) {
    assert!(!first_name.is_empty(), "directory segment is non-empty");
    let mut label = first_name.to_owned();
    let mut full = if prefix.is_empty() {
        first_name.to_owned()
    } else {
        format!("{prefix}/{first_name}")
    };
    let mut collapsed = 0usize;
    while current.files.is_empty() && current.dirs.len() == 1 && collapsed < PATH_DEPTH_MAX {
        let (next_name, next_dir) = current.dirs.iter().next().expect("dirs.len() == 1");
        label.push('/');
        label.push_str(next_name);
        full.push('/');
        full.push_str(next_name);
        current = next_dir;
        collapsed += 1;
    }
    assert!(
        collapsed <= PATH_DEPTH_MAX,
        "collapsed chain length stays within PATH_DEPTH_MAX"
    );
    (label, full, current)
}

impl FileTree {
    #[must_use]
    pub fn build(patch: &Patch, folded: &BTreeSet<String>) -> FileTree {
        assert!(
            patch.files.len() <= FILES_MAX,
            "patch file count stays within FILES_MAX"
        );
        let mut root = DirBuilder::default();
        for (index, file) in patch.files.iter().enumerate() {
            let parts: Vec<&str> = file.path().segments().collect();
            root.insert(&parts, index, file.status(), file.added, file.deleted);
        }
        let entries = root.flatten(folded);
        assert!(
            entries.len() <= TREE_ENTRIES_MAX,
            "file tree entries stay bounded"
        );
        FileTree { entries }
    }

    #[must_use]
    pub fn index_of_file(&self, file_index: usize) -> Option<usize> {
        self.entries.iter().position(
            |entry| matches!(entry.node, TreeNode::File { file, .. } if file == file_index),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::FilePath;

    #[test]
    fn collapses_single_child_directories_and_rolls_up_counts() {
        let raw = "\
diff --git a/crates/diff/src/lib.rs b/crates/diff/src/lib.rs
--- a/crates/diff/src/lib.rs
+++ b/crates/diff/src/lib.rs
@@ -1 +1,2 @@
 a
+b
diff --git a/crates/diff/src/patch.rs b/crates/diff/src/patch.rs
--- a/crates/diff/src/patch.rs
+++ b/crates/diff/src/patch.rs
@@ -1,2 +1 @@
-x
 y
";
        let patch = Patch::parse(raw.as_bytes());
        let tree = FileTree::build(&patch, &BTreeSet::new());
        assert_eq!(tree.entries.len(), 3);
        let TreeNode::Dir {
            name,
            added,
            deleted,
            ..
        } = &tree.entries[0].node
        else {
            panic!("expected collapsed directory");
        };
        assert_eq!(name, "crates/diff/src");
        assert_eq!((*added, *deleted), (1, 1));
    }

    #[test]
    fn deeply_nested_path_builds_iteratively_within_depth_bound() {
        let deep = format!("{}leaf.rs", "d/".repeat(500));
        let mut patch = Patch::parse(b"");
        patch.push_untracked(FilePath::parse(&deep).unwrap(), "fn ok() {}\n");
        let tree = FileTree::build(&patch, &BTreeSet::new());
        assert!(tree.entries.len() >= 2);
        assert!(
            tree.entries
                .iter()
                .all(|entry| entry.depth <= PATH_DEPTH_MAX)
        );
        assert!(tree.entries.iter().any(|entry| matches!(
            &entry.node,
            TreeNode::File { name, .. } if name.ends_with("leaf.rs")
        )));
    }
}

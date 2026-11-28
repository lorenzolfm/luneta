use std::num::NonZeroUsize;
use std::ops::Range;

use crate::patch::{Block, ChangedBlock, ChangedLine, ContextLine, FileBody, Patch};

const DRIFT_ROWS_MAX: usize = 10;

const _: () = assert!(DRIFT_ROWS_MAX > 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextRef {
    file: usize,
    hunk: usize,
    block: usize,
    line: usize,
}

impl ContextRef {
    #[must_use]
    pub const fn file(self) -> usize {
        self.file
    }

    #[must_use]
    pub const fn hunk(self) -> usize {
        self.hunk
    }

    #[must_use]
    pub fn get(self, patch: &Patch) -> &ContextLine {
        let block = &patch.files[self.file].hunks()[self.hunk].blocks[self.block];
        match block {
            Block::Context(lines) => &lines[self.line],
            Block::Changed(_) => unreachable!("ContextRef is only constructed for Context blocks"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeletedRef {
    file: usize,
    hunk: usize,
    block: usize,
    line: usize,
}

impl DeletedRef {
    #[must_use]
    pub const fn file(self) -> usize {
        self.file
    }

    #[must_use]
    pub const fn hunk(self) -> usize {
        self.hunk
    }

    #[must_use]
    pub fn get(self, patch: &Patch) -> &ChangedLine {
        let block = &patch.files[self.file].hunks()[self.hunk].blocks[self.block];
        match block {
            Block::Changed(changed) => &changed.deleted()[self.line],
            Block::Context(_) => unreachable!("DeletedRef is only constructed for Changed blocks"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddedRef {
    file: usize,
    hunk: usize,
    block: usize,
    line: usize,
}

impl AddedRef {
    #[must_use]
    pub const fn file(self) -> usize {
        self.file
    }

    #[must_use]
    pub const fn hunk(self) -> usize {
        self.hunk
    }

    #[must_use]
    pub fn get(self, patch: &Patch) -> &ChangedLine {
        let block = &patch.files[self.file].hunks()[self.hunk].blocks[self.block];
        match block {
            Block::Changed(changed) => &changed.added()[self.line],
            Block::Context(_) => unreachable!("AddedRef is only constructed for Changed blocks"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeftRow {
    HunkHeader {
        file: usize,
        hunk: usize,
        text: String,
    },
    Context {
        line: u32,
        at: ContextRef,
    },
    Deleted {
        line: u32,
        at: DeletedRef,
    },
    Spacer,
}

impl LeftRow {
    #[must_use]
    pub const fn file_index(&self) -> Option<usize> {
        match self {
            LeftRow::HunkHeader { file, .. } => Some(*file),
            LeftRow::Context { at, .. } => Some(at.file()),
            LeftRow::Deleted { at, .. } => Some(at.file()),
            LeftRow::Spacer => None,
        }
    }

    #[must_use]
    pub const fn hunk_index(&self) -> Option<usize> {
        match self {
            LeftRow::HunkHeader { hunk, .. } => Some(*hunk),
            LeftRow::Context { at, .. } => Some(at.hunk()),
            LeftRow::Deleted { at, .. } => Some(at.hunk()),
            LeftRow::Spacer => None,
        }
    }

    #[must_use]
    pub const fn line_number(&self) -> Option<u32> {
        match self {
            LeftRow::Context { line, .. } | LeftRow::Deleted { line, .. } => Some(*line),
            LeftRow::HunkHeader { .. } | LeftRow::Spacer => None,
        }
    }

    #[must_use]
    pub fn text<'a>(&'a self, patch: &'a Patch) -> &'a str {
        match self {
            LeftRow::HunkHeader { text, .. } => text,
            LeftRow::Context { at, .. } => &at.get(patch).text,
            LeftRow::Deleted { at, .. } => &at.get(patch).text,
            LeftRow::Spacer => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RightRow {
    HunkHeader {
        file: usize,
        hunk: usize,
        text: String,
    },
    Context {
        line: u32,
        at: ContextRef,
    },
    Added {
        line: u32,
        at: AddedRef,
    },
    Spacer,
}

impl RightRow {
    #[must_use]
    pub const fn file_index(&self) -> Option<usize> {
        match self {
            RightRow::HunkHeader { file, .. } => Some(*file),
            RightRow::Context { at, .. } => Some(at.file()),
            RightRow::Added { at, .. } => Some(at.file()),
            RightRow::Spacer => None,
        }
    }

    #[must_use]
    pub const fn hunk_index(&self) -> Option<usize> {
        match self {
            RightRow::HunkHeader { hunk, .. } => Some(*hunk),
            RightRow::Context { at, .. } => Some(at.hunk()),
            RightRow::Added { at, .. } => Some(at.hunk()),
            RightRow::Spacer => None,
        }
    }

    #[must_use]
    pub const fn line_number(&self) -> Option<u32> {
        match self {
            RightRow::Context { line, .. } | RightRow::Added { line, .. } => Some(*line),
            RightRow::HunkHeader { .. } | RightRow::Spacer => None,
        }
    }

    #[must_use]
    pub fn text<'a>(&'a self, patch: &'a Patch) -> &'a str {
        match self {
            RightRow::HunkHeader { text, .. } => text,
            RightRow::Context { at, .. } => &at.get(patch).text,
            RightRow::Added { at, .. } => &at.get(patch).text,
            RightRow::Spacer => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitLine {
    FileHeader { file: usize },
    BinaryNotice { file: usize },
    Pair { left: LeftRow, right: RightRow },
}

impl SplitLine {
    #[must_use]
    pub const fn file_index(&self) -> Option<usize> {
        match self {
            SplitLine::FileHeader { file } | SplitLine::BinaryNotice { file } => Some(*file),
            SplitLine::Pair { left, right } => match right.file_index() {
                Some(file) => Some(file),
                None => left.file_index(),
            },
        }
    }

    #[must_use]
    pub const fn primary_line(&self) -> Option<u32> {
        match self {
            SplitLine::FileHeader { .. } | SplitLine::BinaryNotice { .. } => None,
            SplitLine::Pair { left, right } => match right.line_number() {
                Some(line) => Some(line),
                None => left.line_number(),
            },
        }
    }

    #[must_use]
    pub const fn has_line(&self, target: u32) -> bool {
        match self {
            SplitLine::FileHeader { .. } | SplitLine::BinaryNotice { .. } => false,
            SplitLine::Pair { left, right } => {
                matches!(right.line_number(), Some(n) if n == target)
                    || matches!(left.line_number(), Some(n) if n == target)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowSpan {
    start: usize,
    count: NonZeroUsize,
}

impl RowSpan {
    #[must_use]
    pub const fn new(start: usize, count: NonZeroUsize) -> RowSpan {
        RowSpan { start, count }
    }

    #[must_use]
    pub fn from_range(start: usize, end: usize) -> Option<RowSpan> {
        let len = end.checked_sub(start)?;
        Some(RowSpan {
            start,
            count: NonZeroUsize::new(len)?,
        })
    }

    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> usize {
        self.start + self.count.get()
    }

    #[must_use]
    pub const fn count(self) -> NonZeroUsize {
        self.count
    }

    #[must_use]
    pub const fn to_range(self) -> Range<usize> {
        self.start..self.end()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RibbonKind {
    Modified,
    Inserted,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RibbonShape {
    Modified { left: RowSpan, right: RowSpan },
    Inserted { left_seam: usize, right: RowSpan },
    Deleted { left: RowSpan, right_seam: usize },
}

impl RibbonShape {
    #[must_use]
    pub const fn kind(self) -> RibbonKind {
        match self {
            RibbonShape::Modified { .. } => RibbonKind::Modified,
            RibbonShape::Inserted { .. } => RibbonKind::Inserted,
            RibbonShape::Deleted { .. } => RibbonKind::Deleted,
        }
    }

    #[must_use]
    pub const fn spans(self) -> (Range<usize>, Range<usize>) {
        match self {
            RibbonShape::Modified { left, right } => (left.to_range(), right.to_range()),
            RibbonShape::Inserted { left_seam, right } => (left_seam..left_seam, right.to_range()),
            RibbonShape::Deleted { left, right_seam } => (left.to_range(), right_seam..right_seam),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ribbon {
    pub shape: RibbonShape,
    pub file: usize,
    pub hunk: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileSpan {
    pub file: usize,
    pub start_row: usize,
    pub end_row: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HunkAnchor {
    pub file: usize,
    pub hunk: usize,
    pub row: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SplitLayout {
    pub rows: Vec<SplitLine>,
    pub ribbons: Vec<Ribbon>,
    pub files: Vec<FileSpan>,
    pub hunks: Vec<HunkAnchor>,
}

impl SplitLayout {
    #[must_use]
    pub fn build(patch: &Patch) -> SplitLayout {
        let mut rows = Vec::new();
        let mut ribbons = Vec::new();
        let mut files = Vec::new();
        let mut hunks = Vec::new();
        let mut pending_left: Vec<LeftRow> = Vec::new();
        let mut pending_right: Vec<RightRow> = Vec::new();

        for (file_index, file) in patch.files.iter().enumerate() {
            let start_row = rows.len();
            rows.push(SplitLine::FileHeader { file: file_index });
            match &file.body {
                FileBody::Binary => {
                    rows.push(SplitLine::BinaryNotice { file: file_index });
                }
                FileBody::Text(file_hunks) => {
                    for (hunk_index, hunk) in file_hunks.iter().enumerate() {
                        if pending_left.len().abs_diff(pending_right.len()) > DRIFT_ROWS_MAX {
                            flush_pair_rows(&mut rows, &mut pending_left, &mut pending_right);
                        }
                        let anchor = rows.len() + pending_left.len().min(pending_right.len());
                        hunks.push(HunkAnchor {
                            file: file_index,
                            hunk: hunk_index,
                            row: anchor,
                        });
                        let header = hunk.header_text();
                        pending_left.push(LeftRow::HunkHeader {
                            file: file_index,
                            hunk: hunk_index,
                            text: header.clone(),
                        });
                        pending_right.push(RightRow::HunkHeader {
                            file: file_index,
                            hunk: hunk_index,
                            text: header,
                        });
                        for (block_index, block) in hunk.blocks.iter().enumerate() {
                            match block {
                                Block::Context(lines) => {
                                    for (line_index, ctx) in lines.iter().enumerate() {
                                        let at = ContextRef {
                                            file: file_index,
                                            hunk: hunk_index,
                                            block: block_index,
                                            line: line_index,
                                        };
                                        pending_left.push(LeftRow::Context {
                                            line: ctx.old_line,
                                            at,
                                        });
                                        pending_right.push(RightRow::Context {
                                            line: ctx.new_line,
                                            at,
                                        });
                                    }
                                }
                                Block::Changed(changed) => {
                                    let left_start = rows.len() + pending_left.len();
                                    for (line_index, del) in changed.deleted().iter().enumerate() {
                                        let at = DeletedRef {
                                            file: file_index,
                                            hunk: hunk_index,
                                            block: block_index,
                                            line: line_index,
                                        };
                                        pending_left.push(LeftRow::Deleted { line: del.line, at });
                                    }
                                    let right_start = rows.len() + pending_right.len();
                                    for (line_index, add) in changed.added().iter().enumerate() {
                                        let at = AddedRef {
                                            file: file_index,
                                            hunk: hunk_index,
                                            block: block_index,
                                            line: line_index,
                                        };
                                        pending_right.push(RightRow::Added { line: add.line, at });
                                    }
                                    let shape = match changed {
                                        ChangedBlock::Modified { deleted, added } => {
                                            RibbonShape::Modified {
                                                left: RowSpan::new(left_start, deleted.count()),
                                                right: RowSpan::new(right_start, added.count()),
                                            }
                                        }
                                        ChangedBlock::Added(added) => RibbonShape::Inserted {
                                            left_seam: left_start,
                                            right: RowSpan::new(right_start, added.count()),
                                        },
                                        ChangedBlock::Deleted(deleted) => RibbonShape::Deleted {
                                            left: RowSpan::new(left_start, deleted.count()),
                                            right_seam: right_start,
                                        },
                                    };
                                    ribbons.push(Ribbon {
                                        shape,
                                        file: file_index,
                                        hunk: hunk_index,
                                    });
                                }
                            }
                        }
                    }
                    flush_pair_rows(&mut rows, &mut pending_left, &mut pending_right);
                }
            }
            let end_row = rows.len();
            assert!(
                start_row < end_row,
                "every file emits at least its header row"
            );
            files.push(FileSpan {
                file: file_index,
                start_row,
                end_row,
            });
        }
        assert_eq!(
            files.len(),
            patch.files.len(),
            "every patch file has a split span"
        );
        for ribbon in &ribbons {
            let (left, right) = ribbon.shape.spans();
            assert!(left.end <= rows.len(), "ribbon left end fits layout");
            assert!(right.end <= rows.len(), "ribbon right end fits layout");
        }
        for anchor in &hunks {
            assert!(anchor.row < rows.len(), "split hunk anchor fits layout");
        }
        SplitLayout {
            rows,
            ribbons,
            files,
            hunks,
        }
    }

    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

fn flush_pair_rows(
    rows: &mut Vec<SplitLine>,
    pending_left: &mut Vec<LeftRow>,
    pending_right: &mut Vec<RightRow>,
) {
    let count = pending_left.len().max(pending_right.len());
    let mut left_iter = pending_left.drain(..);
    let mut right_iter = pending_right.drain(..);
    for _ in 0..count {
        rows.push(SplitLine::Pair {
            left: left_iter.next().unwrap_or(LeftRow::Spacer),
            right: right_iter.next().unwrap_or(RightRow::Spacer),
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnifiedRow {
    FileHeader {
        file: usize,
    },
    BinaryNotice {
        file: usize,
    },
    HunkHeader {
        file: usize,
        hunk: usize,
        text: String,
    },
    Context {
        old_line: u32,
        new_line: u32,
        at: ContextRef,
    },
    Deleted {
        old_line: u32,
        at: DeletedRef,
    },
    Added {
        new_line: u32,
        at: AddedRef,
    },
}

impl UnifiedRow {
    #[must_use]
    pub const fn file_index(&self) -> usize {
        match self {
            UnifiedRow::FileHeader { file }
            | UnifiedRow::BinaryNotice { file }
            | UnifiedRow::HunkHeader { file, .. } => *file,
            UnifiedRow::Context { at, .. } => at.file(),
            UnifiedRow::Deleted { at, .. } => at.file(),
            UnifiedRow::Added { at, .. } => at.file(),
        }
    }

    #[must_use]
    pub const fn primary_line(&self) -> Option<u32> {
        match self {
            UnifiedRow::Context { new_line, .. } | UnifiedRow::Added { new_line, .. } => {
                Some(*new_line)
            }
            UnifiedRow::Deleted { old_line, .. } => Some(*old_line),
            UnifiedRow::FileHeader { .. }
            | UnifiedRow::BinaryNotice { .. }
            | UnifiedRow::HunkHeader { .. } => None,
        }
    }

    #[must_use]
    pub const fn hunk_index(&self) -> Option<usize> {
        match self {
            UnifiedRow::HunkHeader { hunk, .. } => Some(*hunk),
            UnifiedRow::Context { at, .. } => Some(at.hunk()),
            UnifiedRow::Deleted { at, .. } => Some(at.hunk()),
            UnifiedRow::Added { at, .. } => Some(at.hunk()),
            UnifiedRow::FileHeader { .. } | UnifiedRow::BinaryNotice { .. } => None,
        }
    }

    #[must_use]
    pub fn text<'a>(&'a self, patch: &'a Patch) -> &'a str {
        match self {
            UnifiedRow::FileHeader { file } => {
                assert!(*file < patch.files.len(), "file header index in bounds");
                patch.files[*file].path().as_str()
            }
            UnifiedRow::BinaryNotice { .. } => "Binary file changed",
            UnifiedRow::HunkHeader { text, .. } => text,
            UnifiedRow::Context { at, .. } => &at.get(patch).text,
            UnifiedRow::Deleted { at, .. } => &at.get(patch).text,
            UnifiedRow::Added { at, .. } => &at.get(patch).text,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnifiedLayout {
    pub rows: Vec<UnifiedRow>,
    pub files: Vec<FileSpan>,
    pub hunks: Vec<HunkAnchor>,
}

impl UnifiedLayout {
    #[must_use]
    pub fn build(patch: &Patch) -> UnifiedLayout {
        let mut rows = Vec::new();
        let mut files = Vec::new();
        let mut hunks = Vec::new();

        for (file_index, file) in patch.files.iter().enumerate() {
            let start_row = rows.len();
            rows.push(UnifiedRow::FileHeader { file: file_index });
            match &file.body {
                FileBody::Binary => {
                    rows.push(UnifiedRow::BinaryNotice { file: file_index });
                }
                FileBody::Text(file_hunks) => {
                    for (hunk_index, hunk) in file_hunks.iter().enumerate() {
                        hunks.push(HunkAnchor {
                            file: file_index,
                            hunk: hunk_index,
                            row: rows.len(),
                        });
                        rows.push(UnifiedRow::HunkHeader {
                            file: file_index,
                            hunk: hunk_index,
                            text: hunk.header_text(),
                        });
                        for (block_index, block) in hunk.blocks.iter().enumerate() {
                            match block {
                                Block::Context(lines) => {
                                    for (line_index, ctx) in lines.iter().enumerate() {
                                        let at = ContextRef {
                                            file: file_index,
                                            hunk: hunk_index,
                                            block: block_index,
                                            line: line_index,
                                        };
                                        rows.push(UnifiedRow::Context {
                                            old_line: ctx.old_line,
                                            new_line: ctx.new_line,
                                            at,
                                        });
                                    }
                                }
                                Block::Changed(changed) => {
                                    for (line_index, del) in changed.deleted().iter().enumerate() {
                                        let at = DeletedRef {
                                            file: file_index,
                                            hunk: hunk_index,
                                            block: block_index,
                                            line: line_index,
                                        };
                                        rows.push(UnifiedRow::Deleted {
                                            old_line: del.line,
                                            at,
                                        });
                                    }
                                    for (line_index, add) in changed.added().iter().enumerate() {
                                        let at = AddedRef {
                                            file: file_index,
                                            hunk: hunk_index,
                                            block: block_index,
                                            line: line_index,
                                        };
                                        rows.push(UnifiedRow::Added {
                                            new_line: add.line,
                                            at,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let end_row = rows.len();
            assert!(
                start_row < end_row,
                "every file emits at least its header row"
            );
            files.push(FileSpan {
                file: file_index,
                start_row,
                end_row,
            });
        }
        assert_eq!(
            files.len(),
            patch.files.len(),
            "every patch file has a unified span"
        );
        for anchor in &hunks {
            assert!(anchor.row < rows.len(), "unified hunk anchor fits layout");
        }
        UnifiedLayout { rows, files, hunks }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTI_HUNK: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ a/src/lib.rs
@@ -1,6 +1,4 @@
 a
-b
-c
-d
+x
 e
@@ -10,3 +8,5 @@
 f
+g
+h
 i
";

    #[test]
    fn unpadded_split_flows_compactly_and_connects_spans_with_ribbons() {
        let patch = Patch::parse(MULTI_HUNK.as_bytes());
        let split = SplitLayout::build(&patch);
        assert_eq!(split.ribbons.len(), 2);

        let RibbonShape::Modified { left, right } = split.ribbons[0].shape else {
            panic!("expected modified ribbon");
        };
        assert_eq!(left.count().get(), 3);
        assert_eq!(right.count().get(), 1);
        assert_eq!(left.start(), right.start());

        let RibbonShape::Inserted { left_seam, right } = split.ribbons[1].shape else {
            panic!("expected inserted ribbon");
        };
        assert_eq!(right.count().get(), 2);
        assert!(left_seam > right.start());
    }

    #[test]
    fn unified_layout_preserves_every_line_and_hunk_anchor() {
        let patch = Patch::parse(MULTI_HUNK.as_bytes());
        let unified = UnifiedLayout::build(&patch);
        assert_eq!(unified.files.len(), 1);
        assert_eq!(unified.hunks.len(), 2);
        assert!(matches!(
            unified.rows[unified.hunks[0].row],
            UnifiedRow::HunkHeader { hunk: 0, .. }
        ));
        assert!(matches!(
            unified.rows[unified.hunks[1].row],
            UnifiedRow::HunkHeader { hunk: 1, .. }
        ));
    }
}

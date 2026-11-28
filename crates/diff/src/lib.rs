pub mod highlight;
pub mod layout;
pub mod patch;
pub mod ribbon;
pub mod tree;
pub mod viewer;
pub mod words;

pub use highlight::{Language, SyntaxRole, TokenSpan};
pub use layout::{
    AddedRef, ContextRef, DeletedRef, FileSpan, HunkAnchor, LeftRow, Ribbon, RibbonKind,
    RibbonShape, RightRow, RowSpan, SplitLayout, SplitLine, SplitPair, UnifiedLayout, UnifiedRow,
};
pub use patch::{
    Block, ChangedBlock, ChangedLine, ContextLine, FILES_MAX, FileBody, FileChange, FilePatch,
    FilePath, FileStatus, HUNKS_MAX, Hunk, LINE_BYTES_MAX, LINES_MAX, NonEmpty, PATCH_BYTES_MAX,
    Patch,
};
pub use ribbon::{
    GUTTER_COLS, Rgb, RibbonCell, RibbonImage, RibbonPalette, rasterize_cells, rasterize_rgba,
};
pub use tree::{FileTree, PATH_DEPTH_MAX, TreeEntry, TreeNode};
pub use viewer::{
    ActiveLayout, CommittedSearch, EXCERPT_BYTES_MAX, EXCERPT_LINES_MAX, ExcerptBody, ExcerptError,
    ExcerptText, LINE_NUMBER_COLS, LayoutMode, LineRange, SEARCH_QUERY_BYTES_MAX,
    SPLIT_HARD_MIN_WIDTH, SPLIT_MIN_WIDTH, SearchMatch, SearchMatches, SearchQuery,
    SelectionExcerpt, Viewer, ViewerFocus, find_case_insensitive_range,
};
pub use words::ByteSpan;

//! The confirm step.
//!
//! `Enter` on an empty match set must never create a session in one keystroke, and the layout
//! screen upstream already has *is* that confirm — it creates nothing, it only changes what the
//! next `Enter` means. `Esc` backs out with the session name intact, so an accidental `Enter`
//! costs one keystroke to undo.
//!
//! Deliberately smaller than upstream's `new_session_info.rs` (491 lines): that file also
//! collects the session *name* on its own screen, since upstream's search prompt is a filter
//! first and a name field second. Here the name is already typed — it is the search term — so
//! only the layout list is left. There is no layout search either: with no `layout_dir` in
//! `config.kdl` the list is the handful of built-ins, and a fuzzy filter over five rows would be
//! machinery for its own sake.

use zellij_tile::prelude::LayoutInfo;

/// Layout choice for a session about to be created.
#[derive(Default)]
pub struct LayoutList {
    pub layouts: Vec<LayoutInfo>,
    pub selected: usize,
}

impl LayoutList {
    /// The list arrives on the current session's `SessionInfo`, from the same one-second poll
    /// that feeds the session list — no extra host call and no `SessionUpdate` subscription.
    pub fn update(&mut self, layouts: Vec<LayoutInfo>) {
        if layouts.len() != self.layouts.len() {
            // Same reasoning as upstream: an index into a list that changed length underneath is
            // not worth preserving.
            self.selected = 0;
        }
        self.layouts = layouts;
    }

    /// Entering the screen always starts on the default (`selected = 0`), which is what makes
    /// `Enter` `Enter` mean "create with the default layout".
    pub fn reset(&mut self) {
        self.selected = 0;
    }

    /// No wrap, matching the session list's cursor discipline.
    pub fn move_selection(&mut self, delta: isize) {
        let last = self.layouts.len().saturating_sub(1);
        self.selected = (self.selected as isize + delta).clamp(0, last as isize) as usize;
    }

    pub fn selected_layout(&self) -> Option<&LayoutInfo> {
        self.layouts.get(self.selected)
    }
}

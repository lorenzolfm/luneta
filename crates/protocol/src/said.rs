//! Text that another program said: Claude Code in a session file, zellij of a
//! pane, git of a work tree. One that says nothing has said nothing, so such
//! text is never empty, and the protocol has it absent instead.

/// Text a program said, never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said(String);

impl Said {
    /// `text`, unless it is empty.
    #[must_use]
    pub fn new(text: String) -> Option<Self> {
        (!text.is_empty()).then_some(Said(text))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<Said> for String {
    fn from(said: Said) -> Self {
        said.0
    }
}

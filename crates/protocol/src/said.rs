#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Said(String);

impl Said {
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

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Directory(String);

impl Directory {
    pub fn parse(path: String) -> Result<Self, String> {
        if path.starts_with('/') {
            return Ok(Directory(path));
        }
        Err(format!("{path:?} is no absolute path"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Directory> for String {
    fn from(directory: Directory) -> Self {
        directory.0
    }
}

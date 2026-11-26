use std::fmt;

use lune_domain::projects::PATH_BYTES_MAX;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Directory(String);

impl Directory {
    pub fn parse(path: String) -> Result<Self, String> {
        if path.len() > PATH_BYTES_MAX {
            return Err(format!("a path is longer than {PATH_BYTES_MAX} bytes"));
        }
        if path.contains('\0') {
            return Err(format!("{path:?} has a NUL"));
        }
        if path.starts_with('/') {
            return Ok(Directory(path));
        }
        Err(format!("{path:?} is no absolute path"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn child(&self, name: &str) -> Directory {
        Directory(format!("{}/{name}", self.0.trim_end_matches('/')))
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

impl From<&lune_domain::projects::Dir> for Directory {
    fn from(dir: &lune_domain::projects::Dir) -> Self {
        Directory(dir.as_str().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_is_absolute_bounded_and_has_no_nul() {
        assert_eq!(Directory::parse("/".to_owned()).unwrap().as_str(), "/");
        assert_eq!(Directory::parse("/w/lune".to_owned()).unwrap().as_str(), "/w/lune");
        assert!(Directory::parse(String::new()).is_err());
        assert!(Directory::parse("w/lune".to_owned()).is_err());
        assert!(Directory::parse("/w/\0lune".to_owned()).is_err());
        let too_long = format!("/{}", "a".repeat(PATH_BYTES_MAX));
        assert!(Directory::parse(too_long).is_err());
    }
}

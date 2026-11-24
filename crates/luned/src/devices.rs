use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use serde::Deserialize;

pub const FILE: &str = "devices";

const DEVICES_MAX: usize = 16;

const READ_MAX: u64 = 4096;

const ID_BYTES_MAX: usize = 64;

const WRITABLE_BY_OTHERS: u32 = 0o022;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct DeviceId(String);

impl TryFrom<String> for DeviceId {
    type Error = String;

    fn try_from(id: String) -> Result<DeviceId, String> {
        if !(1..=ID_BYTES_MAX).contains(&id.len()) {
            let bytes = id.len();
            return Err(format!("{id:?} is not a device's StableID: it is {bytes} bytes, not 1 to {ID_BYTES_MAX}"));
        }
        if !id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(format!("{id:?} is not a device's StableID: it has a byte that is no letter or digit"));
        }
        Ok(DeviceId(id))
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Allowed(BTreeSet<DeviceId>);

impl Allowed {
    pub fn lets_in(&self, id: &DeviceId) -> bool {
        self.0.contains(id)
    }
}

pub fn allowed_of(text: &str) -> Result<Allowed, String> {
    let bytes = u64::try_from(text.len()).expect("a length fits in 64 bits");
    assert!(bytes <= READ_MAX, "luned reads {READ_MAX} bytes of the devices file at the most");
    let mut allowed = BTreeSet::new();
    for (number, line) in (1_usize..).zip(text.lines()) {
        let Some(id) = line.split_whitespace().next() else {
            continue;
        };
        let id = DeviceId::try_from(id.to_owned()).map_err(|why| format!("line {number}: {why}"))?;
        allowed.insert(id);
        if allowed.len() > DEVICES_MAX {
            return Err(format!("it names more than {DEVICES_MAX} devices"));
        }
    }
    assert!(allowed.len() <= DEVICES_MAX, "luned lets in {DEVICES_MAX} devices at the most");
    Ok(Allowed(allowed))
}

pub fn allowed(path: &Path) -> Result<Allowed, String> {
    let said = |why: &dyn fmt::Display| format!("{}: {why}", path.display());
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Allowed::default()),
        Err(error) => return Err(said(&error)),
    };
    owned_by_me(&file).map_err(|why| said(&why))?;
    let mut text = Vec::new();
    file.take(READ_MAX + 1).read_to_end(&mut text).map_err(|error| said(&error))?;
    let bytes = u64::try_from(text.len()).expect("a length fits in 64 bits");
    if bytes > READ_MAX {
        return Err(said(&format!("it is longer than {READ_MAX} bytes")));
    }
    let text = String::from_utf8(text).map_err(|_| said(&"it is not UTF-8"))?;
    allowed_of(&text).map_err(|why| said(&why))
}

fn owned_by_me(file: &File) -> Result<(), String> {
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    let me = rustix::process::getuid().as_raw();
    if metadata.uid() != me {
        return Err(format!("it is user {}'s, and luned trusts only user {me}'s", metadata.uid()));
    }
    let writable = metadata.mode() & WRITABLE_BY_OTHERS;
    if writable != 0 {
        return Err(format!("others may write it (mode {:o}): chmod go-w it", metadata.mode() & 0o777));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn id(id: &str) -> DeviceId {
        DeviceId::try_from(id.to_owned()).unwrap()
    }

    #[test]
    fn a_line_lets_in_the_device_its_first_word_names_and_the_rest_is_yours() {
        let allowed = allowed_of("nPhone1CNTRL  phone, the one in my pocket\n\n   \n  nPad2CNTRL\n").unwrap();
        assert!(allowed.lets_in(&id("nPhone1CNTRL")));
        assert!(allowed.lets_in(&id("nPad2CNTRL")));
        assert!(!allowed.lets_in(&id("phone")));
        assert!(!allowed.lets_in(&id("nOther3CNTRL")));
        assert_eq!(allowed_of("").unwrap(), Allowed::default());
    }

    #[test]
    fn a_word_that_is_not_a_stable_id_lets_no_device_in() {
        let refused = allowed_of("nPhone1CNTRL\n# my phone\n").unwrap_err();
        assert_eq!(refused, "line 2: \"#\" is not a device's StableID: it has a byte that is no letter or digit");
        let long = "n".repeat(ID_BYTES_MAX + 1);
        let refused = allowed_of(&long).unwrap_err();
        assert!(refused.ends_with("it is 65 bytes, not 1 to 64"), "{refused}");
        assert!(allowed_of(&"n".repeat(ID_BYTES_MAX)).is_ok());
        assert!(allowed_of("n-1\n").is_err());
    }

    #[test]
    fn a_file_naming_more_than_16_devices_lets_none_in() {
        let ids = |count: usize| (0..count).map(|it| format!("n{it}CNTRL")).collect::<Vec<_>>().join("\n");
        assert!(allowed_of(&ids(DEVICES_MAX)).is_ok());
        assert_eq!(allowed_of(&ids(DEVICES_MAX + 1)).unwrap_err(), "it names more than 16 devices");
        let again = format!("{}\n{}", ids(DEVICES_MAX), ids(1));
        assert!(allowed_of(&again).is_ok(), "a device named twice is one device");
    }

    #[test]
    fn a_missing_file_lets_no_device_in_and_a_long_one_is_not_read() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join(FILE);
        assert_eq!(allowed(&path).unwrap(), Allowed::default());
        std::fs::write(&path, "nPhone1CNTRL phone\n").unwrap();
        std::fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();
        assert!(allowed(&path).unwrap().lets_in(&id("nPhone1CNTRL")));
        let long = format!("nPhone1CNTRL {}\n", "x".repeat(READ_MAX as usize));
        std::fs::write(&path, long).unwrap();
        let refused = allowed(&path).unwrap_err();
        assert_eq!(refused, format!("{}: it is longer than 4096 bytes", path.display()));
        std::fs::write(&path, b"nPhone1CNTRL \xff\n").unwrap();
        assert!(allowed(&path).unwrap_err().ends_with("it is not UTF-8"));
    }

    #[test]
    fn a_file_others_may_write_lets_no_device_in() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join(FILE);
        std::fs::write(&path, "nPhone1CNTRL phone\n").unwrap();
        for mode in [0o600, 0o644, 0o400] {
            std::fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
            assert!(allowed(&path).unwrap().lets_in(&id("nPhone1CNTRL")), "{mode:o}");
        }
        for mode in [0o620, 0o602, 0o666] {
            std::fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
            let refused = allowed(&path).unwrap_err();
            let why = format!("{}: others may write it (mode {mode:o}): chmod go-w it", path.display());
            assert_eq!(refused, why);
        }
    }
}

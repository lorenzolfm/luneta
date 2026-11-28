use std::num::NonZeroU32;

pub const LISTED_MAX: usize = 50;

pub const LABELS_MAX: usize = 16;

pub const TITLE_BYTES_MAX: usize = 1024;

pub const NAME_BYTES_MAX: usize = 256;

const _: () = assert!(LISTED_MAX > 0);
const _: () = assert!(LABELS_MAX > 0);
const _: () = assert!(TITLE_BYTES_MAX > 0);
const _: () = assert!(NAME_BYTES_MAX > 0);

pub fn check_full(kind: &str, listed: usize, more: bool) -> Result<(), String> {
    if listed > LISTED_MAX {
        return Err(format!("at most {LISTED_MAX} {kind}"));
    }
    if more && listed < LISTED_MAX {
        return Err(format!(
            "{listed} {kind} are listed of more, where {LISTED_MAX} would be"
        ));
    }
    Ok(())
}

pub fn check_title(number: NonZeroU32, title: &str) -> Result<(), String> {
    if title.len() <= TITLE_BYTES_MAX {
        return Ok(());
    }
    Err(format!(
        "#{number} has a title longer than {TITLE_BYTES_MAX} bytes"
    ))
}

pub fn check_name(number: NonZeroU32, what: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("#{number}: {what} is empty"));
    }
    if name.len() > NAME_BYTES_MAX {
        return Err(format!(
            "#{number}: {what} is longer than {NAME_BYTES_MAX} bytes"
        ));
    }
    Ok(())
}

use std::io::ErrorKind;
use std::process::Command;
use std::time::Duration;

use lunetad::images::{self, Png, Refused};
use lunetad::log;
use lunetad::program::{self, Ran};

const READ: Duration = Duration::from_secs(10);

const PRINTED_MAX: u64 = 2 * images::BYTES_MAX + 32;

struct Tool {
    program: &'static str,
    args: &'static [&'static str],
    says: Says,
}

#[derive(Clone, Copy)]
enum Says {
    Bytes,
    Printed,
}

const OSASCRIPT: Tool = Tool {
    program: "osascript",
    args: &["-e", "the clipboard as «class PNGf»"],
    says: Says::Printed,
};

const WL_PASTE: Tool =
    Tool { program: "wl-paste", args: &["--type", "image/png"], says: Says::Bytes };

const XCLIP: Tool = Tool {
    program: "xclip",
    args: &["-selection", "clipboard", "-t", "image/png", "-o"],
    says: Says::Bytes,
};

enum Read {
    Absent,
    Nothing,
    Image(Png),
    Refused(String),
}

impl Tool {
    fn read(&self) -> Read {
        let Tool { program, args, says } = self;
        let most = match says {
            Says::Bytes => images::BYTES_MAX,
            Says::Printed => PRINTED_MAX,
        };
        let mut command = Command::new(program);
        command.args(*args);
        let said = match program::run_up_to(&mut command, READ, most) {
            Ok(Ran::Ended { status, stdout, .. }) if status.success() => stdout,
            Ok(Ran::Ended { .. }) => return Read::Nothing,
            Ok(Ran::Stuck) => {
                log::say(&format!("clipboard: {program} did not answer in {}s", READ.as_secs()));
                return Read::Nothing;
            },
            Err(error) if error.kind() == ErrorKind::NotFound => return Read::Absent,
            Err(error) if error.kind() == ErrorKind::FileTooLarge => {
                let most = images::BYTES_MAX / (1024 * 1024);
                return Read::Refused(format!("the image on the clipboard is over {most} MiB"));
            },
            Err(error) => {
                log::say(&format!("clipboard: {program}: {error}"));
                return Read::Nothing;
            },
        };
        let bytes = match says {
            Says::Bytes => Some(said),
            Says::Printed => printed(&said),
        };
        match bytes.map(Png::new) {
            Some(Ok(png)) => Read::Image(png),
            Some(Err(Refused::TooLarge(why))) => Read::Refused(why),
            Some(Err(Refused::NotPng)) | None => Read::Nothing,
        }
    }
}

pub fn image() -> Result<Option<Png>, String> {
    if cfg!(target_os = "macos") { image_by(&[OSASCRIPT]) } else { image_by(&[WL_PASTE, XCLIP]) }
}

fn image_by(tools: &[Tool]) -> Result<Option<Png>, String> {
    let mut absent = Vec::new();
    for tool in tools {
        match tool.read() {
            Read::Absent => absent.push(tool.program),
            Read::Nothing => {},
            Read::Image(png) => return Ok(Some(png)),
            Read::Refused(why) => return Err(why),
        }
    }
    if absent.len() == tools.len() {
        log::say(&format!("clipboard: none of {} is on the PATH", absent.join(", ")));
    }
    Ok(None)
}

fn printed(said: &[u8]) -> Option<Vec<u8>> {
    const CLASS: &[u8] = b"PNGf";
    let class = said.windows(CLASS.len()).position(|word| word == CLASS)?;
    let digits = &said[class + CLASS.len()..];
    let end = digits.iter().position(|digit| !digit.is_ascii_hexdigit()).unwrap_or(digits.len());
    let digits = &digits[..end];
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return None;
    }
    let digit = |digit: u8| char::from(digit).to_digit(16).and_then(|it| u8::try_from(it).ok());
    let (pairs, _) = digits.as_chunks::<2>();
    pairs.iter().map(|[high, low]| Some(digit(*high)? << 4 | digit(*low)?)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(script: &'static [&'static str], says: Says) -> Tool {
        Tool { program: "sh", args: script, says }
    }

    fn image_of(tools: &[Tool]) -> Option<Vec<u8>> {
        let png = image_by(tools).unwrap()?;
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let parts: Vec<_> =
            runtime.block_on(tokio_stream::StreamExt::collect(png.asked(None)));
        let part = |part: &lunetad::api::KeepImageRequest| match &part.part {
            Some(lunetad::api::keep_image_request::Part::Bytes(bytes)) => bytes.clone(),
            _ => Vec::new(),
        };
        Some(parts.iter().flat_map(part).collect())
    }

    #[test]
    fn data_as_applescript_prints_it_is_read_as_bytes() {
        let bytes = printed("«data PNGf89504E47»\n".as_bytes());
        assert_eq!(bytes, Some(vec![0x89, 0x50, 0x4e, 0x47]));
        assert_eq!(printed(b"\xc7data PNGf89504e47\xc8\n"), Some(vec![0x89, 0x50, 0x4e, 0x47]));
        assert_eq!(printed("«data TIFF89504E47»".as_bytes()), None);
        assert_eq!(printed("«data PNGf»".as_bytes()), None);
        assert_eq!(printed("«data PNGf895»".as_bytes()), None);
        assert_eq!(printed(b""), None);
    }

    #[test]
    fn the_first_tool_with_a_png_hands_it_over() {
        const FAILS: &[&str] = &["-c", "echo 'no image' >&2; exit 1"];
        const NOT_PNG: &[&str] = &["-c", "printf 'GIF89a'"];
        const GOOD: &[&str] = &["-c", r"printf '\211PNG\r\n\032\n!'"];
        const SECOND: &[&str] = &["-c", r"printf '\211PNG\r\n\032\n?'"];
        let absent = Tool { program: "luneta-no-such-tool", args: &[], says: Says::Bytes };
        let whole = |last: u8| {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.push(last);
            Some(bytes)
        };
        let (fails, not_png) = (tool(FAILS, Says::Bytes), tool(NOT_PNG, Says::Bytes));
        let (good, second) = (tool(GOOD, Says::Bytes), tool(SECOND, Says::Bytes));
        assert_eq!(image_of(&[absent, fails, not_png, good, second]), whole(b'!'));
        let absent = Tool { program: "luneta-no-such-tool", args: &[], says: Says::Bytes };
        let (fails, not_png) = (tool(FAILS, Says::Bytes), tool(NOT_PNG, Says::Bytes));
        assert_eq!(image_of(&[absent, fails, not_png]), None);
    }

    #[test]
    fn a_tool_loud_on_its_stderr_says_no_image_is_too_large() {
        const LOUD: &[&str] = &["-c", "head -c 16777217 /dev/zero >&2"];
        assert!(matches!(image_by(&[tool(LOUD, Says::Bytes)]), Ok(None)));
    }

    #[test]
    fn a_tool_that_prints_hands_over_the_bytes_printed() {
        const PRINTS: &[&str] = &["-c", "echo '«data PNGf89504E470D0A1A0A21»'"];
        let bytes = image_of(&[tool(PRINTS, Says::Printed)]);
        assert_eq!(bytes, Some(b"\x89PNG\r\n\x1a\n!".to_vec()));
    }

    #[test]
    fn an_image_too_large_is_no_image_to_send_and_says_so() {
        const LARGE: &[&str] = &["-c", "head -c 67108865 /dev/zero"];
        let refused = image_by(&[tool(LARGE, Says::Bytes)]).err();
        assert_eq!(refused.as_deref(), Some("the image on the clipboard is over 64 MiB"));
    }
}

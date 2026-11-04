//! The image on the clipboard of the machine luneta runs on, as its own tools
//! hand it over, and as Claude Code reads it on the machine it runs on:
//! `osascript` on macOS, and `wl-paste` or `xclip` on Linux. A tool that has
//! none to hand over ends with a failure, as one does with no display to ask.

use std::io::ErrorKind;
use std::process::Command;
use std::time::Duration;

use lunetad::images::{self, Png, Refused};
use lunetad::log;
use lunetad::program::{self, Ran};

/// How long a tool has to hand an image over. It answers in milliseconds, and
/// `osascript` takes a second or two for the largest. One that takes longer
/// waits on a program that holds the clipboard, and answers nothing.
const READ: Duration = Duration::from_secs(10);

/// The most `osascript` prints of the largest image: two digits a byte, and
/// what it prints around them.
const PRINTED_MAX: u64 = 2 * images::BYTES_MAX + 32;

/// A tool that hands over what the clipboard holds as a PNG.
struct Tool {
    program: &'static str,
    args: &'static [&'static str],
    says: Says,
}

/// How a tool says the image's bytes.
#[derive(Clone, Copy)]
enum Says {
    /// As they are.
    Bytes,
    /// As `osascript` prints data of the class `PNGf`.
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

/// What a tool said of the clipboard.
enum Read {
    /// It is not on the `PATH`.
    Absent,
    /// It has no image to hand over, or could not say.
    Nothing,
    Image(Png),
    /// Why the image it has is not one to send.
    Refused(String),
}

impl Tool {
    /// What this tool says of the clipboard. Why it could not say goes in the
    /// log, as the key then goes on to the pane, which says what it says with
    /// no image.
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

/// The image on the clipboard: none where it holds none, as where no tool
/// could say; or why it is not one to send, as it is too large.
pub fn image() -> Result<Option<Png>, String> {
    if cfg!(target_os = "macos") { image_by(&[OSASCRIPT]) } else { image_by(&[WL_PASTE, XCLIP]) }
}

/// The image the first of `tools` that has one hands over, as `image` says.
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

/// The bytes in what `osascript` prints of data of the class `PNGf`, as
/// `«data PNGf89504E47…»`: hex digits, after the class, up to the end of its
/// quotes, whose encoding is the locale's. None where it prints no such data.
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
    digits.chunks_exact(2).map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool that runs `script`.
    fn tool(script: &'static [&'static str], says: Says) -> Tool {
        Tool { program: "sh", args: script, says }
    }

    /// The bytes of the image `tools` hand over.
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

    /// `osascript` prints data in its quotes, in the locale's encoding, as
    /// hex digits after the class; what is not data of that class, or digits
    /// that do not make whole bytes, holds no image.
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

    /// The first tool that hands over a PNG is the one: one that fails, as
    /// with no such image, or no display, or that is not there, or that says
    /// what is no PNG, leaves it to the next, and none is no image.
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

    /// A tool that says too much on its stderr says nothing of an image: the
    /// clipboard holds none, rather than one too large.
    #[test]
    fn a_tool_loud_on_its_stderr_says_no_image_is_too_large() {
        const LOUD: &[&str] = &["-c", "head -c 16777217 /dev/zero >&2"];
        assert!(matches!(image_by(&[tool(LOUD, Says::Bytes)]), Ok(None)));
    }

    /// A tool that prints as `osascript` does hands over the bytes printed.
    #[test]
    fn a_tool_that_prints_hands_over_the_bytes_printed() {
        const PRINTS: &[&str] = &["-c", "echo '«data PNGf89504E470D0A1A0A21»'"];
        let bytes = image_of(&[tool(PRINTS, Says::Printed)]);
        assert_eq!(bytes, Some(b"\x89PNG\r\n\x1a\n!".to_vec()));
    }

    /// An image too large to send is told apart from no image, as the key
    /// would go on to the pane, which would say the clipboard holds none.
    #[test]
    fn an_image_too_large_is_no_image_to_send_and_says_so() {
        const LARGE: &[&str] = &["-c", "head -c 67108865 /dev/zero"];
        let refused = image_by(&[tool(LARGE, Says::Bytes)]).err();
        assert_eq!(refused.as_deref(), Some("the image on the clipboard is over 64 MiB"));
    }
}

//! An image for an agent of this machine to read, as luneta has the daemon
//! keep it for `Ctrl v` on an agent's pane on another machine: Claude Code
//! reads the clipboard of the machine it runs on, which is not the one you
//! sit at, and takes a path pasted on its prompt for an image at once. The
//! bytes come from luneta through the bridge, as a stream of parts, and go in
//! a file of their own, kept as `keeps` keeps one.

use std::fs::{self, File};
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

use tokio_stream::{Stream, StreamExt as _};

use crate::api::keep_image_request::{Image, Part};
use crate::api::{KeepImageRequest, On};
use crate::keeps;

/// The largest image that goes: a screenshot of a 5K display, at its worst,
/// is under this. A file holds it in `XDG_RUNTIME_DIR`, which is memory on
/// most Linuxes.
pub const BYTES_MAX: u64 = 64 * 1024 * 1024;

/// How long an image has to come, at both ends. The largest one takes this
/// long at about 900 kbit/s.
pub const PATIENCE: Duration = Duration::from_mins(10);

/// How many of an image's bytes go in a part: well under the 4 MiB tonic takes
/// in a message.
const PART_BYTES: usize = 64 * 1024;

/// The folder the images are kept in, in luneta's.
const FOLDER: &str = "images";

/// How a PNG starts, whatever it holds.
const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Why bytes are no image to send.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// They are no PNG.
    NotPng,
    /// They are, and too many to send, as `String` says.
    TooLarge(String),
}

/// The bytes of a PNG, up to `BYTES_MAX` of them.
pub struct Png(Vec<u8>);

impl Png {
    /// `bytes` as a PNG to send, or why they are not one.
    pub fn new(bytes: Vec<u8>) -> Result<Png, Refused> {
        if !bytes.starts_with(SIGNATURE) {
            return Err(Refused::NotPng);
        }
        fits(bytes.len() as u64).map_err(Refused::TooLarge)?;
        Ok(Png(bytes))
    }

    /// The question that keeps it on the machine `on` names: what it is,
    /// and then its bytes, a part at a time.
    pub fn asked(self, on: Option<On>) -> impl Stream<Item = KeepImageRequest> + Send + 'static {
        let Png(bytes) = self;
        let image = Image { on, size: bytes.len() as u64 };
        let image = KeepImageRequest { part: Some(Part::Image(image)) };
        let parts = (0..bytes.len().div_ceil(PART_BYTES)).map(move |part| {
            let end = bytes.len().min((part + 1) * PART_BYTES);
            let part = bytes[part * PART_BYTES..end].to_vec();
            KeepImageRequest { part: Some(Part::Bytes(part)) }
        });
        tokio_stream::once(image).chain(tokio_stream::iter(parts))
    }
}

/// Whether an image of `size` bytes goes, or why not.
pub fn fits(size: u64) -> Result<(), String> {
    if size == 0 {
        return Err("the image has no bytes".to_owned());
    }
    if size > BYTES_MAX {
        let (size, most) = (size.div_ceil(1024 * 1024), BYTES_MAX / (1024 * 1024));
        return Err(format!("the image is {size} MiB, and the most that goes is {most} MiB"));
    }
    Ok(())
}

/// An image on its way into its file, which goes unless all of it came.
pub(crate) struct Keeping {
    path: String,
    file: File,
    /// How many of its bytes are yet to come.
    left: u64,
    whole: bool,
}

impl Keeping {
    /// The file for an image of `size` bytes, in luneta's folder `runtime`.
    pub(crate) fn new(runtime: &Path, size: u64, now: SystemTime) -> Result<Keeping, String> {
        let (path, file) = keeps::new_file(runtime, FOLDER, "png", now)?;
        Ok(Keeping { path, file, left: size, whole: false })
    }

    /// Writes the next of its bytes, or says why not: as they are more than
    /// it said it has.
    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        let Some(left) = self.left.checked_sub(bytes.len() as u64) else {
            return Err("the image is larger than it said".to_owned());
        };
        self.left = left;
        self.file.write_all(bytes).map_err(|error| format!("{}: {error}", self.path))
    }

    /// The file's path, with all of the image in it, or why it is not.
    pub(crate) fn kept(mut self) -> Result<String, String> {
        if self.left > 0 {
            return Err(format!("the image ended {} bytes short", self.left));
        }
        self.whole = true;
        Ok(self.path.clone())
    }
}

impl Drop for Keeping {
    fn drop(&mut self) {
        if !self.whole {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A PNG of `size` bytes, each saying where it is in the image.
    fn png(size: usize) -> Vec<u8> {
        let mut bytes = SIGNATURE.to_vec();
        bytes.extend((SIGNATURE.len()..size).map(|at| (at % 251) as u8));
        bytes
    }

    /// Only a PNG goes, and one no larger than the most: what a clipboard
    /// tool says that is not one is no image.
    #[test]
    fn a_png_up_to_the_most_is_an_image_to_send() {
        let refused = |bytes| Png::new(bytes).err();
        assert_eq!(refused(png(64)), None);
        assert_eq!(refused(b"GIF89a".to_vec()), Some(Refused::NotPng));
        assert_eq!(refused(Vec::new()), Some(Refused::NotPng));
        let sized = |size| {
            let mut bytes = vec![0; size];
            bytes[..SIGNATURE.len()].copy_from_slice(SIGNATURE);
            bytes
        };
        let most = BYTES_MAX as usize;
        assert_eq!(refused(sized(most)), None);
        let too_large = "the image is 65 MiB, and the most that goes is 64 MiB".to_owned();
        assert_eq!(refused(sized(most + 1)), Some(Refused::TooLarge(too_large)));
    }

    /// What it is goes first, once, and the bytes after it, in parts that a
    /// message has room for, each full but the last: none on its own is
    /// empty, however the size falls.
    #[test]
    fn an_image_goes_as_what_it_is_and_then_its_bytes_in_parts() {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        for size in [SIGNATURE.len(), PART_BYTES, PART_BYTES + 1, 3 * PART_BYTES] {
            let on = On { destination: "far".to_owned() };
            let bytes = png(size);
            let asked = Png::new(bytes.clone()).unwrap().asked(Some(on.clone()));
            let asked: Vec<_> = runtime.block_on(asked.collect());
            let Some((first, parts)) = asked.split_first() else {
                panic!("nothing went");
            };
            let image = Image { on: Some(on), size: size as u64 };
            assert_eq!(first.part, Some(Part::Image(image)));
            let parts: Vec<&Vec<u8>> = parts
                .iter()
                .map(|part| match &part.part {
                    Some(Part::Bytes(bytes)) => bytes,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(parts.len(), size.div_ceil(PART_BYTES));
            assert!(parts.iter().all(|part| !part.is_empty() && part.len() <= PART_BYTES));
            let sent: Vec<u8> = parts.into_iter().flatten().copied().collect();
            assert!(sent == bytes, "{size}");
        }
    }

    /// All of an image goes in a file of its own, for this user alone, in the
    /// folder `images` of luneta's, by a name Claude Code takes for an
    /// image's, and it stays; an image that ended short, or that is more than
    /// it said, leaves nothing.
    #[test]
    fn an_image_is_kept_whole_or_not_at_all() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("luneta");
        let now = SystemTime::now();
        let folder = || fs::read_dir(runtime.join(FOLDER)).unwrap().count();

        let mut keeping = Keeping::new(&runtime, 5, now).unwrap();
        keeping.write(b"PN").unwrap();
        keeping.write(b"G!!").unwrap();
        let kept = keeping.kept().unwrap();
        assert_eq!(Path::new(&kept).extension(), Some("png".as_ref()), "{kept}");
        assert_eq!(fs::read(&kept).unwrap(), b"PNG!!");
        let mode = fs::metadata(&kept).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(folder(), 1);

        let mut short = Keeping::new(&runtime, 5, now).unwrap();
        short.write(b"PN").unwrap();
        assert_eq!(folder(), 2);
        assert_eq!(short.kept().err().unwrap(), "the image ended 3 bytes short");
        assert_eq!(folder(), 1);

        let mut long = Keeping::new(&runtime, 2, now).unwrap();
        assert_eq!(long.write(b"PNG").unwrap_err(), "the image is larger than it said");
        drop(long);
        assert_eq!(folder(), 1);

        let hung_up = Keeping::new(&runtime, 2, now).unwrap();
        drop(hung_up);
        assert_eq!(folder(), 1);
        assert!(Path::new(&kept).exists());
    }

    /// An image kept more than a day ago goes as another is kept.
    #[test]
    fn an_image_goes_a_day_on_as_another_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("luneta");
        let now = SystemTime::now();
        let keep = |at| {
            let mut keeping = Keeping::new(&runtime, 1, at).unwrap();
            keeping.write(b"!").unwrap();
            keeping.kept().unwrap()
        };
        let old = keep(now);
        File::options()
            .write(true)
            .open(&old)
            .and_then(|file| file.set_modified(now - Duration::from_hours(25)))
            .unwrap();
        let young = keep(now);
        assert!(!Path::new(&old).exists());
        assert!(Path::new(&young).exists());
    }
}

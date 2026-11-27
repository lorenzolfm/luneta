use std::path::Path;
use std::time::{Duration, SystemTime};

use tokio_stream::{Stream, StreamExt as _};

use crate::api::keep_image_request::{Image, Part};
use crate::api::{KeepImageRequest, On};
use crate::keeps::Draft;

pub const BYTES_MAX: u64 = 64 * 1024 * 1024;

const FOLDER_BYTES_MAX: u64 = 2 * BYTES_MAX;

pub const PATIENCE: Duration = Duration::from_mins(10);

const PART_BYTES: usize = 64 * 1024;

pub(crate) const FOLDER: &str = "images";

const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    NotPng,
    TooLarge(String),
}

pub struct Png(Vec<u8>);

impl Png {
    pub fn new(bytes: Vec<u8>) -> Result<Png, Refused> {
        if !bytes.starts_with(SIGNATURE) {
            return Err(Refused::NotPng);
        }
        Size::new(bytes.len() as u64).map_err(Refused::TooLarge)?;
        Ok(Png(bytes))
    }

    pub fn asked(self, on: Option<On>) -> impl Stream<Item = KeepImageRequest> + Send + 'static {
        let Png(bytes) = self;
        let image = Image {
            on,
            size: bytes.len() as u64,
        };
        let image = KeepImageRequest {
            part: Some(Part::Image(image)),
        };
        let parts = (0..bytes.len().div_ceil(PART_BYTES)).map(move |part| {
            let end = bytes.len().min((part + 1) * PART_BYTES);
            let part = bytes[part * PART_BYTES..end].to_vec();
            KeepImageRequest {
                part: Some(Part::Bytes(part)),
            }
        });
        tokio_stream::once(image).chain(tokio_stream::iter(parts))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Size(u64);

impl Size {
    pub(crate) fn new(size: u64) -> Result<Size, String> {
        if size < SIGNATURE.len() as u64 {
            return Err(format!("too few bytes to be a PNG: {size}"));
        }
        if size > BYTES_MAX {
            let (size, most) = (size.div_ceil(1024 * 1024), BYTES_MAX / (1024 * 1024));
            return Err(format!(
                "the image is {size} MiB, and the most that goes is {most} MiB"
            ));
        }
        Ok(Size(size))
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

pub(crate) struct Keeping {
    draft: Draft,
    left: u64,
    unseen: &'static [u8],
}

impl Keeping {
    pub(crate) fn new(runtime: &Path, size: Size, now: SystemTime) -> Result<Keeping, String> {
        let draft = Draft::new(runtime, FOLDER, "png", now)?;
        draft.make_room(size.get(), FOLDER_BYTES_MAX);
        Ok(Keeping {
            draft,
            left: size.get(),
            unseen: SIGNATURE,
        })
    }

    pub(crate) fn write(mut self, bytes: &[u8]) -> Result<Keeping, String> {
        let Some(left) = self.left.checked_sub(bytes.len() as u64) else {
            return Err("the image is larger than it said".to_owned());
        };
        let (start, rest) = self.unseen.split_at(self.unseen.len().min(bytes.len()));
        if !bytes.starts_with(start) {
            return Err("the image is no PNG".to_owned());
        }
        self.left = left;
        self.unseen = rest;
        self.draft.write(bytes)?;
        Ok(self)
    }

    pub(crate) fn kept(self) -> Result<String, String> {
        if self.left > 0 {
            return Err(format!("the image ended {} bytes short", self.left));
        }
        self.draft.keep()
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn png(size: usize) -> Vec<u8> {
        let mut bytes = SIGNATURE.to_vec();
        bytes.extend((SIGNATURE.len()..size).map(|at| (at % 251) as u8));
        bytes
    }

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

    #[test]
    fn an_image_goes_as_what_it_is_and_then_its_bytes_in_parts() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for size in [SIGNATURE.len(), PART_BYTES, PART_BYTES + 1, 3 * PART_BYTES] {
            let on = On {
                destination: "far".to_owned(),
            };
            let bytes = png(size);
            let asked = Png::new(bytes.clone()).unwrap().asked(Some(on.clone()));
            let asked: Vec<_> = runtime.block_on(asked.collect());
            let Some((first, parts)) = asked.split_first() else {
                panic!("nothing went");
            };
            let image = Image {
                on: Some(on),
                size: size as u64,
            };
            assert_eq!(first.part, Some(Part::Image(image)));
            let parts: Vec<&Vec<u8>> = parts
                .iter()
                .map(|part| match &part.part {
                    Some(Part::Bytes(bytes)) => bytes,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(parts.len(), size.div_ceil(PART_BYTES));
            assert!(
                parts
                    .iter()
                    .all(|part| !part.is_empty() && part.len() <= PART_BYTES)
            );
            let sent: Vec<u8> = parts.into_iter().flatten().copied().collect();
            assert!(sent == bytes, "{size}");
        }
    }

    #[test]
    fn an_image_is_kept_whole_or_not_at_all() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("lune");
        let now = SystemTime::now();
        let folder = || fs::read_dir(runtime.join(FOLDER)).unwrap().count();

        let size = |size| Size::new(size).unwrap();
        let image = png(16);
        let keeping = Keeping::new(&runtime, size(16), now).unwrap();
        let keeping = keeping.write(&image[..3]).unwrap();
        let keeping = keeping.write(&image[3..]).unwrap();
        let kept = keeping.kept().unwrap();
        assert_eq!(Path::new(&kept).extension(), Some("png".as_ref()), "{kept}");
        assert_eq!(fs::read(&kept).unwrap(), image);
        let mode = fs::metadata(&kept).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(folder(), 1);

        let short = Keeping::new(&runtime, size(16), now).unwrap();
        let short = short.write(&image[..8]).unwrap();
        assert_eq!(folder(), 2);
        let pngs = || {
            let found = fs::read_dir(runtime.join(FOLDER)).unwrap().flatten();
            found
                .filter(|file| file.path().extension() == Some("png".as_ref()))
                .count()
        };
        assert_eq!(pngs(), 1, "one on its way goes by no name of an image's");
        assert_eq!(short.kept().err().unwrap(), "the image ended 8 bytes short");
        assert_eq!(folder(), 1);

        let long = Keeping::new(&runtime, size(8), now).unwrap();
        assert_eq!(
            long.write(&image).err().unwrap(),
            "the image is larger than it said"
        );
        assert_eq!(folder(), 1);

        let not_png = Keeping::new(&runtime, size(16), now).unwrap();
        let not_png = not_png.write(&image[..3]).unwrap();
        assert_eq!(
            not_png.write(b"GIF89a").err().unwrap(),
            "the image is no PNG"
        );
        assert_eq!(folder(), 1);

        let hung_up = Keeping::new(&runtime, size(8), now).unwrap();
        drop(hung_up);
        assert_eq!(folder(), 1);
        assert!(Path::new(&kept).exists());
    }

    #[test]
    fn an_image_goes_a_day_on_as_another_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("lune");
        let now = SystemTime::now();
        let keep = |at| {
            let size = Size::new(8).unwrap();
            let keeping = Keeping::new(&runtime, size, at).unwrap();
            keeping.write(SIGNATURE).unwrap().kept().unwrap()
        };
        let old = keep(now);
        std::mem::forget(Keeping::new(&runtime, Size::new(8).unwrap(), now).unwrap());
        let folder = runtime.join(FOLDER);
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 2);
        for file in fs::read_dir(&folder).unwrap().flatten() {
            File::options()
                .write(true)
                .open(file.path())
                .and_then(|file| file.set_modified(now - Duration::from_hours(25)))
                .unwrap();
        }
        let young = keep(now);
        assert!(!Path::new(&old).exists());
        assert!(Path::new(&young).exists());
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);
    }

    #[test]
    fn the_oldest_images_go_to_make_room_for_another() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("lune");
        let now = SystemTime::now();
        let keeping = |size| drop(Keeping::new(&runtime, Size::new(size).unwrap(), now).unwrap());
        keeping(8);
        let folder = runtime.join(FOLDER);
        let file = |name: &str, megabytes: u64, hours: u64| {
            let made = File::create(folder.join(name)).unwrap();
            made.set_len(megabytes * 1024 * 1024).unwrap();
            made.set_modified(now - Duration::from_hours(hours))
                .unwrap();
        };
        file("killed.png.part", 10, 4);
        file("oldest.png", 30, 3);
        file("older.png", 30, 2);
        file("last.png", 64, 1);
        let left = || {
            let found = fs::read_dir(&folder).unwrap().flatten();
            let mut left: Vec<_> = found
                .map(|file| file.file_name().into_string().unwrap())
                .collect();
            left.sort();
            left
        };

        keeping(8);
        assert_eq!(
            left(),
            ["last.png", "older.png", "oldest.png"],
            "134 MiB, less the oldest"
        );
        keeping(BYTES_MAX);
        assert_eq!(
            left(),
            ["last.png"],
            "188 MiB, less the two oldest, is all there is room for"
        );
        keeping(BYTES_MAX);
        assert_eq!(left(), ["last.png"], "room for two of the largest");
    }
}

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;

const EXEC_PATH_ALIGNMENT: usize = size_of::<u64>();

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    pub argv: Vec<OsString>,
    pub environ: Vec<OsString>,
}

#[must_use]
pub fn arguments(cmdline: &[u8]) -> Vec<&[u8]> {
    if cmdline.is_empty() {
        return Vec::new();
    }
    let ended = cmdline.strip_suffix(&[0]).unwrap_or(cmdline);
    ended.split(|byte| *byte == 0).collect()
}

pub fn laid_in_procargs(procargs: &[u8]) -> Result<Laid, &'static str> {
    let fewer = "KERN_PROCARGS2 holds fewer arguments than its argc";
    let (count, strings) = procargs
        .split_first_chunk::<{ size_of::<i32>() }>()
        .ok_or("KERN_PROCARGS2 holds no argc")?;
    let argument_count = usize::try_from(i32::from_ne_bytes(*count))
        .map_err(|_| "KERN_PROCARGS2 holds a negative argc")?;
    let path_end = strings
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("KERN_PROCARGS2's exec path does not end")?;
    let arguments_start = (path_end + 1).next_multiple_of(EXEC_PATH_ALIGNMENT);
    let padding = strings
        .get(path_end..arguments_start)
        .ok_or("KERN_PROCARGS2 ends in its exec path's padding")?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err("KERN_PROCARGS2 pads its exec path with more than zeros");
    }
    let mut rest = &strings[arguments_start..];
    if rest.len() < argument_count {
        return Err(fewer);
    }
    let mut argv = Vec::with_capacity(argument_count);
    for _ in 0..argument_count {
        let argument_end = rest.iter().position(|byte| *byte == 0).ok_or(fewer)?;
        argv.push(OsString::from_vec(rest[..argument_end].to_vec()));
        rest = &rest[argument_end + 1..];
    }
    let environ_start = rest.iter().position(|byte| *byte != 0).unwrap_or(rest.len());
    let mut environ = Vec::new();
    for entry in rest[environ_start..].split_inclusive(|byte| *byte == 0) {
        let entry = entry.strip_suffix(&[0]).ok_or("KERN_PROCARGS2's environment does not end")?;
        if entry.is_empty() {
            break;
        }
        environ.push(OsString::from_vec(entry.to_vec()));
    }
    Ok(Laid { argv, environ })
}

#[cfg(test)]
mod tests {
    use super::{arguments, laid_in_procargs};

    #[test]
    fn a_cmdline_reads_as_its_arguments_empty_ones_too_and_none_before_it_is_laid_out() {
        let read = |cmdline: &[u8]| {
            let read = arguments(cmdline).into_iter();
            read.map(|arg| String::from_utf8(arg.to_vec()).unwrap()).collect::<Vec<_>>()
        };
        assert_eq!(read(b"sh\0-c\0\0\0"), ["sh", "-c", "", ""]);
        assert_eq!(read(b"\0"), [""]);
        assert_eq!(read(b"claude --resume"), ["claude --resume"]);
        assert_eq!(read(b"sh\0-c\0x"), ["sh", "-c", "x"], "a cmdline cut short ends its last");
        assert_eq!(read(b""), Vec::<String>::new());
    }

    fn procargs(argument_count: i32, path: &str, strings: &[&str]) -> Vec<u8> {
        let mut procargs = argument_count.to_ne_bytes().to_vec();
        let mut padded = path.as_bytes().to_vec();
        padded.resize((path.len() + 1).next_multiple_of(8), 0);
        procargs.extend_from_slice(&padded);
        for string in strings {
            procargs.extend_from_slice(string.as_bytes());
            procargs.push(0);
        }
        procargs
    }

    #[test]
    fn an_environment_starts_after_as_many_arguments_as_argc() {
        let read = |argument_count, path, strings: &[&str]| {
            let read = laid_in_procargs(&procargs(argument_count, path, strings));
            read.map(|laid| {
                laid.environ.into_iter().map(|it| it.into_string().unwrap()).collect::<Vec<_>>()
            })
        };
        let given = |entries: &[&str]| Ok(entries.iter().map(|it| (*it).to_owned()).collect());
        let laid = ["sleep", "60", "A=b", "PATH=/bin", "", "", "ptr_munge="];
        assert_eq!(read(2, "/bin/sleep", &laid), given(&["A=b", "PATH=/bin"]));
        let empty_argument = ["echo", "", "x", "A=b", "", "main_stack="];
        assert_eq!(read(3, "/bin/echo", &empty_argument), given(&["A=b"]), "an empty argument");
        let empty_first = ["", "60", "A=b", "B=c", ""];
        assert_eq!(read(2, "/bin/sleep", &empty_first), given(&["A=b", "B=c"]), "an empty first");
        let unpadded = ["", "A=b", ""];
        assert_eq!(read(1, "/bin/sh", &unpadded), given(&["A=b"]), "a path its alignment fits");
        let titled = [&["claude"][..], &[""; 17], &["A=b", ""]].concat();
        let why = "a title leaves zeros where its arguments were";
        assert_eq!(read(3, "/usr/bin/node", &titled), given(&["A=b"]), "{why}");
        assert_eq!(read(0, "/bin/true", &["", ""]), given(&[]));

        let fewer = Err("KERN_PROCARGS2 holds fewer arguments than its argc");
        assert_eq!(read(5, "/bin/sleep", &["sleep", "60"]), fewer);
        assert_eq!(read(3, "/bin/sleep", &["sleep", "60"]), fewer, "its end ends no argument");
        assert_eq!(read(-1, "/bin/sleep", &[]), Err("KERN_PROCARGS2 holds a negative argc"));
        let raw = |argument_count: i32, strings: &[u8]| {
            laid_in_procargs(&[&argument_count.to_ne_bytes()[..], strings].concat())
                .map(|laid| laid.environ)
        };
        assert_eq!(laid_in_procargs(&[1, 0]), Err("KERN_PROCARGS2 holds no argc"));
        let unended = Err("KERN_PROCARGS2's exec path does not end");
        assert_eq!(raw(0, b"/bin/true"), unended);
        let unpadded = Err("KERN_PROCARGS2 ends in its exec path's padding");
        assert_eq!(raw(0, b"/bin/x\0"), unpadded);
        let misaligned = Err("KERN_PROCARGS2 pads its exec path with more than zeros");
        assert_eq!(raw(1, b"/bin/x\0y\0A=b\0"), misaligned);
        let cut = [&procargs(1, "/bin/sh", &["sh", "A=b"])[..], b"B="].concat();
        let cut_entry = Err("KERN_PROCARGS2's environment does not end");
        assert_eq!(laid_in_procargs(&cut), cut_entry);
    }

    #[test]
    fn arguments_read_as_argc_strings_after_the_exec_path_empty_ones_too() {
        let read = |argument_count, path, strings: &[&str]| {
            let laid = laid_in_procargs(&procargs(argument_count, path, strings)).unwrap();
            laid.argv.into_iter().map(|it| it.into_string().unwrap()).collect::<Vec<_>>()
        };
        let empty_argument = ["echo", "", "x", "A=b", "", "main_stack="];
        assert_eq!(read(3, "/bin/echo", &empty_argument), ["echo", "", "x"]);
        let empty_last = ["sh", "-c", "", "A=b", ""];
        assert_eq!(read(3, "/bin/sh", &empty_last), ["sh", "-c", ""]);
        let empty_first = ["", "60", "A=b", ""];
        assert_eq!(read(2, "/bin/sleep", &empty_first), ["", "60"]);
        let unpadded = ["", "A=b", ""];
        assert_eq!(read(1, "/bin/sh", &unpadded), [""]);
        assert_eq!(read(0, "/bin/true", &["", ""]), Vec::<String>::new());
    }
}

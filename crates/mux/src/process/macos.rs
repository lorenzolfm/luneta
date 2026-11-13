//! Processes as macOS's `libproc` and `sysctl` say them. macOS says where no
//! process sleeps: `ps -o wchan` prints `-` for each, and `kinfo_proc`'s wait
//! message is empty, even for one blocked in a read of its terminal.

#![expect(unsafe_code, reason = "libproc and sysctl are C, which libc only declares")]

use std::ffi::{CStr, c_int, c_void};
use std::mem::{MaybeUninit, size_of};
use std::ptr;

use super::{CHILDREN_MAX, Process, Terminal};

/// What `e_tdev` holds for a process with no terminal: `NODEV`.
const NO_DEVICE: u32 = u32::MAX;

/// The most bytes of a process's arguments read: macOS's `ARG_MAX`.
const ARGS_BYTES_MAX: usize = 1024 * 1024;

/// The process `pid`, as `proc_pidinfo` says it, or none where it is gone.
pub fn process(pid: u32) -> Option<Process> {
    let size = c_int::try_from(size_of::<libc::proc_bsdinfo>()).expect("proc_bsdinfo is small");
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: `info` has room for the `size` bytes asked, and the call writes
    // no more than it is given.
    let written = unsafe {
        libc::proc_pidinfo(
            c_int::try_from(pid).ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<c_void>(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the call wrote all of `info`, as it said.
    let info = unsafe { info.assume_init() };
    let name = info.pbi_comm.iter().map(|byte| byte.cast_unsigned()).take_while(|byte| *byte != 0);
    let terminal = (info.e_tdev != NO_DEVICE).then(|| Terminal {
        device: u64::from(info.e_tdev),
        front: Some(info.e_tpgid).filter(|front| *front > 0),
    });
    Some(Process {
        pid: info.pbi_pid,
        parent: info.pbi_ppid,
        group: info.pbi_pgid,
        name: String::from_utf8_lossy(&name.collect::<Vec<u8>>()).into_owned(),
        sleeps: String::new(),
        stopped: info.pbi_status == libc::SSTOP,
        terminal,
    })
}

/// The children of `pid`, up to `CHILDREN_MAX`: none where it is gone.
pub fn children(pid: u32) -> Vec<u32> {
    let Ok(pid) = c_int::try_from(pid) else {
        return Vec::new();
    };
    let mut pids: Vec<libc::pid_t> = vec![0; CHILDREN_MAX];
    let bytes = c_int::try_from(pids.len() * size_of::<libc::pid_t>()).expect("a few KiB");
    // SAFETY: `pids` has room for the `bytes` it is given as.
    let count = unsafe { libc::proc_listchildpids(pid, pids.as_mut_ptr().cast::<c_void>(), bytes) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    pids.truncate(count.min(CHILDREN_MAX));
    pids.into_iter().filter_map(|pid| u32::try_from(pid).ok().filter(|pid| *pid > 0)).collect()
}

/// The arguments of `pid`, as `KERN_PROCARGS2` holds them: their count, the
/// path of its program, NULs, then each argument ended by a NUL. None where
/// it is gone, or another user's.
pub fn argv(pid: u32) -> Option<Vec<String>> {
    let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, c_int::try_from(pid).ok()?];
    let mut size = 0;
    // SAFETY: with no buffer, the call writes only the size it needs.
    let sized = unsafe {
        libc::sysctl(name.as_mut_ptr(), 3, ptr::null_mut(), &raw mut size, ptr::null_mut(), 0)
    };
    if sized != 0 || size == 0 {
        return None;
    }
    let mut bytes = vec![0_u8; size.min(ARGS_BYTES_MAX)];
    let mut size = bytes.len();
    // SAFETY: `bytes` has room for the `size` it is given as, which the call
    // writes no more than, and sets to what it wrote.
    let read = unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast::<c_void>(),
            &raw mut size,
            ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    bytes.truncate(size);
    args_of(&bytes)
}

/// The arguments in what `KERN_PROCARGS2` wrote. An empty one is one too,
/// but for an empty first one, which the NULs after the path hide.
fn args_of(bytes: &[u8]) -> Option<Vec<String>> {
    let count = usize::try_from(i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?)).ok()?;
    let rest = bytes.get(4..)?;
    let path = CStr::from_bytes_until_nul(rest).ok()?;
    let mut rest = &rest[path.to_bytes_with_nul().len()..];
    while let Some(after) = rest.strip_prefix(b"\0") {
        rest = after;
    }
    let mut args = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let arg = CStr::from_bytes_until_nul(rest).ok()?;
        args.push(arg.to_string_lossy().into_owned());
        rest = &rest[arg.to_bytes_with_nul().len()..];
    }
    (!args.is_empty()).then_some(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The count says how many arguments there are, past the path and its
    /// NULs, an empty one among them, and what follows them is none of them.
    #[test]
    fn the_arguments_read_by_their_count() {
        let mut bytes = 3_i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/bin/sh\0\0\0\0sh\0\0-c x\0HOME=/h\0");
        let args = args_of(&bytes).expect("they read");
        assert_eq!(args, ["sh", "", "-c x"]);
        let mut short = 3_i32.to_ne_bytes().to_vec();
        short.extend_from_slice(b"/bin/sh\0sh\0");
        assert_eq!(args_of(&short), None);
        assert_eq!(args_of(b"\x01\0"), None);
    }
}

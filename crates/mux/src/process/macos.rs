use std::ffi::{c_int, c_void};
use std::mem::{MaybeUninit, size_of};
use std::ptr;

use lune_protocol::process::{EXEC_STRINGS_BYTES_MAX, laid_in_procargs};

use super::{CHILDREN_MAX, Group, Process, ProcessId, Terminal};

const NO_DEVICE: u32 = u32::MAX;

pub fn process(pid: ProcessId) -> Option<Process> {
    let size = c_int::try_from(size_of::<libc::proc_bsdinfo>()).expect("proc_bsdinfo is small");
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    #[expect(
        unsafe_code,
        reason = "libproc is C, which libc only declares. `info` has room for the `size` bytes \
                  asked, and the call writes no more than it is given"
    )]
    let written = unsafe {
        libc::proc_pidinfo(
            c_int::try_from(pid.get()).ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast::<c_void>(),
            size,
        )
    };
    if written != size {
        return None;
    }
    #[expect(unsafe_code, reason = "the call wrote all of `info`: `written` is its `size`")]
    let info = unsafe { info.assume_init() };
    let read_pid = ProcessId::new(info.pbi_pid)?;
    assert_eq!(read_pid, pid, "proc_pidinfo said another process");
    let group = Group::new(info.pbi_pgid)?;
    let name = info.pbi_comm.iter().map(|byte| byte.cast_unsigned()).take_while(|byte| *byte != 0);
    let terminal = (info.e_tdev != NO_DEVICE).then(|| Terminal {
        device: u64::from(info.e_tdev),
        front: Group::new(info.e_tpgid),
    });
    Some(Process {
        pid: read_pid,
        parent: ProcessId::new(info.pbi_ppid),
        group,
        name: String::from_utf8_lossy(&name.collect::<Vec<u8>>()).into_owned(),
        sleeps: String::new(),
        stopped: info.pbi_status == libc::SSTOP,
        terminal,
    })
}

pub fn children(pid: ProcessId) -> Vec<ProcessId> {
    let Ok(pid) = c_int::try_from(pid.get()) else {
        return Vec::new();
    };
    let mut pids: Vec<libc::pid_t> = vec![0; CHILDREN_MAX];
    let bytes = c_int::try_from(pids.len() * size_of::<libc::pid_t>()).expect("a few KiB");
    #[expect(
        unsafe_code,
        reason = "libproc is C, which libc only declares. `pids` has room for the `bytes` it is \
                  given as"
    )]
    let count = unsafe { libc::proc_listchildpids(pid, pids.as_mut_ptr().cast::<c_void>(), bytes) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    pids.truncate(count.min(CHILDREN_MAX));
    pids.into_iter().filter_map(|pid| u32::try_from(pid).ok().and_then(ProcessId::new)).collect()
}

pub fn argv(pid: ProcessId) -> Option<Vec<String>> {
    let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, c_int::try_from(pid.get()).ok()?];
    let mut size = 0;
    #[expect(
        unsafe_code,
        reason = "sysctl is C, which libc only declares. With no buffer, the call writes only \
                  the size it needs"
    )]
    let sized = unsafe {
        libc::sysctl(name.as_mut_ptr(), 3, ptr::null_mut(), &raw mut size, ptr::null_mut(), 0)
    };
    if sized != 0 || size == 0 || size > EXEC_STRINGS_BYTES_MAX {
        return None;
    }
    let mut bytes = vec![0_u8; size];
    let mut size = bytes.len();
    #[expect(
        unsafe_code,
        reason = "sysctl is C, which libc only declares. `bytes` has room for the `size` it is \
                  given as, which the call writes no more than, and sets to what it wrote"
    )]
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
    assert!(size <= bytes.len(), "sysctl wrote {size} bytes into {}", bytes.len());
    bytes.truncate(size);
    let argv = laid_in_procargs(&bytes).ok()?.argv;
    (!argv.is_empty()).then(|| argv.iter().map(|arg| arg.to_string_lossy().into_owned()).collect())
}

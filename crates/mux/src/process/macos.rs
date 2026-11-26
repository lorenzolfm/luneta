use std::ffi::{c_int, c_void};
use std::mem::{MaybeUninit, size_of};
use std::ptr;

use lune_protocol::process::{EXEC_STRINGS_BYTES_MAX, laid_in_procargs};

use super::{CHILDREN_MAX, Group, OwnerInfo, Process, ProcessId, Terminal};

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

pub fn owner_info(pid: u32) -> Option<OwnerInfo> {
    let size = c_int::try_from(size_of::<libc::proc_bsdinfo>()).expect("proc_bsdinfo is small");
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    #[expect(
        unsafe_code,
        reason = "libproc is C, which libc only declares. `info` has room for the `size` bytes \
                  asked, and the call writes no more than it is given"
    )]
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
    #[expect(unsafe_code, reason = "the call wrote all of `info`: `written` is its `size`")]
    let info = unsafe { info.assume_init() };
    assert_eq!(info.pbi_pid, pid, "proc_pidinfo said another process");
    if info.pbi_status == libc::SZOMB {
        return None;
    }
    let name = info.pbi_comm.iter().map(|byte| byte.cast_unsigned()).take_while(|byte| *byte != 0);
    let name = String::from_utf8_lossy(&name.collect::<Vec<u8>>()).into_owned();
    let proc_start = format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec);
    let terminal_device = (info.e_tdev != NO_DEVICE).then(|| u64::from(info.e_tdev));
    let exe = pid_exe(pid);
    let cwd = vnode_paths(pid).map(|(cwd, _)| cwd).filter(|cwd| !cwd.is_empty());
    Some(OwnerInfo {
        pid: info.pbi_pid,
        parent: info.pbi_ppid,
        uid: info.pbi_ruid,
        euid: info.pbi_uid,
        proc_start,
        name,
        exe,
        cwd,
        terminal_device,
    })
}

pub fn same_root(pid: u32, root_pid: u32) -> bool {
    if pid == root_pid {
        return true;
    }
    let Some((_, a_root)) = vnode_paths(pid) else {
        return false;
    };
    let Some((_, b_root)) = vnode_paths(root_pid) else {
        return false;
    };
    a_root == b_root
}

fn pid_exe(pid: u32) -> Option<String> {
    let pid = c_int::try_from(pid).ok()?;
    let max = usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).expect("max path size fits usize");
    let mut buf = vec![0u8; max];
    let buf_size = u32::try_from(buf.len()).expect("max path size fits u32");
    #[expect(
        unsafe_code,
        reason = "proc_pidpath writes at most `buf_size` bytes into `buf` and returns byte count"
    )]
    let written = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast::<c_void>(), buf_size) };
    let written = usize::try_from(written).ok().filter(|count| *count > 0 && *count <= buf.len())?;
    String::from_utf8(buf[..written].to_vec()).ok()
}

fn vnode_paths(pid: u32) -> Option<(String, String)> {
    let pid = c_int::try_from(pid).ok()?;
    let size =
        c_int::try_from(size_of::<libc::proc_vnodepathinfo>()).expect("vnodepathinfo is small");
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::uninit();
    #[expect(
        unsafe_code,
        reason = "proc_pidinfo writes at most `size` bytes into `info`"
    )]
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
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
    let cwd = c_char_array_to_string(info.pvi_cdir.vip_path.as_flattened());
    let root = c_char_array_to_string(info.pvi_rdir.vip_path.as_flattened());
    Some((cwd, root))
}

fn c_char_array_to_string(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().map(|ch| ch.cast_unsigned()).take_while(|b| *b != 0).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

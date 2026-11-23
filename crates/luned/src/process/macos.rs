use std::ffi::OsString;
use std::io;
use std::ptr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::event::kqueue::{kevent, kqueue, Event, EventFilter, EventFlags, ProcessEvents};
use rustix::io::Errno;
use rustix::process::{kill_process, Pid, Signal};
use sysinfo::{
    Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind,
};

use lune_protocol::process::{laid_in_procargs, EXEC_STRINGS_BYTES_MAX};

use super::{Found, Id, Mine, Start, Unended, Unread, Unreadable};

fn with<T>(pid: u32, refresh: ProcessRefreshKind, read: impl FnOnce(&Process) -> T) -> Option<T> {
    let mut system = System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, refresh);
    system.process(pid).map(read)
}

#[must_use]
pub fn start(pid: u32) -> Option<Start> {
    let secs = with(pid, ProcessRefreshKind::nothing(), Process::start_time)?;
    Start::spelled(lstart(i64::try_from(secs).ok()?))
}

#[must_use]
pub fn started(pid: u32) -> Option<SystemTime> {
    let secs = with(pid, ProcessRefreshKind::nothing(), Process::start_time)?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs.checked_add(1)?))
}

pub fn environ(pid: u32) -> Result<Vec<OsString>, Unread> {
    let procargs = procargs(pid)?;
    laid_in_procargs(&procargs).and_then(|laid| laid.environ()).map_err(unparsed)
}

fn unparsed(why: &str) -> Unread {
    Unread::Failed(why.to_owned())
}

#[expect(
    unsafe_code,
    reason = "sysctl writes into the buffer it is handed, and no further than the length it is told"
)]
fn procargs(pid: u32) -> Result<Vec<u8>, Unread> {
    let Ok(native) = libc::c_int::try_from(pid) else {
        return Err(Unread::Failed(format!("pid {pid} does not fit a c_int")));
    };
    let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, native];
    let name_count = libc::c_uint::try_from(name.len()).expect("three names fit a c_uint");
    let mut size: libc::size_t = 0;
    let sized = unsafe {
        let into = ptr::null_mut();
        libc::sysctl(name.as_mut_ptr(), name_count, into, &raw mut size, ptr::null_mut(), 0)
    };
    if sized == -1 {
        return Err(unread(pid, &io::Error::last_os_error()));
    }
    if size > EXEC_STRINGS_BYTES_MAX {
        let most = EXEC_STRINGS_BYTES_MAX;
        let why = format!("KERN_PROCARGS2 of {size} bytes runs past the {most} exec lays out");
        return Err(Unread::Failed(why));
    }
    let mut procargs = vec![0_u8; size];
    let read = unsafe {
        let into = procargs.as_mut_ptr().cast();
        libc::sysctl(name.as_mut_ptr(), name_count, into, &raw mut size, ptr::null_mut(), 0)
    };
    if read == -1 {
        return Err(unread(pid, &io::Error::last_os_error()));
    }
    assert!(size <= procargs.len(), "sysctl wrote {size} bytes into {}", procargs.len());
    procargs.truncate(size);
    Ok(procargs)
}

fn unread(pid: u32, error: &io::Error) -> Unread {
    let errno = Errno::from_io_error(error);
    let ended = errno == Some(Errno::INVAL) && living_start(pid).is_none();
    if ended || errno == Some(Errno::SRCH) {
        Unread::Gone
    } else {
        Unread::Failed(error.to_string())
    }
}

#[must_use]
pub(super) fn listed(names: &[&str]) -> Mine {
    let mut system = System::new();
    let refresh = ProcessRefreshKind::nothing().with_user(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
    let uid = rustix::process::getuid().as_raw();
    let mut seen = Mine::default();
    for (pid, process) in system.processes() {
        let Some(name) = process.name().to_str().filter(|name| names.contains(name)) else {
            continue;
        };
        let mine = process.user_id().is_some_and(|it| **it == uid);
        if !mine || process.status() == ProcessStatus::Zombie {
            continue;
        }
        let start = i64::try_from(process.start_time()).ok().map(lstart);
        let Some(start) = start.and_then(Start::spelled) else {
            continue;
        };
        let id = Id { pid: pid.as_u32(), start };
        let read = procargs(pid.as_u32()).and_then(|procargs| {
            let laid = laid_in_procargs(&procargs).map_err(unparsed)?;
            let environ = laid.environ().map_err(unparsed)?;
            Ok((laid.argv, environ))
        });
        let (argv, environ) = match read {
            Ok(read) => read,
            Err(Unread::Gone) => continue,
            Err(Unread::Failed(why)) => {
                seen.unreadable.push(Unreadable { id, name: name.to_owned(), why });
                continue;
            },
        };
        let parent = process.parent().map(sysinfo::Pid::as_u32);
        assert_ne!(parent, Some(pid.as_u32()), "{pid} is its own parent");
        seen.found.push(Found { id, parent, name: name.to_owned(), argv, environ });
    }
    seen
}

pub(super) fn named(pid: u32) -> Option<String> {
    let name = |process: &Process| process.name().to_str().map(str::to_owned);
    with(pid, ProcessRefreshKind::nothing(), name)?
}

fn living_start(pid: u32) -> Option<Start> {
    let living = |process: &Process| {
        (process.status() != ProcessStatus::Zombie).then(|| process.start_time())
    };
    let secs = with(pid, ProcessRefreshKind::nothing(), living)??;
    Start::spelled(lstart(i64::try_from(secs).ok()?))
}

#[expect(
    unsafe_code,
    reason = "rustix's kevent is unsafe for the files an event may name, and these name none"
)]
#[must_use]
pub fn signalled(ids: &[Id], signal: Signal, wait: Duration) -> Vec<(Id, Unended)> {
    let queue = match kqueue() {
        Ok(queue) => queue,
        Err(error) => {
            return ids.iter().map(|id| (id.clone(), Unended::Unsignalled(error))).collect();
        },
    };
    let mut left = Vec::new();
    let mut waiting: Vec<(&Id, Pid)> = Vec::new();
    for id in ids {
        let Some(pid) = i32::try_from(id.pid).ok().and_then(Pid::from_raw) else {
            left.push((id.clone(), Unended::Unsignalled(Errno::INVAL)));
            continue;
        };
        if living_start(id.pid).as_ref() != Some(&id.start) {
            continue;
        }
        match kill_process(pid, signal) {
            Ok(()) => {},
            Err(Errno::SRCH) => continue,
            Err(error) => {
                left.push((id.clone(), Unended::Unsignalled(error)));
                continue;
            },
        }
        let flags = ProcessEvents::EXIT;
        let end = Event::new(EventFilter::Proc { pid, flags }, EventFlags::ADD, ptr::null_mut());
        let mut unheard: [Event; 0] = [];
        match unsafe { kevent(&queue, &[end], &mut unheard[..], Some(Duration::ZERO)) } {
            Ok(_) => waiting.push((id, pid)),
            Err(Errno::SRCH) => {},
            Err(error) => left.push((id.clone(), Unended::Unwatched(error))),
        }
    }
    let deadline = Instant::now() + wait;
    let flags = ProcessEvents::EXIT;
    let blank = EventFilter::Proc { pid: Pid::INIT, flags };
    let blank = Event::new(blank, EventFlags::empty(), ptr::null_mut());
    while !waiting.is_empty() {
        let Some(until) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let mut heard = vec![blank; waiting.len()];
        let count = match unsafe { kevent(&queue, &[], &mut heard[..], Some(until)) } {
            Ok(0) => break,
            Ok(count) => count,
            Err(Errno::INTR) => continue,
            Err(error) => {
                let unwatched = Unended::Unwatched(error);
                left.extend(waiting.drain(..).map(|(id, _)| (id.clone(), unwatched)));
                break;
            },
        };
        for event in heard.iter().take(count) {
            let EventFilter::Proc { pid, .. } = event.filter() else {
                continue;
            };
            let Some(index) = waiting.iter().position(|(_, waits)| *waits == pid) else {
                continue;
            };
            let (id, _) = waiting.swap_remove(index);
            if !event.flags().contains(EventFlags::ERROR) {
                continue;
            }
            let error = i32::try_from(event.data()).map_or(Errno::INVAL, Errno::from_raw_os_error);
            if error != Errno::SRCH {
                left.push((id.clone(), Unended::Unwatched(error)));
            }
        }
    }
    left.extend(waiting.into_iter().map(|(id, _)| (id.clone(), Unended::Outlived)));
    left
}

#[expect(
    unsafe_code,
    reason = "rustix's kevent is unsafe for the files an event may name, and this one names none"
)]
pub fn ends(pid: Pid) -> io::Result<()> {
    let queue = kqueue()?;
    let flags = ProcessEvents::EXIT;
    let end = Event::new(EventFilter::Proc { pid, flags }, EventFlags::ADD, ptr::null_mut());
    let mut adding: &[Event] = &[end];
    let mut heard = [end];
    loop {
        let failure = match unsafe { kevent(&queue, adding, &mut heard[..], None) } {
            Ok(_) if !heard[0].flags().contains(EventFlags::ERROR) => return Ok(()),
            Ok(_) => i32::try_from(heard[0].data()).map_or(Errno::INVAL, Errno::from_raw_os_error),
            Err(Errno::INTR) => {
                adding = &[];
                continue;
            },
            Err(failure) => failure,
        };
        return if failure == Errno::SRCH { Ok(()) } else { Err(failure.into()) };
    }
}

pub struct Domain;

impl Domain {
    #[must_use]
    pub fn here() -> Self {
        Domain
    }

    #[must_use]
    #[expect(
        clippy::unused_self,
        reason = "one signature for both backends, and Linux's reads itself"
    )]
    pub fn admits(&self, recorded: &str) -> bool {
        recorded == "darwin"
    }
}

fn lstart(secs: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

    let days = secs.div_euclid(86_400);
    let clock = secs.rem_euclid(86_400);
    let (year, month, day) = civil(days);
    assert!((1..=12).contains(&month));
    assert!((1..=31).contains(&day));
    let weekday = usize::try_from(days.rem_euclid(7)).expect("a remainder of 7 is in 0..7");
    let month_index = usize::try_from(month - 1).expect("a month is in 1..=12");
    format!(
        "{} {} {day:>2} {:02}:{:02}:{:02} {year}",
        WEEKDAYS[weekday],
        MONTHS[month_index],
        clock / 3600,
        clock % 3600 / 60,
        clock % 60,
    )
}

fn civil(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

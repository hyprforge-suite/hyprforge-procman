//! Ending a process — the one thing this app does to the system.
//!
//! A process is chosen by its [`Id`]: pid and start time. Between the
//! choosing and the signal the process may end and its pid be handed to
//! a new one, and `kill(pid)` would then end a process nobody chose. So a
//! pidfd is opened first — it names one process for as long as it is
//! held, never a successor — and only then is the start time checked
//! through `/proc`. A match means the pidfd is the chosen process, and
//! the signal goes through the pidfd, so it can reach no other.

use crate::sample::Id;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// SIGTERM: asks it to finish up and exit. What "End" does.
    Ask,
    /// SIGKILL: stops it at once, nothing saved. What "Kill" does, after
    /// asking first did not work.
    Force,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// It already ended, or the pid now names another process.
    Gone,
    /// Another user's process: ending it needs administrator access.
    NotAllowed,
    Other(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Gone => write!(f, "it had already ended"),
            Refused::NotAllowed => write!(f, "it belongs to another user, so ending it needs administrator access"),
            Refused::Other(e) => write!(f, "{e}"),
        }
    }
}

fn refused(e: io::Error) -> Refused {
    match e.raw_os_error() {
        Some(libc::ESRCH) => Refused::Gone,
        Some(libc::EPERM) => Refused::NotAllowed,
        _ => Refused::Other(e.to_string()),
    }
}

/// Sends `how` to the process `id` names, and to no other.
pub fn end(id: Id, how: How) -> Result<(), Refused> {
    send(
        id,
        match how {
            How::Ask => libc::SIGTERM,
            How::Force => libc::SIGKILL,
        },
    )
}

/// Freezes `id` (SIGSTOP) or lets it run again (SIGCONT). A frozen
/// process keeps everything it has and uses no CPU — a way to stop
/// something busy without losing its work, which ending would.
pub fn freeze(id: Id, frozen: bool) -> Result<(), Refused> {
    send(id, if frozen { libc::SIGSTOP } else { libc::SIGCONT })
}

/// Sets `id`'s nice value, on every thread, after the same identity check
/// as a signal.
///
/// Weaker than a signal's guarantee, and said so here rather than
/// claimed: `setpriority` takes a thread id, not a pidfd, and holding a
/// pidfd does *not* stop a process being reaped or its pid being reused
/// — it only stops the pidfd itself from naming a successor. So the
/// start time is checked again afterwards: a process that ended in the
/// gap reports [`Refused::Gone`] instead of success, though a reuse that
/// happened inside it may already have had its priority changed. It
/// needs the pid space to wrap in that gap, and a nice value is the
/// mildest thing that can land on the wrong process.
pub fn renice(id: Id, nice: i32) -> Result<(), Refused> {
    let _held = open(id)?;
    let set = nice_every_thread(id.pid, nice);
    if crate::proc::start_time(id.pid) != Some(id.start) {
        return Err(Refused::Gone);
    }
    set.map_err(refused)
}

/// `setpriority` on each thread of `pid`.
///
/// On Linux `setpriority(PRIO_PROCESS, pid)` changes one *thread* — the
/// one whose id is `pid` — not the process, so a program that had
/// already started its workers kept them at the old priority. Threads
/// and children made afterwards inherit it from whichever thread makes
/// them. Succeeds if any thread was set; a thread ending mid-walk is not
/// a failure.
pub fn nice_every_thread(pid: i32, to: i32) -> io::Result<()> {
    let mut last = None;
    let mut any = false;
    for e in std::fs::read_dir(format!("/proc/{pid}/task"))?.flatten() {
        let Some(tid) = e.file_name().to_str().and_then(|n| n.parse::<libc::id_t>().ok()) else { continue };
        // SAFETY: a plain syscall on a thread id; failure is reported.
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, to) } == 0 {
            any = true;
        } else {
            last = Some(io::Error::last_os_error());
        }
    }
    match (any, last) {
        (true, _) => Ok(()),
        (false, Some(e)) => Err(e),
        // No threads at all: it ended between listing and reading.
        (false, None) => Err(io::Error::from_raw_os_error(libc::ESRCH)),
    }
}

/// A pidfd for `id`, once its start time has been checked against it.
fn open(id: Id) -> Result<OwnedFd, Refused> {
    // SAFETY: pidfd_open takes a pid and flags and returns a new fd or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, id.pid, 0) };
    if fd < 0 {
        return Err(refused(io::Error::last_os_error()));
    }
    // SAFETY: a non-negative return is a descriptor we now own.
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    if crate::proc::start_time(id.pid) != Some(id.start) {
        return Err(Refused::Gone);
    }
    Ok(pidfd)
}

/// The signal `signal` to `id`, through a pidfd. Signal 0 sends nothing
/// and only asks whether sending would be allowed.
pub fn send(id: Id, signal: i32) -> Result<(), Refused> {
    let pidfd = open(id)?;
    // SAFETY: a held pidfd, a signal number, no siginfo, no flags.
    let sent = unsafe {
        libc::syscall(libc::SYS_pidfd_send_signal, pidfd.as_raw_fd(), signal, std::ptr::null::<libc::siginfo_t>(), 0)
    };
    if sent < 0 {
        return Err(refused(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;

    fn sleeper() -> (std::process::Child, Id) {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id() as i32;
        let start = crate::proc::start_time(pid).unwrap();
        (child, Id { pid, start })
    }

    #[test]
    fn end_asks_the_chosen_process_to_stop() {
        let (mut child, id) = sleeper();
        end(id, How::Ask).unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn kill_stops_it_outright() {
        let (mut child, id) = sleeper();
        end(id, How::Force).unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    /// The pid is right and the start time is not — what a reused pid
    /// looks like. Nothing is sent, and the process lives.
    #[test]
    fn a_pid_that_now_names_another_process_is_never_signalled() {
        let (mut child, id) = sleeper();
        let stale = Id { start: id.start.wrapping_sub(1), ..id };
        assert_eq!(end(stale, How::Force), Err(Refused::Gone));
        assert!(child.try_wait().unwrap().is_none(), "still running");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_frozen_process_stops_and_resumes() {
        let (mut child, id) = sleeper();
        freeze(id, true).unwrap();
        let state = || std::fs::read_to_string(format!("/proc/{}/stat", id.pid)).unwrap();
        let stopped = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            state().contains(") T ")
        });
        assert!(stopped, "stopped");
        freeze(id, false).unwrap();
        let running = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            !state().contains(") T ")
        });
        assert!(running, "resumed");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_process_that_already_ended_is_gone() {
        let (mut child, id) = sleeper();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(end(id, How::Ask), Err(Refused::Gone));
    }

    /// Set when this test binary is re-run as the threaded process below.
    const THREADED_CHILD: &str = "HYPRFORGE_PROCMAN_THREADED_CHILD";

    /// A thread's nice value, field 19 of its `stat`.
    fn nice_of(pid: i32, tid: &str) -> i32 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")).unwrap();
        stat.rsplit_once(") ").unwrap().1.split_whitespace().nth(16).unwrap().parse().unwrap()
    }

    /// `setpriority(PRIO_PROCESS, pid)` on Linux sets one *thread*, the
    /// one whose id is the pid. A browser or a compiler has dozens, so
    /// "lower its priority" changed the main thread and left the ones
    /// doing the work as they were.
    ///
    /// The threaded process is this test binary run again, filtered to
    /// this test, with [`THREADED_CHILD`] set: the only multi-threaded
    /// program every machine running these tests is sure to have. The
    /// child role parks threads and never returns; it is killed below.
    #[test]
    fn renice_reaches_every_thread_not_just_the_first() {
        if std::env::var_os(THREADED_CHILD).is_some() {
            for _ in 0..3 {
                std::thread::spawn(|| loop {
                    std::thread::park();
                });
            }
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "signal::tests::renice_reaches_every_thread_not_just_the_first", "--nocapture", "--test-threads=1"])
            .env(THREADED_CHILD, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let tasks = || -> Vec<String> {
            std::fs::read_dir(format!("/proc/{pid}/task"))
                .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default()
        };
        // The harness's own thread, the test's, and the three it starts.
        let started = (0..500).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            tasks().len() >= 5
        });
        let id = Id { pid, start: crate::proc::start_time(pid).unwrap() };
        let to = (nice_of(pid, &pid.to_string()) + 3).min(19);
        let result = renice(id, to);
        let nices: Vec<(String, i32)> = tasks().into_iter().map(|t| { let n = nice_of(pid, &t); (t, n) }).collect();
        child.kill().unwrap();
        child.wait().unwrap();

        assert!(started, "the threaded child never started its threads");
        result.unwrap();
        assert!(nices.iter().all(|(_, n)| *n == to), "every thread at {to}, got {nices:?}");
    }

    /// pid 1 is root's: refused for permission. Asked with signal 0,
    /// which sends nothing, so this test can never end anything but its
    /// own children.
    #[test]
    fn another_users_process_is_refused_as_not_allowed() {
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("HYPRFORGE-SKIP: running as root, so nothing is another user's");
            return;
        }
        let start = crate::proc::start_time(1).unwrap();
        assert_eq!(send(Id { pid: 1, start }, 0), Err(Refused::NotAllowed));
    }
}

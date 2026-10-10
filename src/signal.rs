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

/// The signal `signal` to `id`, through a pidfd. Signal 0 sends nothing
/// and only asks whether sending would be allowed.
fn send(id: Id, signal: i32) -> Result<(), Refused> {
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
    fn a_process_that_already_ended_is_gone() {
        let (mut child, id) = sleeper();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(end(id, How::Ask), Err(Refused::Gone));
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

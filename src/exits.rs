//! Processes as they exit, from the kernel's taskstats interface — how a
//! process that started and finished between two samples is still
//! recorded, with what it used and how it ended. atop does the same.
//!
//! taskstats is a generic-netlink family. The family's number is asked
//! of the netlink controller by name; then either one pid is asked about
//! (`CMD_GET`), or a set of CPUs is registered and every exit on them is
//! sent (`REGISTER_CPUMASK`, which needs `CAP_NET_ADMIN`: the recorder,
//! running as root).
//!
//! The record is the kernel's `struct taskstats`, and its offsets here
//! are the ones `offsetof` gave against this machine's
//! `<linux/taskstats.h>` (version 17, 688 bytes) — not counted by hand,
//! where the `aligned(8)` padding is easy to get wrong. The struct only
//! ever grows at its end, so an older kernel's shorter record is read
//! for whatever fields it is long enough to hold.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// One process's life, as the kernel summed it up when it ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Exit {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub comm: String,
    /// The wait status: `exit_code << 8`, or the signal that ended it.
    pub status: u32,
    /// `acct.h` flags: forked without exec (1), used root (2), dumped
    /// core (8), killed by a signal (16).
    pub flags: u8,
    /// When it began, seconds since the epoch, and how long it ran.
    pub began: u64,
    pub elapsed_us: u64,
    pub user_us: u64,
    pub system_us: u64,
    pub minor_faults: u64,
    pub major_faults: u64,
    /// Its most resident memory, in KiB.
    pub peak_rss_kib: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub cancelled_write_bytes: u64,
    pub voluntary_switches: u64,
    pub forced_switches: u64,
    /// Waiting for a CPU, and for block I/O (the latter only with delay
    /// accounting on), in nanoseconds.
    pub cpu_delay_ns: u64,
    pub blkio_delay_ns: u64,
}

impl Exit {
    /// The exit code, or `None` when a signal ended it.
    pub fn code(&self) -> Option<u32> {
        (self.status & 0x7f == 0).then_some((self.status >> 8) & 0xff)
    }

    /// The signal that ended it.
    pub fn signal(&self) -> Option<u32> {
        let s = self.status & 0x7f;
        (s != 0).then_some(s)
    }
}

// From `offsetof` against <linux/taskstats.h> on this machine.
const OFF_EXITCODE: usize = 4;
const OFF_FLAG: usize = 8;
const OFF_CPU_DELAY: usize = 24;
const OFF_BLKIO_DELAY: usize = 40;
const OFF_COMM: usize = 80;
const COMM_LEN: usize = 32;
const OFF_UID: usize = 120;
const OFF_PID: usize = 128;
const OFF_PPID: usize = 132;
const OFF_BTIME: usize = 136;
const OFF_ETIME: usize = 144;
const OFF_UTIME: usize = 152;
const OFF_STIME: usize = 160;
const OFF_MINFLT: usize = 168;
const OFF_MAJFLT: usize = 176;
const OFF_HIWATER_RSS: usize = 200;
const OFF_READ_BYTES: usize = 248;
const OFF_WRITE_BYTES: usize = 256;
const OFF_CANCELLED: usize = 264;
const OFF_NVCSW: usize = 272;
const OFF_NIVCSW: usize = 280;

/// `struct taskstats` → [`Exit`]. Fields past the end of a shorter
/// (older) record read as zero.
pub fn parse_stats(b: &[u8]) -> Option<Exit> {
    if b.len() < OFF_PPID + 4 {
        return None;
    }
    let u32_at = |o: usize| b.get(o..o + 4).map_or(0, |s| u32::from_ne_bytes(s.try_into().unwrap()));
    let u64_at = |o: usize| b.get(o..o + 8).map_or(0, |s| u64::from_ne_bytes(s.try_into().unwrap()));
    let comm = b.get(OFF_COMM..OFF_COMM + COMM_LEN).unwrap_or(&[]);
    let comm = String::from_utf8_lossy(&comm[..comm.iter().position(|c| *c == 0).unwrap_or(comm.len())]).into_owned();
    Some(Exit {
        pid: u32_at(OFF_PID),
        ppid: u32_at(OFF_PPID),
        uid: u32_at(OFF_UID),
        comm,
        status: u32_at(OFF_EXITCODE),
        flags: b[OFF_FLAG],
        began: u64::from(u32_at(OFF_BTIME)),
        elapsed_us: u64_at(OFF_ETIME),
        user_us: u64_at(OFF_UTIME),
        system_us: u64_at(OFF_STIME),
        minor_faults: u64_at(OFF_MINFLT),
        major_faults: u64_at(OFF_MAJFLT),
        peak_rss_kib: u64_at(OFF_HIWATER_RSS),
        read_bytes: u64_at(OFF_READ_BYTES),
        write_bytes: u64_at(OFF_WRITE_BYTES),
        cancelled_write_bytes: u64_at(OFF_CANCELLED),
        voluntary_switches: u64_at(OFF_NVCSW),
        forced_switches: u64_at(OFF_NIVCSW),
        cpu_delay_ns: u64_at(OFF_CPU_DELAY),
        blkio_delay_ns: u64_at(OFF_BLKIO_DELAY),
    })
}

// Netlink and taskstats constants, from the same headers.
const NETLINK_GENERIC: i32 = 16;
const GENL_ID_CTRL: u16 = 16;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const TASKSTATS_CMD_GET: u8 = 1;
const TASKSTATS_CMD_ATTR_PID: u16 = 1;
const TASKSTATS_CMD_ATTR_REGISTER_CPUMASK: u16 = 3;
const TASKSTATS_TYPE_PID: u16 = 1;
const TASKSTATS_TYPE_TGID: u16 = 2;
const TASKSTATS_TYPE_STATS: u16 = 3;
const TASKSTATS_TYPE_AGGR_PID: u16 = 4;
const TASKSTATS_TYPE_AGGR_TGID: u16 = 5;
const NLM_F_REQUEST: u16 = 1;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// One netlink request: header, generic-netlink header, attributes.
fn message(family: u16, cmd: u8, attrs: &[(u16, &[u8])]) -> Vec<u8> {
    let mut body = vec![cmd, 1, 0, 0];
    for (kind, value) in attrs {
        let len = 4 + value.len();
        body.extend_from_slice(&(len as u16).to_ne_bytes());
        body.extend_from_slice(&kind.to_ne_bytes());
        body.extend_from_slice(value);
        body.resize(align4(body.len()), 0);
    }
    let mut msg = Vec::with_capacity(16 + body.len());
    msg.extend_from_slice(&((16 + body.len()) as u32).to_ne_bytes());
    msg.extend_from_slice(&family.to_ne_bytes());
    msg.extend_from_slice(&NLM_F_REQUEST.to_ne_bytes());
    msg.extend_from_slice(&0u32.to_ne_bytes()); // sequence
    msg.extend_from_slice(&0u32.to_ne_bytes()); // port id: the kernel's
    msg.extend_from_slice(&body);
    msg
}

/// The attributes of a block: `(type, value)`, nesting flags stripped.
fn attrs(mut b: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while b.len() >= 4 {
        let len = u16::from_ne_bytes([b[0], b[1]]) as usize;
        let kind = u16::from_ne_bytes([b[2], b[3]]) & 0x3fff;
        if len < 4 || len > b.len() {
            break;
        }
        out.push((kind, &b[4..len]));
        b = &b[align4(len).min(b.len())..];
    }
    out
}

/// Every netlink message in a datagram: `(type, payload)`.
fn messages(mut b: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while b.len() >= 16 {
        let len = u32::from_ne_bytes(b[0..4].try_into().unwrap()) as usize;
        let kind = u16::from_ne_bytes([b[4], b[5]]);
        if len < 16 || len > b.len() {
            break;
        }
        out.push((kind, &b[16..len]));
        b = &b[align4(len).min(b.len())..];
    }
    out
}

/// The exits in one taskstats message. A whole process's end arrives as
/// `AGGR_TGID` (all its threads summed); one thread's as `AGGR_PID`.
/// `whole` keeps only the former.
pub fn parse_message(payload: &[u8], whole: bool) -> Vec<Exit> {
    let Some(genl_attrs) = payload.get(4..) else { return Vec::new() };
    let mut out = Vec::new();
    for (kind, nested) in attrs(genl_attrs) {
        let wanted = match kind {
            TASKSTATS_TYPE_AGGR_TGID => true,
            TASKSTATS_TYPE_AGGR_PID => !whole,
            _ => false,
        };
        if !wanted {
            continue;
        }
        let mut id = None;
        let mut stats = None;
        for (k, v) in attrs(nested) {
            match k {
                TASKSTATS_TYPE_PID | TASKSTATS_TYPE_TGID => id = v.get(..4).map(|s| u32::from_ne_bytes(s.try_into().unwrap())),
                TASKSTATS_TYPE_STATS => stats = parse_stats(v),
                _ => {}
            }
        }
        if let Some(mut exit) = stats {
            if let Some(id) = id {
                exit.pid = id;
            }
            out.push(exit);
        }
    }
    out
}

/// A taskstats netlink socket.
pub struct Taskstats {
    fd: OwnedFd,
    family: u16,
}

fn check(rc: isize) -> io::Result<usize> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc as usize)
    }
}

impl Taskstats {
    pub fn open() -> io::Result<Taskstats> {
        // SAFETY: plain socket creation; the fd is owned below.
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, NETLINK_GENERIC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor this function owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // Receive with a bound, so a quiet kernel cannot hang a reader.
        let timeout = libc::timeval { tv_sec: 5, tv_usec: 0 };
        // SAFETY: a valid fd and a timeval of the stated size.
        unsafe {
            libc::setsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVTIMEO, (&timeout as *const libc::timeval).cast(), std::mem::size_of::<libc::timeval>() as u32);
        }
        let mut socket = Taskstats { fd, family: 0 };
        socket.send(&message(GENL_ID_CTRL, CTRL_CMD_GETFAMILY, &[(CTRL_ATTR_FAMILY_NAME, b"TASKSTATS\0")]))?;
        for (kind, payload) in socket.receive()? {
            if kind == NLMSG_ERROR {
                return Err(netlink_error(&payload));
            }
            for (k, v) in attrs(payload.get(4..).unwrap_or_default()) {
                if k == CTRL_ATTR_FAMILY_ID && v.len() >= 2 {
                    socket.family = u16::from_ne_bytes([v[0], v[1]]);
                }
            }
        }
        if socket.family == 0 {
            return Err(io::Error::other("the kernel has no taskstats family"));
        }
        Ok(socket)
    }

    fn send(&self, msg: &[u8]) -> io::Result<()> {
        // SAFETY: zeroed sockaddr_nl names the kernel (port 0).
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        // SAFETY: valid fd, buffer and address of the stated sizes.
        let rc = unsafe {
            libc::sendto(self.fd.as_raw_fd(), msg.as_ptr().cast(), msg.len(), 0, (&addr as *const libc::sockaddr_nl).cast(), std::mem::size_of::<libc::sockaddr_nl>() as u32)
        };
        check(rc).map(|_| ())
    }

    /// One datagram's messages, owned.
    fn receive(&self) -> io::Result<Vec<(u16, Vec<u8>)>> {
        let mut buf = vec![0u8; 65536];
        // SAFETY: a buffer of the stated length.
        let n = check(unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) })?;
        Ok(messages(&buf[..n]).into_iter().map(|(k, p)| (k, p.to_vec())).collect())
    }

    /// One living process's figures so far.
    pub fn get(&self, pid: u32) -> io::Result<Exit> {
        self.send(&message(self.family, TASKSTATS_CMD_GET, &[(TASKSTATS_CMD_ATTR_PID, &pid.to_ne_bytes())]))?;
        for (kind, payload) in self.receive()? {
            if kind == NLMSG_ERROR {
                return Err(netlink_error(&payload));
            }
            if let Some(exit) = parse_message(&payload, false).into_iter().next() {
                return Ok(exit);
            }
        }
        Err(io::Error::other("taskstats answered with no record"))
    }

    /// Asks for every exit on `cpus` (`"0-15"`) from now on. Root only.
    pub fn listen(&self, cpus: &str) -> io::Result<()> {
        let mask = format!("{cpus}\0");
        self.send(&message(self.family, TASKSTATS_CMD_GET, &[(TASKSTATS_CMD_ATTR_REGISTER_CPUMASK, mask.as_bytes())]))?;
        Ok(())
    }

    /// Waits (up to the socket's five seconds) for the next exits; whole
    /// processes only.
    pub fn exits(&self) -> io::Result<Vec<Exit>> {
        let mut out = Vec::new();
        for (kind, payload) in self.receive()? {
            match kind {
                NLMSG_ERROR => {
                    let e = netlink_error(&payload);
                    if e.raw_os_error().is_some_and(|c| c != 0) {
                        return Err(e);
                    }
                }
                NLMSG_DONE => {}
                _ => out.extend(parse_message(&payload, true)),
            }
        }
        Ok(out)
    }
}

/// An `NLMSG_ERROR`'s errno, as an error (0 is an acknowledgement).
fn netlink_error(payload: &[u8]) -> io::Error {
    let code = payload.get(..4).map_or(0, |s| i32::from_ne_bytes(s.try_into().unwrap()));
    io::Error::from_raw_os_error(-code)
}

/// "0-15", for every CPU this machine has.
pub fn all_cpus() -> String {
    // SAFETY: sysconf has no preconditions.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) }.max(1);
    format!("0-{}", n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record laid out at the header's offsets, as the kernel sends it.
    fn record() -> Vec<u8> {
        let mut b = vec![0u8; 688];
        let put32 = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_ne_bytes());
        let put64 = |b: &mut Vec<u8>, o: usize, v: u64| b[o..o + 8].copy_from_slice(&v.to_ne_bytes());
        b[0..2].copy_from_slice(&17u16.to_ne_bytes());
        put32(&mut b, OFF_EXITCODE, 3 << 8);
        b[OFF_COMM..OFF_COMM + 5].copy_from_slice(b"sleep");
        put32(&mut b, OFF_PID, 4242);
        put32(&mut b, OFF_PPID, 1);
        put32(&mut b, OFF_UID, 1000);
        put64(&mut b, OFF_UTIME, 1500);
        put64(&mut b, OFF_READ_BYTES, 8192);
        put64(&mut b, OFF_NIVCSW, 7);
        b
    }

    #[test]
    fn a_record_reads_at_the_headers_offsets() {
        let e = parse_stats(&record()).unwrap();
        assert_eq!((e.pid, e.ppid, e.uid, e.comm.as_str()), (4242, 1, 1000, "sleep"));
        assert_eq!((e.code(), e.signal()), (Some(3), None));
        assert_eq!((e.user_us, e.read_bytes, e.forced_switches), (1500, 8192, 7));
    }

    #[test]
    fn an_older_shorter_record_reads_what_it_holds() {
        let short = &record()[..200];
        let e = parse_stats(short).unwrap();
        assert_eq!(e.pid, 4242);
        assert_eq!(e.read_bytes, 0, "past the end reads as zero, not garbage");
    }

    #[test]
    fn a_killed_process_reports_its_signal_not_a_code() {
        let e = Exit { status: 9, ..Exit::default() };
        assert_eq!((e.code(), e.signal()), (None, Some(9)));
    }

    #[test]
    fn a_message_yields_whole_processes_and_their_tgid() {
        let stats = record();
        let mut nested = Vec::new();
        for (k, v) in [(TASKSTATS_TYPE_TGID, &4300u32.to_ne_bytes()[..]), (TASKSTATS_TYPE_STATS, &stats[..])] {
            nested.extend_from_slice(&((4 + v.len()) as u16).to_ne_bytes());
            nested.extend_from_slice(&k.to_ne_bytes());
            nested.extend_from_slice(v);
            nested.resize(align4(nested.len()), 0);
        }
        let msg = message(30, 2, &[(TASKSTATS_TYPE_AGGR_TGID, &nested)]);
        let parsed = messages(&msg);
        let exits = parse_message(parsed[0].1, true);
        assert_eq!(exits.len(), 1);
        assert_eq!(exits[0].pid, 4300, "the tgid, not the last thread's pid");
    }

    /// The real kernel, about this very process: the family resolves and
    /// a record comes back that names it. Unprivileged where the kernel
    /// allows asking about one's own pid; skipped, and said, where not.
    #[test]
    fn the_kernel_answers_about_this_process() {
        let socket = match Taskstats::open() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("HYPRFORGE-SKIP: taskstats unavailable: {e}");
                return;
            }
        };
        match socket.get(std::process::id()) {
            Ok(e) => {
                assert_eq!(e.pid, std::process::id());
                assert!(!e.comm.is_empty());
                assert!(e.user_us + e.system_us > 0, "a test process has run");
            }
            Err(e) => eprintln!("HYPRFORGE-SKIP: taskstats would not answer: {e}"),
        }
    }

    /// Root only: a process that lives a tenth of a second is caught as
    /// it exits, with its code.
    #[test]
    #[ignore = "needs root (CAP_NET_ADMIN) to listen"]
    fn a_short_lived_process_is_caught_as_it_exits() {
        let socket = Taskstats::open().unwrap();
        if let Err(e) = socket.listen(&all_cpus()) {
            eprintln!("HYPRFORGE-SKIP: can't listen for exits: {e}");
            return;
        }
        let mut child = std::process::Command::new("sh").args(["-c", "sleep 0.1; exit 3"]).spawn().unwrap();
        let pid = child.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Some(e) = socket.exits().unwrap_or_default().into_iter().find(|e| e.pid == pid) {
                assert_eq!(e.code(), Some(3));
                child.wait().unwrap();
                return;
            }
        }
        child.wait().unwrap();
        panic!("never heard the exit");
    }
}

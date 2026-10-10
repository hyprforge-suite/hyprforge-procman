//! Reading `/proc`: every process, the CPU's time counters and memory.
//!
//! [`ProcFs`] reads from a root that is `/proc` everywhere but the tests,
//! which hand it a directory laid out the same way, built from files
//! copied off a real `/proc` — not written from memory of the format
//! (the suite's CLAUDE.md: a fixture written from memory tests the
//! memory).
//!
//! Processes come and go while they are being read, so a process whose
//! files have vanished between listing `/proc` and opening them is not
//! an error: it is a process that ended, and is left out. Only failing to
//! read `/proc` itself, or `/proc/stat` and `/proc/meminfo`, is.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// One process, as one read of its files saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: i32,
    /// When it started, in clock ticks since boot. With the pid, this is
    /// the process's identity: a pid is reused, a pid and a start time
    /// are not.
    pub start: u64,
    /// What to call it — see [`display_name`].
    pub name: String,
    /// Its command line, arguments joined with spaces; empty for a kernel
    /// thread or a zombie.
    pub command: String,
    pub state: char,
    pub ppid: i32,
    /// User plus system time, in clock ticks, over its whole life.
    pub ticks: u64,
    pub threads: u32,
    /// Resident memory not shared with anything else, in bytes: what
    /// ending it would give back. `statm`'s resident minus shared.
    pub private_bytes: u64,
    pub uid: u32,
    /// A kernel thread (`PF_KTHREAD`), which has no program, no memory of
    /// its own and nothing a person can end.
    pub kernel: bool,
}

/// The CPU's cumulative time counters, from one `cpu` line of
/// `/proc/stat`, in clock ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuTimes {
    /// Every state but guest time, which the kernel already counts
    /// inside user time.
    pub total: u64,
    /// Idle plus waiting for I/O: time nothing was running.
    pub idle: u64,
}

/// Memory and swap, in bytes, from `/proc/meminfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Memory {
    pub total: u64,
    /// What could be given to a program without swapping — the kernel's
    /// own estimate (`MemAvailable`), which counts reclaimable cache. Used
    /// is total minus this, the figure `free` and every task manager show.
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

impl Memory {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }
}

/// Everything one refresh reads.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub procs: Vec<Proc>,
    /// The `cpu` line: every core added together.
    pub cpu: CpuTimes,
    /// `cpu0`, `cpu1`, … in order.
    pub cores: Vec<CpuTimes>,
    pub memory: Memory,
}

/// `/proc`, or a directory standing in for it.
#[derive(Debug, Clone)]
pub struct ProcFs {
    root: PathBuf,
    page_size: u64,
}

impl Default for ProcFs {
    fn default() -> ProcFs {
        // SAFETY: sysconf has no preconditions; it reads a constant.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        ProcFs { root: PathBuf::from("/proc"), page_size: u64::try_from(page).unwrap_or(4096) }
    }
}

impl ProcFs {
    /// A directory laid out like `/proc`, with this page size.
    pub fn rooted_at(root: impl Into<PathBuf>, page_size: u64) -> ProcFs {
        ProcFs { root: root.into(), page_size }
    }

    pub fn snapshot(&self) -> io::Result<Snapshot> {
        let (cpu, cores) = parse_cpu(&fs::read_to_string(self.root.join("stat"))?)?;
        let memory = parse_meminfo(&fs::read_to_string(self.root.join("meminfo"))?)?;
        let mut procs = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let Ok(entry) = entry else { continue };
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
            if let Some(proc) = self.read(pid, &entry.path()) {
                procs.push(proc);
            }
        }
        procs.sort_by_key(|p| p.pid);
        Ok(Snapshot { procs, cpu, cores, memory })
    }

    /// One process, or `None` if it ended while being read.
    fn read(&self, pid: i32, dir: &Path) -> Option<Proc> {
        let uid = fs::metadata(dir).ok()?.uid();
        let stat = parse_stat(&fs::read_to_string(dir.join("stat")).ok()?)?;
        let statm = fs::read_to_string(dir.join("statm")).ok()?;
        let private_pages = parse_statm_private(&statm)?;
        // Permission to read a command line is not given for every
        // process; a missing one only costs the name its extension.
        let argv = fs::read(dir.join("cmdline")).unwrap_or_default();
        let argv: Vec<String> = argv
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        Some(Proc {
            pid,
            start: stat.start,
            name: display_name(&stat.comm, argv.first().map(String::as_str)),
            command: argv.join(" "),
            state: stat.state,
            ppid: stat.ppid,
            ticks: stat.utime + stat.stime,
            threads: stat.threads,
            private_bytes: private_pages * self.page_size,
            uid,
            kernel: stat.flags & PF_KTHREAD != 0,
        })
    }
}

/// When `pid` started, in clock ticks since boot, or `None` if there is
/// no such process now.
pub fn start_time(pid: i32) -> Option<u64> {
    parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?).map(|s| s.start)
}

/// Whether a `/proc/<pid>/stat` line is a kernel thread's.
pub fn is_kernel_thread(stat: &str) -> bool {
    parse_stat(stat).is_some_and(|s| s.flags & PF_KTHREAD != 0)
}

/// What root can read about `id` that its owner's window may not: storage
/// I/O and how many files it has open. `None` when it ended, or the pid
/// now names another process.
pub fn usage(root: &Path, id: crate::sample::Id) -> Option<crate::helper::Usage> {
    let dir = root.join(id.pid.to_string());
    let stat = parse_stat(&fs::read_to_string(dir.join("stat")).ok()?)?;
    if stat.start != id.start {
        return None;
    }
    let io = fs::read_to_string(dir.join("io")).ok()?;
    let field = |key: &str| io.lines().find_map(|l| l.strip_prefix(key)?.trim().parse::<u64>().ok());
    let open_files = fs::read_dir(dir.join("fd")).map(|d| d.count() as u32).unwrap_or(0);
    Some(crate::helper::Usage {
        pid: id.pid,
        start: id.start,
        read_bytes: field("read_bytes:")?,
        write_bytes: field("write_bytes:")?,
        open_files,
    })
}

/// `include/linux/sched.h`.
const PF_KTHREAD: u64 = 0x0020_0000;

/// The fields of `/proc/<pid>/stat` this reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stat {
    comm: String,
    state: char,
    ppid: i32,
    flags: u64,
    utime: u64,
    stime: u64,
    threads: u32,
    start: u64,
}

/// `pid (comm) state ppid …`. The name is between the *first* `(` and the
/// *last* `)`, because a name may contain both (`(sd-pam)`, or anything a
/// program sets for itself) and spaces; everything after the last `)` is
/// space-separated numbers. proc(5) numbers the fields from 1; after the
/// name, state is field 3.
fn parse_stat(line: &str) -> Option<Stat> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    let comm = line.get(open + 1..close)?.to_owned();
    let rest: Vec<&str> = line.get(close + 1..)?.split_whitespace().collect();
    // rest[0] is field 3; field n is rest[n - 3].
    let field = |n: usize| rest.get(n - 3).copied();
    Some(Stat {
        comm,
        state: field(3)?.chars().next()?,
        ppid: field(4)?.parse().ok()?,
        flags: field(9)?.parse().ok()?,
        utime: field(14)?.parse().ok()?,
        stime: field(15)?.parse().ok()?,
        threads: field(20)?.parse().ok()?,
        start: field(22)?.parse().ok()?,
    })
}

/// `size resident shared text lib data dt`, in pages: resident minus
/// shared is the private set.
fn parse_statm_private(line: &str) -> Option<u64> {
    let mut fields = line.split_whitespace().map(|f| f.parse::<u64>().ok());
    let _size = fields.next()??;
    let resident = fields.next()??;
    let shared = fields.next()??;
    Some(resident.saturating_sub(shared))
}

/// What to call a process: its `comm`, unless that is a truncated copy of
/// the program's name.
///
/// The kernel keeps 15 bytes of a name, so `hyprforge-settings` is
/// `hyprforge-setti` there — the trap the suite's CLAUDE.md records for
/// `pgrep -x`, arriving as a label. When `comm` is exactly 15 bytes and
/// the program's own file name starts with it, the file name is the
/// whole of it. Otherwise `comm` is kept: a program that renamed itself
/// ("Isolated Web Co") said what it wanted to be called.
pub fn display_name(comm: &str, argv0: Option<&str>) -> String {
    const COMM_LEN: usize = 15;
    if comm.len() == COMM_LEN {
        if let Some(base) = argv0.map(|a| a.rsplit('/').next().unwrap_or(a)) {
            if base.len() > COMM_LEN && base.starts_with(comm) {
                return base.to_owned();
            }
        }
    }
    comm.to_owned()
}

/// The `cpu` and `cpuN` lines of `/proc/stat`.
fn parse_cpu(stat: &str) -> io::Result<(CpuTimes, Vec<CpuTimes>)> {
    let mut all = None;
    let mut cores = Vec::new();
    for line in stat.lines() {
        let mut words = line.split_whitespace();
        let Some(label) = words.next() else { continue };
        if !label.starts_with("cpu") {
            continue;
        }
        let n: Vec<u64> = words.map(|w| w.parse().unwrap_or(0)).collect();
        // user nice system idle iowait irq softirq steal [guest guest_nice]
        let get = |i: usize| n.get(i).copied().unwrap_or(0);
        let times = CpuTimes { total: (0..8).map(get).sum(), idle: get(3) + get(4) };
        if label == "cpu" {
            all = Some(times);
        } else {
            cores.push(times);
        }
    }
    let all = all.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "/proc/stat has no cpu line"))?;
    Ok((all, cores))
}

fn parse_meminfo(meminfo: &str) -> io::Result<Memory> {
    let kib = |key: &str| -> Option<u64> {
        meminfo.lines().find_map(|line| {
            let rest = line.strip_prefix(key)?.strip_prefix(':')?;
            rest.split_whitespace().next()?.parse::<u64>().ok().map(|k| k * 1024)
        })
    };
    let missing = |key: &str| io::Error::new(io::ErrorKind::InvalidData, format!("/proc/meminfo has no {key}"));
    Ok(Memory {
        total: kib("MemTotal").ok_or_else(|| missing("MemTotal"))?,
        available: kib("MemAvailable").ok_or_else(|| missing("MemAvailable"))?,
        swap_total: kib("SwapTotal").unwrap_or(0),
        swap_free: kib("SwapFree").unwrap_or(0),
    })
}

#[cfg(test)]
pub(crate) mod fixture {
    //! A stand-in `/proc`, from real files: the lines below were copied
    //! from this machine's `/proc` on 2026-10-10 (a zsh, systemd, a
    //! kernel thread), with only the pids changed where a test needs
    //! several.

    use std::fs;
    use std::path::Path;

    pub const STAT: &str = "cpu  3630361 900 570462 64201556 30835332 146674 93878 0 0 0\n\
cpu0 79669 2 35681 4471677 1616650 10241 22897 0 0 0\n\
cpu1 54370 0 12666 6088752 42603 1640 3512 0 0 0\n\
intr 1 2 3\nctxt 4\nbtime 1760000000\n";

    pub const MEMINFO: &str = "MemTotal:       63572428 kB\nMemFree:         3322068 kB\n\
MemAvailable:   58559836 kB\nBuffers:            1560 kB\nCached:         55472612 kB\n\
SwapTotal:       4194300 kB\nSwapFree:        2725568 kB\n";

    pub const ZSH_STAT: &str = "1173572 (zsh) S 33855 1173572 1173572 0 -1 4194304 362 0 0 0 0 0 0 0 20 0 1 0 6202074 9388032 1159 18446744073709551615 94124431028224 94124431801589 140736964814288 0 0 0 2 4 134283265 1 0 0 17 8 0 0 0 0 0 94124431902864 94124431932308 94124783058944 140736964823151 140736964823911 140736964823911 140736964829163 0\n";
    pub const ZSH_STATM: &str = "2292 1195 1022 189 0 218 0\n";

    /// A kernel thread: flags carry PF_KTHREAD, the command line is empty.
    pub const KTHREAD_STAT: &str = "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 10 0 0 20 0 1 0 16 0 0 18446744073709551615 0 0 0 0 0 0 0 2147483647 0 0 0 0 0 11 0 0 0 0 0 0 0 0 0 0 0 0 0\n";

    /// Writes one process directory.
    pub fn process(root: &Path, pid: i32, stat: &str, statm: &str, cmdline: &[&str]) {
        let dir = root.join(pid.to_string());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stat"), stat).unwrap();
        fs::write(dir.join("statm"), statm).unwrap();
        let mut argv = Vec::new();
        for a in cmdline {
            argv.extend_from_slice(a.as_bytes());
            argv.push(0);
        }
        fs::write(dir.join("cmdline"), argv).unwrap();
    }

    pub fn root(dir: &Path) {
        fs::write(dir.join("stat"), STAT).unwrap();
        fs::write(dir.join("meminfo"), MEMINFO).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    #[test]
    fn a_real_stat_line_reads_as_what_proc5_says_it_holds() {
        let s = parse_stat(ZSH_STAT).unwrap();
        assert_eq!(s.comm, "zsh");
        assert_eq!(s.state, 'S');
        assert_eq!(s.ppid, 33855);
        assert_eq!((s.utime, s.stime, s.threads, s.start), (0, 0, 1, 6202074));
        assert_eq!(s.flags & PF_KTHREAD, 0);
    }

    /// The name is between the first `(` and the last `)`: a name may
    /// hold either, and spaces, and the fields after it must not shift.
    #[test]
    fn a_name_with_parentheses_and_spaces_does_not_shift_the_fields() {
        let line = ZSH_STAT.replace("(zsh)", "(we (ird) name)");
        let s = parse_stat(&line).unwrap();
        assert_eq!(s.comm, "we (ird) name");
        assert_eq!((s.ppid, s.start), (33855, 6202074));
    }

    #[test]
    fn private_memory_is_resident_less_shared() {
        assert_eq!(parse_statm_private(ZSH_STATM), Some(1195 - 1022));
    }

    #[test]
    fn a_truncated_comm_takes_the_programs_whole_name() {
        assert_eq!(display_name("hyprforge-setti", Some("/usr/bin/hyprforge-settings")), "hyprforge-settings");
        // A program that named itself keeps that name.
        assert_eq!(display_name("Isolated Web Co", Some("/usr/lib/firefox/firefox")), "Isolated Web Co");
        // Short names are whole already.
        assert_eq!(display_name("zsh", Some("-zsh")), "zsh");
        assert_eq!(display_name("hyprforge-setti", None), "hyprforge-setti");
    }

    #[test]
    fn cpu_lines_add_every_state_but_guest_and_count_iowait_as_idle() {
        let (all, cores) = parse_cpu(STAT).unwrap();
        assert_eq!(all.total, 3630361 + 900 + 570462 + 64201556 + 30835332 + 146674 + 93878);
        assert_eq!(all.idle, 64201556 + 30835332);
        assert_eq!(cores.len(), 2);
    }

    #[test]
    fn memory_used_is_total_less_available() {
        let m = parse_meminfo(MEMINFO).unwrap();
        assert_eq!(m.used(), (63572428 - 58559836) * 1024);
        assert_eq!(m.swap_used(), (4194300 - 2725568) * 1024);
    }

    #[test]
    fn a_snapshot_reads_every_process_and_marks_kernel_threads() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path());
        process(dir.path(), 1173572, ZSH_STAT, ZSH_STATM, &["-zsh"]);
        process(dir.path(), 2, KTHREAD_STAT, "0 0 0 0 0 0 0\n", &[]);
        std::fs::create_dir(dir.path().join("self")).unwrap();
        let snap = ProcFs::rooted_at(dir.path(), 4096).snapshot().unwrap();
        assert_eq!(snap.procs.len(), 2, "not `self`, not the files");
        let zsh = snap.procs.iter().find(|p| p.pid == 1173572).unwrap();
        assert_eq!(zsh.private_bytes, (1195 - 1022) * 4096);
        assert_eq!(zsh.command, "-zsh");
        assert!(!zsh.kernel);
        assert!(snap.procs.iter().find(|p| p.pid == 2).unwrap().kernel);
    }

    /// A process that ends between the listing and the read is left out,
    /// not an error.
    #[test]
    fn a_process_that_ended_mid_read_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path());
        std::fs::create_dir(dir.path().join("4242")).unwrap();
        let snap = ProcFs::rooted_at(dir.path(), 4096).snapshot().unwrap();
        assert!(snap.procs.is_empty());
    }

    /// And the real one: this test process is in it, as itself.
    #[test]
    fn the_real_proc_has_this_test_in_it() {
        let snap = ProcFs::default().snapshot().unwrap();
        let me = snap.procs.iter().find(|p| p.pid == std::process::id() as i32).expect("this process");
        assert!(me.threads >= 1);
        assert!(me.private_bytes > 0);
        assert!(!snap.cores.is_empty());
        assert!(snap.memory.total > 0);
    }
}

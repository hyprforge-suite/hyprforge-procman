//! Where the TUI's moments come from: this machine now, read exactly as
//! the recorder reads it, or the recorder's journal, replayed.
//!
//! Either way a screen is the interval between two frames
//! (`moment::between`), so live and replay can never disagree about what
//! a figure means.

use crate::journal::{self, Frame, Reader};
use crate::moment::{self, Moment};
use crate::recorder::{Config, Recorder};
use crate::users::Users;
use std::io;
use std::path::{Path, PathBuf};

/// Everything one screen draws.
#[derive(Debug, Clone, Default)]
pub struct Screen {
    pub moment: Moment,
    /// The two frames the moment lies between — for what is a level
    /// rather than a rate (memory, pressure) and for the cgroup view.
    pub prev: Frame,
    pub now: Frame,
    pub cpus: u32,
    pub hostname: String,
    /// Read as an ordinary user: other users' processes are missing
    /// their I/O, their delays and their command lines' secrets.
    pub partial: bool,
    /// The first screen of a live run covers everything since boot,
    /// atop's way; it says so rather than pass off a lifetime as ten
    /// seconds.
    pub since_boot: bool,
    /// Replay: which sample, of how many.
    pub position: Option<(usize, usize)>,
}

/// A frame standing in for "at boot": every counter zero, the same boot,
/// at the moment the machine started — so the first interval is the
/// machine's whole life so far.
fn at_boot(now: &Frame) -> Frame {
    Frame { at_ms: now.snapshot.boot as i64 * 1000, boot_id: now.boot_id.clone(), ..Frame::default() }
}

fn cpus() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_owned()).unwrap_or_default()
}

pub struct Live {
    recorder: Recorder,
    prev: Option<Frame>,
    users: Users,
    tps: u64,
}

impl Live {
    pub fn new() -> Live {
        // The recorder's own reading, minus its journal: threads counted,
        // PSS left out (a page-table walk per process, per refresh).
        let config = Config { threads: true, pss: false, ..Config::default() };
        Live { recorder: Recorder::new(config), prev: None, users: Users::default(), tps: crate::proc::ticks_per_second() }
    }

    pub fn screen(&mut self) -> io::Result<Screen> {
        let now = self.recorder.collect(chrono::Local::now(), Vec::new())?;
        let since_boot = self.prev.is_none();
        let prev = self.prev.take().unwrap_or_else(|| at_boot(&now));
        let moment = moment::between(&prev, &now, self.tps, &mut self.users);
        self.prev = Some(now.clone());
        // SAFETY: geteuid cannot fail.
        let partial = unsafe { libc::geteuid() } != 0;
        Ok(Screen { moment, prev, now, cpus: cpus(), hostname: hostname(), partial, since_boot, position: None })
    }
}

impl Default for Live {
    fn default() -> Live {
        Live::new()
    }
}

pub struct Replay {
    reader: Reader,
    pub index: usize,
    users: Users,
    /// The last two frames decoded, by index, so stepping forward one
    /// decodes one frame rather than two.
    cache: Vec<(usize, Frame)>,
}

/// The journal for `what`: a file path, or a day (`2026-10-10`) in the
/// recorder's directory. A day the caller may not read is asked of the
/// helper, which checks polkit and hands back an open file.
pub fn open_journal(what: &str, directory: &Path) -> io::Result<Reader> {
    let path = if journal::is_day(what) { directory.join(format!("{what}.hfr")) } else { PathBuf::from(what) };
    match Reader::open(&path) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && journal::is_day(what) => through_helper(what),
        other => other,
    }
}

/// `OpenHistory` on the system bus. polkit asks for a password in
/// whatever agent this session has — a text one in a TTY.
fn through_helper(day: &str) -> io::Result<Reader> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let fd = runtime
        .block_on(async {
            let bus = zbus::Connection::system().await?;
            let proxy = crate::helper::ProcManProxy::new(&bus).await?;
            proxy.open_history(day, true).await
        })
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, format!("the journal is root's, and the helper wouldn't open it: {e}")))?;
    Reader::from_file(std::fs::File::from(std::os::fd::OwnedFd::from(fd)))
}

impl Replay {
    pub fn new(reader: Reader) -> io::Result<Replay> {
        if reader.entries.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the journal has no samples yet"));
        }
        Ok(Replay { reader, index: 0, users: Users::default(), cache: Vec::new() })
    }

    pub fn len(&self) -> usize {
        self.reader.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reader.entries.is_empty()
    }

    fn frame(&mut self, i: usize) -> io::Result<Frame> {
        if let Some((_, f)) = self.cache.iter().find(|(k, _)| *k == i) {
            return Ok(f.clone());
        }
        let f = self.reader.frame(i)?;
        self.cache.push((i, f.clone()));
        if self.cache.len() > 2 {
            self.cache.remove(0);
        }
        Ok(f)
    }

    /// Moves by `by` samples, stopping at either end.
    pub fn step(&mut self, by: i64) {
        self.index = (self.index as i64 + by).clamp(0, self.len() as i64 - 1) as usize;
    }

    /// The first sample at or after `at_ms` — atop's `-b`. Past the end
    /// is the last sample.
    pub fn go_to(&mut self, at_ms: i64) {
        let i = self.reader.entries.partition_point(|e| e.at_ms < at_ms);
        self.index = i.min(self.len() - 1);
    }

    /// `hour:minute` on the journal's own day.
    pub fn go_to_time(&mut self, hour: u32, minute: u32) {
        if let Some(at) = crate::report::at_time(&self.reader, hour, minute) {
            self.go_to(at);
        }
    }

    /// The interval ending at the current sample. The first sample of a
    /// file (or of a boot) covers everything since boot.
    pub fn screen(&mut self) -> io::Result<Screen> {
        let now = self.frame(self.index)?;
        let prev = match self.index.checked_sub(1) {
            Some(i) => self.frame(i)?,
            None => at_boot(&now),
        };
        let since_boot = self.index == 0 || prev.boot_id != now.boot_id;
        let prev = if prev.boot_id != now.boot_id { at_boot(&now) } else { prev };
        let moment = moment::between(&prev, &now, self.reader.header.ticks_per_second, &mut self.users);
        Ok(Screen {
            moment,
            prev,
            now,
            cpus: self.reader.header.cpus,
            hostname: self.reader.header.hostname.clone(),
            partial: false,
            since_boot,
            position: Some((self.index, self.len())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Header, Writer};
    use crate::proc::{CpuTimes, Proc, Snapshot};

    fn frame(at_ms: i64, total: u64, ticks: u64) -> Frame {
        let p = Proc { pid: 7, start: 3, name: "blender".into(), state: 'R', threads: 1, ticks, ..Proc::default() };
        Frame { at_ms, boot_id: "b".into(), snapshot: Snapshot { procs: vec![p], cpu: CpuTimes { total, ..CpuTimes::default() }, boot: 1, ..Snapshot::default() }, ..Frame::default() }
    }

    fn journal(dir: &Path) -> PathBuf {
        let path = dir.join("2026-10-10.hfr");
        let header = Header { hostname: "hogwarts".into(), cpus: 4, page_size: 4096, ticks_per_second: 100, interval_seconds: 10, command_lines: true };
        let mut w = Writer::open(&path, &header).unwrap();
        for i in 0..5 {
            w.write(&frame(1_000_000 + i * 10_000, 1000 * (i as u64 + 1), 100 * (i as u64 + 1))).unwrap();
        }
        path
    }

    #[test]
    fn a_replay_steps_between_samples_and_stops_at_the_ends() {
        let t = tempfile::tempdir().unwrap();
        let mut r = Replay::new(open_journal(journal(t.path()).to_str().unwrap(), t.path()).unwrap()).unwrap();
        let first = r.screen().unwrap();
        assert!(first.since_boot, "the first sample has nothing before it");
        r.step(2);
        let s = r.screen().unwrap();
        assert!(!s.since_boot);
        assert_eq!(s.moment.rows[0].cpu_pct, 10.0, "100 ticks of 1000");
        assert_eq!((s.position, s.hostname.as_str()), (Some((2, 5)), "hogwarts"));
        r.step(-10);
        assert_eq!(r.index, 0);
        r.step(10);
        assert_eq!(r.index, 4);
    }

    #[test]
    fn going_to_a_time_lands_on_the_first_sample_at_or_after_it() {
        let t = tempfile::tempdir().unwrap();
        journal(t.path());
        // By day, in the recorder's directory — the way `-r` names one.
        let mut r = Replay::new(open_journal("2026-10-10", t.path()).unwrap()).unwrap();
        r.go_to(1_015_000);
        assert_eq!(r.index, 2);
        r.go_to(0);
        assert_eq!(r.index, 0);
        r.go_to(i64::MAX);
        assert_eq!(r.index, 4);
    }

    #[test]
    fn a_live_screen_starts_from_boot_and_then_covers_one_interval() {
        let mut live = Live::new();
        let first = live.screen().unwrap();
        assert!(first.since_boot && first.moment.system.seconds > 1.0);
        let second = live.screen().unwrap();
        assert!(!second.since_boot && second.moment.system.seconds < 60.0);
        assert!(second.moment.rows.iter().any(|r| r.id.pid == std::process::id() as i32), "it sees itself");
    }
}

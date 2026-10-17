//! The recorder: one sample of everything, written to the day's journal.
//!
//! The binary (`src/bin/recorder.rs`) is the clock, the signal handling
//! and the exits listener; everything it does each tick is here, so it
//! can be tested — and its cost measured — without running as a service.

use crate::exits::Exit;
use crate::journal::{self, ExitsHeard, Frame, Header, Writer};
use crate::machine::SysFs;
use crate::proc::ProcFs;
use serde::Deserialize;
use std::io;
use std::path::{Path, PathBuf};

/// `/etc/hyprforge/procman-recorder.toml`. Every key may be left out.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Seconds between samples.
    pub interval: u32,
    pub keep_days: usize,
    /// The journal's whole size, at most.
    pub max_bytes: u64,
    /// Keep each process's command line. They can hold secrets (a token
    /// passed as an argument); the journal is root's alone either way.
    pub record_cmdlines: bool,
    /// Count each process's threads by state (a file per thread).
    pub threads: bool,
    /// Read each process's proportional set size (a page-table walk).
    pub pss: bool,
    /// Switch on the kernel's delay accounting while recording, so block
    /// I/O delay per process is measured; the previous setting is put
    /// back when the recorder stops.
    pub delay_accounting: bool,
    /// Count each process's network bytes with the eBPF counter (built
    /// with `netbpf`, and only as root).
    pub network: bool,
    pub directory: PathBuf,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            interval: 10,
            keep_days: 30,
            max_bytes: 4 << 30,
            record_cmdlines: true,
            threads: true,
            pss: false,
            delay_accounting: true,
            network: true,
            directory: PathBuf::from("/var/log/hyprforge/procman"),
        }
    }
}

pub const CONFIG_PATH: &str = "/etc/hyprforge/procman-recorder.toml";

/// A missing file is the defaults; one that will not parse is an error,
/// never quietly the defaults — the suite's rule.
pub fn load_config(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let config: Config = toml::from_str(&text).map_err(|e| format!("{} couldn't be read: {e}", path.display()))?;
            if config.interval == 0 {
                return Err(format!("{}: interval must be at least 1 second", path.display()));
            }
            Ok(config)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{} couldn't be read: {e}", path.display())),
    }
}

/// Everything one tick needs, kept between ticks.
pub struct Recorder {
    pub config: Config,
    fs: ProcFs,
    sys: SysFs,
    header: Header,
    writer: Option<(PathBuf, Writer)>,
    ticks: u64,
    #[cfg(feature = "netbpf")]
    net: Option<crate::netbpf::Counter>,
    /// Why processes' network isn't being counted, when it isn't — for
    /// the caller to say once.
    pub net_missing: Option<String>,
}

fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|s| s.trim().to_owned()).unwrap_or_default()
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_owned()).unwrap_or_default()
}

/// The eBPF network counter, if this build has one, the config wants it,
/// and this process may load it — or why not.
#[cfg(feature = "netbpf")]
fn network_counter(config: &Config) -> (Option<crate::netbpf::Counter>, Option<String>) {
    if !config.network {
        return (None, None);
    }
    // SAFETY: geteuid cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return (None, Some("counting network per process needs root".into()));
    }
    match crate::netbpf::Counter::start() {
        Ok(c) => (Some(c), None),
        Err(e) => (None, Some(e)),
    }
}

#[cfg(not(feature = "netbpf"))]
fn network_counter(config: &Config) -> (Option<()>, Option<String>) {
    (None, config.network.then(|| "this build counts no network per process (built without netbpf)".into()))
}

impl Recorder {
    pub fn new(config: Config) -> Recorder {
        let mut fs = ProcFs::default();
        fs.verbose = true;
        // Run delay is every thread's, which only a read per thread gives
        // — and `hyprforge-top`'s live mode runs through here too.
        fs.thread_schedstat = true;
        fs.threads = config.threads;
        fs.pss = config.pss;
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let header = Header {
            hostname: hostname(),
            cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
            page_size: u64::try_from(page).unwrap_or(4096),
            ticks_per_second: crate::proc::ticks_per_second(),
            interval_seconds: config.interval,
            command_lines: config.record_cmdlines,
        };
        let (net, net_missing) = network_counter(&config);
        #[cfg(not(feature = "netbpf"))]
        let _ = net;
        Recorder {
            config,
            fs,
            sys: SysFs::default(),
            header,
            writer: None,
            ticks: 0,
            #[cfg(feature = "netbpf")]
            net,
            net_missing,
        }
    }

    /// One sample at `now`, with the exits `heard` gives — asked only
    /// once `/proc` has been read (see [`Recorder::collect`]) — written to
    /// `now`'s day file. A new day opens a new file and prunes the old
    /// ones.
    pub fn tick(&mut self, now: chrono::DateTime<chrono::Local>, exits: impl FnOnce() -> (Vec<Exit>, ExitsHeard)) -> io::Result<()> {
        let frame = self.collect(now, exits)?;
        let path = journal::day_file(&self.config.directory, now);
        if self.writer.as_ref().is_none_or(|(p, _)| *p != path) {
            self.writer = Some((path.clone(), Writer::open(&path, &self.header)?));
            if let Err(e) = journal::prune(&self.config.directory, self.config.keep_days, self.config.max_bytes, &path) {
                tracing::warn!("couldn't prune old journals: {e}");
            }
        }
        let (_, writer) = self.writer.as_mut().expect("opened above");
        writer.write(&frame)
    }

    /// One sample, not written — what the TUI's live view shows, read
    /// exactly as the recorder reads it.
    ///
    /// The exits are taken *after* `/proc` is read, never before. Taken
    /// first, a process that ended between the two was in neither — not
    /// read, not yet heard — and its exit, heard next time, had no earlier
    /// reading to be measured from, so its whole life landed in one
    /// interval. Taken after, every process that ends was read alive in
    /// some frame first; one that ends between the read and the take is
    /// in both, and `moment::between` lets the exit stand for it.
    pub fn collect(&mut self, now: chrono::DateTime<chrono::Local>, exits: impl FnOnce() -> (Vec<Exit>, ExitsHeard)) -> io::Result<Frame> {
        let mut snapshot = self.fs.snapshot()?;
        let (exits, exits_heard) = exits();
        #[cfg(feature = "netbpf")]
        if let Some(counter) = &mut self.net {
            counter.fill(&mut snapshot.procs);
        }
        if !self.config.record_cmdlines {
            for p in &mut snapshot.procs {
                p.command.clear();
            }
        }
        // Drive temperatures once a minute: each costs a command to the
        // drive (see `SysFs::drive_temperatures`).
        self.sys.drive_temperatures = self.ticks.is_multiple_of((60 / u64::from(self.config.interval.clamp(1, 60))).max(1));
        self.ticks += 1;
        Ok(Frame {
            at_ms: now.timestamp_millis(),
            boot_id: boot_id(),
            snapshot,
            machine: self.sys.read(),
            cgroups: crate::cgroups::all(Path::new("/sys/fs/cgroup"), 6),
            exits,
            // Read every frame, not once: anyone may switch it while the
            // recorder runs, and the recorder's own switch can fail.
            delay_accounting: delay_accounting(),
            exits_heard,
        })
    }
}

const KNOB: &str = "/proc/sys/kernel/task_delayacct";

/// Whether delay accounting is on now; `None` when that can't be read.
pub fn delay_accounting() -> Option<bool> {
    std::fs::read_to_string(KNOB).ok().map(|s| s.trim() == "1")
}

/// Delay accounting: what it was, so it can be put back.
pub fn set_delay_accounting(on: bool) -> io::Result<bool> {
    let was = std::fs::read_to_string(KNOB)?.trim() == "1";
    std::fs::write(KNOB, if on { "1" } else { "0" })?;
    Ok(was)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_config_is_the_defaults_and_a_broken_one_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("procman-recorder.toml");
        assert_eq!(load_config(&p).unwrap(), Config::default());
        std::fs::write(&p, "interval = 30\nrecord_cmdlines = false\n").unwrap();
        let c = load_config(&p).unwrap();
        assert_eq!((c.interval, c.record_cmdlines, c.keep_days), (30, false, 30));
        std::fs::write(&p, "intervl = 30\n").unwrap();
        assert!(load_config(&p).is_err(), "a misspelt key is said, not ignored");
        std::fs::write(&p, "interval = 0\n").unwrap();
        assert!(load_config(&p).is_err());
    }

    /// The example config shipped in packaging/ says the defaults, so
    /// it reads back as exactly them — a default changed in one place
    /// and not the other fails here.
    #[test]
    fn the_shipped_example_is_the_defaults() {
        let shipped: Config = toml::from_str(include_str!("../packaging/procman-recorder.toml")).unwrap();
        assert_eq!(shipped, Config::default());
    }

    #[test]
    fn ticks_write_to_the_days_file_and_command_lines_can_be_left_out() {
        let t = tempfile::tempdir().unwrap();
        let config = Config { directory: t.path().to_path_buf(), record_cmdlines: false, threads: false, ..Config::default() };
        let mut r = Recorder::new(config);
        let now = chrono::Local::now();
        r.tick(now, || (Vec::new(), ExitsHeard::NotListening)).unwrap();
        r.tick(now + chrono::Duration::seconds(10), || (vec![Exit { pid: 7, comm: "sleep".into(), ..Exit::default() }], ExitsHeard::Dropped)).unwrap();
        let mut reader = journal::Reader::open(&journal::day_file(t.path(), now)).unwrap();
        assert_eq!(reader.entries.len(), 2);
        let f = reader.frame(1).unwrap();
        assert_eq!(f.exits[0].comm, "sleep");
        assert_eq!(f.exits_heard, ExitsHeard::Dropped, "the frame says its exits are incomplete");
        assert_eq!(f.delay_accounting, delay_accounting());
        assert!(f.snapshot.procs.iter().all(|p| p.command.is_empty()), "no command lines kept");
        assert!(!f.boot_id.is_empty() && !f.cgroups.is_empty());
        assert!(!reader.header.command_lines);
    }

    /// What a tick costs, here, with every process's detail and threads.
    /// The plan's budget is half a percent of one core at ten seconds:
    /// 50 ms of CPU per tick.
    #[test]
    #[ignore = "measures this machine"]
    fn a_tick_costs_less_than_half_a_percent_of_a_core() {
        // The budget is for the shipped binary. An unoptimised build costs
        // half again as much (61 ms against 40 ms, measured here), so
        // a debug run of it (`tools/hf test manual`) would fail a recorder
        // that is within it.
        if cfg!(debug_assertions) {
            eprintln!("HYPRFORGE-SKIP: a tick's cost is measured only in an optimised build (cargo test --release)");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let mut r = Recorder::new(Config { directory: t.path().to_path_buf(), ..Config::default() });
        let cpu = || {
            // SAFETY: a zeroed rusage is a valid out-parameter.
            let mut u: libc::rusage = unsafe { std::mem::zeroed() };
            unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
            let us = |t: libc::timeval| t.tv_sec as f64 * 1e6 + t.tv_usec as f64;
            us(u.ru_utime) + us(u.ru_stime)
        };
        r.tick(chrono::Local::now(), || (Vec::new(), ExitsHeard::NotListening)).unwrap();
        let before = cpu();
        let n = 10;
        for i in 0..n {
            r.tick(chrono::Local::now() + chrono::Duration::seconds(i + 1), || (Vec::new(), ExitsHeard::NotListening)).unwrap();
        }
        let per_tick_ms = (cpu() - before) / n as f64 / 1000.0;
        eprintln!("a tick: {per_tick_ms:.1} ms of CPU ({:.2} % of a core at 10 s)", per_tick_ms / 100.0);
        assert!(per_tick_ms < 50.0, "{per_tick_ms:.1} ms");
    }
}

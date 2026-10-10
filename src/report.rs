//! atopsar's half of atop: what a stretch of the journal adds up to.
//!
//! The journal is read one sample at a time and every interval is made by
//! `moment::between` — the same figures `hyprforge-top` draws, so a report
//! and the screen it summarises cannot disagree — then folded into buckets
//! of `step`. Plain data, and serialisable, so `--json` carries exactly the
//! numbers the text shows.
//!
//! CPU figures are atop's: 100 per CPU, so a busy 16-core machine reads
//! 1600%. An interval that crosses a reboot is left out rather than
//! counted from boot: the gap is not ten seconds of anything.

use crate::journal::{Frame, Reader};
use crate::moment::{self, Moment};
use crate::users::Users;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cpu,
    Mem,
    Disk,
    Net,
    Psi,
    Top,
}

impl Kind {
    pub const ALL: [(&'static str, Kind); 6] = [("cpu", Kind::Cpu), ("mem", Kind::Mem), ("disk", Kind::Disk), ("net", Kind::Net), ("psi", Kind::Psi), ("top", Kind::Top)];

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.iter().find(|(n, _)| *n == s).map(|(_, k)| *k)
    }

    pub fn name(self) -> &'static str {
        Kind::ALL.iter().find(|(_, k)| *k == self).map_or("", |(n, _)| n)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub from_ms: i64,
    pub to_ms: i64,
    pub step_ms: i64,
    /// How many programs `top` lists.
    pub top: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuLine {
    pub at_ms: i64,
    pub seconds: f64,
    pub sys: f32,
    pub user: f32,
    pub irq: f32,
    pub idle: f32,
    pub wait: f32,
    pub steal: f32,
    /// The one-minute load average at the bucket's last sample.
    pub load1: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemLine {
    pub at_ms: i64,
    pub total: u64,
    /// At the bucket's last sample, and the least it was in the bucket —
    /// the second is the one that says how close it came.
    pub available: u64,
    pub least_available: u64,
    pub cached: u64,
    pub dirty: u64,
    pub swap_used: u64,
    pub committed: u64,
    /// Pages, over the whole bucket.
    pub scanned: u64,
    pub swap_in: u64,
    pub swap_out: u64,
    pub oom_kills: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiskLine {
    pub at_ms: i64,
    pub name: String,
    pub busy_pct: f32,
    pub reads: u64,
    pub writes: u64,
    pub read_bytes: u64,
    pub written_bytes: u64,
    /// Per second, over the bucket.
    pub read_per_second: f64,
    pub write_per_second: f64,
    /// What one request took, on average, in milliseconds.
    pub avio_ms: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetLine {
    pub at_ms: i64,
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_per_second: f64,
    pub tx_per_second: f64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub errors: u64,
    pub drops: u64,
}

/// The kernel's ten-second pressure, averaged over the bucket's samples
/// and at its worst.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Pressure {
    pub mean: f32,
    pub max: f32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PsiLine {
    pub at_ms: i64,
    pub cpu: Pressure,
    pub memory: Pressure,
    pub memory_full: Pressure,
    pub io: Pressure,
    pub io_full: Pressure,
}

/// One program over the whole range: every process of that name added up.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Consumer {
    pub name: String,
    pub processes: usize,
    /// CPU time used in the range, in seconds — one busy core for a
    /// minute is 60.
    pub cpu_seconds: f64,
    /// The most resident memory any one sample saw, all its processes
    /// together.
    pub peak_resident: u64,
    pub read: u64,
    pub written: u64,
    /// Network bytes, where something was counting them (`netbpf`).
    pub received: Option<u64>,
    pub sent: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub hostname: String,
    pub cpus: u32,
    /// What the report covers: from the start of its first interval to
    /// the end of its last — the recorded stretch, not the range asked
    /// for, which may run past either end of the day. With nothing
    /// recorded, the range asked for.
    pub from_ms: i64,
    pub to_ms: i64,
    pub step_seconds: f64,
    /// How many intervals were read. Zero means the range had no samples,
    /// which the caller says rather than print an empty table.
    pub intervals: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cpu: Vec<CpuLine>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mem: Vec<MemLine>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disk: Vec<DiskLine>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<NetLine>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub psi: Vec<PsiLine>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top: Vec<Consumer>,
}

impl Report {
    /// Only `kind`'s table kept — what `--json` prints.
    pub fn only(mut self, kind: Kind) -> Report {
        if kind != Kind::Cpu {
            self.cpu.clear();
        }
        if kind != Kind::Mem {
            self.mem.clear();
        }
        if kind != Kind::Disk {
            self.disk.clear();
        }
        if kind != Kind::Net {
            self.net.clear();
        }
        if kind != Kind::Psi {
            self.psi.clear();
        }
        if kind != Kind::Top {
            self.top.clear();
        }
        self
    }
}

#[derive(Default)]
struct PressureSum {
    sum: f32,
    max: f32,
}

impl PressureSum {
    fn add(&mut self, v: f32) {
        self.sum += v;
        self.max = self.max.max(v);
    }

    fn get(&self, n: usize) -> Pressure {
        Pressure { mean: self.sum / n.max(1) as f32, max: self.max }
    }
}

#[derive(Default)]
struct DiskSum {
    busy_seconds: f64,
    reads: u64,
    writes: u64,
    read_bytes: u64,
    written_bytes: u64,
    /// Request time, summed as avio × requests, for a mean per request.
    io_ms: f64,
    timed: u64,
}

#[derive(Default)]
struct Bucket {
    seconds: f64,
    /// Each CPU state × seconds, for a time-weighted mean.
    cpu: [f64; 8],
    load1: Option<f32>,
    mem: MemLine,
    disks: BTreeMap<String, DiskSum>,
    nets: BTreeMap<String, NetLine>,
    samples: usize,
    /// Samples that had pressure to read — a kernel without PSI has none.
    psi_samples: usize,
    psi: [PressureSum; 5],
}

#[derive(Default)]
struct ProgramSum {
    ids: HashSet<crate::sample::Id>,
    cpu_seconds: f64,
    peak_resident: u64,
    read: u64,
    written: u64,
    received: Option<u64>,
    sent: Option<u64>,
}

/// Adds a figure that may not have been measured: unmeasured stays
/// unmeasured until something is.
fn add_opt(sum: &mut Option<u64>, v: Option<u64>) {
    if let Some(v) = v {
        *sum = Some(sum.unwrap_or(0) + v);
    }
}

/// Folds one interval into its bucket and the range's programs.
fn add(bucket: &mut Bucket, programs: &mut BTreeMap<String, ProgramSum>, m: &Moment, now: &Frame, cpus: u32) {
    let s = &m.system;
    bucket.seconds += s.seconds;
    for (sum, v) in bucket.cpu.iter_mut().zip(s.cpu) {
        *sum += f64::from(v) * s.seconds;
    }
    bucket.load1 = s.load.map(|l| l[0]).or(bucket.load1);

    let mem = &now.snapshot.memory;
    let md = &now.machine.memory;
    let least = if bucket.samples == 0 { mem.available } else { bucket.mem.least_available.min(mem.available) };
    bucket.mem = MemLine {
        at_ms: 0,
        total: mem.total,
        available: mem.available,
        least_available: least,
        cached: md.cached,
        dirty: md.dirty,
        swap_used: mem.swap_used(),
        committed: md.committed,
        scanned: bucket.mem.scanned + s.scanned,
        swap_in: bucket.mem.swap_in + s.swap_in,
        swap_out: bucket.mem.swap_out + s.swap_out,
        oom_kills: bucket.mem.oom_kills + s.oom_kills,
    };

    for d in &s.disks {
        let sum = bucket.disks.entry(d.name.clone()).or_default();
        sum.busy_seconds += f64::from(d.busy_pct) / 100.0 * s.seconds;
        sum.reads += d.reads;
        sum.writes += d.writes;
        sum.read_bytes += d.read_bytes;
        sum.written_bytes += d.written_bytes;
        if let Some(avio) = d.avio_ms {
            sum.io_ms += avio * (d.reads + d.writes) as f64;
            sum.timed += d.reads + d.writes;
        }
    }
    for n in &s.nets {
        let sum = bucket.nets.entry(n.name.clone()).or_default();
        sum.rx_bytes += n.rx_bytes;
        sum.tx_bytes += n.tx_bytes;
        sum.rx_packets += n.rx_packets;
        sum.tx_packets += n.tx_packets;
        sum.errors += n.errors;
        sum.drops += n.drops;
    }
    if let Some(p) = &now.machine.pressure_detail {
        for (sum, w) in bucket.psi.iter_mut().zip([p.cpu_some, p.memory_some, p.memory_full, p.io_some, p.io_full]) {
            sum.add(w.avg10);
        }
        bucket.psi_samples += 1;
    }
    bucket.samples += 1;

    // Programs: CPU as time, memory at its highest sample.
    let mut resident: BTreeMap<&str, u64> = BTreeMap::new();
    for r in &m.rows {
        let p = programs.entry(r.name.clone()).or_default();
        p.ids.insert(r.id);
        p.cpu_seconds += f64::from(r.cpu_pct) / 100.0 * s.seconds * f64::from(cpus);
        p.read += r.read.unwrap_or(0);
        p.written += r.written.unwrap_or(0);
        add_opt(&mut p.received, r.net_rx);
        add_opt(&mut p.sent, r.net_tx);
        // An exited row's resident figure is its peak, not memory still
        // in use alongside the others.
        if r.exit.is_none() {
            *resident.entry(&r.name).or_default() += r.resident_bytes;
        }
    }
    for (name, bytes) in resident {
        if let Some(p) = programs.get_mut(name) {
            p.peak_resident = p.peak_resident.max(bytes);
        }
    }
}

/// A time of day on the day the journal begins, local time — what
/// `--from`, `--to` and `-b` name.
pub fn at_time(reader: &Reader, time: chrono::NaiveTime) -> Option<i64> {
    use chrono::TimeZone;
    let first = reader.entries.first()?.at_ms;
    let day = chrono::Local.timestamp_millis_opt(first).single()?.date_naive();
    Some(chrono::Local.from_local_datetime(&day.and_time(time)).earliest()?.timestamp_millis())
}

/// Reads `reader` over the range and adds it up.
pub fn build(reader: &mut Reader, options: &Options) -> io::Result<Report> {
    let step = options.step_ms.max(1);
    let cpus = reader.header.cpus.max(1);
    let tps = reader.header.ticks_per_second;
    let mut users = Users::default();
    let mut buckets: BTreeMap<i64, Bucket> = BTreeMap::new();
    let mut programs: BTreeMap<String, ProgramSum> = BTreeMap::new();
    let mut intervals = 0;

    // The first interval inside the range ends at the first sample after
    // `from`; it starts at the sample before, wherever that was.
    let first = reader.entries.partition_point(|e| e.at_ms <= options.from_ms).max(1);
    let mut prev: Option<Frame> = None;
    let mut covered: Option<(i64, i64)> = None;
    for i in first - 1..reader.entries.len() {
        if reader.entries[i].at_ms > options.to_ms {
            break;
        }
        let now = reader.frame(i)?;
        if let Some(prev) = prev.as_ref().filter(|p| p.boot_id == now.boot_id) {
            let m = moment::between(prev, &now, tps, &mut users);
            // A bucket holds the intervals that end in it, and is named
            // for its end — atopsar's way of labelling a line.
            let end = (now.at_ms - 1).div_euclid(step) * step + step;
            add(buckets.entry(end).or_default(), &mut programs, &m, &now, cpus);
            intervals += 1;
            covered = Some((covered.map_or(prev.at_ms, |c| c.0), now.at_ms));
        }
        prev = Some(now);
    }

    let (from_ms, to_ms) = covered.unwrap_or((options.from_ms, options.to_ms));
    let mut report = Report { hostname: reader.header.hostname.clone(), cpus, from_ms, to_ms, step_seconds: step as f64 / 1000.0, intervals, ..Report::default() };
    let n = f64::from(cpus);
    for (&at_ms, b) in &buckets {
        let secs = b.seconds.max(0.001);
        let c = |i: usize| (b.cpu[i] / secs * n) as f32;
        report.cpu.push(CpuLine { at_ms, seconds: b.seconds, sys: c(2), user: c(0) + c(1), irq: c(5) + c(6), idle: c(3), wait: c(4), steal: c(7), load1: b.load1 });
        report.mem.push(MemLine { at_ms, ..b.mem.clone() });
        for (name, d) in &b.disks {
            report.disk.push(DiskLine {
                at_ms,
                name: name.clone(),
                busy_pct: (d.busy_seconds / secs * 100.0).min(100.0) as f32,
                reads: d.reads,
                writes: d.writes,
                read_bytes: d.read_bytes,
                written_bytes: d.written_bytes,
                read_per_second: d.read_bytes as f64 / secs,
                write_per_second: d.written_bytes as f64 / secs,
                avio_ms: (d.timed > 0).then(|| d.io_ms / d.timed as f64),
            });
        }
        for (name, net) in &b.nets {
            report.net.push(NetLine { at_ms, name: name.clone(), rx_per_second: net.rx_bytes as f64 / secs, tx_per_second: net.tx_bytes as f64 / secs, ..net.clone() });
        }
        if b.psi_samples > 0 {
            let p = |i: usize| b.psi[i].get(b.psi_samples);
            report.psi.push(PsiLine { at_ms, cpu: p(0), memory: p(1), memory_full: p(2), io: p(3), io_full: p(4) });
        }
    }
    let mut top: Vec<Consumer> = programs
        .into_iter()
        .map(|(name, p)| Consumer { name, processes: p.ids.len(), cpu_seconds: p.cpu_seconds, peak_resident: p.peak_resident, read: p.read, written: p.written, received: p.received, sent: p.sent })
        .collect();
    top.sort_by(|a, b| b.cpu_seconds.total_cmp(&a.cpu_seconds).then_with(|| a.name.cmp(&b.name)));
    top.truncate(options.top);
    report.top = top;
    Ok(report)
}

/// `3.2G`, `512M`, `40K`: atop's widths, so a column never jumps.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n}B")
    } else if v < 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{v:.0}{}", UNITS[u])
    }
}

fn clock(at_ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(at_ms).single().map_or_else(|| "?".into(), |t| t.format("%H:%M:%S").to_string())
}

/// `1h02m`, `3m20s`, `12.5s`: CPU time used.
fn duration(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    }
}

/// The report as a table: atopsar's layout, one line a bucket.
pub fn text(report: &Report, kind: Kind) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let range = if report.intervals > 0 { format!("{} – {}", clock(report.from_ms), clock(report.to_ms)) } else { "nothing recorded in the range".into() };
    let _ = writeln!(out, "{}  {} cpus  {range}  {} intervals, {}-second steps\n", report.hostname, report.cpus, report.intervals, report.step_seconds);
    let pct = |v: f32| format!("{v:.0}%");
    match kind {
        Kind::Cpu => {
            let _ = writeln!(out, "{:<9} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}", "time", "sys", "user", "irq", "idle", "wait", "steal", "load1");
            for l in &report.cpu {
                let load = l.load1.map_or_else(|| "-".into(), |v| format!("{v:.2}"));
                let _ = writeln!(out, "{:<9} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}", clock(l.at_ms), pct(l.sys), pct(l.user), pct(l.irq), pct(l.idle), pct(l.wait), pct(l.steal), load);
            }
        }
        Kind::Mem => {
            let _ = writeln!(out, "{:<9} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>4}", "time", "avail", "lowest", "cache", "dirty", "swap", "vmcom", "scan", "swin", "swout", "oom");
            for l in &report.mem {
                let _ = writeln!(
                    out,
                    "{:<9} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>4}",
                    clock(l.at_ms),
                    bytes(l.available),
                    bytes(l.least_available),
                    bytes(l.cached),
                    bytes(l.dirty),
                    bytes(l.swap_used),
                    bytes(l.committed),
                    l.scanned,
                    l.swap_in,
                    l.swap_out,
                    l.oom_kills
                );
            }
        }
        Kind::Disk => {
            let _ = writeln!(out, "{:<9} {:<10} {:>5} {:>8} {:>8} {:>7} {:>7} {:>7}", "time", "disk", "busy", "read/s", "write/s", "reads", "writes", "avio");
            for l in &report.disk {
                let avio = l.avio_ms.map_or_else(|| "-".into(), |v| format!("{v:.2}ms"));
                let _ = writeln!(out, "{:<9} {:<10} {:>5} {:>8} {:>8} {:>7} {:>7} {:>7}", clock(l.at_ms), l.name, pct(l.busy_pct), bytes(l.read_per_second as u64), bytes(l.write_per_second as u64), l.reads, l.writes, avio);
            }
        }
        Kind::Net => {
            let _ = writeln!(out, "{:<9} {:<12} {:>8} {:>8} {:>8} {:>8} {:>5} {:>5}", "time", "interface", "rx/s", "tx/s", "rxpk", "txpk", "errs", "drops");
            for l in &report.net {
                let _ = writeln!(out, "{:<9} {:<12} {:>8} {:>8} {:>8} {:>8} {:>5} {:>5}", clock(l.at_ms), l.name, bytes(l.rx_per_second as u64), bytes(l.tx_per_second as u64), l.rx_packets, l.tx_packets, l.errors, l.drops);
            }
        }
        Kind::Psi => {
            let _ = writeln!(out, "the share of time something waited, ten-second figures: mean/worst\n");
            let _ = writeln!(out, "{:<9} {:>9} {:>9} {:>9} {:>9} {:>9}", "time", "cpu", "mem", "memfull", "io", "iofull");
            let p = |p: Pressure| format!("{:.0}/{:.0}%", p.mean, p.max);
            for l in &report.psi {
                let _ = writeln!(out, "{:<9} {:>9} {:>9} {:>9} {:>9} {:>9}", clock(l.at_ms), p(l.cpu), p(l.memory), p(l.memory_full), p(l.io), p(l.io_full));
            }
        }
        Kind::Top => {
            // The range's own length, for "on average this much CPU".
            let seconds: f64 = report.cpu.iter().map(|l| l.seconds).sum::<f64>().max(0.001);
            let _ = writeln!(out, "{:<20} {:>5} {:>8} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8}", "program", "procs", "cpu time", "cpu", "peak mem", "read", "written", "net in", "net out");
            let net = |v: Option<u64>| v.map_or_else(|| "-".into(), bytes);
            for c in &report.top {
                // Kernel workers name what they are doing and run long.
                let name = if c.name.chars().count() > 20 { format!("{}…", c.name.chars().take(19).collect::<String>()) } else { c.name.clone() };
                let _ = writeln!(
                    out,
                    "{:<20} {:>5} {:>8} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8}",
                    name,
                    c.processes,
                    duration(c.cpu_seconds),
                    pct((c.cpu_seconds / seconds * 100.0) as f32),
                    bytes(c.peak_resident),
                    bytes(c.read),
                    bytes(c.written),
                    net(c.received),
                    net(c.sent)
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Header, Writer};
    use crate::machine::{Machine, PressureDetail, Windows};
    use crate::proc::{CpuTimes, Memory, Proc, Snapshot};

    /// A sample at `at_s` seconds: one busy program, the CPU counter
    /// moving 1000 ticks a sample (4 CPUs × 250), the program 100 of them.
    fn frame(at_s: i64, n: u64, boot: &str) -> Frame {
        let p = Proc { pid: 7, start: 3, name: "blender".into(), state: 'R', threads: 1, ticks: 100 * n, resident_bytes: 1 << 30, ..Proc::default() };
        let mut cpu = CpuTimes { total: 1000 * n, ..CpuTimes::default() };
        cpu.split[0] = 100 * n;
        cpu.split[3] = 900 * n;
        let memory = Memory { total: 16 << 30, available: (8 << 30) - n * (1 << 20), ..Memory::default() };
        let pressure = PressureDetail { io_some: Windows { avg10: n as f32, ..Windows::default() }, ..PressureDetail::default() };
        Frame {
            at_ms: at_s * 1000,
            boot_id: boot.into(),
            snapshot: Snapshot { procs: vec![p], cpu, memory, boot: 1, ..Snapshot::default() },
            machine: Machine { pressure_detail: Some(pressure), ..Machine::default() },
            ..Frame::default()
        }
    }

    fn reader(frames: &[Frame]) -> (tempfile::TempDir, Reader) {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("2026-10-10.hfr");
        let header = Header { hostname: "hogwarts".into(), cpus: 4, page_size: 4096, ticks_per_second: 100, interval_seconds: 10, command_lines: true };
        let mut w = Writer::open(&path, &header).unwrap();
        for f in frames {
            w.write(f).unwrap();
        }
        let r = Reader::open(&path).unwrap();
        (t, r)
    }

    fn everything(step_s: i64) -> Options {
        Options { from_ms: i64::MIN / 2, to_ms: i64::MAX / 2, step_ms: step_s * 1000, top: 10 }
    }

    #[test]
    fn intervals_fold_into_buckets_named_for_their_end() {
        // Samples every 10 s from 0 to 120: twelve intervals, two minutes.
        let frames: Vec<Frame> = (0..=12).map(|i| frame(i * 10, i as u64 + 1, "b")).collect();
        let (_t, mut r) = reader(&frames);
        let report = build(&mut r, &everything(60)).unwrap();
        assert_eq!(report.intervals, 12);
        let ends: Vec<i64> = report.cpu.iter().map(|l| l.at_ms).collect();
        assert_eq!(ends, [60_000, 120_000], "an interval ending exactly on the minute belongs to the minute it ends");
        assert_eq!(report.cpu[0].seconds, 60.0);
        // 10% of four CPUs is atop's 40%.
        assert!((report.cpu[0].user - 40.0).abs() < 0.01, "{:?}", report.cpu[0]);
        assert!((report.cpu[0].idle - 360.0).abs() < 0.01);
    }

    #[test]
    fn memory_keeps_the_lowest_point_as_well_as_the_last() {
        let frames: Vec<Frame> = (0..=6).map(|i| frame(i * 10, i as u64 + 1, "b")).collect();
        let (_t, mut r) = reader(&frames);
        let m = &build(&mut r, &everything(60)).unwrap().mem[0];
        // Available falls by 1 MiB a sample; the last sample is also the least.
        assert_eq!(m.available, (8 << 30) - 7 * (1 << 20));
        assert_eq!(m.least_available, m.available);
        assert_eq!(m.total, 16 << 30);
    }

    #[test]
    fn pressure_is_both_averaged_and_at_its_worst() {
        let frames: Vec<Frame> = (0..=4).map(|i| frame(i * 10, i as u64 + 1, "b")).collect();
        let (_t, mut r) = reader(&frames);
        let p = &build(&mut r, &everything(600)).unwrap().psi[0];
        // io avg10 at the four interval ends: 2, 3, 4, 5.
        assert_eq!((p.io.mean, p.io.max), (3.5, 5.0));
    }

    #[test]
    fn top_counts_cpu_as_time_used() {
        let frames: Vec<Frame> = (0..=6).map(|i| frame(i * 10, i as u64 + 1, "b")).collect();
        let (_t, mut r) = reader(&frames);
        let top = build(&mut r, &everything(60)).unwrap().top;
        assert_eq!(top[0].name, "blender");
        // 10% of 4 CPUs for 60 s: 24 CPU-seconds.
        assert!((top[0].cpu_seconds - 24.0).abs() < 0.01, "{:?}", top[0]);
        assert_eq!((top[0].processes, top[0].peak_resident), (1, 1 << 30));
    }

    #[test]
    fn a_reboot_is_a_gap_not_an_interval() {
        let mut frames: Vec<Frame> = (0..=3).map(|i| frame(i * 10, i as u64 + 1, "first")).collect();
        frames.extend((4..=6).map(|i| frame(i * 10, i as u64 - 3, "second")));
        let (_t, mut r) = reader(&frames);
        let report = build(&mut r, &everything(600)).unwrap();
        assert_eq!(report.intervals, 5, "three before the reboot, two after; none across it");
    }

    #[test]
    fn the_range_takes_only_intervals_that_end_inside_it() {
        let frames: Vec<Frame> = (0..=12).map(|i| frame(i * 10, i as u64 + 1, "b")).collect();
        let (_t, mut r) = reader(&frames);
        let report = build(&mut r, &Options { from_ms: 30_000, to_ms: 60_000, step_ms: 600_000, top: 10 }).unwrap();
        assert_eq!(report.intervals, 3, "ending at 40, 50 and 60 s");
        assert_eq!((report.from_ms, report.to_ms), (30_000, 60_000), "what was covered");
        let all = build(&mut r, &everything(60)).unwrap();
        assert_eq!((all.from_ms, all.to_ms), (0, 120_000), "the recorded stretch, not an unbounded range");
        let empty = build(&mut r, &Options { from_ms: 500_000, to_ms: 600_000, step_ms: 60_000, top: 10 }).unwrap();
        assert_eq!((empty.intervals, empty.cpu.len()), (0, 0));
    }

    #[test]
    fn the_text_is_one_line_a_bucket_under_a_heading() {
        let frames: Vec<Frame> = (0..=12).map(|i| frame(i * 10, i as u64 + 1, "b")).collect();
        let (_t, mut r) = reader(&frames);
        let report = build(&mut r, &everything(60)).unwrap();
        let cpu = text(&report, Kind::Cpu);
        assert!(cpu.starts_with("hogwarts  4 cpus"), "{cpu}");
        assert_eq!(cpu.lines().filter(|l| l.contains("40%")).count(), 2, "{cpu}");
        let top = text(&report, Kind::Top);
        assert!(top.lines().any(|l| l.starts_with("blender") && l.contains("48.0s") && l.contains("40%") && l.contains("1.0G")), "{top}");
    }

    #[test]
    fn cpu_time_reads_as_a_duration() {
        assert_eq!([duration(12.5), duration(200.0), duration(3720.0)], ["12.5s", "3m20s", "1h02m"]);
    }

    #[test]
    fn json_carries_only_the_table_asked_for() {
        let frames: Vec<Frame> = (0..=3).map(|i| frame(i * 10, i as u64 + 1, "b")).collect();
        let (_t, mut r) = reader(&frames);
        let json = serde_json::to_value(build(&mut r, &everything(60)).unwrap().only(Kind::Psi)).unwrap();
        assert!(json.get("psi").is_some() && json.get("cpu").is_none() && json.get("top").is_none(), "{json}");
        assert_eq!(json["hostname"], "hogwarts");
    }
}

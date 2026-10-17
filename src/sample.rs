//! What two snapshots a second apart say about CPU use.
//!
//! The counters in `/proc` only ever grow; use is how much they grew
//! between two reads. A process's share is its ticks over the ticks every
//! core spent in that time — so the shares are of the whole machine, add
//! up to at most 100, and agree with the CPU graph, which is the same
//! division for the machine as a whole. Clock ticks cancel out, so
//! `CLK_TCK` is never needed.

use crate::proc::{CpuTimes, Snapshot};
use std::collections::HashMap;

/// A process's identity: its pid and when it started. A pid alone is
/// reused; comparing two snapshots by pid would charge a new process with
/// the ticks of the one that had the number before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Id {
    pub pid: i32,
    pub start: u64,
}

/// How busy `now` was since `before`, from 0 to 1.
pub fn busy(before: CpuTimes, now: CpuTimes) -> f32 {
    let total = now.total.saturating_sub(before.total);
    if total == 0 {
        return 0.0;
    }
    let idle = now.idle.saturating_sub(before.idle).min(total);
    (total - idle) as f32 / total as f32
}

/// Each process's share of the whole machine since `before`, in percent.
/// A process `before` did not have (or had under another start time) is
/// not in the map: it has no second reading yet, and a guess would be a
/// number made up.
pub fn shares(before: &Snapshot, now: &Snapshot) -> HashMap<Id, f32> {
    let total = now.cpu.total.saturating_sub(before.cpu.total);
    let earlier: HashMap<Id, u64> = before.procs.iter().map(|p| (Id { pid: p.pid, start: p.start }, p.ticks)).collect();
    let mut out = HashMap::new();
    if total == 0 {
        return out;
    }
    for p in &now.procs {
        let id = Id { pid: p.pid, start: p.start };
        if let Some(&then) = earlier.get(&id) {
            let used = p.ticks.saturating_sub(then);
            out.insert(id, (used as f32 / total as f32 * 100.0).min(100.0));
        }
    }
    out
}

/// Each process's storage traffic since `before`, read and written
/// together, in bytes a second over `seconds`. Only a process with I/O
/// figures in both readings has one: no figure is not "no traffic".
pub fn disk_rates(before: &Snapshot, now: &Snapshot, seconds: f64) -> HashMap<Id, f64> {
    let earlier: HashMap<Id, (u64, u64)> =
        before.procs.iter().filter_map(|p| Some((Id { pid: p.pid, start: p.start }, p.io?))).collect();
    let mut out = HashMap::new();
    if seconds <= 0.0 {
        return out;
    }
    for p in &now.procs {
        let id = Id { pid: p.pid, start: p.start };
        if let (Some((r, w)), Some(&(r0, w0))) = (p.io, earlier.get(&id)) {
            out.insert(id, (r.saturating_sub(r0) + w.saturating_sub(w0)) as f64 / seconds);
        }
    }
    out
}

/// Each process's network traffic since `before`, in and out together,
/// in bytes a second. As with storage, only a process counted in both
/// readings has one — and only the helper's eBPF counter counts.
pub fn net_rates(before: &Snapshot, now: &Snapshot, seconds: f64) -> HashMap<Id, f64> {
    let earlier: HashMap<Id, crate::proc::NetIo> = before.procs.iter().filter_map(|p| Some((Id { pid: p.pid, start: p.start }, p.net?))).collect();
    let mut out = HashMap::new();
    if seconds <= 0.0 {
        return out;
    }
    for p in &now.procs {
        let id = Id { pid: p.pid, start: p.start };
        if let (Some(n), Some(n0)) = (p.net, earlier.get(&id)) {
            out.insert(id, (n.rx_bytes.saturating_sub(n0.rx_bytes) + n.tx_bytes.saturating_sub(n0.tx_bytes)) as f64 / seconds);
        }
    }
    out
}

/// Each process's wakeups a second: times any of its threads was put on
/// a CPU, from [`crate::proc::Proc::timeslices`]. Only a process with a
/// count in both readings has one. A thread that ended between them took
/// its count away, which can make the sum go down; that is floored at
/// zero rather than read as negative wakeups.
pub fn wakeup_rates(before: &Snapshot, now: &Snapshot, seconds: f64) -> HashMap<Id, f64> {
    let earlier: HashMap<Id, u64> = before.procs.iter().filter_map(|p| Some((Id { pid: p.pid, start: p.start }, p.timeslices?))).collect();
    let mut out = HashMap::new();
    if seconds <= 0.0 {
        return out;
    }
    for p in &now.procs {
        let id = Id { pid: p.pid, start: p.start };
        if let (Some(n), Some(&n0)) = (p.timeslices, earlier.get(&id)) {
            out.insert(id, n.saturating_sub(n0) as f64 / seconds);
        }
    }
    out
}

/// What two readings say about one process, for the Advanced view.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Detail {
    /// Of the whole machine, in its own code and in the kernel for it.
    pub user_pct: f32,
    pub system_pct: f32,
    /// Page faults a second: minor, and major (each a wait on the disk).
    pub minor_faults: f64,
    pub major_faults: f64,
    /// Context switches a second, when `status` was read: given up to
    /// wait, and taken away by the scheduler.
    pub voluntary: Option<f64>,
    pub forced: Option<f64>,
}

/// [`Detail`] for every process in both readings.
pub fn details(before: &Snapshot, now: &Snapshot, seconds: f64) -> HashMap<Id, Detail> {
    let total = now.cpu.total.saturating_sub(before.cpu.total);
    let earlier: HashMap<Id, &crate::proc::Proc> = before.procs.iter().map(|p| (Id { pid: p.pid, start: p.start }, p)).collect();
    let mut out = HashMap::new();
    if total == 0 || seconds <= 0.0 {
        return out;
    }
    let pct = |d: u64| (d as f32 / total as f32 * 100.0).min(100.0);
    for p in &now.procs {
        let id = Id { pid: p.pid, start: p.start };
        let Some(b) = earlier.get(&id) else { continue };
        let rate = |now: u64, then: u64| now.saturating_sub(then) as f64 / seconds;
        let (voluntary, forced) = match (p.verbose, b.verbose) {
            (Some(v), Some(w)) => (Some(rate(v.voluntary_switches, w.voluntary_switches)), Some(rate(v.forced_switches, w.forced_switches))),
            _ => (None, None),
        };
        out.insert(
            id,
            Detail {
                user_pct: pct(p.user_ticks.saturating_sub(b.user_ticks)),
                system_pct: pct(p.system_ticks.saturating_sub(b.system_ticks)),
                minor_faults: rate(p.minflt, b.minflt),
                major_faults: rate(p.majflt, b.majflt),
                voluntary,
                forced,
            },
        );
    }
    out
}

/// Where the whole machine's CPU time went between two readings, as
/// fractions in [`crate::proc::CPU_STATES`]' order.
pub fn split(before: crate::proc::CpuTimes, now: crate::proc::CpuTimes) -> [f32; 8] {
    let total = now.total.saturating_sub(before.total);
    std::array::from_fn(|i| if total == 0 { 0.0 } else { now.split[i].saturating_sub(before.split[i]) as f32 / total as f32 })
}

/// What two readings say about each process.
#[derive(Debug, Clone, Default)]
pub struct Use {
    /// Percent of the whole machine.
    pub cpu: HashMap<Id, f32>,
    /// Storage bytes a second.
    pub disk: HashMap<Id, f64>,
    /// Network bytes a second, in and out.
    pub net: HashMap<Id, f64>,
    /// Percent of the busiest GPU engine.
    pub gpu: HashMap<Id, f32>,
    /// Which cards it has open, by name ("AMD", "NVIDIA").
    pub gpu_cards: HashMap<Id, Vec<String>>,
    /// The Advanced view's rates; empty when it is off.
    pub detail: HashMap<Id, Detail>,
    /// Wakeups a second — see [`wakeup_rates`].
    pub wakeups: HashMap<Id, f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::Proc;

    fn proc(pid: i32, start: u64, ticks: u64) -> Proc {
        Proc {
            pid,
            start,
            name: "p".into(),
            command: String::new(),
            state: 'S',
            ppid: 1,
            ticks,
            threads: 1,
            private_bytes: 0,
            uid: 1000,
            kernel: false,
            io: None,
            nice: 0,
            resident_bytes: 0,
            user_ticks: 0,
            system_ticks: 0,
            minflt: 0,
            majflt: 0,
            vsize: 0,
            processor: 0,
            policy: 0,
            verbose: None,
            ..Proc::default()
        }
    }

    fn snap(total: u64, idle: u64, procs: Vec<Proc>) -> Snapshot {
        Snapshot { procs, cpu: CpuTimes { total, idle, ..CpuTimes::default() }, ..Snapshot::default() }
    }

    /// A thread that ended between readings took its count with it; the
    /// process woke no less for that, so the rate floors at zero.
    #[test]
    fn wakeups_are_a_rate_and_never_negative() {
        let with = |pid, n| Proc { timeslices: Some(n), ..proc(pid, 5, 0) };
        let a = snap(0, 0, vec![with(10, 100), with(11, 500), proc(12, 5, 0)]);
        let b = snap(0, 0, vec![with(10, 300), with(11, 400), with(12, 9)]);
        let r = wakeup_rates(&a, &b, 2.0);
        assert_eq!(r[&Id { pid: 10, start: 5 }], 100.0);
        assert_eq!(r[&Id { pid: 11, start: 5 }], 0.0);
        assert!(!r.contains_key(&Id { pid: 12, start: 5 }), "one reading is no rate");
    }

    #[test]
    fn a_process_share_is_its_ticks_over_every_cores_ticks() {
        let a = snap(1000, 900, vec![proc(10, 5, 100)]);
        let b = snap(1400, 1100, vec![proc(10, 5, 200)]);
        assert_eq!(shares(&a, &b)[&Id { pid: 10, start: 5 }], 25.0);
        assert!((busy(a.cpu, b.cpu) - 0.5).abs() < 1e-6);
    }

    /// The pid was reused between the two reads: the new process is not
    /// charged with the old one's ticks — it has no share at all yet.
    #[test]
    fn a_reused_pid_is_a_new_process_with_no_share_yet() {
        let a = snap(1000, 900, vec![proc(10, 5, 5000)]);
        let b = snap(1400, 1100, vec![proc(10, 77, 3)]);
        assert!(shares(&a, &b).is_empty());
    }

    #[test]
    fn a_network_rate_is_in_and_out_and_needs_both_readings() {
        use crate::proc::NetIo;
        let net = |rx, tx| Some(NetIo { rx_bytes: rx, tx_bytes: tx, ..NetIo::default() });
        let a = snap(0, 0, vec![Proc { net: net(1000, 0), ..proc(1, 5, 0) }, proc(2, 5, 0)]);
        let b = snap(0, 0, vec![Proc { net: net(3000, 1000), ..proc(1, 5, 0) }, Proc { net: net(50, 50), ..proc(2, 5, 0) }]);
        let rates = net_rates(&a, &b, 2.0);
        assert_eq!(rates.get(&Id { pid: 1, start: 5 }), Some(&1500.0));
        assert_eq!(rates.get(&Id { pid: 2, start: 5 }), None, "uncounted before: no rate yet, not a burst");
    }

    #[test]
    fn a_disk_rate_needs_figures_in_both_readings() {
        let mut a = proc(10, 5, 0);
        a.io = Some((1000, 0));
        let mut b = proc(10, 5, 0);
        b.io = Some((5000, 2000));
        let hidden = proc(11, 6, 0);
        let rates = disk_rates(&snap(0, 0, vec![a, hidden.clone()]), &snap(0, 0, vec![b, hidden]), 2.0);
        assert_eq!(rates[&Id { pid: 10, start: 5 }], 3000.0);
        assert!(!rates.contains_key(&Id { pid: 11, start: 6 }), "unknown is not zero");
    }

    #[test]
    fn a_detail_splits_cpu_and_counts_faults_and_switches_a_second() {
        let mut a = proc(10, 5, 0);
        a.user_ticks = 100;
        a.system_ticks = 10;
        a.majflt = 3;
        a.verbose = Some(crate::proc::Verbose { swap_bytes: 0, locked_bytes: 0, voluntary_switches: 50, forced_switches: 1 });
        let mut b = a.clone();
        b.user_ticks = 160;
        b.system_ticks = 30;
        b.majflt = 7;
        b.verbose = Some(crate::proc::Verbose { swap_bytes: 0, locked_bytes: 0, voluntary_switches: 250, forced_switches: 5 });
        let d = details(&snap(1000, 0, vec![a]), &snap(1400, 0, vec![b]), 2.0)[&Id { pid: 10, start: 5 }];
        assert!((d.user_pct - 15.0).abs() < 1e-4 && (d.system_pct - 5.0).abs() < 1e-4);
        assert_eq!((d.major_faults, d.voluntary, d.forced), (2.0, Some(100.0), Some(2.0)));
    }

    #[test]
    fn the_machines_split_adds_up_to_its_whole() {
        let before = CpuTimes { total: 0, idle: 0, split: [0; 8] };
        let now = CpuTimes { total: 100, idle: 70, split: [20, 0, 10, 60, 10, 0, 0, 0] };
        let s = split(before, now);
        assert!((s.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert_eq!(s[4], 0.1, "iowait");
    }

    #[test]
    fn no_time_passing_is_no_use_rather_than_a_division_by_zero() {
        let a = snap(1000, 900, vec![proc(10, 5, 100)]);
        assert!(shares(&a, &a).is_empty());
        assert_eq!(busy(a.cpu, a.cpu), 0.0);
    }
}

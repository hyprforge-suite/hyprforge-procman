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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

/// What two readings say about each process.
#[derive(Debug, Clone, Default)]
pub struct Use {
    /// Percent of the whole machine.
    pub cpu: HashMap<Id, f32>,
    /// Storage bytes a second.
    pub disk: HashMap<Id, f64>,
    /// Percent of the busiest GPU engine.
    pub gpu: HashMap<Id, f32>,
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
        }
    }

    fn snap(total: u64, idle: u64, procs: Vec<Proc>) -> Snapshot {
        Snapshot { procs, cpu: CpuTimes { total, idle, ..CpuTimes::default() }, ..Snapshot::default() }
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
    fn no_time_passing_is_no_use_rather_than_a_division_by_zero() {
        let a = snap(1000, 900, vec![proc(10, 5, 100)]);
        assert!(shares(&a, &a).is_empty());
        assert_eq!(busy(a.cpu, a.cpu), 0.0);
    }
}

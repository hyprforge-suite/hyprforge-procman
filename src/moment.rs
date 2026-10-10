//! One interval, between two frames: what every process and the machine
//! did in it. atop's screen is this; the TUI draws it live and from the
//! journal, and the window's history will too — one calculation, so a
//! moment reads the same wherever it is looked at.
//!
//! Counters only ever grow, so a figure for the interval is the later
//! frame's minus the earlier's — matched by pid *and* start time, never
//! pid alone (a reused pid would be charged its predecessor's life). A
//! process with no earlier reading started during the interval: what it
//! used is all its life so far. One that exited during it comes from its
//! taskstats record, less what the earlier frame had already seen.

use crate::journal::Frame;
use crate::proc::{Proc, ThreadStates};
use crate::sample::Id;
use crate::users::Users;
use std::collections::HashMap;

/// One process's interval.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    pub id: Id,
    pub ppid: i32,
    pub uid: u32,
    pub user: String,
    pub name: String,
    pub command: String,
    pub state: char,
    pub threads: u32,
    pub thread_states: Option<ThreadStates>,
    /// Of the whole machine over the interval.
    pub cpu_pct: f32,
    pub user_pct: f32,
    pub system_pct: f32,
    /// Waiting for a CPU, and for block I/O, during the interval (ms).
    pub run_delay_ms: f64,
    pub blkio_delay_ms: f64,
    pub private_bytes: u64,
    pub resident_bytes: u64,
    pub vsize: u64,
    /// How much resident and virtual memory grew in the interval.
    pub resident_growth: i64,
    pub virtual_growth: i64,
    pub pss: Option<u64>,
    pub swap: Option<u64>,
    pub locked: Option<u64>,
    pub minor_faults: u64,
    pub major_faults: u64,
    /// Bytes read and written to storage in the interval; `None` where
    /// the reader may not see them.
    pub read: Option<u64>,
    pub written: Option<u64>,
    pub cancelled: Option<u64>,
    pub voluntary: Option<u64>,
    pub forced: Option<u64>,
    pub nice: i32,
    pub priority: i64,
    pub policy: u32,
    pub processor: i32,
    pub kernel: bool,
    /// Ended during the interval: its exit code, or the signal.
    pub exit: Option<Ended>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Code(u32),
    Signal(u32),
}

/// One disk's interval, as atop's DSK line has it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiskLine {
    pub name: String,
    pub busy_pct: f32,
    pub reads: u64,
    pub writes: u64,
    pub read_bytes: u64,
    pub written_bytes: u64,
    /// Requests waiting, on average; milliseconds a request took.
    pub queue: f64,
    pub avio_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetLine {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub errors: u64,
    pub drops: u64,
}

/// The machine's interval: atop's header.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct System {
    pub seconds: f64,
    /// Processes, threads by state, zombies, and how many exited.
    pub processes: usize,
    pub threads: u64,
    pub running: u64,
    pub sleeping: u64,
    pub blocked: u64,
    pub zombies: usize,
    pub exited: usize,
    /// Where all CPUs' time went, percent of the whole, in
    /// `proc::CPU_STATES` order.
    pub cpu: [f32; 8],
    /// Each core: busy percent and clock.
    pub cores: Vec<(f32, Option<u32>)>,
    pub load: Option<[f32; 3]>,
    pub switches: u64,
    pub interrupts: u64,
    pub forks: u64,
    pub disks: Vec<DiskLine>,
    pub nets: Vec<NetLine>,
    /// Pages scanned and stolen, stalls and swap traffic in the interval.
    pub scanned: u64,
    pub stolen: u64,
    pub stalls: u64,
    pub swap_in: u64,
    pub swap_out: u64,
    pub oom_kills: u64,
    pub tcp_in: u64,
    pub tcp_out: u64,
    pub tcp_retransmitted: u64,
    pub udp_in: u64,
    pub udp_out: u64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Moment {
    pub at_ms: i64,
    pub system: System,
    pub rows: Vec<Row>,
}

fn delta(now: u64, then: u64) -> u64 {
    now.saturating_sub(then)
}

/// The interval from `prev` to `now`. A reboot between them (another
/// boot id) restarts every counter, so the interval is `now` alone, read
/// as if it began at boot.
pub fn between(prev: &Frame, now: &Frame, ticks_per_second: u64, users: &mut Users) -> Moment {
    let rebooted = prev.boot_id != now.boot_id;
    let empty = Frame::default();
    let prev = if rebooted { &empty } else { prev };
    let seconds = ((now.at_ms - prev.at_ms).max(1) as f64 / 1000.0).max(0.001);
    let tps = ticks_per_second.max(1) as f64;
    let total_ticks = delta(now.snapshot.cpu.total, prev.snapshot.cpu.total).max(1) as f64;
    let pct = |ticks: f64| (ticks / total_ticks * 100.0).min(100.0) as f32;
    let earlier: HashMap<Id, &Proc> = prev.snapshot.procs.iter().map(|p| (Id { pid: p.pid, start: p.start }, p)).collect();

    let mut rows: Vec<Row> = now
        .snapshot
        .procs
        .iter()
        .map(|p| {
            let id = Id { pid: p.pid, start: p.start };
            let b = earlier.get(&id).copied();
            let zero = Proc::default();
            let b0 = b.unwrap_or(&zero);
            let opt = |now: Option<u64>, then: Option<u64>| match (now, then, b) {
                (Some(n), Some(t), Some(_)) => Some(delta(n, t)),
                (Some(n), _, None) => Some(n),
                _ => None,
            };
            Row {
                id,
                ppid: p.ppid,
                uid: p.uid,
                user: users.name(p.uid),
                name: p.name.clone(),
                command: p.command.clone(),
                state: p.state,
                threads: p.threads,
                thread_states: p.thread_states,
                cpu_pct: pct(delta(p.ticks, b0.ticks) as f64),
                user_pct: pct(delta(p.user_ticks, b0.user_ticks) as f64),
                system_pct: pct(delta(p.system_ticks, b0.system_ticks) as f64),
                run_delay_ms: delta(p.run_delay_ns.unwrap_or(0), b0.run_delay_ns.unwrap_or(0)) as f64 / 1e6,
                blkio_delay_ms: delta(p.blkio_ticks, b0.blkio_ticks) as f64 / tps * 1000.0,
                private_bytes: p.private_bytes,
                resident_bytes: p.resident_bytes,
                vsize: p.vsize,
                resident_growth: p.resident_bytes as i64 - b0.resident_bytes as i64,
                virtual_growth: p.vsize as i64 - b0.vsize as i64,
                pss: p.pss,
                swap: p.verbose.map(|v| v.swap_bytes),
                locked: p.verbose.map(|v| v.locked_bytes),
                minor_faults: delta(p.minflt, b0.minflt),
                major_faults: delta(p.majflt, b0.majflt),
                read: opt(p.io.map(|i| i.0), b0.io.map(|i| i.0)),
                written: opt(p.io.map(|i| i.1), b0.io.map(|i| i.1)),
                cancelled: opt(p.cancelled_write, b0.cancelled_write),
                voluntary: opt(p.verbose.map(|v| v.voluntary_switches), b0.verbose.map(|v| v.voluntary_switches)),
                forced: opt(p.verbose.map(|v| v.forced_switches), b0.verbose.map(|v| v.forced_switches)),
                nice: p.nice,
                priority: p.priority,
                policy: p.policy,
                processor: p.processor,
                kernel: p.kernel,
                exit: None,
            }
        })
        .collect();

    // Exits: the record's lifetime totals, less what the earlier frame had
    // already counted for that pid (when it was alive then).
    let prev_by_pid: HashMap<u32, &Proc> = prev.snapshot.procs.iter().map(|p| (p.pid as u32, p)).collect();
    for e in &now.exits {
        let b = prev_by_pid.get(&e.pid).copied();
        let used_ticks = (e.user_us + e.system_us) as f64 / 1e6 * tps;
        let before_ticks = b.map_or(0.0, |p| p.ticks as f64);
        let b0_user = b.map_or(0.0, |p| p.user_ticks as f64);
        let b0_sys = b.map_or(0.0, |p| p.system_ticks as f64);
        rows.push(Row {
            id: Id { pid: e.pid as i32, start: b.map_or(0, |p| p.start) },
            ppid: e.ppid as i32,
            uid: e.uid,
            user: users.name(e.uid),
            name: e.comm.clone(),
            command: b.map(|p| p.command.clone()).unwrap_or_default(),
            state: 'E',
            cpu_pct: pct((used_ticks - before_ticks).max(0.0)),
            user_pct: pct((e.user_us as f64 / 1e6 * tps - b0_user).max(0.0)),
            system_pct: pct((e.system_us as f64 / 1e6 * tps - b0_sys).max(0.0)),
            minor_faults: e.minor_faults.saturating_sub(b.map_or(0, |p| p.minflt)),
            major_faults: e.major_faults.saturating_sub(b.map_or(0, |p| p.majflt)),
            read: Some(e.read_bytes.saturating_sub(b.and_then(|p| p.io).map_or(0, |i| i.0))),
            written: Some(e.write_bytes.saturating_sub(b.and_then(|p| p.io).map_or(0, |i| i.1))),
            resident_bytes: e.peak_rss_kib * 1024,
            exit: Some(match e.signal() {
                Some(s) => Ended::Signal(s),
                None => Ended::Code(e.code().unwrap_or(0)),
            }),
            ..Row::default()
        });
    }

    // The machine.
    let (s, p) = (&now.snapshot, &prev.snapshot);
    let split = crate::sample::split(p.cpu, s.cpu);
    let mut system = System {
        seconds,
        processes: s.procs.iter().filter(|p| !p.kernel).count(),
        threads: s.procs.iter().map(|p| u64::from(p.threads)).sum(),
        zombies: s.procs.iter().filter(|p| p.state == 'Z').count(),
        exited: now.exits.len(),
        cpu: split.map(|f| f * 100.0),
        cores: s
            .cores
            .iter()
            .zip(p.cores.iter().chain(std::iter::repeat(&crate::proc::CpuTimes::default())))
            .enumerate()
            .map(|(i, (n, b))| (crate::sample::busy(*b, *n) * 100.0, now.machine.clocks.get(i).and_then(|c| c.0)))
            .collect(),
        load: now.machine.load,
        switches: delta(s.counters.context_switches, p.counters.context_switches),
        interrupts: delta(s.counters.interrupts, p.counters.interrupts),
        forks: delta(s.counters.forks, p.counters.forks),
        ..System::default()
    };
    for t in s.procs.iter().filter_map(|p| p.thread_states.or_else(|| Some(ThreadStates::of_one(p.state)))) {
        system.running += u64::from(t.running);
        system.sleeping += u64::from(t.sleeping);
        system.blocked += u64::from(t.blocked);
    }
    let (m, mb) = (&now.machine, &prev.machine);
    system.scanned = delta(m.paging.scanned, mb.paging.scanned);
    system.stolen = delta(m.paging.stolen, mb.paging.stolen);
    system.stalls = delta(m.paging.alloc_stalls, mb.paging.alloc_stalls);
    system.swap_in = delta(m.paging.swap_in, mb.paging.swap_in);
    system.swap_out = delta(m.paging.swap_out, mb.paging.swap_out);
    system.oom_kills = delta(m.paging.oom_kills, mb.paging.oom_kills);
    system.tcp_in = delta(m.transport.tcp_in, mb.transport.tcp_in);
    system.tcp_out = delta(m.transport.tcp_out, mb.transport.tcp_out);
    system.tcp_retransmitted = delta(m.transport.tcp_retransmitted, mb.transport.tcp_retransmitted);
    system.udp_in = delta(m.transport.udp_in, mb.transport.udp_in);
    system.udp_out = delta(m.transport.udp_out, mb.transport.udp_out);
    for d in &m.disks {
        let b = mb.disks.iter().find(|x| x.name == d.name);
        let get = |f: fn(&crate::machine::Disk) -> u64| delta(f(d), b.map_or(0, f));
        let ops = get(|x| x.reads) + get(|x| x.writes);
        system.disks.push(DiskLine {
            name: d.name.clone(),
            busy_pct: (get(|x| x.busy_ms) as f64 / (seconds * 1000.0) * 100.0).min(100.0) as f32,
            reads: get(|x| x.reads),
            writes: get(|x| x.writes),
            read_bytes: get(|x| x.read_sectors) * 512,
            written_bytes: get(|x| x.write_sectors) * 512,
            queue: get(|x| x.queue_ms) as f64 / (seconds * 1000.0),
            avio_ms: (ops > 0).then(|| get(|x| x.busy_ms) as f64 / ops as f64),
        });
    }
    for n in &m.nets {
        let b = mb.nets.iter().find(|x| x.name == n.name);
        let get = |f: fn(&crate::machine::Net) -> u64| delta(f(n), b.map_or(0, f));
        system.nets.push(NetLine {
            name: n.name.clone(),
            rx_bytes: get(|x| x.rx_bytes),
            tx_bytes: get(|x| x.tx_bytes),
            rx_packets: get(|x| x.rx_packets),
            tx_packets: get(|x| x.tx_packets),
            errors: get(|x| x.errors),
            drops: get(|x| x.drops),
        });
    }
    Moment { at_ms: now.at_ms, system, rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exits::Exit;
    use crate::proc::{CpuTimes, Snapshot};

    fn proc(pid: i32, start: u64, ticks: u64) -> Proc {
        Proc { pid, start, name: format!("p{pid}"), state: 'S', threads: 1, ticks, user_ticks: ticks, uid: 0, ..Proc::default() }
    }

    fn frame(at_ms: i64, total: u64, procs: Vec<Proc>, exits: Vec<Exit>) -> Frame {
        Frame { at_ms, boot_id: "b".into(), snapshot: Snapshot { procs, cpu: CpuTimes { total, ..CpuTimes::default() }, ..Snapshot::default() }, exits, ..Frame::default() }
    }

    #[test]
    fn a_processs_interval_is_its_growth_since_the_last_frame() {
        let a = frame(0, 1000, vec![proc(1, 5, 100)], vec![]);
        let b = frame(10_000, 2000, vec![proc(1, 5, 350)], vec![]);
        let m = between(&a, &b, 100, &mut Users::default());
        assert_eq!(m.rows[0].cpu_pct, 25.0);
        assert_eq!(m.system.seconds, 10.0);
    }

    #[test]
    fn a_process_new_this_interval_is_charged_its_whole_life() {
        let a = frame(0, 1000, vec![], vec![]);
        let b = frame(10_000, 2000, vec![proc(2, 9, 100)], vec![]);
        assert_eq!(between(&a, &b, 100, &mut Users::default()).rows[0].cpu_pct, 10.0);
    }

    #[test]
    fn a_reused_pid_is_not_charged_its_predecessors_life() {
        let a = frame(0, 1000, vec![proc(3, 5, 900)], vec![]);
        let b = frame(10_000, 2000, vec![proc(3, 77, 10)], vec![]);
        assert_eq!(between(&a, &b, 100, &mut Users::default()).rows[0].cpu_pct, 1.0, "10 ticks, its own");
    }

    /// Lived across the earlier frame, then exited: its final record less
    /// what that frame had seen. Started and ended between frames: all
    /// of it, with how it ended.
    #[test]
    fn an_exit_shows_what_it_used_this_interval_and_how_it_ended() {
        let a = frame(0, 1000, vec![proc(4, 5, 100)], vec![]);
        let exits = vec![
            Exit { pid: 4, comm: "build".into(), user_us: 3_000_000, status: 0, ..Exit::default() },
            Exit { pid: 9, comm: "sleep".into(), user_us: 500_000, status: 9, ..Exit::default() },
        ];
        let b = frame(10_000, 2000, vec![], exits);
        let m = between(&a, &b, 100, &mut Users::default());
        let build = m.rows.iter().find(|r| r.name == "build").unwrap();
        assert_eq!((build.cpu_pct, build.exit), (20.0, Some(Ended::Code(0))), "300 ticks less the 100 already seen");
        let sleep = m.rows.iter().find(|r| r.name == "sleep").unwrap();
        assert_eq!((sleep.cpu_pct, sleep.exit), (5.0, Some(Ended::Signal(9))));
        assert_eq!(m.system.exited, 2);
    }

    #[test]
    fn a_reboot_between_frames_starts_every_counter_again() {
        let a = frame(0, 50_000, vec![proc(1, 5, 40_000)], vec![]);
        let mut b = frame(10_000, 1000, vec![proc(1, 5, 100)], vec![]);
        b.boot_id = "new".into();
        let m = between(&a, &b, 100, &mut Users::default());
        assert_eq!(m.rows[0].cpu_pct, 10.0, "not a negative or a huge delta");
    }
}

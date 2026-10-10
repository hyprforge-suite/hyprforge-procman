//! Drawing a screen: atop's header lines, then the list for the view.
//!
//! Labels are atop's where atop has one (PRC, CPU, MEM, RDDSK…), so its
//! users can read this without learning a second vocabulary; a figure
//! that is getting critical is coloured the way atop colours it.

use super::source::{GpuRow, Screen, ThreadRow};
use super::state::{Prompt, SortKey, State, View};
use crate::moment::{Ended, Row};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row as TableRow, Table, TableState};
use ratatui::Frame;
use std::collections::BTreeMap;

pub use crate::report::bytes;

fn signed(n: i64) -> String {
    match n {
        0 => "0B".into(),
        n if n < 0 => format!("-{}", bytes(n.unsigned_abs())),
        n => bytes(n as u64),
    }
}

/// `1234`, `12K`, `3.4M`: counts, not bytes.
fn count(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{}K", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn opt_bytes(n: Option<u64>) -> String {
    n.map_or_else(|| "?".into(), bytes)
}

/// atop's colours: cyan is fine, yellow is getting there, red is the
/// thing to look at.
fn level(value: f32, warn: f32, critical: f32) -> Style {
    if value >= critical {
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if value >= warn {
        Style::new().fg(Color::Yellow)
    } else {
        Style::new()
    }
}

fn label(text: &str) -> Span<'static> {
    Span::styled(format!("{text:<4}"), Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD))
}

fn field(name: &str, value: String) -> Vec<Span<'static>> {
    vec![Span::raw(" | "), Span::styled(name.to_owned(), Style::new().dim()), Span::raw(format!(" {value}"))]
}

fn styled_field(name: &str, value: String, style: Style) -> Vec<Span<'static>> {
    vec![Span::raw(" | "), Span::styled(name.to_owned(), Style::new().dim()), Span::styled(format!(" {value}"), style)]
}

fn line(name: &str, fields: Vec<Vec<Span<'static>>>) -> Line<'static> {
    let mut spans = vec![label(name)];
    spans.extend(fields.into_iter().flatten());
    Line::from(spans)
}

/// The header: one line per subsystem, atop's order.
pub fn header(screen: &Screen) -> Vec<Line<'static>> {
    let m = &screen.moment;
    let s = &m.system;
    let mut out = Vec::new();

    // The title: where, when, how long, and what kind of reading.
    let when = chrono::DateTime::from_timestamp_millis(m.at_ms).map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default();
    let mut title = vec![
        Span::styled(format!("{} ", screen.hostname), Style::new().bold()),
        Span::raw(when),
        Span::styled(format!("  {}", elapsed(s.seconds)), Style::new().dim()),
    ];
    if screen.since_boot {
        title.push(Span::styled("  since boot", Style::new().fg(Color::Yellow)));
    }
    if let Some((i, n)) = screen.position {
        title.push(Span::styled(format!("  replay {}/{n}", i + 1), Style::new().fg(Color::Magenta)));
    }
    if screen.partial {
        title.push(Span::styled("  only what you may read — sudo for everything", Style::new().fg(Color::Yellow)));
    }
    out.push(Line::from(title));

    out.push(line(
        "PRC",
        vec![
            field("proc", s.processes.to_string()),
            field("thr", s.threads.to_string()),
            field("run", s.running.to_string()),
            field("sleep", s.sleeping.to_string()),
            styled_field("blk", s.blocked.to_string(), level(s.blocked as f32, 2.0, 10.0)),
            styled_field("zomb", s.zombies.to_string(), level(s.zombies as f32, 1.0, 50.0)),
            field("exit", s.exited.to_string()),
        ],
    ));

    // CPU: the whole machine, then each core, as a share of all of it.
    let c = s.cpu;
    let busy = 100.0 - c[3] - c[4];
    let cpus = screen.cpus.max(1) as f32;
    let pct = |v: f32| format!("{:.0}%", v * cpus);
    out.push(line(
        "CPU",
        vec![
            styled_field("sys", pct(c[2]), level(busy, 70.0, 90.0)),
            field("user", pct(c[0] + c[1])),
            field("irq", pct(c[5] + c[6])),
            field("idle", pct(c[3])),
            styled_field("wait", pct(c[4]), level(c[4], 5.0, 20.0)),
            field("steal", pct(c[7])),
        ],
    ));
    let mut cores = vec![label("cpu")];
    for (i, (b, clock)) in s.cores.iter().enumerate() {
        cores.push(Span::raw(" "));
        cores.push(Span::styled(format!("{i}:{b:.0}%"), level(*b, 70.0, 90.0)));
        if let Some(mhz) = clock.filter(|_| *b >= 50.0) {
            cores.push(Span::styled(format!("@{:.1}", f64::from(mhz) / 1000.0), Style::new().dim()));
        }
    }
    out.push(Line::from(cores));

    let load = s.load.unwrap_or_default();
    out.push(line(
        "CPL",
        vec![
            styled_field("avg1", format!("{:.2}", load[0]), level(load[0] / cpus, 1.0, 2.0)),
            field("avg5", format!("{:.2}", load[1])),
            field("avg15", format!("{:.2}", load[2])),
            field("csw", count(s.switches)),
            field("intr", count(s.interrupts)),
            field("forks", count(s.forks)),
        ],
    ));

    let mem = &screen.now.snapshot.memory;
    let md = &screen.now.machine.memory;
    let used_pct = mem.used() as f32 / mem.total.max(1) as f32 * 100.0;
    out.push(line(
        "MEM",
        vec![
            field("tot", bytes(mem.total)),
            styled_field("avail", bytes(mem.available), level(used_pct, 85.0, 95.0)),
            field("free", bytes(md.free)),
            field("cache", bytes(md.cached)),
            field("buff", bytes(md.buffers)),
            field("slab", bytes(md.slab)),
            field("shmem", bytes(md.shmem)),
            field("dirty", bytes(md.dirty)),
        ],
    ));
    let swap_pct = mem.swap_used() as f32 / mem.swap_total.max(1) as f32 * 100.0;
    let commit_pct = md.committed as f32 / md.commit_limit.max(1) as f32 * 100.0;
    let mut swp = vec![field("tot", bytes(mem.swap_total)), styled_field("free", bytes(mem.swap_free), level(swap_pct, 50.0, 80.0)), field("swcac", bytes(md.swap_cached))];
    if md.zswap > 0 {
        swp.push(field("zswap", format!("{}→{}", bytes(md.zswapped), bytes(md.zswap))));
    }
    swp.push(styled_field("vmcom", bytes(md.committed), level(commit_pct, 90.0, 100.0)));
    swp.push(field("vmlim", bytes(md.commit_limit)));
    out.push(line("SWP", swp));

    out.push(line(
        "PAG",
        vec![
            styled_field("scan", count(s.scanned), level(s.scanned as f32, 1.0, 10_000.0)),
            field("steal", count(s.stolen)),
            styled_field("stall", count(s.stalls), level(s.stalls as f32, 1.0, 100.0)),
            styled_field("swin", count(s.swap_in), level(s.swap_in as f32, 1.0, 1000.0)),
            styled_field("swout", count(s.swap_out), level(s.swap_out as f32, 1.0, 1000.0)),
            styled_field("oom", count(s.oom_kills), level(s.oom_kills as f32, 1.0, 1.0)),
        ],
    ));

    if let Some(p) = &screen.now.machine.pressure_detail {
        let w = |name: &str, v: f32| styled_field(name, format!("{v:.0}%"), level(v, 10.0, 40.0));
        out.push(line(
            "PSI",
            vec![
                w("cpu", p.cpu_some.avg10),
                w("mem", p.memory_some.avg10),
                w("memfull", p.memory_full.avg10),
                w("io", p.io_some.avg10),
                w("iofull", p.io_full.avg10),
            ],
        ));
    }

    let secs = s.seconds.max(0.001);
    for d in s.disks.iter().filter(|d| d.reads + d.writes > 0 || d.busy_pct > 0.0) {
        let per = |b: u64, n: u64| b.checked_div(n).map_or_else(|| "0K".into(), bytes);
        out.push(line(
            "DSK",
            vec![
                vec![Span::raw(format!(" {:<9}", d.name))],
                styled_field("busy", format!("{:.0}%", d.busy_pct), level(d.busy_pct, 70.0, 90.0)),
                field("read", count(d.reads)),
                field("write", count(d.writes)),
                field("KiB/r", per(d.read_bytes, d.reads)),
                field("KiB/w", per(d.written_bytes, d.writes)),
                field("MBr/s", format!("{:.1}", d.read_bytes as f64 / secs / 1e6)),
                field("MBw/s", format!("{:.1}", d.written_bytes as f64 / secs / 1e6)),
                field("avq", format!("{:.2}", d.queue)),
                field("avio", d.avio_ms.map_or_else(|| "-".into(), |ms| format!("{ms:.2}ms"))),
            ],
        ));
    }

    out.push(line(
        "NET",
        vec![
            vec![Span::raw(" transport")],
            field("tcpi", count(s.tcp_in)),
            field("tcpo", count(s.tcp_out)),
            styled_field("retr", count(s.tcp_retransmitted), level(s.tcp_retransmitted as f32, 10.0, 1000.0)),
            field("udpi", count(s.udp_in)),
            field("udpo", count(s.udp_out)),
        ],
    ));
    for n in s.nets.iter().filter(|n| n.rx_packets + n.tx_packets > 0) {
        out.push(line(
            "NET",
            vec![
                vec![Span::raw(format!(" {:<9}", n.name))],
                field("pcki", count(n.rx_packets)),
                field("pcko", count(n.tx_packets)),
                field("si", format!("{}/s", bytes((n.rx_bytes as f64 / secs) as u64))),
                field("so", format!("{}/s", bytes((n.tx_bytes as f64 / secs) as u64))),
                styled_field("erri", count(n.errors), level(n.errors as f32, 1.0, 100.0)),
                styled_field("drpi", count(n.drops), level(n.drops as f32, 1.0, 1000.0)),
            ],
        ));
    }
    out
}

fn elapsed(seconds: f64) -> String {
    let s = seconds.round() as u64;
    match s {
        0..=99 => format!("{s}s elapsed"),
        100..=5999 => format!("{}m{:02}s elapsed", s / 60, s % 60),
        _ => format!("{}h{:02}m elapsed", s / 3600, s / 60 % 60),
    }
}

/// One user's or program's processes, added up — atop's `u` and `p`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Total {
    pub name: String,
    pub processes: usize,
    pub cpu_pct: f32,
    pub resident: u64,
    pub read: u64,
    pub written: u64,
}

pub fn totals(rows: &[&Row], key: fn(&Row) -> &str) -> Vec<Total> {
    let mut by: BTreeMap<&str, Total> = BTreeMap::new();
    for r in rows {
        let t = by.entry(key(r)).or_insert_with(|| Total { name: key(r).to_owned(), ..Total::default() });
        t.processes += 1;
        t.cpu_pct += r.cpu_pct;
        // An exited row's resident figure is its peak, not memory in use.
        if r.exit.is_none() {
            t.resident += r.resident_bytes;
        }
        t.read += r.read.unwrap_or(0);
        t.written += r.written.unwrap_or(0);
    }
    let mut out: Vec<Total> = by.into_values().collect();
    out.sort_by(|a, b| b.cpu_pct.total_cmp(&a.cpu_pct).then(b.resident.cmp(&a.resident)));
    out
}

/// One cgroup's interval — atop's `j`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CgroupLine {
    pub path: String,
    pub cpu_pct: f32,
    pub memory: Option<u64>,
    pub memory_max: Option<u64>,
    pub pids: Option<u64>,
    pub memory_pressure: Option<f32>,
    pub io_pressure: Option<f32>,
    pub oom_kills: Option<u64>,
}

/// The leaves — services and scopes — with what they used this interval,
/// busiest first. Slices are their children added up, so listing both
/// would count everything twice.
pub fn cgroup_lines(screen: &Screen) -> Vec<CgroupLine> {
    let secs = screen.moment.system.seconds.max(0.001);
    let whole = secs * 1e6 * f64::from(screen.cpus.max(1));
    let paths: Vec<&str> = screen.now.cgroups.iter().map(|c| c.path.as_str()).collect();
    let mut out: Vec<CgroupLine> = screen
        .now
        .cgroups
        .iter()
        .filter(|c| !c.path.is_empty() && !paths.iter().any(|p| p.len() > c.path.len() && p.starts_with(&c.path) && p.as_bytes()[c.path.len()] == b'/'))
        .map(|c| {
            let before = screen.prev.cgroups.iter().find(|b| b.path == c.path).and_then(|b| b.cpu_usec).unwrap_or(0);
            CgroupLine {
                path: c.path.clone(),
                cpu_pct: c.cpu_usec.map_or(0.0, |u| (u.saturating_sub(before) as f64 / whole * 100.0) as f32),
                memory: c.memory,
                memory_max: c.memory_max,
                pids: c.pids,
                memory_pressure: c.memory_pressure.map(|w| w.some),
                io_pressure: c.io_pressure.map(|w| w.some),
                oom_kills: c.oom_kills,
            }
        })
        .collect();
    out.sort_by(|a, b| b.cpu_pct.total_cmp(&a.cpu_pct).then(b.memory.cmp(&a.memory)));
    out
}

fn policy(p: u32) -> &'static str {
    match p {
        0 => "norm",
        1 => "fifo",
        2 => "rr",
        3 => "btch",
        5 => "idle",
        6 => "dl",
        _ => "?",
    }
}

fn name_of(r: &Row) -> String {
    match r.exit {
        Some(_) => format!("<{}>", r.name),
        None => r.name.clone(),
    }
}

fn exit_of(r: &Row) -> String {
    match r.exit {
        Some(Ended::Code(c)) => c.to_string(),
        Some(Ended::Signal(s)) => format!("sig{s}"),
        None => "-".into(),
    }
}

fn cpu_cell(pct: f32) -> Cell<'static> {
    Cell::from(format!("{pct:.1}%")).style(level(pct, 50.0, 90.0))
}

fn right(text: String) -> Cell<'static> {
    Cell::from(Line::from(text).right_aligned())
}

/// The columns for a view: headings, widths, and how to fill a row.
fn columns(view: View) -> (&'static [&'static str], Vec<Constraint>) {
    use Constraint::{Length, Min};
    match view {
        View::Generic => (
            &["PID", "USER", "THR", "S", "SYSCPU", "USRCPU", "RDELAY", "RSIZE", "RGROW", "RDDSK", "WRDSK", "EXC", "CPU", "CMD"],
            vec![Length(7), Length(10), Length(4), Length(1), Length(6), Length(6), Length(7), Length(6), Length(6), Length(6), Length(6), Length(5), Length(6), Min(10)],
        ),
        View::Memory => (
            &["PID", "MINFLT", "MAJFLT", "VSIZE", "VGROW", "RSIZE", "RGROW", "PSIZE", "SWAP", "LOCK", "MEM", "CMD"],
            vec![Length(7), Length(6), Length(6), Length(6), Length(6), Length(6), Length(6), Length(6), Length(6), Length(6), Length(5), Min(10)],
        ),
        View::Disk => (&["PID", "RDDSK", "WRDSK", "WCANCL", "DSK", "CMD"], vec![Length(7), Length(7), Length(7), Length(7), Length(5), Min(10)]),
        View::Network => (&["PID", "RNET", "SNET", "BANDWI", "BANDWO", "NET", "CMD"], vec![Length(7), Length(7), Length(7), Length(10), Length(10), Length(5), Min(10)]),
        View::Scheduling => (
            &["PID", "THR", "TRUN", "TSLPI", "TSLPU", "POLI", "NICE", "PRI", "CPUNR", "RDELAY", "BDELAY", "NVCSW", "NIVCSW", "S", "CPU", "CMD"],
            vec![Length(7), Length(4), Length(4), Length(5), Length(5), Length(4), Length(4), Length(4), Length(5), Length(7), Length(7), Length(6), Length(6), Length(1), Length(6), Min(10)],
        ),
        View::Various => (&["PID", "PPID", "USER", "THR", "S", "EXC", "CPU", "CMD"], vec![Length(7), Length(7), Length(10), Length(4), Length(1), Length(5), Length(6), Min(10)]),
        View::Command => (&["PID", "CPU", "COMMAND-LINE"], vec![Length(7), Length(6), Min(10)]),
        View::Users => (&["USER", "NPROCS", "CPU", "RSIZE", "RDDSK", "WRDSK"], vec![Length(14), Length(6), Length(7), Length(7), Length(7), Min(7)]),
        View::Programs => (&["PROGRAM", "NPROCS", "CPU", "RSIZE", "RDDSK", "WRDSK"], vec![Length(18), Length(6), Length(7), Length(7), Length(7), Min(7)]),
        View::Threads => (&["TID", "PID", "S", "CPUNR", "CPU", "THREAD", "PROCESS"], vec![Length(8), Length(8), Length(1), Length(5), Length(6), Length(18), Min(10)]),
        View::Gpu => (&["PID", "GPUBUSY", "VRAM", "GPU", "CMD"], vec![Length(7), Length(7), Length(7), Length(12), Min(10)]),
        View::Cgroups => (
            &["CPU", "MEM", "MEMMAX", "PIDS", "MPSI", "IOPSI", "OOM", "CGROUP"],
            vec![Length(6), Length(6), Length(6), Length(5), Length(5), Length(5), Length(4), Min(10)],
        ),
    }
}

/// What every listed process's figures are shares of, and the interval's
/// length for rates.
struct Totals {
    memory: u64,
    disk: u64,
    net: u64,
    seconds: f64,
}

/// `12 Kbps`, `3.4 Mbps`: a rate in bits, atop's BANDWI/BANDWO.
fn bits(bytes: u64, seconds: f64) -> String {
    let b = bytes as f64 * 8.0 / seconds.max(0.001);
    match b {
        b if b < 1e6 => format!("{:.0} Kbps", b / 1e3),
        b if b < 1e9 => format!("{:.1} Mbps", b / 1e6),
        b => format!("{:.1} Gbps", b / 1e9),
    }
}

fn process_cells(view: View, r: &Row, totals: &Totals) -> Vec<Cell<'static>> {
    let (total_memory, disk_total) = (totals.memory, totals.disk);
    let pid = right(r.id.pid.to_string());
    let ms = |v: f64| right(format!("{v:.0}ms"));
    match view {
        View::Generic => vec![
            pid,
            Cell::from(r.user.clone()),
            right(r.threads.to_string()),
            Cell::from(r.state.to_string()),
            right(format!("{:.1}%", r.system_pct)),
            right(format!("{:.1}%", r.user_pct)),
            ms(r.run_delay_ms),
            right(bytes(r.resident_bytes)),
            right(signed(r.resident_growth)),
            right(opt_bytes(r.read)),
            right(opt_bytes(r.written)),
            right(exit_of(r)),
            cpu_cell(r.cpu_pct),
            Cell::from(name_of(r)),
        ],
        View::Memory => vec![
            pid,
            right(count(r.minor_faults)),
            right(count(r.major_faults)).style(level(r.major_faults as f32, 1.0, 100.0)),
            right(bytes(r.vsize)),
            right(signed(r.virtual_growth)),
            right(bytes(r.resident_bytes)),
            right(signed(r.resident_growth)),
            right(r.pss.map_or_else(|| "-".into(), bytes)),
            right(opt_bytes(r.swap)),
            right(opt_bytes(r.locked)),
            right(format!("{:.0}%", r.resident_bytes as f64 / total_memory.max(1) as f64 * 100.0)),
            Cell::from(name_of(r)),
        ],
        View::Disk => {
            let mine = r.read.unwrap_or(0) + r.written.unwrap_or(0);
            vec![
                pid,
                right(opt_bytes(r.read)),
                right(opt_bytes(r.written)),
                right(opt_bytes(r.cancelled)),
                right(format!("{:.0}%", mine as f64 / disk_total.max(1) as f64 * 100.0)),
                Cell::from(name_of(r)),
            ]
        }
        View::Network => {
            let mine = r.net_rx.unwrap_or(0) + r.net_tx.unwrap_or(0);
            let rate = |v: Option<u64>| right(v.map_or_else(|| "-".into(), |b| bits(b, totals.seconds)));
            vec![
                pid,
                right(opt_bytes(r.net_rx)),
                right(opt_bytes(r.net_tx)),
                rate(r.net_rx),
                rate(r.net_tx),
                right(if r.net_rx.is_some() { format!("{:.0}%", mine as f64 / totals.net.max(1) as f64 * 100.0) } else { "-".into() }),
                Cell::from(name_of(r)),
            ]
        }
        View::Scheduling => {
            let t = r.thread_states.unwrap_or_default();
            vec![
                pid,
                right(r.threads.to_string()),
                right(t.running.to_string()),
                right(t.sleeping.to_string()),
                right(t.blocked.to_string()).style(level(t.blocked as f32, 1.0, 10.0)),
                Cell::from(policy(r.policy)),
                right(r.nice.to_string()),
                right(r.priority.to_string()),
                right(r.processor.to_string()),
                ms(r.run_delay_ms).style(level(r.run_delay_ms as f32, 100.0, 1000.0)),
                ms(r.blkio_delay_ms).style(level(r.blkio_delay_ms as f32, 100.0, 1000.0)),
                right(r.voluntary.map_or_else(|| "?".into(), count)),
                right(r.forced.map_or_else(|| "?".into(), count)),
                Cell::from(r.state.to_string()),
                cpu_cell(r.cpu_pct),
                Cell::from(name_of(r)),
            ]
        }
        View::Various => vec![
            pid,
            right(r.ppid.to_string()),
            Cell::from(r.user.clone()),
            right(r.threads.to_string()),
            Cell::from(r.state.to_string()),
            right(exit_of(r)),
            cpu_cell(r.cpu_pct),
            Cell::from(name_of(r)),
        ],
        View::Command => vec![pid, cpu_cell(r.cpu_pct), Cell::from(if r.command.is_empty() { name_of(r) } else { r.command.clone() })],
        View::Users | View::Programs | View::Cgroups | View::Threads | View::Gpu => Vec::new(),
    }
}

fn total_cells(t: &Total) -> Vec<Cell<'static>> {
    vec![Cell::from(t.name.clone()), right(t.processes.to_string()), cpu_cell(t.cpu_pct), right(bytes(t.resident)), right(bytes(t.read)), right(bytes(t.written))]
}

fn cgroup_cells(c: &CgroupLine) -> Vec<Cell<'static>> {
    let near_limit = match (c.memory, c.memory_max) {
        (Some(m), Some(max)) => m as f32 / max.max(1) as f32 * 100.0,
        _ => 0.0,
    };
    let psi = |v: Option<f32>| v.map_or_else(|| right("-".into()), |v| right(format!("{v:.0}%")).style(level(v, 10.0, 40.0)));
    vec![
        cpu_cell(c.cpu_pct),
        right(opt_bytes(c.memory)).style(level(near_limit, 80.0, 95.0)),
        right(c.memory_max.map_or_else(|| "max".into(), bytes)),
        right(c.pids.map_or_else(|| "-".into(), |p| p.to_string())),
        psi(c.memory_pressure),
        psi(c.io_pressure),
        right(c.oom_kills.map_or_else(|| "-".into(), |n| n.to_string())).style(level(c.oom_kills.unwrap_or(0) as f32, 1.0, 1.0)),
        Cell::from(short_cgroup(&c.path)),
    ]
}

/// A cgroup path with the part every user's session repeats taken out:
/// `user.slice/user-1000.slice/user@1000.service/app.slice/x.scope` is
/// `user@1000/app.slice/x.scope`. The unit at the end is what tells two
/// rows apart, and the prefix pushed it off a terminal's right edge.
fn short_cgroup(path: &str) -> String {
    let Some(rest) = path.strip_prefix("user.slice/") else { return path.to_owned() };
    let Some((slice, rest)) = rest.split_once('/') else { return path.to_owned() };
    let Some(uid) = slice.strip_prefix("user-").and_then(|s| s.strip_suffix(".slice")) else { return path.to_owned() };
    match rest.strip_prefix(&format!("user@{uid}.service")) {
        Some(tail) => format!("user@{uid}{tail}"),
        None => format!("user-{uid}/{rest}"),
    }
}

/// The threads view's rows: filtered by process, thread name or either
/// id, busiest first.
fn thread_list<'s>(state: &State, screen: &'s Screen) -> Vec<&'s ThreadRow> {
    let mut out: Vec<&ThreadRow> = screen.threads.iter().flatten().filter(|t| state.shows(&[&t.name, &t.process], &[t.pid, t.tid])).collect();
    out.sort_by(|a, b| b.cpu_pct.unwrap_or(-1.0).total_cmp(&a.cpu_pct.unwrap_or(-1.0)).then(a.tid.cmp(&b.tid)));
    out
}

/// The GPU view's rows, already in order (busiest, then most memory).
fn gpu_list<'s>(state: &State, screen: &'s Screen) -> Vec<&'s GpuRow> {
    screen.gpus.iter().flatten().filter(|g| state.shows(&[&g.name], &[g.pid])).collect()
}

fn thread_cells(t: &ThreadRow) -> Vec<Cell<'static>> {
    vec![
        right(t.tid.to_string()),
        right(t.pid.to_string()),
        Cell::from(t.state.to_string()),
        right(t.processor.to_string()),
        t.cpu_pct.map_or_else(|| right("-".into()), cpu_cell),
        Cell::from(t.name.clone()),
        Cell::from(t.process.clone()).style(Style::new().dim()),
    ]
}

fn gpu_cells(g: &GpuRow) -> Vec<Cell<'static>> {
    vec![
        right(g.pid.to_string()),
        g.busy_pct.map_or_else(|| right("-".into()), |p| right(format!("{p:.0}%")).style(level(p, 50.0, 90.0))),
        right(bytes(g.vram)),
        Cell::from(g.cards.join("+")),
        Cell::from(g.name.clone()),
    ]
}

/// How many rows the list has in this view — what the selection is
/// clamped to.
pub fn list_len(state: &State, screen: &Screen) -> usize {
    match state.view {
        View::Users => totals(&state.rows(&screen.moment), |r| &r.user).len(),
        View::Programs => totals(&state.rows(&screen.moment), |r| &r.name).len(),
        View::Cgroups => cgroup_lines(screen).len(),
        View::Threads => thread_list(state, screen).len(),
        View::Gpu => gpu_list(state, screen).len(),
        _ => state.rows(&screen.moment).len(),
    }
}

fn view_name(view: View) -> &'static str {
    match view {
        View::Generic => "generic",
        View::Memory => "memory",
        View::Disk => "disk",
        View::Network => "network",
        View::Scheduling => "scheduling",
        View::Various => "various",
        View::Command => "command line",
        View::Users => "per user",
        View::Programs => "per program",
        View::Cgroups => "cgroups",
        View::Threads => "threads",
        View::Gpu => "gpu",
    }
}

fn sort_name(state: &State, screen: &Screen) -> &'static str {
    // These two keep an order of their own; the sort keys are for processes.
    match state.view {
        View::Threads => return "cpu",
        View::Gpu => return "gpu busy, then video memory",
        _ => {}
    }
    let key = match state.sort {
        SortKey::Auto => super::state::auto_key(&screen.moment),
        k => k,
    };
    match (state.sort, key) {
        (SortKey::Auto, SortKey::Memory) => "auto: memory",
        (SortKey::Auto, SortKey::Disk) => "auto: disk",
        (SortKey::Auto, SortKey::Network) => "auto: network",
        (SortKey::Auto, _) => "auto: cpu",
        (_, SortKey::Memory) => "memory",
        (_, SortKey::Disk) => "disk",
        (_, SortKey::Network) => "network",
        _ => "cpu",
    }
}

fn status(state: &State, screen: &Screen) -> Line<'static> {
    if let Some(p) = &state.prompt {
        let ask = match p {
            Prompt::Filter(t) => format!("Show only (name, command, user or pid): {t}▏"),
            Prompt::Branch(t) => format!("Go to time (HH:MM): {t}▏"),
            Prompt::Interval(t) => format!("Refresh every (seconds): {t}▏"),
            Prompt::Kill { pid, name, .. } => format!("End {name} ({pid})? y to end it, anything else to leave it"),
        };
        return Line::from(Span::styled(ask, Style::new().fg(Color::Black).bg(Color::Yellow)));
    }
    if let Some(m) = &state.message {
        return Line::from(Span::styled(m.clone(), Style::new().fg(Color::Yellow)));
    }
    let mut spans = vec![Span::styled(format!(" {} ", view_name(state.view)), Style::new().reversed()), Span::raw(format!(" sorted by {}", sort_name(state, screen)))];
    if !state.filter.is_empty() {
        spans.push(Span::styled(format!("  showing {:?}", state.filter), Style::new().fg(Color::Magenta)));
    }
    // A live-only view in a replay is empty because the journal never
    // held it, and says so rather than look like an idle machine.
    if state.view.live_only() && screen.position.is_some() {
        spans.push(Span::styled("  threads and GPU use are read live, not kept in the journal", Style::new().fg(Color::Yellow)));
    }
    // The view says it is empty because nothing was counting, not
    // because nothing used the network.
    if state.view == View::Network && screen.moment.rows.iter().all(|r| r.net_rx.is_none()) {
        spans.push(Span::styled("  network per process is counted only under sudo", Style::new().fg(Color::Yellow)));
    }
    if state.paused {
        spans.push(Span::styled("  paused", Style::new().fg(Color::Yellow).bold()));
    }
    spans.push(Span::styled(if state.replay { "   t/T step · b time · r start · h help · q quit" } else { "   z pause · k end · h help · q quit" }, Style::new().dim()));
    Line::from(spans)
}

const HELP: &[(&str, &str)] = &[
    ("g m d n s v c", "process views: generic, memory, disk, network (under sudo), scheduling, various, command line"),
    ("y e", "live only: every thread by its own name; each process holding a GPU"),
    ("u p j", "added up: per user, per program, per cgroup"),
    ("C M D N A", "sort by CPU, memory, disk, network, or whatever is busiest"),
    ("/", "show only processes matching a name, command, user or pid; Esc clears"),
    ("↑ ↓ PgUp PgDn Home", "move through the list"),
    ("k", "end the chosen process (asks first; live only)"),
    ("z", "pause (live)"),
    ("i", "refresh interval (live)"),
    ("t T", "next and previous sample (replay)"),
    ("b", "go to a time of day (replay)"),
    ("r", "back to the first sample (replay)"),
    ("q", "quit"),
];

fn help(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line> = HELP.iter().map(|(k, what)| Line::from(vec![Span::styled(format!("{k:>20}  "), Style::new().fg(Color::Cyan).bold()), Span::raw(*what)])).collect();
    let h = (lines.len() as u16 + 2).min(area.height);
    let w = 96.min(area.width);
    let at = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    frame.render_widget(Clear, at);
    frame.render_widget(Paragraph::new(lines).block(Block::new().borders(Borders::ALL).title(" keys — any key closes ")), at);
}

pub fn draw(frame: &mut Frame, state: &State, screen: &Screen) {
    let head = header(screen);
    let [top, list, bottom] = Layout::vertical([Constraint::Length(head.len() as u16), Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    frame.render_widget(Paragraph::new(head), top);

    let (titles, widths) = columns(state.view);
    let heading = TableRow::new(titles.iter().map(|t| Cell::from(*t))).style(Style::new().reversed().bold());
    let rows: Vec<TableRow> = match state.view {
        View::Users => totals(&state.rows(&screen.moment), |r| &r.user).iter().map(|t| TableRow::new(total_cells(t))).collect(),
        View::Programs => totals(&state.rows(&screen.moment), |r| &r.name).iter().map(|t| TableRow::new(total_cells(t))).collect(),
        View::Cgroups => cgroup_lines(screen).iter().map(|c| TableRow::new(cgroup_cells(c))).collect(),
        View::Threads => thread_list(state, screen).into_iter().map(|t| TableRow::new(thread_cells(t))).collect(),
        View::Gpu => gpu_list(state, screen).into_iter().map(|g| TableRow::new(gpu_cells(g))).collect(),
        view => {
            let rows = state.rows(&screen.moment);
            let totals = Totals {
                memory: screen.now.snapshot.memory.total,
                disk: rows.iter().map(|r| r.read.unwrap_or(0) + r.written.unwrap_or(0)).sum(),
                net: rows.iter().map(|r| r.net_rx.unwrap_or(0) + r.net_tx.unwrap_or(0)).sum(),
                seconds: screen.moment.system.seconds,
            };
            rows.iter()
                .map(|r| {
                    let row = TableRow::new(process_cells(view, r, &totals));
                    // An exited process reads like atop's: dimmed, in brackets.
                    if r.exit.is_some() {
                        row.style(Style::new().dim())
                    } else {
                        row
                    }
                })
                .collect()
        }
    };
    let n = rows.len();
    let table = Table::new(rows, widths).header(heading).column_spacing(1).row_highlight_style(Style::new().bg(Color::DarkGray).bold());
    let mut selection = TableState::default().with_selected((n > 0).then(|| state.selected.min(n - 1)));
    frame.render_stateful_widget(table, list, &mut selection);
    frame.render_widget(Paragraph::new(status(state, screen)), bottom);
    if state.help {
        help(frame, frame.area());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgroups::Cgroup;
    use crate::moment::{DiskLine, Moment, System};
    use crate::proc::{Memory, Snapshot};
    use crate::sample::Id;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn screen() -> Screen {
        let rows = vec![
            Row { id: Id { pid: 4242, start: 1 }, name: "blender".into(), user: "voldemort".into(), state: 'R', threads: 12, cpu_pct: 93.5, resident_bytes: 3 << 30, read: Some(5 << 20), written: Some(0), ..Row::default() },
            Row { id: Id { pid: 99, start: 1 }, name: "sleep".into(), user: "voldemort".into(), state: 'E', exit: Some(Ended::Signal(9)), cpu_pct: 1.0, ..Row::default() },
            Row { id: Id { pid: 1, start: 1 }, name: "systemd".into(), user: "root".into(), state: 'S', cpu_pct: 0.2, resident_bytes: 12 << 20, ..Row::default() },
        ];
        let system = System { seconds: 10.0, processes: 2, cpu: [20.0, 0.0, 5.0, 70.0, 5.0, 0.0, 0.0, 0.0], disks: vec![DiskLine { name: "nvme0n1".into(), busy_pct: 95.0, reads: 100, read_bytes: 400 << 10, ..DiskLine::default() }], ..System::default() };
        let mut now = crate::journal::Frame { snapshot: Snapshot { memory: Memory { total: 32 << 30, available: 1 << 30, ..Memory::default() }, ..Snapshot::default() }, ..Default::default() };
        now.cgroups = vec![
            Cgroup { path: "system.slice".into(), cpu_usec: Some(9_000_000), ..Cgroup::default() },
            Cgroup { path: "system.slice/nix-daemon.service".into(), cpu_usec: Some(5_000_000), memory: Some(900 << 20), memory_max: Some(1 << 30), ..Cgroup::default() },
        ];
        let prev = crate::journal::Frame { cgroups: vec![Cgroup { path: "system.slice/nix-daemon.service".into(), cpu_usec: Some(1_000_000), ..Cgroup::default() }], ..Default::default() };
        Screen { moment: Moment { at_ms: 0, system, rows }, prev, now, cpus: 4, hostname: "hogwarts".into(), ..Screen::default() }
    }

    fn text(state: &State, screen: &Screen) -> String {
        let mut t = Terminal::new(TestBackend::new(140, 30)).unwrap();
        t.draw(|f| draw(f, state, screen)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_owned()).collect::<Vec<_>>().join("\n")
    }

    /// Every view draws, with its own columns, and names itself.
    #[test]
    fn every_view_draws_its_own_columns() {
        let s = screen();
        for (key, heading) in [('g', "RDELAY"), ('m', "MAJFLT"), ('d', "WCANCL"), ('n', "BANDWI"), ('s', "BDELAY"), ('v', "PPID"), ('c', "COMMAND-LINE"), ('u', "NPROCS"), ('p', "PROGRAM"), ('j', "CGROUP"), ('y', "THREAD"), ('e', "GPUBUSY")] {
            let mut state = State::new(false);
            state.key(super::super::state::Key::Char(key), None);
            let out = text(&state, &s);
            assert!(out.contains(heading), "view {key} has no {heading}:\n{out}");
            assert!(out.contains("PRC") && out.contains("MEM") && out.contains("DSK"), "{out}");
        }
    }

    #[test]
    fn threads_and_gpu_users_are_listed_busiest_first_and_filtered_like_processes() {
        use super::super::source::{GpuRow, ThreadRow};
        let mut s = screen();
        let t = |tid, name: &str, cpu| ThreadRow { pid: 4242, tid, process: "blender".into(), name: name.into(), state: 'R', cpu_pct: cpu, processor: 3 };
        s.threads = Some(vec![t(4242, "blender", Some(1.0)), t(4250, "BlenderRender", Some(60.0)), t(4251, "new-thread", None)]);
        s.gpus = Some(vec![GpuRow { pid: 4242, name: "blender".into(), cards: vec!["AMD".into(), "NVIDIA".into()], busy_pct: Some(88.0), vram: 2 << 30 }]);
        let mut state = State::new(false);
        state.key(super::super::state::Key::Char('y'), None);
        let out = text(&state, &s);
        let line = |name: &str| out.lines().position(|l| l.contains(name)).unwrap_or_else(|| panic!("{name} not listed:\n{out}"));
        assert!(line("BlenderRender") < line("1.0%") && line("1.0%") < line("new-thread"), "busiest first, unmeasured last:\n{out}");
        assert_eq!(list_len(&state, &s), 3);
        state.filter = "render".into();
        assert_eq!(list_len(&state, &s), 1, "the filter reaches thread names");
        state.filter.clear();
        state.key(super::super::state::Key::Char('e'), None);
        let out = text(&state, &s);
        assert!(out.contains("AMD+NVIDIA") && out.contains("88%") && out.contains("2.0G"), "{out}");
    }

    #[test]
    fn a_replay_says_threads_and_gpu_use_are_not_in_the_journal() {
        let mut s = screen();
        s.position = Some((0, 10));
        let mut state = State::new(true);
        state.key(super::super::state::Key::Char('y'), None);
        assert!(text(&state, &s).contains("not kept in the journal"));
    }

    #[test]
    fn an_exited_process_is_shown_in_brackets_with_how_it_ended() {
        let out = text(&State::new(false), &screen());
        assert!(out.contains("<sleep>") && out.contains("sig9"), "{out}");
    }

    /// A slice is its services added up: listing both would count the
    /// same CPU twice.
    #[test]
    fn the_cgroup_view_lists_leaves_with_this_intervals_cpu() {
        let lines = cgroup_lines(&screen());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].path, "system.slice/nix-daemon.service");
        assert_eq!(lines[0].cpu_pct, 10.0, "4 s of CPU over 10 s on 4 CPUs");
    }

    #[test]
    fn totals_add_up_by_user_and_leave_an_exits_peak_out_of_memory() {
        let s = screen();
        let state = State::new(false);
        let t = totals(&state.rows(&s.moment), |r| &r.user);
        let v = t.iter().find(|t| t.name == "voldemort").unwrap();
        assert_eq!((v.processes, v.resident), (2, 3 << 30));
    }

    #[test]
    fn a_partial_or_since_boot_reading_says_so_in_the_title() {
        let mut s = screen();
        s.partial = true;
        s.since_boot = true;
        let out = text(&State::new(false), &s);
        assert!(out.lines().next().unwrap().contains("since boot"));
        assert!(out.contains("sudo for everything"));
    }

    #[test]
    fn a_user_cgroup_loses_the_prefix_every_session_repeats() {
        assert_eq!(short_cgroup("user.slice/user-1000.slice/user@1000.service/app.slice/foot.scope"), "user@1000/app.slice/foot.scope");
        assert_eq!(short_cgroup("user.slice/user-1000.slice/session-2.scope"), "user-1000/session-2.scope");
        assert_eq!(short_cgroup("system.slice/sshd.service"), "system.slice/sshd.service");
    }

    #[test]
    fn sizes_keep_atops_widths() {
        assert_eq!(bytes(0), "0B");
        assert_eq!(bytes(1536), "1.5K");
        assert_eq!(bytes(3 << 30), "3.0G");
        assert_eq!(bytes(700 << 20), "700M");
        assert_eq!(signed(-(2 << 20)), "-2.0M");
        assert_eq!(count(12_345), "12K");
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// This machine, drawn as text — to look at, not to assert on.
    #[test]
    #[ignore = "prints this machine"]
    fn draw_this_machine() {
        let mut live = super::super::source::Live::new();
        live.extras = super::super::source::Extras { threads: true, gpu: true };
        live.screen().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        let screen = live.screen().unwrap();
        for view in ['g', 's', 'j', 'y', 'e'] {
            let mut state = State::new(false);
            state.key(super::super::state::Key::Char(view), None);
            let mut t = Terminal::new(TestBackend::new(160, 40)).unwrap();
            t.draw(|f| draw(f, &state, &screen)).unwrap();
            let buf = t.backend().buffer().clone();
            for y in 0..buf.area.height {
                eprintln!("{}", (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end());
            }
        }
    }
}

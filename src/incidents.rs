//! What went wrong: every program that crashed, and every one killed
//! for memory — read from the journal, which already has both.
//!
//! Two writers, three kinds of record, each found by the message ID its
//! writer gives it in systemd's own catalogue
//! (`/usr/lib/systemd/catalog/systemd.catalog`), never by its wording:
//!
//! - **systemd-coredump** writes a crash as [`DUMPED_CORE`], or as
//!   [`ABNORMAL`] when no core was kept, with the program, the signal and
//!   the crashing thread's stack trace.
//! - **The kernel** writes an OOM kill as two lines: `oom-kill:` with
//!   which process and which cgroup, and `Killed process` with what it
//!   held. These are the only record of *which* process died.
//! - **systemd** writes [`UNIT_OOM`] for each unit an OOM kill happened
//!   in — once per level of the unit tree, so one kill in a Flatpak's
//!   scope arrives as five records (the scope, `app-graphical.slice`,
//!   `app.slice`, `user-1000.slice`, `user.slice`), and sometimes with no
//!   kernel line at all: on 2026-09-28 one cascade here had none. Those
//!   collapse to one kill, named after the most specific unit.
//!   **systemd-oomd** writes [`OOMD_KILL`] when it is the one that acted.
//!
//! **Only named fields are asked for** ([`FIELDS`]). A coredump record
//! also carries the dead process's whole environment, its open files and
//! its command line, any of which can hold a token or a password; asking
//! for fields by name means they are never read into this process, so
//! nothing here can show, log or keep them.
//!
//! Reading is [`read`] — two bounded `journalctl` calls — and everything
//! else is [`parse`], pure, tested against records taken from this
//! machine's journal (`src/incidents/*.jsonl`).

use std::collections::HashMap;
use std::path::PathBuf;

/// systemd-coredump: a process dumped core.
pub const DUMPED_CORE: &str = "fc2e22bc6ee647b6b90729ab34a250b1";
/// systemd-coredump: a process ended abnormally and no core was kept.
pub const ABNORMAL: &str = "1f4e0a44a88649939aaea34fc6da8c95";
/// systemd: the kernel's OOM killer killed something in this unit.
pub const UNIT_OOM: &str = "fe6faa94e7774663a0da52717891d8ef";
/// systemd-oomd killed something in this unit.
pub const OOMD_KILL: &str = "d989611b15e44c9dbf31e3c81256e4ed";

/// Every field asked of the journal. Nothing else is read — see the
/// module doc for what a coredump record holds besides these.
pub const FIELDS: &str = "MESSAGE,MESSAGE_ID,COREDUMP_PID,COREDUMP_COMM,COREDUMP_EXE,COREDUMP_SIGNAL_NAME,COREDUMP_UID,\
COREDUMP_UNIT,COREDUMP_USER_UNIT,COREDUMP_THREAD_NAME,COREDUMP_FILENAME,UNIT,USER_UNIT";

/// Records of one kill, written within this long of each other, are the
/// same kill. The 2026-09-28 cascade here spread over 1.15 s.
const SAME_KILL_US: i64 = 3_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crash {
    /// Microseconds since the epoch, as the journal keeps it.
    pub at: i64,
    pub boot: String,
    pub pid: Option<i32>,
    /// The process's own name (`comm`), which is what a person knows it by.
    pub name: String,
    pub exe: Option<String>,
    /// `SIGSEGV`, `SIGABRT`…
    pub signal: Option<String>,
    pub uid: Option<u32>,
    /// The unit it ran in: its user unit when it had one, which is the
    /// specific one (`app-…scope`) rather than `user@1000.service`.
    pub unit: Option<String>,
    /// The thread that crashed, which is often the clue: `files-dnd`.
    pub thread: Option<String>,
    /// Where the core was kept. It may since have been cleaned up;
    /// whoever shows it checks.
    pub core: Option<PathBuf>,
    /// The crashing thread's stack, frame by frame, as systemd-coredump
    /// symbolised it; empty when its message has none. `None` when the
    /// journal gave no message at all — which, without `--all`, it does
    /// for any message over 4 KiB: 46 of this machine's 95 crashes.
    pub stack: Option<Vec<String>>,
    /// Whether a core was dumped ([`DUMPED_CORE`]) or not ([`ABNORMAL`]).
    pub dumped: bool,
}

/// Who did the killing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Killer {
    /// The kernel, because the whole machine ran out (`global_oom`).
    KernelMachine,
    /// The kernel, because a cgroup reached its own limit (`MemoryMax=`),
    /// with the rest of the machine perhaps fine.
    KernelLimit,
    /// The kernel, and only systemd's record of it survives, so which of
    /// the two is not known.
    Kernel,
    /// systemd-oomd, acting on memory pressure before the kernel had to.
    Oomd,
}

/// What a killed process held, from the kernel's own line. In KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    pub total_vm: u64,
    pub anon: u64,
    pub file: u64,
    pub shmem: u64,
}

impl Held {
    /// Resident memory: what it was actually using, in bytes.
    pub fn resident_bytes(&self) -> u64 {
        (self.anon + self.file + self.shmem) * 1024
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kill {
    pub at: i64,
    pub boot: String,
    pub killer: Killer,
    /// `None` when only systemd's per-unit record exists: it says a kill
    /// happened in the unit and not which process.
    pub pid: Option<i32>,
    pub name: Option<String>,
    pub uid: Option<u32>,
    pub unit: Option<String>,
    pub held: Option<Held>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incident {
    Crash(Crash),
    Kill(Kill),
}

impl Incident {
    pub fn at(&self) -> i64 {
        match self {
            Incident::Crash(c) => c.at,
            Incident::Kill(k) => k.at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Incidents {
    /// Newest first.
    pub list: Vec<Incident>,
    /// Whether the system journal itself was readable. When it is not,
    /// the journal answers with this user's records only and says
    /// nothing about it — so the window has to.
    pub everyone: bool,
}

/// A journal field's text. `journalctl -o json` writes a field that is
/// not valid UTF-8 as an array of bytes, and one over its size limit as
/// `null` unless asked for `--all`, which [`read`] is.
fn text(record: &serde_json::Value, field: &str) -> Option<String> {
    match record.get(field)? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(bytes) => {
            let bytes: Vec<u8> = bytes.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect();
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => None,
    }
}

fn number<T: std::str::FromStr>(record: &serde_json::Value, field: &str) -> Option<T> {
    text(record, field)?.trim().parse().ok()
}

/// The first thread's stack in systemd-coredump's message: the lines
/// after `Stack trace of thread N:` up to the blank line ending it.
/// systemd prints the thread that crashed first.
fn stack(message: &str) -> Vec<String> {
    message
        .lines()
        .skip_while(|l| !l.starts_with("Stack trace of thread"))
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect()
}

/// The kernel's `oom-kill:` line, as `key=value` pairs separated by
/// commas — plus the bare `global_oom` flag a machine-wide one carries.
/// `nodemask=(null)` and the cgroup paths hold no commas.
struct OomLine {
    pid: i32,
    task: Option<String>,
    uid: Option<u32>,
    memcg: Option<String>,
    global: bool,
}

fn oom_line(message: &str) -> Option<OomLine> {
    let rest = message.strip_prefix("oom-kill:")?;
    let mut fields = HashMap::new();
    let mut global = false;
    for part in rest.split(',') {
        match part.split_once('=') {
            Some((k, v)) => {
                fields.insert(k.trim(), v.trim());
            }
            None if part.trim() == "global_oom" => global = true,
            None => {}
        }
    }
    Some(OomLine {
        pid: fields.get("pid")?.parse().ok()?,
        task: fields.get("task").map(|s| s.to_string()),
        uid: fields.get("uid").and_then(|s| s.parse().ok()),
        memcg: fields.get("task_memcg").map(|s| s.to_string()),
        global: global || fields.get("constraint") == Some(&"CONSTRAINT_NONE"),
    })
}

/// `… Killed process 69005 (blender) total-vm:65589028kB, anon-rss:…`.
/// The name sits in parentheses and may hold spaces or parentheses of
/// its own, so it runs to the last `) total-vm:`.
fn killed_line(message: &str) -> Option<(i32, String, Held)> {
    let rest = &message[message.find("Killed process ")? + "Killed process ".len()..];
    let (pid, rest) = rest.split_once(" (")?;
    let end = rest.rfind(") total-vm:")?;
    let name = rest[..end].to_string();
    let kb = |key: &str| -> Option<u64> {
        let from = rest.find(key)? + key.len();
        rest[from..].split("kB").next()?.trim().parse().ok()
    };
    Some((
        pid.trim().parse().ok()?,
        name,
        Held { total_vm: kb("total-vm:")?, anon: kb("anon-rss:")?, file: kb("file-rss:")?, shmem: kb("shmem-rss:")? },
    ))
}

/// The unit a cgroup path belongs to: its last component.
fn unit_of(memcg: &str) -> Option<String> {
    memcg.rsplit('/').find(|c| !c.is_empty()).map(str::to_owned)
}

/// Of the units one kill was reported in, the one that says most.
/// Anything a user's own manager reported (`USER_UNIT`) lives inside
/// `user@N.service`, so it is deeper than everything the system manager
/// reported — dash counts cannot see that, and ranked `user-1000.slice`
/// level with `app-graphical.slice`. Within one manager, a scope or
/// service beats any slice, and a slice with more dash-separated parts
/// sits inside the one with fewer (`app-graphical.slice` in `app.slice`).
fn most_specific(units: &[(String, bool)]) -> Option<String> {
    let rank = |(u, user): &(String, bool)| -> (bool, u8, usize) {
        let kind = if u.ends_with(".slice") { 0 } else { 1 };
        (*user, kind, u.split('-').count())
    };
    units.iter().max_by_key(|u| rank(u)).map(|(u, _)| u.clone())
}

/// Everything in two `journalctl -o json` outputs: `records` holds the
/// message-ID records ([`DUMPED_CORE`], [`ABNORMAL`], [`UNIT_OOM`],
/// [`OOMD_KILL`]), `kernel` the kernel's OOM lines. Newest first.
pub fn parse(records: &str, kernel: &str) -> Vec<Incident> {
    let lines = |s: &str| -> Vec<serde_json::Value> { s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect() };
    let at = |r: &serde_json::Value| number::<i64>(r, "__REALTIME_TIMESTAMP").unwrap_or(0);
    let boot = |r: &serde_json::Value| text(r, "_BOOT_ID").unwrap_or_default();

    let mut out = Vec::new();

    // The kernel's pair of lines, joined on the pid within one boot.
    let mut kills: Vec<Kill> = Vec::new();
    let mut pending: HashMap<(String, i32), OomLine> = HashMap::new();
    for r in lines(kernel) {
        let Some(message) = text(&r, "MESSAGE") else { continue };
        if let Some(line) = oom_line(&message) {
            pending.insert((boot(&r), line.pid), line);
        } else if let Some((pid, name, held)) = killed_line(&message) {
            let line = pending.remove(&(boot(&r), pid));
            let global = line.as_ref().map(|l| l.global).unwrap_or(message.starts_with("Out of memory"));
            kills.push(Kill {
                at: at(&r),
                boot: boot(&r),
                killer: if global { Killer::KernelMachine } else { Killer::KernelLimit },
                pid: Some(pid),
                name: Some(line.as_ref().and_then(|l| l.task.clone()).unwrap_or(name)),
                uid: line.as_ref().and_then(|l| l.uid),
                unit: line.as_ref().and_then(|l| l.memcg.as_deref()).and_then(unit_of),
                held: Some(held),
            });
        }
    }

    // systemd's per-unit records, gathered into the kills they describe.
    // (when, boot, unit, whether a user's manager reported it)
    let mut unit_records: Vec<(i64, String, String, bool)> = Vec::new();
    for r in lines(records) {
        let id = text(&r, "MESSAGE_ID").unwrap_or_default();
        match id.as_str() {
            DUMPED_CORE | ABNORMAL => out.push(Incident::Crash(Crash {
                at: at(&r),
                boot: boot(&r),
                pid: number(&r, "COREDUMP_PID"),
                name: text(&r, "COREDUMP_COMM").unwrap_or_else(|| "?".into()),
                exe: text(&r, "COREDUMP_EXE"),
                signal: text(&r, "COREDUMP_SIGNAL_NAME"),
                uid: number(&r, "COREDUMP_UID"),
                unit: text(&r, "COREDUMP_USER_UNIT").or_else(|| text(&r, "COREDUMP_UNIT")),
                thread: text(&r, "COREDUMP_THREAD_NAME"),
                core: text(&r, "COREDUMP_FILENAME").map(PathBuf::from),
                stack: text(&r, "MESSAGE").map(|m| stack(&m)),
                dumped: id == DUMPED_CORE,
            })),
            UNIT_OOM => {
                if let Some(unit) = text(&r, "USER_UNIT") {
                    unit_records.push((at(&r), boot(&r), unit, true));
                } else if let Some(unit) = text(&r, "UNIT") {
                    unit_records.push((at(&r), boot(&r), unit, false));
                }
            }
            OOMD_KILL => kills.push(Kill {
                at: at(&r),
                boot: boot(&r),
                killer: Killer::Oomd,
                pid: None,
                name: None,
                uid: None,
                unit: text(&r, "USER_UNIT").or_else(|| text(&r, "UNIT")),
                held: None,
            }),
            _ => {}
        }
    }
    unit_records.sort();
    let mut i = 0;
    while i < unit_records.len() {
        let (start, boot, _, _) = unit_records[i].clone();
        let mut j = i;
        while j < unit_records.len() && unit_records[j].1 == boot && unit_records[j].0 - start <= SAME_KILL_US {
            j += 1;
        }
        let units: Vec<(String, bool)> = unit_records[i..j].iter().map(|(_, _, u, user)| (u.clone(), *user)).collect();
        let close = |k: &Kill| k.boot == boot && (k.at - start).abs() <= SAME_KILL_US;
        if let Some(k) = kills.iter_mut().find(|k| k.killer != Killer::Oomd && close(k)) {
            // The kernel named the process; systemd may name its unit
            // better than the cgroup path did.
            if k.unit.is_none() {
                k.unit = most_specific(&units);
            }
        } else {
            kills.push(Kill { at: start, boot, killer: Killer::Kernel, pid: None, name: None, uid: None, unit: most_specific(&units), held: None });
        }
        i = j;
    }

    out.extend(kills.into_iter().map(Incident::Kill));
    out.sort_by_key(|i| std::cmp::Reverse(i.at()));
    out
}

/// Whether this user can read the system journal, and so everyone's
/// records. `journalctl` itself answers a user without access with that
/// user's own records and no sign of what was left out.
pub fn sees_everyone() -> bool {
    let Ok(machine) = std::fs::read_to_string("/etc/machine-id") else { return false };
    let machine = machine.trim();
    ["/var/log/journal", "/run/log/journal"]
        .iter()
        .any(|dir| std::fs::File::open(PathBuf::from(dir).join(machine).join("system.journal")).is_ok())
}

/// The last `days` of crashes and memory kills. Two bounded calls: one
/// for the message-ID records, one for the kernel's lines.
pub fn read(days: u32) -> Result<Incidents, String> {
    let since = format!("--since=-{days}d");
    let run = |args: &[&str]| -> Result<String, String> {
        let mut command = std::process::Command::new("journalctl");
        command.args(["--no-pager", "-q", "-a", "-o", "json", &since]).args(args);
        let out = hyprforge_process::output(&mut command, hyprforge_process::TIMEOUT).map_err(|e| format!("journalctl didn't answer: {e}"))?;
        // No match at all exits 1 on some versions; an empty answer is
        // still an answer.
        if !out.status.success() && !out.stdout.is_empty() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let fields = format!("--output-fields={FIELDS}");
    let records = run(&[
        &fields,
        &format!("MESSAGE_ID={DUMPED_CORE}"),
        &format!("MESSAGE_ID={ABNORMAL}"),
        &format!("MESSAGE_ID={UNIT_OOM}"),
        &format!("MESSAGE_ID={OOMD_KILL}"),
    ])?;
    let kernel = run(&["--output-fields=MESSAGE", "_TRANSPORT=kernel", "-g", "oom-kill:constraint=|Killed process "])?;
    Ok(Incidents { list: parse(&records, &kernel), everyone: sees_everyone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDS: &str = include_str!("incidents/units-and-crashes.jsonl");
    const KERNEL: &str = include_str!("incidents/kernel.jsonl");

    fn kills() -> Vec<Kill> {
        parse(RECORDS, KERNEL)
            .into_iter()
            .filter_map(|i| match i {
                Incident::Kill(k) => Some(k),
                _ => None,
            })
            .collect()
    }

    /// 2026-09-28 20:26: the machine ran out and the kernel took Blender,
    /// holding 59 GB. Its two kernel lines become one kill that says who,
    /// how much, and the Flatpak scope it ran in — not the five systemd
    /// records around it.
    #[test]
    fn a_machine_wide_oom_kill_names_the_process_what_it_held_and_its_scope() {
        let blender: Vec<_> = kills().into_iter().filter(|k| k.name.as_deref() == Some("blender")).collect();
        assert_eq!(blender.len(), 1, "one kill, however many records describe it");
        let k = &blender[0];
        assert_eq!(k.pid, Some(69005));
        assert_eq!(k.killer, Killer::KernelMachine);
        assert_eq!(k.uid, Some(1000));
        assert_eq!(k.held.unwrap().anon, 59_630_272);
        assert!(k.held.unwrap().resident_bytes() > 60_000_000_000);
        assert!(k.unit.as_deref().unwrap().starts_with("app-flatpak-org.blender.Blender"), "{:?}", k.unit);
    }

    /// A cgroup at its own `MemoryMax=` is not the machine running out,
    /// and saying so is the difference between "buy memory" and "raise
    /// the limit". The sample is a 50 MB scope asked for 300 MB.
    #[test]
    fn a_kill_at_a_cgroups_own_limit_is_told_apart_from_the_machine_running_out() {
        let k = kills().into_iter().find(|k| k.name.as_deref() == Some("python3")).unwrap();
        assert_eq!(k.killer, Killer::KernelLimit);
        assert_eq!(k.unit.as_deref(), Some("hf-oom-sample.scope"));
        assert_eq!(k.pid, Some(713858));
    }

    /// 22:25 the same evening: four systemd records over 1.15 s, up the
    /// slice tree, and no kernel line. One kill, in the deepest slice,
    /// which cannot say which process — and does not pretend to.
    #[test]
    fn a_cascade_of_unit_records_with_no_kernel_line_is_one_kill_in_the_deepest_unit() {
        let unnamed: Vec<_> = kills().into_iter().filter(|k| k.pid.is_none()).collect();
        assert_eq!(unnamed.len(), 1, "{unnamed:?}");
        assert_eq!(unnamed[0].unit.as_deref(), Some("app-graphical.slice"));
        assert_eq!(unnamed[0].killer, Killer::Kernel, "which kind is not known");
        assert_eq!(kills().len(), 3, "Blender, the cascade, the sample");
    }

    /// Files' own crash, 2026-10-16: the thread that died and its stack
    /// are the whole clue, and both come through.
    #[test]
    fn a_crash_keeps_its_thread_its_signal_and_the_crashing_threads_stack() {
        let files = parse(RECORDS, KERNEL)
            .into_iter()
            .find_map(|i| match i {
                Incident::Crash(c) if c.name == "hyprforge-files" => Some(c),
                _ => None,
            })
            .unwrap();
        assert_eq!(files.signal.as_deref(), Some("SIGSEGV"));
        assert_eq!(files.thread.as_deref(), Some("files-dnd"));
        assert_eq!(files.unit.as_deref(), Some("wayland-wm@hyprland.desktop.service"), "the user unit, not user@1000");
        assert!(files.dumped);
        let stack = files.stack.unwrap();
        assert!(stack[0].starts_with("#0 "), "{stack:?}");
        assert!(stack.iter().any(|f| f.contains("wl_proxy_destroy")));
    }

    #[test]
    fn incidents_come_newest_first() {
        let all = parse(RECORDS, KERNEL);
        assert!(all.windows(2).all(|w| w[0].at() >= w[1].at()));
    }

    /// The field list is the privacy boundary, so it is pinned: nothing
    /// that holds the dead process's environment, files or arguments.
    #[test]
    fn the_journal_is_never_asked_for_a_crashed_processs_environment_files_or_arguments() {
        for field in ["COREDUMP_ENVIRON", "COREDUMP_OPEN_FDS", "COREDUMP_CMDLINE", "COREDUMP_PROC_"] {
            assert!(!FIELDS.contains(field), "{field}");
        }
    }

    #[test]
    fn a_name_with_parentheses_in_it_survives_the_kernels_line() {
        let (pid, name, held) = killed_line("Out of memory: Killed process 7 (Web Content (x)) total-vm:10kB, anon-rss:4kB, file-rss:2kB, shmem-rss:1kB, UID:1000 pgtables:1kB oom_score_adj:0").unwrap();
        assert_eq!((pid, name.as_str()), (7, "Web Content (x)"));
        assert_eq!(held.resident_bytes(), 7 * 1024);
    }

    /// Read-only, and asks what only the real journal can answer: that
    /// both queries come back, and that the coredump records it finds
    /// carry the fields asked for. This machine has crashes on record;
    /// one with none says so rather than passing on nothing.
    #[test]
    #[ignore = "live (systemd): changes nothing"]
    fn this_machines_journal_reads_and_parses() {
        let found = read(30).expect("journalctl answers");
        let crashes: Vec<_> = found.list.iter().filter_map(|i| if let Incident::Crash(c) = i { Some(c) } else { None }).collect();
        if crashes.is_empty() {
            eprintln!("HYPRFORGE-SKIP: no crash in this journal's last 30 days to check the fields of");
            return;
        }
        assert!(crashes.iter().all(|c| c.signal.is_some() && c.pid.is_some()), "a coredump record without its signal or pid");
        let lost = crashes.iter().filter(|c| c.stack.is_none()).count();
        assert_eq!(lost, 0, "{lost} of {} crash messages came back empty: --all is what keeps a long one", crashes.len());
    }
}

//! `hyprforge-procman-rules`: applies Processes → Rules as programs start.
//!
//! Every couple of seconds it lists `/proc` and reads only the processes
//! it hasn't seen, applies the first rule naming each, and watches the
//! ones with a warning size — see `hyprforge_procman::rules` for what a
//! rule may do and why. With no rules file, or an empty one, it only
//! checks the file. A file that doesn't read is logged once per change
//! and nothing new is applied until it does.
//!
//! `--dry-run` reads the rules and every process running now, says what
//! each rule would do, and does none of it.

use hyprforge_procman::limits::{self, Limiter, Plan, Target, UserSystemd};
use hyprforge_procman::proc::ProcFs;
use hyprforge_procman::rules::{self, Act, Checked, Loaded, Seen};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime};

const EVERY: Duration = Duration::from_secs(2);

fn seen(fs: &ProcFs, pid: i32, me: u32) -> Option<Seen> {
    let p = fs.one(pid)?;
    (!p.kernel).then_some(Seen { pid, start: p.start, name: p.name, nice: p.nice, own: p.uid == me })
}

/// `setpriority` on a pid changes one thread; a program can have started
/// several before the scan saw it, so every thread it has is set.
/// Threads and children made afterwards inherit it.
fn nice(pid: i32, to: i32) -> Result<(), String> {
    let mut any = false;
    for e in std::fs::read_dir(format!("/proc/{pid}/task")).map_err(|e| e.to_string())?.flatten() {
        let Some(tid) = e.file_name().to_str().and_then(|n| n.parse::<libc::id_t>().ok()) else { continue };
        // SAFETY: a plain syscall on a thread id; failure is reported.
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, to) } == 0 {
            any = true;
        }
    }
    if any { Ok(()) } else { Err(std::io::Error::last_os_error().to_string()) }
}

/// Every process's parent, read only when a cap is to be applied.
fn parents(all: &HashMap<i32, u64>) -> HashMap<i32, i32> {
    let parent = |pid: i32| -> Option<i32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(") ")?.1.split_whitespace().nth(1)?.parse().ok()
    };
    all.keys().filter_map(|p| Some((*p, parent(*p)?))).collect()
}

/// The process and everything it has started so far, for a cap: a build
/// that spawned its compilers before the scan saw it takes them along.
fn tree(root: i32, parents: &HashMap<i32, i32>) -> Vec<i32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let at = out[i];
        out.extend(parents.iter().filter(|(_, pp)| **pp == at).map(|(p, _)| *p));
        i += 1;
    }
    out
}

fn plan(pid: i32, name: &str, parents: &HashMap<i32, i32>, me: u32) -> Plan {
    let pids = tree(pid, parents);
    // Windows are asked of Hyprland only now, when a cap is to be made.
    let windows: HashSet<i32> = hyprforge_procman::windows::read().unwrap_or_default().into_iter().map(|w| w.pid).collect();
    if let Some(other) = rules::takes_another_app(&pids, &windows) {
        let other = std::fs::read_to_string(format!("/proc/{other}/comm")).map(|s| s.trim().to_string()).unwrap_or_else(|_| other.to_string());
        return Plan::Refuse(format!("capping {name} would take {other} along with it"));
    }
    let proc_root = Path::new("/proc");
    let cgroup = limits::cgroup_of(proc_root, pid);
    let in_unit = cgroup.as_deref().map(|c| limits::processes_in(&Path::new("/sys/fs/cgroup").join(c.trim_start_matches('/')))).unwrap_or_default();
    limits::plan(&Target {
        name: name.to_string(),
        lead: pid,
        pids,
        unit: hyprforge_procman::inspect::unit(proc_root, pid),
        main: limits::is_first(proc_root, pid, &in_unit),
        in_unit,
        cgroup,
        // A rule names a program; the compositor and the session's
        // managers are their services' main processes, which `main`
        // refuses to move — and one alone in its unit is its own app.
        critical: false,
        me,
        own: true,
    })
}

async fn act(a: &Act, parents: &HashMap<i32, i32>, me: u32, cores: u32) {
    match a {
        Act::Nice { pid, to } => match nice(*pid, *to) {
            Ok(()) => tracing::info!(pid, to, "priority set"),
            Err(e) => tracing::warn!(pid, to, "couldn't set its priority: {e}"),
        },
        Act::Limit { pid, name, limits } => {
            let props = limits.properties(cores);
            let result = match plan(*pid, name, parents, me) {
                Plan::Unit(unit) => UserSystemd.set(unit, props).await,
                Plan::NewScope { name: scope, pids } => {
                    UserSystemd.new_scope(scope, format!("{name} (capped by a Processes rule)"), pids.into_iter().map(|p| p as u32).collect(), props).await
                }
                Plan::Refuse(why) => Err(why),
            };
            match result {
                Ok(()) => tracing::info!(pid, %name, "capped"),
                Err(e) => tracing::warn!(pid, %name, "couldn't cap it: {e}"),
            }
        }
    }
}

/// A desktop notification, through whatever notification daemon runs.
async fn warn(name: &str, bytes: u64, at: u64) {
    let body = format!("{name} is using {} — its rule warns past {}.", size(bytes), size(at));
    let sent = async {
        let conn = zbus::Connection::session().await?;
        let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("Processes", 0u32, "dialog-warning-symbolic", format!("{name} is using a lot of memory"), body, Vec::<&str>::new(), hints, -1i32),
        )
        .await?;
        Ok::<(), zbus::Error>(())
    };
    if let Err(e) = tokio::time::timeout(Duration::from_secs(5), sent).await.unwrap_or_else(|_| Err(zbus::Error::Failure("timed out".into()))) {
        tracing::warn!(%name, "couldn't show the warning: {e}");
    }
}

fn size(bytes: u64) -> String {
    let gb = bytes as f64 / f64::from(1u32 << 30);
    if gb >= 1.0 { format!("{gb:.1} GB") } else { format!("{} MB", bytes >> 20) }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .without_time()
        .init();
    // SAFETY: getuid cannot fail.
    let me = unsafe { libc::getuid() };
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let path = rules::path();
    let fs = ProcFs::default();

    if std::env::args().any(|a| a == "--dry-run") {
        let rules = match rules::load(&path) {
            Loaded::None => {
                println!("no rules ({} doesn't exist)", path.display());
                return std::process::ExitCode::SUCCESS;
            }
            Loaded::Broken(e) => {
                eprintln!("{e}");
                return std::process::ExitCode::FAILURE;
            }
            Loaded::Rules(r) => r,
        };
        let all = rules::pids(Path::new("/proc"));
        let now: Vec<Seen> = all.keys().filter_map(|p| seen(&fs, *p, me)).collect();
        let parents = parents(&all);
        let names: HashMap<i32, String> = now.iter().map(|s| (s.pid, s.name.clone())).collect();
        for a in rules::on_start(&rules, &now) {
            let what = match &a {
                Act::Limit { pid, name, .. } if rules::under_another(*pid, name, &parents, &names) => continue,
                Act::Limit { pid, name, .. } => format!("{a:?} via {:?}", plan(*pid, name, &parents, me)),
                _ => format!("{a:?}"),
            };
            println!("would: {what}");
        }
        return std::process::ExitCode::SUCCESS;
    }

    let runtime = match hyprforge_procman::bound::service_runtime() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("couldn't start: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let mut rules: Vec<Checked> = Vec::new();
        let mut stamp = None;
        // Everything running when the rules first load is "already
        // started": a rule applies to programs as they start, never
        // retroactively to a whole session's worth at once.
        let mut known: HashSet<(i32, u64)> = rules::pids(Path::new("/proc")).into_iter().collect();
        let mut watched: HashMap<(i32, u64), Seen> = HashMap::new();
        let mut warned: HashSet<(i32, u64)> = HashSet::new();
        tracing::info!(path = %path.display(), "applying rules");
        loop {
            let now_stamp = modified(&path);
            if now_stamp != stamp {
                stamp = now_stamp;
                let had = !rules.is_empty();
                rules = match rules::load(&path) {
                    Loaded::None => Vec::new(),
                    Loaded::Rules(r) => {
                        tracing::info!(count = r.len(), "rules read");
                        r
                    }
                    Loaded::Broken(e) => {
                        tracing::error!("{e} — nothing new is applied until it reads");
                        Vec::new()
                    }
                };
                // Rules that appear where there were none start from
                // what is running now, as at start-up: no rule applies
                // retroactively to everything already open.
                if !had && !rules.is_empty() {
                    known = rules::pids(Path::new("/proc")).into_iter().collect();
                }
            }
            if !rules.is_empty() {
                let all = rules::pids(Path::new("/proc"));
                let fresh: Vec<Seen> = all.iter().filter(|(p, s)| !known.contains(&(**p, **s))).filter_map(|(p, _)| seen(&fs, *p, me)).collect();
                known = all.iter().map(|(p, s)| (*p, *s)).collect();
                let acts = rules::on_start(&rules, &fresh);
                if !acts.is_empty() {
                    let parents = parents(&all);
                    let names: HashMap<i32, String> =
                        all.keys().filter_map(|p| Some((*p, std::fs::read_to_string(format!("/proc/{p}/comm")).ok()?.trim().to_string()))).chain(fresh.iter().map(|s| (s.pid, s.name.clone()))).collect();
                    for a in acts {
                        if let Act::Limit { pid, name, .. } = &a {
                            if rules::under_another(*pid, name, &parents, &names) {
                                continue;
                            }
                        }
                        act(&a, &parents, me, cores).await;
                    }
                }
                for p in fresh {
                    if rules::rule_for(&rules, &p).is_some_and(|r| r.warn_memory.is_some()) {
                        watched.insert((p.pid, p.start), p);
                    }
                }
                watched.retain(|k, _| known.contains(k));
                warned.retain(|k| known.contains(k));
                let held: Vec<(Seen, u64)> = watched.values().filter_map(|p| Some((p.clone(), fs.one(p.pid)?.resident_bytes))).collect();
                for (pid, name, bytes, at) in rules::over(&rules, &held, &warned) {
                    if let Some(p) = watched.values().find(|p| p.pid == pid) {
                        warned.insert((p.pid, p.start));
                    }
                    warn(&name, bytes, at).await;
                }
            }
            tokio::time::sleep(EVERY).await;
        }
    })
}

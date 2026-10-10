//! Every cgroup: what it has used, what it may use, and how long things
//! in it waited — atop's cgroup view. A systemd service is one cgroup,
//! and so is each slice and scope above and beside it, so this is the
//! machine seen the way systemd arranges it.
//!
//! Only what the kernel offers is read. Which controllers a cgroup has
//! depends on its parents' `cgroup.subtree_control` — on this machine
//! `cpu memory pids` at the top, no `io` — so a figure whose file is
//! absent is `None`, never 0: "no `io.stat`" means the I/O controller is
//! off there, not that nothing was read.

use std::fs;
use std::path::{Path, PathBuf};

/// One pressure file's `avg10` figures, `some` and `full`.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Waits {
    pub some: f32,
    pub full: f32,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Cgroup {
    /// Below the cgroup root: `system.slice/NetworkManager.service`.
    pub path: String,
    /// CPU time over its life, microseconds; user and kernel parts.
    pub cpu_usec: Option<u64>,
    pub user_usec: Option<u64>,
    pub system_usec: Option<u64>,
    pub memory: Option<u64>,
    pub memory_peak: Option<u64>,
    /// `None` is "no limit" when the file says `max`.
    pub memory_max: Option<u64>,
    pub swap: Option<u64>,
    pub pids: Option<u64>,
    pub pids_max: Option<u64>,
    /// Times its memory limit was hit, and processes the kernel killed
    /// for it.
    pub memory_max_hits: Option<u64>,
    pub oom_kills: Option<u64>,
    pub cpu_pressure: Option<Waits>,
    pub memory_pressure: Option<Waits>,
    pub io_pressure: Option<Waits>,
}

fn number(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// A limit: a number, or `max` for none.
fn limit(path: &Path) -> Option<u64> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text == "max" {
        None
    } else {
        text.parse().ok()
    }
}

/// `key value` lines (`cpu.stat`, `memory.events`).
fn keyed(path: &Path, key: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(' ')?.trim().parse().ok())
}

/// `some avg10=… ` / `full avg10=…`.
pub fn parse_waits(text: &str) -> Option<Waits> {
    let avg10 = |kind: &str| -> Option<f32> {
        let line = text.lines().find(|l| l.starts_with(kind))?;
        line.split_whitespace().find_map(|w| w.strip_prefix("avg10="))?.parse().ok()
    };
    Some(Waits { some: avg10("some")?, full: avg10("full").unwrap_or(0.0) })
}

fn waits(path: &Path) -> Option<Waits> {
    parse_waits(&fs::read_to_string(path).ok()?)
}

/// One cgroup's files.
pub fn read(root: &Path, dir: &Path) -> Cgroup {
    let at = |name: &str| dir.join(name);
    Cgroup {
        path: dir.strip_prefix(root).unwrap_or(dir).to_string_lossy().into_owned(),
        cpu_usec: keyed(&at("cpu.stat"), "usage_usec"),
        user_usec: keyed(&at("cpu.stat"), "user_usec"),
        system_usec: keyed(&at("cpu.stat"), "system_usec"),
        memory: number(&at("memory.current")),
        memory_peak: number(&at("memory.peak")),
        memory_max: limit(&at("memory.max")),
        swap: number(&at("memory.swap.current")),
        pids: number(&at("pids.current")),
        pids_max: limit(&at("pids.max")),
        memory_max_hits: keyed(&at("memory.events"), "max"),
        oom_kills: keyed(&at("memory.events"), "oom_kill"),
        cpu_pressure: waits(&at("cpu.pressure")),
        memory_pressure: waits(&at("memory.pressure")),
        io_pressure: waits(&at("io.pressure")),
    }
}

/// Every cgroup under `root`, parents before children, to `depth`
/// levels — deep enough for a user's apps (`user.slice/user-1000.slice/
/// user@1000.service/app.slice/…` is five).
pub fn all(root: &Path, depth: usize) -> Vec<Cgroup> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, level)) = stack.pop() {
        if level > 0 {
            out.push(read(root, &dir));
        }
        if level >= depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        let mut children: Vec<PathBuf> = entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
        children.sort();
        stack.extend(children.into_iter().rev().map(|c| (c, level + 1)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NetworkManager.service's own files on this machine, 2026-10-10.
    fn network_manager(root: &Path) -> PathBuf {
        let d = root.join("system.slice/NetworkManager.service");
        fs::create_dir_all(&d).unwrap();
        let w = |n: &str, t: &str| fs::write(d.join(n), t).unwrap();
        w("cpu.stat", "usage_usec 411560\nuser_usec 263250\nsystem_usec 148309\nnice_usec 0\n");
        w("memory.current", "15519744\n");
        w("memory.max", "max\n");
        w("memory.peak", "16556032\n");
        w("memory.swap.current", "0\n");
        w("memory.events", "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\noom_group_kill 0\n");
        w("pids.current", "5\n");
        w("pids.max", "74198\n");
        w("cpu.pressure", "some avg10=0.00 avg60=0.00 avg300=0.00 total=3935\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=3932\n");
        w("io.pressure", "some avg10=1.50 avg60=0.00 avg300=0.00 total=4803\nfull avg10=0.75 avg60=0.00 avg300=0.00 total=4803\n");
        d
    }

    #[test]
    fn a_service_reads_its_use_limits_and_waits() {
        let t = tempfile::tempdir().unwrap();
        let d = network_manager(t.path());
        let c = read(t.path(), &d);
        assert_eq!(c.path, "system.slice/NetworkManager.service");
        assert_eq!((c.cpu_usec, c.user_usec, c.system_usec), (Some(411560), Some(263250), Some(148309)));
        assert_eq!((c.memory, c.memory_peak, c.memory_max), (Some(15519744), Some(16556032), None), "max is no limit");
        assert_eq!((c.pids, c.pids_max, c.oom_kills), (Some(5), Some(74198), Some(0)));
        assert_eq!(c.io_pressure, Some(Waits { some: 1.5, full: 0.75 }));
        assert_eq!(c.memory_pressure, None, "no file is not zero");
    }

    #[test]
    fn the_walk_puts_parents_first_and_stops_at_its_depth() {
        let t = tempfile::tempdir().unwrap();
        network_manager(t.path());
        fs::create_dir_all(t.path().join("user.slice/user-1000.slice/user@1000.service/app.slice/deep")).unwrap();
        let paths: Vec<String> = all(t.path(), 3).into_iter().map(|c| c.path).collect();
        assert_eq!(paths, ["system.slice", "system.slice/NetworkManager.service", "user.slice", "user.slice/user-1000.slice", "user.slice/user-1000.slice/user@1000.service"]);
    }

    #[test]
    fn the_real_cgroup_tree_has_this_machines_services() {
        let all = all(Path::new("/sys/fs/cgroup"), 6);
        assert!(all.iter().any(|c| c.path == "system.slice" && c.cpu_usec.is_some()));
        assert!(all.len() > 10);
    }
}

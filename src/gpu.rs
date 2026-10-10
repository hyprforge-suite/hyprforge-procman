//! GPU use per process, from the DRM fdinfo the kernel keeps for every
//! open GPU descriptor (`drm-engine-*` busy nanoseconds, `drm-resident-vram`).
//!
//! Two descriptors can be one client — the same `drm-client-id` on the
//! same device — and adding both would count its work twice, so clients
//! are de-duplicated before anything is summed.
//!
//! Finding which processes hold a GPU means reading every descriptor of
//! every process, so [`Scan`] does it only every few refreshes and reads
//! just the known GPU descriptors in between. A process that opens the
//! GPU between scans shows up at the next one.
//!
//! Only processes this user may read are seen; another user's GPU use
//! waits on the helper.

use crate::sample::Id;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// One process's GPU counters at one reading.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DrmUse {
    /// Busy nanoseconds per engine (`gfx`, `compute`, `dma`, …), summed
    /// over its clients.
    pub engines: HashMap<String, u64>,
    /// Video memory it holds, in bytes.
    pub vram: u64,
}

/// The `drm-*` lines of one fdinfo: (device, client id) and its figures.
fn parse(text: &str) -> Option<((String, u64), DrmUse)> {
    let mut pdev = String::new();
    let mut client = None;
    let mut u = DrmUse::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key.trim() {
            "drm-pdev" => pdev = value.to_owned(),
            "drm-client-id" => client = value.parse().ok(),
            "drm-resident-vram" => u.vram = kib(value)?,
            k => {
                if let Some(engine) = k.strip_prefix("drm-engine-") {
                    // `drm-engine-capacity-*` is how many there are, not time.
                    if !engine.starts_with("capacity") {
                        let ns = value.strip_suffix("ns").unwrap_or(value).trim().parse().ok()?;
                        u.engines.insert(engine.to_owned(), ns);
                    }
                }
            }
        }
    }
    Some(((pdev, client?), u))
}

/// `319496 KiB` → bytes.
fn kib(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let n: u64 = parts.next()?.parse().ok()?;
    let unit = parts.next().unwrap_or("KiB");
    Some(match unit {
        "MiB" => n * 1024 * 1024,
        "GiB" => n * 1024 * 1024 * 1024,
        _ => n * 1024,
    })
}

/// Which descriptors of which processes are GPU ones.
#[derive(Debug, Default)]
pub struct Scan {
    fds: HashMap<i32, Vec<String>>,
    reads: u32,
}

/// How many readings between full scans of every descriptor.
const RESCAN_EVERY: u32 = 5;

impl Scan {
    /// This reading's use for every process seen holding a GPU.
    pub fn read(&mut self, root: &Path, pids: &[i32]) -> HashMap<i32, DrmUse> {
        if self.reads.is_multiple_of(RESCAN_EVERY) {
            self.fds = pids.iter().filter_map(|&pid| Some((pid, gpu_fds(root, pid)?))).filter(|(_, fds)| !fds.is_empty()).collect();
        }
        self.reads = self.reads.wrapping_add(1);
        let mut out = HashMap::new();
        for (&pid, fds) in &self.fds {
            let mut seen = HashSet::new();
            let mut total = DrmUse::default();
            for fd in fds {
                let Ok(text) = fs::read_to_string(root.join(pid.to_string()).join("fdinfo").join(fd)) else { continue };
                let Some((client, u)) = parse(&text) else { continue };
                if !seen.insert(client) {
                    continue;
                }
                for (engine, ns) in u.engines {
                    *total.engines.entry(engine).or_default() += ns;
                }
                total.vram += u.vram;
            }
            if !seen.is_empty() {
                out.insert(pid, total);
            }
        }
        out
    }
}

/// The descriptors of `pid` that are a GPU device, or `None` when not
/// this user's to read.
fn gpu_fds(root: &Path, pid: i32) -> Option<Vec<String>> {
    let dir = fs::read_dir(root.join(pid.to_string()).join("fd")).ok()?;
    Some(
        dir.flatten()
            .filter(|e| fs::read_link(e.path()).is_ok_and(|t| t.starts_with("/dev/dri/")))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
    )
}

/// Each process's GPU use since `before`, in percent of its busiest
/// engine — Task Manager's reading, since engines run side by side and
/// adding them would pass 100.
pub fn shares(before: &HashMap<i32, DrmUse>, now: &HashMap<i32, DrmUse>, ids: &HashMap<i32, Id>, seconds: f64) -> HashMap<Id, f32> {
    let mut out = HashMap::new();
    if seconds <= 0.0 {
        return out;
    }
    for (pid, u) in now {
        let (Some(b), Some(id)) = (before.get(pid), ids.get(pid)) else { continue };
        let busiest = u
            .engines
            .iter()
            .map(|(engine, ns)| ns.saturating_sub(b.engines.get(engine).copied().unwrap_or(*ns)))
            .max()
            .unwrap_or(0);
        out.insert(*id, (busiest as f64 / (seconds * 1e9) * 100.0).min(100.0) as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drm lines of Hyprland's render descriptor on this machine.
    const FDINFO: &str = "pos:\t0\nflags:\t02100002\nmnt_id:\t26\nino:\t1061\ndrm-driver:\tamdgpu\ndrm-client-id:\t27\ndrm-pdev:\t0000:c3:00.0\ndrm-total-vram:\t351636 KiB\ndrm-resident-vram:\t319496 KiB\ndrm-memory-vram:\t319496 KiB\ndrm-memory-gtt: \t46496 KiB\ndrm-engine-gfx:\t472202451596 ns\ndrm-engine-compute:\t241757698 ns\n";

    #[test]
    fn an_fdinfo_reads_its_client_engines_and_video_memory() {
        let ((pdev, client), u) = parse(FDINFO).unwrap();
        assert_eq!((pdev.as_str(), client), ("0000:c3:00.0", 27));
        assert_eq!(u.engines["gfx"], 472202451596);
        assert_eq!(u.vram, 319496 * 1024);
    }

    #[test]
    fn two_descriptors_of_one_client_count_once() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("7");
        std::fs::create_dir_all(dir.join("fd")).unwrap();
        std::fs::create_dir_all(dir.join("fdinfo")).unwrap();
        for fd in ["40", "41"] {
            std::os::unix::fs::symlink("/dev/dri/renderD129", dir.join("fd").join(fd)).unwrap();
            std::fs::write(dir.join("fdinfo").join(fd), FDINFO).unwrap();
        }
        let use_ = Scan::default().read(t.path(), &[7]);
        assert_eq!(use_[&7].engines["gfx"], 472202451596, "not twice");
    }

    #[test]
    fn a_share_is_the_busiest_engine_over_the_time_passed() {
        let mut before = DrmUse::default();
        before.engines.insert("gfx".into(), 0);
        before.engines.insert("compute".into(), 0);
        let mut now = before.clone();
        now.engines.insert("gfx".into(), 250_000_000);
        now.engines.insert("compute".into(), 900_000_000);
        let id = Id { pid: 7, start: 1 };
        let shares = shares(&HashMap::from([(7, before)]), &HashMap::from([(7, now)]), &HashMap::from([(7, id)]), 1.0);
        assert!((shares[&id] - 90.0).abs() < 1e-3);
    }
}

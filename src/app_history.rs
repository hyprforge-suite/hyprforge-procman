//! App history: what each app has cost, day by day, for thirty days.
//!
//! Task Manager keeps this with a service that always runs. Apps
//! launched from Hyprland share one cgroup, so the kernel keeps no
//! per-app totals to read back later; this records them itself, **while
//! the window is open**, and says so on the tab — a figure for "today"
//! that silently covers only the minutes someone happened to look would
//! be a number made up by omission.
//!
//! An app is keyed by its window class (`firefox`), the name it has
//! across launches; its CPU, disk and GPU are its whole process tree's,
//! as the Processes tab adds them.
//!
//! The file is `history.toml` in the suite's data folder. One that
//! exists and will not parse is never written over: the tab says so, and
//! recording waits — the suite's CLAUDE.md on never collapsing "could not
//! read" into "nothing there".

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How many days are kept.
pub const DAYS: usize = 30;

/// One app's use on one day.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Use {
    /// CPU time, in seconds of one core.
    pub cpu_seconds: f64,
    pub disk_bytes: f64,
    /// Time its busiest GPU engine was busy, in seconds.
    pub gpu_seconds: f64,
    pub peak_memory: u64,
    /// How long it was seen running while this was recording.
    pub seconds_open: f64,
}

impl Use {
    fn add(&mut self, other: &Use) {
        self.cpu_seconds += other.cpu_seconds;
        self.disk_bytes += other.disk_bytes;
        self.gpu_seconds += other.gpu_seconds;
        self.peak_memory = self.peak_memory.max(other.peak_memory);
        self.seconds_open += other.seconds_open;
    }
}

/// The file: days by date (`2026-10-10`), apps by class.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub days: BTreeMap<String, BTreeMap<String, Use>>,
    /// How long recording ran each day — what "today" actually covers.
    #[serde(default)]
    pub recorded: BTreeMap<String, f64>,
}

pub fn path() -> PathBuf {
    hyprforge_paths::data_home().join("hyprforge").join("procman-history.toml")
}

/// A missing file is a first run; one that will not parse is an error.
pub fn load(path: &Path) -> Result<History, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{} couldn't be read: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(History::default()),
        Err(e) => Err(format!("{} couldn't be read: {e}", path.display())),
    }
}

pub fn save(path: &Path, history: &History) -> Result<(), String> {
    let text = toml::to_string(history).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    hyprforge_paths::write_atomic(path, &text).map_err(|e| e.to_string())
}

/// What to call a window class: its launcher entry's `Name=` when one is
/// installed under that id (exactly, or ignoring case), else the class.
pub fn app_name(db: &hyprforge_mime::MimeDb, class: &str) -> String {
    let wanted = format!("{class}.desktop");
    db.app(&wanted)
        .or_else(|| db.all_apps().into_iter().find(|a| a.id.eq_ignore_ascii_case(&wanted)))
        .map(|a| a.name.clone())
        .unwrap_or_else(|| class.to_owned())
}

/// One app's slice of one reading.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub class: String,
    /// Percent of the whole machine.
    pub cpu: Option<f32>,
    /// Bytes a second.
    pub disk: Option<f64>,
    /// Percent of its busiest GPU engine.
    pub gpu: Option<f32>,
    pub memory: u64,
}

impl History {
    /// Adds `seconds` of `samples` to `day`, and keeps the last [`DAYS`].
    /// `cores` turns a share of the machine into core-seconds.
    pub fn record(&mut self, day: &str, samples: &[Sample], seconds: f64, cores: usize) {
        if seconds <= 0.0 {
            return;
        }
        *self.recorded.entry(day.to_owned()).or_default() += seconds;
        let apps = self.days.entry(day.to_owned()).or_default();
        for s in samples {
            let u = Use {
                cpu_seconds: f64::from(s.cpu.unwrap_or(0.0)) / 100.0 * cores as f64 * seconds,
                disk_bytes: s.disk.unwrap_or(0.0) * seconds,
                gpu_seconds: f64::from(s.gpu.unwrap_or(0.0)) / 100.0 * seconds,
                peak_memory: s.memory,
                seconds_open: seconds,
            };
            apps.entry(s.class.clone()).or_default().add(&u);
        }
        while self.days.len() > DAYS {
            let oldest = self.days.keys().next().cloned().expect("not empty");
            self.days.remove(&oldest);
            self.recorded.remove(&oldest);
        }
    }

    /// Every app's total over the last `days` days ending `today`, the
    /// biggest CPU user first; and how long recording covered them.
    pub fn totals(&self, today: &str, days: usize) -> (Vec<(String, Use)>, f64) {
        let wanted: Vec<&String> = self.days.keys().filter(|d| d.as_str() <= today).rev().take(days).collect();
        let mut sum: BTreeMap<String, Use> = BTreeMap::new();
        let mut covered = 0.0;
        for day in &wanted {
            for (class, u) in &self.days[*day] {
                sum.entry(class.clone()).or_default().add(u);
            }
            covered += self.recorded.get(*day).copied().unwrap_or(0.0);
        }
        let mut out: Vec<(String, Use)> = sum.into_iter().collect();
        out.sort_by(|a, b| b.1.cpu_seconds.total_cmp(&a.1.cpu_seconds));
        (out, covered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(class: &str, cpu: f32, memory: u64) -> Sample {
        Sample { class: class.into(), cpu: Some(cpu), disk: Some(1000.0), gpu: Some(50.0), memory }
    }

    #[test]
    fn a_reading_adds_core_seconds_bytes_and_keeps_the_peak() {
        let mut h = History::default();
        // 12.5 % of a 16-core machine for 2 s is 4 core-seconds.
        h.record("2026-10-10", &[sample("firefox", 12.5, 100)], 2.0, 16);
        h.record("2026-10-10", &[sample("firefox", 0.0, 300)], 2.0, 16);
        let u = h.days["2026-10-10"]["firefox"];
        assert_eq!((u.cpu_seconds, u.disk_bytes, u.gpu_seconds, u.peak_memory, u.seconds_open), (4.0, 4000.0, 2.0, 300, 4.0));
        assert_eq!(h.recorded["2026-10-10"], 4.0);
    }

    #[test]
    fn only_thirty_days_are_kept() {
        let mut h = History::default();
        for d in 1..=35 {
            h.record(&format!("2026-09-{d:02}"), &[sample("a", 1.0, 1)], 1.0, 1);
        }
        assert_eq!(h.days.len(), DAYS);
        assert!(!h.days.contains_key("2026-09-01"));
    }

    #[test]
    fn totals_add_up_the_days_asked_for_biggest_first() {
        let mut h = History::default();
        h.record("2026-10-09", &[sample("blender", 50.0, 1), sample("kitty", 1.0, 1)], 10.0, 1);
        h.record("2026-10-10", &[sample("kitty", 1.0, 1)], 10.0, 1);
        let (today, covered) = h.totals("2026-10-10", 1);
        assert_eq!(today.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(), ["kitty"]);
        assert_eq!(covered, 10.0);
        let (week, _) = h.totals("2026-10-10", 7);
        assert_eq!(week[0].0, "blender");
        assert!((week[1].1.cpu_seconds - 0.2).abs() < 1e-9);
    }

    #[test]
    fn a_missing_file_is_empty_and_a_broken_one_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("procman-history.toml");
        assert_eq!(load(&p).unwrap(), History::default());
        let mut h = History::default();
        h.record("2026-10-10", &[sample("a", 1.0, 1)], 1.0, 1);
        save(&p, &h).unwrap();
        assert_eq!(load(&p).unwrap(), h, "round trip");
        std::fs::write(&p, "days = = broken").unwrap();
        assert!(load(&p).is_err());
    }
}

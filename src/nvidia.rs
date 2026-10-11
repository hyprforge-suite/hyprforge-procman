//! An NVIDIA GPU, which says nothing about itself in sysfs — no busy
//! figure, no sensors — and nothing per process in fdinfo. `nvidia-smi`
//! is the one source, with two rules of its own:
//!
//! - **It wakes the card.** On a laptop the NVIDIA GPU sleeps (runtime
//!   PM, `power/runtime_status: suspended`) to save the battery, and any
//!   query powers it up. So it is asked only when sysfs already says the
//!   card is awake, only while the Performance tab is open, and no more
//!   than every few seconds — a process manager that kept the GPU awake
//!   to report on it would be the thing draining the battery.
//! - **It fails in ways worth saying.** The commonest, after a driver
//!   update without a restart, is "Driver/library version mismatch"
//!   (seen on this machine 2026-10-10: module 615.71.09, libraries
//!   615.78); that is shown as what to do, not as a missing GPU.

use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub name: String,
    /// Percent busy, and percent of its memory bandwidth in use.
    pub busy: Option<f32>,
    pub memory_busy: Option<f32>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    pub celsius: Option<f32>,
    pub watts: Option<f32>,
    pub clock_mhz: Option<u32>,
    pub max_clock_mhz: Option<u32>,
    pub fan_percent: Option<f32>,
    /// `P0` (flat out) to `P8`/`P12` (idle).
    pub pstate: String,
}

/// What `nvidia-smi` is asked, in this order.
const FIELDS: &str = "name,utilization.gpu,utilization.memory,memory.used,memory.total,temperature.gpu,power.draw,clocks.gr,clocks.max.gr,fan.speed,pstate";

/// One `--format=csv,noheader,nounits` line, in `FIELDS`' order. A
/// laptop GPU answers `[N/A]` for a fan it does not control, and older
/// cards for power; those are unknown, never zero.
///
/// The format is NVIDIA's documented one; it could not be sampled from
/// this machine, whose tools refused to run (see the module doc).
pub fn parse(line: &str) -> Option<Reading> {
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    if f.len() < 11 {
        return None;
    }
    let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
    let mib = |i: usize| num(i).map(|m| (m * 1024.0 * 1024.0) as u64);
    Some(Reading {
        name: f[0].to_owned(),
        busy: num(1).map(|v| v as f32),
        memory_busy: num(2).map(|v| v as f32),
        vram_used: mib(3),
        vram_total: mib(4),
        celsius: num(5).map(|v| v as f32),
        watts: num(6).map(|v| v as f32),
        clock_mhz: num(7).map(|v| v as u32),
        max_clock_mhz: num(8).map(|v| v as u32),
        fan_percent: num(9).map(|v| v as f32),
        pstate: f[10].to_owned(),
    })
}

/// What `nvidia-smi` said when it would not answer, in words.
pub fn explain(stdout: &str, stderr: &str) -> String {
    let text = format!("{stdout}\n{stderr}");
    if text.contains("Driver/library version mismatch") {
        let library = text.lines().find_map(|l| l.strip_prefix("NVML library version:")).map(str::trim);
        let driver = std::fs::read_to_string("/proc/driver/nvidia/version")
            .ok()
            .and_then(|v| v.split_whitespace().skip_while(|w| *w != "x86_64").nth(1).map(str::to_owned));
        return match (driver, library) {
            (Some(d), Some(l)) => format!("NVIDIA's tools ({l}) don't match the driver that's loaded ({d}). Restart to finish the driver update."),
            // The tools said their own version; the driver's can be
            // unreadable (or, as on CI, absent) without making that wrong.
            (None, Some(l)) => format!("NVIDIA's tools ({l}) don't match the driver that's loaded. Restart to finish the driver update."),
            _ => "NVIDIA's tools don't match the driver that's loaded. Restart to finish the driver update.".to_string(),
        };
    }
    if text.contains("couldn't communicate with the NVIDIA driver") {
        return "The NVIDIA driver isn't loaded.".to_string();
    }
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("nvidia-smi gave no answer").to_string()
}

/// Whether a refusal will last until the machine restarts — the
/// version mismatch. Each failed ask writes an "API mismatch" line to the
/// kernel log, so asking again every few seconds only fills the journal.
pub fn lasts_until_restart(said: &str) -> bool {
    said.contains("Restart to finish the driver update") || said.contains("isn't installed")
}

/// Asks the card, bounded. Never call this for a card that is asleep.
pub fn query() -> Result<Reading, String> {
    let mut command = Command::new("nvidia-smi");
    command.args([&format!("--query-gpu={FIELDS}"), "--format=csv,noheader,nounits"]);
    let out = hyprforge_process::output(&mut command, Duration::from_secs(3)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "nvidia-smi isn't installed, so this card can't be asked.".to_string(),
        _ => format!("nvidia-smi didn't answer: {e}"),
    })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(explain(&stdout, &String::from_utf8_lossy(&out.stderr)));
    }
    stdout.lines().next().and_then(parse).ok_or_else(|| "nvidia-smi's answer didn't parse.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_keeps_unknown_fields_unknown() {
        let r = parse("NVIDIA GeForce RTX 5070 Laptop GPU, 7, 2, 512, 8151, 46, 12.40, 210, 3090, [N/A], P8").unwrap();
        assert_eq!(r.name, "NVIDIA GeForce RTX 5070 Laptop GPU");
        assert_eq!((r.busy, r.celsius, r.clock_mhz), (Some(7.0), Some(46.0), Some(210)));
        assert_eq!(r.vram_total, Some(8151 * 1024 * 1024));
        assert_eq!(r.fan_percent, None, "[N/A] is unknown, not 0");
        assert_eq!(r.pstate, "P8");
        assert!(parse("garbage").is_none());
    }

    /// This machine's own failure, word for word.
    #[test]
    fn a_version_mismatch_says_to_restart() {
        let said = explain("Failed to initialize NVML: Driver/library version mismatch\nNVML library version: 615.78\n", "");
        assert!(said.contains("Restart to finish the driver update"), "{said}");
        assert!(said.contains("615.78"), "{said}");
        assert!(lasts_until_restart(&said), "and is not asked again this session");
    }
}

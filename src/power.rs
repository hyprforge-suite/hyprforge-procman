//! Which programs the battery is paying for: powertop's question, asked
//! of what this window already measures.
//!
//! Nothing on the machine says how many watts one process draws. What
//! is measured is the whole package — on an APU, amdgpu's `PPT`, the CPU
//! and GPU together — and what each process did with it: its CPU time,
//! its share of the busiest GPU engine, and how often it woke a core.
//! So the figure is an **estimate**, and every place that shows it says
//! so.
//!
//! **Only the draw above idle is shared out.** A package draws a few
//! watts doing nothing at all; handing that to whatever happened to run
//! would make a terminal at 1 % CPU look like it costs 5 W. The floor is
//! measured, not guessed: the least the package has drawn while the
//! window watched ([`Floor`]). Until it has seen a quiet moment the
//! floor is high and the estimates low, which errs the right way.
//!
//! **The weight is CPU time plus GPU time**, a core busy for a second
//! counting the same as the busiest engine busy for a second. Wakeups
//! are shown beside the estimate rather than folded into it: what one
//! costs depends on how deeply the core had gone to sleep, which nothing
//! here can see, and a number that silently mixed them would hide which
//! of the two a program is doing.

use crate::sample::Id;
use std::collections::HashMap;

/// The least package draw seen, in watts.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Floor(Option<f32>);

impl Floor {
    pub fn see(&mut self, watts: f32) {
        if watts.is_finite() && watts >= 0.0 {
            self.0 = Some(self.0.map_or(watts, |f| f.min(watts)));
        }
    }

    pub fn watts(&self) -> Option<f32> {
        self.0
    }
}

/// What one process did between two readings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Load {
    /// Percent of the whole machine, as the CPU column shows it.
    pub cpu_percent: f32,
    /// Percent of its busiest GPU engine, when it has one open.
    pub gpu_percent: Option<f32>,
}

/// Each process's share of what the machine did, in percent (they sum
/// to 100): CPU time plus GPU time. `cores` turns the CPU column's
/// whole-machine percent back into cores' worth. Empty when nothing was
/// busy. This needs no watts, so it is what a plugged-in machine shows.
pub fn shares(loads: &HashMap<Id, Load>, cores: usize) -> HashMap<Id, f32> {
    let weight = |l: &Load| l.cpu_percent.max(0.0) / 100.0 * cores as f32 + l.gpu_percent.unwrap_or(0.0).max(0.0) / 100.0;
    let total: f32 = loads.values().map(weight).sum();
    if total <= 0.0 {
        return HashMap::new();
    }
    loads.iter().map(|(id, l)| (*id, weight(l) / total * 100.0)).filter(|(_, s)| *s > 0.0).collect()
}

/// Each process's estimated watts: its [`shares`] of the draw above the
/// floor. Empty when there is nothing above it.
pub fn estimate(shares: &HashMap<Id, f32>, draw: f32, floor: Floor) -> HashMap<Id, f32> {
    let above = (draw - floor.watts().unwrap_or(draw)).max(0.0);
    if above <= 0.0 {
        return HashMap::new();
    }
    shares.iter().map(|(id, s)| (*id, above * s / 100.0)).collect()
}

/// How long [`smooth`] averages over. One reading is mostly noise: the
/// tray daemon here alternated between 115 and 7 wakeups a second in
/// five-second windows — a burst every ten seconds — and a one-second
/// rate called it the machine's worst on an unlucky refresh.
pub const AVERAGE_OVER_SECONDS: f64 = 10.0;

/// Fold this reading's rates into a running average over about
/// [`AVERAGE_OVER_SECONDS`]: an exponential one, so a reading weighs by
/// how much time it covers whatever the refresh interval. A process
/// that is gone drops out; one that is new starts at its first rate.
pub fn smooth<V: Copy + Into<f64> + From<f32>>(average: &mut HashMap<Id, V>, now: &HashMap<Id, V>, seconds: f64) {
    let keep = (-seconds.max(0.0) / AVERAGE_OVER_SECONDS).exp();
    average.retain(|id, _| now.contains_key(id));
    for (id, v) in now {
        let next = match average.get(id) {
            Some(old) => (*old).into() * keep + (*v).into() * (1.0 - keep),
            None => (*v).into(),
        };
        average.insert(*id, V::from(next as f32));
    }
}

/// The most power-hungry first, at most `n`, by estimate (or share) and then by
/// wakeups — so a program that does almost nothing but wake up all the
/// time still makes the list when the estimates are level.
pub fn hungriest(watts: &HashMap<Id, f32>, wakeups: &HashMap<Id, f64>, n: usize) -> Vec<Id> {
    let mut ids: Vec<Id> = watts.keys().chain(wakeups.keys()).copied().collect();
    ids.sort();
    ids.dedup();
    ids.sort_by(|a, b| {
        let w = |id: &Id| watts.get(id).copied().unwrap_or(0.0);
        let k = |id: &Id| wakeups.get(id).copied().unwrap_or(0.0);
        w(b).total_cmp(&w(a)).then(k(b).total_cmp(&k(a)))
    });
    ids.truncate(n);
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(pid: i32) -> Id {
        Id { pid, start: 1 }
    }

    /// A 5 W idle package now at 15 W: the 10 W above idle is shared, and
    /// a process with twice the CPU time gets twice the share.
    #[test]
    fn only_the_draw_above_idle_is_shared_by_cpu_and_gpu_time() {
        let mut floor = Floor::default();
        floor.see(9.0);
        floor.see(5.0);
        floor.see(7.0);
        assert_eq!(floor.watts(), Some(5.0));
        // On 8 cores, 25 % of the machine is two cores' worth.
        let loads = HashMap::from([
            (id(1), Load { cpu_percent: 25.0, gpu_percent: None }),
            (id(2), Load { cpu_percent: 12.5, gpu_percent: None }),
            (id(3), Load { cpu_percent: 0.0, gpu_percent: Some(100.0) }),
        ]);
        let s = shares(&loads, 8);
        assert!((s.values().sum::<f32>() - 100.0).abs() < 1e-3);
        let w = estimate(&s, 15.0, floor);
        assert!((w[&id(1)] - 5.0).abs() < 1e-4, "{w:?}");
        assert!((w[&id(2)] - 2.5).abs() < 1e-4);
        assert!((w[&id(3)] - 2.5).abs() < 1e-4, "a busy engine counts as a busy core");
        assert!((w.values().sum::<f32>() - 10.0).abs() < 1e-4);
    }

    /// The first reading is its own floor: nothing above it, nothing
    /// shared, rather than the whole package handed to whatever ran.
    #[test]
    fn with_no_quiet_moment_seen_nothing_is_blamed() {
        let mut floor = Floor::default();
        floor.see(12.0);
        let loads = HashMap::from([(id(1), Load { cpu_percent: 1.0, gpu_percent: None })]);
        assert!(estimate(&shares(&loads, 8), 12.0, floor).is_empty());
    }

    /// A burst every ten seconds averages to what it costs, rather than
    /// whichever refresh the burst happened to land in.
    #[test]
    fn a_bursty_rate_settles_to_its_average() {
        let mut avg: HashMap<Id, f64> = HashMap::new();
        for second in 0..120 {
            let rate = if second % 10 == 0 { 550.0 } else { 0.0 };
            smooth(&mut avg, &HashMap::from([(id(1), rate)]), 1.0);
        }
        let settled = avg[&id(1)];
        assert!((20.0..120.0).contains(&settled), "about 55 a second, not 550 or 0: {settled}");
        smooth(&mut avg, &HashMap::new(), 1.0);
        assert!(avg.is_empty(), "a process that is gone drops out");
    }

    #[test]
    fn a_program_that_only_wakes_still_makes_the_list() {
        let watts = HashMap::from([(id(1), 2.0), (id(2), 0.5)]);
        let wakeups = HashMap::from([(id(3), 900.0), (id(2), 10.0)]);
        assert_eq!(hungriest(&watts, &wakeups, 3), vec![id(1), id(2), id(3)]);
        assert_eq!(hungriest(&watts, &wakeups, 2), vec![id(1), id(2)]);
    }
}

//! A minute of readings, for the graphs.

use std::collections::{BTreeMap, VecDeque};

/// How many readings a graph keeps: a minute at the default once a
/// second.
pub const LEN: usize = 60;

/// The last [`LEN`] values of one measure, oldest first, as read — a
/// fraction for a share, bytes a second for a rate. The graph is given
/// its ceiling separately ([`Series::ceiling`] for a rate), so a share
/// is drawn against 1 and a rate against what the minute actually saw.
#[derive(Debug, Clone, Default)]
pub struct Series {
    values: VecDeque<f32>,
}

impl Series {
    pub fn push(&mut self, value: f32) {
        if self.values.len() == LEN {
            self.values.pop_front();
        }
        self.values.push_back(if value.is_finite() { value.max(0.0) } else { 0.0 });
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.values.iter().copied()
    }

    pub fn last(&self) -> Option<f32> {
        self.values.back().copied()
    }

    /// A ceiling for a rate: the minute's peak, rounded up to 1, 2 or 5
    /// times a power of ten, so the scale moves in steps a person can
    /// read rather than with every reading. Never below `floor`, so a
    /// quiet link does not draw its noise as a mountain.
    pub fn ceiling(&self, floor: f32) -> f32 {
        let peak = self.values.iter().copied().fold(0.0f32, f32::max).max(floor);
        let power = 10f32.powf(peak.log10().floor());
        [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * power).find(|c| *c >= peak).unwrap_or(peak)
    }
}

/// Every graph the Performance tab draws, by resource.
#[derive(Debug, Clone, Default)]
pub struct History {
    pub cpu: Series,
    pub cores: Vec<Series>,
    pub memory: Series,
    pub swap: Series,
    /// Active time, by disk name.
    pub disks: BTreeMap<String, Series>,
    /// Received plus sent, bytes a second, by interface.
    pub nets: BTreeMap<String, Series>,
    /// Busy, by card.
    pub gpus: BTreeMap<String, Series>,
    pub battery: Series,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_series_keeps_the_last_minute_and_never_goes_negative() {
        let mut s = Series::default();
        for i in 0..(LEN + 5) {
            s.push(i as f32);
        }
        assert_eq!(s.values().len(), LEN);
        s.push(-3.0);
        assert_eq!(s.last(), Some(0.0));
        s.push(f32::NAN);
        assert_eq!(s.last(), Some(0.0));
    }

    #[test]
    fn a_rates_ceiling_steps_through_one_two_five() {
        let mut s = Series::default();
        s.push(130.0);
        assert_eq!(s.ceiling(1.0), 200.0);
        s.push(4100.0);
        assert_eq!(s.ceiling(1.0), 5000.0);
        let quiet = Series::default();
        assert_eq!(quiet.ceiling(1024.0), 2000.0, "the floor, rounded up");
    }
}

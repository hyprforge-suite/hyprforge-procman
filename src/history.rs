//! A minute of readings, for the graphs.

use std::collections::VecDeque;

/// How many readings a graph keeps: a minute at the default once a
/// second.
pub const LEN: usize = 60;

/// The last [`LEN`] values of one measure, each from 0 to 1, oldest
/// first.
#[derive(Debug, Clone, Default)]
pub struct Series {
    values: VecDeque<f32>,
}

impl Series {
    pub fn push(&mut self, value: f32) {
        if self.values.len() == LEN {
            self.values.pop_front();
        }
        self.values.push_back(value.clamp(0.0, 1.0));
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.values.iter().copied()
    }

    pub fn last(&self) -> Option<f32> {
        self.values.back().copied()
    }
}

/// Every graph the Performance tab draws.
#[derive(Debug, Clone, Default)]
pub struct History {
    pub cpu: Series,
    pub cores: Vec<Series>,
    pub memory: Series,
    pub swap: Series,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_series_keeps_the_last_minute_and_clamps() {
        let mut s = Series::default();
        for i in 0..(LEN + 5) {
            s.push(i as f32);
        }
        assert_eq!(s.values().len(), LEN);
        s.push(-3.0);
        assert_eq!(s.last(), Some(0.0));
    }
}

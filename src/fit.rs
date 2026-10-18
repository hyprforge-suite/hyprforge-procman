//! Which layout a window's size gets.
//!
//! Three, drawn to the 2026-10-18 designs, for the sizes a tiled window
//! actually is on a 1600 × 1000 logical screen: the whole of it (about
//! 1560 × 930 after gaps), half (770 × 930) and a quarter (770 × 455).
//! Sizes are logical pixels — what iced reports and what a font scale is
//! applied on top of — so the thresholds are the same at any output scale.

/// What a window has room for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Everything: the sidebar open, the readouts in the header, the
    /// Overview's cards two and three to a row.
    Full,
    /// Narrow: the sidebar starts hidden and the header keeps only what
    /// it must; the Overview's cards stack, two to a row at most.
    Half,
    /// Short: a glance that does not scroll — four tiles, the cores and
    /// the three busiest processes.
    Quarter,
}

/// Narrower than this and the header's controls start squeezing the
/// search field, and the Overview's processor and memory cards stop
/// fitting side by side.
pub const NARROW: f32 = 1100.0;

/// Shorter than this and the Overview's first card alone is most of the
/// window — a glance that has to scroll is not a glance.
pub const SHORT: f32 = 640.0;

/// Shortness wins over narrowness: a window along the bottom half of a
/// screen is as wide as it but has no more room to scroll in than a
/// quarter.
pub fn fit(width: f32, height: f32) -> Fit {
    if height < SHORT {
        Fit::Quarter
    } else if width < NARROW {
        Fit::Half
    } else {
        Fit::Full
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three sizes the designs were drawn at, on this machine's
    /// 1600 × 1000 logical screen, each getting the layout drawn for it.
    #[test]
    fn each_tiled_size_gets_the_layout_drawn_for_it() {
        assert_eq!(fit(1560.0, 930.0), Fit::Full);
        assert_eq!(fit(770.0, 930.0), Fit::Half);
        assert_eq!(fit(770.0, 455.0), Fit::Quarter);
    }

    #[test]
    fn a_wide_short_window_is_a_glance_not_a_squashed_dashboard() {
        assert_eq!(fit(1560.0, 455.0), Fit::Quarter);
    }

    /// The window procman opens at (1100 × 720) is the full layout:
    /// first launch should show everything there is.
    #[test]
    fn the_window_it_opens_at_shows_everything() {
        assert_eq!(fit(1100.0, 720.0), Fit::Full);
    }
}

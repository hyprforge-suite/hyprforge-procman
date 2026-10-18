//! A history graph: a line over a faint fill, newest on the right.
//!
//! Drawn in the foreground colour, never the accent — the suite's
//! design system keeps the accent for "selected", and a busy CPU is not
//! a choice anyone made.

use crate::history::{Series, LEN};
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{mouse, Color, Point, Rectangle, Renderer, Size, Theme};
use std::time::{Duration, Instant};

/// When the newest reading was taken and how far apart readings are.
/// Given one, a graph scrolls continuously between readings instead of
/// jumping a whole step when each arrives: it is drawn from the same
/// readings, slid left by the share of the interval that has passed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub sampled: Instant,
    pub interval: Duration,
}

/// How often a moving graph is redrawn. A minute across a card is about
/// fifteen pixels a second, so at this rate a line moves half a pixel a
/// frame — smooth to the eye — at a third of a 90 Hz display's frames,
/// in a window whose job is noticing what costs CPU.
pub const FRAME: Duration = Duration::from_millis(33);

impl Motion {
    /// 0 as a reading arrives, 1 when the next is due — held there if it
    /// is late, so a slow reading stops the line rather than running it
    /// past the newest value.
    pub fn phase(&self, now: Instant) -> f32 {
        let interval = self.interval.as_secs_f32();
        if interval <= 0.0 {
            return 0.0;
        }
        (now.saturating_duration_since(self.sampled).as_secs_f32() / interval).clamp(0.0, 1.0)
    }
}

/// Where each of `count` readings sits across `width`, newest last.
///
/// One interval behind, on purpose: the newest reading enters at the
/// right edge plus a step and slides in as `phase` runs to 1, so the
/// edge itself is a point on the line between the last two readings
/// ([`edge`]). Drawn level with the newest instead, the end of the line
/// would snap up or down each time a reading landed — smooth sideways,
/// still jumping. Placed from the right, so a graph with only a few
/// readings fills in from where the newest is.
pub fn positions(count: usize, width: f32, phase: f32) -> Vec<f32> {
    let step = width / (LEN - 1) as f32;
    let x0 = width - step * count.saturating_sub(1) as f32 + step * (1.0 - phase);
    (0..count).map(|i| x0 + step * i as f32).collect()
}

/// The value at the right edge: between the last two readings, `phase`
/// of the way to the newest.
pub fn edge(values: &[f32], phase: f32) -> f32 {
    match values {
        [.., before, newest] => before + (newest - before) * phase,
        [only] => *only,
        [] => 0.0,
    }
}

pub struct Graph<'a> {
    pub series: &'a Series,
    /// What the top edge stands for: 1 for a share, a series' own
    /// ceiling for a rate.
    pub ceiling: f32,
    pub line: Color,
    pub grid: Color,
    /// Scroll between readings; `None` draws them where they fall, as a
    /// sparkline or a paused window does.
    pub motion: Option<Motion>,
}

impl<Message> canvas::Program<Message> for Graph<'_> {
    type State = ();

    /// While moving, ask for the next frame — only the canvas redraws,
    /// not the window's view — until the next reading is due. A hidden
    /// window gets no frames from the compositor, so it costs nothing.
    fn update(&self, _state: &mut (), event: &canvas::Event, _bounds: Rectangle, _cursor: mouse::Cursor) -> Option<canvas::Action<Message>> {
        let motion = self.motion?;
        match event {
            canvas::Event::Window(iced::window::Event::RedrawRequested(now)) if motion.phase(*now) < 1.0 => Some(canvas::Action::request_redraw_at(*now + FRAME)),
            _ => None,
        }
    }

    fn draw(&self, _state: &(), renderer: &Renderer, _theme: &Theme, bounds: Rectangle, _cursor: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let Size { width, height } = bounds.size();
        // Quarter lines, so a level can be read off without a scale.
        for i in 1..4 {
            let y = (height * i as f32 / 4.0).round() + 0.5;
            frame.stroke(&Path::line(Point::new(0.0, y), Point::new(width, y)), Stroke::default().with_color(self.grid).with_width(1.0));
        }
        let ceiling = if self.ceiling > 0.0 { self.ceiling } else { 1.0 };
        let values: Vec<f32> = self.series.values().map(|v| (v / ceiling).min(1.0)).collect();
        if values.len() >= 2 {
            // Still, the newest reading is at the edge (phase 1); moving,
            // it slides in from just past it.
            let phase = self.motion.map_or(1.0, |m| m.phase(Instant::now()));
            let xs = positions(values.len(), width, phase);
            let point = |i: usize, v: f32| Point::new(xs[i], height - v * (height - 1.0));
            let at_edge = Point::new(width, height - edge(&values, phase) * (height - 1.0));
            // Every reading up to the one before the newest, then the
            // edge — the newest itself is past it until it slides in.
            let shown = values.len() - 1;
            let line = Path::new(|b| {
                for (i, v) in values.iter().take(shown).enumerate() {
                    if i == 0 {
                        b.move_to(point(i, *v));
                    } else {
                        b.line_to(point(i, *v));
                    }
                }
                b.line_to(at_edge);
            });
            let area = Path::new(|b| {
                b.move_to(Point::new(xs[0], height));
                for (i, v) in values.iter().take(shown).enumerate() {
                    b.line_to(point(i, *v));
                }
                b.line_to(at_edge);
                b.line_to(Point::new(width, height));
                b.close();
            });
            frame.fill(&area, Color { a: 0.12, ..self.line });
            frame.stroke(&line, Stroke::default().with_color(self.line).with_width(1.5));
        }
        vec![frame.into_geometry()]
    }
}

/// One measure across a recorded day, as a share from 0 to 1: the
/// strained stretches banded, the stretch being summed shaded, and the
/// moment picked as a rule. Pressing or dragging anywhere on it picks the
/// moment under the pointer.
pub struct DayGraph<'a, Message> {
    pub timeline: &'a crate::recorded::Timeline,
    pub points: &'a [(i64, f32)],
    pub at: Option<i64>,
    pub window: Option<(i64, i64)>,
    pub line: Color,
    pub grid: Color,
    /// The strained stretches' band, and the moment's rule.
    pub strain: Color,
    pub mark: Color,
    pub on_pick: fn(i64) -> Message,
}

impl<Message> canvas::Program<Message> for DayGraph<'_, Message> {
    /// Whether a drag that started here is still going.
    type State = bool;

    fn update(&self, dragging: &mut bool, event: &canvas::Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<Message>> {
        // Followed past either edge once a drag is going, so a drag to
        // the end of the day lands on the end rather than stopping short.
        let pick = |x: f32| canvas::Action::publish((self.on_pick)(self.timeline.time_at((x - bounds.x) / bounds.width.max(1.0)))).and_capture();
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_over(bounds)?;
                *dragging = true;
                Some(pick(p.x))
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) if *dragging => Some(pick(position.x)),
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if *dragging => {
                *dragging = false;
                None
            }
            _ => None,
        }
    }

    fn mouse_interaction(&self, dragging: &bool, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        if *dragging || cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(&self, _state: &bool, renderer: &Renderer, _theme: &Theme, bounds: Rectangle, _cursor: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let Size { width, height } = bounds.size();
        let x = |at_ms: i64| self.timeline.fraction(at_ms) * width;
        for &(from, to) in &self.timeline.strained {
            frame.fill_rectangle(Point::new(x(from), 0.0), Size::new((x(to) - x(from)).max(1.0), height), Color { a: 0.18, ..self.strain });
        }
        if let Some((from, to)) = self.window {
            frame.fill_rectangle(Point::new(x(from), 0.0), Size::new((x(to) - x(from)).max(1.0), height), Color { a: 0.08, ..self.line });
        }
        for i in 1..4 {
            let y = (height * i as f32 / 4.0).round() + 0.5;
            frame.stroke(&Path::line(Point::new(0.0, y), Point::new(width, y)), Stroke::default().with_color(self.grid).with_width(1.0));
        }
        if self.points.len() >= 2 {
            let point = |&(at, v): &(i64, f32)| Point::new(x(at), height - v.clamp(0.0, 1.0) * (height - 1.0));
            let line = Path::new(|b| {
                b.move_to(point(&self.points[0]));
                for p in &self.points[1..] {
                    b.line_to(point(p));
                }
            });
            let area = Path::new(|b| {
                b.move_to(Point::new(x(self.points[0].0), height));
                for p in self.points {
                    b.line_to(point(p));
                }
                b.line_to(Point::new(x(self.points[self.points.len() - 1].0), height));
                b.close();
            });
            frame.fill(&area, Color { a: 0.12, ..self.line });
            frame.stroke(&line, Stroke::default().with_color(self.line).with_width(1.5));
        }
        if let Some(at) = self.at {
            let rule = x(at).round() + 0.5;
            frame.stroke(&Path::line(Point::new(rule, 0.0), Point::new(rule, height)), Stroke::default().with_color(self.mark).with_width(2.0));
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property that makes it look smooth: a reading about to be
    /// pushed past sits exactly where its successor appears once the new
    /// reading arrives — so nothing jumps when a reading lands.
    #[test]
    fn a_new_reading_lands_where_the_line_already_was() {
        let before = positions(10, 590.0, 1.0);
        let after = positions(11, 590.0, 0.0);
        for (b, a) in before.iter().zip(&after) {
            assert!((b - a).abs() < 1e-3, "{b} vs {a}");
        }
        assert!((after[9] - 590.0).abs() < 1e-3, "the old newest is at the edge as the new one arrives");
        assert!(after[10] > 590.0, "and the new one just past it, to slide in");
    }

    /// The other half: the edge's value moves from the old newest to the
    /// new one across the interval, so the end of the line never snaps.
    #[test]
    fn the_edge_moves_from_one_reading_to_the_next_without_a_jump() {
        assert_eq!(edge(&[0.2, 0.4], 1.0), 0.4, "still: the newest itself");
        // A reading of 0.9 lands: the edge starts where it was, at 0.4.
        assert_eq!(edge(&[0.2, 0.4, 0.9], 0.0), 0.4);
        assert!((edge(&[0.2, 0.4, 0.9], 0.5) - 0.65).abs() < 1e-6);
        assert_eq!(edge(&[0.5], 0.3), 0.5);
    }

    #[test]
    fn the_phase_runs_from_a_reading_to_the_next_and_holds_if_it_is_late() {
        let t = Instant::now();
        let m = Motion { sampled: t, interval: Duration::from_secs(1) };
        assert_eq!(m.phase(t), 0.0);
        assert!((m.phase(t + Duration::from_millis(500)) - 0.5).abs() < 1e-3);
        assert_eq!(m.phase(t + Duration::from_secs(3)), 1.0, "late: held, not run past");
        assert_eq!(m.phase(t - Duration::from_secs(1)), 0.0);
    }
}

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
/// edge itself is a point on the curve between the last two readings.
/// Drawn level with the newest instead, the end of the line would snap
/// up or down each time a reading landed — smooth sideways, still
/// jumping. Placed from the right, so a graph with only a few
/// readings fills in from where the newest is.
pub fn positions(count: usize, width: f32, phase: f32) -> Vec<f32> {
    let step = width / (LEN - 1) as f32;
    let x0 = width - step * count.saturating_sub(1) as f32 + step * (1.0 - phase);
    (0..count).map(|i| x0 + step * i as f32).collect()
}

/// One stretch of a line between two readings: a cubic from `a` to `b`.
///
/// Its controls sit a third and two thirds of the way across, so x runs
/// *linearly* along it — which is what lets [`clip`] cut it at an edge
/// exactly, and [`Piece::y_at`] read it at any x without solving a cubic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Piece {
    pub a: Point,
    pub c1: Point,
    pub c2: Point,
    pub b: Point,
}

impl Piece {
    fn at(&self, t: f32) -> Point {
        let u = 1.0 - t;
        let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        Point::new(
            w0 * self.a.x + w1 * self.c1.x + w2 * self.c2.x + w3 * self.b.x,
            w0 * self.a.y + w1 * self.c1.y + w2 * self.c2.y + w3 * self.b.y,
        )
    }

    /// The curve's height at `x`, if `x` is within this piece.
    pub fn y_at(&self, x: f32) -> Option<f32> {
        let span = self.b.x - self.a.x;
        (span > 0.0 && (self.a.x..=self.b.x).contains(&x)).then(|| self.at((x - self.a.x) / span).y)
    }

    /// The two halves either side of `t`, by de Casteljau.
    fn split(&self, t: f32) -> (Piece, Piece) {
        let mix = |p: Point, q: Point| Point::new(p.x + (q.x - p.x) * t, p.y + (q.y - p.y) * t);
        let (ab, bc, cd) = (mix(self.a, self.c1), mix(self.c1, self.c2), mix(self.c2, self.b));
        let (abc, bcd) = (mix(ab, bc), mix(bc, cd));
        let mid = mix(abc, bcd);
        (Piece { a: self.a, c1: ab, c2: abc, b: mid }, Piece { a: mid, c1: bcd, c2: cd, b: self.b })
    }
}

/// Each reading's slope, by Steffen's method: a curve through readings
/// that never goes past either of the two it joins. A peak is drawn at
/// the value that was measured, not above it, and a run of zeros —
/// an idle disk — stays on the floor rather than dipping under it, which
/// the usual smoothing (Catmull–Rom) does on every burst.
///
/// Steffen rather than Fritsch–Carlson because every slope here depends
/// only on its own two neighbours. A reading arriving or leaving changes
/// the curve beside it and nowhere else — and both happen off the edge
/// of the graph, so the visible line never changes shape under the eye.
/// Fritsch–Carlson's correcting pass carries a change along the line.
pub fn slopes(p: &[Point]) -> Vec<f32> {
    // `signum` calls 0 positive; a flat stretch has to count as neither.
    let sign = |v: f32| if v > 0.0 { 1.0 } else if v < 0.0 { -1.0 } else { 0.0 };
    let n = p.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let d: Vec<f32> = p.windows(2).map(|w| if w[1].x > w[0].x { (w[1].y - w[0].y) / (w[1].x - w[0].x) } else { 0.0 }).collect();
    let mut m = vec![0.0; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        let (h0, h1) = (p[i].x - p[i - 1].x, p[i + 1].x - p[i].x);
        if h0 + h1 <= 0.0 {
            continue;
        }
        let through = (d[i - 1] * h1 + d[i] * h0) / (h0 + h1);
        m[i] = (sign(d[i - 1]) + sign(d[i])) * d[i - 1].abs().min(d[i].abs()).min(0.5 * through.abs());
    }
    m
}

/// The line through `p` with slopes `m`, as one piece per pair.
pub fn pieces(p: &[Point], m: &[f32]) -> Vec<Piece> {
    p.windows(2)
        .zip(m.windows(2))
        .map(|(w, s)| {
            let third = (w[1].x - w[0].x) / 3.0;
            Piece { a: w[0], c1: Point::new(w[0].x + third, w[0].y + s[0] * third), c2: Point::new(w[1].x - third, w[1].y - s[1] * third), b: w[1] }
        })
        .collect()
}

/// The part of a line between `from` and `to` across, cut exactly where
/// it crosses either. iced's canvas does not clip to its bounds, so a
/// line that runs past an edge would be drawn over its neighbour.
pub fn clip(line: &[Piece], from: f32, to: f32) -> Vec<Piece> {
    line.iter()
        .filter(|p| p.b.x > from && p.a.x < to)
        .map(|p| {
            let mut p = *p;
            if p.a.x < from {
                p = p.split((from - p.a.x) / (p.b.x - p.a.x)).1;
            }
            if p.b.x > to {
                p = p.split((to - p.a.x) / (p.b.x - p.a.x)).0;
            }
            p
        })
        .collect()
}

/// The slopes of a sliding graph's readings. The newest is past the
/// right edge and the one before it at the edge; when a reading lands,
/// the one at the edge stops being the end of the line and its slope
/// would change in one step — a kink just inside the edge, every
/// second. So its slope moves from the end-of-line one to the
/// in-the-middle one across the interval instead, and the line at the
/// edge changes continuously.
pub fn sliding_slopes(p: &[Point], phase: f32) -> Vec<f32> {
    let mut m = slopes(p);
    let n = p.len();
    if n >= 3 {
        let at_end = slopes(&p[..n - 1])[n - 2];
        m[n - 2] = at_end + (m[n - 2] - at_end) * phase;
    }
    m
}

fn trace(b: &mut canvas::path::Builder, line: &[Piece]) {
    for p in line {
        b.bezier_curve_to(p.c1, p.c2, p.b);
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
            let p: Vec<Point> = values.iter().zip(&xs).map(|(v, x)| Point::new(*x, height - v * (height - 1.0))).collect();
            // The whole line through every kept reading, then the part of
            // it on the graph: the oldest are past the left edge and the
            // newest past the right one, sliding in.
            let line = clip(&pieces(&p, &sliding_slopes(&p, phase)), 0.0, width);
            let (Some(first), Some(last)) = (line.first(), line.last()) else {
                return vec![frame.into_geometry()];
            };
            let (start, end) = (first.a, last.b);
            let stroke = Path::new(|b| {
                b.move_to(start);
                trace(b, &line);
            });
            let area = Path::new(|b| {
                b.move_to(Point::new(start.x, height));
                b.line_to(start);
                trace(b, &line);
                b.line_to(Point::new(end.x, height));
                b.close();
            });
            frame.fill(&area, Color { a: 0.12, ..self.line });
            frame.stroke(&stroke, Stroke::default().with_color(self.line).with_width(1.5));
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
            let p: Vec<Point> = self.points.iter().map(|&(at, v)| Point::new(x(at), height - v.clamp(0.0, 1.0) * (height - 1.0))).collect();
            let curve = pieces(&p, &slopes(&p));
            let line = Path::new(|b| {
                b.move_to(p[0]);
                trace(b, &curve);
            });
            let area = Path::new(|b| {
                b.move_to(Point::new(p[0].x, height));
                b.line_to(p[0]);
                trace(b, &curve);
                b.line_to(Point::new(p[p.len() - 1].x, height));
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

    /// A curve drawn as `positions` and the graph place it, sampled
    /// across the visible width.
    fn visible(values: &[f32], phase: f32, width: f32) -> Vec<f32> {
        let xs = positions(values.len(), width, phase);
        let p: Vec<Point> = values.iter().zip(&xs).map(|(v, x)| Point::new(*x, 100.0 - v * 99.0)).collect();
        let line = clip(&pieces(&p, &sliding_slopes(&p, phase)), 0.0, width);
        (0..=200).map(|i| i as f32 * width / 200.0).map(|x| line.iter().find_map(|piece| piece.y_at(x)).expect("covers the width")).collect()
    }

    /// The whole of "smooth": the line on screen just before a reading
    /// lands and just after it is the same line. Both ends are tested
    /// at once — a full graph drops its oldest reading in the same
    /// moment — so a kink at either edge, a step at the start or a
    /// change of shape anywhere fails it.
    #[test]
    fn the_line_on_screen_does_not_change_when_a_reading_lands() {
        let full: Vec<f32> = (0..LEN + 2).map(|i| ((i as f32 * 0.7).sin() + 1.0) / 2.0).collect();
        let before = visible(&full, 1.0, 590.0);
        let mut next = full[1..].to_vec();
        next.push(0.05);
        let after = visible(&next, 0.0, 590.0);
        for (i, (b, a)) in before.iter().zip(&after).enumerate() {
            assert!((b - a).abs() < 1e-3, "sample {i}: {b} then {a}");
        }
    }

    /// Monotone: between two readings the curve stays between them, so
    /// a peak is drawn at what was measured and a quiet disk's zeros sit
    /// on the floor. A Catmull–Rom line fails both on this series.
    #[test]
    fn the_curve_never_goes_past_the_readings_it_joins() {
        let bursty = [0.0, 0.0, 0.9, 0.0, 0.0, 0.0, 0.4, 0.45, 0.0, 1.0, 0.2, 0.0];
        let p: Vec<Point> = bursty.iter().enumerate().map(|(i, v)| Point::new(i as f32 * 10.0, 100.0 - v * 100.0)).collect();
        for piece in pieces(&p, &slopes(&p)) {
            let (lo, hi) = (piece.a.y.min(piece.b.y), piece.a.y.max(piece.b.y));
            for k in 0..=50 {
                let y = piece.at(k as f32 / 50.0).y;
                assert!(y >= lo - 1e-3 && y <= hi + 1e-3, "{piece:?} at {k}/50: {y} outside {lo}..{hi}");
            }
        }
    }

    #[test]
    fn a_curve_is_cut_exactly_at_the_edges() {
        let p = [Point::new(-10.0, 50.0), Point::new(10.0, 0.0), Point::new(30.0, 80.0)];
        let whole = pieces(&p, &slopes(&p));
        let cut = clip(&whole, 0.0, 20.0);
        assert_eq!(cut.first().unwrap().a.x, 0.0);
        assert!((cut.last().unwrap().b.x - 20.0).abs() < 1e-4);
        // And the cut pieces lie on the original curve.
        for x in [0.0, 5.0, 10.0, 15.0, 20.0] {
            let want = whole.iter().find_map(|q| q.y_at(x)).unwrap();
            let got = cut.iter().find_map(|q| q.y_at(x)).unwrap();
            assert!((want - got).abs() < 1e-3, "at {x}: {want} vs {got}");
        }
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

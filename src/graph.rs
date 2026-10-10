//! A history graph: a line over a faint fill, newest on the right.
//!
//! Drawn in the foreground colour, never the accent — the suite's
//! design system keeps the accent for "selected", and a busy CPU is not
//! a choice anyone made.

use crate::history::{Series, LEN};
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{mouse, Color, Point, Rectangle, Renderer, Size, Theme};

pub struct Graph<'a> {
    pub series: &'a Series,
    /// What the top edge stands for: 1 for a share, a series' own
    /// ceiling for a rate.
    pub ceiling: f32,
    pub line: Color,
    pub grid: Color,
}

impl<Message> canvas::Program<Message> for Graph<'_> {
    type State = ();

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
            // Placed from the right, so a graph that has only a few
            // readings fills in from where the newest one is.
            let step = width / (LEN - 1) as f32;
            let x0 = width - step * (values.len() - 1) as f32;
            let point = |i: usize, v: f32| Point::new(x0 + step * i as f32, height - v * (height - 1.0));
            let line = Path::new(|b| {
                for (i, v) in values.iter().enumerate() {
                    if i == 0 {
                        b.move_to(point(i, *v));
                    } else {
                        b.line_to(point(i, *v));
                    }
                }
            });
            let area = Path::new(|b| {
                b.move_to(Point::new(x0, height));
                for (i, v) in values.iter().enumerate() {
                    b.line_to(point(i, *v));
                }
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

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

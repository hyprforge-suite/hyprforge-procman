//! The App history tab: what used the machine, by app, over a day, a
//! week or a month.
//!
//! A chart of each day's CPU, the chosen app's part of each day filled
//! in, then a plain table. Choosing a row is what changes the chart, so
//! "which app" and "when" are answered by one click and no reading.

use crate::view::bytes;
use crate::{App, Message};
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::widget::{button, column, container, row, scrollable, Space};
use iced::{mouse, Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

/// One bar a day: the day's total, the chosen app's part filled from
/// the bottom. Scaled to the busiest day of the range.
struct Days {
    days: Vec<(f64, f64)>,
    track: Color,
    part: Color,
}

impl<M> canvas::Program<M> for Days {
    type State = ();

    fn draw(&self, _state: &(), renderer: &Renderer, _theme: &Theme, bounds: Rectangle, _cursor: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let Size { width, height } = bounds.size();
        let n = self.days.len().max(1) as f32;
        let top = self.days.iter().map(|d| d.0).fold(0.0f64, f64::max).max(1.0);
        // Bars as wide as a seventh of the width at most, so a range of
        // two recorded days does not draw two slabs.
        let slot = width / n.max(7.0);
        let bar = (slot * 0.42).max(2.0);
        let x0 = width - slot * n;
        for (i, (total, mine)) in self.days.iter().enumerate() {
            let x = x0 + slot * i as f32 + (slot - bar) / 2.0;
            let h = (*total / top) as f32 * height;
            let m = (*mine / top) as f32 * height;
            frame.fill(&Path::rectangle(Point::new(x, height - h), Size::new(bar, h)), self.track);
            frame.fill(&Path::rectangle(Point::new(x, height - m), Size::new(bar, m)), self.part);
        }
        vec![frame.into_geometry()]
    }
}

/// `Mon`, or `Today`, under a day's bar.
fn weekday(date: &str, today: &str) -> String {
    if date == today {
        return "Today".to_string();
    }
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map_or_else(|_| date.to_string(), |d| d.format("%a").to_string())
}

/// `1 h 32 m`, `14 m`, `40 s`: CPU time as a person says it.
fn spent(seconds: f64) -> String {
    let s = seconds.round() as u64;
    match s {
        0 => "—".to_string(),
        1..=59 => format!("{s} s"),
        60..=3599 => format!("{} m", s / 60),
        _ => format!("{} h {} m", s / 3600, (s % 3600) / 60),
    }
}

impl App {
    pub fn history_view(&self, s: FontScale) -> Element<'_, Message> {
        let history = match &self.app_history {
            Ok(h) => h,
            Err(e) => {
                return container(widgets::hint_text(format!("{e}. Nothing is being recorded, so nothing is written over it."), s))
                    .padding(spacing::LG)
                    .into()
            }
        };
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let (apps, covered) = history.totals(&today, self.history_days);
        let range = widgets::segmented_choice(
            &[1usize, 7, 30],
            Some(&self.history_days),
            |d| match d {
                1 => "Today".to_string(),
                n => format!("{n} days"),
            },
            Message::HistoryRange,
            s,
        );
        let bar = row![range, Space::new().width(Length::Fill), widgets::meta_text(format!("{} recorded", spent(covered)), density::META_TEXT_BASE, s)]
            .align_y(Alignment::Center);
        let chosen = self.history_app.clone().or_else(|| apps.first().map(|(c, _)| c.clone()));
        let name = |class: &str| match &self.desktop {
            Some(d) => hyprforge_procman::app_history::app_name(&d.db, class),
            None => class.to_string(),
        };

        let mut page = column![bar].spacing(spacing::MD);
        // A day has nothing to compare: the chart is for a range.
        if self.history_days > 1 {
            let days = history.daily(&today, self.history_days, chosen.as_deref());
            if !days.is_empty() {
                let chart = canvas(Days { days: days.iter().map(|d| (d.1, d.2)).collect(), track: surface::card_border(), part: theme::info() }).width(Length::Fill).height(Length::Fixed(s.apply(130.0)));
                // A label under every bar for a week; for a month, only
                // the newest of each week and today.
                let mut labels = row![].spacing(0.0);
                let n = days.len().max(7);
                for _ in days.len()..n {
                    labels = labels.push(Space::new().width(Length::FillPortion(1)));
                }
                for (i, (date, _, _)) in days.iter().enumerate() {
                    let shown = days.len() <= 7 || (days.len() - 1 - i) % 7 == 0;
                    let word = if shown { weekday(date, &today) } else { String::new() };
                    labels = labels.push(container(widgets::meta_text(word, density::META_TEXT_BASE, s)).width(Length::FillPortion(1)).align_x(Alignment::Center));
                }
                let caption = chosen.as_deref().map_or(String::new(), |c| format!("{} of each day's CPU", name(c)));
                page = page.push(column![chart, labels, widgets::meta_text(caption, density::META_TEXT_BASE, s)].spacing(spacing::XS));
            }
        }

        let head = |label: &'static str, w: f32| container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
        let mut table = column![row![
            widgets::meta_text("App", density::META_TEXT_BASE, s).width(Length::FillPortion(5)),
            container(widgets::meta_text("CPU time ↓", density::META_TEXT_BASE, s)).width(Length::FillPortion(5)),
            head("GPU time", 90.0),
            head("Disk", 90.0),
            head("Peak memory", 110.0),
            head("Open", 80.0),
        ]
        .spacing(spacing::MD)
        .padding([spacing::XS, spacing::SM])]
        .spacing(0.0);
        if apps.is_empty() {
            table = table.push(container(widgets::hint_text("Nothing recorded yet for this range.", s)).padding(spacing::SM));
        }
        let most = apps.first().map_or(1.0, |(_, u)| u.cpu_seconds.max(1.0));
        for (class, u) in &apps {
            let selected = chosen.as_deref() == Some(class.as_str());
            let mono = |v: String, w: f32, dim: bool| {
                container(widgets::scaled_text(v, density::ROW_TEXT_BASE, s).font(theme::mono_font()).color(if dim && !selected { theme::text_dim() } else { theme::text() }))
                    .width(Length::Fixed(s.apply(w)))
                    .align_x(Alignment::End)
            };
            let share = (u.cpu_seconds / most) as f32;
            let line = container(Space::new()).width(Length::Fill).height(Length::Fixed(4.0)).style(|_t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(surface::card_border())),
                ..container::Style::default()
            });
            let fill = container(Space::new()).width(Length::FillPortion(((share * 1000.0) as u16).max(1))).height(Length::Fixed(4.0)).style(move |_t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(if selected { theme::info() } else { theme::text_dim() })),
                ..container::Style::default()
            });
            let rest = Space::new().width(Length::FillPortion((((1.0 - share) * 1000.0) as u16).max(1)));
            let track = iced::widget::stack![line, row![fill, rest]];
            let cells = row![
                row![
                    crate::app_icons::view(self.icons.peek(class), s.apply(20.0)),
                    widgets::clamped_text(name(class), 1).size(s.apply(density::ROW_TEXT_BASE)).width(Length::Fill),
                ]
                .spacing(spacing::SM)
                .align_y(Alignment::Center)
                .width(Length::FillPortion(5)),
                row![mono(spent(u.cpu_seconds), 90.0, false), container(track).width(Length::Fill)].spacing(spacing::SM).align_y(Alignment::Center).width(Length::FillPortion(5)),
                mono(spent(u.gpu_seconds), 90.0, u.gpu_seconds < 0.5),
                mono(bytes(u.disk_bytes as u64), 90.0, false),
                mono(bytes(u.peak_memory), 110.0, false),
                mono(spent(u.seconds_open), 80.0, true),
            ]
            .spacing(spacing::MD)
            .align_y(Alignment::Center);
            table = table.push(widgets::divider()).push(
                button(cells)
                    .padding([spacing::SM, spacing::SM])
                    .width(Length::Fill)
                    .on_press(Message::HistoryApp(class.clone()))
                    .style(move |t, status| widgets::selectable_row_style(t, status, selected)),
            );
        }
        page = page.push(table);
        scrollable(page.padding(spacing::LG)).height(Length::Fill).into()
    }
}

fn canvas<P: canvas::Program<Message>>(program: P) -> iced::widget::Canvas<P, Message> {
    iced::widget::canvas(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_spent_reads_in_the_unit_a_person_would_use() {
        assert_eq!(spent(0.2), "—");
        assert_eq!(spent(40.0), "40 s");
        assert_eq!(spent(14.0 * 60.0 + 20.0), "14 m");
        assert_eq!(spent(5520.0), "1 h 32 m");
    }
}

//! The App history tab.

use crate::details::duration;
use crate::view::bytes;
use crate::{App, Message};
use hyprforge_ui::theme::{self, spacing, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::{column, container, row, scrollable};
use iced::{Alignment, Element, Length};

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
        let coverage = format!(
            "Recorded while Processes is open — {} of this range so far. Apps are counted with everything they started, by window class.",
            duration(covered as u64)
        );
        let head = |label: &'static str, w: f32| container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
        let mut table = column![row![
            widgets::meta_text("App", density::META_TEXT_BASE, s).width(Length::Fill),
            head("CPU time", 110.0),
            head("Disk", 110.0),
            head("GPU time", 110.0),
            head("Peak memory", 120.0),
            head("Seen for", 110.0),
        ]
        .spacing(spacing::SM)]
        .spacing(spacing::SM);
        if apps.is_empty() {
            table = table.push(widgets::hint_text("Nothing recorded yet for this range.", s));
        }
        for (class, u) in apps {
            let class = match &self.desktop {
                Some(d) => hyprforge_procman::app_history::app_name(&d.db, &class),
                None => class,
            };
            let mono = |v: String, w: f32| container(widgets::scaled_text(v, density::ROW_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
            table = table.push(
                row![
                    widgets::scaled_text(class, density::ROW_TEXT_BASE, s).width(Length::Fill),
                    mono(duration(u.cpu_seconds as u64), 110.0),
                    mono(bytes(u.disk_bytes as u64), 110.0),
                    mono(duration(u.gpu_seconds as u64), 110.0),
                    mono(bytes(u.peak_memory), 120.0),
                    mono(duration(u.seconds_open as u64), 110.0),
                ]
                .spacing(spacing::SM)
                .align_y(Alignment::Center),
            );
        }
        scrollable(column![range, widgets::meta_text(coverage, density::META_TEXT_BASE, s), table].spacing(spacing::MD).padding(spacing::LG))
            .height(Length::Fill)
            .into()
    }
}

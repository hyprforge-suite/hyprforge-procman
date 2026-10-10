//! Drawing: the header, the process list, the Performance graphs, the
//! status bar and the confirmation.
//!
//! Everything here is built from `hyprforge-ui`'s pieces and the shared
//! `Theme`; there is no colour in this file. The accent appears in two
//! places only — the selected row and the chosen tab — because that is
//! all it means.

use crate::{App, Message, Tab, INTERVALS};
use hyprforge_procman::graph::Graph;
use hyprforge_procman::history::Series;
use hyprforge_procman::model::{self, Column, Entry, Line};
use hyprforge_procman::sample::Id;
use hyprforge_procman::signal::How;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, glyph, widgets};
use iced::widget::{button, canvas, column, container, row, scrollable, stack, text, Space};
use iced::{Alignment, Background, Border, Element, Length};

const NAME_PORTION: u16 = 6;
const CPU_W: f32 = 84.0;
const MEM_W: f32 = 104.0;
const DISK_W: f32 = 104.0;
const PID_W: f32 = 84.0;
const USER_W: f32 = 120.0;

fn scale() -> FontScale {
    FontScale(theme::active().font_scale)
}

/// `12.5 MB`, in the units people read: powers of 1024, named the way
/// every task manager names them.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Bytes a second, or a dash for what is not known yet — or not this
/// user's to see, until Show everything.
pub fn rate(per_second: Option<f64>) -> String {
    match per_second {
        Some(r) if r < 1.0 => "0 B/s".into(),
        Some(r) => format!("{}/s", bytes(r as u64)),
        None => "—".into(),
    }
}

/// A share, or a dash while there is only one reading to go on.
pub fn percent(share: Option<f32>) -> String {
    match share {
        Some(p) if p < 0.05 => "0 %".into(),
        Some(p) => format!("{p:.1} %"),
        None => "—".into(),
    }
}

impl App {
    pub fn view(&self) -> Element<'_, Message> {
        let s = scale();
        let body = match self.tab {
            Tab::Processes => self.processes(s),
            Tab::Performance => self.performance(s),
        };
        let page = column![self.header(s), widgets::divider(), body, widgets::divider(), self.status(s)];
        let page = container(page).width(Length::Fill).height(Length::Fill).style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::root())),
            ..container::Style::default()
        });
        match &self.confirm {
            Some(c) => stack![page, widgets::scrim(self.confirm_card(c), Some(Message::Cancelled))].into(),
            None => page.into(),
        }
    }

    fn header(&self, s: FontScale) -> Element<'_, Message> {
        let tabs = widgets::panel_tabs(&[(Tab::Processes, "Processes"), (Tab::Performance, "Performance")], &self.tab, Message::Tab, s);
        let mut bar = row![tabs, Space::new().width(Length::Fill)].spacing(spacing::MD).align_y(Alignment::Center);
        if self.tab == Tab::Processes {
            bar = bar.push(
                container(widgets::search_field("Search name, command or PID", &self.view.query, Message::Query, None, Some(crate::search_id()), s))
                    .width(Length::Fixed(s.apply(320.0))),
            );
        }
        let speeds = widgets::segmented_choice(
            &INTERVALS,
            (!self.paused).then_some(&self.interval),
            |d| if d.as_millis() < 1000 { format!("{:.1} s", d.as_secs_f32()) } else { format!("{} s", d.as_secs()) },
            Message::Interval,
            s,
        );
        let pause = if self.paused {
            widgets::primary_button("Paused").on_press(Message::Pause(false))
        } else {
            widgets::secondary_button("Pause").on_press(Message::Pause(true))
        };
        let everything = match self.everything {
            crate::Everything::Locked => widgets::secondary_button("Show everything").on_press(Message::ShowEverything),
            crate::Everything::Asking => widgets::secondary_button("Asking…"),
            crate::Everything::Open(_) => widgets::primary_button("Showing everything"),
        };
        bar = bar.push(everything).push(speeds).push(pause);
        container(bar)
            .padding([0.0, spacing::MD])
            .height(Length::Fixed(density::bar_height(s)))
            .align_y(Alignment::Center)
            .style(|_t: &iced::Theme| container::Style { background: Some(Background::Color(surface::sidebar())), ..container::Style::default() })
            .into()
    }

    fn column_head(&self, label: &'static str, column: Column, width: Length, right: bool, s: FontScale) -> Element<'_, Message> {
        let sort = self.view.sort;
        let marker = if sort.column == column { if sort.descending { " ↓" } else { " ↑" } } else { "" };
        let label = widgets::meta_text(format!("{label}{marker}"), density::META_TEXT_BASE, s);
        let label = container(label).width(Length::Fill).align_x(if right { Alignment::End } else { Alignment::Start });
        button(label)
            .padding([spacing::XS, spacing::SM])
            .width(width)
            .on_press(Message::Sort(column))
            .style(|t, status| widgets::selectable_row_style(t, status, false))
            .into()
    }

    fn processes(&self, s: FontScale) -> Element<'_, Message> {
        let heads = row![
            self.column_head("Name", Column::Name, Length::FillPortion(NAME_PORTION), false, s),
            self.column_head("CPU", Column::Cpu, Length::Fixed(s.apply(CPU_W)), true, s),
            self.column_head("Memory", Column::Memory, Length::Fixed(s.apply(MEM_W)), true, s),
            self.column_head("Disk", Column::Disk, Length::Fixed(s.apply(DISK_W)), true, s),
            self.column_head("PID", Column::Pid, Length::Fixed(s.apply(PID_W)), true, s),
            self.column_head("User", Column::User, Length::Fixed(s.apply(USER_W)), false, s),
        ]
        .padding([0.0, spacing::SM]);

        let lines = model::lines(&self.table, &self.view);
        let mut list = column![].spacing(1.0).padding([spacing::XS, spacing::SM]);
        if let Some(why) = &self.no_windows {
            list = list.push(container(widgets::hint_text(format!("No apps group: {why}. Every process is listed below."), s)).padding(spacing::SM));
        }
        if lines.is_empty() && !self.table.background.is_empty() {
            list = list.push(container(widgets::hint_text("Nothing matches the search.", s)).padding(spacing::MD));
        }
        for line in lines {
            list = list.push(self.line(line, s));
        }
        column![heads, widgets::divider(), scrollable(list).height(Length::Fill)].height(Length::Fill).into()
    }

    fn line(&self, line: Line, s: FontScale) -> Element<'_, Message> {
        match line {
            Line::Heading { label, count } => container(widgets::section_label(&format!("{label} ({count})"), s))
                .padding(iced::Padding { top: spacing::SM + spacing::XS, right: spacing::SM, bottom: spacing::XS, left: spacing::SM })
                .into(),
            Line::App { app, open } => {
                let id = app.lead.id;
                let chevron = button(glyph::nav(if open { glyph::Nav::Down } else { glyph::Nav::Forward }, s.apply(16.0), theme::text_dim()))
                    .padding(0)
                    .on_press(Message::Toggle(id))
                    .style(|t, status| widgets::selectable_row_style(t, status, false));
                let mut name = column![widgets::scaled_text(app.lead.name.clone(), density::ROW_TEXT_BASE, s)];
                if !app.title.is_empty() {
                    let windows = if app.windows > 1 { format!(" · {} windows", app.windows) } else { String::new() };
                    name = name.push(widgets::clamped_text(format!("{}{windows}", app.title), 1).size(s.apply(density::META_TEXT_BASE)).width(Length::Fill).color(theme::text_dim()));
                }
                let name = row![chevron, name].spacing(spacing::XS).align_y(Alignment::Center);
                let count = app.members.len();
                let user = if count > 1 { format!("{} · {count}", app.lead.user) } else { app.lead.user.clone() };
                self.row(id, name.into(), app.cpu(), app.private_bytes(), app.disk(), id.pid, user, s)
            }
            Line::Member(e) => {
                let name = row![Space::new().width(Length::Fixed(s.apply(16.0) + spacing::XS * 2.0)), self.entry_name(&e, s)].align_y(Alignment::Center);
                self.row(e.id, name.into(), e.cpu, e.private_bytes, e.disk, e.id.pid, e.user.clone(), s)
            }
            Line::Process(e) => {
                let name = self.entry_name(&e, s);
                self.row(e.id, name, e.cpu, e.private_bytes, e.disk, e.id.pid, e.user.clone(), s)
            }
        }
    }

    fn entry_name(&self, e: &Entry, s: FontScale) -> Element<'_, Message> {
        let mut name = row![widgets::scaled_text(e.name.clone(), density::ROW_TEXT_BASE, s)].spacing(spacing::SM).align_y(Alignment::Center);
        match e.state {
            'T' | 't' => name = name.push(widgets::chip("Frozen", widgets::Tint::Info, s)),
            'Z' => name = name.push(widgets::chip("Ended, not collected", widgets::Tint::Dim, s)),
            'D' => name = name.push(widgets::chip("Waiting on disk", widgets::Tint::Warning, s)),
            _ => {}
        }
        if e.kernel {
            name = name.push(widgets::chip("Kernel", widgets::Tint::Dim, s));
        }
        name.into()
    }

    #[allow(clippy::too_many_arguments)]
    fn row<'a>(&self, id: Id, name: Element<'a, Message>, cpu: Option<f32>, memory: u64, disk: Option<f64>, pid: i32, user: String, s: FontScale) -> Element<'a, Message> {
        let selected = self.selected == Some(id);
        let cell = |content: String, width: f32| {
            container(widgets::scaled_text(content, density::ROW_TEXT_BASE, s).font(theme::mono_font()))
                .width(Length::Fixed(s.apply(width)))
                .padding([0.0, spacing::SM])
                .align_x(Alignment::End)
        };
        let line = row![
            container(name).width(Length::FillPortion(NAME_PORTION)).padding([0.0, spacing::SM]),
            cell(percent(cpu), CPU_W),
            cell(bytes(memory), MEM_W),
            cell(rate(disk), DISK_W),
            cell(pid.to_string(), PID_W),
            container(widgets::meta_text(user, density::ROW_TEXT_BASE, s)).width(Length::Fixed(s.apply(USER_W))).padding([0.0, spacing::SM]),
        ]
        .align_y(Alignment::Center);
        button(line)
            .padding([spacing::XS, 0.0])
            .width(Length::Fill)
            .height(Length::Shrink)
            .on_press(Message::Select(id))
            .style(move |t, status| widgets::selectable_row_style(t, status, selected))
            .into()
    }

    fn status(&self, s: FontScale) -> Element<'_, Message> {
        let t = self.totals;
        let summary = format!(
            "{} processes · {} threads · CPU {} · Memory {} of {}",
            t.processes,
            t.threads,
            percent(t.cpu.map(|c| c * 100.0)),
            bytes(t.memory_used),
            bytes(t.memory_total)
        );
        let mut bar = row![widgets::meta_text(summary, density::META_TEXT_BASE, s)].spacing(spacing::MD).align_y(Alignment::Center);
        if let Some(notice) = &self.notice {
            bar = bar.push(container(text(notice.clone()).size(s.apply(density::META_TEXT_BASE)).color(theme::error())).width(Length::Fill));
            if let Some(c) = &self.admin_offer {
                let verb = if c.how == How::Force { "Kill as administrator" } else { "End as administrator" };
                bar = bar.push(widgets::primary_button(verb).on_press(Message::EndAsAdmin));
            }
            bar = bar.push(widgets::secondary_button("Dismiss").on_press(Message::Dismiss));
        } else {
            bar = bar.push(Space::new().width(Length::Fill));
        }
        if self.tab == Tab::Processes {
            let chosen = self.selected;
            let is_app = chosen.is_some_and(|id| self.table.apps.iter().any(|a| a.lead.id == id));
            if is_app {
                bar = bar.push(widgets::secondary_button("Switch to").on_press(Message::SwitchTo));
            }
            let frozen = chosen.and_then(|id| self.entry(id)).is_some_and(|e| matches!(e.state, 'T' | 't'));
            let freeze = if frozen {
                widgets::secondary_button("Resume").on_press(Message::Freeze(false))
            } else {
                let allowed = chosen.filter(|id| !self.critical.contains(&id.pid));
                widgets::secondary_button("Freeze").on_press_maybe(allowed.map(|_| Message::Freeze(true)))
            };
            bar = bar
                .push(widgets::toggle(self.view.show_kernel, s).label("Kernel threads").on_toggle(Message::ShowKernel))
                .push(freeze)
                .push(widgets::secondary_button("Kill").on_press_maybe(chosen.map(|_| Message::Ask(How::Force))))
                .push(widgets::primary_button("End task").on_press_maybe(chosen.map(|_| Message::Ask(How::Ask))));
        }
        container(bar).padding([spacing::SM, spacing::MD]).style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::sidebar())),
            ..container::Style::default()
        })
        .into()
    }

    /// The selected process's entry, wherever it is listed.
    fn entry(&self, id: Id) -> Option<&Entry> {
        self.table.apps.iter().flat_map(|a| a.members.iter()).chain(self.table.background.iter()).find(|e| e.id == id)
    }

    fn confirm_card<'a>(&'a self, c: &'a crate::Confirm) -> Element<'a, Message> {
        let s = scale();
        let (title, verb) = match c.how {
            How::Ask => (format!("End {}?", c.name), "End"),
            How::Force => (format!("Kill {}?", c.name), "Kill"),
        };
        let what = if c.critical {
            "Your session depends on it: this window runs under it, or it is the compositor. Ending it can end your session and close every app.".to_string()
        } else {
            match (c.how, c.windows) {
            (How::Ask, 0) => "It is asked to finish and exit. Anything it hasn't saved may be lost.".to_string(),
            (How::Ask, 1) => "Its window closes. Anything it hasn't saved may be lost.".to_string(),
            (How::Ask, n) => format!("Its {n} windows close. Anything they haven't saved will be lost."),
            (How::Force, _) => "It is stopped at once, without a chance to save. Use this when End didn't work.".to_string(),
            }
        };
        let command = if c.command.is_empty() { format!("pid {}", c.id.pid) } else { format!("pid {} · {}", c.id.pid, c.command) };
        let card = column![
            widgets::scaled_text(title, 18.0, s),
            widgets::meta_text(what, density::ROW_TEXT_BASE, s),
            container(widgets::clamped_text(command, 2).size(s.apply(density::META_TEXT_BASE)).width(Length::Fill).font(theme::mono_font()).color(theme::text_dim()))
                .padding(spacing::SM)
                .width(Length::Fill)
                .style(|_t: &iced::Theme| container::Style {
                    background: Some(Background::Color(surface::row())),
                    border: Border { radius: density::inner_radius().into(), width: 1.0, color: surface::card_border() },
                    ..container::Style::default()
                }),
            row![
                Space::new().width(Length::Fill),
                widgets::secondary_button("Cancel").on_press(Message::Cancelled),
                widgets::danger_button(verb, Message::Confirmed),
            ]
            .spacing(spacing::SM),
        ]
        .spacing(spacing::MD);
        container(card)
            .padding(spacing::LG)
            .width(Length::Fixed(s.apply(460.0)))
            .style(|_t: &iced::Theme| container::Style {
                background: Some(Background::Color(surface::card())),
                border: Border { radius: density::card_radius().into(), width: 1.0, color: surface::card_border() },
                ..container::Style::default()
            })
            .into()
    }

    fn performance(&self, s: FontScale) -> Element<'_, Message> {
        let t = self.totals;
        let cpu = self.panel(
            "CPU",
            percent(t.cpu.map(|c| c * 100.0)),
            format!("{} cores · {} processes · {} threads", self.history.cores.len(), t.processes, t.threads),
            &self.history.cpu,
            s.apply(200.0),
            s,
        );
        let memory = self.panel(
            "Memory",
            format!("{} of {}", bytes(t.memory_used), bytes(t.memory_total)),
            "In use: what programs hold, not counting cache the system can give back".to_string(),
            &self.history.memory,
            s.apply(140.0),
            s,
        );
        let mut page = column![cpu].spacing(spacing::LG).padding(spacing::LG);
        // Per core: a small multiple each, the same scale as the whole.
        let mut cores = column![widgets::section_label("Each core", s)].spacing(spacing::SM);
        let per_row = 4;
        for chunk in self.history.cores.chunks(per_row).enumerate() {
            let (n, series) = chunk;
            let mut r = row![].spacing(spacing::SM);
            for (i, core) in series.iter().enumerate() {
                let label = format!("{} · {}", n * per_row + i, percent(core.last().map(|v| v * 100.0)));
                r = r.push(
                    column![widgets::meta_text(label, density::META_TEXT_BASE, s), self.graph(core, s.apply(56.0))]
                        .spacing(spacing::XS)
                        .width(Length::FillPortion(1)),
                );
            }
            cores = cores.push(r);
        }
        page = page.push(cores).push(memory);
        if t.swap_total > 0 {
            page = page.push(self.panel(
                "Swap",
                format!("{} of {}", bytes(t.swap_used), bytes(t.swap_total)),
                "Memory written out to disk to make room".to_string(),
                &self.history.swap,
                s.apply(90.0),
                s,
            ));
        }
        scrollable(page).height(Length::Fill).into()
    }

    fn panel<'a>(&self, title: &'a str, figure: String, detail: String, series: &'a Series, height: f32, s: FontScale) -> Element<'a, Message> {
        let head = row![
            widgets::scaled_text(title, 18.0, s),
            Space::new().width(Length::Fill),
            widgets::scaled_text(figure, 18.0, s).font(theme::mono_font()),
        ]
        .align_y(Alignment::Center);
        column![head, self.graph(series, height), widgets::meta_text(detail, density::META_TEXT_BASE, s)].spacing(spacing::SM).into()
    }

    fn graph<'a>(&self, series: &'a Series, height: f32) -> Element<'a, Message> {
        container(canvas(Graph { series, line: theme::text(), grid: surface::card_border() }).width(Length::Fill).height(Length::Fixed(height)))
            .style(|_t: &iced::Theme| container::Style {
                background: Some(Background::Color(surface::card())),
                border: Border { radius: density::inner_radius().into(), width: 1.0, color: surface::card_border() },
                ..container::Style::default()
            })
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_the_way_task_managers_write_them() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(250 * 1024 * 1024), "250 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }

    #[test]
    fn a_share_with_one_reading_is_a_dash_not_a_zero() {
        assert_eq!(percent(None), "—");
        assert_eq!(percent(Some(0.01)), "0 %");
        assert_eq!(percent(Some(12.34)), "12.3 %");
    }
}

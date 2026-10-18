//! Drawing: the header, the process list, the Performance graphs, the
//! status bar and the confirmation.
//!
//! Everything here is built from `hyprforge-ui`'s pieces and the shared
//! `Theme`; there is no colour in this file. The accent appears in two
//! places only — the selected row and the chosen tab — because that is
//! all it means.

use crate::{App, Message, Tab, INTERVALS};
use hyprforge_procman::fit::{fit, Fit};
use hyprforge_procman::graph::Graph;
use std::time::Duration;
use hyprforge_procman::history::Series;
use hyprforge_procman::model::{self, Column, Entry, Line};
use hyprforge_procman::sample::Id;
use hyprforge_procman::services::{self, Action, Scope};
use hyprforge_procman::signal::How;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, glyph, widgets};
use iced::widget::{button, canvas, column, container, row, scrollable, stack, text, Space};
use iced::{Alignment, Background, Border, Element, Length};

pub const CPU_W: f32 = 84.0;
pub const MEM_W: f32 = 104.0;
pub const DISK_W: f32 = 104.0;
pub const GPU_W: f32 = 84.0;
pub const PID_W: f32 = 84.0;
pub const USER_W: f32 = 120.0;
/// The Processes list's minute of CPU.
pub const SPARK_W: f32 = 110.0;
/// Each figure's column in a narrow list.
pub const NARROW_W: f32 = 92.0;

/// A row's figures, whatever it is a row of: an app, a run of one name,
/// or a process.
pub struct Figures {
    pub cpu: Option<f32>,
    pub memory: u64,
    pub disk: Option<f64>,
    pub net: Option<f64>,
    pub gpu: (Option<f32>, Vec<String>),
}

/// The name a run of [`Line::Same`] is opened by.
fn name_of(members: &[Entry]) -> String {
    members.first().map(|m| m.name.clone()).unwrap_or_default()
}

/// A figure on a tint as deep as `share` (0 to 1) — the heat the
/// Processes list draws its CPU, memory and GPU in. The text colour,
/// faint: never the accent, which means selected, and never a state
/// colour, which would say a busy process is news. Kept under a quarter,
/// so the figure on it still reads. Nothing below half a percent is
/// tinted, so an idle list is a quiet one.
pub fn heat_cell<'a, Message: 'a>(content: String, share: f32, width: f32, s: FontScale) -> Element<'a, Message> {
    let alpha = if share <= 0.005 { 0.0 } else { (0.04 + share * 0.8).min(0.24) };
    container(widgets::scaled_text(content, density::ROW_TEXT_BASE, s).font(theme::mono_font()))
        .width(Length::Fixed(s.apply(width)))
        .padding([2.0, spacing::SM])
        .align_x(Alignment::End)
        .style(move |_t: &iced::Theme| container::Style {
            background: (alpha > 0.0).then(|| Background::Color(iced::Color { a: alpha, ..theme::text() })),
            border: Border { radius: density::inner_radius().into(), ..Border::default() },
            ..container::Style::default()
        })
        .into()
}

fn scale() -> FontScale {
    FontScale(theme::active().font_scale)
}

/// The font scale, for a view built outside this file.
pub fn scale_now() -> FontScale {
    scale()
}

/// Secondary text's colour: dim on an ordinary row, the body colour on
/// the selected one, where dim text over the accent cannot be read.
pub fn ink(selected: bool) -> iced::Color {
    if selected {
        theme::text()
    } else {
        theme::text_dim()
    }
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

/// The GPU column: one number, like CPU's — the busiest engine on any
/// card it measures. A process holding only a card that can't say what
/// each program uses (NVIDIA's driver) is a dash, never a 0 it was not
/// measured at; one holding no GPU is blank. Which card and which engine
/// are in Details, and in Advanced's GPU engine column.
pub fn gpu_cell((load, cards): &(Option<f32>, Vec<String>)) -> String {
    match load {
        Some(l) => percent(Some(*l)),
        None if !cards.is_empty() => "—".to_string(),
        None => String::new(),
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
            Tab::Overview => self.overview_view(s),
            Tab::History => self.history_view(s),
            Tab::Recorded => self.recorded_view(s),
            Tab::Processes => self.processes(s),
            Tab::Performance => self.performance(s),
            Tab::Details => self.details_view(s),
            Tab::Startup => self.startup_view(s),
            Tab::Users => self.users_view(s),
            Tab::Services => self.services_view(s),
            Tab::Crashes => self.crashes_view(s),
            Tab::Rules => self.rules_view(s),
        };
        let middle: Element<'_, Message> = if self.sidebar_shown() { row![self.sidebar(s), body].height(Length::Fill).into() } else { body };
        let mut page = column![self.header(s), widgets::divider(), middle];
        // A quarter of a screen has no room for a line that repeats what
        // the tiles say — unless it has something to say of its own.
        if self.fit() != Fit::Quarter || self.notice.is_some() {
            page = page.push(widgets::divider()).push(self.status(s));
        }
        let page = container(page).width(Length::Fill).height(Length::Fill).style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::root())),
            ..container::Style::default()
        });
        if let Some(session) = &self.sign_out {
            return stack![page, widgets::scrim(self.sign_out_card(session), Some(Message::Dismiss))].into();
        }
        if let Some((scope, name, action)) = &self.services.confirm {
            return stack![page, widgets::scrim(self.service_confirm_card(*scope, name, *action), Some(Message::ServiceCancelled))].into();
        }
        if let Some(c) = &self.confirm {
            return stack![page, widgets::scrim(self.confirm_card(c), Some(Message::Cancelled))].into();
        }
        if let Some(d) = &self.limits {
            return stack![page, widgets::scrim(self.limits_card(d), Some(Message::CloseLimits))].into();
        }
        match (self.menu.as_ref(), self.menu_rows()) {
            (Some(menu), Some(rows)) => {
                let window = (self.window_size.width, self.window_size.height);
                stack![page, widgets::menu_overlay(rows, menu.at, window, Message::CloseMenu, s)].into()
            }
            _ => page.into(),
        }
    }

    /// Every tab, down the left in three groups: what is happening now,
    /// what happened, and what runs on its own. Eleven tabs in the header
    /// squeezed its search field to a sliver at half a screen's width.
    fn sidebar(&self, s: FontScale) -> Element<'_, Message> {
        use hyprforge_ui::widgets::{nav_sidebar, NavGroup, NavItem};
        let item = |tab: Tab, label: &str| NavItem { label: label.to_string(), mark: None, badge: None, selected: self.tab == tab, on_press: Message::Tab(tab) };
        let groups = vec![
            NavGroup {
                label: Some("Live"),
                items: vec![
                    item(Tab::Overview, "Overview"),
                    item(Tab::Processes, "Processes"),
                    item(Tab::Performance, "Performance"),
                    item(Tab::Details, "Details"),
                    item(Tab::Services, "Services"),
                    item(Tab::Users, "Users"),
                ],
            },
            NavGroup { label: Some("Over time"), items: vec![item(Tab::History, "App history"), item(Tab::Recorded, "Recorded"), item(Tab::Crashes, "Crashes")] },
            NavGroup { label: Some("Automatic"), items: vec![item(Tab::Startup, "Startup"), item(Tab::Rules, "Rules")] },
        ];
        nav_sidebar(groups, s.apply(190.0), s)
    }

    /// What the window has room for, from its size ([`Fit`]).
    pub(crate) fn fit(&self) -> Fit {
        fit(self.window_size.width, self.window_size.height)
    }

    /// The sidebar as someone last set it, or — until anyone has — open
    /// only when the window has room for it beside the page.
    pub(crate) fn sidebar_shown(&self) -> bool {
        self.sidebar.unwrap_or(self.fit() == Fit::Full)
    }

    fn header(&self, s: FontScale) -> Element<'_, Message> {
        let full = self.fit() == Fit::Full;
        let mut bar = row![widgets::sidebar_toggle(self.sidebar_shown(), Message::ToggleSidebar, s)].spacing(spacing::MD).align_y(Alignment::Center);
        // The search sits by the sidebar, where it is found; the Overview
        // has the processes' one, and typing there opens Processes.
        let search: Option<Element<'_, Message>> = match self.tab {
            Tab::Services => Some(widgets::search_field("Search services", &self.services.query, Message::ServiceQuery, None, None, s).into()),
            Tab::Overview | Tab::Processes | Tab::Details => {
                let hint = if full { "Search, or port:8080, file:~/notes.txt" } else { "Search, or port:8080" };
                Some(widgets::search_field(hint, &self.view.query, Message::Query, None, Some(crate::search_id()), s).into())
            }
            _ => None,
        };
        if let Some(field) = search {
            // Whatever the bar has left, up to 520: the controls are
            // fixed, so the field is what gives — never Pause, pushed off
            // the edge.
            bar = bar.push(container(field).width(Length::FillPortion(3)).max_width(s.apply(520.0)));
        }
        // Narrow, the readouts leave for the status line and the speeds
        // fold into one button that steps through them: the search field
        // is what the header is for, and at half a screen these two were
        // a third of it.
        if full {
            bar = bar.push(self.readouts(s));
        }
        bar = bar.push(Space::new().width(Length::FillPortion(1)));
        let label = |d: &Duration| if d.as_millis() < 1000 { format!("{:.1} s", d.as_secs_f32()) } else { format!("{} s", d.as_secs()) };
        let speeds: Element<'_, Message> = if full {
            widgets::segmented_choice(&INTERVALS, (!self.paused).then_some(&self.interval), label, Message::Interval, s)
        } else {
            let next = INTERVALS.iter().position(|d| *d == self.interval).map_or(INTERVALS[0], |i| INTERVALS[(i + 1) % INTERVALS.len()]);
            widgets::secondary_button(label(&self.interval)).on_press(Message::Interval(next)).into()
        };
        let pause = if self.paused {
            widgets::primary_button("Paused").on_press(Message::Pause(false))
        } else {
            widgets::secondary_button("Pause").on_press(Message::Pause(true))
        };
        let everything = match (&self.everything, full) {
            (crate::Everything::Locked, true) => widgets::secondary_button("Show everything").on_press(Message::ShowEverything),
            (crate::Everything::Locked, false) => widgets::secondary_button("Everything").on_press(Message::ShowEverything),
            (crate::Everything::Asking, _) => widgets::secondary_button("Asking…"),
            (crate::Everything::Open(_), true) => widgets::primary_button("Showing everything"),
            (crate::Everything::Open(_), false) => widgets::primary_button("Everything"),
        };
        // A quarter of a screen keeps the search and Pause; the rest are
        // a wider window's.
        if self.fit() == Fit::Quarter {
            return self.header_bar(bar.push(pause), s);
        }
        bar = bar.push(everything).push(speeds).push(pause);
        self.header_bar(bar, s)
    }

    fn header_bar<'a>(&self, bar: iced::widget::Row<'a, Message>, s: FontScale) -> Element<'a, Message> {
        container(bar)
            .padding([0.0, spacing::MD])
            .height(Length::Fixed(density::bar_height(s)))
            .align_y(Alignment::Center)
            .style(|_t: &iced::Theme| container::Style { background: Some(Background::Color(surface::sidebar())), ..container::Style::default() })
            .into()
    }

    /// How the graphs move between readings: from the last one, at the
    /// refresh interval — or not at all while paused, when nothing new is
    /// coming and a sliding line would be a lie.
    pub(crate) fn motion(&self) -> Option<hyprforge_procman::graph::Motion> {
        if self.paused {
            return None;
        }
        self.last_at.map(|sampled| hyprforge_procman::graph::Motion { sampled, interval: self.interval })
    }

    /// CPU, memory and I/O wait, on every tab: the three a glance at the
    /// header should answer. I/O wait in the warning colour past a tenth,
    /// where it starts to be why things feel slow.
    fn readouts(&self, s: FontScale) -> Element<'_, Message> {
        let t = self.totals;
        let iowait = self.overview.split[4];
        let item = |label: &'static str, value: String, warn: bool| -> Element<'_, Message> {
            let ink = if warn { theme::warning() } else { theme::text() };
            row![
                widgets::meta_text(label, density::META_TEXT_BASE, s).font(theme::mono_font()).color(if warn { ink } else { theme::text_dim() }),
                widgets::scaled_text(value, density::META_TEXT_BASE, s).font(theme::mono_font()).color(ink),
            ]
            .spacing(spacing::XS)
            .into()
        };
        row![
            item("CPU", percent(t.cpu.map(|c| c * 100.0)), false),
            item("MEM", format!("{} / {}", bytes(t.memory_used), bytes(t.memory_total)), false),
            item("I/O WAIT", percent(Some(iowait * 100.0)), iowait > 0.1),
        ]
        .spacing(spacing::MD)
        .into()
    }

    pub fn column_head(&self, label: &'static str, column: Column, width: Length, right: bool, s: FontScale) -> Element<'_, Message> {
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

    /// The machine's total over a column's head, as Task Manager draws
    /// it: "7%" above CPU says "of what" before the eye reaches a row.
    /// Pressing it sorts, as every head does.
    fn total_head(&self, total: String, label: &'static str, column: Column, width: f32, warn: bool, s: FontScale) -> Element<'_, Message> {
        let sort = self.view.sort;
        let marker = if sort.column == column { if sort.descending { " ↓" } else { " ↑" } } else { "" };
        let ink = if warn { theme::warning() } else { theme::text() };
        let body = column![
            widgets::scaled_text(total, density::ROW_TEXT_BASE, s).font(theme::mono_font()).color(ink),
            widgets::meta_text(format!("{label}{marker}"), density::META_TEXT_BASE, s),
        ]
        .align_x(Alignment::End);
        button(container(body).width(Length::Fill).align_x(Alignment::End))
            .padding([spacing::XS, spacing::SM])
            .width(Length::Fixed(s.apply(width)))
            .on_press(Message::Sort(column))
            .style(|t, status| widgets::selectable_row_style(t, status, false))
            .into()
    }

    /// The machine's totals, as the heads show them: CPU, memory used,
    /// every disk's reads and writes, every interface both ways, and the
    /// busiest GPU.
    fn totals_text(&self) -> [(String, bool); 5] {
        let t = self.totals;
        let memory = if t.memory_total == 0 { None } else { Some(t.memory_used as f32 / t.memory_total as f32 * 100.0) };
        let disk: f64 = self.disk_rates.iter().map(|(_, r)| r.read + r.write).sum();
        let net: f64 = self.net_rates.iter().map(|(_, r)| r.rx + r.tx).sum();
        let gpu = self.machine.gpus.iter().filter_map(|g| self.gpu_busy(g)).fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))));
        [
            (percent(t.cpu.map(|c| c * 100.0)), false),
            (percent(memory), false),
            // Amber while the machine is waiting on the disk: the rate can
            // be small and still be the reason things are slow.
            (rate(Some(disk)), self.overview.split[4] > 0.1),
            (rate(Some(net)), false),
            (percent(gpu), false),
        ]
    }

    /// The column a narrow list keeps after CPU and Memory, and its head:
    /// pressing it steps to the next of Disk, Network and GPU.
    fn narrow_head(&self, totals: &[(String, bool); 5], s: FontScale) -> Element<'_, Message> {
        let (i, label, next) = match self.narrow_column {
            Column::Disk => (2, "Disk", Column::Network),
            Column::Network => (3, "Network", Column::Gpu),
            _ => (4, "GPU", Column::Disk),
        };
        let (total, warn) = totals[i].clone();
        let body = column![
            widgets::scaled_text(total, density::ROW_TEXT_BASE, s).font(theme::mono_font()).color(if warn { theme::warning() } else { theme::text() }),
            widgets::meta_text(format!("{label} ⇄"), density::META_TEXT_BASE, s),
        ]
        .align_x(Alignment::End);
        button(container(body).width(Length::Fill).align_x(Alignment::End))
            .padding([spacing::XS, spacing::SM])
            .width(Length::Fixed(s.apply(NARROW_W)))
            .on_press(Message::NarrowColumn(next))
            .style(|t, status| widgets::selectable_row_style(t, status, false))
            .into()
    }

    fn processes(&self, s: FontScale) -> Element<'_, Message> {
        let wide = self.fit() == Fit::Full;
        let totals = self.totals_text();
        let head = |i: usize, label, column, width| {
            let (total, warn) = totals[i].clone();
            self.total_head(total, label, column, width, warn, s)
        };
        let mut heads = row![self.column_head("Name", Column::Name, Length::Fill, false, s)].align_y(Alignment::End);
        if wide {
            heads = heads
                .push(container(widgets::meta_text("Last minute", density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(SPARK_W))).padding([spacing::XS, spacing::SM]))
                .push(head(0, "CPU", Column::Cpu, CPU_W))
                .push(head(1, "Memory", Column::Memory, MEM_W))
                .push(head(2, "Disk", Column::Disk, DISK_W))
                .push(head(3, "Network", Column::Network, DISK_W))
                .push(head(4, "GPU", Column::Gpu, GPU_W))
                .push(self.column_head("PID", Column::Pid, Length::Fixed(s.apply(PID_W)), true, s));
        } else {
            heads = heads.push(head(0, "CPU", Column::Cpu, NARROW_W)).push(head(1, "Memory", Column::Memory, NARROW_W)).push(self.narrow_head(&totals, s));
        }
        let heads = heads.padding([0.0, spacing::SM]);

        let lines = model::lines(&self.table, &self.view);
        let mut list = column![].spacing(1.0).padding([spacing::XS, spacing::SM]);
        if let Some(why) = &self.no_windows {
            list = list.push(container(widgets::hint_text(format!("No apps group: {why}. Every process is listed below."), s)).padding(spacing::SM));
        }
        if lines.is_empty() && !self.table.background.is_empty() {
            list = list.push(container(widgets::hint_text("Nothing matches the search.", s)).padding(spacing::MD));
        }
        for line in lines {
            list = list.push(self.line(line, wide, s));
        }
        let mut page = column![heads, widgets::divider(), scrollable(list).height(Length::Fill)].height(Length::Fill);
        if let Some(bar) = self.selection_bar(wide, s) {
            page = page.push(bar);
        }
        page.into()
    }

    /// The name an app is shown by: its launcher entry's, when one is
    /// installed for its window class, or the process's own.
    pub(crate) fn app_label(&self, app: &model::App) -> String {
        match (&self.desktop, &app.flatpak) {
            // A Flatpak's lead is already named after its app.
            (_, Some(_)) => app.lead.name.clone(),
            (Some(d), None) => hyprforge_procman::app_history::entry(&d.db, &app.class).map_or_else(|| app.lead.name.clone(), |a| a.name.clone()),
            (None, None) => app.lead.name.clone(),
        }
    }

    fn line(&self, line: Line, wide: bool, s: FontScale) -> Element<'_, Message> {
        let chevron = |open: bool, press: Message| {
            button(glyph::nav(if open { glyph::Nav::Down } else { glyph::Nav::Forward }, s.apply(16.0), theme::text_dim()))
                .padding(0)
                .on_press(press)
                .style(|t, status| widgets::selectable_row_style(t, status, false))
        };
        let caption = |content: String, selected: bool| widgets::clamped_text(content, 1).size(s.apply(density::META_TEXT_BASE)).width(Length::Fill).color(ink(selected));
        match line {
            Line::Heading { label, count } => container(widgets::section_label(&format!("{label} ({count})"), s))
                .padding(iced::Padding { top: spacing::SM + spacing::XS, right: spacing::SM, bottom: spacing::XS, left: spacing::SM })
                .into(),
            Line::App { app, open } => {
                let id = app.lead.id;
                let selected = self.selected == Some(id);
                let label = self.app_label(&app);
                let count = app.members.len();
                // Under the name: what it is besides its name — the
                // program, when that differs, and the window's title —
                // and how much of it there is.
                let mut parts: Vec<String> = Vec::new();
                if wide {
                    if label != app.lead.name {
                        parts.push(app.lead.name.clone());
                    }
                    if !app.title.is_empty() && app.title != label {
                        parts.push(app.title.clone());
                    }
                }
                match (&app.flatpak, app.windows) {
                    (Some(_), 0) => parts.push("Flatpak · no window".to_string()),
                    (Some(_), _) => parts.push("Flatpak".to_string()),
                    (None, n) if n > 1 => parts.push(format!("{n} windows")),
                    _ => {}
                }
                if count > 1 {
                    parts.push(format!("{count} processes"));
                }
                if !wide {
                    parts.push(id.pid.to_string());
                }
                let class = app.flatpak.as_deref().unwrap_or(&app.class);
                let mut title = row![widgets::clamped_text(label, 1).size(s.apply(density::ROW_TEXT_BASE)).width(Length::Shrink)].spacing(spacing::SM).align_y(Alignment::Center);
                if !app.lead.own {
                    title = title.push(widgets::chip(app.lead.user.clone(), widgets::Tint::Dim, s));
                }
                // Efficiency mode giving it less CPU while it has no window
                // on screen: said on the row, since it is why the app is
                // slower than it looks like it should be.
                if self.held_back.contains(&id) {
                    title = title.push(widgets::chip("efficiency", widgets::Tint::Dim, s));
                }
                let name = row![
                    chevron(open, Message::Toggle(id)),
                    crate::app_icons::view(self.icons.peek(class), s.apply(crate::app_icons::SIDE as f32)),
                    column![title, caption(parts.join(" · "), selected)],
                ]
                .spacing(spacing::SM)
                .align_y(Alignment::Center);
                let figures = Figures { cpu: app.cpu(), memory: app.private_bytes(), disk: app.disk(), net: app.net(), gpu: (app.gpu(), app.gpu_cards()) };
                self.row(id, name.into(), figures, self.app_cpu.get(&id), Some(Message::Select(id)), wide, s)
            }
            Line::Same { name, members, open } => {
                let id = members[0].id;
                let selected = members.iter().any(|m| self.selected == Some(m.id));
                let mut title = row![widgets::clamped_text(name.clone(), 1).size(s.apply(density::ROW_TEXT_BASE)).width(Length::Shrink), widgets::chip(format!("× {}", members.len()), widgets::Tint::Dim, s)]
                    .spacing(spacing::SM)
                    .align_y(Alignment::Center);
                if let Some(user) = members.iter().find(|m| !m.own).map(|m| m.user.clone()) {
                    title = title.push(widgets::chip(user, widgets::Tint::Dim, s));
                }
                let name_col: Element<'_, Message> = if wide { title.into() } else { column![title, caption(format!("{} processes", members.len()), selected)].into() };
                let name = row![chevron(open, Message::OpenRun(name.clone())), name_col].spacing(spacing::SM).align_y(Alignment::Center);
                let gpu: Vec<f32> = members.iter().filter_map(|m| m.gpu).collect();
                let figures = Figures {
                    cpu: members.iter().map(|m| m.cpu).sum(),
                    memory: members.iter().map(|m| m.private_bytes).sum(),
                    disk: members.iter().map(|m| m.disk).sum(),
                    net: members.iter().map(|m| m.net).sum(),
                    gpu: ((!gpu.is_empty()).then(|| gpu.iter().sum::<f32>().min(100.0)), Vec::new()),
                };
                self.row(id, name.into(), figures, None, Some(Message::OpenRun(name_of(&members))), wide, s)
            }
            Line::Member(e) => {
                let name = row![Space::new().width(Length::Fixed(s.apply(16.0) + spacing::SM)), self.process_name(&e, wide, s)].align_y(Alignment::Center);
                let figures = Figures { cpu: e.cpu, memory: e.private_bytes, disk: e.disk, net: e.net, gpu: (e.gpu, e.gpu_cards.clone()) };
                self.row(e.id, name.into(), figures, self.process_history.get(&e.id), Some(Message::Select(e.id)), wide, s)
            }
            Line::Process(e) => {
                let name = self.process_name(&e, wide, s);
                let figures = Figures { cpu: e.cpu, memory: e.private_bytes, disk: e.disk, net: e.net, gpu: (e.gpu, e.gpu_cards.clone()) };
                self.row(e.id, name, figures, self.process_history.get(&e.id), Some(Message::Select(e.id)), wide, s)
            }
        }
    }

    /// A process's name, its state, and whose it is when it is not this
    /// user's — the column the User head used to be. Narrow, its pid
    /// goes under it, since there is no PID column.
    fn process_name(&self, e: &Entry, wide: bool, s: FontScale) -> Element<'_, Message> {
        let mut name = row![self.entry_name(e, s)].spacing(spacing::SM).align_y(Alignment::Center);
        if !e.own {
            name = name.push(widgets::chip(e.user.clone(), widgets::Tint::Dim, s));
        }
        if wide {
            return name.into();
        }
        let selected = self.selected == Some(e.id);
        column![name, widgets::clamped_text(e.id.pid.to_string(), 1).size(s.apply(density::META_TEXT_BASE)).color(ink(selected))].into()
    }

    pub fn entry_name(&self, e: &Entry, s: FontScale) -> Element<'_, Message> {
        let mut name = row![widgets::clamped_text(e.name.clone(), 1).size(s.apply(density::ROW_TEXT_BASE)).width(Length::Shrink)].spacing(spacing::SM).align_y(Alignment::Center);
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

    /// One row: the name, a minute of CPU, then the figures — CPU, memory
    /// and GPU tinted by their share of the machine, so the heavy rows
    /// stand out before a number is read.
    #[allow(clippy::too_many_arguments)]
    fn row<'a>(&'a self, id: Id, name: Element<'a, Message>, f: Figures, minute: Option<&'a Series>, press: Option<Message>, wide: bool, s: FontScale) -> Element<'a, Message> {
        let selected = self.selected == Some(id);
        let plain = |content: String, width: f32, dim: bool| -> Element<'a, Message> {
            // Dim reads as gone on the accent, so a selected row's quiet
            // figures take the row's own ink.
            container(widgets::scaled_text(content, density::ROW_TEXT_BASE, s).font(theme::mono_font()).color(if dim && !selected { theme::text_dim() } else { theme::text() }))
                .width(Length::Fixed(s.apply(width)))
                .padding([0.0, spacing::SM])
                .align_x(Alignment::End)
                .into()
        };
        let total = self.totals.memory_total.max(1) as f32;
        let cpu = heat_cell(percent(f.cpu), f.cpu.unwrap_or(0.0) / 25.0, if wide { CPU_W } else { NARROW_W }, s);
        let memory = heat_cell(bytes(f.memory), f.memory as f32 / total, if wide { MEM_W } else { NARROW_W }, s);
        let quiet = |v: Option<f64>| v.is_none_or(|v| v <= 0.0);
        // No GPU open is blank, not 0 %, as Task Manager draws it.
        let gpu = heat_cell(gpu_cell(&f.gpu), f.gpu.0.unwrap_or(0.0) / 100.0, if wide { GPU_W } else { NARROW_W }, s);
        let mut line = row![container(name).width(Length::Fill).padding([0.0, spacing::SM])].align_y(Alignment::Center);
        if wide {
            let spark: Element<'a, Message> = match minute {
                Some(series) => container(
                    canvas(Graph { series, ceiling: series.values().fold(0.02f32, f32::max), line: theme::text_dim(), grid: iced::Color::TRANSPARENT, motion: None })
                        .width(Length::Fill)
                        .height(Length::Fixed(s.apply(18.0))),
                )
                .width(Length::Fixed(s.apply(SPARK_W)))
                .padding([0.0, spacing::SM])
                .into(),
                None => Space::new().width(Length::Fixed(s.apply(SPARK_W))).into(),
            };
            line = line
                .push(spark)
                .push(cpu)
                .push(memory)
                .push(plain(rate(f.disk), DISK_W, quiet(f.disk)))
                // Only Show everything's helper counts it, for anyone's.
                .push(plain(rate(f.net), DISK_W, quiet(f.net)))
                .push(gpu)
                .push(plain(id.pid.to_string(), PID_W, true));
        } else {
            let third = match self.narrow_column {
                Column::Disk => plain(rate(f.disk), NARROW_W, quiet(f.disk)),
                Column::Network => plain(rate(f.net), NARROW_W, quiet(f.net)),
                _ => gpu,
            };
            line = line.push(cpu).push(memory).push(third);
        }
        let row = button(line)
            .padding([spacing::XS, 0.0])
            .width(Length::Fill)
            .height(Length::Shrink)
            .on_press_maybe(press)
            .style(move |t, status| widgets::selectable_row_style(t, status, selected));
        iced::widget::mouse_area(row).on_right_press(Message::OpenMenu(crate::menus::MenuFor::Process(id))).into()
    }

    /// What can be done to the selection, with the selection: its name
    /// and figures, then the actions — End task always last. Narrow, the
    /// rarer ones fold into More, which opens the row's own menu.
    fn selection_bar(&self, wide: bool, s: FontScale) -> Option<Element<'_, Message>> {
        let id = self.selected?;
        let app = self.table.apps.iter().find(|a| a.lead.id == id);
        let entry = self.entry(id)?;
        let (label, summary) = match app {
            Some(a) => (self.app_label(a), format!("{} processes · {} CPU · {}", a.members.len(), percent(a.cpu()), bytes(a.private_bytes()))),
            None => (entry.name.clone(), format!("PID {} · {} CPU · {}", id.pid, percent(entry.cpu), bytes(entry.private_bytes))),
        };
        let mut bar = row![widgets::clamped_text(label, 1).size(s.apply(density::ROW_TEXT_BASE)).width(Length::Shrink)].spacing(spacing::SM).align_y(Alignment::Center);
        if wide {
            bar = bar.push(widgets::meta_text(summary, density::META_TEXT_BASE, s).font(theme::mono_font()));
        }
        bar = bar.push(Space::new().width(Length::Fill));
        if app.is_some_and(|a| a.windows > 0) {
            bar = bar.push(widgets::secondary_button("Switch to").on_press(Message::SwitchTo));
        }
        if wide {
            let frozen = matches!(entry.state, 'T' | 't');
            let freeze = if frozen {
                widgets::secondary_button("Resume").on_press(Message::Freeze(false))
            } else {
                widgets::secondary_button("Freeze").on_press_maybe((!self.critical.contains(&id.pid)).then_some(Message::Freeze(true)))
            };
            bar = bar
                .push(widgets::secondary_button("Limits…").on_press(Message::OpenLimits(id)))
                .push(freeze)
                .push(widgets::secondary_button("Kill").on_press(Message::Ask(How::Force)));
        } else {
            bar = bar.push(widgets::secondary_button("More").on_press(Message::OpenMenu(crate::menus::MenuFor::Process(id))));
        }
        bar = bar.push(widgets::primary_button("End task").on_press(Message::Ask(How::Ask)));
        Some(
            container(bar)
                .padding([spacing::SM, spacing::MD])
                .width(Length::Fill)
                .style(|_t: &iced::Theme| container::Style {
                    background: Some(Background::Color(surface::card())),
                    border: Border { width: 1.0, color: surface::card_border(), ..Border::default() },
                    ..container::Style::default()
                })
                .into(),
        )
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
        // The header's I/O wait, when the header had no room for it.
        let iowait = self.overview.split[4];
        if self.fit() != Fit::Full {
            let ink = if iowait > 0.1 { theme::warning() } else { theme::text_dim() };
            bar = bar.push(text(format!("I/O wait {}", percent(Some(iowait * 100.0)))).size(s.apply(density::META_TEXT_BASE)).color(ink));
        }
        let mut bar = bar.spacing(spacing::MD).align_y(Alignment::Center);
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
        // The selection's actions are in its own bar above this line
        // ([`App::selection_bar`]); the list's switches stay here.
        if self.tab == Tab::Processes {
            // Efficiency mode, when its service is installed: apps with no
            // window on screen give way for CPU to the ones that have.
            if let Some(on) = self.efficiency {
                let toggle = widgets::toggle(on, s).label("Efficiency mode");
                bar = bar.push(if self.efficiency_busy { toggle } else { toggle.on_toggle(Message::Efficiency) });
            }
            bar = bar.push(widgets::toggle(self.view.show_kernel, s).label("Kernel threads").on_toggle(Message::ShowKernel));
        }
        container(bar).padding([spacing::SM, spacing::MD]).style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::sidebar())),
            ..container::Style::default()
        })
        .into()
    }

    /// The selected process's entry, wherever it is listed.
    pub(crate) fn entry(&self, id: Id) -> Option<&Entry> {
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

    fn services_view(&self, s: FontScale) -> Element<'_, Message> {
        let tab = &self.services;
        let scope = tab.scope();
        let controls = row![
            widgets::segmented_choice(&[Scope::System, Scope::User], Some(&scope), |s| match s {
                Scope::System => "This computer".to_string(),
                Scope::User => "Your account".to_string(),
            }, Message::ServiceScope, s),
            widgets::segmented_choice(
                &[services::Filter::Relevant, services::Filter::Running, services::Filter::Failed, services::Filter::All],
                Some(&tab.filter),
                |f| match f {
                    services::Filter::Relevant => "Enabled or running".to_string(),
                    services::Filter::Running => "Running".to_string(),
                    services::Filter::Failed => "Failed".to_string(),
                    services::Filter::All => "All".to_string(),
                },
                Message::ServiceFilter,
                s,
            ),
        ]
        .spacing(spacing::MD)
        .padding([spacing::SM, spacing::MD]);

        let units = tab.units.get(&scope).map(Vec::as_slice).unwrap_or(&[]);
        let shown = services::rows(units, tab.filter, &tab.query);
        let head = |label: &'static str, width: Length, right: bool| {
            container(widgets::meta_text(label, density::META_TEXT_BASE, s))
                .width(width)
                .padding([spacing::XS, spacing::SM])
                .align_x(if right { Alignment::End } else { Alignment::Start })
        };
        let heads = row![
            head("Service", Length::FillPortion(4), false),
            head("Status", Length::Fixed(s.apply(130.0)), false),
            head("At boot", Length::Fixed(s.apply(130.0)), false),
            head("CPU", Length::Fixed(s.apply(CPU_W)), true),
            head("Memory", Length::Fixed(s.apply(MEM_W)), true),
            head("Description", Length::FillPortion(5), false),
        ]
        .padding([0.0, spacing::SM]);

        let mut list = column![].spacing(1.0).padding([spacing::XS, spacing::SM]);
        if let Some(e) = &tab.error {
            list = list.push(container(widgets::hint_text(e.clone(), s)).padding(spacing::MD));
        } else if shown.is_empty() {
            let why = if units.is_empty() { "Reading systemd…" } else { "No service matches." };
            list = list.push(container(widgets::hint_text(why, s)).padding(spacing::MD));
        }
        for u in shown {
            let selected = tab.selected.as_deref() == Some(u.name.as_str());
            let (tint, status) = match u.active.as_str() {
                "active" => (widgets::Tint::Success, if u.sub == "running" { "Running".to_string() } else { format!("Active ({})", u.sub) }),
                "failed" => (widgets::Tint::Error, "Failed".to_string()),
                "activating" => (widgets::Tint::Warning, "Starting".to_string()),
                "deactivating" => (widgets::Tint::Warning, "Stopping".to_string()),
                _ => (widgets::Tint::Dim, "Stopped".to_string()),
            };
            let boot = match u.file.as_deref() {
                Some("enabled") | Some("enabled-runtime") => "Starts",
                Some("disabled") => "Off",
                Some("static") | Some("indirect") => "When needed",
                Some("masked") | Some("masked-runtime") => "Masked",
                Some("alias") => "Alias",
                Some("generated") | Some("transient") | None => "—",
                Some(_) => "—",
            };
            let cpu = tab.cpu.get(&u.name).copied();
            let memory = tab.memory.get(&u.name).copied();
            let mono = |text: String, width: f32| {
                container(widgets::scaled_text(text, density::ROW_TEXT_BASE, s).font(theme::mono_font()))
                    .width(Length::Fixed(s.apply(width)))
                    .padding([0.0, spacing::SM])
                    .align_x(Alignment::End)
            };
            let line = row![
                container(widgets::scaled_text(u.name.trim_end_matches(".service").to_string(), density::ROW_TEXT_BASE, s))
                    .width(Length::FillPortion(4))
                    .padding([0.0, spacing::SM]),
                container(row![widgets::status_dot(tint, s), widgets::scaled_text(status, density::ROW_TEXT_BASE, s)].spacing(spacing::SM).align_y(Alignment::Center))
                    .width(Length::Fixed(s.apply(130.0)))
                    .padding([0.0, spacing::SM]),
                container(widgets::meta_text(boot, density::ROW_TEXT_BASE, s).color(ink(selected)).wrapping(iced::widget::text::Wrapping::None))
                    .width(Length::Fixed(s.apply(130.0)))
                    .padding([0.0, spacing::SM]),
                mono(if u.active == "active" { percent(cpu) } else { String::new() }, CPU_W),
                mono(memory.filter(|_| u.active == "active").map(bytes).unwrap_or_default(), MEM_W),
                container(widgets::clamped_text(u.description.clone(), 1).size(s.apply(density::META_TEXT_BASE)).width(Length::Fill).color(ink(selected)))
                    .width(Length::FillPortion(5))
                    .padding([0.0, spacing::SM]),
            ]
            .align_y(Alignment::Center);
            list = list.push(
                iced::widget::mouse_area(
                    button(line)
                        .padding([spacing::XS, 0.0])
                        .width(Length::Fill)
                        .on_press(Message::SelectService(u.name.clone()))
                        .style(move |t, status| widgets::selectable_row_style(t, status, selected)),
                )
                .on_right_press(Message::OpenMenu(crate::menus::MenuFor::Service(u.name.clone()))),
            );
        }
        let mut page = column![controls, widgets::divider(), heads, widgets::divider(), scrollable(list).height(Length::FillPortion(3))];
        if let Some(unit) = tab.selected.as_ref().and_then(|n| units.iter().find(|u| &u.name == n)) {
            page = page.push(widgets::divider()).push(self.service_detail(unit, s));
        }
        page.height(Length::Fill).into()
    }

    fn service_detail<'a>(&'a self, unit: &'a services::Unit, s: FontScale) -> Element<'a, Message> {
        let mut actions = row![].spacing(spacing::SM);
        for action in Action::offered(unit) {
            let button = if matches!(action, Action::Start | Action::Restart) {
                widgets::primary_button(action.label())
            } else {
                widgets::secondary_button(action.label())
            };
            actions = actions.push(button.on_press_maybe((!self.services.busy).then_some(Message::ServiceAct(action))));
        }
        let title = column![
            widgets::scaled_text(unit.name.clone(), 18.0, s),
            widgets::meta_text(if unit.description.is_empty() { "No description".to_string() } else { unit.description.clone() }, density::META_TEXT_BASE, s),
        ]
        .spacing(spacing::XS);
        let journal: Element<'_, Message> = match &self.services.journal {
            Ok(lines) if lines.is_empty() => widgets::hint_text("Nothing in the journal for this service that you can read.", s).into(),
            Ok(lines) => scrollable(
                column(lines.iter().map(|l| widgets::scaled_text(l.clone(), density::META_TEXT_BASE, s).font(theme::mono_font()).color(theme::text_dim()).into()))
                    .spacing(2.0),
            )
            .anchor_bottom()
            .height(Length::Fill)
            .into(),
            Err(e) => widgets::hint_text(e.clone(), s).into(),
        };
        let journal = container(journal).padding(spacing::SM).width(Length::Fill).height(Length::Fill).style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::card())),
            border: Border { radius: density::inner_radius().into(), width: 1.0, color: surface::card_border() },
            ..container::Style::default()
        });
        container(
            column![row![title, Space::new().width(Length::Fill), actions].align_y(Alignment::Center), widgets::section_label("Recent log", s), journal]
                .spacing(spacing::SM),
        )
        .padding(spacing::MD)
        .height(Length::FillPortion(2))
        .into()
    }

    fn service_confirm_card(&self, scope: Scope, name: &str, action: Action) -> Element<'_, Message> {
        let s = scale();
        let short = name.trim_end_matches(".service").to_string();
        let (title, what) = match action {
            Action::Stop => (format!("Stop {short}?"), "Whatever depends on it stops working until it is started again — stopping a network service ends the connection."),
            Action::Disable => (format!("Stop {short} starting at boot?"), "It keeps running now, and won't start on its own next time."),
            _ => (format!("Mask {short}?"), "Nothing can start it, not even another service that needs it, until it is unmasked."),
        };
        let note = match scope {
            Scope::System => "This is the computer's, so you'll be asked for your password.",
            Scope::User => "This is one of your own; nothing will be asked.",
        };
        let card = column![
            widgets::scaled_text(title, 18.0, s),
            widgets::meta_text(what, density::ROW_TEXT_BASE, s),
            widgets::hint_text(note, s),
            row![
                Space::new().width(Length::Fill),
                widgets::secondary_button("Cancel").on_press(Message::ServiceCancelled),
                widgets::danger_button(action.label(), Message::ServiceConfirmed),
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

    /// A card's busy figure, from wherever its driver keeps one.
    pub(crate) fn gpu_busy(&self, g: &hyprforge_procman::machine::Gpu) -> Option<f32> {
        match g.driver.as_str() {
            "nvidia" => self.nvidia.as_ref().and_then(|n| n.as_ref().ok()).and_then(|n| n.busy),
            _ => g.busy_percent.map(f32::from),
        }
    }

    /// What the battery is paying for: the hungriest programs, with
    /// their estimated watts on battery, or their share of the machine's
    /// activity plugged in — and their wakeups either way.
    pub(crate) fn power_users(&self, s: FontScale) -> Element<'_, Message> {
        let on_battery = !self.watts.is_empty() || self.power_floor.watts().is_some();
        let ranked = hyprforge_procman::power::hungriest(if on_battery { &self.watts } else { &self.activity }, &self.wakeups, 8);
        let note = match (on_battery, self.power_floor.watts()) {
            (true, Some(floor)) => format!(
                "Estimated: the battery's draw above the least seen while this window was open ({floor:.1} W), shared by each program's CPU and GPU time. Wakeups are each time one of its threads woke a core. Both are averaged over about ten seconds."
            ),
            _ => "Plugged in, so no watts: the battery isn't the one paying. Each program's share of what the processor and GPU did, and how often it woke a core, averaged over about ten seconds.".to_string(),
        };
        let head = |label: &'static str, w: f32| container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
        let mut list = column![
            widgets::section_label("What's using power", s),
            widgets::hint_text(note, s),
            row![
                widgets::meta_text("Program", density::META_TEXT_BASE, s).width(Length::Fill),
                head(if on_battery { "Power" } else { "Share" }, 90.0),
                head("Wakeups", 110.0),
                head("CPU", CPU_W),
            ]
            .spacing(spacing::SM),
        ]
        .spacing(spacing::SM);
        if ranked.is_empty() {
            list = list.push(widgets::hint_text(if on_battery { "Nothing above idle yet." } else { "Waiting for a second reading." }, s));
        }
        for id in ranked {
            let Some(e) = self.entry(id) else { continue };
            let figure = if on_battery {
                self.watts.get(&id).map(|w| format!("≈ {w:.1} W")).unwrap_or_else(|| "—".into())
            } else {
                self.activity.get(&id).map(|p| format!("{p:.0} %")).unwrap_or_else(|| "—".into())
            };
            let mono = |v: String, w: f32| container(widgets::scaled_text(v, density::ROW_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
            list = list.push(
                row![
                    widgets::scaled_text(e.name.clone(), density::ROW_TEXT_BASE, s).width(Length::Fill),
                    mono(figure, 90.0),
                    mono(self.wakeups.get(&id).map(|k| format!("{k:.0} /s")).unwrap_or_else(|| "—".into()), 110.0),
                    mono(percent(e.cpu), CPU_W),
                ]
                .spacing(spacing::SM)
                .align_y(Alignment::Center),
            );
        }
        list.into()
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// One number, as CPU is: measured, unmeasurable, or no GPU at all —
    /// and never two lines in a row of one.
    #[test]
    fn the_gpu_column_is_a_figure_a_dash_or_nothing() {
        let cards = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(gpu_cell(&(Some(0.4), cards(&["AMD", "NVIDIA"]))), "0.4 %");
        assert_eq!(gpu_cell(&(Some(0.0), cards(&["AMD"]))), "0 %", "open and idle is measured idle");
        assert_eq!(gpu_cell(&(None, cards(&["NVIDIA"]))), "—", "NVIDIA can't say, which is not 0");
        assert_eq!(gpu_cell(&(None, Vec::new())), "");
    }

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

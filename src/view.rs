//! Drawing: the header, the process list, the Performance graphs, the
//! status bar and the confirmation.
//!
//! Everything here is built from `hyprforge-ui`'s pieces and the shared
//! `Theme`; there is no colour in this file. The accent appears in two
//! places only — the selected row and the chosen tab — because that is
//! all it means.

use crate::{App, Message, Resource, Tab, INTERVALS};
use hyprforge_procman::machine::{DiskRate, NetRate};
use hyprforge_procman::graph::Graph;
use hyprforge_procman::history::Series;
use hyprforge_procman::model::{self, Column, Entry, Line};
use hyprforge_procman::sample::Id;
use hyprforge_procman::services::{self, Action, Scope};
use hyprforge_procman::signal::How;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, glyph, widgets};
use iced::widget::{button, canvas, column, container, row, scrollable, stack, text, Space};
use iced::{Alignment, Background, Border, Element, Length};

pub const NAME_PORTION: u16 = 6;
pub const CPU_W: f32 = 84.0;
pub const MEM_W: f32 = 104.0;
pub const DISK_W: f32 = 104.0;
pub const GPU_W: f32 = 170.0;
pub const PID_W: f32 = 84.0;
pub const USER_W: f32 = 120.0;

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

/// The GPU column: `AMD 5.0 %`, `AMD 0.4 % · NVIDIA`, or blank. The load
/// is measured from fdinfo, which NVIDIA's driver does not write, so it
/// sits beside the first card that is not NVIDIA's — never after a list,
/// where it would read as the whole list's.
pub fn gpu_cell((load, cards): &(Option<f32>, Vec<String>)) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut placed = false;
    let mut ordered: Vec<&String> = cards.iter().filter(|c| *c != "NVIDIA").collect();
    ordered.extend(cards.iter().filter(|c| *c == "NVIDIA"));
    for card in ordered {
        match load {
            Some(l) if !placed && card != "NVIDIA" => {
                parts.push(format!("{card} {}", percent(Some(*l))));
                placed = true;
            }
            _ => parts.push(card.clone()),
        }
    }
    parts.join(" · ")
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
        };
        let page = column![self.header(s), widgets::divider(), body, widgets::divider(), self.status(s)];
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
        match (self.menu.as_ref(), self.menu_rows()) {
            (Some(menu), Some(rows)) => {
                let window = (self.window_size.width, self.window_size.height);
                stack![page, widgets::menu_overlay(rows, menu.at, window, Message::CloseMenu, s)].into()
            }
            _ => page.into(),
        }
    }

    fn header(&self, s: FontScale) -> Element<'_, Message> {
        // The Overview is the Advanced view's own tab, first when shown.
        let mut list: Vec<(Tab, &str)> = Vec::new();
        if self.advanced {
            list.push((Tab::Overview, "Overview"));
        }
        list.extend([
            (Tab::Processes, "Processes"),
            (Tab::Performance, "Performance"),
            (Tab::History, "App history"),
            (Tab::Recorded, "Recorded"),
            (Tab::Startup, "Startup"),
            (Tab::Users, "Users"),
            (Tab::Details, "Details"),
            (Tab::Services, "Services"),
        ]);
        let tabs = widgets::panel_tabs(&list, &self.tab, Message::Tab, s);
        let mut bar = row![tabs].spacing(spacing::MD).align_y(Alignment::Center);
        // The space between the tabs and the controls: the search field's,
        // on a tab that has one, so it takes all the room there is.
        let searches = matches!(self.tab, Tab::Services | Tab::Processes | Tab::Details);
        if !searches {
            bar = bar.push(Space::new().width(Length::Fill));
        }
        if self.tab == Tab::Services {
            bar = bar.push(
                container(container(widgets::search_field("Search services", &self.services.query, Message::ServiceQuery, None, None, s)).max_width(s.apply(320.0)))
                    .width(Length::Fill)
                    .align_x(Alignment::End),
            );
        }
        if matches!(self.tab, Tab::Processes | Tab::Details) {
            bar = bar.push(
                // Whatever the bar has left, up to 320: the tabs and the
                // controls are fixed, so the field is what gives — never
                // Pause, pushed off the edge.
                container(container(widgets::search_field("Search, or port:8080, file:~/notes.txt", &self.view.query, Message::Query, None, Some(crate::search_id()), s)).max_width(s.apply(320.0)))
                    .width(Length::Fill)
                    .align_x(Alignment::End),
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
        let advanced = widgets::toggle(self.advanced, s).label("Advanced").on_toggle(Message::Advanced);
        bar = bar.push(advanced).push(everything).push(speeds).push(pause);
        container(bar)
            .padding([0.0, spacing::MD])
            .height(Length::Fixed(density::bar_height(s)))
            .align_y(Alignment::Center)
            .style(|_t: &iced::Theme| container::Style { background: Some(Background::Color(surface::sidebar())), ..container::Style::default() })
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

    fn processes(&self, s: FontScale) -> Element<'_, Message> {
        let heads = row![
            self.column_head("Name", Column::Name, Length::FillPortion(NAME_PORTION), false, s),
            self.column_head("CPU", Column::Cpu, Length::Fixed(s.apply(CPU_W)), true, s),
            self.column_head("Memory", Column::Memory, Length::Fixed(s.apply(MEM_W)), true, s),
            self.column_head("Disk", Column::Disk, Length::Fixed(s.apply(DISK_W)), true, s),
            self.column_head("Network", Column::Network, Length::Fixed(s.apply(DISK_W)), true, s),
            self.column_head("GPU", Column::Gpu, Length::Fixed(s.apply(GPU_W)), true, s),
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
                let selected = self.selected == Some(id);
                let chevron = button(glyph::nav(if open { glyph::Nav::Down } else { glyph::Nav::Forward }, s.apply(16.0), theme::text_dim()))
                    .padding(0)
                    .on_press(Message::Toggle(id))
                    .style(|t, status| widgets::selectable_row_style(t, status, false));
                let mut name = column![widgets::scaled_text(app.lead.name.clone(), density::ROW_TEXT_BASE, s)];
                if !app.title.is_empty() {
                    let windows = if app.windows > 1 { format!(" · {} windows", app.windows) } else { String::new() };
                    name = name.push(widgets::clamped_text(format!("{}{windows}", app.title), 1).size(s.apply(density::META_TEXT_BASE)).width(Length::Fill).color(ink(selected)));
                }
                let name = row![chevron, name].spacing(spacing::XS).align_y(Alignment::Center);
                let count = app.members.len();
                let user = if count > 1 { format!("{} · {count}", app.lead.user) } else { app.lead.user.clone() };
                self.row(id, name.into(), app.cpu(), app.private_bytes(), (app.disk(), app.net()), (app.gpu(), app.gpu_cards()), id.pid, user, s)
            }
            Line::Member(e) => {
                let name = row![Space::new().width(Length::Fixed(s.apply(16.0) + spacing::XS * 2.0)), self.entry_name(&e, s)].align_y(Alignment::Center);
                self.row(e.id, name.into(), e.cpu, e.private_bytes, (e.disk, e.net), (e.gpu, e.gpu_cards.clone()), e.id.pid, e.user.clone(), s)
            }
            Line::Process(e) => {
                let name = self.entry_name(&e, s);
                self.row(e.id, name, e.cpu, e.private_bytes, (e.disk, e.net), (e.gpu, e.gpu_cards.clone()), e.id.pid, e.user.clone(), s)
            }
        }
    }

    pub fn entry_name(&self, e: &Entry, s: FontScale) -> Element<'_, Message> {
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
    fn row<'a>(&self, id: Id, name: Element<'a, Message>, cpu: Option<f32>, memory: u64, (disk, net): (Option<f64>, Option<f64>), gpu: (Option<f32>, Vec<String>), pid: i32, user: String, s: FontScale) -> Element<'a, Message> {
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
            // Only Show everything's helper counts it, for anyone's.
            cell(rate(net), DISK_W),
            // Which cards, and the load where the driver says; no GPU
            // open is blank, not 0 %, as Task Manager draws it.
            cell(gpu_cell(&gpu), GPU_W),
            cell(pid.to_string(), PID_W),
            container(widgets::meta_text(user, density::ROW_TEXT_BASE, s).color(ink(selected))).width(Length::Fixed(s.apply(USER_W))).padding([0.0, spacing::SM]),
        ]
        .align_y(Alignment::Center);
        let row = button(line)
            .padding([spacing::XS, 0.0])
            .width(Length::Fill)
            .height(Length::Shrink)
            .on_press(Message::Select(id))
            .style(move |t, status| widgets::selectable_row_style(t, status, selected));
        iced::widget::mouse_area(row).on_right_press(Message::OpenMenu(crate::menus::MenuFor::Process(id))).into()
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

    fn performance(&self, s: FontScale) -> Element<'_, Message> {
        let rail = scrollable(column(self.resources().into_iter().map(|(resource, title, figure, series, ceiling)| {
            let selected = self.resource == resource;
            let item = column![
                row![
                    widgets::scaled_text(title, density::ROW_TEXT_BASE, s),
                    Space::new().width(Length::Fill),
                    widgets::meta_text(figure, density::META_TEXT_BASE, s).font(theme::mono_font()).color(ink(selected)),
                ]
                .align_y(Alignment::Center),
                self.graph(series, ceiling, s.apply(34.0)),
            ]
            .spacing(spacing::XS);
            button(item)
                .padding(spacing::SM)
                .width(Length::Fill)
                .on_press(Message::Resource(resource))
                .style(move |t, status| widgets::selectable_row_style(t, status, selected))
                .into()
        }))
        .spacing(spacing::XS)
        .padding(spacing::SM));
        let rail = container(rail).width(Length::Fixed(s.apply(260.0))).height(Length::Fill).style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::sidebar())),
            ..container::Style::default()
        });
        let detail = scrollable(container(self.detail(s)).padding(spacing::LG)).height(Length::Fill);
        row![rail, widgets::vertical_divider(), detail].height(Length::Fill).into()
    }

    /// The rail's entries: what each is, its figure now, and its graph.
    fn resources(&self) -> Vec<(Resource, String, String, &Series, f32)> {
        let t = self.totals;
        let h = &self.history;
        let mut out = vec![
            (Resource::Cpu, "CPU".to_string(), percent(t.cpu.map(|c| c * 100.0)), &h.cpu, 1.0),
            (Resource::Memory, "Memory".to_string(), format!("{} / {}", bytes(t.memory_used), bytes(t.memory_total)), &h.memory, 1.0),
        ];
        for (i, d) in self.machine.disks.iter().enumerate() {
            let active = self.disk_rate(&d.name).map(|r| r.active * 100.0);
            if let Some(series) = h.disks.get(&d.name) {
                out.push((Resource::Disk(d.name.clone()), format!("Disk {i}"), percent(active), series, 1.0));
            }
        }
        for n in &self.machine.nets {
            if let Some(series) = h.nets.get(&n.name) {
                let figure = self.net_rate(&n.name).map_or("—".to_string(), |r| rate(Some(r.rx + r.tx)));
                let title = if n.wireless { format!("Wi-Fi · {}", n.name) } else { format!("Ethernet · {}", n.name) };
                out.push((Resource::Net(n.name.clone()), title, figure, series, series.ceiling(64.0 * 1024.0)));
            }
        }
        for (i, g) in self.machine.gpus.iter().enumerate() {
            if let Some(series) = h.gpus.get(&g.card) {
                let figure = if g.power.as_deref() == Some("suspended") {
                    "Asleep".to_string()
                } else {
                    percent(self.gpu_busy(g))
                };
                out.push((Resource::Gpu(g.card.clone()), format!("GPU {i} · {}", g.driver), figure, series, 1.0));
            }
        }
        if let Some(b) = &self.machine.battery {
            out.push((Resource::Battery, "Battery".to_string(), format!("{} %", b.percent), &h.battery, 1.0));
        }
        out
    }

    /// A card's busy figure, from wherever its driver keeps one.
    fn gpu_busy(&self, g: &hyprforge_procman::machine::Gpu) -> Option<f32> {
        match g.driver.as_str() {
            "nvidia" => self.nvidia.as_ref().and_then(|n| n.as_ref().ok()).and_then(|n| n.busy),
            _ => g.busy_percent.map(f32::from),
        }
    }

    fn disk_rate(&self, name: &str) -> Option<DiskRate> {
        self.disk_rates.iter().find(|(n, _)| n == name).map(|(_, r)| *r)
    }

    fn net_rate(&self, name: &str) -> Option<NetRate> {
        self.net_rates.iter().find(|(n, _)| n == name).map(|(_, r)| *r)
    }

    fn detail(&self, s: FontScale) -> Element<'_, Message> {
        let t = self.totals;
        let m = &self.machine;
        let pressure = |share: Option<f32>| share.map_or("—".to_string(), |p| format!("{p:.0} % of the last 10 s"));
        let celsius = |c: Option<f32>| c.map_or("—".to_string(), |c| format!("{c:.0} °C"));
        match &self.resource {
            Resource::Cpu => {
                let mut facts = column![
                    widgets::fact("Processes", t.processes.to_string(), s),
                    widgets::fact("Threads", t.threads.to_string(), s),
                    widgets::fact("Cores", self.history.cores.len().to_string(), s),
                    widgets::fact("Temperature", celsius(m.cpu_celsius), s),
                    widgets::fact("Waiting for CPU", pressure(m.pressure.map(|p| p.cpu_some)), s),
                ]
                .spacing(spacing::XS);
                for f in &m.fans {
                    facts = facts.push(widgets::fact(f.label.clone(), format!("{} rpm", f.rpm), s));
                }
                if let Some(w) = m.gpus.iter().find(|g| g.driver == "amdgpu").and_then(|g| g.watts) {
                    facts = facts.push(widgets::fact("Package power", format!("{w:.1} W (CPU and GPU together)"), s));
                }
                let mut cores = column![widgets::section_label("Each core", s)].spacing(spacing::SM);
                let per_row = 4;
                for (n, series) in self.history.cores.chunks(per_row).enumerate() {
                    let mut r = row![].spacing(spacing::SM);
                    for (i, core) in series.iter().enumerate() {
                        let label = format!("{} · {}", n * per_row + i, percent(core.last().map(|v| v * 100.0)));
                        r = r.push(
                            column![widgets::meta_text(label, density::META_TEXT_BASE, s), self.graph(core, 1.0, s.apply(48.0))]
                                .spacing(spacing::XS)
                                .width(Length::FillPortion(1)),
                        );
                    }
                    cores = cores.push(r);
                }
                self.page("CPU", percent(t.cpu.map(|c| c * 100.0)), &self.history.cpu, 1.0, facts.into(), Some(cores.into()), s)
            }
            Resource::Memory => {
                let mut facts = column![
                    widgets::fact("In use", bytes(t.memory_used), s),
                    widgets::fact("Available", bytes(t.memory_total.saturating_sub(t.memory_used)), s),
                    widgets::fact("Waiting for memory", pressure(m.pressure.map(|p| p.memory_some)), s),
                ]
                .spacing(spacing::XS);
                if t.swap_total > 0 {
                    facts = facts.push(widgets::fact("Swap", format!("{} of {}", bytes(t.swap_used), bytes(t.swap_total)), s));
                }
                let swap = (t.swap_total > 0).then(|| {
                    column![widgets::section_label("Swap", s), self.graph(&self.history.swap, 1.0, s.apply(80.0))].spacing(spacing::SM).into()
                });
                self.page("Memory", format!("{} / {}", bytes(t.memory_used), bytes(t.memory_total)), &self.history.memory, 1.0, facts.into(), swap, s)
            }
            Resource::Disk(name) => {
                let Some(d) = m.disks.iter().find(|d| &d.name == name) else { return self.gone(s) };
                let r = self.disk_rate(name).unwrap_or_default();
                let facts = column![
                    widgets::fact("Drive", d.model.clone().unwrap_or_else(|| d.name.clone()), s),
                    widgets::fact("Read", rate(Some(r.read)), s),
                    widgets::fact("Write", rate(Some(r.write)), s),
                    widgets::fact("Temperature", celsius(d.celsius), s),
                    // PSI has no per-device figure: this is every drive's.
                    widgets::fact("Waiting for storage", pressure(m.pressure.map(|p| p.io_some)), s),
                ]
                .spacing(spacing::XS);
                let series = self.history.disks.get(name);
                match series {
                    Some(series) => self.page("Active time", percent(Some(r.active * 100.0)), series, 1.0, facts.into(), None, s),
                    None => self.gone(s),
                }
            }
            Resource::Net(name) => {
                let Some(n) = m.nets.iter().find(|n| &n.name == name) else { return self.gone(s) };
                let r = self.net_rate(name).unwrap_or_default();
                let facts = column![
                    widgets::fact("Adapter", n.name.clone(), s),
                    widgets::fact("Receiving", rate(Some(r.rx)), s),
                    widgets::fact("Sending", rate(Some(r.tx)), s),
                ]
                .spacing(spacing::XS);
                match self.history.nets.get(name) {
                    Some(series) => {
                        let ceiling = series.ceiling(64.0 * 1024.0);
                        let title = if n.wireless { "Wi-Fi" } else { "Ethernet" };
                        let figure = format!("scale {}", rate(Some(f64::from(ceiling))));
                        self.page(title, figure, series, ceiling, facts.into(), None, s)
                    }
                    None => self.gone(s),
                }
            }
            Resource::Gpu(card) => {
                let Some(g) = m.gpus.iter().find(|g| &g.card == card) else { return self.gone(s) };
                let asleep = g.power.as_deref() == Some("suspended");
                let nvidia = (g.driver == "nvidia").then_some(self.nvidia.as_ref()).flatten();
                let reading = nvidia.and_then(|n| n.as_ref().ok());
                let mut facts = column![
                    widgets::fact("Driver", g.driver.clone(), s),
                    widgets::fact(
                        "Power",
                        match g.power.as_deref() {
                            Some("suspended") => "Asleep, saving power — not woken to be read",
                            Some("active") => "Awake",
                            _ => "—",
                        },
                        s
                    ),
                ]
                .spacing(spacing::XS);
                if let Some(r) = reading {
                    facts = facts.push(widgets::fact("Model", r.name.clone(), s));
                    facts = facts.push(widgets::fact("Temperature", celsius(r.celsius), s));
                    if let (Some(used), Some(total)) = (r.vram_used, r.vram_total) {
                        facts = facts.push(widgets::fact("Video memory", format!("{} of {}", bytes(used), bytes(total)), s));
                    }
                    if let Some(w) = r.watts {
                        facts = facts.push(widgets::fact("Power draw", format!("{w:.1} W"), s));
                    }
                    if let Some(c) = r.clock_mhz {
                        facts = facts.push(widgets::fact("Clock", format!("{c} MHz of {} · {}", r.max_clock_mhz.map_or("—".to_string(), |m| m.to_string()), r.pstate), s));
                    }
                } else if let Some(Err(why)) = nvidia {
                    facts = facts.push(widgets::fact("Can't read it", why.clone(), s));
                } else if !asleep && g.driver != "nvidia" {
                    facts = facts.push(widgets::fact("Temperature", celsius(g.celsius), s));
                }
                if let (Some(used), Some(total)) = (g.vram_used, g.vram_total) {
                    facts = facts.push(widgets::fact("Video memory", format!("{} of {}", bytes(used), bytes(total)), s));
                }
                if let Some(w) = g.watts {
                    facts = facts.push(widgets::fact("Power", format!("{w:.1} W"), s));
                }
                let figure = if asleep { "Asleep".to_string() } else { percent(self.gpu_busy(g)) };
                match self.history.gpus.get(card) {
                    Some(series) => self.page("GPU", figure, series, 1.0, facts.into(), None, s),
                    None => self.gone(s),
                }
            }
            Resource::Battery => {
                let Some(b) = &m.battery else { return self.gone(s) };
                let mut facts = column![widgets::fact("Status", b.status.clone(), s)].spacing(spacing::XS);
                if let Some(w) = b.watts.filter(|w| *w > 0.05) {
                    facts = facts.push(widgets::fact("Power", format!("{w:.1} W"), s));
                }
                self.page("Battery", format!("{} %", b.percent), &self.history.battery, 1.0, facts.into(), None, s)
            }
        }
    }

    /// A resource that was there and is not now (a drive unplugged).
    fn gone(&self, s: FontScale) -> Element<'_, Message> {
        widgets::hint_text("This isn't there any more.", s).into()
    }

    #[allow(clippy::too_many_arguments)]
    fn page<'a>(
        &self,
        title: &'a str,
        figure: String,
        series: &'a Series,
        ceiling: f32,
        facts: Element<'a, Message>,
        below: Option<Element<'a, Message>>,
        s: FontScale,
    ) -> Element<'a, Message> {
        let head = row![
            widgets::scaled_text(title, 22.0, s),
            Space::new().width(Length::Fill),
            widgets::scaled_text(figure, 22.0, s).font(theme::mono_font()),
        ]
        .align_y(Alignment::Center);
        let mut page = column![head, self.graph(series, ceiling, s.apply(220.0)), facts].spacing(spacing::MD);
        if let Some(below) = below {
            page = page.push(below);
        }
        page.into()
    }

    fn graph<'a>(&self, series: &'a Series, ceiling: f32, height: f32) -> Element<'a, Message> {
        container(canvas(Graph { series, ceiling, line: theme::text(), grid: surface::card_border() }).width(Length::Fill).height(Length::Fixed(height)))
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
    fn a_gpu_load_sits_beside_the_card_it_was_measured_on() {
        let cards = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(gpu_cell(&(Some(0.4), cards(&["AMD", "NVIDIA"]))), "AMD 0.4 % · NVIDIA");
        assert_eq!(gpu_cell(&(None, cards(&["NVIDIA"]))), "NVIDIA");
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

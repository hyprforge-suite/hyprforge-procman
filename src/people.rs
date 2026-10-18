//! The Startup and Users tabs.

use crate::view::{bytes, percent, CPU_W, MEM_W};
use crate::{App, Message};
use hyprforge_procman::sessions::{self, Session, SessionAction};
use hyprforge_procman::startup;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::{column, container, row, scrollable, Space};
use iced::{Alignment, Background, Border, Element, Length};

/// A measured start time, or a dash when systemd didn't time it.
fn took(ms: Option<u32>) -> String {
    match ms {
        Some(ms) if ms >= 1000 => format!("{:.1} s", f64::from(ms) / 1000.0),
        Some(ms) => format!("{ms} ms"),
        None => "—".to_string(),
    }
}

impl App {
    /// One table, Task Manager's shape: what starts, whether it is
    /// running, how long it took — beside a bar against the slowest — and
    /// its switch. The slowest first.
    pub fn startup_view(&self, s: FontScale) -> Element<'_, Message> {
        let Some(data) = &self.startup else {
            return container(widgets::hint_text("Reading what starts with you…", s)).padding(spacing::LG).into();
        };
        let state_of = |unit: &str| data.units.iter().find(|u| u.name == unit);
        let mut apps: Vec<&startup::Autostart> = data.autostarts.iter().collect();
        apps.sort_by_key(|a| std::cmp::Reverse(data.blame.get(&startup::autostart_unit(&a.id)).copied().unwrap_or(0)));
        let mut units: Vec<&hyprforge_procman::services::Unit> = data
            .units
            .iter()
            .filter(|u| !u.name.ends_with("@autostart.service") && matches!(u.file.as_deref(), Some("enabled") | Some("disabled")))
            .filter(|u| u.file.as_deref() == Some("enabled") || u.active == "active")
            .collect();
        units.sort_by_key(|u| std::cmp::Reverse(data.blame.get(&u.name).copied().unwrap_or(0)));
        // Against the slowest on this list, not every unit systemd timed:
        // a system unit's seconds would make every row here a sliver.
        let slowest = apps
            .iter()
            .filter_map(|a| data.blame.get(&startup::autostart_unit(&a.id)))
            .chain(units.iter().filter_map(|u| data.blame.get(&u.name)))
            .copied()
            .max()
            .unwrap_or(1)
            .max(1);
        let icon_w = s.apply(20.0);
        let line = |icon: Element<'static, Message>, name: String, detail: String, running: Option<bool>, ms: Option<u32>, control: Element<'static, Message>| -> Element<'static, Message> {
            let status = match running {
                Some(true) => widgets::scaled_text("Running", density::META_TEXT_BASE, s).color(theme::text()),
                Some(false) => widgets::meta_text("Not running", density::META_TEXT_BASE, s),
                None => widgets::meta_text("", density::META_TEXT_BASE, s),
            };
            // How long, as a figure and as a share of the slowest: a slow
            // one is seen before it is read.
            let share = ms.map_or(0.0, |ms| ms as f32 / slowest as f32);
            let fill = container(Space::new()).width(Length::FillPortion(((share * 1000.0) as u16).max(1))).height(Length::Fixed(4.0)).style(|_t: &iced::Theme| container::Style {
                background: Some(Background::Color(theme::text_dim())),
                ..container::Style::default()
            });
            let track = container(row![fill, Space::new().width(Length::FillPortion((((1.0 - share) * 1000.0) as u16).max(1)))].width(Length::Fill)).width(Length::Fill).style(|_t: &iced::Theme| container::Style {
                background: Some(Background::Color(surface::card_border())),
                ..container::Style::default()
            });
            let time = row![
                container(widgets::scaled_text(ms.map_or(String::new(), |ms| took(Some(ms))), density::ROW_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(60.0))).align_x(Alignment::End),
                if ms.is_some() { Element::from(track) } else { Space::new().width(Length::Fill).into() },
            ]
            .spacing(spacing::SM)
            .align_y(Alignment::Center);
            row![
                container(icon).width(Length::Fixed(icon_w)),
                row![
                    widgets::clamped_text(name, 1).size(s.apply(density::ROW_TEXT_BASE)).width(Length::Shrink),
                    widgets::clamped_text(detail, 1).size(s.apply(density::META_TEXT_BASE)).width(Length::Fill).font(theme::mono_font()).color(theme::text_dim()),
                ]
                .spacing(spacing::MD)
                .align_y(Alignment::Center)
                .width(Length::Fill),
                container(status).width(Length::Fixed(s.apply(110.0))),
                container(time).width(Length::Fixed(s.apply(220.0))),
                container(control).width(Length::Fixed(s.apply(70.0))).align_x(Alignment::End),
            ]
            .spacing(spacing::MD)
            .align_y(Alignment::Center)
            .padding([spacing::SM, spacing::SM])
            .into()
        };
        let heading = |label: &str, aside: Option<Element<'static, Message>>| -> Element<'static, Message> {
            let mut r = row![widgets::meta_text(label.to_string(), density::META_TEXT_BASE, s), Space::new().width(Length::Fill)].align_y(Alignment::Center);
            if let Some(aside) = aside {
                r = r.push(aside);
            }
            container(r).padding(iced::Padding { top: spacing::MD, bottom: spacing::XS, left: spacing::SM, right: spacing::SM }).into()
        };
        let switch = |on: bool, toggle: Box<dyn Fn(bool) -> Message>| -> Element<'static, Message> { widgets::toggle(on, s).on_toggle(toggle).into() };

        let off = apps.iter().filter(|a| !a.desktop.enabled()).count() + units.iter().filter(|u| u.file.as_deref() != Some("enabled")).count();
        let starting = apps.len() + units.len() - off;

        let mut summary = row![widgets::scaled_text(format!("{starting} start when you sign in"), density::ROW_TEXT_BASE, s)].spacing(spacing::MD).align_y(Alignment::Center);
        if off > 0 {
            summary = summary.push(widgets::meta_text(format!("{off} switched off"), density::META_TEXT_BASE, s).font(theme::mono_font()));
        }
        let head = |label: &'static str, w: f32, end: bool| container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(w))).align_x(if end { Alignment::End } else { Alignment::Start });
        let mut table = column![row![
            Space::new().width(Length::Fixed(icon_w)),
            widgets::meta_text("Name", density::META_TEXT_BASE, s).width(Length::Fill),
            head("Status", 110.0, false),
            head("Start time ↓", 220.0, false),
            head("Starts", 70.0, true),
        ]
        .spacing(spacing::MD)
        .padding([spacing::XS, spacing::SM])]
        .spacing(0.0);

        table = table.push(heading("Apps", None));
        if apps.is_empty() {
            table = table.push(container(widgets::hint_text("Nothing installed asks to start with you.", s)).padding(spacing::SM));
        }
        for a in apps {
            let unit = startup::autostart_unit(&a.id);
            let id = a.id.clone();
            let icon = crate::app_icons::view(self.icons.peek_named(&a.desktop.icon), icon_w);
            let running = state_of(&unit).map(|u| u.active == "active").or(Some(false));
            table = table
                .push(widgets::divider())
                .push(line(icon, a.desktop.name.clone(), a.desktop.exec.clone(), running, data.blame.get(&unit).copied(), switch(a.desktop.enabled(), Box::new(move |v| Message::Autostart(id.clone(), v)))));
        }
        table = table.push(heading("Services", None));
        for u in units {
            let name = u.name.clone();
            let title = if u.description.is_empty() { u.name.clone() } else { u.description.clone() };
            table = table.push(widgets::divider()).push(line(
                crate::app_icons::view(None, icon_w),
                title,
                u.name.clone(),
                Some(u.active == "active"),
                data.blame.get(&u.name).copied(),
                switch(u.file.as_deref() == Some("enabled"), Box::new(move |v| Message::UnitAtLogin(name.clone(), v))),
            ));
        }
        table = table.push(heading("Started by Hyprland", Some(widgets::secondary_button("Edit in Settings").on_press(Message::OpenSessionSettings).into())));
        match &data.hyprland {
            Err(e) => table = table.push(container(widgets::hint_text(e.clone(), s)).padding(spacing::SM)),
            Ok(programs) if programs.is_empty() => table = table.push(container(widgets::hint_text("Settings → Session lists none.", s)).padding(spacing::SM)),
            Ok(programs) => {
                for p in programs {
                    let state: Element<'static, Message> = widgets::meta_text(if p.enabled { "Starts" } else { "Off" }, density::META_TEXT_BASE, s).into();
                    table = table.push(widgets::divider()).push(line(crate::app_icons::view(None, icon_w), p.command.clone(), p.when.label().to_string(), None, None, state));
                }
            }
        }
        // Said once, because its absence is easy to mistake for a bug.
        table = table.push(container(widgets::meta_text("Lines written by hand in hyprland.lua aren't listed.", density::META_TEXT_BASE, s)).padding(spacing::SM));
        scrollable(column![summary, table].spacing(spacing::MD).padding(spacing::LG)).height(Length::Fill).into()
    }

    pub fn users_view(&self, s: FontScale) -> Element<'_, Message> {
        let sessions = match &self.sessions {
            Ok(list) => list.as_slice(),
            Err(e) => return container(widgets::hint_text(e.clone(), s)).padding(spacing::LG).into(),
        };
        let rows = sessions::users(sessions, hyprforge_procman::model::entries(&self.table));
        let mut page = column![].spacing(spacing::MD).padding(spacing::LG);
        for u in rows {
            let head = row![
                widgets::scaled_text(u.user.clone(), 18.0, s).width(Length::Fill),
                widgets::meta_text(format!("{} processes", u.processes), density::META_TEXT_BASE, s),
                container(widgets::scaled_text(percent(u.cpu), density::ROW_TEXT_BASE, s).font(theme::mono_font()))
                    .width(Length::Fixed(s.apply(CPU_W)))
                    .align_x(Alignment::End),
                container(widgets::scaled_text(bytes(u.private_bytes), density::ROW_TEXT_BASE, s).font(theme::mono_font()))
                    .width(Length::Fixed(s.apply(MEM_W)))
                    .align_x(Alignment::End),
            ]
            .spacing(spacing::MD)
            .align_y(Alignment::Center);
            let mut card = column![head].spacing(spacing::SM);
            if u.sessions.is_empty() {
                card = card.push(widgets::hint_text("Not signed in — these are services running as this user.", s));
            }
            for session in &u.sessions {
                card = card.push(self.session_line(session, s));
            }
            page = page.push(container(card).padding(spacing::MD).width(Length::Fill).style(|_t: &iced::Theme| container::Style {
                background: Some(Background::Color(surface::card())),
                border: Border { radius: density::card_radius().into(), width: 1.0, color: surface::card_border() },
                ..container::Style::default()
            }));
        }
        scrollable(page).height(Length::Fill).into()
    }

    fn session_line(&self, session: &Session, s: FontScale) -> Element<'_, Message> {
        let since = chrono::DateTime::from_timestamp((session.since_us / 1_000_000) as i64, 0)
            .map(|t| t.with_timezone(&chrono::Local).format("since %a %H:%M").to_string())
            .unwrap_or_default();
        let place = if session.seat.is_empty() { session.tty.clone() } else { session.seat.clone() };
        let mut facts = vec![format!("Session {}", session.id), session.kind.clone(), place, session.service.clone(), since];
        facts.retain(|f| !f.is_empty());
        let (tint, state) = if session.locked {
            (widgets::Tint::Info, "Locked")
        } else if session.idle {
            (widgets::Tint::Dim, "Idle")
        } else if session.active {
            (widgets::Tint::Success, "Active")
        } else {
            (widgets::Tint::Dim, "In the background")
        };
        let mut line = row![
            widgets::status_dot(tint, s),
            widgets::scaled_text(state, density::ROW_TEXT_BASE, s),
            widgets::meta_text(facts.join(" · "), density::META_TEXT_BASE, s).width(Length::Fill),
        ]
        .spacing(spacing::SM)
        .align_y(Alignment::Center);
        if session.remote {
            line = line.push(widgets::chip("Remote", widgets::Tint::Info, s));
        }
        line.push(widgets::secondary_button("Lock").on_press_maybe((!session.locked).then(|| Message::Session(session.id.clone(), SessionAction::Lock))))
            .push(widgets::secondary_button("Sign out").on_press(Message::Session(session.id.clone(), SessionAction::SignOut)))
            .into()
    }

    pub fn sign_out_card(&self, session: &Session) -> Element<'_, Message> {
        let s = crate::view::scale_now();
        let mine = session.uid == unsafe { libc::getuid() };
        let what = if mine {
            "Every app in it closes, and anything they haven't saved is lost — including this window, if it is this session."
        } else {
            "Every app in it closes, and anything they haven't saved is lost. It isn't yours, so you'll be asked for your password."
        };
        let card = column![
            widgets::scaled_text(format!("Sign {} out of session {}?", session.user, session.id), 18.0, s),
            widgets::meta_text(what, density::ROW_TEXT_BASE, s),
            row![
                Space::new().width(Length::Fill),
                widgets::secondary_button("Cancel").on_press(Message::Dismiss),
                widgets::danger_button("Sign out", Message::SignOutConfirmed),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_times_read_in_the_unit_that_suits_them() {
        assert_eq!(took(Some(27)), "27 ms");
        assert_eq!(took(Some(1289)), "1.3 s");
        assert_eq!(took(None), "—");
    }
}

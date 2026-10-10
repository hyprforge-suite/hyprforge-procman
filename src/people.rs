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
    pub fn startup_view(&self, s: FontScale) -> Element<'_, Message> {
        let Some(data) = &self.startup else {
            return container(widgets::hint_text("Reading what starts with you…", s)).padding(spacing::LG).into();
        };
        let state_of = |unit: &str| data.units.iter().find(|u| u.name == unit);
        let line = |name: String, detail: String, running: Option<bool>, ms: Option<u32>, control: Element<'static, Message>| -> Element<'static, Message> {
            let status = match running {
                Some(true) => row![widgets::status_dot(widgets::Tint::Success, s), widgets::meta_text("Running", density::META_TEXT_BASE, s)],
                Some(false) => row![widgets::status_dot(widgets::Tint::Dim, s), widgets::meta_text("Not running", density::META_TEXT_BASE, s)],
                None => row![Space::new()],
            }
            .spacing(spacing::SM)
            .align_y(Alignment::Center);
            row![
                column![
                    widgets::scaled_text(name, density::ROW_TEXT_BASE, s),
                    widgets::clamped_text(detail, 1).size(s.apply(density::META_TEXT_BASE)).width(Length::Fill).font(theme::mono_font()).color(theme::text_dim()),
                ]
                .width(Length::Fill),
                container(status).width(Length::Fixed(s.apply(130.0))),
                container(widgets::scaled_text(took(ms), density::ROW_TEXT_BASE, s).font(theme::mono_font()))
                    .width(Length::Fixed(s.apply(90.0)))
                    .align_x(Alignment::End),
                container(control).width(Length::Fixed(s.apply(170.0))).align_x(Alignment::End),
            ]
            .spacing(spacing::MD)
            .align_y(Alignment::Center)
            .padding([spacing::SM, spacing::MD])
            .into()
        };

        let mut page = column![
            widgets::meta_text(
                "Start time is measured by systemd at this sign-in, not rated: the slowest come first.",
                density::META_TEXT_BASE,
                s
            ),
            widgets::section_label("Apps that start themselves", s),
        ]
        .spacing(spacing::SM)
        .padding(spacing::LG);

        let mut apps: Vec<&startup::Autostart> = data.autostarts.iter().collect();
        apps.sort_by_key(|a| std::cmp::Reverse(data.blame.get(&startup::autostart_unit(&a.id)).copied().unwrap_or(0)));
        if apps.is_empty() {
            page = page.push(widgets::hint_text("Nothing installed asks to start with you.", s));
        }
        for a in apps {
            let unit = startup::autostart_unit(&a.id);
            let on = a.desktop.enabled();
            let running = state_of(&unit).map(|u| u.active == "active").or(Some(false));
            let toggle: Element<'static, Message> = {
                let id = a.id.clone();
                widgets::toggle(on, s).label(if on { "Starts" } else { "Off" }).on_toggle(move |v| Message::Autostart(id.clone(), v)).into()
            };
            page = page.push(line(a.desktop.name.clone(), a.desktop.exec.clone(), running, data.blame.get(&unit).copied(), toggle));
        }

        page = page.push(widgets::section_label("Services that start with you", s));
        let mut units: Vec<&hyprforge_procman::services::Unit> = data
            .units
            .iter()
            .filter(|u| !u.name.ends_with("@autostart.service") && matches!(u.file.as_deref(), Some("enabled") | Some("disabled")))
            .filter(|u| u.file.as_deref() == Some("enabled") || u.active == "active")
            .collect();
        units.sort_by_key(|u| std::cmp::Reverse(data.blame.get(&u.name).copied().unwrap_or(0)));
        for u in units {
            let on = u.file.as_deref() == Some("enabled");
            let name = u.name.clone();
            let toggle: Element<'static, Message> =
                widgets::toggle(on, s).label(if on { "Starts" } else { "Off" }).on_toggle(move |v| Message::UnitAtLogin(name.clone(), v)).into();
            let title = if u.description.is_empty() { u.name.clone() } else { u.description.clone() };
            page = page.push(line(title, u.name.clone(), Some(u.active == "active"), data.blame.get(&u.name).copied(), toggle));
        }

        page = page.push(
            row![widgets::section_label("Started by Hyprland", s), Space::new().width(Length::Fill), widgets::secondary_button("Edit in Settings").on_press(Message::OpenSessionSettings)]
                .align_y(Alignment::Center),
        );
        match &data.hyprland {
            Err(e) => page = page.push(widgets::hint_text(e.clone(), s)),
            Ok(programs) if programs.is_empty() => {
                page = page.push(widgets::hint_text("Settings → Session lists none.", s));
            }
            Ok(programs) => {
                for p in programs {
                    let state: Element<'static, Message> = widgets::meta_text(if p.enabled { "Starts" } else { "Off" }, density::META_TEXT_BASE, s).into();
                    page = page.push(line(p.command.clone(), p.when.label().to_string(), None, None, state));
                }
            }
        }
        page = page.push(widgets::hint_text(
            "Lines written by hand in hyprland.lua aren't read here; Settings → Session keeps the ones it manages.",
            s,
        ));
        scrollable(page).height(Length::Fill).into()
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

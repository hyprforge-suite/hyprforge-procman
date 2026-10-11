//! The Details tab: every process, every column, as a tree or a flat
//! list, and what the selected one has open.

use crate::view::{bytes, percent, rate, CPU_W, DISK_W, MEM_W, NAME_PORTION, PID_W, USER_W};
use crate::{App, Message};
use hyprforge_procman::model::{self, Column, Entry};
use hyprforge_procman::signal::How;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::{button, column, container, row, scrollable, Space};
use iced::{Alignment, Background, Border, Element, Length};

const NICE_W: f32 = 70.0;
/// The Advanced columns, after Priority.
const ADVANCED: [(&str, f32); 7] =
    [("user · sys", 110.0), ("Swap", 84.0), ("Faults/s", 100.0), ("Switches/s", 116.0), ("Core", 60.0), ("Policy", 92.0), ("Virtual", 96.0)];
/// The name column when the table scrolls sideways and cannot fill.
const ADVANCED_NAME_W: f32 = 320.0;

/// The kernel's scheduling policies, in words.
pub fn policy(p: u32) -> &'static str {
    match p {
        0 => "normal",
        1 => "FIFO",
        2 => "round-robin",
        3 => "batch",
        5 => "idle",
        6 => "deadline",
        _ => "other",
    }
}

/// `4.1k`, for counts a second.
fn per_second(v: f64) -> String {
    if v >= 1000.0 {
        format!("{:.1}k", v / 1000.0)
    } else {
        format!("{v:.0}")
    }
}
const TIME_W: f32 = 96.0;
const THREADS_W: f32 = 76.0;

/// `1:02:03`, or `2:03` under an hour.
pub fn duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl App {
    pub fn details_view(&self, s: FontScale) -> Element<'_, Message> {
        let advanced = self.advanced;
        let name_width = if advanced { Length::Fixed(s.apply(ADVANCED_NAME_W)) } else { Length::FillPortion(NAME_PORTION) };
        let mut heads = row![
            self.column_head("Name", Column::Name, name_width, false, s),
            self.column_head("PID", Column::Pid, Length::Fixed(s.apply(PID_W)), true, s),
            self.column_head("User", Column::User, Length::Fixed(s.apply(USER_W)), false, s),
            self.column_head("CPU", Column::Cpu, Length::Fixed(s.apply(CPU_W)), true, s),
            container(widgets::meta_text("CPU time", density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(TIME_W))).padding([spacing::XS, spacing::SM]).align_x(Alignment::End),
            self.column_head("Memory", Column::Memory, Length::Fixed(s.apply(MEM_W)), true, s),
            self.column_head("Disk", Column::Disk, Length::Fixed(s.apply(DISK_W)), true, s),
            container(widgets::meta_text("Threads", density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(THREADS_W))).padding([spacing::XS, spacing::SM]).align_x(Alignment::End),
            container(widgets::meta_text("Priority", density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(NICE_W))).padding([spacing::XS, spacing::SM]).align_x(Alignment::End),
        ]
        .padding([0.0, spacing::SM]);
        if advanced {
            for (label, w) in ADVANCED {
                heads = heads.push(
                    container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(w))).padding([spacing::XS, spacing::SM]).align_x(Alignment::End),
                );
            }
        }

        let lines = model::details(&self.table, &self.view, self.details.tree);
        let mut list = column![].spacing(1.0).padding([spacing::XS, spacing::SM]);
        if self.view.only.is_some() {
            // Claimed from what was searched, not from whether Show
            // everything is on: an older helper can be on and not search.
            let note = if self.lookup_everyone {
                "Looking at every process."
            } else if self.everything_open() {
                "Looking at your own processes only — the helper couldn't search the rest."
            } else {
                "Looking at your own processes only — Show everything to search the rest."
            };
            list = list.push(container(widgets::hint_text(note, s)).padding(spacing::SM));
        }
        if lines.is_empty() {
            list = list.push(container(widgets::hint_text("Nothing matches the search.", s)).padding(spacing::MD));
        }
        let tps = hyprforge_procman::proc::ticks_per_second();
        for (depth, e) in lines {
            list = list.push(self.detail_row(depth, e, tps, s));
        }
        let controls = row![
            widgets::segmented_choice(&[true, false], Some(&self.details.tree), |t| if *t { "Tree".to_string() } else { "List".to_string() }, Message::Tree, s),
            Space::new().width(Length::Fill),
        ]
        .padding([spacing::SM, spacing::MD]);
        let table: Element<'_, Message> = if advanced {
            // Wider than the window: the header and the rows scroll
            // sideways together, and the rows alone scroll down.
            let fixed = [PID_W, USER_W, CPU_W, TIME_W, MEM_W, DISK_W, THREADS_W, NICE_W].iter().sum::<f32>() + ADVANCED.iter().map(|(_, w)| w).sum::<f32>();
            let width = s.apply(ADVANCED_NAME_W + fixed) + spacing::SM * 4.0;
            scrollable(
                column![heads, widgets::divider(), scrollable(list).height(Length::Fill)]
                    .width(Length::Fixed(width))
                    .height(Length::Fill),
            )
            .direction(iced::widget::scrollable::Direction::Horizontal(iced::widget::scrollable::Scrollbar::default()))
            .height(Length::FillPortion(3))
            .into()
        } else {
            column![heads, widgets::divider(), scrollable(list).height(Length::Fill)].height(Length::FillPortion(3)).into()
        };
        let mut page = column![controls, widgets::divider(), table];
        if let Some(entry) = self.selected.and_then(|id| model::entries(&self.table).find(|e| e.id == id)) {
            page = page.push(widgets::divider()).push(self.properties(entry, s));
        }
        page.height(Length::Fill).into()
    }

    fn detail_row(&self, depth: usize, e: Entry, tps: u64, s: FontScale) -> Element<'_, Message> {
        let selected = self.selected == Some(e.id);
        let cell = |content: String, width: f32| {
            container(widgets::scaled_text(content, density::ROW_TEXT_BASE, s).font(theme::mono_font()))
                .width(Length::Fixed(s.apply(width)))
                .padding([0.0, spacing::SM])
                .align_x(Alignment::End)
        };
        let indent = Space::new().width(Length::Fixed(s.apply(14.0) * depth as f32));
        let name = row![indent, self.entry_name(&e, s)].align_y(Alignment::Center);
        let name_width = if self.advanced { Length::Fixed(s.apply(ADVANCED_NAME_W)) } else { Length::FillPortion(NAME_PORTION) };
        let mut line = row![
            container(name).width(name_width).padding([0.0, spacing::SM]).clip(true),
            cell(e.id.pid.to_string(), PID_W),
            container(widgets::meta_text(e.user.clone(), density::ROW_TEXT_BASE, s).color(crate::view::ink(selected))).width(Length::Fixed(s.apply(USER_W))).padding([0.0, spacing::SM]),
            cell(percent(e.cpu), CPU_W),
            cell(duration(e.ticks / tps), TIME_W),
            cell(bytes(e.private_bytes), MEM_W),
            cell(rate(e.disk), DISK_W),
            cell(e.threads.to_string(), THREADS_W),
            cell(e.nice.to_string(), NICE_W),
        ]
        .align_y(Alignment::Center);
        if self.advanced {
            let d = e.detail;
            let values = [
                d.map_or("—".to_string(), |d| format!("{:.1} · {:.1}", d.user_pct, d.system_pct)),
                e.swap_bytes.map_or("—".to_string(), bytes),
                d.map_or("—".to_string(), |d| format!("{} · {}", per_second(d.minor_faults), per_second(d.major_faults))),
                match d.and_then(|d| Some((d.voluntary?, d.forced?))) {
                    Some((v, f)) => format!("{} · {}", per_second(v), per_second(f)),
                    None => "—".to_string(),
                },
                if e.processor >= 0 { e.processor.to_string() } else { "—".to_string() },
                policy(e.policy).to_string(),
                bytes(e.vsize),
            ];
            for (value, (_, w)) in values.into_iter().zip(ADVANCED) {
                line = line.push(cell(value, w));
            }
        }
        button(line)
            .padding([spacing::XS, 0.0])
            .width(Length::Fill)
            .on_press(Message::Select(e.id))
            .style(move |t, status| widgets::selectable_row_style(t, status, selected))
            .into()
    }

    fn properties<'a>(&'a self, e: &'a Entry, s: FontScale) -> Element<'a, Message> {
        let p = &self.details.properties;
        let hidden = "not yours to see";
        let started = {
            let tps = hyprforge_procman::proc::ticks_per_second();
            let at = self.boot + e.id.start / tps;
            chrono::DateTime::from_timestamp(at as i64, 0)
                .map(|t| t.with_timezone(&chrono::Local).format("%a %-d %b, %H:%M:%S").to_string())
                .unwrap_or_default()
        };
        let program = p.exe.as_ref().map_or(hidden.to_string(), |x| x.display().to_string());
        let mut facts = column![
            widgets::fact("Command", if e.command.is_empty() { "—".to_string() } else { e.command.clone() }, s),
            widgets::fact("Program", program, s),
            widgets::fact("Folder", p.cwd.as_ref().map_or(hidden.to_string(), |x| x.display().to_string()), s),
            widgets::fact("Runs in", p.unit.clone().unwrap_or_else(|| "—".to_string()), s),
            widgets::fact("Started", started, s),
            widgets::fact("Memory", format!("{} private · {} resident", bytes(e.private_bytes), bytes(e.resident_bytes)), s),
            widgets::fact("Open files", p.open_files.map_or(hidden.to_string(), |n| n.to_string()), s),
        ]
        .spacing(spacing::XS);
        match &p.sockets {
            None => facts = facts.push(widgets::fact("Network", hidden, s)),
            Some(sockets) if sockets.is_empty() => facts = facts.push(widgets::fact("Network", "nothing open", s)),
            Some(sockets) => {
                for (i, sock) in sockets.iter().take(8).enumerate() {
                    let proto = match sock.proto {
                        hyprforge_procman::inspect::Proto::Tcp => "TCP",
                        hyprforge_procman::inspect::Proto::Udp => "UDP",
                    };
                    let line = if sock.state == "Listening" {
                        format!("{proto} listening on {}:{}", sock.local.0, sock.local.1)
                    } else {
                        format!("{proto} {}:{} → {}:{} · {}", sock.local.0, sock.local.1, sock.remote.0, sock.remote.1, sock.state)
                    };
                    facts = facts.push(widgets::fact(if i == 0 { "Network" } else { "" }, line, s));
                }
                if sockets.len() > 8 {
                    facts = facts.push(widgets::fact("", format!("and {} more", sockets.len() - 8), s));
                }
            }
        }
        let frozen = matches!(e.state, 'T' | 't');
        let critical = self.critical.contains(&e.id.pid);
        let actions = row![
            widgets::secondary_button("Open file location").on_press_maybe(p.exe.as_ref().map(|_| Message::OpenLocation)),
            widgets::secondary_button("Lower priority").on_press_maybe((e.nice < 19).then_some(Message::Nicer(true))),
            widgets::secondary_button("Raise priority").on_press_maybe((e.nice > -20).then_some(Message::Nicer(false))),
            if frozen {
                widgets::secondary_button("Resume").on_press(Message::Freeze(false))
            } else {
                widgets::secondary_button("Freeze").on_press_maybe((!critical).then_some(Message::Freeze(true)))
            },
            widgets::secondary_button("End process tree").on_press(Message::EndTree),
            widgets::primary_button("End").on_press(Message::Ask(How::Ask)),
        ]
        .spacing(spacing::SM);
        let title = row![widgets::scaled_text(e.name.clone(), 18.0, s), Space::new().width(Length::Fill), actions].align_y(Alignment::Center);
        container(scrollable(column![title, facts].spacing(spacing::MD)))
            .padding(spacing::MD)
            .height(Length::FillPortion(2))
            .width(Length::Fill)
            .style(|_t: &iced::Theme| container::Style {
                background: Some(Background::Color(surface::sidebar())),
                border: Border::default(),
                ..container::Style::default()
            })
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_read_as_words() {
        assert_eq!(policy(0), "normal");
        assert_eq!(policy(1), "FIFO");
        assert_eq!(policy(6), "deadline");
        assert_eq!(per_second(4123.0), "4.1k");
    }

    #[test]
    fn cpu_time_reads_as_a_clock() {
        assert_eq!(duration(5), "0:05");
        assert_eq!(duration(125), "2:05");
        assert_eq!(duration(3 * 3600 + 7), "3:00:07");
    }
}

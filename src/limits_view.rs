//! The limits card: what an app may take, set from the window.
//!
//! Everything it decides is in `hyprforge_procman::limits`; this is the
//! card, what it is opened on, and the messages it sends.

use crate::view::bytes;
use crate::{App, Message};
use hyprforge_procman::limits::{self, Limits, Offer, Plan, Target};
use hyprforge_procman::sample::Id;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::{column, container, row, text_input, Space};
use iced::{Background, Border, Element, Length};
use std::path::Path;

/// The card, while it is open.
#[derive(Debug, Clone)]
pub struct Dialog {
    pub id: Id,
    pub name: String,
    pub plan: Plan,
    pub offer: Offer,
    /// What the kernel enforces on its unit now.
    pub current: Limits,
    pub memory: String,
    pub cpu: String,
    pub busy: bool,
    pub error: Option<String>,
}

pub fn cores() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

fn sys(cgroup: &str) -> std::path::PathBuf {
    Path::new("/sys/fs/cgroup").join(cgroup.trim_start_matches('/'))
}

/// A limit as the card's fields would hold it.
pub fn field_text(l: Limits) -> (String, String) {
    (l.memory.map(bytes).unwrap_or_default(), l.cpu_percent.map(|p| p.to_string()).unwrap_or_default())
}

/// The fields as limits, or what is wrong with them, in words.
pub fn parse_fields(memory: &str, cpu: &str, offer: Offer) -> Result<Limits, String> {
    let memory = match (offer.memory, memory.trim()) {
        (false, _) | (true, "") => None,
        (true, m) => Some(limits::parse_size(m).ok_or("Write the memory as a size, like 4 GB or 512 MB — 64 MB at the least.")?),
    };
    let cpu_percent = match (offer.cpu, cpu.trim().trim_end_matches('%').trim()) {
        (false, _) | (true, "") => None,
        (true, c) => Some(c.parse::<u32>().ok().filter(|p| (1..=100).contains(p)).ok_or("Write the CPU as a percent of the whole machine, from 1 to 100.")?),
    };
    Ok(Limits { memory, cpu_percent })
}

impl App {
    /// The app a process belongs to, for limiting: its whole group when
    /// it is part of one — people limit apps, not their helpers — and the
    /// process alone otherwise.
    fn limit_target(&self, id: Id) -> Option<(String, Vec<i32>, bool, i32)> {
        if let Some(app) = self.table.apps.iter().find(|a| a.members.iter().any(|m| m.id == id)) {
            let pids = app.members.iter().map(|m| m.id.pid).collect();
            return Some((app.lead.name.clone(), pids, app.lead.own, app.lead.id.pid));
        }
        let e = self.entry(id)?;
        Some((e.name.clone(), vec![e.id.pid], e.own, e.id.pid))
    }

    /// Whether limiting these could take the session with it. The
    /// window's "critical" set is everything it runs under, which is right
    /// for End — ending the terminal procman was started from ends procman
    /// — and wrong here: a terminal's memory can be capped safely, and
    /// counting it critical refused Ghostty in the render. What must never
    /// be limited is what the session runs on, and the compositor and the
    /// session's managers own no window of their own; an app with windows
    /// is an app, wherever it sits above this one.
    pub fn limit_would_endanger(&self, pids: &[i32]) -> bool {
        let windowed: std::collections::HashSet<i32> =
            self.table.apps.iter().filter(|a| a.windows > 0).flat_map(|a| a.members.iter().map(|m| m.id.pid)).collect();
        pids.iter().any(|p| self.critical.contains(p) && !windowed.contains(p))
    }

    pub fn open_limits(&mut self, id: Id) {
        let Some((name, pids, own, lead)) = self.limit_target(id) else { return };
        let proc_root = Path::new("/proc");
        let cgroup = limits::cgroup_of(proc_root, lead);
        let target = Target {
            name: name.clone(),
            lead,
            pids: pids.clone(),
            unit: hyprforge_procman::inspect::unit(proc_root, lead),
            in_unit: cgroup.as_deref().map(|c| limits::processes_in(&sys(c))).unwrap_or_default(),
            cgroup: cgroup.clone(),
            critical: self.limit_would_endanger(&pids),
            me: self.me,
            own,
        };
        let plan = limits::plan(&target);
        // What a limit can reach: the unit's own controllers, or, for a
        // scope yet to be made, those of the slice it would be made in.
        let controllers = match &plan {
            Plan::Unit(_) => cgroup.as_deref().and_then(|c| std::fs::read_to_string(sys(c).join("cgroup.controllers")).ok()),
            Plan::NewScope { .. } => std::fs::read_to_string(sys(&format!("/user.slice/user-{0}.slice/user@{0}.service/app.slice", self.me)).join("cgroup.controllers")).ok(),
            Plan::Refuse(_) => None,
        };
        let offer = limits::offer(&controllers.unwrap_or_default());
        let current = match (&plan, &cgroup) {
            (Plan::Unit(_), Some(c)) => limits::read(&sys(c), cores()),
            _ => Limits::default(),
        };
        let (memory, cpu) = field_text(current);
        self.limits = Some(Dialog { id, name, plan, offer, current, memory, cpu, busy: false, error: None });
    }

    /// Sends the card's limits to systemd — `None` removes them.
    pub fn apply_limits(&mut self, remove: bool) -> iced::Task<Message> {
        let Some(d) = self.limits.as_mut() else { return iced::Task::none() };
        let wanted = if remove { Ok(Limits::default()) } else { parse_fields(&d.memory, &d.cpu, d.offer) };
        let wanted = match wanted {
            Ok(l) => l,
            Err(why) => {
                d.error = Some(why);
                return iced::Task::none();
            }
        };
        d.busy = true;
        d.error = None;
        let limiter = self.limiter.clone();
        let pending = match d.plan.clone() {
            Plan::Unit(unit) => limiter.set(unit, wanted.properties(cores())),
            Plan::NewScope { name, pids } => {
                limiter.new_scope(name, format!("{} (limited by Processes)", d.name), pids.into_iter().map(|p| p as u32).collect(), wanted.properties(cores()))
            }
            Plan::Refuse(_) => return iced::Task::none(),
        };
        iced::Task::perform(pending, Message::LimitsDone)
    }

    pub fn limits_card<'a>(&'a self, d: &'a Dialog) -> Element<'a, Message> {
        let s = crate::view::scale_now();
        let mut card = column![widgets::scaled_text(format!("Limit {}", d.name), 18.0, s)].spacing(spacing::MD);
        if let Plan::Refuse(why) = &d.plan {
            card = card.push(widgets::meta_text(why.clone(), density::ROW_TEXT_BASE, s));
            card = card.push(row![Space::new().width(Length::Fill), widgets::secondary_button("Close").on_press(Message::CloseLimits)]);
            return frame(card, s);
        }
        let how = match &d.plan {
            Plan::NewScope { .. } => "It shares a unit with your desktop, so it is moved into one of its own first. Limits last until it quits or the computer restarts.",
            _ => "Enforced by the kernel. They last until it quits or the computer restarts.",
        };
        card = card.push(widgets::meta_text(how, density::ROW_TEXT_BASE, s));
        let field = |label: &'static str, hint: &'static str, value: &'a str, on: fn(String) -> Message| -> Element<'a, Message> {
            row![
                container(widgets::meta_text(label, density::ROW_TEXT_BASE, s)).width(Length::Fixed(s.apply(110.0))),
                text_input(hint, value).on_input(on).on_submit(Message::ApplyLimits).padding(spacing::SM).style(widgets::inset_input_style).width(Length::Fixed(s.apply(160.0))),
            ]
            .spacing(spacing::MD)
            .align_y(iced::Alignment::Center)
            .into()
        };
        if d.offer.memory {
            card = card.push(field("Memory", "no limit, or e.g. 4 GB", &d.memory, Message::LimitsMemory));
            card = card.push(widgets::hint_text("Past nine tenths of it the app is slowed to stay inside; at the limit itself it is stopped.", s));
        }
        if d.offer.cpu {
            card = card.push(field("CPU %", "no limit", &d.cpu, Message::LimitsCpu));
            card = card.push(widgets::hint_text("A percent of the whole machine, as the CPU column reads it.", s));
        }
        if !d.offer.memory && !d.offer.cpu {
            card = card.push(widgets::hint_text("Where it runs, systemd can't enforce memory or CPU limits.", s));
        }
        if let Some(e) = &d.error {
            card = card.push(widgets::meta_text(e.clone(), density::ROW_TEXT_BASE, s).color(theme::error()));
        }
        let removable = matches!(d.plan, Plan::Unit(_)) && !d.current.is_none();
        let apply = if d.busy { widgets::primary_button("Applying…") } else { widgets::primary_button("Apply").on_press(Message::ApplyLimits) };
        card = card.push(
            row![
                widgets::secondary_button("Remove limits").on_press_maybe((removable && !d.busy).then_some(Message::RemoveLimits)),
                Space::new().width(Length::Fill),
                widgets::secondary_button("Cancel").on_press(Message::CloseLimits),
                apply,
            ]
            .spacing(spacing::SM),
        );
        frame(card, s)
    }
}

fn frame<'a>(card: iced::widget::Column<'a, Message>, s: FontScale) -> Element<'a, Message> {
    container(card)
        .padding(spacing::LG)
        .width(Length::Fixed(s.apply(480.0)))
        .style(|_t: &iced::Theme| container::Style {
            background: Some(Background::Color(surface::card())),
            border: Border { radius: density::card_radius().into(), width: 1.0, color: surface::card_border() },
            ..container::Style::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_procman::limits::{Limiter, Pending, Properties};
    use std::sync::{Arc, Mutex};

    /// systemd in memory, answering yes, keeping what it was asked.
    #[derive(Default)]
    struct Mock(Mutex<Vec<String>>);

    impl Limiter for Mock {
        fn set(&self, unit: String, p: Properties) -> Pending<()> {
            self.0.lock().unwrap().push(format!("set {unit} {p:?}"));
            Box::pin(async { Ok(()) })
        }
        fn new_scope(&self, name: String, _: String, pids: Vec<u32>, p: Properties) -> Pending<()> {
            self.0.lock().unwrap().push(format!("scope {name} {} {p:?}", pids.len()));
            Box::pin(async { Ok(()) })
        }
    }

    /// The card on this test's own process, against a systemd that only
    /// records: a bad field is refused before anything is sent, a good
    /// one sends exactly one call, and the answer closes the card.
    #[test]
    fn the_card_sends_one_call_for_good_fields_and_none_for_bad() {
        let mut app = crate::menus::tests::tests_window();
        let mock = Arc::new(Mock::default());
        app.limiter = mock.clone();
        let me = crate::menus::tests::tests_me();
        // In a test, "the window" is this test process, which the window
        // counts as session-critical — the card would always refuse it,
        // and the test would always skip.
        app.critical.clear();
        app.open_limits(me);
        let d = app.limits.as_mut().expect("opened");
        if matches!(d.plan, Plan::Refuse(_)) {
            eprintln!("HYPRFORGE-SKIP: this process can't be limited where it runs ({:?})", d.plan);
            return;
        }
        d.offer = Offer { memory: true, cpu: true };
        d.memory = "lots".into();
        let _ = app.apply_limits(false);
        assert!(app.limits.as_ref().unwrap().error.is_some());
        assert!(mock.0.lock().unwrap().is_empty(), "nothing sent for a field that doesn't parse");
        app.limits.as_mut().unwrap().memory = "1 GB".into();
        let _ = app.apply_limits(false);
        assert!(app.limits.as_ref().unwrap().busy);
        let _ = app.update(Message::LimitsDone(Ok(())));
        assert!(app.limits.is_none(), "the answer closes the card");
        assert!(app.notice.as_deref().is_some_and(|n| n.contains("are set")));
    }

    const BOTH: Offer = Offer { memory: true, cpu: true };

    /// The window's chain of parents is critical to End. For limits only
    /// the windowless part of it is: the compositor, not the terminal.
    #[test]
    fn a_windowed_app_above_this_window_may_be_limited_and_the_compositor_may_not() {
        let mut app = crate::menus::tests::tests_window();
        let terminal = 13060;
        let compositor = 1287;
        app.critical = [terminal, compositor].into_iter().collect();
        let lead = hyprforge_procman::model::Entry { id: Id { pid: terminal, start: 1 }, ..Default::default() };
        app.table.apps = vec![hyprforge_procman::model::App {
            lead: lead.clone(),
            class: "com.mitchellh.ghostty".into(),
            title: String::new(),
            windows: 3,
            members: vec![lead],
            flatpak: None,
        }];
        assert!(!app.limit_would_endanger(&[terminal]));
        assert!(app.limit_would_endanger(&[compositor]));
    }

    #[test]
    fn empty_fields_are_no_limit_and_bad_ones_say_what_to_write() {
        assert_eq!(parse_fields("", "", BOTH), Ok(Limits::default()));
        assert_eq!(parse_fields("4 GB", "25 %", BOTH), Ok(Limits { memory: Some(4 << 30), cpu_percent: Some(25) }));
        assert!(parse_fields("lots", "", BOTH).unwrap_err().contains("4 GB"));
        assert!(parse_fields("", "150", BOTH).unwrap_err().contains("1 to 100"));
    }

    /// A controller that can't reach the kernel isn't offered, so its
    /// field can't be filled in — and anything left in it is ignored.
    #[test]
    fn a_field_for_a_controller_not_offered_is_ignored() {
        assert_eq!(parse_fields("4 GB", "25", Offer { memory: true, cpu: false }), Ok(Limits { memory: Some(4 << 30), cpu_percent: None }));
    }

    #[test]
    fn current_limits_fill_the_fields_as_they_were_set() {
        assert_eq!(field_text(Limits { memory: Some(4 << 30), cpu_percent: Some(25) }), ("4.0 GB".to_string(), "25".to_string()));
        assert_eq!(field_text(Limits::default()), (String::new(), String::new()));
    }
}

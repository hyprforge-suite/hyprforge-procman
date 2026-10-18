//! What a right click offers: a process's menu and a service's.
//!
//! Every row sends a message the window's buttons already send, after
//! the right click has selected the row it was on — so a menu can do
//! nothing a button can't, and goes through the same confirmations and
//! the same refusals (a session-critical process can't be frozen from a
//! menu either). A row a button would show dimmed is dimmed here too,
//! never left out, so a menu's rows don't move from one process to the
//! next.

use crate::{App, Message, Tab};
use hyprforge_procman::sample::Id;
use hyprforge_procman::services::{Action, Unit};
use hyprforge_procman::signal::How;
use hyprforge_ui::widgets::MenuRow;

/// Which row a menu was opened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuFor {
    Process(Id),
    Service(String),
}

/// An open menu: what it is for and where the pointer was.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenMenu {
    pub on: MenuFor,
    pub at: (f32, f32),
}

/// A row that, chosen, closes the menu and sends `message`.
fn item(label: &str, message: Message) -> MenuRow<Message> {
    MenuRow::item(label, Message::MenuChose(Box::new(message)))
}

fn maybe(label: &str, message: Option<Message>) -> MenuRow<Message> {
    match message {
        Some(m) => item(label, m),
        None => MenuRow::disabled(label),
    }
}

impl App {
    /// The menu for the process `id`, as its buttons would offer it.
    pub fn process_menu(&self, id: Id) -> Vec<MenuRow<Message>> {
        let Some(e) = self.entry(id) else { return Vec::new() };
        // Switch to needs a window: a Flatpak grouped by its scope alone
        // has none.
        let is_app = self.table.apps.iter().any(|a| a.lead.id == id && a.windows > 0);
        let frozen = matches!(e.state, 'T' | 't');
        let critical = self.critical.contains(&id.pid);
        let mut rows = Vec::new();
        if is_app {
            rows.push(item("Switch to", Message::SwitchTo));
            rows.push(MenuRow::Separator);
        }
        rows.push(item("End task", Message::Ask(How::Ask)).hint("Delete"));
        rows.push(item("Kill", Message::Ask(How::Force)).hint("Ctrl+Delete"));
        rows.push(item("End process tree", Message::EndTree));
        rows.push(item("Set limits…", Message::OpenLimits(id)));
        rows.push(MenuRow::Separator);
        rows.push(if frozen {
            item("Resume", Message::Freeze(false))
        } else {
            maybe("Freeze", (!critical).then_some(Message::Freeze(true)))
        });
        rows.push(maybe("Lower priority", (e.nice < 19).then_some(Message::Nicer(true))));
        rows.push(maybe("Raise priority", (e.nice > -20).then_some(Message::Nicer(false))));
        rows.push(MenuRow::Separator);
        // The program's path is read now, not from the Details pane's
        // last reading: the right click may have just chosen this row.
        let exe = hyprforge_procman::inspect::exe(std::path::Path::new("/proc"), id.pid);
        rows.push(maybe("Open file location", exe.map(Message::OpenLocationOf)));
        if self.tab != Tab::Details {
            rows.push(item("Show in Details", Message::Tab(Tab::Details)));
        }
        rows.push(item("Copy process ID", Message::Copy(id.pid.to_string())));
        rows
    }

    /// The menu for a service, as its buttons would offer it.
    pub fn service_menu(&self, unit: &Unit) -> Vec<MenuRow<Message>> {
        let busy = self.services.busy;
        let mut rows: Vec<MenuRow<Message>> =
            Action::offered(unit).into_iter().map(|action| maybe(action.label(), (!busy).then_some(Message::ServiceAct(action)))).collect();
        if !rows.is_empty() {
            rows.push(MenuRow::Separator);
        }
        rows.push(item("Copy name", Message::Copy(unit.name.clone())));
        rows
    }

    /// The open menu's rows, or nothing when what it was opened on has
    /// gone since (a process that ended under the menu closes it).
    pub fn menu_rows(&self) -> Option<Vec<MenuRow<Message>>> {
        let menu = self.menu.as_ref()?;
        let rows = match &menu.on {
            MenuFor::Process(id) => self.process_menu(*id),
            MenuFor::Service(name) => {
                let unit = self.services.units.get(&self.services.scope()).and_then(|units| units.iter().find(|u| &u.name == name))?;
                self.service_menu(unit)
            }
        };
        (!rows.is_empty()).then_some(rows)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::HashMap;
    use hyprforge_procman::proc::ProcFs;

    /// The window over one real reading of this machine, with history
    /// kept in memory so a test never writes the user's file.
    fn window() -> App {
        let (mut app, _) = App::new();
        app.app_history = Ok(Default::default());
        app.history_saved = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        let snapshot = ProcFs::default().snapshot().unwrap();
        app.take(crate::Reading {
            snapshot,
            gpu: Default::default(),
            machine: Default::default(),
            windows: Ok(Vec::new()),
            at: std::time::Instant::now(),
            nvidia: None,
            everything_lapsed: false,
            held: None,
        });
        app
    }

    /// A reading of this machine with one window, focused, belonging to
    /// whatever started this test — another process, as a click on
    /// another window would be.
    fn reading_with_focus_on(pid: i32) -> crate::Reading {
        let window = hyprforge_procman::windows::Window { pid, class: "test.runner".into(), title: "picked".into(), focus: 0, ..Default::default() };
        crate::Reading {
            snapshot: ProcFs::default().snapshot().unwrap(),
            gpu: Default::default(),
            machine: Default::default(),
            windows: Ok(vec![window]),
            at: std::time::Instant::now(),
            nvidia: None,
            everything_lapsed: false,
            held: None,
        }
    }

    /// Picking: the first reading in which another window has the focus
    /// selects its app, stops picking, and asks for this window back —
    /// asks, because the dispatch belongs to the caller, never to a test.
    #[test]
    fn a_click_on_another_window_selects_its_app() {
        let mut app = window();
        app.picking = true;
        let parent = std::os::unix::process::parent_id() as i32;
        app.take(reading_with_focus_on(parent));
        assert!(!app.picking && app.come_back);
        assert_eq!(app.selected.map(|id| id.pid), Some(parent));
        assert_eq!(app.tab, crate::Tab::Processes);
    }

    /// While this window is the focused one nothing has been clicked yet.
    #[test]
    fn this_window_focused_is_not_a_pick() {
        let mut app = window();
        app.picking = true;
        app.take(reading_with_focus_on(std::process::id() as i32));
        assert!(app.picking && !app.come_back);
    }

    /// `--end-focused` selects the window that had the focus and asks —
    /// it never ends anything on a key press alone.
    #[test]
    fn end_focused_asks_about_the_focused_window() {
        let mut app = window();
        let parent = std::os::unix::process::parent_id() as i32;
        app.end_focused = Some(parent);
        app.take(reading_with_focus_on(parent));
        let asked = app.confirm.as_ref().expect("a question, not an ending");
        assert_eq!((asked.id.pid, asked.how), (parent, How::Ask));
        assert_eq!(app.end_focused, None, "asked once");
    }

    /// The window and this process's id, for other modules' tests.
    pub fn tests_window() -> App {
        window()
    }

    pub fn tests_me() -> Id {
        me()
    }

    fn me() -> Id {
        let pid = std::process::id() as i32;
        Id { pid, start: hyprforge_procman::proc::start_time(pid).unwrap() }
    }

    /// Each row's label, and whether it can be chosen.
    fn labels(rows: &[MenuRow<Message>]) -> Vec<(String, bool)> {
        rows.iter()
            .filter_map(|r| match r {
                MenuRow::Item { label, on, .. } => Some((label.clone(), on.is_some())),
                MenuRow::Separator => None,
            })
            .collect()
    }

    /// The window's own process is session-critical to it: its menu,
    /// like its Freeze button, will not freeze it — and still shows the
    /// row, dimmed, so the menu has the same shape for every process.
    #[test]
    fn a_process_menu_offers_what_its_buttons_do_and_refuses_what_they_refuse() {
        let app = window();
        let rows = labels(&app.process_menu(me()));
        let names: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            names,
            ["End task", "Kill", "End process tree", "Set limits…", "Freeze", "Lower priority", "Raise priority", "Open file location", "Show in Details", "Copy process ID"],
            "a test runner has no window, so no Switch to"
        );
        assert!(rows.contains(&("Freeze".into(), false)), "{rows:?}");
        assert!(rows.contains(&("Open file location".into(), true)), "this process's own program is readable");
    }

    #[test]
    fn a_right_click_selects_its_row_and_a_choice_closes_the_menu_and_acts() {
        let mut app = window();
        let _ = app.update(Message::OpenMenu(MenuFor::Process(me())));
        assert_eq!(app.selected, Some(me()), "what the menu does, it does to the row clicked");
        assert!(app.menu_rows().is_some());
        let _ = app.update(Message::MenuChose(Box::new(Message::Tab(Tab::Details))));
        assert!(app.menu.is_none());
        assert_eq!(app.tab, Tab::Details);
        let rows = labels(&app.process_menu(me()));
        assert!(!rows.iter().any(|(l, _)| l == "Show in Details"), "not offered on the tab it would go to");
    }

    /// The tests above hand `update` the message a right click would
    /// make. This one makes the right click: a press and release over
    /// the row, through the view the window draws, and whatever messages
    /// come out of it fed back in.
    #[test]
    fn a_real_right_click_on_a_process_row_opens_its_menu() {
        for tab in [Tab::Processes, Tab::Details] {
            right_click_opens_a_menu_on(tab);
        }
    }

    fn right_click_opens_a_menu_on(tab: Tab) {
        let mut app = window();
        app.tab = tab;
        let size = iced::Size::new(1280.0, 800.0);
        app.window_size = size;
        let mut pids: Vec<i32> = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        pids.sort();
        let messages = {
            let mut sim = iced_test::Simulator::with_size(iced::Settings::default(), size, app.view());
            // Whichever row is on screen: the list scrolls, and a test
            // runner's own row is rarely in the first screenful.
            let at = pids
                .iter()
                .find_map(|pid| sim.find(pid.to_string()).ok()?.visible_bounds())
                .expect("some process row is on screen")
                .center();
            sim.point_at(at);
            use iced::mouse::{Button, Event as M};
            let _ = sim.simulate([
                iced::Event::Mouse(M::CursorMoved { position: at }),
                iced::Event::Mouse(M::ButtonPressed(Button::Right)),
                iced::Event::Mouse(M::ButtonReleased(Button::Right)),
            ]);
            sim.into_messages().collect::<Vec<_>>()
        };
        assert!(
            messages.iter().any(|m| matches!(m, Message::OpenMenu(_))),
            "{tab:?}: a right click on the row produced no OpenMenu: {} message(s)",
            messages.len()
        );
        for m in messages {
            let _ = app.update(m);
        }
        assert!(app.menu_rows().is_some(), "{tab:?}: the menu opened and has rows");
    }

    /// On battery: the first quiet reading is the floor and blames no
    /// one; a busier one shares what is above it. Plugged in: no watts,
    /// and the floor goes, since it meant nothing off the battery.
    #[test]
    fn watts_are_shared_only_on_battery_and_only_above_its_quietest() {
        use hyprforge_procman::machine::Battery;
        let mut app = window();
        let usage = hyprforge_procman::sample::Use { cpu: HashMap::from([(me(), 12.5)]), ..Default::default() };
        let battery = |status: &str, w: f32| Some(Battery { status: status.into(), percent: 50, watts: Some(w) });
        app.machine.battery = battery("Discharging", 8.0);
        app.take_power(&usage, 8, 1.0);
        assert!(app.watts.is_empty(), "the first reading is its own floor");
        app.machine.battery = battery("Discharging", 12.0);
        app.take_power(&usage, 8, 1.0);
        assert!((app.watts[&me()] - 4.0).abs() < 1e-3, "{:?}", app.watts);
        app.machine.battery = battery("Charging", 20.0);
        app.take_power(&usage, 8, 1.0);
        assert!(app.watts.is_empty() && app.power_floor.watts().is_none());
        assert!(app.activity[&me()] > 99.0, "the share is still shown plugged in");
    }

    /// Switching is a wait on systemd: the toggle holds still until it
    /// answers, a refusal is said in words, and the state shown is asked
    /// of systemd again rather than assumed from what was asked for.
    #[test]
    fn efficiency_mode_shows_what_systemd_says_not_what_was_asked() {
        let mut app = window();
        app.efficiency = Some(false);
        let _ = app.update(Message::Efficiency(true));
        assert!(app.efficiency_busy);
        assert_eq!(app.efficiency, Some(false), "not on until systemd says so");
        let _ = app.update(Message::EfficiencySwitched(Err("no such unit".into())));
        assert!(!app.efficiency_busy);
        assert!(app.notice.as_deref().is_some_and(|n| n.contains("no such unit")));
        let _ = app.update(Message::EfficiencyState(Some(true)));
        assert_eq!(app.efficiency, Some(true));
    }

    /// Wakeups cost a file per thread, so only the page that shows them
    /// pays for them.
    #[test]
    fn wakeups_are_read_only_while_the_battery_page_is_showing() {
        let mut app = window();
        app.tab = Tab::Processes;
        assert!(!app.reader().thread_schedstat);
        app.tab = Tab::Performance;
        app.resource = crate::Resource::Cpu;
        assert!(!app.reader().thread_schedstat);
        app.resource = crate::Resource::Battery;
        assert!(app.reader().thread_schedstat);
    }

    #[test]
    fn escape_closes_a_menu_before_anything_else() {
        let mut app = window();
        let _ = app.update(Message::OpenMenu(MenuFor::Process(me())));
        let _ = app.update(Message::Key(hyprforge_keys::KeyPress { key: hyprforge_keys::Key::Escape, mods: Default::default(), text: None }));
        assert!(app.menu.is_none());
        assert_eq!(app.selected, Some(me()), "only the menu goes");
    }

    #[test]
    fn a_menu_on_a_process_that_ended_is_gone_with_it() {
        let mut app = window();
        app.menu = Some(OpenMenu { on: MenuFor::Process(Id { pid: i32::MAX, start: 1 }), at: (0.0, 0.0) });
        assert!(app.menu_rows().is_none());
    }

    #[test]
    fn a_service_menu_offers_its_actions_and_dims_them_while_one_runs() {
        let mut app = window();
        let unit = Unit {
            name: "riddle-house.service".into(),
            description: String::new(),
            load: "loaded".into(),
            active: "active".into(),
            sub: "running".into(),
            file: Some("enabled".into()),
        };
        assert_eq!(labels(&app.service_menu(&unit)), [("Restart".into(), true), ("Stop".into(), true), ("Don't start at boot".into(), true), ("Copy name".into(), true)]);
        app.services.busy = true;
        assert_eq!(labels(&app.service_menu(&unit)), [("Restart".into(), false), ("Stop".into(), false), ("Don't start at boot".into(), false), ("Copy name".into(), true)]);
    }
}

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
        let is_app = self.table.apps.iter().any(|a| a.lead.id == id);
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
mod tests {
    use super::*;
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
            ["End task", "Kill", "End process tree", "Freeze", "Lower priority", "Raise priority", "Open file location", "Show in Details", "Copy process ID"],
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

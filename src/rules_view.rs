//! The Rules tab: what to do to a program every time it starts.
//!
//! The decisions and the file are `hyprforge_procman::rules`; this is the
//! editor and the switch for the service that applies them.

use crate::{App, Message};
use hyprforge_procman::rules::{self, File, Rule};
use hyprforge_ui::theme::{self, spacing, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::{column, container, row, scrollable, text_input, Space};
use iced::{Alignment, Element, Length};

pub const RULES_UNIT: &str = "hyprforge-procman-rules.service";

/// One rule as its row's fields hold it, while it is being edited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub program: String,
    pub priority: String,
    pub memory: String,
    pub cpu: String,
    pub warn: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Program,
    Priority,
    Memory,
    Cpu,
    Warn,
}

#[derive(Debug, Clone, Default)]
pub struct Tab {
    pub rows: Vec<Row>,
    /// The file couldn't be used; editing waits until the person chooses
    /// to start over, so a file they wrote by hand is never overwritten
    /// on the strength of a parse error.
    pub broken: Option<String>,
    pub error: Option<String>,
    pub saved: bool,
    /// The service: running, stopped, or `None` for not installed.
    pub service: Option<bool>,
    pub busy: bool,
}

impl Row {
    fn of(r: &Rule) -> Row {
        Row {
            program: r.program.clone(),
            priority: r.priority.map(|p| p.to_string()).unwrap_or_default(),
            memory: r.memory.clone().unwrap_or_default(),
            cpu: r.cpu.map(|c| c.to_string()).unwrap_or_default(),
            warn: r.warn_memory.clone().unwrap_or_default(),
        }
    }

    /// The rule this row says, or what about it isn't a number.
    pub fn rule(&self) -> Result<Rule, String> {
        let opt = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        let number = |what: &str, s: &str| -> Result<Option<i64>, String> {
            match opt(s) {
                None => Ok(None),
                Some(t) => t.trim_end_matches('%').trim().parse().map(Some).map_err(|_| format!("{}: {what} \"{t}\" isn't a number", self.program.trim())),
            }
        };
        Ok(Rule {
            program: self.program.trim().to_string(),
            priority: number("priority", &self.priority)?.map(|n| n as i32),
            memory: opt(&self.memory),
            cpu: number("cpu", &self.cpu)?.map(|n| n.max(0) as u32),
            warn_memory: opt(&self.warn),
        })
    }
}

/// The tab's rows from the file, or why the file can't be edited here.
pub fn load(path: &std::path::Path) -> Tab {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Tab::default(),
        Err(e) => Tab { broken: Some(format!("procman-rules.toml couldn't be read: {e}")), ..Tab::default() },
        Ok(text) => match rules::parse(&text) {
            Ok(file) => Tab { rows: file.rules.iter().map(Row::of).collect(), ..Tab::default() },
            Err(e) => Tab { broken: Some(e), ..Tab::default() },
        },
    }
}

/// Saves the rows, all of them or none: a row that doesn't check stops
/// the save and says which.
pub fn save(path: &std::path::Path, rows: &[Row]) -> Result<(), String> {
    let rules: Vec<Rule> = rows.iter().filter(|r| *r != &Row::default()).map(Row::rule).collect::<Result<_, _>>()?;
    rules::save(path, &File { rules })
}

impl App {
    pub fn rules_view(&self, s: FontScale) -> Element<'_, Message> {
        let tab = &self.rules;
        let mut page = column![].spacing(spacing::MD).padding(spacing::LG);
        let switch: Element<'_, Message> = match tab.service {
            Some(on) => {
                let t = widgets::toggle(on, s).label("Apply rules");
                if tab.busy { t.into() } else { t.on_toggle(Message::RulesSwitch).into() }
            }
            None => widgets::hint_text("The rules service isn't installed, so rules here are saved and not applied.", s).into(),
        };
        page = page.push(row![widgets::section_label("Rules", s), Space::new().width(Length::Fill), switch].align_y(Alignment::Center));
        page = page.push(widgets::meta_text(
            "Each applies to a program, by the name the Processes tab shows, every time it starts — never to one already running. \
             Priority goes from 0 (as usual) to 19 (last in line). A cap moves the program into a unit of its own, and is refused \
             if it would take another app with it.",
            density::META_TEXT_BASE,
            s,
        ));
        if let Some(why) = &tab.broken {
            page = page.push(widgets::meta_text(format!("{why}. Nothing in it is being applied."), density::ROW_TEXT_BASE, s).color(theme::error()));
            page = page.push(row![
                widgets::hint_text("Fix it by hand, or start a new list here — which replaces the file.", s),
                Space::new().width(Length::Fill),
                widgets::secondary_button("Start a new list").on_press(Message::RulesStartOver),
            ]);
            return scrollable(page).height(Length::Fill).into();
        }
        let head = |label: &'static str, w: f32| container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(w)));
        page = page.push(
            row![
                container(widgets::meta_text("Program", density::META_TEXT_BASE, s)).width(Length::Fill),
                head("Priority", 90.0),
                head("Memory cap", 120.0),
                head("CPU cap %", 100.0),
                head("Warn past", 120.0),
                Space::new().width(Length::Fixed(s.apply(90.0))),
            ]
            .spacing(spacing::SM),
        );
        if tab.rows.is_empty() {
            page = page.push(widgets::hint_text("No rules yet.", s));
        }
        for (i, r) in tab.rows.iter().enumerate() {
            let field = |value: &str, hint: &'static str, f: Field, w: Option<f32>| -> Element<'_, Message> {
                let input = text_input(hint, value).on_input(move |v| Message::RulesEdit(i, f, v)).padding(spacing::SM).style(widgets::inset_input_style);
                match w {
                    Some(w) => input.width(Length::Fixed(s.apply(w))).into(),
                    None => input.width(Length::Fill).into(),
                }
            };
            page = page.push(
                row![
                    field(&r.program, "cargo", Field::Program, None),
                    field(&r.priority, "—", Field::Priority, Some(90.0)),
                    field(&r.memory, "—", Field::Memory, Some(120.0)),
                    field(&r.cpu, "—", Field::Cpu, Some(100.0)),
                    field(&r.warn, "—", Field::Warn, Some(120.0)),
                    container(widgets::secondary_button("Remove").on_press(Message::RulesRemove(i))).width(Length::Fixed(s.apply(90.0))),
                ]
                .spacing(spacing::SM)
                .align_y(Alignment::Center),
            );
        }
        if let Some(e) = &tab.error {
            page = page.push(widgets::meta_text(e.clone(), density::ROW_TEXT_BASE, s).color(theme::error()));
        } else if tab.saved {
            page = page.push(widgets::hint_text("Saved. The service reads it within a couple of seconds.", s));
        }
        page = page.push(row![widgets::secondary_button("Add a rule").on_press(Message::RulesAdd), Space::new().width(Length::Fill), widgets::primary_button("Save").on_press(Message::RulesSave)].spacing(spacing::SM));
        scrollable(page).height(Length::Fill).into()
    }

    pub fn rules_update(&mut self, message: Message) -> iced::Task<Message> {
        let tab = &mut self.rules;
        match message {
            Message::RulesEdit(i, f, v) => {
                if let Some(r) = tab.rows.get_mut(i) {
                    *match f {
                        Field::Program => &mut r.program,
                        Field::Priority => &mut r.priority,
                        Field::Memory => &mut r.memory,
                        Field::Cpu => &mut r.cpu,
                        Field::Warn => &mut r.warn,
                    } = v;
                    tab.saved = false;
                }
            }
            Message::RulesAdd => tab.rows.push(Row::default()),
            Message::RulesRemove(i) => {
                if i < tab.rows.len() {
                    tab.rows.remove(i);
                    tab.saved = false;
                }
            }
            Message::RulesSave => match save(&self.rules_path, &tab.rows) {
                Ok(()) => {
                    tab.error = None;
                    tab.saved = true;
                }
                Err(e) => {
                    tab.error = Some(e);
                    tab.saved = false;
                }
            },
            Message::RulesStartOver => {
                tab.broken = None;
                tab.rows.clear();
            }
            Message::RulesSwitch(on) => {
                tab.busy = true;
                return crate::switch_unit(RULES_UNIT, on, Message::RulesSwitched);
            }
            Message::RulesSwitched(result) => {
                tab.busy = false;
                if let Err(e) = result {
                    tab.error = Some(format!("The rules service couldn't be switched: {e}"));
                }
                return crate::unit_state(RULES_UNIT, Message::RulesState);
            }
            Message::RulesState(state) => tab.service = state,
            _ => {}
        }
        iced::Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(program: &str, priority: &str, memory: &str) -> Row {
        Row { program: program.into(), priority: priority.into(), memory: memory.into(), ..Row::default() }
    }

    #[test]
    fn rows_save_as_rules_and_read_back_as_the_same_rows() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("procman-rules.toml");
        let rows = vec![row("cargo", "10", "8 GB"), Row::default()];
        save(&p, &rows).unwrap();
        assert_eq!(load(&p).rows, vec![row("cargo", "10", "8 GB")], "an untouched blank row is not a rule");
    }

    #[test]
    fn a_row_that_doesnt_check_stops_the_save_and_says_which() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("procman-rules.toml");
        assert!(save(&p, &[row("cargo", "ten", "")]).unwrap_err().contains("isn't a number"));
        assert!(save(&p, &[row("game", "-5", "")]).unwrap_err().contains("administrator"));
        assert!(!p.exists(), "nothing written");
    }

    /// A hand-written file that doesn't parse is shown as broken and kept
    /// until the person chooses to start over.
    #[test]
    fn a_broken_file_is_shown_broken_and_left_alone() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("procman-rules.toml");
        std::fs::write(&p, "[[rule]\n").unwrap();
        let tab = load(&p);
        assert!(tab.broken.is_some() && tab.rows.is_empty());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[[rule]\n");
    }
}

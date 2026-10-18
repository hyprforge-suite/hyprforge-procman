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
    /// The row open for editing; the rest are read as text.
    pub editing: Option<usize>,
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
    /// A table, because a rule is a row of settings: unset is blank, the
    /// row being edited opens in place, and the service being off is one
    /// line beside its switch — the thing to notice before the rules.
    pub fn rules_view(&self, s: FontScale) -> Element<'_, Message> {
        let tab = &self.rules;
        let (state, warn) = match tab.service {
            Some(true) => ("On", false),
            Some(false) => ("Off — nothing is applying these", true),
            None => ("The rules service isn't installed — these are saved, not applied", true),
        };
        let mut bar = row![
            widgets::scaled_text("Rules", density::ROW_TEXT_BASE, s),
            widgets::meta_text("applied to each program as it starts", density::META_TEXT_BASE, s),
            Space::new().width(Length::Fill),
            widgets::scaled_text(state, density::META_TEXT_BASE, s).color(if warn { theme::warning() } else { theme::text_dim() }),
        ]
        .spacing(spacing::MD)
        .align_y(Alignment::Center);
        if let Some(on) = tab.service {
            let t = widgets::toggle(on, s);
            bar = bar.push(if tab.busy { Element::from(t) } else { t.on_toggle(Message::RulesSwitch).into() });
        }
        let mut page = column![bar].spacing(spacing::MD).padding(spacing::LG);
        if let Some(why) = &tab.broken {
            page = page.push(widgets::meta_text(format!("{why}. Nothing in it is being applied."), density::ROW_TEXT_BASE, s).color(theme::error()));
            page = page.push(row![
                widgets::hint_text("Fix it by hand, or start a new list here — which replaces the file.", s),
                Space::new().width(Length::Fill),
                widgets::secondary_button("Start a new list").on_press(Message::RulesStartOver),
            ]);
            return scrollable(page).height(Length::Fill).into();
        }
        let cell = |w: f32, content: Element<'static, Message>| container(content).width(Length::Fixed(s.apply(w))).align_x(Alignment::End);
        let widths = [90.0, 110.0, 90.0, 100.0, 90.0];
        let head = |label: &'static str, w: f32| cell(w, widgets::meta_text(label, density::META_TEXT_BASE, s).into());
        let mut table = column![row![
            widgets::meta_text("Program", density::META_TEXT_BASE, s).width(Length::Fill),
            head("Priority", widths[0]),
            head("Memory cap", widths[1]),
            head("CPU cap", widths[2]),
            head("Warn past", widths[3]),
            head("Running", widths[4]),
        ]
        .spacing(spacing::MD)
        .padding([spacing::XS, spacing::SM])]
        .spacing(0.0);
        for (i, r) in tab.rows.iter().enumerate() {
            let running = hyprforge_procman::model::entries(&self.table).filter(|e| !r.program.trim().is_empty() && e.name == r.program.trim()).count();
            let mono = |v: String, color: iced::Color| -> Element<'static, Message> { widgets::scaled_text(v, density::ROW_TEXT_BASE, s).font(theme::mono_font()).color(color).into() };
            let cpu = if r.cpu.trim().is_empty() { String::new() } else { format!("{}%", r.cpu.trim().trim_end_matches('%')) };
            let line = row![
                container(mono(r.program.clone(), theme::text())).width(Length::Fill),
                cell(widths[0], mono(r.priority.clone(), theme::text())),
                cell(widths[1], mono(r.memory.clone(), theme::text())),
                cell(widths[2], mono(cpu, theme::text())),
                cell(widths[3], mono(r.warn.clone(), theme::text())),
                cell(widths[4], mono(if running == 0 { String::new() } else { running.to_string() }, theme::text_dim())),
            ]
            .spacing(spacing::MD)
            .align_y(Alignment::Center);
            let open = tab.editing == Some(i);
            table = table.push(widgets::divider()).push(
                iced::widget::button(line)
                    .padding([spacing::SM, spacing::SM])
                    .width(Length::Fill)
                    .on_press(Message::RulesOpen(if open { None } else { Some(i) }))
                    .style(move |t, status| widgets::selectable_row_style(t, status, false)),
            );
            if open {
                let field = |value: &str, hint: &'static str, f: Field, w: Option<f32>| -> Element<'_, Message> {
                    let input = text_input(hint, value).on_input(move |v| Message::RulesEdit(i, f, v)).padding(spacing::SM).style(widgets::inset_input_style);
                    match w {
                        Some(w) => input.width(Length::Fixed(s.apply(w))).align_x(Alignment::End).into(),
                        None => input.width(Length::Fill).into(),
                    }
                };
                let buttons = row![widgets::secondary_button("Remove").on_press(Message::RulesRemove(i)), widgets::primary_button("Done").on_press(Message::RulesSave)].spacing(spacing::XS);
                let fields = row![
                    field(&r.program, "program, as Processes names it", Field::Program, None),
                    field(&r.priority, "0 – 19", Field::Priority, Some(widths[0])),
                    field(&r.memory, "GB", Field::Memory, Some(widths[1])),
                    field(&r.cpu, "%", Field::Cpu, Some(widths[2])),
                    field(&r.warn, "GB", Field::Warn, Some(widths[3])),
                ]
                .spacing(spacing::MD)
                .align_y(Alignment::Center);
                let wide = self.fit() == hyprforge_procman::fit::Fit::Full;
                // Beside the fields with room for them; narrow, on a line
                // of their own, rather than squeezed against the edge.
                let top: Element<'_, Message> = if wide {
                    row![fields, container(buttons).width(Length::Fixed(s.apply(widths[4] + 60.0))).align_x(Alignment::End)].spacing(spacing::MD).align_y(Alignment::Center).into()
                } else {
                    column![fields, container(buttons).width(Length::Fill).align_x(Alignment::End)].spacing(spacing::SM).into()
                };
                let edit = column![
                    top,
                    widgets::meta_text(
                        "Priority 0 is as usual, 19 is last in line. A cap gives the program a unit of its own, unless that would take another app with it.",
                        density::META_TEXT_BASE,
                        s
                    ),
                ]
                .spacing(spacing::SM);
                table = table.push(container(edit).padding(spacing::SM).width(Length::Fill).style(|_t: &iced::Theme| container::Style {
                    background: Some(iced::Background::Color(hyprforge_ui::theme::surface::card())),
                    ..container::Style::default()
                }));
            }
        }
        table = table.push(widgets::divider()).push(container(widgets::secondary_button("Add a rule").on_press(Message::RulesAdd)).padding(spacing::SM));
        page = page.push(table);
        if let Some(e) = &tab.error {
            page = page.push(widgets::meta_text(e.clone(), density::ROW_TEXT_BASE, s).color(theme::error()));
        } else if tab.saved {
            page = page.push(widgets::meta_text("Saved.", density::META_TEXT_BASE, s));
        }
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
            Message::RulesAdd => {
                tab.rows.push(Row::default());
                tab.editing = Some(tab.rows.len() - 1);
            }
            Message::RulesOpen(i) => tab.editing = i,
            // Removing is saved at once, as Done is: there is no other
            // Save to forget.
            Message::RulesRemove(i) => {
                if i < tab.rows.len() {
                    tab.rows.remove(i);
                    tab.editing = None;
                    return self.rules_update(Message::RulesSave);
                }
            }
            Message::RulesSave => match save(&self.rules_path, &tab.rows) {
                Ok(()) => {
                    tab.error = None;
                    tab.saved = true;
                    tab.editing = None;
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

//! The Crashes tab: what crashed, and what was killed for memory.
//!
//! Task Manager has nothing here; Windows keeps it in Reliability
//! Monitor, three menus away. On Linux the journal has had all of it
//! all along, and nothing on the desktop showed it.

use crate::view::{bytes, ink};
use crate::{App, Message};
use hyprforge_procman::incidents::{Crash, Incident, Incidents, Killer};
use hyprforge_ui::theme::{self, spacing, FontScale};
use hyprforge_ui::{density, widgets};
use iced::widget::{button, column, container, row, scrollable, Space};
use iced::{Alignment, Element, Length};
use std::collections::HashMap;

/// Everything the tab shows, read off the UI thread.
#[derive(Debug, Clone)]
pub struct CrashesData {
    pub incidents: Result<Incidents, String>,
    /// User names for every uid the incidents name, looked up as they
    /// were read: the view has no way to ask.
    pub names: HashMap<u32, String>,
}

/// The journal's last 30 days. The range control only filters it.
pub async fn load() -> CrashesData {
    tokio::task::spawn_blocking(|| {
        let incidents = hyprforge_procman::incidents::read(30);
        let mut users = hyprforge_procman::users::Users::default();
        let mut names = HashMap::new();
        if let Ok(found) = &incidents {
            for i in &found.list {
                let uid = match i {
                    Incident::Crash(c) => c.uid,
                    Incident::Kill(k) => k.uid,
                };
                if let Some(uid) = uid {
                    names.entry(uid).or_insert_with(|| users.name(uid));
                }
            }
        }
        CrashesData { incidents, names }
    })
    .await
    .unwrap_or_else(|e| CrashesData { incidents: Err(format!("reading the journal failed: {e}")), names: HashMap::new() })
}

/// A signal in words, for the ones a crash usually is. The name stays
/// beside it: it is what a bug report wants.
pub fn signal_words(signal: &str) -> &'static str {
    match signal {
        "SIGSEGV" => "touched memory it didn't own",
        "SIGABRT" => "stopped itself on purpose",
        "SIGBUS" => "read memory that wasn't there",
        "SIGILL" => "ran an instruction the processor refused",
        "SIGFPE" => "divided by zero or overflowed",
        "SIGTRAP" => "hit a breakpoint or a trap",
        "SIGSYS" => "made a system call it isn't allowed",
        _ => "was ended by a signal",
    }
}

fn killer_words(killer: Killer) -> &'static str {
    match killer {
        Killer::KernelMachine => "the kernel — the whole machine ran out of memory",
        Killer::KernelLimit => "the kernel — it reached its own memory limit, with the rest of the machine possibly fine",
        Killer::Kernel => "the kernel's OOM killer",
        Killer::Oomd => "systemd-oomd, easing memory pressure before the kernel had to",
    }
}

/// The local time of a journal timestamp (microseconds since the epoch).
fn when(at: i64) -> String {
    chrono::DateTime::from_timestamp_micros(at)
        .map(|t| t.with_timezone(&chrono::Local).format("%a %d %b, %H:%M:%S").to_string())
        .unwrap_or_else(|| "—".into())
}

/// What to call it. A process's own name (`comm`) is cut to 15
/// characters by the kernel — `EpicGamesLaunch` — so a name exactly
/// that long, which the program's file name begins with, was cut, and
/// the file name is the whole of it. A shorter one was not cut:
/// `python3` running `python3.14` keeps the name it was given.
fn name_of(i: &Incident) -> String {
    match i {
        Incident::Crash(c) => {
            let file = c.exe.as_deref().and_then(|e| e.rsplit('/').next()).filter(|f| c.name.len() == 15 && f.len() > 15 && f.starts_with(&c.name));
            file.map(str::to_owned).unwrap_or_else(|| c.name.clone())
        }
        Incident::Kill(k) => k.name.clone().or_else(|| k.unit.clone()).unwrap_or_else(|| "Unknown".into()),
    }
}

/// One line for the row: what happened, shortly.
fn what(i: &Incident) -> String {
    match i {
        Incident::Crash(c) => match &c.signal {
            Some(sig) => format!("Crashed ({sig})"),
            None => "Crashed".into(),
        },
        Incident::Kill(k) => {
            let size = k.held.map(|h| format!(", using {}", bytes(h.resident_bytes()))).unwrap_or_default();
            match k.killer {
                Killer::KernelLimit => format!("Killed at its memory limit{size}"),
                Killer::Oomd => "Killed by systemd-oomd".into(),
                _ => format!("Killed: out of memory{size}"),
            }
        }
    }
}

/// The stack as one text to paste into a bug report, with what a report
/// needs above it.
pub fn report(c: &Crash) -> String {
    let mut out = format!("{} ({}) — {}\n", c.name, c.exe.as_deref().unwrap_or("?"), c.signal.as_deref().unwrap_or("?"));
    if let Some(t) = &c.thread {
        out.push_str(&format!("thread: {t}\n"));
    }
    out.push('\n');
    for frame in c.stack.iter().flatten() {
        out.push_str(frame);
        out.push('\n');
    }
    out
}

impl App {
    pub fn crashes_view(&self, s: FontScale) -> Element<'_, Message> {
        let Some(data) = &self.crashes else {
            return container(widgets::hint_text("Reading the journal…", s)).padding(spacing::LG).into();
        };
        let found = match &data.incidents {
            Ok(found) => found,
            Err(e) => return container(widgets::hint_text(format!("The journal couldn't be read: {e}"), s)).padding(spacing::LG).into(),
        };
        let range = widgets::segmented_choice(
            &[1usize, 7, 30],
            Some(&self.crash_days),
            |d| match d {
                1 => "Today".to_string(),
                n => format!("{n} days"),
            },
            Message::CrashRange,
            s,
        );
        let from = chrono::Local::now().timestamp_micros() - self.crash_days as i64 * 86_400_000_000;
        let shown: Vec<&Incident> = found.list.iter().filter(|i| i.at() >= from).collect();

        let mut page = column![range].spacing(spacing::MD).padding(spacing::LG);
        page = page.push(widgets::meta_text(summary(&shown), density::META_TEXT_BASE, s));
        if !found.everyone {
            page = page.push(widgets::hint_text(
                "Only your own: the system journal isn't readable by you, so other users' programs and system services aren't listed.",
                s,
            ));
        }
        if shown.is_empty() {
            page = page.push(widgets::hint_text("Nothing crashed and nothing was killed for memory in this range.", s));
        }
        for i in shown {
            let selected = self.crash_selected == Some(i.at());
            let tint = match i {
                Incident::Crash(_) => widgets::Tint::Error,
                Incident::Kill(_) => widgets::Tint::Warning,
            };
            let line = row![
                widgets::status_dot(tint, s),
                widgets::scaled_text(name_of(i), density::ROW_TEXT_BASE, s).width(Length::FillPortion(2)),
                widgets::meta_text(what(i), density::META_TEXT_BASE, s).color(ink(selected)).width(Length::FillPortion(3)),
                container(widgets::meta_text(when(i.at()), density::META_TEXT_BASE, s).color(ink(selected))).width(Length::Fixed(s.apply(190.0))).align_x(Alignment::End),
            ]
            .spacing(spacing::MD)
            .align_y(Alignment::Center);
            let at = i.at();
            page = page.push(
                button(line)
                    .padding([spacing::SM, spacing::MD])
                    .width(Length::Fill)
                    .on_press(Message::CrashSelect(if selected { None } else { Some(at) }))
                    .style(move |t, status| widgets::selectable_row_style(t, status, selected)),
            );
            if selected {
                page = page.push(container(self.incident_detail(i, &data.names, s)).padding([0.0, spacing::LG]));
            }
        }
        scrollable(page).height(Length::Fill).into()
    }

    fn incident_detail<'a>(&self, i: &'a Incident, names: &HashMap<u32, String>, s: FontScale) -> Element<'a, Message> {
        let fact = |label: &'static str, value: String| -> Element<'a, Message> {
            row![
                container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(120.0))),
                widgets::scaled_text(value, density::ROW_TEXT_BASE, s).width(Length::Fill),
            ]
            .spacing(spacing::MD)
            .into()
        };
        let user = |uid: Option<u32>| uid.map(|u| names.get(&u).cloned().unwrap_or_else(|| u.to_string()));
        let mut facts = column![].spacing(spacing::XS);
        match i {
            Incident::Crash(c) => {
                if let Some(sig) = &c.signal {
                    facts = facts.push(fact("What happened", format!("It {} ({sig}).", signal_words(sig))));
                }
                facts = facts.push(fact("Program", c.exe.clone().unwrap_or_else(|| c.name.clone())));
                if let Some(t) = &c.thread {
                    facts = facts.push(fact("Thread", t.clone()));
                }
                if let Some(pid) = c.pid {
                    facts = facts.push(fact("Process ID", pid.to_string()));
                }
                if let Some(u) = user(c.uid) {
                    facts = facts.push(fact("User", u));
                }
                if let Some(unit) = &c.unit {
                    facts = facts.push(fact("Ran in", unit.clone()));
                }
                facts = facts.push(fact(
                    "Core dump",
                    match &c.core {
                        _ if !c.dumped => "Not kept".into(),
                        Some(p) if p.exists() => format!("Kept — coredumpctl debug {}", c.pid.map(|p| p.to_string()).unwrap_or_default()),
                        Some(_) => "Kept at the time, since cleaned up".into(),
                        None => "Not kept".into(),
                    },
                ));
                let mut stack = column![].spacing(2.0);
                match &c.stack {
                    None => stack = stack.push(widgets::hint_text("The journal kept no message for this crash, so there is no stack trace.", s)),
                    Some(frames) if frames.is_empty() => stack = stack.push(widgets::hint_text("systemd-coredump recorded no stack trace.", s)),
                    Some(frames) => {
                        for f in frames {
                            stack = stack.push(widgets::scaled_text(f.clone(), density::META_TEXT_BASE, s).font(theme::mono_font()));
                        }
                    }
                }
                let copy = widgets::secondary_button("Copy stack trace").on_press_maybe(c.stack.as_ref().filter(|f| !f.is_empty()).map(|_| Message::Copy(report(c))));
                column![facts, row![widgets::section_label("Stack trace", s), Space::new().width(Length::Fill), copy].align_y(Alignment::Center), stack]
                    .spacing(spacing::SM)
                    .padding([spacing::SM, 0.0])
                    .into()
            }
            Incident::Kill(k) => {
                facts = facts.push(fact("Killed by", killer_words(k.killer).to_string()));
                if let Some(h) = k.held {
                    facts = facts.push(fact(
                        "Was using",
                        format!("{} resident ({} of its own, {} of files, {} shared)", bytes(h.resident_bytes()), bytes(h.anon * 1024), bytes(h.file * 1024), bytes(h.shmem * 1024)),
                    ));
                }
                if let Some(pid) = k.pid {
                    facts = facts.push(fact("Process ID", pid.to_string()));
                }
                if let Some(u) = user(k.uid) {
                    facts = facts.push(fact("User", u));
                }
                if let Some(unit) = &k.unit {
                    facts = facts.push(fact("Ran in", unit.clone()));
                }
                if k.pid.is_none() {
                    facts = facts.push(widgets::hint_text("systemd recorded a kill in this unit; the kernel's own line naming the process wasn't kept.", s));
                }
                column![facts].padding([spacing::SM, 0.0]).into()
            }
        }
    }
}

/// One sentence over the list: how many of each, and which program
/// crashed most — the thing worth knowing first.
pub fn summary(shown: &[&Incident]) -> String {
    let crashes = shown.iter().filter(|i| matches!(i, Incident::Crash(_))).count();
    let kills = shown.len() - crashes;
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let mut out = format!("{} and {}.", plural(crashes, "crash", "crashes"), plural(kills, "program killed for memory", "programs killed for memory"));
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    for i in shown {
        if let Incident::Crash(c) = i {
            *by_name.entry(c.name.as_str()).or_default() += 1;
        }
    }
    if let Some((name, n)) = by_name.into_iter().filter(|(_, n)| *n > 1).max_by_key(|(name, n)| (*n, std::cmp::Reverse(*name))) {
        out.push_str(&format!(" {name} crashed most, {n} times."));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_procman::incidents::Kill;

    fn crash(name: &str, at: i64) -> Incident {
        Incident::Crash(Crash {
            at,
            boot: String::new(),
            pid: Some(1),
            name: name.into(),
            exe: None,
            signal: Some("SIGSEGV".into()),
            uid: None,
            unit: None,
            thread: None,
            core: None,
            stack: Some(vec!["#0 0x1 f (lib.so)".into()]),
            dumped: true,
        })
    }

    fn kill(at: i64) -> Incident {
        Incident::Kill(Kill { at, boot: String::new(), killer: Killer::KernelMachine, pid: None, name: None, uid: None, unit: Some("app.slice".into()), held: None })
    }

    /// The first thing worth knowing is what keeps crashing, so the
    /// summary names it — and names nothing when nothing repeats.
    #[test]
    fn the_summary_counts_both_kinds_and_names_what_crashed_most() {
        let all = [crash("copyq", 3), crash("copyq", 2), crash("blender", 1), kill(0)];
        let shown: Vec<&Incident> = all.iter().collect();
        assert_eq!(summary(&shown), "3 crashes and 1 program killed for memory. copyq crashed most, 2 times.");
        let once = [crash("copyq", 1)];
        let shown: Vec<&Incident> = once.iter().collect();
        assert_eq!(summary(&shown), "1 crash and 0 programs killed for memory.");
    }

    #[test]
    fn a_name_the_kernel_cut_short_is_shown_whole_from_its_program() {
        let Incident::Crash(mut c) = crash("EpicGamesLaunch", 1) else { unreachable!() };
        c.exe = Some("/tmp/.mount_epic/usr/bin/EpicGamesLauncher".into());
        assert_eq!(name_of(&Incident::Crash(c.clone())), "EpicGamesLauncher");
        c.name = "python3".into();
        c.exe = Some("/usr/bin/python3.14".into());
        assert_eq!(name_of(&Incident::Crash(c)), "python3", "not cut, so not replaced");
    }

    /// A report pasted into a bug tracker starts with what crashed and
    /// how, then the frames, in order.
    #[test]
    fn a_copied_stack_trace_says_what_crashed_before_its_frames() {
        let Incident::Crash(c) = crash("hyprforge-files", 1) else { unreachable!() };
        let text = report(&c);
        assert!(text.starts_with("hyprforge-files (?) — SIGSEGV\n"), "{text}");
        assert!(text.ends_with("#0 0x1 f (lib.so)\n"));
    }

    /// The machine running out and a scope at its own limit want
    /// different fixes, so their rows must not read alike.
    #[test]
    fn a_kill_at_a_limit_and_the_machine_running_out_read_differently() {
        let held = Some(hyprforge_procman::incidents::Held { total_vm: 0, anon: 1024, file: 0, shmem: 0 });
        let machine = Incident::Kill(Kill { at: 0, boot: String::new(), killer: Killer::KernelMachine, pid: Some(1), name: Some("x".into()), uid: None, unit: None, held });
        let limit = Incident::Kill(Kill { killer: Killer::KernelLimit, ..match &machine { Incident::Kill(k) => k.clone(), _ => unreachable!() } });
        assert_ne!(what(&machine), what(&limit));
        assert!(what(&limit).contains("limit"));
    }
}

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
use iced::widget::{button, canvas, column, container, row, scrollable, Space};
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

/// One program's incidents in the range: newest first, as the journal
/// lists them.
pub struct Group<'a> {
    pub name: String,
    pub list: Vec<&'a Incident>,
}

impl Group<'_> {
    fn kills(&self) -> usize {
        self.list.iter().filter(|i| matches!(i, Incident::Kill(_))).count()
    }
}

/// The range's incidents by program, the most first, and by the newest
/// among equals — "what keeps crashing" is the question this tab is
/// opened with, and thirty-three rows of one name said it by repetition.
pub fn by_program<'a>(shown: &[&'a Incident]) -> Vec<Group<'a>> {
    let mut groups: Vec<Group<'a>> = Vec::new();
    for i in shown {
        let name = name_of(i);
        match groups.iter_mut().find(|g| g.name == name) {
            Some(g) => g.list.push(i),
            None => groups.push(Group { name, list: vec![i] }),
        }
    }
    groups.sort_by(|a, b| b.list.len().cmp(&a.list.len()).then(b.list[0].at().cmp(&a.list[0].at())));
    groups
}

/// The toolbar's one line: how many, how many of them for memory, and
/// from how many programs.
pub fn tally(shown: &[&Incident]) -> (String, Option<String>, String) {
    let kills = shown.iter().filter(|i| matches!(i, Incident::Kill(_))).count();
    let crashes = shown.len() - kills;
    let programs = by_program(shown).len();
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    (plural(crashes, "crash", "crashes"), (kills > 0).then(|| format!("{kills} out of memory")), plural(programs, "program", "programs"))
}

/// Each incident's time as a tick across the range.
struct Ticks {
    at: Vec<(i64, bool)>,
    from: i64,
    to: i64,
}

impl canvas::Program<Message> for Ticks {
    type State = ();

    fn draw(&self, _state: &(), renderer: &iced::Renderer, _theme: &iced::Theme, bounds: iced::Rectangle, _cursor: iced::mouse::Cursor) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let span = (self.to - self.from).max(1) as f32;
        for (at, kill) in &self.at {
            let x = ((at - self.from) as f32 / span * bounds.width).round() + 0.5;
            let color = if *kill { theme::warning() } else { theme::error() };
            frame.stroke(&canvas::Path::line(iced::Point::new(x, 0.0), iced::Point::new(x, bounds.height)), canvas::Stroke::default().with_color(color).with_width(1.5));
        }
        vec![frame.into_geometry()]
    }
}

/// A short "when": today's as a time, this week's with the day.
fn short_when(at: i64) -> String {
    let Some(t) = chrono::DateTime::from_timestamp_micros(at).map(|t| t.with_timezone(&chrono::Local)) else { return "—".into() };
    let now = chrono::Local::now();
    if t.date_naive() == now.date_naive() {
        t.format("today %H:%M").to_string()
    } else if (now - t).num_days() < 6 {
        t.format("%a %H:%M").to_string()
    } else {
        t.format("%d %b").to_string()
    }
}

impl App {
    /// A mail client's shape: programs on the left, plain rows with their
    /// count and their range as ticks; the chosen one's newest incident on
    /// the right, then every earlier one. "Every crash" is the flat list,
    /// newest first. Narrow, the detail replaces the list.
    pub fn crashes_view(&self, s: FontScale) -> Element<'_, Message> {
        let Some(data) = &self.crashes else {
            return container(widgets::hint_text("Reading the journal…", s)).padding(spacing::LG).into();
        };
        let found = match &data.incidents {
            Ok(found) => found,
            Err(e) => return container(widgets::hint_text(format!("The journal couldn't be read: {e}"), s)).padding(spacing::LG).into(),
        };
        let wide = self.fit() == hyprforge_procman::fit::Fit::Full;
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
        let view = widgets::segmented_choice(&[true, false], Some(&self.crash_by_program), |by| if *by { "By program".to_string() } else { "Every crash".to_string() }, Message::CrashView, s);
        let now = chrono::Local::now().timestamp_micros();
        let from = now - self.crash_days as i64 * 86_400_000_000;
        let shown: Vec<&Incident> = found.list.iter().filter(|i| i.at() >= from).collect();
        let (crashes, kills, programs) = tally(&shown);
        let mut said = row![widgets::meta_text(crashes, density::META_TEXT_BASE, s).font(theme::mono_font())].spacing(spacing::SM);
        if let Some(k) = kills {
            said = said.push(widgets::meta_text("·", density::META_TEXT_BASE, s)).push(widgets::scaled_text(k, density::META_TEXT_BASE, s).font(theme::mono_font()).color(theme::warning()));
        }
        said = said.push(widgets::meta_text(format!("· {programs}"), density::META_TEXT_BASE, s).font(theme::mono_font()));
        if !found.everyone {
            said = said.push(widgets::meta_text("· yours only", density::META_TEXT_BASE, s));
        }
        // Narrow, the counts take a line of their own under the controls
        // rather than wrapping beside them.
        let mut page = if wide {
            column![row![range, view, Space::new().width(Length::Fill), said].spacing(spacing::MD).align_y(Alignment::Center)]
        } else {
            column![row![range, view].spacing(spacing::MD).align_y(Alignment::Center), said]
        }
        .spacing(spacing::MD)
        .padding(spacing::LG);
        if shown.is_empty() {
            page = page.push(widgets::hint_text("Nothing crashed and nothing was killed for memory in this range.", s));
            return page.into();
        }

        let groups = by_program(&shown);
        // What is chosen: in By program, a program (the busiest, until one
        // is picked) and within it an incident (its newest, until one is);
        // in Every crash, an incident.
        let group = groups.iter().find(|g| Some(&g.name) == self.crash_program.as_ref()).or(groups.first());
        let picked = |list: &[&'_ Incident]| -> Option<i64> { self.crash_selected.filter(|at| list.iter().any(|i| i.at() == *at)).or(list.first().map(|i| i.at())) };

        let mut list = column![].spacing(2.0);
        let detail: Option<Element<'_, Message>>;
        if self.crash_by_program {
            for g in &groups {
                let selected = group.is_some_and(|c| c.name == g.name);
                let newest = g.list[0];
                let ticks = canvas(Ticks { at: g.list.iter().map(|i| (i.at(), matches!(i, Incident::Kill(_)))).collect(), from, to: now }).width(Length::Fixed(s.apply(110.0))).height(Length::Fixed(s.apply(14.0)));
                // The signal is what someone who reads crashes looks for;
                // a kill says how it was killed.
                let how = match newest {
                    Incident::Crash(c) => c.signal.clone().unwrap_or_else(|| "crashed".into()),
                    // The sentence's first letter only: "MB" stays "MB".
                    Incident::Kill(_) => {
                        let w = what(newest);
                        let mut c = w.chars();
                        c.next().map_or(String::new(), |f| f.to_lowercase().chain(c).collect())
                    }
                };
                // A program that both crashed and was killed says the other
                // kind too, counted in its own word.
                let (kills, crashes) = (g.kills(), g.list.len() - g.kills());
                let how = match newest {
                    Incident::Crash(_) if kills > 0 => format!("{how}, and {kills} out of memory"),
                    Incident::Kill(_) if crashes > 0 => format!("{how}, and {crashes} {}", if crashes == 1 { "crash" } else { "crashes" }),
                    _ => how,
                };
                let line = row![
                    column![
                        widgets::clamped_text(g.name.clone(), 1).size(s.apply(density::ROW_TEXT_BASE)).width(Length::Fill),
                        widgets::clamped_text(format!("{how} · {}", short_when(newest.at())), 1).size(s.apply(density::META_TEXT_BASE)).color(ink(selected)).width(Length::Fill),
                    ]
                    .width(Length::Fill),
                    ticks,
                    container(widgets::scaled_text(g.list.len().to_string(), density::ROW_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(34.0))).align_x(Alignment::End),
                ]
                .spacing(spacing::MD)
                .align_y(Alignment::Center);
                list = list.push(button(line).padding([spacing::SM, spacing::SM]).width(Length::Fill).on_press(Message::CrashProgram(Some(g.name.clone()))).style(move |t, status| widgets::selectable_row_style(t, status, selected)));
            }
            detail = group.map(|g| {
                let at = picked(&g.list);
                let chosen = g.list.iter().find(|i| Some(i.at()) == at).copied().unwrap_or(g.list[0]);
                let mut d = column![self.incident_head(&g.name, chosen, s), self.incident_detail(chosen, &data.names, s)].spacing(spacing::MD);
                if g.list.len() > 1 {
                    d = d.push(widgets::meta_text(format!("{} in this range", g.list.len()), density::META_TEXT_BASE, s));
                    let mut others = column![].spacing(0.0);
                    for i in &g.list {
                        let selected = i.at() == chosen.at();
                        let at = i.at();
                        let line = row![
                            container(widgets::scaled_text(when(at), density::META_TEXT_BASE, s).font(theme::mono_font())).width(Length::Fixed(s.apply(190.0))),
                            widgets::meta_text(what(i), density::META_TEXT_BASE, s).color(ink(selected)),
                        ]
                        .spacing(spacing::MD);
                        others = others.push(button(line).padding([spacing::XS, spacing::SM]).width(Length::Fill).on_press(Message::CrashSelect(Some(at))).style(move |t, status| widgets::selectable_row_style(t, status, selected)));
                    }
                    d = d.push(others);
                }
                d.into()
            });
        } else {
            let at = picked(&shown);
            for i in &shown {
                let selected = Some(i.at()) == at;
                let at = i.at();
                let line = row![
                    column![
                        widgets::clamped_text(name_of(i), 1).size(s.apply(density::ROW_TEXT_BASE)).width(Length::Fill),
                        widgets::clamped_text(what(i), 1).size(s.apply(density::META_TEXT_BASE)).color(ink(selected)).width(Length::Fill),
                    ]
                    .width(Length::Fill),
                    widgets::meta_text(short_when(at), density::META_TEXT_BASE, s).color(ink(selected)),
                ]
                .spacing(spacing::MD)
                .align_y(Alignment::Center);
                list = list.push(button(line).padding([spacing::SM, spacing::SM]).width(Length::Fill).on_press(Message::CrashSelect(Some(at))).style(move |t, status| widgets::selectable_row_style(t, status, selected)));
            }
            detail = shown.iter().find(|i| Some(i.at()) == at).map(|i| column![self.incident_head(&name_of(i), i, s), self.incident_detail(i, &data.names, s)].spacing(spacing::MD).into());
        }

        if !wide {
            // Narrow: one or the other. A program or crash someone chose
            // shows its detail, with the way back.
            let chose = if self.crash_by_program { self.crash_program.is_some() } else { self.crash_selected.is_some() };
            if let (true, Some(d)) = (chose, detail) {
                let back = widgets::secondary_button("Back").on_press(if self.crash_by_program { Message::CrashProgram(None) } else { Message::CrashSelect(None) });
                page = page.push(column![back, d].spacing(spacing::MD));
            } else {
                page = page.push(list);
            }
            return scrollable(page).height(Length::Fill).into();
        }
        let split = row![
            container(scrollable(container(list).padding(iced::Padding { right: spacing::MD, ..iced::Padding::ZERO }))).width(Length::Fixed(s.apply(420.0))).height(Length::Fill),
            widgets::vertical_divider(),
            scrollable(container(detail.unwrap_or_else(|| Space::new().into())).padding(iced::Padding { top: 0.0, bottom: spacing::MD, left: spacing::LG, right: spacing::MD })).height(Length::Fill),
        ]
        .height(Length::Fill);
        page.push(column![widgets::divider(), split]).height(Length::Fill).into()
    }

    /// An incident's heading: the program, when, and what to do with it.
    fn incident_head<'a>(&self, name: &str, i: &'a Incident, s: FontScale) -> Element<'a, Message> {
        let mut head = row![widgets::scaled_text(name.to_string(), 20.0, s), widgets::meta_text(when(i.at()), density::META_TEXT_BASE, s), Space::new().width(Length::Fill)].spacing(spacing::MD).align_y(Alignment::Center);
        if let Incident::Crash(c) = i {
            head = head.push(widgets::secondary_button("Copy trace").on_press_maybe(c.stack.as_ref().filter(|f| !f.is_empty()).map(|_| Message::Copy(report(c)))));
        }
        head.into()
    }

    fn incident_detail<'a>(&self, i: &'a Incident, names: &HashMap<u32, String>, s: FontScale) -> Element<'a, Message> {
        let fact = |label: &'static str, value: String| -> Element<'a, Message> {
            row![
                container(widgets::meta_text(label, density::META_TEXT_BASE, s)).width(Length::Fixed(s.apply(90.0))),
                widgets::scaled_text(value, density::META_TEXT_BASE, s).font(theme::mono_font()).width(Length::Fill),
            ]
            .spacing(spacing::MD)
            .into()
        };
        let user = |uid: Option<u32>| uid.map(|u| names.get(&u).cloned().unwrap_or_else(|| u.to_string()));
        let mut facts = column![].spacing(spacing::XS);
        match i {
            Incident::Crash(c) => {
                let said: Element<'a, Message> = match &c.signal {
                    Some(sig) => row![
                        widgets::scaled_text(format!("{}{}", signal_words(sig)[..1].to_uppercase(), &signal_words(sig)[1..]), density::ROW_TEXT_BASE, s),
                        widgets::meta_text(sig.clone(), density::META_TEXT_BASE, s).font(theme::mono_font()),
                    ]
                    .spacing(spacing::SM)
                    .align_y(Alignment::Center)
                    .into(),
                    None => widgets::scaled_text("Crashed", density::ROW_TEXT_BASE, s).into(),
                };
                if let Some(pid) = c.pid {
                    facts = facts.push(fact("pid", pid.to_string()));
                }
                facts = facts.push(fact("binary", c.exe.clone().unwrap_or_else(|| c.name.clone())));
                if let Some(t) = &c.thread {
                    facts = facts.push(fact("thread", t.clone()));
                }
                if let Some(u) = user(c.uid) {
                    facts = facts.push(fact("user", u));
                }
                if let Some(unit) = &c.unit {
                    facts = facts.push(fact("unit", unit.clone()));
                }
                facts = facts.push(fact(
                    "core",
                    match &c.core {
                        _ if !c.dumped => "not kept".into(),
                        Some(p) if p.exists() => format!("kept · coredumpctl debug {}", c.pid.map(|p| p.to_string()).unwrap_or_default()),
                        Some(_) => "kept then, since cleaned up".into(),
                        None => "not kept".into(),
                    },
                ));
                let trace: Element<'a, Message> = match &c.stack {
                    None => widgets::hint_text("The journal kept no message for this crash, so there is no stack trace.", s).into(),
                    Some(frames) if frames.is_empty() => widgets::hint_text("systemd-coredump recorded no stack trace.", s).into(),
                    Some(frames) => {
                        let mut stack = column![].spacing(2.0);
                        for f in frames {
                            stack = stack.push(widgets::scaled_text(f.clone(), density::META_TEXT_BASE, s).font(theme::mono_font()).wrapping(iced::widget::text::Wrapping::None));
                        }
                        // Frames are long; they scroll sideways rather than wrap.
                        container(scrollable(container(stack).padding(spacing::SM)).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(4.0).scroller_width(4.0))))
                            .width(Length::Fill)
                            .style(|_t: &iced::Theme| container::Style { background: Some(iced::Background::Color(hyprforge_ui::theme::surface::sidebar())), border: iced::Border { radius: hyprforge_ui::density::inner_radius().into(), ..iced::Border::default() }, ..container::Style::default() })
                            .into()
                    }
                };
                column![said, facts, trace].spacing(spacing::MD).into()
            }
            Incident::Kill(k) => {
                let said = widgets::scaled_text(format!("Killed by {}", killer_words(k.killer)), density::ROW_TEXT_BASE, s);
                if let Some(h) = k.held {
                    facts = facts.push(fact(
                        "using",
                        format!("{} resident ({} of its own, {} of files, {} shared)", bytes(h.resident_bytes()), bytes(h.anon * 1024), bytes(h.file * 1024), bytes(h.shmem * 1024)),
                    ));
                }
                if let Some(pid) = k.pid {
                    facts = facts.push(fact("pid", pid.to_string()));
                }
                if let Some(u) = user(k.uid) {
                    facts = facts.push(fact("user", u));
                }
                if let Some(unit) = &k.unit {
                    facts = facts.push(fact("unit", unit.clone()));
                }
                if k.pid.is_none() {
                    facts = facts.push(widgets::hint_text("systemd recorded a kill in this unit; the kernel's own line naming the process wasn't kept.", s));
                }
                column![said, facts].spacing(spacing::MD).into()
            }
        }
    }
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

    /// What keeps crashing comes first: by count, the newest among
    /// equals, each program's incidents newest first.
    #[test]
    fn programs_are_ordered_by_how_often_they_crashed() {
        let all = [crash("copyq", 5), crash("blender", 4), crash("copyq", 2), crash("files", 1), kill(0)];
        let shown: Vec<&Incident> = all.iter().collect();
        let g = by_program(&shown);
        let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["copyq", "blender", "files", "app.slice"]);
        assert_eq!(g[0].list.iter().map(|i| i.at()).collect::<Vec<_>>(), [5, 2]);
        assert_eq!(tally(&shown), ("4 crashes".to_string(), Some("1 out of memory".to_string()), "4 programs".to_string()));
        let once = [crash("copyq", 1)];
        let shown: Vec<&Incident> = once.iter().collect();
        assert_eq!(tally(&shown), ("1 crash".to_string(), None, "1 program".to_string()));
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

//! hyprforge-procman — the window.
//!
//! See `lib.rs` for the layers under it and DESIGN.md for where this is
//! going. This file owns the state between frames: the last reading (a
//! process's CPU share needs two), the graphs' history, what is
//! selected, and the one question this app asks before acting — "end
//! this?".

mod view;

use hyprforge_keys::{Key, KeyPress};
use hyprforge_procman::history::History;
use hyprforge_procman::model::{self, Column, Table, View};
use hyprforge_procman::proc::{ProcFs, Snapshot};
use hyprforge_procman::sample::{self, Id};
use hyprforge_procman::signal::{self, How};
use hyprforge_procman::users::Users;
use hyprforge_procman::windows::{self, Window};
use iced::widget::Id as WidgetId;
use iced::{keyboard, window, Size, Subscription, Task, Theme};
use std::time::Duration;

const APP_ID: &str = "hyprforge-procman";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Processes,
    Performance,
}

/// How often to read. Pausing is its own state, not an interval: it is
/// what makes picking a row out of a list that reorders every second
/// possible.
pub const INTERVALS: [Duration; 4] =
    [Duration::from_millis(500), Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(5)];

/// An action waiting on "are you sure".
#[derive(Debug, Clone)]
pub struct Confirm {
    pub id: Id,
    pub name: String,
    pub command: String,
    pub how: How,
    /// For an app: how many windows ending it closes.
    pub windows: usize,
    /// This window's session depends on it: the compositor, or a
    /// process this window runs under. Ending it is allowed, and said.
    pub critical: bool,
}

/// One refresh, read off the UI thread.
#[derive(Debug, Clone)]
pub struct Reading {
    snapshot: Snapshot,
    windows: Result<Vec<Window>, String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Read(Result<Box<Reading>, String>),
    Tab(Tab),
    Query(String),
    Sort(Column),
    Select(Id),
    Toggle(Id),
    ShowKernel(bool),
    Pause(bool),
    Interval(Duration),
    /// Ask before ending: End, or Kill.
    Ask(How),
    Confirmed,
    Cancelled,
    /// Bring the selected app's window forward.
    SwitchTo,
    /// Freeze the selected process, or let it run again.
    Freeze(bool),
    Key(KeyPress),
    Dismiss,
}

pub struct App {
    pub tab: Tab,
    fs: ProcFs,
    users: Users,
    me: u32,
    last: Option<Snapshot>,
    pub table: Table,
    pub view: View,
    pub selected: Option<Id>,
    pub confirm: Option<Confirm>,
    /// What went wrong last, said in the status bar until dismissed.
    pub notice: Option<String>,
    /// Why there are no apps, when Hyprland could not be asked.
    pub no_windows: Option<String>,
    pub paused: bool,
    pub interval: Duration,
    pub history: History,
    /// The whole machine's figures, for the status bar and Performance.
    pub totals: Totals,
    /// This window's own process and everything it runs under — never
    /// frozen, and named in an End confirmation.
    pub critical: std::collections::HashSet<i32>,
    reading: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Totals {
    pub cpu: Option<f32>,
    pub memory_used: u64,
    pub memory_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub processes: usize,
    pub threads: u64,
}

pub fn search_id() -> WidgetId {
    WidgetId::new("procman-search")
}

fn read(fs: ProcFs) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                let snapshot = fs.snapshot().map_err(|e| format!("couldn't read /proc: {e}"))?;
                Ok(Box::new(Reading { snapshot, windows: windows::read() }))
            })
            .await
            .map_err(|e| e.to_string())?
        },
        Message::Read,
    )
}

impl App {
    fn new() -> (App, Task<Message>) {
        let fs = ProcFs::default();
        let app = App {
            tab: Tab::Processes,
            fs: fs.clone(),
            users: Users::default(),
            // SAFETY: getuid has no preconditions.
            me: unsafe { libc::getuid() },
            last: None,
            table: Table::default(),
            view: View::default(),
            selected: None,
            confirm: None,
            notice: None,
            no_windows: None,
            paused: false,
            interval: INTERVALS[1],
            history: History::default(),
            totals: Totals::default(),
            critical: Default::default(),
            reading: true,
        };
        (app, read(fs))
    }

    fn title(&self) -> String {
        "Processes".to_string()
    }

    fn theme(&self) -> Theme {
        hyprforge_ui::theme::app_theme()
    }

    fn subscription(&self) -> Subscription<Message> {
        let tick = if self.paused { Subscription::none() } else { iced::time::every(self.interval).map(|_| Message::Tick) };
        Subscription::batch([tick, keyboard::listen().filter_map(|event| hyprforge_ui::keys::key_press(&event).map(Message::Key))])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                // A read still running is not doubled up behind: a slow
                // machine gets fewer refreshes, not a queue of them.
                if !self.reading {
                    self.reading = true;
                    return read(self.fs.clone());
                }
            }
            Message::Read(result) => {
                self.reading = false;
                match result {
                    Ok(reading) => self.take(*reading),
                    Err(e) => self.notice = Some(e),
                }
            }
            Message::Tab(tab) => self.tab = tab,
            Message::Query(q) => self.view.query = q,
            Message::Sort(column) => self.view.sort = self.view.sort.clicked(column),
            Message::Select(id) => self.selected = Some(id),
            Message::Toggle(id) => {
                if !self.view.expanded.remove(&id) {
                    self.view.expanded.insert(id);
                }
                self.selected = Some(id);
            }
            Message::ShowKernel(on) => self.view.show_kernel = on,
            Message::Pause(on) => self.paused = on,
            Message::Interval(d) => self.interval = d,
            Message::Ask(how) => self.confirm = self.selected.and_then(|id| self.confirmation(id, how)),
            Message::Cancelled => self.confirm = None,
            Message::Confirmed => {
                if let Some(c) = self.confirm.take() {
                    match signal::end(c.id, c.how) {
                        Ok(()) => {
                            self.notice = None;
                            // The next reading shows it gone, sooner than
                            // the next tick would.
                            if !self.reading {
                                self.reading = true;
                                return read(self.fs.clone());
                            }
                        }
                        Err(e) => self.notice = Some(format!("Couldn't end {}: {e}", c.name)),
                    }
                }
            }
            Message::SwitchTo => {
                if let Some(id) = self.selected.filter(|id| self.table.apps.iter().any(|a| a.lead.id == *id)) {
                    switch_to(id.pid);
                }
            }
            Message::Freeze(frozen) => {
                if let Some(id) = self.selected {
                    if frozen && self.critical.contains(&id.pid) {
                        // Freezing the compositor freezes the screen, and
                        // with it the only way to resume it.
                        self.notice = Some("This window runs under that process, so freezing it would freeze this window too.".into());
                    } else if let Err(e) = signal::freeze(id, frozen) {
                        self.notice = Some(format!("Couldn't {} it: {e}", if frozen { "freeze" } else { "resume" }));
                    } else if !self.reading {
                        self.reading = true;
                        return read(self.fs.clone());
                    }
                }
            }
            Message::Key(press) => return self.key(press),
            Message::Dismiss => self.notice = None,
        }
        Task::none()
    }

    fn key(&mut self, press: KeyPress) -> Task<Message> {
        let ctrl = press.mods.ctrl;
        match press.key {
            Key::Escape if self.confirm.is_some() => self.confirm = None,
            Key::Enter if self.confirm.is_some() => return self.update(Message::Confirmed),
            Key::Char('f') if ctrl => return iced::widget::operation::focus(search_id()),
            Key::Delete if self.selected.is_some() && self.confirm.is_none() => {
                return self.update(Message::Ask(if ctrl { How::Force } else { How::Ask }));
            }
            Key::Char('1') if ctrl => self.tab = Tab::Processes,
            Key::Char('2') if ctrl => self.tab = Tab::Performance,
            Key::Char('p') if ctrl => self.paused = !self.paused,
            _ => {}
        }
        Task::none()
    }

    /// What "end this?" says about `id`, or nothing if it is gone.
    fn confirmation(&self, id: Id, how: How) -> Option<Confirm> {
        if let Some(app) = self.table.apps.iter().find(|a| a.lead.id == id) {
            let critical = self.critical.contains(&id.pid);
            return Some(Confirm { id, name: app.lead.name.clone(), command: app.lead.command.clone(), how, windows: app.windows, critical });
        }
        let entry =
            self.table.apps.iter().flat_map(|a| a.members.iter()).chain(self.table.background.iter()).find(|e| e.id == id)?;
        let critical = self.critical.contains(&id.pid);
        Some(Confirm { id, name: entry.name.clone(), command: entry.command.clone(), how, windows: 0, critical })
    }

    fn take(&mut self, reading: Reading) {
        let Reading { snapshot, windows } = reading;
        let shares = self.last.as_ref().map(|before| sample::shares(before, &snapshot)).unwrap_or_default();
        let windows = match windows {
            Ok(w) => {
                self.no_windows = None;
                w
            }
            Err(e) => {
                self.no_windows = Some(e);
                Vec::new()
            }
        };
        let users = &mut self.users;
        self.table = model::table(&snapshot, &shares, &windows, self.me, &mut |uid| users.name(uid));

        let m = snapshot.memory;
        let cpu = self.last.as_ref().map(|before| sample::busy(before.cpu, snapshot.cpu));
        if let Some(before) = &self.last {
            self.history.cpu.push(cpu.unwrap_or(0.0));
            self.history.cores.resize_with(snapshot.cores.len(), Default::default);
            for (i, (a, b)) in before.cores.iter().zip(&snapshot.cores).enumerate() {
                self.history.cores[i].push(sample::busy(*a, *b));
            }
        }
        if m.total > 0 {
            self.history.memory.push(m.used() as f32 / m.total as f32);
        }
        if m.swap_total > 0 {
            self.history.swap.push(m.swap_used() as f32 / m.swap_total as f32);
        }
        self.totals = Totals {
            cpu,
            memory_used: m.used(),
            memory_total: m.total,
            swap_used: m.swap_used(),
            swap_total: m.swap_total,
            processes: snapshot.procs.iter().filter(|p| !p.kernel).count(),
            threads: snapshot.procs.iter().map(|p| u64::from(p.threads)).sum(),
        };
        // This process and what it runs under, plus the compositor by
        // name, in case this window was started some other way.
        self.critical = model::ancestry(&snapshot, std::process::id() as i32);
        self.critical.extend(snapshot.procs.iter().filter(|p| p.name == "Hyprland" && p.uid == self.me).map(|p| p.pid));
        // A selection whose process ended goes with it.
        if let Some(id) = self.selected {
            if !snapshot.procs.iter().any(|p| p.pid == id.pid && p.start == id.start) {
                self.selected = None;
            }
        }
        self.last = Some(snapshot);
    }
}

/// Focuses the window of process `pid` — Files' `hl.dsp.focus` call,
/// checked live there. Off the UI thread and bounded; a refusal leaves
/// focus where it was, which is all a person would see either way.
fn switch_to(pid: i32) {
    let lua = format!("hl.dsp.focus({{ window = \"pid:{pid}\" }})");
    std::thread::spawn(move || {
        let mut command = std::process::Command::new("hyprctl");
        command.args(["eval", &lua]);
        if let Err(e) = hyprforge_process::output(&mut command, hyprforge_process::TIMEOUT) {
            tracing::warn!("couldn't switch to the window: {e}");
        }
    });
}

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    hyprforge_ui::theme::init(hyprforge_appearance::look::resolve());
    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window::Settings {
            size: Size::new(1100.0, 720.0),
            min_size: Some(Size::new(640.0, 420.0)),
            platform_specific: window::settings::PlatformSpecific {
                application_id: APP_ID.to_string(),
                ..window::settings::PlatformSpecific::default()
            },
            ..window::Settings::default()
        })
        .run()
}

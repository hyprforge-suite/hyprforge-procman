//! hyprforge-procman — the window.
//!
//! See `lib.rs` for the layers under it and DESIGN.md for where this is
//! going. This file owns the state between frames: the last reading (a
//! process's CPU share needs two), the graphs' history, what is
//! selected, and the one question this app asks before acting — "end
//! this?".

mod view;

use hyprforge_keys::{Key, KeyPress};
use hyprforge_procman::helper;
use hyprforge_procman::history::History;
use hyprforge_procman::machine::{self, DiskRate, Machine, NetRate, SysFs};
use hyprforge_procman::model::{self, Column, Table, View};
use hyprforge_procman::proc::{ProcFs, Snapshot};
use hyprforge_procman::sample::{self, Id};
use hyprforge_procman::services::{self, Action, Dbus, Scope, Systemd, Unit};
use std::collections::HashMap;
use hyprforge_procman::signal::{self, How};
use hyprforge_procman::users::Users;
use hyprforge_procman::windows::{self, Window};
use iced::widget::Id as WidgetId;
use iced::{keyboard, window, Size, Subscription, Task, Theme};
use std::time::Duration;

const APP_ID: &str = "hyprforge-procman";

/// The Performance tab's rail: which resource is open beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    Cpu,
    Memory,
    Disk(String),
    Net(String),
    Gpu(String),
    Battery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Processes,
    Performance,
    Services,
}

/// The Services tab's state.
#[derive(Debug)]
pub struct ServicesTab {
    pub scope: Option<Scope>,
    pub units: HashMap<Scope, Vec<Unit>>,
    pub error: Option<String>,
    pub filter: services::Filter,
    pub query: String,
    pub selected: Option<String>,
    pub journal: Result<Vec<String>, String>,
    /// Running units' cgroups, refreshed with the list.
    cgroups: HashMap<String, std::path::PathBuf>,
    /// Last counters, for a CPU rate; and the rate, by unit.
    counters: HashMap<String, (services::Counters, std::time::Instant)>,
    pub cpu: HashMap<String, f32>,
    pub memory: HashMap<String, u64>,
    /// Refreshes since the list was read.
    age: u32,
    /// A stop or disable waiting on "are you sure".
    pub confirm: Option<(Scope, String, Action)>,
    pub busy: bool,
}

impl Default for ServicesTab {
    fn default() -> ServicesTab {
        ServicesTab {
            scope: None,
            units: HashMap::new(),
            error: None,
            filter: services::Filter::default(),
            query: String::new(),
            selected: None,
            journal: Ok(Vec::new()),
            cgroups: HashMap::new(),
            counters: HashMap::new(),
            cpu: HashMap::new(),
            memory: HashMap::new(),
            age: 0,
            confirm: None,
            busy: false,
        }
    }
}

impl ServicesTab {
    pub fn scope(&self) -> Scope {
        self.scope.unwrap_or(Scope::System)
    }
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
    machine: Machine,
    windows: Result<Vec<Window>, String>,
    at: std::time::Instant,
    /// Show everything was on and polkit said no this time: its few
    /// minutes ran out. The window locks again rather than prompting
    /// once a second.
    everything_lapsed: bool,
}

/// Whether the helper is filling in what this user cannot read.
#[derive(Debug, Clone)]
pub enum Everything {
    Locked,
    Asking,
    /// Unlocked, over this system bus connection.
    Open(zbus::Connection),
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Read(Result<Box<Reading>, String>),
    Tab(Tab),
    ServicesLoaded(Scope, Result<Vec<Unit>, String>),
    ServiceScope(Scope),
    ServiceFilter(services::Filter),
    ServiceQuery(String),
    SelectService(String),
    ServiceAct(Action),
    ServiceConfirmed,
    ServiceCancelled,
    ServiceActed(Result<(), String>),
    Journal(String, Result<Vec<String>, String>),
    Resource(Resource),
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
    /// Ask polkit, once, to see every process.
    ShowEverything,
    Unlocked(Result<zbus::Connection, String>),
    /// End through the helper, after the window was refused.
    EndAsAdmin,
    EndedAsAdmin(Result<(), String>),
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
    /// An end the window was refused, which the helper could do.
    pub admin_offer: Option<Confirm>,
    pub everything: Everything,
    last_at: Option<std::time::Instant>,
    /// Why there are no apps, when Hyprland could not be asked.
    pub no_windows: Option<String>,
    pub paused: bool,
    pub interval: Duration,
    pub history: History,
    pub resource: Resource,
    pub services: ServicesTab,
    pub machine: Machine,
    pub disk_rates: Vec<(String, DiskRate)>,
    pub net_rates: Vec<(String, NetRate)>,
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

fn read(fs: ProcFs, helper: Option<zbus::Connection>) -> Task<Message> {
    Task::perform(
        async move {
            let (mut snapshot, machine, windows) = tokio::task::spawn_blocking(move || {
                let snapshot = fs.snapshot().map_err(|e| format!("couldn't read /proc: {e}"))?;
                Ok::<_, String>((snapshot, SysFs::default().read(), windows::read()))
            })
            .await
            .map_err(|e| e.to_string())??;
            let at = std::time::Instant::now();
            let mut everything_lapsed = false;
            if let Some(system) = helper {
                everything_lapsed = !fill_in(&system, &mut snapshot).await;
            }
            Ok(Box::new(Reading { snapshot, machine, windows, at, everything_lapsed }))
        },
        Message::Read,
    )
}

/// Asks the helper for what this user could not read, without a prompt.
/// `false` when polkit's yes has lapsed.
async fn fill_in(system: &zbus::Connection, snapshot: &mut Snapshot) -> bool {
    let wanted: Vec<(i32, u64)> = snapshot.procs.iter().filter(|p| p.io.is_none() && !p.kernel).map(|p| (p.pid, p.start)).collect();
    if wanted.is_empty() {
        return true;
    }
    let Ok(proxy) = helper::ProcManProxy::new(system).await else { return false };
    match proxy.usage(wanted, false).await {
        Ok(usage) => {
            let by_id: std::collections::HashMap<(i32, u64), (u64, u64)> =
                usage.into_iter().map(|u| ((u.pid, u.start), (u.read_bytes, u.write_bytes))).collect();
            for p in &mut snapshot.procs {
                if let Some(io) = by_id.get(&(p.pid, p.start)) {
                    p.io = Some(*io);
                }
            }
            true
        }
        Err(_) => false,
    }
}

/// The helper over the system bus, prompting if polkit wants a password.
async fn unlock() -> Result<zbus::Connection, String> {
    let system = zbus::Connection::system().await.map_err(|e| format!("couldn't reach the system bus: {e}"))?;
    let proxy = helper::ProcManProxy::new(&system).await.map_err(|e| e.to_string())?;
    proxy.usage(Vec::new(), true).await.map_err(helper_error)?;
    Ok(system)
}

/// The helper's refusals in words.
fn helper_error(e: zbus::Error) -> String {
    let text = e.to_string();
    if text.contains("ServiceUnknown") || text.contains("was not provided") {
        "the privileged helper isn't installed".into()
    } else if text.contains("not authorized") || text.contains("AccessDenied") {
        "administrator access wasn't given".into()
    } else {
        text
    }
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
            admin_offer: None,
            everything: Everything::Locked,
            last_at: None,
            no_windows: None,
            paused: false,
            interval: INTERVALS[1],
            history: History::default(),
            resource: Resource::Cpu,
            services: ServicesTab::default(),
            machine: Machine::default(),
            disk_rates: Vec::new(),
            net_rates: Vec::new(),
            totals: Totals::default(),
            critical: Default::default(),
            reading: true,
        };
        (app, read(fs, None))
    }

    fn load_services(&mut self) -> Task<Message> {
        let scope = self.services.scope();
        self.services.age = 0;
        self.services.cgroups = services::cgroups(std::path::Path::new("/sys/fs/cgroup"));
        Task::perform(async move { (scope, Dbus.list(scope).await) }, |(scope, result)| Message::ServicesLoaded(scope, result))
    }

    fn load_journal(&self, name: String) -> Task<Message> {
        let scope = self.services.scope();
        Task::perform(
            async move {
                let lines = tokio::task::spawn_blocking({
                    let name = name.clone();
                    move || services::journal(scope, &name, 60)
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
                (name, lines)
            },
            |(name, lines)| Message::Journal(name, lines),
        )
    }

    fn service_act(&mut self, scope: Scope, name: String, action: Action) -> Task<Message> {
        self.services.busy = true;
        Task::perform(async move { Dbus.act(scope, name, action).await }, Message::ServiceActed)
    }

    /// Each running unit's CPU share and memory, from its cgroup.
    fn service_counters(&mut self) {
        let now = std::time::Instant::now();
        let cores = self.history.cores.len().max(1) as f32;
        let mut cpu = HashMap::new();
        let mut memory = HashMap::new();
        for (name, path) in &self.services.cgroups {
            let Some(c) = services::counters(path) else { continue };
            if let Some((before, at)) = self.services.counters.get(name) {
                let seconds = now.duration_since(*at).as_secs_f32();
                if seconds > 0.0 {
                    // A share of the whole machine, as the Processes tab's.
                    let used = c.cpu_usec.saturating_sub(before.cpu_usec) as f32 / 1e6;
                    cpu.insert(name.clone(), (used / seconds / cores * 100.0).min(100.0));
                }
            }
            memory.insert(name.clone(), c.memory);
            self.services.counters.insert(name.clone(), (c, now));
        }
        self.services.cpu = cpu;
        self.services.memory = memory;
    }

    fn helper(&self) -> Option<zbus::Connection> {
        match &self.everything {
            Everything::Open(system) => Some(system.clone()),
            _ => None,
        }
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
                if self.tab == Tab::Services {
                    self.service_counters();
                    self.services.age += 1;
                    // The list itself every few refreshes: units change
                    // state rarely, and a full list is a D-Bus round trip.
                    if self.services.age >= 5 && !self.services.busy {
                        self.services.busy = true;
                        let list = self.load_services();
                        if !self.reading {
                            self.reading = true;
                            return Task::batch([list, read(self.fs.clone(), self.helper())]);
                        }
                        return list;
                    }
                }
                // A read still running is not doubled up behind: a slow
                // machine gets fewer refreshes, not a queue of them.
                if !self.reading {
                    self.reading = true;
                    return read(self.fs.clone(), self.helper());
                }
            }
            Message::Read(result) => {
                self.reading = false;
                match result {
                    Ok(reading) => self.take(*reading),
                    Err(e) => self.notice = Some(e),
                }
            }
            Message::Tab(tab) => {
                self.tab = tab;
                if tab == Tab::Services {
                    return self.load_services();
                }
            }
            Message::ServicesLoaded(scope, result) => {
                self.services.busy = false;
                match result {
                    Ok(units) => {
                        self.services.units.insert(scope, units);
                        self.services.error = None;
                    }
                    Err(e) => self.services.error = Some(e),
                }
            }
            Message::ServiceScope(scope) => {
                self.services.scope = Some(scope);
                self.services.selected = None;
                return self.load_services();
            }
            Message::ServiceFilter(f) => self.services.filter = f,
            Message::ServiceQuery(q) => self.services.query = q,
            Message::SelectService(name) => {
                self.services.selected = Some(name.clone());
                return self.load_journal(name);
            }
            Message::ServiceAct(action) => {
                let Some(name) = self.services.selected.clone() else { return Task::none() };
                let scope = self.services.scope();
                // Stopping or switching off is asked about first: stopping
                // NetworkManager is how a Wi-Fi connection ends.
                if matches!(action, Action::Stop | Action::Disable | Action::Mask) {
                    self.services.confirm = Some((scope, name, action));
                    return Task::none();
                }
                return self.service_act(scope, name, action);
            }
            Message::ServiceConfirmed => {
                if let Some((scope, name, action)) = self.services.confirm.take() {
                    return self.service_act(scope, name, action);
                }
            }
            Message::ServiceCancelled => self.services.confirm = None,
            Message::ServiceActed(result) => {
                self.services.busy = false;
                if let Err(e) = result {
                    self.notice = Some(format!("That didn't work: {e}."));
                }
                let mut tasks = vec![self.load_services()];
                if let Some(name) = self.services.selected.clone() {
                    tasks.push(self.load_journal(name));
                }
                return Task::batch(tasks);
            }
            Message::Journal(name, lines) => {
                if self.services.selected.as_deref() == Some(name.as_str()) {
                    self.services.journal = lines;
                }
            }
            Message::Resource(resource) => self.resource = resource,
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
                                return read(self.fs.clone(), self.helper());
                            }
                        }
                        Err(signal::Refused::NotAllowed) => {
                            self.notice = Some(format!("{} belongs to another user.", c.name));
                            self.admin_offer = Some(c);
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
                        return read(self.fs.clone(), self.helper());
                    }
                }
            }
            Message::ShowEverything => {
                self.everything = Everything::Asking;
                return Task::perform(unlock(), Message::Unlocked);
            }
            Message::Unlocked(result) => match result {
                Ok(system) => {
                    self.everything = Everything::Open(system);
                    self.notice = None;
                }
                Err(e) => {
                    self.everything = Everything::Locked;
                    self.notice = Some(format!("Couldn't show everything: {e}."));
                }
            },
            Message::EndAsAdmin => {
                if let Some(c) = self.admin_offer.take() {
                    let signal = match c.how {
                        How::Ask => libc::SIGTERM,
                        How::Force => libc::SIGKILL,
                    };
                    return Task::perform(
                        async move {
                            let system = zbus::Connection::system().await.map_err(|e| e.to_string())?;
                            let proxy = helper::ProcManProxy::new(&system).await.map_err(|e| e.to_string())?;
                            proxy.signal(c.id.pid, c.id.start, signal, true).await.map_err(helper_error)
                        },
                        Message::EndedAsAdmin,
                    );
                }
            }
            Message::EndedAsAdmin(result) => match result {
                Ok(()) => {
                    self.notice = None;
                    if !self.reading {
                        self.reading = true;
                        return read(self.fs.clone(), self.helper());
                    }
                }
                Err(e) => self.notice = Some(format!("Couldn't end it: {e}.")),
            },
            Message::Key(press) => return self.key(press),
            Message::Dismiss => {
                self.notice = None;
                self.admin_offer = None;
            }
        }
        Task::none()
    }

    fn key(&mut self, press: KeyPress) -> Task<Message> {
        let ctrl = press.mods.ctrl;
        match press.key {
            Key::Escape if self.confirm.is_some() => self.confirm = None,
            Key::Escape if self.services.confirm.is_some() => self.services.confirm = None,
            Key::Enter if self.confirm.is_some() => return self.update(Message::Confirmed),
            Key::Char('f') if ctrl => return iced::widget::operation::focus(search_id()),
            Key::Delete if self.selected.is_some() && self.confirm.is_none() => {
                return self.update(Message::Ask(if ctrl { How::Force } else { How::Ask }));
            }
            Key::Char('1') if ctrl => self.tab = Tab::Processes,
            Key::Char('2') if ctrl => self.tab = Tab::Performance,
            Key::Char('3') if ctrl => return self.update(Message::Tab(Tab::Services)),
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
        let Reading { snapshot, machine, windows, at, everything_lapsed } = reading;
        let seconds = self.last_at.map_or(0.0, |t| at.duration_since(t).as_secs_f64());
        if self.last_at.is_some() {
            let (disks, nets) = machine::rates(&self.machine, &machine, seconds);
            for (name, rate) in &disks {
                self.history.disks.entry(name.clone()).or_default().push(rate.active);
            }
            for (name, rate) in &nets {
                self.history.nets.entry(name.clone()).or_default().push((rate.rx + rate.tx) as f32);
            }
            self.disk_rates = disks;
            self.net_rates = nets;
        }
        for gpu in &machine.gpus {
            if let Some(busy) = gpu.busy_percent {
                self.history.gpus.entry(gpu.card.clone()).or_default().push(f32::from(busy) / 100.0);
            }
        }
        if let Some(b) = &machine.battery {
            self.history.battery.push(f32::from(b.percent) / 100.0);
        }
        self.machine = machine;
        if everything_lapsed && matches!(self.everything, Everything::Open(_)) {
            self.everything = Everything::Locked;
            self.notice = Some("Show everything has ended: administrator access timed out.".into());
        }
        let mut usage = sample::Use::default();
        if let Some(before) = &self.last {
            usage.cpu = sample::shares(before, &snapshot);
            usage.disk = sample::disk_rates(before, &snapshot, seconds);
        }
        self.last_at = Some(at);
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
        self.table = model::table(&snapshot, &usage, &windows, self.me, &mut |uid| users.name(uid));

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

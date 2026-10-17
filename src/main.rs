//! hyprforge-procman — the window.
//!
//! See `lib.rs` for the layers under it and DESIGN.md for where this is
//! going. This file owns the state between frames: the last reading (a
//! process's CPU share needs two), the graphs' history, what is
//! selected, and the one question this app asks before acting — "end
//! this?".

mod crashes_view;
mod details;
mod menus;
mod overview;
mod past;
mod people;
mod recorded_view;
mod view;

use hyprforge_keys::{Key, KeyPress};
use hyprforge_procman::bound;
use hyprforge_procman::helper;
use hyprforge_procman::history::History;
use hyprforge_procman::inspect;
use hyprforge_procman::machine::{self, DiskRate, Machine, NetRate, SysFs};
use hyprforge_procman::model::{self, Column, Table, View};
use hyprforge_procman::proc::{ProcFs, Snapshot};
use hyprforge_procman::sample::{self, Id};
use hyprforge_procman::services::{self, Action, Dbus, Scope, Systemd, Unit};
use std::collections::{HashMap, HashSet};
use hyprforge_procman::sessions::{self, Session, SessionAction};
use hyprforge_procman::signal::{self, How};
use hyprforge_procman::startup;
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
    /// The Advanced view's dashboard: everything at once, btop-style.
    Overview,
    Processes,
    Performance,
    History,
    /// The recorder's journal: the whole machine, any moment of a day.
    Recorded,
    Startup,
    Users,
    Details,
    Services,
    /// What crashed, and what was killed for memory, from the journal.
    Crashes,
}

/// Everything the Startup tab lists, read off the UI thread.
#[derive(Debug, Clone)]
pub struct StartupData {
    pub autostarts: Vec<startup::Autostart>,
    /// This user's units, for their state and file state.
    pub units: Vec<Unit>,
    /// Measured start times, by unit name.
    pub blame: HashMap<String, u32>,
    pub hyprland: Result<Vec<hyprforge_session::autostart::Program>, String>,
}

fn autostart_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_default()
        .join("autostart")
}

fn system_autostart_dirs() -> Vec<std::path::PathBuf> {
    let dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    dirs.split(':').filter(|d| !d.is_empty()).map(|d| std::path::PathBuf::from(d).join("autostart")).collect()
}

async fn load_startup() -> StartupData {
    let units = Dbus.list(Scope::User).await.unwrap_or_default();
    tokio::task::spawn_blocking(move || {
        let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let desktops: Vec<&str> = desktops.split(':').filter(|d| !d.is_empty()).collect();
        StartupData {
            autostarts: startup::autostarts(&system_autostart_dirs(), &autostart_dir(), &desktops),
            units,
            blame: startup::blame(),
            hyprland: startup::hyprland_programs(&hyprforge_paths::hyprforge_config_dir().join("session.toml")),
        }
    })
    .await
    // The read itself failing is said where Hyprland's list would be,
    // never shown as "nothing starts with you".
    .unwrap_or_else(|e| StartupData {
        autostarts: Vec::new(),
        units: Vec::new(),
        blame: HashMap::new(),
        hyprland: Err(format!("reading what starts with you failed: {e}")),
    })
}

/// What the Details tab knows about the selected process beyond its row.
#[derive(Debug, Clone, Default)]
pub struct Properties {
    pub id: Option<Id>,
    pub exe: Option<std::path::PathBuf>,
    pub cwd: Option<std::path::PathBuf>,
    pub unit: Option<String>,
    /// `None`: not this user's to see.
    pub open_files: Option<usize>,
    pub sockets: Option<Vec<inspect::Socket>>,
}

#[derive(Debug, Default)]
pub struct DetailsTab {
    pub tree: bool,
    pub properties: Properties,
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
    /// End it and everything it started.
    pub tree: bool,
    /// This window's session depends on it: the compositor, or a
    /// process this window runs under. Ending it is allowed, and said.
    pub critical: bool,
}

/// One refresh, read off the UI thread.
#[derive(Debug, Clone)]
pub struct Reading {
    snapshot: Snapshot,
    /// GPU counters for the processes seen holding a GPU.
    gpu: HashMap<i32, hyprforge_procman::gpu::DrmUse>,
    machine: Machine,
    windows: Result<Vec<Window>, String>,
    at: std::time::Instant,
    /// The NVIDIA card's answer, when it was asked this time.
    nvidia: Option<Result<hyprforge_procman::nvidia::Reading, String>>,
    /// Show everything was on and polkit said no this time: its few
    /// minutes ran out. The window locks again rather than prompting
    /// once a second.
    everything_lapsed: bool,
    /// What the helper found for the `port:`/`file:` search this reading
    /// was asked with, among every user's processes — `None` when there
    /// was no search, Show everything was off, or the helper couldn't say.
    held: Option<(inspect::Lookup, Vec<(i32, u64)>)>,
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
    Tree(bool),
    /// App history's range, in days.
    HistoryRange(usize),
    Advanced(bool),
    StartupLoaded(Box<StartupData>),
    /// An autostart entry on or off, by id.
    Autostart(String, bool),
    /// A user unit on or off at login.
    UnitAtLogin(String, bool),
    StartupChanged(Result<(), String>),
    OpenSessionSettings,
    SessionsLoaded(Result<Vec<Session>, String>),
    Session(String, SessionAction),
    SignOutConfirmed,
    SessionDone(Result<(), String>),
    EndTree,
    /// Lower (`true`) or raise the selected process's priority.
    Nicer(bool),
    Niced(Result<(), String>),
    OpenLocation,
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
    /// A right click on a row: select it and open its menu at the pointer.
    OpenMenu(menus::MenuFor),
    CloseMenu,
    CrashesLoaded(Box<crashes_view::CrashesData>),
    /// Open one incident's details, or close them with `None`.
    CrashSelect(Option<i64>),
    CrashRange(usize),
    /// A menu row chosen: close the menu, then do what its button does.
    MenuChose(Box<Message>),
    /// Open file location for a program named outright — a menu's, read
    /// when it opened, rather than the Details pane's last reading.
    OpenLocationOf(std::path::PathBuf),
    Copy(String),
    Resized(Size),
    Recorded(recorded_view::Msg),
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
    gpu_scan: GpuScan,
    /// The NVIDIA card's last answer, and when it was asked.
    pub nvidia: Option<Result<hyprforge_procman::nvidia::Reading, String>>,
    nvidia_asked: Option<std::time::Instant>,
    /// The card refused in a way that lasts until a restart; asking again
    /// would only add a line to the kernel log each time.
    nvidia_given_up: bool,
    /// When the drives' temperatures were last read, and what they were.
    drives_read: Option<std::time::Instant>,
    drive_celsius: HashMap<String, f32>,
    last_gpu: HashMap<i32, hyprforge_procman::gpu::DrmUse>,
    /// Why there are no apps, when Hyprland could not be asked.
    pub no_windows: Option<String>,
    pub paused: bool,
    pub interval: Duration,
    pub history: History,
    pub resource: Resource,
    /// App history, or why its file couldn't be read (then nothing is
    /// recorded, so nothing is written over it).
    pub app_history: Result<hyprforge_procman::app_history::History, String>,
    pub history_days: usize,
    pub crashes: Option<crashes_view::CrashesData>,
    /// The open incident, by its journal timestamp.
    pub crash_selected: Option<i64>,
    pub crash_days: usize,
    pub recorded: recorded_view::RecordedTab,
    /// Installed apps, for showing a window class by its app's name.
    pub apps_db: Option<hyprforge_mime::MimeDb>,
    history_saved: std::time::Instant,
    /// The btop/atop-verbose view: the Overview tab and the extra columns.
    pub advanced: bool,
    /// Machine-wide rates the Overview shows.
    pub overview: Overview,
    /// The last reading's running and blocked counts.
    pub counters: hyprforge_procman::proc::Counters,
    /// A minute of each process's CPU, for the Overview's sparklines.
    pub process_history: HashMap<Id, hyprforge_procman::history::Series>,
    pub services: ServicesTab,
    pub details: DetailsTab,
    pub startup: Option<StartupData>,
    pub sessions: Result<Vec<Session>, String>,
    /// A sign-out waiting on "are you sure".
    pub sign_out: Option<Session>,
    /// When the machine booted, seconds since the epoch.
    pub boot: u64,
    pub machine: Machine,
    pub disk_rates: Vec<(String, DiskRate)>,
    pub net_rates: Vec<(String, NetRate)>,
    /// The whole machine's figures, for the status bar and Performance.
    pub totals: Totals,
    /// This window's own process and everything it runs under — never
    /// frozen, and named in an End confirmation.
    pub critical: std::collections::HashSet<i32>,
    /// The last reading's `port:`/`file:` search covered every user's
    /// processes, through the helper — what the Details note says.
    pub lookup_everyone: bool,
    /// The right-click menu, while one is open.
    pub menu: Option<menus::OpenMenu>,
    /// The window's size, which a menu is kept inside.
    pub window_size: Size,
    reading: bool,
}

/// What two readings say about the whole machine, for the Overview.
#[derive(Debug, Clone, Copy, Default)]
pub struct Overview {
    /// Where CPU time went, in `proc::CPU_STATES` order.
    pub split: [f32; 8],
    pub context_switches: f64,
    pub interrupts: f64,
    pub forks: f64,
    pub faults: f64,
    pub major_faults: f64,
    /// Bytes a second swapped in and out.
    pub swap_in: f64,
    pub swap_out: f64,
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

/// The GPU scan's state, kept between readings on the reader's thread.
type GpuScan = std::sync::Arc<std::sync::Mutex<hyprforge_procman::gpu::Scan>>;

pub fn search_id() -> WidgetId {
    WidgetId::new("procman-search")
}

fn read(fs: ProcFs, helper: Option<zbus::Connection>, lookup: Option<inspect::Lookup>, scan: GpuScan, ask_nvidia: bool, drive_temperatures: bool) -> Task<Message> {
    Task::perform(
        async move {
            let (mut snapshot, machine, windows, gpu, nvidia) = tokio::task::spawn_blocking(move || {
                let snapshot = fs.snapshot().map_err(|e| format!("couldn't read /proc: {e}"))?;
                let pids: Vec<i32> = snapshot.procs.iter().filter(|p| !p.kernel).map(|p| p.pid).collect();
                let gpu = scan.lock().map(|mut s| s.read(std::path::Path::new("/proc"), &pids)).unwrap_or_default();
                let mut sys = SysFs::default();
                sys.drive_temperatures = drive_temperatures;
                let machine = sys.read();
                // Only a card that is already awake is asked: asking one
                // that sleeps would wake it.
                let awake = machine.gpus.iter().any(|g| g.driver == "nvidia" && g.power.as_deref() == Some("active"));
                let nvidia = (ask_nvidia && awake).then(hyprforge_procman::nvidia::query);
                Ok::<_, String>((snapshot, machine, windows::read(), gpu, nvidia))
            })
            .await
            .map_err(|e| e.to_string())??;
            let at = std::time::Instant::now();
            let mut everything_lapsed = false;
            let mut held = None;
            if let Some(system) = helper {
                // Bounded: a refresh that never finished would be the last
                // one, since a new one isn't started while one runs. A
                // helper that doesn't answer in time leaves this reading
                // without what it adds — dashes — and isn't a lapse.
                let filled = tokio::time::timeout(bound::QUICK, fill_in(&system, &mut snapshot)).await;
                everything_lapsed = filled == Ok(false);
                if let (Ok(true), Some(lookup)) = (filled, lookup) {
                    held = tokio::time::timeout(bound::QUICK, holders(&system, &lookup)).await.ok().flatten().map(|found| (lookup, found));
                }
            }
            Ok(Box::new(Reading { snapshot, gpu, machine, windows, at, nvidia, everything_lapsed, held }))
        },
        Message::Read,
    )
}

/// The helper's answer to a `port:`/`file:` search, over every user's
/// processes; `None` when it couldn't give one (an older helper, or a
/// path it won't take), so the window says it looked at yours alone.
async fn holders(system: &zbus::Connection, lookup: &inspect::Lookup) -> Option<Vec<(i32, u64)>> {
    let proxy = helper::ProcManProxy::new(system).await.ok()?;
    match lookup {
        inspect::Lookup::Port(port) => proxy.port_holders(*port, false).await.ok(),
        inspect::Lookup::File(path) => proxy.file_holders(path.to_str()?, false).await.ok(),
    }
}

/// Asks the helper for what this user could not read, without a prompt.
/// `false` when polkit's yes has lapsed.
async fn fill_in(system: &zbus::Connection, snapshot: &mut Snapshot) -> bool {
    let Ok(proxy) = helper::ProcManProxy::new(system).await else { return false };
    // Network for every process, this user's own included: only the
    // helper counts it. A helper that can't (built without it, or older
    // than this window) leaves the column at a dash; it isn't a lapse.
    let everyone: Vec<(i32, u64)> = snapshot.procs.iter().filter(|p| !p.kernel).map(|p| (p.pid, p.start)).collect();
    if let Ok(net) = proxy.net_usage(everyone, false).await {
        let by_id: HashMap<(i32, u64), (u64, u64)> = net.into_iter().map(|(pid, start, rx, tx)| ((pid, start), (rx, tx))).collect();
        for p in &mut snapshot.procs {
            if let Some(&(rx, tx)) = by_id.get(&(p.pid, p.start)) {
                p.net = Some(hyprforge_procman::proc::NetIo { rx_bytes: rx, tx_bytes: tx, ..Default::default() });
            }
        }
    }
    let wanted: Vec<(i32, u64)> = snapshot.procs.iter().filter(|p| p.io.is_none() && !p.kernel).map(|p| (p.pid, p.start)).collect();
    if wanted.is_empty() {
        return true;
    }
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
    bound::within(bound::PROMPT, "the privileged helper", async {
        let system = zbus::Connection::system().await.map_err(|e| format!("couldn't reach the system bus: {e}"))?;
        let proxy = helper::ProcManProxy::new(&system).await.map_err(|e| e.to_string())?;
        proxy.usage(Vec::new(), true).await.map_err(helper_error)?;
        Ok(system)
    })
    .await
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
            gpu_scan: GpuScan::default(),
            nvidia: None,
            nvidia_asked: None,
            nvidia_given_up: false,
            drives_read: None,
            drive_celsius: HashMap::new(),
            last_gpu: HashMap::new(),
            no_windows: None,
            paused: false,
            interval: INTERVALS[1],
            history: History::default(),
            resource: Resource::Cpu,
            app_history: hyprforge_procman::app_history::load(&hyprforge_procman::app_history::path()),
            history_days: 1,
            crashes: None,
            crash_selected: None,
            crash_days: 7,
            recorded: recorded_view::RecordedTab::default(),
            apps_db: None,
            history_saved: std::time::Instant::now(),
            advanced: false,
            overview: Overview::default(),
            counters: Default::default(),
            process_history: HashMap::new(),
            services: ServicesTab::default(),
            details: DetailsTab { tree: true, ..DetailsTab::default() },
            startup: None,
            sessions: Ok(Vec::new()),
            sign_out: None,
            boot: 0,
            machine: Machine::default(),
            disk_rates: Vec::new(),
            net_rates: Vec::new(),
            totals: Totals::default(),
            critical: Default::default(),
            lookup_everyone: false,
            menu: None,
            window_size: Size::new(1100.0, 720.0),
            reading: true,
        };
        let scan = app.gpu_scan.clone();
        (app, read(fs, None, None, scan, false, false))
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

    /// Whether this refresh reads the drives' temperatures: only where
    /// they are shown, and every ten seconds at most — each is a command
    /// sent to the drive (`SysFs::drive_temperatures`).
    fn ask_drive_temperatures(&mut self) -> bool {
        let shown = self.tab == Tab::Performance || self.tab == Tab::Overview;
        let due = self.drives_read.is_none_or(|t| t.elapsed() >= Duration::from_secs(10));
        if shown && due {
            self.drives_read = Some(std::time::Instant::now());
        }
        shown && due
    }

    /// Whether this refresh asks the NVIDIA card: only while Performance
    /// is open, at most every three seconds — see `nvidia.rs`.
    fn ask_nvidia(&mut self) -> bool {
        let due = self.nvidia_asked.is_none_or(|t| t.elapsed() >= Duration::from_secs(3));
        let ask = self.tab == Tab::Performance && due && !self.nvidia_given_up;
        if ask {
            self.nvidia_asked = Some(std::time::Instant::now());
        }
        ask
    }

    pub fn everything_open(&self) -> bool {
        matches!(self.everything, Everything::Open(_))
    }

    /// The `port:` or `file:` search the box holds, if it holds one.
    fn lookup(&self) -> Option<inspect::Lookup> {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
        inspect::Lookup::parse(&self.view.query, &home)
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
        Subscription::batch([
            tick,
            keyboard::listen().filter_map(|event| hyprforge_ui::keys::key_press(&event).map(Message::Key)),
            hyprforge_ui::widgets::pointer::track(),
            window::resize_events().map(|(_, size)| Message::Resized(size)),
        ])
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
                            return Task::batch([list, read(self.fs.clone(), self.helper(), self.lookup(), self.gpu_scan.clone(), self.ask_nvidia(), self.ask_drive_temperatures())]);
                        }
                        return list;
                    }
                }
                // A read still running is not doubled up behind: a slow
                // machine gets fewer refreshes, not a queue of them.
                if !self.reading {
                    self.reading = true;
                    return read(self.fs.clone(), self.helper(), self.lookup(), self.gpu_scan.clone(), self.ask_nvidia(), self.ask_drive_temperatures());
                }
            }
            Message::Read(result) => {
                self.reading = false;
                match result {
                    Ok(reading) => self.take(*reading),
                    Err(e) => self.notice = Some(e),
                }
            }
            Message::Tree(on) => self.details.tree = on,
            Message::HistoryRange(days) => self.history_days = days,
            Message::Advanced(on) => {
                self.advanced = on;
                // `status` is one more file per process: read only while
                // something shows what it holds.
                self.fs.verbose = on;
                if !on && self.tab == Tab::Overview {
                    self.tab = Tab::Processes;
                }
                if on {
                    self.tab = Tab::Overview;
                }
            }
            Message::StartupLoaded(data) => self.startup = Some(*data),
            Message::Autostart(id, on) => {
                let Some(entry) = self.startup.as_ref().and_then(|d| d.autostarts.iter().find(|a| a.id == id)).cloned() else {
                    return Task::none();
                };
                let result = startup::set_autostart(&entry, &autostart_dir(), on).map_err(|e| e.to_string());
                return self.update(Message::StartupChanged(result));
            }
            Message::UnitAtLogin(name, on) => {
                let action = if on { Action::Enable } else { Action::Disable };
                return Task::perform(async move { Dbus.act(Scope::User, name, action).await }, Message::StartupChanged);
            }
            Message::StartupChanged(result) => {
                if let Err(e) = result {
                    self.notice = Some(format!("Couldn't change that: {e}."));
                }
                return Task::perform(load_startup(), |d| Message::StartupLoaded(Box::new(d)));
            }
            Message::OpenSessionSettings => {
                let mut command = std::process::Command::new("hyprforge-settings");
                command.args(["--screen", "session"]);
                if let Err(e) = command.spawn() {
                    self.notice = Some(format!("Couldn't open Settings: {e}."));
                }
            }
            Message::SessionsLoaded(result) => self.sessions = result,
            Message::Session(id, action) => {
                if action == SessionAction::SignOut {
                    self.sign_out = self.sessions.as_ref().ok().and_then(|s| s.iter().find(|s| s.id == id)).cloned();
                    return Task::none();
                }
                return Task::perform(sessions::act(id, action), Message::SessionDone);
            }
            Message::SignOutConfirmed => {
                if let Some(s) = self.sign_out.take() {
                    return Task::perform(sessions::act(s.id, SessionAction::SignOut), Message::SessionDone);
                }
            }
            Message::SessionDone(result) => {
                if let Err(e) = result {
                    self.notice = Some(format!("That didn't work: {e}."));
                }
                return Task::perform(sessions::list(), Message::SessionsLoaded);
            }
            Message::EndTree => {
                if let Some(mut c) = self.selected.and_then(|id| self.confirmation(id, How::Ask)) {
                    c.tree = true;
                    // Anything in the tree the session runs under makes
                    // the whole tree critical.
                    c.critical = model::tree_of(&self.table, c.id).iter().any(|i| self.critical.contains(&i.pid));
                    self.confirm = Some(c);
                }
            }
            Message::Nicer(lower) => {
                let Some(id) = self.selected else { return Task::none() };
                let Some(entry) = model::entries(&self.table).find(|e| e.id == id).cloned() else { return Task::none() };
                let nice = (entry.nice + if lower { 5 } else { -5 }).clamp(-20, 19);
                // Lowering one's own needs nothing; anything else goes
                // through the helper, which asks.
                if lower && entry.own {
                    let result = signal::renice(id, nice).map_err(|e| e.to_string());
                    return self.update(Message::Niced(result));
                }
                return Task::perform(
                    bound::within(bound::PROMPT, "the privileged helper", async move {
                        let system = zbus::Connection::system().await.map_err(|e| e.to_string())?;
                        let proxy = helper::ProcManProxy::new(&system).await.map_err(|e| e.to_string())?;
                        proxy.set_nice(id.pid, id.start, nice, true).await.map_err(helper_error)
                    }),
                    Message::Niced,
                );
            }
            Message::Niced(result) => match result {
                Ok(()) => {
                    if !self.reading {
                        self.reading = true;
                        return read(self.fs.clone(), self.helper(), self.lookup(), self.gpu_scan.clone(), self.ask_nvidia(), self.ask_drive_temperatures());
                    }
                }
                Err(e) => self.notice = Some(format!("Couldn't change its priority: {e}.")),
            },
            Message::OpenMenu(on) => {
                let select = match &on {
                    menus::MenuFor::Process(id) => Message::Select(*id),
                    menus::MenuFor::Service(name) => Message::SelectService(name.clone()),
                };
                let task = self.update(select);
                self.menu = Some(menus::OpenMenu { on, at: hyprforge_ui::widgets::pointer::last() });
                return task;
            }
            Message::CloseMenu => self.menu = None,
            Message::CrashesLoaded(data) => self.crashes = Some(*data),
            Message::CrashSelect(at) => self.crash_selected = at,
            Message::CrashRange(days) => self.crash_days = days,
            Message::MenuChose(message) => {
                self.menu = None;
                return self.update(*message);
            }
            Message::OpenLocationOf(exe) => show_in_folder(exe),
            Message::Copy(text) => return iced::clipboard::write(text),
            Message::Resized(size) => self.window_size = size,
            Message::OpenLocation => {
                if let Some(exe) = self.details.properties.exe.clone() {
                    show_in_folder(exe);
                }
            }
            Message::Tab(tab) => {
                self.tab = tab;
                // The app list is read once, when history first needs names.
                if tab == Tab::History && self.apps_db.is_none() {
                    self.apps_db = Some(hyprforge_mime::MimeDb::load());
                }
                match tab {
                    Tab::Services => return self.load_services(),
                    Tab::Startup => return Task::perform(load_startup(), |d| Message::StartupLoaded(Box::new(d))),
                    Tab::Users => return Task::perform(sessions::list(), Message::SessionsLoaded),
                    Tab::Recorded => return self.recorded_opened(),
                    Tab::Crashes => return Task::perform(crashes_view::load(), |d| Message::CrashesLoaded(Box::new(d))),
                    _ => {}
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
            Message::Query(q) => {
                // A lookup is answered at the next reading; until then a
                // half-typed `port:80` must not filter on a stale set.
                if self.view.query.trim() != q.trim() {
                    self.view.only = None;
                }
                self.view.query = q;
            }
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
            Message::Confirmed if self.confirm.as_ref().is_some_and(|c| c.tree) => {
                let c = self.confirm.take().expect("checked");
                let mut refused = 0;
                for id in model::tree_of(&self.table, c.id) {
                    match signal::end(id, How::Ask) {
                        Ok(()) | Err(signal::Refused::Gone) => {}
                        Err(_) => refused += 1,
                    }
                }
                self.notice = (refused > 0).then(|| format!("{refused} of the processes under {} weren't yours to end.", c.name));
                if !self.reading {
                    self.reading = true;
                    return read(self.fs.clone(), self.helper(), self.lookup(), self.gpu_scan.clone(), self.ask_nvidia(), self.ask_drive_temperatures());
                }
            }
            Message::Confirmed => {
                if let Some(c) = self.confirm.take() {
                    match signal::end(c.id, c.how) {
                        Ok(()) => {
                            self.notice = None;
                            // The next reading shows it gone, sooner than
                            // the next tick would.
                            if !self.reading {
                                self.reading = true;
                                return read(self.fs.clone(), self.helper(), self.lookup(), self.gpu_scan.clone(), self.ask_nvidia(), self.ask_drive_temperatures());
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
                        return read(self.fs.clone(), self.helper(), self.lookup(), self.gpu_scan.clone(), self.ask_nvidia(), self.ask_drive_temperatures());
                    }
                }
            }
            Message::Recorded(msg) => return self.recorded_update(msg),
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
                        bound::within(bound::PROMPT, "the privileged helper", async move {
                            let system = zbus::Connection::system().await.map_err(|e| e.to_string())?;
                            let proxy = helper::ProcManProxy::new(&system).await.map_err(|e| e.to_string())?;
                            proxy.signal(c.id.pid, c.id.start, signal, true).await.map_err(helper_error)
                        }),
                        Message::EndedAsAdmin,
                    );
                }
            }
            Message::EndedAsAdmin(result) => match result {
                Ok(()) => {
                    self.notice = None;
                    if !self.reading {
                        self.reading = true;
                        return read(self.fs.clone(), self.helper(), self.lookup(), self.gpu_scan.clone(), self.ask_nvidia(), self.ask_drive_temperatures());
                    }
                }
                Err(e) => self.notice = Some(format!("Couldn't end it: {e}.")),
            },
            Message::Key(press) => return self.key(press),
            Message::Dismiss => {
                self.notice = None;
                self.admin_offer = None;
                self.sign_out = None;
            }
        }
        Task::none()
    }

    fn key(&mut self, press: KeyPress) -> Task<Message> {
        let ctrl = press.mods.ctrl;
        match press.key {
            Key::Escape if self.menu.is_some() => self.menu = None,
            Key::Escape if self.confirm.is_some() => self.confirm = None,
            Key::Escape if self.services.confirm.is_some() => self.services.confirm = None,
            Key::Escape if self.sign_out.is_some() => self.sign_out = None,
            Key::Enter if self.confirm.is_some() => return self.update(Message::Confirmed),
            Key::Char('f') if ctrl => return iced::widget::operation::focus(search_id()),
            Key::Delete if self.selected.is_some() && self.confirm.is_none() => {
                return self.update(Message::Ask(if ctrl { How::Force } else { How::Ask }));
            }
            Key::Char('1') if ctrl => self.tab = Tab::Processes,
            Key::Char('2') if ctrl => self.tab = Tab::Performance,
            Key::Char('3') if ctrl => return self.update(Message::Tab(Tab::Startup)),
            Key::Char('4') if ctrl => return self.update(Message::Tab(Tab::Users)),
            Key::Char('5') if ctrl => self.tab = Tab::Details,
            Key::Char('6') if ctrl => return self.update(Message::Tab(Tab::Services)),
            Key::Char('7') if ctrl => return self.update(Message::Tab(Tab::Crashes)),
            Key::Char('p') if ctrl => self.paused = !self.paused,
            Key::Char('.') if ctrl => return self.update(Message::Advanced(!self.advanced)),
            _ => {}
        }
        Task::none()
    }

    /// What "end this?" says about `id`, or nothing if it is gone.
    fn confirmation(&self, id: Id, how: How) -> Option<Confirm> {
        if let Some(app) = self.table.apps.iter().find(|a| a.lead.id == id) {
            let critical = self.critical.contains(&id.pid);
            return Some(Confirm { id, name: app.lead.name.clone(), command: app.lead.command.clone(), how, windows: app.windows, critical, tree: false });
        }
        let entry =
            self.table.apps.iter().flat_map(|a| a.members.iter()).chain(self.table.background.iter()).find(|e| e.id == id)?;
        let critical = self.critical.contains(&id.pid);
        Some(Confirm { id, name: entry.name.clone(), command: entry.command.clone(), how, windows: 0, critical, tree: false })
    }

    fn take(&mut self, reading: Reading) {
        let Reading { snapshot, gpu, mut machine, windows, at, nvidia, everything_lapsed, held } = reading;
        // Temperatures read now and then are kept between reads.
        for d in &mut machine.disks {
            match d.celsius {
                Some(c) => {
                    self.drive_celsius.insert(d.name.clone(), c);
                }
                None => d.celsius = self.drive_celsius.get(&d.name).copied(),
            }
        }
        if let Some(answer) = nvidia {
            if let Err(why) = &answer {
                self.nvidia_given_up = hyprforge_procman::nvidia::lasts_until_restart(why);
            }
            self.nvidia = Some(answer);
        }
        // A card that went to sleep has no figures; its old ones would lie.
        if machine.gpus.iter().any(|g| g.driver == "nvidia" && g.power.as_deref() == Some("suspended")) {
            self.nvidia = None;
        }
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
            // Every card has a graph, even one with nothing to put in it.
            let series = self.history.gpus.entry(gpu.card.clone()).or_default();
            let busy = match gpu.driver.as_str() {
                "nvidia" => self.nvidia.as_ref().and_then(|n| n.as_ref().ok()).and_then(|n| n.busy).map(|b| b / 100.0),
                _ => gpu.busy_percent.map(|b| f32::from(b) / 100.0),
            };
            if let Some(busy) = busy {
                series.push(busy);
            }
        }
        if let Some(b) = &machine.battery {
            self.history.battery.push(f32::from(b.percent) / 100.0);
        }
        let paging_before = self.machine.paging;
        self.machine = machine;
        if everything_lapsed && matches!(self.everything, Everything::Open(_)) {
            self.everything = Everything::Locked;
            self.notice = Some("Show everything has ended: administrator access timed out.".into());
        }
        let mut usage = sample::Use::default();
        if let Some(before) = &self.last {
            usage.cpu = sample::shares(before, &snapshot);
            usage.disk = sample::disk_rates(before, &snapshot, seconds);
            usage.net = sample::net_rates(before, &snapshot, seconds);
            if self.advanced && seconds > 0.0 {
                usage.detail = sample::details(before, &snapshot, seconds);
                let rate = |now: u64, then: u64| now.saturating_sub(then) as f64 / seconds;
                let (c, b) = (snapshot.counters, before.counters);
                let (pg, pb) = (self.machine.paging, paging_before);
                // Pages are 4 KiB on every machine this targets.
                self.overview = Overview {
                    split: sample::split(before.cpu, snapshot.cpu),
                    context_switches: rate(c.context_switches, b.context_switches),
                    interrupts: rate(c.interrupts, b.interrupts),
                    forks: rate(c.forks, b.forks),
                    faults: rate(pg.faults, pb.faults),
                    major_faults: rate(pg.major_faults, pb.major_faults),
                    swap_in: rate(pg.swap_in, pb.swap_in) * 4096.0,
                    swap_out: rate(pg.swap_out, pb.swap_out) * 4096.0,
                };
            }
            let ids: HashMap<i32, Id> = snapshot.procs.iter().map(|p| (p.pid, Id { pid: p.pid, start: p.start })).collect();
            usage.gpu = hyprforge_procman::gpu::shares(&self.last_gpu, &gpu, &ids, seconds);
            // A card's maker, from its driver; NVIDIA's own device files
            // name no PCI address.
            let maker = |card: &str| hyprforge_procman::gpu::maker(card, &self.machine.gpus).to_string();
            usage.gpu_cards = gpu
                .iter()
                .filter_map(|(pid, u)| {
                    let mut names: Vec<String> = u.cards.iter().map(|c| maker(c)).collect();
                    names.sort();
                    names.dedup();
                    Some((*ids.get(pid)?, names))
                })
                .collect();
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
        // App history: this reading's share of each app's day.
        if let (Ok(history), true) = (&mut self.app_history, seconds > 0.0) {
            let samples: Vec<hyprforge_procman::app_history::Sample> = self
                .table
                .apps
                .iter()
                .map(|a| hyprforge_procman::app_history::Sample {
                    class: if a.class.is_empty() { a.lead.name.clone() } else { a.class.clone() },
                    cpu: a.cpu(),
                    disk: a.disk(),
                    gpu: a.gpu(),
                    memory: a.private_bytes(),
                })
                .collect();
            let day = chrono::Local::now().format("%Y-%m-%d").to_string();
            history.record(&day, &samples, seconds, self.history.cores.len().max(1));
            // Written a minute at a time: at most a minute is lost if
            // the window is closed, and the disk is not touched each second.
            if self.history_saved.elapsed() >= Duration::from_secs(60) {
                self.history_saved = std::time::Instant::now();
                if let Err(e) = hyprforge_procman::app_history::save(&hyprforge_procman::app_history::path(), history) {
                    self.notice = Some(format!("App history couldn't be saved: {e}"));
                }
            }
        }
        // A minute of CPU per process, for the Overview's sparklines; a
        // process that ended takes its history with it.
        if self.advanced {
            let live: std::collections::HashSet<Id> = model::entries(&self.table).map(|e| e.id).collect();
            self.process_history.retain(|id, _| live.contains(id));
            for e in model::entries(&self.table) {
                if let Some(cpu) = e.cpu {
                    self.process_history.entry(e.id).or_default().push(cpu / 100.0);
                }
            }
        }
        // This process and what it runs under, plus the compositor by
        // name, in case this window was started some other way.
        self.critical = model::ancestry(&snapshot, std::process::id() as i32);
        self.critical.extend(snapshot.procs.iter().filter(|p| p.name == "Hyprland" && p.uid == self.me).map(|p| p.pid));
        // A `port:`/`file:` search is answered again with every reading,
        // since what holds a port changes. What this user can read is
        // searched here; with Show everything on, the helper searched the
        // rest — counted only if it answered the search the box still
        // holds, and only for processes that are still the ones it saw.
        self.lookup_everyone = false;
        self.view.only = self.lookup().map(|lookup| {
            let pids: Vec<i32> = snapshot.procs.iter().filter(|p| !p.kernel).map(|p| p.pid).collect();
            let mut found = inspect::who(std::path::Path::new("/proc"), &pids, &lookup);
            if let Some((asked, held)) = &held {
                if *asked == lookup {
                    self.lookup_everyone = true;
                    let seen: HashSet<(i32, u64)> = snapshot.procs.iter().map(|p| (p.pid, p.start)).collect();
                    found.extend(held.iter().filter(|id| seen.contains(id)).map(|&(pid, _)| pid));
                }
            }
            found
        });
        // What the Details tab shows of the selected process.
        self.details.properties = match self.selected {
            Some(id) => {
                let root = std::path::Path::new("/proc");
                Properties {
                    id: Some(id),
                    exe: inspect::exe(root, id.pid),
                    cwd: inspect::cwd(root, id.pid),
                    unit: inspect::unit(root, id.pid),
                    open_files: inspect::open(root, id.pid).map(|o| o.len()),
                    sockets: inspect::sockets(root, id.pid),
                }
            }
            None => Properties::default(),
        };
        // A selection whose process ended goes with it.
        if let Some(id) = self.selected {
            if !snapshot.procs.iter().any(|p| p.pid == id.pid && p.start == id.start) {
                self.selected = None;
            }
        }
        self.last_gpu = gpu;
        self.boot = snapshot.boot;
        self.counters = snapshot.counters;
        self.last = Some(snapshot);
    }
}

/// "Open file location": the program in Files (or whatever answers
/// `org.freedesktop.FileManager1`), selected. Off the UI thread.
fn show_in_folder(path: std::path::PathBuf) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(r) => r,
            Err(_) => return,
        };
        runtime.block_on(async move {
            let Ok(session) = zbus::Connection::session().await else { return };
            let uri = format!("file://{}", path.display());
            let body = (vec![uri.as_str()], "");
            let call = session.call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                "ShowItems",
                &body,
            );
            if let Err(e) = tokio::time::timeout(Duration::from_secs(5), call).await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.to_string())) {
                tracing::warn!("couldn't show the program's folder: {e}");
            }
        });
    });
}

/// Focuses the window of process `pid` — Files' `hl.dsp.focus` call,
/// checked live there. Through `hyprctl dispatch`: `hyprctl eval` only
/// builds the dispatcher and answers "ok" without running it, which is
/// how this first shipped (found 2026-10-10, trying to maximize this
/// window for a screenshot). Off the UI thread and bounded; a refusal leaves
/// focus where it was, which is all a person would see either way.
fn switch_to(pid: i32) {
    let lua = format!("hl.dsp.focus({{ window = \"pid:{pid}\" }})");
    std::thread::spawn(move || {
        let mut command = std::process::Command::new("hyprctl");
        command.args(["dispatch", &lua]);
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

#[cfg(test)]
mod render {
    //! Draws every tab, from real readings of this machine, to
    //! `$SHOTS/<tab>.png`. Ignored: it renders, it asserts nothing — and
    //! it shows this machine's own processes, so its output is for
    //! looking at, not for publishing.

    use super::*;
    use iced::{Color, Rectangle};
    use iced_runtime::core::renderer::Style;
    use iced_runtime::user_interface::{Cache, UserInterface};

    fn reading(fs: &ProcFs, scan: &GpuScan) -> Reading {
        let snapshot = fs.snapshot().unwrap();
        let pids: Vec<i32> = snapshot.procs.iter().filter(|p| !p.kernel).map(|p| p.pid).collect();
        let gpu = scan.lock().unwrap().read(std::path::Path::new("/proc"), &pids);
        // The NVIDIA card is not asked here: each failed ask is a kernel
        // log line, and a dozen readings would be a dozen lines.
        Reading { snapshot, gpu, machine: { let mut sys = SysFs::default(); sys.drive_temperatures = true; sys.read() }, windows: windows::read(), at: std::time::Instant::now(), nvidia: None, everything_lapsed: false, held: None }
    }

    fn draw(app: &App, name: &str, dir: &std::path::Path) {
        draw_sized(app, name, dir, 800.0);
    }

    fn draw_sized(app: &App, name: &str, dir: &std::path::Path, h: f32) {
        let (w, scale) = (1280.0f32, 1.6f32);
        let (pw, ph) = ((w * scale) as u32, (h * scale) as u32);
        let theme = hyprforge_ui::theme::active();
        let font = iced::Font::with_name(Box::leak(theme.font.clone().into_boxed_str()));
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(font, iced::Pixels(theme.font_size)));
        let size = Size::new(w, h);
        let mut ui = UserInterface::build(app.view(), size, Cache::default(), &mut renderer);
        ui.draw(&mut renderer, &app.theme(), &Style { text_color: Color::WHITE }, iced::mouse::Cursor::Unavailable);
        let iced::Renderer::Secondary(mut skia) = renderer else { unreachable!() };
        let mut pixmap = tiny_skia::Pixmap::new(pw, ph).unwrap();
        let mut mask = tiny_skia::Mask::new(pw, ph).unwrap();
        skia.draw(
            &mut pixmap.as_mut(),
            &mut mask,
            &iced_tiny_skia::graphics::Viewport::with_physical_size(Size::new(pw, ph), scale),
            &[Rectangle::with_size(size)],
            Color::BLACK,
        );
        // Drawn text borrows from the tree: only now may it go.
        drop(ui);
        // BGRA for Wayland; a PNG wants RGBA.
        for px in pixmap.data_mut().chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        pixmap.save_png(dir.join(format!("{name}.png"))).unwrap();
    }

    #[test]
    #[ignore = "draws this machine's processes to $SHOTS"]
    fn render_every_tab() {
        // A tool, not a check (the suite's `manual` tier): with nowhere
        // to draw it skips rather than fails.
        let Ok(dir) = std::env::var("SHOTS") else {
            eprintln!("HYPRFORGE-SKIP: render_every_tab draws only with $SHOTS set");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        hyprforge_ui::theme::init(hyprforge_appearance::look::resolve());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (mut app, _) = App::new();
        app.nvidia = Some(hyprforge_procman::nvidia::query());
        // History in memory only: a test never writes the user's file.
        app.app_history = Ok(Default::default());
        app.history_saved = std::time::Instant::now() + Duration::from_secs(3600);
        let _ = app.update(Message::Advanced(true));
        let mut fs = ProcFs::default();
        fs.verbose = true;
        // A dozen readings half a second apart: the graphs get a line,
        // and every share has its second reading.
        for _ in 0..12 {
            let r = reading(&fs, &app.gpu_scan);
            app.take(r);
            std::thread::sleep(Duration::from_millis(500));
        }
        draw_sized(&app, "overview", &dir, 1420.0);
        app.tab = Tab::Processes;
        draw(&app, "processes", &dir);
        // A right click on the first app, as if the pointer were half
        // way down its row — the menu, placed and drawn.
        if let Some(lead) = app.table.apps.first().map(|a| a.lead.id) {
            let _ = app.update(Message::OpenMenu(menus::MenuFor::Process(lead)));
            app.window_size = Size::new(1280.0, 800.0);
            if let Some(menu) = app.menu.as_mut() {
                menu.at = (420.0, 140.0);
            }
            draw(&app, "processes-menu", &dir);
            let _ = app.update(Message::CloseMenu);
        }

        app.tab = Tab::Performance;
        draw(&app, "performance", &dir);
        if let Some(disk) = app.machine.disks.first() {
            app.resource = Resource::Disk(disk.name.clone());
            draw(&app, "performance-disk", &dir);
        }
        if let Some(gpu) = app.machine.gpus.iter().find(|g| g.driver == "nvidia").or(app.machine.gpus.first()) {
            app.resource = Resource::Gpu(gpu.card.clone());
            draw(&app, "performance-gpu", &dir);
        }

        app.tab = Tab::History;
        app.apps_db = Some(hyprforge_mime::MimeDb::load());
        draw(&app, "history", &dir);

        app.tab = Tab::Startup;
        app.startup = Some(runtime.block_on(load_startup()));
        draw(&app, "startup", &dir);

        // This machine's own journal, with the newest crash opened.
        app.tab = Tab::Crashes;
        app.crash_days = 30;
        let crashes = runtime.block_on(crashes_view::load());
        app.crash_selected = crashes.incidents.as_ref().ok().and_then(|f| f.list.iter().find(|i| matches!(i, hyprforge_procman::incidents::Incident::Crash(_)))).map(|i| i.at());
        app.crashes = Some(crashes);
        draw_sized(&app, "crashes", &dir, 1100.0);

        app.tab = Tab::Users;
        app.sessions = runtime.block_on(sessions::list());
        draw(&app, "users", &dir);

        // Details on the compositor, the process with the most to show.
        app.tab = Tab::Details;
        app.selected = app.table.apps.first().map(|a| a.lead.id).or_else(|| app.table.background.first().map(|e| e.id));
        let r = reading(&fs, &app.gpu_scan);
        app.take(r);
        draw(&app, "details", &dir);

        app.tab = Tab::Services;
        app.services.units.insert(Scope::System, runtime.block_on(Dbus.list(Scope::System)).unwrap_or_default());
        app.services.cgroups = services::cgroups(std::path::Path::new("/sys/fs/cgroup"));
        app.service_counters();
        std::thread::sleep(Duration::from_millis(500));
        app.service_counters();
        app.services.selected = Some("NetworkManager.service".into());
        app.services.journal = services::journal(Scope::System, "NetworkManager.service", 30);
        draw(&app, "services", &dir);
    }

    #[test]
    #[ignore = "draws the journal at $JOURNAL to $SHOTS"]
    fn render_recorded() {
        let (Ok(dir), Ok(journal)) = (std::env::var("SHOTS"), std::env::var("JOURNAL")) else {
            eprintln!("HYPRFORGE-SKIP: render_recorded draws only with $SHOTS and $JOURNAL set");
            return;
        };
        let (dir, journal) = (std::path::PathBuf::from(dir), std::path::PathBuf::from(journal));
        hyprforge_ui::theme::init(hyprforge_appearance::look::resolve());
        let (mut app, _) = App::new();
        app.tab = Tab::Recorded;
        app.recorded = recorded_view::RecordedTab::read_now(&journal, 0.6);
        draw_sized(&app, "recorded", &dir, 1500.0);
    }
}

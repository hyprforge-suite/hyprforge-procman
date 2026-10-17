//! Limits per app: how much memory and CPU one app may take, enforced
//! by the kernel through systemd's cgroup properties.
//!
//! Windows has nothing like it. Linux has had it for years, and no
//! desktop tool offers it, because two facts make the obvious version
//! wrong — both checked on this machine:
//!
//! - **Many apps have no unit of their own.** Anything Hyprland launched
//!   runs in the compositor's own unit, `wayland-wm@hyprland.desktop.service`
//!   — Ghostty, Steam, waybar and Hyprland itself, here. A limit set on
//!   that unit would cap the compositor. So an app that shares its unit
//!   is first moved into a scope of its own (`StartTransientUnit` with
//!   its pids — tried on a throwaway process: it moved, and the limit was
//!   on at once), and the limit goes on that. [`plan`] decides which, and
//!   refuses the app the session itself runs on.
//! - **Not every controller reaches the kernel.** A user's manager is
//!   delegated `cpu memory pids` here, not `io`: systemd accepts
//!   `IOWeight` on a user scope without complaint and no `io.weight`
//!   file ever appears. So what is offered is read from the cgroup's own
//!   `cgroup.controllers` ([`Offer`]), never assumed — a control that
//!   silently does nothing is worse than none.
//!
//! Limits are set at runtime only: a reboot clears them, and a scope made
//! for one goes when its last process does.
//!
//! Memory is set as `MemoryMax` with `MemoryHigh` at nine tenths of it:
//! past `MemoryHigh` the kernel makes the app reclaim before it grows,
//! which slows it rather than ends it, and only `MemoryMax` is a hard
//! stop. CPU is a percent of the *whole machine*, as the CPU column
//! reads, turned into systemd's per-core quota by the core count.
//!
//! `u64::MAX` is "no limit" for all of these: set on a probe scope, every
//! one read back as `infinity` / `[not set]` (systemd 258 here).

use std::path::Path;

/// One app's limits. `None` is "no limit".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Limits {
    /// Bytes.
    pub memory: Option<u64>,
    /// Percent of the whole machine, 1–100.
    pub cpu_percent: Option<u32>,
}

/// systemd's "infinity" for each property.
pub const UNLIMITED: u64 = u64::MAX;

impl Limits {
    /// The unit properties that set these, in systemd's own names and
    /// units: bytes, and microseconds of CPU a second (`CPUQuotaPerSecUSec`,
    /// where 1,000,000 is one core). Every property is sent, unset ones
    /// as [`UNLIMITED`], so applying a limit also clears any it replaces.
    pub fn properties(&self, cores: u32) -> Vec<(&'static str, u64)> {
        let (high, max) = match self.memory {
            Some(bytes) => (bytes / 10 * 9, bytes),
            None => (UNLIMITED, UNLIMITED),
        };
        let cpu = match self.cpu_percent {
            Some(p) => u64::from(p.clamp(1, 100)) * u64::from(cores.max(1)) * 10_000,
            None => UNLIMITED,
        };
        vec![("MemoryHigh", high), ("MemoryMax", max), ("CPUQuotaPerSecUSec", cpu)]
    }

    pub fn is_none(&self) -> bool {
        self.memory.is_none() && self.cpu_percent.is_none()
    }
}

/// What the kernel is enforcing on a cgroup now, from its own files:
/// `memory.max` and `cpu.max` (`max 100000`, or `50000 100000` for half a
/// core). A file that is missing — the controller isn't on there — reads
/// as no limit.
pub fn read(cgroup: &Path, cores: u32) -> Limits {
    let memory = std::fs::read_to_string(cgroup.join("memory.max")).ok().and_then(|s| s.trim().parse::<u64>().ok());
    let cpu_percent = std::fs::read_to_string(cgroup.join("cpu.max")).ok().and_then(|s| parse_cpu_max(&s, cores));
    Limits { memory, cpu_percent }
}

/// `cpu.max` as a percent of the whole machine.
pub fn parse_cpu_max(text: &str, cores: u32) -> Option<u32> {
    let mut f = text.split_whitespace();
    let quota: u64 = f.next()?.parse().ok()?;
    let period: u64 = f.next()?.parse().ok()?;
    if period == 0 {
        return None;
    }
    let percent = (quota * 100).div_ceil(period * u64::from(cores.max(1)));
    Some(percent.clamp(1, 100) as u32)
}

/// A process's cgroup path from the root of the hierarchy, from its own
/// `/proc/<pid>/cgroup` (`0::/user.slice/…`), readable for every process.
pub fn cgroup_of(proc_root: &Path, pid: i32) -> Option<String> {
    let text = std::fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup")).ok()?;
    text.lines().find_map(|l| l.strip_prefix("0::")).map(str::to_owned)
}

/// Whether `pid` started first of everything in its unit — its main
/// process, as near as `/proc` can say without asking systemd.
pub fn is_first(proc_root: &Path, pid: i32, in_unit: &[i32]) -> bool {
    let start = |p: i32| crate::proc::start_time_in(proc_root, p);
    match start(pid) {
        Some(mine) => in_unit.iter().filter_map(|p| start(*p)).all(|s| s >= mine),
        None => false,
    }
}

/// Every process in a cgroup and the cgroups inside it.
pub fn processes_in(dir: &Path) -> Vec<i32> {
    let mut out: Vec<i32> = std::fs::read_to_string(dir.join("cgroup.procs")).unwrap_or_default().lines().filter_map(|l| l.trim().parse().ok()).collect();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            out.extend(processes_in(&e.path()));
        }
    }
    out
}

/// Which limits can be enforced where the app would run: the parent
/// cgroup's `cgroup.controllers`, which is what a child scope may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Offer {
    pub memory: bool,
    pub cpu: bool,
}

pub fn offer(controllers: &str) -> Offer {
    let has = |c: &str| controllers.split_whitespace().any(|x| x == c);
    Offer { memory: has("memory"), cpu: has("cpu") }
}

/// The app being limited, as the window sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// What it is called, for the scope's name and description.
    pub name: String,
    pub lead: i32,
    /// Every process of the app.
    pub pids: Vec<i32>,
    /// The unit its lead runs in, and that unit's cgroup path from the
    /// root of the hierarchy (`/user.slice/…/app.slice/x.scope`).
    pub unit: Option<String>,
    pub cgroup: Option<String>,
    /// Every process in that cgroup, the app's or not.
    pub in_unit: Vec<i32>,
    /// The session runs on it — the compositor, or something it cannot
    /// do without. Never limited.
    pub critical: bool,
    /// The unit is a service and the lead is its first-started process —
    /// its main one, as near as `/proc` can say. Moving that out of the
    /// service would leave the service without the thing it runs.
    pub main: bool,
    /// The uid running this window, whose user manager would hold the
    /// limit, and whether the app is that user's own.
    pub me: u32,
    pub own: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// The unit holds this app and nothing else: limit the unit.
    Unit(String),
    /// The unit holds other processes too — the compositor's, usually —
    /// so the app moves into a scope of its own first.
    NewScope { name: String, pids: Vec<i32> },
    /// Not something this window may limit, and why, in words.
    Refuse(String),
}

/// How to limit `t`, or why not.
pub fn plan(t: &Target) -> Plan {
    if t.critical {
        return Plan::Refuse("Your session runs on this, and a limit could freeze the desktop.".into());
    }
    if !t.own {
        return Plan::Refuse("Only your own apps can be limited here.".into());
    }
    let user_manager = format!("/user@{}.service/", t.me);
    let (Some(unit), Some(cgroup)) = (&t.unit, &t.cgroup) else {
        return Plan::Refuse("It isn't in a systemd unit, so there is nothing to put a limit on.".into());
    };
    if !cgroup.contains(&user_manager) {
        return Plan::Refuse("It runs as a system service; only apps in your own session can be limited here.".into());
    }
    let alone = t.in_unit.iter().all(|p| t.pids.contains(p));
    if alone {
        Plan::Unit(unit.clone())
    } else if t.main && unit.ends_with(".service") {
        Plan::Refuse(format!("It is what {unit} runs, and moving it out to limit it would leave the service without it."))
    } else {
        Plan::NewScope { name: scope_name(&t.name, t.lead), pids: t.pids.clone() }
    }
}

/// A scope's name for `app`: `app-procman-<name>-<pid>.scope`, the name
/// kept to what a unit name may hold without escaping.
pub fn scope_name(app: &str, lead: i32) -> String {
    let clean: String = app.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' { c } else { '_' }).collect();
    let clean = if clean.is_empty() { "app".to_string() } else { clean.chars().take(40).collect() };
    format!("app-procman-{clean}-{lead}.scope")
}

/// `4 GB`, `512MB`, `1.5g`, `800 M` — a size as a person types it, in
/// bytes. Units are powers of 1024, as the Memory column shows them.
pub fn parse_size(text: &str) -> Option<u64> {
    let t = text.trim().to_ascii_lowercase().replace(' ', "");
    if t.is_empty() {
        return None;
    }
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (number, unit) = t.split_at(split);
    let n: f64 = number.parse().ok()?;
    let scale = match unit.trim_end_matches('b').trim_end_matches('i') {
        "" if unit.is_empty() => 1024.0 * 1024.0,
        "k" => 1024.0,
        "m" => 1024.0 * 1024.0,
        "g" => 1024.0 * 1024.0 * 1024.0,
        "t" => 1024.0_f64.powi(4),
        _ => return None,
    };
    let bytes = n * scale;
    // Below 64 MB nothing graphical runs at all, and a typo of "4" for
    // "4 GB" would otherwise end the app the moment it is applied.
    (bytes.is_finite() && bytes >= 64.0 * 1024.0 * 1024.0).then_some(bytes as u64)
}

#[cfg(feature = "dbus")]
pub use client::*;

#[cfg(feature = "dbus")]
mod client {
    use std::future::Future;
    use std::pin::Pin;

    pub type Pending<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send>>;

    /// Unit properties in systemd's own names and units — what
    /// [`super::Limits::properties`] and efficiency mode's CPU weight both are.
    pub type Properties = Vec<(&'static str, u64)>;

    /// What setting a limit needs of systemd: the user's own manager,
    /// which asks nothing of polkit for a user's own units.
    pub trait Limiter: Send + Sync {
        /// Sets `properties` on `unit`, at runtime only.
        fn set(&self, unit: String, properties: Properties) -> Pending<()>;
        /// Moves `pids` into a new scope named `name`, with `properties`.
        fn new_scope(&self, name: String, description: String, pids: Vec<u32>, properties: Properties) -> Pending<()>;
    }

    pub struct UserSystemd;

    async fn manager() -> Result<zbus::Proxy<'static>, String> {
        let conn = zbus::Connection::session().await.map_err(|e| format!("couldn't reach your session's systemd: {e}"))?;
        zbus::Proxy::new_owned(conn, "org.freedesktop.systemd1", "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager")
            .await
            .map_err(|e| e.to_string())
    }

    fn props(properties: Properties) -> Vec<(String, zbus::zvariant::OwnedValue)> {
        properties.into_iter().map(|(k, v)| (k.to_string(), zbus::zvariant::OwnedValue::from(v))).collect()
    }

    impl Limiter for UserSystemd {
        fn set(&self, unit: String, properties: Properties) -> Pending<()> {
            Box::pin(crate::bound::within(crate::bound::QUICK, "systemd", async move {
                let m = manager().await?;
                // `true`: runtime only, gone at reboot.
                m.call::<_, _, ()>("SetUnitProperties", &(&unit, true, props(properties)))
                    .await
                    .map_err(|e| format!("systemd refused the limit: {e}"))
            }))
        }

        fn new_scope(&self, name: String, description: String, pids: Vec<u32>, properties: Properties) -> Pending<()> {
            Box::pin(crate::bound::within(crate::bound::QUICK, "systemd", async move {
                let m = manager().await?;
                let mut p = props(properties);
                p.push(("PIDs".into(), zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(pids)).map_err(|e| e.to_string())?));
                p.push(("Description".into(), zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(description)).map_err(|e| e.to_string())?));
                let aux: Vec<(String, Vec<(String, zbus::zvariant::OwnedValue)>)> = Vec::new();
                m.call::<_, _, zbus::zvariant::OwnedObjectPath>("StartTransientUnit", &(&name, "fail", p, aux))
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("systemd couldn't give it a scope of its own: {e}"))
            }))
        }
    }

    /// systemd in memory: what was asked of it, in order.
    #[cfg(test)]
    #[derive(Default)]
    pub struct Mock {
        pub calls: std::sync::Mutex<Vec<String>>,
        pub refuse: bool,
    }

    #[cfg(test)]
    impl Limiter for Mock {
        fn set(&self, unit: String, properties: Properties) -> Pending<()> {
            self.calls.lock().unwrap().push(format!("set {unit} {properties:?}"));
            let refuse = self.refuse;
            Box::pin(async move { if refuse { Err("no".into()) } else { Ok(()) } })
        }
        fn new_scope(&self, name: String, _: String, pids: Vec<u32>, properties: Properties) -> Pending<()> {
            self.calls.lock().unwrap().push(format!("scope {name} {pids:?} {properties:?}"));
            Box::pin(async { Ok(()) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(unit: &str, cgroup: &str, pids: &[i32], in_unit: &[i32]) -> Target {
        Target {
            name: "ghostty".into(),
            lead: pids[0],
            pids: pids.to_vec(),
            unit: Some(unit.into()),
            cgroup: Some(cgroup.into()),
            in_unit: in_unit.to_vec(),
            critical: false,
            main: false,
            me: 1000,
            own: true,
        }
    }

    const WM: &str = "/user.slice/user-1000.slice/user@1000.service/session.slice/wayland-wm@hyprland.desktop.service";

    /// Ghostty on this machine: in the compositor's unit, beside Hyprland
    /// and waybar. Limiting that unit would cap the compositor, so the
    /// app gets a scope of its own instead.
    #[test]
    fn an_app_sharing_the_compositors_unit_is_moved_out_before_it_is_limited() {
        let t = target("wayland-wm@hyprland.desktop.service", WM, &[13060, 13061], &[1287, 1354, 13060, 13061]);
        assert_eq!(plan(&t), Plan::NewScope { name: "app-procman-ghostty-13060.scope".into(), pids: vec![13060, 13061] });
    }

    /// Claude's desktop app here, in `app-com.anthropic.Claude-….scope`
    /// with nothing else: the unit itself takes the limit.
    #[test]
    fn an_app_alone_in_its_unit_is_limited_there() {
        let cg = "/user.slice/user-1000.slice/user@1000.service/app.slice/app-com.anthropic.Claude-232763.scope";
        let t = target("app-com.anthropic.Claude-232763.scope", cg, &[232763, 232764], &[232763, 232764]);
        assert_eq!(plan(&t), Plan::Unit("app-com.anthropic.Claude-232763.scope".into()));
    }

    /// A rule for something a user service runs would otherwise move
    /// the service's own process out of it.
    #[test]
    fn a_services_main_process_is_not_moved_out_of_it() {
        let cg = "/user.slice/user-1000.slice/user@1000.service/app.slice/syncthing.service";
        let mut t = target("syncthing.service", cg, &[500], &[500, 501]);
        t.main = true;
        assert!(matches!(plan(&t), Plan::Refuse(why) if why.contains("syncthing.service")));
        t.main = false;
        assert!(matches!(plan(&t), Plan::NewScope { .. }), "a helper it started may still be moved");
    }

    #[test]
    fn the_session_another_users_app_and_a_system_service_are_refused() {
        let mut t = target("wayland-wm@hyprland.desktop.service", WM, &[1287], &[1287]);
        t.critical = true;
        assert!(matches!(plan(&t), Plan::Refuse(_)));
        let mut t = target("x.scope", WM, &[5], &[5]);
        t.own = false;
        assert!(matches!(plan(&t), Plan::Refuse(_)));
        let t = target("NetworkManager.service", "/system.slice/NetworkManager.service", &[9], &[9]);
        assert!(matches!(plan(&t), Plan::Refuse(_)));
    }

    /// Every property is sent, so a new limit replaces an old one; unset
    /// is systemd's infinity; memory throttles at nine tenths; CPU is the
    /// whole machine's percent in one core's microseconds.
    #[test]
    fn limits_become_systemds_properties() {
        let l = Limits { memory: Some(4 << 30), cpu_percent: Some(25) };
        assert_eq!(l.properties(16), vec![("MemoryHigh", (4u64 << 30) / 10 * 9), ("MemoryMax", 4 << 30), ("CPUQuotaPerSecUSec", 4_000_000)]);
        assert_eq!(Limits::default().properties(16), vec![("MemoryHigh", UNLIMITED), ("MemoryMax", UNLIMITED), ("CPUQuotaPerSecUSec", UNLIMITED)]);
    }

    /// `cpu.max` as the probe scope here read: 500 ms a second on a
    /// 16-thread machine is 3.1 % of it, rounded up so a small limit
    /// never reads as none.
    #[test]
    fn the_kernels_cpu_max_reads_back_as_a_share_of_the_machine() {
        assert_eq!(parse_cpu_max("50000 100000\n", 16), Some(4));
        assert_eq!(parse_cpu_max("400000 100000", 16), Some(25));
        assert_eq!(parse_cpu_max("max 100000\n", 16), None);
    }

    /// This machine's user manager: delegated cpu, memory and pids, and
    /// not io — so no I/O limit is offered for an app in it.
    #[test]
    fn only_controllers_the_cgroup_has_are_offered() {
        assert_eq!(offer("cpu memory pids\n"), Offer { memory: true, cpu: true });
        assert_eq!(offer("memory pids"), Offer { memory: true, cpu: false });
    }

    #[test]
    fn a_size_reads_as_people_type_it_and_a_tiny_one_is_refused() {
        assert_eq!(parse_size("4 GB"), Some(4 << 30));
        assert_eq!(parse_size("512mb"), Some(512 << 20));
        assert_eq!(parse_size("1.5g"), Some((1.5 * (1u64 << 30) as f64) as u64));
        assert_eq!(parse_size("800"), Some(800 << 20), "a bare number is megabytes");
        assert_eq!(parse_size("4"), None, "4 MB would end the app at once: almost certainly 4 GB mistyped");
        assert_eq!(parse_size("lots"), None);
    }

    #[test]
    fn a_scope_name_keeps_to_what_a_unit_name_may_hold() {
        assert_eq!(scope_name("Isolated Web Co", 7), "app-procman-Isolated_Web_Co-7.scope");
    }

    /// This process's own cgroup reads, whatever its limits are: no
    /// limit reads as none, not as zero.
    #[test]
    fn this_processs_own_limits_read_from_its_cgroup() {
        let line = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        let path = line.lines().find_map(|l| l.strip_prefix("0::")).unwrap().to_string();
        let l = read(&Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')), 16);
        assert!(l.memory.is_none_or(|m| m >= 64 << 20));
    }
}

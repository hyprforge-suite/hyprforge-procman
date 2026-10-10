//! Services: systemd's units, what they cost, and starting and stopping
//! them — the suite's Service Manager, as this app's Services tab.
//!
//! Built in the order the suite's CLAUDE.md gives for a D-Bus module:
//! the [`Systemd`] trait and its mock first, the decisions as pure
//! functions over plain data ([`rows`]), and the client last.
//!
//! **No helper.** systemd asks polkit itself on every `StartUnit`,
//! `StopUnit` and `EnableUnitFiles`, so the window calls it directly and
//! marks the call as allowed to prompt (`ALLOW_INTERACTIVE_AUTHORIZATION`)
//! — the prompt is the session's polkit agent, hyprforge-polkit's lock
//! card. A user unit needs no prompt at all.
//!
//! **What a service costs** comes from its own cgroup, not from adding
//! up processes: the kernel already counts a unit's CPU time
//! (`cpu.stat`) and memory (`memory.current`) whether or not anything is
//! watching. Each running unit has a directory under `/sys/fs/cgroup`
//! named after it, so one walk of that tree maps every running unit to
//! its counters — asking systemd for each unit's `ControlGroup` returns
//! nothing for one asked about by an alias (`dbus-broker.service` here).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The machine's: `systemctl`, changes ask for administrator access.
    System,
    /// This user's: `systemctl --user`, nothing to ask.
    User,
}

/// One unit, as systemd lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    pub description: String,
    /// `loaded`, `not-found`, `masked`, `error`.
    pub load: String,
    /// `active`, `inactive`, `failed`, `activating`, `deactivating`.
    pub active: String,
    /// The finer state: `running`, `exited`, `dead`, `start-pre`, …
    pub sub: String,
    /// The unit file's state: `enabled`, `disabled`, `static`, `masked`,
    /// `alias`, … — `None` for a unit with no file (a transient one).
    pub file: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    Reload,
    /// Start at boot (or login, for a user unit).
    Enable,
    Disable,
    Mask,
    Unmask,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Start => "Start",
            Action::Stop => "Stop",
            Action::Restart => "Restart",
            Action::Reload => "Reload",
            Action::Enable => "Start at boot",
            Action::Disable => "Don't start at boot",
            Action::Mask => "Mask",
            Action::Unmask => "Unmask",
        }
    }

    /// The actions that make sense for a unit in this state, in the
    /// order a row offers them. Masking is never offered here: it is a
    /// stronger thing than "off", and a dialog of its own asks for it.
    pub fn offered(unit: &Unit) -> Vec<Action> {
        let mut out = Vec::new();
        match unit.active.as_str() {
            "active" | "reloading" => out.extend([Action::Restart, Action::Stop]),
            "activating" | "deactivating" => out.push(Action::Stop),
            _ => {
                if unit.load != "masked" && unit.file.as_deref() != Some("masked") {
                    out.push(Action::Start);
                }
            }
        }
        match unit.file.as_deref() {
            Some("enabled") | Some("enabled-runtime") => out.push(Action::Disable),
            Some("disabled") => out.push(Action::Enable),
            Some("masked") | Some("masked-runtime") => out.push(Action::Unmask),
            _ => {}
        }
        out
    }
}

pub type Pending<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send>>;

/// systemd, for one scope.
pub trait Systemd: Send + Sync {
    fn list(&self, scope: Scope) -> Pending<Vec<Unit>>;
    fn act(&self, scope: Scope, name: String, action: Action) -> Pending<()>;
}

/// The real thing, over D-Bus: the system bus for system units, the
/// session bus for this user's.
pub struct Dbus;

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Manager {
    #[allow(clippy::type_complexity)]
    fn list_units_by_patterns(
        &self,
        states: &[&str],
        patterns: &[&str],
    ) -> zbus::Result<Vec<(String, String, String, String, String, String, zbus::zvariant::OwnedObjectPath, u32, String, zbus::zvariant::OwnedObjectPath)>>;
    fn list_unit_files_by_patterns(&self, states: &[&str], patterns: &[&str]) -> zbus::Result<Vec<(String, String)>>;
}

async fn bus(scope: Scope) -> Result<zbus::Connection, String> {
    match scope {
        Scope::System => zbus::Connection::system().await,
        Scope::User => zbus::Connection::session().await,
    }
    .map_err(|e| format!("couldn't reach systemd: {e}"))
}

impl Systemd for Dbus {
    fn list(&self, scope: Scope) -> Pending<Vec<Unit>> {
        Box::pin(async move {
            let conn = bus(scope).await?;
            let manager = ManagerProxy::new(&conn).await.map_err(|e| e.to_string())?;
            let units = manager.list_units_by_patterns(&[], &["*.service"]).await.map_err(|e| format!("systemd didn't list its units: {e}"))?;
            let files = manager.list_unit_files_by_patterns(&[], &["*.service"]).await.map_err(|e| e.to_string())?;
            let files: HashMap<String, String> = files
                .into_iter()
                .filter_map(|(path, state)| Some((Path::new(&path).file_name()?.to_string_lossy().into_owned(), state)))
                .collect();
            Ok(merge(units.into_iter().map(|u| (u.0, u.1, u.2, u.3, u.4)).collect(), files))
        })
    }

    fn act(&self, scope: Scope, name: String, action: Action) -> Pending<()> {
        Box::pin(async move {
            let conn = bus(scope).await?;
            let manager = zbus::Proxy::new(&conn, "org.freedesktop.systemd1", "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager")
                .await
                .map_err(|e| e.to_string())?;
            // Allowed to prompt: systemd hands this to polkit with the
            // flag, and the session's agent asks for the password.
            let flags = zbus::proxy::MethodFlags::AllowInteractiveAuth.into();
            let names = [name.as_str()];
            let result: zbus::Result<()> = match action {
                Action::Start => manager.call_with_flags::<_, _, zbus::zvariant::OwnedObjectPath>("StartUnit", flags, &(&name, "replace")).await.map(|_| ()),
                Action::Stop => manager.call_with_flags::<_, _, zbus::zvariant::OwnedObjectPath>("StopUnit", flags, &(&name, "replace")).await.map(|_| ()),
                Action::Restart => manager.call_with_flags::<_, _, zbus::zvariant::OwnedObjectPath>("RestartUnit", flags, &(&name, "replace")).await.map(|_| ()),
                Action::Reload => manager.call_with_flags::<_, _, zbus::zvariant::OwnedObjectPath>("ReloadUnit", flags, &(&name, "replace")).await.map(|_| ()),
                Action::Enable => manager
                    .call_with_flags::<_, _, (bool, Vec<(String, String, String)>)>("EnableUnitFiles", flags, &(&names[..], false, false))
                    .await
                    .map(|_| ()),
                Action::Disable => {
                    manager.call_with_flags::<_, _, Vec<(String, String, String)>>("DisableUnitFiles", flags, &(&names[..], false)).await.map(|_| ())
                }
                Action::Mask => {
                    manager.call_with_flags::<_, _, Vec<(String, String, String)>>("MaskUnitFiles", flags, &(&names[..], false, false)).await.map(|_| ())
                }
                Action::Unmask => {
                    manager.call_with_flags::<_, _, Vec<(String, String, String)>>("UnmaskUnitFiles", flags, &(&names[..], false)).await.map(|_| ())
                }
            };
            result.map_err(|e| said(&e))?;
            // A unit file change is not seen until the manager reloads.
            if matches!(action, Action::Enable | Action::Disable | Action::Mask | Action::Unmask) {
                manager.call_with_flags::<_, _, ()>("Reload", flags, &()).await.map_err(|e| said(&e))?;
            }
            Ok(())
        })
    }
}

/// systemd's refusals, in words.
fn said(e: &zbus::Error) -> String {
    let text = e.to_string();
    if text.contains("InteractiveAuthorizationRequired") || text.contains("AccessDenied") || text.contains("Access denied") {
        "administrator access wasn't given".into()
    } else {
        text
    }
}

/// `ListUnits`' rows (name, description, load, active, sub) joined to
/// the unit files' states, by file name.
pub fn merge(units: Vec<(String, String, String, String, String)>, files: HashMap<String, String>) -> Vec<Unit> {
    let mut out: Vec<Unit> = units
        .into_iter()
        .map(|(name, description, load, active, sub)| {
            let file = files.get(&name).cloned();
            Unit { name, description, load, active, sub, file }
        })
        .collect();
    // A unit file nothing has loaded is still a service someone may want
    // to start or enable: listed, inactive.
    let listed: std::collections::HashSet<String> = out.iter().map(|u| u.name.clone()).collect();
    for (name, state) in files {
        if !listed.contains(&name) && !name.ends_with("@.service") {
            out.push(Unit {
                description: String::new(),
                load: if state == "masked" { "masked".into() } else { "loaded".into() },
                active: "inactive".into(),
                sub: "dead".into(),
                file: Some(state),
                name,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Every running unit's cgroup directory, by unit name, from one walk of
/// the cgroup tree.
pub fn cgroups(root: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if !kind.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name.ends_with(".service") {
                // The first found wins: a user's manager runs in its own
                // `user@1000.service`, under which its units are found.
                out.entry(name).or_insert_with(|| path.clone());
            }
            if depth < 8 {
                stack.push((path, depth + 1));
            }
        }
    }
    out
}

/// What a unit's cgroup says it has used: CPU time in microseconds, and
/// memory in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub cpu_usec: u64,
    pub memory: u64,
}

pub fn counters(cgroup: &Path) -> Option<Counters> {
    let stat = std::fs::read_to_string(cgroup.join("cpu.stat")).ok()?;
    let cpu_usec = stat.lines().find_map(|l| l.strip_prefix("usage_usec ")?.trim().parse().ok())?;
    let memory = std::fs::read_to_string(cgroup.join("memory.current")).ok().and_then(|m| m.trim().parse().ok()).unwrap_or(0);
    Some(Counters { cpu_usec, memory })
}

/// Which units the list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Running and failed, and everything enabled — what a person means
    /// by "my services". Hides the hundreds of static units nothing
    /// asked for.
    #[default]
    Relevant,
    Running,
    Failed,
    All,
}

/// The rows to draw: failed first (the thing to look at), then by name.
pub fn rows<'a>(units: &'a [Unit], filter: Filter, query: &str) -> Vec<&'a Unit> {
    let query = query.trim().to_lowercase();
    let mut out: Vec<&Unit> = units
        .iter()
        .filter(|u| match filter {
            Filter::All => true,
            Filter::Running => u.active == "active",
            Filter::Failed => u.active == "failed",
            Filter::Relevant => {
                u.active == "active" || u.active == "failed" || matches!(u.file.as_deref(), Some("enabled") | Some("enabled-runtime"))
            }
        })
        .filter(|u| query.is_empty() || u.name.to_lowercase().contains(&query) || u.description.to_lowercase().contains(&query))
        .collect();
    out.sort_by_key(|u| (u.active != "failed", u.name.clone()));
    out
}

/// The unit's last lines in the journal, bounded like every other
/// program this suite waits on. Empty when the journal has nothing this
/// user may read for it.
pub fn journal(scope: Scope, name: &str, lines: usize) -> Result<Vec<String>, String> {
    let mut command = std::process::Command::new("journalctl");
    match scope {
        Scope::System => command.arg("-u"),
        Scope::User => command.arg("--user-unit"),
    };
    command.args([name, "-n", &lines.to_string(), "--no-pager", "-o", "short-iso", "-q"]);
    let out = hyprforge_process::output(&mut command, hyprforge_process::TIMEOUT).map_err(|e| format!("journalctl didn't answer: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect())
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::Mutex;

    /// systemd as a list in memory, which acts on itself.
    #[derive(Default)]
    pub struct Mock {
        pub units: Mutex<HashMap<Scope, Vec<Unit>>>,
        pub done: Mutex<Vec<(Scope, String, Action)>>,
        /// A system change answered as polkit refusing it.
        pub refuse_system: bool,
    }

    impl Systemd for Mock {
        fn list(&self, scope: Scope) -> Pending<Vec<Unit>> {
            let units = self.units.lock().unwrap().get(&scope).cloned().unwrap_or_default();
            Box::pin(async move { Ok(units) })
        }

        fn act(&self, scope: Scope, name: String, action: Action) -> Pending<()> {
            if scope == Scope::System && self.refuse_system {
                return Box::pin(async { Err("administrator access wasn't given".to_string()) });
            }
            self.done.lock().unwrap().push((scope, name.clone(), action));
            if let Some(u) = self.units.lock().unwrap().get_mut(&scope).and_then(|us| us.iter_mut().find(|u| u.name == name)) {
                match action {
                    Action::Start | Action::Restart => (u.active, u.sub) = ("active".into(), "running".into()),
                    Action::Stop => (u.active, u.sub) = ("inactive".into(), "dead".into()),
                    Action::Enable => u.file = Some("enabled".into()),
                    Action::Disable => u.file = Some("disabled".into()),
                    Action::Mask => u.file = Some("masked".into()),
                    Action::Unmask => u.file = Some("disabled".into()),
                    Action::Reload => {}
                }
            }
            Box::pin(async { Ok(()) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(name: &str, active: &str, file: Option<&str>) -> Unit {
        Unit {
            name: name.into(),
            description: format!("The {name}"),
            load: "loaded".into(),
            active: active.into(),
            sub: if active == "active" { "running".into() } else { "dead".into() },
            file: file.map(Into::into),
        }
    }

    /// The real reply's shape: `ListUnitsByPatterns` rows (cut to the
    /// five fields kept) and `ListUnitFilesByPatterns` paths, from this
    /// machine on 2026-10-10.
    #[test]
    fn units_take_their_file_state_and_unloaded_files_are_listed() {
        let units = vec![
            ("wpa_supplicant.service".into(), "WPA supplicant".into(), "loaded".into(), "active".into(), "running".into()),
            ("plymouth-start.service".into(), "plymouth-start.service".into(), "not-found".into(), "inactive".into(), "dead".into()),
        ];
        let files = HashMap::from([
            ("wpa_supplicant.service".to_string(), "disabled".to_string()),
            ("dbus-broker.service".to_string(), "disabled".to_string()),
            ("getty@.service".to_string(), "enabled".to_string()),
        ]);
        let all = merge(units, files);
        let names: Vec<&str> = all.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["dbus-broker.service", "plymouth-start.service", "wpa_supplicant.service"], "a template is not a unit");
        assert_eq!(all[2].file.as_deref(), Some("disabled"));
        assert_eq!(all[1].file, None);
    }

    #[test]
    fn failed_units_come_first_and_the_static_crowd_is_hidden() {
        let units = [
            unit("a.service", "active", Some("enabled")),
            unit("b.service", "failed", Some("enabled")),
            unit("c.service", "inactive", Some("static")),
            unit("d.service", "inactive", Some("enabled")),
        ];
        let names = |rows: Vec<&Unit>| rows.into_iter().map(|u| u.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(rows(&units, Filter::Relevant, "")), ["b.service", "a.service", "d.service"]);
        assert_eq!(names(rows(&units, Filter::Failed, "")), ["b.service"]);
        assert_eq!(rows(&units, Filter::All, "").len(), 4);
        assert_eq!(names(rows(&units, Filter::All, "the c")), ["c.service"], "description searched too");
    }

    #[test]
    fn a_row_offers_what_its_state_allows() {
        assert_eq!(Action::offered(&unit("a", "active", Some("enabled"))), [Action::Restart, Action::Stop, Action::Disable]);
        assert_eq!(Action::offered(&unit("a", "inactive", Some("disabled"))), [Action::Start, Action::Enable]);
        assert_eq!(Action::offered(&unit("a", "failed", Some("static"))), [Action::Start]);
        assert_eq!(Action::offered(&unit("a", "inactive", Some("masked"))), [Action::Unmask], "a masked unit can't start");
    }

    #[test]
    fn a_cgroup_walk_finds_units_at_any_depth_and_reads_their_counters() {
        let t = tempfile::tempdir().unwrap();
        let svc = t.path().join("system.slice/system-getty.slice/getty@tty1.service");
        std::fs::create_dir_all(&svc).unwrap();
        // Copied from this machine's app.slice: cpu.stat's first lines.
        std::fs::write(svc.join("cpu.stat"), "usage_usec 17661922422\nuser_usec 15539361890\nsystem_usec 2122560532\n").unwrap();
        std::fs::write(svc.join("memory.current"), "1048576\n").unwrap();
        let user = t.path().join("user.slice/user-1000.slice/user@1000.service/app.slice/pipewire.service");
        std::fs::create_dir_all(&user).unwrap();
        let found = cgroups(t.path());
        assert_eq!(found["getty@tty1.service"], svc);
        assert!(found.contains_key("user@1000.service") && found.contains_key("pipewire.service"));
        assert_eq!(counters(&svc), Some(Counters { cpu_usec: 17661922422, memory: 1048576 }));
    }

    #[tokio::test]
    async fn the_mock_acts_like_systemd_and_refuses_like_polkit() {
        let m = mock::Mock { refuse_system: true, ..Default::default() };
        m.units.lock().unwrap().insert(Scope::User, vec![unit("p.service", "inactive", Some("disabled"))]);
        m.act(Scope::User, "p.service".into(), Action::Start).await.unwrap();
        assert_eq!(m.list(Scope::User).await.unwrap()[0].active, "active");
        assert!(m.act(Scope::System, "x.service".into(), Action::Stop).await.is_err());
    }

    /// Read-only: systemd lists itself, and every state is one this file
    /// knows how to draw.
    #[tokio::test]
    #[ignore = "asks the real systemd"]
    async fn the_real_systemd_lists_services_in_known_states() {
        let units = Dbus.list(Scope::System).await.unwrap();
        assert!(units.iter().any(|u| u.name == "systemd-journald.service"));
        for u in &units {
            assert!(["active", "inactive", "failed", "activating", "deactivating", "reloading", "maintenance", "refreshing"].contains(&u.active.as_str()), "{}: {}", u.name, u.active);
        }
        let found = cgroups(Path::new("/sys/fs/cgroup"));
        assert!(found.contains_key("systemd-journald.service"));
    }
}

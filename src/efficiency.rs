//! Efficiency mode: an app nobody can see yields the CPU to one they can.
//!
//! Windows 11 lowers a background process's priority. Here that has to
//! be a cgroup's CPU weight, for two reasons found on this machine: a
//! user may raise a process's nice value but never lower it again (its
//! `RLIMIT_NICE` is 0), so a niced app could not be given its priority
//! back when it came into view; and a weight only matters when the CPU
//! is contended, so a background app on an idle machine runs exactly as
//! fast as before — it gives way only when something on screen wants the
//! time.
//!
//! **Background is "no window on screen".** Hyprland's own `visible` is
//! true for windows on workspaces nobody is looking at, so on screen is
//! worked out from the monitors' workspaces instead
//! ([`crate::windows::Window::on_screen`]). A video on a second monitor
//! is on screen and left alone.
//!
//! Each background app's unit gets [`BACKGROUND_WEIGHT`]; when it comes
//! back into view the weight is unset (systemd's default, 100). An app
//! sharing the desktop's unit is moved into a scope of its own first, as
//! limits do — that unit has no `cpu` controller to weigh by. Only apps
//! with windows are touched, which leaves the compositor and the
//! session's managers alone by construction.
//!
//! Everything here is plain data in and decisions out; the service that
//! listens to Hyprland and calls systemd is `src/bin/efficiency.rs`.

use crate::limits::{self, Plan, Target};
use crate::model::Table;
use crate::windows::Window;
use std::collections::{HashMap, HashSet};

/// A background app's share against a foreground one's 100: under
/// contention it gets a fifth of the time.
pub const BACKGROUND_WEIGHT: u64 = 20;

/// One windowed app, and whether any of its windows is on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub name: String,
    pub lead: i32,
    pub pids: Vec<i32>,
    pub on_screen: bool,
    pub own: bool,
}

/// Every windowed app in the table, and whether it is on screen.
pub fn seen(table: &Table, windows: &[Window], shown: &HashSet<i64>) -> Vec<Seen> {
    table
        .apps
        .iter()
        .filter(|a| a.windows > 0)
        .map(|a| Seen {
            name: a.lead.name.clone(),
            lead: a.lead.id.pid,
            pids: a.members.iter().map(|m| m.id.pid).collect(),
            on_screen: windows.iter().any(|w| w.pid == a.lead.id.pid && w.on_screen(shown)),
            own: a.lead.own,
        })
        .collect()
}

/// What to do to one app this pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Weigh this unit down.
    Weigh(String),
    /// Move these into a new scope, weighed down.
    Move { name: String, pids: Vec<i32> },
    /// Give this unit its weight back.
    Restore(String),
}

/// This pass's steps. `plans` is each app's limits plan, by lead pid;
/// `weighed` the units this service has weighed down and not yet given
/// back. Nothing is done twice: a unit already down stays down without a
/// call, and only a unit this service weighed is ever restored.
pub fn steps(apps: &[Seen], plans: &HashMap<i32, Plan>, weighed: &HashSet<String>) -> Vec<Step> {
    let mut out = Vec::new();
    for app in apps {
        match (app.on_screen, plans.get(&app.lead)) {
            (false, Some(Plan::Unit(unit))) if !weighed.contains(unit) => out.push(Step::Weigh(unit.clone())),
            (false, Some(Plan::NewScope { name, pids })) => out.push(Step::Move { name: name.clone(), pids: pids.clone() }),
            (true, Some(Plan::Unit(unit))) if weighed.contains(unit) => out.push(Step::Restore(unit.clone())),
            _ => {}
        }
    }
    out
}

/// Units this service weighed whose app has gone, or is no longer in a
/// unit of its own — given back too, so nothing it set outlives the app.
pub fn orphans(plans: &HashMap<i32, Plan>, weighed: &HashSet<String>) -> Vec<String> {
    let live: HashSet<&String> = plans.values().filter_map(|p| if let Plan::Unit(u) = p { Some(u) } else { None }).collect();
    let mut gone: Vec<String> = weighed.iter().filter(|u| !live.contains(u)).cloned().collect();
    gone.sort();
    gone
}

/// The limits plan for one windowed app, from what `/proc` and the
/// cgroup tree say now. Windowed apps only, so `critical` is false: the
/// compositor and the session's managers own no window.
pub fn plan_for(app: &Seen, me: u32) -> Plan {
    let proc_root = std::path::Path::new("/proc");
    let cgroup = limits::cgroup_of(proc_root, app.lead);
    let in_unit = cgroup.as_deref().map(|c| limits::processes_in(&std::path::Path::new("/sys/fs/cgroup").join(c.trim_start_matches('/')))).unwrap_or_default();
    limits::plan(&Target {
        name: app.name.clone(),
        lead: app.lead,
        pids: app.pids.clone(),
        unit: crate::inspect::unit(proc_root, app.lead),
        main: limits::is_first(proc_root, app.lead, &in_unit),
        in_unit,
        cgroup,
        critical: false,
        me,
        own: app.own,
    })
}

/// The CPU weight property, down or given back.
pub fn weight(down: bool) -> Vec<(&'static str, u64)> {
    vec![("CPUWeight", if down { BACKGROUND_WEIGHT } else { limits::UNLIMITED })]
}

/// Whether efficiency mode is holding back the unit `cgroup` names (as
/// `/proc/<pid>/cgroup` gives it), or the unit just above it — read from
/// the weight the kernel has, so what the list says is what is applied,
/// whatever the service meant to do. An app's processes sit in its
/// scope or one level under it.
pub fn held_back(cgroup_root: &std::path::Path, cgroup: &str) -> bool {
    let mut at = std::path::PathBuf::from(cgroup.trim_start_matches('/'));
    for _ in 0..2 {
        let weight = std::fs::read_to_string(cgroup_root.join(&at).join("cpu.weight")).ok();
        if weight.as_deref().and_then(|w| w.trim().parse::<u64>().ok()) == Some(BACKGROUND_WEIGHT) {
            return true;
        }
        if !at.pop() {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_is_held_back_by_its_own_units_weight_or_the_one_above() {
        let root = tempfile::tempdir().unwrap();
        let unit = root.path().join("user.slice/app-steam.scope");
        std::fs::create_dir_all(unit.join("sub")).unwrap();
        std::fs::write(unit.join("cpu.weight"), "20\n").unwrap();
        std::fs::write(unit.join("sub/cpu.weight"), "100\n").unwrap();
        assert!(held_back(root.path(), "/user.slice/app-steam.scope"));
        assert!(held_back(root.path(), "/user.slice/app-steam.scope/sub"), "one level under it");
        std::fs::write(unit.join("cpu.weight"), "100\n").unwrap();
        assert!(!held_back(root.path(), "/user.slice/app-steam.scope"), "the default weight is not held back");
        assert!(!held_back(root.path(), "/nowhere"), "no cgroup is not held back");
    }

    fn app(lead: i32, on_screen: bool) -> Seen {
        Seen { name: format!("app{lead}"), lead, pids: vec![lead], on_screen, own: true }
    }

    /// Off screen, a unit of its own is weighed down once; back on
    /// screen it is given back — and only because this weighed it.
    #[test]
    fn an_app_off_screen_is_weighed_down_once_and_given_back_on_screen() {
        let plans = HashMap::from([(1, Plan::Unit("app-a.scope".into()))]);
        let mut weighed = HashSet::new();
        assert_eq!(steps(&[app(1, false)], &plans, &weighed), [Step::Weigh("app-a.scope".into())]);
        weighed.insert("app-a.scope".to_string());
        assert!(steps(&[app(1, false)], &plans, &weighed).is_empty(), "already down: no second call");
        assert_eq!(steps(&[app(1, true)], &plans, &weighed), [Step::Restore("app-a.scope".into())]);
        assert!(steps(&[app(1, true)], &plans, &HashSet::new()).is_empty(), "never restores a unit it didn't weigh");
    }

    /// Ghostty, sharing the desktop's unit: moved out, but only once it
    /// is off screen — an app in view is never moved for nothing.
    #[test]
    fn an_app_sharing_the_desktops_unit_is_moved_only_when_it_goes_off_screen() {
        let plans = HashMap::from([(13060, Plan::NewScope { name: "app-procman-ghostty-13060.scope".into(), pids: vec![13060, 13061] })]);
        assert!(steps(&[app(13060, true)], &plans, &HashSet::new()).is_empty());
        assert_eq!(steps(&[app(13060, false)], &plans, &HashSet::new()), [Step::Move { name: "app-procman-ghostty-13060.scope".into(), pids: vec![13060, 13061] }]);
    }

    #[test]
    fn a_refused_app_gets_nothing() {
        let plans = HashMap::from([(5, Plan::Refuse("no".into()))]);
        assert!(steps(&[app(5, false)], &plans, &HashSet::new()).is_empty());
    }

    #[test]
    fn a_weighed_unit_whose_app_is_gone_is_given_back() {
        let plans = HashMap::from([(1, Plan::Unit("app-a.scope".into()))]);
        let weighed: HashSet<String> = ["app-a.scope".to_string(), "app-gone.scope".to_string()].into_iter().collect();
        assert_eq!(orphans(&plans, &weighed), ["app-gone.scope"]);
    }

    /// The window list as it was here: three of Ghostty's windows, one on
    /// the monitor's workspace. It is on screen; Files, alone on 4, isn't.
    #[test]
    fn an_app_is_on_screen_when_any_of_its_windows_is() {
        use crate::model::{App, Entry};
        use crate::sample::Id;
        use crate::windows::WorkspaceRef;
        let lead = |pid| Entry { id: Id { pid, start: 1 }, own: true, ..Entry::default() };
        let table = Table {
            apps: vec![
                App { lead: lead(13060), class: "ghostty".into(), title: String::new(), windows: 3, members: vec![lead(13060)], flatpak: None, container: None },
                App { lead: lead(24764), class: "files".into(), title: String::new(), windows: 1, members: vec![lead(24764)], flatpak: None, container: None },
                App { lead: lead(9), class: "bg".into(), title: String::new(), windows: 0, members: vec![lead(9)], flatpak: Some("x".into()), container: None },
            ],
            background: Vec::new(),
        };
        let w = |pid, ws| Window { pid, workspace: WorkspaceRef { id: ws }, ..Window::default() };
        let windows = [w(13060, 3), w(13060, 3), w(13060, 2), w(24764, 4)];
        let shown: HashSet<i64> = [2].into_iter().collect();
        let s = seen(&table, &windows, &shown);
        assert_eq!(s.len(), 2, "a windowless app is never touched");
        assert!(s[0].on_screen);
        assert!(!s[1].on_screen);
    }
}

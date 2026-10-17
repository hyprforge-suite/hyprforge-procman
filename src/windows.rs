//! Which process each window belongs to, from `hyprctl clients -j`.
//!
//! This is what lets the list put apps first, as people think of them —
//! "Firefox", not forty processes — and it is the part only Hyprland can
//! answer. Without Hyprland (or with `hyprctl` not answering) there are
//! no windows, and every process is listed under Background: a state,
//! not a failure.

use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct Window {
    pub pid: i32,
    pub class: String,
    pub title: String,
    /// The workspace it is on. Whether that is on screen is the monitors'
    /// to say ([`shown_workspaces`]): Hyprland's own `visible` is true for
    /// a window on a workspace nobody is looking at — every one of this
    /// machine's windows said so on 2026-10-17, three of them on
    /// workspaces 3 and 4 with the only monitor on 2.
    #[serde(default)]
    pub workspace: WorkspaceRef,
    #[serde(default)]
    pub hidden: bool,
    /// 0 for the focused window, then how long ago each was.
    #[serde(default, rename = "focusHistoryID")]
    pub focus: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub struct WorkspaceRef {
    pub id: i64,
}

impl Window {
    /// On a workspace a monitor is showing, and not hidden.
    pub fn on_screen(&self, shown: &std::collections::HashSet<i64>) -> bool {
        !self.hidden && shown.contains(&self.workspace.id)
    }
}

/// The workspaces on screen, from `hyprctl monitors -j`: each monitor's
/// active one, and its special workspace when one is open over it — a
/// special workspace's id is negative, and `0` means none.
pub fn shown_workspaces(json: &[u8]) -> Result<std::collections::HashSet<i64>, String> {
    #[derive(Deserialize)]
    struct Monitor {
        #[serde(rename = "activeWorkspace")]
        active: WorkspaceRef,
        #[serde(default, rename = "specialWorkspace")]
        special: WorkspaceRef,
    }
    let monitors: Vec<Monitor> = serde_json::from_slice(json).map_err(|e| format!("hyprctl's monitor list didn't parse: {e}"))?;
    Ok(monitors.iter().flat_map(|m| [m.active.id, m.special.id]).filter(|id| *id != 0).collect())
}

/// [`shown_workspaces`], asked of Hyprland.
pub fn read_shown() -> Result<std::collections::HashSet<i64>, String> {
    let out = hyprforge_process::output(Command::new("hyprctl").args(["monitors", "-j"]), hyprforge_process::TIMEOUT)
        .map_err(|e| format!("hyprctl didn't answer: {e}"))?;
    if !out.status.success() {
        return Err("hyprctl couldn't reach Hyprland".into());
    }
    shown_workspaces(&out.stdout)
}

/// The windows Hyprland has, or why there are none to show.
pub fn read() -> Result<Vec<Window>, String> {
    let out = hyprforge_process::output(Command::new("hyprctl").args(["clients", "-j"]), hyprforge_process::TIMEOUT)
        .map_err(|e| format!("hyprctl didn't answer: {e}"))?;
    if !out.status.success() {
        return Err("hyprctl couldn't reach Hyprland".into());
    }
    parse(&out.stdout)
}

/// The fields this needs from each window; Hyprland sends thirty more.
/// A window with no process (a pid of -1 or 0) is skipped.
pub fn parse(json: &[u8]) -> Result<Vec<Window>, String> {
    let windows: Vec<Window> = serde_json::from_slice(json).map_err(|e| format!("hyprctl's window list didn't parse: {e}"))?;
    Ok(windows.into_iter().filter(|w| w.pid > 0).collect())
}

#[cfg(test)]
pub(crate) mod fixture {
    /// The shape of one window from `hyprctl clients -j` on Hyprland
    /// 0.56, keys and types as the real output had them on 2026-10-10;
    /// the values are made up, because the real ones were someone's
    /// window titles.
    pub fn client(pid: i32, class: &str, title: &str) -> String {
        format!(
            r#"{{"address":"0x55d0c3a1e2f0","mapped":true,"hidden":false,"visible":true,"acceptsInput":true,"at":[10,40],"size":[1200,800],"workspace":{{"id":1,"name":"1"}},"floating":false,"monitor":0,"class":"{class}","title":"{title}","initialClass":"{class}","initialTitle":"{title}","pid":{pid},"xwayland":false,"pinned":false,"pinFullscreened":false,"fullscreen":0,"fullscreenClient":0,"fullscreenHandler":"","allowedOverFullscreen":false,"grouped":[],"tags":[],"swallowing":"0x0","focusHistoryID":0,"inhibitingIdle":false,"xdgTag":"","xdgDescription":"","contentType":"none","tearingHint":false,"stableId":"a1"}}"#
        )
    }

    pub fn clients(list: &[(i32, &str, &str)]) -> Vec<u8> {
        let items: Vec<String> = list.iter().map(|(p, c, t)| client(*p, c, t)).collect();
        format!("[{}]", items.join(",")).into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_list_reads_its_pid_class_and_title() {
        let json = fixture::clients(&[(4242, "firefox", "The Daily Prophet"), (-1, "ghost", "")]);
        let windows = parse(&json).unwrap();
        assert_eq!(windows, [Window { pid: 4242, class: "firefox".into(), title: "The Daily Prophet".into(), workspace: WorkspaceRef { id: 1 }, hidden: false, focus: 0 }]);
    }

    /// This machine's monitor on 2026-10-17, and the window list's own
    /// `visible`, which said true for workspaces nobody could see.
    #[test]
    fn on_screen_is_the_monitors_workspaces_not_hyprlands_visible() {
        let shown = shown_workspaces(br#"[{"activeWorkspace":{"id":2,"name":"2"},"specialWorkspace":{"id":0,"name":""}}]"#).unwrap();
        assert_eq!(shown, [2].into_iter().collect());
        let at = |ws: i64| Window { workspace: WorkspaceRef { id: ws }, ..Window::default() };
        assert!(at(2).on_screen(&shown));
        assert!(!at(3).on_screen(&shown), "visible:true in hyprctl, and off screen");
        let with_special = shown_workspaces(br#"[{"activeWorkspace":{"id":2},"specialWorkspace":{"id":-98,"name":"special:term"}}]"#).unwrap();
        assert!(at(-98).on_screen(&with_special), "a special workspace open over the monitor is on screen");
        assert!(!Window { hidden: true, ..at(2) }.on_screen(&shown));
    }

    #[test]
    fn something_that_is_not_a_window_list_says_so() {
        assert!(parse(b"ok").unwrap_err().contains("didn't parse"));
    }
}

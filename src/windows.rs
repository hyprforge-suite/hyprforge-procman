//! Which process each window belongs to, from `hyprctl clients -j`.
//!
//! This is what lets the list put apps first, as people think of them —
//! "Firefox", not forty processes — and it is the part only Hyprland can
//! answer. Without Hyprland (or with `hyprctl` not answering) there are
//! no windows, and every process is listed under Background: a state,
//! not a failure.

use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Window {
    pub pid: i32,
    pub class: String,
    pub title: String,
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
        assert_eq!(windows, [Window { pid: 4242, class: "firefox".into(), title: "The Daily Prophet".into() }]);
    }

    #[test]
    fn something_that_is_not_a_window_list_says_so() {
        assert!(parse(b"ok").unwrap_err().contains("didn't parse"));
    }
}

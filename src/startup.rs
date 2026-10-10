//! Startup apps: everything that starts when you sign in, in one list,
//! each switched off through whatever owns it — and how long each one
//! *actually* took, where Task Manager can only say "High".
//!
//! Three sources, because a Hyprland session has three:
//!
//! - **XDG autostart** (`/etc/xdg/autostart`, `~/.config/autostart`):
//!   what apps install to start themselves. uwsm hands these to systemd
//!   as `app-<id>@autostart.service`, so each has a running state and a
//!   measured start time. Switched off the way the spec says — an
//!   override in `~/.config/autostart` with `Hidden=true` — never by
//!   touching the system file.
//! - **systemd user units** enabled for your login: switched through
//!   systemd, as the Services tab does.
//! - **Hyprland's own** startup programs, which Settings' Session page
//!   keeps in `session.toml` and writes into the Lua config. Listed here
//!   and edited there: that page owns that file and its regeneration.
//!   Lines written by hand in `hyprland.lua` are the person's own and
//!   are not read here.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The fields of a `.desktop` file's `[Desktop Entry]` that autostart
/// reads — unlocalised, as the spec's fallbacks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Desktop {
    pub name: String,
    pub comment: String,
    pub exec: String,
    pub icon: String,
    pub hidden: bool,
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
    /// GNOME's `X-GNOME-Autostart-enabled=false`, which systemd's
    /// generator honours too.
    pub gnome_disabled: bool,
}

pub fn parse_desktop(text: &str) -> Desktop {
    let mut d = Desktop::default();
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let list = |v: &str| v.split(';').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        match key.trim() {
            "Name" => d.name = value.to_owned(),
            "Comment" => d.comment = value.to_owned(),
            "Exec" => d.exec = value.to_owned(),
            "Icon" => d.icon = value.to_owned(),
            "Hidden" => d.hidden = value.trim() == "true",
            "OnlyShowIn" => d.only_show_in = list(value),
            "NotShowIn" => d.not_show_in = list(value),
            "X-GNOME-Autostart-enabled" => d.gnome_disabled = value.trim() == "false",
            _ => {}
        }
    }
    d
}

impl Desktop {
    /// Whether this desktop (`XDG_CURRENT_DESKTOP`, colon-separated) is
    /// one it starts in at all. One it never starts in is not listed: it
    /// is not something this session starts.
    pub fn applies_to(&self, desktops: &[&str]) -> bool {
        let in_list = |list: &[String]| list.iter().any(|d| desktops.iter().any(|c| c.eq_ignore_ascii_case(d)));
        if !self.only_show_in.is_empty() && !in_list(&self.only_show_in) {
            return false;
        }
        !in_list(&self.not_show_in)
    }

    pub fn enabled(&self) -> bool {
        !self.hidden && !self.gnome_disabled
    }
}

/// systemd's unit-name escaping (`systemd-escape`): letters, digits,
/// `_`, `.` and `:` stay; everything else is `\xNN`.
pub fn escape(name: &str) -> String {
    let mut out = String::new();
    for (i, b) in name.bytes().enumerate() {
        let keep = b.is_ascii_alphanumeric() || b == b'_' || b == b':' || (b == b'.' && i > 0);
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

/// The unit uwsm's generator makes for autostart entry `id`
/// (`nm-applet.desktop` → `app-nm\x2dapplet@autostart.service`).
pub fn autostart_unit(id: &str) -> String {
    format!("app-{}@autostart.service", escape(id.trim_end_matches(".desktop")))
}

/// One XDG autostart entry, with where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Autostart {
    /// `nm-applet.desktop`.
    pub id: String,
    pub desktop: Desktop,
    /// The system file, when there is one.
    pub system: Option<PathBuf>,
    /// The user's file, when there is one — a whole entry of their own,
    /// or an override of the system one.
    pub user: Option<PathBuf>,
}

/// Every autostart entry that applies to `desktops`, the user's file
/// winning over the system one with the same name, as the spec says.
pub fn autostarts(system_dirs: &[PathBuf], user_dir: &Path, desktops: &[&str]) -> Vec<Autostart> {
    let mut by_id: HashMap<String, Autostart> = HashMap::new();
    for dir in system_dirs {
        for (id, path) in desktop_files(dir) {
            by_id.entry(id.clone()).or_insert(Autostart { id, desktop: Desktop::default(), system: Some(path), user: None });
        }
    }
    for (id, path) in desktop_files(user_dir) {
        by_id.entry(id.clone()).or_insert(Autostart { id, desktop: Desktop::default(), system: None, user: None }).user = Some(path);
    }
    let mut out: Vec<Autostart> = by_id
        .into_values()
        .filter_map(|mut a| {
            let file = a.user.as_ref().or(a.system.as_ref())?;
            a.desktop = parse_desktop(&fs::read_to_string(file).ok()?);
            // An override that only hides still borrows the system file's
            // name and command for the list.
            if a.desktop.name.is_empty() {
                if let Some(system) = a.system.as_ref().and_then(|p| fs::read_to_string(p).ok()) {
                    let s = parse_desktop(&system);
                    a.desktop.name = s.name;
                    a.desktop.exec = s.exec;
                    a.desktop.comment = s.comment;
                }
            }
            a.desktop.applies_to(desktops).then_some(a)
        })
        .collect();
    out.sort_by_key(|a| a.desktop.name.to_lowercase());
    out
}

fn desktop_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.ends_with(".desktop").then(|| (name, e.path()))
        })
        .collect()
}

/// Switches an autostart entry on or off for this user.
///
/// Off writes `~/.config/autostart/<id>` with `Hidden=true`: a copy of
/// the system entry when there is one (the spec's way to override), or
/// the user's own file with the line changed. On sets `Hidden=false` in
/// the user's file — never deletes it, because it may be an entry the
/// person wrote and not an override. The write is atomic: a crash leaves
/// the old file or the new one, never half of either.
pub fn set_autostart(entry: &Autostart, user_dir: &Path, on: bool) -> std::io::Result<()> {
    let source = entry.user.as_ref().or(entry.system.as_ref()).ok_or_else(|| std::io::Error::other("no file for this entry"))?;
    let text = fs::read_to_string(source)?;
    let rewritten = with_hidden(&text, !on);
    fs::create_dir_all(user_dir)?;
    let target = user_dir.join(&entry.id);
    let temporary = user_dir.join(format!(".{}.tmp", entry.id));
    fs::write(&temporary, rewritten)?;
    fs::rename(&temporary, &target)
}

/// `text` with `Hidden=` set in `[Desktop Entry]`, every other line as it
/// was.
fn with_hidden(text: &str, hidden: bool) -> String {
    let value = format!("Hidden={hidden}");
    let mut out = Vec::new();
    let mut in_entry = false;
    let mut placed = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_entry && !placed {
                out.push(value.clone());
                placed = true;
            }
            in_entry = trimmed == "[Desktop Entry]";
            out.push(line.to_owned());
            continue;
        }
        if in_entry && trimmed.split_once('=').is_some_and(|(k, _)| k.trim() == "Hidden") {
            if !placed {
                out.push(value.clone());
                placed = true;
            }
            continue;
        }
        out.push(line.to_owned());
    }
    if in_entry && !placed {
        out.push(value);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// `systemd-analyze --user blame`: each unit and how long it took to
/// start, in milliseconds.
pub fn parse_blame(text: &str) -> HashMap<String, u32> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (time, unit) = line.rsplit_once(' ')?;
            Some((unit.to_owned(), duration_ms(time.trim())?))
        })
        .collect()
}

/// `1.289s`, `694ms`, `1min 2.503s`, `3h 2min 1.5s`.
fn duration_ms(text: &str) -> Option<u32> {
    let mut total = 0.0f64;
    for part in text.split_whitespace() {
        let (number, factor) = if let Some(n) = part.strip_suffix("ms") {
            (n, 1.0)
        } else if let Some(n) = part.strip_suffix("min") {
            (n, 60_000.0)
        } else if let Some(n) = part.strip_suffix('h') {
            (n, 3_600_000.0)
        } else if let Some(n) = part.strip_suffix("us").or_else(|| part.strip_suffix("µs")) {
            (n, 0.001)
        } else {
            (part.strip_suffix('s')?, 1000.0)
        };
        total += number.parse::<f64>().ok()? * factor;
    }
    Some(total.round() as u32)
}

/// The measured start times for this user's units, or nothing if
/// `systemd-analyze` did not answer.
pub fn blame() -> HashMap<String, u32> {
    let mut command = std::process::Command::new("systemd-analyze");
    command.args(["--user", "blame", "--no-pager"]);
    match hyprforge_process::output(&mut command, hyprforge_process::TIMEOUT) {
        Ok(out) if out.status.success() => parse_blame(&String::from_utf8_lossy(&out.stdout)),
        _ => HashMap::new(),
    }
}

/// Hyprland's startup programs, as Settings keeps them. A file that will
/// not parse is said, never read as "none".
pub fn hyprland_programs(session_toml: &Path) -> Result<Vec<hyprforge_session::autostart::Program>, String> {
    hyprforge_session::storage::load(session_toml).map(|s| s.autostart.programs).map_err(|e| format!("session.toml couldn't be read: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unlocalised lines of this machine's
    /// `/etc/xdg/autostart/nm-applet.desktop` (it has ninety localised
    /// names besides).
    const NM_APPLET: &str = "[Desktop Entry]\nName=NetworkManager Applet\nName[de]=Netzwerk-Manager-Applet\nComment=Manage your network connections\nIcon=nm-device-wireless\nExec=nm-applet\nTerminal=false\nType=Application\nNotShowIn=KDE;GNOME;\nX-GNOME-UsesNotifications=true\n";
    const DISK_NOTIFY: &str = "[Desktop Entry]\nType=Application\nName=gnome-disk-utility notification plugin for GNOME Settings Daemon\nExec=/usr/lib/gsd-disk-utility-notify\nOnlyShowIn=GNOME;\nNoDisplay=true\n";

    #[test]
    fn an_entry_reads_its_unlocalised_fields_and_desktops() {
        let d = parse_desktop(NM_APPLET);
        assert_eq!((d.name.as_str(), d.exec.as_str()), ("NetworkManager Applet", "nm-applet"));
        assert_eq!(d.not_show_in, ["KDE", "GNOME"]);
        assert!(d.applies_to(&["Hyprland"]) && d.enabled());
        assert!(!d.applies_to(&["GNOME"]));
        assert!(!parse_desktop(DISK_NOTIFY).applies_to(&["Hyprland"]), "GNOME-only is not this session's");
    }

    /// Checked against `systemd-escape` on this machine.
    #[test]
    fn names_escape_as_systemd_escapes_them() {
        assert_eq!(autostart_unit("nm-applet.desktop"), "app-nm\\x2dapplet@autostart.service");
        assert_eq!(escape("org.gnome.SettingsDaemon.DiskUtilityNotify"), "org.gnome.SettingsDaemon.DiskUtilityNotify");
        assert_eq!(escape("my app+x"), "my\\x20app\\x2bx");
        assert_eq!(escape("a_b:c"), "a_b:c");
    }

    #[test]
    fn switching_off_writes_an_override_and_never_touches_the_system_file() {
        let t = tempfile::tempdir().unwrap();
        let system = t.path().join("etc");
        let user = t.path().join("home/autostart");
        fs::create_dir_all(&system).unwrap();
        fs::write(system.join("nm-applet.desktop"), NM_APPLET).unwrap();
        fs::write(system.join("disk.desktop"), DISK_NOTIFY).unwrap();
        let all = autostarts(std::slice::from_ref(&system), &user, &["Hyprland"]);
        assert_eq!(all.len(), 1, "the GNOME-only one isn't listed");
        set_autostart(&all[0], &user, false).unwrap();
        assert_eq!(fs::read_to_string(system.join("nm-applet.desktop")).unwrap(), NM_APPLET, "system file untouched");
        let again = autostarts(std::slice::from_ref(&system), &user, &["Hyprland"]);
        assert!(!again[0].desktop.enabled());
        assert!(again[0].user.is_some());
        let written = fs::read_to_string(user.join("nm-applet.desktop")).unwrap();
        assert!(written.contains("Name[de]=Netzwerk-Manager-Applet"), "every other line kept");
        set_autostart(&again[0], &user, true).unwrap();
        assert!(autostarts(&[system], &user, &["Hyprland"])[0].desktop.enabled());
        assert!(user.join("nm-applet.desktop").exists(), "on never deletes the user's file");
    }

    #[test]
    fn hidden_is_set_once_in_the_entry_group_and_nowhere_else() {
        let text = "[Desktop Entry]\nName=x\nHidden=false\n[Desktop Action new]\nName=New\n";
        let out = with_hidden(text, true);
        assert_eq!(out.matches("Hidden=").count(), 1);
        assert!(out.find("Hidden=true").unwrap() < out.find("[Desktop Action new]").unwrap());
    }

    /// Lines from this machine's `systemd-analyze --user blame`.
    #[test]
    fn blame_reads_every_unit_systemd_prints_into_milliseconds() {
        let b = parse_blame("1.289s wayland-session-waitenv.service\n 694ms wayland-wm@hyprland.desktop.service\n  27ms app-nm\\x2dapplet@autostart.service\n1min 2.503s slow.service\n");
        assert_eq!(b["wayland-session-waitenv.service"], 1289);
        assert_eq!(b["app-nm\\x2dapplet@autostart.service"], 27);
        assert_eq!(b["slow.service"], 62503);
    }
}

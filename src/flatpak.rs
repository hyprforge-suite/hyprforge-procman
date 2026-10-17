//! Which processes belong to which Flatpak app.
//!
//! A Flatpak app runs as several processes — `bwrap` three deep, an
//! `xdg-dbus-proxy`, then the app itself — and on their own they read as
//! loose `bwrap` rows nobody recognises. Flatpak starts each app in a
//! systemd scope named after it, `app-flatpak-<app id>-<n>.scope` (seen
//! here: `app-flatpak-com.github.tchx84.Flatseal-2787579150.scope`, and
//! Blender's in the journal), so the cgroup tree already says which
//! process is whose.
//!
//! The scopes are found by walking each user's `app.slice` rather than
//! by reading every process's `cgroup` file: a handful of directories
//! instead of one more file per process per refresh.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// The app id in a Flatpak scope's name, or `None` for any other unit.
/// The trailing number is the scope's own; an app id may itself hold
/// dashes, so only a final all-digits part is taken off.
pub fn app_id_of_scope(unit: &str) -> Option<String> {
    let rest = unit.strip_prefix("app-flatpak-")?.strip_suffix(".scope")?;
    let id = match rest.rsplit_once('-') {
        Some((id, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => id,
        _ => rest,
    };
    // systemd escapes a dash it did not put there as `\x2d`.
    let id = id.replace("\\x2d", "-");
    (!id.is_empty()).then_some(id)
}

/// What to call an app by its id, when nothing better is to hand: the
/// last part of the reverse-DNS name. `org.blender.Blender` is Blender.
pub fn short_name(app_id: &str) -> &str {
    app_id.rsplit('.').next().unwrap_or(app_id)
}

/// Every process in a Flatpak scope, by pid, with its app id. `root` is
/// the cgroup mount, `/sys/fs/cgroup`. Users' managers keep their apps
/// under `user.slice/user-<uid>.slice/user@<uid>.service/app.slice`,
/// nested a slice or two deeper (`app-graphical.slice`).
pub fn scopes(root: &Path) -> HashMap<i32, String> {
    let mut out = HashMap::new();
    let Ok(users) = fs::read_dir(root.join("user.slice")) else { return out };
    for user in users.flatten() {
        let Ok(managers) = fs::read_dir(user.path()) else { continue };
        for manager in managers.flatten() {
            if manager.file_name().to_string_lossy().starts_with("user@") {
                walk(&manager.path().join("app.slice"), 0, &mut out);
            }
        }
    }
    out
}

fn walk(dir: &Path, depth: usize, out: &mut HashMap<i32, String>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(id) = app_id_of_scope(&name) {
            members(&e.path(), &id, 0, out);
        } else if name.ends_with(".slice") {
            walk(&e.path(), depth + 1, out);
        }
    }
}

/// A scope's processes, and those of any cgroup inside it.
fn members(dir: &Path, id: &str, depth: usize, out: &mut HashMap<i32, String>) {
    if let Ok(procs) = fs::read_to_string(dir.join("cgroup.procs")) {
        for pid in procs.lines().filter_map(|l| l.trim().parse::<i32>().ok()) {
            out.insert(pid, id.to_string());
        }
    }
    if depth < 3 {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                members(&e.path(), id, depth + 1, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both names are this machine's: a live scope and Blender's from
    /// the journal.
    #[test]
    fn a_scope_name_gives_its_app_id() {
        assert_eq!(app_id_of_scope("app-flatpak-com.github.tchx84.Flatseal-2787579150.scope").as_deref(), Some("com.github.tchx84.Flatseal"));
        assert_eq!(app_id_of_scope("app-flatpak-org.blender.Blender-1500123921.scope").as_deref(), Some("org.blender.Blender"));
        assert_eq!(app_id_of_scope("app-flatpak-org.example.Two\\x2dWords-12.scope").as_deref(), Some("org.example.Two-Words"));
        assert_eq!(app_id_of_scope("app-firefox-123.scope"), None);
        assert_eq!(app_id_of_scope("flatpak-session-helper.service"), None, "Flatpak's own helper is no app");
        assert_eq!(short_name("org.blender.Blender"), "Blender");
    }

    /// A cgroup tree laid out the way systemd lays out a user's apps,
    /// with a Flatpak two slices down and a plain app beside it.
    #[test]
    fn the_processes_in_each_flatpak_scope_are_found_and_no_others() {
        let t = tempfile::tempdir().unwrap();
        let apps = t.path().join("user.slice/user-1000.slice/user@1000.service/app.slice");
        let scope = apps.join("app-graphical.slice/app-flatpak-com.github.tchx84.Flatseal-2787579150.scope");
        fs::create_dir_all(&scope).unwrap();
        fs::write(scope.join("cgroup.procs"), "830281\n830302\n830307\n").unwrap();
        let plain = apps.join("app-graphical.slice/app-ghostty-1.scope");
        fs::create_dir_all(&plain).unwrap();
        fs::write(plain.join("cgroup.procs"), "42\n").unwrap();
        let found = scopes(t.path());
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(found[&830307], "com.github.tchx84.Flatseal");
        assert!(!found.contains_key(&42));
        assert!(scopes(&t.path().join("nowhere")).is_empty(), "no cgroup tree is no Flatpaks, not an error");
    }
}

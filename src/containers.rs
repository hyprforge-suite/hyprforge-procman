//! Which processes are in which container.
//!
//! Docker starts each container in a systemd scope of its own,
//! `system.slice/docker-<64 hex id>.scope` (seen here, 2026-10-18: a
//! postgres container, its every process in
//! `docker-a86a270095c4….scope`). So, as for Flatpaks, the cgroup tree
//! says which process is whose, and is walked rather than reading every
//! process's own `cgroup` file each refresh.
//!
//! A container's *name* is Docker's, and only its API says it — which
//! needs the `docker` group. Without it a container is still one row,
//! named by its first process and its short id ("postgres ·
//! a86a270095c4"), so it is never loose processes nobody recognises.
//! Podman is not handled: none was installed to read its cgroups from,
//! and a layout written from memory would test the memory.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// The id in a Docker container's scope name, or `None` for any other
/// unit. Only a full 64-character hex id counts: `docker.service`'s own
/// cgroup is beside the scopes and is no container.
pub fn id_of_scope(unit: &str) -> Option<String> {
    let id = unit.strip_prefix("docker-")?.strip_suffix(".scope")?;
    (id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| id.to_string())
}

/// The twelve characters Docker shows an id as.
pub fn short(id: &str) -> &str {
    &id[..id.len().min(12)]
}

/// Every process in a Docker container, by pid, with the container's
/// id. `root` is the cgroup mount, `/sys/fs/cgroup`.
pub fn scopes(root: &Path) -> HashMap<i32, String> {
    let mut out = HashMap::new();
    let Ok(units) = fs::read_dir(root.join("system.slice")) else { return out };
    for unit in units.flatten() {
        if let Some(id) = id_of_scope(&unit.file_name().to_string_lossy()) {
            members(&unit.path(), &id, 0, &mut out);
        }
    }
    out
}

/// A scope's processes, and those of any cgroup inside it.
fn members(dir: &Path, id: &str, depth: usize, out: &mut HashMap<i32, String>) {
    if let Ok(procs) = fs::read_to_string(dir.join("cgroup.procs")) {
        for pid in procs.lines().filter_map(|l| l.trim().parse::<i32>().ok()) {
            out.insert(pid, id.to_string());
        }
    }
    if depth >= 3 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            members(&e.path(), id, depth + 1, out);
        }
    }
}

/// What Docker calls each container, by full id, from `docker ps
/// --no-trunc --format '{{.ID}}\t{{.Names}}\t{{.Image}}'` — this
/// module's own format, so its shape is the one written here.
pub fn parse_ps(text: &str) -> HashMap<String, (String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let (id, name, image) = (parts.next()?.trim(), parts.next()?.trim(), parts.next()?.trim());
            (!id.is_empty()).then(|| (id.to_string(), (name.to_string(), image.to_string())))
        })
        .collect()
}

/// Docker's names for the running containers — or why not: most often
/// that this user is not in the `docker` group, which is said rather
/// than shown as an empty list.
pub fn names() -> Result<HashMap<String, (String, String)>, String> {
    let mut command = std::process::Command::new("docker");
    command.args(["ps", "--no-trunc", "--format", "{{.ID}}\t{{.Names}}\t{{.Image}}"]);
    let out = hyprforge_process::output(&mut command, hyprforge_process::TIMEOUT)
    .map_err(|e| format!("docker couldn't be asked: {e}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        return Err(if said.contains("permission denied") {
            "Names need access to Docker — this user isn't in the docker group".to_string()
        } else {
            said.trim().to_string()
        });
    }
    Ok(parse_ps(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSTGRES: &str = "a86a270095c4b3d04beef4f9bad7569b682a6f9d4198e2b278e3b1f5369187ac";

    /// This machine's container's scope, and the units beside it.
    #[test]
    fn only_a_containers_own_scope_names_one() {
        assert_eq!(id_of_scope(&format!("docker-{POSTGRES}.scope")).as_deref(), Some(POSTGRES));
        assert_eq!(id_of_scope("docker.service"), None);
        assert_eq!(id_of_scope("docker.socket"), None);
        assert_eq!(id_of_scope("docker-abc.scope"), None, "not an id");
        assert_eq!(short(POSTGRES), "a86a270095c4");
    }

    #[test]
    fn a_containers_processes_are_found_in_its_scope_and_under_it() {
        let root = tempfile::tempdir().unwrap();
        let scope = root.path().join(format!("system.slice/docker-{POSTGRES}.scope"));
        std::fs::create_dir_all(scope.join("inner")).unwrap();
        std::fs::write(scope.join("cgroup.procs"), "2167232\n2167435\n").unwrap();
        std::fs::write(scope.join("inner/cgroup.procs"), "2167500\n").unwrap();
        std::fs::create_dir_all(root.path().join("system.slice/docker.service")).unwrap();
        std::fs::write(root.path().join("system.slice/docker.service/cgroup.procs"), "1500\n").unwrap();
        let found = scopes(root.path());
        assert_eq!(found.len(), 3);
        assert_eq!(found.get(&2167500).map(String::as_str), Some(POSTGRES));
        assert!(!found.contains_key(&1500), "dockerd itself is no container");
    }

    #[test]
    fn docker_ps_lines_give_a_name_and_an_image_by_id() {
        let text = format!("{POSTGRES}\tintrada-db\tpostgres:16\n\n");
        let names = parse_ps(&text);
        assert_eq!(names.get(POSTGRES), Some(&("intrada-db".to_string(), "postgres:16".to_string())));
        assert_eq!(names.len(), 1);
    }

    /// The real cgroup tree, read-only: if a container is running, its
    /// scope is found and every process in it maps to a full id.
    #[test]
    fn this_machines_containers_are_found_if_any_run() {
        let found = scopes(Path::new("/sys/fs/cgroup"));
        if found.is_empty() {
            eprintln!("HYPRFORGE-SKIP: no Docker container is running here");
            return;
        }
        assert!(found.values().all(|id| id.len() == 64));
    }
}

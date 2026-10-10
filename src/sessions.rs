//! Users: who is signed in, where, and what their processes cost — from
//! systemd-logind, with Lock and Sign out.
//!
//! logind lists a `manager` session per user besides the real ones (the
//! user's own systemd); only `user` sessions are sign-ins, and only they
//! are listed. Lock and Sign out go to logind marked as allowed to
//! prompt: your own session needs nothing, another user's asks through
//! the session's polkit agent.

use crate::model::Entry;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub uid: u32,
    pub user: String,
    pub seat: String,
    /// `wayland`, `x11`, `tty`, `unspecified`.
    pub kind: String,
    pub service: String,
    pub tty: String,
    pub active: bool,
    pub idle: bool,
    pub locked: bool,
    pub remote: bool,
    /// Signed in at, microseconds since the epoch.
    pub since_us: u64,
}

#[zbus::proxy(interface = "org.freedesktop.login1.Manager", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1")]
trait Login {
    fn list_sessions(&self) -> zbus::Result<Vec<(String, u32, String, String, zbus::zvariant::OwnedObjectPath)>>;
}

#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1")]
trait LoginSession {
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;
    #[zbus(property, name = "Type")]
    fn kind(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn service(&self) -> zbus::Result<String>;
    #[zbus(property, name = "TTY")]
    fn tty(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn idle_hint(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn remote(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn timestamp(&self) -> zbus::Result<u64>;
}

/// Every sign-in session.
pub async fn list() -> Result<Vec<Session>, String> {
    let system = zbus::Connection::system().await.map_err(|e| format!("couldn't reach logind: {e}"))?;
    let login = LoginProxy::new(&system).await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (id, uid, user, seat, path) in login.list_sessions().await.map_err(|e| format!("logind didn't list sessions: {e}"))? {
        let s = LoginSessionProxy::builder(&system).path(path).map_err(|e| e.to_string())?.build().await.map_err(|e| e.to_string())?;
        // A session can end between the listing and the asking.
        let Ok(class) = s.class().await else { continue };
        if class != "user" {
            continue;
        }
        out.push(Session {
            id,
            uid,
            user,
            seat,
            kind: s.kind().await.unwrap_or_default(),
            service: s.service().await.unwrap_or_default(),
            tty: s.tty().await.unwrap_or_default(),
            active: s.active().await.unwrap_or(false),
            idle: s.idle_hint().await.unwrap_or(false),
            locked: s.locked_hint().await.unwrap_or(false),
            remote: s.remote().await.unwrap_or(false),
            since_us: s.timestamp().await.unwrap_or(0),
        });
    }
    out.sort_by(|a, b| (a.uid, &a.id).cmp(&(b.uid, &b.id)));
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    Lock,
    SignOut,
}

/// Locks or ends session `id`.
pub async fn act(id: String, action: SessionAction) -> Result<(), String> {
    let system = zbus::Connection::system().await.map_err(|e| e.to_string())?;
    let manager = zbus::Proxy::new(&system, "org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager")
        .await
        .map_err(|e| e.to_string())?;
    let method = match action {
        SessionAction::Lock => "LockSession",
        SessionAction::SignOut => "TerminateSession",
    };
    manager
        .call_with_flags::<_, _, ()>(method, zbus::proxy::MethodFlags::AllowInteractiveAuth.into(), &(id.as_str(),))
        .await
        .map(|_| ())
        .map_err(|e| {
            let t = e.to_string();
            if t.contains("AccessDenied") || t.contains("InteractiveAuthorizationRequired") {
                "administrator access wasn't given".to_string()
            } else {
                t
            }
        })
}

/// One user's line: their sessions and the sum of their processes.
#[derive(Debug, Clone, PartialEq)]
pub struct UserRow {
    pub user: String,
    pub sessions: Vec<Session>,
    pub processes: usize,
    /// Known only when every process of theirs has a reading.
    pub cpu: Option<f32>,
    pub private_bytes: u64,
}

/// A row per user with a session or a process, signed-in users first.
pub fn users<'a>(sessions: &[Session], entries: impl Iterator<Item = &'a Entry>) -> Vec<UserRow> {
    let mut rows: BTreeMap<String, UserRow> = BTreeMap::new();
    for s in sessions {
        rows.entry(s.user.clone())
            .or_insert_with(|| UserRow { user: s.user.clone(), sessions: Vec::new(), processes: 0, cpu: Some(0.0), private_bytes: 0 })
            .sessions
            .push(s.clone());
    }
    for e in entries.filter(|e| !e.kernel) {
        let row = rows
            .entry(e.user.clone())
            .or_insert_with(|| UserRow { user: e.user.clone(), sessions: Vec::new(), processes: 0, cpu: Some(0.0), private_bytes: 0 });
        row.processes += 1;
        row.private_bytes += e.private_bytes;
        row.cpu = match (row.cpu, e.cpu) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
    }
    let mut out: Vec<UserRow> = rows.into_values().collect();
    out.sort_by_key(|r| (r.sessions.is_empty(), r.user.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::Id;

    fn entry(user: &str, cpu: Option<f32>, bytes: u64) -> Entry {
        Entry {
            id: Id { pid: 1, start: 1 },
            name: "p".into(),
            command: String::new(),
            user: user.into(),
            own: false,
            cpu,
            disk: None,
            net: None,
            gpu: None,
            gpu_cards: Vec::new(),
            private_bytes: bytes,
            threads: 1,
            state: 'S',
            kernel: false,
            ppid: 0,
            nice: 0,
            ticks: 0,
            resident_bytes: 0,
            detail: None,
            swap_bytes: None,
            vsize: 0,
            processor: 0,
            policy: 0,
        }
    }

    fn session(user: &str) -> Session {
        Session {
            id: "3".into(),
            uid: 1000,
            user: user.into(),
            seat: "seat0".into(),
            kind: "wayland".into(),
            service: "greetd".into(),
            tty: String::new(),
            active: true,
            idle: false,
            locked: false,
            remote: false,
            since_us: 0,
        }
    }

    #[test]
    fn signed_in_users_come_first_with_their_processes_added_up() {
        let entries = [entry("voldemort", Some(2.0), 100), entry("voldemort", Some(3.0), 50), entry("root", Some(1.0), 10)];
        let rows = users(&[session("voldemort")], entries.iter());
        assert_eq!(rows[0].user, "voldemort");
        assert_eq!((rows[0].processes, rows[0].cpu, rows[0].private_bytes), (2, Some(5.0), 150));
        assert_eq!(rows[1].user, "root");
        assert!(rows[1].sessions.is_empty());
    }

    #[test]
    fn a_users_cpu_is_unknown_while_any_process_has_no_reading() {
        let entries = [entry("bellatrix", Some(2.0), 1), entry("bellatrix", None, 1)];
        assert_eq!(users(&[], entries.iter())[0].cpu, None);
    }

    #[tokio::test]
    #[ignore = "asks the real logind"]
    async fn the_real_logind_lists_only_sign_ins() {
        let sessions = list().await.unwrap();
        assert!(sessions.iter().all(|s| !s.user.is_empty()));
        assert!(!sessions.is_empty(), "this test runs inside a sign-in");
    }
}

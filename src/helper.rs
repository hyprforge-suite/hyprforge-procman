//! The privileged helper's service, and the window's client of it.
//!
//! `hyprforge-procman-helper` runs as root on the system bus as
//! `dev.hyprforge.ProcMan1`, started by D-Bus activation when the
//! window first asks and gone again when idle. Whatever it is asked, it
//! asks polkit first whether *the caller* may — the bus tells it who the
//! caller is, so the answer is about a person and not about this
//! process. With polkit's `auth_admin_keep`, the first call prompts
//! (through whichever agent the session runs: hyprforge-polkit draws it
//! as the lock card) and the next few minutes' do not.
//!
//! What it will do is narrow on purpose:
//!
//! - **Arguments are typed.** A process is a pid and its start time; a
//!   signal is one of four; a priority is a number in range. It never
//!   takes a path, a command or a string to run.
//! - **A process is the one that was chosen.** The start time is checked
//!   after a pidfd is open, as `signal.rs` does for the window.
//! - **Some processes are never acted on, administrator or not:** pid 1
//!   (signalling init can panic the kernel), kernel threads, and the
//!   helper itself.
//!
//! The polkit check is behind [`Authority`], so the service's tests run
//! as an ordinary user, against a stand-in that says yes or no, over a
//! socket pair instead of the system bus.

use crate::sample::Id;
use crate::signal;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use zbus::message::Header;
use zbus::zvariant::Value;

pub const BUS_NAME: &str = "dev.hyprforge.ProcMan1";
pub const OBJECT_PATH: &str = "/dev/hyprforge/ProcMan1";

/// polkit actions — `packaging/org.hyprforge.procman.policy` defines
/// each, all `auth_admin_keep` for an active local session.
pub mod action {
    /// Reading what only root can: every process's I/O and open files.
    pub const READ_ALL: &str = "org.hyprforge.procman.read-all";
    /// Ending, killing, freezing or resuming another user's process.
    pub const SIGNAL: &str = "org.hyprforge.procman.signal";
    /// Changing another user's priority, or raising any.
    pub const RENICE: &str = "org.hyprforge.procman.renice";
}

/// The signals the helper sends; any other number is refused.
pub const SIGNALS: [i32; 4] = [libc::SIGTERM, libc::SIGKILL, libc::SIGSTOP, libc::SIGCONT];

pub type Checking = Pin<Box<dyn Future<Output = Result<bool, String>> + Send>>;

/// Whether the caller `sender` (a unique bus name) may do `action`.
/// `interactive` lets polkit ask them for a password; without it, an
/// action that would need one is simply not authorized.
pub trait Authority: Send + Sync + 'static {
    fn check(&self, sender: String, action: &'static str, interactive: bool) -> Checking;
}

/// polkitd, on the system bus.
pub struct Polkit {
    pub system: zbus::Connection,
}

impl Authority for Polkit {
    fn check(&self, sender: String, action: &'static str, interactive: bool) -> Checking {
        let system = self.system.clone();
        Box::pin(async move {
            let subject: (&str, HashMap<&str, Value>) = ("system-bus-name", HashMap::from([("name", Value::from(sender.as_str()))]));
            let details: HashMap<&str, &str> = HashMap::new();
            // AllowUserInteraction = 1.
            let flags: u32 = u32::from(interactive);
            let reply = system
                .call_method(
                    Some("org.freedesktop.PolicyKit1"),
                    "/org/freedesktop/PolicyKit1/Authority",
                    Some("org.freedesktop.PolicyKit1.Authority"),
                    "CheckAuthorization",
                    &(subject, action, details, flags, ""),
                )
                .await
                .map_err(|e| format!("couldn't ask polkit: {e}"))?;
            let (authorized, _challenge, _details): (bool, bool, HashMap<String, String>) =
                reply.body().deserialize().map_err(|e| format!("polkit's answer didn't parse: {e}"))?;
            Ok(authorized)
        })
    }
}

pub use crate::proc::Usage;

/// The service. `last_call` is when anything last asked, for the
/// binary's idle exit.
pub struct Service {
    authority: Arc<dyn Authority>,
    pub last_call: Arc<Mutex<Instant>>,
    proc_root: std::path::PathBuf,
    /// The recorder's journal directory, from its config.
    pub history: std::path::PathBuf,
    /// The network counter, loaded on the first `NetUsage` and kept for
    /// as long as the helper runs — or why it couldn't be.
    #[cfg(feature = "netbpf")]
    net: Mutex<Option<Result<crate::netbpf::Counter, String>>>,
}

impl Service {
    #[cfg(feature = "netbpf")]
    async fn net_counts(&self, processes: Vec<(i32, u64)>) -> zbus::fdo::Result<Vec<(i32, u64, u64, u64)>> {
        let mut net = self.net.lock().await;
        let counter = match net.get_or_insert_with(crate::netbpf::Counter::start) {
            Ok(c) => c,
            Err(e) => return Err(zbus::fdo::Error::NotSupported(e.clone())),
        };
        // Only the ids that still name the process asked about.
        let wanted: Vec<(i32, u64)> = processes.into_iter().filter(|&(pid, start)| crate::proc::start_time(pid) == Some(start)).collect();
        let running: std::collections::HashSet<u32> = std::fs::read_dir(&self.proc_root)
            .map(|d| d.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect())
            .unwrap_or_default();
        let counts = counter.read(&running);
        Ok(wanted.into_iter().map(|(pid, start)| {
            let n = counts.get(&(pid as u32)).copied().unwrap_or_default();
            (pid, start, n.rx_bytes, n.tx_bytes)
        }).collect())
    }

    #[cfg(not(feature = "netbpf"))]
    async fn net_counts(&self, _processes: Vec<(i32, u64)>) -> zbus::fdo::Result<Vec<(i32, u64, u64, u64)>> {
        Err(zbus::fdo::Error::NotSupported("this helper was built without network counting (netbpf)".into()))
    }

    pub fn new(authority: Arc<dyn Authority>) -> Service {
        let history = crate::recorder::load_config(std::path::Path::new(crate::recorder::CONFIG_PATH)).map(|c| c.directory).unwrap_or_else(|_| crate::recorder::Config::default().directory);
        Service {
            authority,
            last_call: Arc::new(Mutex::new(Instant::now())),
            proc_root: "/proc".into(),
            history,
            #[cfg(feature = "netbpf")]
            net: Mutex::new(None),
        }
    }

    async fn allow(&self, header: &Header<'_>, action: &'static str, interactive: bool) -> zbus::fdo::Result<()> {
        *self.last_call.lock().await = Instant::now();
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        match self.authority.check(sender, action, interactive).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(zbus::fdo::Error::AccessDenied("not authorized".into())),
            Err(e) => Err(zbus::fdo::Error::Failed(e)),
        }
    }

    /// The refusals that hold for an administrator too.
    fn never(&self, id: Id) -> zbus::fdo::Result<()> {
        if id.pid <= 1 {
            return Err(zbus::fdo::Error::InvalidArgs("pid 1 and below are never signalled".into()));
        }
        if id.pid == std::process::id() as i32 {
            return Err(zbus::fdo::Error::InvalidArgs("the helper doesn't act on itself".into()));
        }
        let stat = std::fs::read_to_string(self.proc_root.join(id.pid.to_string()).join("stat")).unwrap_or_default();
        if crate::proc::is_kernel_thread(&stat) {
            return Err(zbus::fdo::Error::InvalidArgs("kernel threads are never signalled".into()));
        }
        Ok(())
    }
}

fn refused(e: signal::Refused) -> zbus::fdo::Error {
    match e {
        signal::Refused::Gone => zbus::fdo::Error::Failed("gone".into()),
        other => zbus::fdo::Error::Failed(other.to_string()),
    }
}

#[zbus::interface(name = "dev.hyprforge.ProcMan1")]
impl Service {
    /// Sends `signal` (SIGTERM, SIGKILL, SIGSTOP or SIGCONT) to the
    /// process `pid` that started at `start`.
    async fn signal(&self, pid: i32, start: u64, signal: i32, interactive: bool, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<()> {
        if !SIGNALS.contains(&signal) {
            return Err(zbus::fdo::Error::InvalidArgs(format!("signal {signal} isn't one the helper sends")));
        }
        let id = Id { pid, start };
        self.never(id)?;
        self.allow(&header, action::SIGNAL, interactive).await?;
        signal::send(id, signal).map_err(refused)
    }

    /// Sets the process's nice value, -20 (first) to 19 (last).
    async fn set_nice(&self, pid: i32, start: u64, nice: i32, interactive: bool, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<()> {
        if !(-20..=19).contains(&nice) {
            return Err(zbus::fdo::Error::InvalidArgs(format!("{nice} is outside -20 to 19")));
        }
        let id = Id { pid, start };
        self.never(id)?;
        self.allow(&header, action::RENICE, interactive).await?;
        signal::renice(id, nice).map_err(refused)
    }

    /// The days the recorder's journal holds, with their sizes. The names
    /// say only which days were recorded; reading one is `open_history`.
    async fn history_days(&self, interactive: bool, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<Vec<(String, u64)>> {
        self.allow(&header, action::READ_ALL, interactive).await?;
        match crate::journal::days(&self.history) {
            Ok(days) => Ok(days),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(zbus::fdo::Error::Failed(e.to_string())),
        }
    }

    /// One day's journal, opened read-only by root and handed over as a
    /// file descriptor: the caller reads that file and no other, and
    /// never needs root to. `day` is `YYYY-MM-DD` and nothing else.
    async fn open_history(&self, day: String, interactive: bool, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        if !crate::journal::is_day(&day) {
            return Err(zbus::fdo::Error::InvalidArgs(format!("{day:?} isn't a day")));
        }
        self.allow(&header, action::READ_ALL, interactive).await?;
        let file = std::fs::File::open(self.history.join(format!("{day}.hfr"))).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(std::os::fd::OwnedFd::from(file).into())
    }

    /// I/O and open files for each process asked about, as root reads
    /// them. A process that ended, or whose start time no longer matches,
    /// is left out of the answer.
    async fn usage(&self, processes: Vec<(i32, u64)>, interactive: bool, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<Vec<Usage>> {
        self.allow(&header, action::READ_ALL, interactive).await?;
        Ok(processes.into_iter().filter_map(|(pid, start)| crate::proc::usage(&self.proc_root, Id { pid, start })).collect())
    }

    /// Network bytes in and out for each of `processes` (pid, start), as
    /// (pid, start, received, sent), counted since the helper first loaded
    /// its eBPF counter. A process that ended, or a pid that now names
    /// another, is left out. Its own method rather than more fields on
    /// `Usage`, so a window newer than the running helper hears "unknown
    /// method" and shows a dash, instead of failing to read `Usage` at all.
    async fn net_usage(&self, processes: Vec<(i32, u64)>, interactive: bool, #[zbus(header)] header: Header<'_>) -> zbus::fdo::Result<Vec<(i32, u64, u64, u64)>> {
        self.allow(&header, action::READ_ALL, interactive).await?;
        self.net_counts(processes).await
    }
}

/// The window's side.
#[zbus::proxy(interface = "dev.hyprforge.ProcMan1", default_service = "dev.hyprforge.ProcMan1", default_path = "/dev/hyprforge/ProcMan1")]
pub trait ProcMan {
    fn signal(&self, pid: i32, start: u64, signal: i32, interactive: bool) -> zbus::Result<()>;
    fn set_nice(&self, pid: i32, start: u64, nice: i32, interactive: bool) -> zbus::Result<()>;
    fn usage(&self, processes: Vec<(i32, u64)>, interactive: bool) -> zbus::Result<Vec<Usage>>;
    fn net_usage(&self, processes: Vec<(i32, u64)>, interactive: bool) -> zbus::Result<Vec<(i32, u64, u64, u64)>>;
    fn history_days(&self, interactive: bool) -> zbus::Result<Vec<(String, u64)>>;
    fn open_history(&self, day: &str, interactive: bool) -> zbus::Result<zbus::zvariant::OwnedFd>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    /// polkit, as a yes or a no, remembering what it was asked.
    struct Says {
        yes: bool,
        asked: std::sync::Mutex<Vec<(&'static str, bool)>>,
    }

    impl Authority for Says {
        fn check(&self, _sender: String, action: &'static str, interactive: bool) -> Checking {
            self.asked.lock().unwrap().push((action, interactive));
            let yes = self.yes;
            Box::pin(async move { Ok(yes) })
        }
    }

    /// The service on one end of a socket pair, its proxy on the other.
    async fn connect(yes: bool) -> (ProcManProxy<'static>, Arc<Says>, zbus::Connection, zbus::Connection) {
        let says = Arc::new(Says { yes, asked: Default::default() });
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let guid = zbus::Guid::generate();
        let server = zbus::connection::Builder::unix_stream(a)
            .server(guid)
            .unwrap()
            .p2p()
            .serve_at(OBJECT_PATH, Service::new(says.clone()))
            .unwrap()
            .build();
        let client = zbus::connection::Builder::unix_stream(b).p2p().build();
        let (server, client) = tokio::try_join!(server, client).unwrap();
        let proxy = ProcManProxy::builder(&client).destination(BUS_NAME).unwrap().path(OBJECT_PATH).unwrap().build().await.unwrap();
        (proxy, says, server, client)
    }

    fn sleeper() -> (std::process::Child, Id) {
        let child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id() as i32;
        (child, Id { pid, start: crate::proc::start_time(pid).unwrap() })
    }

    /// Every action the helper checks is defined in the policy it ships,
    /// and none of them is ever granted without a password.
    #[test]
    fn the_shipped_policy_defines_every_action_and_grants_none_freely() {
        let policy = include_str!("../packaging/org.hyprforge.procman.policy");
        for id in [action::READ_ALL, action::SIGNAL, action::RENICE] {
            assert!(policy.contains(&format!("<action id=\"{id}\">")), "{id} is not in the policy");
        }
        assert_eq!(policy.matches("<allow_active>auth_admin_keep</allow_active>").count(), 3);
        assert!(!policy.contains(">yes<"), "nothing is allowed without authenticating");
    }

    #[tokio::test]
    async fn an_authorized_signal_reaches_the_chosen_process() {
        let (proxy, says, _s, _c) = connect(true).await;
        let (mut child, id) = sleeper();
        proxy.signal(id.pid, id.start, libc::SIGTERM, true).await.unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
        assert_eq!(says.asked.lock().unwrap().as_slice(), [(action::SIGNAL, true)]);
    }

    #[tokio::test]
    async fn without_polkits_yes_nothing_is_sent() {
        let (proxy, _says, _s, _c) = connect(false).await;
        let (mut child, id) = sleeper();
        let err = proxy.signal(id.pid, id.start, libc::SIGKILL, true).await.unwrap_err();
        assert!(err.to_string().contains("not authorized"), "{err}");
        assert!(child.try_wait().unwrap().is_none(), "still running");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// Refused before polkit is even asked: these hold for an
    /// administrator too.
    #[tokio::test]
    async fn pid_one_a_kernel_thread_and_a_stray_signal_are_refused_for_anyone() {
        let (proxy, says, _s, _c) = connect(true).await;
        let init = crate::proc::start_time(1).unwrap();
        assert!(proxy.signal(1, init, libc::SIGTERM, true).await.is_err());
        let kthreadd = crate::proc::start_time(2).unwrap();
        assert!(proxy.signal(2, kthreadd, libc::SIGKILL, true).await.is_err());
        let (mut child, id) = sleeper();
        assert!(proxy.signal(id.pid, id.start, libc::SIGHUP, true).await.is_err(), "not one of the four");
        assert!(proxy.set_nice(id.pid, id.start, -21, true).await.is_err(), "out of range");
        assert!(says.asked.lock().unwrap().is_empty(), "polkit was never asked");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[tokio::test]
    async fn a_reused_pid_is_never_signalled_even_when_authorized() {
        let (proxy, _says, _s, _c) = connect(true).await;
        let (mut child, id) = sleeper();
        assert!(proxy.signal(id.pid, id.start + 1, libc::SIGKILL, true).await.is_err());
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[tokio::test]
    async fn a_days_journal_comes_back_as_a_file_and_nothing_else_does() {
        let t = tempfile::tempdir().unwrap();
        let header = crate::journal::Header { hostname: "h".into(), cpus: 1, page_size: 4096, ticks_per_second: 100, interval_seconds: 10, command_lines: true };
        let mut w = crate::journal::Writer::open(&t.path().join("2026-10-10.hfr"), &header).unwrap();
        w.write(&crate::journal::Frame { at_ms: 5, ..Default::default() }).unwrap();
        drop(w);
        let says = Arc::new(Says { yes: true, asked: Default::default() });
        let mut service = Service::new(says.clone());
        service.history = t.path().to_path_buf();
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let server = zbus::connection::Builder::unix_stream(a).server(zbus::Guid::generate()).unwrap().p2p().serve_at(OBJECT_PATH, service).unwrap().build();
        let client = zbus::connection::Builder::unix_stream(b).p2p().build();
        let (_server, client) = tokio::try_join!(server, client).unwrap();
        let proxy = ProcManProxy::builder(&client).destination(BUS_NAME).unwrap().path(OBJECT_PATH).unwrap().build().await.unwrap();
        assert_eq!(proxy.history_days(false).await.unwrap(), [("2026-10-10".to_string(), std::fs::metadata(t.path().join("2026-10-10.hfr")).unwrap().len())]);
        let fd: std::os::fd::OwnedFd = proxy.open_history("2026-10-10", false).await.unwrap().into();
        let mut reader = crate::journal::Reader::from_file(std::fs::File::from(fd)).unwrap();
        assert_eq!(reader.frame(0).unwrap().at_ms, 5);
        assert!(proxy.open_history("../../etc/shadow", false).await.is_err());
        assert!(says.asked.lock().unwrap().iter().all(|(a, _)| *a == action::READ_ALL));
    }

    #[tokio::test]
    async fn usage_reads_io_and_open_files_and_leaves_out_the_gone() {
        let (proxy, says, _s, _c) = connect(true).await;
        let me = std::process::id() as i32;
        let start = crate::proc::start_time(me).unwrap();
        let usage = proxy.usage(vec![(me, start), (me, start + 1)], false).await.unwrap();
        assert_eq!(usage.len(), 1);
        assert!(usage[0].open_files >= 3, "stdin, stdout, stderr at least");
        assert_eq!(says.asked.lock().unwrap().as_slice(), [(action::READ_ALL, false)]);
    }

    #[tokio::test]
    async fn lowering_ones_own_priority_works_through_the_helper() {
        let (proxy, _says, _s, _c) = connect(true).await;
        let (mut child, id) = sleeper();
        proxy.set_nice(id.pid, id.start, 10, false).await.unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", id.pid)).unwrap();
        let nice: i32 = stat.rsplit(')').next().unwrap().split_whitespace().nth(16).unwrap().parse().unwrap();
        assert_eq!(nice, 10);
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

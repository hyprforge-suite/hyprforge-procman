//! `hyprforge-procman-efficiency`: efficiency mode, as a user service.
//!
//! Listens to Hyprland's event socket, and after any burst of window
//! events weighs down the CPU of every app with no window on screen and
//! gives it back to any that comes into view — see
//! `hyprforge_procman::efficiency` for what and why. No polling: an idle
//! desktop sends no events, and this does nothing.
//!
//! Off unless switched on: the unit is installed, never enabled by the
//! package. Stopping it (SIGTERM, SIGINT) gives back every weight it
//! set before it exits, so switching efficiency mode off leaves nothing
//! behind; so does Hyprland closing its socket.

use hyprforge_procman::efficiency::{self, Step};
use hyprforge_procman::limits::{Limiter, Plan, UserSystemd};
use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::time::Duration;

/// Events arrive in bursts — a workspace switch is several — and one
/// pass answers the lot.
const SETTLE: Duration = Duration::from_millis(300);

enum Event {
    Changed,
    /// Hyprland closed its socket: the session is ending.
    Closed,
    /// SIGTERM or SIGINT.
    Quit,
}

/// `$XDG_RUNTIME_DIR/hypr/<signature>/.socket2.sock`. The signature is
/// the environment's when the session exported it, and otherwise the one
/// instance `hyprctl instances -j` lists — never the newest directory
/// under `hypr/`, which a nested compositor can win (CLAUDE.md).
fn socket_path() -> Result<std::path::PathBuf, String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR isn't set")?;
    let signature = match std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        Ok(s) if !s.is_empty() => s,
        _ => {
            let out = hyprforge_process::output(std::process::Command::new("hyprctl").args(["instances", "-j"]), hyprforge_process::TIMEOUT)
                .map_err(|e| format!("hyprctl didn't answer: {e}"))?;
            let list: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).map_err(|e| format!("hyprctl's instance list didn't parse: {e}"))?;
            match list.as_slice() {
                [one] => one["instance"].as_str().ok_or("hyprctl named no instance")?.to_string(),
                [] => return Err("no Hyprland is running".into()),
                _ => return Err("more than one Hyprland is running and the session didn't say which is its own".into()),
            }
        }
    };
    Ok(std::path::Path::new(&runtime).join("hypr").join(signature).join(".socket2.sock"))
}

/// The windowed apps and their plans, read from `/proc`, Hyprland and the
/// cgroup tree. Blocking, so it runs off the runtime's one thread.
fn gather(me: u32) -> Result<(Vec<efficiency::Seen>, HashMap<i32, Plan>), String> {
    let snapshot = hyprforge_procman::proc::ProcFs::default().snapshot().map_err(|e| format!("couldn't read /proc: {e}"))?;
    let windows = hyprforge_procman::windows::read()?;
    let shown = hyprforge_procman::windows::read_shown()?;
    let flatpaks = hyprforge_procman::flatpak::scopes(std::path::Path::new("/sys/fs/cgroup"));
    let table = hyprforge_procman::model::table(&snapshot, &Default::default(), &windows, &flatpaks, &Default::default(), me, &mut |_| String::new());
    let seen = efficiency::seen(&table, &windows, &shown);
    let plans = seen.iter().map(|a| (a.lead, efficiency::plan_for(a, me))).collect();
    Ok((seen, plans))
}

async fn pass(me: u32, weighed: &mut HashSet<String>) {
    let gathered = tokio::task::spawn_blocking(move || gather(me)).await.unwrap_or_else(|e| Err(e.to_string()));
    let (seen, plans) = match gathered {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!("couldn't see the desktop this time: {e}");
            return;
        }
    };
    for step in efficiency::steps(&seen, &plans, weighed) {
        let result = match &step {
            Step::Weigh(unit) => UserSystemd.set(unit.clone(), efficiency::weight(true)).await.map(|()| {
                weighed.insert(unit.clone());
            }),
            Step::Move { name, pids } => UserSystemd
                .new_scope(name.clone(), "Off screen, given less CPU by efficiency mode".into(), pids.iter().map(|p| *p as u32).collect(), efficiency::weight(true))
                .await
                .map(|()| {
                    weighed.insert(name.clone());
                }),
            Step::Restore(unit) => UserSystemd.set(unit.clone(), efficiency::weight(false)).await.map(|()| {
                weighed.remove(unit);
            }),
        };
        match result {
            Ok(()) => tracing::info!(?step, "done"),
            Err(e) => tracing::warn!(?step, "systemd refused: {e}"),
        }
    }
    for unit in efficiency::orphans(&plans, weighed) {
        // Gone with its app, usually, and then systemd says so: either
        // way it is no longer this service's to give back.
        let _ = UserSystemd.set(unit.clone(), efficiency::weight(false)).await;
        weighed.remove(&unit);
    }
}

async fn restore_all(weighed: &mut HashSet<String>) {
    for unit in weighed.drain() {
        if let Err(e) = UserSystemd.set(unit.clone(), efficiency::weight(false)).await {
            tracing::warn!(%unit, "couldn't give its CPU weight back: {e}");
        }
    }
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .without_time()
        .init();
    // SIGTERM and SIGINT are taken by one thread with sigwait, so that
    // stopping the service gives the weights back first. Blocked before
    // any other thread exists, so every thread inherits the mask.
    // SAFETY: plain libc signal-set calls on a set this owns.
    let set = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    };
    // `--dry-run`: one pass over the desktop as it is, saying what would
    // be done and doing none of it — the way to see it without letting
    // it touch anything.
    if std::env::args().any(|a| a == "--dry-run") {
        // SAFETY: getuid cannot fail.
        let me = unsafe { libc::getuid() };
        return match gather(me) {
            Ok((seen, plans)) => {
                for app in &seen {
                    println!("{:<24} {:<10} {:?}", app.name, if app.on_screen { "on screen" } else { "off" }, plans.get(&app.lead));
                }
                for step in efficiency::steps(&seen, &plans, &HashSet::new()) {
                    println!("would: {step:?}");
                }
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let path = match socket_path() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("{e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let stream = match std::os::unix::net::UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(path = %path.display(), "couldn't listen to Hyprland: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let events = tx.clone();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stream).lines() {
            if line.is_err() || events.send(Event::Changed).is_err() {
                break;
            }
        }
        let _ = events.send(Event::Closed);
    });
    std::thread::spawn(move || {
        let mut sig = 0;
        // SAFETY: `set` is a valid, initialised signal set.
        unsafe { libc::sigwait(&set, &mut sig) };
        let _ = tx.send(Event::Quit);
    });
    // SAFETY: getuid cannot fail.
    let me = unsafe { libc::getuid() };
    let runtime = match hyprforge_procman::bound::service_runtime() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("couldn't start: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let mut weighed = HashSet::new();
        tracing::info!("efficiency mode is on");
        pass(me, &mut weighed).await;
        while let Some(event) = rx.recv().await {
            match event {
                Event::Changed => {
                    tokio::time::sleep(SETTLE).await;
                    let mut ending = false;
                    while let Ok(more) = rx.try_recv() {
                        ending |= !matches!(more, Event::Changed);
                    }
                    if ending {
                        break;
                    }
                    pass(me, &mut weighed).await;
                }
                Event::Closed | Event::Quit => break,
            }
        }
        restore_all(&mut weighed).await;
        tracing::info!("efficiency mode is off; every weight it set is given back");
    });
    std::process::ExitCode::SUCCESS
}

//! hyprforge-procman-recorder — samples the whole machine every ten
//! seconds into a private journal, as root, for replay later.
//!
//! Started by `hyprforge-procman-recorder.service` (see `packaging/`),
//! which sandboxes it to reading `/proc` and `/sys` and writing its own
//! directory. Each tick is `hyprforge_procman::recorder`; this file is
//! the clock, the exits listener and a clean stop.
//!
//! `--once [--dir PATH]` takes one sample and exits — for checking by
//! hand, as any user (it then sees what that user may read).

use hyprforge_procman::exits::{self, Exit, Taskstats};
use hyprforge_procman::recorder::{self, Config, Recorder};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .without_time() // journald stamps it
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name);
    let value = |name: &str| flag(name).and_then(|i| args.get(i + 1)).cloned();

    let path = value("--config").unwrap_or_else(|| recorder::CONFIG_PATH.into());
    let mut config = match recorder::load_config(std::path::Path::new(&path)) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("{e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Some(dir) = value("--dir") {
        config.directory = dir.into();
    }
    if flag("--once").is_some() {
        return match Recorder::new(config).tick(chrono::Local::now(), Vec::new()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!("couldn't take a sample: {e}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    run(config)
}

fn run(config: Config) -> std::process::ExitCode {
    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let restore = if config.delay_accounting {
        match recorder::set_delay_accounting(true) {
            Ok(was) => Some(was),
            Err(e) => {
                tracing::warn!("couldn't switch on delay accounting, so block I/O delay reads 0: {e}");
                None
            }
        }
    } else {
        None
    };

    let exits: Arc<Mutex<Vec<Exit>>> = Arc::default();
    let listener = {
        let exits = exits.clone();
        std::thread::Builder::new().name("exits".into()).spawn(move || listen(exits))
    };
    if let Err(e) = listener {
        tracing::warn!("couldn't start listening for exits: {e}");
    }

    let interval = Duration::from_secs(u64::from(config.interval));
    let mut recorder = Recorder::new(config);
    tracing::info!("recording every {}s into {}", interval.as_secs(), recorder.config.directory.display());
    while !STOP.load(Ordering::SeqCst) {
        wait_for_next(interval);
        if STOP.load(Ordering::SeqCst) {
            break;
        }
        let heard = std::mem::take(&mut *exits.lock().unwrap_or_else(|e| e.into_inner()));
        if let Err(e) = recorder.tick(chrono::Local::now(), heard) {
            // One failed sample is a gap in the journal, not a reason to
            // stop recording.
            tracing::warn!("couldn't record this sample: {e}");
        }
    }
    if let Some(was) = restore {
        if let Err(e) = recorder::set_delay_accounting(was) {
            tracing::warn!("couldn't put delay accounting back: {e}");
        }
    }
    tracing::info!("stopped");
    std::process::ExitCode::SUCCESS
}

/// Sleeps to the next multiple of `interval` on the wall clock, so
/// samples land on :00, :10, :20 and line up with anything else
/// recorded — in short steps, so a stop is heard promptly.
fn wait_for_next(interval: Duration) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let step = interval.as_millis().max(1);
    let wait = Duration::from_millis((step - now.as_millis() % step) as u64);
    let until = std::time::Instant::now() + wait;
    while !STOP.load(Ordering::SeqCst) {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_millis(250)));
    }
}

/// Hears every process exit, for as long as the recorder runs. Needs
/// `CAP_NET_ADMIN`; without it, short-lived processes are missed and the
/// log says so once.
fn listen(exits: Arc<Mutex<Vec<Exit>>>) {
    let socket = match Taskstats::open().and_then(|s| s.listen(&exits::all_cpus()).map(|()| s)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("not listening for exits, so processes that end between samples are missed: {e}");
            return;
        }
    };
    while !STOP.load(Ordering::SeqCst) {
        match socket.exits() {
            Ok(heard) if !heard.is_empty() => exits.lock().unwrap_or_else(|e| e.into_inner()).extend(heard),
            Ok(_) => {}
            // The socket's five-second timeout, so the stop flag is seen.
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => {
                tracing::warn!("listening for exits failed: {e}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

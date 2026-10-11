//! hyprforge-procman-helper — the privileged half of hyprforge-procman.
//!
//! Started by D-Bus activation as root (see
//! `packaging/dev.hyprforge.ProcMan1.service`), serves
//! `dev.hyprforge.ProcMan1` on the system bus, and exits after a minute
//! with no calls: there is nothing to keep running for, and a root
//! process that is not running cannot be misused. Everything it does is
//! in `hyprforge_procman::helper`; this file only connects it.

use hyprforge_procman::helper::{Polkit, Service, BUS_NAME, OBJECT_PATH};
use std::sync::Arc;
use std::time::Duration;

const IDLE: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .init();
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> zbus::Result<()> {
    let system = zbus::Connection::system().await?;
    let service = Service::new(Arc::new(Polkit { system: system.clone() }));
    let (last_call, asking) = (service.last_call.clone(), service.asking.clone());
    system.object_server().at(OBJECT_PATH, service).await?;
    system.request_name(BUS_NAME).await?;
    loop {
        tokio::time::sleep(IDLE / 4).await;
        // Not while someone is at a password prompt: exiting under it
        // turns the password they typed into "disconnected".
        let waiting = asking.load(std::sync::atomic::Ordering::SeqCst) > 0;
        if !waiting && last_call.lock().await.elapsed() >= IDLE {
            tracing::info!("idle for {}s, exiting", IDLE.as_secs());
            return Ok(());
        }
    }
}

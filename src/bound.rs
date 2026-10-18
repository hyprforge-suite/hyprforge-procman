//! A limit on every wait for another process over D-Bus.
//!
//! Neither zbus nor dbus-broker puts a time limit on a method call, so a
//! helper, systemd or logind that stops answering would leave the call —
//! and whatever waits on it — waiting for good: the window's refresh, in
//! the worst case, which is guarded so as never to run two at once and
//! so would never run again. The suite's rule is that nothing waits on
//! another process without a bound; this is that bound for D-Bus.

use std::future::Future;
use std::time::Duration;

/// A call no person is part of: a listing, a reading.
pub const QUICK: Duration = Duration::from_secs(10);

/// A call that may put a password prompt in front of someone. A person
/// takes their time, but not forever; past this the prompt is still up
/// and the window says the call never came back, rather than waiting on
/// it for good.
pub const PROMPT: Duration = Duration::from_secs(300);

/// `call`, or an error naming `what` once `limit` has passed without an
/// answer.
pub async fn within<T>(limit: Duration, what: &str, call: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    match tokio::time::timeout(limit, call).await {
        Ok(answer) => answer,
        Err(_) => Err(format!("{what} didn't answer within {}s", limit.as_secs())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_call_that_never_answers_ends_as_an_error_that_names_who() {
        let never = std::future::pending::<Result<(), String>>();
        let e = within(Duration::from_millis(20), "systemd", never).await.unwrap_err();
        assert_eq!(e, "systemd didn't answer within 0s");
        assert_eq!(within(QUICK, "systemd", async { Ok::<_, String>(7) }).await, Ok(7));
    }
}

/// The runtime a user service here runs on: one thread, with both of
/// tokio's drivers. Efficiency mode and the rules service were each built
/// with timers only, and the first D-Bus call panicked — "A Tokio 1.x
/// context was found, but IO is disabled" — and systemd restarted them
/// 31 times before anyone looked. Their dry runs never reached the bus,
/// so nothing caught it; the live test below does.
pub fn service_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()
}

#[cfg(test)]
mod runtime_tests {
    /// Read-only: the services' runtime reaches the session bus, as both
    /// services do on their first call to systemd.
    #[test]
    #[ignore = "live (systemd): changes nothing"]
    fn a_services_runtime_can_reach_the_session_bus() {
        let rt = super::service_runtime().unwrap();
        rt.block_on(async {
            let conn = zbus::Connection::session().await.expect("the session bus, through this runtime");
            assert!(conn.unique_name().is_some());
        });
    }
}

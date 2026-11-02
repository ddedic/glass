//! Quit the host after it has been unused for a while, so it can run only on demand.
//!
//! Off unless `--exit-after-idle` or `glass config exit-after-idle` says otherwise.
//! "In use" is a WebTransport session (a viewer, its stream, or its terminal, including
//! one still signing in) or an agent tool call still running. A session is never ended
//! to meet the timer: the quiet clock starts again only after the last one leaves.
//!
//! The process exits with status 0. The macOS agent is installed with
//! `KeepAlive` / `SuccessfulExit` false, and the Linux unit uses `Restart=on-failure`,
//! so a clean exit stays stopped until Glass is started again.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

static OPEN: AtomicUsize = AtomicUsize::new(0);

/// Held for as long as a session or an agent tool call is running.
pub struct SessionGuard;

impl SessionGuard {
    pub fn enter() -> Self {
        OPEN.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        OPEN.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A viewer, stream, terminal, or agent tool call is still open.
pub fn busy() -> bool {
    OPEN.load(Ordering::Acquire) > 0
}

/// CLI `--exit-after-idle` wins when it is present. `0` means stay up.
pub fn minutes(cli: Option<u32>, saved: u32) -> u32 {
    cli.unwrap_or(saved)
}

/// `glass config exit-after-idle`: a number of minutes, `0`, or `off`. Empty clears it.
pub fn parse_minutes(value: &str) -> Result<u32> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("off") {
        return Ok(0);
    }
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("expected a number of minutes, or off"))
}

/// Quiet clock. `limit` of zero never fires. While `busy`, the clock is cleared.
#[derive(Debug, Default)]
pub struct Clock {
    quiet_since: Option<Instant>,
}

impl Clock {
    /// True only after a continuous idle stretch of at least `limit`.
    pub fn should_exit(&mut self, busy: bool, limit: Duration, now: Instant) -> bool {
        if limit.is_zero() || busy {
            self.quiet_since = None;
            return false;
        }
        let since = *self.quiet_since.get_or_insert(now);
        now.saturating_duration_since(since) >= limit
    }
}

/// Watches in the background. `--exit-after-idle 0` forces this run to stay up.
/// With no flag, `glass config exit-after-idle` is read as the host runs.
pub fn watch(cli: Option<u32>) {
    if cli == Some(0) {
        return;
    }
    tokio::spawn(async move {
        let mut clock = Clock::default();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let minutes = minutes(cli, crate::settings::load().exit_after_idle);
            let limit = Duration::from_secs(u64::from(minutes) * 60);
            let now = Instant::now();
            if !clock.should_exit(busy(), limit, now) {
                continue;
            }
            // One more look, so a session that arrived while the clock was checked
            // starts a new quiet period instead of being cut off.
            if busy() {
                clock.should_exit(true, limit, now);
                continue;
            }
            tracing::info!("no viewer, stream or terminal session for {minutes} minutes; exiting");
            std::process::exit(0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_overrides_saved_minutes() {
        assert_eq!(minutes(None, 0), 0);
        assert_eq!(minutes(None, 15), 15);
        assert_eq!(minutes(Some(30), 15), 30);
        assert_eq!(minutes(Some(0), 15), 0);
    }

    #[test]
    fn parses_minutes_off_and_rejects_junk() {
        assert_eq!(parse_minutes("").unwrap(), 0);
        assert_eq!(parse_minutes("off").unwrap(), 0);
        assert_eq!(parse_minutes("Off").unwrap(), 0);
        assert_eq!(parse_minutes("0").unwrap(), 0);
        assert_eq!(parse_minutes(" 45 ").unwrap(), 45);
        assert!(parse_minutes("soon").is_err());
        assert!(parse_minutes("-1").is_err());
        assert!(parse_minutes("1.5").is_err());
    }

    #[test]
    fn an_open_session_is_busy_until_it_ends() {
        assert!(!busy());
        let first = SessionGuard::enter();
        let second = SessionGuard::enter();
        assert!(busy());
        drop(first);
        assert!(busy());
        drop(second);
        assert!(!busy());
    }

    #[test]
    fn idle_clock_waits_out_the_full_quiet_period_and_never_cuts_a_session() {
        let start = Instant::now();
        let limit = Duration::from_secs(10 * 60);
        let mut clock = Clock::default();

        assert!(!clock.should_exit(false, Duration::ZERO, start));
        assert!(!clock.should_exit(false, limit, start));
        assert!(!clock.should_exit(false, limit, start + limit - Duration::from_secs(1)));
        assert!(clock.should_exit(false, limit, start + limit));

        // A session at the deadline clears the clock. It then needs a full new period.
        assert!(!clock.should_exit(true, limit, start + limit));
        let again = start + limit + Duration::from_secs(1);
        assert!(!clock.should_exit(false, limit, again));
        assert!(!clock.should_exit(false, limit, again + limit - Duration::from_secs(1)));
        assert!(clock.should_exit(false, limit, again + limit));
    }
}

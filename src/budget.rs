use std::time::{Duration, Instant};

const SECS_PER_MINUTE: u64 = 60;

/// Test-only hook: when set, its value (in seconds) pre-ages the budget's start
/// instant so an integration test can trip a non-zero cap deterministically
/// without sleeping for real minutes. Read once at [`Budget::start`]. Never set
/// in production; documented in the design's implementation notes.
const PREAGE_ENV: &str = "RWL_BUDGET_PREAGE_SECS";

/// Why the wall-clock budget was exceeded.
///
/// Carries both the elapsed minutes and the configured cap so the surfaced
/// message is machine- and human-readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    /// Whole minutes elapsed when the cap tripped.
    pub elapsed_minutes: u64,
    /// The configured cap, in minutes (`max-total-minutes`).
    pub cap_minutes: u64,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "wall-clock budget exceeded: {} min elapsed >= {} min cap (max-total-minutes)",
            self.elapsed_minutes, self.cap_minutes
        )
    }
}

/// Wall-clock budget for a whole run.
///
/// Tracks the run start instant (monotonic) and the configured cap. A cap of
/// `0` means unlimited and never trips.
#[derive(Debug, Clone)]
pub struct Budget {
    start: Instant,
    cap_minutes: u64,
}

impl Budget {
    /// Start a budget now with the given cap (`0` = unlimited).
    ///
    /// Honors the `RWL_BUDGET_PREAGE_SECS` test hook: if set to a number of
    /// seconds, the start instant is pushed that far into the past so a
    /// non-zero cap can be tripped deterministically in an integration test.
    pub fn start(cap_minutes: u64) -> Self {
        let now = Instant::now();
        let start = match std::env::var(PREAGE_ENV).ok().and_then(|v| v.parse::<u64>().ok()) {
            Some(secs) => now.checked_sub(Duration::from_secs(secs)).unwrap_or(now),
            None => now,
        };
        Self { start, cap_minutes }
    }

    /// Whether the budget has been exceeded as of now.
    ///
    /// Returns `Some(reason)` only when the cap is non-zero AND the elapsed
    /// wall-clock time has reached or passed it; `None` otherwise.
    pub fn exceeded(&self) -> Option<Reason> {
        self.exceeded_at(self.start.elapsed())
    }

    /// Threshold check against an explicit elapsed duration (testable, no clock).
    fn exceeded_at(&self, elapsed: Duration) -> Option<Reason> {
        if self.cap_minutes == 0 {
            return None;
        }
        let cap = Duration::from_secs(self.cap_minutes * SECS_PER_MINUTE);
        if elapsed >= cap {
            Some(Reason {
                elapsed_minutes: elapsed.as_secs() / SECS_PER_MINUTE,
                cap_minutes: self.cap_minutes,
            })
        } else {
            None
        }
    }

    /// Construct a budget whose start is pre-aged by `elapsed`, for deterministic
    /// tests that must trip the cap without sleeping for real minutes.
    #[cfg(test)]
    fn started_ago(cap_minutes: u64, elapsed: Duration) -> Self {
        Self {
            start: Instant::now() - elapsed,
            cap_minutes,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_unlimited_cap_never_exceeds() {
        let budget = Budget::start(0);
        assert!(budget.exceeded_at(Duration::from_secs(10_000)).is_none());
    }

    #[test]
    fn test_under_cap_returns_none() {
        let budget = Budget::start(5);
        // 4m59s < 5m
        assert!(budget.exceeded_at(Duration::from_secs(299)).is_none());
    }

    #[test]
    fn test_at_cap_returns_some() {
        let budget = Budget::start(5);
        let reason = budget.exceeded_at(Duration::from_secs(300)).unwrap();
        assert_eq!(reason.cap_minutes, 5);
        assert_eq!(reason.elapsed_minutes, 5);
    }

    #[test]
    fn test_over_cap_returns_some() {
        let budget = Budget::start(1);
        let reason = budget.exceeded_at(Duration::from_secs(125)).unwrap();
        assert_eq!(reason.cap_minutes, 1);
        assert_eq!(reason.elapsed_minutes, 2);
    }

    #[test]
    fn test_pre_aged_start_trips_via_real_clock() {
        // started_ago places the start far in the past so the live elapsed()
        // already exceeds a non-zero cap, exactly the trick the integration
        // test relies on.
        let budget = Budget::started_ago(1, Duration::from_secs(10 * SECS_PER_MINUTE));
        assert!(budget.exceeded().is_some());
    }

    #[test]
    fn test_reason_display_mentions_cap_and_elapsed() {
        let reason = Reason {
            elapsed_minutes: 7,
            cap_minutes: 5,
        };
        let s = reason.to_string();
        assert!(s.contains('7'));
        assert!(s.contains('5'));
        assert!(s.contains("max-total-minutes"));
    }
}

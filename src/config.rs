//! Timing constants. Display refresh and sampling stay independent.

use std::time::Duration;

/// How often the sampler reads CPU times and publishes a snapshot.
pub const CPU_SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

/// How often process memory counters are refreshed.
/// Between refreshes the previous real reading is reused; it is not replaced with zero.
pub const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// How often the UI thread copies the latest snapshot. It does not sample.
#[cfg(feature = "ui")]
pub const UI_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

/// How long full snapshots stay in the in-memory ring buffer.
pub const HISTORY_WINDOW: Duration = Duration::from_secs(60);

/// Upper bound on sampler sleep slices so shutdown does not wait a full interval.
pub const SHUTDOWN_POLL: Duration = Duration::from_millis(50);

/// Number of CPU samples that cover [`HISTORY_WINDOW`].
pub fn history_capacity() -> usize {
    let window = HISTORY_WINDOW.as_nanos();
    let step = CPU_SAMPLE_INTERVAL.as_nanos().max(1);
    let slots = window.div_ceil(step);
    usize::try_from(slots).unwrap_or(1).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixty_seconds_at_500ms_is_120_slots() {
        assert_eq!(history_capacity(), 120);
        assert_eq!(
            CPU_SAMPLE_INTERVAL.as_millis() * 120,
            HISTORY_WINDOW.as_millis()
        );
    }
}

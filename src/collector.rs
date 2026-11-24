//! Collector extension points.
//!
//! Phase 1 ships one system probe (Windows CPU and memory, or an explicit
//! unsupported probe elsewhere). Later phases add [`MetricEnricher`]s for
//! disk, network, GPU, and ETW without changing the UI types.

use crate::model::RawSample;

/// Reads the process list and the system counters the aggregator understands.
pub trait SystemProbe: Send {
    fn probe(&mut self) -> RawSample;

    /// Thread count of this process from the same enumeration, when it was seen.
    /// The default is unavailable, not zero.
    fn self_threads(&self) -> crate::metric::MetricValue<u32> {
        crate::metric::MetricValue::Unavailable
    }
}

/// Fills metrics the base probe leaves as [`crate::metric::MetricValue::Unsupported`].
///
/// Contract:
/// - Per-process `disk_*_bytes` and `net_*_bytes` are cumulative counters.
/// - Per-process GPU percents are already normalized for the latest interval.
///   Engine percents must not be added together by the enricher to fake a total.
/// - System-wide disk and network fields on [`RawSample`] are interval rates,
///   because physical device throughput is not the sum of process I/O.
/// - Do not replace an existing [`crate::metric::MetricValue::Value`] with a guess.
pub trait MetricEnricher: Send {
    fn id(&self) -> &'static str;
    fn enrich(&mut self, sample: &mut RawSample);
}

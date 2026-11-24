//! Cheap readings of this process, plus sizes PULSE itself can count.
//!
//! Operating-system counters stay [`MetricValue::Unavailable`] until a real
//! query succeeds. A configured ETW ceiling is not a measurement of bytes
//! resident in this process: those buffers live in kernel pool. The ring
//! figure is an upper bound (latest snapshot times occupied slots), not a
//! working-set sample.

use crate::metric::MetricValue;
use crate::model::{LogicalCpuSample, ProcessSnapshot, Snapshot};

/// What one publish can say about PULSE without allocating a report.
#[derive(Clone, Debug, PartialEq)]
pub struct SelfDiagnostics {
    pub private_working_set: MetricValue<u64>,
    pub working_set: MetricValue<u64>,
    pub private_bytes: MetricValue<u64>,
    pub handles: MetricValue<u32>,
    pub threads: MetricValue<u32>,
    /// Whole sampler step, including aggregation.
    pub sample_nanos: u64,
    pub probe_nanos: u64,
    pub enrich_nanos: u64,
    /// Times a PDH query was dropped after repeated hard failures.
    pub pdh_reopens: u32,
    pub ring_slots: u32,
    /// Upper bound on history retention. Shared names are counted once per slot.
    pub ring_bytes: u64,
    /// `MaximumBuffers * BufferSize` configured for `PULSE-CPU`. Not resident private bytes.
    pub etw_buffer_ceiling: MetricValue<u64>,
}

impl SelfDiagnostics {
    pub fn unavailable() -> Self {
        Self {
            private_working_set: MetricValue::Unavailable,
            working_set: MetricValue::Unavailable,
            private_bytes: MetricValue::Unavailable,
            handles: MetricValue::Unavailable,
            threads: MetricValue::Unavailable,
            sample_nanos: 0,
            probe_nanos: 0,
            enrich_nanos: 0,
            pdh_reopens: 0,
            ring_slots: 0,
            ring_bytes: 0,
            etw_buffer_ceiling: MetricValue::Unavailable,
        }
    }
}

/// Bytes a single snapshot owns, ignoring allocator overhead and `Arc` sharing.
pub fn estimate_snapshot_bytes(snapshot: &Snapshot) -> u64 {
    let mut bytes = std::mem::size_of::<Snapshot>() as u64;
    bytes = bytes.saturating_add(
        snapshot.processes.len() as u64 * std::mem::size_of::<ProcessSnapshot>() as u64,
    );
    for process in &snapshot.processes {
        bytes = bytes.saturating_add(process.name.len() as u64);
        bytes = bytes.saturating_add(
            process.gpus.len() as u64 * std::mem::size_of::<crate::model::ProcessGpu>() as u64,
        );
    }
    for cpu in &snapshot.cpu_detail.processors {
        bytes = bytes.saturating_add(logical_cpu_bytes(cpu));
    }
    bytes
}

fn logical_cpu_bytes(cpu: &LogicalCpuSample) -> u64 {
    let mut bytes = std::mem::size_of::<LogicalCpuSample>() as u64;
    bytes = bytes.saturating_add(
        cpu.processes.len() as u64 * std::mem::size_of::<crate::model::LogicalCpuProcess>() as u64,
    );
    for process in &cpu.processes {
        bytes = bytes.saturating_add(process.name.len() as u64);
    }
    bytes
}

/// `slots` copies of `snapshot`, as an upper bound when older slots are no larger.
pub fn estimate_ring_bytes(snapshot: &Snapshot, slots: usize) -> u64 {
    estimate_snapshot_bytes(snapshot).saturating_mul(u64::try_from(slots).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProcessKey, RawProcess, SharedName};
    use std::time::{Instant, SystemTime};

    #[test]
    fn an_unread_meter_is_not_a_zero_reading() {
        let meter = SelfDiagnostics::unavailable();
        assert_eq!(meter.private_working_set, MetricValue::Unavailable);
        assert_eq!(meter.working_set, MetricValue::Unavailable);
        assert_eq!(meter.private_bytes, MetricValue::Unavailable);
        assert_eq!(meter.handles, MetricValue::Unavailable);
        assert_eq!(meter.threads, MetricValue::Unavailable);
        assert_eq!(meter.etw_buffer_ceiling, MetricValue::Unavailable);
        assert_ne!(meter.handles, MetricValue::Value(0));
    }

    #[test]
    fn the_ring_bound_scales_with_slots_and_names() {
        let mut snapshot = Snapshot {
            sequence: 1,
            monotonic: Instant::now(),
            taken_at: SystemTime::UNIX_EPOCH,
            interval: None,
            logical_processors: MetricValue::Value(8),
            cpu_percent: MetricValue::Value(1.0),
            memory_used_bytes: MetricValue::Value(1),
            memory_total_bytes: MetricValue::Value(2),
            disk_read_bps: MetricValue::Unavailable,
            disk_write_bps: MetricValue::Unavailable,
            net_send_bps: MetricValue::Unavailable,
            net_recv_bps: MetricValue::Unavailable,
            gpu_3d_percent: MetricValue::Unavailable,
            gpu_encode_percent: MetricValue::Unavailable,
            gpu_decode_percent: MetricValue::Unavailable,
            vram_dedicated_bytes: MetricValue::Unavailable,
            vram_shared_bytes: MetricValue::Unavailable,
            disks: Vec::new(),
            adapters: Vec::new(),
            gpus: Vec::new(),
            process_net: crate::model::ProcessNetMode::Attributed,
            gpu_presence: crate::model::GpuPresence::Unknown,
            processes: Vec::new(),
            warning: None,
            warning_quiet: false,
            cpu_detail: crate::model::CpuDetailSnapshot::off(),
        };
        let empty = estimate_ring_bytes(&snapshot, 120);
        snapshot.processes.push(ProcessSnapshot {
            key: ProcessKey {
                pid: 1,
                creation_time_100ns: 1,
            },
            parent_pid: 0,
            name: SharedName::from("a-long-process-name.exe"),
            cpu_percent: MetricValue::Value(1.0),
            memory_bytes: MetricValue::Value(1),
            disk_read_bps: MetricValue::Unavailable,
            disk_write_bps: MetricValue::Unavailable,
            net_send_bps: MetricValue::Unavailable,
            net_recv_bps: MetricValue::Unavailable,
            gpu_3d_percent: MetricValue::Unavailable,
            gpu_encode_percent: MetricValue::Unavailable,
            gpu_decode_percent: MetricValue::Unavailable,
            vram_dedicated_bytes: MetricValue::Unavailable,
            vram_shared_bytes: MetricValue::Unavailable,
            gpus: Vec::new(),
        });
        let _ = RawProcess::new(1, 0, "ignored");
        let one = estimate_ring_bytes(&snapshot, 1);
        let full = estimate_ring_bytes(&snapshot, 120);
        assert!(one > empty / 120);
        assert_eq!(full, one.saturating_mul(120));
        assert!(full > one);
    }
}

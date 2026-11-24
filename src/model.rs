//! Shared data types. Collectors fill [`RawSample`]. The aggregator turns two
//! raw samples into a [`Snapshot`] the UI and the history buffer can store.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use crate::metric::MetricValue;

/// A process or image name shared across snapshots.
///
/// The text is immutable. Cloning copies the `Arc`, not the bytes, so a
/// 60-second ring can keep one allocation per distinct name. Identity is still
/// [`ProcessKey`]; two processes may share a name without sharing a key.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SharedName(Arc<str>);

impl SharedName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn as_arc(&self) -> &Arc<str> {
        &self.0
    }
}

impl From<&str> for SharedName {
    fn from(name: &str) -> Self {
        Self(Arc::from(name))
    }
}

impl From<String> for SharedName {
    fn from(name: String) -> Self {
        Self(Arc::from(name))
    }
}

impl AsRef<str> for SharedName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for SharedName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SharedName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl PartialEq<&str> for SharedName {
    fn eq(&self, other: &&str) -> bool {
        self.0.as_ref() == *other
    }
}

impl PartialEq<str> for SharedName {
    fn eq(&self, other: &str) -> bool {
        self.0.as_ref() == other
    }
}

/// Identity that survives PID reuse.
///
/// `creation_time_100ns` is the process creation FILETIME (100 ns since 1601).
/// `0` means the creation time could not be read; that key is weaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    pub pid: u32,
    pub creation_time_100ns: u64,
}

/// One process as read from the operating system, before rates are computed.
#[derive(Clone, Debug, PartialEq)]
pub struct RawProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    pub creation_time_100ns: Option<u64>,
    /// Sum of kernel and user time, in 100 ns units. Cumulative.
    pub cpu_time_100ns: MetricValue<u64>,
    /// Private working set, in bytes. A gauge.
    pub memory_bytes: MetricValue<u64>,
    /// Cumulative counters. The aggregator turns them into bytes per second.
    pub disk_read_bytes: MetricValue<u64>,
    pub disk_write_bytes: MetricValue<u64>,
    pub net_send_bytes: MetricValue<u64>,
    pub net_recv_bytes: MetricValue<u64>,
    /// Interval percentages already normalized by a future GPU collector.
    /// Not summed across engines here.
    pub gpu_3d_percent: MetricValue<f64>,
    pub gpu_encode_percent: MetricValue<f64>,
    pub gpu_decode_percent: MetricValue<f64>,
    pub vram_dedicated_bytes: MetricValue<u64>,
    pub vram_shared_bytes: MetricValue<u64>,
    /// Per-adapter engine and memory readings. Empty when the collector has
    /// not split adapters. The scalar GPU fields remain the cross-adapter max.
    pub gpus: Vec<ProcessGpu>,
}

impl RawProcess {
    /// Disk, network, GPU, and VRAM start as [`MetricValue::Unsupported`].
    /// CPU and memory start as [`MetricValue::Unavailable`] until a read succeeds.
    pub fn new(pid: u32, parent_pid: u32, name: impl Into<String>) -> Self {
        Self {
            pid,
            parent_pid,
            name: name.into(),
            creation_time_100ns: None,
            cpu_time_100ns: MetricValue::Unavailable,
            memory_bytes: MetricValue::Unavailable,
            disk_read_bytes: MetricValue::Unsupported,
            disk_write_bytes: MetricValue::Unsupported,
            net_send_bytes: MetricValue::Unsupported,
            net_recv_bytes: MetricValue::Unsupported,
            gpu_3d_percent: MetricValue::Unsupported,
            gpu_encode_percent: MetricValue::Unsupported,
            gpu_decode_percent: MetricValue::Unsupported,
            vram_dedicated_bytes: MetricValue::Unsupported,
            vram_shared_bytes: MetricValue::Unsupported,
            gpus: Vec::new(),
        }
    }

    pub fn key(&self) -> ProcessKey {
        ProcessKey {
            pid: self.pid,
            creation_time_100ns: self.creation_time_100ns.unwrap_or(0),
        }
    }
}

/// Keep PULSE's own CPU and memory even when Toolhelp omitted this PID or
/// `OpenProcess` on it failed. A reading that is already present is left alone.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn ensure_self_row(
    processes: &mut Vec<RawProcess>,
    pid: u32,
    name: &str,
    creation: Option<u64>,
    cpu: MetricValue<u64>,
    memory: MetricValue<u64>,
) {
    let has_cpu = matches!(cpu, MetricValue::Value(_));
    let has_memory = matches!(memory, MetricValue::Value(_));
    if let Some(existing) = processes.iter_mut().find(|process| process.pid == pid) {
        if has_cpu && !matches!(existing.cpu_time_100ns, MetricValue::Value(_)) {
            if existing.creation_time_100ns.is_none() {
                existing.creation_time_100ns = creation;
            }
            existing.cpu_time_100ns = cpu;
        }
        if has_memory && !matches!(existing.memory_bytes, MetricValue::Value(_)) {
            existing.memory_bytes = memory;
        }
        return;
    }
    if !has_cpu && !has_memory {
        return;
    }
    let label = if name.is_empty() { "PULSE.exe" } else { name };
    let mut process = RawProcess::new(pid, 0, label);
    process.creation_time_100ns = creation;
    if has_cpu {
        process.cpu_time_100ns = cpu;
    }
    if has_memory {
        process.memory_bytes = memory;
    }
    processes.push(process);
}

/// One polling pass, still in counter form where a rate needs a previous sample.
#[derive(Clone, Debug, PartialEq)]
pub struct RawSample {
    pub taken_at: Instant,
    pub wall_time: SystemTime,
    pub logical_processors: MetricValue<u32>,
    /// Busy processor time across all logical processors, 100 ns units. Cumulative.
    pub system_busy_100ns: MetricValue<u64>,
    /// Kernel + user time including idle, 100 ns units. Cumulative.
    /// This is the capacity denominator for CPU percents.
    pub system_total_100ns: MetricValue<u64>,
    pub memory_total_bytes: MetricValue<u64>,
    pub memory_available_bytes: MetricValue<u64>,
    /// System-wide rates and gauges copied through by the aggregator.
    /// Physical disk throughput is not the sum of per-process I/O, so these
    /// are separate from the per-process counters.
    pub system_disk_read_bps: MetricValue<f64>,
    pub system_disk_write_bps: MetricValue<f64>,
    pub system_net_send_bps: MetricValue<f64>,
    pub system_net_recv_bps: MetricValue<f64>,
    pub system_gpu_3d_percent: MetricValue<f64>,
    pub system_gpu_encode_percent: MetricValue<f64>,
    pub system_gpu_decode_percent: MetricValue<f64>,
    pub system_vram_dedicated_bytes: MetricValue<u64>,
    pub system_vram_shared_bytes: MetricValue<u64>,
    /// Physical disks. Not the sum of per-process I/O.
    pub disks: Vec<NamedRate>,
    /// Network adapters. Not per-process attribution.
    pub adapters: Vec<NamedRate>,
    pub gpus: Vec<GpuAdapter>,
    pub process_net: ProcessNetMode,
    pub gpu_presence: GpuPresence,
    pub processes: Vec<RawProcess>,
    pub warning: Option<String>,
    /// Filled by the ETW enricher or the mock. The aggregator copies it through.
    /// It does not feed [`Snapshot::cpu_percent`].
    pub cpu_detail: CpuDetailSnapshot,
}

impl RawSample {
    pub fn empty(taken_at: Instant, wall_time: SystemTime) -> Self {
        Self {
            taken_at,
            wall_time,
            logical_processors: MetricValue::Unsupported,
            system_busy_100ns: MetricValue::Unsupported,
            system_total_100ns: MetricValue::Unsupported,
            memory_total_bytes: MetricValue::Unsupported,
            memory_available_bytes: MetricValue::Unsupported,
            system_disk_read_bps: MetricValue::Unsupported,
            system_disk_write_bps: MetricValue::Unsupported,
            system_net_send_bps: MetricValue::Unsupported,
            system_net_recv_bps: MetricValue::Unsupported,
            system_gpu_3d_percent: MetricValue::Unsupported,
            system_gpu_encode_percent: MetricValue::Unsupported,
            system_gpu_decode_percent: MetricValue::Unsupported,
            system_vram_dedicated_bytes: MetricValue::Unsupported,
            system_vram_shared_bytes: MetricValue::Unsupported,
            disks: Vec::new(),
            adapters: Vec::new(),
            gpus: Vec::new(),
            process_net: ProcessNetMode::Attributed,
            gpu_presence: GpuPresence::Unknown,
            processes: Vec::new(),
            warning: None,
            cpu_detail: CpuDetailSnapshot::off(),
        }
    }

    pub fn note(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.warning = Some(match self.warning.take() {
            Some(existing) if existing.contains(text) => existing,
            Some(existing) => format!("{existing}; {text}"),
            None => text.to_string(),
        });
    }
}

/// One process after rates have been computed for a single interval.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessSnapshot {
    pub key: ProcessKey,
    pub parent_pid: u32,
    pub name: SharedName,
    pub cpu_percent: MetricValue<f64>,
    pub memory_bytes: MetricValue<u64>,
    pub disk_read_bps: MetricValue<f64>,
    pub disk_write_bps: MetricValue<f64>,
    pub net_send_bps: MetricValue<f64>,
    pub net_recv_bps: MetricValue<f64>,
    pub gpu_3d_percent: MetricValue<f64>,
    pub gpu_encode_percent: MetricValue<f64>,
    pub gpu_decode_percent: MetricValue<f64>,
    pub vram_dedicated_bytes: MetricValue<u64>,
    pub vram_shared_bytes: MetricValue<u64>,
    /// Per-adapter readings. The fields above stay the max across adapters
    /// (VRAM stays the existing per-process total) so the process table does
    /// not mix engines by summing them. Ranking for one adapter uses this list
    /// and never substitutes another adapter's number.
    pub gpus: Vec<ProcessGpu>,
}

/// One process on one GPU adapter. Engines are already folded with a max.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessGpu {
    pub luid: u64,
    pub gpu_3d_percent: MetricValue<f64>,
    pub gpu_encode_percent: MetricValue<f64>,
    pub gpu_decode_percent: MetricValue<f64>,
    pub vram_dedicated_bytes: MetricValue<u64>,
    pub vram_shared_bytes: MetricValue<u64>,
}

/// Aggregated view of the machine at one publish.
///
/// The process list is the full set, not a top-N. Ranking is a display concern.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub sequence: u64,
    /// Monotonic sample time. Selection uses this, not the wall clock, so a
    /// system-clock change cannot move the chosen point onto another sample.
    pub monotonic: Instant,
    /// Wall clock captured at the same sample, for the label only.
    pub taken_at: SystemTime,
    pub interval: Option<std::time::Duration>,
    pub logical_processors: MetricValue<u32>,
    pub cpu_percent: MetricValue<f64>,
    pub memory_used_bytes: MetricValue<u64>,
    pub memory_total_bytes: MetricValue<u64>,
    pub disk_read_bps: MetricValue<f64>,
    pub disk_write_bps: MetricValue<f64>,
    pub net_send_bps: MetricValue<f64>,
    pub net_recv_bps: MetricValue<f64>,
    pub gpu_3d_percent: MetricValue<f64>,
    pub gpu_encode_percent: MetricValue<f64>,
    pub gpu_decode_percent: MetricValue<f64>,
    pub vram_dedicated_bytes: MetricValue<u64>,
    pub vram_shared_bytes: MetricValue<u64>,
    pub disks: Vec<NamedRate>,
    pub adapters: Vec<NamedRate>,
    pub gpus: Vec<GpuAdapter>,
    pub process_net: ProcessNetMode,
    pub gpu_presence: GpuPresence,
    pub processes: Vec<ProcessSnapshot>,
    pub warning: Option<String>,
    /// Per-logical-processor breakdown. Empty and [`CpuDetailState::Off`] unless
    /// detailed analysis is on. This never replaces [`Self::cpu_percent`].
    pub cpu_detail: CpuDetailSnapshot,
}

/// One physical disk or one network adapter. These rates are not per-process.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedRate {
    pub name: String,
    pub read_bps: MetricValue<f64>,
    pub write_bps: MetricValue<f64>,
}

/// One GPU adapter. Utilizations are already folded with a max, never a sum.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuAdapter {
    pub luid: u64,
    pub name: String,
    pub util_3d: MetricValue<f64>,
    pub util_encode: MetricValue<f64>,
    pub util_decode: MetricValue<f64>,
    pub dedicated_bytes: MetricValue<u64>,
    pub shared_bytes: MetricValue<u64>,
}

/// How per-process network bytes should be read.
///
/// Adapter totals can exist while per-process attribution cannot. `NeedsAdmin`
/// means the process cells are not a measured zero; the UI says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProcessNetMode {
    /// Per-process counters are meaningful (including Unavailable for one process).
    #[default]
    Attributed,
    /// This build does not attribute flows to a PID without elevation.
    NeedsAdmin,
}

/// Whether a GPU collector found any adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GpuPresence {
    /// No GPU collector has run.
    #[default]
    Unknown,
    /// The collector ran and found no engine.
    None,
    Present,
}

/// Performance-tab selection. Values match the order of the UI buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceId {
    Cpu = 0,
    Memory = 1,
    Disk = 2,
    Network = 3,
    Gpu3d = 4,
    GpuEncode = 5,
    GpuDecode = 6,
    VramDedicated = 7,
    VramShared = 8,
}

impl ResourceId {
    pub fn from_index(index: i32) -> Self {
        match index {
            0 => Self::Cpu,
            1 => Self::Memory,
            2 => Self::Disk,
            3 => Self::Network,
            4 => Self::Gpu3d,
            5 => Self::GpuEncode,
            6 => Self::GpuDecode,
            7 => Self::VramDedicated,
            _ => Self::VramShared,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "cpu" => Self::Cpu,
            "memory" => Self::Memory,
            "disk" => Self::Disk,
            "network" => Self::Network,
            "gpu3d" => Self::Gpu3d,
            "gpu-encode" => Self::GpuEncode,
            "gpu-decode" => Self::GpuDecode,
            "vram" => Self::VramDedicated,
            "shared" => Self::VramShared,
            _ => return None,
        })
    }
}

#[cfg(test)]
impl ProcessSnapshot {
    pub fn bare(pid: u32, creation: u64, name: &str) -> Self {
        Self {
            key: ProcessKey {
                pid,
                creation_time_100ns: creation,
            },
            parent_pid: 0,
            name: SharedName::from(name),
            cpu_percent: MetricValue::Unsupported,
            memory_bytes: MetricValue::Unsupported,
            disk_read_bps: MetricValue::Unsupported,
            disk_write_bps: MetricValue::Unsupported,
            net_send_bps: MetricValue::Unsupported,
            net_recv_bps: MetricValue::Unsupported,
            gpu_3d_percent: MetricValue::Unsupported,
            gpu_encode_percent: MetricValue::Unsupported,
            gpu_decode_percent: MetricValue::Unsupported,
            vram_dedicated_bytes: MetricValue::Unsupported,
            vram_shared_bytes: MetricValue::Unsupported,
            gpus: Vec::new(),
        }
    }
}

#[cfg(test)]
impl Snapshot {
    pub fn bare(sequence: u64, monotonic: Instant) -> Self {
        Self {
            sequence,
            monotonic,
            taken_at: SystemTime::UNIX_EPOCH,
            interval: Some(std::time::Duration::from_millis(500)),
            logical_processors: MetricValue::Value(8),
            cpu_percent: MetricValue::Unavailable,
            memory_used_bytes: MetricValue::Unavailable,
            memory_total_bytes: MetricValue::Unavailable,
            disk_read_bps: MetricValue::Unsupported,
            disk_write_bps: MetricValue::Unsupported,
            net_send_bps: MetricValue::Unsupported,
            net_recv_bps: MetricValue::Unsupported,
            gpu_3d_percent: MetricValue::Unsupported,
            gpu_encode_percent: MetricValue::Unsupported,
            gpu_decode_percent: MetricValue::Unsupported,
            vram_dedicated_bytes: MetricValue::Unsupported,
            vram_shared_bytes: MetricValue::Unsupported,
            disks: Vec::new(),
            adapters: Vec::new(),
            gpus: Vec::new(),
            process_net: ProcessNetMode::Attributed,
            gpu_presence: GpuPresence::Unknown,
            processes: Vec::new(),
            warning: None,
            cpu_detail: CpuDetailSnapshot::off(),
        }
    }
}

/// Windows processor group plus the number inside that group.
///
/// A logical processor is not a physical core. Display order is sorted by
/// `(group, number)` and may be shown as CPU 0, CPU 1, but the identity is
/// this pair, not the slot in a vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogicalCpuId {
    pub group: u16,
    pub number: u8,
}

impl LogicalCpuId {
    /// `index` is the ETW processor index (`group * 64 + number`).
    pub fn from_processor_index(index: u32) -> Self {
        Self {
            group: u16::try_from(index / 64).unwrap_or(u16::MAX),
            number: u8::try_from(index % 64).unwrap_or(63),
        }
    }
}

/// Why per-logical-CPU process time is or is not usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuDetailState {
    /// The user turned detailed analysis off. Overall CPU still updates.
    Off,
    /// The ETW session could not start without elevation.
    NeedsAdmin,
    /// The session failed, events were lost, or the first interval is not settled.
    Unavailable,
    /// This interval's per-CPU times are usable.
    Ready,
}

/// One process's time on one logical processor during one interval.
///
/// `percent` is `cpu_time / interval`, not the machine-wide GetSystemTimes ratio.
/// Idle is never stored here. Time that could not be tied to a process is not
/// stored here either.
#[derive(Clone, Debug, PartialEq)]
pub struct LogicalCpuProcess {
    pub key: ProcessKey,
    pub name: SharedName,
    pub cpu_time_100ns: u64,
    pub percent: f64,
}

/// One logical processor in one snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct LogicalCpuSample {
    pub id: LogicalCpuId,
    pub usage_percent: MetricValue<f64>,
    pub processes: Vec<LogicalCpuProcess>,
}

/// Aggregated ETW (or mock) view attached to a snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct CpuDetailSnapshot {
    pub state: CpuDetailState,
    pub message: String,
    pub lost_events: u64,
    pub events_in_interval: u64,
    /// Time spent folding events into this interval, for the diagnostics line.
    pub aggregate_nanos: u64,
    /// Mean of ready per-CPU busy percents. Compared with GetSystemTimes, not mixed into it.
    pub mean_busy_percent: Option<f64>,
    pub processors: Vec<LogicalCpuSample>,
    /// Threads still in the ETW ledger after this interval was folded.
    pub ledger_threads: u32,
    /// Ended thread ids still remembered so a late switch is not guessed.
    pub ledger_ended: u32,
    /// Process names still held by the ledger, not the history ring.
    pub ledger_names: u32,
}

impl CpuDetailSnapshot {
    pub fn off() -> Self {
        Self {
            state: CpuDetailState::Off,
            message: String::new(),
            lost_events: 0,
            events_in_interval: 0,
            aggregate_nanos: 0,
            mean_busy_percent: None,
            processors: Vec::new(),
            ledger_threads: 0,
            ledger_ended: 0,
            ledger_names: 0,
        }
    }

    pub fn needs_admin() -> Self {
        Self {
            state: CpuDetailState::NeedsAdmin,
            message: "要管理者".to_string(),
            lost_events: 0,
            events_in_interval: 0,
            aggregate_nanos: 0,
            mean_busy_percent: None,
            processors: Vec::new(),
            ledger_threads: 0,
            ledger_ended: 0,
            ledger_names: 0,
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            state: CpuDetailState::Unavailable,
            message: message.into(),
            lost_events: 0,
            events_in_interval: 0,
            aggregate_nanos: 0,
            mean_busy_percent: None,
            processors: Vec::new(),
            ledger_threads: 0,
            ledger_ended: 0,
            ledger_names: 0,
        }
    }
}

#[cfg(test)]
mod self_row_tests {
    use super::*;

    #[test]
    fn a_missing_self_row_is_filled_from_a_direct_reading() {
        let mut processes = vec![RawProcess::new(1, 0, "other.exe")];
        ensure_self_row(
            &mut processes,
            42,
            "PULSE.exe",
            Some(9),
            MetricValue::Value(100),
            MetricValue::Value(4096),
        );
        let mine = processes.iter().find(|process| process.pid == 42).unwrap();
        assert_eq!(mine.cpu_time_100ns, MetricValue::Value(100));
        assert_eq!(mine.memory_bytes, MetricValue::Value(4096));
        assert_eq!(mine.creation_time_100ns, Some(9));
    }

    #[test]
    fn an_unreadable_self_row_takes_the_direct_reading() {
        let mut processes = vec![RawProcess::new(42, 0, "PULSE.exe")];
        ensure_self_row(
            &mut processes,
            42,
            "PULSE.exe",
            Some(9),
            MetricValue::Value(50),
            MetricValue::Value(2048),
        );
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].cpu_time_100ns, MetricValue::Value(50));
        assert_eq!(processes[0].memory_bytes, MetricValue::Value(2048));
        assert_eq!(processes[0].name, "PULSE.exe");
    }

    #[test]
    fn a_real_self_reading_is_not_replaced() {
        let mut row = RawProcess::new(42, 0, "PULSE.exe");
        row.creation_time_100ns = Some(3);
        row.cpu_time_100ns = MetricValue::Value(5);
        row.memory_bytes = MetricValue::Value(8);
        let mut processes = vec![row];
        ensure_self_row(
            &mut processes,
            42,
            "other.exe",
            Some(9),
            MetricValue::Value(99),
            MetricValue::Value(100),
        );
        assert_eq!(processes[0].cpu_time_100ns, MetricValue::Value(5));
        assert_eq!(processes[0].memory_bytes, MetricValue::Value(8));
        assert_eq!(processes[0].creation_time_100ns, Some(3));
        assert_eq!(processes[0].name, "PULSE.exe");
    }
}

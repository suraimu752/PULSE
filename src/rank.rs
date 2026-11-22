//! Top-five ranking from one snapshot.
//!
//! Only real readings are ordered. Unavailable and unsupported values are
//! not zeros. Ties break by PID, then by creation time, so a reused PID is a
//! different process and the order does not flicker.

use std::cmp::Ordering;

use crate::format::{
    bytes_kind, format_bytes, format_bytes_per_sec, format_percent, percent_kind, DisplayKind,
};
use crate::metric::MetricValue;
use crate::model::{ProcessGpu, ProcessNetMode, ProcessSnapshot, ResourceId, Snapshot};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedProcess {
    pub rank: u32,
    pub name: String,
    pub pid: u32,
    pub creation_time_100ns: u64,
    pub value: String,
    pub kind: DisplayKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RankStatus {
    Ranked(Vec<RankedProcess>),
    /// Per-process network is not measured for a normal user.
    NeedsAdmin,
    /// This build has no per-process reading for the resource.
    Unsupported,
}

pub fn rank(
    snapshot: &Snapshot,
    resource: ResourceId,
    adapter: Option<u64>,
    limit: usize,
) -> RankStatus {
    if limit == 0 {
        return RankStatus::Ranked(Vec::new());
    }
    if resource == ResourceId::Network && snapshot.process_net == ProcessNetMode::NeedsAdmin {
        return RankStatus::NeedsAdmin;
    }
    let mut rows: Vec<(&ProcessSnapshot, f64, String, DisplayKind)> = Vec::new();
    let mut saw_unsupported = false;
    let mut saw_other = false;
    for process in &snapshot.processes {
        match reading(process, resource, adapter) {
            Some(MetricValue::Value(number)) if number.is_finite() && number >= 0.0 => {
                let (text, kind) = display(resource, number);
                rows.push((process, number, text, kind));
                saw_other = true;
            }
            Some(MetricValue::Unsupported) => saw_unsupported = true,
            Some(_) => saw_other = true,
            None => saw_other = true,
        }
    }
    if rows.is_empty() && saw_unsupported && !saw_other {
        return RankStatus::Unsupported;
    }
    rows.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.key.pid.cmp(&right.0.key.pid))
            .then_with(|| {
                left.0
                    .key
                    .creation_time_100ns
                    .cmp(&right.0.key.creation_time_100ns)
            })
    });
    let ranked = rows
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(index, (process, _, value, kind))| RankedProcess {
            rank: (index as u32) + 1,
            name: process.name.clone(),
            pid: process.key.pid,
            creation_time_100ns: process.key.creation_time_100ns,
            value,
            kind,
        })
        .collect();
    RankStatus::Ranked(ranked)
}

pub fn rank_caption(resource: ResourceId, adapter_named: bool) -> &'static str {
    match resource {
        ResourceId::Cpu => "この時点の CPU 上位",
        ResourceId::Memory => "この時点のメモリ上位",
        ResourceId::Disk => "この時点のプロセス I/O 上位\n（物理ディスクの転送量とは別）",
        ResourceId::Network => "この時点のプロセス通信上位\n（アダプター合計とは別）",
        ResourceId::Gpu3d if adapter_named => "この時点の GPU 3D 上位\n（選択中のアダプター）",
        ResourceId::Gpu3d => "この時点の GPU 3D 上位",
        ResourceId::GpuEncode if adapter_named => {
            "この時点の GPU Encode 上位\n（選択中のアダプター）"
        }
        ResourceId::GpuEncode => "この時点の GPU Encode 上位",
        ResourceId::GpuDecode if adapter_named => {
            "この時点の GPU Decode 上位\n（選択中のアダプター）"
        }
        ResourceId::GpuDecode => "この時点の GPU Decode 上位",
        ResourceId::VramDedicated if adapter_named => {
            "この時点の専用 VRAM 上位\n（選択中のアダプター）"
        }
        ResourceId::VramDedicated => "この時点の専用 VRAM 上位",
        ResourceId::VramShared if adapter_named => {
            "この時点の共有 GPU メモリ上位\n（選択中のアダプター）"
        }
        ResourceId::VramShared => "この時点の共有 GPU メモリ上位",
    }
}

pub fn empty_rank_text(status: &RankStatus) -> Option<&'static str> {
    match status {
        RankStatus::NeedsAdmin => {
            Some("プロセス別計測には管理者権限が必要です。アダプターの合計は上の値です。順位は作りません。")
        }
        RankStatus::Unsupported => Some("この資源のプロセス別計測は未対応です。"),
        RankStatus::Ranked(rows) if rows.is_empty() => Some("実測値のあるプロセスはありません。"),
        RankStatus::Ranked(_) => None,
    }
}

fn reading(
    process: &ProcessSnapshot,
    resource: ResourceId,
    adapter: Option<u64>,
) -> Option<MetricValue<f64>> {
    if let Some(luid) = adapter {
        if !matches!(
            resource,
            ResourceId::Gpu3d
                | ResourceId::GpuEncode
                | ResourceId::GpuDecode
                | ResourceId::VramDedicated
                | ResourceId::VramShared
        ) {
            return Some(scalar(process, resource));
        }
        let gpu = process.gpus.iter().find(|gpu| gpu.luid == luid)?;
        return Some(adapter_scalar(gpu, resource));
    }
    Some(scalar(process, resource))
}

fn scalar(process: &ProcessSnapshot, resource: ResourceId) -> MetricValue<f64> {
    match resource {
        ResourceId::Cpu => process.cpu_percent,
        ResourceId::Memory => as_f64(process.memory_bytes),
        ResourceId::Disk => rate_sum(process.disk_read_bps, process.disk_write_bps),
        ResourceId::Network => rate_sum(process.net_send_bps, process.net_recv_bps),
        ResourceId::Gpu3d => process.gpu_3d_percent,
        ResourceId::GpuEncode => process.gpu_encode_percent,
        ResourceId::GpuDecode => process.gpu_decode_percent,
        ResourceId::VramDedicated => as_f64(process.vram_dedicated_bytes),
        ResourceId::VramShared => as_f64(process.vram_shared_bytes),
    }
}

fn adapter_scalar(gpu: &ProcessGpu, resource: ResourceId) -> MetricValue<f64> {
    match resource {
        ResourceId::Gpu3d => gpu.gpu_3d_percent,
        ResourceId::GpuEncode => gpu.gpu_encode_percent,
        ResourceId::GpuDecode => gpu.gpu_decode_percent,
        ResourceId::VramDedicated => as_f64(gpu.vram_dedicated_bytes),
        ResourceId::VramShared => as_f64(gpu.vram_shared_bytes),
        _ => MetricValue::Unsupported,
    }
}

fn as_f64(value: MetricValue<u64>) -> MetricValue<f64> {
    match value {
        MetricValue::Value(number) => MetricValue::Value(number as f64),
        MetricValue::Unavailable => MetricValue::Unavailable,
        MetricValue::Unsupported => MetricValue::Unsupported,
    }
}

fn rate_sum(left: MetricValue<f64>, right: MetricValue<f64>) -> MetricValue<f64> {
    match (left, right) {
        (MetricValue::Value(left), MetricValue::Value(right))
            if left.is_finite() && right.is_finite() =>
        {
            MetricValue::Value(left + right)
        }
        (MetricValue::Unsupported, MetricValue::Unsupported) => MetricValue::Unsupported,
        _ => MetricValue::Unavailable,
    }
}

fn display(resource: ResourceId, number: f64) -> (String, DisplayKind) {
    match resource {
        ResourceId::Cpu | ResourceId::Gpu3d | ResourceId::GpuEncode | ResourceId::GpuDecode => {
            let value = MetricValue::Value(number);
            (format_percent(value), percent_kind(value))
        }
        ResourceId::Memory | ResourceId::VramDedicated | ResourceId::VramShared => {
            let bytes = MetricValue::Value(number.round() as u64);
            (format_bytes(bytes), bytes_kind(bytes))
        }
        ResourceId::Disk | ResourceId::Network => {
            let value = MetricValue::Value(number);
            let text = format_bytes_per_sec(value);
            let kind = if number == 0.0 {
                DisplayKind::Zero
            } else {
                DisplayKind::Value
            };
            (text, kind)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProcessGpu, ProcessKey, ProcessNetMode};
    use std::time::{Duration, Instant};

    fn snap(processes: Vec<ProcessSnapshot>) -> Snapshot {
        let mut snapshot = Snapshot::bare(1, Instant::now());
        snapshot.processes = processes;
        snapshot
    }

    fn cpu_row(pid: u32, creation: u64, name: &str, cpu: MetricValue<f64>) -> ProcessSnapshot {
        let mut process = ProcessSnapshot::bare(pid, creation, name);
        process.cpu_percent = cpu;
        process
    }

    #[test]
    fn highest_cpu_is_first_and_ties_follow_pid() {
        let snapshot = snap(vec![
            cpu_row(30, 1, "c", MetricValue::Value(10.0)),
            cpu_row(10, 1, "a", MetricValue::Value(40.0)),
            cpu_row(20, 1, "b", MetricValue::Value(40.0)),
        ]);
        let RankStatus::Ranked(rows) = rank(&snapshot, ResourceId::Cpu, None, 5) else {
            panic!("expected rows");
        };
        assert_eq!(rows[0].name, "a");
        assert_eq!(rows[0].pid, 10);
        assert_eq!(rows[1].pid, 20);
        assert_eq!(rows[2].name, "c");
    }

    #[test]
    fn unavailable_and_unsupported_are_not_zeros() {
        let mut zero = cpu_row(1, 1, "zero", MetricValue::Value(0.0));
        zero.memory_bytes = MetricValue::Value(0);
        let mut missing = cpu_row(2, 1, "missing", MetricValue::Unavailable);
        missing.memory_bytes = MetricValue::Unavailable;
        let mut absent = cpu_row(3, 1, "absent", MetricValue::Unsupported);
        absent.disk_read_bps = MetricValue::Unsupported;
        absent.disk_write_bps = MetricValue::Unsupported;
        let snapshot = snap(vec![zero, missing, absent]);
        let RankStatus::Ranked(cpu) = rank(&snapshot, ResourceId::Cpu, None, 5) else {
            panic!("cpu");
        };
        assert_eq!(cpu.len(), 1);
        assert_eq!(cpu[0].name, "zero");
        assert_eq!(cpu[0].kind, DisplayKind::Zero);
        let RankStatus::Unsupported = rank(&snapshot, ResourceId::Disk, None, 5) else {
            panic!("disk should be unsupported when every row is");
        };
    }

    #[test]
    fn fewer_than_five_real_values_are_not_padded() {
        let snapshot = snap(vec![
            cpu_row(1, 1, "only", MetricValue::Value(3.0)),
            cpu_row(2, 1, "gap", MetricValue::Unavailable),
        ]);
        let RankStatus::Ranked(rows) = rank(&snapshot, ResourceId::Cpu, None, 5) else {
            panic!("rows");
        };
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn an_exited_process_stays_in_the_snapshot_that_recorded_it() {
        let origin = Instant::now();
        let mut past = Snapshot::bare(1, origin);
        past.processes = vec![cpu_row(9, 100, "gone.exe", MetricValue::Value(90.0))];
        let mut now = Snapshot::bare(2, origin + Duration::from_secs(20));
        now.processes = vec![cpu_row(8, 200, "alive.exe", MetricValue::Value(5.0))];
        let RankStatus::Ranked(then) = rank(&past, ResourceId::Cpu, None, 5) else {
            panic!("past");
        };
        let RankStatus::Ranked(live) = rank(&now, ResourceId::Cpu, None, 5) else {
            panic!("live");
        };
        assert_eq!(then[0].name, "gone.exe");
        assert!(live.iter().all(|row| row.name != "gone.exe"));
    }

    #[test]
    fn pid_reuse_is_two_processes() {
        let snapshot = snap(vec![
            cpu_row(42, 1, "old", MetricValue::Value(70.0)),
            cpu_row(42, 2, "new", MetricValue::Value(5.0)),
        ]);
        let RankStatus::Ranked(rows) = rank(&snapshot, ResourceId::Cpu, None, 5) else {
            panic!("rows");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "old");
        assert_eq!(rows[0].creation_time_100ns, 1);
        assert_eq!(rows[1].name, "new");
        assert_ne!(
            ProcessKey {
                pid: rows[0].pid,
                creation_time_100ns: rows[0].creation_time_100ns
            },
            ProcessKey {
                pid: rows[1].pid,
                creation_time_100ns: rows[1].creation_time_100ns
            }
        );
    }

    #[test]
    fn network_without_attribution_is_not_a_ranking() {
        let mut snapshot = snap(vec![cpu_row(1, 1, "app", MetricValue::Value(1.0))]);
        snapshot.process_net = ProcessNetMode::NeedsAdmin;
        snapshot.processes[0].net_send_bps = MetricValue::Value(1_000.0);
        snapshot.processes[0].net_recv_bps = MetricValue::Value(1_000.0);
        assert_eq!(
            rank(&snapshot, ResourceId::Network, None, 5),
            RankStatus::NeedsAdmin
        );
    }

    #[test]
    fn disk_ranking_uses_process_io_not_the_physical_total() {
        let mut process = ProcessSnapshot::bare(4, 1, "writer");
        process.disk_read_bps = MetricValue::Value(1_000.0);
        process.disk_write_bps = MetricValue::Value(3_000.0);
        let mut snapshot = snap(vec![process]);
        snapshot.disk_read_bps = MetricValue::Value(9_000_000.0);
        let RankStatus::Ranked(rows) = rank(&snapshot, ResourceId::Disk, None, 5) else {
            panic!("rows");
        };
        assert_eq!(
            rows[0].value,
            format_bytes_per_sec(MetricValue::Value(4_000.0))
        );
        assert_ne!(rows[0].value, format_bytes_per_sec(snapshot.disk_read_bps));
    }

    #[test]
    fn adapters_are_not_mixed() {
        let mut process = ProcessSnapshot::bare(7, 1, "render");
        process.gpu_3d_percent = MetricValue::Value(80.0);
        process.gpus = vec![
            ProcessGpu {
                luid: 1,
                gpu_3d_percent: MetricValue::Value(80.0),
                gpu_encode_percent: MetricValue::Value(0.0),
                gpu_decode_percent: MetricValue::Value(0.0),
                vram_dedicated_bytes: MetricValue::Value(10),
                vram_shared_bytes: MetricValue::Value(1),
            },
            ProcessGpu {
                luid: 2,
                gpu_3d_percent: MetricValue::Value(4.0),
                gpu_encode_percent: MetricValue::Value(0.0),
                gpu_decode_percent: MetricValue::Value(0.0),
                vram_dedicated_bytes: MetricValue::Value(2),
                vram_shared_bytes: MetricValue::Value(1),
            },
        ];
        let mut other = ProcessSnapshot::bare(8, 1, "encode");
        other.gpu_3d_percent = MetricValue::Value(70.0);
        other.gpus = vec![ProcessGpu {
            luid: 2,
            gpu_3d_percent: MetricValue::Value(70.0),
            gpu_encode_percent: MetricValue::Value(0.0),
            gpu_decode_percent: MetricValue::Value(0.0),
            vram_dedicated_bytes: MetricValue::Value(3),
            vram_shared_bytes: MetricValue::Value(1),
        }];
        let snapshot = snap(vec![process, other]);
        let RankStatus::Ranked(gpu0) = rank(&snapshot, ResourceId::Gpu3d, Some(1), 5) else {
            panic!("gpu0");
        };
        let RankStatus::Ranked(gpu1) = rank(&snapshot, ResourceId::Gpu3d, Some(2), 5) else {
            panic!("gpu1");
        };
        assert_eq!(gpu0.len(), 1);
        assert_eq!(gpu0[0].name, "render");
        assert_eq!(gpu1[0].name, "encode");
    }

    #[test]
    fn memory_order_changes_as_a_process_grows() {
        let mut before = Snapshot::bare(1, Instant::now());
        let mut small = ProcessSnapshot::bare(1, 1, "grow");
        small.memory_bytes = MetricValue::Value(1_000);
        let mut other = ProcessSnapshot::bare(2, 1, "steady");
        other.memory_bytes = MetricValue::Value(5_000);
        before.processes = vec![small.clone(), other.clone()];
        small.memory_bytes = MetricValue::Value(9_000);
        let mut after = Snapshot::bare(2, Instant::now());
        after.processes = vec![small, other];
        let RankStatus::Ranked(first) = rank(&before, ResourceId::Memory, None, 5) else {
            panic!("before");
        };
        let RankStatus::Ranked(second) = rank(&after, ResourceId::Memory, None, 5) else {
            panic!("after");
        };
        assert_eq!(first[0].name, "steady");
        assert_eq!(second[0].name, "grow");
    }

    #[test]
    fn no_processes_is_an_empty_ranking() {
        let snapshot = snap(Vec::new());
        let RankStatus::Ranked(rows) = rank(&snapshot, ResourceId::Cpu, None, 5) else {
            panic!("rows");
        };
        assert!(rows.is_empty());
    }

    #[test]
    fn a_thousand_processes_still_return_five() {
        let processes: Vec<_> = (0..1_200)
            .map(|index| cpu_row(index as u32, 1, "p", MetricValue::Value(index as f64)))
            .collect();
        let snapshot = snap(processes);
        let started = Instant::now();
        let RankStatus::Ranked(rows) = rank(&snapshot, ResourceId::Cpu, None, 5) else {
            panic!("rows");
        };
        let elapsed = started.elapsed();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].pid, 1_199);
        assert_eq!(rows[4].pid, 1_195);
        assert!(
            elapsed < Duration::from_millis(50),
            "ranking 1200 processes took {elapsed:?}"
        );
    }

    #[test]
    fn resource_switch_reads_the_same_snapshot() {
        let mut process = cpu_row(3, 9, "both", MetricValue::Value(12.0));
        process.memory_bytes = MetricValue::Value(4096);
        let snapshot = snap(vec![process]);
        let RankStatus::Ranked(cpu) = rank(&snapshot, ResourceId::Cpu, None, 5) else {
            panic!("cpu");
        };
        let RankStatus::Ranked(memory) = rank(&snapshot, ResourceId::Memory, None, 5) else {
            panic!("memory");
        };
        assert_eq!(cpu[0].pid, memory[0].pid);
        assert!(cpu[0].value.contains('%'));
        assert!(!memory[0].value.contains('%'));
    }

    #[test]
    fn a_real_zero_is_kept_when_it_is_among_the_real_values() {
        let snapshot = snap(vec![
            cpu_row(1, 1, "hot", MetricValue::Value(5.0)),
            cpu_row(2, 1, "idle", MetricValue::Value(0.0)),
            cpu_row(3, 1, "na", MetricValue::Unavailable),
        ]);
        let RankStatus::Ranked(rows) = rank(&snapshot, ResourceId::Cpu, None, 5) else {
            panic!("rows");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].name, "idle");
    }

    #[test]
    fn captions_keep_disk_network_and_admin_distinct() {
        assert!(rank_caption(ResourceId::Disk, false).contains("プロセス I/O"));
        assert!(rank_caption(ResourceId::Network, false).contains("アダプター"));
        assert!(rank_caption(ResourceId::Gpu3d, true).contains("アダプター"));
        assert_eq!(
            empty_rank_text(&RankStatus::NeedsAdmin),
            Some("プロセス別計測には管理者権限が必要です。アダプターの合計は上の値です。順位は作りません。")
        );
        assert!(empty_rank_text(&RankStatus::Ranked(Vec::new())).is_some());
        assert!(empty_rank_text(&RankStatus::Ranked(vec![RankedProcess {
            rank: 1,
            name: "a".to_string(),
            pid: 1,
            creation_time_100ns: 1,
            value: "1.0%".to_string(),
            kind: DisplayKind::Value,
        }]))
        .is_none());
    }
}

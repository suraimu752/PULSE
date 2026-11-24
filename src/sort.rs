//! Sort the full process list at display time.
//! Values stay above unavailable and unsupported rows in both directions,
//! so a descending sort cannot pull "N/A" to the top as if it were a large number.

use std::cmp::Ordering;

use crate::metric::MetricValue;
use crate::model::ProcessSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Pid,
    Cpu,
    Memory,
    Disk,
    Network,
    Gpu3d,
    GpuEncode,
    GpuDecode,
    VramDedicated,
    VramShared,
}

impl SortColumn {
    pub fn parse(name: &str) -> Self {
        match name {
            "name" => Self::Name,
            "pid" => Self::Pid,
            "cpu" => Self::Cpu,
            "memory" => Self::Memory,
            "disk" => Self::Disk,
            "network" => Self::Network,
            "gpu3d" => Self::Gpu3d,
            "gpu-encode" => Self::GpuEncode,
            "gpu-decode" => Self::GpuDecode,
            "vram" => Self::VramDedicated,
            "shared" => Self::VramShared,
            _ => Self::Cpu,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Pid => "pid",
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Disk => "disk",
            Self::Network => "network",
            Self::Gpu3d => "gpu3d",
            Self::GpuEncode => "gpu-encode",
            Self::GpuDecode => "gpu-decode",
            Self::VramDedicated => "vram",
            Self::VramShared => "shared",
        }
    }

    /// Numeric columns open on descending; name and PID open on ascending.
    pub fn default_desc(self) -> bool {
        !matches!(self, Self::Name | Self::Pid)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

pub fn sort_processes(processes: &mut [ProcessSnapshot], column: SortColumn, descending: bool) {
    processes.sort_by(|left, right| compare(left, right, column, descending));
}

pub(crate) fn compare(
    left: &ProcessSnapshot,
    right: &ProcessSnapshot,
    column: SortColumn,
    descending: bool,
) -> Ordering {
    let primary = match column {
        SortColumn::Name => compare_name(left, right, descending),
        SortColumn::Pid => compare_u32(left.key.pid, right.key.pid, descending),
        SortColumn::Cpu => compare_metric(&left.cpu_percent, &right.cpu_percent, descending),
        SortColumn::Memory => compare_metric(&left.memory_bytes, &right.memory_bytes, descending),
        SortColumn::Disk => compare_metric(
            &rate_sum(left.disk_read_bps, left.disk_write_bps),
            &rate_sum(right.disk_read_bps, right.disk_write_bps),
            descending,
        ),
        SortColumn::Network => compare_metric(
            &rate_sum(left.net_send_bps, left.net_recv_bps),
            &rate_sum(right.net_send_bps, right.net_recv_bps),
            descending,
        ),
        SortColumn::Gpu3d => {
            compare_metric(&left.gpu_3d_percent, &right.gpu_3d_percent, descending)
        }
        SortColumn::GpuEncode => compare_metric(
            &left.gpu_encode_percent,
            &right.gpu_encode_percent,
            descending,
        ),
        SortColumn::GpuDecode => compare_metric(
            &left.gpu_decode_percent,
            &right.gpu_decode_percent,
            descending,
        ),
        SortColumn::VramDedicated => compare_metric(
            &left.vram_dedicated_bytes,
            &right.vram_dedicated_bytes,
            descending,
        ),
        SortColumn::VramShared => compare_metric(
            &left.vram_shared_bytes,
            &right.vram_shared_bytes,
            descending,
        ),
    };
    primary
        .then_with(|| compare_name(left, right, false))
        .then_with(|| left.key.pid.cmp(&right.key.pid))
        .then_with(|| {
            left.key
                .creation_time_100ns
                .cmp(&right.key.creation_time_100ns)
        })
}

fn compare_name(left: &ProcessSnapshot, right: &ProcessSnapshot, descending: bool) -> Ordering {
    let order = left.name.to_lowercase().cmp(&right.name.to_lowercase());
    if descending {
        order.reverse()
    } else {
        order
    }
}

fn compare_u32(left: u32, right: u32, descending: bool) -> Ordering {
    let order = left.cmp(&right);
    if descending {
        order.reverse()
    } else {
        order
    }
}

fn compare_metric<T: PartialOrd>(
    left: &MetricValue<T>,
    right: &MetricValue<T>,
    descending: bool,
) -> Ordering {
    match (left, right) {
        (MetricValue::Value(left), MetricValue::Value(right)) => {
            let order = left.partial_cmp(right).unwrap_or(Ordering::Equal);
            if descending {
                order.reverse()
            } else {
                order
            }
        }
        (MetricValue::Value(_), _) => Ordering::Less,
        (_, MetricValue::Value(_)) => Ordering::Greater,
        _ => Ordering::Equal,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProcessKey;

    fn row(
        name: &str,
        pid: u32,
        cpu: MetricValue<f64>,
        memory: MetricValue<u64>,
    ) -> ProcessSnapshot {
        ProcessSnapshot {
            key: ProcessKey {
                pid,
                creation_time_100ns: u64::from(pid),
            },
            parent_pid: 0,
            name: crate::model::SharedName::from(name),
            cpu_percent: cpu,
            memory_bytes: memory,
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

    #[test]
    fn values_stay_above_missing_rows_when_descending() {
        let mut rows = vec![
            row("a", 1, MetricValue::Value(1.0), MetricValue::Value(10)),
            row("b", 2, MetricValue::Value(10.0), MetricValue::Value(5)),
            row("c", 3, MetricValue::Unavailable, MetricValue::Unavailable),
            row("d", 4, MetricValue::Value(0.0), MetricValue::Value(0)),
        ];
        sort_processes(&mut rows, SortColumn::Cpu, true);
        let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["b", "a", "d", "c"]);
    }

    #[test]
    fn ascending_keeps_zero_as_a_number() {
        let mut rows = vec![
            row("a", 1, MetricValue::Value(1.0), MetricValue::Value(10)),
            row("b", 2, MetricValue::Value(10.0), MetricValue::Value(5)),
            row("d", 4, MetricValue::Value(0.0), MetricValue::Value(0)),
            row("c", 3, MetricValue::Unsupported, MetricValue::Unsupported),
        ];
        sort_processes(&mut rows, SortColumn::Cpu, false);
        let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["d", "a", "b", "c"]);
    }

    #[test]
    fn two_thousand_rows_keep_their_values_and_sort_quickly() {
        let mut rows = Vec::with_capacity(2000);
        for pid in 0..2000u32 {
            let cpu = MetricValue::Value(f64::from(pid % 50));
            rows.push(row("proc", pid, cpu, MetricValue::Value(u64::from(pid))));
        }
        let started = std::time::Instant::now();
        sort_processes(&mut rows, SortColumn::Cpu, true);
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "sort took {elapsed:?}"
        );
        assert_eq!(rows.len(), 2000);
        assert_eq!(rows[0].cpu_percent, MetricValue::Value(49.0));
        assert_eq!(rows[1999].cpu_percent, MetricValue::Value(0.0));
        assert!(rows.iter().all(|row| row.cpu_percent.is_value()));
    }
}

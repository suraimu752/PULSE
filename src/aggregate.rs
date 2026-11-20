//! Turn cumulative counters into the percents and rates shown in the UI.
//! The first sample has no baseline, so rates are unavailable rather than zero.

use std::collections::HashMap;
use std::time::Duration;

use crate::metric::MetricValue;
use crate::model::{ProcessSnapshot, RawProcess, RawSample, Snapshot};

pub fn aggregate(previous: Option<&RawSample>, current: &RawSample) -> Snapshot {
    let elapsed = previous.map(|prev| current.taken_at.saturating_duration_since(prev.taken_at));
    let denom = previous
        .and_then(|prev| elapsed.and_then(|elapsed| cpu_denominator(prev, current, elapsed)));
    let previous_processes = previous_index(previous);

    let cpu_percent = match denom {
        Some(denom) => match counter_delta(
            previous.map(|prev| prev.system_busy_100ns),
            current.system_busy_100ns,
        ) {
            MetricValue::Value(delta) => cpu_percent(delta, denom),
            other => map_missing(other),
        },
        None => missing_like(current.system_busy_100ns),
    };

    let processes = current
        .processes
        .iter()
        .map(|process| {
            let prev = previous_processes.get(&process.key()).copied();
            process_snapshot(process, prev, denom, elapsed)
        })
        .collect();

    Snapshot {
        sequence: 0,
        taken_at: current.wall_time,
        interval: elapsed,
        logical_processors: current.logical_processors,
        cpu_percent,
        memory_used_bytes: memory_used(current.memory_total_bytes, current.memory_available_bytes),
        memory_total_bytes: current.memory_total_bytes,
        disk_read_bps: current.system_disk_read_bps,
        disk_write_bps: current.system_disk_write_bps,
        net_send_bps: current.system_net_send_bps,
        net_recv_bps: current.system_net_recv_bps,
        gpu_3d_percent: current.system_gpu_3d_percent,
        gpu_encode_percent: current.system_gpu_encode_percent,
        gpu_decode_percent: current.system_gpu_decode_percent,
        vram_dedicated_bytes: current.system_vram_dedicated_bytes,
        vram_shared_bytes: current.system_vram_shared_bytes,
        disks: current.disks.clone(),
        adapters: current.adapters.clone(),
        gpus: current.gpus.clone(),
        process_net: current.process_net,
        gpu_presence: current.gpu_presence,
        processes,
        warning: current.warning.clone(),
    }
}

fn process_snapshot(
    process: &RawProcess,
    previous: Option<&RawProcess>,
    denom: Option<u64>,
    elapsed: Option<Duration>,
) -> ProcessSnapshot {
    let cpu_percent = match denom {
        Some(denom) => match counter_delta(
            previous.map(|prev| prev.cpu_time_100ns),
            process.cpu_time_100ns,
        ) {
            MetricValue::Value(delta) => cpu_percent(delta, denom),
            other => map_missing(other),
        },
        None => missing_like(process.cpu_time_100ns),
    };

    ProcessSnapshot {
        key: process.key(),
        parent_pid: process.parent_pid,
        name: process.name.clone(),
        cpu_percent,
        memory_bytes: process.memory_bytes,
        disk_read_bps: counter_rate(
            previous.map(|prev| prev.disk_read_bytes),
            process.disk_read_bytes,
            elapsed,
        ),
        disk_write_bps: counter_rate(
            previous.map(|prev| prev.disk_write_bytes),
            process.disk_write_bytes,
            elapsed,
        ),
        net_send_bps: counter_rate(
            previous.map(|prev| prev.net_send_bytes),
            process.net_send_bytes,
            elapsed,
        ),
        net_recv_bps: counter_rate(
            previous.map(|prev| prev.net_recv_bytes),
            process.net_recv_bytes,
            elapsed,
        ),
        gpu_3d_percent: process.gpu_3d_percent,
        gpu_encode_percent: process.gpu_encode_percent,
        gpu_decode_percent: process.gpu_decode_percent,
        vram_dedicated_bytes: process.vram_dedicated_bytes,
        vram_shared_bytes: process.vram_shared_bytes,
    }
}

fn previous_index(previous: Option<&RawSample>) -> HashMap<crate::model::ProcessKey, &RawProcess> {
    previous
        .map(|sample| {
            sample
                .processes
                .iter()
                .map(|process| (process.key(), process))
                .collect()
        })
        .unwrap_or_default()
}

fn cpu_denominator(previous: &RawSample, current: &RawSample, elapsed: Duration) -> Option<u64> {
    if let (MetricValue::Value(before), MetricValue::Value(after)) =
        (previous.system_total_100ns, current.system_total_100ns)
    {
        if after > before {
            return Some(after - before);
        }
    }
    let processors = match current.logical_processors {
        MetricValue::Value(count) if count > 0 => u64::from(count),
        _ => return None,
    };
    duration_to_100ns(elapsed)
        .map(|ticks| ticks.saturating_mul(processors))
        .filter(|ticks| *ticks > 0)
}

fn counter_delta(
    previous: Option<MetricValue<u64>>,
    current: MetricValue<u64>,
) -> MetricValue<u64> {
    match current {
        MetricValue::Unsupported => MetricValue::Unsupported,
        MetricValue::Unavailable => MetricValue::Unavailable,
        MetricValue::Value(current_value) => match previous {
            Some(MetricValue::Value(previous_value)) if current_value >= previous_value => {
                MetricValue::Value(current_value - previous_value)
            }
            Some(MetricValue::Unsupported) => MetricValue::Unsupported,
            _ => MetricValue::Unavailable,
        },
    }
}

fn counter_rate(
    previous: Option<MetricValue<u64>>,
    current: MetricValue<u64>,
    elapsed: Option<Duration>,
) -> MetricValue<f64> {
    let Some(elapsed) = elapsed else {
        return missing_like(current);
    };
    let secs = elapsed.as_secs_f64();
    if secs <= f64::EPSILON {
        return missing_like(current);
    }
    match counter_delta(previous, current) {
        MetricValue::Value(delta) => {
            let rate = delta as f64 / secs;
            if rate.is_finite() {
                MetricValue::Value(rate)
            } else {
                MetricValue::Unavailable
            }
        }
        other => map_missing(other),
    }
}

fn cpu_percent(delta_100ns: u64, denominator_100ns: u64) -> MetricValue<f64> {
    if denominator_100ns == 0 {
        return MetricValue::Unavailable;
    }
    let percent = (delta_100ns as f64) / (denominator_100ns as f64) * 100.0;
    if percent.is_finite() {
        MetricValue::Value(percent.clamp(0.0, 100.0))
    } else {
        MetricValue::Unavailable
    }
}

fn memory_used(total: MetricValue<u64>, available: MetricValue<u64>) -> MetricValue<u64> {
    match (total, available) {
        (MetricValue::Value(total), MetricValue::Value(available)) => {
            MetricValue::Value(total.saturating_sub(available.min(total)))
        }
        (MetricValue::Unsupported, _) | (_, MetricValue::Unsupported) => MetricValue::Unsupported,
        _ => MetricValue::Unavailable,
    }
}

fn missing_like<T, U>(value: MetricValue<T>) -> MetricValue<U> {
    match value {
        MetricValue::Unsupported => MetricValue::Unsupported,
        MetricValue::Value(_) | MetricValue::Unavailable => MetricValue::Unavailable,
    }
}

fn map_missing<U>(value: MetricValue<u64>) -> MetricValue<U> {
    match value {
        MetricValue::Unsupported => MetricValue::Unsupported,
        MetricValue::Unavailable | MetricValue::Value(_) => MetricValue::Unavailable,
    }
}

fn duration_to_100ns(duration: Duration) -> Option<u64> {
    u64::try_from(duration.as_nanos() / 100).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Instant, SystemTime};

    fn base(at: Instant, total: u64, busy: u64, cpu: u64, creation: u64) -> RawSample {
        let mut process = RawProcess::new(7, 1, "demo.exe");
        process.creation_time_100ns = Some(creation);
        process.cpu_time_100ns = MetricValue::Value(cpu);
        process.memory_bytes = MetricValue::Value(4096);
        let mut sample = RawSample::empty(at, SystemTime::UNIX_EPOCH);
        sample.logical_processors = MetricValue::Value(4);
        sample.system_total_100ns = MetricValue::Value(total);
        sample.system_busy_100ns = MetricValue::Value(busy);
        sample.memory_total_bytes = MetricValue::Value(1_000);
        sample.memory_available_bytes = MetricValue::Value(250);
        sample.processes.push(process);
        sample
    }

    #[test]
    fn first_sample_cpu_is_unavailable_but_memory_is_kept() {
        let now = Instant::now();
        let snapshot = aggregate(None, &base(now, 40_000_000, 10_000_000, 5_000_000, 10));
        assert_eq!(snapshot.cpu_percent, MetricValue::Unavailable);
        assert_eq!(snapshot.processes[0].cpu_percent, MetricValue::Unavailable);
        assert_eq!(snapshot.memory_used_bytes, MetricValue::Value(750));
        assert_eq!(snapshot.processes[0].memory_bytes, MetricValue::Value(4096));
        assert_eq!(
            snapshot.processes[0].disk_read_bps,
            MetricValue::Unsupported
        );
        assert_eq!(snapshot.disk_read_bps, MetricValue::Unsupported);
        assert!(snapshot.interval.is_none());
    }

    #[test]
    fn second_sample_uses_shared_capacity_denominator() {
        let start = Instant::now();
        let prev = base(start, 40_000_000, 10_000_000, 5_000_000, 10);
        let curr = base(
            start + Duration::from_millis(500),
            80_000_000,
            20_000_000,
            10_000_000,
            10,
        );
        let snapshot = aggregate(Some(&prev), &curr);
        assert_eq!(snapshot.cpu_percent, MetricValue::Value(25.0));
        assert_eq!(snapshot.processes[0].cpu_percent, MetricValue::Value(12.5));
        assert_eq!(snapshot.interval, Some(Duration::from_millis(500)));
    }

    #[test]
    fn zero_delta_is_a_real_zero() {
        let start = Instant::now();
        let prev = base(start, 10, 0, 50, 10);
        let curr = base(start + Duration::from_millis(500), 110, 0, 50, 10);
        let snapshot = aggregate(Some(&prev), &curr);
        assert_eq!(snapshot.cpu_percent, MetricValue::Value(0.0));
        assert_eq!(snapshot.processes[0].cpu_percent, MetricValue::Value(0.0));
    }

    #[test]
    fn pid_reuse_does_not_create_a_false_delta() {
        let start = Instant::now();
        let prev = base(start, 10, 0, 1_000, 100);
        let curr = base(
            start + Duration::from_millis(500),
            50_000_010,
            0,
            9_000,
            200,
        );
        let snapshot = aggregate(Some(&prev), &curr);
        assert_eq!(snapshot.processes[0].cpu_percent, MetricValue::Unavailable);
    }

    #[test]
    fn decreasing_counter_is_unavailable() {
        let start = Instant::now();
        let prev = base(start, 10, 0, 9_000, 100);
        let curr = base(
            start + Duration::from_millis(500),
            50_000_010,
            0,
            1_000,
            100,
        );
        let snapshot = aggregate(Some(&prev), &curr);
        assert_eq!(snapshot.processes[0].cpu_percent, MetricValue::Unavailable);
    }

    #[test]
    fn disk_bytes_become_a_rate_and_unsupported_stays_unsupported() {
        let start = Instant::now();
        let mut prev = base(start, 10, 0, 0, 1);
        let mut curr = base(start + Duration::from_millis(500), 20, 0, 0, 1);
        prev.processes[0].disk_read_bytes = MetricValue::Value(1_000);
        curr.processes[0].disk_read_bytes = MetricValue::Value(3_000);
        let snapshot = aggregate(Some(&prev), &curr);
        assert_eq!(
            snapshot.processes[0].disk_read_bps,
            MetricValue::Value(4_000.0)
        );
        assert_eq!(snapshot.processes[0].net_send_bps, MetricValue::Unsupported);
    }
}

//! Turn a 60-second history into graph points.
//!
//! Percent charts stay on a fixed 0–100 axis so a small reading does not fill
//! the plot. Byte rates use the peak inside the window, because there is no
//! natural 100%. Gaps stay gaps. They are not drawn as zero.

#[cfg(feature = "ui")]
use crate::format::{format_bytes, format_bytes_per_sec};
use crate::metric::MetricValue;
use crate::model::Snapshot;

#[derive(Clone, Debug)]
pub struct ChartHistory {
    pub cpu: Vec<Option<f64>>,
    pub memory: Vec<Option<f64>>,
    pub disk: Vec<Option<f64>>,
    pub network: Vec<Option<f64>>,
    pub gpu_3d: Vec<Option<f64>>,
    pub gpu_encode: Vec<Option<f64>>,
    pub gpu_decode: Vec<Option<f64>>,
    pub vram: Vec<Option<f64>>,
    pub shared: Vec<Option<f64>>,
}

pub fn chart_history<'a>(
    samples: impl Iterator<Item = &'a Snapshot>,
    capacity: usize,
) -> ChartHistory {
    let samples: Vec<&Snapshot> = samples.collect();
    ChartHistory {
        cpu: align(
            samples.iter().map(|sample| sample.cpu_percent.as_option()),
            capacity,
        ),
        memory: align(
            samples.iter().map(|sample| memory_percent(sample)),
            capacity,
        ),
        disk: align(
            samples
                .iter()
                .map(|sample| sum_rates(sample.disk_read_bps, sample.disk_write_bps)),
            capacity,
        ),
        network: align(
            samples
                .iter()
                .map(|sample| sum_rates(sample.net_recv_bps, sample.net_send_bps)),
            capacity,
        ),
        gpu_3d: align(
            samples
                .iter()
                .map(|sample| sample.gpu_3d_percent.as_option()),
            capacity,
        ),
        gpu_encode: align(
            samples
                .iter()
                .map(|sample| sample.gpu_encode_percent.as_option()),
            capacity,
        ),
        gpu_decode: align(
            samples
                .iter()
                .map(|sample| sample.gpu_decode_percent.as_option()),
            capacity,
        ),
        vram: align(
            samples.iter().map(|sample| {
                sample
                    .vram_dedicated_bytes
                    .as_option()
                    .map(|bytes| bytes as f64)
            }),
            capacity,
        ),
        shared: align(
            samples.iter().map(|sample| {
                sample
                    .vram_shared_bytes
                    .as_option()
                    .map(|bytes| bytes as f64)
            }),
            capacity,
        ),
    }
}

fn align(values: impl Iterator<Item = Option<f64>>, capacity: usize) -> Vec<Option<f64>> {
    let values: Vec<Option<f64>> = values.collect();
    crate::history::align_right(&values, capacity, None)
}

fn memory_percent(snapshot: &Snapshot) -> Option<f64> {
    match (snapshot.memory_used_bytes, snapshot.memory_total_bytes) {
        (MetricValue::Value(used), MetricValue::Value(total)) if total > 0 => {
            Some((used as f64) / (total as f64) * 100.0)
        }
        _ => None,
    }
}

fn sum_rates(left: MetricValue<f64>, right: MetricValue<f64>) -> Option<f64> {
    match (left, right) {
        (MetricValue::Value(left), MetricValue::Value(right))
            if left.is_finite() && right.is_finite() =>
        {
            Some(left + right)
        }
        _ => None,
    }
}

#[cfg(feature = "ui")]
pub struct ScaledSeries {
    /// 0–100 for a bar, or a negative value for a gap.
    pub points: Vec<f32>,
    pub axis_high: String,
    pub axis_low: String,
}

#[cfg(feature = "ui")]
pub fn scale_percent(values: &[Option<f64>]) -> ScaledSeries {
    ScaledSeries {
        points: values
            .iter()
            .map(|value| match *value {
                Some(number) if number.is_finite() => number.clamp(0.0, 100.0) as f32,
                _ => -1.0,
            })
            .collect(),
        axis_high: "100%".to_string(),
        axis_low: "0%".to_string(),
    }
}

#[cfg(feature = "ui")]
pub fn scale_peak(values: &[Option<f64>], high_label: impl Fn(f64) -> String) -> ScaledSeries {
    let peak = values
        .iter()
        .copied()
        .flatten()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .fold(0.0_f64, f64::max);
    let points = values
        .iter()
        .map(|value| match *value {
            Some(number) if number.is_finite() && number >= 0.0 && peak > 0.0 => {
                ((number / peak) * 100.0) as f32
            }
            Some(number) if number.is_finite() && number >= 0.0 => 0.0,
            _ => -1.0,
        })
        .collect();
    ScaledSeries {
        points,
        axis_high: if peak > 0.0 {
            high_label(peak)
        } else {
            "0".to_string()
        },
        axis_low: "0".to_string(),
    }
}

#[cfg(feature = "ui")]
pub fn rate_label(bytes_per_sec: f64) -> String {
    format_bytes_per_sec(MetricValue::Value(bytes_per_sec))
}

#[cfg(feature = "ui")]
pub fn bytes_label(bytes: f64) -> String {
    if bytes <= 0.0 {
        "0 B".to_string()
    } else {
        format_bytes(MetricValue::Value(bytes.round() as u64))
    }
}

/// Keep the shape of a series but cap how many rectangles the UI builds.
#[cfg(feature = "ui")]
pub fn downsample(values: &[f32], max_points: usize) -> Vec<f32> {
    if max_points == 0 || values.len() <= max_points {
        return values.to_vec();
    }
    let mut out = Vec::with_capacity(max_points);
    let last = values.len() - 1;
    for index in 0..max_points {
        let source = index * last / (max_points - 1);
        out.push(values[source]);
    }
    out
}

#[cfg(all(test, feature = "ui"))]
mod tests {
    use super::*;

    #[test]
    fn percent_axis_does_not_stretch_a_small_value() {
        let scaled = scale_percent(&[None, Some(0.0), Some(25.0)]);
        assert_eq!(scaled.points, vec![-1.0, 0.0, 25.0]);
        assert_eq!(scaled.axis_high, "100%");
    }

    #[test]
    fn peak_scale_makes_the_largest_rate_full_height() {
        let scaled = scale_peak(&[None, Some(0.0), Some(50.0), Some(25.0)], rate_label);
        assert_eq!(scaled.points[0], -1.0);
        assert_eq!(scaled.points[1], 0.0);
        assert!((scaled.points[2] - 100.0).abs() < 0.01);
        assert!((scaled.points[3] - 50.0).abs() < 0.01);
    }

    #[test]
    fn downsample_keeps_both_ends() {
        let values: Vec<f32> = (0..120).map(|index| index as f32).collect();
        let small = downsample(&values, 5);
        assert_eq!(small.first().copied(), Some(0.0));
        assert_eq!(small.last().copied(), Some(119.0));
        assert_eq!(small.len(), 5);
    }
}

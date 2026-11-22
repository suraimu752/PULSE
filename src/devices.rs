#![cfg_attr(not(windows), allow(dead_code))]
//! Pair per-device rates without treating a missing side as zero.
//!
//! Physical disk and adapter counters arrive as separate read and write
//! lists. A `_Total` instance is used only when no individual device was
//! returned, so the total is not added on top of its parts.

use crate::metric::MetricValue;
use crate::model::NamedRate;

pub fn zip_device_rates(read: &[(String, f64)], write: &[(String, f64)]) -> Vec<NamedRate> {
    let mut names = Vec::new();
    for (name, _) in read.iter().chain(write.iter()) {
        if !names.iter().any(|existing: &String| existing == name) {
            names.push(name.clone());
        }
    }
    names
        .into_iter()
        .map(|name| NamedRate {
            read_bps: finite_rate(read.iter().find(|(item, _)| item == &name).map(|(_, v)| *v)),
            write_bps: finite_rate(
                write
                    .iter()
                    .find(|(item, _)| item == &name)
                    .map(|(_, v)| *v),
            ),
            name,
        })
        .collect()
}

/// Drop a rollup row when the individual devices are present.
pub fn without_rollup(devices: Vec<NamedRate>) -> Vec<NamedRate> {
    let parts: Vec<_> = devices
        .iter()
        .filter(|device| !is_rollup(&device.name))
        .cloned()
        .collect();
    if parts.is_empty() {
        devices
    } else {
        parts
    }
}

pub fn sum_side(devices: &[NamedRate], read: bool) -> MetricValue<f64> {
    if devices.is_empty() {
        return MetricValue::Unavailable;
    }
    let mut total = 0.0;
    let mut any = false;
    for device in devices {
        let side = if read {
            device.read_bps
        } else {
            device.write_bps
        };
        if let MetricValue::Value(value) = side {
            total += value;
            any = true;
        }
    }
    if any && total.is_finite() {
        MetricValue::Value(total)
    } else {
        MetricValue::Unavailable
    }
}

fn finite_rate(value: Option<f64>) -> MetricValue<f64> {
    match value {
        Some(value) if value.is_finite() && value >= 0.0 => MetricValue::Value(value),
        Some(_) => MetricValue::Unavailable,
        None => MetricValue::Unavailable,
    }
}

fn is_rollup(name: &str) -> bool {
    let trimmed = name.trim();
    trimmed.eq_ignore_ascii_case("_total") || trimmed.ends_with("(_Total)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_is_not_added_to_its_parts() {
        let zipped = zip_device_rates(
            &[("_Total".to_string(), 30.0), ("0 C:".to_string(), 10.0)],
            &[("_Total".to_string(), 5.0), ("0 C:".to_string(), 5.0)],
        );
        let devices = without_rollup(zipped);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "0 C:");
        assert_eq!(sum_side(&devices, true), MetricValue::Value(10.0));
    }

    #[test]
    fn missing_side_is_not_zero() {
        let devices = zip_device_rates(&[("0 C:".to_string(), 4.0)], &[]);
        assert_eq!(devices[0].read_bps, MetricValue::Value(4.0));
        assert_eq!(devices[0].write_bps, MetricValue::Unavailable);
        assert_eq!(sum_side(&devices, false), MetricValue::Unavailable);
    }
}

//! User-visible formatting. Japanese labels distinguish a real zero from a gap.

use crate::metric::MetricValue;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayKind {
    Value,
    Zero,
    Unavailable,
    Unsupported,
}

impl DisplayKind {
    pub fn code(self) -> i32 {
        match self {
            Self::Value => 0,
            Self::Zero => 1,
            Self::Unavailable => 2,
            Self::Unsupported => 3,
        }
    }
}

pub fn format_percent(value: MetricValue<f64>) -> String {
    match finite_percent(value) {
        MetricValue::Value(number) => format!("{number:.1}%"),
        MetricValue::Unavailable => "N/A".to_string(),
        MetricValue::Unsupported => "未対応".to_string(),
    }
}

pub fn percent_kind(value: MetricValue<f64>) -> DisplayKind {
    match finite_percent(value) {
        MetricValue::Value(0.0) => DisplayKind::Zero,
        MetricValue::Value(_) => DisplayKind::Value,
        MetricValue::Unavailable => DisplayKind::Unavailable,
        MetricValue::Unsupported => DisplayKind::Unsupported,
    }
}

pub fn format_bytes(value: MetricValue<u64>) -> String {
    match value {
        MetricValue::Value(bytes) => format_bytes_value(bytes),
        MetricValue::Unavailable => "N/A".to_string(),
        MetricValue::Unsupported => "未対応".to_string(),
    }
}

pub fn bytes_kind(value: MetricValue<u64>) -> DisplayKind {
    match value {
        MetricValue::Value(0) => DisplayKind::Zero,
        MetricValue::Value(_) => DisplayKind::Value,
        MetricValue::Unavailable => DisplayKind::Unavailable,
        MetricValue::Unsupported => DisplayKind::Unsupported,
    }
}

pub fn format_bytes_per_sec(value: MetricValue<f64>) -> String {
    match finite_rate(value) {
        MetricValue::Value(0.0) => "0 B/s".to_string(),
        MetricValue::Value(rate) if rate < 1.0 => format!("{rate:.1} B/s"),
        MetricValue::Value(rate) => format!("{}/s", format_bytes_value(rate.round() as u64)),
        MetricValue::Unavailable => "N/A".to_string(),
        MetricValue::Unsupported => "未対応".to_string(),
    }
}

pub fn format_rate_pair(read: MetricValue<f64>, write: MetricValue<f64>) -> (String, DisplayKind) {
    match (finite_rate(read), finite_rate(write)) {
        (MetricValue::Unsupported, MetricValue::Unsupported) => {
            ("未対応".to_string(), DisplayKind::Unsupported)
        }
        (MetricValue::Value(read), MetricValue::Value(write)) => {
            let kind = if read == 0.0 && write == 0.0 {
                DisplayKind::Zero
            } else {
                DisplayKind::Value
            };
            (
                format!(
                    "R {}  W {}",
                    format_bytes_per_sec(MetricValue::Value(read)),
                    format_bytes_per_sec(MetricValue::Value(write))
                ),
                kind,
            )
        }
        _ => ("N/A".to_string(), DisplayKind::Unavailable),
    }
}

pub fn format_memory_pair(used: MetricValue<u64>, total: MetricValue<u64>) -> String {
    match (used, total) {
        (MetricValue::Value(used), MetricValue::Value(total)) => {
            format!(
                "{} / {}",
                format_bytes_value(used),
                format_bytes_value(total)
            )
        }
        (MetricValue::Unsupported, _) | (_, MetricValue::Unsupported) => "未対応".to_string(),
        _ => "N/A".to_string(),
    }
}

fn finite_percent(value: MetricValue<f64>) -> MetricValue<f64> {
    match value {
        MetricValue::Value(number) if number.is_finite() => {
            MetricValue::Value(number.clamp(0.0, 100.0))
        }
        MetricValue::Value(_) => MetricValue::Unavailable,
        other => other,
    }
}

fn finite_rate(value: MetricValue<f64>) -> MetricValue<f64> {
    match value {
        MetricValue::Value(number) if number.is_finite() && number >= 0.0 => {
            MetricValue::Value(number)
        }
        MetricValue::Value(_) => MetricValue::Unavailable,
        other => other,
    }
}

fn format_bytes_value(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let number = bytes as f64;
    if number >= 100.0 * GB {
        format!("{:.0} GB", number / GB)
    } else if number >= 10.0 * GB {
        format!("{:.1} GB", number / GB)
    } else if number >= GB {
        format!("{:.2} GB", number / GB)
    } else if number >= 100.0 * MB {
        format!("{:.0} MB", number / MB)
    } else if number >= 10.0 * MB {
        format!("{:.1} MB", number / MB)
    } else if number >= MB {
        format!("{:.2} MB", number / MB)
    } else if number >= 10.0 * KB {
        format!("{:.0} KB", number / KB)
    } else if number >= KB {
        format!("{:.1} KB", number / KB)
    } else {
        format!("{number:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_percent_and_bytes_are_not_placeholders() {
        assert_eq!(format_percent(MetricValue::Value(0.0)), "0.0%");
        assert_eq!(format_percent(MetricValue::Unavailable), "N/A");
        assert_eq!(format_percent(MetricValue::Unsupported), "未対応");
        assert_eq!(format_bytes(MetricValue::Value(0)), "0 B");
        assert_eq!(format_bytes(MetricValue::Unavailable), "N/A");
        assert_eq!(format_bytes(MetricValue::Unsupported), "未対応");
        assert_eq!(percent_kind(MetricValue::Value(0.0)), DisplayKind::Zero);
        assert_eq!(bytes_kind(MetricValue::Value(0)), DisplayKind::Zero);
    }

    #[test]
    fn binary_units() {
        assert_eq!(format_bytes(MetricValue::Value(1024)), "1.0 KB");
        assert_eq!(format_bytes(MetricValue::Value(1024 * 1024)), "1.00 MB");
    }
}

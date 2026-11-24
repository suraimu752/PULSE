//! User-visible formatting. Japanese labels distinguish a real zero from a gap.

use crate::metric::MetricValue;
use crate::model::ProcessNetMode;

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

/// Per-process network cell. Receive is the first argument (`InOctets`).
/// An unimplemented collector is `未対応`, never `要管理者`. A real reading
/// still wins over either placeholder.
pub fn format_network_cell(
    recv: MetricValue<f64>,
    send: MetricValue<f64>,
    mode: ProcessNetMode,
) -> (String, DisplayKind) {
    let (text, kind) = format_network_pair(recv, send);
    let missing = !matches!(kind, DisplayKind::Value | DisplayKind::Zero);
    match mode {
        ProcessNetMode::NeedsAdmin if missing => ("要管理者".to_string(), DisplayKind::Unavailable),
        ProcessNetMode::Unimplemented if missing => {
            ("未対応".to_string(), DisplayKind::Unsupported)
        }
        _ => (text, kind),
    }
}

/// One line, receive then send. Disk keeps [`format_rate_pair`].
pub fn format_network_pair(
    recv: MetricValue<f64>,
    send: MetricValue<f64>,
) -> (String, DisplayKind) {
    format_labeled_pair("受信", "送信", recv, send, false)
}

/// Two lines for the narrow resource card.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn format_network_lines(
    recv: MetricValue<f64>,
    send: MetricValue<f64>,
) -> (String, DisplayKind) {
    format_labeled_pair("受信", "送信", recv, send, true)
}

/// Two lines so a disk card can show both rates without ellipsis.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn format_disk_lines(read: MetricValue<f64>, write: MetricValue<f64>) -> (String, DisplayKind) {
    format_labeled_pair("R", "W", read, write, true)
}

pub fn format_rate_pair(read: MetricValue<f64>, write: MetricValue<f64>) -> (String, DisplayKind) {
    format_labeled_pair("R", "W", read, write, false)
}

fn format_labeled_pair(
    left_label: &str,
    right_label: &str,
    left: MetricValue<f64>,
    right: MetricValue<f64>,
    stacked: bool,
) -> (String, DisplayKind) {
    match (finite_rate(left), finite_rate(right)) {
        (MetricValue::Unsupported, MetricValue::Unsupported) => {
            ("未対応".to_string(), DisplayKind::Unsupported)
        }
        (MetricValue::Value(left), MetricValue::Value(right)) => {
            let kind = if left == 0.0 && right == 0.0 {
                DisplayKind::Zero
            } else {
                DisplayKind::Value
            };
            let left_text = format_bytes_per_sec(MetricValue::Value(left));
            let right_text = format_bytes_per_sec(MetricValue::Value(right));
            let text = if stacked {
                format!("{left_label} {left_text}\n{right_label} {right_text}")
            } else {
                format!("{left_label} {left_text}  {right_label} {right_text}")
            };
            (text, kind)
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

    #[test]
    fn network_receive_is_in_octets_and_zero_is_not_na() {
        let recv = MetricValue::Value(3.6 * 1024.0);
        let send = MetricValue::Value(5.4 * 1024.0);
        let (header, _) = format_network_pair(recv, send);
        let (card, _) = format_network_lines(recv, send);
        assert_eq!(header, "受信 3.6 KB/s  送信 5.4 KB/s");
        assert_eq!(card, "受信 3.6 KB/s\n送信 5.4 KB/s");
        assert!(header.find("受信").unwrap() < header.find("送信").unwrap());
        assert!(!header.contains('R'));
        let (zero, kind) = format_network_pair(MetricValue::Value(0.0), MetricValue::Value(0.0));
        assert_eq!(zero, "受信 0 B/s  送信 0 B/s");
        assert_eq!(kind, DisplayKind::Zero);
        assert_eq!(
            format_network_pair(MetricValue::Unavailable, MetricValue::Unavailable).0,
            "N/A"
        );
        let (cell, cell_kind) = format_network_cell(
            MetricValue::Unavailable,
            MetricValue::Unavailable,
            ProcessNetMode::Unimplemented,
        );
        assert_eq!(cell, "未対応");
        assert_eq!(cell_kind, DisplayKind::Unsupported);
        assert_ne!(cell, "要管理者");
    }
}

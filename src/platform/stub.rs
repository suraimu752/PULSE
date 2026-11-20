//! Non-Windows builds do not invent process or CPU numbers.

use std::time::{Instant, SystemTime};

use crate::collector::SystemProbe;
use crate::model::RawSample;

pub struct UnsupportedSystemProbe;

impl SystemProbe for UnsupportedSystemProbe {
    fn probe(&mut self) -> RawSample {
        let mut sample = RawSample::empty(Instant::now(), SystemTime::now());
        sample.warning = Some(
            "この環境では Windows の計測 API を利用できません。数値は表示しません。".to_string(),
        );
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::MetricValue;

    #[test]
    fn stub_does_not_report_zeros() {
        let sample = UnsupportedSystemProbe.probe();
        assert!(sample.processes.is_empty());
        assert_eq!(sample.memory_total_bytes, MetricValue::Unsupported);
        assert_eq!(sample.system_busy_100ns, MetricValue::Unsupported);
        assert_eq!(sample.system_disk_read_bps, MetricValue::Unsupported);
        assert!(sample.warning.is_some());
    }
}

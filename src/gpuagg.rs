//! Fold GPU engine samples without summing them past 100%.
//!
//! A process can have several 3D engines and several adapters. Adding those
//! utilizations invents a number the hardware did not report. The value kept
//! for one engine class is the maximum finite reading.

use crate::metric::MetricValue;

#[cfg_attr(not(any(windows, test, feature = "mock")), allow(dead_code))]
pub fn fold_engine_percents(values: impl IntoIterator<Item = f64>) -> MetricValue<f64> {
    let mut max: Option<f64> = None;
    for value in values {
        if value.is_finite() && value >= 0.0 {
            max = Some(match max {
                Some(current) => current.max(value),
                None => value,
            });
        }
    }
    match max {
        Some(value) => MetricValue::Value(value.clamp(0.0, 100.0)),
        None => MetricValue::Unavailable,
    }
}

/// `pid_1234_luid_0x00000000_0x0000CACE_phys_0_eng_0_engtype_3D`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum EngineClass {
    ThreeD,
    Encode,
    Decode,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct EngineInstance {
    pub pid: u32,
    pub luid: u64,
    pub class: EngineClass,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_engine_instance(name: &str) -> Option<EngineInstance> {
    let rest = name.strip_prefix("pid_")?;
    let (pid_text, rest) = rest.split_once("_luid_")?;
    let pid = pid_text.parse().ok()?;
    let (luid, class) = parse_luid_and_class(rest)?;
    Some(EngineInstance { pid, luid, class })
}

/// `luid_0x00000000_0x0000CACE_phys_0` anywhere in a counter instance name.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_luid_token(name: &str) -> Option<u64> {
    let rest = name.split("luid_").nth(1)?;
    split_luid(rest).map(|(luid, _)| luid)
}

/// `pid_1234_luid_0x00000000_0x0000CACE_phys_0`
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_process_memory_instance(name: &str) -> Option<(u32, u64)> {
    let rest = name.strip_prefix("pid_")?;
    let (pid_text, rest) = rest.split_once("_luid_")?;
    let pid = pid_text.parse().ok()?;
    let (luid, _) = split_luid(rest)?;
    Some((pid, luid))
}

#[cfg_attr(not(test), allow(dead_code))]
fn parse_luid_and_class(rest: &str) -> Option<(u64, EngineClass)> {
    let (luid, after) = split_luid(rest)?;
    let class = after
        .split("engtype_")
        .nth(1)
        .map(engine_class)
        .unwrap_or(EngineClass::Other);
    Some((luid, class))
}

#[cfg_attr(not(test), allow(dead_code))]
fn split_luid(rest: &str) -> Option<(u64, &str)> {
    let rest = rest.strip_prefix("0x")?;
    let (high, rest) = rest.split_once("_0x")?;
    let (low, after) = rest.split_once('_').unwrap_or((rest, ""));
    let high = u64::from_str_radix(high, 16).ok()?;
    let low = u64::from_str_radix(low, 16).ok()?;
    if high > u64::from(u32::MAX) || low > u64::from(u32::MAX) {
        return None;
    }
    Some(((high << 32) | low, after))
}

#[cfg_attr(not(test), allow(dead_code))]
fn engine_class(name: &str) -> EngineClass {
    match name {
        "3D" => EngineClass::ThreeD,
        "VideoEncode" | "Encode" => EngineClass::Encode,
        "VideoDecode" | "Decode" => EngineClass::Decode,
        _ => EngineClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_not_sum_and_parse_engine_name() {
        assert_eq!(fold_engine_percents([80.0, 60.0]), MetricValue::Value(80.0));
        assert_eq!(fold_engine_percents([120.0]), MetricValue::Value(100.0));
        assert_eq!(
            fold_engine_percents([f64::NAN, -1.0]),
            MetricValue::Unavailable
        );
        let parsed = parse_engine_instance(
            "pid_1234_luid_0x00000000_0x0000CACE_phys_0_eng_0_engtype_VideoEncode",
        )
        .unwrap();
        assert_eq!(parsed.pid, 1234);
        assert_eq!(parsed.luid, 0xCACE);
        assert_eq!(parsed.class, EngineClass::Encode);
        assert_eq!(
            parse_process_memory_instance("pid_4_luid_0x00000001_0x00000002_phys_0"),
            Some((4, (1u64 << 32) | 2))
        );
        assert_eq!(
            parse_luid_token("luid_0x00000000_0x0000CACE_phys_0"),
            Some(0xCACE)
        );
    }
}

//! Operating-system boundary. Everything that calls Windows lives under `windows`.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::SystemTime;
#[cfg(not(windows))]
use std::time::UNIX_EPOCH;

use crate::collector::{MetricEnricher, SystemProbe};
use crate::metric::MetricValue;
use crate::model::{CpuDetailState, Snapshot};
use crate::selfmeter::SelfDiagnostics;

#[cfg(windows)]
mod disk;
#[cfg(windows)]
mod etw;
#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod network;
#[cfg(windows)]
mod pdh;
#[cfg(not(windows))]
mod stub;
#[cfg(windows)]
mod windows;

pub fn make_probe() -> Box<dyn SystemProbe> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsSystemProbe::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(stub::UnsupportedSystemProbe)
    }
}

pub fn make_enrichers(cpu_detail: Arc<AtomicBool>) -> Vec<Box<dyn MetricEnricher>> {
    #[cfg(windows)]
    {
        vec![
            Box::new(disk::DiskEnricher::new()),
            Box::new(network::NetworkEnricher::new()),
            Box::new(gpu::GpuEnricher::new()),
            Box::new(etw::EtwEnricher::new(cpu_detail)),
        ]
    }
    #[cfg(not(windows))]
    {
        let _ = cpu_detail;
        Vec::new()
    }
}

/// Start this same executable with the runas verb. Only call this from a click.
#[cfg(feature = "ui")]
pub fn relaunch_elevated() -> Result<(), String> {
    #[cfg(windows)]
    {
        etw::relaunch_elevated()
    }
    #[cfg(not(windows))]
    {
        Err("この環境では管理者として再起動できません".to_string())
    }
}

/// Local time of day for the status line.
///
/// On Windows this is the local clock. Elsewhere it is UTC and labeled as such,
/// so a development build does not pretend to know the user's timezone.
/// Windows app theme: `HKCU\\...\\Themes\\Personalize\\AppsUseLightTheme`.
///
/// `Some(true)` is light, `Some(false)` is dark. `None` means this build cannot
/// read the key (not Windows, or the value is missing). Callers then keep light
/// for system mode instead of guessing dark.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn apps_use_light_theme() -> Option<bool> {
    #[cfg(windows)]
    {
        windows::apps_use_light_theme()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// What the footer should say about this process token.
///
/// Windows reads `TokenElevation`. A failed query is not reported as a normal
/// user. Other systems cannot read the token, so they stay undecided.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn privilege_label() -> &'static str {
    match privilege_state() {
        Some(true) => "管理者として動作",
        Some(false) => "通常のユーザー権限で動作",
        None => "権限は未判定",
    }
}

/// Short form that still fits the collapsed navigation rail.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn privilege_short() -> &'static str {
    match privilege_state() {
        Some(true) => "管理者",
        Some(false) => "通常",
        None => "未判定",
    }
}

fn privilege_state() -> Option<bool> {
    #[cfg(windows)]
    {
        windows::token_is_elevated()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Private working set, working set, private bytes, and handle count for this process.
///
/// Uses the `GetCurrentProcess` pseudo-handle, which is not closed. Thread count
/// is filled by the probe when Toolhelp saw this PID. Unread fields stay unavailable.
pub fn read_self_usage() -> SelfDiagnostics {
    #[cfg(windows)]
    {
        windows::read_self_usage()
    }
    #[cfg(not(windows))]
    {
        SelfDiagnostics::unavailable()
    }
}

/// How many times a PDH query was discarded after repeated hard failures.
/// Zero means none have been discarded. It is not an unread counter.
pub fn pdh_reopen_count() -> u32 {
    #[cfg(windows)]
    {
        pdh::reopen_count()
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// Configured maximum ETW buffer bytes for the current detail state.
///
/// Off is unavailable, not zero: the session is not requested. Any other state
/// reports `MaximumBuffers * 64 KiB`, which is a ceiling in kernel pool, not
/// this process's private bytes.
pub fn etw_buffer_ceiling(snapshot: &Snapshot) -> MetricValue<u64> {
    if snapshot.cpu_detail.state == CpuDetailState::Off {
        return MetricValue::Unavailable;
    }
    let processors = snapshot
        .logical_processors
        .as_option()
        .map(|count| count as usize)
        .unwrap_or(1)
        .max(1);
    MetricValue::Value(etw_buffer_ceiling_bytes(processors))
}

/// `(minimum, maximum)` buffers. Two per logical processor, and at least 64 maximum.
///
/// ETW's per-processor floor is two buffers. Shrinking that to chase a working-set
/// target would raise `EventsLost`. The maximum stays at least 64 so a small
/// machine still has burst room; 64 KiB times 64 buffers is 4 MiB of kernel pool.
pub fn etw_buffer_limits(logical_processors: usize) -> (u32, u32) {
    let processors = u32::try_from(logical_processors.max(1)).unwrap_or(u32::MAX);
    let minimum = processors.saturating_mul(2).max(4);
    let maximum = minimum.max(64);
    (minimum, maximum)
}

pub fn etw_buffer_ceiling_bytes(logical_processors: usize) -> u64 {
    let (_minimum, maximum) = etw_buffer_limits(logical_processors);
    u64::from(maximum).saturating_mul(64).saturating_mul(1024)
}

pub fn format_local_hms(time: SystemTime) -> String {
    #[cfg(windows)]
    {
        let _ = time;
        windows::local_time_hms()
    }
    #[cfg(not(windows))]
    {
        let secs = time
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let hours = (secs / 3600) % 24;
        let minutes = (secs / 60) % 60;
        let seconds = secs % 60;
        format!("{hours:02}:{minutes:02}:{seconds:02} UTC")
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::privilege_label;

    #[test]
    fn non_windows_does_not_pretend_to_know_the_token() {
        assert_eq!(privilege_label(), "権限は未判定");
    }

    #[test]
    fn etw_ceiling_is_the_configured_maximum_not_a_zero() {
        assert_eq!(super::etw_buffer_limits(24), (48, 64));
        assert_eq!(super::etw_buffer_ceiling_bytes(24), 64 * 64 * 1024);
        let mut snapshot = crate::model::Snapshot::bare(1, std::time::Instant::now());
        assert_eq!(
            super::etw_buffer_ceiling(&snapshot),
            crate::metric::MetricValue::Unavailable
        );
        snapshot.cpu_detail = crate::model::CpuDetailSnapshot::needs_admin();
        snapshot.logical_processors = crate::metric::MetricValue::Value(24);
        assert_eq!(
            super::etw_buffer_ceiling(&snapshot),
            crate::metric::MetricValue::Value(64 * 64 * 1024)
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::metric::MetricValue;
    use crate::model::ProcessNetMode;

    #[test]
    fn enrichers_do_not_rewrite_cpu_or_invent_process_network() {
        let mut probe = windows::WindowsSystemProbe::new();
        let mut sample = probe.probe();
        let before: Vec<_> = sample
            .processes
            .iter()
            .map(|process| (process.pid, process.cpu_time_100ns, process.memory_bytes))
            .collect();
        for enricher in &mut make_enrichers(Arc::new(AtomicBool::new(false))) {
            enricher.enrich(&mut sample);
        }
        assert_eq!(sample.process_net, ProcessNetMode::NeedsAdmin);
        for (process, (pid, cpu, memory)) in sample.processes.iter().zip(&before) {
            assert_eq!(process.pid, *pid);
            assert_eq!(process.cpu_time_100ns, *cpu);
            assert_eq!(process.memory_bytes, *memory);
            assert!(matches!(process.net_send_bytes, MetricValue::Unavailable));
            assert!(matches!(process.net_recv_bytes, MetricValue::Unavailable));
            assert!(!matches!(process.disk_read_bytes, MetricValue::Unsupported));
        }
    }
}

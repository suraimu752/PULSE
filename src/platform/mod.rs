//! Operating-system boundary. Everything that calls Windows lives under `windows`.

use std::time::SystemTime;
#[cfg(not(windows))]
use std::time::UNIX_EPOCH;

use crate::collector::{MetricEnricher, SystemProbe};

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

pub fn make_enrichers() -> Vec<Box<dyn MetricEnricher>> {
    Vec::new()
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

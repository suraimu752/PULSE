//! PULSE (Process Usage and Load Surveillance Engine)
//!
//! Phase 1 splits the program into a sampler thread and a Slint UI thread.
//! Collectors publish aggregated snapshots; the UI never calls Windows APIs.

mod aggregate;
mod collector;
mod config;
mod format;
mod gpuagg;
mod history;
mod launch;
mod metric;
#[cfg(feature = "mock")]
mod mock;
mod model;
mod platform;
mod present;
mod sampler;
mod series;
mod sort;
pub mod theme;

#[cfg(feature = "ui")]
mod ui;

pub use aggregate::aggregate;
pub use collector::{MetricEnricher, SystemProbe};
pub use config::{history_capacity, CPU_SAMPLE_INTERVAL, HISTORY_WINDOW, MEMORY_SAMPLE_INTERVAL};
pub use history::{align_right, RingBuffer};
pub use launch::LaunchOptions;
pub use metric::MetricValue;
pub use model::{ProcessKey, ProcessSnapshot, RawProcess, RawSample, ResourceId, Snapshot};
pub use present::top_entries;
pub use present::{present_detail, present_rows, status_line, DetailText, PresentedRow};
pub use sampler::{PublishedSample, SamplerCore, SamplerHandle};
pub use sort::{SortColumn, SortDirection};

/// Start the sampler and, when the `ui` feature is enabled, the window.
///
/// Without the `ui` feature this returns an error. The Windows collectors can
/// still be linked for tests and `cargo check`.
pub fn run() -> Result<(), String> {
    run_with(LaunchOptions::from_env()?)
}

pub fn run_with(options: LaunchOptions) -> Result<(), String> {
    #[cfg(feature = "ui")]
    {
        ui::run(options)
    }
    #[cfg(not(feature = "ui"))]
    {
        let _ = options;
        Err("PULSE was built without the ui feature, so there is no window to open.".to_string())
    }
}

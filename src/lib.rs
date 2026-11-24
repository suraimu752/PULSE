//! PULSE (Process Usage and Load Surveillance Engine)
//!
//! Phase 1 splits the program into a sampler thread and a Slint UI thread.
//! Collectors publish aggregated snapshots; the UI never calls Windows APIs.

mod aggregate;
mod collector;
mod config;
mod cpudetail;
mod devices;
mod format;
mod gpuagg;
mod history;
mod launch;
mod metric;
#[cfg(feature = "mock")]
mod mock;
mod model;
mod names;
#[cfg(any(test, feature = "ui"))]
mod pick;
mod platform;
mod present;
#[cfg(any(test, feature = "ui"))]
mod rank;
mod sampler;
mod selfmeter;
mod series;
mod sort;
pub mod theme;
#[cfg(feature = "ui")]
mod uiprefs;

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

#[cfg(test)]
mod windows_manifest {
    #[test]
    fn elevation_is_require_administrator_and_bins_only() {
        let manifest = include_str!("../assets/windows/pulse.exe.manifest");
        assert!(
            manifest.contains("requireAdministrator"),
            "the embedded manifest must request elevation"
        );
        assert!(
            !manifest.contains("asInvoker"),
            "the manifest must not fall back to asInvoker"
        );
        let build = include_str!("../build.rs");
        assert!(
            build.contains("requireAdministrator"),
            "the MSVC build must refuse to ship without this level"
        );
        assert!(
            build.contains("rustc-link-arg-bin=PULSE"),
            "MSVC elevation is attached to the PULSE bin"
        );
        assert!(
            build.contains("1 24"),
            "the manifest is a resource, not a bare /MANIFESTUAC flag"
        );
        assert!(
            !build.contains("MANIFESTUAC"),
            "/MANIFESTUAC does not embed a manifest when the linker is not writing one"
        );
        assert!(
            !build.contains("rustc-link-arg-bins"),
            "the bins-wide form is broader than the named bin"
        );
        assert!(
            !build.contains("cargo:rustc-link-arg="),
            "cargo:rustc-link-arg would also stamp test binaries"
        );
        let wrap = include_str!("../assets/windows/link-wrap.sh");
        assert!(
            wrap.contains("PULSE.exe"),
            "the GNU linker adds the manifest only for PULSE.exe"
        );
        let config = include_str!("../.cargo/config.toml");
        assert!(config.contains("link-wrap.sh"));
        let package = include_str!("../Cargo.toml");
        assert!(
            package.contains("test = false"),
            "src/main.rs has no tests; CI runs --lib --tests so it does not launch the bin harness"
        );
    }
}

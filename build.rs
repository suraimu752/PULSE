fn main() {
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/windows/pulse.exe.manifest");
    embed_windows_admin_manifest();
    if std::env::var_os("CARGO_FEATURE_UI").is_some() {
        let mut config = slint_build::CompilerConfiguration::new().with_style("fluent".to_string());
        // Element ids are how the headless layout tests find widgets. Release builds stay smaller.
        if std::env::var("PROFILE").ok().as_deref() != Some("release") {
            config = config.with_debug_info(true);
        }
        slint_build::compile_with_config("ui/app.slint", config).expect("compile Slint UI");
    }
}

/// Ask the MSVC linker to elevate `PULSE.exe`.
///
/// GNU Windows builds do this in `assets/windows/link-wrap.sh`, which is the
/// linker from `.cargo/config.toml`. That script looks at the output file name.
/// This function only handles MSVC (the CI release). It names the `PULSE` bin.
/// Cargo 1.100 still forwards that instruction to the empty unit-test harness
/// compiled from `src/main.rs`, so CI runs `cargo test --lib --tests` and does
/// not launch `PULSE-<hash>.exe`.
fn embed_windows_admin_manifest() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "windows" {
        return;
    }
    let abi = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if abi != "msvc" {
        return;
    }
    // `/MANIFESTUAC` updates the linker-generated manifest. A second
    // RT_MANIFEST resource would conflict with it.
    println!("cargo:rustc-link-arg-bin=PULSE=/MANIFESTUAC:level=requireAdministrator");
    println!("cargo:rustc-link-arg-bin=PULSE=/MANIFESTUAC:uiAccess=false");
}

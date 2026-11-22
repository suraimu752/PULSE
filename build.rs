fn main() {
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_UI").is_some() {
        let mut config = slint_build::CompilerConfiguration::new().with_style("fluent".to_string());
        // Element ids are how the headless layout tests find widgets. Release builds stay smaller.
        if std::env::var("PROFILE").ok().as_deref() != Some("release") {
            config = config.with_debug_info(true);
        }
        slint_build::compile_with_config("ui/app.slint", config).expect("compile Slint UI");
    }
}

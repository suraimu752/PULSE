fn main() {
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_UI").is_some() {
        let config = slint_build::CompilerConfiguration::new().with_style("fluent".to_string());
        slint_build::compile_with_config("ui/app.slint", config).expect("compile Slint UI");
    }
}

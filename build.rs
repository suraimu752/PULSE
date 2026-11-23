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

/// Embed `requireAdministrator` into the MSVC `PULSE` bin.
///
/// The linker UAC flag only edits a manifest the linker is already writing.
/// rustc passes `/MANIFEST:NO` and does not emit RT_MANIFEST, so that flag
/// produced an exe with no manifest and Windows launched it asInvoker.
/// This compiles the XML to a single resource (id 1, type 24) and links that
/// `.res`. It does not also pass `/MANIFEST:EMBED`: two manifests make the
/// linker reject the binary.
///
/// GNU Windows builds do this in `assets/windows/link-wrap.sh`. Cargo 1.100
/// still forwards `rustc-link-arg-bin=PULSE` to the empty unit-test harness
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
    let manifest = std::path::PathBuf::from("assets/windows/pulse.exe.manifest");
    if let Err(error) = embed_res(&manifest) {
        panic!("refusing to build PULSE.exe without an embedded requireAdministrator manifest: {error}");
    }
}

fn embed_res(manifest: &std::path::Path) -> Result<(), String> {
    let out =
        std::path::PathBuf::from(std::env::var("OUT_DIR").map_err(|error| error.to_string())?);
    let copied = out.join("pulse.exe.manifest");
    std::fs::copy(manifest, &copied).map_err(|error| error.to_string())?;
    let rc_text = "1 24 \"pulse.exe.manifest\"\r\n";
    std::fs::write(out.join("pulse.rc"), rc_text).map_err(|error| error.to_string())?;
    let rc_exe = find_rc().ok_or_else(|| "rc.exe was not found".to_string())?;
    let status = std::process::Command::new(&rc_exe)
        .current_dir(&out)
        .args(["/nologo", "/fo", "pulse.res", "pulse.rc"])
        .status()
        .map_err(|error| format!("failed to run {}: {error}", rc_exe.display()))?;
    if !status.success() {
        return Err(format!("rc.exe failed with {status}"));
    }
    let res = out.join("pulse.res");
    if !res.is_file() {
        return Err("rc.exe did not write pulse.res".to_string());
    }
    let rendered = res.display().to_string();
    if rendered.contains(' ') {
        println!("cargo:rustc-link-arg-bin=PULSE=\"{rendered}\"");
    } else {
        println!("cargo:rustc-link-arg-bin=PULSE={rendered}");
    }
    Ok(())
}

fn find_rc() -> Option<std::path::PathBuf> {
    if let Some(path) = command_on_path("rc.exe") {
        return Some(path);
    }
    let mut found = Vec::new();
    for key in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Ok(root) = std::env::var(key) else {
            continue;
        };
        let bins = std::path::PathBuf::from(root)
            .join("Windows Kits")
            .join("10")
            .join("bin");
        let Ok(entries) = std::fs::read_dir(&bins) else {
            continue;
        };
        for entry in entries.flatten() {
            let rc = entry.path().join("x64").join("rc.exe");
            if rc.is_file() {
                found.push(rc);
            }
        }
    }
    found.sort();
    found.pop()
}

fn command_on_path(name: &str) -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("where")
        .arg(name)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next()?.trim();
    if first.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(first))
    }
}

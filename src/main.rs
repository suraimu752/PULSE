// SPDX-License-Identifier: GPL-3.0-only

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    if let Err(err) = pulse::run() {
        report_fatal(&err);
        std::process::exit(1);
    }
}

fn report_fatal(message: &str) {
    eprintln!("PULSE の起動に失敗しました: {message}");
    #[cfg(windows)]
    show_message_box(message);
}

#[cfg(windows)]
fn show_message_box(message: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    fn wide(text: &str) -> Vec<u16> {
        OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let text = wide(&format!("PULSE の起動に失敗しました。\n{message}"));
    let title = wide("PULSE");
    // SAFETY: `text` and `title` are NUL-terminated and live for the call.
    // A null owner window is valid for a process-modal error dialog.
    unsafe {
        let _ = MessageBoxW(
            None::<HWND>,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

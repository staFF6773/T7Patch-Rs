#![cfg_attr(not(test), windows_subsystem = "windows")]

// Share only the data/loader modules, not the DLL's game hooks, with the EXE.
#[path = "../launcher/mod.rs"]
mod launcher;
#[allow(dead_code)]
#[path = "../launcher_api.rs"]
mod launcher_api;
#[path = "../settings.rs"]
mod settings;

fn main() {
    let smoke = std::env::args().any(|arg| arg == "--ui-smoke-test");
    if let Err(error) = launcher::run(smoke) {
        if smoke {
            eprintln!("UI smoke test failed: {error}");
            std::process::exit(1);
        }
        let text: Vec<_> = error.encode_utf16().chain(Some(0)).collect();
        let title: Vec<_> = "T7 Patch Rust".encode_utf16().chain(Some(0)).collect();
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                0x10,
            );
        }
        std::process::exit(1);
    }
}

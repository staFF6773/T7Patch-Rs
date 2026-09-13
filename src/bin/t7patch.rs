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
    let args: Vec<_> = std::env::args_os().collect();
    let smoke = args.iter().any(|arg| arg == "--ui-smoke-test");
    let verifying = args
        .get(1)
        .is_some_and(|arg| arg == "--verify-update-package");
    let result = if verifying {
        if args.len() == 3 {
            launcher::updater::verify_package(std::path::Path::new(&args[2]))
        } else {
            Err("Usage: t7patch.exe --verify-update-package <directory>".into())
        }
    } else if args
        .get(1)
        .is_some_and(|arg| arg == "--apply-update" || arg == "--recover-update")
    {
        launcher::updater::install::helper(&args)
    } else {
        launcher::run(smoke)
    };
    if let Err(error) = result {
        if smoke || verifying {
            eprintln!("Launcher check failed: {error}");
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

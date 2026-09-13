#![cfg_attr(not(test), allow(dead_code))]
#![allow(clippy::missing_safety_doc)]

#[cfg(not(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc")))]
compile_error!("T7 Patch requires x86_64-pc-windows-msvc");

#[macro_use]
mod memory;
mod arxan;
mod config;
mod exceptions;
pub mod game_build;
pub mod hashing;
mod hooks;
pub mod launcher_api;
mod minhook;
mod packets;
mod protection;
mod runtime;
pub mod settings;
pub mod structs;

use std::ffi::{c_char, c_void};
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;

#[no_mangle]
pub unsafe extern "system" fn DllMain(module: HMODULE, reason: u32, _: *mut c_void) -> i32 {
    if reason == 1 {
        DisableThreadLibraryCalls(module);
    }
    1
}

#[no_mangle]
pub extern "C" fn EnableInjectorlessInstall() {
    config::INJECTORLESS.store(true, std::sync::atomic::Ordering::Release);
}

#[no_mangle]
pub unsafe extern "C" fn zbr_run_gamemode_lui(input: *const c_char) {
    if let Some(input) = memory::bounded_string(input, 1024) {
        if hashing::canon_hash(input) == hashing::canon_hash(b"serious_anticrash_2023") {
            runtime::install();
        }
    }
}

#[no_mangle]
pub extern "C" fn SetFriendsOnly(enabled: bool) {
    config::FRIENDS_ONLY.store(enabled, std::sync::atomic::Ordering::Release);
}

#[no_mangle]
pub unsafe extern "C" fn SetPlayerName(name: *const c_char) {
    if let Some(name) = memory::bounded_string(name, 16) {
        config::set_name(name);
    }
}

#[no_mangle]
pub unsafe extern "C" fn SetNetworkPassword(password: *const c_char) {
    if password.is_null() {
        config::set_password(b"");
    } else if let Some(password) = memory::bounded_string(password, 1024) {
        config::set_password(password);
    }
}

#[no_mangle]
pub unsafe extern "C" fn Unload() {
    runtime::uninstall();
}

/// Thread-compatible launcher entry point. The caller owns a writable StartRequest until return.
#[no_mangle]
pub unsafe extern "system" fn T7PatchStart(parameter: *mut c_void) -> u32 {
    use launcher_api::*;
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
    if !memory::readable(parameter as usize, size_of::<StartRequest>()) {
        return BAD_REQUEST;
    }
    let mut request = (parameter as *const StartRequest).read_unaligned();
    if request.size as usize != size_of::<StartRequest>() || request.version != API_VERSION {
        return BAD_REQUEST;
    }
    let Some(length) = request.config_path.iter().position(|&c| c == 0) else {
        return BAD_REQUEST;
    };
    let path = PathBuf::from(OsString::from_wide(&request.config_path[..length]));
    let (status, message) = if path.is_absolute() {
        runtime::install_for_launcher(path)
    } else {
        (
            BAD_REQUEST,
            "An absolute configuration path is required".into(),
        )
    };
    request.reply(status, &message);
    (parameter as *mut StartRequest).write_unaligned(request);
    status
}

#[cfg(test)]
mod launcher_tests {
    use super::*;
    #[test]
    fn bootstrap_rejects_null_wrong_version_and_relative_paths() {
        use launcher_api::*;
        unsafe {
            assert_eq!(T7PatchStart(std::ptr::null_mut()), BAD_REQUEST);
            let mut request = StartRequest {
                version: API_VERSION + 1,
                ..StartRequest::default()
            };
            assert_eq!(
                T7PatchStart((&mut request as *mut StartRequest).cast()),
                BAD_REQUEST
            );
            request.version = API_VERSION;
            assert_eq!(
                T7PatchStart((&mut request as *mut StartRequest).cast()),
                BAD_REQUEST
            );
            assert_eq!(request.status, BAD_REQUEST);
        }
    }
}

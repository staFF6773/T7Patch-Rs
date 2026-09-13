//! Load/export/unsupported-host smoke test, runnable on Windows or under Wine.
use std::{
    ffi::{c_char, CStr},
    os::windows::ffi::OsStrExt,
};
use windows_sys::Win32::{Foundation::FreeLibrary, System::LibraryLoader::*};

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .expect("Usage: smoke.exe <absolute path to t7patch.dll>");
    let path = std::path::absolute(path).unwrap();
    let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let dll = LoadLibraryW(wide.as_ptr());
        assert!(
            !dll.is_null(),
            "LoadLibrary failed: {}",
            std::io::Error::last_os_error()
        );
        let symbol = |name: &CStr| {
            GetProcAddress(dll, name.as_ptr().cast())
                .unwrap_or_else(|| panic!("Missing export {name:?}")) as *const ()
        };
        let friends: unsafe extern "C" fn(bool) = std::mem::transmute(symbol(c"SetFriendsOnly"));
        let name: unsafe extern "C" fn(*const c_char) =
            std::mem::transmute(symbol(c"SetPlayerName"));
        let password: unsafe extern "C" fn(*const c_char) =
            std::mem::transmute(symbol(c"SetNetworkPassword"));
        let injectorless: unsafe extern "C" fn() =
            std::mem::transmute(symbol(c"EnableInjectorlessInstall"));
        let install: unsafe extern "C" fn(*const c_char) =
            std::mem::transmute(symbol(c"zbr_run_gamemode_lui"));
        let unload: unsafe extern "C" fn() = std::mem::transmute(symbol(c"Unload"));
        let status = symbol(c"T7PatchStatus").cast::<std::sync::atomic::AtomicU32>();
        assert_eq!((*status).load(std::sync::atomic::Ordering::Acquire), 1);
        friends(true);
        name(c"Rust smoke".as_ptr());
        name(std::ptr::null());
        name(c"too long player name".as_ptr());
        password(c"Test123".as_ptr());
        password(std::ptr::null());
        injectorless();
        install(std::ptr::null());
        install(c"wrong token".as_ptr());
        // This process is not BO3: installation must reject it without writing game addresses.
        install(c"serious_anticrash_2023".as_ptr());
        unload();
        unload();
        assert_ne!(FreeLibrary(dll), 0);
    }
    println!("DLL load, six API exports, null inputs and unsupported-host rejection: OK");
}

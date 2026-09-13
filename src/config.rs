use crate::{game_build, hashing, memory};
use std::{
    ffi::c_char,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicPtr, Ordering},
        Mutex,
    },
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;

pub static INJECTORLESS: AtomicBool = AtomicBool::new(true);
pub static FRIENDS_ONLY: AtomicBool = AtomicBool::new(false);
pub static GAME_READY: AtomicBool = AtomicBool::new(false);
static DEFAULT_NAME: [u8; 16] = *b"Unknown Soldier\0";
static NAME: AtomicPtr<u8> = AtomicPtr::new(DEFAULT_NAME.as_ptr().cast_mut());
static NAME_UPDATE: Mutex<()> = Mutex::new(());
static PASSWORD: Mutex<[u64; 3]> = Mutex::new([0; 3]);
static CONFIG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_path(path: PathBuf) {
    *CONFIG_PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
}
pub(crate) fn path() -> PathBuf {
    CONFIG_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| PathBuf::from("t7patch.conf"))
}

pub fn name_ptr() -> *const c_char {
    NAME.load(Ordering::Acquire).cast()
}
pub fn password() -> [u64; 3] {
    *PASSWORD.lock().unwrap_or_else(|e| e.into_inner())
}
pub fn set_password(bytes: &[u8]) {
    let mut password = PASSWORD.lock().unwrap_or_else(|e| e.into_inner());
    password[0] = password[1];
    password[1] = if bytes.is_empty() {
        0
    } else {
        hashing::canon_hash64(bytes)
    };
    password[2] = unsafe { GetTickCount64() };
}
pub unsafe fn set_name(bytes: &[u8]) {
    if bytes.len() > 15 || bytes.contains(&0) {
        return;
    }
    let mut name = [0; 16];
    name[..bytes.len()].copy_from_slice(bytes);
    let _update = NAME_UPDATE.lock().unwrap_or_else(|e| e.into_inner());
    if memory::read::<[u8; 16]>(NAME.load(Ordering::Acquire) as usize) != name {
        // Steam callers may retain the returned pointer, even after their thread exits.
        // Each changed name gets immutable process-lifetime storage (16 bytes).
        NAME.store(Box::into_raw(Box::new(name)).cast(), Ordering::Release);
    }
    if GAME_READY.load(Ordering::Acquire) {
        for rva in [0x14F344B8, 0x15E056C8, 0x15E84638, 0x113A4970, 0x113A48F0] {
            if let Err(e) = memory::write_bytes(game_build::address(rva), &name) {
                memory::debug(&e);
            }
        }
        let data_ptr = game_build::address(0x3390190);
        if memory::readable(data_ptr, 8) {
            let data = memory::read::<usize>(data_ptr);
            if data != 0 && memory::readable(data + 8, 16) {
                let _ = memory::write_bytes(data + 8, &name);
            }
        }
    }
}

pub use crate::settings::Config;
impl Config {
    pub unsafe fn apply(&self) {
        set_name(&self.name);
        FRIENDS_ONLY.store(self.friends_only, Ordering::Release);
        if password()[1]
            != if self.password.is_empty() {
                0
            } else {
                hashing::canon_hash64(&self.password)
            }
        {
            set_password(&self.password);
        }
    }
}

pub struct Watcher {
    config: Config,
    path: PathBuf,
    contents: Vec<u8>,
}
impl Watcher {
    pub unsafe fn start() -> Self {
        let path = path();
        let mut config = Config::default();
        let mut contents = Vec::new();
        match std::fs::read(&path) {
            Ok(bytes) => {
                config.parse(&bytes);
                contents = bytes;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                use std::io::Write;
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    if let Err(e) = file.write_all(
                        b"playername=Unknown Soldier\nisfriendsonly=1\nnetworkpassword=\n",
                    ) {
                        memory::debug(&e.to_string());
                    }
                }
            }
            Err(e) => memory::debug(&e.to_string()),
        }
        config.apply();
        Self {
            config,
            contents,
            path,
        }
    }
    pub unsafe fn poll(&mut self) {
        let path = path();
        if let Ok(bytes) = std::fs::read(&path) {
            if self.path != path || bytes != self.contents {
                self.config.parse(&bytes);
                self.config.apply();
                self.path = path;
                self.contents = bytes;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_config_crlf_clear_and_last_line() {
        let mut c = Config::default();
        c.parse(b"playername=12345678901234567890\r\nisfriendsonly=0\r\nnetworkpassword=a=b");
        assert_eq!(c.name, b"123456789012345");
        assert!(!c.friends_only);
        assert_eq!(c.password, b"a=b");
        c.parse(b"networkpassword=\n");
        assert!(c.password.is_empty());
    }
}

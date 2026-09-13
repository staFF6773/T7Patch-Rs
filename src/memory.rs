//! The process-memory boundary. Callers must ensure game objects remain live while accessed.
use std::ffi::{c_char, c_void};
use windows_sys::Win32::System::{
    Diagnostics::Debug::FlushInstructionCache, Memory::*, Threading::GetCurrentProcess,
};

macro_rules! game_fn {
    ($rva:expr, $ty:ty) => {
        std::mem::transmute::<usize, $ty>(crate::game_build::address($rva))
    };
}

pub unsafe fn read<T: Copy>(address: usize) -> T {
    (address as *const T).read_unaligned()
}
pub unsafe fn store<T>(address: usize, value: T) {
    (address as *mut T).write_unaligned(value);
}

pub fn readable(address: usize, size: usize) -> bool {
    if address == 0 || size == 0 {
        return false;
    }
    let Some(end) = address.checked_add(size) else {
        return false;
    };
    let mut cursor = address;
    while cursor < end {
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            VirtualQuery(
                cursor as _,
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        } == 0
            || info.State != MEM_COMMIT
            || info.Protect & (PAGE_GUARD | PAGE_NOACCESS) != 0
            || info.Protect
                & (PAGE_READONLY
                    | PAGE_READWRITE
                    | PAGE_WRITECOPY
                    | PAGE_EXECUTE_READ
                    | PAGE_EXECUTE_READWRITE
                    | PAGE_EXECUTE_WRITECOPY)
                == 0
        {
            return false;
        }
        let Some(next) = (info.BaseAddress as usize).checked_add(info.RegionSize) else {
            return false;
        };
        if next <= cursor {
            return false;
        }
        cursor = next;
    }
    true
}

pub unsafe fn bounded_string<'a>(ptr: *const c_char, limit: usize) -> Option<&'a [u8]> {
    // Check each region once, rather than VirtualQuery for every character.
    if ptr.is_null() {
        return None;
    }
    let start = ptr as usize;
    let end = start.checked_add(limit)?;
    let mut cursor = start;
    while cursor < end {
        let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        if VirtualQuery(
            cursor as _,
            &mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        ) == 0
        {
            return None;
        }
        let region_end = (info.BaseAddress as usize)
            .checked_add(info.RegionSize)?
            .min(end);
        if region_end <= cursor || !readable(cursor, region_end - cursor) {
            return None;
        }
        while cursor < region_end {
            if read::<u8>(cursor) == 0 {
                return Some(std::slice::from_raw_parts(ptr.cast(), cursor - start));
            }
            cursor += 1;
        }
    }
    None
}

pub unsafe fn write_bytes(address: usize, bytes: &[u8]) -> Result<(), String> {
    if !readable(address, bytes.len()) {
        return Err(format!("Unreadable patch address {address:#x}"));
    }
    let mut old = 0;
    if VirtualProtect(address as _, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
        return Err("VirtualProtect failed".into());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
    let flushed = FlushInstructionCache(GetCurrentProcess(), address as _, bytes.len());
    let mut ignored = 0;
    let restored = VirtualProtect(address as _, bytes.len(), old, &mut ignored);
    if flushed == 0 || restored == 0 {
        return Err("Could not flush/restore patched memory".into());
    }
    Ok(())
}

pub struct Patch {
    pub address: usize,
    pub original: Vec<u8>,
    pub replacement: Vec<u8>,
}
impl Patch {
    pub unsafe fn prepare(address: usize, replacement: &[u8]) -> Result<Self, String> {
        if !readable(address, replacement.len()) {
            return Err(format!("Invalid patch address {address:#x}"));
        }
        Ok(Self {
            address,
            original: std::slice::from_raw_parts(address as *const u8, replacement.len()).to_vec(),
            replacement: replacement.to_vec(),
        })
    }
    pub unsafe fn apply(&self) -> Result<(), String> {
        write_bytes(self.address, &self.replacement)
    }
    pub unsafe fn restore(&self) -> Result<(), String> {
        write_bytes(self.address, &self.original)
    }
}

pub unsafe fn copy_string(dest: *mut c_char, capacity: usize, bytes: &[u8]) -> bool {
    if capacity == 0 || !readable(dest as usize, capacity) {
        return false;
    }
    let n = bytes.len().min(capacity - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), dest.cast(), n);
    *dest.add(n) = 0;
    true
}

pub fn debug(message: &str) {
    let text = format!("T7 Patch Rust: {}\n\0", message.replace('\0', ""));
    unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringA(text.as_ptr());
    }
}

pub unsafe fn symbol(module: &std::ffi::CStr, name: &std::ffi::CStr) -> Result<usize, String> {
    use windows_sys::Win32::System::LibraryLoader::*;
    let handle = GetModuleHandleA(module.as_ptr().cast());
    GetProcAddress(handle, name.as_ptr().cast())
        .map(|f| f as *const c_void as usize)
        .ok_or_else(|| format!("Missing symbol {}", name.to_string_lossy()))
}

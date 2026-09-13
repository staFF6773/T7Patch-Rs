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

fn readable_region(info: &MEMORY_BASIC_INFORMATION) -> bool {
    info.State == MEM_COMMIT
        && info.Protect & (PAGE_GUARD | PAGE_NOACCESS) == 0
        && info.Protect
            & (PAGE_READONLY
                | PAGE_READWRITE
                | PAGE_WRITECOPY
                | PAGE_EXECUTE_READ
                | PAGE_EXECUTE_READWRITE
                | PAGE_EXECUTE_WRITECOPY)
            != 0
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
            || !readable_region(&info)
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

/// Read a live engine-owned string without querying the process's virtual memory map.
///
/// # Safety
/// Apart from NULL (which is rejected), `ptr` must be readable through the first NUL
/// or `limit` bytes, whichever comes first. Its allocation must remain live and
/// immutable for the returned lifetime. This is the same pointer contract used by
/// the original UI hooks' strlen, with a bounded scan instead of an unbounded one.
/// This does not validate arbitrary pointers; use bounded_string at external boundaries.
pub(crate) unsafe fn game_string<'a>(ptr: *const c_char, limit: usize) -> Option<&'a [u8]> {
    let _sample = crate::profiling::GAME_STRING.enter();
    if ptr.is_null() || limit > isize::MAX as usize {
        return None;
    }
    (ptr as usize).checked_add(limit)?;
    // Do not form a limit-sized slice up front: a short string may end at the
    // allocation/page boundary. Read only as far as the terminator.
    for length in 0..limit {
        if ptr.add(length).read() == 0 {
            return Some(std::slice::from_raw_parts(ptr.cast(), length));
        }
    }
    None
}

pub unsafe fn bounded_string<'a>(ptr: *const c_char, limit: usize) -> Option<&'a [u8]> {
    let _sample = crate::profiling::BOUNDED_STRING.enter();
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
        // Reuse the query above; do not make a second syscall for the same region.
        if region_end <= cursor || !readable_region(&info) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_strings_respect_limits_and_memory_protection_boundaries() {
        unsafe {
            let mut system = std::mem::zeroed();
            windows_sys::Win32::System::SystemInformation::GetSystemInfo(&mut system);
            let page = system.dwPageSize as usize;
            let allocation = VirtualAlloc(
                std::ptr::null(),
                page * 3,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            );
            assert!(!allocation.is_null());
            struct Allocation(*mut c_void);
            impl Drop for Allocation {
                fn drop(&mut self) {
                    unsafe { VirtualFree(self.0, 0, MEM_RELEASE) };
                }
            }
            let _allocation = Allocation(allocation);
            let bytes = allocation.cast::<u8>();
            std::ptr::write_bytes(bytes, b'x', page * 3);
            *bytes.add(page + 1) = 0;
            *bytes.add(page * 2 - 1) = 0;
            let mut old = 0;
            assert_ne!(
                VirtualProtect(bytes.add(page).cast(), page, PAGE_READONLY, &mut old),
                0
            );
            assert_ne!(
                VirtualProtect(bytes.add(page * 2).cast(), page, PAGE_NOACCESS, &mut old),
                0
            );

            // Cross two readable regions with different protections.
            let string = bytes.add(page - 2).cast();
            assert_eq!(bounded_string(string, 4), Some(b"xxx".as_slice()));
            assert_eq!(bounded_string(string, 3), None);
            assert_eq!(game_string(string, 4), Some(b"xxx".as_slice()));
            assert_eq!(game_string(string, 3), None);
            // Stop at NUL without touching the inaccessible next page.
            assert_eq!(
                bounded_string(bytes.add(page * 2 - 2).cast(), 8),
                Some(b"x".as_slice())
            );
            assert_eq!(bounded_string(bytes.add(page * 2).cast(), 8), None);
            assert_eq!(
                game_string(bytes.add(page * 2 - 2).cast(), 65536),
                Some(b"x".as_slice())
            );
            assert!(!readable(bytes.add(page * 2 - 2) as usize, 8));

            assert_ne!(
                VirtualProtect(
                    bytes.add(page).cast(),
                    page,
                    PAGE_READWRITE | PAGE_GUARD,
                    &mut old
                ),
                0
            );
            assert_eq!(bounded_string(string, 4), None);
            assert!(!readable(bytes.add(page) as usize, 1));
            // Querying a guard page must not consume its guard flag.
            let mut info = std::mem::zeroed();
            assert_ne!(
                VirtualQuery(
                    bytes.add(page).cast(),
                    &mut info,
                    size_of::<MEMORY_BASIC_INFORMATION>()
                ),
                0
            );
            assert_ne!(info.Protect & PAGE_GUARD, 0);
            assert_eq!(bounded_string(std::ptr::null(), 8), None);
            assert_eq!(bounded_string(string, 0), None);
            assert_eq!(bounded_string(usize::MAX as *const c_char, 2), None);
        }
    }

    #[test]
    fn game_strings_preserve_length_limits_and_bytes() {
        unsafe {
            assert_eq!(game_string(std::ptr::null(), 4096), None);
            assert_eq!(game_string(c"".as_ptr(), 0), None);
            assert_eq!(game_string(c"".as_ptr(), 1), Some(b"".as_slice()));
            for limit in [64, 4096, 65536] {
                let mut bytes = vec![0xffu8; limit];
                let ptr = bytes.as_ptr().cast();
                assert_eq!(game_string(ptr, limit), None);
                bytes[limit - 1] = 0;
                assert_eq!(game_string(ptr, limit), Some(&bytes[..limit - 1]));
                assert_eq!(game_string(ptr, limit), bounded_string(ptr, limit));
                assert_eq!(game_string(ptr, limit - 1), None);
                bytes[1] = 0;
                assert_eq!(game_string(ptr, limit), Some(&bytes[..1]));
            }
        }
    }

    /// Manual relative-cost check; BO3's address-space/query cost must be measured in-game.
    #[test]
    #[ignore = "manual Release microbenchmark"]
    fn ui_string_reader_benchmark() {
        use std::{hint::black_box, time::Instant};
        let strings = [
            c"lobbyRoot.lobbyNav",
            c"controller0.playerStats.rank",
            c"hudItems.playerHealth",
            c"menu.buttonPrompt.text",
        ];
        const CALLS: usize = 100_000;
        type Reader = unsafe fn(*const c_char, usize) -> Option<&'static [u8]>;
        let mut timings = [Vec::new(), Vec::new()];
        for _ in 0..5 {
            for (reader, times) in [bounded_string as Reader, game_string as Reader]
                .into_iter()
                .zip(&mut timings)
            {
                let started = Instant::now();
                for call in 0..CALLS {
                    let string = strings[call % strings.len()];
                    let result = unsafe { reader(black_box(string.as_ptr()), black_box(65536)) };
                    assert_eq!(black_box(result).unwrap().len(), string.to_bytes().len());
                }
                times.push(started.elapsed().as_nanos() as f64 / CALLS as f64);
            }
        }
        for times in &mut timings {
            times.sort_by(f64::total_cmp);
        }
        println!("Median of 5 batches: queried={:.1} ns/call, direct={:.1} ns/call, ratio={:.1}x (not an in-game FPS measurement)", timings[0][2], timings[1][2], timings[0][2] / timings[1][2]);
    }
}

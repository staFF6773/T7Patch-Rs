use minhook_sys::{MH_CreateHook, MH_DisableHook, MH_EnableHook, MH_Initialize};

fn status(code: i32) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("MinHook status {code}"))
    }
}

pub unsafe fn initialize() -> Result<(), String> {
    match MH_Initialize() {
        0 | 1 => Ok(()),
        code => status(code),
    }
}
pub unsafe fn create(target: usize, detour: usize) -> Result<usize, String> {
    let mut original = std::ptr::null_mut();
    status(MH_CreateHook(target as _, detour as _, &mut original))?;
    Ok(original as usize)
}
pub unsafe fn enable(target: usize) -> Result<(), String> {
    status(MH_EnableHook(target as _))
}
pub unsafe fn disable(target: usize) -> Result<(), String> {
    status(MH_DisableHook(target as _))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::*;
    #[test]
    fn native_hook_and_trampoline() {
        unsafe extern "C" fn detour() -> i32 {
            42
        }
        unsafe {
            let page = VirtualAlloc(
                std::ptr::null(),
                4096,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            );
            assert!(!page.is_null());
            // mov eax,7; ret; nops. No compiler optimization can inline this target.
            let code = [
                0xb8, 7, 0, 0, 0, 0xc3, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
            ];
            crate::memory::write_bytes(page as usize, &code).unwrap();
            let mut old = 0;
            assert_ne!(VirtualProtect(page, 4096, PAGE_EXECUTE_READ, &mut old), 0);
            let target: unsafe extern "C" fn() -> i32 = std::mem::transmute(page);
            assert_eq!(target(), 7);
            initialize().unwrap();
            let original = create(page as usize, detour as *const () as usize).unwrap();
            let trampoline: unsafe extern "C" fn() -> i32 = std::mem::transmute(original);
            enable(page as usize).unwrap();
            assert_eq!(target(), 42);
            assert_eq!(trampoline(), 7);
            disable(page as usize).unwrap();
            assert_eq!(target(), 7);
            assert_eq!(minhook_sys::MH_RemoveHook(page), 0);
            assert_ne!(VirtualFree(page, 0, MEM_RELEASE), 0);
        }
    }
}

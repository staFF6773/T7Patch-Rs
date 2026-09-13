use crate::{
    game_build::address,
    memory::{self, bounded_string, read, Patch},
    protection,
};
use std::{
    ffi::c_void,
    io::Write,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use windows_sys::Win32::System::{Diagnostics::Debug::*, Threading::*};

static CONTINUE: AtomicUsize = AtomicUsize::new(0);
static SUSPEND: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS: AtomicUsize = AtomicUsize::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static LOGGING: AtomicBool = AtomicBool::new(false);

pub struct Handler {
    // Retained while the pinned module is loaded, including after logical deactivation.
    _dispatcher: Option<Patch>,
    _vectored: usize,
}
impl Handler {
    pub unsafe fn install() -> Result<Self, String> {
        CONTINUE.store(
            memory::symbol(c"ntdll.dll", c"ZwContinue")?,
            Ordering::Release,
        );
        SUSPEND.store(
            memory::symbol(c"ntdll.dll", c"NtSuspendProcess")?,
            Ordering::Release,
        );
        let dispatcher = memory::symbol(c"ntdll.dll", c"KiUserExceptionDispatcher")?;
        let signature = read::<u32>(dispatcher);
        let patch = if signature == 0x058B48FC {
            let displacement = read::<i32>(dispatcher + 4) as isize;
            let slot = (dispatcher + 8).wrapping_add_signed(displacement);
            let patch = Patch::prepare(slot, &(dispatch as *const () as usize).to_le_bytes())?;
            PREVIOUS.store(read(slot), Ordering::Release);
            Some(patch)
        } else if signature == 0x248C8B48 {
            // Original Wine dispatcher layout: RSP=CONTEXT, RSP+0x4f0=EXCEPTION_RECORD.
            let mut bytes = [
                0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0x48, 0x89, 0xe2, 0x48, 0x8d, 0x8c, 0x24, 0xf0,
                0x04, 0, 0, 0xff, 0xd0, 0x90, 0x90, 0x90, 0x90,
            ];
            bytes[2..10].copy_from_slice(&(dispatch as *const () as usize).to_le_bytes());
            Some(Patch::prepare(dispatcher + 0xb, &bytes)?)
        } else {
            None
        };
        let vectored = if let Some(patch) = &patch {
            if let Err(e) = patch.apply() {
                let _ = patch.restore();
                return Err(e);
            }
            0
        } else {
            let handle = AddVectoredExceptionHandler(1, Some(vectored));
            if handle.is_null() {
                return Err("Could not install vectored exception handler".into());
            }
            handle as usize
        };
        ACTIVE.store(true, Ordering::Release);
        Ok(Self {
            _dispatcher: patch,
            _vectored: vectored,
        })
    }
    pub fn deactivate(&self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

unsafe extern "system" fn vectored(pointers: *mut EXCEPTION_POINTERS) -> i32 {
    if pointers.is_null() {
        return 0;
    }
    if handle((*pointers).ExceptionRecord, (*pointers).ContextRecord) {
        -1
    } else {
        0
    }
}

unsafe extern "system" fn dispatch(record: *mut EXCEPTION_RECORD, context: *mut CONTEXT) {
    if handle(record, context) {
        let resume: unsafe extern "system" fn(*mut CONTEXT, u8) -> i32 =
            std::mem::transmute(CONTINUE.load(Ordering::Acquire));
        let status = resume(context, 0);
        memory::debug(&format!("ZwContinue returned {status:#x}"));
    }
    let previous = PREVIOUS.load(Ordering::Acquire);
    if previous != 0 {
        let previous: unsafe extern "system" fn(*mut EXCEPTION_RECORD, *mut CONTEXT) =
            std::mem::transmute(previous);
        previous(record, context);
    }
}

unsafe fn handle(record: *mut EXCEPTION_RECORD, context: *mut CONTEXT) -> bool {
    let _pending = crate::profiling::EXCEPTIONS.track();
    let _sample = crate::profiling::EXCEPTIONS.enter();
    if record.is_null() || context.is_null() {
        return false;
    }
    let record = &*record;
    let context = &mut *context;
    // A thread may have loaded the sentinel just before deactivation.
    if protection::inspect_exception(context) {
        return true;
    }
    if !ACTIVE.load(Ordering::Acquire) || context.Rcx == 0xFFEEDDCC44332211 {
        return false;
    }
    let fault = record.ExceptionAddress as usize;
    for &(from, to, clear_rax) in &[
        (0x22D2F0D, 0x22D389E, true),
        (0x464FEF, 0x4651A2, true),
        (0x15E4B7A, 0x15E4BA3, false),
        (0x12EE4EC, 0x12EE5E8, false),
        (0x22C965C, 0x22C9686, false),
        (0x22C9676, 0x22C9686, false),
        (0x1C9F121, 0x1C9F2CE, false),
        (0x1E9E657, 0x1E9E7E3, false),
        (0x1A604C4, 0x1A6054B, false),
        (0x133EC1, 0x133F12, false),
        (0x133EEB, 0x133F12, false),
        (0x133F31, 0x133F42, false),
        (0x13591F3, 0x13591FA, false),
        (0x11D2580, 0x11D2592, true),
        (0x22C4935, 0x22C49FE, true),
    ] {
        if fault == address(from) {
            if clear_rax {
                context.Rax = 0;
            }
            context.Rip = address(to) as u64;
            return true;
        }
    }
    let empty = c"".as_ptr() as u64;
    if fault == address(0x221CEC3) {
        context.Rdx = empty;
        return true;
    }
    if fault == address(0x221C726) {
        context.Rsi = empty;
        return true;
    }
    if fault == address(0x22328F6) {
        context.Rcx = empty;
        return true;
    }
    let csc = [0xC15B80, 0xC15C50, 0xC18CF5]
        .iter()
        .any(|&rva| fault == address(rva));
    let gsc = [
        0x1A5F94B, 0x1A5FA5E, 0x1A5FB5E, 0x1A5FBFD, 0x1A5FE76, 0x1A5FF86, 0x1A6003D, 0x1A602C7,
    ]
    .iter()
    .any(|&rva| fault == address(rva));
    if csc || gsc {
        context.Rcx = csc as u64;
        context.Rdx = c"Clientfield does not exist".as_ptr() as u64;
        context.R8 = 0;
        context.Rip = address(0x12EA450) as u64;
        return true;
    }
    if fault == address(0x12EA4E0) {
        let instance = (context.Rcx & 0xff) as usize;
        if instance > 1 {
            return false;
        }
        context.Rip += 4;
        context.Rsp -= 0x30;
        let fatal_ptr = address(0x5124869) + 35392 * instance;
        let message_ptr = address(0x51A3710) + 0x78 * instance;
        let text = if memory::readable(message_ptr, 8) {
            bounded_string(read(message_ptr), 65536).unwrap_or(b"unknown")
        } else {
            b"unknown"
        };
        let fatal = memory::readable(fatal_ptr, 1) && read::<u8>(fatal_ptr) != 0;
        if fatal || text.windows(14).any(|w| w == b"Invalid opcode") {
            crate::diagnostics::event(format_args!(
                "fatal-script instance={instance} rip={:#x}",
                context.Rip
            ));
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(crate::diagnostics::crash_path())
            {
                let _ = writeln!(
                    file,
                    "Script fatal exception: instance={instance}, {}",
                    String::from_utf8_lossy(text)
                );
                let fs = address(0x513DD30) + instance * 32;
                if memory::readable(fs, 8) {
                    let _ = writeln!(file, "Script FS: {:#x}", read::<u64>(fs));
                }
                let _ = file.flush();
            }
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxA(
                std::ptr::null_mut(),
                c"A fatal script error occurred. Check crashes.log beside t7patch.conf (or the T7 Patch logs in TEMP)."
                    .as_ptr()
                    .cast(),
                c"T7 Patch - Fatal Script Error".as_ptr().cast(),
                0,
            );
            ExitProcess(0);
        }
        return true;
    }
    if record.ExceptionCode as u32 == 0xC0000005 || record.ExceptionFlags & 1 != 0 {
        crate::diagnostics::event(format_args!(
            "unhandled-exception code={:#x} flags={:#x} address={:#x} rip={:#x} rcx={:#x} game_base={:#x}",
            record.ExceptionCode, record.ExceptionFlags, fault, context.Rip, context.Rcx, crate::game_build::image_base()));
        log_crash(record, context);
        crate::diagnostics::event(format_args!(
            "before-NtSuspendProcess rip={:#x}",
            context.Rip
        ));
        let suspend: unsafe extern "system" fn(*mut c_void) -> i32 =
            std::mem::transmute(SUSPEND.load(Ordering::Acquire));
        suspend(GetCurrentProcess());
        crate::diagnostics::event(format_args!("NtSuspendProcess-returned"));
    }
    false
}

unsafe fn log_crash(record: &EXCEPTION_RECORD, context: &CONTEXT) {
    if LOGGING.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::diagnostics::crash_path())
    {
        let mut module = std::ptr::null_mut();
        RtlPcToFileHeader(record.ExceptionAddress, &mut module);
        let mut name = [0u8; 1024];
        let length = windows_sys::Win32::System::LibraryLoader::GetModuleFileNameA(
            module,
            name.as_mut_ptr(),
            name.len() as u32,
        );
        let _ = writeln!(file, "\nCrash {:?}, thread {}, code {:#x}\nGame: {:#x}, module: {} ({module:p}), address: {:p}",
            std::time::SystemTime::now(), GetCurrentThreadId(), record.ExceptionCode, crate::game_build::image_base(),
            String::from_utf8_lossy(&name[..length as usize]), record.ExceptionAddress);
        for (name, value) in [
            ("RIP", context.Rip),
            ("RSP", context.Rsp),
            ("RBP", context.Rbp),
            ("RAX", context.Rax),
            ("RBX", context.Rbx),
            ("RCX", context.Rcx),
            ("RDX", context.Rdx),
            ("RSI", context.Rsi),
            ("RDI", context.Rdi),
            ("R8", context.R8),
            ("R9", context.R9),
            ("R10", context.R10),
            ("R11", context.R11),
            ("R12", context.R12),
            ("R13", context.R13),
            ("R14", context.R14),
            ("R15", context.R15),
            ("DR0", context.Dr0),
            ("DR1", context.Dr1),
            ("DR2", context.Dr2),
            ("DR3", context.Dr3),
            ("DR7", context.Dr7),
        ] {
            let _ = writeln!(file, "{name}: {value:#018x}");
        }
        for offset in (0..0x400).step_by(16) {
            let ptr = context.Rsp as usize + offset;
            if !memory::readable(ptr, 16) {
                break;
            }
            let _ = writeln!(
                file,
                "[{ptr:#018x}] {:#018x} {:#018x}",
                read::<u64>(ptr),
                read::<u64>(ptr + 8)
            );
        }
        let _ = file.flush();
    } else {
        crate::diagnostics::event(format_args!("crash-log-open-failed"));
    }
    LOGGING.store(false, Ordering::Release);
}

//! Standard Windows loader, restricted by the UI to BlackOps3.exe.
//! Remote buffers remain allocated if the UI exits while a remote call is in flight.
use crate::launcher_api::{self, StartRequest};
use std::{
    ffi::{c_void, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{
        Diagnostics::{
            Debug::{ReadProcessMemory, WriteProcessMemory},
            ToolHelp::*,
        },
        LibraryLoader::*,
        Memory::*,
        Threading::*,
    },
};

pub struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn error(operation: &str) -> String {
    format!("{operation}: {}", std::io::Error::last_os_error())
}
fn wide_string(input: &[u16]) -> OsString {
    OsString::from_wide(&input[..input.iter().position(|&c| c == 0).unwrap_or(input.len())])
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .eq_ignore_ascii_case(b.to_string_lossy().trim_start_matches(r"\\?\"))
}

pub fn find_game() -> Result<Option<u32>, String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(error("Process snapshot"));
        }
        let snapshot = Handle(snapshot);
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of_val(&entry) as u32;
        if Process32FirstW(snapshot.0, &mut entry) == 0 {
            return Err(error("Process enumeration"));
        }
        loop {
            if wide_string(&entry.szExeFile)
                .to_string_lossy()
                .eq_ignore_ascii_case("BlackOps3.exe")
            {
                return Ok(Some(entry.th32ProcessID));
            }
            if Process32NextW(snapshot.0, &mut entry) == 0 {
                return if GetLastError() == ERROR_NO_MORE_FILES {
                    Ok(None)
                } else {
                    Err(error("Process enumeration"))
                };
            }
        }
    }
}

struct Module {
    path: PathBuf,
    base: usize,
}
fn modules(pid: u32) -> Result<Vec<Module>, String> {
    unsafe {
        let mut snapshot = INVALID_HANDLE_VALUE;
        for _ in 0..4 {
            snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
            if snapshot != INVALID_HANDLE_VALUE || GetLastError() != ERROR_BAD_LENGTH {
                break;
            }
        }
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(error("Module snapshot"));
        }
        let snapshot = Handle(snapshot);
        let mut entry: MODULEENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of_val(&entry) as u32;
        if Module32FirstW(snapshot.0, &mut entry) == 0 {
            return Err(error("Module enumeration"));
        }
        let mut result = Vec::new();
        loop {
            result.push(Module {
                path: PathBuf::from(wide_string(&entry.szExePath)),
                base: entry.modBaseAddr as usize,
            });
            if Module32NextW(snapshot.0, &mut entry) == 0 {
                if GetLastError() != ERROR_NO_MORE_FILES {
                    return Err(error("Module enumeration"));
                }
                break;
            }
        }
        Ok(result)
    }
}

// Resolve the module that actually owns LoadLibraryW, including forwarded KernelBase exports.
unsafe fn remote_load_library(pid: u32) -> Result<usize, String> {
    let kernel = GetModuleHandleW(wide(Path::new("kernel32.dll")).as_ptr());
    let function = GetProcAddress(kernel, c"LoadLibraryW".as_ptr().cast())
        .ok_or_else(|| error("LoadLibraryW"))? as *const () as usize;
    let mut owner = std::ptr::null_mut();
    if GetModuleHandleExW(
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        function as _,
        &mut owner,
    ) == 0
    {
        return Err(error("LoadLibraryW owner"));
    }
    let mut path = [0u16; 32768];
    let length = GetModuleFileNameW(owner, path.as_mut_ptr(), path.len() as u32);
    if length == 0 || length as usize >= path.len() {
        return Err(error("Loader module path"));
    }
    let path = PathBuf::from(wide_string(&path));
    let name = path
        .file_name()
        .ok_or("Missing loader module filename")?
        .to_string_lossy();
    let remote = modules(pid)?
        .into_iter()
        .find(|m| {
            m.path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&name))
        })
        .ok_or("Loader module is not ready")?;
    remote
        .base
        .checked_add(function - owner as usize)
        .ok_or_else(|| "Loader address overflow".into())
}

struct RemoteBuffer {
    process: Rc<Handle>,
    address: usize,
    release: bool,
}
impl RemoteBuffer {
    unsafe fn new(process: Rc<Handle>, bytes: &[u8]) -> Result<Self, String> {
        let pointer = VirtualAllocEx(
            process.0,
            std::ptr::null(),
            bytes.len(),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );
        if pointer.is_null() {
            return Err(error("Remote allocation"));
        }
        let buffer = Self {
            process,
            address: pointer as usize,
            release: true,
        };
        let mut written = 0;
        if WriteProcessMemory(
            buffer.process.0,
            pointer,
            bytes.as_ptr().cast(),
            bytes.len(),
            &mut written,
        ) == 0
            || written != bytes.len()
        {
            return Err(error("Writing loader request"));
        }
        Ok(buffer)
    }
}
impl Drop for RemoteBuffer {
    fn drop(&mut self) {
        if self.release {
            unsafe {
                VirtualFreeEx(self.process.0, self.address as _, 0, MEM_RELEASE);
            }
        }
    }
}
struct RemoteCall {
    thread: Handle,
    buffer: RemoteBuffer,
    started: Instant,
}
impl RemoteCall {
    unsafe fn start(process: Rc<Handle>, entry: usize, bytes: &[u8]) -> Result<Self, String> {
        let buffer = RemoteBuffer::new(process.clone(), bytes)?;
        let entry: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(entry);
        let thread = CreateRemoteThread(
            process.0,
            std::ptr::null(),
            0,
            Some(entry),
            buffer.address as _,
            0,
            std::ptr::null_mut(),
        );
        if thread.is_null() {
            return Err(error("Starting loader thread"));
        }
        Ok(Self {
            thread: Handle(thread),
            buffer,
            started: Instant::now(),
        })
    }
    fn result(&self) -> Result<Option<u32>, String> {
        unsafe {
            match WaitForSingleObject(self.thread.0, 0) {
                WAIT_TIMEOUT => Ok(None),
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    if GetExitCodeThread(self.thread.0, &mut code) == 0 {
                        Err(error("Loader thread result"))
                    } else {
                        Ok(Some(code))
                    }
                }
                _ => Err(error("Waiting for loader thread")),
            }
        }
    }
    unsafe fn response(&self) -> Result<StartRequest, String> {
        let mut request = StartRequest::default();
        let mut count = 0;
        if ReadProcessMemory(
            self.buffer.process.0,
            self.buffer.address as _,
            (&mut request as *mut StartRequest).cast(),
            size_of::<StartRequest>(),
            &mut count,
        ) == 0
            || count != size_of::<StartRequest>()
        {
            return Err(error("Reading patch status"));
        }
        Ok(request)
    }
}
impl Drop for RemoteCall {
    fn drop(&mut self) {
        // Do not free the argument under an executing remote thread or terminate that thread.
        if unsafe { WaitForSingleObject(self.thread.0, 0) } != WAIT_OBJECT_0 {
            self.buffer.release = false;
        }
    }
}

enum Stage {
    Loading(RemoteCall),
    Starting(RemoteCall),
    Idle(Instant),
    Monitoring(Instant),
    Confirmed,
    Failed,
}
pub struct Session {
    pub pid: u32,
    process: Rc<Handle>,
    dll: PathBuf,
    export: u32,
    status_export: Option<u32>,
    base: usize,
    config: PathBuf,
    stage: Stage,
    pub status: String,
    pub active: bool,
    pub code: u32,
}
impl Session {
    pub fn attach(
        pid: u32,
        expected_executable: &str,
        dll: &Path,
        config: &Path,
    ) -> Result<Self, String> {
        let dll =
            std::fs::canonicalize(dll).map_err(|e| format!("Cannot open t7patch.dll: {e}"))?;
        let image = std::fs::read(&dll).map_err(|e| e.to_string())?;
        let export = export_rva(&image, b"T7PatchStart")?;
        let status_export = data_export_rva(&image, b"T7PatchStatus").ok();
        if !config.is_absolute() || wide(config).len() > 1024 {
            return Err(
                "Configuration path must be absolute and shorter than 1024 UTF-16 units".into(),
            );
        }
        unsafe {
            let handle = OpenProcess(
                PROCESS_CREATE_THREAD
                    | PROCESS_QUERY_INFORMATION
                    | PROCESS_VM_OPERATION
                    | PROCESS_VM_READ
                    | PROCESS_VM_WRITE
                    | PROCESS_SYNCHRONIZE,
                0,
                pid,
            );
            if handle.is_null() {
                return Err(error("Opening BO3"));
            }
            let process = Rc::new(Handle(handle));
            // Recheck through the opened handle so PID reuse cannot select another program.
            let mut image = [0u16; 32768];
            let mut length = image.len() as u32;
            if QueryFullProcessImageNameW(process.0, 0, image.as_mut_ptr(), &mut length) == 0 {
                return Err(error("Checking process identity"));
            }
            let image = PathBuf::from(OsString::from_wide(&image[..length as usize]));
            if !image.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .eq_ignore_ascii_case(expected_executable)
            }) {
                return Err("Process identity changed; waiting for BO3.".into());
            }
            let mut wow = 0;
            if IsWow64Process(process.0, &mut wow) == 0 {
                return Err(error("Checking process architecture"));
            }
            if wow != 0 {
                return Err("BO3 must be a 64-bit process".into());
            }
            let modules = modules(pid)?;
            let existing = modules.iter().find(|m| same_path(&m.path, &dll));
            // Avoid silently combining two different copies of this patch.
            if existing.is_none()
                && modules.iter().any(|m| {
                    m.path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("t7patch.dll"))
                })
            {
                return Err(
                    "A different t7patch.dll is already loaded. Restart BO3 to use this copy."
                        .into(),
                );
            }
            let base = existing.map_or(0, |m| m.base);
            let stage = if base != 0 {
                Stage::Idle(Instant::now())
            } else {
                let path = wide(&dll);
                let bytes = std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), path.len() * 2);
                Stage::Loading(RemoteCall::start(
                    process.clone(),
                    remote_load_library(pid)?,
                    bytes,
                )?)
            };
            Ok(Self {
                pid,
                process,
                dll,
                export,
                status_export,
                base,
                config: config.to_owned(),
                stage,
                status: "Loading patch DLL...".into(),
                active: false,
                code: 0,
            })
        }
    }
    pub fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.process.0, 0) == WAIT_TIMEOUT }
    }
    pub fn poll(&mut self) {
        if let Err(error) = self.advance() {
            self.status = error;
            self.active = false;
            self.code = launcher_api::FAILED;
            self.stage = Stage::Failed;
        }
    }
    fn advance(&mut self) -> Result<(), String> {
        match &self.stage {
            Stage::Loading(call) => {
                if call.result()?.is_none() {
                    if call.started.elapsed() > Duration::from_secs(30) {
                        self.status = "Still waiting for the DLL loader...".into();
                    }
                    return Ok(());
                }
                self.base = modules(self.pid)?
                    .into_iter()
                    .find(|m| same_path(&m.path, &self.dll))
                    .ok_or(
                        "Windows could not load t7patch.dll. Check its x64 runtime dependencies.",
                    )?
                    .base;
                self.stage = Stage::Idle(Instant::now());
            }
            Stage::Idle(next) if Instant::now() >= *next => {
                let mut request = StartRequest::default();
                let path = wide(&self.config);
                request.config_path[..path.len()].copy_from_slice(&path);
                unsafe {
                    let bytes = std::slice::from_raw_parts(
                        (&request as *const StartRequest).cast::<u8>(),
                        size_of::<StartRequest>(),
                    );
                    self.stage = Stage::Starting(RemoteCall::start(
                        self.process.clone(),
                        self.base + self.export as usize,
                        bytes,
                    )?);
                }
                if !self.active {
                    self.status = "Checking game / installing patch...".into();
                }
            }
            Stage::Starting(call) => {
                let Some(code) = call.result()? else {
                    return Ok(());
                };
                let response = unsafe { call.response()? };
                if response.version != launcher_api::API_VERSION || code != response.status {
                    return Err(format!("Patch bootstrap failed (thread status {code:#x})"));
                }
                self.code = code;
                self.active = code == launcher_api::ACTIVE;
                self.status = response.text();
                self.stage = self.after_bootstrap(code);
            }
            Stage::Monitoring(next) if Instant::now() >= *next => {
                let address =
                    self.base + self.status_export.ok_or("Missing passive status export")? as usize;
                let mut code = 0u32;
                let mut count = 0;
                if unsafe {
                    ReadProcessMemory(
                        self.process.0,
                        address as _,
                        (&mut code as *mut u32).cast(),
                        size_of::<u32>(),
                        &mut count,
                    )
                } == 0
                    || count != size_of::<u32>()
                {
                    return Err(error("Reading passive patch status"));
                }
                self.code = code;
                self.active = code == launcher_api::ACTIVE;
                match code {
                    launcher_api::ACTIVE => {
                        self.stage = Stage::Monitoring(Instant::now() + Duration::from_secs(2))
                    }
                    launcher_api::DEACTIVATED => {
                        self.status = "Patch deactivated. Restart BO3 to install again.".into();
                        self.stage = Stage::Failed;
                    }
                    launcher_api::FAILED => return Err("Patch failed. Restart BO3.".into()),
                    _ => return Err(format!("Unexpected passive patch status {code}")),
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn after_bootstrap(&self, code: u32) -> Stage {
        match code {
            launcher_api::WAITING => Stage::Idle(Instant::now() + Duration::from_secs(2)),
            launcher_api::ACTIVE if self.status_export.is_some() => {
                Stage::Monitoring(Instant::now() + Duration::from_secs(2))
            }
            // Old DLLs lack passive status: retain their confirmed state instead
            // of repeatedly creating game threads and running DLL/TLS callbacks.
            launcher_api::ACTIVE => Stage::Confirmed,
            _ => Stage::Failed,
        }
    }
}

/// Parse the on-disk PE export table without loading the DLL in the launcher.
pub fn export_rva(data: &[u8], name: &[u8]) -> Result<u32, String> {
    export_location(data, name, true)
}

fn data_export_rva(data: &[u8], name: &[u8]) -> Result<u32, String> {
    export_location(data, name, false)
}

fn export_location(data: &[u8], name: &[u8], executable: bool) -> Result<u32, String> {
    let bad = || "Invalid x64 patch DLL / export table".to_string();
    let u16_at = |offset: usize| -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            data.get(offset..offset.checked_add(2).ok_or_else(bad)?)
                .ok_or_else(bad)?
                .try_into()
                .unwrap(),
        ))
    };
    let u32_at = |offset: usize| -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            data.get(offset..offset.checked_add(4).ok_or_else(bad)?)
                .ok_or_else(bad)?
                .try_into()
                .unwrap(),
        ))
    };
    if u16_at(0)? != 0x5a4d {
        return Err(bad());
    }
    let nt = u32_at(0x3c)? as usize;
    if u32_at(nt)? != 0x4550 || u16_at(nt + 4)? != 0x8664 || u16_at(nt + 24)? != 0x20b {
        return Err(bad());
    }
    let sections = nt + 24 + u16_at(nt + 20)? as usize;
    let count = u16_at(nt + 6)? as usize;
    if count > 96 || u16_at(nt + 20)? < 120 {
        return Err(bad());
    }
    let rva_to_offset = |rva: u32| -> Result<usize, String> {
        if rva < u32_at(nt + 24 + 60)? {
            return Ok(rva as usize);
        }
        for i in 0..count {
            let section = sections + i * 40;
            let start = u32_at(section + 12)?;
            let raw_size = u32_at(section + 16)?;
            if let Some(delta) = rva.checked_sub(start) {
                if delta < raw_size {
                    return Ok(u32_at(section + 20)? as usize + delta as usize);
                }
            }
        }
        Err(bad())
    };
    let directory_rva = u32_at(nt + 24 + 112)?;
    let directory_size = u32_at(nt + 24 + 116)?;
    if directory_rva == 0 || directory_size < 40 {
        return Err("DLL does not export T7PatchStart; rebuild the DLL and EXE together".into());
    }
    let directory = rva_to_offset(directory_rva)?;
    let names_count = u32_at(directory + 24)? as usize;
    let functions_count = u32_at(directory + 20)? as usize;
    if names_count > 65536 || functions_count > 65536 {
        return Err(bad());
    }
    let functions = rva_to_offset(u32_at(directory + 28)?)?;
    let names = rva_to_offset(u32_at(directory + 32)?)?;
    let ordinals = rva_to_offset(u32_at(directory + 36)?)?;
    for i in 0..names_count {
        let string = rva_to_offset(u32_at(names + i * 4)?)?;
        let Some(candidate) = data.get(string..string + name.len() + 1) else {
            continue;
        };
        if &candidate[..name.len()] != name || candidate[name.len()] != 0 {
            continue;
        }
        let ordinal = u16_at(ordinals + i * 2)? as usize;
        if ordinal >= functions_count {
            return Err(bad());
        }
        let rva = u32_at(functions + ordinal * 4)?;
        if rva >= directory_rva
            && rva < directory_rva.checked_add(directory_size).ok_or_else(bad)?
        {
            return Err("Forwarded bootstrap export is not supported".into());
        }
        for i in 0..count {
            let section = sections + i * 40;
            if let Some(delta) = rva.checked_sub(u32_at(section + 12)?) {
                let size = u32_at(section + 8)?;
                let flags = u32_at(section + 36)?;
                let valid = if executable {
                    delta < size && flags & 0x20000000 != 0
                } else {
                    rva % 4 == 0
                        && delta.checked_add(4).is_some_and(|end| end <= size)
                        && flags & 0x40000000 != 0
                };
                if valid {
                    return Ok(rva);
                }
            }
        }
        return Err(bad());
    }
    Err("T7PatchStart is missing. Rebuild the DLL and EXE together.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_status_is_passive_and_observes_deactivation() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let status = AtomicU32::new(launcher_api::ACTIVE);
        let mut session = Session {
            pid: unsafe { GetCurrentProcessId() },
            process: Rc::new(Handle(unsafe { GetCurrentProcess() })),
            dll: PathBuf::new(),
            export: 0,
            status_export: Some(0),
            base: std::ptr::from_ref(&status) as usize,
            config: PathBuf::new(),
            stage: Stage::Monitoring(Instant::now()),
            status: "Patch active (test build)".into(),
            active: true,
            code: launcher_api::ACTIVE,
        };
        assert!(matches!(
            session.after_bootstrap(launcher_api::WAITING),
            Stage::Idle(_)
        ));
        assert!(matches!(
            session.after_bootstrap(launcher_api::ACTIVE),
            Stage::Monitoring(_)
        ));
        for _ in 0..3 {
            session.stage = Stage::Monitoring(Instant::now());
            session.advance().unwrap();
            assert!(session.active);
            assert!(matches!(session.stage, Stage::Monitoring(_)));
            assert_eq!(session.status, "Patch active (test build)");
        }
        status.store(launcher_api::DEACTIVATED, Ordering::Release);
        session.stage = Stage::Monitoring(Instant::now());
        session.advance().unwrap();
        assert!(!session.active);
        assert_eq!(session.code, launcher_api::DEACTIVATED);
        assert!(matches!(session.stage, Stage::Failed));
        session.status_export = None;
        assert!(matches!(
            session.after_bootstrap(launcher_api::ACTIVE),
            Stage::Confirmed
        ));
    }
    #[test]
    fn rejects_invalid_pe() {
        for bytes in [&[][..], b"MZ", &[0; 128]] {
            assert!(export_rva(bytes, b"T7PatchStart").is_err());
        }
    }

    #[test]
    fn resolves_exports_and_rejects_forwarders_and_bad_ordinals() {
        let mut data = vec![0u8; 2048];
        fn u16_at(data: &mut [u8], offset: usize, value: u16) {
            data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        fn u32_at(data: &mut [u8], offset: usize, value: u32) {
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        u16_at(&mut data, 0, 0x5a4d);
        u32_at(&mut data, 0x3c, 0x80);
        u32_at(&mut data, 0x80, 0x4550);
        u16_at(&mut data, 0x84, 0x8664);
        u16_at(&mut data, 0x86, 1);
        u16_at(&mut data, 0x94, 240);
        u16_at(&mut data, 0x98, 0x20b);
        u32_at(&mut data, 0x98 + 60, 0x200);
        u32_at(&mut data, 0x98 + 112, 0x1000);
        u32_at(&mut data, 0x98 + 116, 0x100);
        let section = 0x98 + 240;
        u32_at(&mut data, section + 8, 0x600);
        u32_at(&mut data, section + 12, 0x1000);
        u32_at(&mut data, section + 16, 0x600);
        u32_at(&mut data, section + 20, 0x200);
        u32_at(&mut data, section + 36, 0x20000000);
        u32_at(&mut data, 0x200 + 20, 1);
        u32_at(&mut data, 0x200 + 24, 1);
        u32_at(&mut data, 0x200 + 28, 0x1048);
        u32_at(&mut data, 0x200 + 32, 0x1040);
        u32_at(&mut data, 0x200 + 36, 0x1050);
        u32_at(&mut data, 0x240, 0x1060);
        u32_at(&mut data, 0x248, 0x1200);
        data[0x260..0x26d].copy_from_slice(b"T7PatchStart\0");
        assert_eq!(export_rva(&data, b"T7PatchStart").unwrap(), 0x1200);
        assert!(export_rva(&data, b"Missing").is_err());
        u32_at(&mut data, 0x248, 0x1080);
        assert!(export_rva(&data, b"T7PatchStart").is_err());
        u32_at(&mut data, 0x248, 0x1200);
        u16_at(&mut data, 0x250, 1);
        assert!(export_rva(&data, b"T7PatchStart").is_err());
        u16_at(&mut data, 0x250, 0);
        u32_at(&mut data, section + 36, 0x40000000);
        assert!(export_rva(&data, b"T7PatchStart").is_err());
        assert_eq!(data_export_rva(&data, b"T7PatchStart").unwrap(), 0x1200);
        u32_at(&mut data, 0x248, 0x1201);
        assert!(data_export_rva(&data, b"T7PatchStart").is_err());
        u32_at(&mut data, 0x248, 0x1600);
        assert!(data_export_rva(&data, b"T7PatchStart").is_err());
    }
}

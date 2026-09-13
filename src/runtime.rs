use crate::{
    arxan::Integrity,
    config,
    exceptions::Handler,
    hooks,
    memory::{self, Patch},
    minhook, protection,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::thread::JoinHandle;
use windows_sys::Win32::System::{LibraryLoader::*, Threading::*};

static STOP: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<State> = Mutex::new(State::new());
static LIFECYCLE: Mutex<()> = Mutex::new(());
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

struct State {
    attempted: bool,
    active: bool,
    integrity: Option<Integrity>,
    handler: Option<Handler>,
    patches: Vec<Patch>,
    targets: Vec<usize>,
    enabled: Vec<usize>,
    worker: Option<JoinHandle<()>>,
    profiler_worker: Option<JoinHandle<()>>,
    priority: u32,
}
impl State {
    const fn new() -> Self {
        Self {
            attempted: false,
            active: false,
            integrity: None,
            handler: None,
            patches: Vec::new(),
            targets: Vec::new(),
            enabled: Vec::new(),
            worker: None,
            profiler_worker: None,
            priority: 0,
        }
    }
    unsafe fn start(&mut self) -> Result<(), String> {
        if crate::game_build::current_build() == crate::game_build::Build::Unknown {
            return Err("Unsupported executable; no game patches applied".into());
        }
        crate::diagnostics::initialize(&config::path());
        let profiler = crate::profiling::Reporter::start(&config::path());
        // A callback/trampoline may still be running after logical Unload. Keep its code mapped.
        let mut module = std::ptr::null_mut();
        if GetModuleHandleExA(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            crate::Unload as *const () as *const u8,
            &mut module,
        ) == 0
        {
            return Err("Could not pin patch DLL".into());
        }
        self.integrity = Some(Integrity::install()?);
        crate::packets::validate_message_reader()?;
        minhook::initialize()?;
        self.handler = Some(Handler::install()?);
        hooks::create(&mut self.targets)?;
        hooks::memory_patches(&mut self.patches)?;
        protection::prepare(&mut self.patches)?;
        protection::configure_game(&mut self.patches)?;
        for patch in &self.patches {
            patch.apply()?;
        }
        for &target in &self.targets {
            minhook::enable(target)?;
            self.enabled.push(target);
        }
        self.priority = GetPriorityClass(GetCurrentProcess());
        SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS);
        config::GAME_READY.store(true, Ordering::Release);
        protection::apply_game_settings();
        let mut watcher = if config::INJECTORLESS.load(Ordering::Acquire) {
            Some(config::Watcher::start())
        } else {
            None
        };
        STOP.store(false, Ordering::Release);
        let worker = std::thread::Builder::new()
            .name("t7patch-config".into())
            .spawn(move || {
                // Keep the original per-process randomization used by the configuration worker.
                let random = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos();
                let slot = crate::game_build::address(0x11250898);
                if memory::readable(slot, 4) {
                    memory::store(slot, random);
                }
                while !STOP.load(Ordering::Acquire) {
                    if let Ok(mut state) = STATE.try_lock() {
                        if let Some(integrity) = &mut state.integrity {
                            let _pending = crate::profiling::INTEGRITY_MAINTAIN.track();
                            integrity.maintain();
                        }
                    }
                    if let Some(watcher) = &mut watcher {
                        let _pending = crate::profiling::CONFIG_POLL.track();
                        watcher.poll();
                    }
                    for _ in 0..10 {
                        if STOP.load(Ordering::Acquire) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        self.worker = Some(worker);
        if let Some(mut profiler) = profiler {
            // Independent of config I/O and the game: a blocked settings worker
            // must not stop the hang reports themselves.
            match std::thread::Builder::new()
                .name("t7patch-diagnostics".into())
                .spawn(move || {
                    while !STOP.load(Ordering::Acquire) {
                        profiler.poll();
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }) {
                Ok(worker) => self.profiler_worker = Some(worker),
                Err(error) => {
                    memory::debug(&format!("Could not start diagnostics worker: {error}"))
                }
            }
        }
        self.active = true;
        crate::T7PatchStatus.store(crate::launcher_api::ACTIVE, Ordering::Release);
        Ok(())
    }
    unsafe fn deactivate(&mut self, rollback: bool) {
        let mut restored = true;
        for &target in self.enabled.iter().rev() {
            if let Err(e) = minhook::disable(target) {
                restored = false;
                memory::debug(&e);
            }
        }
        for patch in self.patches.iter().rev() {
            // Only restore owned, actually-applied writes; preparation itself changes nothing.
            if memory::readable(patch.address, patch.replacement.len())
                && std::slice::from_raw_parts(patch.address as *const u8, patch.replacement.len())
                    == patch.replacement
            {
                if let Err(e) = patch.restore() {
                    restored = false;
                    memory::debug(&e);
                }
            }
        }
        config::GAME_READY.store(false, Ordering::Release);
        config::set_password(b"");
        if restored {
            if let Some(handler) = &self.handler {
                handler.deactivate();
            }
        }
        // Runtime integrity patches stay until process exit, as in the original patch. During a failed
        // install they can be rolled back after other modifications have been restored.
        if rollback && restored {
            if let Some(integrity) = &self.integrity {
                if let Err(e) = integrity.restore() {
                    memory::debug(&e);
                }
            }
        }
        if self.priority != 0 {
            SetPriorityClass(GetCurrentProcess(), self.priority);
        }
        self.active = false;
        crate::T7PatchStatus.store(
            if rollback {
                crate::launcher_api::FAILED
            } else {
                crate::launcher_api::DEACTIVATED
            },
            Ordering::Release,
        );
    }
}

fn active_message() -> &'static str {
    if cfg!(debug_assertions) {
        "Patch active (Debug DLL; rebuild with --release)"
    } else {
        "Patch active"
    }
}

pub unsafe fn install() {
    let (_, message) = install_checked(None);
    memory::debug(&message);
}

pub unsafe fn install_for_launcher(path: std::path::PathBuf) -> (u32, String) {
    install_checked(Some(path))
}

unsafe fn ready() -> bool {
    use crate::game_build::address;
    let pointer = |rva| {
        let slot = address(rva);
        if memory::readable(slot, 8) {
            memory::read::<usize>(slot)
        } else {
            0
        }
    };
    if pointer(0x1686E948) == 0 {
        return false;
    }
    for rva in [0x10B3DC20, 0x10B3DC30, 0x10B3DC40, 0x10B3DC50] {
        let object = pointer(rva);
        if !memory::readable(object, 8) || !memory::readable(memory::read::<usize>(object), 0xe0) {
            return false;
        }
    }
    if !memory::readable(pointer(0x3390190), 24) {
        return false;
    }
    for rva in [0x1686ED20, 0xA0378B8] {
        let object = pointer(rva);
        if object == 0 || !memory::readable(object + 0x18, 4) {
            return false;
        }
    }
    let lobby = pointer(0x9B35878);
    if lobby == 0 || !memory::readable(lobby + 1384, 8) {
        return false;
    }
    let object = memory::read::<usize>(lobby + 1384);
    memory::readable(object, 8) && memory::readable(memory::read::<usize>(object), 50 * 8)
}

unsafe fn install_checked(path: Option<std::path::PathBuf>) -> (u32, String) {
    use crate::launcher_api::*;
    let _lifecycle = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.active {
        if let Some(path) = path {
            config::set_path(path);
        }
        return (ACTIVE, active_message().into());
    }
    if state.attempted {
        let error = LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone();
        return if error.is_empty() {
            (
                DEACTIVATED,
                "Patch deactivated. Restart BO3 to install again.".into(),
            )
        } else {
            (FAILED, error)
        };
    }
    if crate::game_build::current_build() == crate::game_build::Build::Unknown {
        return (UNSUPPORTED, "Unsupported BO3 executable".into());
    }
    if !ready() {
        return (WAITING, "Waiting for game initialization...".into());
    }
    if let Some(path) = path {
        config::set_path(path);
    }
    state.attempted = true;
    match state.start() {
        Ok(()) => {
            memory::debug("Installed");
            (ACTIVE, active_message().into())
        }
        Err(error) => {
            state.deactivate(true);
            memory::debug(&format!("Installation failed: {error}"));
            *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = error.clone();
            (FAILED, error)
        }
    }
}

pub unsafe fn uninstall() {
    let _lifecycle = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    // Joining happens outside the state lock: the worker may be maintaining integrity patches.
    STOP.store(true, Ordering::Release);
    let (worker, profiler_worker) = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        (state.worker.take(), state.profiler_worker.take())
    };
    for worker in [worker, profiler_worker].into_iter().flatten() {
        if worker.join().is_err() {
            memory::debug("Patch worker panicked");
        }
    }
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.active {
        state.deactivate(false);
        memory::debug("Deactivated; DLL remains pinned until process exit");
    }
}

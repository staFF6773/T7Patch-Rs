use super::{process::Handle, Worker};
use crate::settings::Config;
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemServices::{SS_LEFT, SS_LEFTNOWORDWRAP, SS_NOTIFY},
        Threading::CreateMutexW,
    },
    UI::{Controls::*, HiDpi::GetDpiForSystem, Shell::ShellExecuteW, WindowsAndMessaging::*},
};

const CLASS: &str = "T7PatchRustLauncher";
const BLUE: u32 = 0x00ff9900;
const DARK: u32 = 0x001e1e1e;
const NAME: usize = 101;
const PASSWORD: usize = 102;
const FRIENDS: usize = 103;
const LINK: usize = 104;
const CLOSE: usize = 105;
const STATUS: usize = 106;
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct Resources {
    background: HBRUSH,
    border: HBRUSH,
    font: HFONT,
    title_font: HFONT,
}
impl Drop for Resources {
    fn drop(&mut self) {
        unsafe {
            for object in [self.background, self.border, self.font, self.title_font] {
                if !object.is_null() {
                    DeleteObject(object);
                }
            }
        }
    }
}
struct App {
    resources: Resources,
    scale: f64,
    config_path: PathBuf,
    initial: Config,
    worker: Worker,
    error: RefCell<String>,
    last_status: RefCell<String>,
    name: Cell<HWND>,
    password: Cell<HWND>,
    friends: Cell<HWND>,
    status: Cell<HWND>,
    initialized: Cell<bool>,
    dirty: Cell<Option<Instant>>,
    smoke: bool,
    started: Instant,
}
impl App {
    fn px(&self, value: i32) -> i32 {
        (value as f64 * self.scale).round() as i32
    }
    unsafe fn control(
        &self,
        parent: HWND,
        class: &str,
        text: &str,
        id: usize,
        position: [i32; 4],
        style: u32,
    ) -> HWND {
        let [x, y, w, h] = position;
        let window = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            self.px(x),
            self.px(y),
            self.px(w),
            self.px(h),
            parent,
            id as _,
            GetModuleHandleW(null()),
            null(),
        );
        if !window.is_null() {
            SendMessageW(window, WM_SETFONT, self.resources.font as usize, 1);
        }
        window
    }
    unsafe fn create_controls(&self, window: HWND) {
        let title = self.control(
            window,
            "STATIC",
            "T7Patch 3.06 - Rust / Serious <3",
            0,
            [10, 7, 355, 24],
            SS_LEFT,
        );
        SendMessageW(title, WM_SETFONT, self.resources.title_font as usize, 1);
        self.control(
            window,
            "BUTTON",
            "x",
            CLOSE,
            [377, 4, 26, 25],
            BS_OWNERDRAW as u32 | WS_TABSTOP,
        );
        self.control(
            window,
            "STATIC",
            "Change Name:",
            0,
            [10, 47, 120, 22],
            SS_LEFT,
        );
        self.name.set(self.control(
            window,
            "EDIT",
            &String::from_utf8_lossy(&self.initial.name),
            NAME,
            [137, 44, 262, 23],
            ES_AUTOHSCROLL as u32 | WS_TABSTOP,
        ));
        SendMessageW(self.name.get(), EM_SETLIMITTEXT, 15, 0);
        self.control(
            window,
            "STATIC",
            "Network Password:",
            0,
            [10, 79, 124, 22],
            SS_LEFT,
        );
        self.password.set(self.control(
            window,
            "EDIT",
            &String::from_utf8_lossy(&self.initial.password),
            PASSWORD,
            [137, 76, 152, 23],
            ES_AUTOHSCROLL as u32 | WS_TABSTOP,
        ));
        SendMessageW(self.password.get(), EM_SETLIMITTEXT, 1023, 0);
        self.friends.set(self.control(
            window,
            "BUTTON",
            "Friends Only",
            FRIENDS,
            [296, 77, 105, 23],
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        ));
        SetWindowTheme(self.friends.get(), wide("").as_ptr(), wide("").as_ptr());
        SendMessageW(
            self.friends.get(),
            BM_SETCHECK,
            self.initial.friends_only as usize,
            0,
        );
        self.control(
            window,
            "STATIC",
            "Community patch by Serious",
            0,
            [10, 150, 227, 20],
            SS_LEFT,
        );
        self.control(
            window,
            "STATIC",
            "Learn more",
            LINK,
            [245, 150, 85, 20],
            SS_NOTIFY,
        );
        self.status.set(self.control(
            window,
            "STATIC",
            "No game process found.",
            STATUS,
            [10, 181, 389, 25],
            SS_NOTIFY | SS_LEFTNOWORDWRAP,
        ));
        self.initialized.set(true);
        SetTimer(window, 1, 150, None);
        self.refresh();
    }
    unsafe fn text(window: HWND) -> String {
        let length = GetWindowTextLengthW(window).clamp(0, 4096) as usize;
        let mut text = vec![0u16; length + 1];
        let count = GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32).max(0) as usize;
        String::from_utf16_lossy(&text[..count])
    }
    unsafe fn save(&self) {
        self.dirty.set(None);
        if self.smoke {
            return;
        }
        let config = Config {
            name: Self::text(self.name.get()).into_bytes(),
            password: Self::text(self.password.get()).into_bytes(),
            friends_only: SendMessageW(self.friends.get(), BM_GETCHECK, 0, 0) == 1,
        };
        match config.save(&self.config_path) {
            Ok(()) => {
                self.error.borrow_mut().clear();
                self.worker.configured.store(true, Ordering::Release);
            }
            Err(error) => {
                *self.error.borrow_mut() = format!("Settings not saved: {error}");
                self.worker.configured.store(false, Ordering::Release);
            }
        }
        self.refresh();
    }
    unsafe fn refresh(&self) {
        let error = self.error.borrow().clone();
        let text = if error.is_empty() {
            self.worker
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .text
                .clone()
        } else {
            error
        };
        let changed = *self.last_status.borrow() != text;
        if changed {
            *self.last_status.borrow_mut() = text.clone();
            SetWindowTextW(self.status.get(), wide(&text).as_ptr());
            InvalidateRect(self.status.get(), null(), 1);
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let ptr = GetWindowLongPtrW(window, GWLP_USERDATA) as *const App;
    if ptr.is_null() {
        return DefWindowProcW(window, message, wparam, lparam);
    }
    // Shared reference + interior mutability allow synchronous Win32 message reentrancy.
    let app = &*ptr;
    match message {
        WM_CREATE => {
            app.create_controls(window);
            0
        }
        WM_ERASEBKGND => {
            let mut rect = RECT::default();
            GetClientRect(window, &mut rect);
            FillRect(wparam as HDC, &rect, app.resources.background);
            1
        }
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(window, &mut paint);
            let mut rect = RECT::default();
            GetClientRect(window, &mut rect);
            FrameRect(dc, &rect, app.resources.border);
            for (x, y, w, h) in [(136, 43, 264, 25), (136, 75, 154, 25)] {
                let border = RECT {
                    left: app.px(x),
                    top: app.px(y),
                    right: app.px(x + w),
                    bottom: app.px(y + h),
                };
                FrameRect(dc, &border, app.resources.border);
            }
            let line = RECT {
                left: app.px(1),
                top: app.px(174),
                right: rect.right - app.px(1),
                bottom: app.px(175),
            };
            SetDCBrushColor(dc, 0x666666);
            FillRect(dc, &line, GetStockObject(DC_BRUSH) as HBRUSH);
            EndPaint(window, &paint);
            0
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            let dc = wparam as HDC;
            let id = GetDlgCtrlID(lparam as HWND) as usize;
            let active = id == STATUS
                && app.error.borrow().is_empty()
                && app
                    .worker
                    .status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .active;
            SetTextColor(dc, if id == LINK || active { BLUE } else { 0xffffff });
            SetBkColor(dc, DARK);
            app.resources.background as isize
        }
        WM_DRAWITEM if wparam == CLOSE => {
            let item = &*(lparam as *const DRAWITEMSTRUCT);
            FillRect(item.hDC, &item.rcItem, app.resources.background);
            SetTextColor(item.hDC, 0xffffff);
            SetBkMode(item.hDC, TRANSPARENT as i32);
            let mut rect = item.rcItem;
            DrawTextW(
                item.hDC,
                wide("x").as_ptr(),
                1,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            1
        }
        WM_SETCURSOR if GetDlgCtrlID(wparam as HWND) as usize == LINK => {
            SetCursor(LoadCursorW(null_mut(), IDC_HAND));
            1
        }
        WM_NCHITTEST => {
            let mut point = POINT {
                x: lparam as u16 as i16 as i32,
                y: (lparam >> 16) as u16 as i16 as i32,
            };
            ScreenToClient(window, &mut point);
            if point.y < app.px(32) && point.x < app.px(374) {
                HTCAPTION as isize
            } else {
                HTCLIENT as isize
            }
        }
        WM_COMMAND => {
            let id = wparam & 0xffff;
            let notification = (wparam >> 16) as u16;
            if !app.initialized.get() {
                return 0;
            }
            if (id == NAME || id == PASSWORD) && notification == EN_CHANGE as u16 {
                app.dirty.set(Some(Instant::now()));
            }
            if notification == BN_CLICKED as u16 {
                match id {
                    FRIENDS => app.save(),
                    CLOSE => {
                        SendMessageW(window, WM_CLOSE, 0, 0);
                    }
                    LINK => {
                        ShellExecuteW(
                            window,
                            wide("open").as_ptr(),
                            wide("https://github.com/shiversoftdev/t7patch").as_ptr(),
                            null(),
                            null(),
                            SW_SHOWNORMAL,
                        );
                    }
                    STATUS => {
                        let text = app.last_status.borrow().clone();
                        MessageBoxW(
                            window,
                            wide(&text).as_ptr(),
                            wide("Patch status").as_ptr(),
                            MB_OK,
                        );
                    }
                    _ => {}
                }
            }
            0
        }
        WM_TIMER => {
            if app
                .dirty
                .get()
                .is_some_and(|last| last.elapsed() >= Duration::from_millis(400))
            {
                app.save();
            }
            app.refresh();
            if app.smoke && app.started.elapsed() >= Duration::from_secs(2) {
                PostMessageW(window, WM_CLOSE, 0, 0);
            }
            0
        }
        WM_CLOSE => {
            if app.dirty.get().is_some() {
                app.save();
            }
            DestroyWindow(window);
            0
        }
        WM_DESTROY => {
            KillTimer(window, 1);
            PostQuitMessage(0);
            0
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            DefWindowProcW(window, message, wparam, lparam)
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

pub fn run(smoke: bool) -> Result<(), String> {
    unsafe {
        let _singleton = if smoke {
            None
        } else {
            let mutex = CreateMutexW(null(), 0, wide("Local\\T7PatchRustLauncher").as_ptr());
            if mutex.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
            let mutex = Handle(mutex);
            if already_running {
                let existing = FindWindowW(wide(CLASS).as_ptr(), null());
                if !existing.is_null() {
                    ShowWindow(existing, SW_RESTORE);
                    SetForegroundWindow(existing);
                }
                return Ok(());
            }
            Some(mutex)
        };
        SetProcessDPIAware();
        let scale = GetDpiForSystem().max(96) as f64 / 96.0;
        let directory = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("Missing executable directory")?
            .to_owned();
        let path = directory.join("t7patch.conf");
        let (config, error) = if smoke {
            (Config::default(), String::new())
        } else {
            match Config::load(&path) {
                Ok(config) => (config, String::new()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let config = Config::default();
                    let error = config
                        .save(&path)
                        .err()
                        .map_or(String::new(), |e| e.to_string());
                    (config, error)
                }
                Err(error) => (Config::default(), format!("Cannot read settings: {error}")),
            }
        };
        let font = |size: f64| {
            CreateFontW(
                -(size * scale).round() as i32,
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                DEFAULT_PITCH as u32,
                wide("Segoe UI").as_ptr(),
            )
        };
        let resources = Resources {
            background: CreateSolidBrush(DARK),
            border: CreateSolidBrush(BLUE),
            font: font(13.0),
            title_font: font(16.0),
        };
        if resources.background.is_null()
            || resources.border.is_null()
            || resources.font.is_null()
            || resources.title_font.is_null()
        {
            return Err("Cannot create window drawing resources".into());
        }
        let worker = Worker::start(
            directory.join("t7patch.dll"),
            path.clone(),
            error.is_empty(),
            !smoke,
        )
        .map_err(|e| e.to_string())?;
        let app = Box::new(App {
            resources,
            scale,
            config_path: path,
            initial: config,
            worker,
            error: RefCell::new(error),
            last_status: RefCell::new(String::new()),
            name: Cell::new(null_mut()),
            password: Cell::new(null_mut()),
            friends: Cell::new(null_mut()),
            status: Cell::new(null_mut()),
            initialized: Cell::new(false),
            dirty: Cell::new(None),
            smoke,
            started: Instant::now(),
        });
        let instance = GetModuleHandleW(null());
        let class_name = wide(CLASS);
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            hbrBackground: app.resources.background,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let width = app.px(410);
        let height = app.px(214);
        let window = CreateWindowExW(
            WS_EX_APPWINDOW | WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            wide("T7Patch Rust").as_ptr(),
            WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
            width,
            height,
            null_mut(),
            null_mut(),
            instance,
            (&*app as *const App).cast(),
        );
        if window.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if [
            app.name.get(),
            app.password.get(),
            app.friends.get(),
            app.status.get(),
        ]
        .iter()
        .any(|w| w.is_null())
        {
            DestroyWindow(window);
            return Err("Cannot create launcher controls".into());
        }
        ShowWindow(window, SW_SHOW);
        UpdateWindow(window);
        let mut message: MSG = std::mem::zeroed();
        loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);
            if result == 0 {
                break;
            }
            if result == -1 {
                DestroyWindow(window);
                return Err(std::io::Error::last_os_error().to_string());
            }
            if IsDialogMessageW(window, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        UnregisterClassW(class_name.as_ptr(), instance);
        if smoke {
            println!("UI smoke test: window, controls, paint/message loop and close OK (game detection disabled)");
        }
        Ok(())
    }
}

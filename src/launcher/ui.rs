use super::updater::{self, State as UpdateState, Updater};
use super::{process::Handle, Worker};
use crate::settings::Config;
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, GetKeyState, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_CONTROL, VK_SHIFT, VK_TAB,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemServices::{SS_ENDELLIPSIS, SS_LEFT, SS_NOPREFIX, SS_NOTIFY},
        Threading::CreateMutexW,
    },
    UI::{
        Controls::*,
        HiDpi::GetDpiForSystem,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, ShellExecuteW},
        WindowsAndMessaging::*,
    },
};

const CLASS: &str = "T7PatchRustLauncher";
// CreateIconFromResourceEx accepts PNG icon resources on supported Windows versions
// and requires DWORD-aligned resource data. Embed it so no external file is needed.
#[repr(align(4))]
struct IconImage([u8; include_bytes!("../../assets/t7patch-icon.png").len()]);
static ICON_IMAGE: IconImage = IconImage(*include_bytes!("../../assets/t7patch-icon.png"));
// COLORREF uses 0x00BBGGRR, unlike the RGB notation used by design tools.
const ACCENT: u32 = 0x003c72ce;
const DARK: u32 = 0x00191715;
const PANEL: u32 = 0x00262320;
const INPUT: u32 = 0x001e1c19;
const BORDER: u32 = 0x003d3833;
const TEXT: u32 = 0x00edeef0;
const MUTED: u32 = 0x00aaa6a1;
const GREEN: u32 = 0x0093c58c;
const ERROR: u32 = 0x008386ef;
const WIDTH: i32 = 640;
const HEIGHT: i32 = 560;
const NAME: usize = 101;
const PASSWORD: usize = 102;
const FRIENDS: usize = 103;
const LINK: usize = 104;
const CLOSE: usize = 105;
const STATUS: usize = 106;
const UPDATE_CHECK: usize = 107;
const UPDATE_INSTALL: usize = 108;
const UPDATE_STATUS: usize = 109;
const MINIMIZE: usize = 110;
const SAVED: usize = 111;
const STATUS_DETAILS: usize = 112;
const UPDATE_DETAILS: usize = 113;
const TABS: usize = 114;
const SOURCE_LINK: usize = 115;
const RELATED_LINK: usize = 116;
const RUST_LINK: usize = 117;
const MINHOOK_LINK: usize = 118;
const LABEL: usize = 120;
const HINT: usize = 121;
const HEADING: usize = 122;
const CONTENT: [i32; 4] = [24, 264, 592, 272];
const PROGRESS: [i32; 4] = [44, 412, 552, 3];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Settings,
    Updates,
    Credits,
}
impl Page {
    const ALL: [Self; 3] = [Self::Settings, Self::Updates, Self::Credits];

    fn title(self) -> &'static str {
        match self {
            Self::Settings => "Settings",
            Self::Updates => "Updates",
            Self::Credits => "Credits",
        }
    }
}

fn credit_url(id: usize) -> Option<&'static str> {
    match id {
        LINK => Some("https://github.com/shiversoftdev/t7patch"),
        SOURCE_LINK => Some("https://github.com/Scroptss/T7Patch-src"),
        RELATED_LINK => Some("https://github.com/Scroptss/T7Patch"),
        RUST_LINK => Some("https://github.com/staFF6773/T7Patch-Rs"),
        MINHOOK_LINK => Some("https://github.com/TsudaKageyu/minhook"),
        _ => None,
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct Resources {
    background: HBRUSH,
    panel: HBRUSH,
    input: HBRUSH,
    border: HBRUSH,
    font: HFONT,
    title_font: HFONT,
    small_font: HFONT,
    mono_font: HFONT,
    icon: HICON,
    small_icon: HICON,
}
impl Drop for Resources {
    fn drop(&mut self) {
        unsafe {
            for object in [
                self.background,
                self.panel,
                self.input,
                self.border,
                self.font,
                self.title_font,
                self.small_font,
                self.mono_font,
            ] {
                if !object.is_null() {
                    DeleteObject(object);
                }
            }
            for icon in [self.icon, self.small_icon] {
                if !icon.is_null() {
                    DestroyIcon(icon);
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
    updater: RefCell<Updater>,
    update_status: Cell<HWND>,
    update_check: Cell<HWND>,
    update_install: Cell<HWND>,
    last_update_status: RefCell<String>,
    restarting: Cell<bool>,
    error: RefCell<String>,
    last_status: RefCell<String>,
    name: Cell<HWND>,
    password: Cell<HWND>,
    friends: Cell<HWND>,
    status: Cell<HWND>,
    saved: Cell<HWND>,
    status_active: Cell<bool>,
    hovered: Cell<HWND>,
    tabs: Cell<HWND>,
    page: Cell<Page>,
    creating_page: Cell<Option<Page>>,
    page_controls: RefCell<Vec<(Page, HWND)>>,
    controls_failed: Cell<bool>,
    initialized: Cell<bool>,
    dirty: Cell<Option<Instant>>,
    smoke: bool,
    started: Instant,
}
impl App {
    fn px(&self, value: i32) -> i32 {
        (value as f64 * self.scale).round() as i32
    }
    fn rect(&self, [x, y, w, h]: [i32; 4]) -> RECT {
        RECT {
            left: self.px(x),
            top: self.px(y),
            right: self.px(x + w),
            bottom: self.px(y + h),
        }
    }
    unsafe fn fill(&self, dc: HDC, rect: &RECT, color: u32) {
        SetDCBrushColor(dc, color);
        FillRect(dc, rect, GetStockObject(DC_BRUSH) as HBRUSH);
    }
    unsafe fn draw_text(&self, dc: HDC, text: &str, mut rect: RECT, font: HFONT, color: u32) {
        let previous = SelectObject(dc, font);
        SetTextColor(dc, color);
        SetBkMode(dc, TRANSPARENT as i32);
        DrawTextW(
            dc,
            wide(text).as_ptr(),
            -1,
            &mut rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, previous);
    }
    unsafe fn surface(&self, dc: HDC, rect: RECT, fill: u32, border: u32) {
        let brush = SelectObject(dc, GetStockObject(DC_BRUSH));
        let pen = SelectObject(dc, GetStockObject(DC_PEN));
        SetDCBrushColor(dc, fill);
        SetDCPenColor(dc, border);
        RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            self.px(10),
            self.px(10),
        );
        SelectObject(dc, pen);
        SelectObject(dc, brush);
    }
    unsafe fn label(&self, window: HWND, text: &str, id: usize, position: [i32; 4]) -> HWND {
        let label = self.control(window, "STATIC", text, id, position, SS_LEFT);
        let font = match id {
            HEADING => self.resources.mono_font,
            HINT | SAVED => self.resources.small_font,
            _ => self.resources.font,
        };
        SendMessageW(label, WM_SETFONT, font as usize, 0);
        label
    }
    unsafe fn button(&self, window: HWND, text: &str, id: usize, position: [i32; 4]) -> HWND {
        let button = self.control(
            window,
            "BUTTON",
            text,
            id,
            position,
            BS_OWNERDRAW as u32 | WS_TABSTOP,
        );
        if !button.is_null() {
            SetWindowSubclass(button, Some(button_proc), 1, self as *const Self as usize);
        }
        button
    }
    unsafe fn set_text(window: HWND, text: &str) {
        if Self::text(window) != text {
            SetWindowTextW(window, wide(text).as_ptr());
            InvalidateRect(window, null(), 1);
        }
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
        let style = if class == "STATIC" {
            style | SS_NOPREFIX
        } else {
            style
        };
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
            if let Some(page) = self.creating_page.get() {
                self.page_controls.borrow_mut().push((page, window));
            }
        } else {
            self.controls_failed.set(true);
        }
        window
    }
    unsafe fn create_tabs(&self, window: HWND) {
        let tabs = self.control(
            window,
            "SysTabControl32",
            "Launcher sections",
            TABS,
            [24, 212, 592, 44],
            TCS_FIXEDWIDTH | TCS_FOCUSONBUTTONDOWN | WS_TABSTOP,
        );
        self.tabs.set(tabs);
        if tabs.is_null() {
            return;
        }
        if SetWindowSubclass(tabs, Some(tabs_proc), 1, self as *const Self as usize) == 0 {
            self.controls_failed.set(true);
        }
        for page in Page::ALL {
            let mut text = wide(page.title());
            let item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: text.as_mut_ptr(),
                ..std::mem::zeroed()
            };
            if SendMessageW(
                tabs,
                TCM_INSERTITEMW,
                page as usize,
                &item as *const TCITEMW as isize,
            ) == -1
            {
                self.controls_failed.set(true);
            }
        }
        SendMessageW(
            tabs,
            TCM_SETITEMSIZE,
            0,
            (self.px(194) | (self.px(40) << 16)) as isize,
        );
    }
    unsafe fn show_page(&self, window: HWND, page: Page) {
        self.page.set(page);
        SendMessageW(self.tabs.get(), TCM_SETCURSEL, page as usize, 0);
        // Retain the actual controls, including pending edits, when switching pages.
        let controls = self.page_controls.borrow().clone();
        let focus = GetFocus();
        if controls
            .iter()
            .any(|(owner, control)| *owner != page && *control == focus)
        {
            SetFocus(self.tabs.get());
        }
        for (owner, control) in controls {
            ShowWindow(control, if owner == page { SW_SHOWNA } else { SW_HIDE });
        }
        InvalidateRect(self.tabs.get(), null(), 0);
        InvalidateRect(window, &self.rect(CONTENT), 0);
    }
    unsafe fn create_controls(&self, window: HWND) {
        self.label(window, "GAME CONNECTION", HEADING, [44, 112, 350, 20]);
        self.status.set(self.control(
            window,
            "STATIC",
            "No game process found.",
            STATUS,
            [44, 140, 552, 24],
            SS_NOTIFY | SS_ENDELLIPSIS,
        ));
        self.label(
            window,
            "Start Black Ops III. The patch connects automatically.",
            HINT,
            [44, 173, 552, 18],
        );
        self.button(window, "Details", STATUS_DETAILS, [520, 105, 80, 28]);
        self.create_tabs(window);

        self.creating_page.set(Some(Page::Settings));
        self.label(window, "PLAYER SETTINGS", HEADING, [44, 284, 320, 20]);
        self.saved
            .set(self.label(window, "", SAVED, [420, 285, 176, 18]));
        self.label(
            window,
            "Your identity and connection preferences.",
            HINT,
            [44, 314, 552, 20],
        );
        self.label(window, "Player name", LABEL, [44, 348, 252, 21]);
        self.name.set(self.control(
            window,
            "EDIT",
            &String::from_utf8_lossy(&self.initial.name),
            NAME,
            [56, 381, 228, 24],
            ES_AUTOHSCROLL as u32 | WS_TABSTOP,
        ));
        SendMessageW(self.name.get(), EM_SETLIMITTEXT, 15, 0);
        self.label(window, "Network password", LABEL, [320, 348, 276, 21]);
        self.password.set(self.control(
            window,
            "EDIT",
            &String::from_utf8_lossy(&self.initial.password),
            PASSWORD,
            [332, 381, 252, 24],
            ES_AUTOHSCROLL as u32 | WS_TABSTOP,
        ));
        SendMessageW(self.password.get(), EM_SETLIMITTEXT, 1023, 0);
        self.label(window, "Up to 15 characters.", HINT, [44, 423, 252, 18]);
        self.label(
            window,
            "Use the same password as your party.",
            HINT,
            [320, 423, 276, 18],
        );
        self.friends.set(self.control(
            window,
            "BUTTON",
            "Friends only",
            FRIENDS,
            [44, 474, 132, 24],
            BS_AUTOCHECKBOX as u32 | BS_FLAT as u32 | WS_TABSTOP,
        ));
        SetWindowTheme(self.friends.get(), wide("").as_ptr(), wide("").as_ptr());
        SendMessageW(
            self.friends.get(),
            BM_SETCHECK,
            self.initial.friends_only as usize,
            0,
        );
        self.label(
            window,
            "Limit incoming connections to your friends.",
            HINT,
            [196, 477, 400, 20],
        );
        self.creating_page.set(Some(Page::Updates));
        self.label(window, "LAUNCHER UPDATES", HEADING, [44, 284, 350, 20]);
        self.label(
            window,
            concat!(
                "Installed v",
                env!("CARGO_PKG_VERSION"),
                " · stable releases from GitHub"
            ),
            HINT,
            [44, 314, 552, 20],
        );
        self.update_status.set(self.control(
            window,
            "STATIC",
            "",
            UPDATE_STATUS,
            [44, 354, 552, 46],
            SS_LEFT | SS_NOTIFY | SS_ENDELLIPSIS,
        ));
        self.update_check.set(self.button(
            window,
            "Check for updates",
            UPDATE_CHECK,
            [44, 440, 200, 36],
        ));
        self.update_install.set(self.button(
            window,
            "Download update",
            UPDATE_INSTALL,
            [396, 440, 200, 36],
        ));
        self.button(window, "Details", UPDATE_DETAILS, [520, 277, 80, 28]);
        self.label(
            window,
            "Game detection continues while you browse other tabs.",
            HINT,
            [44, 500, 552, 18],
        );

        self.creating_page.set(Some(Page::Credits));
        self.label(
            window,
            "CREDITS & ACKNOWLEDGMENTS",
            HEADING,
            [44, 284, 552, 20],
        );
        self.label(window, "Serious / shiversoftdev", LABEL, [44, 322, 370, 21]);
        self.label(
            window,
            "Creator of the original T7 community patch.",
            HINT,
            [44, 344, 370, 18],
        );
        self.button(window, "Original project  ↗", LINK, [440, 326, 156, 30]);
        self.label(window, "Scroptss", LABEL, [44, 368, 370, 21]);
        self.label(
            window,
            "Source reference and related T7 Patch repository.",
            HINT,
            [44, 390, 370, 18],
        );
        self.button(window, "Source ↗", SOURCE_LINK, [424, 372, 82, 30]);
        self.button(window, "Related ↗", RELATED_LINK, [514, 372, 82, 30]);
        self.label(window, "T7Patch-Rs", LABEL, [44, 414, 370, 21]);
        self.label(
            window,
            "Launcher and patch reimplementation in Rust.",
            HINT,
            [44, 436, 370, 18],
        );
        self.button(window, "Rust project  ↗", RUST_LINK, [440, 418, 156, 30]);
        self.label(window, "MinHook · Tsuda Kageyu", LABEL, [44, 460, 370, 21]);
        self.label(
            window,
            "Native function hooks used by the patch.",
            HINT,
            [44, 482, 370, 18],
        );
        self.button(window, "MinHook  ↗", MINHOOK_LINK, [440, 464, 156, 30]);
        self.label(
            window,
            "All dependencies retain their respective authorship and licenses.",
            HINT,
            [44, 512, 552, 18],
        );

        self.creating_page.set(None);
        self.button(window, "Minimize", MINIMIZE, [536, 20, 36, 32]);
        self.button(window, "Close", CLOSE, [580, 20, 36, 32]);
        if !self.smoke {
            self.updater.borrow_mut().check();
        }
        self.initialized.set(true);
        SetTimer(window, 1, 150, None);
        self.refresh();
        self.refresh_update(window);
        self.show_page(window, Page::Settings);
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
            self.refresh();
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
        Self::set_text(
            self.saved.get(),
            if self.smoke {
                "Preview · not saved"
            } else if !error.is_empty() {
                "Could not save settings"
            } else if self.dirty.get().is_some() {
                "Saving changes…"
            } else {
                "All changes saved"
            },
        );
        let active = error.is_empty()
            && self
                .worker
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active;
        let active_changed = self.status_active.replace(active) != active;
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
        if changed || active_changed {
            *self.last_status.borrow_mut() = text.clone();
            SetWindowTextW(self.status.get(), wide(&text).as_ptr());
            InvalidateRect(self.status.get(), null(), 1);
            InvalidateRect(
                GetParent(self.status.get()),
                &self.rect([24, 96, 4, 104]),
                0,
            );
        }
    }

    unsafe fn update_action(&self, window: HWND) {
        if self.smoke {
            return;
        }
        let state = self.updater.borrow().state();
        match state {
            UpdateState::Available(_) => self.updater.borrow_mut().download(),
            UpdateState::Ready(update) => {
                let game = super::process::find_game();
                let error = match game {
                    Ok(None) => None,
                    Ok(Some(_)) => Some("Close BO3 before installing the update.".to_owned()),
                    Err(error) => Some(error),
                };
                if let Some(error) = error {
                    MessageBoxW(
                        window,
                        wide(&error).as_ptr(),
                        wide("T7 Patch update").as_ptr(),
                        MB_OK,
                    );
                    return;
                }
                if self.dirty.get().is_some() {
                    self.save();
                }
                if !self.error.borrow().is_empty() {
                    MessageBoxW(
                        window,
                        wide("Save valid settings before restarting to update.").as_ptr(),
                        wide("T7 Patch update").as_ptr(),
                        MB_OK,
                    );
                    return;
                }
                self.worker.pause();
                self.updater.borrow().set(UpdateState::Handoff(update));
            }
            _ => {}
        }
        self.refresh_update(window);
    }

    unsafe fn refresh_update(&self, window: HWND) {
        let state = self.updater.borrow().state();
        let text = state.text();
        let changed = *self.last_update_status.borrow() != text;
        if changed {
            *self.last_update_status.borrow_mut() = text.clone();
            SetWindowTextW(self.update_status.get(), wide(&text).as_ptr());
            InvalidateRect(self.update_status.get(), null(), 1);
        }
        if self.page.get() == Page::Updates && (changed || state.busy()) {
            InvalidateRect(window, &self.rect(PROGRESS), 0);
        }
        EnableWindow(
            self.update_check.get(),
            (!self.smoke && !state.busy() && !matches!(state, UpdateState::Ready(_))) as i32,
        );
        EnableWindow(
            self.update_install.get(),
            (!self.smoke && matches!(state, UpdateState::Available(_) | UpdateState::Ready(_)))
                as i32,
        );
        Self::set_text(
            self.update_install.get(),
            if matches!(state, UpdateState::Ready(_) | UpdateState::Handoff(_)) {
                "Install & restart"
            } else {
                "Download update"
            },
        );
        for control in [self.name.get(), self.password.get(), self.friends.get()] {
            EnableWindow(control, (!matches!(state, UpdateState::Handoff(_))) as i32);
        }
        if let UpdateState::Handoff(update) = state {
            if self.worker.is_quiescent() && !self.restarting.get() {
                let directory = self.config_path.parent().expect("configuration directory");
                match updater::install::start_helper(directory, &update.stage, false) {
                    Ok(()) => {
                        self.restarting.set(true);
                        PostMessageW(window, WM_CLOSE, 0, 0);
                    }
                    Err(error) => {
                        self.worker.resume();
                        self.updater.borrow().set(UpdateState::Ready(update));
                        MessageBoxW(
                            window,
                            wide(&error).as_ptr(),
                            wide("T7 Patch update").as_ptr(),
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
            }
        }
    }
}

// Use a real tab control for selection, keyboard navigation and accessibility.
// Only its painting is replaced so the entire tab strip follows the dark theme.
unsafe extern "system" fn tabs_proc(
    window: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    subclass: usize,
    data: usize,
) -> isize {
    let app = &*(data as *const App);
    match message {
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(window, &mut paint);
            let mut bounds = RECT::default();
            GetClientRect(window, &mut bounds);
            FillRect(dc, &bounds, app.resources.background);
            let font = SelectObject(dc, app.resources.font);
            SetBkMode(dc, TRANSPARENT as i32);
            for page in Page::ALL {
                let mut rect = RECT::default();
                if SendMessageW(
                    window,
                    TCM_GETITEMRECT,
                    page as usize,
                    &mut rect as *mut RECT as isize,
                ) == 0
                {
                    continue;
                }
                let selected = app.page.get() == page;
                if selected {
                    FillRect(dc, &rect, app.resources.panel);
                }
                SetTextColor(dc, if selected { ACCENT } else { MUTED });
                DrawTextW(
                    dc,
                    wide(page.title()).as_ptr(),
                    -1,
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
                let underline = RECT {
                    top: rect.bottom - app.px(2),
                    ..rect
                };
                app.fill(dc, &underline, if selected { ACCENT } else { BORDER });
                if selected && GetFocus() == window {
                    InflateRect(&mut rect, -app.px(5), -app.px(5));
                    DrawFocusRect(dc, &rect);
                }
            }
            SelectObject(dc, font);
            EndPaint(window, &paint);
            return 0;
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            InvalidateRect(window, null(), 0);
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(window, Some(tabs_proc), subclass);
        }
        _ => {}
    }
    DefSubclassProc(window, message, wparam, lparam)
}

// Keep the native button's keyboard/accessibility behavior while adding hover feedback.
unsafe extern "system" fn button_proc(
    window: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    subclass: usize,
    data: usize,
) -> isize {
    let app = &*(data as *const App);
    match message {
        WM_MOUSEMOVE if app.hovered.get() != window => {
            app.hovered.set(window);
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: window,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tracking);
            InvalidateRect(window, null(), 0);
        }
        WM_MOUSELEAVE | WM_NCDESTROY => {
            if app.hovered.get() == window {
                app.hovered.set(null_mut());
            }
            InvalidateRect(window, null(), 0);
            if message == WM_NCDESTROY {
                RemoveWindowSubclass(window, Some(button_proc), subclass);
            }
        }
        WM_ENABLE | WM_SETTEXT => {
            InvalidateRect(window, null(), 0);
        }
        _ => {}
    }
    DefSubclassProc(window, message, wparam, lparam)
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
            FillRect(dc, &rect, app.resources.background);
            FrameRect(dc, &rect, app.resources.border);
            app.fill(dc, &app.rect([24, 0, 64, 3]), ACCENT);
            app.surface(dc, app.rect([24, 24, 48, 48]), ACCENT, ACCENT);
            app.draw_text(
                dc,
                "rs",
                app.rect([33, 26, 36, 42]),
                app.resources.title_font,
                DARK,
            );
            app.draw_text(
                dc,
                "T7 / RUST",
                app.rect([88, 20, 240, 36]),
                app.resources.title_font,
                TEXT,
            );
            app.draw_text(
                dc,
                "BLACK OPS III · COMMUNITY PATCH",
                app.rect([90, 59, 410, 18]),
                app.resources.mono_font,
                MUTED,
            );
            app.draw_text(
                dc,
                concat!("v", env!("CARGO_PKG_VERSION")),
                app.rect([354, 28, 150, 20]),
                app.resources.mono_font,
                ACCENT,
            );
            for position in [[24, 96, 592, 104], CONTENT] {
                app.surface(dc, app.rect(position), PANEL, BORDER);
            }
            let status_color = if !app.error.borrow().is_empty() {
                ERROR
            } else if app.status_active.get() {
                GREEN
            } else {
                ACCENT
            };
            app.fill(dc, &app.rect([24, 108, 3, 80]), status_color);
            if app.page.get() == Page::Settings {
                for (id, position) in [(NAME, [44, 374, 252, 40]), (PASSWORD, [320, 374, 276, 40])]
                {
                    let focused = GetFocus() == GetDlgItem(window, id as i32);
                    app.surface(
                        dc,
                        app.rect(position),
                        INPUT,
                        if focused { ACCENT } else { BORDER },
                    );
                }
            }
            if app.page.get() == Page::Updates && app.updater.borrow().state().busy() {
                // An activity indicator; actual download byte counts remain in the status text.
                app.fill(dc, &app.rect(PROGRESS), BORDER);
                let phase = (app.started.elapsed().as_millis() / 12 % 904) as i32;
                let x = if phase <= 452 { phase } else { 904 - phase };
                app.fill(dc, &app.rect([44 + x, 412, 100, 3]), ACCENT);
            }
            EndPaint(window, &paint);
            0
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            let dc = wparam as HDC;
            let id = GetDlgCtrlID(lparam as HWND) as usize;
            let color = match id {
                STATUS if !app.error.borrow().is_empty() => ERROR,
                STATUS if app.status_active.get() => GREEN,
                SAVED if !app.error.borrow().is_empty() => ERROR,
                UPDATE_STATUS if matches!(app.updater.borrow().state(), UpdateState::Error(_)) => {
                    ERROR
                }
                HEADING => ACCENT,
                HINT | SAVED => MUTED,
                _ => TEXT,
            };
            let (background, brush) = match id {
                NAME | PASSWORD => (INPUT, app.resources.input),
                _ => (PANEL, app.resources.panel),
            };
            SetTextColor(dc, color);
            SetBkColor(dc, background);
            brush as isize
        }
        WM_DRAWITEM => {
            let item = &*(lparam as *const DRAWITEMSTRUCT);
            let id = item.CtlID as usize;
            let disabled = item.itemState & ODS_DISABLED != 0;
            let pressed = item.itemState & ODS_SELECTED != 0;
            let focused = item.itemState & ODS_FOCUS != 0;
            let hovered = app.hovered.get() == item.hwndItem && !disabled;
            let quiet = credit_url(id).is_some()
                || matches!(id, CLOSE | MINIMIZE | STATUS_DETAILS | UPDATE_DETAILS);
            let primary = id == UPDATE_INSTALL && !disabled;
            let base = if matches!(id, CLOSE | MINIMIZE) {
                DARK
            } else {
                PANEL
            };
            let fill = if pressed {
                BORDER
            } else if hovered && primary {
                0x00588ee5
            } else if hovered {
                BORDER
            } else if primary {
                ACCENT
            } else {
                base
            };
            FillRect(
                item.hDC,
                &item.rcItem,
                if base == DARK {
                    app.resources.background
                } else {
                    app.resources.panel
                },
            );
            app.surface(
                item.hDC,
                item.rcItem,
                fill,
                if focused {
                    ACCENT
                } else if quiet {
                    fill
                } else {
                    BORDER
                },
            );
            let previous = SelectObject(item.hDC, app.resources.font);
            SetTextColor(
                item.hDC,
                if disabled {
                    MUTED
                } else if primary && !pressed {
                    DARK
                } else if quiet {
                    ACCENT
                } else {
                    TEXT
                },
            );
            SetBkMode(item.hDC, TRANSPARENT as i32);
            let mut rect = item.rcItem;
            let text = match id {
                CLOSE => "×".to_owned(),
                MINIMIZE => "−".to_owned(),
                _ => App::text(item.hwndItem),
            };
            if pressed {
                OffsetRect(&mut rect, 0, app.px(1));
            }
            DrawTextW(
                item.hDC,
                wide(&text).as_ptr(),
                -1,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            if focused && item.itemState & ODS_NOFOCUSRECT == 0 {
                InflateRect(&mut rect, -app.px(4), -app.px(4));
                DrawFocusRect(item.hDC, &rect);
            }
            SelectObject(item.hDC, previous);
            1
        }
        WM_SETCURSOR if credit_url(GetDlgCtrlID(wparam as HWND) as usize).is_some() => {
            SetCursor(LoadCursorW(null_mut(), IDC_HAND));
            1
        }
        WM_NCHITTEST => {
            let mut point = POINT {
                x: lparam as u16 as i16 as i32,
                y: (lparam >> 16) as u16 as i16 as i32,
            };
            ScreenToClient(window, &mut point);
            if point.y < app.px(88) && point.x < app.px(528) {
                HTCAPTION as isize
            } else {
                HTCLIENT as isize
            }
        }
        WM_NOTIFY => {
            let notification = &*(lparam as *const NMHDR);
            if notification.hwndFrom == app.tabs.get() && notification.code == TCN_SELCHANGE {
                let index = SendMessageW(app.tabs.get(), TCM_GETCURSEL, 0, 0) as usize;
                if let Some(page) = Page::ALL.get(index) {
                    app.show_page(window, *page);
                }
            }
            0
        }
        WM_COMMAND => {
            let id = wparam & 0xffff;
            let notification = (wparam >> 16) as u16;
            if !app.initialized.get() {
                return 0;
            }
            if (id == NAME || id == PASSWORD) && notification == EN_CHANGE as u16 {
                app.dirty.set(Some(Instant::now()));
                app.refresh();
            }
            if (id == NAME || id == PASSWORD)
                && (notification == EN_SETFOCUS as u16 || notification == EN_KILLFOCUS as u16)
                && app.page.get() == Page::Settings
            {
                InvalidateRect(window, &app.rect([44, 374, 552, 40]), 0);
            }
            if notification == BN_CLICKED as u16 {
                match id {
                    UPDATE_CHECK => {
                        app.updater.borrow_mut().check();
                        app.refresh_update(window);
                    }
                    UPDATE_INSTALL => app.update_action(window),
                    UPDATE_STATUS | UPDATE_DETAILS => {
                        let text = app.updater.borrow().state().text();
                        MessageBoxW(
                            window,
                            wide(&text).as_ptr(),
                            wide("T7 Patch update").as_ptr(),
                            MB_OK,
                        );
                    }
                    FRIENDS => app.save(),
                    MINIMIZE => {
                        ShowWindow(window, SW_MINIMIZE);
                    }
                    CLOSE => {
                        SendMessageW(window, WM_CLOSE, 0, 0);
                    }
                    id if credit_url(id).is_some() => {
                        ShellExecuteW(
                            window,
                            wide("open").as_ptr(),
                            wide(credit_url(id).unwrap()).as_ptr(),
                            null(),
                            null(),
                            SW_SHOWNORMAL,
                        );
                    }
                    STATUS | STATUS_DETAILS => {
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
            app.refresh_update(window);
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
        let controls = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TAB_CLASSES,
        };
        if InitCommonControlsEx(&controls) == 0 {
            return Err("Cannot initialize launcher tabs".into());
        }
        let mut work_area = RECT::default();
        if SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work_area as *mut RECT).cast(), 0) == 0 {
            work_area.right = GetSystemMetrics(SM_CXSCREEN);
            work_area.bottom = GetSystemMetrics(SM_CYSCREEN);
        }
        // Preserve the whole layout on small displays or at a large system text scale.
        let scale = (GetDpiForSystem().max(96) as f64 / 96.0)
            .min((work_area.right - work_area.left - 32).max(1) as f64 / WIDTH as f64)
            .min((work_area.bottom - work_area.top - 32).max(1) as f64 / HEIGHT as f64);
        let directory = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("Missing executable directory")?
            .to_owned();
        let path = directory.join("t7patch.conf");
        if !smoke && updater::install::before_start(&directory)? {
            return Ok(());
        }
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
        let font = |size: f64, weight: u32, face: &str| {
            CreateFontW(
                -(size * scale).round() as i32,
                0,
                0,
                0,
                weight as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                DEFAULT_PITCH as u32,
                wide(face).as_ptr(),
            )
        };
        let icon = |width, height| {
            CreateIconFromResourceEx(
                ICON_IMAGE.0.as_ptr(),
                ICON_IMAGE.0.len() as u32,
                1,
                0x00030000,
                width,
                height,
                LR_DEFAULTCOLOR,
            )
        };
        let resources = Resources {
            background: CreateSolidBrush(DARK),
            panel: CreateSolidBrush(PANEL),
            input: CreateSolidBrush(INPUT),
            border: CreateSolidBrush(BORDER),
            font: font(14.0, FW_NORMAL, "Segoe UI"),
            title_font: font(28.0, FW_SEMIBOLD, "Segoe UI"),
            small_font: font(12.0, FW_NORMAL, "Segoe UI"),
            mono_font: font(12.0, FW_NORMAL, "Consolas"),
            icon: icon(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON)),
            small_icon: icon(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)),
        };
        if resources.background.is_null()
            || resources.border.is_null()
            || resources.panel.is_null()
            || resources.input.is_null()
            || resources.font.is_null()
            || resources.title_font.is_null()
            || resources.small_font.is_null()
            || resources.mono_font.is_null()
            || resources.icon.is_null()
            || resources.small_icon.is_null()
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
            updater: RefCell::new(Updater::new(directory)),
            update_status: Cell::new(null_mut()),
            update_check: Cell::new(null_mut()),
            update_install: Cell::new(null_mut()),
            last_update_status: RefCell::new(String::new()),
            restarting: Cell::new(false),
            error: RefCell::new(error),
            last_status: RefCell::new(String::new()),
            name: Cell::new(null_mut()),
            password: Cell::new(null_mut()),
            friends: Cell::new(null_mut()),
            status: Cell::new(null_mut()),
            saved: Cell::new(null_mut()),
            status_active: Cell::new(false),
            hovered: Cell::new(null_mut()),
            tabs: Cell::new(null_mut()),
            page: Cell::new(Page::Settings),
            creating_page: Cell::new(None),
            page_controls: RefCell::new(Vec::new()),
            controls_failed: Cell::new(false),
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
            hIcon: app.resources.icon,
            ..std::mem::zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let width = app.px(WIDTH);
        let height = app.px(HEIGHT);
        let window = CreateWindowExW(
            WS_EX_APPWINDOW | WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            wide("T7Patch Rust").as_ptr(),
            WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            work_area.left + ((work_area.right - work_area.left - width) / 2).max(0),
            work_area.top + ((work_area.bottom - work_area.top - height) / 2).max(0),
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
            app.update_status.get(),
            app.update_check.get(),
            app.update_install.get(),
            app.saved.get(),
            app.tabs.get(),
            GetDlgItem(window, STATUS_DETAILS as i32),
            GetDlgItem(window, UPDATE_DETAILS as i32),
            GetDlgItem(window, LINK as i32),
            GetDlgItem(window, MINIMIZE as i32),
            GetDlgItem(window, CLOSE as i32),
        ]
        .iter()
        .any(|w| w.is_null())
            || app.controls_failed.get()
        {
            DestroyWindow(window);
            return Err("Cannot create launcher controls".into());
        }
        SendMessageW(
            window,
            WM_SETICON,
            ICON_BIG as usize,
            app.resources.icon as isize,
        );
        SendMessageW(
            window,
            WM_SETICON,
            ICON_SMALL as usize,
            app.resources.small_icon as isize,
        );
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
            if message.message == WM_KEYDOWN
                && message.wParam == VK_TAB as usize
                && GetKeyState(VK_CONTROL as i32) < 0
            {
                let direction = if GetKeyState(VK_SHIFT as i32) < 0 {
                    2
                } else {
                    1
                };
                let next = (app.page.get() as usize + direction) % Page::ALL.len();
                app.show_page(window, Page::ALL[next]);
                SetFocus(app.tabs.get());
            } else if IsDialogMessageW(window, &message) == 0 {
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

//! A pre-opened event journal for paths that can stop the entire process.
//! Unlike periodic profiling, the record is written before fatal logging/suspension.
use std::{
    fmt::{self, Write as _},
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
};

static EVENTS: OnceLock<File> = OnceLock::new();
static CRASH_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn initialize(config: &Path) {
    let preferred = config.with_file_name("t7patch-events.log");
    let fallback = std::env::temp_dir().join(format!("t7patch-events-{}.log", std::process::id()));
    for path in [&preferred, &fallback] {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => {
                let _ = EVENTS.set(file);
                let _ = CRASH_PATH.set(if path == &preferred {
                    config.with_file_name("crashes.log")
                } else {
                    fallback.with_file_name(format!("t7patch-crashes-{}.log", std::process::id()))
                });
                crate::memory::debug(&format!("Event journal: {}", path.display()));
                event(format_args!(
                    "session-start hang_tracking=v1 lobby_reader=validated-v2 game={:?} debug_assertions={}",
                    crate::game_build::current_build(),
                    cfg!(debug_assertions)
                ));
                return;
            }
            Err(error) => crate::memory::debug(&format!("Cannot open {}: {error}", path.display())),
        }
    }
}

pub fn crash_path() -> &'static Path {
    CRASH_PATH
        .get()
        .map_or(Path::new("crashes.log"), PathBuf::as_path)
}

struct Line {
    bytes: [u8; 512],
    len: usize,
}
impl fmt::Write for Line {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let count = text.len().min(self.bytes.len() - self.len);
        self.bytes[self.len..self.len + count].copy_from_slice(&text.as_bytes()[..count]);
        self.len += count;
        if count == text.len() {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

pub fn event(message: fmt::Arguments<'_>) {
    let Some(mut file) = EVENTS.get() else {
        return;
    };
    let mut line = Line {
        bytes: [0; 512],
        len: 0,
    };
    unsafe {
        let _ = writeln!(
            line,
            "tick={} thread={} {message}",
            windows_sys::Win32::System::SystemInformation::GetTickCount64(),
            windows_sys::Win32::System::Threading::GetCurrentThreadId()
        );
    }
    // No heap formatting, path lookup, or Rust mutex in an exception callback.
    // File is unbuffered; this submits the record before the process is suspended.
    if file.write_all(&line.bytes[..line.len]).is_err() {
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringA(
                c"T7 Patch Rust: event journal write failed\n"
                    .as_ptr()
                    .cast(),
            );
        }
    }
}

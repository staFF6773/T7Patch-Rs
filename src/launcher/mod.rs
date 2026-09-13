pub mod process;
mod ui;
pub mod updater;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};

#[derive(Default)]
pub struct Status {
    pub text: String,
    pub active: bool,
}
pub struct Worker {
    pub status: Arc<Mutex<Status>>,
    pub configured: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    quiescent: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    fn start(
        dll: PathBuf,
        config: PathBuf,
        configured: bool,
        enabled: bool,
    ) -> std::io::Result<Self> {
        let status = Arc::new(Mutex::new(Status {
            text: "No game process found.".into(),
            active: false,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let quiescent = Arc::new(AtomicBool::new(!enabled));
        let configured = Arc::new(AtomicBool::new(configured));
        let thread = if enabled {
            let status = status.clone();
            let stop = stop.clone();
            let configured = configured.clone();
            let paused = paused.clone();
            let quiescent = quiescent.clone();
            Some(
                std::thread::Builder::new()
                    .name("t7patch-loader".into())
                    .spawn(move || {
                        let mut session: Option<process::Session> = None;
                        let mut next_scan = std::time::Instant::now();
                        while !stop.load(Ordering::Acquire) {
                            if paused.load(Ordering::Acquire) {
                                quiescent.store(true, Ordering::Release);
                                std::thread::sleep(Duration::from_millis(100));
                                continue;
                            }
                            quiescent.store(false, Ordering::Release);
                            if session.as_ref().is_some_and(|s| !s.alive()) {
                                session = None;
                            }
                            let current = if let Some(session) = &mut session {
                                session.poll();
                                Status {
                                    text: session.status.clone(),
                                    active: session.active,
                                }
                            } else if std::time::Instant::now() >= next_scan {
                                next_scan = std::time::Instant::now() + Duration::from_secs(1);
                                match process::find_game() {
                                    Ok(Some(pid)) if configured.load(Ordering::Acquire) => {
                                        match process::Session::attach(
                                            pid,
                                            "BlackOps3.exe",
                                            &dll,
                                            &config,
                                        ) {
                                            Ok(new_session) => {
                                                let text = new_session.status.clone();
                                                session = Some(new_session);
                                                Status {
                                                    text,
                                                    active: false,
                                                }
                                            }
                                            Err(error) => Status {
                                                text: error,
                                                active: false,
                                            },
                                        }
                                    }
                                    Ok(Some(_)) => Status {
                                        text: "Save valid settings before patching.".into(),
                                        active: false,
                                    },
                                    Ok(None) => Status {
                                        text: "No game process found.".into(),
                                        active: false,
                                    },
                                    Err(error) => Status {
                                        text: error,
                                        active: false,
                                    },
                                }
                            } else {
                                std::thread::sleep(Duration::from_millis(100));
                                continue;
                            };
                            *status.lock().unwrap_or_else(|e| e.into_inner()) = current;
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    })?,
            )
        } else {
            None
        };
        Ok(Self {
            status,
            stop,
            paused,
            quiescent,
            configured,
            thread,
        })
    }
    pub fn pause(&self) {
        self.paused.store(true, Ordering::Release);
    }
    pub fn resume(&self) {
        self.quiescent.store(false, Ordering::Release);
        self.paused.store(false, Ordering::Release);
    }
    pub fn is_quiescent(&self) -> bool {
        self.quiescent.load(Ordering::Acquire)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn run(smoke: bool) -> Result<(), String> {
    ui::run(smoke)
}

mod http;
pub mod install;
mod package;

use install::Prepared;
use package::{Manifest, Release, ARCHIVE, MANIFEST, MAX_JSON, REPOSITORY};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};

#[derive(Clone, Debug)]
pub enum State {
    Idle(String),
    Checking,
    Available(Release),
    Downloading(String),
    Ready(Prepared),
    Handoff(Prepared),
    Error(String),
}
impl State {
    pub fn text(&self) -> String {
        match self {
            Self::Idle(text) | Self::Error(text) | Self::Downloading(text) => text.clone(),
            Self::Checking => "Checking GitHub for updates...".into(),
            Self::Available(release) => format!(
                "{} available (installed v{}).",
                release.tag_name,
                env!("CARGO_PKG_VERSION")
            ),
            Self::Ready(update) => format!(
                "v{} verified. Close BO3 to install and restart.",
                update.version
            ),
            Self::Handoff(_) => "Pausing game detection and restarting to update...".into(),
        }
    }
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading(_) | Self::Handoff(_)
        )
    }
}
pub struct Updater {
    shared: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    directory: PathBuf,
}
impl Updater {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            shared: Arc::new(Mutex::new(State::Idle(
                "Updates have not been checked.".into(),
            ))),
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            directory,
        }
    }
    pub fn state(&self) -> State {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn set(&self, state: State) {
        *self.shared.lock().unwrap_or_else(|e| e.into_inner()) = state;
    }
    fn launch(
        &mut self,
        work: impl FnOnce(Arc<AtomicBool>, Arc<Mutex<State>>) -> Result<State, String> + Send + 'static,
    ) {
        self.stop.store(true, Ordering::Release);
        self.stop = Arc::new(AtomicBool::new(false));
        let stop = self.stop.clone();
        let shared = self.shared.clone();
        self.worker = match std::thread::Builder::new()
            .name("t7patch-updater".into())
            .spawn(move || {
                let result = work(stop.clone(), shared.clone());
                if !stop.load(Ordering::Acquire) {
                    *shared.lock().unwrap_or_else(|e| e.into_inner()) =
                        result.unwrap_or_else(State::Error);
                } else if let Ok(State::Ready(update)) = result {
                    let _ = std::fs::remove_dir_all(update.stage);
                }
            }) {
            Ok(worker) => Some(worker),
            Err(error) => {
                self.set(State::Error(error.to_string()));
                None
            }
        };
    }
    pub fn check(&mut self) {
        if self.state().busy() || matches!(self.state(), State::Ready(_)) {
            return;
        }
        self.set(State::Checking);
        self.launch(|stop, _| {
            let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
            let (status, body) = http::get(&url, MAX_JSON, &stop, |_| {})?;
            if status == 404 {
                return Ok(State::Idle("No published update releases yet.".into()));
            }
            if status != 200 {
                return Err(http::status_error(status));
            }
            let release: Release = serde_json::from_slice(&body)
                .map_err(|e| format!("Invalid GitHub response: {e}"))?;
            if release.newer_than(env!("CARGO_PKG_VERSION"))? {
                Ok(State::Available(release))
            } else {
                Ok(State::Idle(format!(
                    "v{} is up to date.",
                    env!("CARGO_PKG_VERSION")
                )))
            }
        });
    }
    pub fn download(&mut self) {
        let State::Available(release) = self.state() else {
            return;
        };
        if !std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_owned()))
            .is_some_and(|n| n == "t7patch.exe")
        {
            self.set(State::Error(
                "Run the packaged dist/t7patch.exe to install updates.".into(),
            ));
            return;
        }
        let directory = self.directory.clone();
        self.set(State::Downloading("Downloading update manifest...".into()));
        self.launch(move |stop, shared| {
            release.newer_than(env!("CARGO_PKG_VERSION"))?;
            let (status, bytes) = http::get(
                &release.asset_url(MANIFEST),
                release.asset_size(MANIFEST, MAX_JSON as u64)? as usize,
                &stop,
                |_| {},
            )?;
            if status != 200 {
                return Err(http::status_error(status));
            }
            let manifest = Manifest::parse(&bytes)?;
            manifest.matches_release(&release)?;
            let (status, archive) = http::get(
                &release.asset_url(ARCHIVE),
                manifest.archive.size as usize,
                &stop,
                |received| {
                    *shared.lock().unwrap_or_else(|e| e.into_inner()) =
                        State::Downloading(format!(
                            "Downloading {}: {} / {} KB",
                            release.tag_name,
                            received / 1024,
                            manifest.archive.size / 1024
                        ));
                },
            )?;
            if status != 200 {
                return Err(http::status_error(status));
            }
            *shared.lock().unwrap_or_else(|e| e.into_inner()) =
                State::Downloading("Verifying update files...".into());
            let stage = install::create_stage(&directory)?;
            package::extract(&manifest, &archive, &stage.path.join("new"), || {
                stop.load(Ordering::Acquire)
            })?;
            install::write_new(&stage.path.join(MANIFEST), &bytes)?;
            if stop.load(Ordering::Acquire) {
                return Err("Update cancelled".into());
            }
            Ok(State::Ready(Prepared {
                stage: stage.persist(),
                version: manifest.version,
            }))
        });
    }
}
impl Drop for Updater {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Requests have bounded timeouts and own their buffers/handles. Do not
        // hold the Win32 UI open waiting for an unreachable network on exit.
        if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

/// Offline packaging check used by the release workflow, using the same verifier
/// as the download path. The temporary extraction is removed on success or error.
pub fn verify_package(directory: &std::path::Path) -> Result<(), String> {
    let path = directory.join(MANIFEST);
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > MAX_JSON as u64 {
        return Err("Update manifest is too large".into());
    }
    let manifest = Manifest::parse(&std::fs::read(path).map_err(|e| e.to_string())?)?;
    let archive = directory.join(ARCHIVE);
    if std::fs::metadata(&archive)
        .map_err(|e| e.to_string())?
        .len()
        != manifest.archive.size
    {
        return Err("Archive size mismatch".into());
    }
    let bytes = std::fs::read(archive).map_err(|e| e.to_string())?;
    let stage = install::create_stage(directory)?;
    package::extract(&manifest, &bytes, &stage.path.join("new"), || false)?;
    manifest.verify_directory(&stage.path.join("new"))?;
    println!(
        "Update package v{}: ZIP, hashes, x64 EXE/DLL and DLL bootstrap verified.",
        manifest.version
    );
    Ok(())
}

#[cfg(test)]
mod tests;

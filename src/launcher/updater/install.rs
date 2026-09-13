use super::super::process::{self, Handle};
use super::package::{self, Entry, Manifest, FILES};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
};
use windows_sys::Win32::{Foundation::*, Storage::FileSystem::*, System::Threading::*};

const JOURNAL: &str = ".t7patch-update.json";
const OWNER: &str = "t7patch-updater-v1";
const PREFIX: &str = ".t7patch-update-";
const UPDATE_MUTEX: &str = "Local\\T7PatchRustUpdate";
const LAUNCHER_MUTEX: &str = "Local\\T7PatchRustLauncher";
static NEXT_PATH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct Prepared {
    pub stage: PathBuf,
    pub version: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    stage: String,
    originals: Vec<Entry>,
}

pub struct StageGuard {
    pub path: PathBuf,
    keep: bool,
}
impl StageGuard {
    pub fn persist(mut self) -> PathBuf {
        self.keep = true;
        self.path.clone()
    }
}
impl Drop for StageGuard {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
pub fn create_stage(directory: &Path) -> Result<StageGuard, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let nonce = NEXT_PATH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stage = directory.join(format!("{PREFIX}{}-{stamp}-{nonce}", std::process::id()));
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let guard = StageGuard {
        path: stage,
        keep: false,
    };
    write_new(&guard.path.join("owner"), OWNER.as_bytes())?;
    fs::create_dir(guard.path.join("new")).map_err(|e| e.to_string())?;
    fs::create_dir(guard.path.join("backup")).map_err(|e| e.to_string())?;
    Ok(guard)
}
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
fn replace(source: &Path, destination: &Path) -> Result<(), String> {
    if unsafe {
        MoveFileExW(
            wide(source).as_ptr(),
            wide(destination).as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(format!(
            "Replacing {}: {}",
            destination.display(),
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}
pub fn update_lock() -> Result<Handle, String> {
    named_lock(UPDATE_MUTEX)
}
fn named_lock(name: &str) -> Result<Handle, String> {
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, wide(Path::new(name)).as_ptr());
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        let handle = Handle(handle);
        if exists {
            return Err("Another launcher or update is running. Try again when it closes.".into());
        }
        Ok(handle)
    }
}
fn stage_in(directory: &Path, name: &str) -> Result<PathBuf, String> {
    if !name.starts_with(PREFIX)
        || !name[PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-')
        || name.len() <= PREFIX.len()
    {
        return Err("Invalid update staging directory".into());
    }
    let root = fs::canonicalize(directory).map_err(|e| e.to_string())?;
    let stage = fs::canonicalize(root.join(name)).map_err(|e| e.to_string())?;
    if stage.parent() != Some(root.as_path())
        || fs::read(stage.join("owner")).map_err(|e| e.to_string())? != OWNER.as_bytes()
    {
        return Err("Update staging directory is not owned by this launcher".into());
    }
    Ok(stage)
}
fn stage_name(stage: &Path) -> Result<String, String> {
    stage
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or("Invalid staging directory name".into())
}
fn load_journal(directory: &Path) -> Result<Journal, String> {
    let path = directory.join(JOURNAL);
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > package::MAX_JSON as u64 {
        return Err("Invalid update journal size".into());
    }
    let journal: Journal = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("Cannot recover update journal: {e}"))?;
    if journal.schema != 1
        || journal.originals.len() != 2
        || FILES.iter().any(|name| {
            journal
                .originals
                .iter()
                .filter(|e| e.name == *name && e.size <= package::MAX_FILE && e.sha256.len() == 64)
                .count()
                != 1
        })
    {
        return Err("Invalid update recovery journal".into());
    }
    stage_in(directory, &journal.stage)?;
    Ok(journal)
}
fn load_manifest(stage: &Path) -> Result<Manifest, String> {
    let path = stage.join(package::MANIFEST);
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > package::MAX_JSON as u64 {
        return Err("Invalid staged manifest size".into());
    }
    Manifest::parse(&fs::read(path).map_err(|e| e.to_string())?)
}

// The journal is durable before the first replacement. Backups stay intact until
// both files are verified; removal of the journal is the commit point.
fn transaction(
    directory: &Path,
    stage: &Path,
    mut move_file: impl FnMut(&Path, &Path) -> Result<(), String>,
) -> Result<(), String> {
    if directory
        .join(JOURNAL)
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        return Err("Recover the previous update before installing another one.".into());
    }
    let manifest = load_manifest(stage)?;
    manifest.verify_directory(&stage.join("new"))?;
    let mut originals = Vec::new();
    for name in FILES {
        let path = directory.join(name);
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > package::MAX_FILE
        {
            return Err(format!("Invalid installed file: {name}"));
        }
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        write_new(&stage.join("backup").join(name), &bytes)?;
        originals.push(Entry {
            name: name.into(),
            size: bytes.len() as u64,
            sha256: package::hash(&bytes),
        });
    }
    let journal = Journal {
        schema: 1,
        stage: stage_name(stage)?,
        originals,
    };
    let prepared_journal = stage.join("transaction.json");
    write_new(
        &prepared_journal,
        &serde_json::to_vec(&journal).map_err(|e| e.to_string())?,
    )?;
    // Publish the complete, synced journal with a same-volume rename. A partial
    // write must never leave an unreadable recovery marker beside an intact pair.
    // No REPLACE_EXISTING: an earlier transaction must not be overwritten.
    if unsafe {
        MoveFileExW(
            wide(&prepared_journal).as_ptr(),
            wide(&directory.join(JOURNAL)).as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(format!(
            "Publishing update journal: {}",
            std::io::Error::last_os_error()
        ));
    }
    let result: Result<(), String> = (|| {
        for name in FILES {
            move_file(&stage.join("new").join(name), &directory.join(name))?;
        }
        manifest.verify_directory(directory)?;
        fs::remove_file(directory.join(JOURNAL)).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(error) = result {
        return match recover(directory) {
            Ok(()) => Err(format!("Update failed; previous version restored. {error}")),
            Err(restore) => Err(format!(
                "Update interrupted. Backups retained for recovery. {error}; {restore}"
            )),
        };
    }
    let _ = write_new(&stage.join("complete"), b"committed");
    Ok(())
}
fn recover(directory: &Path) -> Result<(), String> {
    let journal = load_journal(directory)?;
    let stage = stage_in(directory, &journal.stage)?;
    // Validate every backup before restoring any file.
    let mut backups = Vec::new();
    for entry in &journal.originals {
        let path = stage.join("backup").join(&entry.name);
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() != entry.size {
            return Err("Incomplete update backup".into());
        }
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        entry.verify(&bytes)?;
        backups.push((entry, bytes));
    }
    for (entry, bytes) in backups {
        let target = directory.join(&entry.name);
        if fs::metadata(&target)
            .ok()
            .is_some_and(|m| m.len() == entry.size)
            && fs::read(&target)
                .ok()
                .is_some_and(|b| entry.verify(&b).is_ok())
        {
            continue;
        }
        let temporary = stage.join(format!("restore-{}", entry.name));
        if temporary.exists() {
            fs::remove_file(&temporary).map_err(|e| e.to_string())?;
        }
        write_new(&temporary, &bytes)?;
        replace(&temporary, &target)?;
    }
    fs::remove_file(directory.join(JOURNAL)).map_err(|e| e.to_string())?;
    let _ = write_new(&stage.join("complete"), b"restored");
    Ok(())
}

fn creation_time(process: HANDLE) -> Result<u64, String> {
    unsafe {
        let mut created = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetProcessTimes(process, &mut created, &mut exit, &mut kernel, &mut user) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
    }
}
pub fn start_helper(directory: &Path, stage: &Path, recovery: bool) -> Result<(), String> {
    let stage = stage_in(directory, &stage_name(stage)?)?;
    let source = std::env::current_exe().map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let nonce = NEXT_PATH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let helper = stage.join(format!("helper-{}-{stamp}-{nonce}.exe", std::process::id()));
    write_new(&helper, &fs::read(source).map_err(|e| e.to_string())?)?;
    let created = creation_time(unsafe { GetCurrentProcess() })?;
    Command::new(&helper)
        .arg(if recovery {
            "--recover-update"
        } else {
            "--apply-update"
        })
        .arg(std::process::id().to_string())
        .arg(created.to_string())
        .arg(directory)
        .arg(&stage)
        .current_dir(directory)
        .spawn()
        .map_err(|e| format!("Starting update helper: {e}"))?;
    Ok(())
}

/// Called under the launcher's singleton, before starting game detection.
pub fn before_start(directory: &Path) -> Result<bool, String> {
    let lock = update_lock()?;
    if directory
        .join(JOURNAL)
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        let journal = load_journal(directory)?;
        let stage = stage_in(directory, &journal.stage)?;
        drop(lock);
        start_helper(directory, &stage, true)?;
        return Ok(true);
    }
    cleanup(directory);
    Ok(false)
}
fn cleanup(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(PREFIX) {
            continue;
        }
        let Ok(stage) = stage_in(directory, &name) else {
            continue;
        };
        // Called at startup under the singleton and update lock, only when no
        // recovery journal exists. Previous incomplete downloads are stale too.
        // If a helper is still mapped, keep its ownership marker for the next run.
        let helpers_removed = fs::read_dir(&stage).ok().is_some_and(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("helper-"))
                .all(|e| fs::remove_file(e.path()).is_ok())
        });
        if helpers_removed {
            let _ = fs::remove_dir_all(stage);
        }
    }
}

pub fn helper(args: &[std::ffi::OsString]) -> Result<(), String> {
    if args.len() != 6 {
        return Err("Invalid update helper arguments".into());
    }
    let recovery = args[1] == "--recover-update";
    let pid: u32 = args[2]
        .to_str()
        .ok_or("Invalid parent PID")?
        .parse()
        .map_err(|_| "Invalid parent PID")?;
    let created: u64 = args[3]
        .to_str()
        .ok_or("Invalid parent identity")?
        .parse()
        .map_err(|_| "Invalid parent identity")?;
    let directory = fs::canonicalize(&args[4]).map_err(|e| e.to_string())?;
    let stage = stage_in(&directory, &stage_name(Path::new(&args[5]))?)?;
    let own = fs::canonicalize(std::env::current_exe().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if own.parent() != Some(stage.as_path()) {
        return Err("Updater helper must run from its staging directory".into());
    }
    let update_lock = update_lock()?;
    unsafe {
        let parent = OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if !parent.is_null() {
            let parent = Handle(parent);
            if creation_time(parent.0)? == created
                && WaitForSingleObject(parent.0, 30000) != WAIT_OBJECT_0
            {
                return Err("Launcher did not exit; update was not applied.".into());
            }
        } else if GetLastError() != ERROR_INVALID_PARAMETER {
            return Err("Cannot verify that the launcher exited".into());
        }
    }
    let launcher_lock = named_lock(LAUNCHER_MUTEX)?;
    if process::find_game()?.is_some() {
        return Err("Close BO3 before applying or recovering an update.".into());
    }
    let result = if recovery {
        recover(&directory)
    } else {
        transaction(&directory, &stage, replace)
    };
    if let Err(error) = &result {
        let _ = fs::write(directory.join("t7patch-update.log"), error);
    }
    drop(launcher_lock);
    drop(update_lock);
    // A pending journal prevents starting a mismatched EXE/DLL pair. A successful
    // rollback may relaunch the old pair; a committed update launches the new one.
    if !directory
        .join(JOURNAL)
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        Command::new(directory.join("t7patch.exe"))
            .current_dir(&directory)
            .spawn()
            .map_err(|e| {
                format!("Files are ready, but restart failed: {e}. Open t7patch.exe manually.")
            })?;
    }
    result
}

#[cfg(test)]
mod tests;

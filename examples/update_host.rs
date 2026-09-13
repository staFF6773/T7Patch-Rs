//! Harmless successor executable used by scripts/test-update.ps1. Never loads a DLL or searches for BO3.
fn main() {
    let executable = std::env::current_exe().unwrap();
    std::fs::write(
        executable.parent().unwrap().join("update-host-started.txt"),
        b"started",
    )
    .unwrap();
}

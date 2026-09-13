//! Harmless child process used exclusively by loader_smoke (it is not named BlackOps3.exe).
fn main() {
    std::thread::sleep(std::time::Duration::from_secs(60));
}

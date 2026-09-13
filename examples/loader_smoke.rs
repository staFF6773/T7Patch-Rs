//! Exercise the actual remote loader against our own child process, never a running game.
#[allow(dead_code)]
#[path = "../src/launcher_api.rs"]
mod launcher_api;
#[allow(dead_code)]
#[path = "../src/launcher/process.rs"]
mod process;

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn main() {
    let directory = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let dll = directory.parent().unwrap().join("t7patch.dll");
    let host = directory.join("launcher_host.exe");
    assert!(host.is_file(), "Build examples/launcher_host first");
    let child = Child(std::process::Command::new(host).spawn().unwrap());
    let config = std::env::temp_dir().join("t7patch-loader-smoke.conf");
    std::thread::sleep(std::time::Duration::from_millis(150));
    for pass in 0..2 {
        let mut session =
            process::Session::attach(child.0.id(), "launcher_host.exe", &dll, &config).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            session.poll();
            if session.code != 0 {
                break;
            }
            assert!(session.alive(), "Child exited unexpectedly");
            assert!(
                std::time::Instant::now() < deadline,
                "Loader timed out: {}",
                session.status
            );
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        assert_eq!(
            session.code,
            launcher_api::UNSUPPORTED,
            "{}",
            session.status
        );
        assert!(!session.active);
        println!("Remote loader pass {}: {}", pass + 1, session.status);
    }
    println!("Remote DLL loading, existing-module detection and bootstrap reply OK; no game was targeted.");
}

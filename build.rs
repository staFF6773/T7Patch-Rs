fn main() {
    let target = std::env::var("TARGET").unwrap();
    assert_eq!(
        target, "x86_64-pc-windows-msvc",
        "Build for Windows x64 MSVC (also used by Wine/Proton)"
    );
    // minhook-sys builds the native library with the matching CRT.
    println!("cargo:rerun-if-changed=build.rs");
}

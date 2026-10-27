//! Links the vendored Cubism Core DLL when building for the GNU toolchain.
//!
//! `bongocat-live2d/build.rs` emits the link directives for
//! `x86_64-pc-windows-msvc` only. The vendored DLL exports the same C symbols
//! and mingw's `ld` links against a DLL directly, so a GNU development build
//! needs nothing beyond a search path and the import name. Release builds stay
//! on MSVC, where this script does nothing.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        return;
    }
    let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/cubism/5-r.5/Core/dll/windows/x86_64");
    println!("cargo::rustc-link-search=native={}", core.display());
    println!("cargo::rustc-link-arg=-l:Live2DCubismCore.dll");
}

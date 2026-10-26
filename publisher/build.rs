//! Links the vendored Cubism Core DLL when building for the GNU toolchain.
//!
//! `bongocat-live2d/build.rs` emits link directives for `x86_64-pc-windows-msvc`
//! only. The vendored DLL exports the same C symbols and mingw's `ld` links
//! against a DLL directly, so a GNU build needs nothing beyond a search path and
//! the import name.
//!
//! The Core binary belongs to the upstream checkout this publisher reads its
//! models and crates from, so the default search path is that checkout rather
//! than a second copy here. `CATSCREEN_VENDOR` overrides it when the upstream
//! tree lives somewhere else.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        return;
    }
    let vendor = std::env::var_os("CATSCREEN_VENDOR").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../kat")
                .join("vendor")
        },
        PathBuf::from,
    );
    let core = vendor.join("cubism/5-r.5/Core/dll/windows/x86_64");
    println!("cargo::rustc-link-search=native={}", core.display());
    println!("cargo::rustc-link-arg=-l:Live2DCubismCore.dll");
}

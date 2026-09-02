//! This script links the committed Windows resource.
//! The resource supplies the executable icon and the application manifest.
//! The manifest requests Common Controls v6.
//!
//! **The project keeps the `.res` file in the repository.** This script does not
//! compile the resource. A build script that invokes `rc.exe` would require every
//! build to locate a Windows SDK. The project has no build dependencies.
//! Regenerate the file when `assets/chibipop.ico` or
//! `assets/chibipop.manifest` changes. Use **PowerShell** for this task.
//! MSYS2 changes the `/flag` arguments for `rc.exe` when you run the command from
//! git-bash:
//!
//! ```powershell
//! $rc = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter rc.exe |
//!       Where-Object { $_.FullName -like "*x64*" } | Select-Object -Last 1
//! Push-Location assets; & $rc.FullName /nologo /fo chibipop.res chibipop.rc; Pop-Location
//! ```
//!
//! Cargo does not need a `build = "build.rs"` key in `Cargo.toml`.
//! Cargo compiles and runs `build.rs` in the package root by convention.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo always sets this");
    println!("cargo:rerun-if-changed={dir}/assets/chibipop.res");
    println!("cargo:rerun-if-changed={dir}/assets/chibipop.manifest");
    // Select `-bins`, not the plain form. The plain form also applies to test and
    // doctest link steps. Those steps must not link this resource.
    println!("cargo:rustc-link-arg-bins={dir}/assets/chibipop.res");
}

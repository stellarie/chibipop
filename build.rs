//! Links the checked-in Windows resource: the executable's icon, and the
//! application manifest that asks for Common Controls v6.
//!
//! **The `.res` is committed rather than compiled here, on purpose.** Invoking
//! `rc.exe` from a build script would make every build depend on locating a
//! Windows SDK, and this project has taken no build dependencies since it
//! started. Regenerate it whenever `assets/chibipop.ico` or
//! `assets/chibipop.manifest` changes — from **PowerShell**, because MSYS2
//! mangles `rc.exe`'s `/flag` arguments under git-bash:
//!
//! ```powershell
//! $rc = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter rc.exe |
//!       Where-Object { $_.FullName -like "*x64*" } | Select-Object -Last 1
//! Push-Location assets; & $rc.FullName /nologo /fo chibipop.res chibipop.rc; Pop-Location
//! ```
//!
//! No `build = "build.rs"` key is needed in `Cargo.toml`: Cargo compiles and
//! runs a `build.rs` in the package root by convention.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo always sets this");
    println!("cargo:rerun-if-changed={dir}/assets/chibipop.res");
    println!("cargo:rerun-if-changed={dir}/assets/chibipop.manifest");
    // `-bins`, not the plain form: the plain one also applies to test and
    // doctest link steps, where a resource file has no business.
    println!("cargo:rustc-link-arg-bins={dir}/assets/chibipop.res");
}

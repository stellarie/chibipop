//! The release tarball, built and taken apart again.
//!
//! `scripts/package-linux.sh` decides where the asset puts things and
//! `src/ocr/models.rs` decides where the running binary looks for them.
//! Those are two files, and the failure they can produce between them is
//! the worst kind: a release that builds green, extracts cleanly, and then
//! refuses to OCR because the models are one directory off. So this test
//! runs the real script over a stub binary, extracts the real tarball, and
//! asks the binary's own search where the models are.
//!
//! It is cheap enough to keep in the default sweep: the models compress in
//! about a second, and the alternative is finding out at release time.
#![cfg(target_os = "linux")]

use chibipop_linux::ocr::models;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A version the repo will never carry, so nothing here can be confused
/// for a real asset.
const VERSION: &str = "v99.98.97";
const ASSET: &str = "chibipop-v99.98.97-linux-x64";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// A scratch root, per test, cleaned on entry rather than on exit: a
/// failure that leaves the tree behind is a failure you can go and look at.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chibipop-tarball-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Enough of a binary for packaging to be a fair test: the ELF magic the
/// script checks, and the executable bit. Packaging the real 62 MB release
/// binary would only measure `cp`.
fn stub_binary(dir: &Path) -> PathBuf {
    let path = dir.join("chibipop-stub");
    std::fs::write(&path, b"\x7fELF stub binary, not a real one\n").unwrap();
    executable(&path);
    path
}

fn executable(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn package(out: &Path, version: &str, binary: &Path) -> Output {
    Command::new("bash")
        .arg(repo().join("scripts/package-linux.sh"))
        .arg(version)
        .arg(binary)
        .env("OUT", out)
        .current_dir(repo())
        .output()
        .expect("bash must be able to run the packaging script")
}

fn expect_ok(out: &Output) {
    assert!(
        out.status.success(),
        "packaging failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_asset_extracts_to_a_tree_the_binary_can_find_its_models_in() {
    let root = scratch("layout");
    let out = root.join("dist");
    let binary = stub_binary(&root);

    let first = package(&out, VERSION, &binary);
    expect_ok(&first);

    // The forever contract (ARCHITECTURE.md#packaging-and-ci): the update
    // check parses this name off releases/latest, so its shape is not ours
    // to drift.
    let tarball = out.join(format!("{ASSET}.tar.gz"));
    assert!(tarball.is_file(), "no {} - packaging wrote something else", tarball.display());

    // Sorted names, no owners, no timestamps, `gzip -n`: the same inputs
    // must give the same bytes, or "is this the asset CI built" stops
    // being a question a sha256sum can answer.
    let once = std::fs::read(&tarball).unwrap();
    expect_ok(&package(&out, VERSION, &binary));
    assert_eq!(once, std::fs::read(&tarball).unwrap(), "the tarball is not reproducible");

    let extract = root.join("extract");
    std::fs::create_dir_all(&extract).unwrap();
    let status = Command::new("tar")
        .arg("xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&extract)
        .status()
        .unwrap();
    assert!(status.success(), "tar xzf refused the asset");

    // One directory at the top, named after the asset: `tar xzf` in a
    // home directory must not scatter files across it.
    let entries: Vec<PathBuf> =
        std::fs::read_dir(&extract).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(1, entries.len(), "expected one top-level directory, got {entries:?}");
    let root_dir = extract.join(ASSET);
    assert_eq!(vec![root_dir.clone()], entries);

    // The whole point: the shipped binary's own search, run against a
    // really extracted tree.
    assert_eq!(
        Some(root_dir.join("models/meiki")),
        models::beside(&root_dir),
        "the binary would not find the bundled models beside itself"
    );

    // The Windows zip's shape, plus what Linux adds. Named explicitly:
    // every one of these is something a user notices missing.
    for rel in [
        "chibipop",
        "data/deconjugator.json",
        "README.md",
        "LICENSE",
        "models/meiki/LICENSE.md",
        "models/meiki/SHA256SUMS.txt",
        "extras/README.md",
        "extras/chibipop.desktop",
        "extras/chibipop.service",
        "extras/hyprland.conf",
    ] {
        assert!(root_dir.join(rel).is_file(), "the asset is missing {rel}");
    }
    for (name, _) in models::ALL {
        assert!(root_dir.join("models/meiki").join(name).is_file(), "the asset is missing {name}");
    }

    // Executable, and only the binary is: a data file arriving with the
    // executable bit is how a tarball ends up looking like malware.
    let mode = |rel: &str| {
        std::fs::metadata(root_dir.join(rel)).unwrap().permissions().mode() & 0o777
    };
    assert_eq!(0o755, mode("chibipop"));
    assert_eq!(0o644, mode("data/deconjugator.json"));
    assert_eq!(0o644, mode("extras/chibipop.service"));

    std::fs::remove_dir_all(&root).ok();
}

/// A version string that is not the contract must stop the release, not
/// produce an asset no update check can read.
#[test]
fn a_version_that_breaks_the_asset_name_is_refused() {
    let root = scratch("version");
    let out = root.join("dist");
    let binary = stub_binary(&root);

    for bad in ["0.8.2", "v0.8", "latest", "v0.8.2 ", ""] {
        let attempt = package(&out, bad, &binary);
        assert!(!attempt.status.success(), "packaging accepted the version {bad:?}");
        assert!(
            String::from_utf8_lossy(&attempt.stderr).contains("version must look like"),
            "wrong refusal for {bad:?}: {}",
            String::from_utf8_lossy(&attempt.stderr)
        );
    }
    assert!(!out.exists(), "a refused version must not leave a staging tree behind");

    std::fs::remove_dir_all(&root).ok();
}

/// Both bin crates produce a binary named `chibipop` and cargo uplifts
/// both to `target/<profile>/chibipop`, so packaging the wrong one is a
/// live hazard - see the workspace Cargo.toml.
#[test]
fn a_binary_that_is_not_an_elf_is_refused() {
    let root = scratch("elf");
    let out = root.join("dist");
    let fake = root.join("chibipop.exe");
    std::fs::write(&fake, b"MZ\x90\x00this is a PE, not an ELF").unwrap();
    executable(&fake);

    let attempt = package(&out, VERSION, &fake);
    assert!(!attempt.status.success(), "packaging accepted a non-ELF binary");
    assert!(
        String::from_utf8_lossy(&attempt.stderr).contains("is not an ELF binary"),
        "{}",
        String::from_utf8_lossy(&attempt.stderr)
    );

    let missing = package(&out, VERSION, &root.join("nothing-here"));
    assert!(!missing.status.success(), "packaging accepted a missing binary");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no binary at"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );

    std::fs::remove_dir_all(&root).ok();
}

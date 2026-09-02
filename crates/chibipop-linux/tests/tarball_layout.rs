//! Build the release tarball, extract it, and check its layout.
//!
//! `scripts/package-linux.sh` chooses each asset path.
//! `src/ocr/models.rs` chooses the model path that a binary uses.
//! A mismatch can pass the release build and extraction, then stop OCR because the models
//! sit one directory away.
//! This test runs the real script with a stub binary, extracts the real tarball, and asks the
//! binary search code for the model path.
//!
//! The test fits the default sweep. Model compression takes about one second.
//! A cheap test can prevent a broken release.
#![cfg(target_os = "linux")]

use chibipop_linux::ocr::models;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A version that the repository never carries.
/// No file in this test can look like a real asset.
const VERSION: &str = "v99.98.97";
const ASSET: &str = "chibipop-v99.98.97-linux-x64";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Create one scratch root per test.
/// Clean the root on entry, not on exit, so a failure leaves the tree for inspection.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chibipop-tarball-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Use only the binary parts that make this test fair.
/// The stub has the ELF magic that the script checks and the executable bit.
/// A real 62 MB release binary needs only `cp`.
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

    // The asset name is a permanent contract (ARCHITECTURE.md#packaging-and-ci).
    // The update check parses this name from releases/latest.
    // Do not change its shape.
    let tarball = out.join(format!("{ASSET}.tar.gz"));
    assert!(tarball.is_file(), "no {} - packaging wrote something else", tarball.display());

    // The script sorts names, removes owners and timestamps, and runs `gzip -n`.
    // The same inputs must produce the same bytes.
    // Without reproducibility, a sha256sum cannot answer whether CI built this asset.
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

    // The archive has one top directory named after the asset.
    // A `tar xzf` in a home directory must not spread files outside that directory.
    let entries: Vec<PathBuf> =
        std::fs::read_dir(&extract).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(1, entries.len(), "expected one top-level directory, got {entries:?}");
    let root_dir = extract.join(ASSET);
    assert_eq!(vec![root_dir.clone()], entries);

    // Check the shipped binary's search code against the tree that `tar` extracted.
    assert_eq!(
        Some(root_dir.join("models/meiki")),
        models::beside(&root_dir),
        "the binary would not find the bundled models beside itself"
    );

    // The list matches the Windows zip and the files that Linux adds.
    // Name each file because a user notices each missing file.
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

    // The binary is executable. No other file is executable.
    // An executable data file can make the tarball look malicious.
    let mode = |rel: &str| {
        std::fs::metadata(root_dir.join(rel)).unwrap().permissions().mode() & 0o777
    };
    assert_eq!(0o755, mode("chibipop"));
    assert_eq!(0o644, mode("data/deconjugator.json"));
    assert_eq!(0o644, mode("extras/chibipop.service"));

    std::fs::remove_dir_all(&root).ok();
}

/// A version outside the asset-name contract must stop the release.
/// It must not produce an asset that an update check cannot read.
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

/// Both bin crates produce a binary named `chibipop`.
/// Cargo copies both binaries to `target/<profile>/chibipop`.
/// A package step can therefore select the wrong binary. See the workspace Cargo.toml.
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

//! Test the install layout of the AUR packages from the real PKGBUILDs.
//!
//! `packaging/aur/*/PKGBUILD` sets the install paths for the models and the
//! deconjugation rules. `src/ocr/models.rs` sets the search path for
//! `/usr/bin/chibipop`. These files must agree. Otherwise a package can build
//! and install, then refuse OCR because the models sit one directory away.
//! A clean-chroot build cannot catch this case. The models exist, but their
//! directory does not match the binary search path.
//!
//! This test runs each PKGBUILD's `package()` in Bash with a stub `$srcdir`.
//! It checks the binary search against the models in the `$pkgdir` that
//! `package()` creates. `makepkg` is not needed or available on a CI runner.
//! `package()` is a shell function that calls `install`. `$srcdir` and `$pkgdir`
//! are variables.
//!
//! Both packages use one layout. Apply the same assertions to both packages.
#![cfg(target_os = "linux")]

use chibipop_linux::ocr::models;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn aur() -> PathBuf {
    repo().join("packaging/aur")
}

/// Create a scratch root for one test. Delete it before the test, not after it.
/// If a failure leaves the tree, you can inspect it.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chibipop-aur-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Read the current version from a template.
///
/// Get the value from the template instead of a fixed value here. `bump.sh` moves
/// the value. This avoids a version edit in the test when the template changes.
fn pkgver(pkgbuild: &Path) -> String {
    let out = bash(&format!("source '{}'; printf %s \"$pkgver\"", pkgbuild.display()));
    String::from_utf8(out).unwrap()
}

/// Run a Bash snippet and require a clean exit.
fn bash(script: &str) -> Vec<u8> {
    let out = Command::new("bash")
        .arg("-euo")
        .arg("pipefail")
        .arg("-c")
        .arg(script)
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "bash failed: {script}\n--- stdout\n{}\n--- stderr\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Run `package()` from `pkgbuild`. Set makepkg's two directories by hand.
fn run_package(pkgbuild: &Path, srcdir: &Path, pkgdir: &Path) {
    bash(&format!(
        "export srcdir='{}' pkgdir='{}'; cd \"$srcdir\"; source '{}'; package",
        srcdir.display(),
        pkgdir.display(),
        pkgbuild.display()
    ));
}

/// Create a binary file for the package. The test never reads it. A copy of the
/// real 62 MB file would measure only `cp`.
fn stub_binary(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"\x7fELF stub binary, not a real one\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Check every package requirement against a real `$pkgdir`.
///
/// Check the two path rules first. A `/usr/bin` install finds its models through
/// the second `models::LAYOUTS` entry (`../share/chibipop/models`). The daemon
/// finds its rules in the same relative location. If either path is wrong, the
/// package can still install.
fn assert_installed_tree(pkgdir: &Path) {
    let bin_dir = pkgdir.join("usr/bin");
    // `beside` returns a path that contains `..`. Compare the canonical paths to
    // check where both paths point.
    let found = models::beside(&bin_dir)
        .expect("/usr/bin/chibipop would not find the packaged models at all");
    assert_eq!(
        pkgdir.join("usr/share/chibipop/models/meiki").canonicalize().unwrap(),
        found.canonicalize().unwrap(),
        "/usr/bin/chibipop resolves its models somewhere else: {}",
        found.display()
    );
    assert!(
        pkgdir.join("usr/share/chibipop/data/deconjugator.json").is_file(),
        "no deconjugation rules under ../share/chibipop - every conjugated form would stop resolving"
    );

    for (name, _) in models::ALL {
        let path = pkgdir.join("usr/share/chibipop/models/meiki").join(name);
        // Use real bytes instead of a stub. `models::verify` hashes them when the
        // engine opens, and `package()` checks them as it stages them.
        assert!(path.is_file(), "the package is missing {name}");
    }

    for rel in [
        "usr/bin/chibipop",
        "usr/share/chibipop/models/meiki/SHA256SUMS.txt",
        "usr/share/chibipop/models/meiki/LICENSE.md",
        "usr/share/applications/chibipop.desktop",
        "usr/lib/systemd/user/chibipop.service",
        "usr/share/icons/hicolor/scalable/apps/chibipop.svg",
        "usr/share/doc/chibipop/README.md",
        "usr/share/doc/chibipop/extras/README.md",
        "usr/share/doc/chibipop/extras/hyprland.conf",
    ] {
        assert!(pkgdir.join(rel).is_file(), "the package is missing {rel}");
    }

    // Ship the GPL-3.0-or-later license and the LGPL weights. Without the license,
    // the package violates the terms, and pacman expects the files here.
    let licenses: Vec<PathBuf> = std::fs::read_dir(pkgdir.join("usr/share/licenses"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(1, licenses.len(), "expected one licence directory, got {licenses:?}");
    assert!(licenses[0].join("LICENSE").is_file());
    assert!(licenses[0].join("LICENSE.models.md").is_file());

    // Make only the binary executable. An executable data file can make a package
    // look like malware.
    assert_eq!(0o755, mode(&pkgdir.join("usr/bin/chibipop")));
    assert_eq!(0o644, mode(&pkgdir.join("usr/share/chibipop/data/deconjugator.json")));
    assert_eq!(0o644, mode(&pkgdir.join("usr/lib/systemd/user/chibipop.service")));
}

/// `chibipop-bin` repacks the release asset. Its `$srcdir` is the asset from
/// `scripts/package-linux.sh`. The test checks that the script and the PKGBUILD
/// agree, not an assumption about the tarball.
#[test]
fn the_binary_package_installs_a_tree_the_daemon_can_read() {
    let root = scratch("bin");
    let pkgbuild = aur().join("chibipop-bin/PKGBUILD");
    let version = pkgver(&pkgbuild);

    let binary = root.join("stub/chibipop");
    stub_binary(&binary);
    let srcdir = root.join("src");
    let out = Command::new("bash")
        .arg(repo().join("scripts/package-linux.sh"))
        .arg(format!("v{version}"))
        .arg(&binary)
        .env("OUT", &srcdir)
        .current_dir(repo())
        .output()
        .expect("bash must be able to run the packaging script");
    assert!(
        out.status.success(),
        "packaging failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The script leaves the staged tree beside the tarball. It is byte-identical to
    // the tree that `tar xzf` gives makepkg. `tarball_layout.rs` checks that half.
    // The icon is the second PKGBUILD source.
    std::fs::copy(
        repo().join("crates/chibipop-windows/assets/chibipop.svg"),
        srcdir.join(format!("chibipop-{version}.svg")),
    )
    .unwrap();

    let pkgdir = root.join("pkg");
    run_package(&pkgbuild, &srcdir, &pkgdir);
    assert_installed_tree(&pkgdir);

    std::fs::remove_dir_all(&root).ok();
}

/// The source package builds from a tag archive. This tree stands in for that
/// archive. Link it under the name that GitHub's archive uses. Add the binary
/// that its `build()` would create.
#[test]
fn the_source_package_installs_the_same_tree() {
    let root = scratch("src");
    let pkgbuild = aur().join("chibipop/PKGBUILD");
    let version = pkgver(&pkgbuild);

    let srcdir = root.join("src");
    let tree = srcdir.join(format!("chibipop-{version}"));
    std::fs::create_dir_all(&tree).unwrap();
    for rel in ["data", "extras", "crates", "README.md", "LICENSE"] {
        symlink(repo().join(rel), tree.join(rel)).unwrap();
    }
    stub_binary(&tree.join("target/release/chibipop"));

    let pkgdir = root.join("pkg");
    run_package(&pkgbuild, &srcdir, &pkgdir);
    assert_installed_tree(&pkgdir);

    std::fs::remove_dir_all(&root).ok();
}

fn sha256_of(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Test the edits that `bump.sh` makes for a tag. Run it on template copies.
/// Supply each source from disk so the digests are known and the script makes no
/// network request.
#[test]
fn bump_rewrites_both_templates_for_a_new_tag() {
    let root = scratch("bump");
    let copy = root.join("aur");
    assert!(Command::new("cp")
        .arg("-r")
        .arg(aur())
        .arg(&copy)
        .status()
        .unwrap()
        .success());

    // Use a version that this repository will never carry. A failed run must not
    // look like a valid template.
    let sources = root.join("sources");
    std::fs::create_dir_all(&sources).unwrap();
    let mut files = Vec::new();
    for (i, name) in ["chibipop-v99.98.97-linux-x64.tar.gz", "chibipop-99.98.97.svg", "chibipop-99.98.97.tar.gz"]
        .iter()
        .enumerate()
    {
        let path = sources.join(name);
        std::fs::write(&path, format!("stand-in source {i}\n")).unwrap();
        files.push(path);
    }

    let mut cmd = Command::new("bash");
    cmd.arg(copy.join("bump.sh")).arg("v99.98.97").arg("--no-srcinfo");
    for path in &files {
        cmd.arg("--local").arg(path);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "bump.sh failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Check the digests in source order and the version in both templates. A bump
    // that moves the version but not the checksums is the failure that this script
    // must prevent.
    for (pkg, want) in [
        ("chibipop-bin", vec![sha256_of(&files[0]), sha256_of(&files[1])]),
        ("chibipop", vec![sha256_of(&files[2])]),
    ] {
        let build = copy.join(pkg).join("PKGBUILD");
        let read = |var: &str| {
            String::from_utf8(bash(&format!(
                "source '{}'; printf '%s\\n' \"${{{var}[@]}}\"",
                build.display()
            )))
            .unwrap()
        };
        assert_eq!("99.98.97\n", read("pkgver"), "{pkg} kept its old pkgver");
        assert_eq!("1\n", read("pkgrel"), "{pkg} did not reset pkgrel");
        assert_eq!(
            want.join("\n") + "\n",
            read("sha256sums"),
            "{pkg} carries checksums for other bytes than its sources"
        );
        // The rewrite must leave a template that makepkg can read. The URLs must use
        // the new version.
        let sources = read("source");
        assert!(
            sources.contains("chibipop-v99.98.97-linux-x64.tar.gz")
                || sources.contains("v99.98.97.tar.gz"),
            "{pkg} sources did not follow the version: {sources}"
        );
    }

    std::fs::remove_dir_all(&root).ok();
}

/// Keep the release asset name as a permanent contract
/// (ARCHITECTURE.md#packaging-and-ci). `chibipop-bin` uses that name in its
/// source URL. Reject a version that cannot produce that name. Do not create a
/// PKGBUILD for an asset that no release carries.
#[test]
fn bump_refuses_a_version_that_breaks_the_asset_name() {
    for bad in ["0.9.0", "v0.9", "release-1"] {
        let out = Command::new("bash")
            .arg(aur().join("bump.sh"))
            .arg(bad)
            .arg("--no-srcinfo")
            .output()
            .unwrap();
        assert!(!out.status.success(), "bump.sh accepted the version {bad:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("version must look like"), "unexpected refusal: {stderr}");
    }
}

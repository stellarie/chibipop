//! The AUR packages' install layout, produced by the real PKGBUILDs.
//!
//! `packaging/aur/*/PKGBUILD` decides where a pacman install puts the
//! models and the deconjugation rules; `src/ocr/models.rs` decides where
//! `/usr/bin/chibipop` looks for them. Those are separate files, and the
//! failure between them is the quiet kind: a package that builds, installs
//! and then refuses to OCR because the models are one directory off. A
//! clean-chroot build cannot catch it either - the models are *there*, just
//! not where the binary walks.
//!
//! So this runs each PKGBUILD's own `package()` in plain bash over a stub
//! `$srcdir`, and asks the binary's search where the models are in the
//! `$pkgdir` that comes out. `makepkg` is not needed and is not on a CI
//! runner: `package()` is a shell function over `install`, and `$srcdir`
//! and `$pkgdir` are just variables.
//!
//! The two packages install one layout deliberately, so both are checked
//! against the same assertions.
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

/// A scratch root, per test, cleaned on entry rather than on exit: a
/// failure that leaves the tree behind is a failure you can go and look at.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chibipop-aur-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whatever the template says today. Read from the template rather than
/// pinned here: `bump.sh` moves it, and a test that has to be bumped in
/// the same commit is a test that will be bumped without being read.
fn pkgver(pkgbuild: &Path) -> String {
    let out = bash(&format!("source '{}'; printf %s \"$pkgver\"", pkgbuild.display()));
    String::from_utf8(out).unwrap()
}

/// Run a bash snippet, refusing anything but a clean exit.
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

/// `package()` out of `pkgbuild`, with makepkg's two directories supplied
/// by hand.
fn run_package(pkgbuild: &Path, srcdir: &Path, pkgdir: &Path) {
    bash(&format!(
        "export srcdir='{}' pkgdir='{}'; cd \"$srcdir\"; source '{}'; package",
        srcdir.display(),
        pkgdir.display(),
        pkgbuild.display()
    ));
}

/// Enough of a binary to be packaged: nothing here reads it, and copying
/// the real 62 MB one would only measure `cp`.
fn stub_binary(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"\x7fELF stub binary, not a real one\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Everything both packages promise, asserted against a real `$pkgdir`.
///
/// The two loud ones are first: a `/usr/bin` install resolves its models
/// through `models::LAYOUTS`' second entry (`../share/chibipop/models`),
/// and the daemon's rules search has the same shape one directory over. Get
/// either wrong and the package still installs.
fn assert_installed_tree(pkgdir: &Path) {
    let bin_dir = pkgdir.join("usr/bin");
    // `beside` answers with the layout's own `..` in it, so compare where
    // the two paths actually land.
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
        // Real bytes, not a stub: `models::verify` re-digests these when
        // the engine opens, and `package()` checks them as it stages.
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

    // GPL-3.0-or-later plus the LGPL weights: shipping the models without
    // their licence would be a violation, and pacman looks here for it.
    let licenses: Vec<PathBuf> = std::fs::read_dir(pkgdir.join("usr/share/licenses"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(1, licenses.len(), "expected one licence directory, got {licenses:?}");
    assert!(licenses[0].join("LICENSE").is_file());
    assert!(licenses[0].join("LICENSE.models.md").is_file());

    // Only the binary is executable: a data file arriving with the
    // executable bit is how a package ends up looking like malware.
    assert_eq!(0o755, mode(&pkgdir.join("usr/bin/chibipop")));
    assert_eq!(0o644, mode(&pkgdir.join("usr/share/chibipop/data/deconjugator.json")));
    assert_eq!(0o644, mode(&pkgdir.join("usr/lib/systemd/user/chibipop.service")));
}

/// `chibipop-bin` repacks the release asset, so its `$srcdir` is the real
/// asset: built here by the real packaging script, which is what makes this
/// a test of the two files agreeing rather than of my idea of the tarball.
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
    // The script leaves the staged tree beside the tarball, and it is
    // byte-identical to what `tar xzf` gives makepkg (tarball_layout.rs
    // proves that half); the icon is the PKGBUILD's second source.
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

/// The source package builds from the tag archive, which is this tree - so
/// `$srcdir` is this tree, symlinked in under the name GitHub's archive
/// unpacks to, plus the binary its `build()` would have produced.
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

/// What `bump.sh` is for: a tag's worth of edits nobody should make by
/// hand. Run over a copy of the templates, with every source supplied from
/// disk so the digests are known and nothing touches the network.
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

    // A version this repo will never carry, so a template left behind by a
    // failure cannot be mistaken for a real one.
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

    // The digests, in source order, and the version in both templates:
    // a bump that moves the version but not the checksums is the failure
    // this script exists to make impossible.
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
        // The rewrite must leave a template makepkg can still read, and
        // the URLs must have followed the version.
        let sources = read("source");
        assert!(
            sources.contains("chibipop-v99.98.97-linux-x64.tar.gz")
                || sources.contains("v99.98.97.tar.gz"),
            "{pkg} sources did not follow the version: {sources}"
        );
    }

    std::fs::remove_dir_all(&root).ok();
}

/// The release asset name is a forever contract
/// (ARCHITECTURE.md#packaging-and-ci) and `chibipop-bin`'s source URL is
/// that name, so a version the contract cannot express has to stop here -
/// not produce a PKGBUILD pointing at an asset no release will ever carry.
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

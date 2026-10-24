//! Locate the three meikiocr models and prove that they match the benchmark.
//!
//! Every install bundles the models and has no first-run download path
//! (ARCHITECTURE.md#ocr-engine). A committed digest identifies each file
//! instead of a URL. The engine checks the files once when it opens. The check
//! takes about 30 ms for 44 MB. A mismatch stops the engine. It does not
//! silently use another file. The quality gate values apply only to these
//! exact bytes.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// A file name and its SHA-256 digest. The values come directly from
/// `tools/ocr-bench/models/SHA256SUMS.txt`. The source-AUR package pins the
/// same digests for its download. Upstream stores the models in
/// `rtr46/meiki.text.detect.v0` and `rtr46/meiki.txt.recognition.v0` on
/// Hugging Face. The weights use LGPL-3.0. See `models/meiki/LICENSE.md`.
pub const DETECT: (&str, &str) = (
    "meiki.text.detect.v0.1.960x544.onnx",
    "40b6a016667745cae7d3055929ae3b8b1e7716aac795f5904cd3c2c7c3b8404b",
);
pub const RECOGNISE: (&str, &str) = (
    "meiki.text.rec.v0.960x32.onnx",
    "3e96bc772fbee9717e536a6353032bb944c3382dd2f6960ef4890decda43b000",
);
pub const RECOGNISE_VERTICAL: (&str, &str) = (
    "meiki.text.rec.v0.vertical.32x480.onnx",
    "2c2a83a23bc3b7e6c63962175f507ecc6c5e85cc174f17bdec37d9bbd0bf895a",
);

pub const ALL: [(&str, &str); 3] = [DETECT, RECOGNISE, RECOGNISE_VERTICAL];

/// Override the search below with the directory that holds the three `.onnx` files.
/// This path points to the model directory, not its parent.
pub const DIR_ENV: &str = "CHIBIPOP_MODEL_DIR";

/// Find the bundled model directory.
///
/// Check `$CHIBIPOP_MODEL_DIR` first. Then check each release layout with
/// [`beside`]. A debug build also checks the source tree, so `cargo run` works
/// without an environment variable. A release build does not include that path.
pub fn locate() -> Result<PathBuf> {
    let mut tried: Vec<PathBuf> = Vec::new();

    if let Some(dir) = std::env::var_os(DIR_ENV) {
        let dir = PathBuf::from(dir);
        if has_models(&dir) {
            return Ok(dir);
        }
        tried.push(dir);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin) = exe.parent() {
            if let Some(dir) = beside(bin) {
                return Ok(dir);
            }
            tried.extend(LAYOUTS.iter().map(|rel| bin.join(rel)));
        }
    }

    if cfg!(debug_assertions) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("models/meiki");
        if has_models(&dir) {
            return Ok(dir);
        }
        tried.push(dir);
    }

    bail!(
        "no meikiocr models found; set {DIR_ENV} or install them beside the binary. Looked in: {}",
        tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
    )
}

/// A release supports two layouts relative to the directory that holds the binary:
/// `models/meiki` beside it for the tarball and `chibipop-bin`, and
/// `../share/chibipop/models/meiki` for a distro `/usr/bin` install.
///
/// `scripts/package-linux.sh` creates the first layout. `tests/tarball_layout.rs`
/// checks that the produced asset resolves here. This list and that script
/// define one contract in two places.
pub const LAYOUTS: [&str; 2] = ["models/meiki", "../share/chibipop/models/meiki"];

/// Find the release layout where `bin`, a directory that holds a chibipop
/// binary, keeps its models. Return `None` when neither layout has all models.
///
/// [`locate`] calls this function for the path-only part of the search. The
/// rest uses `current_exe` and the process environment, which tests share and
/// must not change at the same time.
pub fn beside(bin: &Path) -> Option<PathBuf> {
    LAYOUTS.iter().map(|rel| bin.join(rel)).find(|dir| has_models(dir))
}

fn has_models(dir: &Path) -> bool {
    ALL.iter().all(|(name, _)| dir.join(name).is_file())
}

/// Check that every bundled model has the digest that the gate measured.
pub fn verify(dir: &Path) -> Result<()> {
    for (name, want) in ALL {
        let path = dir.join(name);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let got = hex(Sha256::digest(&bytes).as_slice());
        if got != want {
            bail!(
                "{} is not the model this build was tested against: expected sha256 {want}, got {got}",
                path.display()
            );
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Return the model directory that this repository guarantees.
    fn bundled() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("models/meiki")
    }

    #[test]
    fn the_bundled_models_match_their_pinned_digests() {
        verify(&bundled()).expect("bundled models must match the digests the gate was measured on");
    }

    #[test]
    fn a_tampered_model_is_refused() {
        let dir = std::env::temp_dir().join(format!("chibipop-ocr-digest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, _) in ALL {
            std::fs::write(dir.join(name), b"not an onnx file").unwrap();
        }
        let err = verify(&dir).expect_err("a wrong file must not pass");
        assert!(err.to_string().contains("not the model this build was tested against"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}

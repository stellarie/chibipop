//! Where the three meikiocr models live, and proof they are the ones the
//! benchmark measured.
//!
//! ADR-0009: the models ship bundled everywhere and there is no first-run
//! download path, so "which file is this" is answered by a committed digest
//! rather than by a URL. The check runs once when the engine opens - about
//! 30 ms for 44 MB - and a mismatch refuses the engine instead of quietly
//! recognising with something else. The quality gate's numbers are only
//! meaningful for these exact bytes.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// File name plus SHA-256, copied verbatim from
/// `tools/ocr-bench/models/SHA256SUMS.txt` - the same digests ADR-0009
/// pins the source-AUR download to. Upstream:
/// `rtr46/meiki.text.detect.v0` and `rtr46/meiki.txt.recognition.v0` on
/// Hugging Face (weights LGPL-3.0; see `models/meiki/LICENSE.md`).
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

/// Overrides the search below. Points at the directory holding the three
/// `.onnx` files, not at their parent.
pub const DIR_ENV: &str = "CHIBIPOP_MODEL_DIR";

/// The bundled model directory.
///
/// `$CHIBIPOP_MODEL_DIR` first, then the two layouts a release can take:
/// `models/meiki` beside the binary (the tarball and `chibipop-bin`), and
/// `../share/chibipop/models/meiki` relative to it (a distro `/usr/bin`
/// install). A debug build also falls back to the source tree so a
/// `cargo run` on a dev box works without an env var; a release build
/// never carries that path.
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
            for rel in ["models/meiki", "../share/chibipop/models/meiki"] {
                let dir = bin.join(rel);
                if has_models(&dir) {
                    return Ok(dir);
                }
                tried.push(dir);
            }
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

fn has_models(dir: &Path) -> bool {
    ALL.iter().all(|(name, _)| dir.join(name).is_file())
}

/// Every bundled model is byte-for-byte the one the gate measured.
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

    /// The one directory this repo guarantees.
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

    /// What every step of the search order asks. Not the order itself:
    /// that turns on `current_exe` and the process environment, which
    /// tests share and must not fight over.
    #[test]
    fn a_directory_counts_only_when_it_holds_all_three_models() {
        assert!(has_models(&bundled()));
        assert!(!has_models(Path::new("/nonexistent/meiki")));
        assert!(!has_models(&bundled().join("..")), "the parent holds none of them directly");
    }

    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!("000f10ff", hex(&[0x00, 0x0f, 0x10, 0xff]));
    }
}

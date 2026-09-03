use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

use super::MODEL_SHA256;

/// Read one model. Reject bytes that do not match the pinned release digest.
pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let got = hex(Sha256::digest(&bytes).as_slice());
    if got != MODEL_SHA256 {
        bail!(
            "{} is not the model this build was tested against: expected sha256 {}, got {}",
            path.display(),
            MODEL_SHA256,
            got
        );
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn bundled() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("data/ipadic")
    }

    #[test]
    fn the_bundled_model_matches_its_pinned_digest() {
        read(&bundled().join("system.dic")).expect("the bundled model must match its pinned digest");
    }

    #[test]
    fn the_checksum_file_pins_the_bundled_model() {
        let text = std::fs::read_to_string(bundled().join("SHA256SUMS.txt")).unwrap();
        assert!(
            text.lines().any(|line| {
                let mut fields = line.split_whitespace();
                fields.next() == Some(MODEL_SHA256) && fields.next() == Some("system.dic")
            }),
            "SHA256SUMS.txt must list system.dic with MODEL_SHA256"
        );
    }

    #[test]
    fn a_tampered_model_error_names_the_path_and_both_digests() {
        let path = std::env::temp_dir().join(format!(
            "chibipop-analysis-tampered-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"not the bundled model").unwrap();
        let error = read(&path).expect_err("a tampered model must fail its digest check");
        let message = error.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains(MODEL_SHA256), "{message}");
        assert!(message.contains("got "), "{message}");
        std::fs::remove_file(path).ok();
    }
}

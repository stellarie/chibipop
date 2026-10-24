//! Check the protocol version that the plugin handshake selects.
//!
//! The host accepts a version only when the plugin offered it and the manifest
//! declares the same version. This check prevents mismatched protocol contracts.

use anyhow::{bail, Result};

pub fn agree(offered: &[u32], picked: u32, declared: u32) -> Result<u32> {
    if !offered.contains(&picked) {
        bail!("plugin picked protocol {picked}, which was not offered: {offered:?}");
    }
    if picked != declared {
        bail!("manifest declares protocol {declared}, handshake picked {picked}");
    }
    Ok(picked)
}

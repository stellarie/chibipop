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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picked_offer_that_matches_the_manifest_is_agreed() {
        assert_eq!(agree(&[1, 2], 2, 2).unwrap(), 2);
    }

    #[test]
    fn an_old_plugin_may_pick_the_lower_offer() {
        assert_eq!(agree(&[1, 2], 1, 1).unwrap(), 1);
    }

    #[test]
    fn a_pick_outside_the_offer_is_refused() {
        let e = agree(&[1], 2, 2).unwrap_err().to_string();
        assert!(e.contains("not offered"), "{e}");
    }

    #[test]
    fn a_manifest_that_disagrees_names_both_numbers() {
        let e = agree(&[1, 2], 2, 1).unwrap_err().to_string();
        assert!(e.contains('1'), "{e}");
        assert!(e.contains('2'), "{e}");
    }
}

//! Stores the ScreenCast restore token for the fallback capture rung.
//!
//! The portal asks the user for consent.
//! A background daemon cannot request consent at every startup without a dialog each time.
//! The first launch shows one dialog for every monitor.
//! `Start` returns a restore token.
//! Later launches reuse that token to open the same session without a dialog.
//!
//! The portal rotates the token because this module requests `persist_mode=2` ("persist until
//! revoked").
//! With this mode, the portal returns a new token after each successful `Start`.
//! The portal can invalidate the token that the call used.
//! A token saved only after first consent can fail months later.
//! Always save the newest token and replace the previous token.
//! [`TokenStore::rotate`] implements this replacement instead of a create-once save.
//!
//! The file mode is `0600`.
//! A restore token is a capability.
//! Anyone who has it can reopen a screen-capture session as this user without a dialog.
//! Treat it like a session cookie.
//! No other user on the machine can read it.
//!
//! A missing or blank token is normal, not an error.
//! This state occurs on the first launch, after `clear`, or after the user revokes the grant in the
//! portal settings.
//! These cases all need consent again.
//! [`TokenStore::load`] returns `None`.
//! The caller takes the dialog path and avoids a failure that it cannot fix.

use anyhow::{Context, Result};
use std::fs::{OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// File mode for the owner only. The token reopens a capture session without a dialog, so treat it
/// like a session cookie.
const OWNER_ONLY: u32 = 0o600;

/// Stores the newest restore token that the portal issued in the XDG state directory.
///
/// The caller supplies the directory with `Paths::state_dir`.
/// Path discovery stays in `src/paths.rs`. Tests can use a scratch directory and leave the environment
/// unchanged.
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    /// The file name within the state directory.
    pub const FILE: &'static str = "portal-restore-token";

    /// Creates a store for `state_dir`.
    /// This method does not access the file system.
    /// A missing directory represents a first launch, and [`TokenStore::rotate`] creates it.
    pub fn new(state_dir: &Path) -> TokenStore {
        TokenStore { path: state_dir.join(Self::FILE) }
    }

    /// Returns the token path for diagnostics and error messages.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the token for the portal.
    /// Returns `None` when the file does not exist, cannot be read, or contains only whitespace.
    ///
    /// Each case means that no silent session is available.
    /// The caller uses the same consent dialog for each case.
    /// A first launch is not a failure.
    /// The next successful `Start` replaces an unreadable file.
    pub fn load(&self) -> Option<String> {
        // Remove whitespace around the token because this method writes a newline at the end.
        // Also accept a hand-edited file with extra whitespace.
        let stored = std::fs::read_to_string(&self.path).ok()?;
        let token = stored.trim();
        if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        }
    }

    /// Stores the token that the portal issued and replaces the previous token.
    ///
    /// Write the new file before the rename.
    /// A crash in the write then leaves the previous token intact instead of a truncated token.
    /// An old token can still work, but a partial token cannot.
    pub fn rotate(&self, token: &str) -> Result<()> {
        let token = token.trim();
        // Reject a blank token because it cannot come from a valid portal grant.
        // Keep the stored token. Do not replace it with an empty value.
        anyhow::ensure!(
            !token.is_empty(),
            "the portal returned a blank restore token; keeping the one in {}",
            self.path.display()
        );
        let dir = self
            .path
            .parent()
            .with_context(|| format!("{} names no state dir", self.path.display()))?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating the state dir {}", dir.display()))?;

        // Keep the temporary file beside the target.
        // Rename is atomic only within one file system, and the state directory can be its own mount.
        let tmp = dir.join(format!("{}.new", Self::FILE));
        if let Err(e) = write_private(&tmp, token) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| {
                format!("replacing {} with {}", self.path.display(), tmp.display())
            });
        }
        Ok(())
    }

    /// Removes the token so the next launch asks for consent again.
    ///
    /// This method is idempotent.
    /// An absent token already has the requested state, so the method reports success.
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e)
                .with_context(|| format!("forgetting the restore token {}", self.path.display())),
        }
    }
}

/// Writes `token` and a newline to `path` with owner-only permissions.
/// Flushes the file before the caller renames it into place.
fn write_private(path: &Path, token: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(OWNER_ONLY)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    // The mode applies only when this call creates the file.
    // An interrupted rotation can leave a sibling with a different mode.
    // Set the mode again so the token never becomes readable by other users.
    file.set_permissions(Permissions::from_mode(OWNER_ONLY))
        .with_context(|| format!("restricting {} to its owner", path.display()))?;
    file.write_all(token.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    // Sync the file before the rename.
    // Otherwise, a power loss can replace the target with an empty file.
    file.sync_all().with_context(|| format!("flushing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gives each test a scratch directory. The suite can then run in parallel.
    fn scratch(what: &str) -> PathBuf {
        std::env::temp_dir().join(format!("chibipop_portal_token_{}_{what}", std::process::id()))
    }

    /// Creates a ready state directory for tests that do not test its creation.
    fn ready(what: &str) -> (PathBuf, TokenStore) {
        let dir = scratch(what);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch state dir");
        let store = TokenStore::new(&dir);
        (dir, store)
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).expect("the token file").permissions().mode() & 0o777
    }

    #[test]
    fn the_token_sits_in_the_state_dir_under_its_documented_name() {
        let store = TokenStore::new(Path::new("/home/x/.local/state/chibipop"));
        assert_eq!(
            Path::new("/home/x/.local/state/chibipop/portal-restore-token"),
            store.path()
        );
    }

    /// A first launch has no token, and it is not a failure.
    #[test]
    fn an_absent_token_file_reads_as_no_token_rather_than_an_error() {
        let dir = scratch("absent");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(None, TokenStore::new(&dir).load());
    }

    /// Use the dialog path for a truncated or empty file.
    /// Do not send an empty string to the portal.
    #[test]
    fn a_blank_or_whitespace_only_file_reads_as_no_token() {
        let (dir, store) = ready("blank");
        std::fs::write(store.path(), "").unwrap();
        assert_eq!(None, store.load());
        std::fs::write(store.path(), "  \n\t\n").unwrap();
        assert_eq!(None, store.load());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stored_token_loads_back_without_its_surrounding_whitespace() {
        let (dir, store) = ready("trim");
        store.rotate("tok-abc").expect("rotate");
        assert_eq!(Some("tok-abc".to_string()), store.load());
        std::fs::write(store.path(), "\n  tok-xyz \n\n").unwrap();
        assert_eq!(Some("tok-xyz".to_string()), store.load());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `persist_mode=2` needs the newest token to replace the stored token.
    #[test]
    fn rotating_replaces_the_token_that_was_stored_before() {
        let (dir, store) = ready("rotate");
        store.rotate("tok-first").expect("first rotate");
        assert_eq!(Some("tok-first".to_string()), store.load());
        store.rotate("tok-second").expect("second rotate");
        assert_eq!(Some("tok-second".to_string()), store.load());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A first launch can have no state directory.
    #[test]
    fn rotating_creates_the_state_dir_when_it_is_absent() {
        let dir = scratch("mkdir").join("nested");
        let _ = std::fs::remove_dir_all(&dir);
        let store = TokenStore::new(&dir);
        store.rotate("tok-new-dir").expect("rotate must create the state dir");
        assert_eq!(Some("tok-new-dir".to_string()), store.load());
        let _ = std::fs::remove_dir_all(scratch("mkdir"));
    }

    /// The token is a capability. Other users on the machine must not read it.
    #[test]
    fn a_rotated_token_is_readable_only_by_its_owner() {
        let (dir, store) = ready("mode");
        store.rotate("tok-secret").expect("rotate");
        assert_eq!(0o600, mode_of(store.path()));
        // A rotation over a file with loose permissions also sets mode `0600`.
        std::fs::set_permissions(store.path(), Permissions::from_mode(0o644)).unwrap();
        store.rotate("tok-secret-2").expect("second rotate");
        assert_eq!(0o600, mode_of(store.path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reject a blank token. Do not destroy a token that still works.
    #[test]
    fn a_blank_rotation_is_refused_and_keeps_the_stored_token() {
        let (dir, store) = ready("refuse");
        store.rotate("tok-good").expect("rotate");
        for blank in ["", "   ", "\n\t "] {
            let err = store.rotate(blank).expect_err("a blank token must be refused");
            let msg = err.to_string();
            assert!(msg.contains("blank"), "{msg}");
            assert!(msg.contains(TokenStore::FILE), "{msg}");
        }
        assert_eq!(Some("tok-good".to_string()), store.load());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_successful_rotation_leaves_no_temp_file_behind() {
        let (dir, store) = ready("no_temp");
        store.rotate("tok-clean").expect("rotate");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != TokenStore::FILE)
            .collect();
        assert!(leftovers.is_empty(), "stale scratch files: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After revocation, no token remains to replay.
    /// A second clear call must also succeed.
    #[test]
    fn clearing_forgets_the_token_and_is_idempotent() {
        let (dir, store) = ready("clear");
        store.rotate("tok-doomed").expect("rotate");
        store.clear().expect("clear");
        assert_eq!(None, store.load());
        assert!(!store.path().exists());
        store.clear().expect("clearing an absent token is not an error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `clear` method must succeed for an absent token, even when the state directory does not exist.
    #[test]
    fn clearing_before_any_token_was_ever_written_is_not_an_error() {
        let dir = scratch("clear_absent");
        let _ = std::fs::remove_dir_all(&dir);
        TokenStore::new(&dir).clear().expect("an absent token is already forgotten");
    }
}

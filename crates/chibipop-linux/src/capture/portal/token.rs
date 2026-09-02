//! The rotating ScreenCast restore token (the fallback capture rung).
//!
//! The portal asks the user for consent, and consent is the one thing a
//! background daemon cannot re-ask for politely: a dialog on every
//! startup would be the loudest thing chibipop does. So the first launch
//! pays for one dialog covering every monitor, `Start` hands back a
//! restore token, and every later launch replays that token to open the
//! same session silently.
//!
//! It *rotates* because we ask for `persist_mode=2` ("persist until
//! revoked"). Under that mode the portal issues a **fresh** token on
//! every successful `Start` and is free to invalidate the one we just
//! spent, so "store it once at first consent" quietly decays into a
//! dialog months later. The only correct discipline is to write back
//! whatever the newest `Start` returned, replacing what we had — hence
//! [`TokenStore::rotate`] rather than a create-once save.
//!
//! The file is `0600`. A restore token is a capability: anyone holding
//! it can reopen a screen-capture session as this user without a dialog,
//! so it is worth exactly as much as a session cookie and belongs to
//! nobody else on the machine.
//!
//! A missing or blank token is *normal*, never an error. It is what a
//! first launch looks like, what a launch after `clear` looks like, and
//! what a launch after the user revokes the grant in the portal's
//! settings looks like. All three mean the same thing — ask for consent
//! again — so [`TokenStore::load`] answers `None` and lets the caller
//! take the dialog path instead of surfacing a failure it cannot fix.

use anyhow::{Context, Result};
use std::fs::{OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Only the owner: the token reopens a capture session without a
/// dialog, so it is a secret in the same sense a session cookie is.
const OWNER_ONLY: u32 = 0o600;

/// The newest restore token the portal issued, on disk in the XDG state
/// dir.
///
/// The directory is supplied by the caller (`Paths::state_dir`) rather
/// than resolved here: path discovery has one home, `src/paths.rs`, and
/// tests need a scratch dir without touching the environment.
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    /// The file name inside the state dir.
    pub const FILE: &'static str = "portal-restore-token";

    /// Point the store at `state_dir`. Touches no filesystem — a store
    /// for a directory that does not exist yet is exactly a first
    /// launch, and [`TokenStore::rotate`] creates it.
    pub fn new(state_dir: &Path) -> TokenStore {
        TokenStore { path: state_dir.join(Self::FILE) }
    }

    /// Where the token is (or would be), for diagnostics and for the
    /// error messages that name it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The token to hand the portal, or `None` when there is none to
    /// hand it (absent file, unreadable file, blank contents).
    ///
    /// Every one of those is answered `None` rather than an error: they
    /// all mean "no silent session is available", the caller's response
    /// to all three is the same consent dialog, and a first launch is
    /// not a failure. A file we cannot read is also one the next
    /// successful `Start` will simply rotate over.
    pub fn load(&self) -> Option<String> {
        // Trimmed because we write a trailing newline, and because a
        // hand-edited file should not fail with an invisible difference.
        let stored = std::fs::read_to_string(&self.path).ok()?;
        let token = stored.trim();
        if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        }
    }

    /// Rotate: persist the token the portal just issued, replacing any
    /// previous one.
    ///
    /// Atomic by write-then-rename within the state dir, so a crash
    /// mid-write leaves the previous token intact instead of a truncated
    /// one — a half-written token is worse than an old token, because
    /// the old one may still work.
    pub fn rotate(&self, token: &str) -> Result<()> {
        let token = token.trim();
        // A blank token is never something the portal meant to give us,
        // and writing it would destroy a working one to no purpose. Loud
        // and unwritten beats silent and self-inflicted.
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

        // The temp file is a sibling on purpose: rename is only atomic
        // within one filesystem, and the state dir may be a mount of its
        // own.
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

    /// Forget the token, so the next launch prompts again.
    ///
    /// Idempotent: "there is no token" is the state this asks for, so
    /// finding it already true is success, not a race to report.
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e)
                .with_context(|| format!("forgetting the restore token {}", self.path.display())),
        }
    }
}

/// Write `token` (plus a newline) to `path` as an owner-only file, and
/// get it onto the disk before the caller renames it into place.
fn write_private(path: &Path, token: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(OWNER_ONLY)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    // `mode` applies only to a file this call creates, and a killed
    // rotate can leave a sibling behind with whatever mode it had; set
    // it outright so the token is never briefly world-readable.
    file.set_permissions(Permissions::from_mode(OWNER_ONLY))
        .with_context(|| format!("restricting {} to its owner", path.display()))?;
    file.write_all(token.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    // Renaming a file whose contents are still in flight is how an
    // atomic replace becomes an empty token after a power cut.
    file.sync_all().with_context(|| format!("flushing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One scratch dir per test, so the suite stays parallel.
    fn scratch(what: &str) -> PathBuf {
        std::env::temp_dir().join(format!("chibipop_portal_token_{}_{what}", std::process::id()))
    }

    /// The dir pre-created, for the tests that are not about creating it.
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

    /// A first launch has no token, and a first launch is not a failure.
    #[test]
    fn an_absent_token_file_reads_as_no_token_rather_than_an_error() {
        let dir = scratch("absent");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(None, TokenStore::new(&dir).load());
    }

    /// A truncated or hand-emptied file must take the dialog path, not
    /// hand the portal an empty string.
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

    /// The whole point of `persist_mode=2`: the newest token wins.
    #[test]
    fn rotating_replaces_the_token_that_was_stored_before() {
        let (dir, store) = ready("rotate");
        store.rotate("tok-first").expect("first rotate");
        assert_eq!(Some("tok-first".to_string()), store.load());
        store.rotate("tok-second").expect("second rotate");
        assert_eq!(Some("tok-second".to_string()), store.load());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// First launch: the state dir may not exist yet.
    #[test]
    fn rotating_creates_the_state_dir_when_it_is_absent() {
        let dir = scratch("mkdir").join("nested");
        let _ = std::fs::remove_dir_all(&dir);
        let store = TokenStore::new(&dir);
        store.rotate("tok-new-dir").expect("rotate must create the state dir");
        assert_eq!(Some("tok-new-dir".to_string()), store.load());
        let _ = std::fs::remove_dir_all(scratch("mkdir"));
    }

    /// The token is a capability; the rest of the machine does not get it.
    #[test]
    fn a_rotated_token_is_readable_only_by_its_owner() {
        let (dir, store) = ready("mode");
        store.rotate("tok-secret").expect("rotate");
        assert_eq!(0o600, mode_of(store.path()));
        // And a rotation over a loose-moded file tightens it.
        std::fs::set_permissions(store.path(), Permissions::from_mode(0o644)).unwrap();
        store.rotate("tok-secret-2").expect("second rotate");
        assert_eq!(0o600, mode_of(store.path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Refusing loudly beats destroying a token that still works.
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

    /// After a revoke there must be nothing left to replay, and asking
    /// twice must not turn into an error.
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

    /// Even a state dir that was never created must clear cleanly.
    #[test]
    fn clearing_before_any_token_was_ever_written_is_not_an_error() {
        let dir = scratch("clear_absent");
        let _ = std::fs::remove_dir_all(&dir);
        TokenStore::new(&dir).clear().expect("an absent token is already forgotten");
    }
}

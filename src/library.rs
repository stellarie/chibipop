//! The on-disk Dictionary library.
//!
//! `Library` lists archive files in one directory and the manifest that names
//! them. `Library::load` reconciles the manifest with the directory on every
//! read. This keeps the list correct after a manual edit or a crash.
//! Unchanged archives reuse bank-derived roles from a disposable cache. File
//! metadata invalidates the cache, so opening settings does not decompress every
//! frequency bank just to search it again for pitch rows.
//!
//! `Pending` tracks one archive removal and its later rebuild.
//! It moves a removed archive into a quarantine folder without deleting it.
//! If the rebuild fails, `Pending::rollback` restores the archive
//! (ARCHITECTURE.md#dictionary-and-lookup).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The file name of the manifest.
const MANIFEST: &str = "library.json";

/// A versioned cache, separate from the user-editable manifest. A missing or
/// unreadable cache requires inspection, but never prevents library access.
const ROLE_CACHE: &str = ".roles-v1.json";

/// File size and modification time can survive an in-place edit or replacement.
/// Bind cached roles to the file's device, inode, and change time as well.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ArchiveStamp {
    size: u64,
    modified: std::time::SystemTime,
    device: u64,
    inode: u64,
    changed: (i64, i64),
}

impl ArchiveStamp {
    /// Open the file before reading metadata. A cached role must not make an
    /// archive appear readable after its permissions change.
    #[cfg(unix)]
    fn read(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::File::open(path).ok()?.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            size: metadata.len(),
            modified: metadata.modified().ok()?,
            device: metadata.dev(),
            inode: metadata.ino(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }

    /// Standard metadata has no change-time token on other platforms. Inspect
    /// the archive instead of trusting a timestamp that a copy can preserve.
    #[cfg(not(unix))]
    fn read(_path: &Path) -> Option<Self> {
        None
    }
}

/// Keep the inspected roles beside their file identity, not beside a manifest
/// claim. A manual manifest edit therefore cannot assign an archive a role.
#[derive(Serialize, Deserialize)]
struct CachedRoles {
    stamp: ArchiveStamp,
    roles: Roles,
}

/// Reuse inspection results across settings processes. An in-memory cache
/// would disappear each time the settings window closes.
struct RoleCache {
    entries: std::collections::BTreeMap<String, CachedRoles>,
    dirty: bool,
}

impl RoleCache {
    /// Treat cache errors as misses. The archives remain the authority, and a
    /// read-only library must still open.
    fn load(dir: &Path) -> Self {
        Self {
            entries: std::fs::read(dir.join(ROLE_CACHE))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default(),
            dirty: false,
        }
    }

    /// Cache only a complete inspection of a file that stayed unchanged.
    /// A failed metadata walk must not hide a role on later loads.
    fn roles(&mut self, path: &Path, file: &str) -> Roles {
        let stamp = ArchiveStamp::read(path);
        if let Some(stamp) = &stamp {
            if let Some(cached) = self.entries.get(file).filter(|cached| cached.stamp == *stamp) {
                return cached.roles;
            }
        }
        let (roles, complete) = crate::dict::archive::read_roles(path);
        if complete && !roles.is_empty() {
            if let Some(stamp) = stamp.filter(|stamp| ArchiveStamp::read(path).as_ref() == Some(stamp)) {
                self.entries.insert(file.to_string(), CachedRoles { stamp, roles });
                self.dirty = true;
                return roles;
            }
        }
        self.dirty |= self.entries.remove(file).is_some();
        roles
    }

    /// Publish only changed cache data. Unique temporary files keep concurrent
    /// loads from mixing their writes. Cache write failures do not change roles.
    fn save(&mut self, dir: &Path) {
        let before = self.entries.len();
        self.entries.retain(|file, _| dir.join(file).is_file());
        if !self.dirty && self.entries.len() == before {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(&self.entries) else { return };
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = dir.join(format!("{ROLE_CACHE}.{}.{serial}.tmp", std::process::id()));
        let Ok(mut file) = std::fs::File::create_new(&tmp) else { return };
        let written = std::io::Write::write_all(&mut file, &bytes);
        drop(file);
        if written.and_then(|()| std::fs::rename(&tmp, dir.join(ROLE_CACHE))).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// The folder name for a removed archive.
///
/// `Pending::commit` deletes the archive from this folder. The folder is one
/// level below the library root. `reconcile` scans only the top level, so it
/// never lists a quarantined file as a Dictionary.
const QUARANTINE: &str = ".removed";

/// The role that an archive supplies.
///
/// chibipop derives each role from banks inside the archive.
/// A role is not a preference and never comes from the filename.
/// A `term_bank_` member means [`Role::Terms`].
/// A `term_meta_bank_` row tagged `"freq"` means [`Role::Frequency`].
/// A row tagged `"pitch"` means [`Role::Pitch`]
/// (ARCHITECTURE.md#dictionary-and-lookup).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Terms,
    Frequency,
    Pitch,
}

impl Role {
    /// All roles in the order that the settings window shows their sections.
    pub const EVERY: [Role; 3] = [Role::Terms, Role::Frequency, Role::Pitch];
}

/// The set of roles that an archive supplies, in any combination.
///
/// One archive is one library entry, whatever roles it holds.
/// An archive with a term bank and frequency data has one row and two roles.
/// The old single-valued `Kind` type forced that archive to choose one role.
/// It also made a pitch archive claim that it was a term archive.
///
/// An unreadable archive supplies the empty set.
/// The library still lists the entry so the user can remove it later.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<Role>", into = "Vec<Role>")]
pub struct Roles {
    terms: bool,
    frequency: bool,
    pitch: bool,
}

impl Roles {
    /// Return exactly these roles and no others.
    pub fn only(roles: &[Role]) -> Roles {
        roles.iter().copied().collect()
    }

    /// Return true when the archive supplies this role.
    pub fn has(self, role: Role) -> bool {
        match role {
            Role::Terms => self.terms,
            Role::Frequency => self.frequency,
            Role::Pitch => self.pitch,
        }
    }

    /// Record that the archive supplies this role.
    pub fn insert(&mut self, role: Role) {
        match role {
            Role::Terms => self.terms = true,
            Role::Frequency => self.frequency = true,
            Role::Pitch => self.pitch = true,
        }
    }

    /// Return true when the set has no role. This means that the build cannot
    /// read the archive.
    pub fn is_empty(self) -> bool {
        !self.terms && !self.frequency && !self.pitch
    }

    /// Iterate over supplied roles in [`Role::EVERY`] order.
    pub fn iter(self) -> impl Iterator<Item = Role> {
        Role::EVERY.into_iter().filter(move |r| self.has(*r))
    }
}

impl FromIterator<Role> for Roles {
    fn from_iter<I: IntoIterator<Item = Role>>(roles: I) -> Roles {
        let mut set = Roles::default();
        for role in roles {
            set.insert(role);
        }
        set
    }
}

impl From<Vec<Role>> for Roles {
    fn from(roles: Vec<Role>) -> Roles {
        roles.into_iter().collect()
    }
}

impl From<Roles> for Vec<Role> {
    fn from(roles: Roles) -> Vec<Role> {
        roles.iter().collect()
    }
}

/// Return the roles that the archive itself supplies.
///
/// Read the index and bank roles in one archive inspection. Separate frequency
/// and pitch checks would decompress the same metadata banks twice.
/// If this build cannot read the archive's `index.json`, return no roles.
/// The title, stylesheet, and media paths all depend on that file.
/// Without it, the library keeps the entry as a file to remove, not a
/// Dictionary with roles.
pub fn roles_of(archive: &Path) -> Roles {
    crate::dict::archive::read_roles(archive).0
}

/// Describe one archive entry in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub file: String,
    pub name: String,
    /// The library checks this field against the archive's file identity on load.
    /// A manifest from before `Roles` existed has the old single-valued `kind`
    /// field and no `roles` field.
    /// Such a manifest reads as the empty set. `reconcile` corrects the value
    /// before any caller reads it.
    #[serde(default)]
    pub roles: Roles,
}

/// Describe the manifest and its archive entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    pub entries: Vec<Entry>,
}

impl Library {
    /// Load the manifest and reconcile it with the files on disk.
    pub fn load(dir: &Path) -> Result<Library> {
        let path = dir.join(MANIFEST);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut lib: Library = if text.trim().is_empty() {
            Library::default()
        } else {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        };
        lib.reconcile(dir);
        Ok(lib)
    }

    /// Make the entry list match the files on disk.
    fn reconcile(&mut self, dir: &Path) {
        restore_quarantined(dir);
        let mut cache = RoleCache::load(dir);
        self.entries.retain(|e| dir.join(&e.file).exists());
        // Reuse bank-derived roles only while the archive stays unchanged.
        for e in &mut self.entries {
            e.roles = cache.roles(&dir.join(&e.file), &e.file);
        }
        let mut strays: Vec<String> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(std::result::Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| Path::new(n).extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
                .filter(|n| !self.entries.iter().any(|e| &e.file == n))
                .collect(),
            Err(_) => return,
        };
        // `read_dir` does not define an order.
        strays.sort();
        for file in strays {
            let path = dir.join(&file);
            let roles = cache.roles(&path, &file);
            let index = crate::dict::archive::read_index(&path).unwrap_or(Value::Null);
            self.entries.push(Entry { name: title_of(&index, &file), file, roles });
        }
        // Run this call last. The loop above appends stray files after the
        // manifest entries.
        self.collapse_duplicates(dir);
        cache.save(dir);
    }

    /// Collapse byte-identical archives into one entry.
    ///
    /// Two copies of one Dictionary with different names still represent one
    /// Dictionary. If the library keeps both entries, the build creates two
    /// `dict` rows, two stylesheets, two copies of every asset, and two answers
    /// for each headword.
    ///
    /// A user reported this exact bug. `library.json` named one archive, and an
    /// unlisted byte-identical copy sat beside it.
    ///
    /// **The first entry wins.** `reconcile` keeps manifest entries in manifest
    /// order and appends adopted strays after them. A listed archive therefore
    /// beats an unlisted copy. The manifest records the user's choice. The
    /// stray archive is only a file that happens to sit in the directory.
    ///
    /// This function deletes neither archive. Adoption is deliberate, and a
    /// file that the user placed here stays in that location.
    ///
    /// **Cost.** A hash of every archive on every load takes too much
    /// time. [`crate::dict::build::hash_file`] runs at about 400 MiB/s. One
    /// 38 MB Dictionary takes about 95 ms to hash, and a library with twelve
    /// Dictionaries takes seconds. This path runs every time the settings
    /// window opens.
    ///
    /// The function first uses file length as a test with no false negatives.
    /// Byte-identical files have the same length. A library of distinct
    /// Dictionaries therefore hashes **nothing at all**. Only a real length
    /// collision adds the hash cost. That cost hashes two 38 MB files once, for
    /// about 190 ms.
    fn collapse_duplicates(&mut self, dir: &Path) {
        let sizes: Vec<u64> = self
            .entries
            .iter()
            .map(|e| std::fs::metadata(dir.join(&e.file)).map(|m| m.len()).unwrap_or(0))
            .collect();
        // A unique length cannot belong to a duplicate, so the code does not
        // read its bytes. A length of 0 means that stat failed. Keep that entry
        // listed so the user can remove it.
        let contested = |i: usize| {
            sizes[i] > 0 && sizes.iter().enumerate().any(|(j, s)| j != i && *s == sizes[i])
        };
        if !(0..sizes.len()).any(contested) {
            return;
        }
        let mut kept: Vec<Entry> = Vec::with_capacity(self.entries.len());
        let mut hashes: Vec<String> = Vec::new();
        for (i, entry) in std::mem::take(&mut self.entries).into_iter().enumerate() {
            if !contested(i) {
                kept.push(entry);
                continue;
            }
            match crate::dict::build::hash_file(&dir.join(&entry.file)) {
                // This hash matches a kept archive. This entry is not a second
                // Dictionary.
                Ok(hash) if hashes.contains(&hash) => {}
                Ok(hash) => {
                    hashes.push(hash);
                    kept.push(entry);
                }
                // An unreadable archive is not a duplicate. Keep it listed so
                // the user can remove it.
                Err(_) => kept.push(entry),
            }
        }
        self.entries = kept;
    }

    /// Write the manifest as one atomic operation.
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(MANIFEST);
        let mut text = serde_json::to_string_pretty(self)
            .with_context(|| format!("serialising {}", path.display()))?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        // A torn write could lose the whole list. Write to a temporary file
        // first and rename it into place.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// Copy an archive into the library.
    pub fn import(&mut self, dir: &Path, source: &Path) -> Result<Entry> {
        let index = crate::dict::archive::read_index(source)?;
        let file = self.free_name(dir, source);
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let dest = dir.join(&file);
        std::fs::copy(source, &dest)
            .with_context(|| format!("copying {} to {}", source.display(), dest.display()))?;
        let entry = Entry { name: title_of(&index, &file), file, roles: roles_of(&dest) };
        self.entries.push(entry.clone());
        Ok(entry)
    }

    /// Remove an entry from the list without deleting its archive.
    ///
    /// `Pending::commit` deletes the archive later.
    pub fn quarantine(&mut self, dir: &Path, file: &str) -> Result<()> {
        self.entries.retain(|e| e.file != file);
        let from = dir.join(file);
        if !from.exists() {
            return Ok(());
        }
        let held = dir.join(QUARANTINE);
        std::fs::create_dir_all(&held)
            .with_context(|| format!("creating {}", held.display()))?;
        let to = held.join(file);
        std::fs::rename(&from, &to)
            .with_context(|| format!("moving {} to {}", from.display(), to.display()))
    }

    /// Return every readable archive that the build installs as a Dictionary,
    /// in order.
    ///
    /// Return every readable archive, whatever roles it holds. One archive is
    /// one `dict` row because a frequency archive owns its frequency data and
    /// a pitch archive owns its pitch data. Both roles need a Dictionary row to
    /// own their data (ARCHITECTURE.md#dictionary-and-lookup). An archive
    /// without the Terms role adds no term rows, so this function does not
    /// check for that role.
    ///
    /// Leave unreadable archives out. The library still lists them so the user
    /// can remove them, but they provide no data for the build.
    pub fn dict_paths(&self, dir: &Path) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|e| !e.roles.is_empty())
            .map(|e| dir.join(&e.file))
            .collect()
    }

    /// Return Frequency archives in their library order.
    ///
    /// Return archives whose Reported frequencies the build loads and stores.
    /// An archive that also supplies Terms appears in this list and
    /// [`Library::dict_paths`]. It still owns one `dict` row.
    pub fn freq_paths(&self, dir: &Path) -> Vec<PathBuf> {
        self.paths(dir, Role::Frequency)
    }

    /// Return true when the library has no archive.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return paths for archives that supply the selected role.
    fn paths(&self, dir: &Path, role: Role) -> Vec<PathBuf> {
        self.entries.iter().filter(|e| e.roles.has(role)).map(|e| dir.join(&e.file)).collect()
    }

    /// Return a file name that does not overwrite a present file.
    fn free_name(&self, dir: &Path, source: &Path) -> String {
        let base = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dictionary.zip".to_string());
        if !self.taken(dir, &base) {
            return base;
        }
        let as_path = Path::new(&base);
        let stem =
            as_path.file_stem().unwrap_or(as_path.as_os_str()).to_string_lossy().into_owned();
        let ext = as_path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let mut n = 2usize;
        loop {
            let candidate = format!("{stem} ({n}){ext}");
            if !self.taken(dir, &candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Return true when a file or entry already uses this name.
    fn taken(&self, dir: &Path, file: &str) -> bool {
        dir.join(file).exists() || self.entries.iter().any(|e| e.file == file)
    }
}

/// A library change that awaits its build.
#[derive(Debug)]
pub struct Pending {
    dir: PathBuf,
    before: Library,
    added: Vec<String>,
    held: Vec<String>,
}

impl Pending {
    /// No archive has moved yet.
    pub fn new(dir: &Path, before: &Library) -> Pending {
        Pending {
            dir: dir.to_path_buf(),
            before: before.clone(),
            added: Vec::new(),
            held: Vec::new(),
        }
    }

    /// Record an archive copied into the library.
    pub fn added(&mut self, file: String) {
        self.added.push(file);
    }

    /// Record an archive moved into quarantine.
    pub fn held(&mut self, file: String) {
        self.held.push(file);
    }

    /// Delete every archive in quarantine and remove the quarantine folder.
    pub fn commit(&self) -> Result<()> {
        let held = self.dir.join(QUARANTINE);
        for file in &self.held {
            let path = held.join(file);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("deleting {}", path.display())),
            }
        }
        let _ = std::fs::remove_dir(&held);
        Ok(())
    }

    /// Undo a pending change. Restore every held archive and remove every added
    /// archive.
    ///
    /// Continue after an error and return only the first error.
    pub fn rollback(&self) -> Result<()> {
        let held = self.dir.join(QUARANTINE);
        let mut first: Option<anyhow::Error> = None;
        for file in &self.held {
            let from = held.join(file);
            if !from.exists() {
                continue;
            }
            let to = self.dir.join(file);
            if let Err(e) = std::fs::rename(&from, &to) {
                let e = anyhow::Error::new(e).context(format!("restoring {}", to.display()));
                first.get_or_insert(e);
            }
        }
        for file in &self.added {
            let path = self.dir.join(file);
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    let e = anyhow::Error::new(e).context(format!("undoing {}", path.display()));
                    first.get_or_insert(e);
                }
            }
        }
        let _ = std::fs::remove_dir(&held);
        if let Err(e) = self.before.save(&self.dir) {
            first.get_or_insert(e);
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Restore archives left by a killed Apply process.
///
/// A process can die between quarantine and commit and leave archives in the
/// quarantine folder. Move them back before `Library::load` reads the manifest.
fn restore_quarantined(dir: &Path) {
    let held = dir.join(QUARANTINE);
    let Ok(listing) = std::fs::read_dir(&held) else {
        return;
    };
    for entry in listing.filter_map(std::result::Result::ok) {
        let back = dir.join(entry.file_name());
        if !back.exists() {
            let _ = std::fs::rename(entry.path(), &back);
        }
    }
    let _ = std::fs::remove_dir(&held);
}

/// Return the index title. If no title exists, return the file stem.
fn title_of(index: &Value, file: &str) -> String {
    index
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            Path::new(file).file_stem().unwrap_or_default().to_string_lossy().into_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDirGuard(PathBuf);

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan").join(name)
    }

    fn scratch(test_name: &str) -> (PathBuf, TempDirGuard) {
        let dir = std::env::temp_dir()
            .join("chibipop_library_test")
            .join(format!("t_{}_{test_name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (dir.clone(), TempDirGuard(dir))
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let Ok(listing) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut out: Vec<String> = listing
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    fn entry(file: &str, roles: &[Role]) -> Entry {
        Entry { file: file.to_string(), name: file.to_string(), roles: Roles::only(roles) }
    }

    /// Store members without compression so the replacement tests control
    /// archive length instead of depending on compression output.
    fn write_role_archive(path: &Path, banks: &[(&str, &str)]) {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("index.json", options).unwrap();
        zip.write_all(br#"{"title":"CacheFixture"}"#).unwrap();
        for (name, text) in banks {
            zip.start_file(*name, options).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn a_missing_directory_loads_as_an_empty_library() {
        let (dir, _guard) = scratch("missing_dir");
        let lib = Library::load(&dir).unwrap();
        assert!(lib.entries.is_empty());
        assert!(lib.is_empty());
    }

    #[test]
    fn a_directory_without_a_manifest_loads_as_an_empty_library() {
        let (dir, _guard) = scratch("no_manifest");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Library::load(&dir).unwrap().is_empty());
    }

    /// Verify that roles come from archive banks, not the filename. `freq.zip`
    /// carries only a term-meta bank with `"freq"` rows, so it supplies Frequency
    /// and no Terms in one library entry.
    #[test]
    fn an_import_reads_its_roles_from_the_archives_banks() {
        let (dir, _guard) = scratch("freq_roles");
        let mut lib = Library::default();
        let made = lib.import(&dir, &fixture("freq.zip")).unwrap();
        assert_eq!(Roles::only(&[Role::Frequency]), made.roles);
        assert_eq!("freq.zip", made.file);
        assert!(!lib.is_empty());
    }

    #[test]
    fn import_classifies_a_term_archive() {
        let (dir, _guard) = scratch("term_roles");
        let mut lib = Library::default();
        let made = lib.import(&dir, &fixture("terms.zip")).unwrap();
        assert_eq!(Roles::only(&[Role::Terms]), made.roles);
        assert!(dir.join("terms.zip").is_file());
    }

    /// Verify that one archive with two roles produces one entry. The old
    /// single-valued `Kind` type could not express this case.
    #[test]
    fn an_archive_supplying_two_roles_is_one_entry_holding_both() {
        let (dir, _guard) = scratch("both_roles");
        let mut lib = Library::default();
        let made = lib.import(&dir, &fixture("both.zip")).unwrap();
        assert_eq!(Roles::only(&[Role::Terms, Role::Pitch]), made.roles);
        assert_eq!(1, lib.entries.len());
    }

    /// Verify that a pitch-only archive supplies only Pitch.
    /// The old filename heuristic called it a term Dictionary.
    #[test]
    fn a_pitch_only_archive_supplies_pitch_and_neither_other_role() {
        let (dir, _guard) = scratch("pitch_roles");
        let mut lib = Library::default();
        let made = lib.import(&dir, &fixture("pitch.zip")).unwrap();
        assert_eq!(Roles::only(&[Role::Pitch]), made.roles);
    }

    #[test]
    fn import_reads_the_title_from_index_json() {
        let (dir, _guard) = scratch("title");
        let mut lib = Library::default();
        let made = lib.import(&dir, &fixture("terms.zip")).unwrap();
        assert_eq!("FixtureTerms", made.name);
    }

    #[test]
    fn importing_the_same_filename_twice_keeps_both() {
        let (dir, _guard) = scratch("twice");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        let second = lib.import(&dir, &fixture("terms.zip")).unwrap();
        assert_eq!("terms (2).zip", second.file);
        assert_eq!(2, lib.entries.len());
        assert_eq!(names_in(&dir), ["terms (2).zip", "terms.zip"]);
    }

    #[test]
    fn a_corrupt_archive_fails_import_and_changes_nothing() {
        let (dir, _guard) = scratch("corrupt");
        let (src, _src_guard) = scratch("corrupt_src");
        std::fs::create_dir_all(&src).unwrap();
        let bad = src.join("broken.zip");
        std::fs::write(&bad, b"not a zip at all").unwrap();
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        assert!(lib.import(&dir, &bad).is_err());
        assert_eq!(1, lib.entries.len());
        assert_eq!(names_in(&dir), ["terms.zip"]);
    }

    /// Verify that quarantine keeps the file. The file belongs to the user.
    #[test]
    fn quarantine_drops_the_entry_without_deleting_the_file() {
        let (dir, _guard) = scratch("quarantine");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        lib.quarantine(&dir, "terms.zip").unwrap();
        assert!(lib.is_empty());
        assert!(!dir.join("terms.zip").exists());
        assert!(dir.join(QUARANTINE).join("terms.zip").is_file());
    }

    #[test]
    fn quarantine_of_an_entry_whose_file_is_gone_still_removes_it() {
        let (dir, _guard) = scratch("quarantine_gone");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        std::fs::remove_file(dir.join("terms.zip")).unwrap();
        lib.quarantine(&dir, "terms.zip").unwrap();
        assert!(lib.is_empty());
    }

    /// Verify that a quarantined archive is not a top-level `*.zip` file.
    #[test]
    fn a_quarantined_archive_is_not_a_top_level_zip() {
        let (dir, _guard) = scratch("not_top_level");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        lib.quarantine(&dir, "terms.zip").unwrap();
        let tops: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(tops.is_empty(), "got {tops:?}");
    }

    #[test]
    fn commit_deletes_the_quarantine_and_rollback_restores_it() {
        let (dir, _guard) = scratch("commit");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        let mut pending = Pending::new(&dir, &lib);
        lib.quarantine(&dir, "terms.zip").unwrap();
        pending.held("terms.zip".to_string());

        pending.rollback().unwrap();
        assert!(dir.join("terms.zip").is_file(), "a failed rebuild loses nothing");
        assert!(!dir.join(QUARANTINE).exists());
        assert_eq!(1, Library::load(&dir).unwrap().entries.len());

        let mut again = Pending::new(&dir, &lib);
        lib.quarantine(&dir, "terms.zip").unwrap();
        again.held("terms.zip".to_string());
        again.commit().unwrap();
        assert!(!dir.join("terms.zip").exists());
        assert!(!dir.join(QUARANTINE).exists());
    }

    /// Verify that rollback removes an import without a second copy.
    #[test]
    fn rollback_undoes_an_import() {
        let (dir, _guard) = scratch("rollback_add");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("freq.zip")).unwrap();
        let before = lib.clone();
        let mut pending = Pending::new(&dir, &before);
        let added = lib.import(&dir, &fixture("terms.zip")).unwrap();
        pending.added(added.file);

        pending.rollback().unwrap();

        assert_eq!(names_in(&dir), ["freq.zip", "library.json"]);
        assert_eq!(before.entries, Library::load(&dir).unwrap().entries);
    }

    /// Verify that a quarantine left by a killed Apply remains visible to the
    /// library.
    #[test]
    fn a_quarantine_left_by_a_dead_process_is_restored_on_load() {
        let (dir, _guard) = scratch("orphan");
        std::fs::create_dir_all(dir.join(QUARANTINE)).unwrap();
        std::fs::copy(fixture("terms.zip"), dir.join(QUARANTINE).join("terms.zip")).unwrap();

        let lib = Library::load(&dir).unwrap();

        assert_eq!(1, lib.entries.len(), "the archive is back in the library");
        assert!(dir.join("terms.zip").is_file());
        assert!(!dir.join(QUARANTINE).exists());
    }

    #[test]
    fn an_unreadable_archive_is_listed_but_is_not_a_dictionary() {
        let (dir, _guard) = scratch("unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.zip"), b"not a zip at all").unwrap();
        std::fs::copy(fixture("terms.zip"), dir.join("terms.zip")).unwrap();

        let lib = Library::load(&dir).unwrap();

        assert_eq!(2, lib.entries.len(), "it stays visible");
        assert!(lib.entries[0].roles.is_empty(), "got {:?}", lib.entries);
        assert_eq!(vec![dir.join("terms.zip")], lib.dict_paths(&dir));
    }

    /// Verify that the archive on disk takes precedence over a stale `roles` value
    /// in the JSON manifest.
    #[test]
    fn an_entry_that_became_unreadable_is_reclassified_on_load() {
        let (dir, _guard) = scratch("became_bad");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        lib.save(&dir).unwrap();
        assert_eq!(
            Roles::only(&[Role::Terms]), Library::load(&dir).unwrap().entries[0].roles,
        );
        std::fs::write(dir.join("terms.zip"), b"corrupted since").unwrap();

        let loaded = Library::load(&dir).unwrap();

        assert!(loaded.entries[0].roles.is_empty());
    }

    #[test]
    fn the_builders_own_question_is_the_one_roles_of_asks() {
        assert_eq!(Roles::only(&[Role::Terms]), roles_of(&fixture("terms.zip")));
        assert_eq!(Roles::only(&[Role::Frequency]), roles_of(&fixture("freq.zip")));
        assert!(roles_of(Path::new("nope.zip")).is_empty());
    }

    /// The second mode can appear in a later bank. The first frequency row
    /// must not end the shared inspection before it discovers pitch.
    #[test]
    fn one_inspection_finds_terms_and_both_metadata_roles() {
        let (dir, _guard) = scratch("all_roles");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mixed.zip");
        write_role_archive(&path, &[
            ("term_bank_1.json", "[]"),
            ("term_meta_bank_1.json", r#"[["word","freq",1]]"#),
            ("term_meta_bank_2.json", r#"[["word","pitch",{"reading":"word","pitches":[]}]]"#),
        ]);
        assert_eq!(Roles::only(&Role::EVERY), roles_of(&path));
    }

    /// A later metadata error must not erase a role that the independent
    /// readers already accepted. It must still prevent access to later banks.
    #[test]
    fn a_metadata_error_preserves_only_roles_found_before_it() {
        let (dir, _guard) = scratch("partial_roles");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("partial.zip");
        write_role_archive(&path, &[
            ("term_bank_1.json", "[]"),
            ("term_meta_bank_1.json", r#"[["word","freq",1],broken]"#),
            ("term_meta_bank_2.json", r#"[["word","pitch",0]]"#),
        ]);
        let (roles, complete) = crate::dict::archive::read_roles(&path);
        assert_eq!(Roles::only(&[Role::Terms, Role::Frequency]), roles);
        assert!(!complete, "a parser error must not become a cached absence");
    }

    /// Length and modification time can both survive a replacement. Check an
    /// ordinary edit and an edit that restores the previous modification time.
    #[test]
    fn a_same_length_archive_edit_invalidates_cached_roles() {
        let (dir, _guard) = scratch("cached_replacement");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("changed.zip");
        write_role_archive(&path, &[("term_meta_bank_1.json", r#"[["word","freq",10]]"#)]);
        let first_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(first_time).unwrap();
        let length = file.metadata().unwrap().len();
        drop(file);
        let lib = Library::load(&dir).unwrap();
        assert_eq!(Roles::only(&[Role::Frequency]), lib.entries[0].roles);
        lib.save(&dir).unwrap();

        write_role_archive(&path, &[("term_meta_bank_1.json", r#"[["word","pitch",0]]"#)]);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(first_time + std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(length, file.metadata().unwrap().len(), "the replacement has the same length");
        drop(file);

        assert_eq!(Roles::only(&[Role::Pitch]), Library::load(&dir).unwrap().entries[0].roles);

        write_role_archive(&path, &[("term_meta_bank_1.json", r#"[["word","freq",10]]"#)]);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(first_time + std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(length, file.metadata().unwrap().len());
        drop(file);

        assert_eq!(Roles::only(&[Role::Frequency]), Library::load(&dir).unwrap().entries[0].roles);

        let replacement = dir.join("replacement");
        write_role_archive(&replacement, &[("term_meta_bank_1.json", r#"[["word","pitch",0]]"#)]);
        let file = std::fs::File::options().write(true).open(&replacement).unwrap();
        file.set_modified(first_time + std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(length, file.metadata().unwrap().len());
        drop(file);
        std::fs::rename(replacement, &path).unwrap();

        assert_eq!(Roles::only(&[Role::Pitch]), Library::load(&dir).unwrap().entries[0].roles);
    }

    /// The manifest is user-editable. A cache hit must restore the inspected
    /// role instead of trusting a changed manifest claim.
    #[test]
    fn cached_roles_override_a_manual_manifest_role_change() {
        let (dir, _guard) = scratch("cached_manifest");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(fixture("freq.zip"), dir.join("freq.zip")).unwrap();
        let mut lib = Library::load(&dir).unwrap();
        lib.entries[0].roles = Roles::only(&[Role::Terms]);
        lib.save(&dir).unwrap();

        assert_eq!(Roles::only(&[Role::Frequency]), Library::load(&dir).unwrap().entries[0].roles);
    }

    /// The cache is disposable. A bad cache and a failed cache write must
    /// leave the same usable Dictionary list.
    #[test]
    fn cache_read_and_write_failures_do_not_hide_archives() {
        let (dir, _guard) = scratch("cache_failure");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(fixture("terms.zip"), dir.join("terms.zip")).unwrap();
        std::fs::write(dir.join(ROLE_CACHE), b"not json").unwrap();
        let lib = Library::load(&dir).unwrap();
        assert_eq!(vec![dir.join("terms.zip")], lib.dict_paths(&dir));

        std::fs::remove_file(dir.join(ROLE_CACHE)).unwrap();
        std::fs::create_dir(dir.join(ROLE_CACHE)).unwrap();
        let lib = Library::load(&dir).unwrap();
        assert_eq!(vec![dir.join("terms.zip")], lib.dict_paths(&dir));
    }

    #[test]
    fn save_then_load_round_trips_order_and_roles() {
        let (dir, _guard) = scratch("round_trip");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("freq.zip")).unwrap();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        lib.save(&dir).unwrap();
        let loaded = Library::load(&dir).unwrap();
        assert_eq!(lib.entries, loaded.entries);
        assert_eq!(Roles::only(&[Role::Frequency]), loaded.entries[0].roles);
        assert_eq!("FixtureFreq", loaded.entries[0].name);
        assert_eq!("terms.zip", loaded.entries[1].file);
        assert!(!dir.join("library.json.tmp").exists());
    }

    /// Verify that the build list contains every readable archive. An archive
    /// with the Frequency role is a Dictionary and needs its
    /// own `dict` row. An archive with both roles appears in both lists but
    /// still gets one row.
    #[test]
    fn the_build_list_holds_every_readable_archive_and_freq_paths_the_frequency_ones() {
        let dir = Path::new("L");
        let lib = Library {
            entries: vec![
                entry("b.zip", &[Role::Terms]),
                entry("f.zip", &[Role::Frequency]),
                entry("a.zip", &[Role::Terms, Role::Frequency]),
                entry("broken.zip", &[]),
            ],
        };
        assert_eq!(
            vec![dir.join("b.zip"), dir.join("f.zip"), dir.join("a.zip")],
            lib.dict_paths(dir),
            "manifest order, and the unreadable one has nothing to build",
        );
        assert_eq!(vec![dir.join("f.zip"), dir.join("a.zip")], lib.freq_paths(dir));
    }

    #[test]
    fn a_zip_the_manifest_never_listed_is_adopted_on_load() {
        let (dir, _guard) = scratch("adopt");
        std::fs::create_dir_all(&dir).unwrap();
        // The build writes these files without a manifest entry.
        std::fs::copy(fixture("terms.zip"), dir.join("zz_dropped_in.zip")).unwrap();
        std::fs::copy(fixture("freq.zip"), dir.join("aa_dropped_in.zip")).unwrap();
        Library::default().save(&dir).unwrap();

        let lib = Library::load(&dir).unwrap();

        assert_eq!(2, lib.entries.len(), "both strays adopted");
        assert_eq!("aa_dropped_in.zip", lib.entries[0].file, "sorted, not read_dir order");
        assert_eq!(Roles::only(&[Role::Frequency]), lib.entries[0].roles, "read on adoption");
        assert_eq!("FixtureTerms", lib.entries[1].name, "title read, not the stem");
    }

    #[test]
    fn an_entry_whose_file_vanished_is_dropped_on_load() {
        let (dir, _guard) = scratch("vanished");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        lib.save(&dir).unwrap();
        std::fs::remove_file(dir.join("terms.zip")).unwrap();

        let loaded = Library::load(&dir).unwrap();

        assert!(loaded.is_empty(), "a listed file that is gone is not offered");
    }

    #[test]
    fn a_non_zip_in_the_folder_is_not_adopted() {
        let (dir, _guard) = scratch("non_zip");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), b"not an archive").unwrap();
        Library::default().save(&dir).unwrap();

        assert!(Library::load(&dir).unwrap().is_empty());
    }

    // ---- one Dictionary under two names ----

    /// Verify the reported bug shape. `library.json` names one archive, and a
    /// byte-identical copy sits unlisted beside it. Adoption turns the copy
    /// into an entry, and the build then creates a second Dictionary: two
    /// `dict` rows, two stylesheets, and every headword gets two answers.
    ///
    /// The test runs a real build instead of a path count because "one
    /// dictionary" is a claim about the database. `dict_paths` only supplies
    /// paths to the builder for the rebuild.
    #[test]
    fn two_byte_identical_archives_under_different_names_are_one_dictionary() {
        let (dir, _guard) = scratch("identical_copy");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        lib.save(&dir).unwrap();
        // This copy is unlisted beside the listed archive.
        std::fs::copy(fixture("terms.zip"), dir.join("[JA-EN] terms (2026-08-11).zip")).unwrap();

        let loaded = Library::load(&dir).unwrap();

        assert_eq!(1, loaded.entries.len(), "got {:?}", loaded.entries);
        assert_eq!(
            "terms.zip", loaded.entries[0].file,
            "the manifest's own entry wins over an unlisted copy of it",
        );
        assert_eq!(vec![dir.join("terms.zip")], loaded.dict_paths(&dir));

        let out = dir.join("built.sqlite");
        crate::dict::build::build(&loaded.dict_paths(&dir), &[], &out, &|_| {}).unwrap();
        let conn = rusqlite::Connection::open(&out).unwrap();
        let dicts: i64 = conn.query_row("SELECT COUNT(*) FROM dict", [], |r| r.get(0)).unwrap();
        assert_eq!(1, dicts, "one archive, however many names it wears");
    }

    /// Verify that duplicate collapse deletes no file. Adoption is deliberate,
    /// and the file belongs to the user. Only the second *entry* disappears,
    /// and the list stays collapsed on every later load.
    #[test]
    fn collapsing_a_duplicate_leaves_the_file_where_the_user_put_it() {
        let (dir, _guard) = scratch("keeps_the_file");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        lib.save(&dir).unwrap();
        std::fs::copy(fixture("terms.zip"), dir.join("copy.zip")).unwrap();

        let loaded = Library::load(&dir).unwrap();
        loaded.save(&dir).unwrap();

        assert!(dir.join("copy.zip").is_file(), "the archive itself is untouched");
        assert_eq!(1, Library::load(&dir).unwrap().entries.len(), "and stays collapsed");
    }

    /// Verify that the hash, not the length, makes the final decision.
    /// Two archives can have the same length and still contain different
    /// Dictionaries. The length only decides whether the code reads their
    /// bytes.
    #[test]
    fn two_equally_long_archives_with_different_contents_stay_two_dictionaries() {
        let (dir, _guard) = scratch("same_size");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.zip");
        let b = dir.join("b.zip");
        std::fs::copy(fixture("terms.zip"), &a).unwrap();
        std::fs::copy(fixture("terms.zip"), &b).unwrap();
        // Flip one byte inside the zip comment. Every reader ignores this part.
        // The length stays the same, but sha256 differs, and both archives
        // remain readable.
        let mut bytes = std::fs::read(&b).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&b, &bytes).unwrap();
        assert_eq!(
            std::fs::metadata(&a).unwrap().len(),
            std::fs::metadata(&b).unwrap().len(),
            "the gate has to actually be contested",
        );
        Library::default().save(&dir).unwrap();

        let loaded = Library::load(&dir).unwrap();

        assert_eq!(2, loaded.entries.len(), "got {:?}", loaded.entries);
    }
}

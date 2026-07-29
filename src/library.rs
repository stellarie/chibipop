//! The archive library.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The manifest file name.
const MANIFEST: &str = "library.json";

/// What an archive supplies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Term,
    Frequency,
}

/// One archive in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub file: String,
    pub name: String,
    pub kind: Kind,
}

/// The manifest and its files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    pub entries: Vec<Entry>,
}

impl Library {
    /// Manifest, reconciled to disk.
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

    /// Makes the list match disk.
    fn reconcile(&mut self, dir: &Path) {
        self.entries.retain(|e| dir.join(&e.file).exists());
        let mut strays: Vec<String> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(std::result::Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| Path::new(n).extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
                .filter(|n| !self.entries.iter().any(|e| &e.file == n))
                .collect(),
            Err(_) => return,
        };
        // read_dir order is not defined.
        strays.sort();
        for file in strays {
            let path = dir.join(&file);
            let kind = if crate::dict::archive::is_frequency_archive(&path) {
                Kind::Frequency
            } else {
                Kind::Term
            };
            let index = crate::dict::archive::read_index(&path).unwrap_or(Value::Null);
            self.entries.push(Entry { name: title_of(&index, &file), file, kind });
        }
    }

    /// Atomic manifest write.
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(MANIFEST);
        let mut text = serde_json::to_string_pretty(self)
            .with_context(|| format!("serialising {}", path.display()))?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        // Torn write loses the list.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// Copies an archive in.
    pub fn import(&mut self, dir: &Path, source: &Path) -> Result<Entry> {
        let index = crate::dict::archive::read_index(source)?;
        let file = self.free_name(dir, source);
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let dest = dir.join(&file);
        std::fs::copy(source, &dest)
            .with_context(|| format!("copying {} to {}", source.display(), dest.display()))?;
        let kind = if crate::dict::archive::is_frequency_archive(&dest) {
            Kind::Frequency
        } else {
            Kind::Term
        };
        let entry = Entry { name: title_of(&index, &file), file, kind };
        self.entries.push(entry.clone());
        Ok(entry)
    }

    /// Drops an entry and its file.
    pub fn remove(&mut self, dir: &Path, file: &str) -> Result<()> {
        self.entries.retain(|e| e.file != file);
        let path = dir.join(file);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("deleting {}", path.display())),
        }
    }

    /// Term archives, in order.
    pub fn term_paths(&self, dir: &Path) -> Vec<PathBuf> {
        self.paths(dir, Kind::Term)
    }

    /// Frequency archives, in order.
    pub fn freq_paths(&self, dir: &Path) -> Vec<PathBuf> {
        self.paths(dir, Kind::Frequency)
    }

    /// No archives recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Paths of one kind.
    fn paths(&self, dir: &Path, kind: Kind) -> Vec<PathBuf> {
        self.entries.iter().filter(|e| e.kind == kind).map(|e| dir.join(&e.file)).collect()
    }

    /// A name that won't clobber.
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

    /// Is this filename in use?
    fn taken(&self, dir: &Path, file: &str) -> bool {
        dir.join(file).exists() || self.entries.iter().any(|e| e.file == file)
    }
}

/// The index title, or the stem.
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

    fn entry(file: &str, kind: Kind) -> Entry {
        Entry { file: file.to_string(), name: file.to_string(), kind }
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

    #[test]
    fn import_classifies_a_frequency_archive_by_its_index() {
        let (dir, _guard) = scratch("freq_kind");
        let mut lib = Library::default();
        let made = lib.import(&dir, &fixture("freq.zip")).unwrap();
        assert_eq!(Kind::Frequency, made.kind);
        assert_eq!("freq.zip", made.file);
        assert!(!lib.is_empty());
    }

    #[test]
    fn import_classifies_a_term_archive() {
        let (dir, _guard) = scratch("term_kind");
        let mut lib = Library::default();
        let made = lib.import(&dir, &fixture("terms.zip")).unwrap();
        assert_eq!(Kind::Term, made.kind);
        assert!(dir.join("terms.zip").is_file());
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

    #[test]
    fn remove_deletes_the_file_and_the_entry() {
        let (dir, _guard) = scratch("remove");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        lib.remove(&dir, "terms.zip").unwrap();
        assert!(lib.is_empty());
        assert!(!dir.join("terms.zip").exists());
    }

    #[test]
    fn remove_of_an_entry_whose_file_is_gone_still_removes_it() {
        let (dir, _guard) = scratch("remove_gone");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        std::fs::remove_file(dir.join("terms.zip")).unwrap();
        lib.remove(&dir, "terms.zip").unwrap();
        assert!(lib.is_empty());
    }

    #[test]
    fn save_then_load_round_trips_order_and_kind() {
        let (dir, _guard) = scratch("round_trip");
        let mut lib = Library::default();
        lib.import(&dir, &fixture("freq.zip")).unwrap();
        lib.import(&dir, &fixture("terms.zip")).unwrap();
        lib.save(&dir).unwrap();
        let loaded = Library::load(&dir).unwrap();
        assert_eq!(lib.entries, loaded.entries);
        assert_eq!(Kind::Frequency, loaded.entries[0].kind);
        assert_eq!("FixtureFreq", loaded.entries[0].name);
        assert_eq!("terms.zip", loaded.entries[1].file);
        assert!(!dir.join("library.json.tmp").exists());
    }

    #[test]
    fn term_and_freq_paths_split_by_kind_in_manifest_order() {
        let dir = Path::new("L");
        let lib = Library {
            entries: vec![
                entry("b.zip", Kind::Term),
                entry("f.zip", Kind::Frequency),
                entry("a.zip", Kind::Term),
            ],
        };
        assert_eq!(vec![dir.join("b.zip"), dir.join("a.zip")], lib.term_paths(dir));
        assert_eq!(vec![dir.join("f.zip")], lib.freq_paths(dir));
    }

    #[test]
    fn a_zip_the_manifest_never_listed_is_adopted_on_load() {
        let (dir, _guard) = scratch("adopt");
        std::fs::create_dir_all(&dir).unwrap();
        // Built regardless of manifest.
        std::fs::copy(fixture("terms.zip"), dir.join("zz_dropped_in.zip")).unwrap();
        std::fs::copy(fixture("freq.zip"), dir.join("aa_dropped_in.zip")).unwrap();
        Library::default().save(&dir).unwrap();

        let lib = Library::load(&dir).unwrap();

        assert_eq!(2, lib.entries.len(), "both strays adopted");
        assert_eq!("aa_dropped_in.zip", lib.entries[0].file, "sorted, not read_dir order");
        assert_eq!(Kind::Frequency, lib.entries[0].kind, "classified on adoption");
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
}

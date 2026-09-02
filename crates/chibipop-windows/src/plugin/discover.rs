//! Finds plugins under a root directory.

use crate::plugin::manifest::{self, Manifest};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn discover(root: &Path) -> Vec<(PathBuf, Result<Manifest>)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let toml = dir.join("plugin.toml");
        if !toml.is_file() {
            continue;
        }
        let parsed = std::fs::read_to_string(&toml)
            .map_err(anyhow::Error::from)
            .and_then(|t| manifest::parse(&t));
        found.push((dir, parsed));
    }
    found
}

/// Returns the names of discovered text providers.
pub fn text_provider_names(found: &[(PathBuf, Result<Manifest>)]) -> Vec<String> {
    let mut names = Vec::new();
    for (_, parsed) in found {
        let Ok(manifest) = parsed else {
            continue;
        };
        if manifest.roles.contains(&manifest::Role::TextProvider) && !names.contains(&manifest.name)
        {
            names.push(manifest.name.clone());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("plugin.toml"), body).unwrap();
    }

    const GOOD: &str = r#"
name = "alpha"
version = "0.1.0"
protocol = 1
command = "cmd"
roles = ["text-provider"]
[text_provider]
provides_geometry = true
"#;

    #[test]
    fn a_missing_root_yields_nothing() {
        assert!(discover(Path::new("no/such/dir/here")).is_empty());
    }

    #[test]
    fn a_broken_manifest_is_listed_with_its_error() {
        let tmp = std::env::temp_dir().join("chibipop-discover-broken");
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, "alpha", GOOD);
        write(&tmp, "beta", "name = \"beta\"\n");
        let found = discover(&tmp);
        assert_eq!(found.len(), 2);
        assert!(found[0].1.is_ok(), "alpha should parse");
        assert!(found[1].1.is_err(), "beta should fail");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_directory_without_a_manifest_is_skipped() {
        let tmp = std::env::temp_dir().join("chibipop-discover-skip");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("notaplugin")).unwrap();
        write(&tmp, "alpha", GOOD);
        let found = discover(&tmp);
        assert_eq!(found.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_discovered_text_provider_is_returned() {
        let manifest = manifest::parse(GOOD).unwrap();
        let found = vec![(PathBuf::from("alpha"), Ok(manifest))];
        assert_eq!(vec!["alpha".to_string()], text_provider_names(&found));
    }

    #[test]
    fn a_field_contributor_is_not_returned() {
        let body = r#"
name = "scorer"
version = "0.1.0"
protocol = 1
command = "cmd"
roles = ["field-contributor"]
[field_contributor]
provides = ["frequency"]
"#;
        let manifest = manifest::parse(body).unwrap();
        let found = vec![(PathBuf::from("scorer"), Ok(manifest))];
        assert!(text_provider_names(&found).is_empty());
    }

    #[test]
    fn a_refused_manifest_is_not_returned() {
        let found = vec![(PathBuf::from("beta"), Err(anyhow::anyhow!("bad manifest")))];
        assert!(text_provider_names(&found).is_empty());
    }

    #[test]
    fn duplicate_names_keep_the_first_occurrence() {
        let first = manifest::parse(GOOD).unwrap();
        let second = manifest::parse(GOOD).unwrap();
        let found = vec![
            (PathBuf::from("first"), Ok(first)),
            (PathBuf::from("second"), Ok(second)),
        ];
        assert_eq!(vec!["alpha".to_string()], text_provider_names(&found));
    }
}

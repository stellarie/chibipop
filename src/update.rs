//! GitHub release auto-updater.

use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const REPO: &str = "stellarie/chibipop";
const ASSET_PREFIX: &str = "chibipop-v";
const ASSET_SUFFIX: &str = "-windows-x64.zip";

/// A matched GitHub release.
pub struct Release {
    pub tag: String,
    pub asset_url: String,
    pub asset_name: String,
}

/// The newer release, if any.
pub fn check(current: &str) -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut resp = ureq::get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "chibipop")
        .config()
        .timeout_global(Some(TIMEOUT))
        .build()
        .call()
        .context("reaching GitHub")?;
    let json: serde_json::Value = resp
        .body_mut()
        .read_json()
        .context("reading release JSON")?;

    let tag = json.get("tag_name")
        .and_then(|t| t.as_str())
        .context("no tag_name")?
        .to_string();
    let remote = tag.strip_prefix('v').unwrap_or(&tag);

    if !is_newer(remote, current) {
        return Ok(None);
    }

    let assets = json.get("assets")
        .and_then(|a| a.as_array())
        .context("no assets array")?;
    let asset = assets.iter().find(|a| {
        a.get("name")
            .and_then(|n| n.as_str())
            .is_some_and(|n| n.starts_with(ASSET_PREFIX) && n.ends_with(ASSET_SUFFIX))
    }).context("no matching asset in release")?;

    let asset_url = asset.get("browser_download_url")
        .and_then(|u| u.as_str())
        .context("no download URL")?
        .to_string();
    let asset_name = asset.get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("chibipop.zip")
        .to_string();

    Ok(Some(Release { tag, asset_url, asset_name }))
}

/// Downloads and swaps the exe.
pub fn download_and_replace(release: &Release) -> Result<()> {
    let exe = std::env::current_exe().context("locating chibipop.exe")?;
    let dir = exe.parent().context("exe has no parent")?;
    let zip_path = dir.join("chibipop.update.zip");
    let new_exe = dir.join("chibipop.new.exe");
    let old_exe = dir.join("chibipop.old");

    let resp = ureq::get(&release.asset_url)
        .header("User-Agent", "chibipop")
        .config()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .call()
        .context("downloading release")?;

    let mut body = resp.into_body();
    let mut file = std::fs::File::create(&zip_path)
        .context("creating update zip")?;
    std::io::copy(&mut body.as_reader(), &mut file)
        .context("writing update zip")?;
    drop(file);

    extract_exe(&zip_path, &new_exe)?;
    let _ = std::fs::remove_file(&zip_path);

    std::fs::rename(&exe, &old_exe)
        .context("renaming current exe to .old")?;
    if let Err(e) = std::fs::rename(&new_exe, &exe) {
        let _ = std::fs::rename(&old_exe, &exe);
        return Err(e).context("replacing exe");
    }

    Ok(())
}

/// Extracts the exe from zip.
fn extract_exe(zip_path: &Path, out: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)
        .context("opening update zip")?;
    let mut archive = zip::ZipArchive::new(file)
        .context("reading update zip")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if name.ends_with("chibipop.exe") {
            let mut out_file = std::fs::File::create(out)
                .context("creating new exe")?;
            std::io::copy(&mut entry, &mut out_file)
                .context("extracting exe")?;
            return Ok(());
        }
    }
    anyhow::bail!("no chibipop.exe found in the release zip")
}

/// Removes a stale .old exe.
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let old = exe.with_file_name("chibipop.old");
        let _ = std::fs::remove_file(old);
    }
}

/// True if remote > current.
fn is_newer(remote: &str, current: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let mut parts = s.split('.');
        let ma = parts.next()?.parse().ok()?;
        let mi = parts.next()?.parse().ok()?;
        let pa = parts.next()?.parse().ok()?;
        Some((ma, mi, pa))
    };
    match (parse(remote), parse(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_version_is_detected() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.1", "0.1.0"));
    }

    #[test]
    fn same_or_older_is_not_newer() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn garbage_is_not_newer() {
        assert!(!is_newer("abc", "0.1.0"));
        assert!(!is_newer("0.1.0", "abc"));
    }
}

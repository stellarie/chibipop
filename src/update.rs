//! The GitHub release check, and the Windows-only self-update.
//!
//! Both platforms read the same `releases/latest` endpoint and match the
//! same asset naming contract — one suffix per platform, both a forever
//! contract (`docs/RELEASING.md`).
//!
//! Only Windows *writes*. The `.new`/`.old` exe swap below is
//! `#[cfg(windows)]`, so on Linux there is no path that replaces the
//! installed binary: a pacman-owned `/usr/bin/chibipop` must never
//! self-modify, and the settings button there reports the newer version
//! and stops (ADR-0007). That is a compile-time property rather than a
//! runtime flag — on Linux the swap is not built, so it cannot be
//! reached, wired up, or accidentally re-enabled.

use anyhow::{Context, Result};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const REPO: &str = "stellarie/chibipop";
const ASSET_PREFIX: &str = "chibipop-v";

/// Which platform's asset a release check is looking for.
///
/// A release carries one asset per platform and the names differ only in
/// their tail, so the tail is the whole decision. The prefix and both
/// suffixes are a forever contract: every shipped binary — including the
/// ones already installed — parses them off `releases/latest`, so this
/// matcher has to keep reading names in both shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
}

impl Platform {
    /// The platform this binary was built for.
    pub const HOST: Platform = if cfg!(windows) { Platform::Windows } else { Platform::Linux };

    /// The release asset suffix for this platform.
    pub const fn asset_suffix(self) -> &'static str {
        match self {
            Platform::Windows => "-windows-x64.zip",
            Platform::Linux => "-linux-x64.tar.gz",
        }
    }

    /// True when `name` is this platform's release asset.
    pub fn owns_asset(self, name: &str) -> bool {
        name.starts_with(ASSET_PREFIX) && name.ends_with(self.asset_suffix())
    }
}

/// A release newer than this build, as `releases/latest` describes it.
///
/// The asset is optional because the version news is worth having
/// without it: a release that carries no asset for this platform — one
/// cut before the platform existed, or one whose upload half-failed —
/// is still a release the user wants to hear about. Only *downloading*
/// needs the asset, which is why [`Release`] and not this type is what
/// the Windows swap takes.
#[derive(Debug)]
pub struct News {
    pub tag: String,
    pub asset: Option<Asset>,
}

/// One release asset: what to fetch, and what to name to a user who
/// will fetch it themselves.
#[derive(Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

/// A newer release with a downloadable asset — the swap's input, and so
/// Windows-only like the swap. On Linux this type does not exist, which
/// is the tidy end of the same guarantee: nothing there can be holding
/// a URL it is expected to write to disk.
#[cfg(windows)]
#[derive(Debug)]
pub struct Release {
    pub tag: String,
    pub asset_url: String,
    pub asset_name: String,
}

/// The newer release for this build's platform, if any, and only if it
/// can be downloaded.
#[cfg(windows)]
pub fn check(current: &str) -> Result<Option<Release>> {
    match news(current)? {
        Some(news) => Ok(Some(downloadable(news)?)),
        None => Ok(None),
    }
}

/// The swap's input. Version news is not enough for it: without an
/// asset there is nothing to fetch, which is a failed update check
/// rather than a report.
#[cfg(windows)]
fn downloadable(news: News) -> Result<Release> {
    let asset = news.asset.context("no matching asset in release")?;
    Ok(Release { tag: news.tag, asset_url: asset.url, asset_name: asset.name })
}

/// The newer release for this build's platform, asset or not.
pub fn news(current: &str) -> Result<Option<News>> {
    news_at(&format!("https://api.github.com/repos/{REPO}/releases/latest"), current)
}

/// [`news`] against a named endpoint.
///
/// The URL is an argument so a test can point the whole path — request,
/// payload, matcher — at a local server; the shipped call sites pin the
/// repo.
pub fn news_at(api_url: &str, current: &str) -> Result<Option<News>> {
    let mut resp = ureq::get(api_url)
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

    latest(&json, current, Platform::HOST)
}

/// The release a `releases/latest` payload describes, if it is newer
/// than `current`, with `platform`'s asset if it has one.
fn latest(
    json: &serde_json::Value,
    current: &str,
    platform: Platform,
) -> Result<Option<News>> {
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
    let matched = assets
        .iter()
        .filter_map(|a| Some((a.get("name")?.as_str()?, a)))
        .find(|(name, _)| platform.owns_asset(name));
    // A matched name with no URL is a broken payload, not a release
    // without an asset: say so rather than quietly reporting less.
    let asset = match matched {
        Some((name, a)) => Some(Asset {
            name: name.to_string(),
            url: a.get("browser_download_url")
                .and_then(|u| u.as_str())
                .context("no download URL")?
                .to_string(),
        }),
        None => None,
    };

    Ok(Some(News { tag, asset }))
}

/// Downloads and swaps the exe. Windows only, forever: see the module
/// note.
#[cfg(windows)]
pub fn download_and_replace(release: &Release) -> Result<()> {
    const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

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
#[cfg(windows)]
fn extract_exe(zip_path: &std::path::Path, out: &std::path::Path) -> Result<()> {
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
#[cfg(windows)]
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

    /// A `releases/latest` payload shaped like GitHub's, carrying both
    /// platforms' assets plus the files a real release also holds.
    fn payload(tag: &str) -> serde_json::Value {
        let asset = |name: String| {
            serde_json::json!({
                "name": name,
                "browser_download_url":
                    format!("https://example.invalid/{tag}/{name}"),
            })
        };
        serde_json::json!({
            "tag_name": tag,
            "assets": [
                asset(format!("chibipop-{tag}-linux-x64.tar.gz")),
                asset(format!("chibipop-{tag}-windows-x64.zip")),
                asset("SHA256SUMS.txt".to_string()),
            ],
        })
    }

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

    #[test]
    fn each_platform_owns_its_own_asset() {
        let zip = "chibipop-v0.9.0-windows-x64.zip";
        let tarball = "chibipop-v0.9.0-linux-x64.tar.gz";

        assert!(Platform::Windows.owns_asset(zip));
        assert!(!Platform::Windows.owns_asset(tarball));
        assert!(Platform::Linux.owns_asset(tarball));
        assert!(!Platform::Linux.owns_asset(zip));
    }

    #[test]
    fn foreign_release_files_are_not_assets() {
        for name in [
            "SHA256SUMS.txt",
            // Unversioned, and a second architecture: both are the
            // shape the contract exists to keep out.
            "chibipop-latest-linux-x64.tar.gz",
            "chibipop-v0.9.0-linux-arm64.tar.gz",
            // A detached signature is not the asset it signs.
            "chibipop-v0.9.0-linux-x64.tar.gz.sig",
            "chibipop-v0.9.0-windows-x64.zip.sig",
            // The source archives GitHub attaches to every release.
            "v0.9.0.tar.gz",
        ] {
            assert!(!Platform::Linux.owns_asset(name), "{name}");
            assert!(!Platform::Windows.owns_asset(name), "{name}");
        }
    }

    #[test]
    fn one_release_answers_both_platforms() {
        let json = payload("v0.9.0");

        let win = latest(&json, "0.8.2", Platform::Windows).unwrap().unwrap();
        let win = win.asset.expect("the release carries the zip");
        assert_eq!("chibipop-v0.9.0-windows-x64.zip", win.name);
        assert!(win.url.ends_with("chibipop-v0.9.0-windows-x64.zip"), "{}", win.url);

        let lin = latest(&json, "0.8.2", Platform::Linux).unwrap().unwrap();
        assert_eq!("v0.9.0", lin.tag);
        let lin = lin.asset.expect("the release carries the tarball");
        assert_eq!("chibipop-v0.9.0-linux-x64.tar.gz", lin.name);
        assert!(lin.url.ends_with("chibipop-v0.9.0-linux-x64.tar.gz"), "{}", lin.url);
    }

    #[test]
    fn the_current_version_is_not_an_update() {
        let json = payload("v0.9.0");
        assert!(latest(&json, "0.9.0", Platform::Linux).unwrap().is_none());
        assert!(latest(&json, "0.9.1", Platform::Windows).unwrap().is_none());
    }

    /// A release from before this platform shipped still carries the one
    /// fact worth reporting - which version it is.
    #[test]
    fn a_release_without_this_platforms_asset_still_reports_the_version() {
        let json = serde_json::json!({
            "tag_name": "v0.9.3",
            "assets": [{
                "name": "chibipop-v0.9.3-windows-x64.zip",
                "browser_download_url": "https://example.invalid/zip",
            }],
        });
        let news = latest(&json, "0.8.2", Platform::Linux).unwrap().unwrap();
        assert_eq!("v0.9.3", news.tag);
        assert!(news.asset.is_none(), "{:?}", news.asset);
    }

    /// The swap is stricter than the report: no asset, no update. Only
    /// Windows has a swap, so only Windows compiles this.
    #[cfg(windows)]
    #[test]
    fn news_without_an_asset_is_not_downloadable() {
        let news = News { tag: "v0.9.3".to_string(), asset: None };
        let err = downloadable(news).unwrap_err().to_string();
        assert!(err.contains("no matching asset"), "{err}");
    }

    /// A matched asset with no URL is a broken payload; reporting the
    /// version and shrugging would hide it.
    #[test]
    fn a_matched_asset_without_a_url_is_an_error() {
        let json = serde_json::json!({
            "tag_name": "v0.9.0",
            "assets": [{ "name": "chibipop-v0.9.0-linux-x64.tar.gz" }],
        });
        let err = latest(&json, "0.8.2", Platform::Linux).unwrap_err().to_string();
        assert!(err.contains("no download URL"), "{err}");
    }

    #[test]
    fn the_host_platform_is_the_one_this_binary_was_built_for() {
        #[cfg(windows)]
        assert_eq!(Platform::Windows, Platform::HOST);
        #[cfg(not(windows))]
        assert_eq!(Platform::Linux, Platform::HOST);
    }
}

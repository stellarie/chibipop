//! The update check control
//! (ARCHITECTURE.md#packaging-and-ci).
//!
//! This module matches Windows behavior. The system runs the check only
//! when the user clicks the button. The system does not contact servers at
//! startup. The check returns one status line. Core `chibipop::update` does
//! not compile binary replacement on Linux. The module names a newer
//! release and names the file asset to download.
//!
//! A tarball installation and an AUR installation update by different
//! methods. The application identifies which release asset belongs to it.
//! The checker matches that asset by name. The release asset naming scheme
//! is a fixed contract. If the project renames an asset, installed clients
//! cannot find updates.

use chibipop::update::{self, News};

/// Return the status text after the update check finishes.
pub fn report(current: &str) -> String {
    describe(update::news(current))
}

/// Build the status line for a finished check.
fn describe(outcome: anyhow::Result<Option<News>>) -> String {
    match outcome {
        Ok(None) => "You already have the latest version.".to_string(),
        Ok(Some(news)) => match news.asset {
            Some(asset) => format!(
                "{} is available. chibipop does not replace itself on Linux: \
                 update with your package manager, or download {} from \
                 github.com/stellarie/chibipop/releases.",
                news.tag, asset.name,
            ),
            // A release without a linux-x64 asset is valid news.
            // Do not report a failure when the release exists.
            None => format!(
                "{} is available, but that release carries no linux-x64 \
                 tarball. Update with your package manager, or see \
                 github.com/stellarie/chibipop/releases.",
                news.tag,
            ),
        },
        Err(e) => format!("Update check failed: {e:#}"),
    }
}

/// Run the full update button flow against a test release endpoint.
///
/// This test verifies negative behavior. When the check reports a newer
/// release, the process does not modify the running binary. The build on
/// Linux excludes binary replacement code. This test confirms that binary
/// replacement code remains excluded.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    /// Serve one response for `releases/latest`, then close the connection.
    fn fake_release_endpoint(body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binding a loopback port");
        let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else { return };
            serve(stream, &body);
        });
        url
    }

    fn serve(mut stream: TcpStream, body: &str) {
        // Read the request header. The ureq client waits until the server
        // finishes reading the request.
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
            if line == "\r\n" || line == "\n" {
                break;
            }
            line.clear();
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len(),
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        let _ = stream.flush();
    }

    fn release_json(tag: &str) -> String {
        serde_json::json!({
            "tag_name": tag,
            "assets": [
                {
                    "name": format!("chibipop-{tag}-windows-x64.zip"),
                    "browser_download_url":
                        format!("https://example.invalid/{tag}/win.zip"),
                },
                {
                    "name": format!("chibipop-{tag}-linux-x64.tar.gz"),
                    "browser_download_url":
                        format!("https://example.invalid/{tag}/linux.tar.gz"),
                },
            ],
        })
        .to_string()
    }

    /// Files that binary replacement on Windows writes near the executable.
    /// The Linux build does not create these files. The test verifies this condition.
    const SWAP_LEAVINGS: [&str; 3] = ["chibipop.update.zip", "chibipop.new.exe", "chibipop.old"];

    #[test]
    fn a_newer_release_is_reported_and_nothing_is_written() {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().unwrap().to_path_buf();
        let before = std::fs::metadata(&exe).unwrap();

        let url = fake_release_endpoint(release_json("v9.9.9"));
        let msg = describe(update::news_at(&url, "0.8.2"));

        assert!(msg.contains("v9.9.9"), "{msg}");
        // Verify the Linux tarball asset, not the Windows zip asset.
        assert!(msg.contains("chibipop-v9.9.9-linux-x64.tar.gz"), "{msg}");
        assert!(!msg.contains("windows-x64.zip"), "{msg}");
        // Verify the message points to an actionable location and does not
        // report an automatic update.
        assert!(msg.contains("package manager"), "{msg}");
        assert!(!msg.to_lowercase().contains("restart"), "{msg}");

        for leaving in SWAP_LEAVINGS {
            assert!(!dir.join(leaving).exists(), "the check wrote {leaving}");
        }
        let after = std::fs::metadata(&exe).unwrap();
        assert_eq!(before.len(), after.len(), "the running binary changed size");
        assert_eq!(
            before.modified().unwrap(),
            after.modified().unwrap(),
            "the running binary was rewritten",
        );
    }

    #[test]
    fn the_current_version_reports_no_update() {
        let url = fake_release_endpoint(release_json("v0.8.2"));
        let msg = describe(update::news_at(&url, "0.8.2"));
        assert_eq!("You already have the latest version.", msg);
    }

    #[test]
    fn an_unreachable_endpoint_says_so_and_names_nothing() {
        // Bind and drop the listener. The closed port causes the connection to fail.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}/releases/latest", l.local_addr().unwrap())
        };
        let msg = describe(update::news_at(&dead, "0.8.2"));
        assert!(msg.starts_with("Update check failed:"), "{msg}");
    }

    /// Version v0.9.3 was a Windows-only release. A Linux check against this
    /// release must report the version instead of an error.
    #[test]
    fn a_windows_only_release_reports_the_version_and_says_what_is_missing() {
        let json = serde_json::json!({
            "tag_name": "v0.9.3",
            "assets": [{
                "name": "chibipop-v0.9.3-windows-x64.zip",
                "browser_download_url": "https://example.invalid/win.zip",
            }],
        })
        .to_string();
        let url = fake_release_endpoint(json);

        let msg = describe(update::news_at(&url, "0.8.2"));

        assert!(msg.contains("v0.9.3"), "{msg}");
        assert!(msg.contains("no linux-x64 tarball"), "{msg}");
        assert!(!msg.contains("failed"), "{msg}");
        assert!(!msg.contains("windows-x64.zip"), "{msg}");
    }
}

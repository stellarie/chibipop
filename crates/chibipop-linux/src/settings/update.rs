//! The "Check for updates" button: check-only, forever (ADR-0007).
//!
//! Windows parity is the whole shape of this module — the check runs on
//! a click and nowhere else, so there is no startup phone-home — but the
//! answer stops at a sentence. Core's `chibipop::update` builds no exe
//! swap on this platform (`download_and_replace` is `#[cfg(windows)]`),
//! so this module cannot grow one by accident: the only thing it can do
//! with a newer release is name it, and name the asset to fetch.
//!
//! Naming the asset is not decoration. A tarball install and an AUR
//! install update by different means, and the one thing chibipop knows
//! for certain is which file on the release page is its own.

use chibipop::update::{self, News};

/// What the button reports, having checked.
pub fn report(current: &str) -> String {
    describe(update::news(current))
}

/// The status line for a finished check.
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
            // A release with no linux-x64 asset is still news; saying
            // "check failed" for one would be a lie about the version.
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

/// The button, end to end, against a release endpoint this test owns.
///
/// The claim under test is the negative one: a newer version is found and
/// reported, and the binary this process is running is untouched
/// afterwards. There is nothing to stub out to make that true — the swap
/// is not compiled here — so the test's job is to notice if that ever
/// stops being the case.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    /// Serves exactly one `releases/latest` response, then closes.
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
        // Drain the request head; ureq will not read a response it has
        // not finished sending a request for.
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

    /// What the Windows swap would write beside the binary. On Linux
    /// nothing builds these names; the test asserts the ground truth
    /// rather than trusting the `cfg`.
    const SWAP_LEAVINGS: [&str; 3] = ["chibipop.update.zip", "chibipop.new.exe", "chibipop.old"];

    #[test]
    fn a_newer_release_is_reported_and_nothing_is_written() {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().unwrap().to_path_buf();
        let before = std::fs::metadata(&exe).unwrap();

        let url = fake_release_endpoint(release_json("v9.9.9"));
        let msg = describe(update::news_at(&url, "0.8.2"));

        assert!(msg.contains("v9.9.9"), "{msg}");
        // The Linux asset, not the zip the same release also carries.
        assert!(msg.contains("chibipop-v9.9.9-linux-x64.tar.gz"), "{msg}");
        assert!(!msg.contains("windows-x64.zip"), "{msg}");
        // The report points somewhere a user can act, and never claims
        // to have done anything.
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
        // Bound and dropped: the port is closed, so the connect fails.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}/releases/latest", l.local_addr().unwrap())
        };
        let msg = describe(update::news_at(&dead, "0.8.2"));
        assert!(msg.starts_with("Update check failed:"), "{msg}");
    }

    /// v0.9.3 is real: the last Windows-only release. A Linux build
    /// checking against it must report the version rather than an
    /// error - the news is true even when the tarball is missing.
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

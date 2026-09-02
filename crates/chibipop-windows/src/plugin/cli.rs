//! Commands to list plugins or test one plugin.

use crate::plugin::manifest::Role;
use crate::plugin::{discover, host, proto};
use base64::Engine;
use std::path::Path;
use std::time::{Duration, Instant};

pub fn list(root: &Path) -> i32 {
    let found = discover::discover(root);
    if found.is_empty() {
        println!("no plugins in {}", root.display());
        return 0;
    }
    let mut bad = 0;
    for (dir, parsed) in &found {
        match parsed {
            Ok(m) => println!(
                "{:<20} {:<8} protocol {}  roles {:?}",
                m.name, m.version, m.protocol, m.roles
            ),
            Err(e) => {
                bad += 1;
                println!("{:<20} REFUSED  {e:#}", dir.file_name().unwrap_or_default().to_string_lossy());
            }
        }
    }
    if bad > 0 { 1 } else { 0 }
}

pub fn test_one(root: &Path, name: &str, image: &Path) -> i32 {
    let found = discover::discover(root);
    let Some((dir, parsed)) = found.iter().find(|(d, p)| {
        p.as_ref().map(|m| m.name == name).unwrap_or(false)
            || d.file_name().map(|f| f == name).unwrap_or(false)
    }) else {
        eprintln!("no plugin named \"{name}\" under {}", root.display());
        return 2;
    };
    let m = match parsed {
        Ok(m) => m,
        Err(e) => {
            eprintln!("plugin \"{name}\": {e:#}");
            return 1;
        }
    };
    if !m.roles.contains(&Role::TextProvider) {
        eprintln!("plugin \"{name}\" is not a text-provider");
        return 2;
    }
    let bytes = match std::fs::read(image) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("reading {}: {e}", image.display());
            return 2;
        }
    };

    let t = Instant::now();
    let mut h = match host::spawn(m, dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("handshake failed: {e:#}");
            return 1;
        }
    };
    println!("handshake ok in {:?}: {}", t.elapsed(), h.ready().name);

    let cfg = m.text_provider.as_ref().expect("checked at parse");
    let params = proto::RecogniseParams {
        image_png: base64::engine::general_purpose::STANDARD.encode(&bytes),
        region: proto::Rect { x: 0, y: 0, w: 0, h: 0 },
        scale: 1,
        language: cfg.languages.first().cloned().unwrap_or_default(),
        deadline_ms: cfg.timeout_ms,
    };
    let params = match serde_json::to_value(&params) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("encoding the request: {e}");
            return 1;
        }
    };

    let t = Instant::now();
    let v = match h.call(
        "text/recognise",
        params,
        Duration::from_millis(cfg.timeout_ms),
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("text/recognise failed: {e:#}");
            return 1;
        }
    };
    let took = t.elapsed();

    let parsed: proto::RecogniseResult = match serde_json::from_value(v) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("response did not match the contract: {e}");
            return 1;
        }
    };
    println!("recognise ok in {took:?}, {} line(s)", parsed.lines.len());
    for (i, line) in parsed.lines.iter().enumerate() {
        let n = line.words.as_ref().map(|w| w.len()).unwrap_or(0);
        println!("  line {i}: {:?}  words {n}", line.text);
    }

    if violates_geometry(cfg.provides_geometry, &parsed) {
        eprintln!("VIOLATION: manifest claims geometry, the response carries none");
        return 1;
    }
    0
}

fn violates_geometry(claimed: bool, r: &proto::RecogniseResult) -> bool {
    if !claimed || r.lines.is_empty() {
        return false;
    }
    !r.lines.iter().any(|l| l.words.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_response_is_not_a_geometry_violation() {
        let r = proto::RecogniseResult { lines: vec![] };
        assert!(!violates_geometry(true, &r), "no lines means no text, not a broken plugin");
    }

    #[test]
    fn a_geometry_claim_with_text_but_no_words_is_a_violation() {
        let r = proto::RecogniseResult {
            lines: vec![proto::Line { text: "宿舎".into(), words: None }],
        };
        assert!(violates_geometry(true, &r));
    }

    #[test]
    fn a_text_only_plugin_never_violates() {
        let r = proto::RecogniseResult {
            lines: vec![proto::Line { text: "宿舎".into(), words: None }],
        };
        assert!(!violates_geometry(false, &r));
    }
}

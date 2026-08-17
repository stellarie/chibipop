//! A fixture plugin for tests.

use std::io::{BufRead, Write};

pub fn run(mode: &str) -> ! {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let id = v.get("id").and_then(|i| i.as_u64()).unwrap_or(0);
        let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let mut reply = match (method, mode) {
            ("hello", _) => serde_json::json!({"result": {
                "protocol": 1, "name": "echo", "version": "0.1.0",
                "roles": ["text-provider"], "features": [],
                "capabilities": {"geometry": true, "languages": ["ja"]}}}),
            (_, "crash") => std::process::exit(3),
            // park() may wake spuriously.
            (_, "hang") => loop {
                std::thread::park();
            },
            (_, "garbage") => {
                let _ = writeln!(out, "not json at all");
                let _ = out.flush();
                continue;
            }
            ("text/recognise", _) => serde_json::json!({"result": {"lines": [
                {"text": "宿舎", "words": [
                    {"text": "宿舎", "rect": {"x": 0, "y": 0, "w": 112, "h": 60}}]}]}}),
            _ => serde_json::json!({"error": "unknown method"}),
        };
        reply["id"] = serde_json::json!(id);
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
    }
    std::process::exit(0)
}

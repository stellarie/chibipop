use chibipop::plugin::{host, manifest};
use std::time::Duration;

fn fixture(mode: &str) -> manifest::Manifest {
    let toml = format!(
        r#"
name = "echo"
version = "0.1.0"
protocol = 1
command = "{}"
args = ["plugin-echo", "{mode}"]
roles = ["text-provider"]

[text_provider]
provides_geometry = true
languages = ["ja"]
timeout_ms = 2000
"#,
        env!("CARGO_BIN_EXE_chibipop").replace('\\', "\\\\")
    );
    manifest::parse(&toml).expect("fixture manifest")
}

#[test]
fn a_clean_exchange_returns_words() {
    let m = fixture("ok");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    assert_eq!(h.ready().name, "echo");
    let v = h
        .call("text/recognise", serde_json::json!({}), Duration::from_secs(2))
        .unwrap();
    assert_eq!(v["lines"][0]["text"], "宿舎");
    h.shutdown();
}

#[test]
fn a_hang_times_out_without_killing_the_test() {
    let m = fixture("hang");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    let e = h
        .call("text/recognise", serde_json::json!({}), Duration::from_millis(150))
        .unwrap_err()
        .to_string();
    assert!(e.contains("deadline"), "{e}");
    h.shutdown();
}

#[test]
fn a_crash_is_reported_as_an_error() {
    let m = fixture("crash");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    assert!(h
        .call("text/recognise", serde_json::json!({}), Duration::from_secs(2))
        .is_err());
    h.shutdown();
}

#[test]
fn garbage_output_is_reported_as_an_error() {
    let m = fixture("garbage");
    let mut h = host::spawn(&m, std::path::Path::new(".")).unwrap();
    assert!(h
        .call("text/recognise", serde_json::json!({}), Duration::from_secs(2))
        .is_err());
    h.shutdown();
}

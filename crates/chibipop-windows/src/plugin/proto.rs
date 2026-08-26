//! The plugin wire protocol.

use crate::plugin::manifest::Role;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl From<crate::geom::PhysRect> for Rect {
    fn from(r: crate::geom::PhysRect) -> Self {
        Rect { x: r.x, y: r.y, w: r.w, h: r.h }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Hello {
    pub chibipop: String,
    pub protocol_supported: Vec<u32>,
    pub features: Vec<String>,
    pub roles_wanted: Vec<Role>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Caps {
    #[serde(default)]
    pub geometry: bool,
    #[serde(default)]
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Ready {
    pub protocol: u32,
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub roles: Vec<Role>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub capabilities: Option<Caps>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecogniseParams {
    pub image_png: String,
    pub region: Rect,
    pub scale: i32,
    pub language: String,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Word {
    pub text: String,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Line {
    pub text: String,
    pub words: Option<Vec<Word>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RecogniseResult {
    #[serde(default)]
    pub lines: Vec<Line>,
}

pub fn request(id: u64, method: &str, params: serde_json::Value) -> String {
    let v = serde_json::json!({ "id": id, "method": method, "params": params });
    format!("{v}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_one_line_and_ends_in_a_newline() {
        let s = request(7, "text/recognise", serde_json::json!({"scale": 2}));
        assert!(s.ends_with('\n'));
        assert_eq!(s.matches('\n').count(), 1);
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "text/recognise");
    }

    #[test]
    fn a_geometry_response_parses() {
        let json = r#"{"lines":[{"text":"宿舎に","words":[
            {"text":"宿舎","rect":{"x":0,"y":0,"w":112,"h":60}}]}]}"#;
        let r: RecogniseResult = serde_json::from_str(json).unwrap();
        assert_eq!(r.lines[0].text, "宿舎に");
        assert_eq!(r.lines[0].words.as_ref().unwrap()[0].rect.w, 112);
    }

    #[test]
    fn a_text_only_response_parses_with_no_words() {
        let json = r#"{"lines":[{"text":"宿舎に戻る"}]}"#;
        let r: RecogniseResult = serde_json::from_str(json).unwrap();
        assert!(r.lines[0].words.is_none());
    }

    #[test]
    fn an_unknown_field_is_ignored_not_rejected() {
        let json = r#"{"lines":[{"text":"x","confidence":0.9}],"engine":"v9"}"#;
        let r: RecogniseResult = serde_json::from_str(json).unwrap();
        assert_eq!(r.lines[0].text, "x");
    }

    #[test]
    fn a_ready_without_capabilities_parses() {
        let json = r#"{"protocol":1,"name":"p","roles":["text-provider"]}"#;
        let r: Ready = serde_json::from_str(json).unwrap();
        assert_eq!(r.protocol, 1);
        assert!(r.capabilities.is_none());
    }
}

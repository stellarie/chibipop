//! Define the JSON messages for the wire protocol between the plugin host and each plugin.

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
    fn a_geometry_response_parses() {
        let json = r#"{"lines":[{"text":"宿舎に","words":[
            {"text":"宿舎","rect":{"x":0,"y":0,"w":112,"h":60}}]}]}"#;
        let r: RecogniseResult = serde_json::from_str(json).unwrap();
        assert_eq!(r.lines[0].text, "宿舎に");
        assert_eq!(r.lines[0].words.as_ref().unwrap()[0].rect.w, 112);
    }

    #[test]
    fn an_unknown_field_is_ignored_not_rejected() {
        let json = r#"{"lines":[{"text":"x","confidence":0.9}],"engine":"v9"}"#;
        let r: RecogniseResult = serde_json::from_str(json).unwrap();
        assert_eq!(r.lines[0].text, "x");
    }
}

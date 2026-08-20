//! Pixels in, lines out.

use crate::text::layout::OcrLine;
use anyhow::Result;

pub trait Recogniser {
    fn recognise(&self, buf: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>>;
    fn name(&self) -> &str;
    fn provides_geometry(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::PhysRect;
    use crate::text::layout::OcrWord;

    struct Fake;

    impl Recogniser for Fake {
        fn recognise(&self, _b: &[u8], _w: i32, _h: i32) -> Result<Vec<OcrLine>> {
            Ok(vec![OcrLine {
                words: vec![OcrWord {
                    text: "宿舎".into(),
                    rect: PhysRect { x: 0, y: 0, w: 56, h: 30 },
                }],
            }])
        }
        fn name(&self) -> &str { "fake" }
        fn provides_geometry(&self) -> bool { true }
    }

    struct TextOnly;

    impl Recogniser for TextOnly {
        fn recognise(&self, _b: &[u8], _w: i32, _h: i32) -> Result<Vec<OcrLine>> {
            Ok(Vec::new())
        }
        fn name(&self) -> &str { "text-only" }
        fn provides_geometry(&self) -> bool { false }
    }

    #[test]
    fn a_recogniser_is_usable_as_a_trait_object() {
        let r: &dyn Recogniser = &Fake;
        let lines = r.recognise(&[], 10, 10).unwrap();
        assert_eq!(lines[0].words[0].text, "宿舎");
        assert_eq!(lines[0].words[0].rect.w, 56);
        assert_eq!(r.name(), "fake");
        assert!(r.provides_geometry());
    }

    #[test]
    fn a_text_only_recogniser_says_so() {
        let r: Box<dyn Recogniser> = Box::new(TextOnly);
        assert!(r.recognise(&[], 10, 10).unwrap().is_empty());
        assert_eq!(r.name(), "text-only");
        assert!(!r.provides_geometry());
    }
}

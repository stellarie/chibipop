use crate::geom::PhysPoint;
use crate::text::TextSpan;
use anyhow::Result;

pub trait TextProvider {
    fn resolve_at(&self, cursor: PhysPoint) -> Result<Option<TextSpan>>;
    fn name(&self) -> &str;
    fn provides_geometry(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::PhysRect;

    struct Fake;

    impl TextProvider for Fake {
        fn resolve_at(&self, _c: PhysPoint) -> Result<Option<TextSpan>> {
            Ok(Some(TextSpan {
                text: "宿舎".into(),
                cursor_byte_offset: 0,
                anchor: PhysRect { x: 1, y: 2, w: 3, h: 4 },
                geom: vec![],
            }))
        }
        fn name(&self) -> &str { "fake" }
        fn provides_geometry(&self) -> bool { false }
    }

    #[test]
    fn a_provider_is_usable_as_a_trait_object() {
        let p: &dyn TextProvider = &Fake;
        let span = p.resolve_at(PhysPoint { x: 0, y: 0 }).unwrap().unwrap();
        assert_eq!(span.text, "宿舎");
        assert_eq!(p.name(), "fake");
        assert!(!p.provides_geometry());
    }
}

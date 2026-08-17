use crate::geom::{PhysPoint, ScanRect};
use crate::text::layout::Resolved;
use anyhow::Result;

pub struct TextRead {
    pub resolved: Option<Resolved>,
    pub scan: Vec<ScanRect>,
}

pub trait TextProvider {
    fn read_at(&self, cursor: PhysPoint, collect_scan: bool) -> Result<TextRead>;
    fn name(&self) -> &str;
    fn provides_geometry(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::PhysRect;
    use crate::text::layout::Orientation;
    use crate::text::TextSpan;

    struct Fake;

    impl TextProvider for Fake {
        fn read_at(&self, _c: PhysPoint, _collect_scan: bool) -> Result<TextRead> {
            Ok(TextRead {
                resolved: Some(Resolved {
                    span: TextSpan {
                        text: "宿舎".into(),
                        cursor_byte_offset: 0,
                        anchor: PhysRect { x: 1, y: 2, w: 3, h: 4 },
                        geom: vec![],
                    },
                    orientation: Orientation::Horizontal,
                }),
                scan: vec![],
            })
        }
        fn name(&self) -> &str { "fake" }
        fn provides_geometry(&self) -> bool { false }
    }

    #[test]
    fn a_provider_is_usable_as_a_trait_object() {
        let p: &dyn TextProvider = &Fake;
        let read = p.read_at(PhysPoint { x: 0, y: 0 }, false).unwrap();
        assert_eq!(read.resolved.unwrap().span.text, "宿舎");
        assert_eq!(p.name(), "fake");
        assert!(!p.provides_geometry());
    }
}

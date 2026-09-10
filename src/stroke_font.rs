//! The Xschem stroke font, compiled into the executable and decoded once.
//! Upstream attribution and license are included under assets/ and licenses/.
use crate::{
    document::{Document, RecordKind},
    geometry::Point,
    scene::TextRun,
};
use std::sync::OnceLock;

pub struct StrokeFont {
    glyphs: Vec<Vec<(Point, Point)>>,
}
impl StrokeFont {
    pub fn bundled() -> &'static Self {
        static FONT: OnceLock<StrokeFont> = OnceLock::new();
        FONT.get_or_init(|| {
            let document = Document::parse(include_str!("../assets/xschem-font.sch"))
                .expect("bundled font must be valid Xschem data");
            Self::from_document(&document).expect("bundled font must contain stroke glyphs")
        })
    }
    pub fn from_document(doc: &Document) -> Result<Self, String> {
        let mut glyphs = vec![Vec::new(); 128];
        for record in &doc.records {
            if let RecordKind::Line { a, b, .. } = &record.kind {
                let code = (a.x.min(b.x) / 40.).floor() as i64;
                if !(0..127).contains(&code) {
                    continue;
                }
                let origin = Point::new(code as f64 * 40., -40.);
                let (a, b) = (*a - origin, *b - origin);
                if [a, b]
                    .iter()
                    .all(|p| p.x >= -1. && p.x <= 21. && p.y >= -1. && p.y <= 66.)
                {
                    glyphs[code as usize].push((a, b));
                }
            }
        }
        if glyphs.iter().filter(|g| !g.is_empty()).count() < 30 {
            return Err("not an Xschem stroke-font schematic".into());
        }
        Ok(Self { glyphs })
    }
    pub fn lines(&self, run: &TextRun, mut draw: impl FnMut(Point, Point)) {
        let transform = run.glyph_transform();
        let scale = Point::new(run.scale.x * 0.85, run.scale.y * 0.88);
        for (row, line) in run.text.split('\n').enumerate() {
            for (column, c) in line.chars().enumerate() {
                let code = if c.is_ascii() {
                    c as usize
                } else {
                    b'?' as usize
                };
                for &(a, b) in &self.glyphs[code] {
                    let map = |p: Point| {
                        transform.point(Point::new(
                            (p.x + column as f64 * 30.) * scale.x,
                            (p.y + row as f64 * 65.) * scale.y,
                        ))
                    };
                    draw(map(a), map(b));
                }
            }
        }
    }
}

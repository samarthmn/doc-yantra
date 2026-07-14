//! OpenType shaping adapter backed by the shared `text-core` crate.

use crate::fonts::db::Face;
use crate::fonts::metrics::GlyphId;
use crate::{OfficeError, Result, UnsupportedCode};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// A single placed glyph.
#[derive(Debug, Clone)]
pub struct Glyph {
    /// Glyph id in the original face.
    pub id: GlyphId,
    /// Horizontal advance in points.
    pub advance_pt: f64,
    /// Horizontal placement offset in points.
    pub x_offset_pt: f64,
    /// Vertical placement offset in points.
    pub y_offset_pt: f64,
    /// Byte offset of the source grapheme cluster.
    pub cluster: usize,
    /// Index into the candidate face slice supplied to the shaper.
    pub face_index: usize,
    /// Complete logical source sequence owned by this glyph's cluster.
    pub unicode: Option<String>,
}

/// The result of shaping a run of text with one face.
#[derive(Debug, Clone, Default)]
pub struct ShapedRun {
    /// Placed glyphs in visual order.
    pub glyphs: Vec<Glyph>,
    /// Total advance width in points.
    pub width_pt: f64,
    /// Characters with no glyph in the face.
    pub missing_glyphs: Vec<char>,
}

/// Shapes `text` with HarfRust. Character spacing is applied per shaped
/// grapheme cluster. `kern` is explicit because Word disables OpenType kerning
/// unless the run's resolved `w:kern` setting enables it; standard ligatures
/// stay disabled to preserve Office's one-source-character glyph IR.
pub fn shape(text: &str, face: &Face, size_pt: f64, char_spacing_pt: f64, kern: bool) -> ShapedRun {
    shape_with_fallback(
        text,
        std::slice::from_ref(&face),
        size_pt,
        char_spacing_pt,
        kern,
    )
}

/// Shapes with ordered, cluster-boundary face fallback.
pub fn shape_with_fallback(
    text: &str,
    faces: &[&Face],
    size_pt: f64,
    char_spacing_pt: f64,
    kern: bool,
) -> ShapedRun {
    let (normalized, source_map) = normalize_with_source_map(text);
    let shared_faces: Vec<text_core::Face<'_>> = faces
        .iter()
        .filter_map(|face| text_core::Face::new(&face.data, face.index).ok())
        .collect();
    if shared_faces.len() != faces.len() || shared_faces.is_empty() {
        return ShapedRun {
            missing_glyphs: text.chars().collect(),
            ..Default::default()
        };
    }
    let features = text_core::FeatureConfig::default().kern(kern).liga(false);
    match text_core::shape_text_with_features(&normalized, &shared_faces, features) {
        Ok(runs) => {
            let mut out = ShapedRun::default();
            for run in runs {
                let scale = size_pt / f64::from(run.units_per_em);
                let mut previous_cluster = None;
                for glyph in run.glyphs {
                    let spacing = if previous_cluster != Some(glyph.cluster) {
                        char_spacing_pt
                    } else {
                        0.0
                    };
                    let advance_pt = f64::from(glyph.x_advance) * scale + spacing;
                    out.width_pt += advance_pt;
                    let normalized_cluster = run.source_range.start + glyph.cluster;
                    let source = source_map
                        .iter()
                        .rfind(|entry| entry.normalized_start <= normalized_cluster)
                        .or_else(|| source_map.first());
                    out.glyphs.push(Glyph {
                        id: GlyphId(glyph.glyph_id),
                        advance_pt,
                        x_offset_pt: f64::from(glyph.x_offset) * scale,
                        y_offset_pt: f64::from(glyph.y_offset) * scale,
                        cluster: source.map_or(normalized_cluster, |entry| entry.original_start),
                        face_index: glyph.face_index,
                        unicode: source.map(|entry| entry.original.clone()),
                    });
                    previous_cluster = Some(glyph.cluster);
                }
            }
            out
        }
        Err(text_core::ShapeError::MissingGlyphs(missing)) => ShapedRun {
            missing_glyphs: missing,
            ..Default::default()
        },
        Err(_) => ShapedRun {
            missing_glyphs: text.chars().collect(),
            ..Default::default()
        },
    }
}

#[derive(Debug)]
struct NormalizedSource {
    normalized_start: usize,
    original_start: usize,
    original: String,
}

fn normalize_with_source_map(source: &str) -> (String, Vec<NormalizedSource>) {
    let mut normalized = String::with_capacity(source.len());
    let mut map = Vec::new();
    for (original_start, grapheme) in source.grapheme_indices(true) {
        map.push(NormalizedSource {
            normalized_start: normalized.len(),
            original_start,
            original: grapheme.to_string(),
        });
        normalized.extend(grapheme.nfc());
    }
    (normalized, map)
}

/// Fails closed when shaping found characters absent from the selected face.
pub fn require_glyphs(run: ShapedRun) -> Result<ShapedRun> {
    if run.missing_glyphs.is_empty() {
        return Ok(run);
    }
    let mut missing = run.missing_glyphs;
    missing.sort_unstable();
    missing.dedup();
    let detail = missing
        .iter()
        .take(4)
        .map(|ch| format!("U+{:04X}", *ch as u32))
        .collect::<Vec<_>>()
        .join(", ");
    Err(OfficeError::unsupported(
        UnsupportedCode::MissingGlyph,
        format!("no selected font covers {detail}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::db::FontDb;
    use crate::fonts::metrics::FaceMetrics;

    #[test]
    fn shapes_latin_with_bundled_face() {
        let db = FontDb::with_bundled();
        let face = db.faces().first().expect("bundled font");
        let run = shape("Hi", face, 12.0, 0.0, true);
        assert_eq!(run.glyphs.len(), 2);
        assert!(run.width_pt > 0.0);
        assert!(run.missing_glyphs.is_empty());
    }

    #[test]
    fn shapes_hebrew_in_visual_order_with_liberation() {
        let db = FontDb::with_bundled();
        let face = db
            .faces()
            .iter()
            .find(|face| face.family_name == "Liberation Sans")
            .expect("Liberation Sans");
        let run = require_glyphs(shape("שלום", face, 12.0, 0.0, true)).unwrap();
        assert!(
            run.glyphs
                .windows(2)
                .all(|pair| pair[0].cluster >= pair[1].cluster)
        );
    }

    #[test]
    fn kern_false_matches_word_un_kerned_advances() {
        let db = FontDb::with_bundled();
        let face = db
            .faces()
            .iter()
            .find(|face| face.family_name == "Liberation Sans")
            .expect("Liberation Sans");
        let metrics = FaceMetrics::new(face.parsed().unwrap());
        let text = "AVAWAY To Yo Wa.";
        let expected: f64 = text
            .chars()
            .map(|ch| metrics.advance_pt(metrics.glyph_id(ch).unwrap(), 24.0))
            .sum();
        let actual = shape(text, face, 24.0, 0.0, false).width_pt;

        assert!(
            (actual - expected).abs() < 0.001,
            "actual={actual} expected={expected}"
        );
    }
}

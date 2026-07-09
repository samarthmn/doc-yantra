//! Shaping-lite: 1:1 character→glyph mapping with kern/GPOS pair adjustment and
//! character spacing (`w:spacing`) baked into per-glyph advances, so the emitter
//! can place glyphs directly.
//!
//! Complex-script shaping and cross-face fallback are v1-deferred (see
//! [`super::script`]); a character the face cannot map is recorded in
//! [`ShapedRun::missing_glyphs`] and the caller fails closed.

use crate::fonts::metrics::{FaceMetrics, GlyphId};

/// A single placed glyph.
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    /// Glyph id in the (original) face.
    pub id: GlyphId,
    /// Horizontal advance in points (kerning + char spacing already applied).
    pub advance_pt: f64,
    /// Byte offset of the source character (cluster) in the run text.
    pub cluster: usize,
}

/// The result of shaping a run of text with one face.
#[derive(Debug, Clone, Default)]
pub struct ShapedRun {
    /// Placed glyphs, in visual (== logical, for non-complex text) order.
    pub glyphs: Vec<Glyph>,
    /// Total advance width in points.
    pub width_pt: f64,
    /// Characters with no glyph in the face (out of envelope; caller fails closed).
    pub missing_glyphs: Vec<char>,
}

/// Shapes `text` with `metrics` at `size_pt`, applying `char_spacing_pt` to each
/// glyph and, when `kern` is set, pair kerning between adjacent glyphs. Word only
/// kerns runs that carry `w:kern` (off by default), so the caller passes the
/// resolved decision here rather than kerning unconditionally.
pub fn shape(
    text: &str,
    metrics: &FaceMetrics,
    size_pt: f64,
    char_spacing_pt: f64,
    kern: bool,
) -> ShapedRun {
    let mut run = ShapedRun::default();
    let mut prev: Option<GlyphId> = None;

    for (byte_idx, ch) in text.char_indices() {
        let Some(gid) = metrics.glyph_id(ch) else {
            run.missing_glyphs.push(ch);
            prev = None; // break kerning across a gap
            continue;
        };
        // Fold kerning into the previous glyph's advance so x-positions are exact.
        if let Some(p) = prev.filter(|_| kern) {
            let k = metrics.kern_pt(p, gid, size_pt);
            if k != 0.0 {
                if let Some(last) = run.glyphs.last_mut() {
                    last.advance_pt += k;
                }
                run.width_pt += k;
            }
        }
        let advance = metrics.advance_pt(gid, size_pt) + char_spacing_pt;
        run.glyphs.push(Glyph {
            id: gid,
            advance_pt: advance,
            cluster: byte_idx,
        });
        run.width_pt += advance;
        prev = Some(gid);
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::db::FontDb;

    /// Shapes Latin text with a bundled face when available (skips before H2).
    #[test]
    fn shapes_latin_with_bundled_face() {
        let db = FontDb::with_bundled();
        let Some(face) = db.faces().first() else {
            eprintln!("skipping: no bundled fonts vendored (human task H2)");
            return;
        };
        let metrics = FaceMetrics::new(face.parsed().unwrap());
        let run = shape("Hi", &metrics, 12.0, 0.0, true);
        assert_eq!(run.glyphs.len(), 2);
        assert!(run.width_pt > 0.0);
        assert!(run.missing_glyphs.is_empty());
        // Clusters are byte offsets of each character.
        assert_eq!(run.glyphs[0].cluster, 0);
        assert_eq!(run.glyphs[1].cluster, 1);
    }

    /// Kerning must be gated: with `kern=false` a run is never narrower than with
    /// `kern=true` (kerning only ever pulls glyphs together). Word leaves kerning
    /// off by default, so this gate is what keeps line breaks matching Word.
    #[test]
    fn kerning_is_gated_and_never_widens() {
        let db = FontDb::with_bundled();
        let Some(face) = db.faces().first() else {
            eprintln!("skipping: no bundled fonts vendored (human task H2)");
            return;
        };
        let metrics = FaceMetrics::new(face.parsed().unwrap());
        // Pairs like AV / To / Yo carry negative kerning in text faces.
        let text = "AVAWAY To Yo Wa.";
        let kerned = shape(text, &metrics, 24.0, 0.0, true).width_pt;
        let unkerned = shape(text, &metrics, 24.0, 0.0, false).width_pt;
        assert!(
            kerned <= unkerned,
            "kerning must not widen text: kerned={kerned} unkerned={unkerned}"
        );
    }
}

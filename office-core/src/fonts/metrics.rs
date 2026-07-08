//! Per-face metrics: line height (using OS/2 Windows metrics — Word's choice),
//! glyph lookup, advances, and kerning (legacy `kern` table + GPOS pair
//! adjustment).

pub use ttf_parser::GlyphId;
use ttf_parser::gpos::{PairAdjustment, PositioningSubtable};

/// Metrics for one parsed face. Cheap to construct; wraps a `ttf_parser::Face`.
pub struct FaceMetrics<'a> {
    face: ttf_parser::Face<'a>,
    units_per_em: f64,
    /// Ascent in font units (positive, up from baseline).
    ascent: f64,
    /// Descent in font units (positive magnitude, down from baseline).
    descent: f64,
    /// Line gap in font units.
    line_gap: f64,
}

impl<'a> FaceMetrics<'a> {
    /// Builds metrics from a parsed face. **Prefers OS/2 `usWinAscent`/
    /// `usWinDescent`** (what Word uses for line height), falling back to hhea.
    /// This is the single biggest line-height parity lever.
    pub fn new(face: ttf_parser::Face<'a>) -> FaceMetrics<'a> {
        let units_per_em = face.units_per_em() as f64;
        let (ascent, descent, line_gap) = match face.tables().os2 {
            Some(os2) => {
                let asc = os2.windows_ascender() as f64;
                let desc = (os2.windows_descender() as f64).abs();
                // Windows line height has no separate gap; keep 0.
                (asc, desc, 0.0)
            }
            None => (
                face.ascender() as f64,
                (face.descender() as f64).abs(),
                face.line_gap() as f64,
            ),
        };
        FaceMetrics {
            face,
            units_per_em: units_per_em.max(1.0),
            ascent,
            descent,
            line_gap,
        }
    }

    /// The underlying parsed face.
    pub fn face(&self) -> &ttf_parser::Face<'a> {
        &self.face
    }

    /// Font design units per em.
    pub fn units_per_em(&self) -> f64 {
        self.units_per_em
    }

    fn scale(&self, units: f64, size_pt: f64) -> f64 {
        units / self.units_per_em * size_pt
    }

    /// Ascent in points at `size_pt`.
    pub fn ascent_pt(&self, size_pt: f64) -> f64 {
        self.scale(self.ascent, size_pt)
    }

    /// Descent (positive magnitude) in points at `size_pt`.
    pub fn descent_pt(&self, size_pt: f64) -> f64 {
        self.scale(self.descent, size_pt)
    }

    /// Line gap in points at `size_pt`.
    pub fn line_gap_pt(&self, size_pt: f64) -> f64 {
        self.scale(self.line_gap, size_pt)
    }

    /// Natural line height in points: `(ascent + descent + gap) / upem * size`.
    pub fn line_height_pt(&self, size_pt: f64) -> f64 {
        self.scale(self.ascent + self.descent + self.line_gap, size_pt)
    }

    /// Maps a character to a glyph id via cmap (formats 4 + 12 handled by ttf-parser).
    pub fn glyph_id(&self, ch: char) -> Option<GlyphId> {
        self.face.glyph_index(ch)
    }

    /// Horizontal advance of a glyph in points at `size_pt`.
    pub fn advance_pt(&self, gid: GlyphId, size_pt: f64) -> f64 {
        let adv = self.face.glyph_hor_advance(gid).unwrap_or(0) as f64;
        self.scale(adv, size_pt)
    }

    /// Advance of a glyph in 1000-unit em space (for PDF `/W` arrays).
    pub fn advance_1000(&self, gid: GlyphId) -> u16 {
        let adv = self.face.glyph_hor_advance(gid).unwrap_or(0) as f64;
        (adv / self.units_per_em * 1000.0).round() as u16
    }

    /// Underline (position, thickness) in points at `size_pt` (position is
    /// negative below the baseline). Falls back to a synthetic metric.
    pub fn underline_pt(&self, size_pt: f64) -> (f64, f64) {
        match self.face.underline_metrics() {
            Some(m) => (
                self.scale(m.position as f64, size_pt),
                self.scale(m.thickness as f64, size_pt),
            ),
            None => (-0.1 * size_pt, 0.05 * size_pt),
        }
    }

    /// Kerning adjustment between `left` and `right` in points at `size_pt`
    /// (legacy `kern` table first, then GPOS pair adjustment).
    pub fn kern_pt(&self, left: GlyphId, right: GlyphId, size_pt: f64) -> f64 {
        self.scale(self.kern_units(left, right) as f64, size_pt)
    }

    /// Kerning in font units (0 when neither table adjusts the pair).
    pub fn kern_units(&self, left: GlyphId, right: GlyphId) -> i32 {
        if let Some(kern) = self.face.tables().kern {
            for subtable in kern.subtables {
                if subtable.horizontal
                    && !subtable.variable
                    && let Some(v) = subtable.glyphs_kerning(left, right)
                {
                    return v as i32;
                }
            }
        }
        if let Some(gpos) = self.face.tables().gpos
            && let Some(v) = gpos_pair_kern(gpos, left, right)
        {
            return v;
        }
        0
    }
}

/// Finds a GPOS pair-adjustment x-advance for `(left, right)`, scanning all
/// lookups' Pair subtables (Format 1 and 2). Returns the first match.
fn gpos_pair_kern(
    gpos: ttf_parser::opentype_layout::LayoutTable,
    left: GlyphId,
    right: GlyphId,
) -> Option<i32> {
    let lookups = gpos.lookups;
    for i in 0..lookups.len() {
        let Some(lookup) = lookups.get(i) else {
            continue;
        };
        for subtable in lookup.subtables.into_iter::<PositioningSubtable>() {
            if let PositioningSubtable::Pair(pair) = subtable
                && let Some(adj) = pair_value(pair, left, right)
            {
                return Some(adj);
            }
        }
    }
    None
}

fn pair_value(pair: PairAdjustment, left: GlyphId, right: GlyphId) -> Option<i32> {
    match pair {
        PairAdjustment::Format1 { coverage, sets } => {
            let idx = coverage.get(left)?;
            let set = sets.get(idx)?;
            let (first, _second) = set.get(right)?;
            Some(first.x_advance as i32)
        }
        PairAdjustment::Format2 {
            coverage,
            classes,
            matrix,
        } => {
            coverage.get(left)?; // left must be covered
            let c1 = classes.0.get(left);
            let c2 = classes.1.get(right);
            let (first, _second) = matrix.get((c1, c2))?;
            Some(first.x_advance as i32)
        }
    }
}

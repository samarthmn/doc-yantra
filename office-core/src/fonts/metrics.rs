//! Per-face metrics: line height (OS/2 Windows ascent/descent plus the hhea
//! line gap), glyph lookup, advances, and kerning (legacy `kern` table + GPOS
//! pair adjustment).

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
    /// Legacy East Asian class whose per-size line box splits Word measures
    /// from the classic Windows fonts' bitmap-strike metrics (extracted from
    /// the ground-truth PDF text matrices; see `size_split`).
    legacy_class: Option<LegacyCjkClass>,
}

/// The classic Windows East Asian font classes with size-dependent line
/// metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyCjkClass {
    /// SimSun lineage (and its substitutes).
    Sc,
    /// MingLiU lineage.
    Tc,
    /// MS Gothic / MS Mincho lineage.
    Jp,
    /// Batang lineage.
    Kr,
}

/// Measured `(ascent_em, descent_em)` line-box split for a legacy class at a
/// size band, from exact Word PDF baselines (2026-07-18). `None` keeps the
/// class's flat legacy constants.
fn legacy_size_split(class: LegacyCjkClass, size_pt: f64) -> Option<(f64, f64)> {
    use LegacyCjkClass::*;
    let band = if size_pt < 11.75 {
        0
    } else if size_pt < 13.0 {
        1
    } else if size_pt < 15.0 {
        2
    } else {
        3
    };
    // Equivalents of the calibrated jp/sc grid shifts (best measured state);
    // refine per class with A/B-verified exact-baseline values only.
    match (class, band) {
        (Sc, 0 | 1) => Some((1.00583, 0.28587)),
        (Sc, 2) => Some((1.005, 0.3283)),
        (Sc, 3) => Some((1.020, 0.2821)),
        (Jp, 2 | 3) => Some((1.029375, 0.270625)),
        (Kr, 0) => Some((0.9609, 0.339075)),
        _ => None,
    }
}

impl<'a> FaceMetrics<'a> {
    /// Builds metrics from a parsed face. **Prefers OS/2 `usWinAscent`/
    /// `usWinDescent`** (what Word uses for the em box), falling back to hhea,
    /// and retains the hhea line gap in either case.
    pub fn new(face: ttf_parser::Face<'a>) -> FaceMetrics<'a> {
        let units_per_em = face.units_per_em() as f64;
        let line_gap = face.line_gap() as f64;
        let (ascent, descent) = match face.tables().os2 {
            Some(os2) => {
                let asc = os2.windows_ascender() as f64;
                let desc = (os2.windows_descender() as f64).abs();
                (asc, desc)
            }
            None => (face.ascender() as f64, (face.descender() as f64).abs()),
        };
        FaceMetrics {
            face,
            units_per_em: units_per_em.max(1.0),
            ascent,
            descent,
            line_gap,
            legacy_class: None,
        }
    }

    /// Tags the face with a legacy East Asian class (see `legacy_size_split`).
    pub fn with_legacy_class(mut self, class: LegacyCjkClass) -> Self {
        self.legacy_class = Some(class);
        self
    }

    /// The underlying parsed face.
    pub fn face(&self) -> &ttf_parser::Face<'a> {
        &self.face
    }

    /// Replaces the line-box metrics with explicit em-unit values, keeping
    /// every glyph metric. Used to present metric-compatible line heights
    /// for substituted faces whose own metrics would reflow the document.
    pub fn with_line_metrics_em(mut self, ascent: f64, descent: f64, line_gap: f64) -> Self {
        self.ascent = ascent * self.units_per_em;
        self.descent = descent * self.units_per_em;
        self.line_gap = line_gap * self.units_per_em;
        self
    }

    /// Re-splits the face's own line gap evenly around the em box, matching
    /// how Word centers the classic Windows East Asian faces (SimSun,
    /// MS Gothic, Batang, MingLiU…): the baseline sits at
    /// `winAscent + lineGap/2` from the line top instead of below the whole
    /// gap. Total line height is unchanged.
    pub fn with_centered_line_gap(mut self) -> Self {
        let half = self.line_gap / 2.0;
        self.descent += half;
        self.line_gap = half;
        self
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

    /// Baseline offset inside a Word line box. Word places the hhea line gap
    /// above the glyph ascent rather than leaving it below the descent.
    pub fn line_box_ascent_pt(&self, size_pt: f64) -> f64 {
        if let Some((asc, _)) = self
            .legacy_class
            .and_then(|class| legacy_size_split(class, size_pt))
        {
            return asc * size_pt;
        }
        self.scale(self.ascent + self.line_gap, size_pt)
    }

    /// Natural line height in points: `(ascent + descent + gap) / upem * size`.
    pub fn line_height_pt(&self, size_pt: f64) -> f64 {
        if let Some((asc, desc)) = self
            .legacy_class
            .and_then(|class| legacy_size_split(class, size_pt))
        {
            return (asc + desc) * size_pt;
        }
        self.scale(self.ascent + self.descent + self.line_gap, size_pt)
    }

    /// OS/2 subscript baseline offset in points at the unscaled base size.
    pub fn subscript_y_offset_pt(&self, size_pt: f64) -> Option<f64> {
        self.face
            .subscript_metrics()
            .map(|metrics| self.scale(f64::from(metrics.y_offset), size_pt))
    }

    /// OS/2 superscript baseline offset in points at the unscaled base size.
    pub fn superscript_y_offset_pt(&self, size_pt: f64) -> Option<f64> {
        self.face
            .superscript_metrics()
            .map(|metrics| self.scale(f64::from(metrics.y_offset), size_pt))
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

#[cfg(test)]
mod tests {
    use super::FaceMetrics;
    use crate::layout::paragraph::apply_line_spacing;
    use crate::oxml::wml::LineSpacing;

    #[test]
    fn liberation_sans_natural_and_auto_line_heights_include_hhea_line_gap() {
        let bytes = include_bytes!("../../fonts/liberation/LiberationSans-Regular.ttf");
        let face = ttf_parser::Face::parse(bytes, 0).expect("bundled Liberation Sans face");
        let os2 = face.tables().os2.expect("Liberation Sans OS/2 table");
        assert!(face.line_gap() > 0, "fixture must exercise a real hhea gap");

        let size_pt = 12.0;
        let expected_natural = (f64::from(os2.windows_ascender())
            + f64::from(os2.windows_descender()).abs()
            + f64::from(face.line_gap()))
            / f64::from(face.units_per_em())
            * size_pt;
        let line_gap = face.line_gap();
        let units_per_em = face.units_per_em();
        let metrics = FaceMetrics::new(face);

        assert!(
            (metrics.line_height_pt(size_pt) - expected_natural).abs() < 1e-9,
            "actual={} expected={} asc={} desc={} gap={} upem={}",
            metrics.line_height_pt(size_pt),
            expected_natural,
            os2.windows_ascender(),
            os2.windows_descender(),
            line_gap,
            units_per_em,
        );
        assert!(
            (apply_line_spacing(Some(LineSpacing::Auto(276.0 / 240.0)), expected_natural)
                - 1.15 * expected_natural)
                .abs()
                < 1e-9
        );
    }
}

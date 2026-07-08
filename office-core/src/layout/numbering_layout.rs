//! List numbering layout: stateful counters keyed by `(num_id, ilvl)`, label
//! text construction from `lvlText`, and shaping the label into a positioned
//! run placed in the paragraph's hanging indent.

use super::assemble::WordDoc;
use super::{GlyphRun, PlacedGlyph};
use crate::fonts::matching;
use crate::fonts::metrics::FaceMetrics;
use crate::oxml::numbering::{LevelDef, NumFmt, format_number};
use crate::oxml::wml::{NumRef, ParaProps, Paragraph};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::text::shape;
use std::collections::HashMap;

/// A shaped, positioned list label.
#[derive(Debug, Clone)]
pub struct PlacedLabel {
    /// The label glyph run.
    pub run: GlyphRun,
    /// Line height contributed by the label.
    pub line_height_pt: f64,
    /// Ascent of the label.
    pub ascent_pt: f64,
    /// The level's left indent (`marL`), if any.
    pub left_pt: Option<f64>,
    /// The level's hanging indent, if any.
    pub hanging_pt: Option<f64>,
}

/// Stateful list counters. `advance` increments the referenced level and resets
/// deeper levels (Word restart semantics).
#[derive(Default)]
pub struct ListCounters {
    counters: HashMap<(i64, u32), i64>,
}

impl ListCounters {
    /// Advances the counter for `num_ref` and returns the formatted label text
    /// and its level definition. `None` if the reference is dangling.
    pub fn advance(&mut self, doc: &WordDoc, num_ref: NumRef) -> Option<(String, LevelDef)> {
        let level = doc.numbering.resolve(num_ref.num_id, num_ref.ilvl)?;
        let key = (num_ref.num_id, num_ref.ilvl);
        let entry = self.counters.entry(key).or_insert(level.start - 1);
        *entry += 1;
        // Reset deeper levels of the same list.
        self.counters
            .retain(|&(nid, lvl), _| !(nid == num_ref.num_id && lvl > num_ref.ilvl));

        let text = self.build_label_text(doc, num_ref, &level);
        Some((text, level))
    }

    fn build_label_text(&self, doc: &WordDoc, num_ref: NumRef, level: &LevelDef) -> String {
        if let NumFmt::Bullet = level.num_fmt {
            return level.lvl_text.clone();
        }
        let chars: Vec<char> = level.lvl_text.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '%'
                && i + 1 < chars.len()
                && let Some(n) = chars[i + 1].to_digit(10)
                && (1..=9).contains(&n)
            {
                let lvl_idx = n - 1;
                let lvl_def = doc
                    .numbering
                    .resolve(num_ref.num_id, lvl_idx)
                    .unwrap_or_else(|| level.clone());
                let value = self
                    .counters
                    .get(&(num_ref.num_id, lvl_idx))
                    .copied()
                    .unwrap_or(lvl_def.start);
                if let Ok(s) = format_number(&lvl_def.num_fmt, value) {
                    out.push_str(&s);
                }
                i += 2;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    }
}

/// Builds a positioned label from label text + level, shaping it with the
/// level's run properties merged over the paragraph's effective run properties.
pub fn build_label(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    label_text: &str,
    level: &LevelDef,
) -> Option<PlacedLabel> {
    // Effective run props: level rPr overlaid on the paragraph's run defaults.
    let eff = doc.styles.run_props(&para.props, &level.rpr);
    let size = eff.size_pt.unwrap_or(11.0);
    let color = Rgba::opaque(Rgb::new(0, 0, 0));
    let family = eff
        .fonts
        .ascii
        .clone()
        .or_else(|| doc.theme.minor_latin.clone())
        .unwrap_or_else(|| "Calibri".to_string());

    // Map symbol/bullet code points to Unicode using the label font as the hint.
    let mapped: String = label_text
        .chars()
        .map(|c| matching::map_symbol_char(&family, c as u32).unwrap_or(c))
        .collect();

    let weight = if eff.bold == Some(true) { 700 } else { 400 };
    let resolved = matching::resolve(&doc.fonts, &family, weight, eff.italic == Some(true));
    for w in resolved.warnings {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    let face = resolved.face?;
    let parsed = doc.fonts.face(face)?.parsed()?;
    let metrics = FaceMetrics::new(parsed);
    let shaped = shape::shape(&mapped, &metrics, size, 0.0);
    if shaped.glyphs.is_empty() {
        return None;
    }
    let chars: Vec<char> = mapped.chars().collect();
    let glyphs: Vec<PlacedGlyph> = shaped
        .glyphs
        .iter()
        .enumerate()
        .map(|(i, g)| PlacedGlyph {
            gid: g.id.0,
            advance_pt: g.advance_pt,
            ch: chars.get(i).copied().unwrap_or(' '),
        })
        .collect();

    Some(PlacedLabel {
        run: GlyphRun {
            face,
            size_pt: size,
            color,
            glyphs,
            underline: None,
            strike: None,
        },
        line_height_pt: metrics.line_height_pt(size),
        ascent_pt: metrics.ascent_pt(size),
        left_pt: level.indent.left_pt,
        hanging_pt: level.indent.hanging_pt,
    })
}

/// A paragraph's list context passed to line layout.
#[derive(Debug, Clone)]
pub struct ListContext {
    /// The positioned label (placed in the hanging indent).
    pub label: PlacedLabel,
}

/// Retained for the paragraph layout API: builds a label only when the effective
/// paragraph carries numbering *and* a caller-driven counter is unavailable.
/// (Flow drives [`ListCounters`] directly; this is the standalone fallback used
/// where no counter state is threaded.)
pub fn resolve_label(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    eff_para: &ParaProps,
    para: &Paragraph,
) -> Option<PlacedLabel> {
    let num_ref = eff_para.numbering?;
    let level = doc.numbering.resolve(num_ref.num_id, num_ref.ilvl)?;
    // Standalone: assume the first item (start value) — flow overrides with real
    // counter state via [`build_label`].
    let text = if let NumFmt::Bullet = level.num_fmt {
        level.lvl_text.clone()
    } else {
        format_number(&level.num_fmt, level.start).unwrap_or_default() + suffix_of(&level)
    };
    build_label(doc, warnings, para, &text, &level)
}

fn suffix_of(level: &LevelDef) -> &'static str {
    // The lvlText already includes literal punctuation; nothing extra here.
    let _ = level;
    ""
}

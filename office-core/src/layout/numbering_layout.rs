//! List numbering layout: stateful counters keyed by `(num_id, ilvl)`, label
//! text construction from `lvlText`, and shaping the label into a positioned
//! run placed in the paragraph's hanging indent.

use super::GlyphRun;
use super::assemble::WordDoc;
use crate::Result;
use crate::fonts::{FontSlot, matching};
use crate::oxml::numbering::{
    LabelJustification, LevelDef, LevelIndentProvenance, LevelSuffix, NumFmt, format_number,
};
use crate::oxml::wml::{NumRef, ParaProps, Paragraph, TabStop};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::text::shape;
use std::collections::HashMap;

/// A shaped, positioned list label.
#[derive(Debug, Clone)]
pub struct PlacedLabel {
    /// Ordered label glyph runs, split where the selected fallback face changes.
    pub runs: Vec<GlyphRun>,
    /// Line height contributed by the label.
    pub line_height_pt: f64,
    /// Ascent of the label.
    pub ascent_pt: f64,
    /// Whether this textual label participates in the first-line box.
    /// Bullet markers are decorative ink and use the paragraph body's box.
    pub contributes_to_line_box: bool,
    /// The level's left indent (`marL`), if any.
    pub left_pt: Option<f64>,
    /// The level's hanging indent, if any.
    pub hanging_pt: Option<f64>,
    /// Typed tab stops authored on the resolved numbering level.
    pub tabs: Vec<TabStop>,
    /// Abstract-level versus concrete-level-override indent provenance.
    pub indent_provenance: LevelIndentProvenance,
    /// Label justification inside the hanging area.
    pub justification: LabelJustification,
    /// Separator between the label and item text. An omitted `w:suff` is Tab.
    pub suffix: LevelSuffix,
    /// Shaped advance of the explicit `space` suffix.
    pub suffix_space_advance_pt: f64,
}

/// Stateful list counters. `advance` increments the referenced level and resets
/// deeper levels (Word restart semantics).
#[derive(Clone, Default)]
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
                let format = if level.legal {
                    &NumFmt::Decimal
                } else {
                    &lvl_def.num_fmt
                };
                if let Ok(s) = format_number(format, value) {
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
) -> Result<Option<PlacedLabel>> {
    // Effective run props: level rPr overlaid on the paragraph's run defaults.
    // Word formats a list label from the numbering level's run properties
    // merged over the paragraph mark's (`pPr/rPr`) — a mark-level size or
    // font reaches the label even though it never styles the item text.
    let mark = doc
        .styles
        .para_props(&para.props)
        .mark_rpr
        .unwrap_or_default();
    let eff = doc
        .styles
        .run_props(&para.props, &level.rpr.merge_over(&mark));
    let size = eff.size_pt.unwrap_or(11.0);
    let color = Rgba::opaque(Rgb::new(0, 0, 0));
    let family = matching::wml_run_family(&eff.fonts, FontSlot::Ascii, &doc.theme);

    // Symbol/Wingdings mapping is the first normalization layer.
    let symbol_mapped: String = label_text
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
    let Some(face) = resolved.face else {
        return Ok(None);
    };
    let fallback_ids =
        matching::coverage_fallbacks(&doc.fonts, face, weight, eff.italic == Some(true));
    let mapped = if level.num_fmt == NumFmt::Bullet {
        symbol_mapped
            .chars()
            .map(|source| {
                let rendered = matching::substitute_bullet_char(&doc.fonts, &fallback_ids, source)
                    .unwrap_or(source);
                if rendered != source {
                    let warning = Warning::new(
                        "bullet_substituted",
                        format!("U+{:04X} → U+{:04X}", source as u32, rendered as u32),
                    );
                    if !warnings.contains(&warning) {
                        warnings.push(warning);
                    }
                }
                rendered
            })
            .collect::<String>()
    } else {
        symbol_mapped
    };
    let (fallback_ids, fallback_faces): (Vec<_>, Vec<_>) = fallback_ids
        .into_iter()
        .filter_map(|id| doc.fonts.face(id).map(|font| (id, font)))
        .unzip();
    let kern = eff.kern_min_pt.is_some_and(|min| size >= min);
    let suffix_space_advance_pt = if level.suffix == LevelSuffix::Space {
        shape::require_glyphs(shape::shape_with_fallback(
            " ",
            &fallback_faces,
            size,
            0.0,
            kern,
        ))?
        .glyphs
        .iter()
        .map(|glyph| glyph.advance_pt)
        .sum()
    } else {
        0.0
    };
    let shaped = shape::require_glyphs(shape::shape_with_fallback(
        &mapped,
        &fallback_faces,
        size,
        0.0,
        kern,
    ))?;
    if shaped.glyphs.is_empty() {
        return Ok(None);
    }
    let semantic = crate::text::semantic::map_shaped_run(&mapped, &shaped);
    let mut semantics = Some(semantic.semantic);
    let mut runs = Vec::new();
    for (glyph, visible) in shaped.glyphs.iter().zip(semantic.visible) {
        let Some(&glyph_face) = fallback_ids.get(glyph.face_index) else {
            continue;
        };
        if runs
            .last()
            .is_none_or(|run: &GlyphRun| run.face != glyph_face)
        {
            runs.push(GlyphRun {
                face: glyph_face,
                size_pt: size,
                color,
                base_direction: None,
                glyphs: Vec::new(),
                underline: None,
                strike: None,
            });
        }
        let run = runs.last_mut().expect("numbering run was just inserted");
        run.glyphs.push(visible);
        if let Some(semantic) = semantics.take() {
            run.glyphs.extend(semantic);
        }
    }
    if runs.is_empty() {
        return Ok(None);
    }
    // The label's line box follows the resolved label font. A coverage
    // fallback that painted a bullet glyph supplies ink only — its own
    // (often much taller) metrics must not grow the list item's line. The
    // pan-CJK faces are the exception: East Asian label text is a
    // whole-script substitution and takes the legacy CJK line convention,
    // exactly as it would in Word with the real face installed.
    let (mut line_height_pt, mut ascent_pt) = doc
        .fonts
        .face(face)
        .and_then(|font| font.line_metrics())
        .map(|metrics| {
            (
                metrics.line_height_pt(size),
                metrics.line_box_ascent_pt(size),
            )
        })
        .unwrap_or((size * 1.2, size));
    for run in &runs {
        let Some(font) = doc.fonts.face(run.face) else {
            continue;
        };
        if !matching::pan_cjk_face(&font.family_name) {
            continue;
        }
        // Hangul label glyphs reach the pan-CJK face through Word's
        // font-linking fallback to a Korean face, whose measured line
        // geometry is the classic 0.86/0.14 body with a 0.38em hhea gap
        // reserved above the ascent (calibrated against the Korean claims
        // ground truth). Other pan-CJK label glyphs keep the face's legacy
        // line convention.
        let hangul = run.glyphs.iter().any(|glyph| {
            matches!(u32::from(glyph.ch),
                0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF)
        });
        let metrics = if hangul {
            font.parsed()
                .map(crate::fonts::FaceMetrics::new)
                .map(|metrics| metrics.with_line_metrics_em(1.239375, 0.140625, 0.0))
        } else {
            font.line_metrics()
        };
        if let Some(metrics) = metrics {
            line_height_pt = line_height_pt.max(metrics.line_height_pt(size));
            ascent_pt = ascent_pt.max(metrics.line_box_ascent_pt(size));
        }
    }

    Ok(Some(PlacedLabel {
        runs,
        line_height_pt,
        ascent_pt,
        contributes_to_line_box: level.num_fmt != NumFmt::Bullet,
        left_pt: level.indent.left_pt,
        hanging_pt: level.indent.hanging_pt,
        tabs: level.tabs.clone(),
        indent_provenance: level.indent_provenance,
        justification: level.justification,
        suffix: level.suffix,
        suffix_space_advance_pt,
    }))
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
) -> Result<Option<PlacedLabel>> {
    let Some(num_ref) = eff_para.numbering else {
        return Ok(None);
    };
    let Some(level) = doc.numbering.resolve(num_ref.num_id, num_ref.ilvl) else {
        return Ok(None);
    };
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

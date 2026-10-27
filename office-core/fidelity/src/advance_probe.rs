//! Exact PDF text-advance differences grouped by font and shaping context.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::probe::{PdfGlyphRecord, PdfPageContent, TextShowRecord};

#[derive(Debug, Serialize)]
pub struct AdvanceProbeReport {
    pub page: u32,
    pub groups: Vec<AdvanceGroup>,
    pub line_end_deltas: Vec<LineEndDelta>,
}

#[derive(Debug, Serialize)]
pub struct AdvanceGroup {
    pub font_family: String,
    pub font_style: String,
    pub size_pt: f64,
    pub unicode_scalar: String,
    pub left_context: String,
    pub right_context: String,
    pub samples: Vec<AdvanceSample>,
}

#[derive(Debug, Serialize)]
pub struct AdvanceSample {
    pub occurrence: usize,
    pub ours_text_units: Option<f64>,
    pub truth_text_units: Option<f64>,
    pub truth_minus_ours_text_units: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct LineEndDelta {
    pub occurrence: usize,
    pub unicode: String,
    pub ours_advance_pt: Option<f64>,
    pub truth_advance_pt: Option<f64>,
    pub truth_minus_ours_pt: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    family: String,
    style: String,
    size_milli_pt: i64,
    scalar: String,
    left: String,
    right: String,
}

pub fn compare(ours: &PdfPageContent, truth: &PdfPageContent) -> AdvanceProbeReport {
    let ours_groups = grouped_glyphs(&ours.text);
    let truth_groups = grouped_glyphs(&truth.text);
    let keys = ours_groups
        .keys()
        .chain(truth_groups.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let groups = keys
        .into_iter()
        .map(|key| {
            let ours = ours_groups.get(&key).map(Vec::as_slice).unwrap_or_default();
            let truth = truth_groups
                .get(&key)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let samples = (0..ours.len().max(truth.len()))
                .map(|index| {
                    let ours = ours.get(index).copied();
                    let truth = truth.get(index).copied();
                    AdvanceSample {
                        occurrence: index,
                        ours_text_units: ours,
                        truth_text_units: truth,
                        truth_minus_ours_text_units: truth
                            .zip(ours)
                            .map(|(truth, ours)| truth - ours),
                    }
                })
                .collect();
            AdvanceGroup {
                font_family: key.family,
                font_style: key.style,
                size_pt: key.size_milli_pt as f64 / 1_000.0,
                unicode_scalar: key.scalar,
                left_context: key.left,
                right_context: key.right,
                samples,
            }
        })
        .collect();
    AdvanceProbeReport {
        page: ours.page,
        groups,
        line_end_deltas: pair_line_ends(&ours.text, &truth.text),
    }
}

fn grouped_glyphs(records: &[TextShowRecord]) -> BTreeMap<GroupKey, Vec<f64>> {
    let mut groups = BTreeMap::<GroupKey, Vec<f64>>::new();
    for record in records {
        for (index, glyph) in record.glyphs.iter().enumerate() {
            let key = glyph_key(record, &record.glyphs, index, glyph);
            groups
                .entry(key)
                .or_default()
                .push(glyph.resulting_advance_text_units);
        }
    }
    groups
}

fn glyph_key(
    record: &TextShowRecord,
    glyphs: &[PdfGlyphRecord],
    index: usize,
    glyph: &PdfGlyphRecord,
) -> GroupKey {
    GroupKey {
        family: record.font_family.clone().unwrap_or_default(),
        style: record.font_style.clone().unwrap_or_default(),
        size_milli_pt: (record.tf_pt * 1_000.0).round() as i64,
        scalar: glyph.unicode.clone(),
        left: index
            .checked_sub(1)
            .and_then(|index| glyphs.get(index))
            .map(|glyph| glyph.unicode.clone())
            .unwrap_or_default(),
        right: glyphs
            .get(index + 1)
            .map(|glyph| glyph.unicode.clone())
            .unwrap_or_default(),
    }
}

fn pair_line_ends(ours: &[TextShowRecord], truth: &[TextShowRecord]) -> Vec<LineEndDelta> {
    let count = ours.len().max(truth.len());
    (0..count)
        .map(|index| {
            let ours_record = ours.get(index);
            let truth_record = truth.get(index);
            let ours_advance = ours_record.map(|record| record.resulting_advance_pt);
            let truth_advance = truth_record.map(|record| record.resulting_advance_pt);
            LineEndDelta {
                occurrence: index,
                unicode: truth_record
                    .or(ours_record)
                    .map(|record| record.unicode.clone())
                    .unwrap_or_default(),
                ours_advance_pt: ours_advance,
                truth_advance_pt: truth_advance,
                truth_minus_ours_pt: truth_advance
                    .zip(ours_advance)
                    .map(|(truth, ours)| truth - ours),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{PdfGlyphRecord, TextShowRecord};

    fn text_record(advance: f64) -> TextShowRecord {
        TextShowRecord {
            operator_index: 0,
            operator: "TJ".into(),
            resource_name: "F1".into(),
            font_base_name: Some("Fixture-Regular".into()),
            font_family: Some("Fixture".into()),
            font_style: Some("Regular".into()),
            font_subtype: Some("Type0".into()),
            descendant_subtype: Some("CIDFontType2".into()),
            font_encoding: Some("/Identity-H".into()),
            tf_pt: 11.0,
            tm: [1.0, 0.0, 0.0, 1.0, 10.0, 20.0],
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            origin: [10.0, 20.0],
            unicode: "A".into(),
            encoded_hex: "0001".into(),
            glyphs: vec![PdfGlyphRecord {
                source_code: 1,
                source_cid: Some(1),
                source_gid: Some(65),
                unicode: "A".into(),
                nominal_width_text_units: 600.0,
                following_tj: 0.0,
                resulting_advance_text_units: advance,
            }],
            tj_adjustments: Vec::new(),
            resulting_advance_text_units: advance,
            resulting_advance_pt: advance / 1_000.0 * 11.0,
        }
    }

    #[test]
    fn compare_reports_exact_truth_minus_ours_text_units() {
        let ours = PdfPageContent {
            page: 1,
            text: vec![text_record(590.0)],
            images: Vec::new(),
        };
        let truth = PdfPageContent {
            page: 1,
            text: vec![text_record(602.0)],
            images: Vec::new(),
        };

        let report = compare(&ours, &truth);

        assert_eq!(
            report.groups[0].samples[0].truth_minus_ours_text_units,
            Some(12.0)
        );
    }
}

//! Feature-gated fidelity diagnostics for DOCX paragraph lines.

use serde::Serialize;

use crate::layout::Item;
use crate::layout::paragraph::LineItem;
use crate::oxml::wml::{Indent, LineSpacing, NumRef, Spacing, TabStop};

/// Serializable per-line layout evidence returned by diagnostic conversions.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LayoutTrace {
    /// Body paragraph lines in final page/paint coordinates.
    pub lines: Vec<LineTrace>,
}

/// Final geometry and OOXML provenance for one laid-out paragraph line.
#[derive(Debug, Clone, Serialize)]
pub struct LineTrace {
    /// Zero-based source paragraph index in the main document story.
    pub source_paragraph_index: usize,
    /// Zero-based wrapped-line index within the source paragraph.
    pub source_line_index: usize,
    /// One-based final page number.
    pub page: u32,
    /// Paragraph-property values split by provenance and effective result.
    pub paragraph: ParagraphPropertyTrace,
    /// Final top of the line box in page points.
    pub line_box_top_pt: f64,
    /// Final line-box height in points.
    pub line_box_height_pt: f64,
    /// Natural ascent from line top to baseline in points.
    pub line_ascent_pt: f64,
    /// Natural descent below the baseline in points.
    pub line_descent_pt: f64,
    /// Final baseline in page points.
    pub baseline_pt: f64,
    /// Natural line height before the paragraph's line-spacing rule.
    pub natural_line_height_pt: f64,
    /// Largest resolved text line box, including an inline object's owning run.
    pub text_natural_height_pt: f64,
    /// Resolved font metrics that contributed to this line.
    pub run_metrics: Vec<RunMetricTrace>,
    /// Visual bounds of non-math inline objects in page coordinates.
    pub object_ink_bounds: Vec<InkBoundsTrace>,
    /// Visual bounds of math composites in page coordinates.
    pub math_ink_bounds: Vec<InkBoundsTrace>,
    /// Final origins of glyphs emitted by this line.
    pub glyph_origins: Vec<GlyphOriginTrace>,
}

/// Direct/style/numbering paragraph values and the values used by layout.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ParagraphPropertyTrace {
    /// Direct paragraph style id, when present.
    pub style_id: Option<String>,
    /// Effective numbering reference, when present.
    pub numbering: Option<NumberingReferenceTrace>,
    /// Document-default and default-paragraph-style spacing.
    pub default_spacing: SpacingTrace,
    /// Resolved selected paragraph-style spacing.
    pub style_spacing: SpacingTrace,
    /// Directly authored paragraph spacing.
    pub direct_spacing: SpacingTrace,
    /// Spacing values actually used by paragraph layout.
    pub effective_spacing: SpacingTrace,
    /// Directly authored paragraph indent.
    pub direct_indent: IndentTrace,
    /// Resolved paragraph-style indent before direct formatting.
    pub style_indent: IndentTrace,
    /// Numbering-level indent.
    pub numbering_indent: IndentTrace,
    /// Indent values actually used by paragraph layout.
    pub effective_indent: IndentTrace,
    /// Directly authored paragraph tabs.
    pub direct_tabs: Vec<TabStopTrace>,
    /// Resolved paragraph-style tabs.
    pub style_tabs: Vec<TabStopTrace>,
    /// Tabs authored on the numbering level.
    pub numbering_tabs: Vec<TabStopTrace>,
    /// Tabs actually supplied to line layout.
    pub effective_tabs: Vec<TabStopTrace>,
    /// Resolved list-label alignment and following-text geometry.
    pub numbering_layout: Option<NumberingLayoutTrace>,
}

/// Serializable paragraph spacing with the OOXML rule/value split preserved.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SpacingTrace {
    /// Space before the paragraph in points.
    pub before_pt: Option<f64>,
    /// Space after the paragraph in points.
    pub after_pt: Option<f64>,
    /// `auto`, `at_least`, or `exact` when line spacing is authored.
    pub line_rule: Option<String>,
    /// Auto multiplier or fixed/at-least point value, according to `line_rule`.
    pub line_value: Option<f64>,
}

/// One resolved line-box metric source.
#[derive(Debug, Clone, Serialize)]
pub struct RunMetricTrace {
    /// Font database face id.
    pub face_id: usize,
    /// Resolved face family name.
    pub family_name: String,
    /// Unscripted run size governing the line box.
    pub size_pt: f64,
    /// Resolved line-box ascent in points.
    pub line_box_ascent_pt: f64,
    /// Resolved natural line-box height in points.
    pub line_box_height_pt: f64,
    /// Whether these metrics came from an inline object's owning run.
    pub inline_object: bool,
    /// Whether a directly authored run size is smaller than the paragraph
    /// mark's effective size.
    pub direct_size_smaller_than_paragraph: bool,
}

/// Serializable numbering geometry, measured from the page's left text margin.
#[derive(Debug, Clone, Serialize)]
pub struct NumberingLayoutTrace {
    /// Start of the aligned label box.
    pub label_alignment_box_start_pt: f64,
    /// End of the aligned label box.
    pub label_alignment_box_end_pt: f64,
    /// End of the rendered label advance.
    pub label_ink_end_pt: f64,
    /// Start of the following paragraph text after applying `w:suff`.
    pub following_text_start_pt: f64,
    /// Provenance of the effective left indent.
    pub left_origin: String,
    /// Provenance of the effective hanging indent.
    pub hanging_origin: String,
    /// Provenance of the tab target used for a tab suffix.
    pub tab_origin: String,
}

/// Serializable numbering id and level.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct NumberingReferenceTrace {
    /// Concrete `w:numId`.
    pub num_id: i64,
    /// Zero-based `w:ilvl`.
    pub ilvl: u32,
}

/// Serializable paragraph indent in points.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndentTrace {
    /// Left/start indent.
    pub left_pt: Option<f64>,
    /// Right/end indent.
    pub right_pt: Option<f64>,
    /// Positive first-line indent.
    pub first_line_pt: Option<f64>,
    /// Hanging indent.
    pub hanging_pt: Option<f64>,
}

/// Serializable tab stop and its alignment/leader behavior.
#[derive(Debug, Clone, Serialize)]
pub struct TabStopTrace {
    /// Position from the left text margin in points.
    pub pos_pt: f64,
    /// Parsed alignment name.
    pub alignment: String,
    /// Parsed leader name.
    pub leader: String,
}

/// Axis-aligned visual bounds in page coordinates.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct InkBoundsTrace {
    /// Left edge.
    pub x0_pt: f64,
    /// Top edge.
    pub y0_pt: f64,
    /// Right edge.
    pub x1_pt: f64,
    /// Bottom edge.
    pub y1_pt: f64,
}

/// Final origin and identity of one visible glyph.
#[derive(Debug, Clone, Serialize)]
pub struct GlyphOriginTrace {
    /// Font database face id.
    pub face_id: usize,
    /// Original font glyph id.
    pub gid: u16,
    /// Source scalar carried by the placed glyph.
    pub unicode: String,
    /// Final horizontal origin in page points.
    pub x_pt: f64,
    /// Final top-origin baseline/origin in page points.
    pub y_pt: f64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LineTraceDraft {
    pub paragraph: ParagraphPropertyTrace,
    pub ascent_pt: f64,
    pub descent_pt: f64,
    pub natural_height_pt: f64,
    pub text_natural_height_pt: f64,
    pub run_metrics: Vec<RunMetricTrace>,
    pub object_ink_bounds: Vec<InkBoundsTrace>,
    pub math_ink_bounds: Vec<InkBoundsTrace>,
}

impl From<&Spacing> for SpacingTrace {
    fn from(spacing: &Spacing) -> Self {
        let (line_rule, line_value) = match spacing.line {
            Some(LineSpacing::Auto(value)) => (Some("auto".to_string()), Some(value)),
            Some(LineSpacing::AtLeast(value)) => (Some("at_least".to_string()), Some(value)),
            Some(LineSpacing::Exact(value)) => (Some("exact".to_string()), Some(value)),
            None => (None, None),
        };
        Self {
            before_pt: spacing.before_pt,
            after_pt: spacing.after_pt,
            line_rule,
            line_value,
        }
    }
}

impl From<&Indent> for IndentTrace {
    fn from(indent: &Indent) -> Self {
        Self {
            left_pt: indent.left_pt,
            right_pt: indent.right_pt,
            first_line_pt: indent.first_line_pt,
            hanging_pt: indent.hanging_pt,
        }
    }
}

impl From<NumRef> for NumberingReferenceTrace {
    fn from(numbering: NumRef) -> Self {
        Self {
            num_id: numbering.num_id,
            ilvl: numbering.ilvl,
        }
    }
}

impl From<&TabStop> for TabStopTrace {
    fn from(tab: &TabStop) -> Self {
        Self {
            pos_pt: tab.pos_pt,
            alignment: format!("{:?}", tab.alignment).to_ascii_lowercase(),
            leader: format!("{:?}", tab.leader).to_ascii_lowercase(),
        }
    }
}

pub(crate) fn tab_traces(tabs: &[TabStop]) -> Vec<TabStopTrace> {
    tabs.iter().map(TabStopTrace::from).collect()
}

pub(crate) fn glyph_origins(
    items: &[LineItem],
    origin_x: f64,
    origin_y: f64,
) -> Vec<GlyphOriginTrace> {
    let mut origins = Vec::new();
    for item in items {
        let Item::Glyphs(run) = &item.item else {
            continue;
        };
        let mut cursor = origin_x + item.x_pt;
        for glyph in &run.glyphs {
            if glyph.y_offset_pt > -1.0e50 {
                origins.push(GlyphOriginTrace {
                    face_id: run.face.0,
                    gid: glyph.gid,
                    unicode: glyph.ch.to_string(),
                    x_pt: cursor + glyph.x_offset_pt,
                    y_pt: origin_y + item.y_pt - glyph.y_offset_pt,
                });
            }
            cursor += glyph.advance_pt;
        }
    }
    origins
}

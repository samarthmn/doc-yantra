//! Paragraph line layout: resolve fonts, shape runs, break into lines, align,
//! and decorate. Produces a [`ParaLayout`] of lines that [`super::flow`] places
//! into pages. Line height uses real OS/2 Windows metrics (the parity lever);
//! super/sub factors and decoration offsets are tuned by the fidelity harness.

use super::assemble::WordDoc;
use super::numbering_layout::{self, PlacedLabel};
use super::{
    Decor, GlyphRun, ImageId, Item, LinkTarget, PlacedGlyph, SemanticCluster, SemanticRun, Stroke,
};
#[cfg(feature = "fidelity-diagnostics")]
use crate::diagnostics::{
    IndentTrace, InkBoundsTrace, LineTraceDraft, NumberingLayoutTrace, ParagraphPropertyTrace,
    tab_traces,
};
use crate::fonts::{FaceId, FontDb, FontSlot, matching};
use crate::oxml::numbering::LabelJustification;
use crate::oxml::numbering::LevelSuffix;
use crate::oxml::wml::{
    AnchorOrigin, AnchorProps, BreakKind, Field, FrameAlign, InlineContent, Jc, LineSpacing,
    NoteKind, Paragraph, Run, RunColor, RunContent, RunProps, TabAlign, TabLeader, TabStop,
    TextBox, UnderlineKind, VertAlign, VmlShape, WrapMode,
};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::text::shape;
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

pub(crate) type NoteSequence = HashMap<(NoteKind, i64), u32>;

/// Shaped intrinsic widths used by Word AutoFit table measurement.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct IntrinsicWidths {
    pub(crate) min_content_pt: f64,
    pub(crate) max_content_pt: f64,
}

/// Field substitution context (page numbering) for headers/footers.
#[derive(Debug, Clone)]
pub struct FieldContext {
    /// The current 1-based page number.
    pub page: u32,
    /// The total page count.
    pub total: u32,
    /// PAGE field text after applying section `pgNumType@fmt`.
    pub page_label: String,
}

/// Word's default size when unresolved (points).
const DEFAULT_SIZE_PT: f64 = 11.0;
/// Super/subscript size factor.
const SCRIPT_SCALE: f64 = 0.65;
/// Super/subscript baseline rise when a face has no OS/2 script metrics.
const SCRIPT_RISE_FALLBACK: f64 = 0.35;
/// Security envelope for authored tab-leader gaps. This is an allocation cap,
/// not a visual calibration constant: a ten-page horizontal gap is never a
/// useful paragraph layout request.
pub(crate) const MAX_TAB_LEADER_GAP_PT: f64 = 1440.0;
/// Defense-in-depth cap for the number of extraction-visible leader glyphs in
/// one tab. Kept separate from the physical-gap envelope for unusually narrow
/// authored fonts.
const MAX_TAB_LEADER_GLYPHS: usize = 4096;

#[derive(Debug, Clone, Copy)]
enum NumberingGeometryOrigin {
    DirectParagraph,
    ParagraphStyle,
    NumberingLevel,
    InheritedStyle,
    Default,
}

impl NumberingGeometryOrigin {
    #[cfg(feature = "fidelity-diagnostics")]
    fn trace_name(self) -> &'static str {
        match self {
            Self::DirectParagraph => "direct_paragraph",
            Self::ParagraphStyle => "paragraph_style",
            Self::NumberingLevel => "numbering_level",
            Self::InheritedStyle => "inherited_style",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(feature = "fidelity-diagnostics"), allow(dead_code))]
struct ResolvedNumberingGeometry {
    label_alignment_box_start_pt: f64,
    label_alignment_box_end_pt: f64,
    label_ink_end_pt: f64,
    following_text_start_pt: f64,
    left_origin: NumberingGeometryOrigin,
    hanging_origin: NumberingGeometryOrigin,
    tab_origin: NumberingGeometryOrigin,
}

pub(crate) const TAB_LEADER_GAP_LIMIT_DETAIL: &str = "tab leader gap exceeds 1440pt safety limit";

/// The laid-out lines of one paragraph, plus flow-control flags.
#[derive(Debug, Default)]
pub struct ParaLayout {
    /// Space above the paragraph.
    pub space_before_pt: f64,
    /// Space below the paragraph.
    pub space_after_pt: f64,
    /// Laid-out lines.
    pub lines: Vec<Line>,
    /// Keep with the next paragraph.
    pub keep_next: bool,
    /// Keep all lines together.
    pub keep_lines: bool,
    /// Widow/orphan control (default on).
    pub widow_control: bool,
    /// Force a page break before.
    pub page_break_before: bool,
    /// Outline level (0–2) for bookmarks.
    pub outline_level: Option<u8>,
    /// Style id (to detect contextual-spacing neighbours).
    pub style_id: Option<String>,
    /// Contextual spacing suppresses space between same-style paragraphs.
    pub contextual_spacing: bool,
    /// First-line text (for the outline title).
    pub text: String,
    /// Notes referenced by this paragraph, in document order.
    pub note_refs: Vec<(NoteKind, i64)>,
    /// Rectangular float exclusion that continues into the next paragraph.
    pub(crate) wrap_after: Option<WrapExclusion>,
    /// Word float-line clearance added before this paragraph's first line.
    pub(crate) float_leading_pt: f64,
    /// Page-scoped floats carried by this paragraph's first flow unit.
    pub(crate) page_floats: Vec<PageFloat>,
}

/// One edge of a rectangular exclusion carried across paragraphs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExclusionBand {
    /// Stable page-float identity used to deduplicate a scheduled band from
    /// the same band already advanced through preceding blocks.
    pub source: Option<(usize, usize)>,
    /// Remaining distance to the top of the exclusion.
    pub top_remaining_pt: f64,
    /// Remaining vertical extent from the next paragraph's content top.
    pub remaining_pt: f64,
    /// Width unavailable from this content edge.
    pub inset_pt: f64,
    /// Tight wrapping follows the text baseline through its polygon, while
    /// square wrapping excludes the complete line box.
    pub uses_text_baseline: bool,
}

/// Edge-aligned rectangular exclusions; both edges may be active together.
#[derive(Debug, Clone, Default)]
pub(crate) struct WrapExclusion {
    pub(crate) left_bands: Vec<ExclusionBand>,
    pub(crate) right_bands: Vec<ExclusionBand>,
}

#[derive(Debug, Clone, Copy)]
struct EdgeWrapBand {
    source: Option<(usize, usize)>,
    top_pt: f64,
    bottom_pt: f64,
    inset_pt: f64,
    uses_text_baseline: bool,
}

#[derive(Debug, Clone, Default)]
struct WrapBand {
    left: Vec<EdgeWrapBand>,
    right: Vec<EdgeWrapBand>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PageHorizontalContext {
    pub(crate) content_left_pt: f64,
    pub(crate) page_width_pt: f64,
}

impl EdgeWrapBand {
    fn applies_at(self, top: f64, line_height_pt: f64, baseline_offset_pt: f64) -> bool {
        if self.uses_text_baseline {
            let baseline = top + baseline_offset_pt;
            baseline > self.top_pt + 0.01 && baseline < self.bottom_pt - 0.01
        } else {
            let bottom = top + line_height_pt;
            top < self.bottom_pt - 0.01 && bottom > self.top_pt + 0.01
        }
    }

    fn applies_to_line(
        self,
        line_index: usize,
        line_height_pt: f64,
        baseline_offset_pt: f64,
    ) -> bool {
        let top = line_index as f64 * line_height_pt;
        self.applies_at(top, line_height_pt, baseline_offset_pt)
    }
}

impl WrapBand {
    fn from_exclusion(wrap: WrapExclusion) -> Self {
        let convert = |band: ExclusionBand| EdgeWrapBand {
            source: band.source,
            top_pt: band.top_remaining_pt.max(0.0),
            bottom_pt: band.remaining_pt,
            inset_pt: band.inset_pt,
            uses_text_baseline: band.uses_text_baseline,
        };
        Self {
            left: wrap
                .left_bands
                .into_iter()
                .filter(|band| band.remaining_pt > 0.01)
                .map(convert)
                .collect(),
            right: wrap
                .right_bands
                .into_iter()
                .filter(|band| band.remaining_pt > 0.01)
                .map(convert)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ScheduledLineConstraint {
    clearance_pt: f64,
    left_inset_pt: f64,
    right_inset_pt: f64,
}

fn scheduled_line_constraint(
    wrap: Option<&WrapBand>,
    avail_width_pt: f64,
    line_height_pt: f64,
    baseline_offset_pt: f64,
) -> ScheduledLineConstraint {
    let Some(wrap) = wrap else {
        return ScheduledLineConstraint::default();
    };
    let mut full_bands = wrap
        .left
        .iter()
        .copied()
        .filter(|band| band.inset_pt >= avail_width_pt - 0.01)
        .collect::<Vec<_>>();
    full_bands.sort_by(|left, right| {
        left.top_pt
            .partial_cmp(&right.top_pt)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut clearance_pt = 0.0_f64;
    for band in full_bands {
        if band.applies_at(clearance_pt, line_height_pt, baseline_offset_pt) {
            clearance_pt = clearance_pt.max(band.bottom_pt);
        }
    }
    let applies = |band: &&EdgeWrapBand| {
        band.inset_pt < avail_width_pt - 0.01
            && band.applies_at(clearance_pt, line_height_pt, baseline_offset_pt)
    };
    ScheduledLineConstraint {
        clearance_pt,
        left_inset_pt: wrap
            .left
            .iter()
            .filter(applies)
            .map(|band| band.inset_pt)
            .fold(0.0, f64::max),
        right_inset_pt: wrap
            .right
            .iter()
            .filter(applies)
            .map(|band| band.inset_pt)
            .fold(0.0, f64::max),
    }
}

fn resolve_paragraph_anchor_x(
    anchor: AnchorProps,
    width_pt: f64,
    content_width_pt: f64,
    page: Option<PageHorizontalContext>,
) -> f64 {
    let (reference_width, relative_shift) = if anchor.relative_from_h == AnchorOrigin::Page {
        page.map_or((content_width_pt, 0.0), |page| {
            (page.page_width_pt, page.content_left_pt)
        })
    } else {
        (content_width_pt, 0.0)
    };
    let resolved = match anchor.x_align {
        Some(FrameAlign::Start) => 0.0,
        Some(FrameAlign::Center) => (reference_width - width_pt).max(0.0) / 2.0,
        Some(FrameAlign::End) => (reference_width - width_pt).max(0.0),
        None => anchor.x_pt,
    };
    resolved - relative_shift
}

/// One laid-out line: items positioned relative to the line's top-left.
#[derive(Debug, Default)]
pub struct Line {
    /// Line box height.
    pub height_pt: f64,
    /// Items (`x` relative to content-left, `y` relative to line-top; glyph `y`
    /// is the baseline).
    pub items: Vec<LineItem>,
    /// Anchored items whose page-level paint order is resolved by flow.
    pub(crate) anchored_items: Vec<AnchoredLineItem>,
    /// Internal bookmark names whose zero-width marker falls on this line.
    pub(crate) bookmarks: Vec<String>,
    /// Internal hyperlink rectangles whose targets are resolved after pagination.
    pub(crate) internal_links: Vec<InternalLinkRect>,
    /// An explicit page break (`<w:br w:type="page"/>`) ends this line — the flow
    /// must start the next line on a new page.
    pub page_break_after: bool,
    /// A `w:br type="column"` follows this line.
    pub column_break_after: bool,
    /// Auto line-spacing growth below the natural text box. Word paginates
    /// on the ink bottom, letting this trailing leading overflow the
    /// content bottom for the last line of a page.
    pub bottom_slack_pt: f64,
    #[cfg(feature = "fidelity-diagnostics")]
    pub(crate) diagnostics: LineTraceDraft,
}

/// An item within a [`Line`], positioned relative to the line's top-left.
#[derive(Debug)]
pub struct LineItem {
    /// X relative to the content-left edge.
    pub x_pt: f64,
    /// Y relative to the line top (baseline for glyph runs).
    pub y_pt: f64,
    /// The primitive.
    pub item: Item,
}

/// An anchored item within a [`Line`], kept out of the public primitive enum so
/// page-layer metadata cannot leak into a completed [`super::LaidOutDocument`].
#[derive(Debug)]
pub(crate) struct AnchoredLineItem {
    pub(crate) x_pt: f64,
    pub(crate) y_pt: f64,
    pub(crate) behind_doc: bool,
    pub(crate) relative_height: u32,
    pub(crate) item: Item,
}

/// A line-local internal hyperlink, deliberately kept out of public [`Item`].
#[derive(Debug, Clone)]
pub(crate) struct InternalLinkRect {
    pub(crate) x_pt: f64,
    pub(crate) y_pt: f64,
    pub(crate) w_pt: f64,
    pub(crate) h_pt: f64,
    pub(crate) target: String,
}

/// A margin/page-relative float whose placement is resolved during pagination.
#[derive(Debug)]
pub(crate) struct PageFloat {
    pub(crate) items: Vec<LineItem>,
    pub(crate) w_pt: f64,
    pub(crate) h_pt: f64,
    pub(crate) x_origin: AnchorOrigin,
    pub(crate) x_pt: f64,
    pub(crate) x_align: Option<FrameAlign>,
    pub(crate) y_origin: AnchorOrigin,
    pub(crate) y_pt: f64,
    pub(crate) y_align: Option<FrameAlign>,
    pub(crate) wrap: WrapMode,
    pub(crate) distances_pt: [f64; 4],
    pub(crate) behind_doc: bool,
    pub(crate) relative_height: u32,
}

/// Shared per-run style. Glyphs from one run share an `Rc` so coalescing is a
/// pointer comparison and hyperlink strings are not cloned per glyph.
#[derive(Debug, Clone)]
struct RunStyle {
    face: FaceId,
    size_pt: f64,
    color: Rgba,
    background: Option<Rgba>,
    underline: Option<Decor>,
    strike: Option<Decor>,
    rise_pt: f64,
    ascent_pt: f64,
    line_height_pt: f64,
    line_box_size_pt: f64,
    line_box_ascent_pt: f64,
    line_box_height_pt: f64,
    link: Option<RunLink>,
}

#[derive(Debug, Clone)]
enum RunLink {
    Public(LinkTarget),
    Internal(String),
}

#[derive(Debug, Clone)]
struct AtomSemanticCluster {
    group_id: usize,
    source_order: usize,
    style: Rc<RunStyle>,
    base_direction: text_core::Direction,
    cluster: SemanticCluster,
}

struct LeaderGlyph {
    gid: u16,
    advance_pt: f64,
    x_offset_pt: f64,
    y_offset_pt: f64,
    ch: char,
    style: Rc<RunStyle>,
}

/// An atom in the pre-break stream.
enum Atom {
    Bookmark(String),
    Glyph {
        gid: u16,
        advance_pt: f64,
        x_offset_pt: f64,
        y_offset_pt: f64,
        ch: char,
        is_space: bool,
        style: Rc<RunStyle>,
        semantic: Vec<AtomSemanticCluster>,
    },
    /// A tab plus extraction-visible glyph candidates in the tab run's style.
    Tab {
        leaders: Vec<(TabLeader, LeaderGlyph)>,
    },
    Break(BreakKind),
    Image {
        image_id: ImageId,
        w_pt: f64,
        h_pt: f64,
        link: Option<RunLink>,
    },
    Composite {
        w_pt: f64,
        h_pt: f64,
        baseline_pt: Option<f64>,
        items: Vec<LineItem>,
        anchor: Option<AnchorProps>,
    },
}

impl Atom {
    fn advance(&self) -> f64 {
        match self {
            Atom::Glyph { advance_pt, .. } => *advance_pt,
            Atom::Image { w_pt, .. } => *w_pt,
            Atom::Composite {
                w_pt, anchor: None, ..
            } => *w_pt,
            Atom::Composite { .. } => 0.0,
            _ => 0.0,
        }
    }
    fn is_space(&self) -> bool {
        matches!(self, Atom::Glyph { is_space: true, .. })
    }

    fn allows_line_break_after(&self) -> bool {
        self.is_space()
            || matches!(
                self,
                Atom::Glyph {
                    ch: '-' | '\u{2010}',
                    ..
                }
            )
    }

    fn glyph_char(&self) -> Option<char> {
        match self {
            Atom::Glyph { ch, .. } => Some(*ch),
            _ => None,
        }
    }
}

/// Whether a line may break between `current` and `next`, per Word's East
/// Asian defaults: ideographs, kana, and hangul break anywhere except where
/// kinsoku forbids it (no closing punctuation at a line start, no opening
/// bracket at a line end).
fn allows_line_break_between(current: &Atom, next: Option<&Atom>) -> bool {
    if next
        .and_then(Atom::glyph_char)
        .is_some_and(forbids_line_start)
    {
        return false;
    }
    if current.allows_line_break_after() {
        return true;
    }
    current
        .glyph_char()
        .is_some_and(|ch| cjk_breaks_after(ch) && !forbids_line_end(ch))
}

/// East Asian characters Word wraps without inter-word spaces.
fn cjk_breaks_after(ch: char) -> bool {
    matches!(u32::from(ch),
        0x3000..=0x303F      // CJK symbols and punctuation
        | 0x3040..=0x30FF    // hiragana, katakana
        | 0x31F0..=0x31FF    // katakana phonetic extensions
        | 0x3400..=0x9FFF    // CJK ideographs
        | 0xAC00..=0xD7AF    // hangul syllables
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0xFF00..=0xFFEF    // full-width forms
    )
}

/// Kinsoku 行頭禁則: characters that must not start a line. Restricted to
/// East Asian punctuation so Latin wrapping keeps its calibrated behavior.
fn forbids_line_start(ch: char) -> bool {
    matches!(
        ch,
        '、' | '。'
            | '，'
            | '．'
            | '：'
            | '；'
            | '！'
            | '？'
            | '％'
            | '）'
            | '］'
            | '｝'
            | '〉'
            | '》'
            | '」'
            | '』'
            | '】'
            | '〕'
            | '〗'
            | '〙'
            | '〛'
            | '々'
            | '〜'
            | 'ー'
            | '・'
            | '゛'
            | '゜'
            | 'ゝ'
            | 'ゞ'
            | 'ヽ'
            | 'ヾ'
            | 'ぁ'
            | 'ぃ'
            | 'ぅ'
            | 'ぇ'
            | 'ぉ'
            | 'っ'
            | 'ゃ'
            | 'ゅ'
            | 'ょ'
            | 'ゎ'
            | 'ァ'
            | 'ィ'
            | 'ゥ'
            | 'ェ'
            | 'ォ'
            | 'ッ'
            | 'ャ'
            | 'ュ'
            | 'ョ'
            | 'ヮ'
            | 'ヵ'
            | 'ヶ'
            | '｡'
            | '｣'
            | '､'
            | '･'
            | 'ｰ'
            | '～'
            | '‰'
            | '℃'
    )
}

/// Kinsoku 行末禁則: opening punctuation that must not end a line.
fn forbids_line_end(ch: char) -> bool {
    matches!(
        ch,
        '（' | '［'
            | '｛'
            | '〈'
            | '《'
            | '「'
            | '『'
            | '【'
            | '〔'
            | '〖'
            | '〘'
            | '〚'
            | '｢'
            | '￥'
            | '＄'
    )
}

/// Lays out `para` into lines within `avail_width_pt`.
pub fn layout_paragraph(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    avail_width_pt: f64,
    label_override: Option<PlacedLabel>,
    field_ctx: Option<FieldContext>,
) -> Result<ParaLayout> {
    layout_paragraph_with_note_sequence(
        doc,
        warnings,
        para,
        rels,
        avail_width_pt,
        label_override,
        field_ctx,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_paragraph_with_note_sequence(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    avail_width_pt: f64,
    label_override: Option<PlacedLabel>,
    field_ctx: Option<FieldContext>,
    note_sequence: Option<&NoteSequence>,
) -> Result<ParaLayout> {
    layout_paragraph_with_wrap(
        doc,
        warnings,
        para,
        rels,
        avail_width_pt,
        label_override,
        field_ctx,
        None,
        note_sequence,
    )
}

/// Lays out a body paragraph while honoring one active edge float.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_paragraph_with_wrap(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    avail_width_pt: f64,
    label_override: Option<PlacedLabel>,
    field_ctx: Option<FieldContext>,
    incoming_wrap: Option<WrapExclusion>,
    note_sequence: Option<&NoteSequence>,
) -> Result<ParaLayout> {
    layout_paragraph_with_wrap_context(
        doc,
        warnings,
        para,
        rels,
        avail_width_pt,
        label_override,
        field_ctx,
        incoming_wrap,
        None,
        Vec::new(),
        note_sequence,
    )
}

/// Lays out a body paragraph with the page box available for page-relative
/// horizontal paragraph anchors.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_paragraph_with_page_wrap(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    avail_width_pt: f64,
    label_override: Option<PlacedLabel>,
    field_ctx: Option<FieldContext>,
    incoming_wrap: Option<WrapExclusion>,
    page_horizontal: PageHorizontalContext,
    scheduled_line_wraps: Vec<Option<WrapExclusion>>,
    note_sequence: Option<&NoteSequence>,
) -> Result<ParaLayout> {
    layout_paragraph_with_wrap_context(
        doc,
        warnings,
        para,
        rels,
        avail_width_pt,
        label_override,
        field_ctx,
        incoming_wrap,
        Some(page_horizontal),
        scheduled_line_wraps,
        note_sequence,
    )
}

#[allow(clippy::too_many_arguments)]
fn layout_paragraph_with_wrap_context(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    avail_width_pt: f64,
    label_override: Option<PlacedLabel>,
    field_ctx: Option<FieldContext>,
    incoming_wrap: Option<WrapExclusion>,
    page_horizontal: Option<PageHorizontalContext>,
    scheduled_line_wraps: Vec<Option<WrapExclusion>>,
    note_sequence: Option<&NoteSequence>,
) -> Result<ParaLayout> {
    let eff_para = doc.styles.para_props(&para.props);
    if let Some(detail) = eff_para.borders.unsupported_detail(&doc.theme) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::UnknownElement,
            detail,
        ));
    }
    // Flow supplies a counter-accurate label; otherwise fall back to a
    // first-item approximation.
    let label = match label_override {
        Some(label) => Some(label),
        None => numbering_layout::resolve_label(doc, warnings, &eff_para, para)?,
    };

    // Word resolves list indentation as direct pPr > the named style's own
    // indent > numbering level > inherited style chain: an indent declared
    // on the paragraph's style itself outranks the list level (chinese
    // claims ground truth), while one merely inherited through basedOn
    // (e.g. ListParagraph's 720) must not shadow the numbering definition.
    let own_style_indent = para
        .props
        .style_id
        .as_deref()
        .and_then(|id| doc.styles.get(id))
        .map(|style| &style.ppr.indent);
    let label_left = label.as_ref().and_then(|l| l.left_pt);
    let label_hanging = label.as_ref().and_then(|l| l.hanging_pt);
    let direct_indent = &para.props.indent;
    let (left, left_origin) = if let Some(value) = direct_indent.left_pt {
        (value, NumberingGeometryOrigin::DirectParagraph)
    } else if let Some(value) = own_style_indent.and_then(|indent| indent.left_pt) {
        (value, NumberingGeometryOrigin::ParagraphStyle)
    } else if let Some(value) = label_left {
        (value, NumberingGeometryOrigin::NumberingLevel)
    } else if let Some(value) = eff_para.indent.left_pt {
        (value, NumberingGeometryOrigin::InheritedStyle)
    } else {
        (0.0, NumberingGeometryOrigin::Default)
    };
    let left = left.max(0.0);
    let right_indent = eff_para.indent.right_pt.unwrap_or(0.0).max(0.0);
    let right_border = eff_para
        .borders
        .right
        .as_ref()
        .filter(|border| !matches!(border.val.as_str(), "nil" | "none"));
    let right = right_indent;
    // `firstLine`/`hanging` share one logical slot, so the pair resolves
    // from a single source at each precedence step.
    let own_style_pair = own_style_indent
        .filter(|indent| indent.first_line_pt.is_some() || indent.hanging_pt.is_some());
    let (first_line, hanging, hanging_origin) =
        if direct_indent.first_line_pt.is_some() || direct_indent.hanging_pt.is_some() {
            (
                direct_indent.first_line_pt.unwrap_or(0.0),
                direct_indent.hanging_pt.unwrap_or(0.0),
                NumberingGeometryOrigin::DirectParagraph,
            )
        } else if let Some(own) = own_style_pair {
            (
                own.first_line_pt.unwrap_or(0.0),
                own.hanging_pt.unwrap_or(0.0),
                NumberingGeometryOrigin::ParagraphStyle,
            )
        } else if let Some(label_hanging) = label_hanging {
            (0.0, label_hanging, NumberingGeometryOrigin::NumberingLevel)
        } else {
            (
                eff_para.indent.first_line_pt.unwrap_or(0.0),
                eff_para.indent.hanging_pt.unwrap_or(0.0),
                if eff_para.indent.first_line_pt.is_some() || eff_para.indent.hanging_pt.is_some() {
                    NumberingGeometryOrigin::InheritedStyle
                } else {
                    NumberingGeometryOrigin::Default
                },
            )
        };

    // Numbering-level `pPr` sits between inherited/style properties and direct
    // paragraph formatting. Tab stops overlay by authored position; a `clear`
    // removes the lower-precedence stop at that position.
    let mut para_without_direct_tabs = para.props.clone();
    para_without_direct_tabs.tabs.clear();
    let inherited_tabs = doc.styles.para_props(&para_without_direct_tabs).tabs;
    let resolved_tabs = resolve_numbering_tabs(
        &inherited_tabs,
        label
            .as_ref()
            .map(|label| label.tabs.as_slice())
            .unwrap_or(&[]),
        &para.props.tabs,
    );
    let tab_stops = resolved_tabs
        .iter()
        .map(|resolved| resolved.stop)
        .collect::<Vec<_>>();
    validate_tab_leader_stops(&tab_stops)?;

    let numbering_geometry = label.as_ref().map(|label| {
        resolve_numbering_geometry(
            label,
            left,
            hanging,
            doc.settings.default_tab_stop_pt,
            &resolved_tabs,
            left_origin,
            hanging_origin,
        )
    });
    let first_offset = numbering_geometry.as_ref().map_or_else(
        || (first_line - hanging).max(-left),
        |geometry| (geometry.following_text_start_pt - left + first_line.max(0.0)).max(-left),
    );

    // A hanging indent moves the list label into the area before `left`; it
    // does not grant the first text line extra room at the right edge. Only a
    // positive first-line indent reduces that line's usable text width.
    let content_first = (avail_width_pt - left - right - first_offset.max(0.0)).max(1.0);
    let content_rest = (avail_width_pt - left - right).max(1.0);
    let atoms = flatten(doc, warnings, para, rels, field_ctx, note_sequence)?;
    let text: String = atoms
        .iter()
        .filter_map(|a| match a {
            Atom::Glyph { ch: '\0', .. } => None,
            Atom::Glyph { ch, .. } => Some(*ch),
            _ => None,
        })
        .collect();

    let rtl = eff_para.bidi == Some(true);
    let alignment = display_math_alignment(para).unwrap_or_else(|| {
        eff_para
            .alignment
            .unwrap_or(if rtl { Jc::End } else { Jc::Start })
    });
    let line_spacing = eff_para.spacing.line;
    let default_tab = doc.settings.default_tab_stop_pt;
    #[cfg(feature = "fidelity-diagnostics")]
    let diagnostic_paragraph = {
        let style_props = para
            .props
            .style_id
            .as_deref()
            .map(|id| doc.styles.diagnostic_style_para_props(id))
            .unwrap_or_default();
        let numbering = eff_para.numbering;
        let numbering_level =
            numbering.and_then(|numbering| doc.numbering.resolve(numbering.num_id, numbering.ilvl));
        ParagraphPropertyTrace {
            style_id: para.props.style_id.clone(),
            numbering: numbering.map(Into::into),
            direct_indent: IndentTrace::from(&para.props.indent),
            style_indent: IndentTrace::from(&style_props.indent),
            numbering_indent: numbering_level
                .as_ref()
                .map(|level| IndentTrace::from(&level.indent))
                .unwrap_or_default(),
            effective_indent: IndentTrace {
                left_pt: Some(left),
                right_pt: Some(right_indent),
                first_line_pt: Some(first_line),
                hanging_pt: Some(hanging),
            },
            direct_tabs: tab_traces(&para.props.tabs),
            style_tabs: tab_traces(&style_props.tabs),
            numbering_tabs: numbering_level
                .as_ref()
                .map(|level| tab_traces(&level.tabs))
                .unwrap_or_default(),
            effective_tabs: tab_traces(&tab_stops),
            numbering_layout: numbering_geometry.map(|geometry| NumberingLayoutTrace {
                label_alignment_box_start_pt: geometry.label_alignment_box_start_pt,
                label_alignment_box_end_pt: geometry.label_alignment_box_end_pt,
                label_ink_end_pt: geometry.label_ink_end_pt,
                following_text_start_pt: geometry.following_text_start_pt,
                left_origin: geometry.left_origin.trace_name().to_string(),
                hanging_origin: geometry.hanging_origin.trace_name().to_string(),
                tab_origin: geometry.tab_origin.trace_name().to_string(),
            }),
        }
    };

    let nominal_natural_height = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Glyph { style, .. } => Some(style.line_box_height_pt),
            _ => None,
        })
        .fold(0.0_f64, f64::max)
        .max(para_mark_height(doc, para, false));
    let nominal_line_height = apply_line_spacing(line_spacing, nominal_natural_height).max(0.01);
    let nominal_baseline_offset = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Glyph { style, .. } => Some(style.line_box_ascent_pt),
            _ => None,
        })
        .fold(0.0_f64, f64::max);
    let nominal_baseline_offset = if nominal_baseline_offset > 0.0 {
        nominal_baseline_offset.min(nominal_line_height)
    } else {
        nominal_line_height * 0.8
    };

    let anchored: Vec<(f64, f64, AnchorProps)> = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Composite {
                w_pt,
                h_pt,
                anchor: Some(anchor),
                ..
            } if anchor.relative_from_v == AnchorOrigin::Paragraph => Some((*w_pt, *h_pt, *anchor)),
            _ => None,
        })
        .collect();
    if anchored.len() > 1 {
        return Err(OfficeError::unsupported(
            UnsupportedCode::AnchoredDrawing,
            "multiple paragraph-relative drawings in one paragraph",
        ));
    }

    let page_floats = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Composite {
                w_pt,
                h_pt,
                items,
                anchor: Some(anchor),
                ..
            } if anchor.relative_from_v != AnchorOrigin::Paragraph => Some(PageFloat {
                items: items
                    .iter()
                    .map(|item| LineItem {
                        x_pt: item.x_pt,
                        y_pt: item.y_pt,
                        item: item.item.clone(),
                    })
                    .collect(),
                w_pt: *w_pt,
                h_pt: *h_pt,
                x_origin: anchor.relative_from_h,
                x_pt: anchor.x_pt,
                x_align: anchor.x_align,
                y_origin: anchor.relative_from_v,
                y_pt: anchor.y_pt,
                y_align: anchor.y_align,
                wrap: anchor.wrap,
                distances_pt: anchor.distances_pt,
                behind_doc: anchor.behind_doc,
                relative_height: anchor.relative_height,
            }),
            _ => None,
        })
        .collect();

    let mut wrap_band = incoming_wrap
        .map(WrapBand::from_exclusion)
        .filter(|wrap| !wrap.left.is_empty() || !wrap.right.is_empty());
    let scheduled_line_wraps = scheduled_line_wraps
        .into_iter()
        .map(|wrap| {
            wrap.map(WrapBand::from_exclusion)
                .filter(|wrap| !wrap.left.is_empty() || !wrap.right.is_empty())
        })
        .collect::<Vec<_>>();
    let mut full_width_bands = Vec::new();
    if let Some(band) = &mut wrap_band {
        let mut side_bands = Vec::new();
        for left in band.left.drain(..) {
            if left.inset_pt >= avail_width_pt - 0.01 {
                full_width_bands.push(left);
            } else {
                side_bands.push(left);
            }
        }
        band.left = side_bands;
    }
    if wrap_band
        .as_ref()
        .is_some_and(|band| band.left.is_empty() && band.right.is_empty())
    {
        wrap_band = None;
    }
    let mut top_bottom_reserve = None;
    let mut float_leading_pt = 0.0;
    if let Some((w_pt, h_pt, anchor)) = anchored.first().copied() {
        match anchor.wrap {
            WrapMode::Square | WrapMode::Tight => {
                if anchor.wrap == WrapMode::Square {
                    float_leading_pt = nominal_line_height;
                }
                if wrap_band.is_some() {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        "overlapping anchored drawing exclusions",
                    ));
                }
                let x_pt =
                    resolve_paragraph_anchor_x(anchor, w_pt, avail_width_pt, page_horizontal);
                let is_left_edge = x_pt <= 0.01;
                let is_right_edge = x_pt + w_pt >= avail_width_pt - 0.01;
                let [dist_top, dist_bottom, dist_left, dist_right] = anchor.distances_pt;
                let left_available = (x_pt - dist_left).max(0.0);
                let right_start = (x_pt + w_pt + dist_right).max(0.0);
                let right_available = (avail_width_pt - right_start).max(0.0);
                let use_right_interval =
                    !is_right_edge && (is_left_edge || right_available >= left_available);
                let anchor_y_for_wrap = anchor.y_pt
                    - if anchor.wrap == WrapMode::Square {
                        nominal_line_height
                    } else {
                        0.0
                    };
                let edge = EdgeWrapBand {
                    source: None,
                    top_pt: (anchor_y_for_wrap - dist_top).max(0.0),
                    bottom_pt: (anchor_y_for_wrap + h_pt + dist_bottom).max(0.0),
                    inset_pt: if use_right_interval {
                        right_start
                    } else {
                        (avail_width_pt - left_available).max(0.0)
                    },
                    uses_text_baseline: anchor.wrap == WrapMode::Tight,
                };
                wrap_band = Some(if use_right_interval {
                    WrapBand {
                        left: vec![edge],
                        right: Vec::new(),
                    }
                } else {
                    WrapBand {
                        left: Vec::new(),
                        right: vec![edge],
                    }
                });
            }
            WrapMode::TopAndBottom => {
                top_bottom_reserve = Some((
                    (anchor.y_pt - anchor.distances_pt[0]).max(0.0),
                    (anchor.y_pt + h_pt + anchor.distances_pt[1]).max(0.0),
                ));
            }
            WrapMode::None => {}
        }
    }

    let constrained_width = |line_index: usize, base_width: f64, base_x: f64| {
        let scheduled = scheduled_line_constraint(
            scheduled_line_wraps
                .get(line_index)
                .and_then(Option::as_ref),
            avail_width_pt,
            nominal_line_height,
            nominal_baseline_offset,
        );
        let left_inset = wrap_band
            .as_ref()
            .into_iter()
            .flat_map(|band| &band.left)
            .filter(|band| {
                band.applies_to_line(line_index, nominal_line_height, nominal_baseline_offset)
            })
            .map(|band| band.inset_pt)
            .fold(scheduled.left_inset_pt, f64::max);
        let right_inset = wrap_band
            .as_ref()
            .into_iter()
            .flat_map(|band| &band.right)
            .filter(|band| {
                band.applies_to_line(line_index, nominal_line_height, nominal_baseline_offset)
            })
            .map(|band| band.inset_pt)
            .fold(scheduled.right_inset_pt, f64::max);
        let positive_first = if line_index == 0 {
            first_offset.max(0.0)
        } else {
            0.0
        };
        let normal_left = base_x - positive_first;
        let constrained_x = normal_left.max(left_inset) + positive_first;
        let left_shift = (constrained_x - base_x).max(0.0);
        let right_cut = (right_inset - right).max(0.0);
        (
            (base_width - left_shift - right_cut).max(1.0),
            constrained_x,
            scheduled.clearance_pt,
        )
    };

    let broken = break_lines(
        &atoms,
        content_first,
        content_rest,
        default_tab,
        &tab_stops,
        |line_index, base_width| {
            let base_x = left + if line_index == 0 { first_offset } else { 0.0 };
            let (width, constrained_x, _) = constrained_width(line_index, base_width, base_x);
            (width, constrained_x)
        },
    );
    let n = broken.len();
    let mut lines = Vec::new();
    for (i, line_atoms) in broken.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i + 1 == n;
        let base_content_w = if is_first {
            content_first
        } else {
            content_rest
        };
        let initial_x = left + if is_first { first_offset } else { 0.0 };
        let (content_w, base_x, clearance_pt) = constrained_width(i, base_content_w, initial_x);
        let this_label = if is_first { label.as_ref() } else { None };
        let mut line = place_line(
            &atoms,
            line_atoms,
            content_w,
            avail_width_pt,
            base_x,
            alignment,
            line_spacing,
            is_last,
            default_tab,
            &tab_stops,
            this_label,
            if is_first {
                numbering_geometry.as_ref()
            } else {
                None
            },
            rtl,
            page_horizontal,
        )?;
        prepend_line_clearance(std::slice::from_mut(&mut line), clearance_pt);
        lines.push(line);
    }

    if lines.is_empty() {
        let h = para_mark_height(doc, para, true);
        let height_pt = apply_line_spacing(line_spacing, h);
        lines.push(Line {
            height_pt,
            items: Vec::new(),
            anchored_items: Vec::new(),
            bookmarks: Vec::new(),
            internal_links: Vec::new(),
            page_break_after: false,
            column_break_after: false,
            bottom_slack_pt: line_spacing_bottom_slack(line_spacing, height_pt, h),
            #[cfg(feature = "fidelity-diagnostics")]
            diagnostics: LineTraceDraft {
                ascent_pt: height_pt * 0.8,
                descent_pt: height_pt * 0.2,
                ..LineTraceDraft::default()
            },
        });
    }

    #[cfg(feature = "fidelity-diagnostics")]
    for line in &mut lines {
        line.diagnostics.paragraph = diagnostic_paragraph.clone();
    }

    // A paragraph-relative drawing is anchored to the paragraph, not the
    // broken line that happened to contain its source run. Carry it on the
    // first unit so its authored y offset is measured from the paragraph top.
    let mut paragraph_anchors = Vec::new();
    for line in &mut lines {
        paragraph_anchors.append(&mut line.anchored_items);
    }

    full_width_bands.sort_by(|left, right| {
        left.top_pt
            .partial_cmp(&right.top_pt)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for band in &full_width_bands {
        let occupied_before: f64 = lines.iter().map(|line| line.height_pt).sum();
        if occupied_before <= band.top_pt + 0.01 {
            continue;
        } else if band.top_pt <= 0.01 {
            prepend_line_clearance(&mut lines, band.bottom_pt.max(0.0));
        } else {
            let mut before = 0.0;
            let mut lines_before = 0usize;
            for line in &lines {
                if before + line.height_pt > band.top_pt + 0.01 {
                    break;
                }
                before += line.height_pt;
                lines_before += 1;
            }
            if lines_before == 0 {
                prepend_line_clearance(&mut lines, band.bottom_pt.max(0.0));
            } else if let Some(previous) = lines.get_mut(lines_before - 1) {
                previous.height_pt += (band.bottom_pt - before).max(0.0);
            }
        }
    }

    let top_bottom_reserve_extension = if let Some((band_top, band_bottom)) = top_bottom_reserve {
        let occupied_before: f64 = lines.iter().map(|line| line.height_pt).sum();
        apply_top_bottom_band(&mut lines, band_top, band_bottom);
        let occupied_after: f64 = lines.iter().map(|line| line.height_pt).sum();
        (occupied_after - occupied_before).max(0.0)
    } else {
        0.0
    };
    if let Some(first_line) = lines.first_mut() {
        first_line.anchored_items = paragraph_anchors;
    }

    if let Some(fill) = eff_para.shading.map(Rgba::opaque) {
        let external_right = right_border
            .map(|border| border.space_pt + border.size_pt)
            .unwrap_or(0.0);
        let width = (avail_width_pt - left - right_indent + external_right).max(0.0);
        for line in &mut lines {
            line.items.insert(
                0,
                LineItem {
                    x_pt: left,
                    y_pt: 0.0,
                    item: Item::Rect {
                        w_pt: width,
                        h_pt: line.height_pt,
                        fill: Some(fill),
                        stroke: None,
                    },
                },
            );
        }
    }

    if let Some(border) = eff_para
        .borders
        .bottom
        .as_ref()
        .filter(|border| !matches!(border.val.as_str(), "nil" | "none"))
        && let Some(last_line) = lines.last_mut()
    {
        let width_pt = border.size_pt.max(0.25);
        let border_y = last_line.height_pt + border.space_pt + width_pt / 2.0;
        last_line.items.push(LineItem {
            x_pt: left,
            y_pt: border_y,
            item: Item::Line {
                dx_pt: (avail_width_pt - left - right_indent).max(0.0),
                dy_pt: 0.0,
                stroke: Stroke {
                    width_pt,
                    color: Rgba::opaque(
                        border
                            .resolved_color(&doc.theme)
                            .unwrap_or(Rgb::new(0, 0, 0)),
                    ),
                    dash: None,
                },
            },
        });
        last_line.height_pt += border.space_pt + width_pt;
    }

    if let Some(border) = right_border {
        let paragraph_height: f64 = lines.iter().map(|line| line.height_pt).sum();
        let width_pt = border.size_pt;
        let border_x = (avail_width_pt - right_indent + border.space_pt + width_pt / 2.0).max(0.0);
        if let Some(first_line) = lines.first_mut() {
            first_line.items.push(LineItem {
                x_pt: border_x,
                y_pt: 0.0,
                item: Item::Line {
                    dx_pt: 0.0,
                    dy_pt: paragraph_height,
                    stroke: Stroke {
                        width_pt,
                        color: Rgba::opaque(
                            border
                                .resolved_color(&doc.theme)
                                .unwrap_or(Rgb::new(0, 0, 0)),
                        ),
                        dash: None,
                    },
                },
            });
        }
    }

    let occupied_height: f64 = lines.iter().map(|line| line.height_pt).sum();
    let occupied_for_bands = occupied_height;
    let carry = |edges: Vec<EdgeWrapBand>| {
        edges
            .into_iter()
            .filter_map(|edge| {
                let remaining_pt = edge.bottom_pt - occupied_for_bands;
                (remaining_pt > 0.01).then_some(ExclusionBand {
                    source: edge.source,
                    top_remaining_pt: (edge.top_pt - occupied_for_bands).max(0.0),
                    remaining_pt,
                    inset_pt: edge.inset_pt,
                    uses_text_baseline: edge.uses_text_baseline,
                })
            })
            .collect::<Vec<_>>()
    };
    let mut left_edges = wrap_band
        .as_ref()
        .map(|band| band.left.clone())
        .unwrap_or_default();
    left_edges.extend(full_width_bands);
    let right_edges = wrap_band.map(|band| band.right).unwrap_or_default();
    let wrap_after = WrapExclusion {
        left_bands: carry(left_edges),
        right_bands: carry(right_edges),
    };
    let wrap_after = (!wrap_after.left_bands.is_empty() || !wrap_after.right_bands.is_empty())
        .then_some(wrap_after);

    let authored_space_after = eff_para.spacing.after_pt.unwrap_or(0.0).max(0.0);
    Ok(ParaLayout {
        space_before_pt: eff_para.spacing.before_pt.unwrap_or(0.0).max(0.0),
        space_after_pt: (authored_space_after - top_bottom_reserve_extension).max(0.0),
        lines,
        keep_next: eff_para.keep_next,
        keep_lines: eff_para.keep_lines,
        widow_control: eff_para.widow_control.unwrap_or(true),
        page_break_before: eff_para.page_break_before,
        outline_level: eff_para.outline_lvl.filter(|&l| l <= 2),
        style_id: eff_para.style_id.clone(),
        contextual_spacing: eff_para.contextual_spacing,
        text,
        note_refs: paragraph_note_refs(para),
        wrap_after,
        float_leading_pt,
        page_floats,
    })
}

/// Rejects authored positive leader stops outside the bounded layout envelope.
/// Negative/no-leader/clear stops cannot cause leader materialization.
pub(crate) fn validate_tab_leader_stops(tab_stops: &[TabStop]) -> Result<()> {
    if tab_stops.iter().any(|stop| {
        stop.leader != TabLeader::None
            && stop.alignment != TabAlign::Clear
            && stop.pos_pt.is_finite()
            && stop.pos_pt > MAX_TAB_LEADER_GAP_PT
    }) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::InputTooLarge,
            TAB_LEADER_GAP_LIMIT_DETAIL,
        ));
    }
    Ok(())
}

fn tab_leader_count(gap_pt: f64, advance_pt: f64) -> Result<usize> {
    if !gap_pt.is_finite() || gap_pt > MAX_TAB_LEADER_GAP_PT {
        return Err(OfficeError::unsupported(
            UnsupportedCode::InputTooLarge,
            TAB_LEADER_GAP_LIMIT_DETAIL,
        ));
    }
    if gap_pt <= 0.0 || !advance_pt.is_finite() || advance_pt <= 0.0 {
        return Ok(0);
    }
    let mut count = 0usize;
    let mut width = advance_pt;
    while width <= gap_pt {
        count = count.checked_add(1).ok_or_else(|| {
            OfficeError::unsupported(UnsupportedCode::InputTooLarge, TAB_LEADER_GAP_LIMIT_DETAIL)
        })?;
        if count > MAX_TAB_LEADER_GLYPHS {
            return Err(OfficeError::unsupported(
                UnsupportedCode::InputTooLarge,
                "tab leader glyph count exceeds 4096 safety limit",
            ));
        }
        width += advance_pt;
    }
    Ok(count)
}

fn prepend_line_clearance(lines: &mut [Line], clearance_pt: f64) {
    if clearance_pt <= 0.01 {
        return;
    }
    if let Some(first) = lines.first_mut() {
        for item in &mut first.items {
            item.y_pt += clearance_pt;
        }
        for item in &mut first.anchored_items {
            item.y_pt += clearance_pt;
        }
        first.height_pt += clearance_pt;
        #[cfg(feature = "fidelity-diagnostics")]
        shift_line_diagnostics(first, clearance_pt);
    }
}

#[cfg(feature = "fidelity-diagnostics")]
fn shift_line_diagnostics(line: &mut Line, offset_pt: f64) {
    line.diagnostics.ascent_pt += offset_pt;
    for bounds in line
        .diagnostics
        .object_ink_bounds
        .iter_mut()
        .chain(line.diagnostics.math_ink_bounds.iter_mut())
    {
        bounds.y0_pt += offset_pt;
        bounds.y1_pt += offset_pt;
    }
}

/// Applies a paragraph-relative `wrapTopAndBottom` exclusion in paragraph
/// coordinates. Lines wholly above the band retain their authored positions;
/// the first intersecting line is cleared to the bottom. When every line is
/// above the band, the paragraph still occupies through the bottom so the next
/// block cannot overlap the drawing. Anchored items are deliberately not moved:
/// their y coordinates remain relative to the authored paragraph origin.
fn apply_top_bottom_band(lines: &mut [Line], top_pt: f64, bottom_pt: f64) {
    if bottom_pt <= 0.01 || lines.is_empty() {
        return;
    }
    let mut line_top = 0.0;
    for index in 0..lines.len() {
        let line_bottom = line_top + lines[index].height_pt;
        if line_bottom <= top_pt + 0.01 {
            line_top = line_bottom;
            continue;
        }

        let clearance = (bottom_pt - line_top).max(0.0);
        if clearance <= 0.01 {
            return;
        }
        if index == 0 {
            for item in &mut lines[0].items {
                item.y_pt += clearance;
            }
            for link in &mut lines[0].internal_links {
                link.y_pt += clearance;
            }
            lines[0].height_pt += clearance;
            #[cfg(feature = "fidelity-diagnostics")]
            shift_line_diagnostics(&mut lines[0], clearance);
        } else {
            lines[index - 1].height_pt += clearance;
        }
        return;
    }

    // Every text line ends before the exclusion begins. Preserve those line
    // positions while extending block occupancy through the drawing band.
    if let Some(last) = lines.last_mut() {
        last.height_pt += (bottom_pt - line_top).max(0.0);
    }
}

fn paragraph_note_refs(para: &Paragraph) -> Vec<(NoteKind, i64)> {
    let mut refs = Vec::new();
    let mut scan_run = |run: &Run| {
        for content in &run.content {
            if let RunContent::NoteReference { kind, id } = content {
                refs.push((*kind, *id));
            }
        }
    };
    fn scan_inlines(inlines: &[InlineContent], scan_run: &mut impl FnMut(&Run)) {
        for inline in inlines {
            match inline {
                InlineContent::Bookmark(_) => {}
                InlineContent::Run(run) => scan_run(run),
                InlineContent::Hyperlink { runs, .. } => runs.iter().for_each(&mut *scan_run),
                InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                    scan_inlines(&field.content, scan_run);
                }
                InlineContent::Math(_) => {}
            }
        }
    }
    scan_inlines(&para.runs, &mut scan_run);
    refs
}

/// Applies a [`LineSpacing`] rule to a natural line height.
pub fn apply_line_spacing(spacing: Option<LineSpacing>, natural: f64) -> f64 {
    match spacing {
        Some(LineSpacing::Auto(m)) => m * natural,
        Some(LineSpacing::AtLeast(v)) => v.max(natural),
        Some(LineSpacing::Exact(v)) => v,
        None => natural,
    }
}

/// Resolves a run's font to a face id, warning as needed.
fn resolve_face(doc: &WordDoc, warnings: &mut Vec<Warning>, props: &RunProps) -> Option<FaceId> {
    let family = matching::wml_run_family(
        &props.fonts,
        if props.rtl == Some(true) {
            FontSlot::Cs
        } else {
            FontSlot::Ascii
        },
        &doc.theme,
    );
    let weight = if props.bold == Some(true) { 700 } else { 400 };
    let italic = props.italic == Some(true);
    let resolved = matching::resolve(&doc.fonts, &family, weight, italic);
    for w in resolved.warnings {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    resolved.face
}

fn flatten(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    field_ctx: Option<FieldContext>,
    note_sequence: Option<&NoteSequence>,
) -> Result<Vec<Atom>> {
    let mut atoms = Vec::new();
    let context = FlattenContext {
        rels,
        note_sequence,
    };
    flatten_inlines(
        doc, warnings, para, &para.runs, &context, field_ctx, &mut atoms,
    )?;
    Ok(atoms)
}

/// Measures the paragraph with the same resolved run properties and shaper as
/// normal layout, without assigning it a line box. `min_content_pt` is the
/// widest breakable word/atom and `max_content_pt` is the widest explicit line
/// when left unwrapped.
pub(crate) fn measure_intrinsic_widths(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    note_sequence: Option<&NoteSequence>,
) -> Result<IntrinsicWidths> {
    let atoms = flatten(doc, warnings, para, rels, None, note_sequence)?;
    let eff_para = doc.styles.para_props(&para.props);
    let mut tab_stops = eff_para.tabs.clone();
    tab_stops.sort_by(|left, right| left.pos_pt.total_cmp(&right.pos_pt));

    let mut line_width = 0.0_f64;
    let mut trailing_space = 0.0_f64;
    let mut unit_width = 0.0_f64;
    let mut widest_line = 0.0_f64;
    let mut widest_unit = 0.0_f64;
    let finish_unit = |unit: &mut f64, widest: &mut f64| {
        *widest = (*widest).max(*unit);
        *unit = 0.0;
    };
    let finish_line = |line: &mut f64, trailing: &mut f64, widest: &mut f64| {
        *widest = (*widest).max((*line - *trailing).max(0.0));
        *line = 0.0;
        *trailing = 0.0;
    };

    for atom in &atoms {
        match atom {
            Atom::Break(_) => {
                finish_unit(&mut unit_width, &mut widest_unit);
                finish_line(&mut line_width, &mut trailing_space, &mut widest_line);
            }
            Atom::Tab { .. } => {
                finish_unit(&mut unit_width, &mut widest_unit);
                line_width =
                    next_tab_stop(line_width, doc.settings.default_tab_stop_pt, &tab_stops);
                trailing_space = 0.0;
            }
            Atom::Bookmark(_) => {}
            _ => {
                let advance = atom.advance();
                line_width += advance;
                if atom.is_space() {
                    finish_unit(&mut unit_width, &mut widest_unit);
                    trailing_space += advance;
                } else {
                    unit_width += advance;
                    trailing_space = 0.0;
                    if atom.allows_line_break_after() {
                        finish_unit(&mut unit_width, &mut widest_unit);
                    }
                }
            }
        }
    }
    finish_unit(&mut unit_width, &mut widest_unit);
    finish_line(&mut line_width, &mut trailing_space, &mut widest_line);

    let left = eff_para.indent.left_pt.unwrap_or(0.0).max(0.0);
    let right = eff_para.indent.right_pt.unwrap_or(0.0).max(0.0);
    let first = eff_para.indent.first_line_pt.unwrap_or(0.0).max(0.0);
    let horizontal_insets = left + right + first;
    let min_content_pt = widest_unit + horizontal_insets;
    let max_content_pt = (widest_line + horizontal_insets).max(min_content_pt);
    Ok(IntrinsicWidths {
        min_content_pt,
        max_content_pt,
    })
}

struct FlattenContext<'a> {
    rels: &'a HashMap<String, String>,
    note_sequence: Option<&'a NoteSequence>,
}

fn flatten_inlines(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    inlines: &[InlineContent],
    context: &FlattenContext<'_>,
    field_ctx: Option<FieldContext>,
    atoms: &mut Vec<Atom>,
) -> Result<()> {
    for inline in inlines {
        match inline {
            InlineContent::Bookmark(name) => atoms.push(Atom::Bookmark(name.clone())),
            InlineContent::Run(r) => flatten_run(doc, warnings, para, r, None, context, atoms)?,
            InlineContent::Hyperlink {
                rel_id,
                anchor,
                runs,
            } => {
                let target = link_target(doc, context.rels, rel_id.as_deref(), anchor.as_deref());
                for r in runs {
                    flatten_run(doc, warnings, para, r, target.clone(), context, atoms)?;
                }
            }
            InlineContent::SimpleField(f) | InlineContent::ComplexFieldRegion(f) => {
                if let Some(run) = substituted_field_run(f, field_ctx.clone()) {
                    flatten_run(doc, warnings, para, &run, None, context, atoms)?;
                } else {
                    flatten_inlines(
                        doc,
                        warnings,
                        para,
                        &f.content,
                        context,
                        field_ctx.clone(),
                        atoms,
                    )?;
                }
            }
            InlineContent::Math(math) => push_math_atom(doc, math, atoms)?,
        }
    }
    Ok(())
}

/// Resolves PAGE/NUMPAGES to a substituted run when a field context is present.
/// Every other field renders its cached inline content directly.
fn substituted_field_run(field: &Field, ctx: Option<FieldContext>) -> Option<Run> {
    let first = field
        .instruction
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let ctx = ctx?;
    let text = match first.as_str() {
        "PAGE" => ctx.page_label,
        "NUMPAGES" => ctx.total.to_string(),
        _ => return None,
    };
    let props = first_field_run(&field.content)
        .map(|run| run.props.clone())
        .unwrap_or_default();
    Some(Run {
        props,
        content: vec![RunContent::Text(text)],
    })
}

fn first_field_run(inlines: &[InlineContent]) -> Option<&Run> {
    for inline in inlines {
        match inline {
            InlineContent::Bookmark(_) => {}
            InlineContent::Run(run) => return Some(run),
            InlineContent::Hyperlink { runs, .. } => {
                if let Some(run) = runs.first() {
                    return Some(run);
                }
            }
            InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                if let Some(run) = first_field_run(&field.content) {
                    return Some(run);
                }
            }
            InlineContent::Math(_) => {}
        }
    }
    None
}

fn link_target(
    doc: &WordDoc,
    rels: &HashMap<String, String>,
    rel_id: Option<&str>,
    anchor: Option<&str>,
) -> Option<RunLink> {
    match rel_id {
        Some(rid) => doc
            .hyperlink_url(rels, rid)
            .map(LinkTarget::Url)
            .map(RunLink::Public),
        None => anchor.map(|name| RunLink::Internal(name.to_string())),
    }
}

fn flatten_run(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    run: &Run,
    link: Option<RunLink>,
    context: &FlattenContext<'_>,
    atoms: &mut Vec<Atom>,
) -> Result<()> {
    let rels = context.rels;
    let eff = doc.styles.run_props(&para.props, &run.props);
    if eff.vanish == Some(true) {
        return Ok(());
    }
    let Some(face) = resolve_face(doc, warnings, &eff) else {
        return Ok(());
    };
    let Some(font_face) = doc.fonts.face(face) else {
        return Ok(());
    };
    let Some(metrics) = font_face.line_metrics() else {
        return Ok(());
    };

    let base_size = eff.size_pt.unwrap_or(DEFAULT_SIZE_PT);
    let (size, rise) = match eff.vert_align {
        Some(VertAlign::Super) => (
            base_size * SCRIPT_SCALE,
            metrics
                .superscript_y_offset_pt(base_size)
                .unwrap_or(SCRIPT_RISE_FALLBACK * base_size),
        ),
        Some(VertAlign::Sub) => (
            base_size * SCRIPT_SCALE,
            -metrics
                .subscript_y_offset_pt(base_size)
                .unwrap_or(SCRIPT_RISE_FALLBACK * base_size),
        ),
        _ => (base_size, 0.0),
    };
    let (u_pos, u_thick) = metrics.underline_pt(size);
    let style = Rc::new(RunStyle {
        face,
        size_pt: size,
        color: resolve_color(&eff),
        background: eff.highlight.or(eff.shading).map(Rgba::opaque),
        underline: matches!(eff.underline, Some(u) if u != UnderlineKind::None).then(|| Decor {
            thickness_pt: u_thick.abs().max(0.4),
            y_offset_pt: (-u_pos).max(0.5),
        }),
        strike: (eff.strike == Some(true) || eff.dstrike == Some(true)).then(|| Decor {
            thickness_pt: u_thick.abs().max(0.4),
            y_offset_pt: -metrics.ascent_pt(size) * 0.32,
        }),
        rise_pt: rise,
        ascent_pt: metrics.ascent_pt(size),
        line_height_pt: metrics.line_height_pt(size),
        line_box_size_pt: base_size,
        line_box_ascent_pt: metrics.line_box_ascent_pt(base_size),
        line_box_height_pt: metrics.line_height_pt(base_size),
        link,
    });

    let char_spacing = eff.spacing_pt.unwrap_or(0.0);
    // Word only kerns runs carrying `w:kern` whose size clears the threshold; it is
    // off by default. Kerning unconditionally narrows text and shifts line breaks.
    let kern = eff.kern_min_pt.is_some_and(|min| size >= min);
    let base_direction =
        if eff.rtl == Some(true) || doc.styles.para_props(&para.props).bidi == Some(true) {
            text_core::Direction::Rtl
        } else {
            text_core::Direction::Ltr
        };
    let allow_direct_text = matches!(base_direction, text_core::Direction::Ltr);
    let fallback_ids = matching::coverage_fallbacks(&doc.fonts, face, weight(&eff), italic(&eff));
    let requested_family = matching::wml_run_family(
        &eff.fonts,
        if eff.rtl == Some(true) {
            FontSlot::Cs
        } else {
            FontSlot::Ascii
        },
        &doc.theme,
    );
    let halfwidth_ascii = matching::halfwidth_ascii_legacy_family(&requested_family);
    for content in &run.content {
        match content {
            RunContent::Text(t) => shape_text(
                &doc.fonts,
                &fallback_ids,
                t,
                size,
                char_spacing,
                kern,
                allow_direct_text,
                base_direction,
                halfwidth_ascii,
                &style,
                atoms,
            )?,
            RunContent::Tab => atoms.push(tab_atom(
                &doc.fonts,
                &fallback_ids,
                size,
                char_spacing,
                kern,
                allow_direct_text,
                base_direction,
                halfwidth_ascii,
                &style,
            )?),
            RunContent::Break(k) => atoms.push(Atom::Break(*k)),
            RunContent::Symbol { font, char_code } => {
                if let Some(ch) = matching::map_symbol_char(font, *char_code) {
                    shape_text(
                        &doc.fonts,
                        &fallback_ids,
                        &ch.to_string(),
                        size,
                        char_spacing,
                        kern,
                        allow_direct_text,
                        base_direction,
                        halfwidth_ascii,
                        &style,
                        atoms,
                    )?;
                }
            }
            RunContent::Drawing(d) => {
                if let Some(detail) = d.anchor_violations.first() {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        detail.clone(),
                    ));
                }
                if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_PICTURE)
                    && !d
                        .drawing
                        .extent_pt
                        .is_some_and(|(width, height)| width > 0.0 && height > 0.0)
                {
                    return Err(crate::OfficeError::unsupported(
                        crate::UnsupportedCode::MissingRelationshipTarget,
                        "picture drawing has no positive extent",
                    ));
                }
                if let Some(text_box) = &d.text_box {
                    atoms.push(Atom::Composite {
                        w_pt: text_box.width_pt,
                        h_pt: text_box.height_pt,
                        baseline_pt: None,
                        items: text_box_items(doc, warnings, text_box, rels)?,
                        anchor: d.anchor,
                    });
                } else if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_DIAGRAM)
                    && let Some((w, h)) = d.drawing.extent_pt
                    && let Some(rel_id) = &d.drawing.graphic_rel_id
                    && let Some(diagram) = doc.diagram(rels, rel_id)
                {
                    atoms.push(Atom::Composite {
                        w_pt: w,
                        h_pt: h,
                        baseline_pt: None,
                        items: diagram
                            .render(&doc.fonts, &doc.theme, w, h)?
                            .into_iter()
                            .map(|item| LineItem {
                                x_pt: item.x_pt,
                                y_pt: item.y_pt,
                                item: item.item,
                            })
                            .collect(),
                        anchor: d.anchor,
                    });
                } else if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_CHART)
                    && let Some((w, h)) = d.drawing.extent_pt
                    && let Some(rel_id) = &d.drawing.graphic_rel_id
                    && let Some(chart) = doc.chart(rels, rel_id)
                {
                    atoms.push(Atom::Composite {
                        w_pt: w,
                        h_pt: h,
                        baseline_pt: None,
                        items: chart
                            .render(&doc.fonts, &doc.theme, w, h)?
                            .into_iter()
                            .map(|item| LineItem {
                                x_pt: item.x_pt,
                                y_pt: item.y_pt,
                                item: item.item,
                            })
                            .collect(),
                        anchor: d.anchor,
                    });
                } else if let Some((w, h)) = d.drawing.extent_pt
                    && let Some(rel_id) = &d.drawing.blip_rel_id
                {
                    let image_id = doc.image_id_for_drawing(rels, &d.drawing).ok_or_else(|| {
                        crate::OfficeError::unsupported(
                            crate::UnsupportedCode::MissingRelationshipTarget,
                            format!("image relationship {rel_id} or its target is missing"),
                        )
                    })?;
                    if d.anchor.is_some() || d.crop.is_some() {
                        let image = if let Some(crop) = d.crop {
                            crate::drawing::image::validate_crop_fractions(crop)?;
                            Item::CroppedImage {
                                w_pt: w,
                                h_pt: h,
                                image_id,
                                crop,
                            }
                        } else {
                            Item::Image {
                                w_pt: w,
                                h_pt: h,
                                image_id,
                            }
                        };
                        atoms.push(Atom::Composite {
                            w_pt: w,
                            h_pt: h,
                            baseline_pt: None,
                            items: vec![LineItem {
                                x_pt: 0.0,
                                y_pt: 0.0,
                                item: image,
                            }],
                            anchor: d.anchor,
                        });
                    } else {
                        atoms.push(Atom::Image {
                            image_id,
                            w_pt: w,
                            h_pt: h,
                            link: style.link.clone(),
                        });
                    }
                } else if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_PICTURE) {
                    return Err(crate::OfficeError::unsupported(
                        crate::UnsupportedCode::MissingRelationshipTarget,
                        "picture drawing has no image relationship or extent",
                    ));
                }
            }
            RunContent::VmlShape(shape) => {
                atoms.push(Atom::Composite {
                    w_pt: shape.width_pt,
                    h_pt: shape.height_pt,
                    baseline_pt: None,
                    items: vml_items(doc, warnings, shape, rels)?,
                    anchor: Some(AnchorProps {
                        x_pt: shape.x_pt,
                        x_align: None,
                        y_pt: shape.y_pt,
                        y_align: None,
                        relative_from_h: crate::oxml::wml::AnchorOrigin::Paragraph,
                        relative_from_v: crate::oxml::wml::AnchorOrigin::Paragraph,
                        wrap: crate::oxml::wml::WrapMode::None,
                        distances_pt: [0.0; 4],
                        behind_doc: false,
                        relative_height: 0,
                    }),
                });
            }
            RunContent::OlePreview(shape) => {
                let rel_id = shape.image_rel_id.as_deref().ok_or_else(|| {
                    crate::OfficeError::unsupported(
                        crate::UnsupportedCode::OleObject,
                        "OLE object has no preview relationship",
                    )
                })?;
                let image_id = doc.image_id(rels, rel_id).ok_or_else(|| {
                    crate::OfficeError::unsupported(
                        crate::UnsupportedCode::MissingRelationshipTarget,
                        format!("OLE raster preview relationship {rel_id} is unavailable"),
                    )
                })?;
                atoms.push(Atom::Composite {
                    w_pt: shape.width_pt,
                    h_pt: shape.height_pt,
                    baseline_pt: None,
                    items: vec![LineItem {
                        x_pt: 0.0,
                        y_pt: 0.0,
                        item: Item::Image {
                            w_pt: shape.width_pt,
                            h_pt: shape.height_pt,
                            image_id,
                        },
                    }],
                    anchor: None,
                });
            }
            RunContent::Math(math) => push_math_atom(doc, math, atoms)?,
            RunContent::NoteReference { kind, id } => {
                let note_size = size * 0.75;
                let mut note_style = (*style).clone();
                note_style.size_pt = note_size;
                note_style.rise_pt += size * 0.35;
                note_style.line_height_pt = note_style.line_height_pt.max(size);
                shape_text(
                    &doc.fonts,
                    &fallback_ids,
                    &context
                        .note_sequence
                        .and_then(|sequence| sequence.get(&(*kind, *id)))
                        .copied()
                        .unwrap_or_else(|| u32::try_from(*id).unwrap_or_default())
                        .to_string(),
                    note_size,
                    char_spacing,
                    kern,
                    allow_direct_text,
                    base_direction,
                    halfwidth_ascii,
                    &Rc::new(note_style),
                    atoms,
                )?;
            }
            RunContent::NoteMarker { .. } => {
                return Err(OfficeError::Internal(
                    "unresolved inline note marker reached paragraph layout".into(),
                ));
            }
        }
    }
    Ok(())
}

/// The `m:jc` override for a paragraph that is a display equation: Word
/// positions such equations by the math-paragraph justification, not the
/// paragraph's own alignment. Only applies when the equation (plus
/// bookmarks) is the paragraph's entire inline content, so mixed text
/// keeps its authored paragraph alignment.
fn display_math_alignment(para: &Paragraph) -> Option<Jc> {
    let mut authored = None;
    for inline in &para.runs {
        match inline {
            InlineContent::Math(math) => {
                if math.display {
                    // An absent m:jc means Word's centerGroup default, so the
                    // equation centers even though the paragraph itself does
                    // not (`math_paragraph_alignment` maps None to center).
                    authored.get_or_insert(crate::oxml::wml::math_paragraph_alignment(math));
                } else {
                    return None;
                }
            }
            InlineContent::Bookmark(_) => {}
            _ => return None,
        }
    }
    authored
}

fn push_math_atom(
    doc: &WordDoc,
    math: &crate::oxml::omml::MathContent,
    atoms: &mut Vec<Atom>,
) -> Result<()> {
    let equation = super::math::layout_equation(doc, math, DEFAULT_SIZE_PT)?;
    atoms.push(Atom::Composite {
        w_pt: equation.width,
        h_pt: equation.height,
        baseline_pt: Some(equation.baseline),
        items: equation.items,
        anchor: None,
    });
    Ok(())
}

fn text_box_items(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    text_box: &TextBox,
    rels: &HashMap<String, String>,
) -> Result<Vec<LineItem>> {
    let mut items = vec![LineItem {
        x_pt: 0.0,
        y_pt: 0.0,
        item: Item::Rect {
            w_pt: text_box.width_pt,
            h_pt: text_box.height_pt,
            fill: text_box.fill.map(Rgba::opaque),
            stroke: text_box.outline.map(|color| Stroke {
                width_pt: 0.75,
                color: Rgba::opaque(color),
                dash: None,
            }),
        },
    }];
    let mut y = 4.0;
    let width = (text_box.width_pt - 8.0).max(1.0);
    let mut counters = numbering_layout::ListCounters::default();
    for block in &text_box.content {
        match block {
            crate::oxml::wml::BlockContent::Paragraph(paragraph) => {
                let layout = layout_paragraph(doc, warnings, paragraph, rels, width, None, None)?;
                if layout
                    .lines
                    .iter()
                    .any(|line| !line.anchored_items.is_empty())
                {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        "anchored drawing in a text box",
                    ));
                }
                y += layout.space_before_pt;
                for line in layout.lines {
                    for item in line.items {
                        items.push(LineItem {
                            x_pt: 4.0 + item.x_pt,
                            y_pt: y + item.y_pt,
                            item: item.item,
                        });
                    }
                    y += line.height_pt;
                }
                y += layout.space_after_pt;
            }
            crate::oxml::wml::BlockContent::Table(table) => {
                for row in super::table::layout_table(
                    doc,
                    warnings,
                    table,
                    rels,
                    width,
                    0.0,
                    &mut counters,
                )? {
                    for item in row.items {
                        items.push(LineItem {
                            x_pt: 4.0 + item.x_pt,
                            y_pt: y + item.y_pt,
                            item: item.item,
                        });
                    }
                    y += row.height_pt;
                }
            }
        }
    }
    Ok(items)
}

fn vml_items(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    shape: &VmlShape,
    rels: &HashMap<String, String>,
) -> Result<Vec<LineItem>> {
    let color = Rgba::opaque(shape.outline.unwrap_or(Rgb::new(0, 0, 0)));
    let stroke = Stroke {
        width_pt: 0.75,
        color,
        dash: None,
    };
    let mut items = Vec::new();
    match shape.kind.as_str() {
        "line" => items.push(LineItem {
            x_pt: 0.0,
            y_pt: 0.0,
            item: Item::Line {
                dx_pt: shape.width_pt,
                dy_pt: shape.height_pt,
                stroke: stroke.clone(),
            },
        }),
        "oval" => {
            let segments = 24;
            for index in 0..segments {
                let a = std::f64::consts::TAU * index as f64 / segments as f64;
                let b = std::f64::consts::TAU * (index + 1) as f64 / segments as f64;
                let x1 = shape.width_pt * (0.5 + 0.5 * a.cos());
                let y1 = shape.height_pt * (0.5 + 0.5 * a.sin());
                let x2 = shape.width_pt * (0.5 + 0.5 * b.cos());
                let y2 = shape.height_pt * (0.5 + 0.5 * b.sin());
                items.push(LineItem {
                    x_pt: x1,
                    y_pt: y1,
                    item: Item::Line {
                        dx_pt: x2 - x1,
                        dy_pt: y2 - y1,
                        stroke: stroke.clone(),
                    },
                });
            }
        }
        "shape" if shape.path.is_some() => {
            for (x1, y1, x2, y2) in simple_vml_path(shape.path.as_deref().unwrap_or_default())? {
                items.push(LineItem {
                    x_pt: x1 * shape.width_pt,
                    y_pt: y1 * shape.height_pt,
                    item: Item::Line {
                        dx_pt: (x2 - x1) * shape.width_pt,
                        dy_pt: (y2 - y1) * shape.height_pt,
                        stroke: stroke.clone(),
                    },
                });
            }
        }
        _ => items.push(LineItem {
            x_pt: 0.0,
            y_pt: 0.0,
            item: Item::Rect {
                w_pt: shape.width_pt,
                h_pt: shape.height_pt,
                fill: shape.fill.map(Rgba::opaque),
                stroke: Some(stroke),
            },
        }),
    }
    if let Some(rel_id) = &shape.image_rel_id {
        let image_id = doc.image_id(rels, rel_id).ok_or_else(|| {
            crate::OfficeError::unsupported(
                crate::UnsupportedCode::MissingRelationshipTarget,
                format!("VML image relationship {rel_id} or its target is missing"),
            )
        })?;
        items.push(LineItem {
            x_pt: 0.0,
            y_pt: 0.0,
            item: Item::Image {
                w_pt: shape.width_pt,
                h_pt: shape.height_pt,
                image_id,
            },
        });
    }
    if !shape.text_box.is_empty() {
        items.extend(text_box_items(
            doc,
            warnings,
            &TextBox {
                width_pt: shape.width_pt,
                height_pt: shape.height_pt,
                fill: None,
                outline: None,
                content: shape.text_box.clone(),
            },
            rels,
        )?);
    }
    Ok(items)
}

fn simple_vml_path(path: &str) -> Result<Vec<(f64, f64, f64, f64)>> {
    let normalized = path
        .replace(',', " ")
        .replace('m', " m ")
        .replace('l', " l ")
        .replace('e', " e ");
    let tokens: Vec<_> = normalized.split_whitespace().collect();
    let mut index = 0usize;
    let mut current = (0.0, 0.0);
    let mut segments = Vec::new();
    while index < tokens.len() {
        match tokens[index] {
            "m" => {
                let x = tokens
                    .get(index + 1)
                    .and_then(|value| value.parse::<f64>().ok());
                let y = tokens
                    .get(index + 2)
                    .and_then(|value| value.parse::<f64>().ok());
                let (Some(x), Some(y)) = (x, y) else {
                    break;
                };
                current = (x / 21_600.0, y / 21_600.0);
                index += 3;
            }
            "l" => {
                index += 1;
                while index + 1 < tokens.len() && !matches!(tokens[index], "m" | "l" | "e") {
                    let Some(x) = tokens[index].parse::<f64>().ok() else {
                        break;
                    };
                    let Some(y) = tokens[index + 1].parse::<f64>().ok() else {
                        break;
                    };
                    let next = (x / 21_600.0, y / 21_600.0);
                    segments.push((current.0, current.1, next.0, next.1));
                    current = next;
                    index += 2;
                }
            }
            "e" => break,
            _ => index += 1,
        }
    }
    if segments.is_empty() {
        return Err(crate::OfficeError::unsupported(
            crate::UnsupportedCode::UnknownElement,
            "v:shape path",
        ));
    }
    Ok(segments)
}

#[expect(
    clippy::too_many_arguments,
    reason = "shape inputs mirror resolved Word run properties"
)]
fn shape_text(
    fonts: &FontDb,
    face_ids: &[FaceId],
    text: &str,
    size: f64,
    char_spacing: f64,
    kern: bool,
    allow_direct_text: bool,
    base_direction: text_core::Direction,
    halfwidth_ascii: bool,
    style: &Rc<RunStyle>,
    atoms: &mut Vec<Atom>,
) -> Result<()> {
    let faces: Vec<_> = face_ids.iter().filter_map(|id| fonts.face(*id)).collect();
    let shaped = shape::require_glyphs(shape::shape_with_fallback(
        text,
        &faces,
        size,
        char_spacing,
        kern,
    ))?;
    let styles: Vec<Rc<RunStyle>> = face_ids
        .iter()
        .map(|face_id| fallback_style(fonts, *face_id, style))
        .collect();
    let mut mapped = crate::text::semantic::map_shaped_run_clusters_with_direct(
        text,
        &shaped,
        allow_direct_text,
    );
    // Word's fixed-pitch legacy CJK faces advance every printable ASCII
    // glyph exactly half an em; keep that metric under the pan-CJK
    // substitution so Latin fragments and AutoFit tables match Word.
    if halfwidth_ascii {
        for visible in &mut mapped.visible {
            if matches!(visible.ch, ' '..='~') {
                visible.advance_pt = size * 0.5 + char_spacing;
            }
        }
    }
    let semantic_style = styles
        .get(mapped.owner_face_index)
        .cloned()
        .unwrap_or_else(|| Rc::clone(style));
    let group_id = atoms.len();
    let mut semantic_by_owner: HashMap<usize, Vec<AtomSemanticCluster>> = HashMap::new();
    for cluster in mapped.semantic_clusters {
        semantic_by_owner
            .entry(cluster.owner_visible_index)
            .or_default()
            .push(AtomSemanticCluster {
                group_id,
                source_order: cluster.source_offset,
                style: Rc::clone(&semantic_style),
                base_direction,
                cluster: SemanticCluster {
                    gid: cluster.gid,
                    source: cluster.source,
                    advance_pt: cluster.advance_pt,
                },
            });
    }
    for (visible_index, ((g, visible), is_space)) in shaped
        .glyphs
        .iter()
        .zip(mapped.visible)
        .zip(mapped.is_space)
        .enumerate()
    {
        atoms.push(Atom::Glyph {
            gid: visible.gid,
            advance_pt: visible.advance_pt,
            x_offset_pt: visible.x_offset_pt,
            y_offset_pt: visible.y_offset_pt,
            ch: visible.ch,
            is_space,
            style: styles
                .get(g.face_index)
                .cloned()
                .unwrap_or_else(|| Rc::clone(style)),
            semantic: semantic_by_owner.remove(&visible_index).unwrap_or_default(),
        });
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "tab leader candidates use the surrounding run's complete shaping context"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "leader shaping mirrors resolved Word run properties"
)]
fn tab_atom(
    fonts: &FontDb,
    face_ids: &[FaceId],
    size: f64,
    char_spacing: f64,
    kern: bool,
    allow_direct_text: bool,
    base_direction: text_core::Direction,
    halfwidth_ascii: bool,
    style: &Rc<RunStyle>,
) -> Result<Atom> {
    let mut leaders = Vec::new();
    for (leader, ch) in [
        (TabLeader::Dot, '.'),
        (TabLeader::Hyphen, '-'),
        (TabLeader::Underscore, '_'),
    ] {
        let mut shaped = Vec::new();
        shape_text(
            fonts,
            face_ids,
            &ch.to_string(),
            size,
            char_spacing,
            kern,
            allow_direct_text,
            base_direction,
            halfwidth_ascii,
            style,
            &mut shaped,
        )?;
        if let Some(Atom::Glyph {
            gid,
            advance_pt,
            x_offset_pt,
            y_offset_pt,
            style,
            ..
        }) = shaped.into_iter().next()
        {
            leaders.push((
                leader,
                LeaderGlyph {
                    gid,
                    advance_pt,
                    x_offset_pt,
                    y_offset_pt,
                    ch,
                    style,
                },
            ));
        }
    }
    Ok(Atom::Tab { leaders })
}

fn weight(props: &RunProps) -> u16 {
    if props.bold == Some(true) { 700 } else { 400 }
}

fn italic(props: &RunProps) -> bool {
    props.italic == Some(true)
}

fn fallback_style(fonts: &FontDb, face: FaceId, base: &RunStyle) -> Rc<RunStyle> {
    // A coverage fallback supplies ink, not line geometry: Word never falls
    // back (it has the real fonts), so the primary face keeps governing the
    // line box. Pan-CJK faces are the exception — East Asian text reaching
    // them is a whole-script substitution and takes the legacy CJK line
    // convention (`useFELayout` growth), not the Latin primary's metrics.
    let metrics = fonts
        .face(face)
        .filter(|font| matching::pan_cjk_face(&font.family_name))
        .and_then(|font| font.line_metrics());
    Rc::new(RunStyle {
        face,
        size_pt: base.size_pt,
        color: base.color,
        background: base.background,
        underline: base.underline,
        strike: base.strike,
        rise_pt: base.rise_pt,
        ascent_pt: metrics
            .as_ref()
            .map_or(base.ascent_pt, |m| m.ascent_pt(base.size_pt)),
        line_height_pt: metrics
            .as_ref()
            .map_or(base.line_height_pt, |m| m.line_height_pt(base.size_pt)),
        line_box_size_pt: base.line_box_size_pt,
        line_box_ascent_pt: metrics.as_ref().map_or(base.line_box_ascent_pt, |m| {
            m.line_box_ascent_pt(base.line_box_size_pt)
        }),
        line_box_height_pt: metrics.as_ref().map_or(base.line_box_height_pt, |m| {
            m.line_height_pt(base.line_box_size_pt)
        }),
        link: base.link.clone(),
    })
}

fn resolve_color(props: &RunProps) -> Rgba {
    match props.color {
        Some(RunColor::Rgb(c)) => Rgba::opaque(c),
        _ => Rgba::opaque(Rgb::new(0, 0, 0)),
    }
}

/// Greedy line breaking on spaces and mandatory breaks; emergency-breaks a word
/// longer than the line at glyph granularity.
fn break_lines(
    atoms: &[Atom],
    width_first: f64,
    width_rest: f64,
    default_tab: f64,
    tab_stops: &[TabStop],
    line_metrics: impl Fn(usize, f64) -> (f64, f64),
) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut width_used = 0.0_f64;
    let mut last_break: Option<usize> = None;

    let metrics_of = |line_index: usize| {
        let base = if line_index == 0 {
            width_first
        } else {
            width_rest
        };
        line_metrics(line_index, base)
    };

    let mut i = 0;
    while i < atoms.len() {
        match &atoms[i] {
            Atom::Break(_) => {
                current.push(i);
                lines.push(std::mem::take(&mut current));
                width_used = 0.0;
                last_break = None;
                i += 1;
                continue;
            }
            Atom::Tab { .. } => {
                let following = (i + 1..atoms.len())
                    .take_while(|index| !matches!(atoms[*index], Atom::Tab { .. } | Atom::Break(_)))
                    .collect::<Vec<_>>();
                let (_, tab_origin_x) = metrics_of(lines.len());
                width_used = tab_placement(
                    atoms,
                    &following,
                    width_used,
                    tab_origin_x,
                    default_tab,
                    tab_stops,
                )
                .aligned;
                current.push(i);
                last_break = Some(current.len());
                i += 1;
                continue;
            }
            _ => {}
        }

        let adv = atoms[i].advance();
        let (avail, _) = metrics_of(lines.len());
        let is_space = atoms[i].is_space();
        let allows_break_after = allows_line_break_between(&atoms[i], atoms.get(i + 1));

        if width_used + adv > avail && !current.is_empty() && !is_space {
            if let Some(bp) = last_break.filter(|&bp| bp < current.len()) {
                let rest = current.split_off(bp);
                lines.push(std::mem::take(&mut current));
                width_used = rest.iter().map(|&k| atoms[k].advance()).sum();
                current = rest;
                last_break = None;
                continue;
            }
            lines.push(std::mem::take(&mut current));
            width_used = 0.0;
            last_break = None;
            continue;
        }

        current.push(i);
        width_used += adv;
        if allows_break_after {
            last_break = Some(current.len());
        }
        i += 1;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The next tab stop position ≥ `x` (explicit stops first, else the default grid).
fn next_tab_stop(x: f64, default_tab: f64, tab_stops: &[TabStop]) -> f64 {
    for stop in tab_stops {
        if stop.alignment != TabAlign::Clear && stop.pos_pt > x + 0.01 {
            return stop.pos_pt;
        }
    }
    let n = (x / default_tab).floor() + 1.0;
    n * default_tab
}

#[derive(Debug, Clone, Copy)]
struct ResolvedTabStop {
    stop: TabStop,
    origin: NumberingGeometryOrigin,
}

fn resolve_numbering_tabs(
    inherited: &[TabStop],
    numbering_level: &[TabStop],
    direct: &[TabStop],
) -> Vec<ResolvedTabStop> {
    fn overlay(
        effective: &mut Vec<ResolvedTabStop>,
        authored: &[TabStop],
        origin: NumberingGeometryOrigin,
    ) {
        for stop in authored {
            effective.retain(|existing| (existing.stop.pos_pt - stop.pos_pt).abs() > 0.01);
            if stop.alignment != TabAlign::Clear {
                effective.push(ResolvedTabStop {
                    stop: *stop,
                    origin,
                });
            }
        }
    }

    let mut effective = Vec::new();
    overlay(
        &mut effective,
        inherited,
        NumberingGeometryOrigin::InheritedStyle,
    );
    overlay(
        &mut effective,
        numbering_level,
        NumberingGeometryOrigin::NumberingLevel,
    );
    overlay(
        &mut effective,
        direct,
        NumberingGeometryOrigin::DirectParagraph,
    );
    effective.sort_by(|left, right| {
        left.stop
            .pos_pt
            .partial_cmp(&right.stop.pos_pt)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    effective
}

fn resolve_numbering_geometry(
    label: &PlacedLabel,
    left: f64,
    hanging: f64,
    default_tab: f64,
    tab_stops: &[ResolvedTabStop],
    left_origin: NumberingGeometryOrigin,
    hanging_origin: NumberingGeometryOrigin,
) -> ResolvedNumberingGeometry {
    let label_width: f64 = label
        .runs
        .iter()
        .flat_map(|run| &run.glyphs)
        .map(|glyph| glyph.advance_pt)
        .sum();
    // ECMA-376's numbering position is the alignment point at
    // `left - hanging`: start labels begin there, centered labels straddle it,
    // and end/right labels finish there. The hanging interval is the gap from
    // that point to the following-text target, not a box in which a right
    // label is aligned against the text indent.
    let alignment_point = (left - hanging).max(0.0);
    let label_start = match label.justification {
        LabelJustification::Start => alignment_point,
        LabelJustification::Center => (alignment_point - label_width / 2.0).max(0.0),
        LabelJustification::End => (alignment_point - label_width).max(0.0),
    };
    let label_end = label_start + label_width;
    let (following_text_start_pt, tab_origin) = match label.suffix {
        LevelSuffix::Nothing => (label_end, left_origin),
        LevelSuffix::Space => (
            label_end + label.suffix_space_advance_pt,
            NumberingGeometryOrigin::Default,
        ),
        LevelSuffix::Tab => {
            if let Some(next) = tab_stops
                .iter()
                .find(|resolved| resolved.stop.pos_pt > label_end + 0.01)
            {
                (next.stop.pos_pt, next.origin)
            } else if tab_stops.is_empty() && label_end < left - 0.01 {
                // Omitted `w:suff` means tab. With no authored stop, Word
                // first targets the text indent itself.
                (left, left_origin)
            } else {
                // Reaching the target counts as consuming it; continue to the
                // next document-default stop rather than abutting/overprinting
                // the following text.
                (
                    next_tab_stop(label_end, default_tab.max(0.01), &[]),
                    NumberingGeometryOrigin::Default,
                )
            }
        }
    };
    ResolvedNumberingGeometry {
        label_alignment_box_start_pt: label_start,
        label_alignment_box_end_pt: label_end,
        label_ink_end_pt: label_end,
        following_text_start_pt,
        left_origin,
        hanging_origin,
        tab_origin,
    }
}

#[derive(Clone, Copy)]
struct TabPlacement {
    target: f64,
    aligned: f64,
    stop_index: Option<usize>,
}

/// Resolve a tab's start position using the complete following segment. Line
/// breaking and paint placement must share this calculation: right, center,
/// and decimal tabs align the segment around the stop rather than consuming a
/// left-tab advance and then adding the segment width again.
fn tab_placement(
    atoms: &[Atom],
    following: &[usize],
    current_x: f64,
    tab_origin_x: f64,
    default_tab: f64,
    tab_stops: &[TabStop],
) -> TabPlacement {
    let current_from_left = current_x + tab_origin_x;
    let target_from_left = next_tab_stop(current_from_left, default_tab, tab_stops);
    let stop_index = tab_stops.iter().position(|stop| {
        stop.alignment != TabAlign::Clear && (stop.pos_pt - target_from_left).abs() < 0.01
    });
    let segment = following
        .iter()
        .copied()
        .take_while(|index| !matches!(atoms[*index], Atom::Tab { .. } | Atom::Break(_)))
        .collect::<Vec<_>>();
    let segment_width: f64 = segment.iter().map(|index| atoms[*index].advance()).sum();
    let decimal_width: f64 = segment
        .iter()
        .take_while(|index| !matches!(atoms[**index], Atom::Glyph { ch: '.', .. }))
        .map(|index| atoms[*index].advance())
        .sum();
    let aligned_from_left = match stop_index.map(|index| tab_stops[index].alignment) {
        Some(TabAlign::Center) => target_from_left - segment_width / 2.0,
        Some(TabAlign::Right) => target_from_left - segment_width,
        Some(TabAlign::Decimal) => target_from_left - decimal_width,
        _ => target_from_left,
    }
    .max(current_from_left);
    TabPlacement {
        target: target_from_left - tab_origin_x,
        aligned: aligned_from_left - tab_origin_x,
        stop_index,
    }
}

#[allow(clippy::too_many_arguments)]
fn place_line(
    atoms: &[Atom],
    indices: &[usize],
    content_w: f64,
    anchor_width_pt: f64,
    base_x: f64,
    alignment: Jc,
    line_spacing: Option<LineSpacing>,
    is_last: bool,
    default_tab: f64,
    tab_stops: &[TabStop],
    label: Option<&PlacedLabel>,
    numbering_geometry: Option<&ResolvedNumberingGeometry>,
    rtl: bool,
    page_horizontal: Option<PageHorizontalContext>,
) -> Result<Line> {
    let visual_indices = visual_run_order(atoms, indices, rtl);
    let indices = visual_indices.as_slice();
    // Natural width and expandable space count.
    let tab_origin_x = base_x;
    let mut natural = 0.0_f64;
    let mut spaces = 0usize;
    for &k in indices {
        match &atoms[k] {
            Atom::Tab { .. } => {
                natural =
                    next_tab_stop(natural + tab_origin_x, default_tab, tab_stops) - tab_origin_x
            }
            Atom::Glyph {
                advance_pt,
                is_space,
                ..
            } => {
                natural += advance_pt;
                if *is_space {
                    spaces += 1;
                }
            }
            Atom::Image { w_pt, .. } => natural += w_pt,
            Atom::Composite { w_pt, anchor, .. } => {
                if anchor.is_none() {
                    natural += w_pt;
                }
            }
            Atom::Bookmark(_) | Atom::Break(_) => {}
        }
    }
    let trailing: f64 = indices
        .iter()
        .rev()
        .map(|&k| &atoms[k])
        .take_while(|a| a.is_space())
        .map(|a| a.advance())
        .sum();
    let effective = natural - trailing;

    let (start_shift, space_extra) = match alignment {
        Jc::Center => (((content_w - effective) / 2.0).max(0.0), 0.0),
        Jc::End => ((content_w - effective).max(0.0), 0.0),
        Jc::Both | Jc::Distribute if !is_last && spaces > 0 && effective < content_w => {
            (0.0, (content_w - effective) / spaces as f64)
        }
        _ => (0.0, 0.0),
    };

    // Textual numbering is part of Word's first-line box. A `numFmt=bullet`
    // marker is decorative ink instead: Task 0's anchors trace showed its
    // Noto-symbol fallback incorrectly adding 0.99pt per row, while the CJK
    // corpus proves textual labels such as `[0003]` and `[청구항 1]` retain
    // their authored label metrics.
    let mut natural_h = 0.0_f64;
    let mut text_natural_h = 0.0_f64;
    let mut ascent = 0.0_f64;
    let mut descent = 0.0_f64;
    if let Some(label) = label.filter(|label| label.contributes_to_line_box) {
        natural_h = natural_h.max(label.line_height_pt);
        text_natural_h = text_natural_h.max(label.line_height_pt);
        ascent = ascent.max(label.ascent_pt);
        descent = descent.max((label.line_height_pt - label.ascent_pt).max(0.0));
    }
    for &k in indices {
        match &atoms[k] {
            Atom::Glyph { style, .. } => {
                natural_h = natural_h.max(style.line_box_height_pt);
                text_natural_h = text_natural_h.max(style.line_box_height_pt);
                ascent = ascent.max(style.line_box_ascent_pt);
                descent =
                    descent.max((style.line_box_height_pt - style.line_box_ascent_pt).max(0.0));
            }
            Atom::Tab { leaders } => {
                for (_, leader) in leaders {
                    natural_h = natural_h.max(leader.style.line_box_height_pt);
                    text_natural_h = text_natural_h.max(leader.style.line_box_height_pt);
                    ascent = ascent.max(leader.style.line_box_ascent_pt);
                    descent = descent.max(
                        (leader.style.line_box_height_pt - leader.style.line_box_ascent_pt)
                            .max(0.0),
                    );
                }
            }
            Atom::Image { h_pt, .. } => {
                natural_h = natural_h.max(*h_pt);
                ascent = ascent.max(*h_pt);
            }
            Atom::Composite {
                h_pt,
                baseline_pt,
                anchor,
                ..
            } if anchor.is_none() => {
                natural_h = natural_h.max(*h_pt);
                let baseline = baseline_pt.unwrap_or(*h_pt).clamp(0.0, *h_pt);
                ascent = ascent.max(baseline);
                descent = descent.max(*h_pt - baseline);
            }
            _ => {}
        }
    }
    if natural_h <= 0.0 {
        if let Some(label) = label {
            natural_h = label.line_height_pt;
            ascent = label.ascent_pt;
            descent = (label.line_height_pt - label.ascent_pt).max(0.0);
        } else {
            natural_h = DEFAULT_SIZE_PT * 1.2;
            ascent = DEFAULT_SIZE_PT;
        }
    }
    if text_natural_h <= 0.0 {
        text_natural_h = DEFAULT_SIZE_PT * 1.2;
    }
    natural_h = natural_h.max(ascent + descent);
    let has_inline_object = indices.iter().any(|&k| {
        matches!(
            atoms[k],
            Atom::Image { .. } | Atom::Composite { anchor: None, .. }
        )
    });
    let height = match line_spacing {
        Some(LineSpacing::Auto(multiplier)) => {
            // Word grows an inline-object line by the surplus leading of one
            // text line — the object itself is never multiplied (calibrated
            // against the CJK ground truths' inline equation images).
            let object_growth = if has_inline_object && multiplier > 1.0 {
                (multiplier - 1.0) * text_natural_h
            } else {
                0.0
            };
            (natural_h + object_growth).max(apply_line_spacing(line_spacing, text_natural_h))
        }
        _ => apply_line_spacing(line_spacing, natural_h),
    };

    let mut items: Vec<LineItem> = Vec::new();
    #[cfg(feature = "fidelity-diagnostics")]
    let mut object_ink_bounds = Vec::new();
    #[cfg(feature = "fidelity-diagnostics")]
    let mut math_ink_bounds = Vec::new();
    // Semantic text represents complex visible glyphs in logical source order.
    // Prefer it on an equal source key so a complex cluster at the start of a
    // same-style glyph run is exposed before later directly mapped characters.
    let mut text_items: Vec<(usize, u8, LineItem)> = Vec::new();
    let mut anchored_items: Vec<AnchoredLineItem> = Vec::new();
    let mut bookmarks = Vec::new();

    if let (Some(label), Some(geometry)) = (label, numbering_geometry) {
        let mut run_x = geometry.label_alignment_box_start_pt;
        for run in &label.runs {
            text_items.push((
                0,
                1,
                LineItem {
                    x_pt: run_x,
                    y_pt: ascent,
                    item: Item::Glyphs(run.clone()),
                },
            ));
            run_x += run.glyphs.iter().map(|glyph| glyph.advance_pt).sum::<f64>();
        }
    }

    let mut x = base_x + start_shift;
    let mut pending: Vec<PlacedGlyph> = Vec::new();
    let mut pending_style: Option<Rc<RunStyle>> = None;
    let mut pending_order: Option<usize> = None;
    let mut pending_x = x;
    let mut link_start: Option<(f64, RunLink)> = None;
    let mut internal_links = Vec::new();
    let mut line_semantics: BTreeMap<usize, Vec<(f64, AtomSemanticCluster)>> = BTreeMap::new();
    let mut background_items = Vec::new();

    let flush = |pending: &mut Vec<PlacedGlyph>,
                 pending_style: &mut Option<Rc<RunStyle>>,
                 pending_order: &mut Option<usize>,
                 pending_x: f64,
                 background_items: &mut Vec<LineItem>,
                 text_items: &mut Vec<(usize, u8, LineItem)>| {
        if let Some(st) = pending_style.take()
            && !pending.is_empty()
        {
            if let Some(fill) = st.background {
                let width = pending.iter().map(|glyph| glyph.advance_pt).sum();
                background_items.push(LineItem {
                    x_pt: pending_x,
                    y_pt: (ascent - st.ascent_pt - st.rise_pt).max(0.0),
                    item: Item::Rect {
                        w_pt: width,
                        h_pt: st.line_height_pt.min(height),
                        fill: Some(fill),
                        stroke: None,
                    },
                });
            }
            text_items.push((
                pending_order.take().unwrap_or(usize::MAX),
                1,
                LineItem {
                    x_pt: pending_x,
                    y_pt: ascent - st.rise_pt,
                    item: Item::Glyphs(build_run(&st, std::mem::take(pending), rtl)),
                },
            ));
        }
    };

    for (visual_position, &k) in indices.iter().enumerate() {
        match &atoms[k] {
            Atom::Bookmark(name) => bookmarks.push(name.clone()),
            Atom::Glyph {
                gid,
                advance_pt,
                x_offset_pt,
                y_offset_pt,
                ch,
                is_space,
                style,
                semantic,
            } => {
                let same = pending_style
                    .as_ref()
                    .map(|s| Rc::ptr_eq(s, style))
                    .unwrap_or(false);
                if !same {
                    flush(
                        &mut pending,
                        &mut pending_style,
                        &mut pending_order,
                        pending_x,
                        &mut background_items,
                        &mut text_items,
                    );
                    pending_style = Some(Rc::clone(style));
                    pending_order = Some(k.saturating_add(1));
                    pending_x = x;
                } else {
                    pending_order =
                        Some(pending_order.unwrap_or(usize::MAX).min(k.saturating_add(1)));
                }
                update_link(
                    &mut link_start,
                    style.link.as_ref(),
                    x,
                    height,
                    &mut items,
                    &mut internal_links,
                );
                let adv = advance_pt + if *is_space { space_extra } else { 0.0 };
                let advance_end = x + adv;
                for owned in semantic {
                    let mut owned = owned.clone();
                    let semantic_advance =
                        owned.cluster.advance_pt + if *is_space { space_extra } else { 0.0 };
                    owned.cluster.advance_pt = semantic_advance;
                    line_semantics
                        .entry(owned.group_id)
                        .or_default()
                        .push((advance_end - semantic_advance, owned));
                }
                pending.push(PlacedGlyph {
                    gid: *gid,
                    advance_pt: adv,
                    x_offset_pt: *x_offset_pt,
                    y_offset_pt: *y_offset_pt,
                    ch: *ch,
                });
                x += adv;
            }
            Atom::Tab { leaders } => {
                flush(
                    &mut pending,
                    &mut pending_style,
                    &mut pending_order,
                    pending_x,
                    &mut background_items,
                    &mut text_items,
                );
                let relative_x = x - base_x;
                let following = &indices[visual_position + 1..];
                let placement = tab_placement(
                    atoms,
                    following,
                    relative_x,
                    tab_origin_x,
                    default_tab,
                    tab_stops,
                );
                let target = placement.target;
                let aligned = placement.aligned;
                let stop = placement.stop_index.map(|index| &tab_stops[index]);
                if let Some(stop) = stop {
                    if stop.alignment == TabAlign::Bar {
                        items.push(LineItem {
                            x_pt: base_x + target,
                            y_pt: 0.0,
                            item: Item::Line {
                                dx_pt: 0.0,
                                dy_pt: height,
                                stroke: Stroke {
                                    width_pt: 0.5,
                                    color: Rgba::opaque(Rgb::new(0, 0, 0)),
                                    dash: None,
                                },
                            },
                        });
                    }
                    if stop.leader != TabLeader::None
                        && aligned > relative_x
                        && let Some((_, leader)) =
                            leaders.iter().find(|(kind, _)| *kind == stop.leader)
                        && leader.advance_pt > 0.0
                    {
                        let count = tab_leader_count(aligned - relative_x, leader.advance_pt)?;
                        if count > 0 {
                            let leader_width = count as f64 * leader.advance_pt;
                            let glyphs = (0..count)
                                .map(|_| PlacedGlyph {
                                    gid: leader.gid,
                                    advance_pt: leader.advance_pt,
                                    x_offset_pt: leader.x_offset_pt,
                                    y_offset_pt: leader.y_offset_pt,
                                    ch: leader.ch,
                                })
                                .collect();
                            text_items.push((
                                k.saturating_add(1),
                                1,
                                LineItem {
                                    x_pt: base_x + aligned - leader_width,
                                    y_pt: ascent - leader.style.rise_pt,
                                    item: Item::Glyphs(build_run(&leader.style, glyphs, rtl)),
                                },
                            ));
                        }
                    }
                }
                x = base_x + aligned;
            }
            Atom::Image {
                image_id,
                w_pt,
                h_pt,
                link,
            } => {
                flush(
                    &mut pending,
                    &mut pending_style,
                    &mut pending_order,
                    pending_x,
                    &mut background_items,
                    &mut text_items,
                );
                update_link(
                    &mut link_start,
                    link.as_ref(),
                    x,
                    height,
                    &mut items,
                    &mut internal_links,
                );
                let image_y = (ascent - h_pt).max(0.0);
                items.push(LineItem {
                    x_pt: x,
                    y_pt: image_y,
                    item: Item::Image {
                        w_pt: *w_pt,
                        h_pt: *h_pt,
                        image_id: *image_id,
                    },
                });
                #[cfg(feature = "fidelity-diagnostics")]
                object_ink_bounds.push(InkBoundsTrace {
                    x0_pt: x,
                    y0_pt: image_y,
                    x1_pt: x + w_pt,
                    y1_pt: image_y + h_pt,
                });
                x += w_pt;
            }
            Atom::Composite {
                w_pt,
                h_pt,
                baseline_pt,
                items: nested,
                anchor,
            } => {
                flush(
                    &mut pending,
                    &mut pending_style,
                    &mut pending_order,
                    pending_x,
                    &mut background_items,
                    &mut text_items,
                );
                let (origin_x, origin_y) = anchor
                    .map(|position| {
                        let origin_x = resolve_paragraph_anchor_x(
                            position,
                            *w_pt,
                            anchor_width_pt,
                            page_horizontal,
                        );
                        let origin_y = match position.y_align {
                            Some(crate::oxml::wml::FrameAlign::Start) => 0.0,
                            Some(crate::oxml::wml::FrameAlign::Center) => {
                                (height - h_pt).max(0.0) / 2.0
                            }
                            Some(crate::oxml::wml::FrameAlign::End) => (height - h_pt).max(0.0),
                            None if position.wrap == WrapMode::Square => position.y_pt - height,
                            None => position.y_pt,
                        };
                        (origin_x, origin_y)
                    })
                    .unwrap_or((
                        x,
                        (ascent - baseline_pt.unwrap_or(*h_pt).clamp(0.0, *h_pt)).max(0.0),
                    ));
                #[cfg(feature = "fidelity-diagnostics")]
                {
                    let bounds = InkBoundsTrace {
                        x0_pt: origin_x,
                        y0_pt: origin_y,
                        x1_pt: origin_x + w_pt,
                        y1_pt: origin_y + h_pt,
                    };
                    if baseline_pt.is_some() {
                        math_ink_bounds.push(bounds);
                    } else {
                        object_ink_bounds.push(bounds);
                    }
                }
                if let Some(position) = anchor
                    && position.relative_from_v == AnchorOrigin::Paragraph
                {
                    anchored_items.extend(nested.iter().map(|nested_item| AnchoredLineItem {
                        x_pt: origin_x + nested_item.x_pt,
                        y_pt: origin_y + nested_item.y_pt,
                        behind_doc: position.behind_doc,
                        relative_height: position.relative_height,
                        item: nested_item.item.clone(),
                    }));
                } else if anchor.is_none() {
                    items.extend(nested.iter().map(|nested_item| LineItem {
                        x_pt: origin_x + nested_item.x_pt,
                        y_pt: origin_y + nested_item.y_pt,
                        item: nested_item.item.clone(),
                    }));
                    x += w_pt;
                }
            }
            Atom::Break(_) => {}
        }
    }
    flush(
        &mut pending,
        &mut pending_style,
        &mut pending_order,
        pending_x,
        &mut background_items,
        &mut text_items,
    );
    update_link(
        &mut link_start,
        None,
        x,
        height,
        &mut items,
        &mut internal_links,
    );
    for (_, mut semantic) in line_semantics {
        semantic.sort_by_key(|(_, owned)| owned.source_order);
        let Some((_, first)) = semantic.first() else {
            continue;
        };
        let origin_x = semantic
            .iter()
            .map(|(origin, _)| *origin)
            .fold(f64::INFINITY, f64::min);
        let style = Rc::clone(&first.style);
        let base_direction = first.base_direction;
        let group_id = first.group_id;
        let clusters = semantic
            .into_iter()
            .map(|(_, owned)| owned.cluster)
            .collect();
        text_items.push((
            group_id.saturating_add(1),
            0,
            LineItem {
                x_pt: origin_x,
                y_pt: ascent - style.rise_pt,
                item: Item::SemanticText(SemanticRun {
                    face: style.face,
                    size_pt: style.size_pt,
                    base_direction,
                    clusters,
                }),
            },
        ));
    }
    text_items.sort_by_key(|(order, priority, _)| (*order, *priority));
    background_items.append(&mut items);
    items = background_items;
    items.extend(text_items.into_iter().map(|(_, _, item)| item));

    let page_break_after = indices
        .iter()
        .any(|&k| matches!(atoms[k], Atom::Break(BreakKind::Page)));
    let column_break_after = indices
        .iter()
        .any(|&k| matches!(atoms[k], Atom::Break(BreakKind::Column)));
    Ok(Line {
        height_pt: height,
        items,
        anchored_items,
        bookmarks,
        internal_links,
        page_break_after,
        column_break_after,
        bottom_slack_pt: line_spacing_bottom_slack(line_spacing, height, natural_h),
        #[cfg(feature = "fidelity-diagnostics")]
        diagnostics: LineTraceDraft {
            paragraph: ParagraphPropertyTrace::default(),
            ascent_pt: ascent,
            descent_pt: (height - ascent).max(0.0),
            object_ink_bounds,
            math_ink_bounds,
        },
    })
}

/// The auto-growth below a line's natural text box. Word paginates on the
/// ink bottom, letting this leading overflow the content bottom; fixed and
/// at-least line rules keep their full box.
pub(crate) fn line_spacing_bottom_slack(
    spacing: Option<LineSpacing>,
    line_height_pt: f64,
    natural_height_pt: f64,
) -> f64 {
    match spacing {
        Some(LineSpacing::Auto(_)) => (line_height_pt - natural_height_pt).max(0.0),
        _ => 0.0,
    }
}

fn visual_run_order(atoms: &[Atom], indices: &[usize], rtl: bool) -> Vec<usize> {
    if !rtl {
        return indices.to_vec();
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for index in indices.iter().copied() {
        let same_style = match (groups.last().and_then(|group| group.first()), &atoms[index]) {
            (Some(previous), Atom::Glyph { style, .. }) => matches!(
                &atoms[*previous],
                Atom::Glyph { style: previous_style, .. } if Rc::ptr_eq(previous_style, style)
            ),
            _ => false,
        };
        if same_style {
            groups.last_mut().expect("group exists").push(index);
        } else {
            groups.push(vec![index]);
        }
    }
    groups.reverse();
    groups.into_iter().flatten().collect()
}

/// Opens/closes link rectangles as the current link target changes.
fn update_link(
    link_start: &mut Option<(f64, RunLink)>,
    now: Option<&RunLink>,
    x: f64,
    height: f64,
    items: &mut Vec<LineItem>,
    internal_links: &mut Vec<InternalLinkRect>,
) {
    let changed = link_key(link_start.as_ref().map(|(_, t)| t)) != link_key(now);
    if !changed {
        return;
    }
    if let Some((sx, target)) = link_start.take()
        && x > sx
    {
        match target {
            RunLink::Public(target) => items.push(LineItem {
                x_pt: sx,
                y_pt: 0.0,
                item: Item::Link {
                    w_pt: x - sx,
                    h_pt: height,
                    target,
                },
            }),
            RunLink::Internal(target) => internal_links.push(InternalLinkRect {
                x_pt: sx,
                y_pt: 0.0,
                w_pt: x - sx,
                h_pt: height,
                target,
            }),
        }
    }
    if let Some(t) = now {
        *link_start = Some((x, t.clone()));
    }
}

fn link_key(l: Option<&RunLink>) -> String {
    match l {
        None => String::new(),
        Some(RunLink::Public(LinkTarget::Url(u))) => format!("u:{u}"),
        Some(RunLink::Public(LinkTarget::Page(p))) => format!("p:{p}"),
        Some(RunLink::Internal(name)) => format!("a:{name}"),
    }
}

fn build_run(style: &RunStyle, glyphs: Vec<PlacedGlyph>, rtl: bool) -> GlyphRun {
    GlyphRun {
        face: style.face,
        size_pt: style.size_pt,
        color: style.color,
        base_direction: Some(if rtl {
            text_core::Direction::Rtl
        } else {
            text_core::Direction::Ltr
        }),
        glyphs,
        underline: style.underline,
        strike: style.strike,
    }
}

fn para_mark_height(doc: &WordDoc, para: &Paragraph, use_mark_rpr: bool) -> f64 {
    let mark = use_mark_rpr
        .then(|| doc.styles.para_props(&para.props).mark_rpr)
        .flatten()
        .unwrap_or_default();
    let eff = doc.styles.run_props(&para.props, &mark);
    let size = eff.size_pt.unwrap_or(DEFAULT_SIZE_PT);
    // Use the resolved face's metrics when available.
    let family = matching::wml_run_family(&eff.fonts, FontSlot::Ascii, &doc.theme);
    if let Some(face) = matching::resolve(&doc.fonts, &family, 400, false).face
        && let Some(metrics) = doc.fonts.face(face).and_then(|f| f.line_metrics())
    {
        return metrics.line_height_pt(size);
    }
    size * 1.2
}

#[cfg(test)]
mod tests {
    use super::EdgeWrapBand;

    #[test]
    fn tight_wrap_releases_a_line_when_its_baseline_clears_the_polygon() {
        let tight = EdgeWrapBand {
            source: None,
            top_pt: 0.0,
            bottom_pt: 31.0,
            inset_pt: 100.0,
            uses_text_baseline: true,
        };
        assert!(tight.applies_to_line(1, 15.0, 11.0));
        assert!(
            !tight.applies_to_line(2, 15.0, 11.0),
            "line-box top overlaps by 1pt, but the text baseline is below the polygon"
        );

        let square = EdgeWrapBand {
            uses_text_baseline: false,
            ..tight
        };
        assert!(
            square.applies_to_line(2, 15.0, 11.0),
            "square wrapping continues to exclude the complete intersecting line box"
        );
    }
}

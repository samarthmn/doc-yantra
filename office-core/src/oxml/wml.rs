//! WordprocessingML model of `word/document.xml` (parse only — no layout).
//!
//! Parsing is fail-closed (see [`super`]): known children are modelled,
//! ignorable bookkeeping is skipped, enumerated-but-unsupported constructs set a
//! flag on [`DocFlags`] (so `profile()` can report them all at once), and any
//! other element is an `Unsupported{UnknownElement}` error.

use super::dml::{self, Drawing};
use super::{
    M, MC, Node, R, Rgb, TblWidth, VML, W, alternate_content_fallback, attr, child, children,
    eighth_point_to_pt, is, measure_twip_to_pt, on_off, parse_i64, parse_measure, twip_to_pt,
    unknown_child, wml_ignorable,
};
use crate::{OfficeError, Result};
use std::collections::BTreeMap;

/// A fully-parsed word document (main body + final section + profile flags).
#[derive(Debug, Clone)]
pub struct Document {
    /// Block-level content of `w:body`.
    pub body: Vec<BlockContent>,
    /// The document's final section properties (the trailing `w:sectPr`).
    pub final_sect: SectPr,
    /// Enumerated-but-unsupported constructs detected during parsing.
    pub flags: DocFlags,
}

/// Out-of-envelope constructs detected while parsing (surfaced as profile
/// reasons rather than parse errors, so all are reported at once).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocFlags {
    /// A `w:footnoteReference` is present.
    pub footnotes: bool,
    /// A `w:endnoteReference` is present.
    pub endnotes: bool,
    /// An `m:oMath` equation is present.
    pub equation: bool,
    /// A framed paragraph (`w:framePr`) — a text box.
    pub text_box: bool,
    /// A floating/anchored drawing (`wp:anchor`).
    pub anchored_drawing: bool,
    /// A floating table (`w:tblpPr`).
    pub floating_table: bool,
    /// A multi-column section.
    pub multi_column: bool,
    /// A cropped image (`a:srcRect`).
    pub image_crop: bool,
    /// A legacy VML picture (`w:pict`).
    pub vml_pict: bool,
    /// A chart drawing (graphic-data chart URI).
    pub chart: bool,
    /// A SmartArt/diagram drawing (graphic-data diagram URI).
    pub smart_art: bool,
    /// An embedded OLE object (`w:object`).
    pub ole_object: bool,
    /// A real (non-`none`) WordArt text warp.
    pub wordart_warp: bool,
    /// Visible constructs deliberately kept fail-closed.
    pub visible_unsupported: Vec<String>,
}

impl DocFlags {
    /// Merges flags from a related visible part (header/footer/note).
    pub fn merge(&mut self, other: &DocFlags) {
        self.footnotes |= other.footnotes;
        self.endnotes |= other.endnotes;
        self.equation |= other.equation;
        self.text_box |= other.text_box;
        self.anchored_drawing |= other.anchored_drawing;
        self.floating_table |= other.floating_table;
        self.multi_column |= other.multi_column;
        self.image_crop |= other.image_crop;
        self.vml_pict |= other.vml_pict;
        self.chart |= other.chart;
        self.smart_art |= other.smart_art;
        self.ole_object |= other.ole_object;
        self.wordart_warp |= other.wordart_warp;
        for value in &other.visible_unsupported {
            if !self.visible_unsupported.contains(value) {
                self.visible_unsupported.push(value.clone());
            }
        }
    }
}

/// A block-level item in a body or table cell.
#[derive(Debug, Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "boxing Paragraph would churn every hot layout match; blocks are already heap-owned in Vec"
)]
pub enum BlockContent {
    /// A paragraph.
    Paragraph(Paragraph),
    /// A table.
    Table(Table),
}

/// A paragraph: properties, inline content, and an optional section break.
#[derive(Debug, Clone, Default)]
pub struct Paragraph {
    /// Paragraph properties (`w:pPr`).
    pub props: ParaProps,
    /// Inline content (runs, hyperlinks, fields).
    pub runs: Vec<InlineContent>,
    /// A `w:sectPr` inside this paragraph's `w:pPr` ends a section here.
    pub sect_break: Option<SectPr>,
}

/// Paragraph-level properties (all optional for the style cascade).
#[derive(Debug, Clone, Default)]
pub struct ParaProps {
    /// Referenced paragraph style id (`w:pStyle`).
    pub style_id: Option<String>,
    /// Justification.
    pub alignment: Option<Jc>,
    /// Right-to-left paragraph base direction (`w:bidi`).
    pub bidi: Option<bool>,
    /// Spacing before/after and line spacing.
    pub spacing: Spacing,
    /// Indentation.
    pub indent: Indent,
    /// Explicit tab stops.
    pub tabs: Vec<TabStop>,
    /// List/numbering reference.
    pub numbering: Option<NumRef>,
    /// Keep with next paragraph.
    pub keep_next: bool,
    /// Keep all lines on one page.
    pub keep_lines: bool,
    /// Force a page break before this paragraph.
    pub page_break_before: bool,
    /// Widow/orphan control (`None` = inherit; Word default on).
    pub widow_control: Option<bool>,
    /// Paragraph borders.
    pub borders: ParaBorders,
    /// Paragraph background shading fill color.
    pub shading: Option<Rgb>,
    /// Suppress spacing between same-style paragraphs.
    pub contextual_spacing: bool,
    /// Outline level 0–8 (Heading 1 = 0); drives PDF bookmarks.
    pub outline_lvl: Option<u8>,
    /// Floating paragraph frame (`w:framePr`) in the supported common subset.
    pub frame: Option<FrameProps>,
    /// Run properties of the paragraph mark (`w:pPr/w:rPr`). Affects the height
    /// of an empty paragraph's line and the pilcrow; never the runs' own props.
    pub mark_rpr: Option<RunProps>,
    /// The table style's contribution to this paragraph's cascade, for
    /// paragraphs inside a table cell. Not parsed from `w:pPr`: it is resolved
    /// from the enclosing table's style by [`super::table_style`] and consumed
    /// by [`super::styles::StyleResolver`], which slots it in below the
    /// paragraph style (ECMA-376 §17.7.2).
    pub tbl_style: Option<std::rc::Rc<TblStyleText>>,
    /// Parsed visible properties not yet rendered.
    pub visible_unsupported: Vec<String>,
}

/// The paragraph- and run-level formatting a table style contributes to the
/// text inside one cell (whole-table plus every conditional slot that cell
/// falls into, already flattened).
#[derive(Debug, Clone, Default)]
pub struct TblStyleText {
    /// Paragraph properties from the table style.
    pub ppr: ParaProps,
    /// Run properties from the table style.
    pub rpr: RunProps,
}

/// Position, extent, and conservative wrapping for a `w:framePr` paragraph.
#[derive(Debug, Clone, Copy)]
pub struct FrameProps {
    /// Explicit horizontal offset from the selected anchor.
    pub x_pt: Option<f64>,
    /// Explicit vertical offset from the selected anchor.
    pub y_pt: Option<f64>,
    /// Alignment used when no explicit horizontal offset is supplied.
    pub x_align: FrameAlign,
    /// Alignment used when no explicit vertical offset is supplied.
    pub y_align: FrameAlign,
    /// Horizontal anchor origin.
    pub horizontal: AnchorOrigin,
    /// Vertical anchor origin.
    pub vertical: AnchorOrigin,
    /// Explicit frame width; absent means the available text width.
    pub width_pt: Option<f64>,
    /// Explicit frame height.
    pub height_pt: Option<f64>,
    /// How the explicit height interacts with laid-out content.
    pub height_rule: FrameHeightRule,
    /// Conservative exclusion behavior for following body text.
    pub wrap: WrapMode,
    /// Unsupported visible frame attribute/value, if any.
    pub unsupported: Option<&'static str>,
}

/// Axis alignment for a framed paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAlign {
    /// Start/top edge of the anchor area.
    Start,
    /// Center of the anchor area.
    Center,
    /// End/bottom edge of the anchor area.
    End,
}

/// Height rule for a framed paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameHeightRule {
    /// Grow to the content, treating `h` as a minimum when present.
    Auto,
    /// Use at least the declared height.
    AtLeast,
    /// Use exactly the declared height.
    Exact,
}

/// Paragraph justification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jc {
    /// Left / start.
    Start,
    /// Right / end.
    End,
    /// Centered.
    Center,
    /// Justified (both edges).
    Both,
    /// Distributed.
    Distribute,
}

impl Jc {
    fn parse(v: &str) -> Option<Jc> {
        match v {
            "left" | "start" => Some(Jc::Start),
            "right" | "end" => Some(Jc::End),
            "center" => Some(Jc::Center),
            "both" | "justify" => Some(Jc::Both),
            "distribute" => Some(Jc::Distribute),
            _ => None,
        }
    }
}

/// Paragraph spacing (points) and line spacing.
#[derive(Debug, Clone, Default)]
pub struct Spacing {
    /// Space before the paragraph, points.
    pub before_pt: Option<f64>,
    /// Space after the paragraph, points.
    pub after_pt: Option<f64>,
    /// Line spacing rule.
    pub line: Option<LineSpacing>,
}

/// Line-spacing rule. `Auto` is a multiple of natural line height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineSpacing {
    /// Multiple of natural line height (`w:line`/240).
    Auto(f64),
    /// At-least, in points.
    AtLeast(f64),
    /// Exact, in points.
    Exact(f64),
}

/// Paragraph indentation (points; first-line positive, hanging negative first line).
#[derive(Debug, Clone, Default)]
pub struct Indent {
    /// Left indent.
    pub left_pt: Option<f64>,
    /// Right indent.
    pub right_pt: Option<f64>,
    /// Positive additional first-line indent.
    pub first_line_pt: Option<f64>,
    /// Hanging indent (first line pulled left by this amount).
    pub hanging_pt: Option<f64>,
}

/// A tab stop.
#[derive(Debug, Clone, Copy)]
pub struct TabStop {
    /// Position from the left text margin, points.
    pub pos_pt: f64,
    /// Alignment of text at the stop.
    pub alignment: TabAlign,
    /// Leader fill character.
    pub leader: TabLeader,
}

/// Tab-stop alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAlign {
    /// Left-aligned text after the stop.
    Left,
    /// Centered on the stop.
    Center,
    /// Right-aligned to the stop.
    Right,
    /// Decimal-aligned.
    Decimal,
    /// Bar (vertical rule).
    Bar,
    /// Clears an inherited stop.
    Clear,
}

/// Tab leader fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabLeader {
    /// No leader.
    None,
    /// Dot leader.
    Dot,
    /// Hyphen leader.
    Hyphen,
    /// Underscore leader.
    Underscore,
}

/// A reference to a numbering definition and level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumRef {
    /// `w:numId`.
    pub num_id: i64,
    /// Zero-based list level (`w:ilvl`).
    pub ilvl: u32,
}

/// Inline content within a paragraph.
#[derive(Debug, Clone)]
pub enum InlineContent {
    /// A text/formatting run.
    Run(Run),
    /// A hyperlink wrapping runs.
    Hyperlink {
        /// Relationship id for an external target.
        rel_id: Option<String>,
        /// Internal bookmark anchor.
        anchor: Option<String>,
        /// The link's runs.
        runs: Vec<Run>,
    },
    /// A simple field (`w:fldSimple`).
    SimpleField(Field),
    /// An assembled complex field region (`w:fldChar` begin/separate/end).
    ComplexFieldRegion(Field),
    /// Inline or display Office Math expression.
    Math(super::omml::MathContent),
}

/// A field: its instruction text plus the cached result Word last rendered.
#[derive(Debug, Clone, Default)]
pub struct Field {
    /// The field instruction (e.g. ` PAGE `, ` HYPERLINK "url" `).
    pub instruction: String,
    /// Cached result content (between `separate` and `end`).
    pub content: Vec<InlineContent>,
    /// True only on the terminal segment when the whole field produced no
    /// visible cached result content.
    pub starved: bool,
}

/// A run: run properties plus a sequence of run content.
#[derive(Debug, Clone, Default)]
pub struct Run {
    /// Run properties (`w:rPr`).
    pub props: RunProps,
    /// Run content pieces.
    pub content: Vec<RunContent>,
}

/// A single piece of run content.
#[derive(Debug, Clone)]
pub enum RunContent {
    /// Literal text.
    Text(String),
    /// A tab character.
    Tab,
    /// A break.
    Break(BreakKind),
    /// An (inline) drawing/image.
    Drawing(WmlDrawing),
    /// Legacy VML shape carried by `w:pict`.
    VmlShape(VmlShape),
    /// Raster preview shape belonging to an embedded OLE object.
    OlePreview(VmlShape),
    /// Office Math expression nested in a run.
    Math(super::omml::MathContent),
    /// A footnote or endnote reference marker.
    NoteReference {
        /// Which note part owns the referenced content.
        kind: NoteKind,
        /// `w:id` in that part.
        id: i64,
    },
    /// A symbol glyph from a specific font (`w:sym`).
    Symbol {
        /// The symbol font family.
        font: String,
        /// The character code (often in the F0xx PUA range).
        char_code: u32,
    },
}

/// WML-specific drawing data layered over shared DrawingML picture parsing.
#[derive(Debug, Clone)]
pub struct WmlDrawing {
    /// Shared picture fields.
    pub drawing: Drawing,
    /// Anchor positioning/wrap fields for `wp:anchor`.
    pub anchor: Option<AnchorProps>,
    /// Stable reasons why an authored anchor is outside the rendered subset.
    pub anchor_violations: Vec<String>,
    /// Source crop fractions in `[0, 1]` (`left, top, right, bottom`).
    pub crop: Option<[f64; 4]>,
    /// Text-box content carried by a word-processing shape.
    pub text_box: Option<TextBox>,
    /// Non-`none` preset text warp, when present.
    pub wordart_warp: Option<String>,
}

/// Positioning and wrapping for an anchored drawing.
#[derive(Debug, Clone, Copy)]
pub struct AnchorProps {
    /// X offset in points from selected horizontal origin.
    pub x_pt: f64,
    /// Horizontal alignment when the anchor uses `wp:align` instead of an offset.
    pub x_align: Option<FrameAlign>,
    /// Y offset in points from selected vertical origin.
    pub y_pt: f64,
    /// Vertical alignment when the anchor uses `wp:align` instead of an offset.
    pub y_align: Option<FrameAlign>,
    /// Horizontal reference (`page`, `margin`, `column`, `character`).
    pub relative_from_h: AnchorOrigin,
    /// Vertical reference (`page`, `margin`, `paragraph`, `line`).
    pub relative_from_v: AnchorOrigin,
    /// Text wrapping mode.
    pub wrap: WrapMode,
    /// Text clearance above, below, left, and right of the object.
    pub distances_pt: [f64; 4],
    /// Paint behind document text.
    pub behind_doc: bool,
    /// Drawing-layer ordering key.
    pub relative_height: u32,
}

/// Drawing/table anchor origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorOrigin {
    /// Physical page edge.
    Page,
    /// Page margin/content edge.
    Margin,
    /// Current paragraph/line/column.
    Paragraph,
}

/// Supported floating-object wrap modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapMode {
    /// Object does not exclude body text.
    None,
    /// Body text stays above or below the object.
    TopAndBottom,
    /// Body text flows around its rectangular bounds.
    Square,
    /// Body text follows a tight rectangular exclusion.
    Tight,
}

/// Text box geometry and content.
#[derive(Debug, Clone)]
pub struct TextBox {
    /// Box width.
    pub width_pt: f64,
    /// Box height.
    pub height_pt: f64,
    /// Fill color; `None` means transparent.
    pub fill: Option<Rgb>,
    /// Outline color; `None` means no outline.
    pub outline: Option<Rgb>,
    /// Paragraph/table content inside the box.
    pub content: Vec<BlockContent>,
}

/// Supported legacy VML shape.
#[derive(Debug, Clone)]
pub struct VmlShape {
    /// VML local element name.
    pub kind: String,
    /// Left offset parsed from `style`.
    pub x_pt: f64,
    /// Top offset parsed from `style`.
    pub y_pt: f64,
    /// Width parsed from `style`.
    pub width_pt: f64,
    /// Height parsed from `style`.
    pub height_pt: f64,
    /// Fill color.
    pub fill: Option<Rgb>,
    /// Stroke color.
    pub outline: Option<Rgb>,
    /// Optional raster relationship from `v:imagedata`.
    pub image_rel_id: Option<String>,
    /// Optional textbox content.
    pub text_box: Vec<BlockContent>,
    /// Simple VML path string for `v:shape`.
    pub path: Option<String>,
}

/// Note family used by inline references and note parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// `word/footnotes.xml`.
    Footnote,
    /// `word/endnotes.xml`.
    Endnote,
}

/// Parsed footnote/endnote part.
#[derive(Debug, Clone, Default)]
pub struct NotesPart {
    /// User notes keyed by `w:id`; negative ids are reserved separators.
    pub notes: BTreeMap<i64, Vec<BlockContent>>,
    /// Explicit separator content (`w:id=-1`), when supplied.
    pub separator: Vec<BlockContent>,
    /// Continuation separator content (`w:id=-2`), when supplied.
    pub continuation_separator: Vec<BlockContent>,
    /// Visible/unsupported constructs found inside the part.
    pub flags: DocFlags,
}

/// The kind of an explicit break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakKind {
    /// Line break.
    Line,
    /// Page break.
    Page,
    /// Column break.
    Column,
}

/// Run-level properties (all optional for the style cascade).
#[derive(Debug, Clone, Default)]
pub struct RunProps {
    /// Referenced character style id (`w:rStyle`).
    pub style_id: Option<String>,
    /// Bold.
    pub bold: Option<bool>,
    /// Italic.
    pub italic: Option<bool>,
    /// Underline kind.
    pub underline: Option<UnderlineKind>,
    /// Single strikethrough.
    pub strike: Option<bool>,
    /// Double strikethrough.
    pub dstrike: Option<bool>,
    /// All caps.
    pub caps: Option<bool>,
    /// Small caps.
    pub small_caps: Option<bool>,
    /// Hidden (`w:vanish`).
    pub vanish: Option<bool>,
    /// Text color.
    pub color: Option<RunColor>,
    /// Highlight color.
    pub highlight: Option<Rgb>,
    /// Run shading fill color.
    pub shading: Option<Rgb>,
    /// Font size, points.
    pub size_pt: Option<f64>,
    /// Font families by script slot.
    pub fonts: RunFonts,
    /// Vertical alignment (super/subscript).
    pub vert_align: Option<VertAlign>,
    /// Character spacing (tracking), points.
    pub spacing_pt: Option<f64>,
    /// Font-kerning threshold in points (`w:kern`): pair kerning applies only when
    /// this is set and the effective size is at least this value. Absent (the Word
    /// default) means no automatic kerning.
    pub kern_min_pt: Option<f64>,
    /// Right-to-left run.
    pub rtl: Option<bool>,
    /// Parsed visible properties not yet rendered.
    pub visible_unsupported: Vec<String>,
}

/// A text color: an explicit sRGB or `auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunColor {
    /// `w:val="auto"` — the consumer chooses (usually black).
    Auto,
    /// An explicit color.
    Rgb(Rgb),
}

/// Underline styles (drawn as a decoration line by the layout engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnderlineKind {
    /// Explicitly no underline.
    None,
    /// Single line.
    Single,
    /// Double line.
    Double,
    /// Thick line.
    Thick,
    /// Dotted.
    Dotted,
    /// Dashed.
    Dashed,
    /// Wavy.
    Wave,
    /// Words only (rendered as single v1).
    Words,
}

impl UnderlineKind {
    fn parse(v: &str) -> UnderlineKind {
        match v {
            "none" => UnderlineKind::None,
            "double" => UnderlineKind::Double,
            "thick" => UnderlineKind::Thick,
            "dotted" => UnderlineKind::Dotted,
            "dash" | "dashed" => UnderlineKind::Dashed,
            "wave" | "wavyHeavy" | "wavyDouble" => UnderlineKind::Wave,
            "words" => UnderlineKind::Words,
            _ => UnderlineKind::Single,
        }
    }
}

/// Vertical run alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertAlign {
    /// Normal baseline.
    Baseline,
    /// Superscript.
    Super,
    /// Subscript.
    Sub,
}

/// Font families for a run, by script slot (`w:rFonts`).
#[derive(Debug, Clone, Default)]
pub struct RunFonts {
    /// ASCII/Latin slot.
    pub ascii: Option<String>,
    /// High-ANSI slot.
    pub h_ansi: Option<String>,
    /// East-Asian slot.
    pub east_asia: Option<String>,
    /// Complex-script slot.
    pub cs: Option<String>,
}

impl RunFonts {
    /// Overlays `self` (higher priority) over `base`, field by field.
    pub fn merge_over(&self, base: &RunFonts) -> RunFonts {
        RunFonts {
            ascii: self.ascii.clone().or_else(|| base.ascii.clone()),
            h_ansi: self.h_ansi.clone().or_else(|| base.h_ansi.clone()),
            east_asia: self.east_asia.clone().or_else(|| base.east_asia.clone()),
            cs: self.cs.clone().or_else(|| base.cs.clone()),
        }
    }
}

impl RunProps {
    /// Returns `self` (higher priority) overlaid over `base`: each `Some`/set
    /// field of `self` wins; otherwise `base`'s value is kept. This is the
    /// primitive the style cascade (see [`super::styles`]) composes with.
    pub fn merge_over(&self, base: &RunProps) -> RunProps {
        RunProps {
            style_id: self.style_id.clone().or_else(|| base.style_id.clone()),
            bold: self.bold.or(base.bold),
            italic: self.italic.or(base.italic),
            underline: self.underline.or(base.underline),
            strike: self.strike.or(base.strike),
            dstrike: self.dstrike.or(base.dstrike),
            caps: self.caps.or(base.caps),
            small_caps: self.small_caps.or(base.small_caps),
            vanish: self.vanish.or(base.vanish),
            color: self.color.or(base.color),
            highlight: self.highlight.or(base.highlight),
            shading: self.shading.or(base.shading),
            size_pt: self.size_pt.or(base.size_pt),
            fonts: self.fonts.merge_over(&base.fonts),
            vert_align: self.vert_align.or(base.vert_align),
            spacing_pt: self.spacing_pt.or(base.spacing_pt),
            kern_min_pt: self.kern_min_pt.or(base.kern_min_pt),
            rtl: self.rtl.or(base.rtl),
            visible_unsupported: merge_reason_lists(
                &base.visible_unsupported,
                &self.visible_unsupported,
            ),
        }
    }
}

impl ParaProps {
    /// Overlays `self` (higher priority) over `base` for the paragraph cascade.
    /// Bools use "true wins if either set at its own level"; the direct level's
    /// explicit values dominate via `self` being applied last by the resolver.
    pub fn merge_over(&self, base: &ParaProps) -> ParaProps {
        ParaProps {
            style_id: self.style_id.clone().or_else(|| base.style_id.clone()),
            alignment: self.alignment.or(base.alignment),
            bidi: self.bidi.or(base.bidi),
            spacing: Spacing {
                before_pt: self.spacing.before_pt.or(base.spacing.before_pt),
                after_pt: self.spacing.after_pt.or(base.spacing.after_pt),
                line: self.spacing.line.or(base.spacing.line),
            },
            indent: Indent {
                left_pt: self.indent.left_pt.or(base.indent.left_pt),
                right_pt: self.indent.right_pt.or(base.indent.right_pt),
                first_line_pt: self.indent.first_line_pt.or(base.indent.first_line_pt),
                hanging_pt: self.indent.hanging_pt.or(base.indent.hanging_pt),
            },
            tabs: if self.tabs.is_empty() {
                base.tabs.clone()
            } else {
                self.tabs.clone()
            },
            numbering: self.numbering.or(base.numbering),
            keep_next: self.keep_next || base.keep_next,
            keep_lines: self.keep_lines || base.keep_lines,
            page_break_before: self.page_break_before || base.page_break_before,
            widow_control: self.widow_control.or(base.widow_control),
            borders: ParaBorders {
                top: self
                    .borders
                    .top
                    .clone()
                    .or_else(|| base.borders.top.clone()),
                bottom: self
                    .borders
                    .bottom
                    .clone()
                    .or_else(|| base.borders.bottom.clone()),
                left: self
                    .borders
                    .left
                    .clone()
                    .or_else(|| base.borders.left.clone()),
                right: self
                    .borders
                    .right
                    .clone()
                    .or_else(|| base.borders.right.clone()),
                between: self
                    .borders
                    .between
                    .clone()
                    .or_else(|| base.borders.between.clone()),
                bar: self
                    .borders
                    .bar
                    .clone()
                    .or_else(|| base.borders.bar.clone()),
                unsupported: self.borders.unsupported.or(base.borders.unsupported),
            },
            shading: self.shading.or(base.shading),
            contextual_spacing: self.contextual_spacing || base.contextual_spacing,
            outline_lvl: self.outline_lvl.or(base.outline_lvl),
            frame: self.frame.or(base.frame),
            mark_rpr: self.mark_rpr.clone().or_else(|| base.mark_rpr.clone()),
            tbl_style: self.tbl_style.clone().or_else(|| base.tbl_style.clone()),
            visible_unsupported: merge_reason_lists(
                &base.visible_unsupported,
                &self.visible_unsupported,
            ),
        }
    }
}

fn merge_reason_lists(base: &[String], overlay: &[String]) -> Vec<String> {
    let mut merged = base.to_vec();
    for reason in overlay {
        if !merged.contains(reason) {
            merged.push(reason.clone());
        }
    }
    merged
}

/// A single border edge specification.
#[derive(Debug, Clone, PartialEq)]
pub struct Border {
    /// The line style (`single`, `double`, …). Non-single is simplified v1.
    pub val: String,
    /// Line width in points (from eighth-point `w:sz`).
    pub size_pt: f64,
    /// Border color.
    pub color: Option<Rgb>,
    /// Theme color slot, preferred over the explicit fallback color.
    pub theme_color: Option<String>,
    /// Space from the text in points.
    pub space_pt: f64,
    /// Parse-time violation for the faithfully rendered border subset.
    pub(crate) unsupported: Option<&'static str>,
}

impl Border {
    /// Resolves the authored theme color, falling back to the explicit color.
    pub fn resolved_color(&self, theme: &super::theme::Theme) -> Option<Rgb> {
        self.theme_color
            .as_deref()
            .and_then(|slot| theme.color(slot))
            .or(self.color)
    }
}

/// Paragraph border set.
#[derive(Debug, Clone, Default)]
pub struct ParaBorders {
    /// Top edge.
    pub top: Option<Border>,
    /// Bottom edge.
    pub bottom: Option<Border>,
    /// Left edge.
    pub left: Option<Border>,
    /// Right edge.
    pub right: Option<Border>,
    /// Between consecutive bordered paragraphs.
    pub between: Option<Border>,
    /// Bar edge (outside the rendered subset when visible).
    pub bar: Option<Border>,
    /// Unknown child classification retained for profile/layout agreement.
    pub(crate) unsupported: Option<&'static str>,
}

impl ParaBorders {
    /// Returns the stable reason when paragraph borders exceed the faithfully
    /// rendered bottom/right-single subset. Non-visible `nil`/`none` edges are safe.
    pub fn unsupported_detail(&self, theme: &super::theme::Theme) -> Option<&'static str> {
        let visible = |border: &Option<Border>| {
            border
                .as_ref()
                .is_some_and(|border| !matches!(border.val.as_str(), "nil" | "none"))
        };
        if let Some(detail) = self.unsupported {
            return Some(detail);
        }
        if visible(&self.bar) {
            return Some("visible paragraph bar border is outside the rendered subset");
        }
        if visible(&self.top) || visible(&self.left) || visible(&self.between) {
            return Some("paragraph border edge outside bottom-single subset");
        }
        let validate = |border: &Option<Border>, edge: &str| {
            let border = border.as_ref()?;
            if !matches!(border.val.as_str(), "nil" | "none" | "single") {
                return Some(match edge {
                    "right" => "paragraph right border style outside single subset",
                    _ => "paragraph bottom border style outside single subset",
                });
            }
            if matches!(border.val.as_str(), "nil" | "none") {
                return None;
            }
            if let Some(detail) = border.unsupported {
                return Some(detail);
            }
            if border
                .theme_color
                .as_deref()
                .is_some_and(|slot| theme.color(slot).is_none())
            {
                return Some(match edge {
                    "right" => "paragraph right border references an unresolved theme color",
                    _ => "paragraph bottom border references an unresolved theme color",
                });
            }
            None
        };
        validate(&self.bottom, "bottom").or_else(|| validate(&self.right, "right"))
    }
}

/// A table.
#[derive(Debug, Clone, Default)]
pub struct Table {
    /// Table properties.
    pub props: TblProps,
    /// Grid column widths, points.
    pub grid: Vec<f64>,
    /// Rows.
    pub rows: Vec<Row>,
}

/// Table-level properties.
#[derive(Debug, Clone, Default)]
pub struct TblProps {
    /// Preferred table width.
    pub width: Option<TblWidth>,
    /// Table alignment.
    pub alignment: Option<Jc>,
    /// Table-wide borders.
    pub borders: TblBorders,
    /// Default cell margins.
    pub cell_margins: CellMargins,
    /// Fixed layout (`w:tblLayout w:type="fixed"`).
    pub layout_fixed: bool,
    /// Referenced table style id.
    pub style_id: Option<String>,
    /// Table indent from the left margin, points.
    pub indent_pt: Option<f64>,
    /// Floating-table positioning from `w:tblpPr`.
    pub floating: Option<FloatingTableProps>,
    /// Which conditional slots of the table style apply (`w:tblLook`).
    pub look: TblLook,
    /// Rows per horizontal band (`w:tblStyleRowBandSize`, default 1).
    pub row_band_size: Option<u32>,
    /// Columns per vertical band (`w:tblStyleColBandSize`, default 1).
    pub col_band_size: Option<u32>,
    /// Parsed visible properties not yet rendered.
    pub visible_unsupported: Vec<String>,
}

/// `w:tblLook`: which conditional-formatting slots of the referenced table
/// style this table opts into. The all-false default is the spec default
/// (`w:val="0000"`): no first/last emphasis, banding enabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TblLook {
    /// Apply the `firstRow` slot.
    pub first_row: bool,
    /// Apply the `lastRow` slot.
    pub last_row: bool,
    /// Apply the `firstCol` slot.
    pub first_col: bool,
    /// Apply the `lastCol` slot.
    pub last_col: bool,
    /// Suppress horizontal (row) banding.
    pub no_h_band: bool,
    /// Suppress vertical (column) banding.
    pub no_v_band: bool,
}

impl TblLook {
    /// Parses `w:tblLook`. Word writes both the modern boolean attributes and
    /// the legacy `w:val` bitmask; the explicit attributes win where present.
    fn parse(node: Node) -> TblLook {
        let bits = attr(node, W, "val")
            .and_then(|value| u32::from_str_radix(value.trim(), 16).ok())
            .unwrap_or(0);
        let flag = |name: &str, mask: u32| match attr(node, W, name) {
            Some(value) => on_off(Some(value)),
            None => bits & mask != 0,
        };
        TblLook {
            first_row: flag("firstRow", 0x0020),
            last_row: flag("lastRow", 0x0040),
            first_col: flag("firstColumn", 0x0080),
            last_col: flag("lastColumn", 0x0100),
            no_h_band: flag("noHBand", 0x0200),
            no_v_band: flag("noVBand", 0x0400),
        }
    }
}

impl TblBorders {
    /// Overlays `self` (higher priority) over `base`, edge by edge.
    pub fn merge_over(&self, base: &TblBorders) -> TblBorders {
        let edge = |mine: &Option<Border>, theirs: &Option<Border>| {
            mine.clone().or_else(|| theirs.clone())
        };
        TblBorders {
            top: edge(&self.top, &base.top),
            bottom: edge(&self.bottom, &base.bottom),
            left: edge(&self.left, &base.left),
            right: edge(&self.right, &base.right),
            inside_h: edge(&self.inside_h, &base.inside_h),
            inside_v: edge(&self.inside_v, &base.inside_v),
        }
    }
}

impl CellMargins {
    /// Overlays `self` (higher priority) over `base`, edge by edge.
    pub fn merge_over(&self, base: &CellMargins) -> CellMargins {
        CellMargins {
            top: self.top.or(base.top),
            right: self.right.or(base.right),
            bottom: self.bottom.or(base.bottom),
            left: self.left.or(base.left),
        }
    }
}

impl TblProps {
    /// Overlays `self` (higher priority) over `base` — used to lay the table's
    /// direct `w:tblPr` over the properties its table style supplies.
    pub fn merge_over(&self, base: &TblProps) -> TblProps {
        TblProps {
            width: self.width.or(base.width),
            alignment: self.alignment.or(base.alignment),
            borders: self.borders.merge_over(&base.borders),
            cell_margins: self.cell_margins.merge_over(&base.cell_margins),
            layout_fixed: self.layout_fixed || base.layout_fixed,
            style_id: self.style_id.clone().or_else(|| base.style_id.clone()),
            indent_pt: self.indent_pt.or(base.indent_pt),
            floating: self.floating.or(base.floating),
            // `w:tblLook` is a property of the table instance, never of the style.
            look: self.look,
            row_band_size: self.row_band_size.or(base.row_band_size),
            col_band_size: self.col_band_size.or(base.col_band_size),
            visible_unsupported: merge_reason_lists(
                &base.visible_unsupported,
                &self.visible_unsupported,
            ),
        }
    }
}

impl RowProps {
    /// Overlays `self` (higher priority) over `base`.
    pub fn merge_over(&self, base: &RowProps) -> RowProps {
        RowProps {
            grid_before: if self.grid_before > 0 {
                self.grid_before
            } else {
                base.grid_before
            },
            grid_after: if self.grid_after > 0 {
                self.grid_after
            } else {
                base.grid_after
            },
            width_before: self.width_before.or(base.width_before),
            width_after: self.width_after.or(base.width_after),
            height: self.height.or(base.height),
            hidden: self.hidden || base.hidden,
            header: self.header || base.header,
            cant_split: self.cant_split || base.cant_split,
            visible_unsupported: merge_reason_lists(
                &base.visible_unsupported,
                &self.visible_unsupported,
            ),
        }
    }
}

impl CellProps {
    /// Overlays `self` (higher priority) over `base`.
    pub fn merge_over(&self, base: &CellProps) -> CellProps {
        CellProps {
            width: self.width.or(base.width),
            grid_span: self.grid_span.max(1),
            v_merge: self.v_merge.or(base.v_merge),
            borders: self.borders.merge_over(&base.borders),
            shading: self.shading.or(base.shading),
            margins: self.margins.merge_over(&base.margins),
            v_align: if self.v_align == VAlign::default() {
                base.v_align
            } else {
                self.v_align
            },
            visible_unsupported: merge_reason_lists(
                &base.visible_unsupported,
                &self.visible_unsupported,
            ),
        }
    }
}

/// Positioning for a floating table.
#[derive(Debug, Clone, Copy)]
pub struct FloatingTableProps {
    /// Horizontal offset in points.
    pub x_pt: f64,
    /// Vertical offset in points.
    pub y_pt: f64,
    /// Horizontal anchor origin.
    pub horizontal: AnchorOrigin,
    /// Vertical anchor origin.
    pub vertical: AnchorOrigin,
    /// Maximum left/right text clearance used by wrapping and block fallback.
    pub wrap_padding_pt: f64,
}

/// Exact text clearances authored on a floating table.
///
/// This stays crate-private so the legacy public [`FloatingTableProps`] shape
/// remains source-compatible while layout can still honor asymmetric sides
/// and the independent bottom clearance.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct FloatingTableDistances {
    pub(crate) left_pt: f64,
    pub(crate) right_pt: f64,
    pub(crate) bottom_pt: f64,
}

/// A table row.
#[derive(Debug, Clone, Default)]
pub struct Row {
    /// Row properties.
    pub props: RowProps,
    /// Cells.
    pub cells: Vec<Cell>,
}

/// Row-level properties.
#[derive(Debug, Clone, Default)]
pub struct RowProps {
    /// Shared table-grid columns omitted before this row's first cell.
    pub grid_before: u32,
    /// Shared table-grid columns omitted after this row's last cell.
    pub grid_after: u32,
    /// Preferred aggregate width of the omitted leading columns.
    pub width_before: Option<TblWidth>,
    /// Preferred aggregate width of the omitted trailing columns.
    pub width_after: Option<TblWidth>,
    /// Row height rule.
    pub height: Option<RowHeight>,
    /// Hidden rows do not paint or consume height.
    pub hidden: bool,
    /// This is a repeatable header row.
    pub header: bool,
    /// The row may not split across pages.
    pub cant_split: bool,
    /// Parsed visible properties not yet rendered.
    pub visible_unsupported: Vec<String>,
}

/// Row height rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowHeight {
    /// Minimum height, points.
    AtLeast(f64),
    /// Exact height, points.
    Exact(f64),
}

/// A table cell (cells recurse: nested tables are in-envelope).
#[derive(Debug, Clone, Default)]
pub struct Cell {
    /// Cell properties.
    pub props: CellProps,
    /// Block content.
    pub content: Vec<BlockContent>,
}

/// Cell-level properties.
#[derive(Debug, Clone, Default)]
pub struct CellProps {
    /// Preferred cell width.
    pub width: Option<TblWidth>,
    /// Horizontal grid span (columns covered).
    pub grid_span: u32,
    /// Vertical merge state.
    pub v_merge: Option<VMerge>,
    /// Per-cell borders.
    pub borders: TblBorders,
    /// Cell background shading fill.
    pub shading: Option<Rgb>,
    /// Cell margins.
    pub margins: CellMargins,
    /// Vertical content alignment.
    pub v_align: VAlign,
    /// Parsed visible properties not yet rendered.
    pub visible_unsupported: Vec<String>,
}

/// Vertical-merge state of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VMerge {
    /// Starts (or restarts) a vertical merge.
    Restart,
    /// Continues the merge above (renders nothing).
    Continue,
}

/// Vertical alignment of cell content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VAlign {
    /// Top-aligned (default).
    #[default]
    Top,
    /// Centered.
    Center,
    /// Bottom-aligned.
    Bottom,
}

/// Border set for a table or cell (interior + exterior edges).
#[derive(Debug, Clone, Default)]
pub struct TblBorders {
    /// Top edge.
    pub top: Option<Border>,
    /// Bottom edge.
    pub bottom: Option<Border>,
    /// Left edge.
    pub left: Option<Border>,
    /// Right edge.
    pub right: Option<Border>,
    /// Interior horizontal edges.
    pub inside_h: Option<Border>,
    /// Interior vertical edges.
    pub inside_v: Option<Border>,
}

/// Cell margins in points.
#[derive(Debug, Clone, Default)]
pub struct CellMargins {
    /// Top margin.
    pub top: Option<f64>,
    /// Right margin.
    pub right: Option<f64>,
    /// Bottom margin.
    pub bottom: Option<f64>,
    /// Left margin.
    pub left: Option<f64>,
}

/// Section properties (page geometry, margins, columns, headers/footers).
#[derive(Debug, Clone)]
pub struct SectPr {
    /// Page width, points.
    pub page_w_pt: f64,
    /// Page height, points.
    pub page_h_pt: f64,
    /// Page orientation.
    pub orientation: Orientation,
    /// Page margins.
    pub margins: Margins,
    /// Column layout.
    pub cols: ColumnSpec,
    /// Header references by kind.
    pub header_refs: Vec<HdrFtrRef>,
    /// Footer references by kind.
    pub footer_refs: Vec<HdrFtrRef>,
    /// Different first-page header/footer.
    pub title_pg: bool,
    /// Restart page numbering at this value.
    pub page_num_start: Option<u32>,
    /// Page-number format used by PAGE fields.
    pub page_num_fmt: String,
    /// Section start type.
    pub sect_type: SectType,
    /// Visible page borders.
    pub page_borders: ParaBorders,
    /// Vertical body alignment within page content area.
    pub vertical_align: PageVAlign,
}

impl Default for SectPr {
    fn default() -> Self {
        // Word's default: US Letter portrait, 1" margins, 0.5" header/footer.
        SectPr {
            page_w_pt: 612.0,
            page_h_pt: 792.0,
            orientation: Orientation::Portrait,
            margins: Margins {
                top: 72.0,
                right: 72.0,
                bottom: 72.0,
                left: 72.0,
                header: 36.0,
                footer: 36.0,
                gutter: 0.0,
            },
            cols: ColumnSpec::default(),
            header_refs: Vec::new(),
            footer_refs: Vec::new(),
            title_pg: false,
            page_num_start: None,
            page_num_fmt: "decimal".to_string(),
            sect_type: SectType::NextPage,
            page_borders: ParaBorders::default(),
            vertical_align: PageVAlign::Top,
        }
    }
}

/// Vertical section alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageVAlign {
    /// Begin at top margin.
    #[default]
    Top,
    /// Center body vertically.
    Center,
    /// Align body bottom with bottom margin.
    Bottom,
}

/// Page orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// Portrait.
    Portrait,
    /// Landscape.
    Landscape,
}

/// Page margins in points.
#[derive(Debug, Clone, Copy)]
pub struct Margins {
    /// Top margin.
    pub top: f64,
    /// Right margin.
    pub right: f64,
    /// Bottom margin.
    pub bottom: f64,
    /// Left margin.
    pub left: f64,
    /// Header band distance from the top edge.
    pub header: f64,
    /// Footer band distance from the bottom edge.
    pub footer: f64,
    /// Binding gutter.
    pub gutter: f64,
}

/// Column layout for a section.
#[derive(Debug, Clone, Default)]
pub struct ColumnSpec {
    /// Number of columns (0/1 = single column).
    pub num: u32,
    /// Explicit per-column widths (points), when present.
    pub cols: Vec<f64>,
    /// Default spacing between columns, points.
    pub space_pt: Option<f64>,
    /// Per-column trailing spacing for custom columns.
    pub spaces: Vec<f64>,
}

impl ColumnSpec {
    /// True when the section is multi-column (out of envelope v1).
    pub fn is_multi(&self) -> bool {
        self.num > 1 || self.cols.len() > 1
    }
}

/// A header/footer reference.
#[derive(Debug, Clone)]
pub struct HdrFtrRef {
    /// Which page-kind this applies to.
    pub kind: HdrFtrKind,
    /// The relationship id of the header/footer part.
    pub rel_id: String,
}

/// Header/footer applicability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrFtrKind {
    /// Default (odd/all) pages.
    Default,
    /// First page only.
    First,
    /// Even pages.
    Even,
}

/// Section start type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectType {
    /// Start on the next page.
    NextPage,
    /// Continue on the same page (flow).
    Continuous,
    /// Start on the next odd page.
    OddPage,
    /// Start on the next even page.
    EvenPage,
}

// --- Parsing ----------------------------------------------------------------

/// Private parse metadata kept outside the public document model.
#[derive(Debug, Clone, Default)]
pub(crate) struct DocumentMetadata {
    /// One entry per logical top-level body block, after transparent wrappers
    /// such as `w:sdt` and `mc:AlternateContent` have been expanded.
    pub(crate) floating_table_distances: Vec<Option<FloatingTableDistances>>,
}

/// Parses `word/document.xml` into a [`Document`].
pub fn parse_document(xml: &str) -> Result<Document> {
    Ok(parse_document_with_metadata(xml)?.0)
}

/// Parses the public document plus private layout metadata that cannot be
/// represented without changing the legacy public table-property API.
pub(crate) fn parse_document_with_metadata(xml: &str) -> Result<(Document, DocumentMetadata)> {
    let doc = super::parse_xml(xml, "document.xml")?;
    let root = doc.root_element();
    let body = child(root, W, "body")
        .ok_or_else(|| OfficeError::Invalid("document.xml has no w:body".into()))?;

    let mut floating_table_distances = Vec::new();
    collect_document_block_metadata(body, &mut floating_table_distances)?;

    let mut p = WmlParser::default();
    let mut content = p.block_level(body, "w:body")?;
    p.finish_story(&mut content);

    if floating_table_distances.len() != content.len() {
        return Err(OfficeError::Internal(format!(
            "document block metadata count {} does not match parsed block count {}",
            floating_table_distances.len(),
            content.len()
        )));
    }

    // The trailing direct-child w:sectPr is the final section.
    let final_sect = match child(body, W, "sectPr") {
        Some(sp) => p.sect_pr(sp)?,
        None => SectPr::default(),
    };

    Ok((
        Document {
            body: content,
            final_sect,
            flags: p.flags,
        },
        DocumentMetadata {
            floating_table_distances,
        },
    ))
}

fn collect_document_block_metadata(
    container: Node,
    out: &mut Vec<Option<FloatingTableDistances>>,
) -> Result<()> {
    for node in children(container) {
        if is(node, W, "p") || is(node, M, "oMathPara") {
            out.push(None);
        } else if is(node, W, "tbl") {
            out.push(child(node, W, "tblPr").and_then(parse_floating_table_distances));
        } else if is(node, W, "sdt") {
            if let Some(content) = child(node, W, "sdtContent") {
                collect_document_block_metadata(content, out)?;
            }
        } else if is(node, MC, "AlternateContent") {
            for fallback in alternate_content_fallback(node)? {
                collect_document_block_metadata_node(fallback, out)?;
            }
        }
    }
    Ok(())
}

fn collect_document_block_metadata_node(
    node: Node,
    out: &mut Vec<Option<FloatingTableDistances>>,
) -> Result<()> {
    if is(node, W, "p") || is(node, M, "oMathPara") {
        out.push(None);
    } else if is(node, W, "tbl") {
        out.push(child(node, W, "tblPr").and_then(parse_floating_table_distances));
    } else if is(node, W, "sdt") {
        if let Some(content) = child(node, W, "sdtContent") {
            collect_document_block_metadata(content, out)?;
        }
    } else if is(node, MC, "AlternateContent") {
        for fallback in alternate_content_fallback(node)? {
            collect_document_block_metadata_node(fallback, out)?;
        }
    }
    Ok(())
}

/// Parses a header/footer part body into block content (same engine as the body).
pub fn parse_hdr_ftr(xml: &str) -> Result<Vec<BlockContent>> {
    Ok(parse_hdr_ftr_with_flags(xml)?.0)
}

/// Parses header/footer content and returns its profile flags.
pub fn parse_hdr_ftr_with_flags(xml: &str) -> Result<(Vec<BlockContent>, DocFlags)> {
    let doc = super::parse_xml(xml, "header/footer")?;
    let root = doc.root_element(); // w:hdr or w:ftr
    let mut p = WmlParser::default();
    let mut blocks = p.block_level(root, "w:hdr/ftr")?;
    p.finish_story(&mut blocks);
    Ok((blocks, p.flags))
}

/// Parses `word/footnotes.xml` or `word/endnotes.xml`.
pub fn parse_notes(xml: &str, kind: NoteKind) -> Result<NotesPart> {
    let label = match kind {
        NoteKind::Footnote => "footnotes.xml",
        NoteKind::Endnote => "endnotes.xml",
    };
    let element = match kind {
        NoteKind::Footnote => "footnote",
        NoteKind::Endnote => "endnote",
    };
    let doc = super::parse_xml(xml, label)?;
    let mut parser = WmlParser::default();
    let mut part = NotesPart::default();
    for node in children(doc.root_element()) {
        if !is(node, W, element) {
            if !wml_ignorable(node) {
                return Err(unknown_child(label, node));
            }
            continue;
        }
        let Some(id) = parse_i64(attr(node, W, "id")) else {
            continue;
        };
        let mut blocks = parser.block_level(node, element)?;
        parser.finish_story(&mut blocks);
        match id {
            -1 => part.separator = blocks,
            -2 => part.continuation_separator = blocks,
            _ if id >= 0 => {
                part.notes.insert(id, blocks);
            }
            _ => {}
        }
    }
    part.flags = parser.flags;
    Ok(part)
}

#[derive(Default)]
struct WmlParser {
    flags: DocFlags,
    field: Option<FieldBuild>,
}

impl WmlParser {
    /// Flushes an open complex field at the end of one independent story.
    fn finish_story(&mut self, blocks: &mut [BlockContent]) {
        let Some(field) = self.field.take() else {
            return;
        };
        if let Some(paragraph) = last_paragraph_mut(blocks) {
            paragraph
                .runs
                .push(InlineContent::ComplexFieldRegion(field.finish(true)));
        }
    }

    /// Parses a text-box story without leaking the enclosing story's field state.
    fn text_box_story(&mut self, content: Node) -> Result<Vec<BlockContent>> {
        let outer = std::mem::take(&mut self.field);
        let parsed = self
            .block_level(content, "w:txbxContent")
            .map(|mut blocks| {
                self.finish_story(&mut blocks);
                blocks
            });
        self.field = outer;
        parsed
    }

    /// Walks a container's block-level children (paragraphs, tables, sdt, sectPr).
    fn block_level(&mut self, container: Node, parent: &str) -> Result<Vec<BlockContent>> {
        let mut out = Vec::new();
        for node in children(container) {
            if !self.block_child(node, parent, &mut out)? {
                return Err(unknown_child(parent, node));
            }
        }
        Ok(out)
    }

    /// Handles one block-level child; returns `Ok(false)` if unrecognized.
    fn block_child(
        &mut self,
        node: Node,
        parent: &str,
        out: &mut Vec<BlockContent>,
    ) -> Result<bool> {
        if is(node, W, "p") {
            out.push(BlockContent::Paragraph(self.paragraph(node)?));
        } else if is(node, M, "oMathPara") {
            out.push(BlockContent::Paragraph(Paragraph {
                props: ParaProps {
                    alignment: Some(Jc::Center),
                    ..ParaProps::default()
                },
                runs: vec![InlineContent::Math(super::omml::MathContent::parse(
                    node, true,
                ))],
                sect_break: None,
            }));
        } else if is(node, W, "tbl") {
            out.push(BlockContent::Table(self.table(node)?));
        } else if is(node, W, "sdt") {
            // Unwrap structured document tags transparently.
            if let Some(sdt_content) = child(node, W, "sdtContent") {
                out.extend(self.block_level(sdt_content, parent)?);
            }
        } else if is(node, W, "sectPr") {
            // Handled as final_sect by the caller; ignore here.
        } else if is(node, MC, "AlternateContent") {
            for fb in alternate_content_fallback(node)? {
                if !self.block_child(fb, parent, out)? {
                    return Err(unknown_child(parent, fb));
                }
            }
        } else if !wml_ignorable(node) {
            return Ok(false);
        }
        Ok(true)
    }

    fn paragraph(&mut self, node: Node) -> Result<Paragraph> {
        let mut para = Paragraph::default();
        if let Some(ppr) = child(node, W, "pPr") {
            let (props, sect) = self.para_props(ppr)?;
            para.props = props;
            para.sect_break = sect;
        }
        para.runs = self.inline_level(node)?;
        Ok(para)
    }

    fn para_props(&mut self, ppr: Node) -> Result<(ParaProps, Option<SectPr>)> {
        let props = parse_para_props(ppr);
        let mut sect = None;
        for c in children(ppr) {
            if c.tag_name().name() == "sectPr" {
                sect = Some(self.sect_pr(c)?);
            }
        }
        if props.frame.is_some_and(|frame| frame.unsupported.is_some()) {
            self.flags.text_box = true;
        }
        Ok((props, sect))
    }

    /// Walks inline-level children of one paragraph, assembling field segments.
    fn inline_level(&mut self, container: Node) -> Result<Vec<InlineContent>> {
        let mut out = Vec::new();
        self.inline_children(container, &mut out)?;
        if let Some(field) = self.field.as_mut() {
            out.push(InlineContent::ComplexFieldRegion(field.segment()));
        }
        Ok(out)
    }

    fn inline_children(&mut self, container: Node, out: &mut Vec<InlineContent>) -> Result<()> {
        for node in children(container) {
            self.inline_child(node, out)?;
        }
        Ok(())
    }

    fn inline_child(&mut self, node: Node, out: &mut Vec<InlineContent>) -> Result<()> {
        if is(node, W, "pPr") || wml_ignorable(node) {
            return Ok(());
        }
        if is(node, W, "r") {
            self.run_into_field(node, out)?;
        } else if is(node, W, "hyperlink") {
            let rel_id = attr(node, R, "id").map(str::to_string);
            let anchor = attr(node, W, "anchor").map(str::to_string);
            let mut runs = Vec::new();
            for run in children(node).filter(|child| is(*child, W, "r")) {
                runs.push(self.run(run)?);
            }
            self.push_inline(
                InlineContent::Hyperlink {
                    rel_id,
                    anchor,
                    runs,
                },
                out,
            );
        } else if is(node, W, "fldSimple") {
            let instruction = attr(node, W, "instr").unwrap_or_default().to_string();
            let mut content = Vec::new();
            for run in children(node).filter(|child| is(*child, W, "r")) {
                content.push(InlineContent::Run(self.run(run)?));
            }
            let starved = !content.iter().any(inline_has_result_content);
            self.push_inline(
                InlineContent::SimpleField(Field {
                    instruction,
                    content,
                    starved,
                }),
                out,
            );
        } else if is(node, M, "oMath") || is(node, M, "oMathPara") {
            self.push_inline(
                InlineContent::Math(super::omml::MathContent::parse(
                    node,
                    is(node, M, "oMathPara"),
                )),
                out,
            );
        } else if is(node, W, "ins") {
            // Inserted tracked text: unwrap, render runs normally.
            for run in children(node).filter(|child| is(*child, W, "r")) {
                self.run_into_field(run, out)?;
            }
        } else if is(node, W, "sdt") {
            if let Some(content) = child(node, W, "sdtContent") {
                self.inline_children(content, out)?;
            }
        } else if is(node, MC, "AlternateContent") {
            for fallback in alternate_content_fallback(node)? {
                self.inline_child(fallback, out)?;
            }
        } else {
            return Err(unknown_child("w:p", node));
        }
        Ok(())
    }

    /// Routes a run through the complex-field state machine, or emits it plainly.
    fn run_into_field(&mut self, r: Node, out: &mut Vec<InlineContent>) -> Result<()> {
        let fld_char = child(r, W, "fldChar").and_then(|f| attr(f, W, "fldCharType"));
        match (fld_char, self.field.as_mut()) {
            (Some("begin"), _) => {
                // TODO(nested fields): v1 keeps final-view order by terminating
                // the outer segment before opening the inner field.
                if let Some(prev) = self.field.take() {
                    out.push(InlineContent::ComplexFieldRegion(prev.finish(true)));
                }
                self.field = Some(FieldBuild::default());
            }
            (Some("separate"), Some(fb)) => fb.in_result = true,
            (Some("end"), _) => {
                if let Some(fb) = self.field.take() {
                    out.push(InlineContent::ComplexFieldRegion(fb.finish(true)));
                }
            }
            (_, Some(fb)) => {
                if fb.in_result {
                    let run = self.run(r)?;
                    self.push_inline(InlineContent::Run(run), out);
                } else {
                    for it in children(r) {
                        if is(it, W, "instrText") {
                            fb.instruction.push_str(it.text().unwrap_or_default());
                        }
                    }
                }
            }
            (_, None) => out.push(InlineContent::Run(self.run(r)?)),
        }
        Ok(())
    }

    fn push_inline(&mut self, item: InlineContent, out: &mut Vec<InlineContent>) {
        let has_content = inline_has_result_content(&item);
        if let Some(field) = self.field.as_mut().filter(|field| field.in_result) {
            field.any_content |= has_content;
            field.content.push(item);
        } else {
            out.push(item);
        }
    }

    fn run(&mut self, r: Node) -> Result<Run> {
        let mut run = Run::default();
        if let Some(rpr) = child(r, W, "rPr") {
            run.props = parse_run_props(rpr);
        }
        for c in children(r) {
            let name = c.tag_name().name();
            let ns = c.tag_name().namespace();
            if ns == Some(W) {
                match name {
                    "rPr" => {}
                    "t" => run
                        .content
                        .push(RunContent::Text(c.text().unwrap_or_default().to_string())),
                    "tab" => run.content.push(RunContent::Tab),
                    "br" => run.content.push(RunContent::Break(parse_break(c))),
                    "cr" => run.content.push(RunContent::Break(BreakKind::Line)),
                    "noBreakHyphen" => run.content.push(RunContent::Text("\u{2011}".into())),
                    "softHyphen" => {}
                    "drawing" => {
                        let drawing = self.wml_drawing(c)?;
                        self.note_drawing(&drawing);
                        run.content.push(RunContent::Drawing(drawing));
                    }
                    "sym" => {
                        if let Some(sym) = parse_sym(c) {
                            run.content.push(sym);
                        }
                    }
                    "footnoteReference" => {
                        if let Some(id) = parse_i64(attr(c, W, "id")) {
                            run.content.push(RunContent::NoteReference {
                                kind: NoteKind::Footnote,
                                id,
                            });
                        }
                    }
                    "endnoteReference" => {
                        if let Some(id) = parse_i64(attr(c, W, "id")) {
                            run.content.push(RunContent::NoteReference {
                                kind: NoteKind::Endnote,
                                id,
                            });
                        }
                    }
                    // Marker inside the note body; flow emits a marker from the
                    // owning note id so it always matches the in-text reference.
                    "footnoteRef"
                    | "endnoteRef"
                    | "separator"
                    | "continuationSeparator"
                    | "continuationNotice" => {}
                    "object" => {
                        let previews = self.vml_shapes(c)?;
                        let mut found_preview = false;
                        for preview in previews {
                            if preview.image_rel_id.is_some() {
                                found_preview = true;
                                run.content.push(RunContent::OlePreview(preview));
                            }
                        }
                        if !found_preview {
                            self.flags.ole_object = true;
                        }
                    }
                    "pict" => {
                        for shape in self.vml_shapes(c)? {
                            run.content.push(RunContent::VmlShape(shape));
                        }
                    }
                    "ptab" => {
                        return Err(OfficeError::unsupported(
                            crate::UnsupportedCode::UnknownElement,
                            "w:ptab",
                        ));
                    }
                    "lastRenderedPageBreak" | "fldChar" | "instrText" => {}
                    _ if wml_ignorable(c) => {}
                    _ => return Err(unknown_child("w:r", c)),
                }
            } else if ns == Some(M) && (name == "oMath" || name == "oMathPara") {
                run.content
                    .push(RunContent::Math(super::omml::MathContent::parse(
                        c,
                        name == "oMathPara",
                    )));
            } else if ns == Some(VML) {
                self.flags.vml_pict = true;
            } else if ns == Some(MC) && name == "AlternateContent" {
                for fb in alternate_content_fallback(c)? {
                    if is(fb, W, "drawing") {
                        let drawing = self.wml_drawing(fb)?;
                        self.note_drawing(&drawing);
                        run.content.push(RunContent::Drawing(drawing));
                    } else if is(fb, W, "pict") {
                        for shape in self.vml_shapes(fb)? {
                            run.content.push(RunContent::VmlShape(shape));
                        }
                    } else {
                        return Err(unknown_child("mc:Fallback", fb));
                    }
                }
            } else if !wml_ignorable(c) {
                return Err(unknown_child("w:r", c));
            }
        }
        Ok(run)
    }

    fn note_drawing(&mut self, d: &WmlDrawing) {
        if d.wordart_warp.is_some() {
            self.flags.wordart_warp = true;
        }
        match d.drawing.graphic_uri.as_deref() {
            Some(dml::URI_CHART) => self.flags.chart = true,
            Some(dml::URI_DIAGRAM) => self.flags.smart_art = true,
            _ => {}
        }
    }

    fn wml_drawing(&mut self, node: Node) -> Result<WmlDrawing> {
        let drawing = Drawing::parse_w_drawing(node);
        let container = node.descendants().find(|candidate| {
            candidate.tag_name().namespace() == Some(super::WP)
                && matches!(candidate.tag_name().name(), "inline" | "anchor")
        });
        let parsed_anchor = container
            .filter(|candidate| candidate.tag_name().name() == "anchor")
            .map(parse_anchor);
        let anchor = parsed_anchor.as_ref().map(|parsed| parsed.props);
        let anchor_violations = parsed_anchor
            .map(|parsed| parsed.violations)
            .unwrap_or_default();
        let crop = node
            .descendants()
            .find(|candidate| is(*candidate, super::A, "srcRect"))
            .map(parse_crop);
        let text_box = node
            .descendants()
            .find(|candidate| is(*candidate, W, "txbxContent"))
            .map(|content| {
                let blocks = self.text_box_story(content)?;
                let (width_pt, height_pt) = drawing.extent_pt.unwrap_or((144.0, 72.0));
                Ok(TextBox {
                    width_pt,
                    height_pt,
                    fill: drawing_fill(node),
                    outline: drawing_outline(node),
                    content: blocks,
                })
            })
            .transpose()?;
        let wordart_warp = node
            .descendants()
            .find(|candidate| is(*candidate, super::A, "prstTxWarp"))
            .and_then(|warp| attr(warp, super::A, "prst"))
            .filter(|preset| *preset != "none")
            .map(str::to_string);
        if text_box.is_none()
            && drawing.blip_rel_id.is_none()
            && !matches!(
                drawing.graphic_uri.as_deref(),
                Some(dml::URI_CHART | dml::URI_DIAGRAM)
            )
        {
            self.flags.visible_unsupported.push(format!(
                "a:graphicData:{}",
                drawing.graphic_uri.as_deref().unwrap_or("missing-uri")
            ));
        }
        Ok(WmlDrawing {
            drawing,
            anchor,
            anchor_violations,
            crop,
            text_box,
            wordart_warp,
        })
    }

    fn vml_shapes(&mut self, pict: Node) -> Result<Vec<VmlShape>> {
        let mut shapes = Vec::new();
        for node in children(pict).filter(|node| node.tag_name().namespace() == Some(VML)) {
            let kind = node.tag_name().name();
            if !matches!(kind, "rect" | "oval" | "line" | "shape") {
                return Err(OfficeError::unsupported(
                    crate::UnsupportedCode::UnknownElement,
                    format!("v:{kind}"),
                ));
            }
            let style = attr(node, VML, "style")
                .or_else(|| node.attribute("style"))
                .unwrap_or_default();
            let get = |name: &str| vml_style_pt(style, name).unwrap_or(0.0);
            let text_box = node
                .descendants()
                .find(|candidate| is(*candidate, W, "txbxContent"))
                .map(|content| self.text_box_story(content))
                .transpose()?
                .unwrap_or_default();
            let image_rel_id = node
                .descendants()
                .find(|candidate| is(*candidate, VML, "imagedata"))
                .and_then(|image| attr(image, R, "id"))
                .map(str::to_string);
            shapes.push(VmlShape {
                kind: kind.to_string(),
                x_pt: get("left"),
                y_pt: get("top"),
                width_pt: get("width").max(1.0),
                height_pt: get("height").max(1.0),
                fill: node
                    .attribute("fillcolor")
                    .and_then(|value| Rgb::from_hex(value.trim_start_matches('#'))),
                outline: node
                    .attribute("strokecolor")
                    .and_then(|value| Rgb::from_hex(value.trim_start_matches('#'))),
                image_rel_id,
                text_box,
                path: node.attribute("path").map(str::to_string),
            });
        }
        Ok(shapes)
    }

    fn table(&mut self, node: Node) -> Result<Table> {
        let mut table = Table::default();
        for c in children(node) {
            let name = c.tag_name().name();
            match name {
                "tblPr" => table.props = parse_tbl_props(c),
                "tblGrid" => {
                    for gc in children(c) {
                        if is(gc, W, "gridCol") {
                            let width = measure_twip_to_pt(attr(gc, W, "w")).unwrap_or(0.0);
                            table.grid.push(width);
                        }
                    }
                }
                "tr" => table.rows.push(self.row(c)?),
                "sdt" => {
                    if let Some(sc) = child(c, W, "sdtContent") {
                        for inner in children(sc) {
                            if is(inner, W, "tr") {
                                table.rows.push(self.row(inner)?);
                            }
                        }
                    }
                }
                _ if wml_ignorable(c) => {}
                _ => self
                    .flags
                    .visible_unsupported
                    .push(format!("w:{} in w:tbl", c.tag_name().name())),
            }
        }
        Ok(table)
    }

    fn row(&mut self, node: Node) -> Result<Row> {
        let mut row = Row::default();
        for c in children(node) {
            let name = c.tag_name().name();
            match name {
                "trPr" => row.props = parse_row_props(c),
                "tc" => row.cells.push(self.cell(c)?),
                "sdt" => {
                    if let Some(sc) = child(c, W, "sdtContent") {
                        for inner in children(sc) {
                            if is(inner, W, "tc") {
                                row.cells.push(self.cell(inner)?);
                            }
                        }
                    }
                }
                "tblPrEx" => self.flags.visible_unsupported.push("w:tblPrEx".to_string()),
                _ if wml_ignorable(c) => {}
                _ => self
                    .flags
                    .visible_unsupported
                    .push(format!("w:{} in w:tr", c.tag_name().name())),
            }
        }
        Ok(row)
    }

    fn cell(&mut self, node: Node) -> Result<Cell> {
        let mut cell = Cell::default();
        cell.props.grid_span = 1;
        for c in children(node) {
            if is(c, W, "tcPr") {
                cell.props = parse_cell_props(c);
            } else if !self.block_child(c, "w:tc", &mut cell.content)? {
                return Err(unknown_child("w:tc", c));
            }
        }
        Ok(cell)
    }

    fn sect_pr(&mut self, node: Node) -> Result<SectPr> {
        let mut sect = SectPr::default();
        for c in children(node) {
            match c.tag_name().name() {
                "pgSz" => {
                    if let Some(width) = measure_twip_to_pt(attr(c, W, "w")) {
                        sect.page_w_pt = width;
                    }
                    if let Some(height) = measure_twip_to_pt(attr(c, W, "h")) {
                        sect.page_h_pt = height;
                    }
                    match attr(c, W, "orient") {
                        None | Some("portrait") => {}
                        Some("landscape") => sect.orientation = Orientation::Landscape,
                        Some(value) => self
                            .flags
                            .visible_unsupported
                            .push(format!("w:pgSz@orient:{value}")),
                    }
                }
                "pgMar" => sect.margins = parse_margins(c),
                "cols" => {
                    if attr(c, W, "sep").is_some_and(|value| on_off(Some(value))) {
                        self.flags
                            .visible_unsupported
                            .push("w:cols@sep".to_string());
                    }
                    sect.cols = parse_cols(c);
                }
                "headerReference" => {
                    if !matches!(
                        attr(c, W, "type"),
                        None | Some("default" | "first" | "even")
                    ) {
                        self.flags.visible_unsupported.push(format!(
                            "w:headerReference@type:{}",
                            attr(c, W, "type").unwrap_or_default()
                        ));
                    }
                    if let Some(hr) = parse_hdr_ftr_ref(c) {
                        sect.header_refs.push(hr);
                    } else {
                        self.flags
                            .visible_unsupported
                            .push("w:headerReference missing r:id".to_string());
                    }
                }
                "footerReference" => {
                    if !matches!(
                        attr(c, W, "type"),
                        None | Some("default" | "first" | "even")
                    ) {
                        self.flags.visible_unsupported.push(format!(
                            "w:footerReference@type:{}",
                            attr(c, W, "type").unwrap_or_default()
                        ));
                    }
                    if let Some(fr) = parse_hdr_ftr_ref(c) {
                        sect.footer_refs.push(fr);
                    } else {
                        self.flags
                            .visible_unsupported
                            .push("w:footerReference missing r:id".to_string());
                    }
                }
                "titlePg" => sect.title_pg = toggle_child(c),
                "pgNumType" => {
                    sect.page_num_start = parse_i64(attr(c, W, "start")).map(|v| v.max(0) as u32);
                    if let Some(format) = attr(c, W, "fmt") {
                        sect.page_num_fmt = format.to_string();
                    }
                    if attr(c, W, "chapStyle").is_some() || attr(c, W, "chapSep").is_some() {
                        self.flags
                            .visible_unsupported
                            .push("w:pgNumType chapter numbering".to_string());
                    }
                }
                "type" => {
                    sect.sect_type = match attr(c, W, "val") {
                        None | Some("nextPage") => SectType::NextPage,
                        Some("continuous") => SectType::Continuous,
                        Some("oddPage") => SectType::OddPage,
                        Some("evenPage") => SectType::EvenPage,
                        Some(value) => {
                            self.flags
                                .visible_unsupported
                                .push(format!("w:type@val:{value} in w:sectPr"));
                            SectType::NextPage
                        }
                    }
                }
                "pgBorders" => sect.page_borders = parse_para_borders(c),
                "vAlign" => {
                    sect.vertical_align = match attr(c, W, "val") {
                        None | Some("top") => PageVAlign::Top,
                        Some("center") => PageVAlign::Center,
                        Some("bottom") => PageVAlign::Bottom,
                        Some(value) => {
                            self.flags
                                .visible_unsupported
                                .push(format!("w:vAlign@val:{value} in w:sectPr"));
                            PageVAlign::Top
                        }
                    }
                }
                // A document grid only constrains layout when a grid type is
                // selected. `w:type` defaults to `default` — no grid — which is
                // what Word stamps on nearly every section, so the East-Asian
                // grid types are the only ones that change the painted view.
                "docGrid" => match attr(c, W, "type") {
                    None | Some("default") => {}
                    Some(value) => self
                        .flags
                        .visible_unsupported
                        .push(format!("w:docGrid@type:{value}")),
                },
                "lnNumType" | "textDirection" | "bidi" | "rtlGutter" | "footnotePr"
                | "endnotePr" | "noEndnote" | "footnoteColumns" => self
                    .flags
                    .visible_unsupported
                    .push(format!("w:{}", c.tag_name().name())),
                // Print-driver ids, protection metadata, and superseded
                // revision properties have no painted effect.
                "paperSrc" | "printerSettings" | "sectPrChange" | "formProt" => {}
                _ => self
                    .flags
                    .visible_unsupported
                    .push(format!("w:{} in w:sectPr", c.tag_name().name())),
            }
        }
        Ok(sect)
    }
}

/// Accumulator for a complex field spanning multiple runs.
#[derive(Default)]
struct FieldBuild {
    instruction: String,
    content: Vec<InlineContent>,
    in_result: bool,
    any_content: bool,
}

impl FieldBuild {
    fn segment(&mut self) -> Field {
        Field {
            instruction: self.instruction.clone(),
            content: std::mem::take(&mut self.content),
            starved: false,
        }
    }

    fn finish(self, terminal: bool) -> Field {
        Field {
            instruction: self.instruction,
            content: self.content,
            starved: terminal && !self.any_content,
        }
    }
}

fn inline_has_result_content(inline: &InlineContent) -> bool {
    fn run_has_content(run: &Run) -> bool {
        run.content.iter().any(|content| match content {
            RunContent::Text(text) => !text.trim().is_empty(),
            _ => true,
        })
    }

    match inline {
        InlineContent::Run(run) => run_has_content(run),
        InlineContent::Hyperlink { runs, .. } => runs.iter().any(run_has_content),
        InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
            field.content.iter().any(inline_has_result_content)
        }
        InlineContent::Math(math) => !math.source_text().trim().is_empty(),
    }
}

fn last_paragraph_mut(blocks: &mut [BlockContent]) -> Option<&mut Paragraph> {
    for block in blocks.iter_mut().rev() {
        match block {
            BlockContent::Paragraph(paragraph) => return Some(paragraph),
            BlockContent::Table(table) => {
                for row in table.rows.iter_mut().rev() {
                    for cell in row.cells.iter_mut().rev() {
                        if let Some(paragraph) = last_paragraph_mut(&mut cell.content) {
                            return Some(paragraph);
                        }
                    }
                }
            }
        }
    }
    None
}

// --- Small parse helpers ----------------------------------------------------

fn toggle_child(node: Node) -> bool {
    on_off(attr(node, W, "val"))
}

struct ParsedAnchor {
    props: AnchorProps,
    violations: Vec<String>,
}

fn parse_anchor(node: Node) -> ParsedAnchor {
    let mut violations = Vec::new();
    let mut reject = |detail: &str| {
        if !violations.iter().any(|value| value == detail) {
            violations.push(detail.to_string());
        }
    };
    let direct_children = |name: &str| {
        children(node)
            .filter(|child| {
                child.tag_name().namespace() == Some(super::WP) && child.tag_name().name() == name
            })
            .collect::<Vec<_>>()
    };
    let simple_nodes = direct_children("simplePos");
    let horizontal_nodes = direct_children("positionH");
    let vertical_nodes = direct_children("positionV");
    if simple_nodes.len() != 1 {
        reject("wp:anchor must contain exactly one wp:simplePos child");
    }
    if horizontal_nodes.len() != 1 {
        reject("wp:anchor must contain exactly one wp:positionH child");
    }
    if vertical_nodes.len() != 1 {
        reject("wp:anchor must contain exactly one wp:positionV child");
    }
    let simple = simple_nodes.first().copied();
    let horizontal = horizontal_nodes.first().copied();
    let vertical = vertical_nodes.first().copied();
    let strict_on_off = |value: Option<&str>| match value {
        Some("1" | "true") => Some(true),
        Some("0" | "false") => Some(false),
        _ => None,
    };
    let use_simple_position = strict_on_off(node.attribute("simplePos")).unwrap_or_else(|| {
        reject("wp:anchor has invalid or missing simplePos");
        false
    });
    if use_simple_position {
        reject("wp:anchor simplePos=true is outside the supported positioning subset");
    }
    match node
        .attribute("allowOverlap")
        .and_then(|value| strict_on_off(Some(value)))
    {
        Some(true) => {}
        Some(false) => reject("wp:anchor allowOverlap=false is outside the supported subset"),
        None => reject("wp:anchor has invalid or missing allowOverlap"),
    }
    match node.attribute("hidden") {
        None | Some("0" | "false") => {}
        Some("1" | "true") => reject("wp:anchor hidden=true is outside the visible subset"),
        Some(_) => reject("wp:anchor has invalid hidden"),
    }
    if simple.is_none()
        || simple.is_some_and(|value| {
            parse_i64(value.attribute("x")).is_none() || parse_i64(value.attribute("y")).is_none()
        })
    {
        reject("wp:anchor has an invalid or missing wp:simplePos child");
    }

    let parse_position =
        |position: Option<Node>, horizontal_axis: bool, reject: &mut dyn FnMut(&str)| {
            let Some(position) = position else {
                reject(if horizontal_axis {
                    "wp:anchor is missing wp:positionH"
                } else {
                    "wp:anchor is missing wp:positionV"
                });
                return (0.0, None);
            };
            let offsets: Vec<_> = children(position)
                .filter(|child| {
                    child.tag_name().namespace() == Some(super::WP)
                        && child.tag_name().name() == "posOffset"
                })
                .collect();
            let aligns: Vec<_> = children(position)
                .filter(|child| {
                    child.tag_name().namespace() == Some(super::WP)
                        && child.tag_name().name() == "align"
                })
                .collect();
            if offsets.len() + aligns.len() != 1 {
                reject(if horizontal_axis {
                    "wp:positionH must contain exactly one align or posOffset"
                } else {
                    "wp:positionV must contain exactly one align or posOffset"
                });
            }
            let offset = offsets
                .first()
                .and_then(|value| value.text())
                .and_then(|value| parse_measure(Some(value)))
                .map(|emu| emu / 12_700.0);
            if !offsets.is_empty() && offset.is_none() {
                reject(if horizontal_axis {
                    "wp:positionH has an invalid posOffset"
                } else {
                    "wp:positionV has an invalid posOffset"
                });
            }
            let align = aligns.first().and_then(|value| match value.text() {
                Some(value) => match (horizontal_axis, value.trim()) {
                    (true, "left") => Some(FrameAlign::Start),
                    (true, "center") => Some(FrameAlign::Center),
                    (true, "right") => Some(FrameAlign::End),
                    (false, "top") => Some(FrameAlign::Start),
                    (false, "center") => Some(FrameAlign::Center),
                    (false, "bottom") => Some(FrameAlign::End),
                    _ => {
                        reject(if horizontal_axis {
                            "wp:positionH has an unsupported alignment"
                        } else {
                            "wp:positionV alignment is outside the supported subset"
                        });
                        None
                    }
                },
                None => {
                    reject(if horizontal_axis {
                        "wp:positionH has an empty alignment"
                    } else {
                        "wp:positionV has an empty alignment"
                    });
                    None
                }
            });
            (offset.unwrap_or(0.0), align)
        };

    let (x_pt, x_align) = parse_position(horizontal, true, &mut reject);
    let (y_pt, y_align) = parse_position(vertical, false, &mut reject);
    let h_origin = match horizontal.and_then(|value| value.attribute("relativeFrom")) {
        Some("margin") => AnchorOrigin::Margin,
        Some("page") => AnchorOrigin::Page,
        Some("column") => AnchorOrigin::Paragraph,
        other => {
            reject(&format!(
                "wp:positionH relativeFrom {other:?} is outside the supported subset"
            ));
            AnchorOrigin::Margin
        }
    };
    let v_origin = match vertical.and_then(|value| value.attribute("relativeFrom")) {
        Some("paragraph") => AnchorOrigin::Paragraph,
        Some("margin") => AnchorOrigin::Margin,
        Some("page") => AnchorOrigin::Page,
        other => {
            reject(&format!(
                "wp:positionV relativeFrom {other:?} is outside the supported subset"
            ));
            AnchorOrigin::Paragraph
        }
    };

    let wraps: Vec<_> = children(node)
        .filter(|child| {
            child.tag_name().namespace() == Some(super::WP)
                && child.tag_name().name().starts_with("wrap")
        })
        .collect();
    if wraps.len() != 1 {
        reject("wp:anchor must contain exactly one wrapping mode");
    }
    let wrap_node = wraps.first().copied();
    let wrap = match wrap_node.map(|value| value.tag_name().name()) {
        Some("wrapNone") => WrapMode::None,
        Some("wrapTopAndBottom") => WrapMode::TopAndBottom,
        Some("wrapSquare") => {
            match wrap_node.and_then(|value| value.attribute("wrapText")) {
                Some("largest") => {}
                Some("bothSides")
                    if matches!(x_align, Some(FrameAlign::Start | FrameAlign::End))
                        || h_origin != AnchorOrigin::Paragraph => {}
                _ => reject("wp:wrapSquare wrapText/position is outside the rendered subset"),
            }
            WrapMode::Square
        }
        Some("wrapTight") => {
            match wrap_node.and_then(|value| value.attribute("wrapText")) {
                Some("largest") => {}
                Some("bothSides")
                    if matches!(x_align, Some(FrameAlign::Start | FrameAlign::End))
                        || h_origin != AnchorOrigin::Paragraph => {}
                _ => reject("wp:wrapTight wrapText/position is outside the rendered subset"),
            }
            if !wrap_node.is_some_and(tight_polygon_is_full_rectangle) {
                reject("wp:wrapTight requires one canonical full-image rectangular wrapPolygon");
            }
            WrapMode::Tight
        }
        Some("wrapThrough") => {
            reject("wp:wrapThrough is outside the supported wrapping subset");
            WrapMode::None
        }
        Some(_) | None => {
            reject("wp:anchor has an unsupported wrapping mode");
            WrapMode::None
        }
    };

    let distances_pt =
        ["distT", "distB", "distL", "distR"].map(|name| match node.attribute(name) {
            None => 0.0,
            Some(value) => match parse_measure(Some(value)).filter(|value| *value >= 0.0) {
                Some(value) => value / 12_700.0,
                None => {
                    reject("wp:anchor has an invalid wrap distance");
                    0.0
                }
            },
        });
    let behind_doc = strict_on_off(node.attribute("behindDoc")).unwrap_or_else(|| {
        reject("wp:anchor has invalid or missing behindDoc");
        false
    });
    let relative_height = node
        .attribute("relativeHeight")
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or_else(|| {
            reject("wp:anchor has invalid or missing relativeHeight");
            0
        });
    for value in node
        .descendants()
        .filter(|child| matches!(child.tag_name().name(), "pctWidth" | "pctHeight"))
    {
        if value
            .text()
            .and_then(|text| text.trim().parse::<i64>().ok())
            != Some(0)
        {
            reject("relative-size wp14 anchor extensions are outside the supported subset");
        }
    }

    ParsedAnchor {
        props: AnchorProps {
            x_pt,
            x_align,
            y_pt,
            y_align,
            relative_from_h: h_origin,
            relative_from_v: v_origin,
            wrap,
            distances_pt,
            behind_doc,
            relative_height,
        },
        violations,
    }
}

fn tight_polygon_is_full_rectangle(node: Node) -> bool {
    let polygons: Vec<_> = children(node)
        .filter(|child| {
            child.tag_name().namespace() == Some(super::WP)
                && child.tag_name().name() == "wrapPolygon"
        })
        .collect();
    let [polygon] = polygons.as_slice() else {
        return false;
    };
    let point_nodes: Vec<_> = children(*polygon)
        .filter(|child| matches!(child.tag_name().name(), "start" | "lineTo"))
        .collect();
    if point_nodes.len() != 5 {
        return false;
    }
    let Some(points) = point_nodes
        .into_iter()
        .map(|child| {
            Some((
                parse_i64(child.attribute("x"))?,
                parse_i64(child.attribute("y"))?,
            ))
        })
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    if points.first() != points.last() {
        return false;
    }
    let corners = [(0, 0), (0, 21_600), (21_600, 0), (21_600, 21_600)];
    corners.iter().all(|corner| points[..4].contains(corner))
        && points[..4].iter().all(|point| corners.contains(point))
        && points
            .windows(2)
            .all(|edge| edge[0].0 == edge[1].0 || edge[0].1 == edge[1].1)
}

fn parse_crop(node: Node) -> [f64; 4] {
    let side = |name: &str| match node.attribute(name) {
        Some(value) => value
            .parse::<f64>()
            .map(|value| value / 100_000.0)
            .unwrap_or(f64::NAN),
        None => 0.0,
    };
    [side("l"), side("t"), side("r"), side("b")]
}

fn drawing_fill(node: Node) -> Option<Rgb> {
    node.descendants()
        .find(|candidate| is(*candidate, super::A, "solidFill"))
        .and_then(|fill| {
            fill.descendants()
                .find(|candidate| is(*candidate, super::A, "srgbClr"))
        })
        .and_then(|color| color.attribute("val"))
        .and_then(Rgb::from_hex)
}

fn drawing_outline(node: Node) -> Option<Rgb> {
    node.descendants()
        .find(|candidate| is(*candidate, super::A, "ln"))
        .and_then(|line| {
            line.descendants()
                .find(|candidate| is(*candidate, super::A, "srgbClr"))
        })
        .and_then(|color| color.attribute("val"))
        .and_then(Rgb::from_hex)
}

fn vml_style_pt(style: &str, name: &str) -> Option<f64> {
    let value = style.split(';').find_map(|entry| {
        let (key, value) = entry.split_once(':')?;
        (key.trim() == name).then_some(value.trim())
    })?;
    for (suffix, scale) in [("pt", 1.0), ("in", 72.0), ("cm", 72.0 / 2.54)] {
        if let Some(number) = value.strip_suffix(suffix) {
            return number
                .trim()
                .parse::<f64>()
                .ok()
                .map(|number| number * scale);
        }
    }
    value.parse().ok()
}

fn parse_break(node: Node) -> BreakKind {
    match attr(node, W, "type") {
        Some("page") => BreakKind::Page,
        Some("column") => BreakKind::Column,
        _ => BreakKind::Line,
    }
}

fn parse_sym(node: Node) -> Option<RunContent> {
    let font = attr(node, W, "font")?.to_string();
    let code = attr(node, W, "char")?;
    let char_code = u32::from_str_radix(code.trim(), 16).ok()?;
    Some(RunContent::Symbol { font, char_code })
}

fn parse_spacing(node: Node) -> Spacing {
    let before = measure_twip_to_pt(attr(node, W, "before"));
    let after = measure_twip_to_pt(attr(node, W, "after"));
    let line = parse_measure(attr(node, W, "line"));
    let rule = attr(node, W, "lineRule");
    let line = line.map(|l| match rule {
        Some("exact") => LineSpacing::Exact(l / 20.0),
        Some("atLeast") => LineSpacing::AtLeast(l / 20.0),
        _ => LineSpacing::Auto(l / 240.0),
    });
    Spacing {
        before_pt: before,
        after_pt: after,
        line,
    }
}

fn parse_indent(node: Node) -> Indent {
    let g = |a: &str| measure_twip_to_pt(attr(node, W, a));
    Indent {
        left_pt: g("left").or_else(|| g("start")),
        right_pt: g("right").or_else(|| g("end")),
        first_line_pt: g("firstLine"),
        hanging_pt: g("hanging"),
    }
}

fn parse_tabs(node: Node) -> Vec<TabStop> {
    let mut tabs = Vec::new();
    for c in children(node) {
        if !is(c, W, "tab") {
            continue;
        }
        let Some(pos_pt) = measure_twip_to_pt(attr(c, W, "pos")) else {
            continue;
        };
        let alignment = match attr(c, W, "val") {
            Some("center") => TabAlign::Center,
            Some("right") | Some("end") => TabAlign::Right,
            Some("decimal") => TabAlign::Decimal,
            Some("bar") => TabAlign::Bar,
            Some("clear") => TabAlign::Clear,
            _ => TabAlign::Left,
        };
        let leader = match attr(c, W, "leader") {
            Some("dot") => TabLeader::Dot,
            Some("hyphen") => TabLeader::Hyphen,
            Some("underscore") => TabLeader::Underscore,
            _ => TabLeader::None,
        };
        tabs.push(TabStop {
            pos_pt,
            alignment,
            leader,
        });
    }
    tabs
}

fn parse_num_pr(node: Node) -> Option<NumRef> {
    let ilvl = child(node, W, "ilvl")
        .and_then(|n| parse_i64(attr(n, W, "val")))
        .unwrap_or(0)
        .max(0) as u32;
    let num_id = child(node, W, "numId").and_then(|n| parse_i64(attr(n, W, "val")))?;
    Some(NumRef { num_id, ilvl })
}

fn parse_shd_fill(node: Node) -> Option<Rgb> {
    // Prefer explicit fill; ignore pattern (rendered as solid v1).
    match attr(node, W, "fill") {
        Some("auto") | None => None,
        Some(hex) => Rgb::from_hex(hex),
    }
}

fn parse_border(node: Node) -> Border {
    let val = attr(node, W, "val");
    let size = attr(node, W, "sz").and_then(|value| value.parse::<i64>().ok());
    let space = attr(node, W, "space").and_then(|value| value.parse::<i64>().ok());
    let color_attr = attr(node, W, "color");
    let color = color_attr.and_then(|value| match value {
        "auto" => Some(Rgb::new(0, 0, 0)),
        value => Rgb::from_hex(value),
    });
    let theme_color = attr(node, W, "themeColor").map(str::to_string);
    let has_unknown_attribute = node.attributes().any(|attribute| {
        !matches!(attribute.namespace(), None | Some(W))
            || !matches!(
                attribute.name(),
                "val"
                    | "sz"
                    | "space"
                    | "color"
                    | "themeColor"
                    | "themeTint"
                    | "themeShade"
                    | "shadow"
                    | "frame"
            )
    });
    let unsupported = if matches!(val, Some("nil" | "none")) {
        None
    } else if has_unknown_attribute {
        Some("visible paragraph border has an unsupported attribute")
    } else if val.is_none() {
        Some("paragraph border is missing w:val")
    } else if !size.is_some_and(|value| (2..=96).contains(&value)) {
        Some("paragraph border has invalid or missing w:sz")
    } else if !space.is_some_and(|value| (0..=31).contains(&value)) {
        Some("paragraph border has invalid or missing w:space")
    } else if color.is_none() && theme_color.is_none() {
        Some("paragraph border has invalid or missing color")
    } else if attr(node, W, "themeTint").is_some() || attr(node, W, "themeShade").is_some() {
        Some("paragraph border theme tint/shade is outside the rendered subset")
    } else if attr(node, W, "shadow").is_some_and(|value| !matches!(value, "0" | "false")) {
        Some("paragraph border shadow is outside the rendered subset")
    } else if attr(node, W, "frame").is_some_and(|value| !matches!(value, "0" | "false")) {
        Some("paragraph border frame is outside the rendered subset")
    } else {
        None
    };
    Border {
        val: val.unwrap_or_default().to_string(),
        size_pt: size.map(eighth_point_to_pt).unwrap_or(0.0),
        color,
        theme_color,
        space_pt: space.map(|value| value as f64).unwrap_or(0.0),
        unsupported,
    }
}

fn parse_para_borders(node: Node) -> ParaBorders {
    let mut b = ParaBorders::default();
    for c in children(node) {
        if c.tag_name().namespace() != Some(W) {
            b.unsupported = Some("unknown paragraph border element");
            continue;
        }
        let border = Some(parse_border(c));
        match c.tag_name().name() {
            "top" => b.top = border,
            "bottom" => b.bottom = border,
            "left" => b.left = border,
            "right" => b.right = border,
            "between" => b.between = border,
            "bar" => b.bar = border,
            _ => b.unsupported = Some("unknown paragraph border element"),
        }
    }
    b
}

fn parse_tbl_borders(node: Node) -> TblBorders {
    let mut b = TblBorders::default();
    for c in children(node) {
        let border = Some(parse_border(c));
        match c.tag_name().name() {
            "top" => b.top = border,
            "bottom" => b.bottom = border,
            "left" | "start" => b.left = border,
            "right" | "end" => b.right = border,
            "insideH" => b.inside_h = border,
            "insideV" => b.inside_v = border,
            _ => {}
        }
    }
    b
}

fn parse_cell_margins(node: Node) -> CellMargins {
    let mut m = CellMargins::default();
    let width = |c: Node| match TblWidth::parse(attr(c, W, "w"), attr(c, W, "type")) {
        TblWidth::Pt(v) => Some(v),
        _ => None,
    };
    for c in children(node) {
        match c.tag_name().name() {
            "top" => m.top = width(c),
            "bottom" => m.bottom = width(c),
            "left" | "start" => m.left = width(c),
            "right" | "end" => m.right = width(c),
            _ => {}
        }
    }
    m
}

fn parse_floating_table_distances(node: Node) -> Option<FloatingTableDistances> {
    let positioning = children(node).find(|child| is(*child, W, "tblpPr"))?;
    let distance = |name| {
        parse_i64(attr(positioning, W, name))
            .map(twip_to_pt)
            .unwrap_or(0.0)
            .max(0.0)
    };
    Some(FloatingTableDistances {
        left_pt: distance("leftFromText"),
        right_pt: distance("rightFromText"),
        bottom_pt: distance("bottomFromText"),
    })
}

/// Parses `w:tblPr` — both the table's direct properties and the `w:tblPr` of a
/// table style in `word/styles.xml`.
pub fn parse_tbl_props(node: Node) -> TblProps {
    let mut props = TblProps::default();
    for c in children(node) {
        match c.tag_name().name() {
            "tblW" => props.width = Some(TblWidth::parse(attr(c, W, "w"), attr(c, W, "type"))),
            "jc" => match attr(c, W, "val").and_then(Jc::parse) {
                Some(alignment @ (Jc::Start | Jc::Center | Jc::End)) => {
                    props.alignment = Some(alignment)
                }
                _ => props.visible_unsupported.push(format!(
                    "w:jc@val:{} in w:tblPr",
                    attr(c, W, "val").unwrap_or("missing")
                )),
            },
            "tblBorders" => {
                if children(c).any(|edge| matches!(edge.tag_name().name(), "tl2br" | "tr2bl")) {
                    props
                        .visible_unsupported
                        .push("diagonal table border".to_string());
                }
                props.borders = parse_tbl_borders(c);
            }
            "tblCellMar" => props.cell_margins = parse_cell_margins(c),
            "tblLayout" => match attr(c, W, "type") {
                None | Some("auto") => props.layout_fixed = false,
                Some("fixed") => props.layout_fixed = true,
                Some(value) => props
                    .visible_unsupported
                    .push(format!("w:tblLayout@type:{value}")),
            },
            "tblStyle" => props.style_id = attr(c, W, "val").map(str::to_string),
            "tblLook" => props.look = TblLook::parse(c),
            "tblStyleRowBandSize" => {
                props.row_band_size = parse_i64(attr(c, W, "val")).map(|v| v.max(1) as u32)
            }
            "tblStyleColBandSize" => {
                props.col_band_size = parse_i64(attr(c, W, "val")).map(|v| v.max(1) as u32)
            }
            "tblInd" => {
                props.indent_pt = match TblWidth::parse(attr(c, W, "w"), attr(c, W, "type")) {
                    TblWidth::Pt(v) => Some(v),
                    _ => {
                        props
                            .visible_unsupported
                            .push("w:tblInd without dxa width".to_string());
                        None
                    }
                }
            }
            "tblpPr" => {
                let origin = |value: Option<&str>| match value {
                    Some("page") => AnchorOrigin::Page,
                    Some("margin") => AnchorOrigin::Margin,
                    _ => AnchorOrigin::Paragraph,
                };
                let wrap_padding_pt = ["leftFromText", "rightFromText"]
                    .map(|name| {
                        parse_i64(attr(c, W, name))
                            .map(twip_to_pt)
                            .unwrap_or(0.0)
                            .max(0.0)
                    })
                    .into_iter()
                    .fold(0.0, f64::max);
                props.floating = Some(FloatingTableProps {
                    x_pt: parse_i64(attr(c, W, "tblpX"))
                        .map(twip_to_pt)
                        .unwrap_or(0.0),
                    y_pt: parse_i64(attr(c, W, "tblpY"))
                        .map(twip_to_pt)
                        .unwrap_or(0.0),
                    horizontal: origin(attr(c, W, "horzAnchor")),
                    vertical: origin(attr(c, W, "vertAnchor")),
                    wrap_padding_pt,
                });
            }
            "shd" | "bidiVisual" | "tblOverlap" | "tblCellSpacing" => props
                .visible_unsupported
                .push(format!("w:{}", c.tag_name().name())),
            // Accessibility/description metadata has no painted effect.
            "tblCaption" | "tblDescription" => {}
            // Revision history stores superseded properties; current
            // properties already describe the painted final view.
            "tblPrChange" => {}
            _ => props
                .visible_unsupported
                .push(format!("w:{} in w:tblPr", c.tag_name().name())),
        }
    }
    props
}

/// Parses `w:trPr` — a row's direct properties or a table style's row block.
pub fn parse_row_props(node: Node) -> RowProps {
    let mut props = RowProps::default();
    for c in children(node) {
        match c.tag_name().name() {
            "gridBefore" => {
                props.grid_before = parse_i64(attr(c, W, "val")).unwrap_or(0).max(0) as u32
            }
            "gridAfter" => {
                props.grid_after = parse_i64(attr(c, W, "val")).unwrap_or(0).max(0) as u32
            }
            "wBefore" => {
                props.width_before = Some(TblWidth::parse(attr(c, W, "w"), attr(c, W, "type")))
            }
            "wAfter" => {
                props.width_after = Some(TblWidth::parse(attr(c, W, "w"), attr(c, W, "type")))
            }
            "trHeight" => {
                let height = measure_twip_to_pt(attr(c, W, "val"));
                match (attr(c, W, "hRule"), height) {
                    (Some("auto"), _) => props.height = None,
                    (None | Some("atLeast"), Some(height)) => {
                        props.height = Some(RowHeight::AtLeast(height))
                    }
                    (Some("exact"), Some(height)) => props.height = Some(RowHeight::Exact(height)),
                    (_, None) => props
                        .visible_unsupported
                        .push("w:trHeight missing valid w:val".to_string()),
                    (Some(value), Some(_)) => props
                        .visible_unsupported
                        .push(format!("w:trHeight@hRule:{value}")),
                }
            }
            "tblHeader" => props.header = toggle_child(c),
            "cantSplit" => props.cant_split = toggle_child(c),
            "hidden" => props.hidden = toggle_child(c),
            "tblCellSpacing" | "jc" | "ins" | "del" => props
                .visible_unsupported
                .push(format!("w:{} in w:trPr", c.tag_name().name())),
            // `divId` is a content-structure id. `trPrChange` stores superseded
            // row properties. `cnfStyle` caches which conditional slots of the
            // table style Word decided this row falls into — the same decision
            // `super::table_style` recomputes from the table's position grid,
            // and it carries no formatting of its own. None paint independently.
            "divId" | "trPrChange" | "cnfStyle" => {}
            _ => props
                .visible_unsupported
                .push(format!("w:{} in w:trPr", c.tag_name().name())),
        }
    }
    props
}

/// Parses `w:tcPr` — a cell's direct properties or a table style's cell block.
pub fn parse_cell_props(node: Node) -> CellProps {
    let mut props = CellProps {
        grid_span: 1,
        ..Default::default()
    };
    for c in children(node) {
        match c.tag_name().name() {
            "tcW" => props.width = Some(TblWidth::parse(attr(c, W, "w"), attr(c, W, "type"))),
            "gridSpan" => props.grid_span = parse_i64(attr(c, W, "val")).unwrap_or(1).max(1) as u32,
            "vMerge" => {
                props.v_merge = Some(match attr(c, W, "val") {
                    Some("restart") => VMerge::Restart,
                    _ => VMerge::Continue,
                })
            }
            "tcBorders" => {
                if children(c).any(|edge| matches!(edge.tag_name().name(), "tl2br" | "tr2bl")) {
                    props
                        .visible_unsupported
                        .push("diagonal cell border".to_string());
                }
                props.borders = parse_tbl_borders(c);
            }
            "shd" => props.shading = parse_shd_fill(c),
            "tcMar" => props.margins = parse_cell_margins(c),
            "vAlign" => match attr(c, W, "val") {
                None | Some("top") => props.v_align = VAlign::Top,
                Some("center") => props.v_align = VAlign::Center,
                Some("bottom") => props.v_align = VAlign::Bottom,
                Some(value) => props
                    .visible_unsupported
                    .push(format!("w:vAlign@val:{value} in w:tcPr")),
            },
            "hMerge" | "textDirection" | "noWrap" | "tcFitText" | "hideMark" | "cellIns"
            | "cellDel" | "cellMerge" => props
                .visible_unsupported
                .push(format!("w:{} in w:tcPr", c.tag_name().name())),
            // Revision history stores superseded properties; current `tcPr`
            // already describes the painted final view. `cnfStyle` only caches
            // the conditional slots `super::table_style` recomputes positionally.
            "tcPrChange" | "cnfStyle" => {}
            _ => props
                .visible_unsupported
                .push(format!("w:{} in w:tcPr", c.tag_name().name())),
        }
    }
    props
}

fn parse_margins(node: Node) -> Margins {
    let g = |a: &str, d: f64| measure_twip_to_pt(attr(node, W, a)).unwrap_or(d);
    Margins {
        top: g("top", 72.0),
        right: g("right", 72.0),
        bottom: g("bottom", 72.0),
        left: g("left", 72.0),
        header: g("header", 36.0),
        footer: g("footer", 36.0),
        gutter: g("gutter", 0.0),
    }
}

fn parse_cols(node: Node) -> ColumnSpec {
    let num = parse_i64(attr(node, W, "num")).unwrap_or(1).max(0) as u32;
    let space_pt = measure_twip_to_pt(attr(node, W, "space"));
    let mut cols = Vec::new();
    let mut spaces = Vec::new();
    for c in children(node) {
        if is(c, W, "col")
            && let Some(width) = measure_twip_to_pt(attr(c, W, "w"))
        {
            cols.push(width);
            spaces.push(
                measure_twip_to_pt(attr(c, W, "space"))
                    .or(space_pt)
                    .unwrap_or(36.0),
            );
        }
    }
    ColumnSpec {
        num,
        cols,
        space_pt,
        spaces,
    }
}

fn parse_hdr_ftr_ref(node: Node) -> Option<HdrFtrRef> {
    let rel_id = attr(node, R, "id")?.to_string();
    let kind = match attr(node, W, "type") {
        Some("first") => HdrFtrKind::First,
        Some("even") => HdrFtrKind::Even,
        _ => HdrFtrKind::Default,
    };
    Some(HdrFtrRef { kind, rel_id })
}

/// Parses a `w:pPr` element into [`ParaProps`] (public: styles.rs reuses it).
///
/// Ignores `w:sectPr` (handled by the section walker) and does not raise the
/// text-box flag for `w:framePr` (styles are not document content).
pub fn parse_para_props(ppr: Node) -> ParaProps {
    let mut props = ParaProps::default();
    for c in children(ppr) {
        match c.tag_name().name() {
            "pStyle" => props.style_id = attr(c, W, "val").map(str::to_string),
            "jc" => match attr(c, W, "val").and_then(Jc::parse) {
                Some(alignment) => props.alignment = Some(alignment),
                None => props.visible_unsupported.push(format!(
                    "w:jc@val:{} in w:pPr",
                    attr(c, W, "val").unwrap_or("missing")
                )),
            },
            "bidi" => props.bidi = Some(toggle_child(c)),
            "spacing" => props.spacing = parse_spacing(c),
            "ind" => props.indent = parse_indent(c),
            "tabs" => props.tabs = parse_tabs(c),
            "numPr" => props.numbering = parse_num_pr(c),
            "keepNext" => props.keep_next = toggle_child(c),
            "keepLines" => props.keep_lines = toggle_child(c),
            "pageBreakBefore" => props.page_break_before = toggle_child(c),
            "widowControl" => props.widow_control = Some(toggle_child(c)),
            "pBdr" => props.borders = parse_para_borders(c),
            "shd" => props.shading = parse_shd_fill(c),
            "contextualSpacing" => props.contextual_spacing = toggle_child(c),
            "outlineLvl" => {
                props.outline_lvl = parse_i64(attr(c, W, "val")).map(|v| v.clamp(0, 8) as u8)
            }
            "framePr" => props.frame = Some(parse_frame_props(c)),
            "rPr" => props.mark_rpr = Some(parse_run_props(c)),
            "suppressLineNumbers"
            | "suppressAutoHyphens"
            | "kinsoku"
            | "wordWrap"
            | "overflowPunct"
            | "topLinePunct"
            | "autoSpaceDE"
            | "autoSpaceDN"
            | "adjustRightInd"
            | "snapToGrid"
            | "mirrorIndents"
            | "suppressOverlap"
            | "textDirection"
            | "textAlignment"
            | "textboxTightWrap" => props
                .visible_unsupported
                .push(format!("w:{} in w:pPr", c.tag_name().name())),
            // Section properties are parsed by the section walker. `divId` is
            // structural metadata and `pPrChange` stores superseded properties.
            // `cnfStyle` caches the conditional table-style slots that
            // `super::table_style` recomputes positionally. None paint
            // independently.
            "sectPr" | "divId" | "pPrChange" | "cnfStyle" => {}
            _ => props
                .visible_unsupported
                .push(format!("w:{} in w:pPr", c.tag_name().name())),
        }
    }
    props
}

fn parse_frame_props(node: Node) -> FrameProps {
    let twips = |name| measure_twip_to_pt(attr(node, W, name));
    let mut unsupported = None;
    let x_pt = twips("x");
    let y_pt = twips("y");
    if attr(node, W, "x").is_some() && x_pt.is_none()
        || attr(node, W, "y").is_some() && y_pt.is_none()
    {
        unsupported = Some("invalid frame offset");
    }
    let width_pt = twips("w");
    let height_pt = twips("h");
    if attr(node, W, "w").is_some_and(|_| width_pt.is_none_or(|value| value <= 0.0))
        || attr(node, W, "h").is_some_and(|_| height_pt.is_none_or(|value| value < 0.0))
    {
        unsupported = Some("invalid frame size");
    }
    let mut align = |value: Option<&str>, axis: &'static str| match value {
        None | Some("left" | "top") => FrameAlign::Start,
        Some("center") => FrameAlign::Center,
        Some("right" | "bottom") => FrameAlign::End,
        Some(_) => {
            unsupported = Some(axis);
            FrameAlign::Start
        }
    };
    let x_align = align(attr(node, W, "xAlign"), "unsupported frame xAlign");
    let y_align = align(attr(node, W, "yAlign"), "unsupported frame yAlign");
    let mut anchor = |value: Option<&str>, axis: &'static str| match value {
        None | Some("text") => AnchorOrigin::Paragraph,
        Some("margin") => AnchorOrigin::Margin,
        Some("page") => AnchorOrigin::Page,
        Some(_) => {
            unsupported = Some(axis);
            AnchorOrigin::Paragraph
        }
    };
    let horizontal = anchor(attr(node, W, "hAnchor"), "unsupported frame hAnchor");
    let vertical = anchor(attr(node, W, "vAnchor"), "unsupported frame vAnchor");
    let height_rule = match attr(node, W, "hRule") {
        None | Some("auto") => FrameHeightRule::Auto,
        Some("atLeast") => FrameHeightRule::AtLeast,
        Some("exact") if height_pt.is_some() => FrameHeightRule::Exact,
        Some("exact") => {
            unsupported = Some("exact frame height is missing");
            FrameHeightRule::Exact
        }
        Some(_) => {
            unsupported = Some("unsupported frame hRule");
            FrameHeightRule::Auto
        }
    };
    let wrap = match attr(node, W, "wrap") {
        None | Some("around" | "auto") => WrapMode::Square,
        Some("none") => WrapMode::None,
        Some("notBeside") => WrapMode::TopAndBottom,
        Some(_) => {
            unsupported = Some("unsupported frame wrap");
            WrapMode::TopAndBottom
        }
    };
    if attr(node, W, "dropCap").is_some_and(|value| value != "none") {
        unsupported = Some("unsupported frame dropCap");
    }
    if attr(node, W, "lines").is_some()
        || attr(node, W, "hSpace").is_some()
        || attr(node, W, "vSpace").is_some()
    {
        unsupported = Some("unsupported frame spacing or line constraint");
    }
    FrameProps {
        x_pt,
        y_pt,
        x_align,
        y_align,
        horizontal,
        vertical,
        width_pt,
        height_pt,
        height_rule,
        wrap,
        unsupported,
    }
}

/// Parses a `w:rPr` element into [`RunProps`] (public: styles.rs reuses it).
pub fn parse_run_props(rpr: Node) -> RunProps {
    let mut props = RunProps::default();
    let mut bold_cs = None;
    let mut italic_cs = None;
    let mut size_cs = None;
    let mut saw_size_cs = false;
    for c in children(rpr) {
        match c.tag_name().name() {
            "rStyle" => props.style_id = attr(c, W, "val").map(str::to_string),
            "b" => props.bold = Some(toggle_child(c)),
            "bCs" => bold_cs = Some(toggle_child(c)),
            "i" => props.italic = Some(toggle_child(c)),
            "iCs" => italic_cs = Some(toggle_child(c)),
            "u" => {
                props.underline = Some(
                    attr(c, W, "val")
                        .map(UnderlineKind::parse)
                        .unwrap_or(UnderlineKind::Single),
                )
            }
            "strike" => props.strike = Some(toggle_child(c)),
            "dstrike" => props.dstrike = Some(toggle_child(c)),
            "caps" => props.caps = Some(toggle_child(c)),
            "smallCaps" => props.small_caps = Some(toggle_child(c)),
            "vanish" => props.vanish = Some(toggle_child(c)),
            "color" => {
                props.color = attr(c, W, "val").map(|v| match v {
                    "auto" => RunColor::Auto,
                    hex => Rgb::from_hex(hex)
                        .map(RunColor::Rgb)
                        .unwrap_or(RunColor::Auto),
                })
            }
            "highlight" => props.highlight = attr(c, W, "val").and_then(named_highlight),
            "shd" => props.shading = parse_shd_fill(c),
            "sz" => props.size_pt = parse_measure(attr(c, W, "val")).map(|value| value / 2.0),
            "szCs" => {
                saw_size_cs = true;
                size_cs = parse_measure(attr(c, W, "val")).map(|value| value / 2.0);
            }
            "rFonts" => props.fonts = parse_run_fonts(c),
            "vertAlign" => {
                props.vert_align = match attr(c, W, "val") {
                    Some("superscript") => Some(VertAlign::Super),
                    Some("subscript") => Some(VertAlign::Sub),
                    Some("baseline") => Some(VertAlign::Baseline),
                    _ => None,
                }
            }
            "spacing" => props.spacing_pt = measure_twip_to_pt(attr(c, W, "val")),
            // `w:kern` present enables kerning at sizes >= val (half-points → points);
            // a missing val defaults to 0 (kern at any size).
            "kern" => {
                props.kern_min_pt = Some(
                    parse_measure(attr(c, W, "val"))
                        .map(|value| value / 2.0)
                        .unwrap_or(0.0),
                )
            }
            "rtl" => props.rtl = Some(toggle_child(c)),
            "outline" | "shadow" | "emboss" | "imprint" | "snapToGrid" | "w" | "position"
            | "effect" | "bdr" | "fitText" | "cs" | "em" | "eastAsianLayout" | "specVanish"
            | "oMath" => props
                .visible_unsupported
                .push(format!("w:{} in w:rPr", c.tag_name().name())),
            // Language/proofing/revision metadata does not paint in print
            // layout. `webHidden` applies only to web-layout display.
            "lang" | "noProof" | "rPrChange" | "webHidden" => {}
            _ => props
                .visible_unsupported
                .push(format!("w:{} in w:rPr", c.tag_name().name())),
        }
    }
    // Word commonly writes complex-script mirrors beside the primary property.
    // They are provably redundant only when both values at this cascade level
    // are identical; a distinct/missing primary value needs script-aware style
    // splitting and therefore stays fail-closed.
    if bold_cs.is_some() && bold_cs != props.bold {
        props
            .visible_unsupported
            .push("w:bCs differs from w:b".to_string());
    }
    if italic_cs.is_some() && italic_cs != props.italic {
        props
            .visible_unsupported
            .push("w:iCs differs from w:i".to_string());
    }
    if saw_size_cs && (size_cs.is_none() || size_cs != props.size_pt) {
        props
            .visible_unsupported
            .push("w:szCs differs from w:sz".to_string());
    }
    props
}

fn parse_run_fonts(node: Node) -> RunFonts {
    RunFonts {
        ascii: attr(node, W, "ascii").map(str::to_string),
        h_ansi: attr(node, W, "hAnsi").map(str::to_string),
        east_asia: attr(node, W, "eastAsia").map(str::to_string),
        cs: attr(node, W, "cs").map(str::to_string),
    }
}

/// Maps a WordprocessingML named highlight to an approximate sRGB color.
fn named_highlight(name: &str) -> Option<Rgb> {
    let c = match name {
        "yellow" => Rgb::new(255, 255, 0),
        "green" => Rgb::new(0, 255, 0),
        "cyan" => Rgb::new(0, 255, 255),
        "magenta" => Rgb::new(255, 0, 255),
        "blue" => Rgb::new(0, 0, 255),
        "red" => Rgb::new(255, 0, 0),
        "darkBlue" => Rgb::new(0, 0, 139),
        "darkCyan" => Rgb::new(0, 139, 139),
        "darkGreen" => Rgb::new(0, 100, 0),
        "darkMagenta" => Rgb::new(139, 0, 139),
        "darkRed" => Rgb::new(139, 0, 0),
        "darkYellow" => Rgb::new(128, 128, 0),
        "darkGray" => Rgb::new(169, 169, 169),
        "lightGray" => Rgb::new(211, 211, 211),
        "black" => Rgb::new(0, 0, 0),
        "white" => Rgb::new(255, 255, 255),
        "none" => return None,
        _ => return None,
    };
    Some(c)
}

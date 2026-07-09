//! WordprocessingML model of `word/document.xml` (parse only — no layout).
//!
//! Parsing is fail-closed (see [`super`]): known children are modelled,
//! ignorable bookkeeping is skipped, enumerated-but-unsupported constructs set a
//! flag on [`DocFlags`] (so `profile()` can report them all at once), and any
//! other element is an `Unsupported{UnknownElement}` error.

use super::dml::{self, Drawing};
use super::{
    M, MC, Node, R, Rgb, TblWidth, VML, W, alternate_content_fallback, attr, child, children,
    eighth_point_to_pt, half_point_to_pt, is, on_off, parse_i64, twip_to_pt, unknown_child,
    wml_ignorable,
};
use crate::{OfficeError, Result};

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
}

/// A block-level item in a body or table cell.
#[derive(Debug, Clone)]
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
}

/// A field: its instruction text plus the cached result runs Word last rendered.
#[derive(Debug, Clone, Default)]
pub struct Field {
    /// The field instruction (e.g. ` PAGE `, ` HYPERLINK "url" `).
    pub instruction: String,
    /// Cached result runs (between `separate` and `end`).
    pub cached_runs: Vec<Run>,
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
    Drawing(Drawing),
    /// A symbol glyph from a specific font (`w:sym`).
    Symbol {
        /// The symbol font family.
        font: String,
        /// The character code (often in the F0xx PUA range).
        char_code: u32,
    },
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
            },
            shading: self.shading.or(base.shading),
            contextual_spacing: self.contextual_spacing || base.contextual_spacing,
            outline_lvl: self.outline_lvl.or(base.outline_lvl),
        }
    }
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
    /// Space from the text in points.
    pub space_pt: f64,
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
    /// Row height rule.
    pub height: Option<RowHeight>,
    /// This is a repeatable header row.
    pub header: bool,
    /// The row may not split across pages.
    pub cant_split: bool,
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
    /// Section start type.
    pub sect_type: SectType,
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
            sect_type: SectType::NextPage,
        }
    }
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

/// Parses `word/document.xml` into a [`Document`].
pub fn parse_document(xml: &str) -> Result<Document> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| OfficeError::Invalid(format!("document.xml XML: {e}")))?;
    let root = doc.root_element();
    let body = child(root, W, "body")
        .ok_or_else(|| OfficeError::Invalid("document.xml has no w:body".into()))?;

    let mut p = WmlParser::default();
    let content = p.block_level(body, "w:body")?;

    // The trailing direct-child w:sectPr is the final section.
    let final_sect = match child(body, W, "sectPr") {
        Some(sp) => p.sect_pr(sp)?,
        None => SectPr::default(),
    };

    Ok(Document {
        body: content,
        final_sect,
        flags: p.flags,
    })
}

/// Parses a header/footer part body into block content (same engine as the body).
pub fn parse_hdr_ftr(xml: &str) -> Result<Vec<BlockContent>> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| OfficeError::Invalid(format!("header/footer XML: {e}")))?;
    let root = doc.root_element(); // w:hdr or w:ftr
    let mut p = WmlParser::default();
    p.block_level(root, "w:hdr/ftr")
}

#[derive(Default)]
struct WmlParser {
    flags: DocFlags,
}

impl WmlParser {
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
            match c.tag_name().name() {
                "framePr" => self.flags.text_box = true,
                "sectPr" => sect = Some(self.sect_pr(c)?),
                _ => {}
            }
        }
        Ok((props, sect))
    }

    /// Walks inline-level children of `w:p` (or `w:hyperlink`), assembling
    /// complex fields spanning multiple runs.
    fn inline_level(&mut self, container: Node) -> Result<Vec<InlineContent>> {
        let mut out = Vec::new();
        let mut field: Option<FieldBuild> = None;
        for node in children(container) {
            if is(node, W, "pPr") || wml_ignorable(node) {
                continue;
            }
            if is(node, W, "r") {
                self.run_into_field(node, &mut field, &mut out)?;
            } else if is(node, W, "hyperlink") {
                let rel_id = attr(node, R, "id").map(str::to_string);
                let anchor = attr(node, W, "anchor").map(str::to_string);
                let mut runs = Vec::new();
                for r in children(node) {
                    if is(r, W, "r") {
                        runs.push(self.run(r)?);
                    }
                }
                out.push(InlineContent::Hyperlink {
                    rel_id,
                    anchor,
                    runs,
                });
            } else if is(node, W, "fldSimple") {
                let instruction = attr(node, W, "instr").unwrap_or_default().to_string();
                let mut cached_runs = Vec::new();
                for r in children(node) {
                    if is(r, W, "r") {
                        cached_runs.push(self.run(r)?);
                    }
                }
                out.push(InlineContent::SimpleField(Field {
                    instruction,
                    cached_runs,
                }));
            } else if is(node, W, "ins") {
                // Inserted tracked text: unwrap, render runs normally.
                for r in children(node) {
                    if is(r, W, "r") {
                        self.run_into_field(r, &mut field, &mut out)?;
                    }
                }
            } else if is(node, W, "sdt") {
                if let Some(sc) = child(node, W, "sdtContent") {
                    out.extend(self.inline_level(sc)?);
                }
            } else if is(node, MC, "AlternateContent") {
                for fb in alternate_content_fallback(node)? {
                    if is(fb, W, "r") {
                        self.run_into_field(fb, &mut field, &mut out)?;
                    }
                }
            } else {
                return Err(unknown_child("w:p", node));
            }
        }
        // An unterminated field: flush whatever we have as a region.
        if let Some(fb) = field {
            out.push(InlineContent::ComplexFieldRegion(fb.finish()));
        }
        Ok(out)
    }

    /// Routes a run through the complex-field state machine, or emits it plainly.
    fn run_into_field(
        &mut self,
        r: Node,
        field: &mut Option<FieldBuild>,
        out: &mut Vec<InlineContent>,
    ) -> Result<()> {
        let fld_char = child(r, W, "fldChar").and_then(|f| attr(f, W, "fldCharType"));
        match (fld_char, field.as_mut()) {
            (Some("begin"), _) => {
                if let Some(prev) = field.take() {
                    out.push(InlineContent::ComplexFieldRegion(prev.finish()));
                }
                *field = Some(FieldBuild::default());
            }
            (Some("separate"), Some(fb)) => fb.in_result = true,
            (Some("end"), _) => {
                if let Some(fb) = field.take() {
                    out.push(InlineContent::ComplexFieldRegion(fb.finish()));
                }
            }
            (_, Some(fb)) => {
                if fb.in_result {
                    fb.cached_runs.push(self.run(r)?);
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
                        let d = Drawing::parse_w_drawing(c);
                        self.note_drawing(&d);
                        run.content.push(RunContent::Drawing(d));
                    }
                    "sym" => {
                        if let Some(sym) = parse_sym(c) {
                            run.content.push(sym);
                        }
                    }
                    "footnoteReference" => self.flags.footnotes = true,
                    "endnoteReference" => self.flags.endnotes = true,
                    "object" => self.flags.ole_object = true,
                    "pict" => self.flags.vml_pict = true,
                    "ptab" | "lastRenderedPageBreak" | "fldChar" | "instrText" => {}
                    _ if wml_ignorable(c) => {}
                    _ => return Err(unknown_child("w:r", c)),
                }
            } else if ns == Some(M) && (name == "oMath" || name == "oMathPara") {
                self.flags.equation = true;
            } else if ns == Some(VML) {
                self.flags.vml_pict = true;
            } else if ns == Some(MC) && name == "AlternateContent" {
                for fb in alternate_content_fallback(c)? {
                    if is(fb, W, "drawing") {
                        let d = Drawing::parse_w_drawing(fb);
                        self.note_drawing(&d);
                        run.content.push(RunContent::Drawing(d));
                    }
                }
            }
            // Unknown non-W run children are tolerated (extension content).
        }
        Ok(run)
    }

    fn note_drawing(&mut self, d: &Drawing) {
        if d.kind == dml::DrawingKind::Anchor {
            self.flags.anchored_drawing = true;
        }
        if d.cropped {
            self.flags.image_crop = true;
        }
        match d.graphic_uri.as_deref() {
            Some(dml::URI_CHART) => self.flags.chart = true,
            Some(dml::URI_DIAGRAM) => self.flags.smart_art = true,
            _ => {}
        }
    }

    fn table(&mut self, node: Node) -> Result<Table> {
        let mut table = Table::default();
        for c in children(node) {
            let name = c.tag_name().name();
            match name {
                "tblPr" => table.props = self.tbl_props(c),
                "tblGrid" => {
                    for gc in children(c) {
                        if is(gc, W, "gridCol") {
                            let w = parse_i64(attr(gc, W, "w")).unwrap_or(0);
                            table.grid.push(twip_to_pt(w));
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
                _ => {}
            }
        }
        Ok(table)
    }

    fn tbl_props(&mut self, node: Node) -> TblProps {
        let mut props = TblProps::default();
        for c in children(node) {
            match c.tag_name().name() {
                "tblW" => props.width = Some(TblWidth::parse(attr(c, W, "w"), attr(c, W, "type"))),
                "jc" => props.alignment = attr(c, W, "val").and_then(Jc::parse),
                "tblBorders" => props.borders = parse_tbl_borders(c),
                "tblCellMar" => props.cell_margins = parse_cell_margins(c),
                "tblLayout" => props.layout_fixed = attr(c, W, "type") == Some("fixed"),
                "tblStyle" => props.style_id = attr(c, W, "val").map(str::to_string),
                "tblInd" => {
                    props.indent_pt = match TblWidth::parse(attr(c, W, "w"), attr(c, W, "type")) {
                        TblWidth::Pt(v) => Some(v),
                        _ => None,
                    }
                }
                "tblpPr" => self.flags.floating_table = true,
                _ => {}
            }
        }
        props
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
                "tblPrEx" => {} // row-level table property exceptions — tolerated v1
                _ if wml_ignorable(c) => {}
                _ => {}
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
                    if let Some(w) = parse_i64(attr(c, W, "w")) {
                        sect.page_w_pt = twip_to_pt(w);
                    }
                    if let Some(h) = parse_i64(attr(c, W, "h")) {
                        sect.page_h_pt = twip_to_pt(h);
                    }
                    if attr(c, W, "orient") == Some("landscape") {
                        sect.orientation = Orientation::Landscape;
                    }
                }
                "pgMar" => sect.margins = parse_margins(c),
                "cols" => sect.cols = parse_cols(c),
                "headerReference" => {
                    if let Some(hr) = parse_hdr_ftr_ref(c) {
                        sect.header_refs.push(hr);
                    }
                }
                "footerReference" => {
                    if let Some(fr) = parse_hdr_ftr_ref(c) {
                        sect.footer_refs.push(fr);
                    }
                }
                "titlePg" => sect.title_pg = toggle_child(c),
                "pgNumType" => {
                    sect.page_num_start = parse_i64(attr(c, W, "start")).map(|v| v.max(0) as u32)
                }
                "type" => {
                    sect.sect_type = match attr(c, W, "val") {
                        Some("continuous") => SectType::Continuous,
                        Some("oddPage") => SectType::OddPage,
                        Some("evenPage") => SectType::EvenPage,
                        _ => SectType::NextPage,
                    }
                }
                _ => {}
            }
        }
        if sect.cols.is_multi() {
            self.flags.multi_column = true;
        }
        Ok(sect)
    }
}

/// Accumulator for a complex field spanning multiple runs.
#[derive(Default)]
struct FieldBuild {
    instruction: String,
    cached_runs: Vec<Run>,
    in_result: bool,
}

impl FieldBuild {
    fn finish(self) -> Field {
        Field {
            instruction: self.instruction,
            cached_runs: self.cached_runs,
        }
    }
}

// --- Small parse helpers ----------------------------------------------------

fn toggle_child(node: Node) -> bool {
    on_off(attr(node, W, "val"))
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
    let before = parse_i64(attr(node, W, "before")).map(twip_to_pt);
    let after = parse_i64(attr(node, W, "after")).map(twip_to_pt);
    let line = parse_i64(attr(node, W, "line"));
    let rule = attr(node, W, "lineRule");
    let line = line.map(|l| match rule {
        Some("exact") => LineSpacing::Exact(twip_to_pt(l)),
        Some("atLeast") => LineSpacing::AtLeast(twip_to_pt(l)),
        _ => LineSpacing::Auto(l as f64 / 240.0),
    });
    Spacing {
        before_pt: before,
        after_pt: after,
        line,
    }
}

fn parse_indent(node: Node) -> Indent {
    let g = |a: &str| parse_i64(attr(node, W, a)).map(twip_to_pt);
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
        let Some(pos) = parse_i64(attr(c, W, "pos")) else {
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
            pos_pt: twip_to_pt(pos),
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
    Border {
        val: attr(node, W, "val").unwrap_or("single").to_string(),
        size_pt: parse_i64(attr(node, W, "sz"))
            .map(eighth_point_to_pt)
            .unwrap_or(0.5),
        color: attr(node, W, "color").and_then(Rgb::from_hex),
        space_pt: parse_i64(attr(node, W, "space"))
            .map(|v| v as f64)
            .unwrap_or(0.0),
    }
}

fn parse_para_borders(node: Node) -> ParaBorders {
    let mut b = ParaBorders::default();
    for c in children(node) {
        let border = Some(parse_border(c));
        match c.tag_name().name() {
            "top" => b.top = border,
            "bottom" => b.bottom = border,
            "left" => b.left = border,
            "right" => b.right = border,
            "between" => b.between = border,
            _ => {}
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

fn parse_row_props(node: Node) -> RowProps {
    let mut props = RowProps::default();
    for c in children(node) {
        match c.tag_name().name() {
            "trHeight" => {
                if let Some(h) = parse_i64(attr(c, W, "val")).map(twip_to_pt) {
                    props.height = Some(match attr(c, W, "hRule") {
                        Some("exact") => RowHeight::Exact(h),
                        _ => RowHeight::AtLeast(h),
                    });
                }
            }
            "tblHeader" => props.header = toggle_child(c),
            "cantSplit" => props.cant_split = toggle_child(c),
            _ => {}
        }
    }
    props
}

fn parse_cell_props(node: Node) -> CellProps {
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
            "tcBorders" => props.borders = parse_tbl_borders(c),
            "shd" => props.shading = parse_shd_fill(c),
            "tcMar" => props.margins = parse_cell_margins(c),
            "vAlign" => {
                props.v_align = match attr(c, W, "val") {
                    Some("center") => VAlign::Center,
                    Some("bottom") => VAlign::Bottom,
                    _ => VAlign::Top,
                }
            }
            _ => {}
        }
    }
    props
}

fn parse_margins(node: Node) -> Margins {
    let g = |a: &str, d: f64| parse_i64(attr(node, W, a)).map(twip_to_pt).unwrap_or(d);
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
    let space_pt = parse_i64(attr(node, W, "space")).map(twip_to_pt);
    let mut cols = Vec::new();
    for c in children(node) {
        if is(c, W, "col")
            && let Some(w) = parse_i64(attr(c, W, "w"))
        {
            cols.push(twip_to_pt(w));
        }
    }
    ColumnSpec {
        num,
        cols,
        space_pt,
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
            "jc" => props.alignment = attr(c, W, "val").and_then(Jc::parse),
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
            _ => {}
        }
    }
    props
}

/// Parses a `w:rPr` element into [`RunProps`] (public: styles.rs reuses it).
pub fn parse_run_props(rpr: Node) -> RunProps {
    let mut props = RunProps::default();
    for c in children(rpr) {
        match c.tag_name().name() {
            "rStyle" => props.style_id = attr(c, W, "val").map(str::to_string),
            "b" => props.bold = Some(toggle_child(c)),
            "i" => props.italic = Some(toggle_child(c)),
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
            "sz" => props.size_pt = parse_i64(attr(c, W, "val")).map(half_point_to_pt),
            "rFonts" => props.fonts = parse_run_fonts(c),
            "vertAlign" => {
                props.vert_align = match attr(c, W, "val") {
                    Some("superscript") => Some(VertAlign::Super),
                    Some("subscript") => Some(VertAlign::Sub),
                    Some("baseline") => Some(VertAlign::Baseline),
                    _ => None,
                }
            }
            "spacing" => props.spacing_pt = parse_i64(attr(c, W, "val")).map(twip_to_pt),
            // `w:kern` present enables kerning at sizes >= val (half-points → points);
            // a missing val defaults to 0 (kern at any size).
            "kern" => {
                props.kern_min_pt = Some(
                    parse_i64(attr(c, W, "val"))
                        .map(half_point_to_pt)
                        .unwrap_or(0.0),
                )
            }
            "rtl" => props.rtl = Some(toggle_child(c)),
            _ => {}
        }
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

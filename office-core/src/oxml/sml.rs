//! SpreadsheetML model: `workbook.xml`, `sharedStrings.xml`, `styles.xml`, and
//! per-sheet `worksheetN.xml`. Parse-only (no layout).

use super::theme::Theme;
use super::{Rgb, S, attr, child, children, parse_f64, parse_i64};
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

/// Excel's fixed worksheet envelope (XFD is the 16,384th column).
pub const MAX_XLSX_COLUMNS: u32 = 16_384;
/// Excel's fixed worksheet envelope (rows are one-based in authored XML).
pub const MAX_XLSX_ROWS: u32 = 1_048_576;

fn reference_resource_limit(detail: impl Into<String>) -> OfficeError {
    OfficeError::unsupported(UnsupportedCode::ResourceLimit, detail)
}

/// A reference to a sheet in the workbook.
#[derive(Debug, Clone)]
pub struct SheetRef {
    /// Sheet name.
    pub name: String,
    /// The relationship id of the sheet part.
    pub rel_id: String,
    /// Whether the sheet is visible.
    pub visible: bool,
}

/// The workbook.
#[derive(Debug, Clone, Default)]
pub struct Workbook {
    /// Sheets in order.
    pub sheets: Vec<SheetRef>,
    /// Whether the 1904 date system is in effect.
    pub date1904: bool,
    /// Workbook-scoped print metadata. These names are deliberately parsed
    /// rather than treated as arbitrary formulas: Excel stores print areas and
    /// repeating titles in `definedName` records.
    pub defined_names: Vec<DefinedName>,
}

/// A workbook `definedName` relevant to rendering.
#[derive(Debug, Clone)]
pub struct DefinedName {
    /// The defined-name identifier (including `_xlnm.` when present).
    pub name: String,
    /// Sheet index for a local name, if specified.
    pub local_sheet_id: Option<usize>,
    /// The OOXML formula/reference text.
    pub value: String,
}

impl Workbook {
    /// Parses `xl/workbook.xml`.
    pub fn parse(xml: &str) -> Result<Workbook> {
        const MAX_SHEETS: usize = 2_000;
        let doc = super::parse_xml(xml, "workbook.xml")?;
        let root = doc.root_element();
        let mut wb = Workbook::default();
        if let Some(pr) = child(root, S, "workbookPr") {
            wb.date1904 = matches!(attr(pr, S, "date1904"), Some("1") | Some("true"));
        }
        if let Some(sheets) = child(root, S, "sheets") {
            for sh in children(sheets) {
                if super::is(sh, S, "sheet") {
                    if wb.sheets.len() >= MAX_SHEETS {
                        return Err(OfficeError::unsupported(
                            crate::UnsupportedCode::DecompressionLimit,
                            format!("sheet count exceeds limit {MAX_SHEETS}"),
                        ));
                    }
                    let name = attr(sh, S, "name").unwrap_or("Sheet").to_string();
                    let rel_id = attr(sh, super::R, "id").unwrap_or_default().to_string();
                    let visible =
                        !matches!(attr(sh, S, "state"), Some("hidden") | Some("veryHidden"));
                    wb.sheets.push(SheetRef {
                        name,
                        rel_id,
                        visible,
                    });
                }
            }
        }
        if let Some(names) = child(root, S, "definedNames") {
            for node in children(names).filter(|node| super::is(*node, S, "definedName")) {
                let Some(name) = attr(node, S, "name") else {
                    continue;
                };
                let value = node.text().unwrap_or_default().trim();
                if value.is_empty() {
                    continue;
                }
                wb.defined_names.push(DefinedName {
                    name: name.to_string(),
                    local_sheet_id: parse_i64(attr(node, S, "localSheetId"))
                        .and_then(|id| usize::try_from(id).ok()),
                    value: value.to_string(),
                });
            }
        }
        Ok(wb)
    }
}

/// Shared strings (`sharedStrings.xml`).
#[derive(Debug, Clone, Default)]
pub struct SharedStrings(pub Vec<String>);

impl SharedStrings {
    /// Parses `xl/sharedStrings.xml` (plain + rich-text runs concatenated).
    pub fn parse(xml: &str) -> Result<SharedStrings> {
        let doc = super::parse_xml(xml, "sharedStrings.xml")?;
        let mut out = Vec::new();
        for si in children(doc.root_element()) {
            if !super::is(si, S, "si") {
                continue;
            }
            let mut s = String::new();
            for node in si.descendants() {
                if super::is(node, S, "t") {
                    s.push_str(node.text().unwrap_or_default());
                }
            }
            out.push(s);
        }
        Ok(SharedStrings(out))
    }

    /// Looks up a shared string by index.
    pub fn get(&self, i: usize) -> Option<&str> {
        self.0.get(i).map(String::as_str)
    }
}

/// Cell formatting derived from a `cellXfs` entry.
#[derive(Debug, Clone, Default)]
pub struct CellStyle {
    /// Requested font family.
    pub font_name: Option<String>,
    /// Number-format id (builtin < 164, or a custom id).
    pub num_fmt_id: u32,
    /// Font size in points.
    pub font_size: f64,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Font color.
    pub color: Option<Rgb>,
    /// Solid fill color.
    pub fill: Option<Rgb>,
    /// Horizontal alignment (`left`/`center`/`right`), if set.
    pub align: Option<String>,
    /// Whether text wraps.
    pub wrap: bool,
    /// Vertical alignment (`top`/`center`/`bottom`).
    pub vertical_align: Option<String>,
    /// Per-edge cell borders.
    pub borders: CellBorders,
    /// Visible style constructs retained for fail-closed profiling.
    pub unsupported: Vec<String>,
}

/// A visible spreadsheet border edge.
#[derive(Debug, Clone, Default)]
pub struct BorderEdge {
    /// Border color.
    pub color: Option<Rgb>,
    /// Approximate border width in points.
    pub width_pt: f64,
    /// Dash pattern in points, when applicable.
    pub dash: Option<Vec<f64>>,
    /// Visible border style that cannot be represented exactly.
    pub unsupported: Option<String>,
}

/// Four cell border edges.
#[derive(Debug, Clone, Default)]
pub struct CellBorders {
    /// Top edge.
    pub top: Option<BorderEdge>,
    /// Right edge.
    pub right: Option<BorderEdge>,
    /// Bottom edge.
    pub bottom: Option<BorderEdge>,
    /// Left edge.
    pub left: Option<BorderEdge>,
}

/// Parsed `styles.xml`: custom number formats + per-cellXf styles.
#[derive(Debug, Clone, Default)]
pub struct Styles {
    /// Custom number-format patterns by id (≥ 164).
    pub custom_formats: HashMap<u32, String>,
    /// Per-`cellXfs`-index style.
    pub cell_styles: Vec<CellStyle>,
    /// Differential styles used by conditional-formatting rules.
    pub dxf_styles: Vec<CellStyle>,
    /// Workbook indexed-color palette used by cell and conditional styles.
    pub indexed_colors: Vec<Rgb>,
}

impl Styles {
    /// Parses `xl/styles.xml`.
    pub fn parse(xml: &str) -> Result<Styles> {
        Self::parse_with_theme(xml, &Theme::default())
    }

    /// Parses styles while resolving theme and indexed color references.
    pub fn parse_with_theme(xml: &str, theme: &Theme) -> Result<Styles> {
        let doc = super::parse_xml(xml, "styles.xml")?;
        let root = doc.root_element();
        let mut styles = Styles {
            indexed_colors: parse_indexed_colors(root),
            ..Styles::default()
        };

        if let Some(numfmts) = child(root, S, "numFmts") {
            for nf in children(numfmts) {
                if super::is(nf, S, "numFmt")
                    && let (Some(id), Some(code)) = (
                        parse_i64(attr(nf, S, "numFmtId")),
                        attr(nf, S, "formatCode"),
                    )
                {
                    styles
                        .custom_formats
                        .insert(id.max(0) as u32, code.to_string());
                }
            }
        }

        let fonts = parse_fonts(root, theme, &styles.indexed_colors);
        let fills = parse_fills(root, theme, &styles.indexed_colors);
        let borders = parse_borders(root, theme, &styles.indexed_colors);

        if let Some(cell_xfs) = child(root, S, "cellXfs") {
            for xf in children(cell_xfs) {
                if !super::is(xf, S, "xf") {
                    continue;
                }
                let font_idx = parse_i64(attr(xf, S, "fontId")).unwrap_or(0).max(0) as usize;
                let fill_idx = parse_i64(attr(xf, S, "fillId")).unwrap_or(0).max(0) as usize;
                let border_idx = parse_i64(attr(xf, S, "borderId")).unwrap_or(0).max(0) as usize;
                let (font_name, font_size, bold, italic, color) = fonts
                    .get(font_idx)
                    .cloned()
                    .unwrap_or((None, 11.0, false, false, None));
                let (mut align, mut wrap, mut vertical_align) = (None, false, None);
                let mut alignment_unsupported = Vec::new();
                if let Some(a) = child(xf, S, "alignment") {
                    align = attr(a, S, "horizontal").map(str::to_string);
                    wrap = matches!(attr(a, S, "wrapText"), Some("1") | Some("true"));
                    vertical_align = attr(a, S, "vertical").map(str::to_string);
                    if parse_i64(attr(a, S, "textRotation")).is_some_and(|value| value != 0) {
                        alignment_unsupported
                            .push("unsupported spreadsheet text rotation".to_string());
                    }
                    if parse_i64(attr(a, S, "indent")).is_some_and(|value| value != 0) {
                        alignment_unsupported
                            .push("unsupported spreadsheet text indentation".to_string());
                    }
                }
                let (fill, fill_unsupported) = fills.get(fill_idx).cloned().unwrap_or((None, None));
                let mut unsupported: Vec<String> = fill_unsupported.into_iter().collect();
                unsupported.extend(alignment_unsupported);
                if align
                    .as_deref()
                    .is_some_and(|value| !matches!(value, "left" | "center" | "right" | "general"))
                {
                    unsupported.push(format!(
                        "unsupported spreadsheet horizontal alignment {}",
                        align.as_deref().unwrap_or_default()
                    ));
                }
                if vertical_align
                    .as_deref()
                    .is_some_and(|value| !matches!(value, "top" | "center" | "bottom"))
                {
                    unsupported.push(format!(
                        "unsupported spreadsheet vertical alignment {}",
                        vertical_align.as_deref().unwrap_or_default()
                    ));
                }
                styles.cell_styles.push(CellStyle {
                    font_name,
                    num_fmt_id: parse_i64(attr(xf, S, "numFmtId")).unwrap_or(0).max(0) as u32,
                    font_size,
                    bold,
                    italic,
                    color,
                    fill,
                    align,
                    wrap,
                    vertical_align,
                    borders: borders.get(border_idx).cloned().unwrap_or_default(),
                    unsupported,
                });
            }
        }
        if let Some(dxfs) = child(root, S, "dxfs") {
            for dxf in children(dxfs).filter(|node| super::is(*node, S, "dxf")) {
                let font = child(dxf, S, "font");
                let (fill, fill_unsupported) = child(dxf, S, "fill")
                    .map(|fill| parse_fill(fill, theme, &styles.indexed_colors))
                    .unwrap_or((None, None));
                let color = font
                    .and_then(|font| child(font, S, "color"))
                    .and_then(|color| resolve_sml_color(color, theme, &styles.indexed_colors));
                let borders = child(dxf, S, "border")
                    .map(|border| parse_border(border, theme, &styles.indexed_colors))
                    .unwrap_or_default();
                styles.dxf_styles.push(CellStyle {
                    bold: font.and_then(|font| child(font, S, "b")).is_some(),
                    italic: font.and_then(|font| child(font, S, "i")).is_some(),
                    color,
                    fill,
                    borders,
                    unsupported: fill_unsupported.into_iter().collect(),
                    ..CellStyle::default()
                });
            }
        }
        Ok(styles)
    }

    /// The style for a cell's `s` index.
    pub fn cell(&self, idx: usize) -> CellStyle {
        self.cell_styles.get(idx).cloned().unwrap_or_default()
    }

    /// The format code for a number-format id.
    pub fn format_code(&self, id: u32) -> Option<&str> {
        self.custom_formats.get(&id).map(String::as_str)
    }

    /// Differential style for a conditional-formatting `dxfId`.
    pub fn dxf(&self, idx: usize) -> Option<CellStyle> {
        self.dxf_styles.get(idx).cloned()
    }
}

type ParsedFont = (Option<String>, f64, bool, bool, Option<Rgb>);

fn parse_fonts(root: super::Node, theme: &Theme, indexed: &[Rgb]) -> Vec<ParsedFont> {
    let mut out = Vec::new();
    if let Some(fonts) = child(root, S, "fonts") {
        for f in children(fonts) {
            if !super::is(f, S, "font") {
                continue;
            }
            let size = child(f, S, "sz")
                .and_then(|n| parse_f64(attr(n, S, "val")))
                .unwrap_or(11.0);
            let bold = child(f, S, "b").is_some();
            let italic = child(f, S, "i").is_some();
            let color =
                child(f, S, "color").and_then(|color| resolve_sml_color(color, theme, indexed));
            let name = child(f, S, "name")
                .and_then(|name| attr(name, S, "val"))
                .map(str::to_string);
            out.push((name, size, bold, italic, color));
        }
    }
    out
}

fn parse_borders(root: super::Node, theme: &Theme, indexed: &[Rgb]) -> Vec<CellBorders> {
    let mut out = Vec::new();
    if let Some(borders) = child(root, S, "borders") {
        for border in children(borders).filter(|node| super::is(*node, S, "border")) {
            out.push(parse_border(border, theme, indexed));
        }
    }
    out
}

fn parse_border(node: super::Node, theme: &Theme, indexed: &[Rgb]) -> CellBorders {
    CellBorders {
        top: child(node, S, "top").and_then(|edge| parse_border_edge(edge, theme, indexed)),
        right: child(node, S, "right").and_then(|edge| parse_border_edge(edge, theme, indexed)),
        bottom: child(node, S, "bottom").and_then(|edge| parse_border_edge(edge, theme, indexed)),
        left: child(node, S, "left").and_then(|edge| parse_border_edge(edge, theme, indexed)),
    }
}

fn parse_border_edge(node: super::Node, theme: &Theme, indexed: &[Rgb]) -> Option<BorderEdge> {
    let style = attr(node, S, "style")?;
    let width_pt = match style {
        "hair" => 0.25,
        "thin" | "dotted" | "dashed" => 0.75,
        "medium" | "mediumDashed" | "mediumDashDot" | "mediumDashDotDot" => 1.5,
        "thick" | "double" => 2.25,
        _ => 0.75,
    };
    let dash = match style {
        "dotted" => Some(vec![1.0, 1.0]),
        "dashed" | "mediumDashed" => Some(vec![3.0, 2.0]),
        "dashDot" | "mediumDashDot" => Some(vec![3.0, 1.5, 1.0, 1.5]),
        "dashDotDot" | "mediumDashDotDot" => Some(vec![3.0, 1.5, 1.0, 1.5, 1.0, 1.5]),
        _ => None,
    };
    let unsupported = (style == "double").then(|| "double cell border".to_string());
    let color = child(node, S, "color").and_then(|color| resolve_sml_color(color, theme, indexed));
    Some(BorderEdge {
        color,
        width_pt,
        dash,
        unsupported,
    })
}

fn parse_fills(
    root: super::Node,
    theme: &Theme,
    indexed: &[Rgb],
) -> Vec<(Option<Rgb>, Option<String>)> {
    let mut out = Vec::new();
    if let Some(fills) = child(root, S, "fills") {
        for f in children(fills) {
            out.push(parse_fill(f, theme, indexed));
        }
    }
    out
}

fn parse_fill(fill: super::Node, theme: &Theme, indexed: &[Rgb]) -> (Option<Rgb>, Option<String>) {
    if let Some(pattern) = child(fill, S, "patternFill") {
        return match attr(pattern, S, "patternType").unwrap_or("none") {
            "solid" => (
                child(pattern, S, "fgColor")
                    .and_then(|color| resolve_sml_color(color, theme, indexed)),
                None,
            ),
            "none" | "gray125" => (None, None),
            other => (
                None,
                Some(format!("unsupported spreadsheet fill pattern {other}")),
            ),
        };
    }
    if child(fill, S, "gradientFill").is_some() {
        return (None, Some("unsupported spreadsheet gradient fill".into()));
    }
    (None, None)
}

/// Parses an 8-hex `AARRGGBB` (or 6-hex `RRGGBB`) color.
fn parse_argb(s: &str) -> Option<Rgb> {
    let hex = if s.len() == 8 { &s[2..] } else { s };
    Rgb::from_hex(hex)
}

fn resolve_sml_color(node: super::Node, theme: &Theme, indexed: &[Rgb]) -> Option<Rgb> {
    let base = attr(node, S, "rgb")
        .and_then(parse_argb)
        .or_else(|| {
            parse_i64(attr(node, S, "theme")).and_then(|index| {
                let slot = match index {
                    0 => "lt1",
                    1 => "dk1",
                    2 => "lt2",
                    3 => "dk2",
                    4 => "accent1",
                    5 => "accent2",
                    6 => "accent3",
                    7 => "accent4",
                    8 => "accent5",
                    9 => "accent6",
                    10 => "hlink",
                    11 => "folHlink",
                    _ => return None,
                };
                theme.color(slot)
            })
        })
        .or_else(|| {
            parse_i64(attr(node, S, "indexed"))
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| {
                    indexed
                        .get(index)
                        .copied()
                        .or_else(|| default_indexed(index))
                })
        })?;
    let tint = parse_f64(attr(node, S, "tint"))
        .unwrap_or(0.0)
        .clamp(-1.0, 1.0);
    let transform = |channel: u8| {
        let channel = f64::from(channel);
        let value = if tint < 0.0 {
            channel * (1.0 + tint)
        } else {
            channel + (255.0 - channel) * tint
        };
        value.round().clamp(0.0, 255.0) as u8
    };
    Some(Rgb::new(
        transform(base.r),
        transform(base.g),
        transform(base.b),
    ))
}

fn parse_indexed_colors(root: super::Node) -> Vec<Rgb> {
    child(root, S, "colors")
        .and_then(|colors| child(colors, S, "indexedColors"))
        .map(|colors| {
            children(colors)
                .filter(|color| super::is(*color, S, "rgbColor"))
                .filter_map(|color| attr(color, S, "rgb").and_then(parse_argb))
                .collect()
        })
        .unwrap_or_default()
}

fn default_indexed(index: usize) -> Option<Rgb> {
    Some(match index {
        0 => Rgb::new(0, 0, 0),
        1 => Rgb::new(255, 255, 255),
        2 => Rgb::new(255, 0, 0),
        3 => Rgb::new(0, 255, 0),
        4 => Rgb::new(0, 0, 255),
        5 => Rgb::new(255, 255, 0),
        6 => Rgb::new(255, 0, 255),
        7 => Rgb::new(0, 255, 255),
        _ => return None,
    })
}

/// A cell's typed value.
#[derive(Debug, Clone)]
pub enum CellValue {
    /// A number (already parsed).
    Number(f64),
    /// A string.
    Text(String),
    /// A boolean.
    Bool(bool),
    /// An error literal (e.g. `#DIV/0!`).
    Error(String),
    /// A formula whose cached value was missing (out of envelope).
    UncachedFormula,
    /// An empty cell.
    Empty,
}

/// A worksheet cell.
#[derive(Debug, Clone)]
pub struct Cell {
    /// Zero-based column.
    pub col: u32,
    /// Zero-based row.
    pub row: u32,
    /// Value.
    pub value: CellValue,
    /// The `cellXfs` style index.
    pub style_idx: usize,
}

/// A column width/visibility span (`<cols><col>`).
#[derive(Debug, Clone)]
pub struct ColInfo {
    /// First column (0-based).
    pub min: u32,
    /// Last column (0-based).
    pub max: u32,
    /// Width in "characters" (Excel's stored unit).
    pub width_chars: f64,
    /// Hidden.
    pub hidden: bool,
}

/// A parsed worksheet.
#[derive(Debug, Clone, Default)]
pub struct Worksheet {
    /// Default row height (points).
    pub default_row_height: f64,
    /// Column spans.
    pub cols: Vec<ColInfo>,
    /// Per-row explicit heights (0-based row → points).
    pub row_heights: HashMap<u32, f64>,
    /// Hidden rows.
    pub hidden_rows: std::collections::HashSet<u32>,
    /// Cells, in row-major order.
    pub cells: Vec<Cell>,
    /// Used range `(max_row, max_col)` inclusive.
    pub dim: (u32, u32),
    /// Relationship id of a DrawingML part, when present.
    pub drawing_rel_id: Option<String>,
    /// Merged cell ranges.
    pub merges: Vec<MergeRange>,
    /// Conditional-formatting rules evaluated against cached values.
    pub conditional_formats: Vec<ConditionalFormat>,
    /// Page setup controls used to derive print geometry.
    pub page_setup: PageSetup,
    /// Page margins in points.
    pub page_margins: PageMargins,
    /// Print gridline setting. Absent means Excel's print default: off.
    pub print_grid_lines: bool,
    /// Row indices before which Excel inserts a manual page break.
    pub row_breaks: Vec<u32>,
    /// Column indices before which Excel inserts a manual page break.
    pub col_breaks: Vec<u32>,
    /// Header/footer text bands.
    pub header_footer: HeaderFooter,
    /// Optional workbook-defined print range.
    pub print_area: Option<CellRange>,
    /// Rows to repeat on every printed page.
    pub print_title_rows: Option<(u32, u32)>,
    /// Columns to repeat on every printed page.
    pub print_title_cols: Option<(u32, u32)>,
    /// Visible worksheet constructs retained as typed profile reasons.
    pub visible_unsupported: Vec<WorksheetUnsupported>,
}

/// A visible worksheet construct that cannot be rendered safely.
#[derive(Debug, Clone)]
pub struct WorksheetUnsupported {
    /// Stable fail-closed reason code.
    pub code: UnsupportedCode,
    /// Human-readable element detail.
    pub detail: String,
}

/// Inclusive rectangular cell range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRange {
    /// First row, zero-based and inclusive.
    pub first_row: u32,
    /// First column, zero-based and inclusive.
    pub first_col: u32,
    /// Last row, zero-based and inclusive.
    pub last_row: u32,
    /// Last column, zero-based and inclusive.
    pub last_col: u32,
}

/// Spreadsheet page setup; paper IDs follow ECMA-376/Excel values.
#[derive(Debug, Clone, Copy)]
pub struct PageSetup {
    /// ECMA-376 paper-size identifier.
    pub paper_size: u32,
    /// Whether width and height are swapped for landscape output.
    pub landscape: bool,
    /// Explicit print scale as a ratio, when present.
    pub scale: Option<f64>,
    /// Requested horizontal page count for fit-to-page output.
    pub fit_to_width: Option<u32>,
    /// Requested vertical page count for fit-to-page output.
    pub fit_to_height: Option<u32>,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            paper_size: 9,
            landscape: false,
            scale: None,
            fit_to_width: None,
            fit_to_height: None,
        }
    }
}

/// Print margins in points, including the header/footer offsets.
#[derive(Debug, Clone, Copy)]
pub struct PageMargins {
    /// Left page margin in points.
    pub left: f64,
    /// Right page margin in points.
    pub right: f64,
    /// Top page margin in points.
    pub top: f64,
    /// Bottom page margin in points.
    pub bottom: f64,
    /// Header baseline offset from the top edge, in points.
    pub header: f64,
    /// Footer baseline offset from the bottom edge, in points.
    pub footer: f64,
}

impl Default for PageMargins {
    fn default() -> Self {
        Self {
            left: 54.0,
            right: 54.0,
            top: 72.0,
            bottom: 72.0,
            header: 36.0,
            footer: 36.0,
        }
    }
}

/// Excel header/footer strings. They are rendered deterministically by the
/// renderer (fixed date/time unless core metadata supplies a date).
#[derive(Debug, Clone, Default)]
pub struct HeaderFooter {
    /// Whether even pages use the even header/footer strings.
    pub different_odd_even: bool,
    /// Whether the first page uses the first header/footer strings.
    pub different_first: bool,
    /// Header used on ordinary odd-numbered pages.
    pub odd_header: Option<String>,
    /// Footer used on ordinary odd-numbered pages.
    pub odd_footer: Option<String>,
    /// Header used on even-numbered pages when supplied.
    pub even_header: Option<String>,
    /// Footer used on even-numbered pages when supplied.
    pub even_footer: Option<String>,
    /// Header used on the first page when supplied.
    pub first_header: Option<String>,
    /// Footer used on the first page when supplied.
    pub first_footer: Option<String>,
}

/// Parsed conditional-formatting block and the cells it addresses.
#[derive(Debug, Clone, Default)]
pub struct ConditionalFormat {
    /// Cell ranges addressed by the block.
    pub ranges: Vec<CellRange>,
    /// Rules in workbook priority order.
    pub rules: Vec<ConditionalRule>,
}

/// A supported subset of Excel conditional-formatting rules.
#[derive(Debug, Clone)]
pub struct ConditionalRule {
    /// Parsed rule behavior.
    pub kind: ConditionalKind,
    /// Differential-style index, when the rule applies one.
    pub dxf_id: Option<usize>,
    /// Excel rule priority; smaller values run first.
    pub priority: u32,
}

/// Conditional-formatting rules understood by the XLSX renderer.
#[derive(Debug, Clone)]
pub enum ConditionalKind {
    /// Compares the cached cell value with one or two formula operands.
    CellIs {
        /// OOXML comparison operator.
        operator: String,
        /// Cached numeric literals or simple cell-reference expressions.
        formulas: Vec<String>,
    },
    /// Evaluates a small comparison/arithmetic expression subset.
    Expression {
        /// Rule formula without evaluation state.
        formula: String,
    },
    /// Interpolates between two or three colors across cached values.
    ColorScale {
        /// Thresholds corresponding to the ordered colors.
        values: Vec<ConditionalValue>,
        /// Ordered low/mid/high colors.
        colors: Vec<Rgb>,
    },
    /// Draws a value-proportional bar inside the cell.
    DataBar {
        /// Minimum bar threshold.
        minimum: ConditionalValue,
        /// Maximum bar threshold.
        maximum: ConditionalValue,
        /// Bar color.
        color: Rgb,
    },
    /// An icon-set rule; retained as typed unsupported until icon assets exist.
    IconSet,
    /// A recognized conditional-format construct outside the supported subset.
    Unsupported(String),
}

/// A cached-value threshold used by a color scale or data bar.
#[derive(Debug, Clone, Copy)]
pub enum ConditionalValue {
    /// Minimum numeric value in the formatted range.
    Min,
    /// Maximum numeric value in the formatted range.
    Max,
    /// Explicit numeric threshold.
    Number(f64),
    /// Percentage between the range minimum and maximum.
    Percent(f64),
    /// Percentile of sorted cached values.
    Percentile(f64),
}

/// Inclusive rectangular merged-cell range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeRange {
    /// Top row.
    pub first_row: u32,
    /// Left column.
    pub first_col: u32,
    /// Bottom row.
    pub last_row: u32,
    /// Right column.
    pub last_col: u32,
}

impl Worksheet {
    /// Parses a `xl/worksheets/sheetN.xml`, resolving cell values against
    /// `shared`.
    pub fn parse(xml: &str, shared: &SharedStrings) -> Result<Worksheet> {
        Self::parse_with_colors(xml, shared, &Theme::default(), &[])
    }

    /// Parses a worksheet while resolving conditional-format theme/indexed colors.
    pub fn parse_with_colors(
        xml: &str,
        shared: &SharedStrings,
        theme: &Theme,
        indexed: &[Rgb],
    ) -> Result<Worksheet> {
        let doc = super::parse_xml(xml, "worksheet")?;
        let root = doc.root_element();
        let mut ws = Worksheet {
            default_row_height: 15.0,
            ..Default::default()
        };

        if let Some(fmt) = child(root, S, "sheetFormatPr")
            && let Some(h) = parse_f64(attr(fmt, S, "defaultRowHeight"))
        {
            ws.default_row_height = h;
        }
        if let Some(cols) = child(root, S, "cols") {
            for c in children(cols) {
                if !super::is(c, S, "col") {
                    continue;
                }
                let min = parse_required_one_based_index(
                    attr(c, S, "min"),
                    MAX_XLSX_COLUMNS,
                    "worksheet column min",
                )?;
                let max = parse_required_one_based_index(
                    attr(c, S, "max"),
                    MAX_XLSX_COLUMNS,
                    "worksheet column max",
                )?;
                if min > max {
                    return Err(OfficeError::Invalid(format!(
                        "worksheet column range {}..{} is reversed",
                        min + 1,
                        max + 1
                    )));
                }
                let width_chars = parse_f64(attr(c, S, "width")).unwrap_or(8.43);
                let hidden = matches!(attr(c, S, "hidden"), Some("1") | Some("true"));
                ws.cols.push(ColInfo {
                    min,
                    max,
                    width_chars,
                    hidden,
                });
            }
        }
        ws.drawing_rel_id = child(root, S, "drawing")
            .and_then(|drawing| attr(drawing, super::R, "id"))
            .map(str::to_string);
        ws.print_grid_lines = child(root, S, "printOptions").is_some_and(|options| {
            matches!(attr(options, S, "gridLines"), Some("1") | Some("true"))
        });
        if let Some(margins) = child(root, S, "pageMargins") {
            let inch = |name| parse_f64(attr(margins, S, name)).map(|value| value * 72.0);
            ws.page_margins.left = inch("left").unwrap_or(ws.page_margins.left);
            ws.page_margins.right = inch("right").unwrap_or(ws.page_margins.right);
            ws.page_margins.top = inch("top").unwrap_or(ws.page_margins.top);
            ws.page_margins.bottom = inch("bottom").unwrap_or(ws.page_margins.bottom);
            ws.page_margins.header = inch("header").unwrap_or(ws.page_margins.header);
            ws.page_margins.footer = inch("footer").unwrap_or(ws.page_margins.footer);
        }
        if let Some(setup) = child(root, S, "pageSetup") {
            ws.page_setup.paper_size =
                parse_i64(attr(setup, S, "paperSize")).unwrap_or(9).max(0) as u32;
            ws.page_setup.landscape = attr(setup, S, "orientation") == Some("landscape");
            ws.page_setup.scale = parse_f64(attr(setup, S, "scale"))
                .filter(|scale| *scale > 0.0)
                .map(|scale| scale / 100.0);
            ws.page_setup.fit_to_width =
                parse_i64(attr(setup, S, "fitToWidth")).and_then(|value| u32::try_from(value).ok());
            ws.page_setup.fit_to_height = parse_i64(attr(setup, S, "fitToHeight"))
                .and_then(|value| u32::try_from(value).ok());
        }
        if let Some(header_footer) = child(root, S, "headerFooter") {
            ws.header_footer = HeaderFooter {
                different_odd_even: matches!(
                    attr(header_footer, S, "differentOddEven"),
                    Some("1") | Some("true")
                ),
                different_first: matches!(
                    attr(header_footer, S, "differentFirst"),
                    Some("1") | Some("true")
                ),
                odd_header: child(header_footer, S, "oddHeader")
                    .and_then(|node| node.text())
                    .map(str::to_string),
                odd_footer: child(header_footer, S, "oddFooter")
                    .and_then(|node| node.text())
                    .map(str::to_string),
                even_header: child(header_footer, S, "evenHeader")
                    .and_then(|node| node.text())
                    .map(str::to_string),
                even_footer: child(header_footer, S, "evenFooter")
                    .and_then(|node| node.text())
                    .map(str::to_string),
                first_header: child(header_footer, S, "firstHeader")
                    .and_then(|node| node.text())
                    .map(str::to_string),
                first_footer: child(header_footer, S, "firstFooter")
                    .and_then(|node| node.text())
                    .map(str::to_string),
            };
        }
        ws.row_breaks = parse_breaks(child(root, S, "rowBreaks"));
        ws.col_breaks = parse_breaks(child(root, S, "colBreaks"));
        for node in children(root) {
            let (code, detail) = match node.tag_name().name() {
                "oleObjects" | "controls" | "legacyDrawing" => {
                    (UnsupportedCode::OleObject, node.tag_name().name())
                }
                "hyperlinks" => (UnsupportedCode::Hyperlink, "worksheet hyperlinks"),
                _ => continue,
            };
            ws.visible_unsupported.push(WorksheetUnsupported {
                code,
                detail: detail.to_string(),
            });
        }
        if child(root, S, "sheetViews").is_some_and(|views| {
            children(views).any(|view| {
                super::is(view, S, "sheetView")
                    && matches!(attr(view, S, "rightToLeft"), Some("1") | Some("true"))
            })
        }) {
            ws.visible_unsupported.push(WorksheetUnsupported {
                code: UnsupportedCode::RtlText,
                detail: "right-to-left worksheet view".into(),
            });
        }
        for cf in children(root).filter(|node| super::is(*node, S, "conditionalFormatting")) {
            ws.conditional_formats
                .push(parse_conditional_format(cf, theme, indexed));
        }

        let (mut max_row, mut max_col) = (0u32, 0u32);
        if let Some(data) = child(root, S, "sheetData") {
            for row in children(data) {
                if !super::is(row, S, "row") {
                    continue;
                }
                let r_idx = match attr(row, S, "r") {
                    Some(value) => {
                        parse_required_one_based_index(Some(value), MAX_XLSX_ROWS, "worksheet row")?
                    }
                    None => 0,
                };
                if let Some(h) = parse_f64(attr(row, S, "ht")) {
                    ws.row_heights.insert(r_idx, h);
                }
                if matches!(attr(row, S, "hidden"), Some("1") | Some("true")) {
                    ws.hidden_rows.insert(r_idx);
                }
                for c in children(row) {
                    if !super::is(c, S, "c") {
                        continue;
                    }
                    let reference = attr(c, S, "r").ok_or_else(|| {
                        OfficeError::Invalid(
                            "worksheet cell is missing required r reference".into(),
                        )
                    })?;
                    let (col, row_n) = parse_required_a1(reference, "worksheet cell reference")?;
                    let style_idx = parse_i64(attr(c, S, "s")).unwrap_or(0).max(0) as usize;
                    let value = parse_cell_value(c, attr(c, S, "t"), shared);
                    max_row = max_row.max(row_n);
                    max_col = max_col.max(col);
                    ws.cells.push(Cell {
                        col,
                        row: row_n,
                        value,
                        style_idx,
                    });
                }
            }
        }
        if let Some(merges) = child(root, S, "mergeCells") {
            for merge in children(merges).filter(|node| super::is(*node, S, "mergeCell")) {
                let reference = attr(merge, S, "ref").ok_or_else(|| {
                    OfficeError::Invalid("mergeCell is missing required ref".into())
                })?;
                let range = parse_required_range(reference, "merged-cell reference")?;
                let (first_col, first_row, last_col, last_row) = (
                    range.first_col,
                    range.first_row,
                    range.last_col,
                    range.last_row,
                );
                ws.merges.push(MergeRange {
                    first_row: first_row.min(last_row),
                    first_col: first_col.min(last_col),
                    last_row: first_row.max(last_row),
                    last_col: first_col.max(last_col),
                });
                max_row = max_row.max(last_row);
                max_col = max_col.max(last_col);
            }
        }
        ws.dim = (max_row, max_col);
        Ok(ws)
    }
}

fn parse_breaks(node: Option<super::Node>) -> Vec<u32> {
    node.into_iter()
        .flat_map(children)
        .filter(|node| super::is(*node, S, "brk"))
        .filter_map(|node| parse_i64(attr(node, S, "id")).and_then(|id| u32::try_from(id).ok()))
        .collect()
}

fn parse_conditional_format(
    node: super::Node,
    theme: &Theme,
    indexed: &[Rgb],
) -> ConditionalFormat {
    let ranges = attr(node, S, "sqref").map(parse_ranges).unwrap_or_default();
    let rules = children(node)
        .filter(|rule| super::is(*rule, S, "cfRule"))
        .map(|rule| {
            let formulas = children(rule)
                .filter(|node| super::is(*node, S, "formula"))
                .filter_map(|node| node.text())
                .map(str::to_string)
                .collect::<Vec<_>>();
            let kind = match attr(rule, S, "type") {
                Some("cellIs") => ConditionalKind::CellIs {
                    operator: attr(rule, S, "operator").unwrap_or("equal").to_string(),
                    formulas,
                },
                Some("expression") => formulas
                    .first()
                    .filter(|formula| supports_conditional_expression(formula))
                    .cloned()
                    .map(|formula| ConditionalKind::Expression { formula })
                    .unwrap_or_else(|| {
                        ConditionalKind::Unsupported(
                            "expression outside arithmetic/comparison/cell-reference subset".into(),
                        )
                    }),
                Some("colorScale") => {
                    let values = parse_cf_values(rule);
                    let colors: Vec<_> = rule
                        .descendants()
                        .filter(|node| super::is(*node, S, "color"))
                        .filter_map(|node| resolve_sml_color(node, theme, indexed))
                        .collect();
                    if matches!(values.len(), 2 | 3) && values.len() == colors.len() {
                        ConditionalKind::ColorScale { values, colors }
                    } else {
                        ConditionalKind::Unsupported(
                            "colorScale requires two or three resolvable cfvo/color pairs".into(),
                        )
                    }
                }
                Some("dataBar") => {
                    let values = parse_cf_values(rule);
                    let color = rule
                        .descendants()
                        .find(|node| super::is(*node, S, "color"))
                        .and_then(|node| resolve_sml_color(node, theme, indexed))
                        .unwrap_or(Rgb::new(99, 190, 123));
                    if let [minimum, maximum] = values.as_slice() {
                        ConditionalKind::DataBar {
                            minimum: *minimum,
                            maximum: *maximum,
                            color,
                        }
                    } else {
                        ConditionalKind::Unsupported(
                            "dataBar requires two resolvable cfvo thresholds".into(),
                        )
                    }
                }
                Some("iconSet") => ConditionalKind::IconSet,
                other => ConditionalKind::Unsupported(other.unwrap_or("missing type").to_string()),
            };
            ConditionalRule {
                kind,
                dxf_id: parse_i64(attr(rule, S, "dxfId")).and_then(|id| usize::try_from(id).ok()),
                priority: parse_i64(attr(rule, S, "priority")).unwrap_or(0).max(0) as u32,
            }
        })
        .collect();
    ConditionalFormat { ranges, rules }
}

fn parse_cf_values(rule: super::Node) -> Vec<ConditionalValue> {
    rule.descendants()
        .filter(|node| super::is(*node, S, "cfvo"))
        .filter_map(|node| {
            let value = parse_f64(attr(node, S, "val"));
            match attr(node, S, "type") {
                Some("min") => Some(ConditionalValue::Min),
                Some("max") => Some(ConditionalValue::Max),
                Some("num") => value.map(ConditionalValue::Number),
                Some("percent") => value.map(ConditionalValue::Percent),
                Some("percentile") => value.map(ConditionalValue::Percentile),
                _ => None,
            }
        })
        .collect()
}

fn supports_conditional_expression(formula: &str) -> bool {
    let formula = formula.trim_start_matches('=');
    let mut depth = 0i32;
    let mut cursor = 0usize;
    let bytes = formula.as_bytes();
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if byte == b'(' {
            depth += 1;
            cursor += 1;
            continue;
        }
        if byte == b')' {
            depth -= 1;
            if depth < 0 {
                return false;
            }
            cursor += 1;
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'$' {
            let start = cursor;
            cursor += 1;
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'$')
            {
                cursor += 1;
            }
            if parse_a1(&formula[start..cursor]).is_none() {
                return false;
            }
            continue;
        }
        if !(byte.is_ascii_digit()
            || byte.is_ascii_whitespace()
            || matches!(byte, b'.' | b'+' | b'-' | b'*' | b'/' | b'<' | b'>' | b'='))
        {
            return false;
        }
        cursor += 1;
    }
    depth == 0
        && [">=", "<=", "<>", "=", ">", "<"]
            .iter()
            .any(|operator| formula.contains(operator))
}

fn parse_ranges(value: &str) -> Vec<CellRange> {
    value.split_whitespace().filter_map(parse_range).collect()
}

/// Parses a single A1 rectangular range. Sheet prefixes are discarded because
/// Workbook-level names are matched to their local sheet before this call.
pub fn parse_range(value: &str) -> Option<CellRange> {
    parse_required_range(value, "cell range").ok()
}

pub(crate) fn parse_required_range(value: &str, label: &str) -> Result<CellRange> {
    let value = value.rsplit('!').next().ok_or_else(|| {
        OfficeError::Invalid(format!("{label} has no A1 range after sheet prefix"))
    })?;
    let mut endpoints = value.split(':');
    let first = endpoints
        .next()
        .ok_or_else(|| OfficeError::Invalid(format!("{label} is empty")))?;
    let (first_col, first_row) = parse_required_a1(first, label)?;
    let (last_col, last_row) = match endpoints.next() {
        Some(last) => parse_required_a1(last, label)?,
        None => (first_col, first_row),
    };
    if endpoints.next().is_some() {
        return Err(OfficeError::Invalid(format!(
            "{label} contains more than two endpoints"
        )));
    }
    Ok(CellRange {
        first_row: first_row.min(last_row),
        first_col: first_col.min(last_col),
        last_row: first_row.max(last_row),
        last_col: first_col.max(last_col),
    })
}

fn parse_cell_value(cell: super::Node, ty: Option<&str>, shared: &SharedStrings) -> CellValue {
    let has_formula = child(cell, S, "f").is_some();
    let v = child(cell, S, "v").map(|node| node.text().unwrap_or_default().to_string());
    let inline = child(cell, S, "is").map(|is_node| {
        let mut s = String::new();
        for d in is_node.descendants() {
            if super::is(d, S, "t") {
                s.push_str(d.text().unwrap_or_default());
            }
        }
        s
    });

    if has_formula && v.is_none() && inline.is_none() {
        return CellValue::UncachedFormula;
    }

    match ty {
        Some("s") => match v.as_deref() {
            Some(value) if value.trim().is_empty() => CellValue::Empty,
            Some(value) => value
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|index| shared.get(index))
                .map(|value| CellValue::Text(value.to_string()))
                .unwrap_or_else(|| missing_formula_cache_or_empty(has_formula)),
            None => CellValue::Empty,
        },
        Some("str") => v.map(CellValue::Text).unwrap_or(CellValue::Empty),
        Some("inlineStr") => inline
            .or(v)
            .map(CellValue::Text)
            .unwrap_or(CellValue::Empty),
        Some("b") => match v.as_deref().map(str::trim) {
            Some("") => CellValue::Empty,
            Some("1") => CellValue::Bool(true),
            Some("0") => CellValue::Bool(false),
            Some(_) => missing_formula_cache_or_empty(has_formula),
            None => CellValue::Empty,
        },
        Some("e") => match v {
            Some(value) if value.is_empty() => CellValue::Empty,
            Some(value) => CellValue::Error(value),
            None => CellValue::Empty,
        },
        _ => match v.as_deref() {
            Some(value) if value.trim().is_empty() => CellValue::Empty,
            Some(value) => value
                .trim()
                .parse::<f64>()
                .map(CellValue::Number)
                .unwrap_or_else(|_| missing_formula_cache_or_empty(has_formula)),
            None => CellValue::Empty,
        },
    }
}

fn missing_formula_cache_or_empty(has_formula: bool) -> CellValue {
    if has_formula {
        CellValue::UncachedFormula
    } else {
        CellValue::Empty
    }
}

/// Parses an A1 reference (e.g. `AB12`, `$C$5`) into zero-based `(col, row)`.
pub fn parse_a1(s: &str) -> Option<(u32, u32)> {
    parse_required_a1(s, "A1 reference").ok()
}

pub(crate) fn parse_required_a1(s: &str, label: &str) -> Result<(u32, u32)> {
    let value = s.trim();
    let bytes = value.as_bytes();
    let mut cursor = usize::from(bytes.first() == Some(&b'$'));
    let letters_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_alphabetic) {
        cursor += 1;
    }
    if cursor == letters_start {
        return Err(OfficeError::Invalid(format!(
            "{label} has no column letters: {value}"
        )));
    }
    let letters_end = cursor;
    if bytes.get(cursor) == Some(&b'$') {
        cursor += 1;
    }
    let digits_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    if cursor != bytes.len() || cursor == digits_start {
        return Err(OfficeError::Invalid(format!(
            "{label} is not valid A1 syntax: {value}"
        )));
    }

    let mut column = 0u32;
    for byte in &bytes[letters_start..letters_end] {
        let digit = u32::from(byte.to_ascii_uppercase() - b'A' + 1);
        column = column
            .checked_mul(26)
            .and_then(|current| current.checked_add(digit))
            .ok_or_else(|| {
                reference_resource_limit(format!("{label} column exceeds Excel bounds"))
            })?;
        if column > MAX_XLSX_COLUMNS {
            return Err(reference_resource_limit(format!(
                "{label} column exceeds XFD"
            )));
        }
    }

    let row = parse_decimal_u32(&value[digits_start..], label)?;
    if row == 0 || row > MAX_XLSX_ROWS {
        return Err(reference_resource_limit(format!(
            "{label} row {row} is outside 1..={MAX_XLSX_ROWS}"
        )));
    }
    Ok((column - 1, row - 1))
}

fn parse_decimal_u32(value: &str, label: &str) -> Result<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OfficeError::Invalid(format!(
            "{label} is not an unsigned integer: {value}"
        )));
    }
    let mut parsed = 0u32;
    for digit in value.bytes().map(|byte| u32::from(byte - b'0')) {
        parsed = parsed
            .checked_mul(10)
            .and_then(|current| current.checked_add(digit))
            .ok_or_else(|| reference_resource_limit(format!("{label} exceeds u32")))?;
    }
    Ok(parsed)
}

fn parse_required_one_based_index(value: Option<&str>, limit: u32, label: &str) -> Result<u32> {
    let raw = value.ok_or_else(|| OfficeError::Invalid(format!("{label} is missing")))?;
    let parsed = parse_decimal_u32(raw, label)?;
    if parsed == 0 || parsed > limit {
        return Err(reference_resource_limit(format!(
            "{label} {parsed} is outside 1..={limit}"
        )));
    }
    Ok(parsed - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a1_parsing() {
        assert_eq!(parse_a1("A1"), Some((0, 0)));
        assert_eq!(parse_a1("B2"), Some((1, 1)));
        assert_eq!(parse_a1("Z1"), Some((25, 0)));
        assert_eq!(parse_a1("AA1"), Some((26, 0)));
        assert_eq!(parse_a1("$C$5"), Some((2, 4)));
        assert_eq!(parse_a1("AB100"), Some((27, 99)));
    }

    #[test]
    fn a1_parsing_enforces_excel_bounds_without_panicking() {
        assert_eq!(parse_a1("XFD1048576"), Some((16_383, 1_048_575)));
        for invalid in ["A0", "XFE1", "A1048577", "AAAAAAAAAAAAAAAAAAAAAAAA1"] {
            assert_eq!(parse_a1(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn worksheet_required_references_distinguish_invalid_from_resource_limits() {
        let shared = SharedStrings::default();
        let worksheet = |body: &str| {
            format!(
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">{body}</worksheet>"#
            )
        };

        let malformed = Worksheet::parse(
            &worksheet(r#"<sheetData><row r="1"><c r="not-a-cell"/></row></sheetData>"#),
            &shared,
        )
        .expect_err("malformed cell reference");
        assert!(matches!(malformed, OfficeError::Invalid(_)));

        for body in [
            r#"<sheetData><row r="1"><c r="XFE1"/></row></sheetData>"#,
            r#"<cols><col min="1" max="16385"/></cols>"#,
            r#"<sheetData/><mergeCells><mergeCell ref="A1:XFE1"/></mergeCells>"#,
        ] {
            let error = Worksheet::parse(&worksheet(body), &shared).expect_err("Excel limit");
            assert!(matches!(
                error,
                OfficeError::Unsupported {
                    code: UnsupportedCode::ResourceLimit,
                    ..
                }
            ));
        }
    }

    #[test]
    fn worksheet_does_not_silently_default_malformed_required_references() {
        let shared = SharedStrings::default();
        let xml = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c/></row></sheetData></worksheet>"#;
        assert!(matches!(
            Worksheet::parse(xml, &shared).expect_err("missing cell reference"),
            OfficeError::Invalid(_)
        ));

        let xml = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/><mergeCells><mergeCell ref="A1:not-a-cell"/></mergeCells></worksheet>"#;
        assert!(matches!(
            Worksheet::parse(xml, &shared).expect_err("malformed merge reference"),
            OfficeError::Invalid(_)
        ));
    }
}

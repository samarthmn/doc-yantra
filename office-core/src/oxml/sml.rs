//! SpreadsheetML model: `workbook.xml`, `sharedStrings.xml`, `styles.xml`, and
//! per-sheet `worksheetN.xml`. Parse-only (no layout).

use super::{Rgb, S, attr, child, children, parse_f64, parse_i64};
use crate::{OfficeError, Result};
use std::collections::HashMap;

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
}

impl Workbook {
    /// Parses `xl/workbook.xml`.
    pub fn parse(xml: &str) -> Result<Workbook> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| OfficeError::Invalid(format!("workbook.xml: {e}")))?;
        let root = doc.root_element();
        let mut wb = Workbook::default();
        if let Some(pr) = child(root, S, "workbookPr") {
            wb.date1904 = matches!(attr(pr, S, "date1904"), Some("1") | Some("true"));
        }
        if let Some(sheets) = child(root, S, "sheets") {
            for sh in children(sheets) {
                if super::is(sh, S, "sheet") {
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
        Ok(wb)
    }
}

/// Shared strings (`sharedStrings.xml`).
#[derive(Debug, Clone, Default)]
pub struct SharedStrings(pub Vec<String>);

impl SharedStrings {
    /// Parses `xl/sharedStrings.xml` (plain + rich-text runs concatenated).
    pub fn parse(xml: &str) -> Result<SharedStrings> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| OfficeError::Invalid(format!("sharedStrings.xml: {e}")))?;
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
    /// Number-format id (builtin < 164, or a custom id).
    pub num_fmt_id: u32,
    /// Font size in points.
    pub font_size: f64,
    /// Bold.
    pub bold: bool,
    /// Font color.
    pub color: Option<Rgb>,
    /// Solid fill color.
    pub fill: Option<Rgb>,
    /// Horizontal alignment (`left`/`center`/`right`), if set.
    pub align: Option<String>,
    /// Whether text wraps.
    pub wrap: bool,
}

/// Parsed `styles.xml`: custom number formats + per-cellXf styles.
#[derive(Debug, Clone, Default)]
pub struct Styles {
    /// Custom number-format patterns by id (≥ 164).
    pub custom_formats: HashMap<u32, String>,
    /// Per-`cellXfs`-index style.
    pub cell_styles: Vec<CellStyle>,
}

impl Styles {
    /// Parses `xl/styles.xml`.
    pub fn parse(xml: &str) -> Result<Styles> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| OfficeError::Invalid(format!("styles.xml: {e}")))?;
        let root = doc.root_element();
        let mut styles = Styles::default();

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

        let fonts = parse_fonts(root);
        let fills = parse_fills(root);

        if let Some(cell_xfs) = child(root, S, "cellXfs") {
            for xf in children(cell_xfs) {
                if !super::is(xf, S, "xf") {
                    continue;
                }
                let font_idx = parse_i64(attr(xf, S, "fontId")).unwrap_or(0).max(0) as usize;
                let fill_idx = parse_i64(attr(xf, S, "fillId")).unwrap_or(0).max(0) as usize;
                let (font_size, bold, color) =
                    fonts.get(font_idx).copied().unwrap_or((11.0, false, None));
                let (mut align, mut wrap) = (None, false);
                if let Some(a) = child(xf, S, "alignment") {
                    align = attr(a, S, "horizontal").map(str::to_string);
                    wrap = matches!(attr(a, S, "wrapText"), Some("1") | Some("true"));
                }
                styles.cell_styles.push(CellStyle {
                    num_fmt_id: parse_i64(attr(xf, S, "numFmtId")).unwrap_or(0).max(0) as u32,
                    font_size,
                    bold,
                    color,
                    fill: fills.get(fill_idx).copied().flatten(),
                    align,
                    wrap,
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
}

fn parse_fonts(root: super::Node) -> Vec<(f64, bool, Option<Rgb>)> {
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
            let color = child(f, S, "color")
                .and_then(|c| attr(c, S, "rgb"))
                .and_then(parse_argb);
            out.push((size, bold, color));
        }
    }
    out
}

fn parse_fills(root: super::Node) -> Vec<Option<Rgb>> {
    let mut out = Vec::new();
    if let Some(fills) = child(root, S, "fills") {
        for f in children(fills) {
            let color = child(f, S, "patternFill")
                .filter(|pf| attr(*pf, S, "patternType") == Some("solid"))
                .and_then(|pf| child(pf, S, "fgColor"))
                .and_then(|c| attr(c, S, "rgb"))
                .and_then(parse_argb);
            out.push(color);
        }
    }
    out
}

/// Parses an 8-hex `AARRGGBB` (or 6-hex `RRGGBB`) color.
fn parse_argb(s: &str) -> Option<Rgb> {
    let hex = if s.len() == 8 { &s[2..] } else { s };
    Rgb::from_hex(hex)
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
    /// Whether the sheet has any drawings/charts (out of envelope).
    pub has_drawing: bool,
}

impl Worksheet {
    /// Parses a `xl/worksheets/sheetN.xml`, resolving cell values against
    /// `shared`.
    pub fn parse(xml: &str, shared: &SharedStrings) -> Result<Worksheet> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| OfficeError::Invalid(format!("worksheet: {e}")))?;
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
                let min = parse_i64(attr(c, S, "min")).unwrap_or(1).max(1) as u32 - 1;
                let max = parse_i64(attr(c, S, "max")).unwrap_or(1).max(1) as u32 - 1;
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
        if super::child(root, S, "drawing").is_some() {
            ws.has_drawing = true;
        }

        let (mut max_row, mut max_col) = (0u32, 0u32);
        if let Some(data) = child(root, S, "sheetData") {
            for row in children(data) {
                if !super::is(row, S, "row") {
                    continue;
                }
                let r_idx = parse_i64(attr(row, S, "r")).unwrap_or(1).max(1) as u32 - 1;
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
                    let (col, row_n) =
                        parse_a1(attr(c, S, "r").unwrap_or("A1")).unwrap_or((0, r_idx));
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
        ws.dim = (max_row, max_col);
        Ok(ws)
    }
}

fn parse_cell_value(cell: super::Node, ty: Option<&str>, shared: &SharedStrings) -> CellValue {
    let has_formula = child(cell, S, "f").is_some();
    let v = child(cell, S, "v")
        .and_then(|n| n.text())
        .map(str::to_string);
    let inline = child(cell, S, "is").map(|is_node| {
        let mut s = String::new();
        for d in is_node.descendants() {
            if super::is(d, S, "t") {
                s.push_str(d.text().unwrap_or_default());
            }
        }
        s
    });

    match ty {
        Some("s") => v
            .and_then(|i| i.trim().parse::<usize>().ok())
            .and_then(|i| shared.get(i).map(|s| CellValue::Text(s.to_string())))
            .unwrap_or(CellValue::Empty),
        Some("str") => CellValue::Text(v.unwrap_or_default()),
        Some("inlineStr") => CellValue::Text(inline.unwrap_or_default()),
        Some("b") => CellValue::Bool(v.as_deref() == Some("1")),
        Some("e") => CellValue::Error(v.unwrap_or_default()),
        _ => match v.and_then(|s| s.trim().parse::<f64>().ok()) {
            Some(n) => CellValue::Number(n),
            None if has_formula => CellValue::UncachedFormula,
            None => CellValue::Empty,
        },
    }
}

/// Parses an A1 reference (e.g. `AB12`, `$C$5`) into zero-based `(col, row)`.
pub fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let s: String = s.trim().chars().filter(|c| *c != '$').collect();
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    let mut col = 0u32;
    for ch in letters.chars() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        col = col * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
    }
    let row: u32 = digits.parse().ok()?;
    Some((col - 1, row - 1))
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
}

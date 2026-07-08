//! XLSX print rendering: paginates each sheet's used range into Letter pages and
//! renders cell fills, gridlines, and number-formatted text.
//!
//! v1 targets Excel's default "Save As PDF" for a single-scale sheet (down-then-
//! over pagination, gridlines on). Fit-to-page scaling and print-area/title-row
//! refinements are calibration follow-ups.

use super::numfmt::{self, Formatted};
use super::paginate::XlsxDoc;
use super::widths::{default_width_pt, width_chars_to_pt};
use crate::fonts::matching;
use crate::fonts::metrics::FaceMetrics;
use crate::layout::{GlyphRun, Item, LaidOutDocument, Page, PlacedGlyph, Positioned, Stroke};
use crate::oxml::sml::{Cell, CellStyle, CellValue, Worksheet};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::text::shape;
use std::collections::HashMap;

const PAGE_W: f64 = 612.0;
const PAGE_H: f64 = 792.0;
const MARGIN: f64 = 36.0;
const GRID: Rgba = Rgba {
    r: 190,
    g: 190,
    b: 190,
    a: 1.0,
};

/// Renders all sheets into a laid-out document.
pub fn render_xlsx(doc: &XlsxDoc) -> (LaidOutDocument, Vec<Warning>) {
    let mut warnings = Vec::new();
    let mut pages = Vec::new();
    for sheet in &doc.sheets {
        render_sheet(doc, &sheet.sheet, &mut pages, &mut warnings);
    }
    if pages.is_empty() {
        pages.push(Page {
            width_pt: PAGE_W,
            height_pt: PAGE_H,
            items: Vec::new(),
        });
    }
    let laid = LaidOutDocument {
        pages,
        images: Vec::new(),
        outline: Vec::new(),
        dates: doc.dates.clone(),
    };
    (laid, warnings)
}

fn render_sheet(doc: &XlsxDoc, ws: &Worksheet, pages: &mut Vec<Page>, warnings: &mut Vec<Warning>) {
    let (max_row, max_col) = ws.dim;
    if ws.cells.is_empty() {
        return;
    }
    let col_w = column_widths(ws, max_col);
    let row_h = |r: u32| {
        ws.row_heights
            .get(&r)
            .copied()
            .unwrap_or(ws.default_row_height)
    };

    let content_w = PAGE_W - 2.0 * MARGIN;
    let content_h = PAGE_H - 2.0 * MARGIN;

    // Cell lookup.
    let cells: HashMap<(u32, u32), &Cell> = ws.cells.iter().map(|c| ((c.row, c.col), c)).collect();

    // Column bands (each fits content width).
    let bands = split_bands(&col_w, content_w);
    for band in &bands {
        // Row pages within this band.
        let mut r = 0u32;
        while r <= max_row {
            let mut page = Page {
                width_pt: PAGE_W,
                height_pt: PAGE_H,
                items: Vec::new(),
            };
            let mut y = MARGIN;
            let mut rows_on_page = 0;
            while r <= max_row {
                if ws.hidden_rows.contains(&r) {
                    r += 1;
                    continue;
                }
                let h = row_h(r);
                if y + h > MARGIN + content_h && rows_on_page > 0 {
                    break;
                }
                render_row(
                    doc, &cells, band, &col_w, r, MARGIN, y, h, &mut page, warnings,
                );
                y += h;
                rows_on_page += 1;
                r += 1;
            }
            if !page.items.is_empty() {
                pages.push(page);
            } else {
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    doc: &XlsxDoc,
    cells: &HashMap<(u32, u32), &Cell>,
    band: &(u32, u32),
    col_w: &[f64],
    row: u32,
    x0: f64,
    y: f64,
    h: f64,
    page: &mut Page,
    warnings: &mut Vec<Warning>,
) {
    let mut x = x0;
    for col in band.0..=band.1 {
        let w = col_w
            .get(col as usize)
            .copied()
            .unwrap_or(default_width_pt());
        // Gridlines.
        for (dx0, dy0, dx1, dy1) in [
            (0.0, 0.0, w, 0.0),
            (0.0, h, w, h),
            (0.0, 0.0, 0.0, h),
            (w, 0.0, w, h),
        ] {
            page.items.push(Positioned {
                x_pt: x + dx0,
                y_pt: y + dy0,
                item: Item::Line {
                    dx_pt: dx1 - dx0,
                    dy_pt: dy1 - dy0,
                    stroke: Stroke {
                        width_pt: 0.25,
                        color: GRID,
                        dash: None,
                    },
                },
            });
        }
        if let Some(cell) = cells.get(&(row, col)) {
            let style = doc.styles.cell(cell.style_idx);
            if let Some(fill) = style.fill {
                page.items.push(Positioned {
                    x_pt: x,
                    y_pt: y,
                    item: Item::Rect {
                        w_pt: w,
                        h_pt: h,
                        fill: Some(Rgba::opaque(fill)),
                        stroke: None,
                    },
                });
            }
            render_cell_text(doc, cell, &style, x, y, w, h, page, warnings);
        }
        x += w;
    }
}

#[allow(clippy::too_many_arguments)]
fn render_cell_text(
    doc: &XlsxDoc,
    cell: &Cell,
    style: &CellStyle,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    page: &mut Page,
    warnings: &mut Vec<Warning>,
) {
    let (text, color, default_align) = format_cell(doc, cell, style);
    if text.is_empty() {
        return;
    }
    let size = if style.font_size > 0.0 {
        style.font_size
    } else {
        11.0
    };
    let weight = if style.bold { 700 } else { 400 };
    let resolved = matching::resolve(&doc.fonts, "Calibri", weight, false);
    for wn in resolved.warnings {
        if !warnings.contains(&wn) {
            warnings.push(wn);
        }
    }
    let Some(face) = resolved.face else { return };
    let Some(parsed) = doc.fonts.face(face).and_then(|f| f.parsed()) else {
        return;
    };
    let metrics = FaceMetrics::new(parsed);
    let shaped = shape::shape(&text, &metrics, size, 0.0);
    let text_w = shaped.width_pt;

    let align = style.align.as_deref().unwrap_or(default_align);
    let pad = 2.0;
    let tx = match align {
        "center" => x + (w - text_w) / 2.0,
        "right" => x + w - text_w - pad,
        _ => x + pad,
    };
    let baseline = y + (h + metrics.ascent_pt(size) - metrics.descent_pt(size)) / 2.0;

    let glyphs: Vec<PlacedGlyph> = {
        let chars: Vec<char> = text.chars().collect();
        shaped
            .glyphs
            .iter()
            .enumerate()
            .map(|(i, g)| PlacedGlyph {
                gid: g.id.0,
                advance_pt: g.advance_pt,
                ch: chars.get(i).copied().unwrap_or(' '),
            })
            .collect()
    };
    page.items.push(Positioned {
        x_pt: tx.max(x + pad),
        y_pt: baseline,
        item: Item::Glyphs(GlyphRun {
            face,
            size_pt: size,
            color: color.unwrap_or(Rgba::opaque(Rgb::new(0, 0, 0))),
            glyphs,
            underline: None,
            strike: None,
        }),
    });
}

/// Formats a cell to `(text, color, default alignment)`.
fn format_cell(
    doc: &XlsxDoc,
    cell: &Cell,
    style: &CellStyle,
) -> (String, Option<Rgba>, &'static str) {
    let code = resolve_code(doc, style.num_fmt_id);
    let base_color = style.color.map(Rgba::opaque);
    match &cell.value {
        CellValue::Number(n) => {
            let Formatted { text, color } = numfmt::format_number(*n, &code, doc.workbook.date1904);
            (text, color.map(Rgba::opaque).or(base_color), "right")
        }
        CellValue::Text(t) => (numfmt::format_text(t, &code), base_color, "left"),
        CellValue::Bool(b) => (
            (if *b { "TRUE" } else { "FALSE" }).to_string(),
            base_color,
            "center",
        ),
        CellValue::Error(e) => (e.clone(), base_color, "center"),
        CellValue::UncachedFormula | CellValue::Empty => (String::new(), None, "left"),
    }
}

fn resolve_code(doc: &XlsxDoc, num_fmt_id: u32) -> String {
    if let Some(b) = numfmt::builtin_code(num_fmt_id) {
        return b.to_string();
    }
    doc.styles
        .format_code(num_fmt_id)
        .map(str::to_string)
        .unwrap_or_else(|| "General".to_string())
}

/// Per-column widths (points) for columns `0..=max_col`, from `<cols>` or default.
fn column_widths(ws: &Worksheet, max_col: u32) -> Vec<f64> {
    let mut widths = vec![default_width_pt(); (max_col + 1) as usize];
    for info in &ws.cols {
        let w = if info.hidden {
            0.0
        } else {
            width_chars_to_pt(info.width_chars)
        };
        for c in info.min..=info.max.min(max_col) {
            if (c as usize) < widths.len() {
                widths[c as usize] = w;
            }
        }
    }
    widths
}

/// Splits columns into left-to-right bands that each fit `content_w`.
fn split_bands(col_w: &[f64], content_w: f64) -> Vec<(u32, u32)> {
    let mut bands = Vec::new();
    let mut start = 0u32;
    let mut acc = 0.0;
    for (i, &w) in col_w.iter().enumerate() {
        let i = i as u32;
        if acc + w > content_w && i > start {
            bands.push((start, i - 1));
            start = i;
            acc = 0.0;
        }
        acc += w;
    }
    bands.push((start, col_w.len().saturating_sub(1) as u32));
    bands
}

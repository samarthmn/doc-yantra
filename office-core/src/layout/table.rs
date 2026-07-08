//! Table layout: resolve column widths, lay out cell content, and draw borders
//! and shading. Produces per-row [`RowLayout`]s that [`super::flow`] places and
//! page-breaks between (repeating header rows).
//!
//! v1 uses Word's stored `tblGrid` widths (Word writes resolved widths even for
//! autofit tables), scaled to the available width; the min/max autofit refit is
//! a fidelity-harness refinement.

use super::assemble::WordDoc;
use super::numbering_layout::ListCounters;
use super::paragraph::{LineItem, layout_paragraph};
use super::{Item, Stroke};
use crate::oxml::wml::{BlockContent, Border, Cell, Table, VAlign, VMerge};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use std::collections::HashMap;

/// Default cell side margin (108 twips = 5.4pt) when unset.
const DEFAULT_CELL_MARGIN_PT: f64 = 5.4;

/// A laid-out table row: items positioned relative to the row's top-left
/// (row-left is the table's left edge in content coordinates).
#[derive(Debug, Default)]
pub struct RowLayout {
    /// Row height in points.
    pub height_pt: f64,
    /// Whether this is a repeatable header row.
    pub is_header: bool,
    /// Positioned items (shading, borders, glyph runs), `x`/`y` relative to the
    /// row top-left.
    pub items: Vec<LineItem>,
}

/// Lays out `table` into rows within `avail_width_pt`, starting at `table_left`
/// (relative to the content-left edge).
pub fn layout_table(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    table: &Table,
    rels: &HashMap<String, String>,
    avail_width_pt: f64,
    table_left: f64,
    counters: &mut ListCounters,
) -> Vec<RowLayout> {
    let cols = resolve_columns(table, avail_width_pt);
    let col_x = column_offsets(&cols, table_left);
    let mut rows = Vec::new();

    for row in &table.rows {
        let mut col = 0usize;
        let mut cell_boxes: Vec<CellBox> = Vec::new();
        let mut row_content_h = 0.0_f64;

        for cell in &row.cells {
            let span =
                (cell.props.grid_span.max(1) as usize).min(cols.len().saturating_sub(col).max(1));
            let x = col_x.get(col).copied().unwrap_or(table_left);
            let width: f64 = cols.iter().skip(col).take(span).sum();
            let (m_l, m_r, m_t, m_b) = cell_margins(cell, table);
            let content_w = (width - m_l - m_r).max(1.0);

            let is_continue = cell.props.v_merge == Some(VMerge::Continue);
            let (items, height) = if is_continue {
                (Vec::new(), 0.0)
            } else {
                layout_cell(doc, warnings, &cell.content, rels, content_w, counters)
            };
            row_content_h = row_content_h.max(height + m_t + m_b);

            cell_boxes.push(CellBox {
                x,
                width,
                m_l,
                m_t,
                is_continue,
                v_align: cell.props.v_align,
                shading: cell.props.shading.map(Rgba::opaque),
                borders: resolve_cell_borders(cell, table),
                content: items,
                content_h: height,
            });
            col += span;
        }

        let height = apply_row_height(row.props.height, row_content_h);
        let mut items = Vec::new();
        for cb in &cell_boxes {
            emit_cell(cb, height, &mut items);
        }
        rows.push(RowLayout {
            height_pt: height,
            is_header: row.props.header,
            items,
        });
    }
    rows
}

struct CellBox {
    x: f64,
    width: f64,
    m_l: f64,
    m_t: f64,
    is_continue: bool,
    v_align: VAlign,
    shading: Option<Rgba>,
    borders: [Option<Stroke>; 4], // top, right, bottom, left
    content: Vec<LineItem>,
    content_h: f64,
}

/// Emits a cell's shading, content, and borders into the row's item list.
fn emit_cell(cb: &CellBox, row_height: f64, items: &mut Vec<LineItem>) {
    if cb.is_continue {
        // Merged-away cell: draw only side borders so the column stays framed.
        push_border(
            items,
            cb,
            1,
            cb.x + cb.width,
            0.0,
            cb.x + cb.width,
            row_height,
        ); // right
        push_border(items, cb, 3, cb.x, 0.0, cb.x, row_height); // left
        return;
    }
    // Shading behind the whole cell box.
    if let Some(fill) = cb.shading {
        items.push(LineItem {
            x_pt: cb.x,
            y_pt: 0.0,
            item: Item::Rect {
                w_pt: cb.width,
                h_pt: row_height,
                fill: Some(fill),
                stroke: None,
            },
        });
    }
    // Content, vertically aligned within the row.
    let free = (row_height - cb.content_h - cb.m_t).max(0.0);
    let dy = match cb.v_align {
        VAlign::Center => free / 2.0,
        VAlign::Bottom => free,
        VAlign::Top => 0.0,
    };
    for li in &cb.content {
        items.push(LineItem {
            x_pt: cb.x + cb.m_l + li.x_pt,
            y_pt: cb.m_t + dy + li.y_pt,
            item: li.item.clone(),
        });
    }
    // Borders (top, right, bottom, left).
    push_border(items, cb, 0, cb.x, 0.0, cb.x + cb.width, 0.0);
    push_border(
        items,
        cb,
        1,
        cb.x + cb.width,
        0.0,
        cb.x + cb.width,
        row_height,
    );
    push_border(items, cb, 2, cb.x, row_height, cb.x + cb.width, row_height);
    push_border(items, cb, 3, cb.x, 0.0, cb.x, row_height);
}

fn push_border(
    items: &mut Vec<LineItem>,
    cb: &CellBox,
    edge: usize,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
) {
    if let Some(stroke) = &cb.borders[edge] {
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

/// Lays out a cell's block content (paragraphs; nested tables recurse) into a
/// box of `width`, returning items (top-left origin) and total height.
fn layout_cell(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    blocks: &[BlockContent],
    rels: &HashMap<String, String>,
    width: f64,
    counters: &mut ListCounters,
) -> (Vec<LineItem>, f64) {
    let mut items = Vec::new();
    let mut y = 0.0_f64;
    for block in blocks {
        match block {
            BlockContent::Paragraph(p) => {
                let label = counter_label(doc, warnings, p, counters);
                let layout = layout_paragraph(doc, warnings, p, rels, width, label, None);
                y += layout.space_before_pt;
                for line in &layout.lines {
                    for li in &line.items {
                        items.push(LineItem {
                            x_pt: li.x_pt,
                            y_pt: y + li.y_pt,
                            item: li.item.clone(),
                        });
                    }
                    y += line.height_pt;
                }
                y += layout.space_after_pt;
            }
            BlockContent::Table(t) => {
                let rows = layout_table(doc, warnings, t, rels, width, 0.0, counters);
                for row in rows {
                    for li in &row.items {
                        items.push(LineItem {
                            x_pt: li.x_pt,
                            y_pt: y + li.y_pt,
                            item: li.item.clone(),
                        });
                    }
                    y += row.height_pt;
                }
            }
        }
    }
    (items, y)
}

/// Computes a list label from the counters for a paragraph (if numbered).
fn counter_label(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    p: &crate::oxml::wml::Paragraph,
    counters: &mut ListCounters,
) -> Option<super::numbering_layout::PlacedLabel> {
    let eff = doc.styles.para_props(&p.props);
    let num_ref = eff.numbering?;
    let (text, level) = counters.advance(doc, num_ref)?;
    super::numbering_layout::build_label(doc, warnings, p, &text, &level)
}

/// Resolves column widths from the grid, scaled to the available width.
fn resolve_columns(table: &Table, avail: f64) -> Vec<f64> {
    let mut cols: Vec<f64> = if table.grid.is_empty() {
        // No grid: infer column count from the widest row, equal widths.
        let n = table
            .rows
            .iter()
            .map(|r| r.cells.len())
            .max()
            .unwrap_or(1)
            .max(1);
        vec![avail / n as f64; n]
    } else {
        table.grid.clone()
    };
    let total: f64 = cols.iter().sum();
    if total > avail && total > 0.0 {
        let scale = avail / total;
        for c in &mut cols {
            *c *= scale;
        }
    }
    cols
}

/// Left x of each column (relative to content-left), given the table's left.
fn column_offsets(cols: &[f64], table_left: f64) -> Vec<f64> {
    let mut xs = Vec::with_capacity(cols.len());
    let mut x = table_left;
    for &w in cols {
        xs.push(x);
        x += w;
    }
    xs
}

fn cell_margins(cell: &Cell, table: &Table) -> (f64, f64, f64, f64) {
    let m = &cell.props.margins;
    let d = &table.props.cell_margins;
    let l = m.left.or(d.left).unwrap_or(DEFAULT_CELL_MARGIN_PT);
    let r = m.right.or(d.right).unwrap_or(DEFAULT_CELL_MARGIN_PT);
    let t = m.top.or(d.top).unwrap_or(0.0);
    let b = m.bottom.or(d.bottom).unwrap_or(0.0);
    (l, r, t, b)
}

fn apply_row_height(height: Option<crate::oxml::wml::RowHeight>, content: f64) -> f64 {
    use crate::oxml::wml::RowHeight;
    match height {
        Some(RowHeight::Exact(v)) => v,
        Some(RowHeight::AtLeast(v)) => v.max(content),
        None => content.max(1.0),
    }
}

/// Resolves the four edge strokes for a cell: `[top, right, bottom, left]`.
fn resolve_cell_borders(cell: &Cell, table: &Table) -> [Option<Stroke>; 4] {
    let cb = &cell.props.borders;
    let tb = &table.props.borders;
    [
        stroke_for(cb.top.as_ref().or(tb.inside_h.as_ref()).or(tb.top.as_ref())),
        stroke_for(
            cb.right
                .as_ref()
                .or(tb.inside_v.as_ref())
                .or(tb.right.as_ref()),
        ),
        stroke_for(
            cb.bottom
                .as_ref()
                .or(tb.inside_h.as_ref())
                .or(tb.bottom.as_ref()),
        ),
        stroke_for(
            cb.left
                .as_ref()
                .or(tb.inside_v.as_ref())
                .or(tb.left.as_ref()),
        ),
    ]
}

fn stroke_for(border: Option<&Border>) -> Option<Stroke> {
    let b = border?;
    if b.val == "nil" || b.val == "none" {
        return None;
    }
    Some(Stroke {
        width_pt: b.size_pt.max(0.25),
        color: b
            .color
            .map(Rgba::opaque)
            .unwrap_or(Rgba::opaque(Rgb::new(0, 0, 0))),
        dash: match b.val.as_str() {
            "dotted" => Some(vec![1.0, 1.0]),
            "dashed" | "dashSmallGap" => Some(vec![3.0, 1.0]),
            _ => None,
        },
    })
}

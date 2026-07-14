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
use super::{FillRule, Item, PathCommand, Positioned, Stroke};
use crate::Result;
use crate::oxml::TblWidth;
use crate::oxml::wml::{BlockContent, Border, Cell, Jc, Row, RowHeight, Table, VAlign, VMerge};
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

/// Laid-out table rows plus the resolved grid footprint.
#[derive(Debug, Default)]
pub struct TableLayout {
    /// Resolved table width in points.
    pub width_pt: f64,
    /// Rows positioned relative to the table's left edge.
    pub rows: Vec<RowLayout>,
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
) -> Result<TableLayout> {
    let cols = resolve_columns(table, avail_width_pt);
    let table_width: f64 = cols.iter().sum();
    let free_width = (avail_width_pt - table_width).max(0.0);
    let leading_cell_margin = table
        .props
        .cell_margins
        .left
        .unwrap_or(DEFAULT_CELL_MARGIN_PT);
    let start_offset = table
        .props
        .indent_pt
        .map_or(0.0, |indent| indent - leading_cell_margin);
    let aligned_left = table_left
        + match table.props.alignment.unwrap_or(Jc::Start) {
            Jc::Center => free_width / 2.0,
            Jc::End => free_width,
            Jc::Start => start_offset,
            Jc::Both | Jc::Distribute => 0.0,
        };
    let col_x = column_offsets(&cols, aligned_left);
    let mut rows = Vec::new();

    let visible_rows: Vec<&Row> = table.rows.iter().filter(|row| !row.props.hidden).collect();
    let horizontal_boundaries =
        horizontal_boundary_plan(doc, &visible_rows, table, &cols, aligned_left);

    for (row_index, row) in visible_rows.into_iter().enumerate() {
        let mut col = row.props.grid_before as usize;
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
                layout_cell(doc, warnings, &cell.content, rels, content_w, counters)?
            };
            let border_top = horizontal_boundaries[row_index].half_width_for_span(col, col + span);
            let border_bottom =
                horizontal_boundaries[row_index + 1].half_width_for_span(col, col + span);
            row_content_h = row_content_h.max(height + m_t + m_b + border_top + border_bottom);

            cell_boxes.push(CellBox {
                x,
                width,
                m_l,
                m_r,
                m_t,
                m_b,
                border_top,
                border_bottom,
                is_continue,
                v_align: cell.props.v_align,
                shading: cell.props.shading.map(Rgba::opaque),
                borders: resolve_cell_borders(doc, cell, table, col == 0, col + span == cols.len()),
                content: items,
                content_h: height,
                clip_content: matches!(row.props.height, Some(RowHeight::Exact(_))),
            });
            col += span;
        }

        row_content_h = row_content_h.max(
            (horizontal_boundaries[row_index].max_width_pt
                + horizontal_boundaries[row_index + 1].max_width_pt)
                / 2.0,
        );
        let height = apply_row_height(row.props.height, row_content_h);
        let mut items = Vec::new();
        for cb in &cell_boxes {
            emit_cell(cb, height, &mut items);
        }
        if row_index == 0 {
            emit_horizontal_boundary(&horizontal_boundaries[0], 0.0, &mut items);
        }
        emit_horizontal_boundary(&horizontal_boundaries[row_index + 1], height, &mut items);
        rows.push(RowLayout {
            height_pt: height,
            is_header: row.props.header,
            items,
        });
    }

    // Row heights run from horizontal border center to border center. Reserve
    // the two outer half-strokes as part of the table block as well, so the
    // paragraph gap ends at the table's outer edge and following content starts
    // after its bottom outer edge. Internal shared boundaries remain unchanged.
    let leading = horizontal_boundaries
        .first()
        .map_or(0.0, |boundary| boundary.max_width_pt / 2.0);
    let trailing = horizontal_boundaries
        .last()
        .map_or(0.0, |boundary| boundary.max_width_pt / 2.0);
    if let Some(first) = rows.first_mut() {
        for item in &mut first.items {
            item.y_pt += leading;
        }
        first.height_pt += leading;
    }
    if let Some(last) = rows.last_mut() {
        last.height_pt += trailing;
    }
    Ok(TableLayout {
        width_pt: table_width,
        rows,
    })
}

struct CellBox {
    x: f64,
    width: f64,
    m_l: f64,
    m_r: f64,
    m_t: f64,
    m_b: f64,
    border_top: f64,
    border_bottom: f64,
    is_continue: bool,
    v_align: VAlign,
    shading: Option<Rgba>,
    borders: [Option<Stroke>; 4], // top, right, bottom, left
    content: Vec<LineItem>,
    content_h: f64,
    clip_content: bool,
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
    // Content is aligned and, for exact rows, clipped inside the border and
    // authored cell-margin box. Keeping both bottom insets here prevents
    // center/bottom alignment from drifting into the bottom margin.
    let content_left = cb.m_l.min(cb.width);
    let content_right = (cb.width - cb.m_r).max(content_left);
    let content_top = (cb.border_top + cb.m_t).min(row_height);
    let content_bottom = (row_height - cb.border_bottom - cb.m_b).max(content_top);
    let content_height = content_bottom - content_top;
    let free = (content_height - cb.content_h).max(0.0);
    let dy = match cb.v_align {
        VAlign::Center => free / 2.0,
        VAlign::Bottom => free,
        VAlign::Top => 0.0,
    };
    let content_y = content_top + dy;
    if cb.clip_content {
        let mut clipped = Vec::new();
        for li in &cb.content {
            match &li.item {
                Item::Link { w_pt, h_pt, target } => {
                    let left = (cb.m_l + li.x_pt).max(content_left);
                    let top = (content_y + li.y_pt).max(content_top);
                    let right = (cb.m_l + li.x_pt + w_pt).min(content_right);
                    let bottom = (content_y + li.y_pt + h_pt).min(content_bottom);
                    if right > left && bottom > top {
                        items.push(LineItem {
                            x_pt: cb.x + left,
                            y_pt: top,
                            item: Item::Link {
                                w_pt: right - left,
                                h_pt: bottom - top,
                                target: target.clone(),
                            },
                        });
                    }
                }
                _ => clipped.push(Positioned {
                    x_pt: cb.m_l + li.x_pt,
                    y_pt: content_y + li.y_pt,
                    item: li.item.clone(),
                }),
            }
        }
        items.push(LineItem {
            x_pt: cb.x,
            y_pt: 0.0,
            item: Item::Clip {
                commands: rectangle_path(
                    content_left,
                    content_top,
                    content_right - content_left,
                    content_height,
                ),
                fill_rule: FillRule::NonZero,
                items: clipped,
            },
        });
    } else {
        for li in &cb.content {
            items.push(LineItem {
                x_pt: cb.x + cb.m_l + li.x_pt,
                y_pt: content_y + li.y_pt,
                item: li.item.clone(),
            });
        }
    }
    // Vertical borders remain cell-owned. Horizontal collapsed boundaries are
    // emitted once from the precomputed table boundary plan.
    push_border(
        items,
        cb,
        1,
        cb.x + cb.width,
        0.0,
        cb.x + cb.width,
        row_height,
    );
    push_border(items, cb, 3, cb.x, 0.0, cb.x, row_height);
}

fn rectangle_path(left: f64, top: f64, width: f64, height: f64) -> Vec<PathCommand> {
    let right = left + width;
    let bottom = top + height;
    vec![
        PathCommand::MoveTo { x: left, y: top },
        PathCommand::LineTo { x: right, y: top },
        PathCommand::LineTo {
            x: right,
            y: bottom,
        },
        PathCommand::LineTo { x: left, y: bottom },
        PathCommand::Close,
    ]
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
) -> Result<(Vec<LineItem>, f64)> {
    let mut items = Vec::new();
    let mut y = 0.0_f64;
    for block in blocks {
        match block {
            BlockContent::Paragraph(p) => {
                let label = counter_label(doc, warnings, p, counters)?;
                let layout = layout_paragraph(doc, warnings, p, rels, width, label, None)?;
                if !layout.page_floats.is_empty()
                    || layout
                        .lines
                        .iter()
                        .any(|line| !line.anchored_items.is_empty())
                {
                    return Err(crate::OfficeError::unsupported(
                        crate::UnsupportedCode::AnchoredDrawing,
                        "anchored drawing in a table cell",
                    ));
                }
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
                let table_layout = layout_table(doc, warnings, t, rels, width, 0.0, counters)?;
                for row in table_layout.rows {
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
    Ok((items, y))
}

/// Computes a list label from the counters for a paragraph (if numbered).
fn counter_label(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    p: &crate::oxml::wml::Paragraph,
    counters: &mut ListCounters,
) -> Result<Option<super::numbering_layout::PlacedLabel>> {
    let eff = doc.styles.para_props(&p.props);
    let Some(num_ref) = eff.numbering else {
        return Ok(None);
    };
    let Some((text, level)) = counters.advance(doc, num_ref) else {
        return Ok(None);
    };
    super::numbering_layout::build_label(doc, warnings, p, &text, &level)
}

/// Resolves column widths from the grid, scaled to the available width.
fn resolve_columns(table: &Table, avail: f64) -> Vec<f64> {
    let column_count = table
        .rows
        .iter()
        .map(|row| {
            row.props.grid_before as usize
                + row
                    .cells
                    .iter()
                    .map(|cell| cell.props.grid_span.max(1) as usize)
                    .sum::<usize>()
                + row.props.grid_after as usize
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let target_width = table
        .props
        .width
        .and_then(|width| width_points(width, avail));
    let mut cols: Vec<f64> = if table.grid.is_empty() {
        vec![0.0; column_count]
    } else {
        table.grid.clone()
    };
    if cols.len() < column_count {
        cols.resize(column_count, 0.0);
    }

    if table.props.layout_fixed {
        for row in &table.rows {
            apply_aggregate_width(
                &mut cols,
                0,
                row.props.grid_before as usize,
                row.props.width_before,
                avail,
            );
            let mut column = row.props.grid_before as usize;
            for cell in &row.cells {
                let span = cell.props.grid_span.max(1) as usize;
                apply_aggregate_width(&mut cols, column, span, cell.props.width, avail);
                column += span;
            }
            apply_aggregate_width(
                &mut cols,
                column,
                row.props.grid_after as usize,
                row.props.width_after,
                avail,
            );
        }
    }

    let unspecified = cols.iter().filter(|width| **width <= 0.0).count();
    if unspecified > 0 {
        let specified: f64 = cols.iter().copied().filter(|width| *width > 0.0).sum();
        let fallback_total = target_width.unwrap_or(avail);
        let fallback = ((fallback_total - specified).max(0.0) / unspecified as f64).max(1.0);
        for width in &mut cols {
            if *width <= 0.0 {
                *width = fallback;
            }
        }
    }

    let total: f64 = cols.iter().sum();
    let desired = target_width.or_else(|| (!table.props.layout_fixed).then_some(avail.min(total)));
    if let Some(desired) = desired
        && total > 0.0
        && (total - desired).abs() > 0.01
    {
        let scale = desired / total;
        for c in &mut cols {
            *c *= scale;
        }
    }
    cols
}

fn apply_aggregate_width(
    columns: &mut [f64],
    start: usize,
    span: usize,
    width: Option<TblWidth>,
    available: f64,
) {
    let Some(total) = width.and_then(|width| width_points(width, available)) else {
        return;
    };
    if span == 0 || start >= columns.len() {
        return;
    }
    let end = (start + span).min(columns.len());
    let current: f64 = columns[start..end].iter().sum();
    let deficit = (total - current).max(0.0);
    if deficit <= f64::EPSILON {
        return;
    }
    let each = deficit / (end - start) as f64;
    for column in &mut columns[start..end] {
        *column += each;
    }
}

fn width_points(width: TblWidth, available: f64) -> Option<f64> {
    match width {
        TblWidth::Pt(points) => Some(points.max(0.0)),
        TblWidth::Pct(fraction) => Some((available * fraction).max(0.0)),
        TblWidth::Auto | TblWidth::Nil => None,
    }
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

#[derive(Debug, Clone)]
enum BorderState {
    Absent,
    Suppressed,
    Visible(Border),
}

#[derive(Debug, Clone, Copy)]
struct CellPlacement<'a> {
    cell: &'a Cell,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
struct BoundarySegment {
    x_pt: f64,
    width_pt: f64,
    stroke: Stroke,
}

#[derive(Debug, Clone, Default)]
struct HorizontalBoundary {
    max_width_pt: f64,
    column_widths: Vec<f64>,
    segments: Vec<BoundarySegment>,
}

impl HorizontalBoundary {
    fn half_width_for_span(&self, start: usize, end: usize) -> f64 {
        let end = end.min(self.column_widths.len());
        if start >= end {
            return 0.0;
        }
        self.column_widths[start..end]
            .iter()
            .copied()
            .fold(0.0_f64, f64::max)
            / 2.0
    }
}

fn horizontal_boundary_plan(
    doc: &WordDoc,
    rows: &[&Row],
    table: &Table,
    cols: &[f64],
    table_left: f64,
) -> Vec<HorizontalBoundary> {
    if rows.is_empty() {
        return vec![HorizontalBoundary::default()];
    }

    let mut edges = Vec::with_capacity(cols.len() + 1);
    let mut x = table_left;
    edges.push(x);
    for width in cols {
        x += *width;
        edges.push(x);
    }

    (0..=rows.len())
        .map(|boundary| {
            let strokes: Vec<Option<Stroke>> = (0..cols.len())
                .map(|column| {
                    let state = if boundary == 0 {
                        let placement = row_cell_placement(rows[0], column);
                        horizontal_edge_state(placement, table, true, true)
                    } else if boundary == rows.len() {
                        let placement = row_cell_placement(rows[rows.len() - 1], column);
                        horizontal_edge_state(placement, table, false, true)
                    } else {
                        let above = row_cell_placement(rows[boundary - 1], column);
                        let below = row_cell_placement(rows[boundary], column);
                        if is_valid_vertical_merge_seam(above, below) {
                            BorderState::Suppressed
                        } else {
                            choose_boundary_border(
                                horizontal_edge_state(above, table, false, false),
                                horizontal_edge_state(below, table, true, false),
                                doc,
                            )
                        }
                    };
                    stroke_from_state(&state, doc)
                })
                .collect();

            let mut segments = Vec::new();
            let mut start = 0usize;
            while start < strokes.len() {
                let mut end = start + 1;
                while end < strokes.len() && same_optional_stroke(&strokes[start], &strokes[end]) {
                    end += 1;
                }
                if let Some(stroke) = &strokes[start] {
                    segments.push(BoundarySegment {
                        x_pt: edges[start],
                        width_pt: edges[end] - edges[start],
                        stroke: stroke.clone(),
                    });
                }
                start = end;
            }
            let max_width_pt = segments
                .iter()
                .map(|segment| segment.stroke.width_pt)
                .fold(0.0_f64, f64::max);
            let column_widths = strokes
                .iter()
                .map(|stroke| stroke.as_ref().map_or(0.0, |stroke| stroke.width_pt))
                .collect();
            HorizontalBoundary {
                max_width_pt,
                column_widths,
                segments,
            }
        })
        .collect()
}

fn row_cell_placement(row: &Row, column: usize) -> Option<CellPlacement<'_>> {
    let mut start = row.props.grid_before as usize;
    for cell in &row.cells {
        let end = start + cell.props.grid_span.max(1) as usize;
        if (start..end).contains(&column) {
            return Some(CellPlacement { cell, start, end });
        }
        start = end;
    }
    None
}

fn is_valid_vertical_merge_seam(
    above: Option<CellPlacement<'_>>,
    below: Option<CellPlacement<'_>>,
) -> bool {
    let (Some(above), Some(below)) = (above, below) else {
        return false;
    };
    below.cell.props.v_merge == Some(VMerge::Continue)
        && matches!(
            above.cell.props.v_merge,
            Some(VMerge::Restart | VMerge::Continue)
        )
        && above.start == below.start
        && above.end == below.end
}

fn horizontal_edge_state(
    placement: Option<CellPlacement<'_>>,
    table: &Table,
    top: bool,
    outer: bool,
) -> BorderState {
    let cell_border = placement.and_then(|placement| {
        if top {
            placement.cell.props.borders.top.as_ref()
        } else {
            placement.cell.props.borders.bottom.as_ref()
        }
    });
    let table_border = if outer {
        if top {
            table.props.borders.top.as_ref()
        } else {
            table.props.borders.bottom.as_ref()
        }
    } else {
        table.props.borders.inside_h.as_ref()
    };
    effective_border_state(cell_border, table_border)
}

fn effective_border_state(
    cell_border: Option<&Border>,
    table_border: Option<&Border>,
) -> BorderState {
    if let Some(border) = cell_border {
        return authored_border_state(border);
    }
    if let Some(border) = table_border {
        return authored_border_state(border);
    }
    BorderState::Absent
}

fn authored_border_state(border: &Border) -> BorderState {
    if matches!(border.val.as_str(), "nil" | "none") {
        BorderState::Suppressed
    } else {
        BorderState::Visible(border.clone())
    }
}

fn choose_boundary_border(first: BorderState, second: BorderState, doc: &WordDoc) -> BorderState {
    match (first, second) {
        (BorderState::Visible(first), BorderState::Visible(second)) => {
            match first.size_pt.total_cmp(&second.size_pt) {
                std::cmp::Ordering::Greater => BorderState::Visible(first),
                std::cmp::Ordering::Less => BorderState::Visible(second),
                std::cmp::Ordering::Equal => {
                    if border_darkness(&first, doc) <= border_darkness(&second, doc) {
                        BorderState::Visible(first)
                    } else {
                        BorderState::Visible(second)
                    }
                }
            }
        }
        (visible @ BorderState::Visible(_), _) => visible,
        (_, visible @ BorderState::Visible(_)) => visible,
        (BorderState::Suppressed, _) | (_, BorderState::Suppressed) => BorderState::Suppressed,
        (BorderState::Absent, BorderState::Absent) => BorderState::Absent,
    }
}

fn border_darkness(border: &Border, doc: &WordDoc) -> u16 {
    let color = border
        .resolved_color(&doc.theme)
        .unwrap_or(Rgb::new(0, 0, 0));
    u16::from(color.r) + u16::from(color.g) + u16::from(color.b)
}

fn same_optional_stroke(left: &Option<Stroke>, right: &Option<Stroke>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            left.width_pt == right.width_pt && left.color == right.color && left.dash == right.dash
        }
        (None, None) => true,
        _ => false,
    }
}

fn emit_horizontal_boundary(boundary: &HorizontalBoundary, y_pt: f64, items: &mut Vec<LineItem>) {
    for segment in &boundary.segments {
        items.push(LineItem {
            x_pt: segment.x_pt,
            y_pt,
            item: Item::Line {
                dx_pt: segment.width_pt,
                dy_pt: 0.0,
                stroke: segment.stroke.clone(),
            },
        });
    }
}

fn apply_row_height(height: Option<crate::oxml::wml::RowHeight>, content: f64) -> f64 {
    use crate::oxml::wml::RowHeight;
    match height {
        Some(RowHeight::Exact(v)) => v,
        Some(RowHeight::AtLeast(v)) => v.max(content),
        None => content.max(1.0),
    }
}

/// Resolves the cell-owned vertical strokes. Horizontal strokes are collapsed
/// and emitted by [`horizontal_boundary_plan`].
fn resolve_cell_borders(
    doc: &WordDoc,
    cell: &Cell,
    table: &Table,
    outer_left: bool,
    outer_right: bool,
) -> [Option<Stroke>; 4] {
    let cb = &cell.props.borders;
    let tb = &table.props.borders;
    let right_table = if outer_right {
        tb.right.as_ref()
    } else {
        tb.inside_v.as_ref()
    };
    let left_table = if outer_left {
        tb.left.as_ref()
    } else {
        tb.inside_v.as_ref()
    };
    [
        None,
        stroke_from_state(&effective_border_state(cb.right.as_ref(), right_table), doc),
        None,
        stroke_from_state(&effective_border_state(cb.left.as_ref(), left_table), doc),
    ]
}

fn stroke_from_state(state: &BorderState, doc: &WordDoc) -> Option<Stroke> {
    let BorderState::Visible(border) = state else {
        return None;
    };
    Some(Stroke {
        width_pt: border.size_pt.max(0.25),
        color: Rgba::opaque(
            border
                .resolved_color(&doc.theme)
                .unwrap_or(Rgb::new(0, 0, 0)),
        ),
        dash: match border.val.as_str() {
            "dotted" => Some(vec![1.0, 1.0]),
            "dashed" | "dashSmallGap" => Some(vec![3.0, 1.0]),
            _ => None,
        },
    })
}

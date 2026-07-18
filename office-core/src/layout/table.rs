//! Table layout: resolve column widths, lay out cell content, and draw borders
//! and shading. Produces per-row [`RowLayout`]s that [`super::flow`] places and
//! page-breaks between (repeating header rows).
//!
//! Stored `tblGrid` widths remain authoritative for fixed or explicitly-sized
//! tables. Tables with Word's unqualified automatic layout use shaped
//! min/max-content widths instead.

use super::assemble::WordDoc;
use super::numbering_layout::ListCounters;
use super::paragraph::{
    InternalLinkRect, IntrinsicWidths, LineItem, NoteSequence, layout_paragraph_with_note_sequence,
    measure_intrinsic_widths,
};
use super::{FillRule, Item, PathCommand, Positioned, Stroke};
use crate::Result;
use crate::oxml::TblWidth;
use crate::oxml::wml::{BlockContent, Border, Cell, Jc, Row, RowHeight, Table, VAlign, VMerge};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use std::collections::HashMap;

/// Default cell side margin (108 twips = 5.4pt) when unset.
const DEFAULT_CELL_MARGIN_PT: f64 = 5.4;
/// Word's implicit side inset for an unqualified AutoFit table. Unlike the
/// `TableNormal` default used by stored-grid tables, a bare synthetic table
/// exported by Word places text 0.5pt inside the grid centerline.
const AUTOFIT_CELL_MARGIN_PT: f64 = 0.5;

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
pub(crate) struct TableLayout {
    /// Resolved painted table width in points, including both outer half-strokes.
    pub(crate) width_pt: f64,
    /// Outer half-stroke reserved before the first row of every page fragment.
    pub(crate) leading_pt: f64,
    /// Outer half-stroke reserved after the last row of every page fragment.
    pub(crate) trailing_pt: f64,
    /// Rows positioned relative to the table's left edge.
    pub(crate) rows: Vec<TableRowLayout>,
}

#[derive(Debug, Default)]
pub(crate) struct TableRowLayout {
    pub(crate) layout: RowLayout,
    pub(crate) bookmarks: Vec<(String, f64)>,
    pub(crate) note_refs: Vec<(crate::oxml::wml::NoteKind, i64)>,
    pub(crate) internal_links: Vec<InternalLinkRect>,
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
) -> Result<Vec<RowLayout>> {
    let options = TableLayoutOptions {
        rels,
        avail_width_pt,
        table_left,
        note_sequence: None,
    };
    let layout = layout_table_with_width(doc, warnings, table, &options, counters)?;
    let mut rows = layout
        .rows
        .into_iter()
        .map(|row| row.layout)
        .collect::<Vec<_>>();
    // Standalone callers (headers, footers, and text boxes) never paginate a
    // table row-by-row, so retain the complete-table reservation here. Body
    // flow uses `layout_table_with_width` and applies these values per fragment.
    if let Some(first) = rows.first_mut() {
        for item in &mut first.items {
            item.y_pt += layout.leading_pt;
        }
        first.height_pt += layout.leading_pt;
    }
    if let Some(last) = rows.last_mut() {
        last.height_pt += layout.trailing_pt;
    }
    Ok(rows)
}

#[derive(Clone, Copy)]
pub(crate) struct TableLayoutOptions<'a> {
    pub(crate) rels: &'a HashMap<String, String>,
    pub(crate) avail_width_pt: f64,
    pub(crate) table_left: f64,
    pub(crate) note_sequence: Option<&'a NoteSequence>,
}

/// Internal additive form used by floating-table flow to retain the resolved
/// grid width without changing the public Rust API.
pub(crate) fn layout_table_with_width(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    table: &Table,
    options: &TableLayoutOptions<'_>,
    counters: &mut ListCounters,
) -> Result<TableLayout> {
    let TableLayoutOptions {
        rels,
        avail_width_pt,
        table_left,
        note_sequence,
    } = *options;
    let cols = resolve_columns(doc, warnings, table, rels, avail_width_pt, note_sequence)?;
    let grid_width: f64 = cols.iter().sum();
    let visible_rows: Vec<&Row> = table.rows.iter().filter(|row| !row.props.hidden).collect();
    let (left_outer_half, right_outer_half) =
        vertical_outer_half_widths(doc, &visible_rows, table, cols.len());
    let painted_width = left_outer_half + grid_width + right_outer_half;
    let free_width = (avail_width_pt - painted_width).max(0.0);
    // `tblInd` positions an in-flow table. A floating table's frame is instead
    // positioned exclusively by `tblpPr`; applying both offsets shifts its
    // painted grid away from the wrap exclusion Word computes for that frame.
    let leading_cell_margin = table
        .props
        .cell_margins
        .left
        .unwrap_or(if should_autofit(table) {
            AUTOFIT_CELL_MARGIN_PT
        } else {
            DEFAULT_CELL_MARGIN_PT
        });
    let start_offset = if table.props.floating.is_some() {
        0.0
    } else {
        // A positive `tblInd` locates the leading cell content edge, so the
        // grid begins one leading margin earlier. Google Docs exports an
        // explicit negative `tblInd` to locate the grid edge itself; subtracting
        // the margin again would double that authored outdent. A zero indent
        // (Word's TableNormal default) depends on the document's
        // compatibility mode: Word 2013+ (mode 15) aligns the grid edge at
        // the paragraph margin, while legacy modes keep the historical
        // content-edge placement one cell margin to the left — both measured
        // against Word-exported ground truths (chinese/japanese vs complex).
        table.props.indent_pt.map_or(0.0, |indent| {
            if indent < 0.0 {
                indent
            } else if indent > 0.0 {
                indent - leading_cell_margin
            } else if doc.settings.compat_mode >= 15 {
                0.0
            } else {
                -leading_cell_margin
            }
        })
    };
    let alignment = table.props.alignment.unwrap_or(Jc::Start);
    let painted_left = table_left
        + match alignment {
            Jc::Center => free_width / 2.0,
            Jc::End => free_width,
            Jc::Start => start_offset,
            Jc::Both | Jc::Distribute => 0.0,
        };
    // Center/end/floating placement operates on the painted footprint. For a
    // start-aligned in-flow table, Word preserves the historical centerline
    // anchor when `tblInd` is absent; `complex` page 6 measures within 0.02pt
    // under that convention. An explicit negative `tblInd`, however, anchors
    // the painted outer edge: the page-41 probe for `gdocs-tables-longdoc`
    // measured the complete block +0.5pt from the former centerline anchor.
    let reserves_painted_left = table.props.floating.is_some()
        || matches!(alignment, Jc::Center | Jc::End)
        || (alignment == Jc::Start && table.props.indent_pt.is_some_and(|indent| indent < 0.0));
    let grid_left = painted_left
        + if reserves_painted_left {
            left_outer_half
        } else {
            0.0
        };
    let col_x = column_offsets(&cols, grid_left);
    let mut rows = Vec::new();

    let horizontal_boundaries =
        horizontal_boundary_plan(doc, &visible_rows, table, &cols, grid_left);
    let vertical_boundaries = vertical_boundary_plan(doc, &visible_rows, table, &cols, grid_left);

    let mut prepared_rows = Vec::with_capacity(visible_rows.len());
    for (row_index, row) in visible_rows.iter().copied().enumerate() {
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
            let merge_end_row = if cell.props.v_merge == Some(VMerge::Restart) {
                vertical_merge_end(&visible_rows, row_index, col, span)
            } else {
                row_index
            };
            let content = if is_continue {
                CellLayout::default()
            } else {
                layout_cell(
                    doc,
                    warnings,
                    &cell.content,
                    rels,
                    content_w,
                    counters,
                    note_sequence,
                )?
            };
            let border_top = horizontal_boundaries[row_index].half_width_for_span(col, col + span);
            let border_bottom =
                horizontal_boundaries[row_index + 1].half_width_for_span(col, col + span);
            let content_border_bottom =
                horizontal_boundaries[merge_end_row + 1].half_width_for_span(col, col + span);
            if !is_continue && merge_end_row == row_index {
                row_content_h =
                    row_content_h.max(content.height_pt + m_t + m_b + border_top + border_bottom);
            }

            cell_boxes.push(CellBox {
                x,
                width,
                m_l,
                m_r,
                m_t,
                m_b,
                border_top,
                content_border_bottom,
                is_continue,
                merge_end_row,
                v_align: cell.props.v_align,
                shading: cell.props.shading.map(Rgba::opaque),
                content,
                clip_content: matches!(row.props.height, Some(RowHeight::Exact(_))),
            });
            col += span;
        }

        row_content_h = row_content_h.max(
            (horizontal_boundaries[row_index].max_width_pt
                + horizontal_boundaries[row_index + 1].max_width_pt)
                / 2.0,
        );
        prepared_rows.push(PreparedRow {
            height_rule: row.props.height,
            is_header: row.props.header,
            content_height_pt: row_content_h,
            cells: cell_boxes,
        });
    }

    let mut row_heights = prepared_rows
        .iter()
        .map(|row| apply_row_height(row.height_rule, row.content_height_pt))
        .collect::<Vec<_>>();
    for (row_index, row) in prepared_rows.iter().enumerate() {
        for cell in &row.cells {
            if cell.is_continue || cell.merge_end_row == row_index {
                continue;
            }
            let required = cell.content.height_pt
                + cell.m_t
                + cell.m_b
                + cell.border_top
                + cell.content_border_bottom;
            let current: f64 = row_heights[row_index..=cell.merge_end_row].iter().sum();
            let deficit = (required - current).max(0.0);
            if deficit <= f64::EPSILON {
                continue;
            }
            let growable = (row_index..=cell.merge_end_row)
                .filter(|index| {
                    !matches!(prepared_rows[*index].height_rule, Some(RowHeight::Exact(_)))
                })
                .collect::<Vec<_>>();
            if growable.is_empty() {
                continue;
            }
            let share = deficit / growable.len() as f64;
            for index in growable {
                row_heights[index] += share;
            }
        }
    }

    for (row_index, prepared) in prepared_rows.iter().enumerate() {
        let height = row_heights[row_index];
        let mut items = Vec::new();
        let mut bookmarks = Vec::new();
        let mut note_refs = Vec::new();
        let mut internal_links = Vec::new();
        for cb in &prepared.cells {
            let content_span_height: f64 = row_heights[row_index..=cb.merge_end_row].iter().sum();
            emit_cell(cb, content_span_height, &mut items);
            place_cell_metadata(
                cb,
                content_span_height,
                &mut bookmarks,
                &mut note_refs,
                &mut internal_links,
            );
        }
        // Every row carries its already-collapsed top boundary. Adjacent rows
        // repaint the same opaque stroke in-place, while a row that starts a
        // new page still has the top edge Word paints for that fragment.
        emit_horizontal_boundary(&horizontal_boundaries[row_index], 0.0, &mut items);
        emit_horizontal_boundary(&horizontal_boundaries[row_index + 1], height, &mut items);
        emit_vertical_boundaries(&vertical_boundaries[row_index], height, &mut items);
        rows.push(TableRowLayout {
            layout: RowLayout {
                height_pt: height,
                is_header: prepared.is_header,
                items,
            },
            bookmarks,
            note_refs,
            internal_links,
        });
    }

    // Row heights run from horizontal border center to border center. Flow owns
    // these outer reservations because every page fragment needs them, not just
    // the first and last row of the complete table.
    let leading = horizontal_boundaries
        .first()
        .map_or(0.0, |boundary| boundary.max_width_pt / 2.0);
    let trailing = horizontal_boundaries
        .last()
        .map_or(0.0, |boundary| boundary.max_width_pt / 2.0);
    Ok(TableLayout {
        width_pt: painted_width,
        leading_pt: leading,
        trailing_pt: trailing,
        rows,
    })
}

struct PreparedRow {
    height_rule: Option<RowHeight>,
    is_header: bool,
    content_height_pt: f64,
    cells: Vec<CellBox>,
}

struct CellBox {
    x: f64,
    width: f64,
    m_l: f64,
    m_r: f64,
    m_t: f64,
    m_b: f64,
    border_top: f64,
    content_border_bottom: f64,
    is_continue: bool,
    merge_end_row: usize,
    v_align: VAlign,
    shading: Option<Rgba>,
    content: CellLayout,
    clip_content: bool,
}

#[derive(Debug, Default)]
struct CellLayout {
    items: Vec<LineItem>,
    height_pt: f64,
    bookmarks: Vec<(String, f64)>,
    note_refs: Vec<(crate::oxml::wml::NoteKind, i64)>,
    internal_links: Vec<InternalLinkRect>,
}

/// Emits a cell's shading, content, and borders into the row's item list.
fn emit_cell(cb: &CellBox, content_span_height: f64, items: &mut Vec<LineItem>) {
    if cb.is_continue {
        // The shared-edge planner owns every border, including the sides of a
        // vertical-merge continuation cell.
        return;
    }
    // Row coordinates run from collapsed-border centre to collapsed-border
    // centre. Keep the fill inside the two half-strokes; otherwise the next
    // row's fill is painted after this row's bottom boundary and hides half
    // (or more) of a thick conditional border.
    if let Some(fill) = cb.shading {
        let shade_top = cb.border_top.min(content_span_height);
        let shade_height = (content_span_height - shade_top - cb.content_border_bottom).max(0.0);
        items.push(LineItem {
            x_pt: cb.x,
            y_pt: shade_top,
            item: Item::Rect {
                w_pt: cb.width,
                h_pt: shade_height,
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
    let content_top = (cb.border_top + cb.m_t).min(content_span_height);
    let content_bottom = (content_span_height - cb.content_border_bottom - cb.m_b).max(content_top);
    let content_height = content_bottom - content_top;
    let free = (content_height - cb.content.height_pt).max(0.0);
    let dy = match cb.v_align {
        VAlign::Center => free / 2.0,
        VAlign::Bottom => free,
        VAlign::Top => 0.0,
    };
    let content_y = content_top + dy;
    if cb.clip_content {
        let mut clipped = Vec::new();
        for li in &cb.content.items {
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
        for li in &cb.content.items {
            items.push(LineItem {
                x_pt: cb.x + cb.m_l + li.x_pt,
                y_pt: content_y + li.y_pt,
                item: li.item.clone(),
            });
        }
    }
}

fn place_cell_metadata(
    cb: &CellBox,
    content_span_height: f64,
    bookmarks: &mut Vec<(String, f64)>,
    note_refs: &mut Vec<(crate::oxml::wml::NoteKind, i64)>,
    internal_links: &mut Vec<InternalLinkRect>,
) {
    if cb.is_continue {
        return;
    }
    let content_left = cb.m_l.min(cb.width);
    let content_right = (cb.width - cb.m_r).max(content_left);
    let content_top = (cb.border_top + cb.m_t).min(content_span_height);
    let content_bottom = (content_span_height - cb.content_border_bottom - cb.m_b).max(content_top);
    let free = (content_bottom - content_top - cb.content.height_pt).max(0.0);
    let content_y = content_top
        + match cb.v_align {
            VAlign::Center => free / 2.0,
            VAlign::Bottom => free,
            VAlign::Top => 0.0,
        };
    for note_ref in &cb.content.note_refs {
        if !note_refs.contains(note_ref) {
            note_refs.push(*note_ref);
        }
    }
    for (name, y_pt) in &cb.content.bookmarks {
        let y_pt = content_y + y_pt;
        if !cb.clip_content || (content_top..content_bottom).contains(&y_pt) {
            bookmarks.push((name.clone(), y_pt));
        }
    }
    for link in &cb.content.internal_links {
        let left = cb.m_l + link.x_pt;
        let top = content_y + link.y_pt;
        let right = left + link.w_pt;
        let bottom = top + link.h_pt;
        let (left, top, right, bottom) = if cb.clip_content {
            (
                left.max(content_left),
                top.max(content_top),
                right.min(content_right),
                bottom.min(content_bottom),
            )
        } else {
            (left, top, right, bottom)
        };
        if right > left && bottom > top {
            internal_links.push(InternalLinkRect {
                x_pt: cb.x + left,
                y_pt: top,
                w_pt: right - left,
                h_pt: bottom - top,
                target: link.target.clone(),
            });
        }
    }
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

/// Lays out a cell's block content (paragraphs; nested tables recurse) into a
/// box of `width`, returning items (top-left origin) and total height.
fn layout_cell(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    blocks: &[BlockContent],
    rels: &HashMap<String, String>,
    width: f64,
    counters: &mut ListCounters,
    note_sequence: Option<&NoteSequence>,
) -> Result<CellLayout> {
    let mut items = Vec::new();
    let mut y = 0.0_f64;
    let mut bookmarks = Vec::new();
    let mut note_refs = Vec::new();
    let mut internal_links = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
        match block {
            BlockContent::Paragraph(p) => {
                let is_structural_terminal = block_index + 1 == blocks.len()
                    && block_index > 0
                    && matches!(blocks[block_index - 1], BlockContent::Table(_))
                    && !p
                        .runs
                        .iter()
                        .any(crate::oxml::wml::inline_has_result_content);
                let label = if is_structural_terminal {
                    None
                } else {
                    counter_label(doc, warnings, p, counters)?
                };
                let layout = layout_paragraph_with_note_sequence(
                    doc,
                    warnings,
                    p,
                    rels,
                    width,
                    label,
                    None,
                    note_sequence,
                )?;
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
                for note_ref in &layout.note_refs {
                    if !note_refs.contains(note_ref) {
                        note_refs.push(*note_ref);
                    }
                }
                if is_structural_terminal {
                    for line in &layout.lines {
                        bookmarks.extend(line.bookmarks.iter().cloned().map(|name| (name, y)));
                        internal_links.extend(line.internal_links.iter().cloned().map(
                            |mut link| {
                                link.y_pt += y;
                                link
                            },
                        ));
                    }
                    continue;
                }
                y += layout.space_before_pt;
                for line in &layout.lines {
                    bookmarks.extend(line.bookmarks.iter().cloned().map(|name| (name, y)));
                    internal_links.extend(line.internal_links.iter().cloned().map(|mut link| {
                        link.y_pt += y;
                        link
                    }));
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
                let nested_options = TableLayoutOptions {
                    rels,
                    avail_width_pt: width,
                    table_left: 0.0,
                    note_sequence,
                };
                let nested = layout_table_with_width(doc, warnings, t, &nested_options, counters)?;
                y += nested.leading_pt;
                for row in nested.rows {
                    for note_ref in row.note_refs {
                        if !note_refs.contains(&note_ref) {
                            note_refs.push(note_ref);
                        }
                    }
                    bookmarks.extend(
                        row.bookmarks
                            .into_iter()
                            .map(|(name, y_pt)| (name, y + y_pt)),
                    );
                    internal_links.extend(row.internal_links.into_iter().map(|mut link| {
                        link.y_pt += y;
                        link
                    }));
                    for li in &row.layout.items {
                        items.push(LineItem {
                            x_pt: li.x_pt,
                            y_pt: y + li.y_pt,
                            item: li.item.clone(),
                        });
                    }
                    y += row.layout.height_pt;
                }
                y += nested.trailing_pt;
            }
        }
    }
    Ok(CellLayout {
        items,
        height_pt: y,
        bookmarks,
        note_refs,
        internal_links,
    })
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

/// Resolves column widths. Word AutoFit applies only to an automatic table
/// whose cells carry no explicit absolute/percentage widths; every other table
/// retains the calibrated stored-grid path.
fn resolve_columns(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    table: &Table,
    rels: &HashMap<String, String>,
    avail: f64,
    note_sequence: Option<&NoteSequence>,
) -> Result<Vec<f64>> {
    if should_autofit(table) {
        // Word runs content AutoFit and writes the result back into
        // `tblGrid`, so in a Word-authored file (rsids present) a grid that
        // spans every column IS the rendered layout. Other producers
        // (converters, generators) store speculative grids, so intrinsic
        // measurement stays authoritative for them.
        let columns = table_column_count(table);
        if doc.settings.word_authored
            && columns > 0
            && table.grid.len() == columns
            && table.grid.iter().all(|w| *w > 0.0)
        {
            return Ok(resolve_stored_columns(table, avail));
        }
        return autofit_columns(doc, warnings, table, rels, avail, note_sequence);
    }
    Ok(resolve_stored_columns(table, avail))
}

fn table_column_count(table: &Table) -> usize {
    table
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
        .max(1)
}

fn should_autofit(table: &Table) -> bool {
    !table.props.layout_fixed
        && matches!(
            table.props.width,
            None | Some(TblWidth::Auto | TblWidth::Nil)
        )
        && !table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .any(|cell| matches!(cell.props.width, Some(TblWidth::Pt(_) | TblWidth::Pct(_))))
}

#[derive(Debug, Clone, Copy)]
struct SpanningIntrinsic {
    start: usize,
    span: usize,
    widths: IntrinsicWidths,
}

fn autofit_columns(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    table: &Table,
    rels: &HashMap<String, String>,
    avail: f64,
    note_sequence: Option<&NoteSequence>,
) -> Result<Vec<f64>> {
    let (column_min, column_max) =
        autofit_intrinsic_columns(doc, warnings, table, rels, note_sequence)?;
    let min_total: f64 = column_min.iter().sum();
    let max_total: f64 = column_max.iter().sum();
    let available = avail.max(1.0);
    let target = if min_total > available {
        min_total
    } else {
        max_total.min(available)
    };
    if (target - max_total).abs() <= 0.01 {
        return Ok(column_max);
    }

    let remaining = (target - min_total).max(0.0);
    let flexibility = column_min
        .iter()
        .zip(&column_max)
        .map(|(minimum, maximum)| (maximum - minimum).max(0.0))
        .collect::<Vec<_>>();
    let total_flexibility: f64 = flexibility.iter().sum();
    if total_flexibility <= f64::EPSILON {
        return Ok(column_min);
    }
    Ok(column_min
        .into_iter()
        .zip(flexibility)
        .map(|(minimum, flex)| minimum + remaining * flex / total_flexibility)
        .collect())
}

fn autofit_intrinsic_columns(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    table: &Table,
    rels: &HashMap<String, String>,
    note_sequence: Option<&NoteSequence>,
) -> Result<(Vec<f64>, Vec<f64>)> {
    let column_count = table_column_count(table);
    let mut requirements = Vec::new();
    for row in table.rows.iter().filter(|row| !row.props.hidden) {
        let mut column = row.props.grid_before as usize;
        for cell in &row.cells {
            let span = (cell.props.grid_span.max(1) as usize)
                .min(column_count.saturating_sub(column).max(1));
            if cell.props.v_merge != Some(VMerge::Continue) {
                let content =
                    measure_cell_intrinsics(doc, warnings, &cell.content, rels, note_sequence)?;
                let (margin_left, margin_right, _, _) = cell_margins(cell, table);
                requirements.push(SpanningIntrinsic {
                    start: column,
                    span,
                    widths: IntrinsicWidths {
                        min_content_pt: content.min_content_pt + margin_left + margin_right,
                        max_content_pt: content.max_content_pt + margin_left + margin_right,
                    },
                });
            }
            column += span;
        }
    }
    requirements.sort_by_key(|requirement| requirement.span);

    let mut column_min = vec![1.0; column_count];
    let mut column_max = vec![1.0; column_count];
    for requirement in requirements {
        apply_required_width(
            &mut column_min,
            requirement.start,
            requirement.span,
            requirement.widths.min_content_pt,
        );
        apply_required_width(
            &mut column_max,
            requirement.start,
            requirement.span,
            requirement.widths.max_content_pt,
        );
        for index in requirement.start..(requirement.start + requirement.span).min(column_count) {
            column_max[index] = column_max[index].max(column_min[index]);
        }
    }
    Ok((column_min, column_max))
}

fn measure_cell_intrinsics(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    blocks: &[BlockContent],
    rels: &HashMap<String, String>,
    note_sequence: Option<&NoteSequence>,
) -> Result<IntrinsicWidths> {
    let mut widths = IntrinsicWidths::default();
    for block in blocks {
        let block_widths = match block {
            BlockContent::Paragraph(paragraph) => {
                measure_intrinsic_widths(doc, warnings, paragraph, rels, note_sequence)?
            }
            BlockContent::Table(table) => {
                measure_table_intrinsics(doc, warnings, table, rels, note_sequence)?
            }
        };
        widths.min_content_pt = widths.min_content_pt.max(block_widths.min_content_pt);
        widths.max_content_pt = widths.max_content_pt.max(block_widths.max_content_pt);
    }
    Ok(widths)
}

fn measure_table_intrinsics(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    table: &Table,
    rels: &HashMap<String, String>,
    note_sequence: Option<&NoteSequence>,
) -> Result<IntrinsicWidths> {
    if should_autofit(table) {
        let (minimums, maximums) =
            autofit_intrinsic_columns(doc, warnings, table, rels, note_sequence)?;
        return Ok(IntrinsicWidths {
            min_content_pt: minimums.iter().sum(),
            max_content_pt: maximums.iter().sum(),
        });
    }
    let columns = resolve_stored_columns(table, f64::MAX / 4.0);
    let total: f64 = columns.iter().sum();
    Ok(IntrinsicWidths {
        min_content_pt: total,
        max_content_pt: total,
    })
}

fn apply_required_width(columns: &mut [f64], start: usize, span: usize, required: f64) {
    if span == 0 || start >= columns.len() || !required.is_finite() {
        return;
    }
    let end = (start + span).min(columns.len());
    let current: f64 = columns[start..end].iter().sum();
    let deficit = (required - current).max(0.0);
    if deficit <= f64::EPSILON {
        return;
    }
    let share = deficit / (end - start) as f64;
    for column in &mut columns[start..end] {
        *column += share;
    }
}

/// Resolves the calibrated non-AutoFit path from the stored grid and authored
/// widths, scaled to the available width.
fn resolve_stored_columns(table: &Table, avail: f64) -> Vec<f64> {
    let column_count = table_column_count(table);
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
    let default_side = if should_autofit(table) {
        AUTOFIT_CELL_MARGIN_PT
    } else {
        DEFAULT_CELL_MARGIN_PT
    };
    let l = m.left.or(d.left).unwrap_or(default_side);
    let r = m.right.or(d.right).unwrap_or(default_side);
    let t = m.top.or(d.top).unwrap_or(0.0);
    let b = m.bottom.or(d.bottom).unwrap_or(0.0);
    (l, r, t, b)
}

fn vertical_merge_end(rows: &[&Row], start_row: usize, start_column: usize, span: usize) -> usize {
    let mut end_row = start_row;
    for (row_index, row) in rows.iter().enumerate().skip(start_row + 1) {
        let mut column = row.props.grid_before as usize;
        let continuation = row.cells.iter().find_map(|cell| {
            let cell_span = cell.props.grid_span.max(1) as usize;
            let matches = column == start_column
                && cell_span == span
                && cell.props.v_merge == Some(VMerge::Continue);
            column += cell_span;
            matches.then_some(())
        });
        if continuation.is_none() {
            break;
        }
        end_row = row_index;
    }
    end_row
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

#[derive(Debug, Clone)]
struct VerticalBoundarySegment {
    x_pt: f64,
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

fn vertical_outer_half_widths(
    doc: &WordDoc,
    rows: &[&Row],
    table: &Table,
    column_count: usize,
) -> (f64, f64) {
    let width_at = |boundary: usize| {
        if rows.is_empty() {
            let table_border = if boundary == 0 {
                table.props.borders.left.as_ref()
            } else {
                table.props.borders.right.as_ref()
            };
            return stroke_from_state(&effective_border_state(None, table_border), doc)
                .map_or(0.0, |stroke| stroke.width_pt);
        }
        rows.iter()
            .filter_map(|row| {
                stroke_from_state(
                    &vertical_boundary_state(row, table, boundary, column_count, doc),
                    doc,
                )
            })
            .map(|stroke| stroke.width_pt)
            .fold(0.0_f64, f64::max)
    };
    (width_at(0) / 2.0, width_at(column_count) / 2.0)
}

fn vertical_boundary_plan(
    doc: &WordDoc,
    rows: &[&Row],
    table: &Table,
    cols: &[f64],
    grid_left: f64,
) -> Vec<Vec<VerticalBoundarySegment>> {
    let mut edges = Vec::with_capacity(cols.len() + 1);
    let mut x = grid_left;
    edges.push(x);
    for width in cols {
        x += *width;
        edges.push(x);
    }
    rows.iter()
        .map(|row| {
            (0..=cols.len())
                .filter_map(|boundary| {
                    let state = vertical_boundary_state(row, table, boundary, cols.len(), doc);
                    stroke_from_state(&state, doc).map(|stroke| VerticalBoundarySegment {
                        x_pt: edges[boundary],
                        stroke,
                    })
                })
                .collect()
        })
        .collect()
}

fn vertical_boundary_state(
    row: &Row,
    table: &Table,
    boundary: usize,
    column_count: usize,
    doc: &WordDoc,
) -> BorderState {
    if boundary == 0 {
        return vertical_edge_state(row_cell_placement(row, 0), table, true, true);
    }
    if boundary == column_count {
        return vertical_edge_state(
            row_cell_placement(row, column_count.saturating_sub(1)),
            table,
            false,
            true,
        );
    }
    let left = row_cell_placement(row, boundary - 1);
    let right = row_cell_placement(row, boundary);
    if left
        .zip(right)
        .is_some_and(|(left, right)| left.start == right.start && left.end == right.end)
    {
        return BorderState::Suppressed;
    }
    choose_boundary_border(
        vertical_edge_state(left, table, false, false),
        vertical_edge_state(right, table, true, false),
        doc,
    )
}

fn vertical_edge_state(
    placement: Option<CellPlacement<'_>>,
    table: &Table,
    left: bool,
    outer: bool,
) -> BorderState {
    let cell_border = placement.and_then(|placement| {
        if left {
            placement.cell.props.borders.left.as_ref()
        } else {
            placement.cell.props.borders.right.as_ref()
        }
    });
    let table_border = if outer {
        if left {
            table.props.borders.left.as_ref()
        } else {
            table.props.borders.right.as_ref()
        }
    } else {
        table.props.borders.inside_v.as_ref()
    };
    effective_border_state(cell_border, table_border)
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

fn emit_vertical_boundaries(
    boundaries: &[VerticalBoundarySegment],
    height_pt: f64,
    items: &mut Vec<LineItem>,
) {
    for boundary in boundaries {
        items.push(LineItem {
            x_pt: boundary.x_pt,
            y_pt: 0.0,
            item: Item::Line {
                dx_pt: 0.0,
                dy_pt: height_pt,
                stroke: boundary.stroke.clone(),
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

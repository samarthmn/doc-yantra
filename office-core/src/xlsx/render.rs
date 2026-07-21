//! XLSX print rendering: paginates each sheet's used range into Letter pages and
//! renders cell fills, gridlines, and number-formatted text.
//!
//! v1 targets Excel's default "Save As PDF" for a single-scale sheet (down-then-
//! over pagination, gridlines on). Fit-to-page scaling and print-area/title-row
//! refinements are calibration follow-ups.

use super::numfmt::{self, Formatted};
use super::paginate::{DrawingAnchor, DrawingMarker, XlsxDoc, XlsxDrawing};
use super::widths::{default_width_pt, width_chars_to_pt};
use crate::fonts::db::{Face, FaceId};
use crate::fonts::matching;
use crate::fonts::metrics::FaceMetrics;
use crate::layout::{GlyphRun, Item, LaidOutDocument, Page, Positioned, Stroke};
use crate::oxml::pml::{PresetGeometry, ShapeProps, Xfrm};
use crate::oxml::sml::{
    BorderEdge, Cell, CellStyle, CellValue, ConditionalKind, ConditionalValue, MergeRange,
    Worksheet,
};
use crate::oxml::{Rgb, Rgba};
use crate::pptx::geometry::{Contour, evaluate_shape, resolve_colorspec};
use crate::profile::Warning;
use crate::resource_limits::{PageBudget, try_reserve};
use crate::text::shape;
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

const PAGE_W: f64 = 595.28;
const PAGE_H: f64 = 841.89;
const GRID: Rgba = Rgba {
    r: 190,
    g: 190,
    b: 190,
    a: 1.0,
};

#[derive(Clone, Copy)]
struct PageGeometry {
    width: f64,
    height: f64,
    left: f64,
    top: f64,
    content_w: f64,
    content_h: f64,
}

fn page_geometry(ws: &Worksheet) -> PageGeometry {
    // Excel/ECMA paper IDs: Letter=1, Legal=5, A3=8, A4=9. A4 is the
    // deterministic no-pageSetup default used by this engine.
    let (mut width, mut height) = match ws.page_setup.paper_size {
        1 => (612.0, 792.0),
        5 => (612.0, 1008.0),
        8 => (841.89, 1190.55),
        _ => (595.28, 841.89),
    };
    if ws.page_setup.landscape {
        std::mem::swap(&mut width, &mut height);
    }
    let margins = ws.page_margins;
    PageGeometry {
        width,
        height,
        left: margins.left,
        top: margins.top,
        content_w: (width - margins.left - margins.right).max(1.0),
        content_h: (height - margins.top - margins.bottom).max(1.0),
    }
}

/// Renders all sheets into a laid-out document.
pub fn render_xlsx(doc: &XlsxDoc) -> Result<(LaidOutDocument, Vec<Warning>)> {
    let mut page_budget = PageBudget::production();
    render_xlsx_with_page_budget(doc, &mut page_budget)
}

fn render_xlsx_with_page_budget(
    doc: &XlsxDoc,
    page_budget: &mut PageBudget,
) -> Result<(LaidOutDocument, Vec<Warning>)> {
    let mut warnings = Vec::new();
    let mut pages = Vec::new();
    for sheet in &doc.sheets {
        if let Some(reason) = sheet.drawing_reasons.first() {
            return Err(OfficeError::unsupported(
                reason.code,
                format!("sheet {}: {}", sheet.name, reason.detail),
            ));
        }
        render_sheet(
            doc,
            &sheet.name,
            &sheet.sheet,
            &sheet.drawings,
            &mut pages,
            &mut warnings,
            page_budget,
        )?;
    }
    if pages.is_empty() {
        page_budget.consume("XLSX empty fallback page")?;
        pages.push(Page {
            width_pt: PAGE_W,
            height_pt: PAGE_H,
            items: Vec::new(),
        });
    }
    let laid = LaidOutDocument {
        pages,
        images: doc.images.clone(),
        outline: Vec::new(),
        dates: doc.dates.clone(),
        #[cfg(feature = "fidelity-diagnostics")]
        diagnostics: crate::diagnostics::LayoutTrace::default(),
    };
    Ok((laid, warnings))
}

fn render_sheet(
    doc: &XlsxDoc,
    sheet_name: &str,
    ws: &Worksheet,
    drawings: &[XlsxDrawing],
    pages: &mut Vec<Page>,
    warnings: &mut Vec<Warning>,
    page_budget: &mut PageBudget,
) -> Result<()> {
    let first_page = pages.len();
    let geometry = page_geometry(ws);
    let (mut max_row, mut max_col) = ws.dim;
    for drawing in drawings {
        if let Some((col, row)) = drawing_last_marker(drawing) {
            max_col = max_col.max(col);
            max_row = max_row.max(row);
        }
    }
    let (mut min_row, mut min_col) = (0, 0);
    if let Some(area) = ws.print_area {
        min_row = area.first_row;
        min_col = area.first_col;
        max_row = max_row.min(area.last_row);
        max_col = max_col.min(area.last_col);
    }
    if min_row > max_row || min_col > max_col {
        return Ok(());
    }
    if ws.cells.is_empty() && drawings.is_empty() {
        return Ok(());
    }
    let col_w = column_widths(ws, max_col)?;

    // Cell lookup.
    let cells: HashMap<(u32, u32), &Cell> = ws.cells.iter().map(|c| ((c.row, c.col), c)).collect();
    let row_heights = effective_row_heights(doc, ws, &cells, &col_w, max_row)?;
    let total_width: f64 = col_w[min_col as usize..=max_col as usize].iter().sum();
    let total_height: f64 = row_heights[min_row as usize..=max_row as usize]
        .iter()
        .enumerate()
        .filter(|(offset, _)| !ws.hidden_rows.contains(&(min_row + *offset as u32)))
        .map(|(_, height)| *height)
        .sum();
    let fit_width = ws
        .page_setup
        .fit_to_width
        .filter(|pages| *pages > 0)
        .map(|pages| f64::from(pages) * geometry.content_w / total_width.max(1.0));
    let fit_height = ws
        .page_setup
        .fit_to_height
        .filter(|pages| *pages > 0)
        .map(|pages| f64::from(pages) * geometry.content_h / total_height.max(1.0));
    let scale = match (fit_width, fit_height) {
        (Some(width), Some(height)) => width.min(height),
        (Some(width), None) => width,
        (None, Some(height)) => height,
        (None, None) => ws.page_setup.scale.unwrap_or(1.0),
    }
    .clamp(0.1, 4.0);
    let content_w = geometry.content_w / scale;
    let content_h = geometry.content_h / scale;

    // Column bands (each fits content width).
    let bands = split_bands_range(
        &col_w,
        content_w,
        min_col,
        max_col,
        &ws.col_breaks,
        ws.print_title_cols,
    );
    for band in &bands {
        // Row pages within this band.
        let mut r = min_row;
        while r <= max_row {
            page_budget.consume("XLSX worksheet page")?;
            let page_row_start = r;
            let mut page = Page {
                width_pt: geometry.width,
                height_pt: geometry.height,
                items: Vec::new(),
            };
            let mut y = geometry.top;
            let mut data_rows_on_page = 0;
            if r != min_row
                && let Some((first_title, last_title)) = ws.print_title_rows
            {
                for title_row in first_title..=last_title {
                    if title_row > max_row || ws.hidden_rows.contains(&title_row) {
                        continue;
                    }
                    let title_h = row_heights[title_row as usize];
                    if y + title_h > geometry.top + content_h {
                        break;
                    }
                    render_paginated_row(
                        doc,
                        ws,
                        &cells,
                        band,
                        &col_w,
                        &row_heights,
                        title_row,
                        geometry.left,
                        y,
                        title_h,
                        &mut page,
                        warnings,
                    )?;
                    y += title_h;
                }
            }
            while r <= max_row {
                if ws.hidden_rows.contains(&r) {
                    r += 1;
                    continue;
                }
                if data_rows_on_page > 0 && ws.row_breaks.contains(&r) {
                    break;
                }
                let h = row_heights[r as usize];
                if y + h > geometry.top + content_h && data_rows_on_page > 0 {
                    break;
                }
                render_paginated_row(
                    doc,
                    ws,
                    &cells,
                    band,
                    &col_w,
                    &row_heights,
                    r,
                    geometry.left,
                    y,
                    h,
                    &mut page,
                    warnings,
                )?;
                y += h;
                data_rows_on_page += 1;
                r += 1;
            }
            let page_row_end = r.saturating_sub(1).max(page_row_start);
            render_drawings(
                doc,
                drawings,
                band,
                page_row_start,
                page_row_end,
                min_col,
                min_row,
                &col_w,
                &row_heights,
                geometry,
                content_w,
                content_h,
                &mut page,
            )?;
            if !page.items.is_empty() {
                if (scale - 1.0).abs() > f64::EPSILON {
                    for positioned in &mut page.items {
                        positioned.item = Item::Transform {
                            matrix: [
                                scale,
                                0.0,
                                0.0,
                                scale,
                                geometry.left * (1.0 - scale),
                                geometry.top * (1.0 - scale),
                            ],
                            item: Box::new(positioned.item.clone()),
                        };
                    }
                }
                pages.push(page);
            } else {
                break;
            }
        }
    }
    let total = pages.len().saturating_sub(first_page);
    for (offset, page) in pages[first_page..].iter_mut().enumerate() {
        render_header_footer(doc, ws, sheet_name, page, offset + 1, total)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_paginated_row(
    doc: &XlsxDoc,
    ws: &Worksheet,
    cells: &HashMap<(u32, u32), &Cell>,
    band: &(u32, u32),
    col_w: &[f64],
    row_heights: &[f64],
    row: u32,
    x0: f64,
    y: f64,
    h: f64,
    page: &mut Page,
    warnings: &mut Vec<Warning>,
) -> Result<()> {
    let mut body_x = x0;
    if let Some((title_first, title_last)) = ws.print_title_cols
        && band.0 > title_last
    {
        let title_band = (
            title_first,
            title_last.min(col_w.len().saturating_sub(1) as u32),
        );
        if title_band.0 <= title_band.1 {
            render_row(
                doc,
                ws,
                cells,
                &title_band,
                col_w,
                row_heights,
                row,
                x0,
                y,
                h,
                page,
                warnings,
            )?;
            body_x += col_w[title_band.0 as usize..=title_band.1 as usize]
                .iter()
                .sum::<f64>();
        }
    }
    render_row(
        doc,
        ws,
        cells,
        band,
        col_w,
        row_heights,
        row,
        body_x,
        y,
        h,
        page,
        warnings,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_drawings(
    doc: &XlsxDoc,
    drawings: &[XlsxDrawing],
    band: &(u32, u32),
    page_row_start: u32,
    page_row_end: u32,
    min_col: u32,
    min_row: u32,
    col_widths: &[f64],
    row_heights: &[f64],
    geometry: PageGeometry,
    content_w: f64,
    content_h: f64,
    page: &mut Page,
) -> Result<()> {
    for drawing in drawings {
        let anchor = match drawing {
            XlsxDrawing::Picture { anchor, .. }
            | XlsxDrawing::Shape { anchor, .. }
            | XlsxDrawing::Chart { anchor, .. } => *anchor,
        };
        let Some((x, y, width, height)) = drawing_page_rect(
            anchor,
            band,
            page_row_start,
            page_row_end,
            min_col,
            min_row,
            col_widths,
            row_heights,
            geometry,
        ) else {
            continue;
        };
        if x < geometry.left
            || y < geometry.top
            || x + width > geometry.left + content_w + 0.01
            || y + height > geometry.top + content_h + 0.01
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::AnchoredDrawing,
                "xlsx drawing crosses a printed page boundary",
            ));
        }
        match drawing {
            XlsxDrawing::Picture { image_id, .. } => page.items.push(Positioned {
                x_pt: x,
                y_pt: y,
                item: Item::Image {
                    w_pt: width,
                    h_pt: height,
                    image_id: *image_id,
                },
            }),
            XlsxDrawing::Shape {
                preset, fill, line, ..
            } => {
                let props = ShapeProps {
                    preset: Some(PresetGeometry {
                        name: preset.clone(),
                        adjustments: Vec::new(),
                    }),
                    ..ShapeProps::default()
                };
                let contours = evaluate_shape(
                    &props,
                    Xfrm {
                        w_pt: width,
                        h_pt: height,
                        present: true,
                        ..Xfrm::default()
                    },
                )?;
                if let Some(fill) = fill {
                    fill_shape_contours(&contours, x, y, resolve_colorspec(&doc.theme, fill), page);
                }
                if let Some((line, width_pt)) = line {
                    stroke_shape_contours(
                        &contours,
                        x,
                        y,
                        Stroke {
                            width_pt: *width_pt,
                            color: resolve_colorspec(&doc.theme, line),
                            dash: None,
                        },
                        page,
                    );
                }
            }
            XlsxDrawing::Chart { chart, .. } => {
                for child in chart.render(&doc.fonts, &doc.theme, width, height)? {
                    page.items.push(Positioned {
                        x_pt: x + child.x_pt,
                        y_pt: y + child.y_pt,
                        item: child.item,
                    });
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn drawing_page_rect(
    anchor: DrawingAnchor,
    band: &(u32, u32),
    page_row_start: u32,
    page_row_end: u32,
    min_col: u32,
    min_row: u32,
    col_widths: &[f64],
    row_heights: &[f64],
    geometry: PageGeometry,
) -> Option<(f64, f64, f64, f64)> {
    let (sheet_x, sheet_y, width, height) = anchor_rect(anchor, col_widths, row_heights);
    let Some((start_col, start_row)) = anchor_start(anchor) else {
        return (band.0 == min_col && page_row_start == min_row).then_some((
            geometry.left + sheet_x,
            geometry.top + sheet_y,
            width,
            height,
        ));
    };
    if !(band.0..=band.1).contains(&start_col)
        || !(page_row_start..=page_row_end).contains(&start_row)
    {
        return None;
    }
    Some((
        geometry.left + sheet_x - cumulative_columns(col_widths, band.0),
        geometry.top + sheet_y - cumulative_rows(row_heights, page_row_start),
        width,
        height,
    ))
}

fn fill_shape_contours(contours: &[Contour], x: f64, y: f64, color: Rgba, page: &mut Page) {
    for contour in contours
        .iter()
        .filter(|contour| contour.closed && contour.points.len() >= 3)
    {
        let min_y = contour
            .points
            .iter()
            .map(|point| point.y)
            .fold(f64::INFINITY, f64::min);
        let max_y = contour
            .points
            .iter()
            .map(|point| point.y)
            .fold(f64::NEG_INFINITY, f64::max);
        let mut scan_y = min_y;
        while scan_y < max_y {
            let sample_y = scan_y + 0.5;
            let mut intersections = Vec::new();
            for index in 0..contour.points.len() {
                let a = contour.points[index];
                let b = contour.points[(index + 1) % contour.points.len()];
                if (a.y <= sample_y && b.y > sample_y) || (b.y <= sample_y && a.y > sample_y) {
                    intersections.push(a.x + (sample_y - a.y) * (b.x - a.x) / (b.y - a.y));
                }
            }
            intersections.sort_by(f64::total_cmp);
            for pair in intersections.chunks_exact(2) {
                if pair[1] > pair[0] {
                    page.items.push(Positioned {
                        x_pt: x + pair[0],
                        y_pt: y + scan_y,
                        item: Item::Rect {
                            w_pt: pair[1] - pair[0] + 0.02,
                            h_pt: 1.02,
                            fill: Some(color),
                            stroke: None,
                        },
                    });
                }
            }
            scan_y += 1.0;
        }
    }
}

fn stroke_shape_contours(contours: &[Contour], x: f64, y: f64, stroke: Stroke, page: &mut Page) {
    for contour in contours {
        for pair in contour.points.windows(2) {
            page.items.push(Positioned {
                x_pt: x + pair[0].x,
                y_pt: y + pair[0].y,
                item: Item::Line {
                    dx_pt: pair[1].x - pair[0].x,
                    dy_pt: pair[1].y - pair[0].y,
                    stroke: stroke.clone(),
                },
            });
        }
        if contour.closed && contour.points.len() > 2 {
            let first = contour.points[0];
            let last = contour.points[contour.points.len() - 1];
            page.items.push(Positioned {
                x_pt: x + last.x,
                y_pt: y + last.y,
                item: Item::Line {
                    dx_pt: first.x - last.x,
                    dy_pt: first.y - last.y,
                    stroke: stroke.clone(),
                },
            });
        }
    }
}

fn anchor_start(anchor: DrawingAnchor) -> Option<(u32, u32)> {
    match anchor {
        DrawingAnchor::TwoCell { from, .. } | DrawingAnchor::OneCell { from, .. } => {
            Some((from.col, from.row))
        }
        DrawingAnchor::Absolute { .. } => None,
    }
}

fn drawing_last_marker(drawing: &XlsxDrawing) -> Option<(u32, u32)> {
    match drawing {
        XlsxDrawing::Picture { anchor, .. }
        | XlsxDrawing::Shape { anchor, .. }
        | XlsxDrawing::Chart { anchor, .. } => match anchor {
            DrawingAnchor::TwoCell { to, .. } => Some((to.col, to.row)),
            DrawingAnchor::OneCell { from, .. } => Some((from.col, from.row)),
            DrawingAnchor::Absolute { .. } => None,
        },
    }
}

fn anchor_rect(
    anchor: DrawingAnchor,
    col_widths: &[f64],
    row_heights: &[f64],
) -> (f64, f64, f64, f64) {
    match anchor {
        DrawingAnchor::TwoCell { from, to } => {
            let (x, y) = marker_position(from, col_widths, row_heights);
            let (right, bottom) = marker_position(to, col_widths, row_heights);
            (x, y, (right - x).max(0.1), (bottom - y).max(0.1))
        }
        DrawingAnchor::OneCell {
            from,
            width_pt,
            height_pt,
        } => {
            let (x, y) = marker_position(from, col_widths, row_heights);
            (x, y, width_pt.max(0.1), height_pt.max(0.1))
        }
        DrawingAnchor::Absolute {
            x_pt,
            y_pt,
            width_pt,
            height_pt,
        } => (x_pt, y_pt, width_pt.max(0.1), height_pt.max(0.1)),
    }
}

fn marker_position(marker: DrawingMarker, col_widths: &[f64], row_heights: &[f64]) -> (f64, f64) {
    (
        cumulative_columns(col_widths, marker.col) + marker.col_off_pt,
        cumulative_rows(row_heights, marker.row) + marker.row_off_pt,
    )
}

fn cumulative_columns(widths: &[f64], before: u32) -> f64 {
    widths.iter().take(before as usize).sum()
}

fn cumulative_rows(heights: &[f64], before: u32) -> f64 {
    heights.iter().take(before as usize).sum()
}

/// Expands Excel's printable header/footer subset. We intentionally use the
/// fixed `2000-01-01 00:00` values for `&D`/`&T`, independent of conversion
/// time, so output stays reproducible. Unknown `&x` codes follow Excel's
/// visible fallback and render as `x` (the ampersand is consumed).
fn render_header_footer(
    doc: &XlsxDoc,
    ws: &Worksheet,
    sheet_name: &str,
    page: &mut Page,
    page_number: usize,
    page_count: usize,
) -> Result<()> {
    let (header, footer) = if page_number == 1 && ws.header_footer.different_first {
        (
            ws.header_footer
                .first_header
                .as_deref()
                .or(ws.header_footer.odd_header.as_deref()),
            ws.header_footer
                .first_footer
                .as_deref()
                .or(ws.header_footer.odd_footer.as_deref()),
        )
    } else if page_number.is_multiple_of(2) && ws.header_footer.different_odd_even {
        (
            ws.header_footer
                .even_header
                .as_deref()
                .or(ws.header_footer.odd_header.as_deref()),
            ws.header_footer
                .even_footer
                .as_deref()
                .or(ws.header_footer.odd_footer.as_deref()),
        )
    } else {
        (
            ws.header_footer.odd_header.as_deref(),
            ws.header_footer.odd_footer.as_deref(),
        )
    };
    let geometry = page_geometry(ws);
    if let Some(header) = header {
        render_header_footer_band(
            doc,
            page,
            header,
            page_number,
            page_count,
            geometry.left,
            ws.page_margins.header,
            geometry.width - ws.page_margins.right,
            sheet_name,
        )?;
    }
    if let Some(footer) = footer {
        render_header_footer_band(
            doc,
            page,
            footer,
            page_number,
            page_count,
            geometry.left,
            geometry.height - ws.page_margins.footer,
            geometry.width - ws.page_margins.right,
            sheet_name,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_header_footer_band(
    doc: &XlsxDoc,
    page: &mut Page,
    source: &str,
    page_number: usize,
    page_count: usize,
    left: f64,
    baseline: f64,
    right: f64,
    sheet_name: &str,
) -> Result<()> {
    let sections = header_sections(source, page_number, page_count, sheet_name);
    for (section_index, spans) in sections.into_iter().enumerate() {
        if spans.is_empty() {
            continue;
        }
        let mut shaped_spans = Vec::new();
        let mut total_width = 0.0;
        for span in spans {
            let weight = if span.bold { 700 } else { 400 };
            let Some(face_id) = matching::resolve(&doc.fonts, "Calibri", weight, span.italic).face
            else {
                continue;
            };
            let (fallback_ids, fallback_faces) = coverage_faces(doc, face_id, weight, span.italic);
            if fallback_faces.is_empty() {
                continue;
            }
            let shaped = shape::require_glyphs(shape::shape_with_fallback(
                &span.text,
                &fallback_faces,
                9.0,
                0.0,
                false,
            ))?;
            total_width += shaped.width_pt;
            shaped_spans.push((span.text, fallback_ids, shaped));
        }
        let mut cursor = match section_index {
            0 => left,
            1 => (left + right - total_width) / 2.0,
            _ => right - total_width,
        };
        for (text, fallback_ids, shaped) in shaped_spans {
            let mapped = crate::text::semantic::map_shaped_run(&text, &shaped);
            let mut glyphs = mapped.visible;
            let mut semantics = Some(mapped.semantic);
            let mut run_start = cursor;
            let mut run_face = None;
            let mut run_glyphs = Vec::new();
            for (glyph, visible) in shaped.glyphs.iter().zip(glyphs.drain(..)) {
                let Some(&glyph_face) = fallback_ids.get(glyph.face_index) else {
                    continue;
                };
                if run_face.is_some_and(|face| face != glyph_face) {
                    push_header_footer_run(
                        page,
                        run_start,
                        baseline,
                        run_face.expect("header/footer run has a face"),
                        std::mem::take(&mut run_glyphs),
                    );
                    run_start = cursor;
                }
                run_face = Some(glyph_face);
                run_glyphs.push(visible);
                if let Some(semantic) = semantics.take() {
                    run_glyphs.extend(semantic);
                }
                cursor += glyph.advance_pt;
            }
            if let Some(face) = run_face {
                push_header_footer_run(page, run_start, baseline, face, run_glyphs);
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct HeaderSpan {
    pub(crate) text: String,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
}

fn push_header_footer_run(
    page: &mut Page,
    x_pt: f64,
    baseline: f64,
    face: FaceId,
    glyphs: Vec<crate::layout::PlacedGlyph>,
) {
    page.items.push(Positioned {
        x_pt,
        y_pt: baseline,
        item: Item::Glyphs(GlyphRun {
            face,
            size_pt: 9.0,
            color: Rgba::opaque(Rgb::new(0, 0, 0)),
            base_direction: None,
            glyphs,
            underline: None,
            strike: None,
        }),
    });
}

pub(crate) fn header_footer_coverage_spans(source: &str, sheet_name: &str) -> Vec<HeaderSpan> {
    header_sections(source, 1, 1, sheet_name)
        .into_iter()
        .flatten()
        .collect()
}

fn header_sections(
    source: &str,
    page_number: usize,
    page_count: usize,
    sheet_name: &str,
) -> [Vec<HeaderSpan>; 3] {
    let mut sections: [Vec<HeaderSpan>; 3] = std::array::from_fn(|_| Vec::new());
    let mut current = 1usize;
    let mut bold = false;
    let mut italic = false;
    let mut chars = source.chars();
    while let Some(ch) = chars.next() {
        if ch != '&' {
            push_header_span(&mut sections[current], &ch.to_string(), bold, italic);
            continue;
        }
        let Some(code) = chars.next() else {
            break;
        };
        match code {
            'L' => current = 0,
            'C' => current = 1,
            'R' => current = 2,
            'P' => push_header_span(
                &mut sections[current],
                &page_number.to_string(),
                bold,
                italic,
            ),
            'N' => push_header_span(
                &mut sections[current],
                &page_count.to_string(),
                bold,
                italic,
            ),
            'D' => push_header_span(&mut sections[current], "2000-01-01", bold, italic),
            'T' => push_header_span(&mut sections[current], "00:00", bold, italic),
            'F' => push_header_span(&mut sections[current], "workbook.xlsx", bold, italic),
            'A' => push_header_span(&mut sections[current], sheet_name, bold, italic),
            '&' => push_header_span(&mut sections[current], "&", bold, italic),
            'B' => bold = !bold,
            'I' => italic = !italic,
            other => push_header_span(&mut sections[current], &other.to_string(), bold, italic),
        }
    }
    sections
}

fn push_header_span(spans: &mut Vec<HeaderSpan>, text: &str, bold: bool, italic: bool) {
    if let Some(last) = spans.last_mut()
        && last.bold == bold
        && last.italic == italic
    {
        last.text.push_str(text);
    } else {
        spans.push(HeaderSpan {
            text: text.to_string(),
            bold,
            italic,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    doc: &XlsxDoc,
    ws: &Worksheet,
    cells: &HashMap<(u32, u32), &Cell>,
    band: &(u32, u32),
    col_w: &[f64],
    row_heights: &[f64],
    row: u32,
    x0: f64,
    y: f64,
    h: f64,
    page: &mut Page,
    warnings: &mut Vec<Warning>,
) -> Result<()> {
    let mut x = x0;
    let mut col = band.0;
    while col <= band.1 {
        let w = col_w
            .get(col as usize)
            .copied()
            .unwrap_or(default_width_pt());
        let merge = ws.merges.iter().find(|merge| merge.contains(row, col));
        if let Some(merge) = merge
            && !merge.is_anchor(row, col)
        {
            x += w;
            col += 1;
            continue;
        }
        let (cell_w, cell_h, col_span) = if let Some(merge) = merge {
            let last_col = merge.last_col.min(band.1);
            let width = col_w[merge.first_col as usize..=last_col as usize]
                .iter()
                .sum();
            let height = row_heights[merge.first_row as usize..=merge.last_row as usize]
                .iter()
                .sum();
            let span = last_col
                .checked_sub(col)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    OfficeError::unsupported(
                        UnsupportedCode::ResourceLimit,
                        "xlsx merged-cell column span overflow",
                    )
                })?;
            (width, height, span)
        } else {
            (w, h, 1)
        };
        let mut style = cells
            .get(&(row, col))
            .map(|cell| doc.styles.cell(cell.style_idx))
            .unwrap_or_default();
        if let Some(cell) = cells.get(&(row, col)) {
            apply_conditional_style(doc, ws, cells, cell, &mut style);
        }
        if let Some(cell) = cells.get(&(row, col)) {
            if let Some(fill) = style.fill {
                page.items.push(Positioned {
                    x_pt: x,
                    y_pt: y,
                    item: Item::Rect {
                        w_pt: cell_w,
                        h_pt: cell_h,
                        fill: Some(Rgba::opaque(fill)),
                        stroke: None,
                    },
                });
            }
            if let Some((color, ratio)) = conditional_data_bar(ws, cells, cell) {
                page.items.push(Positioned {
                    x_pt: x + 1.0,
                    y_pt: y + 1.0,
                    item: Item::Rect {
                        w_pt: ((cell_w - 2.0) * ratio).max(0.1),
                        h_pt: (cell_h - 2.0).max(0.1),
                        fill: Some(Rgba::opaque(color)),
                        stroke: None,
                    },
                });
            }
            render_cell_text(doc, cell, &style, x, y, cell_w, cell_h, page, warnings)?;
        }
        render_cell_borders(&style, ws.print_grid_lines, x, y, cell_w, cell_h, page);
        x += cell_w;
        col += col_span;
    }
    Ok(())
}

/// Applies `cellIs`/expression differential formats against cached values.
/// Rules are ordered by priority in OOXML, but a cell's first matching dxf is
/// what Excel paints for the common non-stopIfTrue rule family we support.
fn apply_conditional_style(
    doc: &XlsxDoc,
    ws: &Worksheet,
    cells: &HashMap<(u32, u32), &Cell>,
    cell: &Cell,
    style: &mut CellStyle,
) {
    for format in &ws.conditional_formats {
        if !format.ranges.iter().any(|range| {
            (range.first_row..=range.last_row).contains(&cell.row)
                && (range.first_col..=range.last_col).contains(&cell.col)
        }) {
            continue;
        }
        for rule in &format.rules {
            if let ConditionalKind::ColorScale { values, colors } = &rule.kind
                && let Some(value) = cached_number(cell)
            {
                let samples = numeric_values(format, cells);
                let thresholds: Vec<_> = values
                    .iter()
                    .filter_map(|threshold| resolve_threshold(*threshold, &samples))
                    .collect();
                if thresholds.len() == colors.len() {
                    style.fill = interpolate_scale(value, &thresholds, colors);
                }
                continue;
            }
            let matches = match &rule.kind {
                ConditionalKind::CellIs { operator, formulas } => {
                    cached_number(cell).is_some_and(|value| {
                        let left = formulas
                            .first()
                            .and_then(|formula| formula_value(formula, cells))
                            .unwrap_or(f64::NAN);
                        let right = formulas
                            .get(1)
                            .and_then(|formula| formula_value(formula, cells));
                        compare_conditional(value, left, right, operator)
                    })
                }
                ConditionalKind::Expression { formula } => expression_matches(formula, cell, cells),
                _ => false,
            };
            if !matches {
                continue;
            }
            if let Some(dxf) = rule.dxf_id.and_then(|id| doc.styles.dxf(id)) {
                if dxf.fill.is_some() {
                    style.fill = dxf.fill;
                }
                if dxf.color.is_some() {
                    style.color = dxf.color;
                }
                if dxf.bold {
                    style.bold = true;
                }
                for (target, source) in [
                    (&mut style.borders.top, dxf.borders.top),
                    (&mut style.borders.right, dxf.borders.right),
                    (&mut style.borders.bottom, dxf.borders.bottom),
                    (&mut style.borders.left, dxf.borders.left),
                ] {
                    if source.is_some() {
                        *target = source;
                    }
                }
            }
        }
    }
}

fn conditional_data_bar(
    ws: &Worksheet,
    cells: &HashMap<(u32, u32), &Cell>,
    cell: &Cell,
) -> Option<(Rgb, f64)> {
    let value = cached_number(cell)?;
    for format in &ws.conditional_formats {
        if !format.ranges.iter().any(|range| {
            (range.first_row..=range.last_row).contains(&cell.row)
                && (range.first_col..=range.last_col).contains(&cell.col)
        }) {
            continue;
        }
        let samples = numeric_values(format, cells);
        for rule in &format.rules {
            if let ConditionalKind::DataBar {
                minimum,
                maximum,
                color,
            } = rule.kind
            {
                let low = resolve_threshold(minimum, &samples)?;
                let high = resolve_threshold(maximum, &samples)?;
                let ratio = if high > low {
                    ((value - low) / (high - low)).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                return Some((color, ratio));
            }
        }
    }
    None
}

fn numeric_values(
    format: &crate::oxml::sml::ConditionalFormat,
    cells: &HashMap<(u32, u32), &Cell>,
) -> Vec<f64> {
    let mut values: Vec<_> = cells
        .iter()
        .filter(|((row, col), _)| {
            format.ranges.iter().any(|range| {
                (range.first_row..=range.last_row).contains(row)
                    && (range.first_col..=range.last_col).contains(col)
            })
        })
        .filter_map(|(_, cell)| cached_number(cell))
        .collect();
    values.sort_by(f64::total_cmp);
    values
}

fn resolve_threshold(threshold: ConditionalValue, samples: &[f64]) -> Option<f64> {
    let low = *samples.first()?;
    let high = *samples.last()?;
    Some(match threshold {
        ConditionalValue::Min => low,
        ConditionalValue::Max => high,
        ConditionalValue::Number(value) => value,
        ConditionalValue::Percent(percent) => low + (high - low) * percent / 100.0,
        ConditionalValue::Percentile(percent) => {
            let position = ((samples.len() - 1) as f64 * percent.clamp(0.0, 100.0) / 100.0)
                .clamp(0.0, (samples.len() - 1) as f64);
            let lower = position.floor() as usize;
            let upper = position.ceil() as usize;
            let fraction = position - lower as f64;
            samples[lower] + (samples[upper] - samples[lower]) * fraction
        }
    })
}

fn interpolate_scale(value: f64, thresholds: &[f64], colors: &[Rgb]) -> Option<Rgb> {
    if thresholds.len() != colors.len() || thresholds.len() < 2 {
        return None;
    }
    let segment = thresholds
        .windows(2)
        .position(|pair| value <= pair[1])
        .unwrap_or(thresholds.len() - 2);
    let low = thresholds[segment];
    let high = thresholds[segment + 1];
    let ratio = if high > low {
        ((value - low) / (high - low)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let from = colors[segment];
    let to = colors[segment + 1];
    let blend = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * ratio).round() as u8;
    Some(Rgb::new(
        blend(from.r, to.r),
        blend(from.g, to.g),
        blend(from.b, to.b),
    ))
}

fn cached_number(cell: &Cell) -> Option<f64> {
    if let CellValue::Number(value) = &cell.value {
        Some(*value)
    } else {
        None
    }
}

fn formula_value(formula: &str, cells: &HashMap<(u32, u32), &Cell>) -> Option<f64> {
    ArithmeticParser::new(formula.trim().trim_start_matches('='), cells).parse()
}

fn compare_conditional(value: f64, first: f64, second: Option<f64>, operator: &str) -> bool {
    match operator {
        "between" => second.is_some_and(|last| value >= first && value <= last),
        "notBetween" => second.is_some_and(|last| value < first || value > last),
        "equal" => value == first,
        "notEqual" => value != first,
        "greaterThan" => value > first,
        "lessThan" => value < first,
        "greaterThanOrEqual" => value >= first,
        "lessThanOrEqual" => value <= first,
        _ => false,
    }
}

fn expression_matches(formula: &str, cell: &Cell, cells: &HashMap<(u32, u32), &Cell>) -> bool {
    let formula = formula.trim_start_matches('=');
    let Some((index, operator)) = find_comparison(formula) else {
        return false;
    };
    let left = formula_value(&formula[..index], cells).or_else(|| cached_number(cell));
    let right = formula_value(&formula[index + operator.len()..], cells);
    left.zip(right).is_some_and(|(a, b)| {
        compare_conditional(
            a,
            b,
            None,
            match operator {
                ">=" => "greaterThanOrEqual",
                "<=" => "lessThanOrEqual",
                "<>" => "notEqual",
                "=" => "equal",
                ">" => "greaterThan",
                _ => "lessThan",
            },
        )
    })
}

fn find_comparison(formula: &str) -> Option<(usize, &'static str)> {
    let bytes = formula.as_bytes();
    let mut depth = 0i32;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ if depth == 0 => {
                for operator in [">=", "<=", "<>", "=", ">", "<"] {
                    if formula[index..].starts_with(operator) {
                        return Some((index, operator));
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

struct ArithmeticParser<'a, 'cells> {
    input: &'a [u8],
    cursor: usize,
    cells: &'cells HashMap<(u32, u32), &'cells Cell>,
}

impl<'a, 'cells> ArithmeticParser<'a, 'cells> {
    fn new(input: &'a str, cells: &'cells HashMap<(u32, u32), &'cells Cell>) -> Self {
        Self {
            input: input.as_bytes(),
            cursor: 0,
            cells,
        }
    }

    fn parse(mut self) -> Option<f64> {
        let value = self.expression()?;
        self.skip_space();
        (self.cursor == self.input.len()).then_some(value)
    }

    fn expression(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        loop {
            self.skip_space();
            match self.peek() {
                Some(b'+') => {
                    self.cursor += 1;
                    value += self.term()?;
                }
                Some(b'-') => {
                    self.cursor += 1;
                    value -= self.term()?;
                }
                _ => return Some(value),
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut value = self.factor()?;
        loop {
            self.skip_space();
            match self.peek() {
                Some(b'*') => {
                    self.cursor += 1;
                    value *= self.factor()?;
                }
                Some(b'/') => {
                    self.cursor += 1;
                    let denominator = self.factor()?;
                    if denominator == 0.0 {
                        return None;
                    }
                    value /= denominator;
                }
                _ => return Some(value),
            }
        }
    }

    fn factor(&mut self) -> Option<f64> {
        self.skip_space();
        match self.peek()? {
            b'+' => {
                self.cursor += 1;
                self.factor()
            }
            b'-' => {
                self.cursor += 1;
                self.factor().map(|value| -value)
            }
            b'(' => {
                self.cursor += 1;
                let value = self.expression()?;
                self.skip_space();
                (self.peek()? == b')').then(|| {
                    self.cursor += 1;
                    value
                })
            }
            byte if byte.is_ascii_digit() || byte == b'.' => self.number(),
            byte if byte.is_ascii_alphabetic() || byte == b'$' => self.cell_reference(),
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.cursor;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_digit() || byte == b'.')
        {
            self.cursor += 1;
        }
        std::str::from_utf8(&self.input[start..self.cursor])
            .ok()?
            .parse()
            .ok()
    }

    fn cell_reference(&mut self) -> Option<f64> {
        let start = self.cursor;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'$')
        {
            self.cursor += 1;
        }
        let reference = std::str::from_utf8(&self.input[start..self.cursor]).ok()?;
        let (col, row) = crate::oxml::sml::parse_a1(reference)?;
        self.cells
            .get(&(row, col))
            .and_then(|cell| cached_number(cell))
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.cursor += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.cursor).copied()
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
) -> Result<()> {
    if matches!(&cell.value, CellValue::UncachedFormula) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::FormulaCacheMissing,
            format!(
                "cell {}{} has no usable formula cache",
                column_name(cell.col),
                cell.row + 1
            ),
        ));
    }
    let (text, color, default_align) = format_cell(doc, cell, style);
    if text.is_empty() {
        return Ok(());
    }
    let size = if style.font_size > 0.0 {
        style.font_size
    } else {
        11.0
    };
    let weight = if style.bold { 700 } else { 400 };
    let family = style.font_name.as_deref().unwrap_or("Calibri");
    let resolved = matching::resolve(&doc.fonts, family, weight, style.italic);
    for wn in resolved.warnings {
        if !warnings.contains(&wn) {
            warnings.push(wn);
        }
    }
    let Some(face) = resolved.face else {
        return Ok(());
    };
    let Some(font_face) = doc.fonts.face(face) else {
        return Ok(());
    };
    let Some(parsed) = font_face.parsed() else {
        return Ok(());
    };
    let metrics = FaceMetrics::new(parsed);
    let (fallback_ids, fallback_faces) = coverage_faces(doc, face, weight, style.italic);
    let align = style.align.as_deref().unwrap_or(default_align);
    let pad = 2.0;
    let lines = if style.wrap || text.contains('\n') {
        wrap_lines(&text, &fallback_faces, size, (w - 2.0 * pad).max(1.0))?
    } else {
        vec![text]
    };
    let line_height = metrics.line_height_pt(size);
    let block_height = line_height * lines.len() as f64;
    let first_baseline = match style.vertical_align.as_deref().unwrap_or("bottom") {
        "top" => y + pad + metrics.ascent_pt(size),
        "center" => y + ((h - block_height) / 2.0).max(0.0) + metrics.ascent_pt(size),
        _ => y + (h - block_height - pad).max(0.0) + metrics.ascent_pt(size),
    };
    let text_color = color.unwrap_or(Rgba::opaque(Rgb::new(0, 0, 0)));
    for (line_index, line) in lines.iter().enumerate() {
        let shaped = shape::require_glyphs(shape::shape_with_fallback(
            line,
            &fallback_faces,
            size,
            0.0,
            false,
        ))?;
        let mapped = crate::text::semantic::map_shaped_run(line, &shaped);
        let mut semantics = Some(mapped.semantic);
        let tx = match align {
            "center" => x + (w - shaped.width_pt) / 2.0,
            "right" => x + w - shaped.width_pt - pad,
            _ => x + pad,
        };
        let mut cursor = tx.max(x + pad);
        let baseline = first_baseline + line_index as f64 * line_height;
        let mut run_start = cursor;
        let mut run_face = None;
        let mut run_glyphs = Vec::new();
        for (glyph, visible) in shaped.glyphs.iter().zip(mapped.visible) {
            let Some(&glyph_face) = fallback_ids.get(glyph.face_index) else {
                continue;
            };
            if run_face.is_some_and(|face| face != glyph_face) {
                push_cell_text_run(
                    page,
                    run_start,
                    baseline,
                    run_face.expect("cell text run has a face"),
                    size,
                    text_color,
                    std::mem::take(&mut run_glyphs),
                );
                run_start = cursor;
            }
            run_face = Some(glyph_face);
            run_glyphs.push(visible);
            if let Some(semantic) = semantics.take() {
                run_glyphs.extend(semantic);
            }
            cursor += glyph.advance_pt;
        }
        if let Some(face) = run_face {
            push_cell_text_run(
                page, run_start, baseline, face, size, text_color, run_glyphs,
            );
        }
    }
    Ok(())
}

fn push_cell_text_run(
    page: &mut Page,
    x_pt: f64,
    baseline: f64,
    face: FaceId,
    size_pt: f64,
    color: Rgba,
    glyphs: Vec<crate::layout::PlacedGlyph>,
) {
    page.items.push(Positioned {
        x_pt,
        y_pt: baseline,
        item: Item::Glyphs(GlyphRun {
            face,
            size_pt,
            color,
            base_direction: None,
            glyphs,
            underline: None,
            strike: None,
        }),
    });
}

fn column_name(mut col: u32) -> String {
    let mut name = String::new();
    loop {
        let digit = (col % 26) as u8;
        name.push(char::from(b'A' + digit));
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    name.chars().rev().collect()
}

impl MergeRange {
    fn contains(self, row: u32, col: u32) -> bool {
        (self.first_row..=self.last_row).contains(&row)
            && (self.first_col..=self.last_col).contains(&col)
    }

    fn is_anchor(self, row: u32, col: u32) -> bool {
        row == self.first_row && col == self.first_col
    }
}

fn render_cell_borders(
    style: &CellStyle,
    print_grid_lines: bool,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    page: &mut Page,
) {
    let edges = [
        (0.0, 0.0, w, 0.0, style.borders.top.as_ref()),
        (w, 0.0, w, h, style.borders.right.as_ref()),
        (0.0, h, w, h, style.borders.bottom.as_ref()),
        (0.0, 0.0, 0.0, h, style.borders.left.as_ref()),
    ];
    for (x0, y0, x1, y1, edge) in edges {
        let Some(stroke) = edge
            .map(border_stroke)
            .or_else(|| print_grid_lines.then(default_grid_stroke))
        else {
            continue;
        };
        page.items.push(Positioned {
            x_pt: x + x0,
            y_pt: y + y0,
            item: Item::Line {
                dx_pt: x1 - x0,
                dy_pt: y1 - y0,
                stroke,
            },
        });
    }
}

fn default_grid_stroke() -> Stroke {
    Stroke {
        width_pt: 0.25,
        color: GRID,
        dash: None,
    }
}

fn border_stroke(edge: &BorderEdge) -> Stroke {
    Stroke {
        width_pt: edge.width_pt,
        color: Rgba::opaque(edge.color.unwrap_or(Rgb::new(0, 0, 0))),
        dash: edge.dash.clone(),
    }
}

fn wrap_lines(text: &str, faces: &[&Face], size: f64, max_width: f64) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for source_line in text.split('\n') {
        let mut current = String::new();
        for word in source_line.split_inclusive(char::is_whitespace) {
            let candidate = format!("{current}{word}");
            let width = shape::require_glyphs(shape::shape_with_fallback(
                &candidate, faces, size, 0.0, false,
            ))?
            .width_pt;
            if width > max_width && !current.trim().is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            current.push_str(word);
        }
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    Ok(lines)
}

fn effective_row_heights(
    doc: &XlsxDoc,
    ws: &Worksheet,
    cells: &HashMap<(u32, u32), &Cell>,
    col_widths: &[f64],
    max_row: u32,
) -> Result<Vec<f64>> {
    let height_count = usize::try_from(max_row)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| {
            OfficeError::unsupported(
                UnsupportedCode::ResourceLimit,
                "xlsx row-height allocation overflows usize",
            )
        })?;
    let mut heights = Vec::new();
    try_reserve(&mut heights, height_count, "xlsx row heights")?;
    heights.extend((0..=max_row).map(|row| {
        ws.row_heights
            .get(&row)
            .copied()
            .unwrap_or(ws.default_row_height)
    }));
    for (&(row, col), cell) in cells {
        let style = doc.styles.cell(cell.style_idx);
        if !style.wrap || ws.row_heights.contains_key(&row) {
            continue;
        }
        if ws.merges.iter().any(|merge| merge.contains(row, col)) {
            continue;
        }
        let (text, _, _) = format_cell(doc, cell, &style);
        if text.is_empty() {
            continue;
        }
        let size = if style.font_size > 0.0 {
            style.font_size
        } else {
            11.0
        };
        let weight = if style.bold { 700 } else { 400 };
        let resolved = matching::resolve(
            &doc.fonts,
            style.font_name.as_deref().unwrap_or("Calibri"),
            weight,
            style.italic,
        );
        let Some(primary_face) = resolved.face else {
            continue;
        };
        let Some(font_face) = doc.fonts.face(primary_face) else {
            continue;
        };
        let Some(parsed) = font_face.parsed() else {
            continue;
        };
        let metrics = FaceMetrics::new(parsed);
        let width = col_widths
            .get(col as usize)
            .copied()
            .unwrap_or(default_width_pt());
        let (_, fallback_faces) = coverage_faces(doc, primary_face, weight, style.italic);
        let line_count = wrap_lines(&text, &fallback_faces, size, (width - 4.0).max(1.0))?.len();
        heights[row as usize] =
            heights[row as usize].max(line_count as f64 * metrics.line_height_pt(size) + 4.0);
    }
    Ok(heights)
}

fn coverage_faces(
    doc: &XlsxDoc,
    primary: FaceId,
    weight: u16,
    italic: bool,
) -> (Vec<FaceId>, Vec<&Face>) {
    matching::coverage_fallbacks(&doc.fonts, primary, weight, italic)
        .into_iter()
        .filter_map(|id| doc.fonts.face(id).map(|face| (id, face)))
        .unzip()
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
fn column_widths(ws: &Worksheet, max_col: u32) -> Result<Vec<f64>> {
    let width_count = usize::try_from(max_col)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| {
            OfficeError::unsupported(
                UnsupportedCode::ResourceLimit,
                "xlsx column-width allocation overflows usize",
            )
        })?;
    let mut widths = Vec::new();
    try_reserve(&mut widths, width_count, "xlsx column widths")?;
    widths.resize(width_count, default_width_pt());
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
    Ok(widths)
}

/// Splits a print-area column slice. Manual `colBreaks` begin a fresh band
/// even when the preceding columns would still fit.
fn split_bands_range(
    col_w: &[f64],
    content_w: f64,
    first: u32,
    last: u32,
    breaks: &[u32],
    title_cols: Option<(u32, u32)>,
) -> Vec<(u32, u32)> {
    let mut bands = Vec::new();
    let mut start = first;
    let mut acc = 0.0;
    let title_width = title_cols
        .map(|(first, last)| {
            let last = last.min(col_w.len().saturating_sub(1) as u32);
            if first <= last {
                col_w[first as usize..=last as usize].iter().sum()
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);
    for col in first..=last {
        let width = col_w.get(col as usize).copied().unwrap_or_default();
        let capacity = if start > first {
            (content_w - title_width).max(1.0)
        } else {
            content_w
        };
        if col > start && (acc + width > capacity || breaks.contains(&col)) {
            bands.push((start, col - 1));
            start = col;
            acc = 0.0;
        }
        acc += width;
    }
    bands.push((start, last));
    bands
}

#[cfg(all(test, feature = "bundled-fonts"))]
mod tests {
    use super::*;
    use crate::resource_limits::PageBudget;
    use crate::testkit::zip_package;
    use crate::{ConvertOptions, OfficeError, UnsupportedCode};

    fn three_page_fixture() -> Vec<u8> {
        let content_types = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
        let root_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let workbook = br#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let workbook_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let sheet = br#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row></sheetData><rowBreaks count="2" manualBreakCount="2"><brk id="1" man="1"/><brk id="2" man="1"/></rowBreaks></worksheet>"#;
        zip_package(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", workbook_rels),
            ("xl/worksheets/sheet1.xml", sheet),
        ])
    }

    #[test]
    fn page_budget_refuses_third_worksheet_page_before_building_it() {
        let bytes = three_page_fixture();
        let doc = XlsxDoc::open(&bytes, &ConvertOptions::default()).unwrap();
        let mut budget = PageBudget::new(2);
        let error = render_xlsx_with_page_budget(&doc, &mut budget).unwrap_err();
        assert_eq!(budget.used(), 2);
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ));
    }
}

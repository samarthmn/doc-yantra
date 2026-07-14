//! Slide rendering: turns resolved slides into a [`LaidOutDocument`] (one page
//! per slide) of positioned primitives, then the shared PDF emitter runs.

use super::assemble::{Category, PptxDoc, ResolvedSlide};
use super::geometry::{Contour, evaluate_shape, resolve_colorspec, solid_fill_color};
use super::inherit::{category, resolve_text_body, resolve_xfrm};
use super::table_style::CellBounds;
use super::textbody::layout_textbody;
use crate::layout::{FillRule, Item, LaidOutDocument, Page, PathPaint, Positioned, Stroke};
use crate::oxml::pml::{
    Fill, LineSpec, PictureMode, PmlTable, ShapeElem, ShapeProps, TextBody, Xfrm,
};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

/// Renders all slides into a laid-out document.
pub fn render_pptx(doc: &PptxDoc) -> Result<(LaidOutDocument, Vec<Warning>)> {
    let mut warnings = Vec::new();
    let size = doc.presentation.size;
    let mut pages = Vec::new();

    for rs in &doc.slides {
        let mut page = Page {
            width_pt: size.w_pt,
            height_pt: size.h_pt,
            items: Vec::new(),
        };
        // Background: slide → layout → master.
        let background = rs
            .slide
            .background
            .as_ref()
            .map(|fill| (fill, &rs.rels))
            .or_else(|| {
                rs.layout
                    .background
                    .as_ref()
                    .map(|fill| (fill, &rs.layout_rels))
            })
            .or_else(|| {
                rs.master
                    .background
                    .as_ref()
                    .map(|fill| (fill, &rs.master_rels))
            });
        if let Some((fill, rels)) = background {
            render_fill(
                doc, rs, rels, fill, 0.0, 0.0, size.w_pt, size.h_pt, &mut page,
            )?;
        }

        let offset = (0.0, 0.0);
        if rs.slide.show_master_shapes != Some(false) {
            for elem in &rs.master.shapes {
                if !is_placeholder(elem) {
                    render_elem(
                        doc,
                        rs,
                        elem,
                        &rs.master_rels,
                        offset,
                        &mut page,
                        &mut warnings,
                    )?;
                }
            }
        }
        for elem in &rs.layout.shapes {
            if !is_placeholder(elem) {
                render_elem(
                    doc,
                    rs,
                    elem,
                    &rs.layout_rels,
                    offset,
                    &mut page,
                    &mut warnings,
                )?;
            }
        }
        for elem in &rs.slide.shapes {
            render_elem(doc, rs, elem, &rs.rels, offset, &mut page, &mut warnings)?;
        }
        pages.push(page);
    }

    let laid = LaidOutDocument {
        pages,
        images: doc.images.clone(),
        outline: Vec::new(),
        dates: doc.dates.clone(),
    };
    Ok((laid, warnings))
}

fn render_elem(
    doc: &PptxDoc,
    rs: &ResolvedSlide,
    elem: &ShapeElem,
    rels: &HashMap<String, String>,
    offset: (f64, f64),
    page: &mut Page,
    warnings: &mut Vec<Warning>,
) -> Result<()> {
    match elem {
        ShapeElem::Shape { ph, props, text } => {
            let ph_ref = ph.clone().unwrap_or_default();
            let xfrm = resolve_xfrm(&ph_ref, props.xfrm, &rs.layout, &rs.master);
            let x = xfrm.x_pt + offset.0;
            let y = xfrm.y_pt + offset.1;
            let item_start = page.items.len();

            let contours = evaluate_shape(props, xfrm)?;
            render_shape_geometry(doc, rs, rels, props, &contours, x, y, xfrm, page)?;

            // Text.
            if let Some(body) = text
                && !body.paragraphs.is_empty()
            {
                let body = resolve_text_body(ph.as_ref(), body, &rs.layout, &rs.master);
                place_text(
                    doc,
                    rs,
                    rels,
                    &body,
                    category(&ph_ref),
                    x,
                    y,
                    xfrm,
                    warnings,
                    page,
                )?;
            }
            transform_new_items(page, item_start, shape_matrix(x, y, xfrm));
        }
        ShapeElem::Picture { props, drawing } => {
            let xfrm = props.xfrm.unwrap_or_default();
            let item_start = page.items.len();
            if let Some(error) = &drawing.unsupported {
                return Err(OfficeError::unsupported(error.code, error.detail.clone()));
            }
            let rel_id = drawing.blip_rel_id.as_deref().ok_or_else(|| {
                OfficeError::unsupported(
                    UnsupportedCode::MissingRelationshipTarget,
                    "p:pic has no image relationship",
                )
            })?;
            if let Some((code, detail)) = doc.image_error(rels, rel_id, drawing.crop) {
                return Err(OfficeError::unsupported(code, detail));
            }
            let image_id = doc
                .image_id_with_crop(rels, rel_id, drawing.crop)
                .ok_or_else(|| {
                    OfficeError::unsupported(
                        UnsupportedCode::MissingRelationshipTarget,
                        format!("image relationship {rel_id} has no decoded resource"),
                    )
                })?;
            page.items.push(Positioned {
                x_pt: xfrm.x_pt + offset.0,
                y_pt: xfrm.y_pt + offset.1,
                item: Item::Image {
                    w_pt: xfrm.w_pt,
                    h_pt: xfrm.h_pt,
                    image_id,
                },
            });
            transform_new_items(
                page,
                item_start,
                shape_matrix(xfrm.x_pt + offset.0, xfrm.y_pt + offset.1, xfrm),
            );
            if let Some(stroke) = outline_stroke(rs, props) {
                render_contours(
                    &[rect_contour(xfrm.w_pt, xfrm.h_pt)],
                    xfrm.x_pt + offset.0,
                    xfrm.y_pt + offset.1,
                    &stroke,
                    props.line.as_ref(),
                    page,
                );
            }
        }
        ShapeElem::Table { xfrm, table } => {
            let x = xfrm.map(|x| x.x_pt).unwrap_or(60.0) + offset.0;
            let y = xfrm.map(|x| x.y_pt).unwrap_or(150.0) + offset.1;
            render_table(doc, rs, rels, table, x, y, warnings, page)?;
        }
        ShapeElem::Chart { xfrm, rel_id } => {
            let xfrm = xfrm.unwrap_or(Xfrm {
                x_pt: 60.0,
                y_pt: 90.0,
                w_pt: 360.0,
                h_pt: 216.0,
                present: true,
                ..Xfrm::default()
            });
            let item_start = page.items.len();
            if let Some(chart) = doc.chart(rels, rel_id) {
                for child in chart.render(&doc.fonts, &rs.theme, xfrm.w_pt, xfrm.h_pt)? {
                    page.items.push(Positioned {
                        x_pt: xfrm.x_pt + offset.0 + child.x_pt,
                        y_pt: xfrm.y_pt + offset.1 + child.y_pt,
                        item: child.item,
                    });
                }
            } else if let Some((code, detail)) = doc.chart_error(rels, rel_id) {
                return Err(OfficeError::unsupported(code, detail));
            }
            transform_new_items(
                page,
                item_start,
                shape_matrix(xfrm.x_pt + offset.0, xfrm.y_pt + offset.1, xfrm),
            );
        }
        ShapeElem::SmartArt { xfrm, data_rel_id } => {
            let frame = xfrm.unwrap_or(Xfrm {
                w_pt: 360.0,
                h_pt: 216.0,
                present: true,
                ..Xfrm::default()
            });
            let item_start = page.items.len();
            if let Some(diagram) = doc.diagram(rels, data_rel_id) {
                for child in &diagram.shapes {
                    render_elem(
                        doc,
                        rs,
                        child,
                        &diagram.rels,
                        (frame.x_pt + offset.0, frame.y_pt + offset.1),
                        page,
                        warnings,
                    )?;
                }
            }
            transform_new_items(
                page,
                item_start,
                shape_matrix(frame.x_pt + offset.0, frame.y_pt + offset.1, frame),
            );
        }
        ShapeElem::OlePreview {
            xfrm,
            preview_rel_id,
        } => {
            let frame = xfrm.unwrap_or(Xfrm {
                w_pt: 144.0,
                h_pt: 72.0,
                present: true,
                ..Xfrm::default()
            });
            let item_start = page.items.len();
            if let Some((code, detail)) = doc.ole_preview_error(rels, preview_rel_id) {
                return Err(OfficeError::unsupported(code, detail));
            }
            let image_id = doc.image_id(rels, preview_rel_id).ok_or_else(|| {
                OfficeError::unsupported(
                    UnsupportedCode::MissingRelationshipTarget,
                    format!("OLE preview {preview_rel_id} has no decoded image"),
                )
            })?;
            page.items.push(Positioned {
                x_pt: frame.x_pt + offset.0,
                y_pt: frame.y_pt + offset.1,
                item: Item::Image {
                    w_pt: frame.w_pt,
                    h_pt: frame.h_pt,
                    image_id,
                },
            });
            transform_new_items(
                page,
                item_start,
                shape_matrix(frame.x_pt + offset.0, frame.y_pt + offset.1, frame),
            );
        }
        ShapeElem::Group {
            xfrm,
            child_space,
            children,
        } => {
            let item_start = page.items.len();
            for child in children {
                render_elem(doc, rs, child, rels, (0.0, 0.0), page, warnings)?;
            }
            transform_new_items(page, item_start, group_matrix(*xfrm, *child_space, offset));
        }
        ShapeElem::Unsupported(_) => {} // reported by profile; convert fails first
    }
    Ok(())
}

fn is_placeholder(elem: &ShapeElem) -> bool {
    matches!(elem, ShapeElem::Shape { ph: Some(_), .. })
}

#[expect(
    clippy::too_many_arguments,
    reason = "fill rendering needs document resources, relationship scope, and target bounds"
)]
fn render_fill(
    doc: &PptxDoc,
    rs: &ResolvedSlide,
    rels: &HashMap<String, String>,
    fill: &Fill,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    page: &mut Page,
) -> Result<()> {
    if let Fill::Unsupported(error) = fill {
        return Err(OfficeError::unsupported(error.code, error.detail.clone()));
    }
    if let Some(slot) = fill.unresolved_scheme(&rs.theme) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::InvalidFill,
            format!("fill references unresolved theme color {slot}"),
        ));
    }
    if let Fill::LinearGradient { angle_deg, stops } = fill
        && stops.len() >= 2
    {
        let mut resolved: Vec<(f64, Rgba)> = stops
            .iter()
            .map(|(position, color)| {
                (
                    position.clamp(0.0, 1.0),
                    resolve_colorspec(&rs.theme, color),
                )
            })
            .collect();
        resolved.sort_by(|left, right| left.0.total_cmp(&right.0));
        let radians = angle_deg.to_radians();
        let horizontal = radians.sin().abs() < 0.001;
        let vertical = radians.cos().abs() < 0.001;
        const BANDS: usize = 32;
        if horizontal || vertical {
            for band in 0..BANDS {
                let t0 = band as f64 / BANDS as f64;
                let t1 = (band + 1) as f64 / BANDS as f64;
                let color = gradient_color(&resolved, (t0 + t1) / 2.0);
                let (bx, by, bw, bh) = if horizontal {
                    (x + w * t0, y, w * (t1 - t0) + 0.02, h)
                } else {
                    (x, y + h * t0, w, h * (t1 - t0) + 0.02)
                };
                page.items.push(Positioned {
                    x_pt: bx,
                    y_pt: by,
                    item: Item::Rect {
                        w_pt: bw,
                        h_pt: bh,
                        fill: Some(color),
                        stroke: None,
                    },
                });
            }
        } else {
            let (dx, dy) = (radians.cos(), radians.sin());
            let span = dx.abs() + dy.abs();
            for row in 0..BANDS {
                for col in 0..BANDS {
                    let nx = (col as f64 + 0.5) / BANDS as f64 - 0.5;
                    let ny = (row as f64 + 0.5) / BANDS as f64 - 0.5;
                    let t = 0.5 + (nx * dx + ny * dy) / span;
                    page.items.push(Positioned {
                        x_pt: x + w * col as f64 / BANDS as f64,
                        y_pt: y + h * row as f64 / BANDS as f64,
                        item: Item::Rect {
                            w_pt: w / BANDS as f64 + 0.02,
                            h_pt: h / BANDS as f64 + 0.02,
                            fill: Some(gradient_color(&resolved, t.clamp(0.0, 1.0))),
                            stroke: None,
                        },
                    });
                }
            }
        }
    } else if let Fill::Pattern {
        preset,
        foreground,
        background,
    } = fill
    {
        let background = resolve_colorspec(&rs.theme, background);
        let foreground = resolve_colorspec(&rs.theme, foreground);
        page.items.push(Positioned {
            x_pt: x,
            y_pt: y,
            item: Item::Rect {
                w_pt: w,
                h_pt: h,
                fill: Some(background),
                stroke: None,
            },
        });
        render_pattern(preset, foreground, x, y, w, h, page);
    } else if let Fill::Picture(picture) = fill {
        if let Some((code, detail)) = doc.image_error(rels, &picture.rel_id, picture.crop) {
            return Err(OfficeError::unsupported(code, detail));
        }
        let image_id = doc
            .image_id_with_crop(rels, &picture.rel_id, picture.crop)
            .ok_or_else(|| {
                OfficeError::unsupported(
                    UnsupportedCode::MissingRelationshipTarget,
                    format!(
                        "picture-fill relationship {} has no decoded image",
                        picture.rel_id
                    ),
                )
            })?;
        match picture.mode {
            PictureMode::Stretch => page.items.push(Positioned {
                x_pt: x,
                y_pt: y,
                item: Item::Image {
                    w_pt: w,
                    h_pt: h,
                    image_id,
                },
            }),
            PictureMode::Tile { scale_x, scale_y } => {
                let tile_w = (72.0 * scale_x.abs()).clamp(4.0, w.max(4.0));
                let tile_h = (72.0 * scale_y.abs()).clamp(4.0, h.max(4.0));
                let mut ty = y;
                while ty < y + h - 0.01 {
                    let mut tx = x;
                    while tx < x + w - 0.01 {
                        page.items.push(Positioned {
                            x_pt: tx,
                            y_pt: ty,
                            item: Item::Image {
                                w_pt: tile_w.min(x + w - tx),
                                h_pt: tile_h.min(y + h - ty),
                                image_id,
                            },
                        });
                        tx += tile_w;
                    }
                    ty += tile_h;
                }
            }
        }
    } else if let Some(color) = solid_fill_color(&rs.theme, fill) {
        page.items.push(Positioned {
            x_pt: x,
            y_pt: y,
            item: Item::Rect {
                w_pt: w,
                h_pt: h,
                fill: Some(color),
                stroke: None,
            },
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_shape_geometry(
    doc: &PptxDoc,
    rs: &ResolvedSlide,
    rels: &HashMap<String, String>,
    props: &ShapeProps,
    contours: &[Contour],
    x: f64,
    y: f64,
    xfrm: Xfrm,
    page: &mut Page,
) -> Result<()> {
    let commands = contour_commands(contours);
    let stroke = outline_stroke(rs, props);
    match props.fill.as_ref() {
        Some(Fill::Solid(color)) => {
            if let Some(slot) = props
                .fill
                .as_ref()
                .and_then(|fill| fill.unresolved_scheme(&rs.theme))
            {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::InvalidFill,
                    format!("shape fill references unresolved theme color {slot}"),
                ));
            }
            page.items.push(Positioned {
                x_pt: x,
                y_pt: y,
                item: Item::Path {
                    commands,
                    paint: PathPaint {
                        fill: Some(resolve_colorspec(&rs.theme, color)),
                        stroke: stroke.clone(),
                        fill_rule: FillRule::NonZero,
                    },
                },
            });
        }
        Some(fill) if !matches!(fill, Fill::None) => {
            let mut clipped = Page::default();
            render_fill(
                doc,
                rs,
                rels,
                fill,
                0.0,
                0.0,
                xfrm.w_pt,
                xfrm.h_pt,
                &mut clipped,
            )?;
            page.items.push(Positioned {
                x_pt: x,
                y_pt: y,
                item: Item::Clip {
                    commands: commands.clone(),
                    fill_rule: FillRule::NonZero,
                    items: clipped.items,
                },
            });
            if let Some(stroke) = stroke {
                push_path(page, commands, x, y, None, Some(stroke));
            }
        }
        _ => {
            if let Some(stroke) = stroke {
                push_path(page, commands, x, y, None, Some(stroke));
            }
        }
    }
    Ok(())
}

fn render_contours(
    contours: &[Contour],
    x: f64,
    y: f64,
    stroke: &Stroke,
    line: Option<&LineSpec>,
    page: &mut Page,
) {
    for contour in contours {
        push_path(
            page,
            contour.commands.clone(),
            x,
            y,
            None,
            Some(stroke.clone()),
        );
        if let Some(line) = line {
            if line.head_end.is_some() && contour.points.len() >= 2 {
                render_arrowhead(contour.points[1], contour.points[0], x, y, stroke, page);
            }
            if line.tail_end.is_some() && contour.points.len() >= 2 {
                let count = contour.points.len();
                render_arrowhead(
                    contour.points[count - 2],
                    contour.points[count - 1],
                    x,
                    y,
                    stroke,
                    page,
                );
            }
        }
    }
}

fn contour_commands(contours: &[Contour]) -> Vec<crate::layout::PathCommand> {
    contours
        .iter()
        .flat_map(|contour| contour.commands.iter().copied())
        .collect()
}

fn push_path(
    page: &mut Page,
    commands: Vec<crate::layout::PathCommand>,
    x: f64,
    y: f64,
    fill: Option<Rgba>,
    stroke: Option<Stroke>,
) {
    page.items.push(Positioned {
        x_pt: x,
        y_pt: y,
        item: Item::Path {
            commands,
            paint: PathPaint {
                fill,
                stroke,
                fill_rule: FillRule::NonZero,
            },
        },
    });
}

fn render_arrowhead(
    from: super::geometry::Point,
    tip: super::geometry::Point,
    x: f64,
    y: f64,
    stroke: &Stroke,
    page: &mut Page,
) {
    let angle = (tip.y - from.y).atan2(tip.x - from.x);
    let length = (stroke.width_pt * 4.0).clamp(4.0, 12.0);
    for delta in [-0.55_f64, 0.55] {
        let endpoint = super::geometry::Point {
            x: tip.x - length * (angle + delta).cos(),
            y: tip.y - length * (angle + delta).sin(),
        };
        page.items.push(Positioned {
            x_pt: x + tip.x,
            y_pt: y + tip.y,
            item: Item::Line {
                dx_pt: endpoint.x - tip.x,
                dy_pt: endpoint.y - tip.y,
                stroke: stroke.clone(),
            },
        });
    }
}

fn rect_contour(w: f64, h: f64) -> Contour {
    Contour {
        commands: vec![
            crate::layout::PathCommand::MoveTo { x: 0.0, y: 0.0 },
            crate::layout::PathCommand::LineTo { x: w, y: 0.0 },
            crate::layout::PathCommand::LineTo { x: w, y: h },
            crate::layout::PathCommand::LineTo { x: 0.0, y: h },
            crate::layout::PathCommand::Close,
        ],
        points: vec![
            super::geometry::Point { x: 0.0, y: 0.0 },
            super::geometry::Point { x: w, y: 0.0 },
            super::geometry::Point { x: w, y: h },
            super::geometry::Point { x: 0.0, y: h },
        ],
        closed: true,
    }
}

fn render_pattern(preset: &str, color: Rgba, x: f64, y: f64, w: f64, h: f64, page: &mut Page) {
    let stroke = Stroke {
        width_pt: 0.6,
        color,
        dash: None,
    };
    let spacing = match preset {
        "pct5" | "pct10" | "pct20" | "pct25" | "pct30" | "pct40" | "pct50" | "pct60" | "pct70"
        | "pct75" | "pct80" | "pct90" => 6.0,
        "ltHorz" | "ltVert" | "ltDnDiag" | "ltUpDiag" | "smCheck" | "smGrid" | "dotGrid"
        | "smConfetti" => 5.0,
        "dkHorz" | "dkVert" | "dkDnDiag" | "dkUpDiag" | "solidDmnd" => 6.0,
        "lgCheck" | "lgGrid" | "lgConfetti" => 12.0,
        _ => 9.0,
    };
    match preset {
        name if name.starts_with("pct") => {
            let mut py = y + spacing / 2.0;
            while py < y + h {
                let mut px = x + spacing / 2.0;
                while px < x + w {
                    page.items.push(Positioned {
                        x_pt: px,
                        y_pt: py,
                        item: Item::Rect {
                            w_pt: 1.2,
                            h_pt: 1.2,
                            fill: Some(color),
                            stroke: None,
                        },
                    });
                    px += spacing;
                }
                py += spacing;
            }
        }
        "horz" | "ltHorz" | "dkHorz" | "narHorz" | "dashHorz" => {
            pattern_parallel(false, false, &stroke, x, y, w, h, spacing, page)
        }
        "vert" | "ltVert" | "dkVert" | "narVert" | "dashVert" => {
            pattern_parallel(true, false, &stroke, x, y, w, h, spacing, page)
        }
        "dnDiag" | "ltDnDiag" | "dkDnDiag" | "wdDnDiag" | "dashDnDiag" => {
            pattern_parallel(false, true, &stroke, x, y, w, h, spacing, page)
        }
        "upDiag" | "ltUpDiag" | "dkUpDiag" | "wdUpDiag" | "dashUpDiag" => {
            pattern_parallel(true, true, &stroke, x, y, w, h, spacing, page)
        }
        "diagCross" => {
            pattern_parallel(false, true, &stroke, x, y, w, h, spacing, page);
            pattern_parallel(true, true, &stroke, x, y, w, h, spacing, page);
        }
        "cross" => {
            pattern_parallel(false, false, &stroke, x, y, w, h, spacing, page);
            pattern_parallel(true, false, &stroke, x, y, w, h, spacing, page);
        }
        "smCheck" | "lgCheck" => pattern_checker(color, x, y, w, h, spacing, page),
        "smGrid" | "lgGrid" => {
            pattern_parallel(false, false, &stroke, x, y, w, h, spacing, page);
            pattern_parallel(true, false, &stroke, x, y, w, h, spacing, page);
        }
        "dotGrid" | "smConfetti" | "lgConfetti" | "sphere" | "divot" => {
            pattern_dots(color, x, y, w, h, spacing, page)
        }
        "horzBrick" | "plaid" => {
            pattern_parallel(false, false, &stroke, x, y, w, h, spacing, page);
            pattern_parallel(true, false, &stroke, x, y, w, h, spacing * 2.0, page);
        }
        "diagBrick" | "weave" | "trellis" => {
            pattern_parallel(false, true, &stroke, x, y, w, h, spacing, page);
            pattern_parallel(true, true, &stroke, x, y, w, h, spacing * 2.0, page);
        }
        "solidDmnd" | "openDmnd" | "dotDmnd" => {
            pattern_parallel(false, true, &stroke, x, y, w, h, spacing, page);
            pattern_parallel(true, true, &stroke, x, y, w, h, spacing, page);
            if preset == "solidDmnd" {
                pattern_dots(color, x, y, w, h, spacing, page);
            }
        }
        "shingle" | "wave" | "zigZag" => pattern_zigzag(&stroke, x, y, w, h, spacing, page),
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn pattern_dots(color: Rgba, x: f64, y: f64, w: f64, h: f64, spacing: f64, page: &mut Page) {
    let mut row = 0usize;
    let mut py = y + spacing / 2.0;
    while py < y + h {
        let offset = if row.is_multiple_of(2) {
            0.0
        } else {
            spacing / 2.0
        };
        let mut px = x + spacing / 2.0 + offset;
        while px < x + w {
            page.items.push(Positioned {
                x_pt: px,
                y_pt: py,
                item: Item::Rect {
                    w_pt: 1.4,
                    h_pt: 1.4,
                    fill: Some(color),
                    stroke: None,
                },
            });
            px += spacing;
        }
        row += 1;
        py += spacing;
    }
}

#[allow(clippy::too_many_arguments)]
fn pattern_checker(color: Rgba, x: f64, y: f64, w: f64, h: f64, spacing: f64, page: &mut Page) {
    let mut row = 0usize;
    let mut py = y;
    while py < y + h {
        let mut col = row % 2;
        let mut px = x;
        while px < x + w {
            if col.is_multiple_of(2) {
                page.items.push(Positioned {
                    x_pt: px,
                    y_pt: py,
                    item: Item::Rect {
                        w_pt: spacing.min(x + w - px),
                        h_pt: spacing.min(y + h - py),
                        fill: Some(color),
                        stroke: None,
                    },
                });
            }
            col += 1;
            px += spacing;
        }
        row += 1;
        py += spacing;
    }
}

#[allow(clippy::too_many_arguments)]
fn pattern_zigzag(stroke: &Stroke, x: f64, y: f64, w: f64, h: f64, spacing: f64, page: &mut Page) {
    let mut py = y;
    while py < y + h {
        let mut px = x;
        let mut rising = true;
        while px < x + w {
            let dx = (spacing / 2.0).min(x + w - px);
            let dy = if rising {
                spacing / 3.0
            } else {
                -spacing / 3.0
            };
            page.items.push(Positioned {
                x_pt: px,
                y_pt: py + if rising { 0.0 } else { spacing / 3.0 },
                item: Item::Line {
                    dx_pt: dx,
                    dy_pt: dy,
                    stroke: stroke.clone(),
                },
            });
            rising = !rising;
            px += dx;
        }
        py += spacing;
    }
}

#[allow(clippy::too_many_arguments)]
fn pattern_parallel(
    vertical_or_up: bool,
    diagonal: bool,
    stroke: &Stroke,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    spacing: f64,
    page: &mut Page,
) {
    if diagonal {
        let mut offset = -h;
        while offset < w {
            let (sx, sy, dx, dy) = if vertical_or_up {
                (
                    x + offset.max(0.0),
                    y + (h + offset.min(0.0)),
                    h.min(w - offset.max(0.0)),
                    -h.min(w - offset.max(0.0)),
                )
            } else {
                (
                    x + offset.max(0.0),
                    y + (-offset).max(0.0),
                    h.min(w - offset.max(0.0)),
                    h.min(w - offset.max(0.0)),
                )
            };
            page.items.push(Positioned {
                x_pt: sx,
                y_pt: sy,
                item: Item::Line {
                    dx_pt: dx,
                    dy_pt: dy,
                    stroke: stroke.clone(),
                },
            });
            offset += spacing;
        }
    } else if vertical_or_up {
        let mut px = x;
        while px <= x + w {
            page.items.push(Positioned {
                x_pt: px,
                y_pt: y,
                item: Item::Line {
                    dx_pt: 0.0,
                    dy_pt: h,
                    stroke: stroke.clone(),
                },
            });
            px += spacing;
        }
    } else {
        let mut py = y;
        while py <= y + h {
            page.items.push(Positioned {
                x_pt: x,
                y_pt: py,
                item: Item::Line {
                    dx_pt: w,
                    dy_pt: 0.0,
                    stroke: stroke.clone(),
                },
            });
            py += spacing;
        }
    }
}

fn gradient_color(stops: &[(f64, Rgba)], t: f64) -> Rgba {
    let right = stops.partition_point(|(position, _)| *position < t);
    if right == 0 {
        return stops[0].1;
    }
    if right == stops.len() {
        return stops[stops.len() - 1].1;
    }
    let (p0, c0) = stops[right - 1];
    let (p1, c1) = stops[right];
    let mix = if p1 > p0 { (t - p0) / (p1 - p0) } else { 0.0 };
    let lerp = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * mix).round() as u8;
    Rgba {
        r: lerp(c0.r, c1.r),
        g: lerp(c0.g, c1.g),
        b: lerp(c0.b, c1.b),
        a: 1.0,
    }
}

fn transform_new_items(page: &mut Page, start: usize, matrix: [f64; 6]) {
    if is_identity(matrix) {
        return;
    }
    for positioned in &mut page.items[start..] {
        positioned.item = Item::Transform {
            matrix,
            item: Box::new(positioned.item.clone()),
        };
    }
}

fn shape_matrix(x: f64, y: f64, xfrm: Xfrm) -> [f64; 6] {
    around_center(
        x + xfrm.w_pt / 2.0,
        y + xfrm.h_pt / 2.0,
        xfrm.rot_deg,
        xfrm.flip_h,
        xfrm.flip_v,
    )
}

fn group_matrix(
    xfrm: Option<Xfrm>,
    child_space: Option<(Xfrm, Xfrm)>,
    offset: (f64, f64),
) -> [f64; 6] {
    let Some(group) = xfrm else {
        return [1.0, 0.0, 0.0, 1.0, offset.0, offset.1];
    };
    let base = if let Some((child_offset, child_extent)) = child_space
        && child_extent.w_pt > 0.0
        && child_extent.h_pt > 0.0
    {
        let sx = group.w_pt / child_extent.w_pt;
        let sy = group.h_pt / child_extent.h_pt;
        [
            sx,
            0.0,
            0.0,
            sy,
            offset.0 + group.x_pt - child_offset.x_pt * sx,
            offset.1 + group.y_pt - child_offset.y_pt * sy,
        ]
    } else {
        [
            1.0,
            0.0,
            0.0,
            1.0,
            offset.0 + group.x_pt,
            offset.1 + group.y_pt,
        ]
    };
    let own = around_center(
        offset.0 + group.x_pt + group.w_pt / 2.0,
        offset.1 + group.y_pt + group.h_pt / 2.0,
        group.rot_deg,
        group.flip_h,
        group.flip_v,
    );
    compose(own, base)
}

fn around_center(cx: f64, cy: f64, rotation: f64, flip_h: bool, flip_v: bool) -> [f64; 6] {
    let radians = rotation.to_radians();
    let (sin, cos) = radians.sin_cos();
    let sx = if flip_h { -1.0 } else { 1.0 };
    let sy = if flip_v { -1.0 } else { 1.0 };
    let a = cos * sx;
    let b = sin * sx;
    let c = -sin * sy;
    let d = cos * sy;
    [a, b, c, d, cx - a * cx - c * cy, cy - b * cx - d * cy]
}

fn compose(left: [f64; 6], right: [f64; 6]) -> [f64; 6] {
    let [a1, b1, c1, d1, e1, f1] = left;
    let [a2, b2, c2, d2, e2, f2] = right;
    [
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    ]
}

fn is_identity(matrix: [f64; 6]) -> bool {
    let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    matrix
        .iter()
        .zip(identity)
        .all(|(value, expected)| (value - expected).abs() < 1e-9)
}

fn outline_stroke(rs: &ResolvedSlide, props: &ShapeProps) -> Option<Stroke> {
    props.line.as_ref().map(|line| Stroke {
        width_pt: line.width_pt.max(0.25),
        color: resolve_colorspec(&rs.theme, &line.color),
        dash: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn place_text(
    doc: &PptxDoc,
    rs: &ResolvedSlide,
    rels: &HashMap<String, String>,
    body: &TextBody,
    category: Category,
    x: f64,
    y: f64,
    xfrm: Xfrm,
    warnings: &mut Vec<Warning>,
    page: &mut Page,
) -> Result<()> {
    let content_w = (xfrm.w_pt - body.inset_lr.0 - body.inset_lr.1).max(1.0);
    let layout = layout_textbody(
        &doc.fonts,
        &rs.theme,
        &rs.text_defaults,
        warnings,
        body,
        category,
        content_w,
        |rel_id| doc.link_target(rels, rel_id),
    )?;
    let free = (xfrm.h_pt - body.inset_tb.0 - body.inset_tb.1 - layout.height_pt).max(0.0);
    let dy = match body.anchor.as_deref() {
        Some("ctr") => free / 2.0,
        Some("b") => free,
        _ => 0.0,
    };
    let ox = x + body.inset_lr.0;
    let oy = y + body.inset_tb.0 + dy;
    for li in layout.items {
        page.items.push(Positioned {
            x_pt: ox + li.x_pt,
            y_pt: oy + li.y_pt,
            item: li.item,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_table(
    doc: &PptxDoc,
    rs: &ResolvedSlide,
    rels: &HashMap<String, String>,
    table: &PmlTable,
    x0: f64,
    y0: f64,
    warnings: &mut Vec<Warning>,
    page: &mut Page,
) -> Result<()> {
    let border = Stroke {
        width_pt: 0.75,
        color: Rgba::opaque(Rgb::new(0, 0, 0)),
        dash: None,
    };
    let row_heights: Vec<f64> = table
        .rows
        .iter()
        .map(|row| {
            if row.height_pt > 0.0 {
                row.height_pt
            } else {
                24.0
            }
        })
        .collect();
    if let Some(fill) = &table.fill {
        let width = table.col_widths.iter().sum::<f64>().max(48.0);
        let height = row_heights.iter().sum::<f64>();
        render_fill(doc, rs, rels, fill, x0, y0, width, height, page)?;
    }
    let mut y = y0;
    for (row_index, row) in table.rows.iter().enumerate() {
        let mut x = x0;
        let mut col = 0usize;
        let row_h = row_heights[row_index];
        for cell in &row.cells {
            let span = cell.grid_span.max(1) as usize;
            let w: f64 = table
                .col_widths
                .iter()
                .skip(col)
                .take(span)
                .sum::<f64>()
                .max(48.0);
            if cell.h_merge || cell.v_merge {
                x += w;
                col += span;
                continue;
            }
            let cell_h = row_heights
                .iter()
                .skip(row_index)
                .take(cell.row_span.max(1) as usize)
                .sum::<f64>();
            let style = doc.table_cell_style(table, row_index, col)?;
            // Cell fill.
            if let Some(fill) = &cell.fill {
                render_fill(doc, rs, rels, fill, x, y, w, cell_h, page)?;
            } else if let Some(fill) = style.as_ref().and_then(|style| style.fill.as_ref()) {
                render_fill(doc, rs, rels, fill, x, y, w, cell_h, page)?;
            }
            // Borders (4 edges).
            for (edge, (dx0, dy0, dx1, dy1)) in [
                (0, (0.0, 0.0, w, 0.0)),
                (2, (0.0, cell_h, w, cell_h)),
                (3, (0.0, 0.0, 0.0, cell_h)),
                (1, (w, 0.0, w, cell_h)),
            ] {
                let styled_line = style.as_ref().and_then(|style| {
                    style.border(
                        edge,
                        CellBounds {
                            row_start: row_index,
                            row_end: row_index + cell.row_span.max(1) as usize - 1,
                            col_start: col,
                            col_end: col + span - 1,
                            row_count: table.rows.len(),
                            col_count: table.col_widths.len(),
                        },
                    )
                });
                let line = if cell.border_specified[edge] {
                    cell.borders[edge].as_ref()
                } else {
                    styled_line
                };
                let edge_border = line
                    .map(|line| Stroke {
                        width_pt: line.width_pt.max(0.25),
                        color: resolve_colorspec(&rs.theme, &line.color),
                        dash: None,
                    })
                    .or_else(|| style.is_none().then(|| border.clone()));
                let Some(edge_border) = edge_border else {
                    continue;
                };
                page.items.push(Positioned {
                    x_pt: x + dx0,
                    y_pt: y + dy0,
                    item: Item::Line {
                        dx_pt: dx1 - dx0,
                        dy_pt: dy1 - dy0,
                        stroke: edge_border,
                    },
                });
            }
            // Cell text.
            if let Some(body) = &cell.text {
                let styled_body = style
                    .as_ref()
                    .map_or_else(|| body.clone(), |style| style.apply_text(body));
                let layout = layout_textbody(
                    &doc.fonts,
                    &rs.theme,
                    &rs.text_defaults,
                    warnings,
                    &styled_body,
                    Category::Other,
                    (w - cell.inset_lr.0 - cell.inset_lr.1).max(1.0),
                    |rel_id| doc.link_target(rels, rel_id),
                )?;
                let free = (cell_h - cell.inset_tb.0 - cell.inset_tb.1 - layout.height_pt).max(0.0);
                let text_y = match cell.anchor.as_deref() {
                    Some("ctr") => cell.inset_tb.0 + free / 2.0,
                    Some("b") => cell.inset_tb.0 + free,
                    _ => cell.inset_tb.0,
                };
                for li in layout.items {
                    page.items.push(Positioned {
                        x_pt: x + cell.inset_lr.0 + li.x_pt,
                        y_pt: y + text_y + li.y_pt,
                        item: li.item,
                    });
                }
            }
            x += w;
            col += span;
        }
        y += row_h;
    }
    Ok(())
}

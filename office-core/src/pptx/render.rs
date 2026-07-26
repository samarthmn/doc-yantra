//! Slide rendering: turns resolved slides into a [`LaidOutDocument`] (one page
//! per slide) of positioned primitives, then the shared PDF emitter runs.

use super::assemble::{Category, PptxDoc, ResolvedSlide};
use super::geometry::{Contour, evaluate_shape, resolve_colorspec, solid_fill_color};
use super::inherit::{resolve_text_body, resolve_xfrm, shape_category};
use super::table_style::{CellBounds, TableCellStyle};
use super::textbody::layout_textbody;
use crate::layout::{FillRule, Item, LaidOutDocument, Page, PathPaint, Positioned, Stroke};
use crate::oxml::pml::{
    Fill, LineSpec, PictureMode, PmlCell, PmlTable, ShapeElem, ShapeProps, TextBody, Xfrm,
};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::resource_limits::{PageBudget, checked_count_add, try_reserve};
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::{BTreeMap, HashMap};

/// Renders all slides into a laid-out document.
pub fn render_pptx(doc: &PptxDoc) -> Result<(LaidOutDocument, Vec<Warning>)> {
    let mut page_budget = PageBudget::production();
    render_pptx_with_page_budget(doc, &mut page_budget)
}

fn render_pptx_with_page_budget(
    doc: &PptxDoc,
    page_budget: &mut PageBudget,
) -> Result<(LaidOutDocument, Vec<Warning>)> {
    let mut warnings = Vec::new();
    let size = doc.presentation.size;
    let mut pages = Vec::new();

    for rs in &doc.slides {
        page_budget.consume("PPTX slide")?;
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
                        true,
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
                    true,
                    &mut page,
                    &mut warnings,
                )?;
            }
        }
        for elem in &rs.slide.shapes {
            render_elem(
                doc,
                rs,
                elem,
                &rs.rels,
                offset,
                true,
                &mut page,
                &mut warnings,
            )?;
        }
        pages.push(page);
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

fn render_elem(
    doc: &PptxDoc,
    rs: &ResolvedSlide,
    elem: &ShapeElem,
    rels: &HashMap<String, String>,
    offset: (f64, f64),
    quantize_picture_frame: bool,
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
                let body =
                    resolve_text_body(ph.as_ref(), body, &rs.layout, &rs.master, &rs.text_defaults);
                place_text(
                    doc,
                    rs,
                    rels,
                    &body,
                    shape_category(ph.as_ref()),
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
            let authored_xfrm = props.xfrm.unwrap_or_default();
            let xfrm = if quantize_picture_frame {
                powerpoint_web_picture_frame(authored_xfrm)
            } else {
                authored_xfrm
            };
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
                        false,
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
                render_elem(doc, rs, child, rels, (0.0, 0.0), false, page, warnings)?;
            }
            transform_new_items(page, item_start, group_matrix(*xfrm, *child_space, offset));
        }
        ShapeElem::Unsupported(_) => {} // reported by profile; convert fails first
    }
    Ok(())
}

const POWERPOINT_WEB_DEVICE_PIXEL_PT: f64 = 72.0 / 96.0;

fn powerpoint_web_picture_frame(mut xfrm: Xfrm) -> Xfrm {
    let values = [xfrm.x_pt, xfrm.y_pt, xfrm.w_pt, xfrm.h_pt];
    if values.iter().any(|value| !value.is_finite())
        || xfrm.w_pt < 0.0
        || xfrm.h_pt < 0.0
        || xfrm.rot_deg != 0.0
        || xfrm.flip_h
        || xfrm.flip_v
    {
        return xfrm;
    }
    let snap = |value: f64| {
        (value / POWERPOINT_WEB_DEVICE_PIXEL_PT).round() * POWERPOINT_WEB_DEVICE_PIXEL_PT
    };
    let left = snap(xfrm.x_pt);
    let top = snap(xfrm.y_pt);
    let right = snap(xfrm.x_pt + xfrm.w_pt);
    let bottom = snap(xfrm.y_pt + xfrm.h_pt);
    xfrm.x_pt = left;
    xfrm.y_pt = top;
    xfrm.w_pt = (right - left).max(0.0);
    xfrm.h_pt = (bottom - top).max(0.0);
    xfrm
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
    validate_table_grid(table)?;
    let mut row_heights = Vec::new();
    try_reserve(
        &mut row_heights,
        table.rows.len(),
        "PowerPoint table row heights",
    )?;
    for row in &table.rows {
        row_heights.push(if row.height_pt > 0.0 {
            row.height_pt
        } else {
            24.0
        });
    }
    let x_position_count = checked_count_add(
        "PowerPoint table column positions",
        table.col_widths.len(),
        1,
    )?;
    let mut x_positions = Vec::new();
    try_reserve(
        &mut x_positions,
        x_position_count,
        "PowerPoint table column positions",
    )?;
    x_positions.push(x0);
    for width in &table.col_widths {
        if !width.is_finite() || *width < 0.0 {
            return Err(OfficeError::Invalid(
                "PowerPoint table column width is not finite and non-negative".to_string(),
            ));
        }
        let next = x_positions.last().copied().unwrap_or(x0) + width;
        if !next.is_finite() {
            return Err(OfficeError::Invalid(
                "PowerPoint table column positions overflow finite geometry".to_string(),
            ));
        }
        x_positions.push(next);
    }
    let y_position_count =
        checked_count_add("PowerPoint table row positions", row_heights.len(), 1)?;
    let mut y_positions = Vec::new();
    try_reserve(
        &mut y_positions,
        y_position_count,
        "PowerPoint table row positions",
    )?;
    y_positions.push(y0);
    for height in &row_heights {
        let next = y_positions.last().copied().unwrap_or(y0) + height;
        if !next.is_finite() {
            return Err(OfficeError::Invalid(
                "PowerPoint table row positions overflow finite geometry".to_string(),
            ));
        }
        y_positions.push(next);
    }
    if let Some(fill) = &table.fill {
        let width = x_positions.last().copied().unwrap_or(x0) - x0;
        let height = y_positions.last().copied().unwrap_or(y0) - y0;
        render_fill(doc, rs, rels, fill, x0, y0, width, height, page)?;
    }

    let row_count = table.rows.len();
    let col_count = table.col_widths.len();
    let mut cells = Vec::new();
    let authored_cell_count = table.rows.iter().try_fold(0usize, |count, row| {
        checked_count_add("PowerPoint table cells", count, row.cells.len())
    })?;
    try_reserve(&mut cells, authored_cell_count, "PowerPoint table cells")?;
    for (row_index, row) in table.rows.iter().enumerate() {
        let mut col = 0usize;
        for cell in &row.cells {
            let next_col = col.checked_add(1).ok_or_else(|| {
                OfficeError::Invalid("PowerPoint table cell cursor overflows usize".to_string())
            })?;
            if next_col > col_count {
                return Err(OfficeError::Invalid(
                    "PowerPoint table row has more cells than a:tblGrid columns".to_string(),
                ));
            }
            let span = usize::try_from(cell.grid_span.max(1)).map_err(|_| {
                OfficeError::Invalid("PowerPoint table gridSpan exceeds usize".to_string())
            })?;
            let col_end = col.checked_add(span).ok_or_else(|| {
                OfficeError::Invalid("PowerPoint table column span overflows usize".to_string())
            })?;
            if col_end > col_count {
                return Err(OfficeError::Invalid(
                    "PowerPoint table cell span exceeds a:tblGrid".to_string(),
                ));
            }
            if cell.h_merge || cell.v_merge {
                col = next_col;
                continue;
            }
            let row_span = usize::try_from(cell.row_span.max(1)).map_err(|_| {
                OfficeError::Invalid("PowerPoint table rowSpan exceeds usize".to_string())
            })?;
            let row_end = row_index.checked_add(row_span).ok_or_else(|| {
                OfficeError::Invalid("PowerPoint table row span overflows usize".to_string())
            })?;
            if row_end > row_count {
                return Err(OfficeError::Invalid(
                    "PowerPoint table cell span exceeds its row count".to_string(),
                ));
            }
            let style = doc.table_cell_style(table, row_index, col)?;
            cells.push(TableRenderCell {
                cell,
                style,
                bounds: CellBounds {
                    row_start: row_index,
                    row_end: row_end - 1,
                    col_start: col,
                    col_end: col_end - 1,
                    row_count,
                    col_count,
                },
                x: x_positions[col],
                y: y_positions[row_index],
                width: x_positions[col_end] - x_positions[col],
                height: y_positions[row_end] - y_positions[row_index],
            });
            col = next_col;
        }
    }

    // DrawingML paints cell backgrounds first. Borders are resolved on the
    // shared physical grid and painted once afterwards, so a later cell fill
    // cannot erase an earlier cell's right/bottom edge.
    for rendered in &cells {
        let cell = rendered.cell;
        let style = rendered.style.as_ref();
        if let Some(fill) = &cell.fill {
            render_fill(
                doc,
                rs,
                rels,
                fill,
                rendered.x,
                rendered.y,
                rendered.width,
                rendered.height,
                page,
            )?;
        } else if let Some(fill) = style.and_then(|style| style.fill.as_ref()) {
            render_fill(
                doc,
                rs,
                rels,
                fill,
                rendered.x,
                rendered.y,
                rendered.width,
                rendered.height,
                page,
            )?;
        }
    }

    let mut edges = BTreeMap::new();
    for rendered in &cells {
        for edge in 0..4 {
            let candidate = table_edge_candidate(rendered, edge, rs);
            match edge {
                0 => {
                    for segment in rendered.bounds.col_start..=rendered.bounds.col_end {
                        insert_table_edge(
                            &mut edges,
                            TableEdgeKey::Horizontal(rendered.bounds.row_start, segment),
                            candidate.clone(),
                        );
                    }
                }
                1 => {
                    for segment in rendered.bounds.row_start..=rendered.bounds.row_end {
                        insert_table_edge(
                            &mut edges,
                            TableEdgeKey::Vertical(segment, rendered.bounds.col_end + 1),
                            candidate.clone(),
                        );
                    }
                }
                2 => {
                    for segment in rendered.bounds.col_start..=rendered.bounds.col_end {
                        insert_table_edge(
                            &mut edges,
                            TableEdgeKey::Horizontal(rendered.bounds.row_end + 1, segment),
                            candidate.clone(),
                        );
                    }
                }
                3 => {
                    for segment in rendered.bounds.row_start..=rendered.bounds.row_end {
                        insert_table_edge(
                            &mut edges,
                            TableEdgeKey::Vertical(segment, rendered.bounds.col_start),
                            candidate.clone(),
                        );
                    }
                }
                _ => {}
            }
        }
    }
    render_table_edges(&edges, &x_positions, &y_positions, page);

    for rendered in &cells {
        let cell = rendered.cell;
        let style = rendered.style.as_ref();
        if let Some(body) = &cell.text {
            let styled_body = style.map_or_else(|| body.clone(), |style| style.apply_text(body));
            let layout = layout_textbody(
                &doc.fonts,
                &rs.theme,
                &rs.text_defaults,
                warnings,
                &styled_body,
                Category::Other,
                (rendered.width - cell.inset_lr.0 - cell.inset_lr.1).max(1.0),
                |rel_id| doc.link_target(rels, rel_id),
            )?;
            let free =
                (rendered.height - cell.inset_tb.0 - cell.inset_tb.1 - layout.height_pt).max(0.0);
            let text_y = match cell.anchor.as_deref() {
                Some("ctr") => cell.inset_tb.0 + free / 2.0,
                Some("b") => cell.inset_tb.0 + free,
                _ => cell.inset_tb.0,
            };
            for li in layout.items {
                page.items.push(Positioned {
                    x_pt: rendered.x + cell.inset_lr.0 + li.x_pt,
                    y_pt: rendered.y + text_y + li.y_pt,
                    item: li.item,
                });
            }
        }
    }
    Ok(())
}

/// Validates DrawingML's physical-cell merge encoding before layout.
///
/// Every `a:tr` contains one `a:tc` for each `a:tblGrid` slot. A horizontal
/// anchor's `gridSpan` covers the following physical cells, which must carry
/// `hMerge`; a vertical anchor's `rowSpan` similarly requires `vMerge`
/// continuations. Rejecting gaps and overlaps keeps malformed producer input
/// from being painted as overlapping visible cells.
pub(crate) fn validate_table_grid(table: &PmlTable) -> Result<()> {
    #[derive(Clone, Copy, Default)]
    struct ActiveVerticalMerge {
        until: usize,
        col_start: usize,
        col_end: usize,
    }

    let row_count = table.rows.len();
    let col_count = table.col_widths.len();
    let mut active_vertical = Vec::new();
    try_reserve(
        &mut active_vertical,
        col_count,
        "PowerPoint table merge validation",
    )?;
    active_vertical.resize(col_count, ActiveVerticalMerge::default());
    let mut pending_vertical = Vec::new();
    try_reserve(
        &mut pending_vertical,
        col_count,
        "PowerPoint table merge validation",
    )?;

    for (row_index, row) in table.rows.iter().enumerate() {
        if row.cells.len() != col_count {
            return Err(OfficeError::Invalid(format!(
                "PowerPoint table row {row_index} has {} physical cells for {col_count} a:tblGrid columns",
                row.cells.len()
            )));
        }
        let mut horizontal_until = 0usize;
        let mut horizontal_start = 0usize;
        let mut horizontal_row_span = 1usize;
        pending_vertical.clear();
        for (col, cell) in row.cells.iter().enumerate() {
            if let Some(detail) = cell.unsupported.first() {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::UnknownElement,
                    detail.clone(),
                ));
            }
            let span = usize::try_from(cell.grid_span.max(1)).map_err(|_| {
                OfficeError::Invalid("PowerPoint table gridSpan exceeds usize".to_string())
            })?;
            let col_end = col.checked_add(span).ok_or_else(|| {
                OfficeError::Invalid("PowerPoint table column span overflows usize".to_string())
            })?;
            if col_end > col_count {
                return Err(OfficeError::Invalid(
                    "PowerPoint table cell span exceeds a:tblGrid".to_string(),
                ));
            }
            let row_span = usize::try_from(cell.row_span.max(1)).map_err(|_| {
                OfficeError::Invalid("PowerPoint table rowSpan exceeds usize".to_string())
            })?;
            let row_end = row_index.checked_add(row_span).ok_or_else(|| {
                OfficeError::Invalid("PowerPoint table row span overflows usize".to_string())
            })?;
            if row_end > row_count {
                return Err(OfficeError::Invalid(
                    "PowerPoint table cell span exceeds its row count".to_string(),
                ));
            }

            let expected_h_merge = col < horizontal_until;
            if cell.h_merge != expected_h_merge {
                return Err(OfficeError::Invalid(format!(
                    "PowerPoint table cell ({row_index},{col}) has an inconsistent hMerge continuation"
                )));
            }
            if expected_h_merge {
                if span != 1 {
                    return Err(OfficeError::Invalid(
                        "PowerPoint table hMerge continuation gridSpan must be one".to_string(),
                    ));
                }
                if row_span != horizontal_row_span {
                    return Err(OfficeError::Invalid(
                        "PowerPoint table hMerge continuation rowSpan differs from its anchor"
                            .to_string(),
                    ));
                }
            } else {
                horizontal_start = col;
                horizontal_until = col_end;
                horizontal_row_span = row_span;
            }

            let active = active_vertical[col];
            let expected_v_merge = active.until > row_index;
            if cell.v_merge != expected_v_merge {
                return Err(OfficeError::Invalid(format!(
                    "PowerPoint table cell ({row_index},{col}) has an inconsistent vMerge continuation"
                )));
            }
            if expected_v_merge && row_span != 1 {
                return Err(OfficeError::Invalid(
                    "PowerPoint table vMerge continuation rowSpan must be one".to_string(),
                ));
            }
            if expected_v_merge
                && (active.col_start != horizontal_start || active.col_end != horizontal_until)
            {
                return Err(OfficeError::Invalid(
                    "PowerPoint table vMerge continuation gridSpan differs from its anchor"
                        .to_string(),
                ));
            }
            if !cell.h_merge && !cell.v_merge && row_span > 1 {
                pending_vertical.push((col, col_end, row_end));
            }
        }
        // A rectangular merge's top-row hMerge continuations are not vMerge
        // continuations. Activate rowSpan coverage only after validating that
        // complete physical row.
        for &(col_start, col_end, row_end) in &pending_vertical {
            for continuation in &mut active_vertical[col_start..col_end] {
                if continuation.until > row_index {
                    return Err(OfficeError::Invalid(
                        "PowerPoint table merge regions overlap".to_string(),
                    ));
                }
                *continuation = ActiveVerticalMerge {
                    until: row_end,
                    col_start,
                    col_end,
                };
            }
        }
    }
    Ok(())
}

struct TableRenderCell<'a> {
    cell: &'a PmlCell,
    style: Option<TableCellStyle>,
    bounds: CellBounds,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TableEdgeKey {
    Horizontal(usize, usize),
    Vertical(usize, usize),
}

#[derive(Clone)]
enum TableEdgePaint {
    NoFill,
    Stroke(Stroke),
}

#[derive(Clone)]
struct TableEdgeCandidate {
    paint: TableEdgePaint,
}

fn table_edge_candidate(
    rendered: &TableRenderCell<'_>,
    edge: usize,
    rs: &ResolvedSlide,
) -> TableEdgeCandidate {
    let (specified, line) = if rendered.cell.border_specified[edge] {
        (true, rendered.cell.borders[edge].as_ref())
    } else if let Some(style) = &rendered.style {
        style.border(edge, rendered.bounds)
    } else {
        (false, None)
    };
    let fallback = match rendered.style.as_ref() {
        None => Some(Stroke {
            // Preserve the engine's legacy no-style table geometry.
            width_pt: 0.75,
            color: Rgba::opaque(Rgb::new(0, 0, 0)),
            dash: None,
        }),
        Some(style) if style.inherited_base_grid => Some(Stroke {
            // Sparse Medium Style 2 package entries in Microsoft-produced
            // files resolve to this measured 1 pt theme-dark grid.
            width_pt: 1.0,
            color: Rgba::opaque(rs.theme.color("dk1").unwrap_or(Rgb::new(0, 0, 0))),
            dash: None,
        }),
        Some(_) => None,
    };
    if line.is_none() && (specified || fallback.is_none()) {
        return TableEdgeCandidate {
            paint: TableEdgePaint::NoFill,
        };
    }
    let stroke = if let Some(line) = line {
        Stroke {
            width_pt: line.width_pt.max(0.25),
            color: resolve_colorspec(&rs.theme, &line.color),
            dash: None,
        }
    } else if let Some(fallback) = fallback {
        fallback
    } else {
        return TableEdgeCandidate {
            paint: TableEdgePaint::NoFill,
        };
    };
    TableEdgeCandidate {
        paint: TableEdgePaint::Stroke(stroke),
    }
}

fn insert_table_edge(
    edges: &mut BTreeMap<TableEdgeKey, TableEdgeCandidate>,
    key: TableEdgeKey,
    candidate: TableEdgeCandidate,
) {
    let replace = edges
        .get(&key)
        .is_none_or(|existing| table_edge_precedes(existing, &candidate));
    if replace {
        edges.insert(key, candidate);
    }
}

fn table_edge_precedes(existing: &TableEdgeCandidate, candidate: &TableEdgeCandidate) -> bool {
    match (&existing.paint, &candidate.paint) {
        (TableEdgePaint::NoFill, TableEdgePaint::Stroke(_)) => true,
        (TableEdgePaint::Stroke(_), TableEdgePaint::NoFill) => false,
        (TableEdgePaint::Stroke(existing), TableEdgePaint::Stroke(candidate)) => {
            candidate.width_pt > existing.width_pt
                || ((candidate.width_pt - existing.width_pt).abs() < 1e-9
                    && table_border_brightness(candidate.color)
                        < table_border_brightness(existing.color))
        }
        (TableEdgePaint::NoFill, TableEdgePaint::NoFill) => false,
    }
}

fn table_border_brightness(color: Rgba) -> (u16, u16, u16) {
    let red = u16::from(color.r);
    let green = u16::from(color.g);
    let blue = u16::from(color.b);
    (red + blue + 2 * green, blue + 2 * green, green)
}

fn same_table_stroke(left: &Stroke, right: &Stroke) -> bool {
    (left.width_pt - right.width_pt).abs() < 1e-9
        && left.color == right.color
        && left.dash == right.dash
}

fn render_table_edges(
    edges: &BTreeMap<TableEdgeKey, TableEdgeCandidate>,
    xs: &[f64],
    ys: &[f64],
    page: &mut Page,
) {
    for (row_boundary, &y) in ys.iter().enumerate() {
        let mut col = 0usize;
        while col + 1 < xs.len() {
            let Some(TableEdgeCandidate {
                paint: TableEdgePaint::Stroke(stroke),
                ..
            }) = edges.get(&TableEdgeKey::Horizontal(row_boundary, col))
            else {
                col += 1;
                continue;
            };
            let start = col;
            col += 1;
            while col + 1 < xs.len()
                && matches!(
                    edges.get(&TableEdgeKey::Horizontal(row_boundary, col)),
                    Some(TableEdgeCandidate {
                        paint: TableEdgePaint::Stroke(next),
                        ..
                    }) if same_table_stroke(stroke, next)
                )
            {
                col += 1;
            }
            page.items.push(Positioned {
                x_pt: xs[start],
                y_pt: y,
                item: Item::Line {
                    dx_pt: xs[col] - xs[start],
                    dy_pt: 0.0,
                    stroke: stroke.clone(),
                },
            });
        }
    }
    for (col_boundary, &x) in xs.iter().enumerate() {
        let mut row = 0usize;
        while row + 1 < ys.len() {
            let Some(TableEdgeCandidate {
                paint: TableEdgePaint::Stroke(stroke),
                ..
            }) = edges.get(&TableEdgeKey::Vertical(row, col_boundary))
            else {
                row += 1;
                continue;
            };
            let start = row;
            row += 1;
            while row + 1 < ys.len()
                && matches!(
                    edges.get(&TableEdgeKey::Vertical(row, col_boundary)),
                    Some(TableEdgeCandidate {
                        paint: TableEdgePaint::Stroke(next),
                        ..
                    }) if same_table_stroke(stroke, next)
                )
            {
                row += 1;
            }
            page.items.push(Positioned {
                x_pt: x,
                y_pt: ys[start],
                item: Item::Line {
                    dx_pt: 0.0,
                    dy_pt: ys[row] - ys[start],
                    stroke: stroke.clone(),
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_limits::PageBudget;
    use crate::testkit::zip_package;
    use crate::{ConvertOptions, OfficeError, UnsupportedCode};

    fn three_slide_fixture() -> Vec<u8> {
        let content_types = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/></Types>"#;
        let root_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#;
        let presentation = br#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/><p:sldId id="258" r:id="rId3"/></p:sldIdLst></p:presentation>"#;
        let presentation_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide3.xml"/></Relationships>"#;
        let slide = br#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree/></p:cSld></p:sld>"#;
        zip_package(&[
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("ppt/presentation.xml", presentation),
            ("ppt/_rels/presentation.xml.rels", presentation_rels),
            ("ppt/slides/slide1.xml", slide),
            ("ppt/slides/slide2.xml", slide),
            ("ppt/slides/slide3.xml", slide),
        ])
    }

    #[test]
    fn page_budget_refuses_third_slide_before_rendering_it() {
        let bytes = three_slide_fixture();
        let doc = PptxDoc::open(&bytes, &ConvertOptions::default()).unwrap();
        let mut budget = PageBudget::new(2);
        let error = render_pptx_with_page_budget(&doc, &mut budget).unwrap_err();
        assert_eq!(budget.used(), 2);
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ));
    }

    #[test]
    fn picture_frame_edges_snap_independently_to_powerpoint_web_device_pixels() {
        let xfrm = Xfrm {
            x_pt: 50.4,
            y_pt: 129.6,
            w_pt: 619.2,
            h_pt: 348.3,
            ..Xfrm::default()
        };
        let snapped = powerpoint_web_picture_frame(xfrm);
        assert!((snapped.x_pt - 50.25).abs() < 0.001);
        assert!((snapped.y_pt - 129.75).abs() < 0.001);
        assert!((snapped.w_pt - 619.5).abs() < 0.001);
        assert!((snapped.h_pt - 348.0).abs() < 0.001);

        let rotated = Xfrm {
            rot_deg: 15.0,
            ..xfrm
        };
        let unchanged = powerpoint_web_picture_frame(rotated);
        assert!((unchanged.x_pt - rotated.x_pt).abs() < f64::EPSILON);
        assert!((unchanged.w_pt - rotated.w_pt).abs() < f64::EPSILON);
    }
}

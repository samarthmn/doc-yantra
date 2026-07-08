//! Slide rendering: turns resolved slides into a [`LaidOutDocument`] (one page
//! per slide) of positioned primitives, then the shared PDF emitter runs.

use super::assemble::{Category, PptxDoc, ResolvedSlide};
use super::geometry::{resolve_colorspec, solid_fill_color};
use super::inherit::{category, resolve_xfrm};
use super::textbody::layout_textbody;
use crate::layout::{Item, LaidOutDocument, Page, Positioned, Stroke};
use crate::oxml::pml::{PmlTable, ShapeElem, ShapeProps, TextBody, Xfrm};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;

/// Renders all slides into a laid-out document.
pub fn render_pptx(doc: &PptxDoc) -> (LaidOutDocument, Vec<Warning>) {
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
        let bg = rs
            .slide
            .background
            .as_ref()
            .or(rs.layout.background.as_ref())
            .or(doc.master.background.as_ref());
        if let Some(fill) = bg
            && let Some(color) = solid_fill_color(&doc.theme, fill)
        {
            page.items.push(Positioned {
                x_pt: 0.0,
                y_pt: 0.0,
                item: Item::Rect {
                    w_pt: size.w_pt,
                    h_pt: size.h_pt,
                    fill: Some(color),
                    stroke: None,
                },
            });
        }

        let offset = (0.0, 0.0);
        for elem in &rs.slide.shapes {
            render_elem(doc, rs, elem, offset, &mut page, &mut warnings);
        }
        pages.push(page);
    }

    let laid = LaidOutDocument {
        pages,
        images: doc.images.clone(),
        outline: Vec::new(),
        dates: doc.dates.clone(),
    };
    (laid, warnings)
}

fn render_elem(
    doc: &PptxDoc,
    rs: &ResolvedSlide,
    elem: &ShapeElem,
    offset: (f64, f64),
    page: &mut Page,
    warnings: &mut Vec<Warning>,
) {
    match elem {
        ShapeElem::Shape { ph, props, text } => {
            let ph_ref = ph.clone().unwrap_or_default();
            let xfrm = resolve_xfrm(&ph_ref, props.xfrm, &rs.layout, &doc.master);
            let x = xfrm.x_pt + offset.0;
            let y = xfrm.y_pt + offset.1;

            // Fill.
            if let Some(fill) = &props.fill
                && let Some(color) = solid_fill_color(&doc.theme, fill)
            {
                page.items.push(Positioned {
                    x_pt: x,
                    y_pt: y,
                    item: Item::Rect {
                        w_pt: xfrm.w_pt,
                        h_pt: xfrm.h_pt,
                        fill: Some(color),
                        stroke: outline_stroke(doc, props),
                    },
                });
            } else if let Some(stroke) = outline_stroke(doc, props) {
                page.items.push(Positioned {
                    x_pt: x,
                    y_pt: y,
                    item: Item::Rect {
                        w_pt: xfrm.w_pt,
                        h_pt: xfrm.h_pt,
                        fill: None,
                        stroke: Some(stroke),
                    },
                });
            }

            // Text.
            if let Some(body) = text
                && !body.paragraphs.is_empty()
            {
                place_text(doc, body, category(&ph_ref), x, y, xfrm, warnings, page);
            }
        }
        ShapeElem::Picture { props, drawing } => {
            let xfrm = props.xfrm.unwrap_or_default();
            if let Some(rel_id) = &drawing.blip_rel_id
                && let Some(image_id) = doc.image_id(&rs.rels, rel_id)
            {
                page.items.push(Positioned {
                    x_pt: xfrm.x_pt + offset.0,
                    y_pt: xfrm.y_pt + offset.1,
                    item: Item::Image {
                        w_pt: xfrm.w_pt,
                        h_pt: xfrm.h_pt,
                        image_id,
                    },
                });
            }
        }
        ShapeElem::Table { xfrm, table } => {
            let x = xfrm.map(|x| x.x_pt).unwrap_or(60.0) + offset.0;
            let y = xfrm.map(|x| x.y_pt).unwrap_or(150.0) + offset.1;
            render_table(doc, table, x, y, warnings, page);
        }
        ShapeElem::Group { xfrm, children, .. } => {
            let go = xfrm.map(|x| (x.x_pt, x.y_pt)).unwrap_or((0.0, 0.0));
            let child_offset = (offset.0 + go.0, offset.1 + go.1);
            for child in children {
                render_elem(doc, rs, child, child_offset, page, warnings);
            }
        }
        ShapeElem::Unsupported(_) => {} // reported by profile; convert fails first
    }
}

fn outline_stroke(doc: &PptxDoc, props: &ShapeProps) -> Option<Stroke> {
    props.line.as_ref().map(|(color, width)| Stroke {
        width_pt: (*width).max(0.25),
        color: resolve_colorspec(&doc.theme, color),
        dash: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn place_text(
    doc: &PptxDoc,
    body: &TextBody,
    category: Category,
    x: f64,
    y: f64,
    xfrm: Xfrm,
    warnings: &mut Vec<Warning>,
    page: &mut Page,
) {
    let content_w = (xfrm.w_pt - body.inset_lr.0 - body.inset_lr.1).max(1.0);
    let layout = layout_textbody(
        &doc.fonts,
        &doc.theme,
        &doc.text_defaults,
        warnings,
        body,
        category,
        content_w,
    );
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
}

fn render_table(
    doc: &PptxDoc,
    table: &PmlTable,
    x0: f64,
    y0: f64,
    warnings: &mut Vec<Warning>,
    page: &mut Page,
) {
    let border = Stroke {
        width_pt: 0.75,
        color: Rgba::opaque(Rgb::new(0, 0, 0)),
        dash: None,
    };
    let mut y = y0;
    for row in &table.rows {
        let mut x = x0;
        let mut col = 0usize;
        let row_h = if row.height_pt > 0.0 {
            row.height_pt
        } else {
            24.0
        };
        for cell in &row.cells {
            if cell.h_merge {
                col += 1;
                continue;
            }
            let span = cell.grid_span.max(1) as usize;
            let w: f64 = table
                .col_widths
                .iter()
                .skip(col)
                .take(span)
                .sum::<f64>()
                .max(48.0);
            // Cell fill.
            if let Some(fill) = &cell.fill
                && let Some(color) = solid_fill_color(&doc.theme, fill)
            {
                page.items.push(Positioned {
                    x_pt: x,
                    y_pt: y,
                    item: Item::Rect {
                        w_pt: w,
                        h_pt: row_h,
                        fill: Some(color),
                        stroke: None,
                    },
                });
            }
            // Borders (4 edges).
            for (dx0, dy0, dx1, dy1) in [
                (0.0, 0.0, w, 0.0),
                (0.0, row_h, w, row_h),
                (0.0, 0.0, 0.0, row_h),
                (w, 0.0, w, row_h),
            ] {
                page.items.push(Positioned {
                    x_pt: x + dx0,
                    y_pt: y + dy0,
                    item: Item::Line {
                        dx_pt: dx1 - dx0,
                        dy_pt: dy1 - dy0,
                        stroke: border.clone(),
                    },
                });
            }
            // Cell text.
            if let Some(body) = &cell.text {
                let layout = layout_textbody(
                    &doc.fonts,
                    &doc.theme,
                    &doc.text_defaults,
                    warnings,
                    body,
                    Category::Other,
                    (w - 7.2).max(1.0),
                );
                for li in layout.items {
                    page.items.push(Positioned {
                        x_pt: x + 3.6 + li.x_pt,
                        y_pt: y + 3.6 + li.y_pt,
                        item: li.item,
                    });
                }
            }
            x += w;
            col += span;
        }
        y += row_h;
    }
}

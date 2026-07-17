//! Recursive box layout for the supported OMML structural subset.

use super::assemble::WordDoc;
use super::paragraph::LineItem;
use super::{GlyphRun, Item, Stroke};
use crate::Result;
use crate::fonts::matching;
use crate::oxml::omml::{MathContent, MathNode};
use crate::oxml::{Rgb, Rgba};
use crate::text::shape;

/// Laid-out equation dimensions and primitives.
pub struct EquationBox {
    /// Width in points.
    pub width: f64,
    /// Height in points.
    pub height: f64,
    /// Baseline from the top.
    pub baseline: f64,
    /// Positioned drawing primitives.
    pub items: Vec<LineItem>,
}

/// Lays out an equation using the document font database and fallback pipeline.
pub fn layout_equation(doc: &WordDoc, math: &MathContent, size: f64) -> Result<EquationBox> {
    let context = MathContext {
        display: math.display,
    };
    layout_node(doc, &math.expression, size, false, context)
}

/// Layout context threaded through the expression tree.
#[derive(Clone, Copy)]
struct MathContext {
    /// Display equations draw n-ary operators at Word's large display
    /// variant size; inline math keeps the compact text-style operator.
    display: bool,
}

fn layout_node(
    doc: &WordDoc,
    node: &MathNode,
    size: f64,
    upright: bool,
    context: MathContext,
) -> Result<EquationBox> {
    match node {
        MathNode::Sequence(nodes) => sequence(doc, nodes, size, upright, context),
        MathNode::Run(text) => text_box(doc, text, size, upright),
        MathNode::Fraction {
            numerator,
            denominator,
            bar,
        } => {
            let numerator = layout_node(doc, numerator, size * 0.76, false, context)?;
            let denominator = layout_node(doc, denominator, size * 0.76, false, context)?;
            let width = numerator.width.max(denominator.width) + 4.0;
            let line_y = numerator.height + 1.5;
            let denominator_y = line_y + 2.5;
            let mut items = Vec::new();
            append(
                &mut items,
                numerator.items,
                (width - numerator.width) / 2.0,
                0.0,
            );
            if *bar {
                items.push(LineItem {
                    x_pt: 0.5,
                    y_pt: line_y,
                    item: Item::Line {
                        dx_pt: width - 1.0,
                        dy_pt: 0.0,
                        stroke: black_stroke(0.7),
                    },
                });
            }
            append(
                &mut items,
                denominator.items,
                (width - denominator.width) / 2.0,
                denominator_y,
            );
            Ok(EquationBox {
                width,
                height: denominator_y + denominator.height,
                baseline: line_y + 1.0,
                items,
            })
        }
        MathNode::Script {
            base,
            subscript,
            superscript,
        } => {
            let base = layout_node(doc, base, size, false, context)?;
            let sub = subscript
                .as_deref()
                .map(|node| layout_node(doc, node, size * 0.65, false, context))
                .transpose()?;
            let sup = superscript
                .as_deref()
                .map(|node| layout_node(doc, node, size * 0.65, false, context))
                .transpose()?;
            let sup_height = sup.as_ref().map_or(0.0, |value| value.height * 0.8);
            let base_y = sup_height;
            let script_x = base.width + 0.8;
            let script_width = sub
                .as_ref()
                .map_or(0.0, |value| value.width)
                .max(sup.as_ref().map_or(0.0, |value| value.width));
            let mut items = Vec::new();
            append(&mut items, base.items, 0.0, base_y);
            if let Some(sup) = sup {
                append(&mut items, sup.items, script_x, 0.0);
            }
            if let Some(sub) = sub {
                append(
                    &mut items,
                    sub.items,
                    script_x,
                    base_y + base.baseline + 1.0,
                );
            }
            let height = items_height(&items).max(base_y + base.height);
            Ok(EquationBox {
                width: base.width + 0.8 + script_width,
                height,
                baseline: base_y + base.baseline,
                items,
            })
        }
        MathNode::Radical { degree, radicand } => {
            let radicand = layout_node(doc, radicand, size, false, context)?;
            let radical = text_box(doc, "√", size * 1.25, true)?;
            let degree = degree
                .as_deref()
                .map(|node| layout_node(doc, node, size * 0.5, false, context))
                .transpose()?;
            let degree_width = degree.as_ref().map_or(0.0, |value| value.width * 0.75);
            let radical_x = degree_width;
            let body_x = radical_x + radical.width - 1.0;
            let top = 1.5;
            let baseline = top + radical.baseline.max(radicand.baseline);
            let mut items = Vec::new();
            append(
                &mut items,
                radical.items,
                radical_x,
                baseline - radical.baseline,
            );
            append(
                &mut items,
                radicand.items,
                body_x,
                baseline - radicand.baseline,
            );
            if let Some(degree) = degree {
                append(&mut items, degree.items, 0.0, 0.0);
            }
            items.push(LineItem {
                x_pt: body_x,
                y_pt: top,
                item: Item::Line {
                    dx_pt: radicand.width + 1.0,
                    dy_pt: 0.0,
                    stroke: black_stroke(0.6),
                },
            });
            Ok(EquationBox {
                width: body_x + radicand.width + 1.0,
                height: items_height(&items).max(baseline + size * 0.25),
                baseline,
                items,
            })
        }
        MathNode::Nary {
            operator,
            lower,
            upper,
            operand,
        } => {
            // Word draws display-style n-ary operators with the large glyph
            // variant (~2x base, calibrated against Word CJK ground truths);
            // inline math keeps the compact text-style operator.
            let operator_scale = if context.display { 2.0 } else { 1.35 };
            let operator = text_box(doc, &operator.to_string(), size * operator_scale, true)?;
            let lower = lower
                .as_deref()
                .map(|node| layout_node(doc, node, size * 0.58, false, context))
                .transpose()?;
            let upper = upper
                .as_deref()
                .map(|node| layout_node(doc, node, size * 0.58, false, context))
                .transpose()?;
            let operand = layout_node(doc, operand, size, false, context)?;
            let upper_h = upper.as_ref().map_or(0.0, |value| value.height);
            let operator_y = upper_h;
            let baseline = operator_y + operator.baseline;
            let operator_width = operator
                .width
                .max(lower.as_ref().map_or(0.0, |value| value.width))
                .max(upper.as_ref().map_or(0.0, |value| value.width));
            let mut items = Vec::new();
            if let Some(upper) = upper {
                append(
                    &mut items,
                    upper.items,
                    (operator_width - upper.width) / 2.0,
                    0.0,
                );
            }
            append(
                &mut items,
                operator.items,
                (operator_width - operator.width) / 2.0,
                operator_y,
            );
            if let Some(lower) = lower {
                append(
                    &mut items,
                    lower.items,
                    (operator_width - lower.width) / 2.0,
                    operator_y + operator.height,
                );
            }
            append(
                &mut items,
                operand.items,
                operator_width + 2.0,
                baseline - operand.baseline,
            );
            Ok(EquationBox {
                width: operator_width + 2.0 + operand.width,
                height: items_height(&items),
                baseline,
                items,
            })
        }
        MathNode::Delimiter { open, close, body } => {
            let body = layout_node(doc, body, size, false, context)?;
            let delimiter_size = size.max((body.height * 0.9).min(size * 4.0));
            let open = text_box(doc, open, delimiter_size, true)?;
            let close = text_box(doc, close, delimiter_size, true)?;
            combine(vec![open, body, close])
        }
        MathNode::Function { name, argument } => {
            let name = layout_node(doc, name, size, true, context)?;
            let argument = layout_node(doc, argument, size, false, context)?;
            combine(vec![name, spacer(2.0, size), argument])
        }
        MathNode::Unsupported(detail) => Err(crate::OfficeError::unsupported(
            crate::UnsupportedCode::Equation,
            detail.clone(),
        )),
    }
}

fn sequence(
    doc: &WordDoc,
    nodes: &[MathNode],
    size: f64,
    upright: bool,
    context: MathContext,
) -> Result<EquationBox> {
    let boxes = nodes
        .iter()
        .map(|node| layout_node(doc, node, size, upright, context))
        .collect::<Result<Vec<_>>>()?;
    combine(boxes)
}

fn combine(boxes: Vec<EquationBox>) -> Result<EquationBox> {
    let baseline = boxes.iter().map(|value| value.baseline).fold(0.0, f64::max);
    let descent = boxes
        .iter()
        .map(|value| value.height - value.baseline)
        .fold(0.0, f64::max);
    let mut x = 0.0;
    let mut items = Vec::new();
    for value in boxes {
        append(&mut items, value.items, x, baseline - value.baseline);
        x += value.width;
    }
    Ok(EquationBox {
        width: x,
        height: baseline + descent,
        baseline,
        items,
    })
}

fn text_box(doc: &WordDoc, text: &str, size: f64, upright: bool) -> Result<EquationBox> {
    if text.is_empty() {
        return Ok(spacer(0.0, size));
    }
    let italic = !upright && text.chars().any(char::is_alphabetic);
    let resolved = matching::resolve(&doc.fonts, "Cambria Math", 400, italic);
    let face = resolved.face.ok_or_else(|| {
        crate::OfficeError::unsupported(
            crate::UnsupportedCode::MissingGlyph,
            "no font face available for equation",
        )
    })?;
    let fallback_ids = matching::coverage_fallbacks(&doc.fonts, face, 400, italic);
    let fallback_faces: Vec<_> = fallback_ids
        .iter()
        .filter_map(|face| doc.fonts.face(*face))
        .collect();
    let shaped = shape::require_glyphs(shape::shape_with_fallback(
        text,
        &fallback_faces,
        size,
        0.0,
        false,
    ))?;
    let mapped = crate::text::semantic::map_shaped_run(text, &shaped);
    let mut semantics = Some(mapped.semantic);
    let baseline = size * 0.82;
    let mut cursor = 0.0;
    let mut items = Vec::new();
    for (index, (glyph, visible)) in shaped.glyphs.iter().zip(mapped.visible).enumerate() {
        let Some(&glyph_face) = fallback_ids.get(glyph.face_index) else {
            continue;
        };
        let mut glyphs = vec![visible];
        if index == 0 {
            glyphs.extend(semantics.take().unwrap_or_default());
        }
        items.push(LineItem {
            x_pt: cursor,
            y_pt: baseline,
            item: Item::Glyphs(GlyphRun {
                face: glyph_face,
                size_pt: size,
                color: Rgba::opaque(Rgb::new(0, 0, 0)),
                base_direction: None,
                glyphs,
                underline: None,
                strike: None,
            }),
        });
        cursor += glyph.advance_pt;
    }
    Ok(EquationBox {
        width: shaped.width_pt,
        height: size * 1.15,
        baseline,
        items,
    })
}

fn append(target: &mut Vec<LineItem>, source: Vec<LineItem>, x: f64, y: f64) {
    target.extend(source.into_iter().map(|item| LineItem {
        x_pt: item.x_pt + x,
        y_pt: item.y_pt + y,
        item: item.item,
    }));
}

fn items_height(items: &[LineItem]) -> f64 {
    items
        .iter()
        .map(|item| match &item.item {
            Item::Glyphs(run) => item.y_pt + run.size_pt * 0.3,
            Item::Line { dy_pt, .. } => item.y_pt + dy_pt.abs(),
            Item::Rect { h_pt, .. } | Item::Image { h_pt, .. } => item.y_pt + h_pt,
            _ => item.y_pt,
        })
        .fold(0.0, f64::max)
}

fn spacer(width: f64, size: f64) -> EquationBox {
    EquationBox {
        width,
        height: size,
        baseline: size * 0.8,
        items: Vec::new(),
    }
}

fn black_stroke(width_pt: f64) -> Stroke {
    Stroke {
        width_pt,
        color: Rgba::opaque(Rgb::new(0, 0, 0)),
        dash: None,
    }
}

#[cfg(all(test, feature = "bundled-fonts"))]
mod tests {
    use super::*;
    use crate::fonts::FontDb;

    #[test]
    fn bundled_fallback_covers_required_math_operators() {
        let fonts = FontDb::with_bundled();
        let primary = matching::resolve(&fonts, "Cambria Math", 400, false)
            .face
            .expect("bundled primary math substitute");
        let ids = matching::coverage_fallbacks(&fonts, primary, 400, false);
        let faces: Vec<_> = ids.iter().filter_map(|id| fonts.face(*id)).collect();
        let shaped =
            shape::require_glyphs(shape::shape_with_fallback("∑∫√", &faces, 12.0, 0.0, false))
                .expect("packaged fonts must cover required OMML operators");
        assert_eq!(shaped.glyphs.len(), 3);
    }
}

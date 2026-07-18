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
            // OpenType MATH fraction geometry: display style keeps full-size
            // components with the display shift constants; inline scales to
            // script size with the text-style shifts (Cambria Math values).
            let comp_scale = if context.display { 1.0 } else { 0.73 };
            let (num_shift_em, den_shift_em) = if context.display {
                (0.7568, 0.6689)
            } else {
                (0.5859, 0.5029)
            };
            let numerator = layout_node(doc, numerator, size * comp_scale, false, context)?;
            let denominator = layout_node(doc, denominator, size * comp_scale, false, context)?;
            let width = numerator.width.max(denominator.width) + 4.0;
            let axis = 0.2856 * size;
            // Box baseline sits at the fraction's outer baseline; numerator
            // baseline is `num_shift` above it, denominator `den_shift` below.
            let ascent = (num_shift_em * size + numerator.baseline).max(axis + 1.0);
            let line_y = ascent - axis;
            let denominator_y = ascent + den_shift_em * size - denominator.baseline;
            let numerator_y = (ascent - num_shift_em * size - numerator.baseline).max(0.0);
            let mut items = Vec::new();
            append(
                &mut items,
                numerator.items,
                (width - numerator.width) / 2.0,
                numerator_y,
            );
            if *bar {
                items.push(LineItem {
                    x_pt: 0.5,
                    y_pt: line_y,
                    item: Item::Line {
                        dx_pt: width - 1.0,
                        dy_pt: 0.0,
                        stroke: black_stroke(0.0649 * size),
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
                height: (denominator_y + denominator.height).max(ascent + 1.0),
                baseline: ascent,
                items,
            })
        }
        MathNode::Script {
            base,
            subscript,
            superscript,
        } => {
            // OpenType MATH geometry (Cambria Math constants): scripts at
            // ScriptPercentScaleDown, superscript baseline raised by
            // SuperscriptShiftUp, subscript dropped by SubscriptShiftDown.
            const SCRIPT_SCALE: f64 = 0.73;
            const SUP_SHIFT_UP_EM: f64 = 0.3662;
            const SUB_SHIFT_DOWN_EM: f64 = 0.2041;
            const SPACE_AFTER_SCRIPT_EM: f64 = 0.0415;
            let base = layout_node(doc, base, size, false, context)?;
            let sub = subscript
                .as_deref()
                .map(|node| layout_node(doc, node, size * SCRIPT_SCALE, false, context))
                .transpose()?;
            let sup = superscript
                .as_deref()
                .map(|node| layout_node(doc, node, size * SCRIPT_SCALE, false, context))
                .transpose()?;
            // Ascent of the box above the base baseline.
            let sup_rise = sup
                .as_ref()
                .map_or(0.0, |s| SUP_SHIFT_UP_EM * size + s.baseline);
            let ascent = base.baseline.max(sup_rise);
            let base_y = ascent - base.baseline;
            let sub_drop = sub.as_ref().map_or(0.0, |s| {
                SUB_SHIFT_DOWN_EM * size + (s.height - s.baseline)
            });
            let descent = (base.height - base.baseline).max(sub_drop);
            let script_x = base.width + 0.4;
            let script_width = sub
                .as_ref()
                .map_or(0.0, |value| value.width)
                .max(sup.as_ref().map_or(0.0, |value| value.width));
            let mut items = Vec::new();
            append(&mut items, base.items, 0.0, base_y);
            if let Some(sup) = sup {
                let sup_baseline_y = ascent - SUP_SHIFT_UP_EM * size;
                append(&mut items, sup.items, script_x, sup_baseline_y - sup.baseline);
            }
            if let Some(sub) = sub {
                let sub_baseline_y = ascent + SUB_SHIFT_DOWN_EM * size;
                append(&mut items, sub.items, script_x, sub_baseline_y - sub.baseline);
            }
            let height = items_height(&items).max(ascent + descent);
            Ok(EquationBox {
                width: base.width + 0.4 + script_width + SPACE_AFTER_SCRIPT_EM * size,
                height,
                baseline: ascent,
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
            let operator_scale = if context.display { 1.75 } else { 1.35 };
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

/// Maps a letter to its Mathematical Italic alphanumeric codepoint (the
/// glyphs Word uses for default math-style letters). `h` is the Planck
/// exception (U+1D455 is unassigned).
fn math_italic_char(c: char) -> Option<char> {
    let cp = match c {
        'h' => 0x210E,
        'a'..='z' => 0x1D44E + (c as u32 - 'a' as u32),
        'A'..='Z' => 0x1D434 + (c as u32 - 'A' as u32),
        _ => return None,
    };
    char::from_u32(cp)
}

fn text_box(doc: &WordDoc, text: &str, size: f64, upright: bool) -> Result<EquationBox> {
    if text.is_empty() {
        return Ok(spacer(0.0, size));
    }
    let wants_italic = !upright && text.chars().any(char::is_alphabetic);
    // Word draws default math-italic letters with the math face's own
    // Mathematical Alphanumeric glyphs (Cambria Math ships no italic
    // variant). Map letters the upright face covers; when the resolved
    // face has no math alphabet (e.g. the Caladea substitute), fall back
    // to the italic-styled face with plain letters.
    let mut resolved = matching::resolve(&doc.fonts, "Cambria Math", 400, false);
    let mut italic = false;
    let mut owned;
    let mut text = text;
    if wants_italic {
        let mapped = resolved
            .face
            .and_then(|id| doc.fonts.face(id))
            .and_then(|face| face.parsed())
            .map(|parsed| {
                text.chars()
                    .map(|c| {
                        math_italic_char(c)
                            .filter(|mc| parsed.glyph_index(*mc).is_some())
                            .unwrap_or(c)
                    })
                    .collect::<String>()
            });
        match mapped {
            Some(m) if m != text => {
                owned = m;
                text = &owned;
            }
            _ => {
                resolved = matching::resolve(&doc.fonts, "Cambria Math", 400, true);
                italic = true;
            }
        }
    }
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

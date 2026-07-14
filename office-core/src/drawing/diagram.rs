//! Resolution of pre-rendered SmartArt diagram drawing parts.

use crate::fonts::{FontDb, matching};
use crate::layout::{FillRule, GlyphRun, Item, PathPaint, Positioned};
use crate::opc::Package;
use crate::oxml::pml::{Fill, ShapeElem, Xfrm, parse_diagram_drawing};
use crate::oxml::theme::Theme;
use crate::oxml::{Rgb, Rgba};
use crate::pptx::geometry::{evaluate_shape, resolve_colorspec};
use crate::text::shape;
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

/// A resolved `dsp:drawing` plus relationships scoped to that drawing part.
#[derive(Debug, Clone)]
pub struct DiagramFallback {
    /// Parsed fallback shape tree.
    pub shapes: Vec<ShapeElem>,
    /// Relationship map of the `diagrams/drawing*.xml` part.
    pub rels: HashMap<String, String>,
}

impl DiagramFallback {
    /// Renders the resolved shape fallback into frame-relative primitives.
    pub fn render(
        &self,
        fonts: &FontDb,
        theme: &Theme,
        width: f64,
        height: f64,
    ) -> Result<Vec<Positioned>> {
        render_shapes(&self.shapes, fonts, theme, width, height)
    }
}

/// Package-wide SmartArt data-part to drawing-fallback index.
#[derive(Debug, Default)]
pub struct DiagramStore {
    drawings: HashMap<String, DiagramFallback>,
    errors: HashMap<String, String>,
}

fn render_shapes(
    shapes: &[ShapeElem],
    fonts: &FontDb,
    theme: &Theme,
    width: f64,
    height: f64,
) -> Result<Vec<Positioned>> {
    let mut items = Vec::new();
    for shape in shapes {
        match shape {
            ShapeElem::Shape { props, text, .. } => {
                if let Some(detail) = props.unsupported.first() {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::SmartArt,
                        detail.clone(),
                    ));
                }
                let xfrm = props.xfrm.unwrap_or(Xfrm {
                    w_pt: width,
                    h_pt: height,
                    present: true,
                    ..Xfrm::default()
                });
                let contours = evaluate_shape(props, xfrm).map_err(|error| match error {
                    OfficeError::Unsupported { detail, .. } => {
                        OfficeError::unsupported(UnsupportedCode::SmartArt, detail)
                    }
                    other => other,
                })?;
                let commands = contours
                    .iter()
                    .flat_map(|contour| contour.commands.iter().copied())
                    .collect();
                let fill = match props.fill.as_ref() {
                    Some(Fill::Solid(color)) => Some(resolve_colorspec(theme, color)),
                    Some(Fill::None) | None => None,
                    Some(Fill::Unsupported(error)) => {
                        return Err(OfficeError::unsupported(error.code, error.detail.clone()));
                    }
                    Some(_) => {
                        return Err(OfficeError::unsupported(
                            UnsupportedCode::SmartArt,
                            "SmartArt fallback uses unsupported non-solid fill",
                        ));
                    }
                };
                let stroke = props.line.as_ref().map(|line| crate::layout::Stroke {
                    width_pt: line.width_pt,
                    color: resolve_colorspec(theme, &line.color),
                    dash: None,
                });
                items.push(Positioned {
                    x_pt: xfrm.x_pt,
                    y_pt: xfrm.y_pt,
                    item: Item::Path {
                        commands,
                        paint: PathPaint {
                            fill,
                            stroke,
                            fill_rule: FillRule::NonZero,
                        },
                    },
                });
                if let Some(text) = text {
                    let source = basic_text_source(text)?;
                    if !source.is_empty() {
                        items.extend(render_text(
                            fonts,
                            &source,
                            12.0,
                            xfrm.x_pt + xfrm.w_pt / 2.0,
                            xfrm.y_pt + xfrm.h_pt / 2.0 + 4.0,
                        )?);
                    }
                }
            }
            ShapeElem::Group {
                children,
                xfrm,
                child_space,
            } => {
                let mut nested = render_shapes(children, fonts, theme, width, height)?;
                let matrix = diagram_group_matrix(*xfrm, *child_space);
                for positioned in &mut nested {
                    positioned.item = Item::Transform {
                        matrix,
                        item: Box::new(positioned.item.clone()),
                    };
                }
                items.extend(nested);
            }
            ShapeElem::Picture { .. }
            | ShapeElem::Table { .. }
            | ShapeElem::Chart { .. }
            | ShapeElem::SmartArt { .. }
            | ShapeElem::OlePreview { .. }
            | ShapeElem::Unsupported(_) => {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::SmartArt,
                    "SmartArt drawing fallback contains a non-shape object",
                ));
            }
        }
    }
    Ok(items)
}

fn render_text(
    fonts: &FontDb,
    text: &str,
    size: f64,
    center_x: f64,
    baseline: f64,
) -> Result<Vec<Positioned>> {
    let face = matching::resolve(fonts, "Calibri", 400, false)
        .face
        .ok_or_else(|| {
            OfficeError::unsupported(UnsupportedCode::MissingGlyph, "SmartArt text has no font")
        })?;
    let fallback_ids = matching::coverage_fallbacks(fonts, face, 400, false);
    let fallback_faces: Vec<_> = fallback_ids
        .iter()
        .filter_map(|id| fonts.face(*id))
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
    let mut cursor = center_x - shaped.width_pt / 2.0;
    let mut items = Vec::new();
    for (index, (glyph, visible)) in shaped.glyphs.iter().zip(mapped.visible).enumerate() {
        let face = *fallback_ids.get(glyph.face_index).ok_or_else(|| {
            OfficeError::unsupported(
                UnsupportedCode::MissingGlyph,
                "SmartArt text fallback face is unavailable",
            )
        })?;
        let mut glyphs = vec![visible];
        if index == 0 {
            glyphs.extend(semantics.take().unwrap_or_default());
        }
        items.push(Positioned {
            x_pt: cursor,
            y_pt: baseline,
            item: Item::Glyphs(GlyphRun {
                face,
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
    Ok(items)
}

fn basic_text_source(text: &crate::oxml::pml::TextBody) -> Result<String> {
    if text.paragraphs.len() > 1
        || text.anchor.is_some()
        || text.font_scale.is_some()
        || !text.unsupported.is_empty()
    {
        return Err(OfficeError::unsupported(
            UnsupportedCode::SmartArt,
            "DOCX SmartArt fallback uses styled or multi-paragraph text",
        ));
    }
    let Some(paragraph) = text.paragraphs.first() else {
        return Ok(String::new());
    };
    if paragraph.level != 0
        || paragraph.align.is_some()
        || paragraph.rtl
        || paragraph.bullet_char.is_some()
        || paragraph.bullet_auto_num.is_some()
        || !paragraph.unsupported.is_empty()
        || paragraph.runs.iter().any(|run| {
            run.bold.is_some()
                || run.italic.is_some()
                || run.underline
                || run.size_pt.is_some()
                || run.typeface.is_some()
                || run.color.is_some()
                || run.hlink.is_some()
                || !run.unsupported.is_empty()
        })
    {
        return Err(OfficeError::unsupported(
            UnsupportedCode::SmartArt,
            "DOCX SmartArt fallback text formatting is unsupported",
        ));
    }
    Ok(paragraph.runs.iter().map(|run| run.text.as_str()).collect())
}

fn diagram_group_matrix(xfrm: Option<Xfrm>, child_space: Option<(Xfrm, Xfrm)>) -> [f64; 6] {
    let Some(group) = xfrm else {
        return [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    };
    if let Some((offset, extent)) = child_space
        && extent.w_pt > 0.0
        && extent.h_pt > 0.0
    {
        let sx = group.w_pt / extent.w_pt;
        let sy = group.h_pt / extent.h_pt;
        return [
            sx,
            0.0,
            0.0,
            sy,
            group.x_pt - offset.x_pt * sx,
            group.y_pt - offset.y_pt * sy,
        ];
    }
    [1.0, 0.0, 0.0, 1.0, group.x_pt, group.y_pt]
}

impl DiagramStore {
    /// Resolves every `diagrams/data*.xml` part to its declared drawing fallback.
    pub fn load(package: &Package) -> Result<Self> {
        let mut store = DiagramStore::default();
        let data_parts: Vec<String> = package
            .part_names()
            .filter(|part| part.contains("/diagrams/data") && part.ends_with(".xml"))
            .map(str::to_string)
            .collect();
        for data_part in data_parts {
            let bytes = package.part(&data_part).ok_or_else(|| {
                OfficeError::Invalid(format!("missing SmartArt data part {data_part}"))
            })?;
            let xml = std::str::from_utf8(bytes).map_err(|_| {
                OfficeError::Invalid(format!("SmartArt data part {data_part} is not UTF-8"))
            })?;
            let doc = crate::oxml::parse_xml(xml, "SmartArt data")?;
            let fallback_rel = doc
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "dataModelExt")
                .and_then(|node| node.attribute("relId"))
                .filter(|value| !value.is_empty());
            let Some(fallback_rel) = fallback_rel else {
                store.errors.insert(
                    data_part,
                    "SmartArt data part has no pre-resolved drawing fallback".to_string(),
                );
                continue;
            };
            let relationships = package.relationships(&data_part)?;
            let Some(drawing_part) = relationships
                .iter()
                .find(|relationship| relationship.id == fallback_rel)
                .map(|relationship| relationship.target.clone())
            else {
                store.errors.insert(
                    data_part,
                    format!("SmartArt drawing fallback relationship {fallback_rel} is missing"),
                );
                continue;
            };
            let Some(drawing_bytes) = package.part(&drawing_part) else {
                store.errors.insert(
                    data_part,
                    format!("SmartArt drawing fallback part {drawing_part} is missing"),
                );
                continue;
            };
            let drawing_xml = std::str::from_utf8(drawing_bytes).map_err(|_| {
                OfficeError::Invalid(format!(
                    "SmartArt drawing fallback {drawing_part} is not UTF-8"
                ))
            })?;
            match parse_diagram_drawing(drawing_xml) {
                Ok(shapes) => {
                    let rels = package
                        .relationships(&drawing_part)?
                        .into_iter()
                        .map(|relationship| (relationship.id, relationship.target))
                        .collect();
                    store
                        .drawings
                        .insert(data_part, DiagramFallback { shapes, rels });
                }
                Err(OfficeError::Unsupported { detail, .. }) => {
                    store.errors.insert(data_part, detail);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(store)
    }

    /// Resolves a source-part relationship to its diagram fallback.
    pub fn drawing<'a>(
        &'a self,
        rels: &HashMap<String, String>,
        rel_id: &str,
    ) -> Option<&'a DiagramFallback> {
        self.drawings.get(rels.get(rel_id)?)
    }

    /// Returns the stable typed reason for an unresolved diagram relationship.
    pub fn error(
        &self,
        rels: &HashMap<String, String>,
        rel_id: &str,
    ) -> Option<(UnsupportedCode, String)> {
        let Some(data_part) = rels.get(rel_id) else {
            return Some((
                UnsupportedCode::SmartArt,
                format!("SmartArt data relationship {rel_id} is missing"),
            ));
        };
        self.errors
            .get(data_part)
            .cloned()
            .map(|detail| (UnsupportedCode::SmartArt, detail))
            .or_else(|| {
                (!self.drawings.contains_key(data_part)).then(|| {
                    (
                        UnsupportedCode::SmartArt,
                        format!("SmartArt data part {data_part} has no drawing fallback"),
                    )
                })
            })
    }
}

//! XLSX assembly: opens a workbook and resolves each visible sheet's model,
//! shared strings, styles, theme, fonts, and dates.

use crate::drawing::chart::Chart;
use crate::drawing::image::{add_to_budget, decode_required, validate_crop_units};
use crate::fonts::FontDb;
use crate::layout::{DocDates, EncodedImage, ImageId};
use crate::opc::Package;
use crate::oxml::pml::ColorSpec;
use crate::oxml::sml::{SharedStrings, Styles, Workbook, Worksheet, parse_a1, parse_range};
use crate::oxml::theme::{Theme, parse_color_element};
use crate::{ConvertOptions, OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

const XDR: &str = "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing";
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DrawingMarker {
    pub(crate) col: u32,
    pub(crate) col_off_pt: f64,
    pub(crate) row: u32,
    pub(crate) row_off_pt: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DrawingAnchor {
    TwoCell {
        from: DrawingMarker,
        to: DrawingMarker,
    },
    OneCell {
        from: DrawingMarker,
        width_pt: f64,
        height_pt: f64,
    },
    Absolute {
        x_pt: f64,
        y_pt: f64,
        width_pt: f64,
        height_pt: f64,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum XlsxDrawing {
    Picture {
        anchor: DrawingAnchor,
        image_id: ImageId,
    },
    Shape {
        anchor: DrawingAnchor,
        preset: String,
        fill: Option<ColorSpec>,
        line: Option<(ColorSpec, f64)>,
    },
    Chart {
        anchor: DrawingAnchor,
        chart: Chart,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct DrawingReason {
    pub(crate) code: UnsupportedCode,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
struct SourceCrop {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

struct PictureRequest {
    anchor: DrawingAnchor,
    rel_id: String,
    crop: Option<SourceCrop>,
}

enum DrawingRequest {
    Picture(PictureRequest),
    Shape {
        anchor: DrawingAnchor,
        preset: String,
        fill: Option<ColorSpec>,
        line: Option<(ColorSpec, f64)>,
    },
    Chart {
        anchor: DrawingAnchor,
        rel_id: String,
    },
}

/// A resolved sheet.
pub struct ResolvedSheet {
    /// Sheet name.
    pub name: String,
    /// The worksheet model.
    pub sheet: Worksheet,
    pub(crate) drawings: Vec<XlsxDrawing>,
    pub(crate) drawing_reasons: Vec<DrawingReason>,
}

/// A fully-resolved workbook.
pub struct XlsxDoc {
    /// The workbook (date system, sheet order).
    pub workbook: Workbook,
    /// Visible (and filtered) sheets in order.
    pub sheets: Vec<ResolvedSheet>,
    /// Cell styles + custom number formats.
    pub styles: Styles,
    /// The theme.
    pub theme: Theme,
    /// Fonts.
    pub fonts: FontDb,
    /// Decoded images referenced by sheet drawings.
    pub images: Vec<EncodedImage>,
    /// PDF dates.
    pub dates: DocDates,
}

impl XlsxDoc {
    /// Opens and resolves a `.xlsx`/`.xlsm` package.
    pub fn open(bytes: &[u8], opts: &ConvertOptions) -> Result<XlsxDoc> {
        let pkg = Package::open(bytes)?;
        let wb_part = pkg
            .relationships("")?
            .into_iter()
            .find(|r| r.rel_type.ends_with("/officeDocument"))
            .map(|r| r.target)
            .unwrap_or_else(|| "xl/workbook.xml".to_string());
        // Reject a mismatched-format package (e.g. a .docx routed here): without
        // this the main part parses to zero sheets and we emit an invalid 0-page
        // PDF. The main part's content type must be spreadsheetml.
        if !pkg
            .content_type(&wb_part)
            .is_some_and(|ct| ct.contains("spreadsheetml"))
        {
            return Err(OfficeError::Invalid("main part is not a workbook".into()));
        }
        let workbook = parse(&pkg, &wb_part, Workbook::parse)?;
        let wb_rels = rel_map(&pkg, &wb_part);

        let shared =
            optional(&pkg, "xl/sharedStrings.xml", SharedStrings::parse)?.unwrap_or_default();
        let theme = optional(&pkg, "xl/theme/theme1.xml", Theme::parse)?.unwrap_or_default();
        let styles = optional(&pkg, "xl/styles.xml", |xml| {
            Styles::parse_with_theme(xml, &theme)
        })?
        .unwrap_or_default();

        let filter = opts.sheet_filter.as_ref();
        let mut sheets = Vec::new();
        let mut images = Vec::new();
        let mut image_cache = HashMap::new();
        let mut total_image_pixels = 0u64;
        for (sheet_index, sr) in workbook.sheets.iter().enumerate() {
            if !sr.visible {
                continue;
            }
            if let Some(filter) = filter
                && !filter.iter().any(|n| n == &sr.name)
            {
                continue;
            }
            let part = wb_rels.get(&sr.rel_id).ok_or_else(|| {
                OfficeError::Invalid(format!(
                    "sheet {}: missing relationship {}",
                    sr.name, sr.rel_id
                ))
            })?;
            let mut sheet = parse(&pkg, part, |xml| {
                Worksheet::parse_with_colors(xml, &shared, &theme, &styles.indexed_colors)
            })
            .map_err(|error| error.context(format_args!("sheet {}", sr.name)))?;
            apply_print_names(&mut sheet, &workbook, &sr.name, sheet_index);
            let (drawings, drawing_reasons) = resolve_drawings(
                &pkg,
                part,
                &sheet,
                &mut images,
                &mut image_cache,
                &mut total_image_pixels,
            )?;
            sheets.push(ResolvedSheet {
                name: sr.name.clone(),
                sheet,
                drawings,
                drawing_reasons,
            });
        }

        let mut fonts = FontDb::with_sources(&opts.font_dirs, opts.include_system_fonts);
        fonts.set_resolution_mode(opts.resolution_mode);
        for fb in &opts.font_bytes {
            fonts.register(fb.clone())?;
        }
        crate::register_runtime_fonts(&mut fonts)?;
        fonts.mark_hash_pinned(&opts.font_hash_pins);

        Ok(XlsxDoc {
            workbook,
            sheets,
            styles,
            theme,
            fonts,
            images,
            dates: core_dates(&pkg),
        })
    }
}

fn resolve_drawings(
    pkg: &Package,
    sheet_part: &str,
    sheet: &Worksheet,
    images: &mut Vec<EncodedImage>,
    image_cache: &mut HashMap<(String, Option<SourceCrop>), usize>,
    total_image_pixels: &mut u64,
) -> Result<(Vec<XlsxDrawing>, Vec<DrawingReason>)> {
    let Some(rel_id) = &sheet.drawing_rel_id else {
        return Ok((Vec::new(), Vec::new()));
    };
    let sheet_rels = rel_map(pkg, sheet_part);
    let Some(drawing_part) = sheet_rels.get(rel_id) else {
        return Ok((
            Vec::new(),
            vec![DrawingReason {
                code: UnsupportedCode::MissingRelationshipTarget,
                detail: format!("missing worksheet drawing relationship {rel_id}"),
            }],
        ));
    };
    let xml = pkg
        .part(drawing_part)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .ok_or_else(|| OfficeError::Invalid(format!("invalid drawing part {drawing_part}")))?;
    let (requests, reasons) = parse_drawing(xml)?;
    let drawing_rels = rel_map(pkg, drawing_part);
    let mut drawings = Vec::new();
    let mut reasons = reasons;
    for request in requests {
        if let DrawingRequest::Chart { anchor, rel_id } = request {
            let Some(chart_part) = drawing_rels.get(&rel_id) else {
                reasons.push(DrawingReason {
                    code: UnsupportedCode::Chart,
                    detail: format!("missing chart relationship {rel_id}"),
                });
                continue;
            };
            let Some(xml) = pkg
                .part(chart_part)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            else {
                reasons.push(DrawingReason {
                    code: UnsupportedCode::Chart,
                    detail: format!("missing chart part {chart_part}"),
                });
                continue;
            };
            match Chart::parse(xml) {
                Ok(chart) => drawings.push(XlsxDrawing::Chart { anchor, chart }),
                Err(OfficeError::Unsupported { code, detail }) => {
                    reasons.push(DrawingReason { code, detail });
                }
                Err(error) => return Err(error.context(chart_part)),
            }
            continue;
        }
        let DrawingRequest::Picture(request) = request else {
            if let DrawingRequest::Shape {
                anchor,
                preset,
                fill,
                line,
            } = request
            {
                drawings.push(XlsxDrawing::Shape {
                    anchor,
                    preset,
                    fill,
                    line,
                });
            }
            continue;
        };
        let Some(image_part) = drawing_rels.get(&request.rel_id) else {
            reasons.push(DrawingReason {
                code: UnsupportedCode::MissingRelationshipTarget,
                detail: format!("missing picture relationship {}", request.rel_id),
            });
            continue;
        };
        let key = (image_part.clone(), request.crop);
        let image_id = if let Some(&existing) = image_cache.get(&key) {
            existing
        } else {
            let Some(bytes) = pkg.part(image_part) else {
                reasons.push(DrawingReason {
                    code: UnsupportedCode::MissingRelationshipTarget,
                    detail: format!("missing picture part {image_part}"),
                });
                continue;
            };
            let Some(content_type) = pkg.content_type(image_part) else {
                reasons.push(DrawingReason {
                    code: UnsupportedCode::ImageContentTypeMissing,
                    detail: format!("image part {image_part} has no content type"),
                });
                continue;
            };
            let decoded = decode_required(content_type, bytes)
                .and_then(|(image, pixels)| {
                    let image = crop_xlsx_image(image, bytes, request.crop)?;
                    Ok((image, pixels))
                })
                .and_then(|decoded| {
                    add_to_budget(images.len(), total_image_pixels, decoded.1)?;
                    Ok(decoded)
                })
                .map_err(|error| error.context(image_part));
            let (image, _) = match decoded {
                Ok(decoded) => decoded,
                Err(OfficeError::Unsupported { code, detail }) => {
                    reasons.push(DrawingReason { code, detail });
                    continue;
                }
                Err(error) => return Err(error),
            };
            let id = images.len();
            images.push(image);
            image_cache.insert(key, id);
            id
        };
        drawings.push(XlsxDrawing::Picture {
            anchor: request.anchor,
            image_id: ImageId(image_id),
        });
    }
    Ok((drawings, reasons))
}

fn parse_drawing(xml: &str) -> Result<(Vec<DrawingRequest>, Vec<DrawingReason>)> {
    let doc = crate::oxml::parse_xml(xml, "xlsx drawing")?;
    let mut pictures = Vec::new();
    let mut reasons = Vec::new();
    for anchor in doc
        .root_element()
        .children()
        .filter(roxmltree::Node::is_element)
    {
        if xdr_child(anchor, "clientData").is_some_and(|client_data| {
            matches!(
                client_data.attribute("fPrintsWithSheet"),
                Some("0") | Some("false")
            )
        }) {
            continue;
        }
        let parsed_anchor = match anchor.tag_name().name() {
            "twoCellAnchor" if anchor.tag_name().namespace() == Some(XDR) => {
                let from = xdr_child(anchor, "from").map(parse_marker);
                let to = xdr_child(anchor, "to").map(parse_marker);
                from.zip(to)
                    .map(|(from, to)| DrawingAnchor::TwoCell { from, to })
            }
            "oneCellAnchor" if anchor.tag_name().namespace() == Some(XDR) => {
                let from = xdr_child(anchor, "from").map(parse_marker);
                let ext = xdr_child(anchor, "ext").map(parse_ext);
                from.zip(ext)
                    .map(|(from, (width_pt, height_pt))| DrawingAnchor::OneCell {
                        from,
                        width_pt,
                        height_pt,
                    })
            }
            "absoluteAnchor" if anchor.tag_name().namespace() == Some(XDR) => {
                let pos =
                    xdr_child(anchor, "pos").map(|node| (emu_attr(node, "x"), emu_attr(node, "y")));
                let ext = xdr_child(anchor, "ext").map(parse_ext);
                pos.zip(ext).map(
                    |((x_pt, y_pt), (width_pt, height_pt))| DrawingAnchor::Absolute {
                        x_pt,
                        y_pt,
                        width_pt,
                        height_pt,
                    },
                )
            }
            other => {
                reasons.push(DrawingReason {
                    code: UnsupportedCode::AnchoredDrawing,
                    detail: format!("unsupported xdr anchor {other}"),
                });
                continue;
            }
        };
        let Some(parsed_anchor) = parsed_anchor else {
            reasons.push(DrawingReason {
                code: UnsupportedCode::AnchoredDrawing,
                detail: "drawing anchor lacks position or extent".into(),
            });
            continue;
        };
        if !valid_anchor(parsed_anchor) {
            reasons.push(DrawingReason {
                code: UnsupportedCode::AnchoredDrawing,
                detail: "xlsx drawing anchor has a zero/reversed extent".into(),
            });
            continue;
        }
        if let Some(pic) = xdr_child(anchor, "pic") {
            if let Some(reason) = unsupported_picture_reason(pic) {
                reasons.push(reason);
                continue;
            }
            let blip_fill = xdr_child(pic, "blipFill");
            let blip = blip_fill.and_then(|fill| {
                fill.descendants().find(|node| {
                    node.is_element()
                        && node.tag_name().namespace() == Some(crate::oxml::A)
                        && node.tag_name().name() == "blip"
                })
            });
            let rel_id = blip.and_then(|node| node.attribute((crate::oxml::R, "embed")));
            let Some(rel_id) = rel_id else {
                reasons.push(DrawingReason {
                    code: UnsupportedCode::AnchoredDrawing,
                    detail: "xdr picture has no embedded raster relationship".into(),
                });
                continue;
            };
            let crop = blip_fill
                .and_then(|fill| {
                    fill.descendants().find(|node| {
                        node.is_element()
                            && node.tag_name().namespace() == Some(crate::oxml::A)
                            && node.tag_name().name() == "srcRect"
                    })
                })
                .map(parse_crop)
                .filter(|crop| *crop != SourceCrop::default());
            pictures.push(DrawingRequest::Picture(PictureRequest {
                anchor: parsed_anchor,
                rel_id: rel_id.to_string(),
                crop,
            }));
        } else if let Some(frame) = xdr_child(anchor, "graphicFrame") {
            let chart = frame.descendants().find(|node| {
                node.is_element()
                    && node.tag_name().namespace()
                        == Some("http://schemas.openxmlformats.org/drawingml/2006/chart")
                    && node.tag_name().name() == "chart"
            });
            if let Some(rel_id) = chart.and_then(|node| node.attribute((crate::oxml::R, "id"))) {
                pictures.push(DrawingRequest::Chart {
                    anchor: parsed_anchor,
                    rel_id: rel_id.to_string(),
                });
            } else {
                reasons.push(DrawingReason {
                    code: UnsupportedCode::AnchoredDrawing,
                    detail: "unsupported xlsx graphic frame".into(),
                });
            }
        } else if let Some(shape) = xdr_child(anchor, "sp") {
            match parse_xdr_shape(shape, parsed_anchor) {
                Ok(request) => pictures.push(request),
                Err(reason) => reasons.push(reason),
            }
        } else if xdr_child(anchor, "cxnSp").is_some() || xdr_child(anchor, "grpSp").is_some() {
            reasons.push(DrawingReason {
                code: UnsupportedCode::ShapeGeometry,
                detail: "grouped or connector xlsx drawing shape".into(),
            });
        } else {
            reasons.push(DrawingReason {
                code: UnsupportedCode::AnchoredDrawing,
                detail: "unsupported visible xdr drawing object".into(),
            });
        }
    }
    Ok((pictures, reasons))
}

fn valid_anchor(anchor: DrawingAnchor) -> bool {
    match anchor {
        DrawingAnchor::TwoCell { from, to } => {
            (to.row, to.row_off_pt, to.col, to.col_off_pt)
                > (from.row, from.row_off_pt, from.col, from.col_off_pt)
        }
        DrawingAnchor::OneCell {
            width_pt,
            height_pt,
            ..
        }
        | DrawingAnchor::Absolute {
            width_pt,
            height_pt,
            ..
        } => width_pt > 0.0 && height_pt > 0.0,
    }
}

fn unsupported_picture_reason(pic: roxmltree::Node<'_, '_>) -> Option<DrawingReason> {
    if xdr_child(pic, "style").is_some()
        || pic.descendants().any(|node| {
            node.is_element()
                && node.tag_name().namespace() == Some(crate::oxml::A)
                && matches!(node.tag_name().name(), "effectLst" | "effectDag" | "tile")
        })
    {
        return Some(DrawingReason {
            code: UnsupportedCode::AnchoredDrawing,
            detail: "unsupported xlsx picture style/effect/tile".into(),
        });
    }
    if pic.descendants().any(|node| {
        node.is_element()
            && node.tag_name().namespace() == Some(crate::oxml::A)
            && matches!(node.tag_name().name(), "hlinkClick" | "hlinkHover")
    }) {
        return Some(DrawingReason {
            code: UnsupportedCode::Hyperlink,
            detail: "hyperlink on xlsx picture".into(),
        });
    }
    let preset = pic
        .descendants()
        .find(|node| {
            node.is_element()
                && node.tag_name().namespace() == Some(crate::oxml::A)
                && node.tag_name().name() == "prstGeom"
        })
        .and_then(|node| node.attribute("prst"));
    if preset.is_some_and(|preset| preset != "rect") {
        return Some(DrawingReason {
            code: UnsupportedCode::ShapeGeometry,
            detail: format!(
                "non-rectangular xlsx picture frame: {}",
                preset.unwrap_or_default()
            ),
        });
    }
    None
}

fn parse_xdr_shape(
    shape: roxmltree::Node<'_, '_>,
    anchor: DrawingAnchor,
) -> std::result::Result<DrawingRequest, DrawingReason> {
    if xdr_child(shape, "style").is_some() {
        return Err(DrawingReason {
            code: UnsupportedCode::ShapeGeometry,
            detail: "theme-styled xlsx drawing shape".into(),
        });
    }
    if shape.descendants().any(|node| {
        node.is_element()
            && node.tag_name().namespace() == Some(crate::oxml::A)
            && node.tag_name().name() == "t"
            && node.text().is_some_and(|text| !text.is_empty())
    }) {
        return Err(DrawingReason {
            code: UnsupportedCode::UnknownElement,
            detail: "text inside an xlsx drawing shape".into(),
        });
    }
    let Some(properties) = xdr_child(shape, "spPr") else {
        return Err(DrawingReason {
            code: UnsupportedCode::ShapeGeometry,
            detail: "xlsx drawing shape lacks spPr".into(),
        });
    };
    if properties.children().any(|node| {
        node.is_element()
            && node.tag_name().namespace() == Some(crate::oxml::A)
            && matches!(
                node.tag_name().name(),
                "custGeom" | "gradFill" | "pattFill" | "blipFill" | "effectLst" | "effectDag"
            )
    }) {
        return Err(DrawingReason {
            code: UnsupportedCode::ShapeGeometry,
            detail: "unsupported xlsx drawing shape properties".into(),
        });
    }
    let preset = properties
        .children()
        .find(|node| {
            node.is_element()
                && node.tag_name().namespace() == Some(crate::oxml::A)
                && node.tag_name().name() == "prstGeom"
        })
        .and_then(|node| node.attribute("prst"))
        .unwrap_or("rect")
        .to_string();
    if !crate::pptx::geometry::supports_preset(&preset) {
        return Err(DrawingReason {
            code: UnsupportedCode::ShapeGeometry,
            detail: format!("unsupported xlsx preset:{preset}"),
        });
    }
    let fill = parse_solid_fill(properties);
    let line = properties
        .children()
        .find(|node| {
            node.is_element()
                && node.tag_name().namespace() == Some(crate::oxml::A)
                && node.tag_name().name() == "ln"
        })
        .and_then(|line| {
            parse_solid_fill(line).map(|color| {
                let width = line
                    .attribute("w")
                    .and_then(|value| value.parse().ok())
                    .map(crate::oxml::emu_to_pt)
                    .unwrap_or(0.75);
                (color, width)
            })
        });
    if fill.is_none() && line.is_none() {
        return Err(DrawingReason {
            code: UnsupportedCode::ShapeGeometry,
            detail: "xlsx drawing shape has no explicit supported fill or line".into(),
        });
    }
    Ok(DrawingRequest::Shape {
        anchor,
        preset,
        fill,
        line,
    })
}

fn parse_solid_fill(node: roxmltree::Node<'_, '_>) -> Option<ColorSpec> {
    let solid = node.children().find(|child| {
        child.is_element()
            && child.tag_name().namespace() == Some(crate::oxml::A)
            && child.tag_name().name() == "solidFill"
    })?;
    solid
        .children()
        .find(roxmltree::Node::is_element)
        .and_then(parse_color_element)
        .map(|(base, transforms)| ColorSpec { base, transforms })
}

fn xdr_child<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<roxmltree::Node<'a, 'a>> {
    node.children().find(|child| {
        child.is_element()
            && child.tag_name().namespace() == Some(XDR)
            && child.tag_name().name() == name
    })
}

fn parse_marker(node: roxmltree::Node<'_, '_>) -> DrawingMarker {
    DrawingMarker {
        col: xdr_text_u32(node, "col"),
        col_off_pt: crate::oxml::emu_to_pt(i64::from(xdr_text_u32(node, "colOff"))),
        row: xdr_text_u32(node, "row"),
        row_off_pt: crate::oxml::emu_to_pt(i64::from(xdr_text_u32(node, "rowOff"))),
    }
}

fn xdr_text_u32(node: roxmltree::Node<'_, '_>, name: &str) -> u32 {
    xdr_child(node, name)
        .and_then(|child| child.text())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn parse_ext(node: roxmltree::Node<'_, '_>) -> (f64, f64) {
    (emu_attr(node, "cx"), emu_attr(node, "cy"))
}

fn emu_attr(node: roxmltree::Node<'_, '_>, name: &str) -> f64 {
    crate::oxml::emu_to_pt(
        node.attribute(name)
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    )
}

fn parse_crop(node: roxmltree::Node<'_, '_>) -> SourceCrop {
    let value = |name| match node.attribute(name) {
        Some(value) => value
            .parse::<i64>()
            .ok()
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(i32::MIN),
        None => 0,
    };
    SourceCrop {
        left: value("l"),
        top: value("t"),
        right: value("r"),
        bottom: value("b"),
    }
}

fn crop_xlsx_image(
    image: EncodedImage,
    bytes: &[u8],
    crop: Option<SourceCrop>,
) -> Result<EncodedImage> {
    if let Some(crop) = crop {
        validate_crop_units([crop.left, crop.top, crop.right, crop.bottom])?;
        let image = image::load_from_memory(bytes).map_err(|error| {
            OfficeError::unsupported(
                UnsupportedCode::ImageDecodeFailure,
                format!("image cannot be decoded for cropping: {error}"),
            )
        })?;
        let width = image.width();
        let height = image.height();
        let left = (u64::from(width) * crop.left as u64 / 100_000) as u32;
        let top = (u64::from(height) * crop.top as u64 / 100_000) as u32;
        let right = (u64::from(width) * crop.right as u64 / 100_000) as u32;
        let bottom = (u64::from(height) * crop.bottom as u64 / 100_000) as u32;
        let crop_width = width.saturating_sub(left).saturating_sub(right);
        let crop_height = height.saturating_sub(top).saturating_sub(bottom);
        if crop_width == 0 || crop_height == 0 {
            return Err(OfficeError::unsupported(
                UnsupportedCode::InvalidImageCrop,
                "a:srcRect removes the entire xlsx picture",
            ));
        }
        let data = image
            .crop_imm(left, top, crop_width, crop_height)
            .into_rgba8()
            .into_raw();
        return Ok(EncodedImage::Rgba8 {
            width: crop_width,
            height: crop_height,
            data,
        });
    }
    Ok(image)
}

/// Applies Excel's built-in workbook names after the sheet is known. A local
/// name wins; a global name is accepted for producers that omit localSheetId.
fn apply_print_names(
    sheet: &mut Worksheet,
    workbook: &Workbook,
    sheet_name: &str,
    sheet_index: usize,
) {
    for name in &workbook.defined_names {
        if name.local_sheet_id.is_some_and(|id| id != sheet_index) {
            continue;
        }
        let applies_to_sheet = name.value.contains(&format!("'{}'!", sheet_name))
            || name.value.contains(&format!("{}!", sheet_name))
            || name.local_sheet_id == Some(sheet_index);
        if !applies_to_sheet {
            continue;
        }
        match name.name.as_str() {
            "_xlnm.Print_Area" | "Print_Area" => {
                let mut areas = name.value.split(',').filter_map(parse_range);
                sheet.print_area = areas.next();
                if areas.next().is_some() {
                    sheet
                        .visible_unsupported
                        .push(crate::oxml::sml::WorksheetUnsupported {
                            code: UnsupportedCode::UnknownElement,
                            detail: "multi-range xlsx print area".into(),
                        });
                }
            }
            "_xlnm.Print_Titles" | "Print_Titles" => parse_print_titles(sheet, &name.value),
            _ => {}
        }
    }
}

fn parse_print_titles(sheet: &mut Worksheet, value: &str) {
    for part in value.split(',') {
        let reference = part.rsplit('!').next().unwrap_or(part).trim();
        if let Some(rows) = reference.strip_prefix('$').and_then(|r| r.split_once(':'))
            && rows.0.chars().all(|c| c.is_ascii_digit())
            && rows
                .1
                .trim_start_matches('$')
                .chars()
                .all(|c| c.is_ascii_digit())
        {
            let first = rows
                .0
                .parse::<u32>()
                .ok()
                .and_then(|row| row.checked_sub(1));
            let last = rows
                .1
                .trim_start_matches('$')
                .parse::<u32>()
                .ok()
                .and_then(|row| row.checked_sub(1));
            if let (Some(first), Some(last)) = (first, last) {
                sheet.print_title_rows = Some((first.min(last), first.max(last)));
            }
        } else if let Some((first, last)) = reference.split_once(':') {
            let first = first
                .trim_matches('$')
                .trim_end_matches(|c: char| c.is_ascii_digit());
            let last = last
                .trim_matches('$')
                .trim_end_matches(|c: char| c.is_ascii_digit());
            if let (Some((first, _)), Some((last, _))) = (
                parse_a1(&format!("{first}1")),
                parse_a1(&format!("{last}1")),
            ) {
                sheet.print_title_cols = Some((first.min(last), first.max(last)));
            }
        }
    }
}

fn parse<T>(pkg: &Package, part: &str, f: impl Fn(&str) -> Result<T>) -> Result<T> {
    let bytes = pkg
        .part(part)
        .ok_or_else(|| OfficeError::Invalid(format!("missing part {part}")))?;
    let text = std::str::from_utf8(bytes)
        .map_err(|_| OfficeError::Invalid(format!("{part} is not UTF-8")))?;
    f(text)
}

fn optional<T>(pkg: &Package, part: &str, f: impl Fn(&str) -> Result<T>) -> Result<Option<T>> {
    match pkg.part(part) {
        Some(_) => parse(pkg, part, f).map(Some),
        None => Ok(None),
    }
}

fn rel_map(pkg: &Package, source_part: &str) -> HashMap<String, String> {
    pkg.relationships(source_part)
        .map(|rels| rels.into_iter().map(|r| (r.id, r.target)).collect())
        .unwrap_or_default()
}

fn core_dates(pkg: &Package) -> DocDates {
    let mut dates = DocDates::default();
    let Some(bytes) = pkg.part("docprops/core.xml") else {
        return dates;
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return dates;
    };
    let Ok(doc) = roxmltree::Document::parse(text) else {
        return dates;
    };
    for node in doc.root_element().descendants() {
        let d = node.text().and_then(iso_to_pdf_date);
        match node.tag_name().name() {
            "created" => {
                if let Some(d) = d {
                    dates.created = d;
                }
            }
            "modified" => {
                if let Some(d) = d {
                    dates.modified = d;
                }
            }
            _ => {}
        }
    }
    dates
}

fn iso_to_pdf_date(s: &str) -> Option<String> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    let mut out = String::from("D:");
    out.push_str(&digits[..digits.len().min(14)]);
    while out.len() < 16 {
        out.push('0');
    }
    out.push('Z');
    Some(out)
}

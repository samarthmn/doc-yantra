//! Zone 1 — watermark stamping. Mirrors DocYantra's pdf-lib algorithm
//! (doc-yantra/src/modules/pdf/pdf.service.ts) so output is visually
//! identical: single or grid pattern, rotation about each anchor, primary
//! line + smaller meta lines, opacity via ExtGState. Pure lopdf.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

use crate::embed_font::{FontChoice, FontSession, encode_winansi_literal};
use crate::error::{PdfCoreError, Result};
use crate::fonts::StandardFont;

/// Tiling pattern for watermark placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pattern {
    /// One watermark at page center (plus offset).
    #[default]
    Single,
    /// Repeated grid across the page at (x_spacing_pt, y_spacing_pt).
    Grid,
}

/// Optional image stamped alongside (hybrid) or instead of (image) text.
#[derive(Debug, Clone)]
pub struct WatermarkImage {
    /// PNG or JPEG bytes.
    pub bytes: Vec<u8>,
    /// Rendered width in points, clamped to 24..=1440 like DocYantra's
    /// parseWatermark (non-finite values fall back to 180, DocYantra's
    /// default). Adapters must default absent payload values to 180.
    pub width_pt: f64,
    /// Rendered height in points; same 24..=1440 clamp / 180 fallback as
    /// `width_pt`.
    pub height_pt: f64,
}

/// Watermark description. Defaults mirror DocYantra's payload defaults.
///
/// Known, deliberate deviations from DocYantra's pdf-lib implementation:
///
/// * **Hybrid with empty `lines`**: DocYantra's parseWatermark falls back to
///   `["CONFIDENTIAL"]` whenever the effective lines are empty and the
///   payload `contentType` is not `image` — *including* `hybrid` — which
///   also shifts the image up by the fallback text block's height. This
///   engine applies the fallback only when `image` is `None`; an
///   image-plus-empty-lines spec renders the image alone, centered. Payload
///   parsing (and injecting the CONFIDENTIAL fallback for hybrid payloads)
///   is the adapter's job.
/// * **Kerning**: line centering sums bare Helvetica AFM advances, while
///   pdf-lib's widthOfTextAtSize also applies AFM kern pairs; kerned strings
///   can sit up to ~0.2pt off DocYantra's position (visually imperceptible).
#[derive(Debug, Clone)]
pub struct WatermarkSpec {
    pub pattern: Pattern,
    pub rotation_deg: f64,
    pub x_spacing_pt: f64,
    pub y_spacing_pt: f64,
    pub offset_x_pt: f64,
    pub offset_y_pt: f64,
    /// First line is primary (full size); subsequent lines render at 0.65x.
    pub lines: Vec<String>,
    /// RGB color, sRGB 0..=255 per channel.
    pub color: [u8; 3],
    pub font_size_pt: f64,
    /// 0.05..=1.0
    pub opacity: f64,
    pub image: Option<WatermarkImage>,
}

impl Default for WatermarkSpec {
    fn default() -> Self {
        WatermarkSpec {
            pattern: Pattern::Single,
            rotation_deg: 0.0,
            x_spacing_pt: 320.0,
            y_spacing_pt: 320.0,
            offset_x_pt: 0.0,
            offset_y_pt: 0.0,
            lines: vec!["CONFIDENTIAL".to_string()],
            color: [0x4B, 0x55, 0x63],
            font_size_pt: 14.0,
            opacity: 0.65,
            image: None,
        }
    }
}

// Layout constants ported verbatim from pdf.service.ts applyWatermarkToPage.
const PRIMARY_LINE_HEIGHT_FACTOR: f64 = 1.2;
const PRIMARY_TO_META_GAP_FACTOR: f64 = 0.35;
const META_TO_META_GAP_FACTOR: f64 = 0.18;
const META_FONT_SCALE: f64 = 0.65;

// Base resource names added to each page's /Resources. When a name is
// already taken (e.g. re-watermarking) the first free suffixed variant is
// used instead: DKwmGS, DKwmGS2, DKwmGS3, ...
const FONT_RES: &str = "DKwmF";
const GS_RES: &str = "DKwmGS";
const IMG_RES: &str = "DKwmImg";

/// Stamps `spec` onto every page of `pdf` and returns the new document bytes.
/// The watermark is embedded into page content (not an annotation), so it
/// survives printing and cannot be toggled off by viewers.
///
/// Page `/Rotate` is deliberately NOT compensated: stamps are placed in
/// unrotated page space, exactly like DocYantra's pdf-lib implementation
/// (spec parity), so a /Rotate 90 page shows the watermark rotated with it.
///
/// Lines longer than the page are NOT shrunk to fit and may overflow the
/// page edges — DocYantra's pdf-lib implementation does not shrink either
/// (spec parity).
pub fn apply_watermark(pdf: &[u8], spec: &WatermarkSpec) -> Result<Vec<u8>> {
    let mut doc = Document::load_mem(pdf)?;

    // Clamps mirror DocYantra's parseWatermark defaults/limits.
    let x_spacing = clamp_or(spec.x_spacing_pt, 72.0, 1440.0, 320.0);
    let y_spacing = clamp_or(spec.y_spacing_pt, 72.0, 1440.0, 320.0);
    let primary_size = clamp_or(spec.font_size_pt, 8.0, 64.0, 14.0);
    let opacity = clamp_or(spec.opacity, 0.05, 1.0, 0.65);
    let rotation_deg = if spec.rotation_deg.is_finite() {
        spec.rotation_deg
    } else {
        0.0
    };
    let offset_x = if spec.offset_x_pt.is_finite() {
        spec.offset_x_pt
    } else {
        0.0
    };
    let offset_y = if spec.offset_y_pt.is_finite() {
        spec.offset_y_pt
    } else {
        0.0
    };

    // Trim lines, drop empties; text-only watermarks fall back to CONFIDENTIAL.
    let mut lines: Vec<String> = spec
        .lines
        .iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if spec.image.is_none() && lines.is_empty() {
        lines.push("CONFIDENTIAL".to_string());
    }
    let has_text = !lines.is_empty();

    // Embed image XObject once, shared by every page. Rendered dimensions
    // are clamped to DocYantra's 24..=1440pt range (180 = TS default when
    // the payload omits them).
    let image: Option<(ObjectId, f64, f64)> = match &spec.image {
        Some(img) => Some((
            build_image_xobject(&mut doc, img)?,
            clamp_or(img.width_pt, 24.0, 1440.0, 180.0),
            clamp_or(img.height_pt, 24.0, 1440.0, 180.0),
        )),
        None => None,
    };
    let hybrid = image.is_some() && has_text;

    let rot = rotation_deg.to_radians();
    let (sin, cos) = rot.sin_cos();

    let meta_size = clamp_or(
        primary_size * META_FONT_SCALE,
        6.0,
        primary_size,
        primary_size * META_FONT_SCALE,
    );

    struct LineMetric {
        shown: Vec<u8>,
        embedded_font: Option<usize>,
        size: f64,
        width: f64,
        line_height: f64,
        gap_before: f64,
    }

    let mut font_session = FontSession::new()?;
    let mut metrics = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let size = if i == 0 { primary_size } else { meta_size };
        let gap_before = if i == 0 {
            0.0
        } else if i == 1 {
            size * PRIMARY_TO_META_GAP_FACTOR
        } else {
            size * META_TO_META_GAP_FACTOR
        };
        let choice = font_session.resolve(line)?;
        let width = choice.text_width_pt(line, StandardFont::Helvetica, size)?;
        let (shown, embedded_font) = match choice {
            FontChoice::Standard => (escape_pdf_text(line), None),
            FontChoice::Embedded(face) => {
                let encoded = font_session.encode(&face, line)?;
                (encoded.bytes, Some(encoded.resource_index))
            }
        };
        metrics.push(LineMetric {
            shown,
            embedded_font,
            size,
            width,
            line_height: size * PRIMARY_LINE_HEIGHT_FACTOR,
            gap_before,
        });
    }
    let embedded_fonts = font_session.finish()?;
    let total_text_height: f64 = metrics.iter().map(|m| m.line_height + m.gap_before).sum();

    let hybrid_gap = clamp_or(primary_size * 0.6, 4.0, 96.0, primary_size * 0.6);
    let img_h = image.map(|(_, _, h)| h).unwrap_or(0.0);
    let image_center_y = if hybrid {
        (total_text_height + hybrid_gap) / 2.0
    } else {
        0.0
    };
    let text_center_y = if hybrid {
        -(img_h + hybrid_gap) / 2.0
    } else {
        0.0
    };

    let color = [
        f64::from(spec.color[0]) / 255.0,
        f64::from(spec.color[1]) / 255.0,
        f64::from(spec.color[2]) / 255.0,
    ];

    // Shared resource objects.
    let standard_font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let mut embedded_font_ids = Vec::with_capacity(embedded_fonts.len());
    for font in &embedded_fonts {
        embedded_font_ids.push(font.add_to_lopdf(&mut doc)?);
    }
    let gs_id = doc.add_object(dictionary! {
        "Type" => "ExtGState",
        "ca" => opacity,
        "CA" => opacity,
    });

    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for page_id in page_ids {
        let (bx, by, w, h) = page_box(&doc, page_id)?;

        // Collision-free resource names for THIS page: re-watermarking must
        // never rebind an earlier pass's /DKwmGS (etc.) to a new object, or
        // the earlier pass's ops would silently pick up the new opacity or
        // image.
        let gs_name = free_resource_name(&doc, page_id, "ExtGState", GS_RES)?;
        let standard_font_name = free_resource_name(&doc, page_id, "Font", FONT_RES)?;
        let mut embedded_font_names = Vec::with_capacity(embedded_font_ids.len());
        for index in 0..embedded_font_ids.len() {
            embedded_font_names.push(free_resource_name(
                &doc,
                page_id,
                "Font",
                &format!("{FONT_RES}{}", index + 2),
            )?);
        }
        let img_name = free_resource_name(&doc, page_id, "XObject", IMG_RES)?;

        // Anchor centers, mirroring the TS single/grid loops (shifted by the
        // MediaBox origin, which pdf-lib's page.getSize()-based math never
        // sees because it also draws relative to the page box).
        let anchors: Vec<(f64, f64)> = match spec.pattern {
            Pattern::Single => vec![(bx + w / 2.0 + offset_x, by + h / 2.0 + offset_y)],
            Pattern::Grid => {
                // Normalizing the offsets into one spacing period keeps the
                // anchor lattice identical while bounding the loop for
                // arbitrarily large offsets (offset_x = -1e9 used to grind
                // through millions of off-page anchors).
                let ox = offset_x.rem_euclid(x_spacing);
                let oy = offset_y.rem_euclid(y_spacing);
                let mut v = Vec::new();
                let mut x = bx - w + ox;
                while x <= bx + w * 2.0 {
                    let mut y = by - h + oy;
                    while y <= by + h * 2.0 {
                        v.push((x, y));
                        y += y_spacing;
                    }
                    x += x_spacing;
                }
                v
            }
        };

        let mut ops: Vec<u8> = Vec::new();
        push_ops(&mut ops, &format!("q\n/{gs_name} gs\n"));
        for (cx, cy) in anchors {
            let l2g = |lx: f64, ly: f64| (cx + lx * cos - ly * sin, cy + lx * sin + ly * cos);
            if let Some((_, iw, ih)) = image {
                // Bottom-left of the image in local space, rotated about it.
                let (ox, oy) = l2g(-iw / 2.0, image_center_y - ih / 2.0);
                push_ops(
                    &mut ops,
                    &format!(
                        "q\n{} {} {} {} {} {} cm\n/{img_name} Do\nQ\n",
                        fnum(iw * cos),
                        fnum(iw * sin),
                        fnum(-ih * sin),
                        fnum(ih * cos),
                        fnum(ox),
                        fnum(oy),
                    ),
                );
            }
            if has_text {
                let mut cursor_top = text_center_y + total_text_height / 2.0;
                for m in &metrics {
                    cursor_top -= m.gap_before;
                    cursor_top -= m.line_height;
                    let (ox, oy) = l2g(-m.width / 2.0, cursor_top);
                    push_ops(
                        &mut ops,
                        &format!(
                            "BT\n/{} {} Tf\n{} {} {} rg\n{} {} {} {} {} {} Tm\n",
                            m.embedded_font
                                .map(|index| embedded_font_names[index].as_str())
                                .unwrap_or(standard_font_name.as_str()),
                            fnum(m.size),
                            fnum(color[0]),
                            fnum(color[1]),
                            fnum(color[2]),
                            fnum(cos),
                            fnum(sin),
                            fnum(-sin),
                            fnum(cos),
                            fnum(ox),
                            fnum(oy),
                        ),
                    );
                    if m.embedded_font.is_some() {
                        push_hex_string(&mut ops, &m.shown);
                    } else {
                        ops.push(b'(');
                        ops.extend_from_slice(&m.shown);
                        ops.push(b')');
                    }
                    push_ops(&mut ops, " Tj\nET\n");
                }
            }
        }
        push_ops(&mut ops, "Q\n");

        let mut entries: Vec<(&str, &str, ObjectId)> = vec![("ExtGState", gs_name.as_str(), gs_id)];
        if metrics.iter().any(|metric| metric.embedded_font.is_none()) {
            entries.push(("Font", standard_font_name.as_str(), standard_font_id));
        }
        for (index, font_id) in embedded_font_ids.iter().copied().enumerate() {
            entries.push(("Font", embedded_font_names[index].as_str(), font_id));
        }
        if let Some((img_id, _, _)) = image {
            entries.push(("XObject", img_name.as_str(), img_id));
        }
        attach_resources(&mut doc, page_id, &entries)?;
        wrap_page_content(&mut doc, page_id, ops)?;
    }

    let mut out = Vec::new();
    doc.save_to(&mut out)?;
    Ok(out)
}

/// Where page-number stamps are anchored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StampPosition {
    #[default]
    BottomRight,
    BottomCenter,
    BottomLeft,
    TopRight,
    TopCenter,
    TopLeft,
}

/// Stamps per-page labels (page numbers / Bates numbering) onto `pdf`.
/// `template` supports `{page}` and `{total}` placeholders; `start_at` offsets
/// the printed page number (Bates-style sequences use e.g. "DK-{page:06}",
/// pad width capped at 16 digits).
///
/// Page `/Rotate` is deliberately NOT compensated: labels are placed in
/// unrotated page space, exactly like DocYantra's pdf-lib implementation
/// (spec parity), so on a /Rotate 90 page the label appears rotated along
/// an unexpected edge.
pub fn stamp_page_numbers(
    pdf: &[u8],
    template: &str,
    position: StampPosition,
    font_size_pt: f64,
    start_at: u32,
) -> Result<Vec<u8>> {
    let mut doc = Document::load_mem(pdf)?;
    let size = clamp_or(font_size_pt, 4.0, 144.0, 10.0);
    const MARGIN: f64 = 36.0;

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });

    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    let total = page_ids.len();
    for (index, page_id) in page_ids.into_iter().enumerate() {
        let display = u64::from(start_at) + index as u64;
        let label = expand_template(template, display, total);
        let (bx, by, w, h) = page_box(&doc, page_id)?;
        let font_name = free_resource_name(&doc, page_id, "Font", FONT_RES)?;
        let text_width = StandardFont::Helvetica.text_width_pt(&label, size);

        let x = bx
            + match position {
                StampPosition::BottomLeft | StampPosition::TopLeft => MARGIN,
                StampPosition::BottomCenter | StampPosition::TopCenter => (w - text_width) / 2.0,
                StampPosition::BottomRight | StampPosition::TopRight => w - MARGIN - text_width,
            };
        let y = by
            + match position {
                StampPosition::BottomLeft
                | StampPosition::BottomCenter
                | StampPosition::BottomRight => MARGIN,
                StampPosition::TopLeft | StampPosition::TopCenter | StampPosition::TopRight => {
                    h - MARGIN
                }
            };

        let mut ops: Vec<u8> = Vec::new();
        push_ops(
            &mut ops,
            &format!(
                "q\nBT\n/{font_name} {} Tf\n0 0 0 rg\n1 0 0 1 {} {} Tm\n",
                fnum(size),
                fnum(x),
                fnum(y),
            ),
        );
        ops.push(b'(');
        ops.extend_from_slice(&escape_pdf_text(&label));
        push_ops(&mut ops, ") Tj\nET\nQ\n");

        attach_resources(&mut doc, page_id, &[("Font", font_name.as_str(), font_id)])?;
        wrap_page_content(&mut doc, page_id, ops)?;
    }

    let mut out = Vec::new();
    doc.save_to(&mut out)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// TS-style clamp: non-finite input yields `fallback`, else clamps to range.
fn clamp_or(value: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if !value.is_finite() {
        return fallback;
    }
    if value < min {
        return min;
    }
    if value > max {
        return max;
    }
    value
}

/// Formats a number for a content stream: up to 5 decimals, trailing zeros
/// trimmed ("64", "9.1", "0.86603").
fn fnum(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

fn push_ops(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
}

fn push_hex_string(buf: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    buf.push(b'<');
    for &byte in bytes {
        buf.push(HEX[usize::from(byte >> 4)]);
        buf.push(HEX[usize::from(byte & 0x0F)]);
    }
    buf.push(b'>');
}

/// Encodes `text` as a PDF literal-string body: WinAnsi bytes with
/// backslash-escaped `\`, `(`, `)`. Control characters degrade to '?'.
fn escape_pdf_text(text: &str) -> Vec<u8> {
    encode_winansi_literal(text)
}

/// Follows reference chains to the concrete object (cycle-guarded).
fn deref<'a>(doc: &'a Document, mut obj: &'a Object) -> Result<&'a Object> {
    let mut hops = 0;
    while let Object::Reference(id) = obj {
        obj = doc.get_object(*id)?;
        hops += 1;
        if hops > 16 {
            return Err(PdfCoreError::InvalidInput(
                "reference cycle in PDF object graph".to_string(),
            ));
        }
    }
    Ok(obj)
}

/// Looks up `key` on the page dict, then up the /Parent chain (inheritable
/// page attributes such as /Resources and /MediaBox). Returns the
/// dereferenced object.
fn find_inherited<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    key: &[u8],
) -> Result<Option<&'a Object>> {
    let mut dict = doc.get_dictionary(page_id)?;
    for _ in 0..64 {
        if let Ok(obj) = dict.get(key) {
            return Ok(Some(deref(doc, obj)?));
        }
        match dict.get(b"Parent") {
            Ok(parent) => dict = deref(doc, parent)?.as_dict()?,
            Err(_) => return Ok(None),
        }
    }
    Ok(None)
}

/// Effective MediaBox for a page (inherited if absent on the page dict) as
/// `(x0, y0, width, height)`, normalized so `(x0, y0)` is the lower-left
/// corner. Stamp math must add the origin: MediaBoxes need not start at 0,0.
fn page_box(doc: &Document, page_id: ObjectId) -> Result<(f64, f64, f64, f64)> {
    let media_box = find_inherited(doc, page_id, b"MediaBox")?.ok_or_else(|| {
        PdfCoreError::InvalidInput("page has no MediaBox (direct or inherited)".to_string())
    })?;
    let arr = media_box.as_array()?;
    if arr.len() != 4 {
        return Err(PdfCoreError::InvalidInput(
            "MediaBox must contain exactly 4 numbers".to_string(),
        ));
    }
    let mut vals = [0.0f64; 4];
    for (i, obj) in arr.iter().enumerate() {
        vals[i] = f64::from(deref(doc, obj)?.as_float()?);
    }
    Ok((
        vals[0].min(vals[2]),
        vals[1].min(vals[3]),
        (vals[2] - vals[0]).abs(),
        (vals[3] - vals[1]).abs(),
    ))
}

/// Picks the first name in the sequence `base`, `base2`, `base3`, ... that is
/// unused in the page's effective `/Resources/<category>` dict, so a second
/// watermark/stamp pass never rebinds a name an earlier pass's content ops
/// still reference.
fn free_resource_name(
    doc: &Document,
    page_id: ObjectId,
    category: &str,
    base: &str,
) -> Result<String> {
    let cat_dict: Option<&Dictionary> = match find_inherited(doc, page_id, b"Resources")? {
        Some(res) => match res.as_dict()?.get(category.as_bytes()) {
            Ok(obj) => Some(deref(doc, obj)?.as_dict()?),
            Err(_) => None,
        },
        None => None,
    };
    let Some(cat) = cat_dict else {
        return Ok(base.to_string());
    };
    if !cat.has(base.as_bytes()) {
        return Ok(base.to_string());
    }
    for i in 2u64.. {
        let candidate = format!("{base}{i}");
        if !cat.has(candidate.as_bytes()) {
            return Ok(candidate);
        }
    }
    unreachable!("fewer than u64::MAX resource names in a dict")
}

/// Ensures the page has a DIRECT /Resources dict (cloning an inherited or
/// referenced one if needed) and adds `entries` as
/// (category e.g. "Font", name e.g. "DKwmF", object id) references.
fn attach_resources(
    doc: &mut Document,
    page_id: ObjectId,
    entries: &[(&str, &str, ObjectId)],
) -> Result<()> {
    let mut resources: Dictionary = match find_inherited(doc, page_id, b"Resources")? {
        Some(obj) => obj.as_dict()?.clone(),
        None => Dictionary::new(),
    };
    for (category, name, id) in entries {
        let mut cat_dict: Dictionary = match resources.get(category.as_bytes()) {
            Ok(obj) => deref(doc, obj)?.as_dict()?.clone(),
            Err(_) => Dictionary::new(),
        };
        cat_dict.set(*name, Object::Reference(*id));
        resources.set(*category, Object::Dictionary(cat_dict));
    }
    let page = doc.get_object_mut(page_id)?.as_dict_mut()?;
    page.set("Resources", Object::Dictionary(resources));
    Ok(())
}

/// Wraps the page's existing content in a `q` / `Q` sandwich so leaked
/// graphics state cannot affect the watermark, then appends `trailing_ops`:
/// /Contents becomes [ "q" stream, ...existing streams, "Q <ops>" stream ].
fn wrap_page_content(doc: &mut Document, page_id: ObjectId, trailing_ops: Vec<u8>) -> Result<()> {
    // Snapshot the existing content list (single ref, ref-to-array, direct
    // array, or even a direct stream from lenient parsers).
    enum Existing {
        List(Vec<Object>),
        DirectStream(Stream),
        None,
    }
    let existing = {
        let page = doc.get_dictionary(page_id)?;
        match page.get(b"Contents") {
            Ok(Object::Reference(r)) => match deref(doc, page.get(b"Contents")?)? {
                Object::Array(arr) => Existing::List(arr.clone()),
                _ => Existing::List(vec![Object::Reference(*r)]),
            },
            Ok(Object::Array(arr)) => Existing::List(arr.clone()),
            Ok(Object::Stream(s)) => Existing::DirectStream(s.clone()),
            _ => Existing::None,
        }
    };
    let existing_list = match existing {
        Existing::List(list) => list,
        Existing::DirectStream(s) => vec![Object::Reference(doc.add_object(s))],
        Existing::None => Vec::new(),
    };

    let save_id = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let mut restore_and_ops = b"Q\n".to_vec();
    restore_and_ops.extend_from_slice(&trailing_ops);
    // Grid watermarks repeat near-identical ops hundreds of times per page;
    // Flate-compressing the ops stream keeps output within ~1x of pdf-lib's
    // size instead of ~7x input. compress() is a no-op when it wouldn't
    // shrink the stream (tiny single-anchor watermarks).
    let mut end_stream = Stream::new(Dictionary::new(), restore_and_ops);
    let _ = end_stream.compress();
    let end_id = doc.add_object(end_stream);

    let mut list = Vec::with_capacity(existing_list.len() + 2);
    list.push(Object::Reference(save_id));
    list.extend(existing_list);
    list.push(Object::Reference(end_id));

    let page = doc.get_object_mut(page_id)?.as_dict_mut()?;
    page.set("Contents", Object::Array(list));
    Ok(())
}

/// Builds the image XObject for a watermark image. JPEGs pass through with
/// DCTDecode (after verifying decodability); everything else (PNG etc.) is
/// decoded to RGB8 + optional alpha SMask, Flate-compressed.
fn build_image_xobject(doc: &mut Document, img: &WatermarkImage) -> Result<ObjectId> {
    // width_pt/height_pt are not validated here: the caller clamps them to
    // DocYantra's 24..=1440 range (parity), and the XObject itself only
    // carries pixel dimensions.
    let format = image::guess_format(&img.bytes)?;
    if format == image::ImageFormat::Jpeg {
        // Decode to verify the JPEG is intact and learn its color model.
        let decoded = image::load_from_memory_with_format(&img.bytes, image::ImageFormat::Jpeg)?;
        let colorspace = match decoded.color() {
            image::ColorType::L8 | image::ColorType::L16 => Some("DeviceGray"),
            image::ColorType::Rgb8 | image::ColorType::Rgb16 => Some("DeviceRGB"),
            _ => None, // unusual model (e.g. CMYK): re-encode below
        };
        if let Some(cs) = colorspace {
            let stream = Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Image",
                    "Width" => i64::from(decoded.width()),
                    "Height" => i64::from(decoded.height()),
                    "ColorSpace" => cs,
                    "BitsPerComponent" => 8,
                    "Filter" => "DCTDecode",
                },
                img.bytes.clone(),
            )
            .with_compression(false);
            return Ok(doc.add_object(stream));
        }
        return flate_image_xobject(doc, &decoded);
    }
    let decoded = image::load_from_memory(&img.bytes)?;
    flate_image_xobject(doc, &decoded)
}

/// Writes a decoded image as a DeviceRGB 8-bpc Flate image XObject, with a
/// DeviceGray /SMask carrying the alpha channel when one is present.
fn flate_image_xobject(doc: &mut Document, img: &image::DynamicImage) -> Result<ObjectId> {
    let (w, h) = (img.width(), img.height());

    let smask_id = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        let alpha: Vec<u8> = rgba.pixels().map(|p| p.0[3]).collect();
        let mut smask = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => i64::from(w),
                "Height" => i64::from(h),
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            alpha,
        );
        smask.compress()?;
        Some(doc.add_object(smask))
    } else {
        None
    };

    let mut dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => i64::from(w),
        "Height" => i64::from(h),
        "ColorSpace" => "DeviceRGB",
        "BitsPerComponent" => 8,
    };
    if let Some(id) = smask_id {
        dict.set("SMask", Object::Reference(id));
    }
    let mut stream = Stream::new(dict, img.to_rgb8().into_raw());
    stream.compress()?;
    Ok(doc.add_object(stream))
}

/// Widest supported `{page:0N}` zero-padding; larger user-supplied widths are
/// treated as this (Rust's `format!` stores widths as u16 and panics at
/// 65536+, and a u64 page number never exceeds 20 digits anyway).
const MAX_PAGE_PAD: usize = 16;

/// Expands `{page}`, `{total}`, and zero-padded `{page:0N}` placeholders
/// (N clamped to 0..=16). Unrecognized `{...}` runs are kept literally.
fn expand_template(template: &str, page: u64, total: usize) -> String {
    let mut out = String::with_capacity(template.len() + 8);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match tail.find('}') {
            Some(end) => {
                let inner = &tail[1..end];
                if inner == "page" {
                    out.push_str(&page.to_string());
                } else if inner == "total" {
                    out.push_str(&total.to_string());
                } else if let Some(width) = inner.strip_prefix("page:0").and_then(|d| {
                    // A parse failure on an all-digit run means overflow, so
                    // it also clamps to MAX_PAGE_PAD.
                    (!d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())).then(|| {
                        d.parse::<usize>()
                            .map_or(MAX_PAGE_PAD, |w| w.min(MAX_PAGE_PAD))
                    })
                }) {
                    out.push_str(&format!("{page:0width$}"));
                } else {
                    out.push_str(&tail[..=end]);
                }
                rest = &tail[end + 1..];
            }
            None => {
                out.push_str(tail);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

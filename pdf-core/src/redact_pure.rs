//! Zone 1 PDF redaction over decoded lopdf content streams.
//!
//! The analyzer deliberately supports a bounded PDF text/font envelope. Any
//! construct whose geometry cannot be proved returns [`PdfCoreError::Unsupported`].

use std::collections::{BTreeMap, BTreeSet};

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use crate::error::{PdfCoreError, Result};
use crate::fonts::StandardFont;
use crate::redact_types::{
    BoxPts, PageRotation, RedactArea, RedactOutput, derotate_box, exceeds_requested, resolve_rect,
};

const BBOX_MARGIN_PT: f64 = 1.0;
const MAX_DECODED_CONTENT_BYTES: usize = 128 * 1024 * 1024;
const MAX_CONTENT_OPERATIONS: usize = 250_000;
const MAX_CONTENT_TOKENS: usize = 1_000_000;
const MAX_TEXT_BYTES_PER_OBJECT: usize = 32 * 1024 * 1024;
const MAX_TEXT_SHOWS_PER_OBJECT: usize = 250_000;
const MAX_TJ_ITEMS: usize = 500_000;
const MAX_REDACTION_AREAS: usize = 10_000;
const MAX_REMOVED_OBJECTS_PER_PAGE: usize = 10_000;
const MAX_REMOVED_OBJECTS_TOTAL: usize = 20_000;
const MAX_DEFLATE_BLOCKS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Matrix {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Matrix {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    const fn translation(x: f64, y: f64) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }

    fn concat(self, rhs: Self) -> Self {
        Self {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            e: self.a * rhs.e + self.c * rhs.f + self.e,
            f: self.b * rhs.e + self.d * rhs.f + self.f,
        }
    }

    fn point(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    fn is_finite(self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|value| value.is_finite())
    }
}

#[derive(Debug, Clone)]
struct TextStyle {
    font: Option<Vec<u8>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    leading: f64,
    rendering_mode: i64,
    rise: f64,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            leading: 0.0,
            rendering_mode: 0,
            rise: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
struct GraphicsState {
    ctm: Matrix,
    text: TextStyle,
    line_width: f64,
    miter_limit: f64,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            text: TextStyle::default(),
            line_width: 1.0,
            miter_limit: 10.0,
        }
    }
}

#[derive(Debug, Clone)]
struct TextObject {
    operations: Vec<Operation>,
    post_graphics_ops: Vec<Operation>,
    matrix: Matrix,
    line_matrix: Matrix,
    bounds: Option<BoxPts>,
    used_text_clip: bool,
    shown_bytes: usize,
    show_count: usize,
    marked_depth_at_start: usize,
    unsafe_marked_content: bool,
}

impl TextObject {
    fn new(operation: Operation, marked_stack: &[bool]) -> Self {
        Self {
            operations: vec![operation],
            post_graphics_ops: Vec::new(),
            matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            bounds: None,
            used_text_clip: false,
            shown_bytes: 0,
            show_count: 0,
            marked_depth_at_start: marked_stack.len(),
            unsafe_marked_content: marked_stack.iter().copied().any(|value| value),
        }
    }
}

#[derive(Debug)]
struct AnnotationRemoval {
    removed: Vec<(BoxPts, &'static str)>,
    object_ids: BTreeSet<ObjectId>,
}

#[derive(Debug, Clone)]
enum FontWidths {
    Simple {
        widths: Box<[Option<f64>; 256]>,
        missing: Option<f64>,
        standard: Option<(StandardFont, StandardCodeEncoding)>,
    },
    Cid {
        widths: BTreeMap<u16, f64>,
        default_width: f64,
    },
}

#[derive(Debug, Clone, Copy)]
enum StandardCodeEncoding {
    Builtin,
    WinAnsi,
}

#[derive(Debug, Clone)]
struct FontMetrics {
    widths: FontWidths,
    ascent: f64,
    descent: f64,
    x_min: f64,
    x_max: f64,
}

impl FontMetrics {
    fn for_each_code_width(
        &self,
        bytes: &[u8],
        page_index: u32,
        mut visit: impl FnMut(f64, bool) -> Result<()>,
    ) -> Result<()> {
        match &self.widths {
            FontWidths::Simple {
                widths,
                missing,
                standard,
            } => {
                for code in bytes {
                    let width = widths[usize::from(*code)]
                        .or(*missing)
                        .or_else(|| {
                            standard.map(|(font, encoding)| {
                                f64::from(match encoding {
                                    StandardCodeEncoding::Builtin => {
                                        font.width_for_builtin_code_milli(*code)
                                    }
                                    StandardCodeEncoding::WinAnsi => {
                                        font.width_for_code_milli(*code)
                                    }
                                })
                            })
                        })
                        .ok_or_else(|| {
                            unsupported(
                                page_index,
                                format!("font has no width for character code {code}"),
                            )
                        })?;
                    if width < 0.0 {
                        return Err(unsupported(
                            page_index,
                            format!("font has a negative width for character code {code}"),
                        ));
                    }
                    visit(width, *code == b' ')?;
                }
                Ok(())
            }
            FontWidths::Cid {
                widths,
                default_width,
            } => {
                if !bytes.len().is_multiple_of(2) {
                    return Err(unsupported(
                        page_index,
                        "Identity-H text string contains an incomplete two-byte CID",
                    ));
                }
                for pair in bytes.chunks_exact(2) {
                    let cid = u16::from_be_bytes([pair[0], pair[1]]);
                    let width = widths.get(&cid).copied().unwrap_or(*default_width);
                    if width < 0.0 {
                        return Err(unsupported(
                            page_index,
                            format!("font has a negative width for CID {cid}"),
                        ));
                    }
                    visit(width, false)?;
                }
                Ok(())
            }
        }
    }
}

struct FontResolver<'a> {
    document: &'a Document,
    resources: Option<&'a Dictionary>,
    page_index: u32,
    cache: BTreeMap<Vec<u8>, FontMetrics>,
}

impl<'a> FontResolver<'a> {
    fn new(document: &'a Document, resources: Option<&'a Dictionary>, page_index: u32) -> Self {
        Self {
            document,
            resources,
            page_index,
            cache: BTreeMap::new(),
        }
    }

    fn get(&mut self, name: &[u8]) -> Result<&FontMetrics> {
        if !self.cache.contains_key(name) {
            let font_object = resource_entry(
                self.document,
                self.resources,
                b"Font",
                name,
                self.page_index,
            )?;
            let metrics = parse_font(self.document, font_object, self.page_index)?;
            self.cache.insert(name.to_vec(), metrics);
        }
        self.cache.get(name).ok_or_else(|| {
            unsupported(
                self.page_index,
                format!(
                    "font /{} disappeared from the analysis cache",
                    String::from_utf8_lossy(name)
                ),
            )
        })
    }
}

#[derive(Debug, Clone)]
struct PagePlan {
    page_id: ObjectId,
    page_index: u32,
    width: f32,
    height: f32,
    boxes: Vec<BoxPts>,
}

#[derive(Debug)]
struct ContentResult {
    operations: Vec<Operation>,
    removed: Vec<(BoxPts, &'static str)>,
    removed_xobjects: BTreeSet<Vec<u8>>,
    removed_form_ids: BTreeSet<ObjectId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyzeMode {
    Rewrite,
    Verify,
}

#[derive(Clone, Copy)]
struct PageAnalysisContext<'a> {
    document: &'a Document,
    resources: Option<&'a Dictionary>,
    page_index: u32,
}

/// Applies fail-closed Zone-1 redaction to service-compatible areas.
///
/// Text objects, intersecting Form XObject invocations, and intersecting
/// annotations are removed in full. Images remain and requested/removed bounds
/// receive opaque black coverage. Saved bytes are reloaded and re-analyzed
/// before this function returns.
pub fn redact_pure(pdf: &[u8], areas: &[RedactArea]) -> Result<RedactOutput> {
    let mut document = Document::load_mem(pdf)?;
    crate::meta::ensure_not_encrypted(pdf, &document, true)?;

    let page_count = u32::try_from(document.get_pages().len()).map_err(|_| {
        PdfCoreError::Unsupported(
            "PDF has more pages than the redaction API can address".to_string(),
        )
    })?;
    if page_count == 0 {
        return Err(PdfCoreError::InvalidInput("PDF has zero pages".to_string()));
    }

    let plans = build_page_plans(&document, areas)?;
    if plans.is_empty() {
        return Ok(RedactOutput {
            pdf: pdf.to_vec(),
            page_count,
            warnings: Vec::new(),
        });
    }

    let mut warnings = Vec::new();
    let mut removed_total = 0usize;
    let mut removed_form_ids = BTreeSet::new();
    let mut removed_annotation_ids = BTreeSet::new();
    for plan in &plans {
        let annotation_removal = remove_annotations(&mut document, plan)?;
        removed_annotation_ids.extend(annotation_removal.object_ids);
        let result = analyze_page_content(&document, plan, AnalyzeMode::Rewrite)?;
        removed_form_ids.extend(result.removed_form_ids.iter().copied());

        let mut removed = annotation_removal.removed;
        removed.extend(result.removed);
        if removed.len() > MAX_REMOVED_OBJECTS_PER_PAGE {
            return Err(unsupported(
                plan.page_index,
                "redaction intersects too many objects for safe rewriting",
            ));
        }
        removed_total = removed_total.checked_add(removed.len()).ok_or_else(|| {
            PdfCoreError::Unsupported("redaction removal count overflowed".to_string())
        })?;
        if removed_total > MAX_REMOVED_OBJECTS_TOTAL {
            return Err(PdfCoreError::Unsupported(
                "redaction intersects too many objects for safe rewriting".to_string(),
            ));
        }
        for (bounds, kind) in &removed {
            if exceeds_requested(bounds, &plan.boxes, plan.width, plan.height) {
                warnings.push(format!(
                    "page {}: redaction removed a whole {kind} spanning ({:.1}, {:.1})-({:.1}, {:.1}) pt, beyond the requested area; the full bounds were covered with a black box",
                    plan.page_index,
                    bounds.left,
                    bounds.bottom,
                    bounds.right,
                    bounds.top,
                ));
            }
        }

        let mut coverage = plan.boxes.clone();
        coverage.extend(
            removed
                .iter()
                .map(|(bounds, _)| *bounds)
                .filter(|bounds| bounds.area() > 0.0),
        );
        detach_unused_xobjects(
            &mut document,
            plan.page_id,
            &result.operations,
            &result.removed_xobjects,
            plan.page_index,
        )?;
        replace_page_content(
            &mut document,
            plan.page_id,
            result.operations,
            &coverage,
            plan.page_index,
        )?;
    }

    document.prune_objects();
    ensure_removed_objects_are_unreachable(&document, &removed_form_ids, "Form XObject")?;
    ensure_removed_objects_are_unreachable(&document, &removed_annotation_ids, "annotation")?;
    document.renumber_objects();
    let mut output = Vec::new();
    document.save_to(&mut output).map_err(|error| {
        PdfCoreError::Unsupported(format!("redaction output could not be serialized: {error}"))
    })?;

    verify_output(&output, areas, page_count)?;
    Ok(RedactOutput {
        pdf: output,
        page_count,
        warnings,
    })
}

fn verify_output(pdf: &[u8], areas: &[RedactArea], expected_pages: u32) -> Result<()> {
    let document = Document::load_mem(pdf).map_err(|error| {
        PdfCoreError::Unsupported(format!("redaction output could not be reloaded: {error}"))
    })?;
    crate::meta::ensure_not_encrypted(pdf, &document, true)?;
    let actual_pages = u32::try_from(document.get_pages().len()).map_err(|_| {
        PdfCoreError::Unsupported("redaction output page count overflowed u32".to_string())
    })?;
    if actual_pages != expected_pages {
        return Err(PdfCoreError::Unsupported(format!(
            "redaction round-trip changed page count from {expected_pages} to {actual_pages}"
        )));
    }

    for plan in build_page_plans(&document, areas)? {
        verify_annotations(&document, &plan)?;
        let result = analyze_page_content(&document, &plan, AnalyzeMode::Verify)?;
        if !result.removed.is_empty() {
            return Err(PdfCoreError::Unsupported(format!(
                "page {}: output verification found content still intersecting a redaction area",
                plan.page_index
            )));
        }
    }
    Ok(())
}

fn build_page_plans(document: &Document, areas: &[RedactArea]) -> Result<Vec<PagePlan>> {
    if areas.len() > MAX_REDACTION_AREAS {
        return Err(PdfCoreError::Unsupported(format!(
            "redaction request exceeds the safe limit of {MAX_REDACTION_AREAS} areas"
        )));
    }
    let pages = document.get_pages();
    let page_by_index: BTreeMap<u32, ObjectId> = pages
        .values()
        .copied()
        .enumerate()
        .filter_map(|(index, id)| u32::try_from(index).ok().map(|index| (index, id)))
        .collect();
    let mut grouped: BTreeMap<u32, Vec<_>> = BTreeMap::new();
    for area in areas {
        grouped
            .entry(area.page_index)
            .or_default()
            .push(area.rect());
    }

    let mut plans = Vec::new();
    for (page_index, rects) in grouped {
        let Some(&page_id) = page_by_index.get(&page_index) else {
            continue;
        };
        let (origin_x, origin_y, width, height) = page_box(document, page_id, page_index)?;
        let rotation = page_rotation(document, page_id, page_index)?;
        let (displayed_width, displayed_height) = match rotation {
            PageRotation::Degrees90 | PageRotation::Degrees270 => (height, width),
            _ => (width, height),
        };
        let page_width = width as f32;
        let page_height = height as f32;
        if !page_width.is_finite()
            || !page_height.is_finite()
            || page_width <= 0.0
            || page_height <= 0.0
        {
            return Err(unsupported(
                page_index,
                "page dimensions exceed supported coordinate precision",
            ));
        }
        let boxes: Vec<_> = rects
            .iter()
            .map(|rect| {
                let bounds = resolve_rect(rect, displayed_width, displayed_height).ok_or_else(|| {
                    unsupported(
                        page_index,
                        "redaction rectangle is non-finite or degenerate after coordinate conversion",
                    )
                })?;
                let mut bounds = derotate_box(
                    bounds,
                    rotation,
                    displayed_width as f32,
                    displayed_height as f32,
                );
                bounds.left += origin_x as f32;
                bounds.right += origin_x as f32;
                bounds.bottom += origin_y as f32;
                bounds.top += origin_y as f32;
                if !bounds.is_finite_and_nonempty() {
                    return Err(unsupported(
                        page_index,
                        "redaction rectangle exceeds supported coordinate precision",
                    ));
                }
                Ok(bounds)
            })
            .collect::<Result<Vec<_>>>()?;
        if !boxes.is_empty() {
            plans.push(PagePlan {
                page_id,
                page_index,
                width: page_width,
                height: page_height,
                boxes,
            });
        }
    }
    Ok(plans)
}

fn page_box(
    document: &Document,
    page_id: ObjectId,
    page_index: u32,
) -> Result<(f64, f64, f64, f64)> {
    let object = find_inherited(document, page_id, b"CropBox")?
        .or(find_inherited(document, page_id, b"MediaBox")?)
        .ok_or_else(|| unsupported(page_index, "page has no CropBox or MediaBox"))?;
    let array = deref(document, object)?
        .as_array()
        .map_err(|_| unsupported(page_index, "page CropBox/MediaBox is not an array"))?;
    let values = number_array::<4>(document, array, page_index, "page box")?;
    let left = values[0].min(values[2]);
    let right = values[0].max(values[2]);
    let bottom = values[1].min(values[3]);
    let top = values[1].max(values[3]);
    if right <= left || top <= bottom {
        return Err(unsupported(page_index, "page box is degenerate"));
    }
    Ok((left, bottom, right - left, top - bottom))
}

fn page_rotation(document: &Document, page_id: ObjectId, page_index: u32) -> Result<PageRotation> {
    let Some(object) = find_inherited(document, page_id, b"Rotate")? else {
        return Ok(PageRotation::None);
    };
    let degrees = deref(document, object)?
        .as_i64()
        .map_err(|_| unsupported(page_index, "page /Rotate is not an integer"))?;
    PageRotation::from_degrees(degrees).ok_or_else(|| {
        unsupported(
            page_index,
            format!("page /Rotate {degrees} is not a multiple of 90"),
        )
    })
}

fn strict_page_content(document: &Document, page_id: ObjectId, page_index: u32) -> Result<Vec<u8>> {
    let page = document.get_dictionary(page_id).map_err(|error| {
        unsupported(
            page_index,
            format!("page dictionary cannot be read for /Contents: {error}"),
        )
    })?;
    let Ok(contents) = page.get(b"Contents") else {
        return Ok(Vec::new());
    };
    let mut bytes = Vec::new();
    let mut seen_streams = BTreeSet::new();
    append_content_object(
        document,
        contents,
        page_index,
        0,
        &mut seen_streams,
        &mut bytes,
    )?;
    Ok(bytes)
}

fn append_content_object(
    document: &Document,
    object: &Object,
    page_index: u32,
    depth: usize,
    seen_streams: &mut BTreeSet<ObjectId>,
    output: &mut Vec<u8>,
) -> Result<()> {
    if depth > 64 {
        return Err(unsupported(
            page_index,
            "page /Contents nesting exceeds 64 levels",
        ));
    }
    let (object_id, resolved) = document.dereference(object).map_err(|error| {
        unsupported(
            page_index,
            format!("page /Contents reference cannot be resolved: {error}"),
        )
    })?;
    match resolved {
        Object::Stream(stream) => {
            if object_id.is_some_and(|id| !seen_streams.insert(id)) {
                return Err(unsupported(
                    page_index,
                    "page /Contents repeats the same stream reference",
                ));
            }
            let remaining = MAX_DECODED_CONTENT_BYTES
                .checked_sub(output.len())
                .ok_or_else(|| unsupported(page_index, "page content exceeds safe limits"))?;
            let data = strict_stream_content(
                stream,
                page_index,
                "page content stream",
                remaining.saturating_sub(1),
            )?;
            output
                .try_reserve(data.len() + 1)
                .map_err(|_| unsupported(page_index, "page content cannot be allocated safely"))?;
            output.extend_from_slice(&data);
            output.push(b'\n');
            Ok(())
        }
        Object::Array(streams) => {
            for stream in streams {
                append_content_object(
                    document,
                    stream,
                    page_index,
                    depth + 1,
                    seen_streams,
                    output,
                )?;
            }
            Ok(())
        }
        _ => Err(unsupported(
            page_index,
            "page /Contents contains an object that is neither a stream nor an array of streams",
        )),
    }
}

fn strict_stream_content(
    stream: &Stream,
    page_index: u32,
    label: &str,
    max_output: usize,
) -> Result<Vec<u8>> {
    if stream.dict.has(b"Filter") {
        let filters = stream.filters().map_err(|error| {
            unsupported(
                page_index,
                format!("{label} has an invalid /Filter: {error}"),
            )
        })?;
        if filters.as_slice() != [b"FlateDecode".as_slice()] {
            return Err(unsupported(
                page_index,
                format!("{label} uses a filter chain outside the safe redaction envelope"),
            ));
        }
        if stream.dict.has(b"DecodeParms") {
            return Err(unsupported(
                page_index,
                format!("{label} uses unsupported /DecodeParms"),
            ));
        }
        validate_zlib_header(&stream.content, page_index, label)?;
        let expected_len = validate_deflate_stream(
            &stream.content[2..stream.content.len() - 4],
            max_output,
            page_index,
            label,
        )?;
        if expected_len > max_output {
            return Err(unsupported(
                page_index,
                format!("{label} exceeds the safe decoded-content limit"),
            ));
        }
        let data = stream.decompressed_content().map_err(|error| {
            unsupported(
                page_index,
                format!("{label} cannot be decompressed: {error}"),
            )
        })?;
        if data.len() > max_output {
            return Err(unsupported(
                page_index,
                format!("{label} exceeds the safe decoded-content limit"),
            ));
        }
        if data.len() != expected_len {
            return Err(unsupported(
                page_index,
                format!("{label} decompressor did not consume a complete DEFLATE stream"),
            ));
        }
        validate_zlib_checksum(&stream.content, &data, page_index, label)?;
        Ok(data)
    } else {
        if stream.content.len() > max_output {
            return Err(unsupported(
                page_index,
                format!("{label} exceeds the safe decoded-content limit"),
            ));
        }
        Ok(stream.content.clone())
    }
}

fn validate_zlib_header(input: &[u8], page_index: u32, label: &str) -> Result<()> {
    if input.len() < 6 {
        return Err(unsupported(
            page_index,
            format!("{label} has a truncated zlib wrapper"),
        ));
    }
    let cmf = input[0];
    let flg = input[1];
    if cmf & 0x0f != 8
        || cmf >> 4 > 7
        || (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0
        || flg & 0x20 != 0
    {
        return Err(unsupported(
            page_index,
            format!("{label} has an invalid zlib header"),
        ));
    }
    Ok(())
}

fn validate_zlib_checksum(input: &[u8], output: &[u8], page_index: u32, label: &str) -> Result<()> {
    let expected = u32::from_be_bytes(
        input[input.len() - 4..]
            .try_into()
            .map_err(|_| unsupported(page_index, format!("{label} has no zlib checksum")))?,
    );
    let mut a = 1u32;
    let mut b = 0u32;
    for chunk in output.chunks(5_552) {
        for byte in chunk {
            a += u32::from(*byte);
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    let actual = (b << 16) | a;
    if actual != expected {
        return Err(unsupported(
            page_index,
            format!("{label} failed strict zlib checksum verification"),
        ));
    }
    Ok(())
}

struct DeflateBits<'a> {
    bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> DeflateBits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.bytes
            .len()
            .saturating_mul(8)
            .saturating_sub(self.bit_offset)
    }

    fn read(&mut self, bits: usize) -> Option<u32> {
        if bits > 24 || self.remaining() < bits {
            return None;
        }
        let mut value = 0u32;
        for shift in 0..bits {
            let offset = self.bit_offset + shift;
            let bit = (self.bytes[offset / 8] >> (offset % 8)) & 1;
            value |= u32::from(bit) << shift;
        }
        self.bit_offset += bits;
        Some(value)
    }

    fn peek(&self, bits: usize) -> u32 {
        let available = bits.min(self.remaining());
        let mut value = 0u32;
        for shift in 0..available {
            let offset = self.bit_offset + shift;
            let bit = (self.bytes[offset / 8] >> (offset % 8)) & 1;
            value |= u32::from(bit) << shift;
        }
        value
    }

    fn align_byte(&mut self) -> Option<()> {
        self.bit_offset = self.bit_offset.checked_add(7)? & !7;
        (self.bit_offset <= self.bytes.len().saturating_mul(8)).then_some(())
    }
}

struct DeflateHuffman {
    table: Vec<(u16, u8)>,
    max_bits: usize,
}

impl DeflateHuffman {
    fn new(lengths: &[u8], page_index: u32, label: &str) -> Result<Self> {
        let max_bits = usize::from(lengths.iter().copied().max().unwrap_or(0));
        if max_bits == 0 || max_bits > 15 {
            return Err(unsupported(
                page_index,
                format!("{label} contains an invalid DEFLATE Huffman tree"),
            ));
        }
        let mut counts = [0u16; 16];
        for length in lengths.iter().copied().filter(|length| *length != 0) {
            if length > 15 {
                return Err(unsupported(
                    page_index,
                    format!("{label} contains an overlong DEFLATE Huffman code"),
                ));
            }
            counts[usize::from(length)] += 1;
        }
        let mut remaining = 1i32;
        for count in counts.iter().skip(1).take(max_bits) {
            remaining = (remaining << 1) - i32::from(*count);
            if remaining < 0 {
                return Err(unsupported(
                    page_index,
                    format!("{label} contains an oversubscribed DEFLATE Huffman tree"),
                ));
            }
        }

        let mut next_code = [0u16; 16];
        let mut code = 0u16;
        for bits in 1..=15 {
            code = (code + counts[bits - 1]) << 1;
            next_code[bits] = code;
        }
        let size = 1usize << max_bits;
        let mut table = vec![(0u16, 0u8); size];
        for (symbol, length) in lengths.iter().copied().enumerate() {
            if length == 0 {
                continue;
            }
            let length_usize = usize::from(length);
            let canonical = next_code[length_usize];
            next_code[length_usize] += 1;
            let reversed = reverse_low_bits(canonical, length_usize);
            let step = 1usize << length_usize;
            for index in (usize::from(reversed)..size).step_by(step) {
                if table[index].1 != 0 {
                    return Err(unsupported(
                        page_index,
                        format!("{label} contains duplicate DEFLATE Huffman codes"),
                    ));
                }
                table[index] = (symbol as u16, length);
            }
        }
        Ok(Self { table, max_bits })
    }

    fn decode(&self, bits: &mut DeflateBits<'_>, page_index: u32, label: &str) -> Result<u16> {
        let index = bits.peek(self.max_bits) as usize;
        let (symbol, length) = self.table[index];
        if length == 0 || usize::from(length) > bits.remaining() {
            return Err(unsupported(
                page_index,
                format!("{label} contains a truncated DEFLATE Huffman code"),
            ));
        }
        bits.bit_offset += usize::from(length);
        Ok(symbol)
    }
}

fn reverse_low_bits(mut value: u16, bits: usize) -> u16 {
    let mut reversed = 0u16;
    for _ in 0..bits {
        reversed = (reversed << 1) | (value & 1);
        value >>= 1;
    }
    reversed
}

fn validate_deflate_stream(
    input: &[u8],
    max_output: usize,
    page_index: u32,
    label: &str,
) -> Result<usize> {
    const LENGTH_BASE: [usize; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LENGTH_EXTRA: [usize; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DISTANCE_BASE: [usize; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DISTANCE_EXTRA: [usize; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    let mut bits = DeflateBits::new(input);
    let mut produced = 0usize;
    let mut block_count = 0usize;
    loop {
        block_count += 1;
        if block_count > MAX_DEFLATE_BLOCKS {
            return Err(unsupported(
                page_index,
                format!("{label} exceeds safe DEFLATE block complexity limits"),
            ));
        }
        let final_block = read_deflate_bits(&mut bits, 1, page_index, label)? != 0;
        let block_type = read_deflate_bits(&mut bits, 2, page_index, label)?;
        match block_type {
            0 => {
                bits.align_byte().ok_or_else(|| {
                    unsupported(page_index, format!("{label} has a truncated stored block"))
                })?;
                let length = read_deflate_bits(&mut bits, 16, page_index, label)? as u16;
                let complement = read_deflate_bits(&mut bits, 16, page_index, label)? as u16;
                if length != !complement {
                    return Err(unsupported(
                        page_index,
                        format!("{label} has an invalid stored DEFLATE block length"),
                    ));
                }
                let byte_count = usize::from(length);
                if bits.remaining() < byte_count.saturating_mul(8) {
                    return Err(unsupported(
                        page_index,
                        format!("{label} has a truncated stored DEFLATE block"),
                    ));
                }
                bits.bit_offset += byte_count * 8;
                produced =
                    checked_deflate_output(produced, byte_count, max_output, page_index, label)?;
            }
            1 | 2 => {
                let (literal_tree, distance_tree) = if block_type == 1 {
                    fixed_deflate_trees(page_index, label)?
                } else {
                    dynamic_deflate_trees(&mut bits, page_index, label)?
                };
                loop {
                    let symbol = literal_tree.decode(&mut bits, page_index, label)?;
                    match symbol {
                        0..=255 => {
                            produced =
                                checked_deflate_output(produced, 1, max_output, page_index, label)?;
                        }
                        256 => break,
                        257..=285 => {
                            let index = usize::from(symbol - 257);
                            let extra = read_deflate_bits(
                                &mut bits,
                                LENGTH_EXTRA[index],
                                page_index,
                                label,
                            )? as usize;
                            let length = LENGTH_BASE[index] + extra;
                            let distance_symbol =
                                distance_tree.decode(&mut bits, page_index, label)?;
                            if distance_symbol > 29 {
                                return Err(unsupported(
                                    page_index,
                                    format!("{label} uses a reserved DEFLATE distance code"),
                                ));
                            }
                            let distance_index = usize::from(distance_symbol);
                            let distance_extra = read_deflate_bits(
                                &mut bits,
                                DISTANCE_EXTRA[distance_index],
                                page_index,
                                label,
                            )? as usize;
                            let distance = DISTANCE_BASE[distance_index] + distance_extra;
                            if distance == 0 || distance > produced {
                                return Err(unsupported(
                                    page_index,
                                    format!("{label} has an invalid DEFLATE back-reference"),
                                ));
                            }
                            produced = checked_deflate_output(
                                produced, length, max_output, page_index, label,
                            )?;
                        }
                        _ => {
                            return Err(unsupported(
                                page_index,
                                format!("{label} uses a reserved DEFLATE length code"),
                            ));
                        }
                    }
                }
            }
            _ => {
                return Err(unsupported(
                    page_index,
                    format!("{label} uses a reserved DEFLATE block type"),
                ));
            }
        }
        if final_block {
            break;
        }
    }
    bits.align_byte()
        .ok_or_else(|| unsupported(page_index, format!("{label} has invalid DEFLATE padding")))?;
    if bits.remaining() != 0 {
        return Err(unsupported(
            page_index,
            format!("{label} has bytes after the final DEFLATE block"),
        ));
    }
    Ok(produced)
}

fn read_deflate_bits(
    bits: &mut DeflateBits<'_>,
    count: usize,
    page_index: u32,
    label: &str,
) -> Result<u32> {
    bits.read(count).ok_or_else(|| {
        unsupported(
            page_index,
            format!("{label} contains a truncated DEFLATE stream"),
        )
    })
}

fn checked_deflate_output(
    current: usize,
    additional: usize,
    max_output: usize,
    page_index: u32,
    label: &str,
) -> Result<usize> {
    let output = current
        .checked_add(additional)
        .ok_or_else(|| unsupported(page_index, format!("{label} output size overflows")))?;
    if output > max_output {
        return Err(unsupported(
            page_index,
            format!("{label} exceeds the safe decoded-content limit"),
        ));
    }
    Ok(output)
}

fn fixed_deflate_trees(page_index: u32, label: &str) -> Result<(DeflateHuffman, DeflateHuffman)> {
    let mut literal_lengths = vec![0u8; 288];
    literal_lengths[..144].fill(8);
    literal_lengths[144..256].fill(9);
    literal_lengths[256..280].fill(7);
    literal_lengths[280..].fill(8);
    let distance_lengths = vec![5u8; 32];
    Ok((
        DeflateHuffman::new(&literal_lengths, page_index, label)?,
        DeflateHuffman::new(&distance_lengths, page_index, label)?,
    ))
}

fn dynamic_deflate_trees(
    bits: &mut DeflateBits<'_>,
    page_index: u32,
    label: &str,
) -> Result<(DeflateHuffman, DeflateHuffman)> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let literal_count = read_deflate_bits(bits, 5, page_index, label)? as usize + 257;
    let distance_count = read_deflate_bits(bits, 5, page_index, label)? as usize + 1;
    let code_count = read_deflate_bits(bits, 4, page_index, label)? as usize + 4;
    if literal_count > 286 || distance_count > 32 {
        return Err(unsupported(
            page_index,
            format!("{label} has invalid dynamic DEFLATE tree sizes"),
        ));
    }
    let mut code_lengths = [0u8; 19];
    for index in 0..code_count {
        code_lengths[ORDER[index]] = read_deflate_bits(bits, 3, page_index, label)? as u8;
    }
    let code_tree = DeflateHuffman::new(&code_lengths, page_index, label)?;
    let total = literal_count + distance_count;
    let mut lengths = Vec::with_capacity(total);
    while lengths.len() < total {
        match code_tree.decode(bits, page_index, label)? {
            value @ 0..=15 => lengths.push(value as u8),
            16 => {
                let previous = lengths.last().copied().ok_or_else(|| {
                    unsupported(
                        page_index,
                        format!("{label} repeats a missing DEFLATE code length"),
                    )
                })?;
                let repeat = read_deflate_bits(bits, 2, page_index, label)? as usize + 3;
                if lengths.len() + repeat > total {
                    return Err(unsupported(
                        page_index,
                        format!("{label} overflows its dynamic DEFLATE code lengths"),
                    ));
                }
                lengths.resize(lengths.len() + repeat, previous);
            }
            17 => {
                let repeat = read_deflate_bits(bits, 3, page_index, label)? as usize + 3;
                if lengths.len() + repeat > total {
                    return Err(unsupported(
                        page_index,
                        format!("{label} overflows its dynamic DEFLATE code lengths"),
                    ));
                }
                lengths.resize(lengths.len() + repeat, 0);
            }
            18 => {
                let repeat = read_deflate_bits(bits, 7, page_index, label)? as usize + 11;
                if lengths.len() + repeat > total {
                    return Err(unsupported(
                        page_index,
                        format!("{label} overflows its dynamic DEFLATE code lengths"),
                    ));
                }
                lengths.resize(lengths.len() + repeat, 0);
            }
            _ => {
                return Err(unsupported(
                    page_index,
                    format!("{label} uses an invalid DEFLATE code-length symbol"),
                ));
            }
        }
    }
    if lengths.get(256).copied().unwrap_or(0) == 0 {
        return Err(unsupported(
            page_index,
            format!("{label} dynamic DEFLATE tree has no end-of-block code"),
        ));
    }
    let distance_lengths = &lengths[literal_count..];
    if distance_lengths.iter().all(|length| *length == 0) {
        return Err(unsupported(
            page_index,
            format!("{label} dynamic DEFLATE tree has no distance code"),
        ));
    }
    Ok((
        DeflateHuffman::new(&lengths[..literal_count], page_index, label)?,
        DeflateHuffman::new(distance_lengths, page_index, label)?,
    ))
}

fn decode_content_strict(bytes: &[u8], page_index: u32, label: &str) -> Result<Content> {
    if bytes.len() > MAX_DECODED_CONTENT_BYTES {
        return Err(unsupported(
            page_index,
            format!("{label} exceeds the safe decoded-content limit"),
        ));
    }
    preflight_inline_images(bytes, page_index)?;
    let content = Content::decode_strict(bytes)
        .map_err(|error| unsupported(page_index, format!("{label} is malformed: {error}")))?;
    if content.operations.len() > MAX_CONTENT_OPERATIONS {
        return Err(unsupported(
            page_index,
            format!("{label} exceeds safe operator complexity limits"),
        ));
    }
    Ok(content)
}

#[derive(Debug, Clone, Copy)]
enum InlineHeaderValue<'a> {
    Integer(i64),
    Name(&'a [u8]),
    Boolean(bool),
    Other,
}

fn preflight_inline_images(bytes: &[u8], page_index: u32) -> Result<()> {
    let mut offset = 0;
    let mut token_count = 0usize;
    while offset < bytes.len() {
        offset = skip_content_space_and_comments(bytes, offset);
        if offset >= bytes.len() {
            break;
        }
        token_count += 1;
        if token_count > MAX_CONTENT_TOKENS {
            return Err(unsupported(
                page_index,
                "page content exceeds safe token complexity limits",
            ));
        }
        match bytes[offset] {
            b'(' => offset = skip_literal_string(bytes, offset, page_index)?,
            b'<' if bytes.get(offset + 1) != Some(&b'<') => {
                offset = skip_hex_string(bytes, offset, page_index)?;
            }
            b'/' => offset = skip_name(bytes, offset),
            byte if is_pdf_regular(byte) => {
                let start = offset;
                offset = skip_regular(bytes, offset);
                if &bytes[start..offset] == b"BI" {
                    offset = preflight_inline_image(bytes, offset, page_index)?;
                }
            }
            _ => offset += 1,
        }
    }
    Ok(())
}

fn preflight_inline_image(bytes: &[u8], mut offset: usize, page_index: u32) -> Result<usize> {
    let mut width = None;
    let mut height = None;
    let mut bits_per_component = None;
    let mut color_space = None;
    let mut image_mask = false;
    let mut has_filter = false;
    let mut entry_count = 0usize;

    loop {
        offset = skip_content_space_and_comments(bytes, offset);
        if regular_token(bytes, offset) == Some(b"ID".as_slice()) {
            offset += 2;
            while bytes
                .get(offset)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            {
                offset += 1;
            }
            break;
        }
        if bytes.get(offset) != Some(&b'/') {
            return Err(unsupported(
                page_index,
                "inline image header cannot be parsed safely",
            ));
        }
        entry_count += 1;
        if entry_count > 1_024 {
            return Err(unsupported(
                page_index,
                "inline image header exceeds safe complexity limits",
            ));
        }
        let key_start = offset + 1;
        offset = skip_name(bytes, offset);
        let key = &bytes[key_start..offset];
        offset = skip_content_space_and_comments(bytes, offset);
        let (next, value) = inline_header_value(bytes, offset, page_index)?;
        offset = next;
        match key {
            b"W" | b"Width" => width = inline_integer(value),
            b"H" | b"Height" => height = inline_integer(value),
            b"BPC" | b"BitsPerComponent" => bits_per_component = inline_integer(value),
            b"CS" | b"ColorSpace" => color_space = inline_name(value),
            b"IM" | b"ImageMask" => {
                image_mask = match value {
                    InlineHeaderValue::Boolean(value) => value,
                    _ => {
                        return Err(unsupported(
                            page_index,
                            "inline image /ImageMask is not boolean",
                        ));
                    }
                };
            }
            b"F" | b"Filter" => has_filter = true,
            _ => {}
        }
    }

    if has_filter {
        return Err(unsupported(
            page_index,
            "filtered inline images are outside the safe redaction envelope",
        ));
    }
    let width = positive_inline_usize(width, page_index, "/Width")?;
    let height = positive_inline_usize(height, page_index, "/Height")?;
    let bits = positive_inline_usize(bits_per_component, page_index, "/BitsPerComponent")?;
    let colors = if image_mask {
        1usize
    } else {
        match color_space {
            Some(b"DeviceGray" | b"Gray") => 1,
            Some(b"DeviceRGB" | b"RGB") => 3,
            Some(b"DeviceRGBA" | b"RGBA" | b"DeviceCMYK" | b"CMYK") => 4,
            _ => {
                return Err(unsupported(
                    page_index,
                    "inline image has an unsupported or missing /ColorSpace",
                ));
            }
        }
    };
    let row_bits = width
        .checked_mul(colors)
        .and_then(|value| value.checked_mul(bits))
        .ok_or_else(|| unsupported(page_index, "inline image dimensions overflow"))?;
    let stride = row_bits
        .checked_add(7)
        .ok_or_else(|| unsupported(page_index, "inline image row size overflows"))?
        / 8;
    let length = height
        .checked_mul(stride)
        .ok_or_else(|| unsupported(page_index, "inline image sample size overflows"))?;
    offset = offset
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| unsupported(page_index, "inline image samples are truncated"))?;
    while bytes
        .get(offset)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
    {
        offset += 1;
    }
    if bytes.get(offset..offset + 2) != Some(b"EI".as_slice()) {
        return Err(unsupported(
            page_index,
            "inline image has no operator-safe EI terminator",
        ));
    }
    Ok(offset + 2)
}

fn inline_header_value<'a>(
    bytes: &'a [u8],
    offset: usize,
    page_index: u32,
) -> Result<(usize, InlineHeaderValue<'a>)> {
    match bytes.get(offset).copied() {
        Some(b'/') => {
            let end = skip_name(bytes, offset);
            Ok((end, InlineHeaderValue::Name(&bytes[offset + 1..end])))
        }
        Some(b'(') => Ok((
            skip_literal_string(bytes, offset, page_index)?,
            InlineHeaderValue::Other,
        )),
        Some(b'<') if bytes.get(offset + 1) != Some(&b'<') => Ok((
            skip_hex_string(bytes, offset, page_index)?,
            InlineHeaderValue::Other,
        )),
        Some(b'[' | b'<') => Err(unsupported(
            page_index,
            "compound inline image dictionary values are outside the safe redaction envelope",
        )),
        Some(byte) if is_pdf_regular(byte) => {
            let end = skip_regular(bytes, offset);
            let token = &bytes[offset..end];
            let value = match token {
                b"true" => InlineHeaderValue::Boolean(true),
                b"false" => InlineHeaderValue::Boolean(false),
                _ => std::str::from_utf8(token)
                    .ok()
                    .and_then(|token| token.parse::<i64>().ok())
                    .map_or(InlineHeaderValue::Other, InlineHeaderValue::Integer),
            };
            Ok((end, value))
        }
        _ => Err(unsupported(
            page_index,
            "inline image dictionary value cannot be parsed safely",
        )),
    }
}

fn inline_integer(value: InlineHeaderValue<'_>) -> Option<i64> {
    match value {
        InlineHeaderValue::Integer(value) => Some(value),
        _ => None,
    }
}

fn inline_name(value: InlineHeaderValue<'_>) -> Option<&[u8]> {
    match value {
        InlineHeaderValue::Name(value) => Some(value),
        _ => None,
    }
}

fn positive_inline_usize(value: Option<i64>, page_index: u32, label: &str) -> Result<usize> {
    let value =
        value.ok_or_else(|| unsupported(page_index, format!("inline image has no {label}")))?;
    if value <= 0 {
        return Err(unsupported(
            page_index,
            format!("inline image {label} is not positive"),
        ));
    }
    usize::try_from(value)
        .map_err(|_| unsupported(page_index, format!("inline image {label} is too large")))
}

fn skip_literal_string(bytes: &[u8], mut offset: usize, page_index: u32) -> Result<usize> {
    offset += 1;
    let mut depth = 1usize;
    while offset < bytes.len() {
        match bytes[offset] {
            b'\\' => offset = (offset + 2).min(bytes.len()),
            b'(' => {
                depth += 1;
                offset += 1;
            }
            b')' => {
                depth -= 1;
                offset += 1;
                if depth == 0 {
                    return Ok(offset);
                }
            }
            _ => offset += 1,
        }
    }
    Err(unsupported(page_index, "literal string is unterminated"))
}

fn skip_hex_string(bytes: &[u8], mut offset: usize, page_index: u32) -> Result<usize> {
    offset += 1;
    while offset < bytes.len() {
        if bytes[offset] == b'>' {
            return Ok(offset + 1);
        }
        offset += 1;
    }
    Err(unsupported(
        page_index,
        "hexadecimal string is unterminated",
    ))
}

fn skip_content_space_and_comments(bytes: &[u8], mut offset: usize) -> usize {
    loop {
        while bytes
            .get(offset)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            offset += 1;
        }
        if bytes.get(offset) != Some(&b'%') {
            return offset;
        }
        while bytes
            .get(offset)
            .is_some_and(|byte| !matches!(byte, b'\r' | b'\n'))
        {
            offset += 1;
        }
    }
}

fn skip_name(bytes: &[u8], offset: usize) -> usize {
    skip_regular(bytes, offset + 1)
}

fn skip_regular(bytes: &[u8], mut offset: usize) -> usize {
    while bytes.get(offset).is_some_and(|byte| is_pdf_regular(*byte)) {
        offset += 1;
    }
    offset
}

fn regular_token(bytes: &[u8], offset: usize) -> Option<&[u8]> {
    bytes
        .get(offset)
        .copied()
        .filter(|byte| is_pdf_regular(*byte))
        .map(|_| &bytes[offset..skip_regular(bytes, offset)])
}

fn is_pdf_regular(byte: u8) -> bool {
    !byte.is_ascii_whitespace()
        && !matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

fn analyze_page_content(
    document: &Document,
    plan: &PagePlan,
    mode: AnalyzeMode,
) -> Result<ContentResult> {
    let bytes = strict_page_content(document, plan.page_id, plan.page_index)?;
    let content = decode_content_strict(&bytes, plan.page_index, "page content stream")?;
    let resources = find_inherited(document, plan.page_id, b"Resources")?
        .map(|object| deref(document, object))
        .transpose()?
        .map(|object| {
            object
                .as_dict()
                .map_err(|_| unsupported(plan.page_index, "page /Resources is not a dictionary"))
        })
        .transpose()?;
    analyze_operations(document, resources, content.operations, plan, mode)
}

fn analyze_operations(
    document: &Document,
    resources: Option<&Dictionary>,
    operations: Vec<Operation>,
    plan: &PagePlan,
    mode: AnalyzeMode,
) -> Result<ContentResult> {
    let mut fonts = FontResolver::new(document, resources, plan.page_index);
    let mut graphics = GraphicsState::default();
    let mut stack = Vec::new();
    let mut marked_stack = Vec::new();
    let mut structure_replacement_text = None;
    let mut text_object: Option<TextObject> = None;
    let mut output = Vec::with_capacity(operations.len());
    let mut removed = Vec::new();
    let mut removed_xobjects = BTreeSet::new();
    let mut removed_form_ids = BTreeSet::new();

    for operation in operations {
        if let Some(text) = text_object.as_mut() {
            if operation.operator == "BT" {
                return Err(unsupported(plan.page_index, "nested BT text object"));
            }
            if operation.operator == "ET" {
                require_operands(&operation, 0, plan.page_index)?;
                if marked_stack.len() != text.marked_depth_at_start {
                    return Err(unsupported(
                        plan.page_index,
                        "marked-content scope crosses a BT..ET boundary",
                    ));
                }
                text.operations.push(operation);
                let completed = text_object
                    .take()
                    .ok_or_else(|| unsupported(plan.page_index, "text object state disappeared"))?;
                let intersects = completed
                    .bounds
                    .is_some_and(|bounds| intersects_any(bounds, &plan.boxes));
                if intersects {
                    if completed.unsafe_marked_content {
                        return Err(unsupported(
                            plan.page_index,
                            "intersecting text participates in tagged or replacement marked content",
                        ));
                    }
                    if completed.used_text_clip {
                        return Err(unsupported(
                            plan.page_index,
                            "intersecting text uses a clipping rendering mode; removal could alter unrelated page content",
                        ));
                    }
                    let bounds = completed.bounds.ok_or_else(|| {
                        unsupported(
                            plan.page_index,
                            "intersecting text has no computable bounds",
                        )
                    })?;
                    if removed.len() >= MAX_REMOVED_OBJECTS_PER_PAGE {
                        return Err(unsupported(
                            plan.page_index,
                            "redaction intersects too many content objects for safe rewriting",
                        ));
                    }
                    removed.push((bounds, "text object"));
                    if mode == AnalyzeMode::Rewrite {
                        output.extend(style_carrier(&graphics.text));
                        output.extend(completed.post_graphics_ops);
                    }
                } else {
                    output.extend(completed.operations);
                }
                continue;
            }
            process_text_operation(
                PageAnalysisContext {
                    document,
                    resources,
                    page_index: plan.page_index,
                },
                &operation,
                text,
                &mut graphics,
                &mut fonts,
                &mut marked_stack,
                &mut structure_replacement_text,
            )?;
            text.operations.push(operation);
            continue;
        }

        match operation.operator.as_str() {
            "BT" => {
                require_operands(&operation, 0, plan.page_index)?;
                text_object = Some(TextObject::new(operation, &marked_stack));
            }
            "ET" => return Err(unsupported(plan.page_index, "ET without matching BT")),
            "q" => {
                require_operands(&operation, 0, plan.page_index)?;
                stack.push(graphics.clone());
                output.push(operation);
            }
            "Q" => {
                require_operands(&operation, 0, plan.page_index)?;
                graphics = stack
                    .pop()
                    .ok_or_else(|| unsupported(plan.page_index, "graphics-state Q underflow"))?;
                output.push(operation);
            }
            "cm" => {
                let matrix = operation_matrix(document, &operation, plan.page_index)?;
                graphics.ctm = graphics.ctm.concat(matrix);
                if !graphics.ctm.is_finite() {
                    return Err(unsupported(
                        plan.page_index,
                        "non-finite current transformation matrix",
                    ));
                }
                output.push(operation);
            }
            "Do" => {
                let remove = analyze_xobject(document, resources, &operation, graphics.ctm, plan)?;
                if let Some((bounds, name, form_id)) = remove {
                    if removed.len() >= MAX_REMOVED_OBJECTS_PER_PAGE {
                        return Err(unsupported(
                            plan.page_index,
                            "redaction intersects too many content objects for safe rewriting",
                        ));
                    }
                    removed.push((bounds, "Form XObject"));
                    removed_xobjects.insert(name);
                    removed_form_ids.insert(form_id);
                } else {
                    output.push(operation);
                }
            }
            "BI" => {
                validate_inline_image(&operation, plan.page_index)?;
                let _placed = transform_box(graphics.ctm, [0.0, 0.0, 1.0, 1.0], plan.page_index)?;
                output.push(operation);
            }
            "gs" => {
                apply_ext_gstate(
                    document,
                    resources,
                    &operation,
                    &mut graphics,
                    &mut fonts,
                    plan.page_index,
                )?;
                output.push(operation);
            }
            "SCN" | "scn" => {
                validate_pattern_color_operation(document, &operation, plan.page_index)?;
                output.push(operation);
            }
            "BMC" => {
                validate_marked_tag(document, &operation, plan.page_index, 1)?;
                marked_stack.push(false);
                output.push(operation);
            }
            "BDC" => {
                validate_marked_tag(document, &operation, plan.page_index, 2)?;
                let semantic = marked_property_is_semantic(
                    document,
                    resources,
                    &operation.operands[1],
                    &mut structure_replacement_text,
                    plan.page_index,
                )?;
                marked_stack.push(semantic);
                output.push(operation);
            }
            "EMC" => {
                require_operands(&operation, 0, plan.page_index)?;
                marked_stack.pop().ok_or_else(|| {
                    unsupported(plan.page_index, "marked-content EMC stack underflow")
                })?;
                output.push(operation);
            }
            "MP" => {
                validate_marked_tag(document, &operation, plan.page_index, 1)?;
                output.push(operation);
            }
            "DP" => {
                validate_marked_tag(document, &operation, plan.page_index, 2)?;
                marked_property_is_semantic(
                    document,
                    resources,
                    &operation.operands[1],
                    &mut structure_replacement_text,
                    plan.page_index,
                )?;
                output.push(operation);
            }
            "w" => {
                let width = single_number(document, &operation, plan.page_index)?;
                if width < 0.0 {
                    return Err(unsupported(plan.page_index, "line width is negative"));
                }
                graphics.line_width = width;
                output.push(operation);
            }
            "M" => {
                let limit = single_number(document, &operation, plan.page_index)?;
                if limit < 1.0 {
                    return Err(unsupported(plan.page_index, "miter limit is less than one"));
                }
                graphics.miter_limit = limit;
                output.push(operation);
            }
            operator if is_text_only_operator(operator) => {
                return Err(unsupported(
                    plan.page_index,
                    format!("text operator {operator} appears outside BT..ET"),
                ));
            }
            _ => output.push(operation),
        }
    }

    if text_object.is_some() {
        return Err(unsupported(
            plan.page_index,
            "BT text object has no matching ET",
        ));
    }
    if !stack.is_empty() {
        return Err(unsupported(
            plan.page_index,
            "unbalanced q/Q graphics-state stack",
        ));
    }
    if !marked_stack.is_empty() {
        return Err(unsupported(
            plan.page_index,
            "unbalanced marked-content BMC/BDC/EMC stack",
        ));
    }
    Ok(ContentResult {
        operations: output,
        removed,
        removed_xobjects,
        removed_form_ids,
    })
}

fn process_text_operation(
    context: PageAnalysisContext<'_>,
    operation: &Operation,
    text: &mut TextObject,
    graphics: &mut GraphicsState,
    fonts: &mut FontResolver<'_>,
    marked_stack: &mut Vec<bool>,
    structure_replacement_text: &mut Option<bool>,
) -> Result<()> {
    let PageAnalysisContext {
        document,
        resources,
        page_index,
    } = context;
    match operation.operator.as_str() {
        "Tf" => {
            require_operands(operation, 2, page_index)?;
            let name = operation.operands[0]
                .as_name()
                .map_err(|_| unsupported(page_index, "Tf font operand is not a name"))?;
            let size = number(document, &operation.operands[1], page_index, "Tf font size")?;
            if size == 0.0 {
                return Err(unsupported(page_index, "Tf font size is zero"));
            }
            fonts.get(name)?;
            graphics.text.font = Some(name.to_vec());
            graphics.text.size = size;
        }
        "Tc" => graphics.text.char_spacing = single_number(document, operation, page_index)?,
        "Tw" => graphics.text.word_spacing = single_number(document, operation, page_index)?,
        "Tz" => {
            graphics.text.horizontal_scale =
                single_number(document, operation, page_index)? / 100.0;
            if graphics.text.horizontal_scale == 0.0 {
                return Err(unsupported(page_index, "Tz horizontal scale is zero"));
            }
        }
        "TL" => graphics.text.leading = single_number(document, operation, page_index)?,
        "Tr" => {
            require_operands(operation, 1, page_index)?;
            let mode = deref(document, &operation.operands[0])?
                .as_i64()
                .map_err(|_| unsupported(page_index, "Tr rendering mode is not an integer"))?;
            if !(0..=7).contains(&mode) {
                return Err(unsupported(
                    page_index,
                    format!("Tr rendering mode {mode} is invalid"),
                ));
            }
            graphics.text.rendering_mode = mode;
        }
        "Ts" => graphics.text.rise = single_number(document, operation, page_index)?,
        "Tm" => {
            text.matrix = operation_matrix(document, operation, page_index)?;
            text.line_matrix = text.matrix;
        }
        "Td" => {
            let [tx, ty] = two_numbers(document, operation, page_index)?;
            text.line_matrix = text.line_matrix.concat(Matrix::translation(tx, ty));
            text.matrix = text.line_matrix;
        }
        "TD" => {
            let [tx, ty] = two_numbers(document, operation, page_index)?;
            graphics.text.leading = -ty;
            text.line_matrix = text.line_matrix.concat(Matrix::translation(tx, ty));
            text.matrix = text.line_matrix;
        }
        "T*" => move_to_next_line(operation, text, &graphics.text, page_index)?,
        "Tj" => {
            require_operands(operation, 1, page_index)?;
            let bytes = operation.operands[0]
                .as_str()
                .map_err(|_| unsupported(page_index, "Tj operand is not a string"))?;
            show_text(bytes, text, graphics, fonts, page_index)?;
        }
        "TJ" => {
            require_operands(operation, 1, page_index)?;
            let array = operation.operands[0]
                .as_array()
                .map_err(|_| unsupported(page_index, "TJ operand is not an array"))?;
            if array.len() > MAX_TJ_ITEMS {
                return Err(unsupported(
                    page_index,
                    "TJ array exceeds safe analysis complexity limits",
                ));
            }
            for item in array {
                match deref(document, item)? {
                    Object::String(bytes, _) => {
                        show_text(bytes, text, graphics, fonts, page_index)?
                    }
                    Object::Integer(_) | Object::Real(_) => {
                        let adjustment = number(document, item, page_index, "TJ adjustment")?;
                        let advance = -adjustment / 1000.0
                            * graphics.text.size
                            * graphics.text.horizontal_scale;
                        text.matrix = text.matrix.concat(Matrix::translation(advance, 0.0));
                    }
                    _ => {
                        return Err(unsupported(
                            page_index,
                            "TJ array contains a non-string, non-number item",
                        ));
                    }
                }
            }
        }
        "'" => {
            require_operands(operation, 1, page_index)?;
            move_to_next_line_unchecked(text, &graphics.text);
            let bytes = operation.operands[0]
                .as_str()
                .map_err(|_| unsupported(page_index, "' operand is not a string"))?;
            show_text(bytes, text, graphics, fonts, page_index)?;
        }
        "\"" => {
            require_operands(operation, 3, page_index)?;
            graphics.text.word_spacing = number(
                document,
                &operation.operands[0],
                page_index,
                "\" word spacing",
            )?;
            graphics.text.char_spacing = number(
                document,
                &operation.operands[1],
                page_index,
                "\" character spacing",
            )?;
            move_to_next_line_unchecked(text, &graphics.text);
            let bytes = operation.operands[2]
                .as_str()
                .map_err(|_| unsupported(page_index, "\" text operand is not a string"))?;
            show_text(bytes, text, graphics, fonts, page_index)?;
        }
        "gs" => {
            apply_ext_gstate(document, resources, operation, graphics, fonts, page_index)?;
            text.post_graphics_ops.push(operation.clone());
        }
        "w" => {
            let width = single_number(document, operation, page_index)?;
            if width < 0.0 {
                return Err(unsupported(page_index, "line width is negative"));
            }
            graphics.line_width = width;
            text.post_graphics_ops.push(operation.clone());
        }
        "M" => {
            let limit = single_number(document, operation, page_index)?;
            if limit < 1.0 {
                return Err(unsupported(page_index, "miter limit is less than one"));
            }
            graphics.miter_limit = limit;
            text.post_graphics_ops.push(operation.clone());
        }
        "SCN" | "scn" => {
            validate_pattern_color_operation(document, operation, page_index)?;
            text.post_graphics_ops.push(operation.clone());
        }
        "BMC" => {
            validate_marked_tag(document, operation, page_index, 1)?;
            marked_stack.push(false);
        }
        "BDC" => {
            validate_marked_tag(document, operation, page_index, 2)?;
            let semantic = marked_property_is_semantic(
                document,
                resources,
                &operation.operands[1],
                structure_replacement_text,
                page_index,
            )?;
            text.unsafe_marked_content |= semantic;
            marked_stack.push(semantic);
        }
        "EMC" => {
            require_operands(operation, 0, page_index)?;
            if marked_stack.len() <= text.marked_depth_at_start {
                return Err(unsupported(
                    page_index,
                    "marked-content scope crosses into a BT..ET boundary",
                ));
            }
            marked_stack.pop();
        }
        "MP" => validate_marked_tag(document, operation, page_index, 1)?,
        "DP" => {
            validate_marked_tag(document, operation, page_index, 2)?;
            text.unsafe_marked_content |= marked_property_is_semantic(
                document,
                resources,
                &operation.operands[1],
                structure_replacement_text,
                page_index,
            )?;
        }
        "q" | "Q" | "cm" | "Do" | "BI" => {
            return Err(unsupported(
                page_index,
                format!(
                    "graphics operator {} inside BT..ET is outside the safe redaction envelope",
                    operation.operator
                ),
            ));
        }
        operator if is_replayable_graphics_operator(operator) => {
            text.post_graphics_ops.push(operation.clone());
        }
        operator => {
            return Err(unsupported(
                page_index,
                format!("operator {operator} inside BT..ET is outside the safe redaction envelope"),
            ));
        }
    }
    Ok(())
}

fn show_text(
    bytes: &[u8],
    text: &mut TextObject,
    graphics: &GraphicsState,
    fonts: &mut FontResolver<'_>,
    page_index: u32,
) -> Result<()> {
    text.shown_bytes = text.shown_bytes.checked_add(bytes.len()).ok_or_else(|| {
        unsupported(
            page_index,
            "text-object byte count overflowed supported limits",
        )
    })?;
    text.show_count = text.show_count.checked_add(1).ok_or_else(|| {
        unsupported(
            page_index,
            "text-object show count overflowed supported limits",
        )
    })?;
    if text.shown_bytes > MAX_TEXT_BYTES_PER_OBJECT || text.show_count > MAX_TEXT_SHOWS_PER_OBJECT {
        return Err(unsupported(
            page_index,
            "text object exceeds safe analysis complexity limits",
        ));
    }
    let font_name = graphics
        .text
        .font
        .as_deref()
        .ok_or_else(|| unsupported(page_index, "text-show operation has no selected font"))?;
    if graphics.text.size == 0.0 || !graphics.text.size.is_finite() {
        return Err(unsupported(
            page_index,
            "text-show operation has invalid font size",
        ));
    }
    let metrics = fonts.get(font_name)?;
    let glyph_x1 = metrics.x_min * graphics.text.size / 1000.0 * graphics.text.horizontal_scale;
    let glyph_x2 = metrics.x_max * graphics.text.size / 1000.0 * graphics.text.horizontal_scale;
    let glyph_left = glyph_x1.min(glyph_x2);
    let glyph_right = glyph_x1.max(glyph_x2);
    let mut advance = 0.0;
    let mut left = f64::INFINITY;
    let mut right = f64::NEG_INFINITY;
    metrics.for_each_code_width(bytes, page_index, |width, is_space| {
        left = left.min(advance + glyph_left);
        right = right.max(advance + glyph_right);
        let word_spacing = if is_space {
            graphics.text.word_spacing
        } else {
            0.0
        };
        let step =
            (width / 1000.0 * graphics.text.size + graphics.text.char_spacing + word_spacing)
                * graphics.text.horizontal_scale;
        advance += step;
        left = left.min(advance);
        right = right.max(advance);
        Ok(())
    })?;
    if ![advance, left, right].iter().all(|value| value.is_finite()) && !bytes.is_empty() {
        return Err(unsupported(page_index, "text advance is non-finite"));
    }

    if !bytes.is_empty() {
        if (4..=7).contains(&graphics.text.rendering_mode) {
            text.used_text_clip = true;
        }
        let lower = metrics.descent * graphics.text.size / 1000.0 + graphics.text.rise;
        let upper = metrics.ascent * graphics.text.size / 1000.0 + graphics.text.rise;
        let transform = graphics.ctm.concat(text.matrix);
        let mut bounds = transform_box(
            transform,
            [left, lower.min(upper), right, lower.max(upper)],
            page_index,
        )?;
        if matches!(graphics.text.rendering_mode, 1 | 2 | 5 | 6) {
            bounds = inflate_stroked_text(bounds, graphics, page_index)?;
        }
        text.bounds = Some(match text.bounds {
            Some(existing) => union_box(existing, bounds),
            None => bounds,
        });
    }
    text.matrix = text.matrix.concat(Matrix::translation(advance, 0.0));
    Ok(())
}

fn analyze_xobject(
    document: &Document,
    resources: Option<&Dictionary>,
    operation: &Operation,
    ctm: Matrix,
    plan: &PagePlan,
) -> Result<Option<(BoxPts, Vec<u8>, ObjectId)>> {
    require_operands(operation, 1, plan.page_index)?;
    let name = operation.operands[0]
        .as_name()
        .map_err(|_| unsupported(plan.page_index, "Do operand is not a name"))?;
    let object = resource_entry(document, resources, b"XObject", name, plan.page_index)?;
    let (xobject_id, resolved) = document.dereference(object).map_err(|error| {
        unsupported(
            plan.page_index,
            format!("XObject reference cannot be resolved: {error}"),
        )
    })?;
    let stream = resolved
        .as_stream()
        .map_err(|_| unsupported(plan.page_index, "Do resource is not a stream"))?;
    let subtype = stream
        .dict
        .get(b"Subtype")
        .map_err(|_| unsupported(plan.page_index, "XObject has no /Subtype"))?;
    let subtype = deref(document, subtype)?
        .as_name()
        .map_err(|_| unsupported(plan.page_index, "XObject /Subtype is not a name"))?;
    match subtype {
        b"Image" => {
            let _bounds = transform_box(ctm, [0.0, 0.0, 1.0, 1.0], plan.page_index)?;
            Ok(None)
        }
        b"Form" => {
            let bbox_object = stream.dict.get(b"BBox").map_err(|_| {
                unsupported(
                    plan.page_index,
                    format!(
                        "Form XObject /{} has no /BBox",
                        String::from_utf8_lossy(name)
                    ),
                )
            })?;
            let bbox_array = deref(document, bbox_object)?
                .as_array()
                .map_err(|_| unsupported(plan.page_index, "Form XObject /BBox is not an array"))?;
            let values =
                number_array::<4>(document, bbox_array, plan.page_index, "Form XObject /BBox")?;
            if values[0] == values[2] || values[1] == values[3] {
                return Err(unsupported(
                    plan.page_index,
                    "Form XObject /BBox is degenerate",
                ));
            }
            let matrix = match stream.dict.get(b"Matrix") {
                Ok(object) => {
                    let array = deref(document, object)?.as_array().map_err(|_| {
                        unsupported(plan.page_index, "Form XObject /Matrix is not an array")
                    })?;
                    let values = number_array::<6>(
                        document,
                        array,
                        plan.page_index,
                        "Form XObject /Matrix",
                    )?;
                    Matrix {
                        a: values[0],
                        b: values[1],
                        c: values[2],
                        d: values[3],
                        e: values[4],
                        f: values[5],
                    }
                }
                Err(_) => Matrix::IDENTITY,
            };
            let bounds = transform_box(
                ctm.concat(matrix),
                [values[0], values[1], values[2], values[3]],
                plan.page_index,
            )?;
            if bounds.area() <= 0.0 {
                return Err(unsupported(
                    plan.page_index,
                    "Form XObject has a degenerate placed /BBox",
                ));
            }
            if !intersects_any(bounds, &plan.boxes) {
                return Ok(None);
            }
            let form_id = xobject_id.ok_or_else(|| {
                unsupported(
                    plan.page_index,
                    "intersecting Form XObject is a direct stream and cannot be pruned safely",
                )
            })?;
            Ok(Some((bounds, name.to_vec(), form_id)))
        }
        other => Err(unsupported(
            plan.page_index,
            format!(
                "unsupported XObject subtype /{}",
                String::from_utf8_lossy(other)
            ),
        )),
    }
}

fn apply_ext_gstate(
    document: &Document,
    resources: Option<&Dictionary>,
    operation: &Operation,
    graphics: &mut GraphicsState,
    _fonts: &mut FontResolver<'_>,
    page_index: u32,
) -> Result<()> {
    require_operands(operation, 1, page_index)?;
    let name = operation.operands[0]
        .as_name()
        .map_err(|_| unsupported(page_index, "gs operand is not a name"))?;
    let object = resource_entry(document, resources, b"ExtGState", name, page_index)?;
    let dict = deref(document, object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "ExtGState resource is not a dictionary"))?;
    if let Ok(object) = dict.get(b"LW") {
        let width = number(document, object, page_index, "ExtGState /LW")?;
        if width < 0.0 {
            return Err(unsupported(page_index, "ExtGState /LW is negative"));
        }
        graphics.line_width = width;
    }
    if let Ok(object) = dict.get(b"ML") {
        let limit = number(document, object, page_index, "ExtGState /ML")?;
        if limit < 1.0 {
            return Err(unsupported(page_index, "ExtGState /ML is less than one"));
        }
        graphics.miter_limit = limit;
    }
    if let Ok(font_object) = dict.get(b"Font") {
        let _array = deref(document, font_object)?
            .as_array()
            .map_err(|_| unsupported(page_index, "ExtGState /Font is not an array"))?;
        return Err(unsupported(
            page_index,
            "ExtGState /Font cannot be represented safely after whole-object removal",
        ));
    }
    if let Ok(mask_object) = dict.get(b"SMask") {
        let mask = deref(document, mask_object)?;
        if mask.as_name().ok() != Some(b"None") {
            return Err(unsupported(
                page_index,
                "ExtGState soft masks are outside the safe redaction envelope",
            ));
        }
    }
    Ok(())
}

fn validate_pattern_color_operation(
    document: &Document,
    operation: &Operation,
    page_index: u32,
) -> Result<()> {
    if operation.operands.is_empty() {
        return Err(unsupported(
            page_index,
            format!("{} has no color operands", operation.operator),
        ));
    }
    for operand in &operation.operands {
        match deref(document, operand)? {
            Object::Integer(_) | Object::Real(_) => {}
            Object::Name(_) => {
                return Err(unsupported(
                    page_index,
                    "tiling or shading pattern color operations are outside the safe redaction envelope",
                ));
            }
            _ => {
                return Err(unsupported(
                    page_index,
                    format!(
                        "{} contains a non-numeric color operand",
                        operation.operator
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn parse_font(document: &Document, object: &Object, page_index: u32) -> Result<FontMetrics> {
    let dict = deref(document, object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "font resource is not a dictionary"))?;
    let subtype = dict
        .get(b"Subtype")
        .map_err(|_| unsupported(page_index, "font has no /Subtype"))?;
    let subtype = deref(document, subtype)?
        .as_name()
        .map_err(|_| unsupported(page_index, "font /Subtype is not a name"))?;
    match subtype {
        b"Type3" => Err(unsupported(
            page_index,
            "Type3 fonts are outside the pure redaction envelope",
        )),
        b"Type0" => parse_cid_font(document, dict, page_index),
        b"Type1" | b"TrueType" | b"MMType1" => parse_simple_font(document, dict, page_index),
        other => Err(unsupported(
            page_index,
            format!(
                "font subtype /{} is unsupported",
                String::from_utf8_lossy(other)
            ),
        )),
    }
}

fn parse_simple_font(
    document: &Document,
    dict: &Dictionary,
    page_index: u32,
) -> Result<FontMetrics> {
    let base_name = dict
        .get(b"BaseFont")
        .ok()
        .and_then(|object| deref(document, object).ok())
        .and_then(|object| object.as_name().ok());
    let subset_name = base_name.is_some_and(|name| {
        name.len() > 7 && name[6] == b'+' && name[..6].iter().all(|byte| byte.is_ascii_uppercase())
    });
    let standard_font = base_name.and_then(StandardFont::from_base_name);
    let standard = match (standard_font, dict.get(b"Encoding")) {
        (Some(font), Err(_)) if !subset_name => Some((font, StandardCodeEncoding::Builtin)),
        (Some(font), Ok(object)) if !subset_name => match deref(document, object)? {
            Object::Name(name) if name == b"WinAnsiEncoding" => {
                Some((font, StandardCodeEncoding::WinAnsi))
            }
            Object::Name(name) if name == b"StandardEncoding" => {
                Some((font, StandardCodeEncoding::Builtin))
            }
            _ => None,
        },
        (Some(_), _) | (None, _) => None,
    };
    let descriptor = dict
        .get(b"FontDescriptor")
        .ok()
        .map(|object| deref(document, object))
        .transpose()?
        .map(|object| {
            object
                .as_dict()
                .map_err(|_| unsupported(page_index, "font /FontDescriptor is not a dictionary"))
        })
        .transpose()?;
    let (ascent, descent, x_min, x_max) = font_extents(document, descriptor, page_index)?;
    let missing = descriptor
        .and_then(|descriptor| descriptor.get(b"MissingWidth").ok())
        .map(|object| number(document, object, page_index, "FontDescriptor /MissingWidth"))
        .transpose()?;

    let mut widths = Box::new([None; 256]);
    if let Ok(widths_object) = dict.get(b"Widths") {
        let first = dict
            .get(b"FirstChar")
            .map_err(|_| unsupported(page_index, "font with /Widths has no /FirstChar"))?;
        let first = deref(document, first)?
            .as_i64()
            .map_err(|_| unsupported(page_index, "font /FirstChar is not an integer"))?;
        let array = deref(document, widths_object)?
            .as_array()
            .map_err(|_| unsupported(page_index, "font /Widths is not an array"))?;
        for (offset, object) in array.iter().enumerate() {
            let code = first
                + i64::try_from(offset)
                    .map_err(|_| unsupported(page_index, "font /Widths array is too large"))?;
            if !(0..=255).contains(&code) {
                continue;
            }
            widths[code as usize] = Some(number(document, object, page_index, "font width")?);
        }
    } else if standard.is_none() {
        return Err(unsupported(
            page_index,
            "non-standard simple font has no /Widths array",
        ));
    }
    Ok(FontMetrics {
        widths: FontWidths::Simple {
            widths,
            missing,
            standard,
        },
        ascent,
        descent,
        x_min,
        x_max,
    })
}

fn parse_cid_font(document: &Document, dict: &Dictionary, page_index: u32) -> Result<FontMetrics> {
    let encoding = dict
        .get(b"Encoding")
        .map_err(|_| unsupported(page_index, "Type0 font has no /Encoding"))?;
    let encoding = deref(document, encoding)?
        .as_name()
        .map_err(|_| unsupported(page_index, "Type0 /Encoding is not a named CMap"))?;
    if encoding != b"Identity-H" {
        return Err(unsupported(
            page_index,
            format!(
                "Type0 CMap /{} is unsupported; only /Identity-H is safe",
                String::from_utf8_lossy(encoding)
            ),
        ));
    }
    let descendants = dict
        .get(b"DescendantFonts")
        .map_err(|_| unsupported(page_index, "Type0 font has no /DescendantFonts"))?;
    let descendants = deref(document, descendants)?
        .as_array()
        .map_err(|_| unsupported(page_index, "Type0 /DescendantFonts is not an array"))?;
    if descendants.len() != 1 {
        return Err(unsupported(
            page_index,
            "Type0 font must have exactly one descendant",
        ));
    }
    let descendant = deref(document, &descendants[0])?
        .as_dict()
        .map_err(|_| unsupported(page_index, "CID descendant is not a dictionary"))?;
    let descendant_subtype = descendant
        .get(b"Subtype")
        .map_err(|_| unsupported(page_index, "CID descendant has no /Subtype"))?;
    let descendant_subtype = deref(document, descendant_subtype)?
        .as_name()
        .map_err(|_| unsupported(page_index, "CID descendant /Subtype is not a name"))?;
    if !matches!(descendant_subtype, b"CIDFontType0" | b"CIDFontType2") {
        return Err(unsupported(
            page_index,
            format!(
                "CID descendant subtype /{} is unsupported",
                String::from_utf8_lossy(descendant_subtype)
            ),
        ));
    }
    let descriptor = descendant
        .get(b"FontDescriptor")
        .ok()
        .map(|object| deref(document, object))
        .transpose()?
        .map(|object| {
            object
                .as_dict()
                .map_err(|_| unsupported(page_index, "CID /FontDescriptor is not a dictionary"))
        })
        .transpose()?;
    let (ascent, descent, x_min, x_max) = font_extents(document, descriptor, page_index)?;
    let default_width = descendant
        .get(b"DW")
        .map(|object| number(document, object, page_index, "CID /DW"))
        .unwrap_or(Ok(1000.0))?;
    let mut widths = BTreeMap::new();
    if let Ok(object) = descendant.get(b"W") {
        let array = deref(document, object)?
            .as_array()
            .map_err(|_| unsupported(page_index, "CID /W is not an array"))?;
        parse_cid_widths(document, array, &mut widths, page_index)?;
    }
    Ok(FontMetrics {
        widths: FontWidths::Cid {
            widths,
            default_width,
        },
        ascent,
        descent,
        x_min,
        x_max,
    })
}

fn parse_cid_widths(
    document: &Document,
    array: &[Object],
    widths: &mut BTreeMap<u16, f64>,
    page_index: u32,
) -> Result<()> {
    let mut index = 0usize;
    while index < array.len() {
        let start = cid_number(document, &array[index], page_index)?;
        index += 1;
        let next = array
            .get(index)
            .ok_or_else(|| unsupported(page_index, "CID /W ends after a start CID"))?;
        match deref(document, next)? {
            Object::Array(values) => {
                for (offset, value) in values.iter().enumerate() {
                    let cid = usize::from(start) + offset;
                    let cid = u16::try_from(cid)
                        .map_err(|_| unsupported(page_index, "CID /W range exceeds 65535"))?;
                    widths.insert(cid, number(document, value, page_index, "CID width")?);
                }
                index += 1;
            }
            Object::Integer(_) => {
                let end = cid_number(document, next, page_index)?;
                let width_object = array
                    .get(index + 1)
                    .ok_or_else(|| unsupported(page_index, "CID /W range has no width"))?;
                let width = number(document, width_object, page_index, "CID range width")?;
                if end < start {
                    return Err(unsupported(page_index, "CID /W range is inverted"));
                }
                for cid in start..=end {
                    widths.insert(cid, width);
                }
                index += 2;
            }
            _ => return Err(unsupported(page_index, "CID /W contains an invalid range")),
        }
    }
    Ok(())
}

fn font_extents(
    document: &Document,
    descriptor: Option<&Dictionary>,
    page_index: u32,
) -> Result<(f64, f64, f64, f64)> {
    let Some(descriptor) = descriptor else {
        return Ok((1000.0, -350.0, -500.0, 1500.0));
    };
    let ascent = descriptor
        .get(b"Ascent")
        .ok()
        .map(|object| number(document, object, page_index, "FontDescriptor /Ascent"))
        .transpose()?;
    let descent = descriptor
        .get(b"Descent")
        .ok()
        .map(|object| number(document, object, page_index, "FontDescriptor /Descent"))
        .transpose()?;
    let (ascent, descent) = match (ascent, descent) {
        (Some(ascent), Some(descent)) if ascent > descent => (ascent, descent),
        _ => (1000.0, -350.0),
    };
    let (x_min, x_max) = match descriptor.get(b"FontBBox") {
        Ok(object) => {
            let array = deref(document, object)?
                .as_array()
                .map_err(|_| unsupported(page_index, "FontDescriptor /FontBBox is not an array"))?;
            let values =
                number_array::<4>(document, array, page_index, "FontDescriptor /FontBBox")?;
            if values[0] < values[2] {
                (values[0], values[2])
            } else {
                (-500.0, 1500.0)
            }
        }
        Err(_) => (-500.0, 1500.0),
    };
    Ok((ascent, descent, x_min, x_max))
}

fn remove_annotations(document: &mut Document, plan: &PagePlan) -> Result<AnnotationRemoval> {
    let entries = annotation_entries(document, plan.page_id, plan.page_index)?;
    let Some(entries) = entries else {
        return Ok(AnnotationRemoval {
            removed: Vec::new(),
            object_ids: BTreeSet::new(),
        });
    };

    struct AnnotationInfo {
        entry: Object,
        id: Option<ObjectId>,
        bounds: BoxPts,
        widget: bool,
        links: Vec<ObjectId>,
    }

    let mut annotations = Vec::with_capacity(entries.len());
    for entry in entries {
        let (id, object) = document.dereference(&entry).map_err(|error| {
            unsupported(
                plan.page_index,
                format!("page annotation reference cannot be resolved: {error}"),
            )
        })?;
        let dict = object
            .as_dict()
            .map_err(|_| unsupported(plan.page_index, "page /Annots entry is not a dictionary"))?;
        let bounds = annotation_rect(document, dict, plan.page_index)?;
        let widget = dict
            .get(b"Subtype")
            .ok()
            .and_then(|object| deref(document, object).ok())
            .and_then(|object| object.as_name().ok())
            == Some(b"Widget");
        let links = [b"Popup".as_slice(), b"Parent".as_slice(), b"IRT".as_slice()]
            .iter()
            .filter_map(|key| dict.get(key).ok())
            .filter_map(|object| document.dereference(object).ok()?.0)
            .collect();
        annotations.push(AnnotationInfo {
            entry,
            id,
            bounds,
            widget,
            links,
        });
    }

    let mut doomed: BTreeSet<usize> = annotations
        .iter()
        .enumerate()
        .filter(|(_, annotation)| intersects_any(annotation.bounds, &plan.boxes))
        .map(|(index, _)| index)
        .collect();
    loop {
        let doomed_ids: BTreeSet<ObjectId> = doomed
            .iter()
            .filter_map(|index| annotations[*index].id)
            .collect();
        let linked: Vec<usize> = annotations
            .iter()
            .enumerate()
            .filter(|(index, annotation)| {
                !doomed.contains(index)
                    && (annotation.links.iter().any(|id| doomed_ids.contains(id))
                        || annotation.id.is_some_and(|id| {
                            doomed
                                .iter()
                                .any(|doomed_index| annotations[*doomed_index].links.contains(&id))
                        }))
            })
            .map(|(index, _)| index)
            .collect();
        if linked.is_empty() {
            break;
        }
        doomed.extend(linked);
    }

    if doomed.iter().any(|index| annotations[*index].widget) {
        return Err(unsupported(
            plan.page_index,
            "intersecting Widget annotation may remain reachable from AcroForm fields",
        ));
    }
    if doomed.len() > MAX_REMOVED_OBJECTS_PER_PAGE {
        return Err(unsupported(
            plan.page_index,
            "redaction intersects too many annotations for safe rewriting",
        ));
    }
    let mut kept = Vec::new();
    let mut removed = Vec::new();
    let mut removed_ids = BTreeSet::new();
    for (index, annotation) in annotations.into_iter().enumerate() {
        if doomed.contains(&index) {
            removed.push((annotation.bounds, "annotation"));
            if let Some(id) = annotation.id {
                removed_ids.insert(id);
            }
        } else {
            kept.push(annotation.entry);
        }
    }
    let page = document.get_dictionary_mut(plan.page_id).map_err(|error| {
        unsupported(
            plan.page_index,
            format!("page dictionary cannot be updated: {error}"),
        )
    })?;
    if kept.is_empty() {
        page.remove(b"Annots");
    } else {
        page.set("Annots", Object::Array(kept));
    }
    Ok(AnnotationRemoval {
        removed,
        object_ids: removed_ids,
    })
}

fn verify_annotations(document: &Document, plan: &PagePlan) -> Result<()> {
    let Some(entries) = annotation_entries(document, plan.page_id, plan.page_index)? else {
        return Ok(());
    };
    for entry in entries {
        let dict = deref(document, &entry)?
            .as_dict()
            .map_err(|_| unsupported(plan.page_index, "page /Annots entry is not a dictionary"))?;
        let bounds = annotation_rect(document, dict, plan.page_index)?;
        if intersects_any(bounds, &plan.boxes) {
            return Err(unsupported(
                plan.page_index,
                "output verification found an intersecting annotation",
            ));
        }
    }
    Ok(())
}

fn annotation_entries(
    document: &Document,
    page_id: ObjectId,
    page_index: u32,
) -> Result<Option<Vec<Object>>> {
    let page = document.get_dictionary(page_id).map_err(|error| {
        unsupported(
            page_index,
            format!("page dictionary cannot be read: {error}"),
        )
    })?;
    let Ok(object) = page.get(b"Annots") else {
        return Ok(None);
    };
    let array = deref(document, object)?
        .as_array()
        .map_err(|_| unsupported(page_index, "page /Annots is not an array"))?;
    Ok(Some(array.clone()))
}

fn annotation_rect(document: &Document, dict: &Dictionary, page_index: u32) -> Result<BoxPts> {
    let object = dict
        .get(b"Rect")
        .map_err(|_| unsupported(page_index, "annotation has no /Rect"))?;
    let array = deref(document, object)?
        .as_array()
        .map_err(|_| unsupported(page_index, "annotation /Rect is not an array"))?;
    let values = number_array::<4>(document, array, page_index, "annotation /Rect")?;
    normalized_box(values, page_index, "annotation /Rect")
}

fn detach_unused_xobjects(
    document: &mut Document,
    page_id: ObjectId,
    operations: &[Operation],
    removed_names: &BTreeSet<Vec<u8>>,
    page_index: u32,
) -> Result<()> {
    if removed_names.is_empty() {
        return Ok(());
    }
    let remaining: BTreeSet<&[u8]> = operations
        .iter()
        .filter(|operation| operation.operator == "Do")
        .filter_map(|operation| operation.operands.first())
        .filter_map(|operand| operand.as_name().ok())
        .collect();
    let unused: Vec<&Vec<u8>> = removed_names
        .iter()
        .filter(|name| !remaining.contains(name.as_slice()))
        .collect();
    if unused.is_empty() {
        return Ok(());
    }

    let resources_object = find_inherited(document, page_id, b"Resources")?
        .ok_or_else(|| unsupported(page_index, "removed XObject has no page /Resources"))?;
    let inherited_resources = deref(document, resources_object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "page /Resources is not a dictionary"))?;
    for operation in operations
        .iter()
        .filter(|operation| operation.operator == "Do")
    {
        let name = operation
            .operands
            .first()
            .and_then(|operand| operand.as_name().ok())
            .ok_or_else(|| unsupported(page_index, "surviving Do operand is not a name"))?;
        let object = resource_entry(
            document,
            Some(inherited_resources),
            b"XObject",
            name,
            page_index,
        )?;
        let stream = deref(document, object)?
            .as_stream()
            .map_err(|_| unsupported(page_index, "surviving XObject is not a stream"))?;
        if stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|object| deref(document, object).ok())
            .and_then(|object| object.as_name().ok())
            == Some(b"Form")
        {
            let form_resources = stream.dict.get(b"Resources").map_err(|_| {
                unsupported(
                    page_index,
                    "a surviving Form inherits page resources that contain a removed XObject",
                )
            })?;
            deref(document, form_resources)?.as_dict().map_err(|_| {
                unsupported(page_index, "surviving Form /Resources is not a dictionary")
            })?;
        }
    }
    let mut resources = inherited_resources.clone();
    let xobjects_object = resources
        .get(b"XObject")
        .map_err(|_| unsupported(page_index, "removed XObject has no resource dictionary"))?;
    let mut xobjects = deref(document, xobjects_object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "page /Resources/XObject is not a dictionary"))?
        .clone();
    for name in unused {
        xobjects.remove(name);
    }
    if xobjects.is_empty() {
        resources.remove(b"XObject");
    } else {
        resources.set("XObject", Object::Dictionary(xobjects));
    }
    document
        .get_dictionary_mut(page_id)
        .map_err(|error| {
            unsupported(
                page_index,
                format!("page resources cannot be updated: {error}"),
            )
        })?
        .set("Resources", Object::Dictionary(resources));
    Ok(())
}

fn ensure_removed_objects_are_unreachable(
    document: &Document,
    removed_ids: &BTreeSet<ObjectId>,
    kind: &str,
) -> Result<()> {
    if let Some(id) = removed_ids
        .iter()
        .find(|id| document.objects.contains_key(id))
    {
        return Err(PdfCoreError::Unsupported(format!(
            "removed {kind} {} {} remains reachable elsewhere in the PDF",
            id.0, id.1
        )));
    }
    Ok(())
}

fn replace_page_content(
    document: &mut Document,
    page_id: ObjectId,
    mut operations: Vec<Operation>,
    coverage: &[BoxPts],
    page_index: u32,
) -> Result<()> {
    let mut wrapped = Vec::with_capacity(operations.len() + coverage.len() + 8);
    wrapped.push(Operation::new("q", vec![]));
    wrapped.append(&mut operations);
    wrapped.push(Operation::new("Q", vec![]));
    wrapped.push(Operation::new("q", vec![]));
    wrapped.push(Operation::new("rg", vec![0.into(), 0.into(), 0.into()]));
    for bounds in coverage {
        wrapped.push(Operation::new(
            "re",
            vec![
                bounds.left.into(),
                bounds.bottom.into(),
                (bounds.right - bounds.left).into(),
                (bounds.top - bounds.bottom).into(),
            ],
        ));
        wrapped.push(Operation::new("f", vec![]));
    }
    wrapped.push(Operation::new("Q", vec![]));
    let bytes = encode_operations(&wrapped, page_index)?;
    let mut stream = Stream::new(Dictionary::new(), bytes);
    let _ = stream.compress();
    let stream_id = document.add_object(stream);
    document
        .get_dictionary_mut(page_id)
        .map_err(|error| {
            unsupported(
                page_index,
                format!("page content cannot be replaced: {error}"),
            )
        })?
        .set("Contents", Object::Reference(stream_id));
    Ok(())
}

fn encode_operations(operations: &[Operation], page_index: u32) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    for (index, operation) in operations.iter().enumerate() {
        if index > 0 {
            output.push(b'\n');
        }
        if operation.operator != "BI" {
            output.extend(
                Content {
                    operations: std::slice::from_ref(operation),
                }
                .encode()
                .map_err(|error| {
                    unsupported(
                        page_index,
                        format!("content operation could not be encoded: {error}"),
                    )
                })?,
            );
            continue;
        }
        let stream = inline_image_stream(operation, page_index)?;
        output.extend_from_slice(b"BI\n");
        let mut operands = Vec::with_capacity(stream.dict.len() * 2);
        for (key, value) in stream.dict.iter() {
            if key.as_slice() == b"Length" {
                continue;
            }
            operands.push(Object::Name(key.clone()));
            operands.push(value.clone());
        }
        output.extend(
            Content {
                operations: [Operation::new("ID", operands)],
            }
            .encode()
            .map_err(|error| {
                unsupported(
                    page_index,
                    format!("inline image header could not be encoded: {error}"),
                )
            })?,
        );
        output.push(b'\n');
        output.extend_from_slice(&stream.content);
        output.extend_from_slice(b"\nEI");
    }
    Ok(output)
}

fn validate_inline_image(operation: &Operation, page_index: u32) -> Result<()> {
    let _ = inline_image_stream(operation, page_index)?;
    Ok(())
}

fn inline_image_stream(operation: &Operation, page_index: u32) -> Result<&Stream> {
    if operation.operands.len() != 1 {
        return Err(unsupported(
            page_index,
            "inline image data could not be parsed safely",
        ));
    }
    operation.operands[0]
        .as_stream()
        .map_err(|_| unsupported(page_index, "inline image operand is not a parsed stream"))
}

fn style_carrier(style: &TextStyle) -> Vec<Operation> {
    let mut operations = vec![Operation::new("BT", vec![])];
    if let Some(font) = &style.font {
        operations.push(Operation::new(
            "Tf",
            vec![Object::Name(font.clone()), (style.size as f32).into()],
        ));
    }
    operations.extend([
        Operation::new("Tc", vec![(style.char_spacing as f32).into()]),
        Operation::new("Tw", vec![(style.word_spacing as f32).into()]),
        Operation::new("Tz", vec![(style.horizontal_scale as f32 * 100.0).into()]),
        Operation::new("TL", vec![(style.leading as f32).into()]),
        Operation::new("Tr", vec![style.rendering_mode.into()]),
        Operation::new("Ts", vec![(style.rise as f32).into()]),
        Operation::new("ET", vec![]),
    ]);
    operations
}

fn transform_box(matrix: Matrix, values: [f64; 4], page_index: u32) -> Result<BoxPts> {
    if !matrix.is_finite() || !values.iter().all(|value| value.is_finite()) {
        return Err(unsupported(
            page_index,
            "object bounds contain non-finite values",
        ));
    }
    let left = values[0].min(values[2]);
    let right = values[0].max(values[2]);
    let bottom = values[1].min(values[3]);
    let top = values[1].max(values[3]);
    let points = [
        matrix.point(left, bottom),
        matrix.point(left, top),
        matrix.point(right, bottom),
        matrix.point(right, top),
    ];
    let min_x = points
        .iter()
        .map(|point| point.0)
        .fold(f64::INFINITY, f64::min)
        - BBOX_MARGIN_PT;
    let max_x = points
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max)
        + BBOX_MARGIN_PT;
    let min_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min)
        - BBOX_MARGIN_PT;
    let max_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max)
        + BBOX_MARGIN_PT;
    if ![min_x, max_x, min_y, max_y]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err(unsupported(
            page_index,
            "transformed object bounds are non-finite",
        ));
    }
    let bounds = BoxPts {
        left: min_x as f32,
        right: max_x as f32,
        bottom: min_y as f32,
        top: max_y as f32,
    };
    if ![bounds.left, bounds.right, bounds.bottom, bounds.top]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err(unsupported(
            page_index,
            "transformed object bounds exceed supported precision",
        ));
    }
    Ok(bounds)
}

fn inflate_stroked_text(
    bounds: BoxPts,
    graphics: &GraphicsState,
    page_index: u32,
) -> Result<BoxPts> {
    let radius = graphics.line_width * graphics.miter_limit / 2.0;
    let x_margin = radius * (graphics.ctm.a.abs() + graphics.ctm.c.abs());
    let y_margin = radius * (graphics.ctm.b.abs() + graphics.ctm.d.abs());
    if ![x_margin, y_margin].iter().all(|value| value.is_finite()) {
        return Err(unsupported(
            page_index,
            "stroked text bounds are non-finite",
        ));
    }
    let bounds = BoxPts {
        left: (f64::from(bounds.left) - x_margin) as f32,
        right: (f64::from(bounds.right) + x_margin) as f32,
        bottom: (f64::from(bounds.bottom) - y_margin) as f32,
        top: (f64::from(bounds.top) + y_margin) as f32,
    };
    if ![bounds.left, bounds.right, bounds.bottom, bounds.top]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err(unsupported(
            page_index,
            "stroked text bounds exceed supported precision",
        ));
    }
    Ok(bounds)
}

fn normalized_box(values: [f64; 4], page_index: u32, label: &str) -> Result<BoxPts> {
    if !values.iter().all(|value| value.is_finite()) {
        return Err(unsupported(
            page_index,
            format!("{label} contains non-finite values"),
        ));
    }
    let bounds = BoxPts {
        left: values[0].min(values[2]) as f32,
        right: values[0].max(values[2]) as f32,
        bottom: values[1].min(values[3]) as f32,
        top: values[1].max(values[3]) as f32,
    };
    if !bounds.is_finite_and_nonempty() {
        return Err(unsupported(
            page_index,
            format!("{label} is degenerate or exceeds supported precision"),
        ));
    }
    Ok(bounds)
}

fn union_box(a: BoxPts, b: BoxPts) -> BoxPts {
    BoxPts {
        left: a.left.min(b.left),
        right: a.right.max(b.right),
        bottom: a.bottom.min(b.bottom),
        top: a.top.max(b.top),
    }
}

fn intersects_any(bounds: BoxPts, boxes: &[BoxPts]) -> bool {
    boxes
        .iter()
        .any(|area| area.intersects(bounds.left, bounds.right, bounds.bottom, bounds.top))
}

fn move_to_next_line(
    operation: &Operation,
    text: &mut TextObject,
    style: &TextStyle,
    page_index: u32,
) -> Result<()> {
    require_operands(operation, 0, page_index)?;
    move_to_next_line_unchecked(text, style);
    Ok(())
}

fn move_to_next_line_unchecked(text: &mut TextObject, style: &TextStyle) {
    text.line_matrix = text
        .line_matrix
        .concat(Matrix::translation(0.0, -style.leading));
    text.matrix = text.line_matrix;
}

fn operation_matrix(document: &Document, operation: &Operation, page_index: u32) -> Result<Matrix> {
    require_operands(operation, 6, page_index)?;
    let values = operation
        .operands
        .iter()
        .map(|object| number(document, object, page_index, "matrix operand"))
        .collect::<Result<Vec<_>>>()?;
    let matrix = Matrix {
        a: values[0],
        b: values[1],
        c: values[2],
        d: values[3],
        e: values[4],
        f: values[5],
    };
    if !matrix.is_finite() {
        return Err(unsupported(page_index, "matrix contains non-finite values"));
    }
    Ok(matrix)
}

fn single_number(document: &Document, operation: &Operation, page_index: u32) -> Result<f64> {
    require_operands(operation, 1, page_index)?;
    number(
        document,
        &operation.operands[0],
        page_index,
        &operation.operator,
    )
}

fn two_numbers(document: &Document, operation: &Operation, page_index: u32) -> Result<[f64; 2]> {
    require_operands(operation, 2, page_index)?;
    Ok([
        number(
            document,
            &operation.operands[0],
            page_index,
            &operation.operator,
        )?,
        number(
            document,
            &operation.operands[1],
            page_index,
            &operation.operator,
        )?,
    ])
}

fn number_array<const N: usize>(
    document: &Document,
    array: &[Object],
    page_index: u32,
    label: &str,
) -> Result<[f64; N]> {
    if array.len() != N {
        return Err(unsupported(
            page_index,
            format!("{label} must contain {N} numbers, found {}", array.len()),
        ));
    }
    let mut values = [0.0; N];
    for (index, object) in array.iter().enumerate() {
        values[index] = number(document, object, page_index, label)?;
    }
    Ok(values)
}

fn cid_number(document: &Document, object: &Object, page_index: u32) -> Result<u16> {
    let value = deref(document, object)?
        .as_i64()
        .map_err(|_| unsupported(page_index, "CID value is not an integer"))?;
    u16::try_from(value)
        .map_err(|_| unsupported(page_index, format!("CID {value} is outside 0..=65535")))
}

fn number(document: &Document, object: &Object, page_index: u32, label: &str) -> Result<f64> {
    let value = f64::from(
        deref(document, object)?
            .as_float()
            .map_err(|_| unsupported(page_index, format!("{label} is not numeric")))?,
    );
    if !value.is_finite() {
        return Err(unsupported(page_index, format!("{label} is non-finite")));
    }
    Ok(value)
}

fn require_operands(operation: &Operation, expected: usize, page_index: u32) -> Result<()> {
    if operation.operands.len() != expected {
        return Err(unsupported(
            page_index,
            format!(
                "operator {} expects {expected} operand(s), found {}",
                operation.operator,
                operation.operands.len()
            ),
        ));
    }
    Ok(())
}

fn is_text_only_operator(operator: &str) -> bool {
    matches!(
        operator,
        "Tf" | "Tc"
            | "Tw"
            | "Tz"
            | "TL"
            | "Tr"
            | "Ts"
            | "Tm"
            | "Td"
            | "TD"
            | "T*"
            | "Tj"
            | "TJ"
            | "'"
            | "\""
    )
}

fn is_replayable_graphics_operator(operator: &str) -> bool {
    matches!(
        operator,
        "G" | "g"
            | "RG"
            | "rg"
            | "K"
            | "k"
            | "CS"
            | "cs"
            | "SC"
            | "SCN"
            | "sc"
            | "scn"
            | "ri"
            | "i"
            | "J"
            | "j"
            | "d"
    )
}

fn validate_marked_tag(
    document: &Document,
    operation: &Operation,
    page_index: u32,
    operands: usize,
) -> Result<()> {
    require_operands(operation, operands, page_index)?;
    deref(document, &operation.operands[0])?
        .as_name()
        .map_err(|_| unsupported(page_index, "marked-content tag is not a name"))?;
    Ok(())
}

fn marked_property_is_semantic(
    document: &Document,
    resources: Option<&Dictionary>,
    object: &Object,
    structure_replacement_text: &mut Option<bool>,
    page_index: u32,
) -> Result<bool> {
    let resolved = deref(document, object)?;
    let dict = match resolved {
        Object::Name(name) => {
            let property = resource_entry(document, resources, b"Properties", name, page_index)?;
            deref(document, property)?.as_dict().map_err(|_| {
                unsupported(
                    page_index,
                    "marked-content /Properties resource is not a dictionary",
                )
            })?
        }
        Object::Dictionary(dict) => dict,
        _ => {
            return Err(unsupported(
                page_index,
                "marked-content property list is neither a name nor a dictionary",
            ));
        }
    };
    if [b"ActualText".as_slice(), b"Alt", b"E"]
        .iter()
        .any(|key| dict.has(key))
    {
        return Ok(true);
    }
    if !dict.has(b"MCID") {
        return Ok(false);
    }
    if let Some(value) = *structure_replacement_text {
        return Ok(value);
    }
    let value = structure_tree_has_replacement_text(document)?;
    *structure_replacement_text = Some(value);
    Ok(value)
}

fn structure_tree_has_replacement_text(document: &Document) -> Result<bool> {
    let catalog = document.catalog().map_err(|error| {
        PdfCoreError::Unsupported(format!(
            "PDF catalog cannot be inspected for tagged replacement text: {error}"
        ))
    })?;
    let Ok(root) = catalog.get(b"StructTreeRoot") else {
        return Ok(false);
    };
    let mut pending = vec![root];
    let mut visited = BTreeSet::new();
    let mut inspected = 0usize;
    while let Some(object) = pending.pop() {
        inspected += 1;
        if inspected > 100_000 {
            return Err(PdfCoreError::Unsupported(
                "tag structure exceeds safe inspection complexity limits".to_string(),
            ));
        }
        match object {
            Object::Reference(id) => {
                if visited.insert(*id) {
                    let referenced = document.objects.get(id).ok_or_else(|| {
                        PdfCoreError::Unsupported(format!(
                            "tag structure reference {} {} cannot be resolved",
                            id.0, id.1
                        ))
                    })?;
                    pending.push(referenced);
                }
            }
            Object::Array(values) => pending.extend(values),
            Object::Dictionary(dict) => {
                if [b"ActualText".as_slice(), b"Alt", b"E"]
                    .iter()
                    .any(|key| dict.has(key))
                {
                    return Ok(true);
                }
                pending.extend(dict.iter().map(|(_, value)| value));
            }
            Object::Stream(stream) => {
                if [b"ActualText".as_slice(), b"Alt", b"E"]
                    .iter()
                    .any(|key| stream.dict.has(key))
                {
                    return Ok(true);
                }
                pending.extend(stream.dict.iter().map(|(_, value)| value));
            }
            _ => {}
        }
    }
    Ok(false)
}

fn resource_entry<'a>(
    document: &'a Document,
    resources: Option<&'a Dictionary>,
    category: &[u8],
    name: &[u8],
    page_index: u32,
) -> Result<&'a Object> {
    let resources = resources.ok_or_else(|| {
        unsupported(
            page_index,
            format!(
                "page has no /Resources for /{}",
                String::from_utf8_lossy(name)
            ),
        )
    })?;
    let category_object = resources.get(category).map_err(|_| {
        unsupported(
            page_index,
            format!(
                "page resources have no /{} dictionary",
                String::from_utf8_lossy(category)
            ),
        )
    })?;
    let category_dict = deref(document, category_object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "resource category is not a dictionary"))?;
    category_dict.get(name).map_err(|_| {
        unsupported(
            page_index,
            format!(
                "resource /{} cannot be resolved",
                String::from_utf8_lossy(name)
            ),
        )
    })
}

fn find_inherited<'a>(
    document: &'a Document,
    page_id: ObjectId,
    key: &[u8],
) -> Result<Option<&'a Object>> {
    let mut dict = document.get_dictionary(page_id).map_err(|error| {
        PdfCoreError::Unsupported(format!("page dictionary cannot be read: {error}"))
    })?;
    for _ in 0..64 {
        if let Ok(object) = dict.get(key) {
            return Ok(Some(object));
        }
        match dict.get(b"Parent") {
            Ok(parent) => {
                dict = deref(document, parent)?.as_dict().map_err(|_| {
                    PdfCoreError::Unsupported("page /Parent is not a dictionary".to_string())
                })?;
            }
            Err(_) => return Ok(None),
        }
    }
    Err(PdfCoreError::Unsupported(
        "page inheritance chain exceeds 64 levels".to_string(),
    ))
}

fn deref<'a>(document: &'a Document, object: &'a Object) -> Result<&'a Object> {
    document
        .dereference(object)
        .map(|(_, object)| object)
        .map_err(|error| {
            PdfCoreError::Unsupported(format!(
                "PDF object reference cannot be resolved during redaction: {error}"
            ))
        })
}

fn unsupported(page_index: u32, message: impl Into<String>) -> PdfCoreError {
    PdfCoreError::Unsupported(format!("page {page_index}: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_concat_applies_rhs_before_lhs() {
        let translate = Matrix::translation(10.0, 20.0);
        let scale = Matrix {
            a: 2.0,
            d: 3.0,
            ..Matrix::IDENTITY
        };
        assert_eq!(translate.concat(scale).point(1.0, 1.0), (12.0, 23.0));
    }

    #[test]
    fn transformed_box_accounts_for_rotation() {
        let rotate = Matrix {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: 0.0,
            f: 0.0,
        };
        let bounds = transform_box(rotate, [0.0, 0.0, 10.0, 20.0], 0).unwrap();
        assert!(bounds.left <= -21.0 && bounds.right >= 1.0);
        assert!(bounds.bottom <= -1.0 && bounds.top >= 11.0);
    }
}

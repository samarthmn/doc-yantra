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
    BoxPts, PageRotation, RedactArea, RedactOutput, RedactionObjectKind, RedactionWarning,
    RedactionWarningCode, derotate_box, exceeds_requested, resolve_rect,
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
    clip: ActiveClip,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            text: TextStyle::default(),
            line_width: 1.0,
            miter_limit: 10.0,
            clip: ActiveClip::Unbounded,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy)]
enum ActiveClip {
    Unbounded,
    Empty { last_rule: ClipRule },
    ConservativeBounds { bounds: BoxPts, last_rule: ClipRule },
}

impl ActiveClip {
    fn intersect(&mut self, path_bounds: Option<BoxPts>, rule: ClipRule) {
        *self = match (*self, path_bounds) {
            (_, None) => Self::Empty { last_rule: rule },
            (Self::Unbounded, Some(bounds)) => Self::ConservativeBounds {
                bounds,
                last_rule: rule,
            },
            (Self::Empty { last_rule }, Some(_)) => {
                let _winding_history = (last_rule, rule);
                Self::Empty { last_rule: rule }
            }
            (
                Self::ConservativeBounds {
                    bounds: active,
                    last_rule,
                },
                Some(path),
            ) => {
                let _winding_history = (last_rule, rule);
                let bounds = BoxPts {
                    left: active.left.max(path.left),
                    right: active.right.min(path.right),
                    bottom: active.bottom.max(path.bottom),
                    top: active.top.min(path.top),
                };
                if bounds.is_finite_and_nonempty() {
                    Self::ConservativeBounds {
                        bounds,
                        last_rule: rule,
                    }
                } else {
                    Self::Empty { last_rule: rule }
                }
            }
        };
    }

    fn conservative_bounds(self, page_bounds: BoxPts) -> Option<BoxPts> {
        match self {
            Self::Unbounded => Some(page_bounds),
            Self::Empty { .. } => None,
            Self::ConservativeBounds { bounds, .. } => intersect_box(bounds, page_bounds),
        }
    }
}

#[derive(Debug, Clone)]
struct TextObject {
    operations: Vec<Operation>,
    post_graphics_ops: Vec<Operation>,
    matrix: Matrix,
    line_matrix: Matrix,
    removed_glyphs: Vec<BoxPts>,
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
            removed_glyphs: Vec::new(),
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
    removed: Vec<(BoxPts, RedactionObjectKind)>,
    object_ids: BTreeSet<ObjectId>,
}

#[derive(Debug, Clone)]
enum FontWidths {
    Simple {
        widths: Box<[Option<f64>; 256]>,
        missing: Option<f64>,
        standard: Option<(StandardFont, StandardCodeEncoding)>,
        valid_codes: Option<Box<[bool; 256]>>,
    },
    Cid {
        widths: BTreeMap<u16, f64>,
        default_width: f64,
        vertical_widths: BTreeMap<u16, VerticalMetric>,
        default_vertical: VerticalMetric,
        encoding: CidCodeMap,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritingMode {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy)]
struct VerticalMetric {
    /// `w1y`: the vertical advance, negative because vertical text runs downward.
    displacement_y: f64,
    /// `v_x`: the horizontal component of the position vector. Defaults to half the
    /// glyph's width, which centres the glyph on the vertical baseline.
    position_x: Option<f64>,
    // `v_y` is parsed but not carried: for the standard vertical origin it places the
    // em box exactly where the advance already puts it, so the displacement-derived
    // cell below is equivalent. Deriving the cell from `v_y` and the font's ascent
    // instead would inflate it past the advance and start swallowing the neighbouring
    // glyph in a vertical run.
}

#[derive(Debug, Clone, Copy)]
struct DecodedCode {
    start: usize,
    end: usize,
    width: f64,
    is_space: bool,
    vertical: Option<VerticalMetric>,
    glyph_box: [f64; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CodeKey {
    len: u8,
    value: u32,
}

#[derive(Debug, Clone, Copy)]
struct CodeSpace {
    len: u8,
    start: u32,
    end: u32,
}

#[derive(Debug, Clone)]
struct EmbeddedCMap {
    codespaces: Vec<CodeSpace>,
    cid_map: BTreeMap<CodeKey, u16>,
    unicode_map: BTreeMap<CodeKey, Vec<u16>>,
    writing_mode: WritingMode,
}

#[derive(Debug, Clone)]
enum CidCodeMap {
    IdentityH,
    IdentityV,
    Embedded(EmbeddedCMap),
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
    glyph_boxes: Option<Box<[Option<[f64; 4]>; 256]>>,
    cid_glyph_boxes: BTreeMap<u16, [f64; 4]>,
}

impl FontMetrics {
    fn writing_mode(&self) -> WritingMode {
        match &self.widths {
            FontWidths::Simple { .. } => WritingMode::Horizontal,
            FontWidths::Cid { encoding, .. } => match encoding {
                CidCodeMap::IdentityH => WritingMode::Horizontal,
                CidCodeMap::IdentityV => WritingMode::Vertical,
                CidCodeMap::Embedded(cmap) => cmap.writing_mode,
            },
        }
    }

    fn for_each_code(
        &self,
        bytes: &[u8],
        page_index: u32,
        mut visit: impl FnMut(DecodedCode) -> Result<()>,
    ) -> Result<()> {
        match &self.widths {
            FontWidths::Simple {
                widths,
                missing,
                standard,
                valid_codes,
            } => {
                for (start, code) in bytes.iter().enumerate() {
                    if valid_codes
                        .as_ref()
                        .is_some_and(|valid| !valid[usize::from(*code)])
                    {
                        return Err(unsupported(
                            page_index,
                            format!(
                                "Type3 character code {code} has no validated Encoding/CharProc"
                            ),
                        ));
                    }
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
                    let glyph_box = self
                        .glyph_boxes
                        .as_ref()
                        .and_then(|boxes| boxes[usize::from(*code)])
                        .unwrap_or([0.0, self.descent, width, self.ascent]);
                    visit(DecodedCode {
                        start,
                        end: start + 1,
                        width,
                        is_space: *code == b' ',
                        vertical: None,
                        glyph_box,
                    })?;
                }
                Ok(())
            }
            FontWidths::Cid {
                widths,
                default_width,
                vertical_widths,
                default_vertical,
                encoding,
            } => {
                let mut offset = 0usize;
                while offset < bytes.len() {
                    let (consumed, cid, is_space) = match encoding {
                        CidCodeMap::IdentityH | CidCodeMap::IdentityV => {
                            let pair = bytes.get(offset..offset + 2).ok_or_else(|| {
                                unsupported(
                                    page_index,
                                    "Identity-H text string contains an incomplete two-byte CID",
                                )
                            })?;
                            (2, u16::from_be_bytes([pair[0], pair[1]]), false)
                        }
                        CidCodeMap::Embedded(cmap) => {
                            let (consumed, key) = cmap.decode_next(bytes, offset, page_index)?;
                            let cid = cmap.cid_map.get(&key).copied().ok_or_else(|| {
                                unsupported(
                                    page_index,
                                    format!(
                                        "embedded CMap has no CID mapping for {}-byte code {:X}",
                                        key.len, key.value
                                    ),
                                )
                            })?;
                            cmap.unicode_map.get(&key).ok_or_else(|| {
                                unsupported(
                                    page_index,
                                    format!(
                                        "ToUnicode has no mapping for {}-byte code {:X}",
                                        key.len, key.value
                                    ),
                                )
                            })?;
                            // PDF word spacing is keyed to a one-byte source
                            // character code 0x20. ToUnicode affects extraction,
                            // not text positioning.
                            (consumed, cid, key.len == 1 && key.value == 0x20)
                        }
                    };
                    let width = widths.get(&cid).copied().unwrap_or(*default_width);
                    if width < 0.0 {
                        return Err(unsupported(
                            page_index,
                            format!("font has a negative width for CID {cid}"),
                        ));
                    }
                    let vertical = (self.writing_mode() == WritingMode::Vertical).then(|| {
                        let mut metric = vertical_widths
                            .get(&cid)
                            .copied()
                            .unwrap_or(*default_vertical);
                        if metric.position_x.is_none() {
                            metric.position_x = Some(width / 2.0);
                        }
                        metric
                    });
                    visit(DecodedCode {
                        start: offset,
                        end: offset + consumed,
                        width,
                        is_space,
                        vertical,
                        glyph_box: self.cid_glyph_boxes.get(&cid).copied().unwrap_or([
                            0.0,
                            self.descent,
                            width,
                            self.ascent,
                        ]),
                    })?;
                    offset += consumed;
                }
                Ok(())
            }
        }
    }
}

impl EmbeddedCMap {
    fn decode_next(
        &self,
        bytes: &[u8],
        offset: usize,
        page_index: u32,
    ) -> Result<(usize, CodeKey)> {
        let mut matches = Vec::new();
        for length in 1..=4usize {
            let Some(code_bytes) = bytes.get(offset..offset + length) else {
                continue;
            };
            let value = code_value(code_bytes);
            if self.codespaces.iter().any(|space| {
                usize::from(space.len) == length && value >= space.start && value <= space.end
            }) {
                matches.push(CodeKey {
                    len: length as u8,
                    value,
                });
            }
        }
        if matches.len() != 1 {
            return Err(unsupported(
                page_index,
                if matches.is_empty() {
                    "text bytes do not match any embedded CMap codespace"
                } else {
                    "text bytes ambiguously match multiple embedded CMap codespaces"
                },
            ));
        }
        let key = matches[0];
        Ok((usize::from(key.len), key))
    }
}

fn code_value(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |value, byte| (value << 8) | u32::from(*byte))
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
    page_bounds: BoxPts,
    boxes: Vec<BoxPts>,
}

#[derive(Debug)]
struct ContentResult {
    operations: Vec<Operation>,
    removed: Vec<(BoxPts, RedactionObjectKind)>,
    removed_xobjects: BTreeSet<Vec<u8>>,
    removed_xobject_ids: BTreeSet<ObjectId>,
    removed_shadings: BTreeSet<Vec<u8>>,
    removed_shading_ids: BTreeSet<ObjectId>,
    rewritten_forms: BTreeMap<Vec<u8>, FormRewrite>,
    rewritten_form_ids: BTreeSet<ObjectId>,
}

#[derive(Debug)]
struct XObjectRemoval {
    bounds: BoxPts,
    name: Vec<u8>,
    object_ids: BTreeSet<ObjectId>,
    kind: RedactionObjectKind,
}

#[derive(Debug)]
struct FormRewrite {
    original_id: ObjectId,
    stream: Stream,
    resources: Dictionary,
    operations: Vec<Operation>,
    nested_forms: BTreeMap<Vec<u8>, FormRewrite>,
}

#[derive(Debug)]
enum XObjectAction {
    Keep,
    Remove(XObjectRemoval),
    Rewrite(FormRewrite, Box<ContentResult>),
}

#[derive(Debug, Default)]
struct PathGeometry {
    bounds: Option<BoxPts>,
    output_indices: Vec<usize>,
    pending_clip: Option<ClipRule>,
    has_current_point: bool,
    has_subpath_start: bool,
    stroke_scale_x: f64,
    stroke_scale_y: f64,
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
/// Intersecting text, raster, vector, shading, Form XObject, and annotation
/// content is removed in full. Requested and over-removed bounds receive opaque
/// black coverage. Saved bytes are reloaded and re-analyzed before this function
/// returns.
pub fn redact_pure(pdf: &[u8], areas: &[RedactArea]) -> Result<RedactOutput> {
    if areas.is_empty() {
        return Err(PdfCoreError::InvalidInput(
            "redaction request contains no areas".to_string(),
        ));
    }

    let mut document = Document::load_mem(pdf)?;
    crate::meta::guard_object_count(&document)?;
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

    let mut warnings = Vec::new();
    let mut removed_total = 0usize;
    let mut removed_xobject_ids = BTreeSet::new();
    let mut removed_shading_ids = BTreeSet::new();
    let mut rewritten_form_ids = BTreeSet::new();
    let mut removed_annotation_ids = BTreeSet::new();
    let mut pages_with_removed_content = BTreeSet::new();
    for plan in &plans {
        let annotation_removal = remove_annotations(&mut document, plan)?;
        removed_annotation_ids.extend(annotation_removal.object_ids);
        let result = analyze_page_content(&document, plan, AnalyzeMode::Rewrite)?;
        removed_xobject_ids.extend(result.removed_xobject_ids.iter().copied());
        removed_shading_ids.extend(result.removed_shading_ids.iter().copied());
        rewritten_form_ids.extend(result.rewritten_form_ids.iter().copied());
        if !result.removed.is_empty() {
            pages_with_removed_content.insert(plan.page_index);
            if structure_tree_has_replacement_text(&document)? {
                return Err(unsupported(
                    plan.page_index,
                    "removed page content may have replacement text in the structure tree",
                ));
            }
        }

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
                warnings.push(RedactionWarning {
                    code: RedactionWarningCode::OverRedaction,
                    page: plan.page_index,
                    kind: *kind,
                    message: format!(
                        "page {}: redaction removed a whole {} spanning ({:.1}, {:.1})-({:.1}, {:.1}) pt, beyond the requested area; the full bounds were covered with a black box",
                        plan.page_index,
                        kind.message_label(),
                        bounds.left,
                        bounds.bottom,
                        bounds.right,
                        bounds.top,
                    ),
                });
            }
        }

        let mut coverage = plan.boxes.clone();
        coverage.extend(
            removed
                .iter()
                .map(|(bounds, _)| *bounds)
                .filter(|bounds| bounds.area() > 0.0),
        );
        attach_rewritten_forms(
            &mut document,
            plan.page_id,
            result.rewritten_forms,
            plan.page_index,
        )?;
        detach_unused_xobjects(
            &mut document,
            plan.page_id,
            &result.operations,
            &result.removed_xobjects,
            plan.page_index,
        )?;
        detach_unused_shadings(
            &mut document,
            plan.page_id,
            &result.operations,
            &result.removed_shadings,
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
    ensure_removed_objects_are_unreachable(
        &document,
        &removed_xobject_ids,
        "XObject or image mask",
    )?;
    ensure_removed_objects_are_unreachable(&document, &removed_shading_ids, "shading resource")?;
    ensure_removed_objects_are_unreachable(
        &document,
        &removed_annotation_ids,
        "annotation or form field",
    )?;
    ensure_removed_objects_are_unreachable(
        &document,
        &rewritten_form_ids,
        "original rewritten Form XObject",
    )?;
    document.renumber_objects();
    let mut output = Vec::new();
    document.save_to(&mut output).map_err(|error| {
        PdfCoreError::Unsupported(format!("redaction output could not be serialized: {error}"))
    })?;

    verify_output(&output, areas, page_count, &pages_with_removed_content)?;
    if output == pdf {
        return Err(PdfCoreError::Unsupported(
            "redaction produced unchanged output".to_string(),
        ));
    }
    warnings.sort_by(|left, right| {
        (left.page, left.code, left.kind, left.message.as_str()).cmp(&(
            right.page,
            right.code,
            right.kind,
            right.message.as_str(),
        ))
    });
    Ok(RedactOutput {
        pdf: output,
        page_count,
        warnings,
    })
}

fn verify_output(
    pdf: &[u8],
    areas: &[RedactArea],
    expected_pages: u32,
    pages_with_removed_content: &BTreeSet<u32>,
) -> Result<()> {
    let document = Document::load_mem(pdf).map_err(|error| {
        PdfCoreError::Unsupported(format!("redaction output could not be reloaded: {error}"))
    })?;
    crate::meta::guard_object_count(&document)?;
    crate::meta::ensure_not_encrypted(pdf, &document, true)?;
    let actual_pages = u32::try_from(document.get_pages().len()).map_err(|_| {
        PdfCoreError::Unsupported("redaction output page count overflowed u32".to_string())
    })?;
    if actual_pages != expected_pages {
        return Err(PdfCoreError::Unsupported(format!(
            "redaction round-trip changed page count from {expected_pages} to {actual_pages}"
        )));
    }
    if !pages_with_removed_content.is_empty() && structure_tree_has_replacement_text(&document)? {
        return Err(PdfCoreError::Unsupported(
            "redaction output retains replacement text after page-content removal".to_string(),
        ));
    }

    for plan in build_page_plans(&document, areas)? {
        verify_annotations(&document, &plan)?;
        verify_acroform_fields(&document, &plan)?;
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
    if let Some(page_index) = grouped
        .keys()
        .find(|page_index| !page_by_index.contains_key(page_index))
    {
        return Err(out_of_range_page_error(*page_index, pages.len()));
    }

    let mut plans = Vec::new();
    for (page_index, rects) in grouped {
        let &page_id = page_by_index
            .get(&page_index)
            .ok_or_else(|| out_of_range_page_error(page_index, pages.len()))?;
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
                page_bounds: BoxPts {
                    left: origin_x as f32,
                    right: (origin_x + width) as f32,
                    bottom: origin_y as f32,
                    top: (origin_y + height) as f32,
                },
                boxes,
            });
        }
    }
    Ok(plans)
}

fn out_of_range_page_error(page_index: u32, page_count: usize) -> PdfCoreError {
    let page_label = if page_count == 1 { "page" } else { "pages" };
    PdfCoreError::InvalidInput(format!(
        "redaction page index {page_index} is out of range (document has {page_count} {page_label})"
    ))
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
    let mut content = decode_content_strict(&bytes, plan.page_index, "page content stream")?;
    if mode == AnalyzeMode::Verify {
        content.operations = strip_generated_coverage(document, content.operations, plan)?;
    }
    let resources = find_inherited(document, plan.page_id, b"Resources")?
        .map(|object| deref(document, object))
        .transpose()?
        .map(|object| {
            object
                .as_dict()
                .map_err(|_| unsupported(plan.page_index, "page /Resources is not a dictionary"))
        })
        .transpose()?;
    analyze_operations(
        document,
        resources,
        content.operations,
        plan,
        mode,
        GraphicsState::default(),
        &mut Vec::new(),
    )
}

fn strip_generated_coverage(
    document: &Document,
    operations: Vec<Operation>,
    plan: &PagePlan,
) -> Result<Vec<Operation>> {
    let black_index = operations
        .iter()
        .rposition(|operation| operation.operator == "rg")
        .ok_or_else(|| unsupported(plan.page_index, "output has no generated black coverage"))?;
    if black_index == 0
        || operations[black_index - 1].operator != "q"
        || operations
            .last()
            .map(|operation| operation.operator.as_str())
            != Some("Q")
    {
        return Err(unsupported(
            plan.page_index,
            "output generated coverage wrapper is malformed",
        ));
    }
    require_operands(&operations[black_index - 1], 0, plan.page_index)?;
    require_operands(&operations[black_index], 3, plan.page_index)?;
    if operations[black_index]
        .operands
        .iter()
        .map(|operand| number(document, operand, plan.page_index, "coverage color"))
        .collect::<Result<Vec<_>>>()?
        .iter()
        .any(|value| value.abs() > f64::EPSILON)
    {
        return Err(unsupported(
            plan.page_index,
            "output generated coverage is not opaque black",
        ));
    }
    require_operands(
        operations
            .last()
            .ok_or_else(|| unsupported(plan.page_index, "output content is empty"))?,
        0,
        plan.page_index,
    )?;

    let coverage_operations = &operations[black_index + 1..operations.len() - 1];
    if coverage_operations.is_empty() || !coverage_operations.len().is_multiple_of(2) {
        return Err(unsupported(
            plan.page_index,
            "output generated coverage rectangles are malformed",
        ));
    }
    let mut coverage_bounds = Vec::with_capacity(coverage_operations.len() / 2);
    for pair in coverage_operations.chunks_exact(2) {
        if pair[0].operator != "re" || pair[1].operator != "f" {
            return Err(unsupported(
                plan.page_index,
                "output generated coverage contains an unexpected operator",
            ));
        }
        require_operands(&pair[0], 4, plan.page_index)?;
        require_operands(&pair[1], 0, plan.page_index)?;
        let values = pair[0]
            .operands
            .iter()
            .map(|operand| number(document, operand, plan.page_index, "coverage rectangle"))
            .collect::<Result<Vec<_>>>()?;
        if values[2] <= 0.0 || values[3] <= 0.0 {
            return Err(unsupported(
                plan.page_index,
                "output generated coverage rectangle is degenerate",
            ));
        }
        coverage_bounds.push(BoxPts {
            left: values[0] as f32,
            right: (values[0] + values[2]) as f32,
            bottom: values[1] as f32,
            top: (values[1] + values[3]) as f32,
        });
    }
    if plan.boxes.iter().any(|requested| {
        !coverage_bounds
            .iter()
            .any(|coverage| boxes_approximately_equal(*requested, *coverage))
    }) {
        return Err(unsupported(
            plan.page_index,
            "output is missing a requested black coverage rectangle",
        ));
    }
    Ok(operations[..black_index - 1].to_vec())
}

fn boxes_approximately_equal(a: BoxPts, b: BoxPts) -> bool {
    (a.left - b.left).abs() < 0.01
        && (a.right - b.right).abs() < 0.01
        && (a.bottom - b.bottom).abs() < 0.01
        && (a.top - b.top).abs() < 0.01
}

fn analyze_operations(
    document: &Document,
    resources: Option<&Dictionary>,
    operations: Vec<Operation>,
    plan: &PagePlan,
    mode: AnalyzeMode,
    initial_graphics: GraphicsState,
    form_stack: &mut Vec<ObjectId>,
) -> Result<ContentResult> {
    let mut fonts = FontResolver::new(document, resources, plan.page_index);
    let mut graphics = initial_graphics;
    let mut stack = Vec::new();
    let mut marked_stack = Vec::new();
    let mut structure_replacement_text = None;
    let mut text_object: Option<TextObject> = None;
    let mut output = Vec::with_capacity(operations.len());
    let mut removed = Vec::new();
    let mut removed_xobjects = BTreeSet::new();
    let mut removed_xobject_ids = BTreeSet::new();
    let mut removed_shadings = BTreeSet::new();
    let mut removed_shading_ids = BTreeSet::new();
    let mut rewritten_forms = BTreeMap::new();
    let mut rewritten_form_ids = BTreeSet::new();
    let mut path = PathGeometry::default();
    let mut suppressed_output_indices = BTreeSet::new();
    let do_counts = operations
        .iter()
        .filter(|operation| operation.operator == "Do")
        .filter_map(|operation| operation.operands.first())
        .filter_map(|operand| operand.as_name().ok())
        .fold(BTreeMap::<Vec<u8>, usize>::new(), |mut counts, name| {
            *counts.entry(name.to_vec()).or_default() += 1;
            counts
        });

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
                let mut completed = text_object
                    .take()
                    .ok_or_else(|| unsupported(plan.page_index, "text object state disappeared"))?;
                if !completed.removed_glyphs.is_empty() {
                    prune_unobservable_shows(&mut completed.operations);
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
                    if removed.len() + completed.removed_glyphs.len() > MAX_REMOVED_OBJECTS_PER_PAGE
                    {
                        return Err(unsupported(
                            plan.page_index,
                            "redaction intersects too many content objects for safe rewriting",
                        ));
                    }
                    removed.extend(
                        completed
                            .removed_glyphs
                            .into_iter()
                            .map(|bounds| (bounds, RedactionObjectKind::Text)),
                    );
                }
                output.extend(completed.operations);
                continue;
            }
            let emitted = process_text_operation(
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
                plan,
            )?;
            text.operations.extend(emitted);
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
                let name = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    .ok_or_else(|| unsupported(plan.page_index, "Do operand is not a name"))?;
                let action = analyze_xobject(
                    PageAnalysisContext {
                        document,
                        resources,
                        page_index: plan.page_index,
                    },
                    &operation,
                    &graphics,
                    plan,
                    mode,
                    do_counts.get(name).copied().unwrap_or_default(),
                    form_stack,
                )?;
                match action {
                    XObjectAction::Keep => output.push(operation),
                    XObjectAction::Remove(removal) => {
                        if removed.len() >= MAX_REMOVED_OBJECTS_PER_PAGE {
                            return Err(unsupported(
                                plan.page_index,
                                "redaction intersects too many content objects for safe rewriting",
                            ));
                        }
                        removed.push((removal.bounds, removal.kind));
                        removed_xobjects.insert(removal.name);
                        removed_xobject_ids.extend(removal.object_ids);
                    }
                    XObjectAction::Rewrite(rewrite, nested) => {
                        removed.extend(nested.removed);
                        removed_xobjects.extend(nested.removed_xobjects);
                        removed_xobject_ids.extend(nested.removed_xobject_ids);
                        removed_shadings.extend(nested.removed_shadings);
                        removed_shading_ids.extend(nested.removed_shading_ids);
                        rewritten_form_ids.extend(nested.rewritten_form_ids);
                        rewritten_form_ids.insert(rewrite.original_id);
                        rewritten_forms.insert(name.to_vec(), rewrite);
                        output.push(operation);
                    }
                }
            }
            "BI" => {
                validate_inline_image(&operation, plan.page_index)?;
                let placed = transform_box(graphics.ctm, [0.0, 0.0, 1.0, 1.0], plan.page_index)?;
                let visible = visible_graphics_bounds(placed, graphics.clip, plan.page_bounds);
                if visible.is_some_and(|bounds| intersects_any(bounds, &plan.boxes)) {
                    if removed.len() >= MAX_REMOVED_OBJECTS_PER_PAGE {
                        return Err(unsupported(
                            plan.page_index,
                            "redaction intersects too many content objects for safe rewriting",
                        ));
                    }
                    removed.push((
                        visible.ok_or_else(|| {
                            unsupported(plan.page_index, "inline image has no visible bounds")
                        })?,
                        RedactionObjectKind::InlineImage,
                    ));
                } else {
                    output.push(operation);
                }
            }
            "m" | "l" | "c" | "v" | "y" | "h" | "re" => {
                let bounds = analyze_path_construction(
                    document,
                    &operation,
                    graphics.ctm,
                    &mut path,
                    plan.page_index,
                )?;
                path.stroke_scale_x = path
                    .stroke_scale_x
                    .max(graphics.ctm.a.abs() + graphics.ctm.c.abs());
                path.stroke_scale_y = path
                    .stroke_scale_y
                    .max(graphics.ctm.b.abs() + graphics.ctm.d.abs());
                if let Some(bounds) = bounds {
                    path.bounds = Some(match path.bounds {
                        Some(existing) => union_box(existing, bounds),
                        None => bounds,
                    });
                }
                path.output_indices.push(output.len());
                output.push(operation);
            }
            "W" | "W*" => {
                require_operands(&operation, 0, plan.page_index)?;
                if path.pending_clip.is_some() {
                    return Err(unsupported(
                        plan.page_index,
                        "path has more than one clipping operator before termination",
                    ));
                }
                path.pending_clip = Some(match operation.operator.as_str() {
                    "W" => ClipRule::NonZero,
                    "W*" => ClipRule::EvenOdd,
                    _ => unreachable!("matched clipping operator"),
                });
                path.output_indices.push(output.len());
                output.push(operation);
            }
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                require_operands(&operation, 0, plan.page_index)?;
                let mut bounds = path.bounds;
                if matches!(
                    operation.operator.as_str(),
                    "S" | "s" | "B" | "B*" | "b" | "b*"
                ) {
                    bounds = bounds
                        .map(|bounds| {
                            inflate_stroked_path_bounds(bounds, &graphics, &path, plan.page_index)
                        })
                        .transpose()?;
                }
                let visible = bounds.and_then(|bounds| {
                    visible_graphics_bounds(bounds, graphics.clip, plan.page_bounds)
                });
                if visible.is_some_and(|bounds| intersects_any(bounds, &plan.boxes)) {
                    if removed.len() >= MAX_REMOVED_OBJECTS_PER_PAGE {
                        return Err(unsupported(
                            plan.page_index,
                            "redaction intersects too many content objects for safe rewriting",
                        ));
                    }
                    if path.pending_clip.is_some() {
                        if matches!(operation.operator.as_str(), "s" | "b" | "b*") {
                            output.push(Operation::new("h", vec![]));
                        }
                        output.push(Operation::new("n", vec![]));
                    } else {
                        suppressed_output_indices.extend(path.output_indices.iter().copied());
                    }
                    removed.push((
                        visible.ok_or_else(|| {
                            unsupported(plan.page_index, "painted vector path has no bounds")
                        })?,
                        RedactionObjectKind::VectorPath,
                    ));
                } else {
                    output.push(operation);
                }
                if let Some(rule) = path.pending_clip {
                    graphics.clip.intersect(path.bounds, rule);
                }
                path = PathGeometry::default();
            }
            "n" => {
                require_operands(&operation, 0, plan.page_index)?;
                output.push(operation);
                if let Some(rule) = path.pending_clip {
                    graphics.clip.intersect(path.bounds, rule);
                }
                path = PathGeometry::default();
            }
            "sh" => {
                let (name, shading_id) = analyze_shading(document, resources, &operation, plan)?;
                let Some(bounds) = graphics.clip.conservative_bounds(plan.page_bounds) else {
                    output.push(operation);
                    continue;
                };
                if !intersects_any(bounds, &plan.boxes) {
                    output.push(operation);
                    continue;
                }
                if removed.len() >= MAX_REMOVED_OBJECTS_PER_PAGE {
                    return Err(unsupported(
                        plan.page_index,
                        "redaction intersects too many content objects for safe rewriting",
                    ));
                }
                removed.push((bounds, RedactionObjectKind::Shading));
                removed_shadings.insert(name);
                removed_shading_ids.insert(shading_id);
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
    if path.pending_clip.is_some() {
        return Err(unsupported(
            plan.page_index,
            "clipping path has no terminating painting or n operator",
        ));
    }
    let operations = output
        .into_iter()
        .enumerate()
        .filter_map(|(index, operation)| {
            (!suppressed_output_indices.contains(&index)).then_some(operation)
        })
        .collect();
    Ok(ContentResult {
        operations,
        removed,
        removed_xobjects,
        removed_xobject_ids,
        removed_shadings,
        removed_shading_ids,
        rewritten_forms,
        rewritten_form_ids,
    })
}

#[allow(clippy::too_many_arguments)]
fn process_text_operation(
    context: PageAnalysisContext<'_>,
    operation: &Operation,
    text: &mut TextObject,
    graphics: &mut GraphicsState,
    fonts: &mut FontResolver<'_>,
    marked_stack: &mut Vec<bool>,
    structure_replacement_text: &mut Option<bool>,
    plan: &PagePlan,
) -> Result<Vec<Operation>> {
    let PageAnalysisContext {
        document,
        resources,
        page_index,
    } = context;
    let mut emitted = vec![operation.clone()];
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
            if let Some(rewritten) = show_text(bytes, text, graphics, fonts, plan)? {
                emitted = vec![rewritten];
            }
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
            let mut rewritten_items = Vec::with_capacity(array.len());
            let mut changed = false;
            for item in array {
                match deref(document, item)? {
                    Object::String(bytes, _) => {
                        if let Some(rewritten) = show_text(bytes, text, graphics, fonts, plan)? {
                            let items = rewritten
                                .operands
                                .first()
                                .and_then(|operand| operand.as_array().ok())
                                .ok_or_else(|| {
                                    unsupported(
                                        page_index,
                                        "glyph rewrite did not produce a TJ array",
                                    )
                                })?;
                            rewritten_items.extend(items.iter().cloned());
                            changed = true;
                        } else {
                            rewritten_items.push(item.clone());
                        }
                    }
                    Object::Integer(_) | Object::Real(_) => {
                        let adjustment = number(document, item, page_index, "TJ adjustment")?;
                        apply_tj_adjustment(text, graphics, fonts, adjustment, page_index)?;
                        rewritten_items.push(item.clone());
                    }
                    _ => {
                        return Err(unsupported(
                            page_index,
                            "TJ array contains a non-string, non-number item",
                        ));
                    }
                }
            }
            if changed {
                emitted = vec![Operation::new("TJ", vec![Object::Array(rewritten_items)])];
            }
        }
        "'" => {
            require_operands(operation, 1, page_index)?;
            move_to_next_line_unchecked(text, &graphics.text);
            let bytes = operation.operands[0]
                .as_str()
                .map_err(|_| unsupported(page_index, "' operand is not a string"))?;
            if let Some(rewritten) = show_text(bytes, text, graphics, fonts, plan)? {
                emitted = vec![Operation::new("T*", vec![]), rewritten];
            }
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
            if let Some(rewritten) = show_text(bytes, text, graphics, fonts, plan)? {
                emitted = vec![
                    Operation::new("Tw", vec![operation.operands[0].clone()]),
                    Operation::new("Tc", vec![operation.operands[1].clone()]),
                    Operation::new("T*", vec![]),
                    rewritten,
                ];
            }
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
    Ok(emitted)
}

fn show_text(
    bytes: &[u8],
    text: &mut TextObject,
    graphics: &GraphicsState,
    fonts: &mut FontResolver<'_>,
    plan: &PagePlan,
) -> Result<Option<Operation>> {
    let page_index = plan.page_index;
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
    let mut boundaries = Vec::new();
    let mut removed = Vec::new();
    let mut displacement_milli = Vec::new();
    metrics.for_each_code(bytes, page_index, |code| {
        let word_spacing = if code.is_space {
            graphics.text.word_spacing
        } else {
            0.0
        };
        let spacing = graphics.text.char_spacing + word_spacing;
        let (step_milli, step_x, step_y, glyph_left, glyph_right, lower, upper) =
            if let Some(vertical) = code.vertical {
                let position_x = vertical.position_x.ok_or_else(|| {
                    unsupported(
                        page_index,
                        "vertical glyph has no horizontal origin displacement",
                    )
                })?;
                let step_milli = vertical.displacement_y + 1000.0 * spacing / graphics.text.size;
                let origin_x =
                    -position_x / 1000.0 * graphics.text.size * graphics.text.horizontal_scale;
                let glyph_left = origin_x
                    + code.glyph_box[0] / 1000.0
                        * graphics.text.size
                        * graphics.text.horizontal_scale;
                let glyph_right = origin_x
                    + code.glyph_box[2] / 1000.0
                        * graphics.text.size
                        * graphics.text.horizontal_scale;
                let cell_end = vertical.displacement_y / 1000.0 * graphics.text.size;
                (
                    step_milli,
                    0.0,
                    step_milli / 1000.0 * graphics.text.size,
                    glyph_left,
                    glyph_right,
                    cell_end.min(0.0) + graphics.text.rise,
                    cell_end.max(0.0) + graphics.text.rise,
                )
            } else {
                let step_milli = code.width + 1000.0 * spacing / graphics.text.size;
                let step_x =
                    step_milli / 1000.0 * graphics.text.size * graphics.text.horizontal_scale;
                // `glyph_box` already describes where the glyph paints relative to the
                // pen: the advance cell for fonts whose outlines we do not resolve, and
                // the /FontMatrix-transformed box for Type 3. Do not union it with the
                // advance cell — a Type 3 /FontMatrix may translate the glyph away from
                // the origin, and stretching the box back to the pen would swallow
                // neighbouring glyphs that carry no ink there.
                (
                    step_milli,
                    step_x,
                    0.0,
                    code.glyph_box[0] / 1000.0
                        * graphics.text.size
                        * graphics.text.horizontal_scale,
                    code.glyph_box[2] / 1000.0
                        * graphics.text.size
                        * graphics.text.horizontal_scale,
                    code.glyph_box[1] / 1000.0 * graphics.text.size + graphics.text.rise,
                    code.glyph_box[3] / 1000.0 * graphics.text.size + graphics.text.rise,
                )
            };
        let transform = graphics.ctm.concat(text.matrix);
        let mut bounds = transform_glyph_box(
            transform,
            [
                glyph_left.min(glyph_right),
                lower.min(upper),
                glyph_left.max(glyph_right),
                lower.max(upper),
            ],
            page_index,
        )?;
        if matches!(graphics.text.rendering_mode, 1 | 2 | 5 | 6) {
            bounds = inflate_stroked_text_bounds(bounds, graphics, page_index)?;
        }
        let visible = visible_graphics_bounds(bounds, graphics.clip, plan.page_bounds);
        let remove = visible.is_some_and(|visible| intersects_any(visible, &plan.boxes));
        if remove {
            text.removed_glyphs.push(visible.ok_or_else(|| {
                unsupported(page_index, "intersecting glyph has no visible bounds")
            })?);
            if (4..=7).contains(&graphics.text.rendering_mode) {
                text.used_text_clip = true;
            }
        }
        boundaries.push((code.start, code.end));
        removed.push(remove);
        displacement_milli.push(step_milli);
        text.matrix = text.matrix.concat(Matrix::translation(step_x, step_y));
        Ok(())
    })?;
    if removed.iter().any(|value| *value) {
        rewrite_glyph_bytes_as_tj(
            bytes,
            &boundaries,
            &removed,
            &displacement_milli,
            page_index,
        )
        .map(Some)
    } else {
        Ok(None)
    }
}

fn apply_tj_adjustment(
    text: &mut TextObject,
    graphics: &GraphicsState,
    fonts: &mut FontResolver<'_>,
    adjustment: f64,
    page_index: u32,
) -> Result<()> {
    let font_name = graphics
        .text
        .font
        .as_deref()
        .ok_or_else(|| unsupported(page_index, "TJ adjustment has no selected font"))?;
    let writing_mode = fonts.get(font_name)?.writing_mode();
    let displacement = -adjustment / 1000.0 * graphics.text.size;
    text.matrix = match writing_mode {
        WritingMode::Horizontal => text.matrix.concat(Matrix::translation(
            displacement * graphics.text.horizontal_scale,
            0.0,
        )),
        WritingMode::Vertical => text.matrix.concat(Matrix::translation(0.0, displacement)),
    };
    Ok(())
}

fn analyze_path_construction(
    document: &Document,
    operation: &Operation,
    ctm: Matrix,
    path: &mut PathGeometry,
    page_index: u32,
) -> Result<Option<BoxPts>> {
    let points = match operation.operator.as_str() {
        "m" => {
            let [x, y] = two_numbers(document, operation, page_index)?;
            path.has_current_point = true;
            path.has_subpath_start = true;
            vec![(x, y)]
        }
        "l" => {
            require_path_current_point(path, page_index, "l")?;
            let [x, y] = two_numbers(document, operation, page_index)?;
            vec![(x, y)]
        }
        "c" => {
            require_path_current_point(path, page_index, "c")?;
            require_operands(operation, 6, page_index)?;
            let values = operation
                .operands
                .iter()
                .map(|operand| number(document, operand, page_index, "c"))
                .collect::<Result<Vec<_>>>()?;
            vec![
                (values[0], values[1]),
                (values[2], values[3]),
                (values[4], values[5]),
            ]
        }
        "v" | "y" => {
            require_path_current_point(path, page_index, &operation.operator)?;
            require_operands(operation, 4, page_index)?;
            let values = operation
                .operands
                .iter()
                .map(|operand| number(document, operand, page_index, &operation.operator))
                .collect::<Result<Vec<_>>>()?;
            vec![(values[0], values[1]), (values[2], values[3])]
        }
        "re" => {
            require_operands(operation, 4, page_index)?;
            let values = operation
                .operands
                .iter()
                .map(|operand| number(document, operand, page_index, "re"))
                .collect::<Result<Vec<_>>>()?;
            let x1 = values[0] + values[2];
            let y1 = values[1] + values[3];
            if ![x1, y1].iter().all(|value| value.is_finite()) {
                return Err(unsupported(page_index, "re bounds are non-finite"));
            }
            path.has_current_point = true;
            path.has_subpath_start = true;
            vec![
                (values[0], values[1]),
                (values[0], y1),
                (x1, values[1]),
                (x1, y1),
            ]
        }
        "h" => {
            require_operands(operation, 0, page_index)?;
            if !path.has_current_point || !path.has_subpath_start {
                return Err(unsupported(
                    page_index,
                    "h appears before a path subpath start",
                ));
            }
            return Ok(None);
        }
        _ => {
            return Err(unsupported(
                page_index,
                format!(
                    "unexpected path construction operator {}",
                    operation.operator
                ),
            ));
        }
    };
    transformed_points_box(ctm, &points, page_index).map(Some)
}

fn require_path_current_point(path: &PathGeometry, page_index: u32, operator: &str) -> Result<()> {
    if !path.has_current_point {
        return Err(unsupported(
            page_index,
            format!("{operator} appears before a path current point"),
        ));
    }
    Ok(())
}

fn transformed_points_box(
    matrix: Matrix,
    points: &[(f64, f64)],
    page_index: u32,
) -> Result<BoxPts> {
    if !matrix.is_finite()
        || points
            .iter()
            .flat_map(|(x, y)| [x, y])
            .any(|value| !value.is_finite())
    {
        return Err(unsupported(
            page_index,
            "path bounds contain non-finite values",
        ));
    }
    let transformed: Vec<_> = points.iter().map(|(x, y)| matrix.point(*x, *y)).collect();
    let bounds = BoxPts {
        left: (transformed
            .iter()
            .map(|point| point.0)
            .fold(f64::INFINITY, f64::min)
            - BBOX_MARGIN_PT) as f32,
        right: (transformed
            .iter()
            .map(|point| point.0)
            .fold(f64::NEG_INFINITY, f64::max)
            + BBOX_MARGIN_PT) as f32,
        bottom: (transformed
            .iter()
            .map(|point| point.1)
            .fold(f64::INFINITY, f64::min)
            - BBOX_MARGIN_PT) as f32,
        top: (transformed
            .iter()
            .map(|point| point.1)
            .fold(f64::NEG_INFINITY, f64::max)
            + BBOX_MARGIN_PT) as f32,
    };
    if ![bounds.left, bounds.right, bounds.bottom, bounds.top]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err(unsupported(
            page_index,
            "transformed path bounds exceed supported precision",
        ));
    }
    Ok(bounds)
}

fn analyze_shading(
    document: &Document,
    resources: Option<&Dictionary>,
    operation: &Operation,
    plan: &PagePlan,
) -> Result<(Vec<u8>, ObjectId)> {
    require_operands(operation, 1, plan.page_index)?;
    let name = operation.operands[0]
        .as_name()
        .map_err(|_| unsupported(plan.page_index, "sh operand is not a name"))?;
    let shading = resource_entry(document, resources, b"Shading", name, plan.page_index)?;
    let (shading_id, resolved) = document.dereference(shading).map_err(|error| {
        unsupported(
            plan.page_index,
            format!("shading resource cannot be resolved: {error}"),
        )
    })?;
    if !matches!(resolved, Object::Dictionary(_) | Object::Stream(_)) {
        return Err(unsupported(
            plan.page_index,
            "shading resource is neither a dictionary nor a stream",
        ));
    }
    let shading_id = shading_id.ok_or_else(|| {
        unsupported(
            plan.page_index,
            "shading resource is direct and cannot be pruned safely",
        )
    })?;
    Ok((name.to_vec(), shading_id))
}

fn analyze_xobject(
    context: PageAnalysisContext<'_>,
    operation: &Operation,
    invoking_graphics: &GraphicsState,
    plan: &PagePlan,
    mode: AnalyzeMode,
    invocation_count: usize,
    form_stack: &mut Vec<ObjectId>,
) -> Result<XObjectAction> {
    let PageAnalysisContext {
        document,
        resources,
        page_index: _,
    } = context;
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
            let placed =
                transform_box(invoking_graphics.ctm, [0.0, 0.0, 1.0, 1.0], plan.page_index)?;
            let Some(bounds) =
                visible_graphics_bounds(placed, invoking_graphics.clip, plan.page_bounds)
            else {
                return Ok(XObjectAction::Keep);
            };
            if !intersects_any(bounds, &plan.boxes) {
                return Ok(XObjectAction::Keep);
            }
            let image_id = xobject_id.ok_or_else(|| {
                unsupported(
                    plan.page_index,
                    "intersecting image XObject is a direct stream and cannot be pruned safely",
                )
            })?;
            let mut object_ids = BTreeSet::from([image_id]);
            for key in [b"SMask".as_slice(), b"Mask"] {
                let Ok(mask) = stream.dict.get(key) else {
                    continue;
                };
                if let Object::Reference(mask_id) = mask {
                    deref(document, mask)?.as_stream().map_err(|_| {
                        unsupported(
                            plan.page_index,
                            format!(
                                "image /{} reference is not a stream",
                                String::from_utf8_lossy(key)
                            ),
                        )
                    })?;
                    object_ids.insert(*mask_id);
                } else if key == b"SMask"
                    && !matches!(deref(document, mask)?, Object::Name(value) if value == b"None")
                {
                    return Err(unsupported(
                        plan.page_index,
                        "image /SMask is neither an indirect stream nor /None",
                    ));
                } else if key == b"Mask" && !matches!(deref(document, mask)?, Object::Array(_)) {
                    return Err(unsupported(
                        plan.page_index,
                        "image /Mask is neither an indirect stream nor a color-key array",
                    ));
                }
            }
            Ok(XObjectAction::Remove(XObjectRemoval {
                bounds,
                name: name.to_vec(),
                object_ids,
                kind: RedactionObjectKind::ImageXObject,
            }))
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
            let form_bounds = transform_box(
                invoking_graphics.ctm.concat(matrix),
                [values[0], values[1], values[2], values[3]],
                plan.page_index,
            )?;
            if form_bounds.area() <= 0.0 {
                return Err(unsupported(
                    plan.page_index,
                    "Form XObject has a degenerate placed /BBox",
                ));
            }
            let Some(bounds) =
                visible_graphics_bounds(form_bounds, invoking_graphics.clip, plan.page_bounds)
            else {
                return Ok(XObjectAction::Keep);
            };
            if !intersects_any(bounds, &plan.boxes) {
                return Ok(XObjectAction::Keep);
            }
            let form_id = xobject_id.ok_or_else(|| {
                unsupported(
                    plan.page_index,
                    "intersecting Form XObject is a direct stream and cannot be pruned safely",
                )
            })?;
            if invocation_count != 1 {
                return Err(unsupported(
                    plan.page_index,
                    format!(
                        "intersecting Form XObject /{} is invoked {invocation_count} times in one content stream",
                        String::from_utf8_lossy(name)
                    ),
                ));
            }
            if form_stack.len() >= 64 {
                return Err(unsupported(
                    plan.page_index,
                    "Form XObject nesting exceeds 64 levels",
                ));
            }
            if form_stack.contains(&form_id) {
                return Err(unsupported(
                    plan.page_index,
                    "cyclic Form XObject invocation cannot be rewritten safely",
                ));
            }

            let local_resources = stream
                .dict
                .get(b"Resources")
                .ok()
                .map(|object| deref(document, object))
                .transpose()?
                .map(|object| {
                    object.as_dict().map_err(|_| {
                        unsupported(
                            plan.page_index,
                            "Form XObject /Resources is not a dictionary",
                        )
                    })
                })
                .transpose()?;
            let mut effective_resources =
                merge_resources(document, resources, local_resources, plan.page_index)?;
            remove_self_form_binding(
                document,
                &mut effective_resources,
                name,
                form_id,
                plan.page_index,
            )?;
            let bytes = strict_stream_content(
                stream,
                plan.page_index,
                &format!("Form XObject /{}", String::from_utf8_lossy(name)),
                MAX_DECODED_CONTENT_BYTES,
            )?;
            let content = decode_content_strict(
                &bytes,
                plan.page_index,
                &format!(
                    "Form XObject /{} content stream",
                    String::from_utf8_lossy(name)
                ),
            )?;
            if !content
                .operations
                .iter()
                .any(|operation| matches!(operation.operator.as_str(), "W" | "W*"))
            {
                return Ok(XObjectAction::Remove(XObjectRemoval {
                    bounds,
                    name: name.to_vec(),
                    object_ids: BTreeSet::from([form_id]),
                    kind: RedactionObjectKind::FormXObject,
                }));
            }
            form_stack.push(form_id);
            let mut form_graphics = invoking_graphics.clone();
            form_graphics.ctm = invoking_graphics.ctm.concat(matrix);
            // A Form XObject establishes its /BBox as an additional clip while
            // otherwise inheriting the invoking graphics state.
            form_graphics
                .clip
                .intersect(Some(form_bounds), ClipRule::NonZero);
            let nested = analyze_operations(
                document,
                Some(&effective_resources),
                content.operations,
                plan,
                mode,
                form_graphics,
                form_stack,
            );
            let popped = form_stack.pop();
            if popped != Some(form_id) {
                return Err(unsupported(
                    plan.page_index,
                    "Form XObject analysis stack became inconsistent",
                ));
            }
            let mut nested = nested?;
            if nested.removed.is_empty() && nested.rewritten_forms.is_empty() {
                return Ok(XObjectAction::Keep);
            }
            let mut rewritten_resources = effective_resources;
            prune_unused_resource_names(
                document,
                &mut rewritten_resources,
                &nested.operations,
                b"XObject",
                "Do",
                &nested.removed_xobjects,
                plan.page_index,
            )?;
            prune_unused_resource_names(
                document,
                &mut rewritten_resources,
                &nested.operations,
                b"Shading",
                "sh",
                &nested.removed_shadings,
                plan.page_index,
            )?;
            let rewrite = FormRewrite {
                original_id: form_id,
                stream: stream.clone(),
                resources: rewritten_resources,
                operations: std::mem::take(&mut nested.operations),
                nested_forms: std::mem::take(&mut nested.rewritten_forms),
            };
            Ok(XObjectAction::Rewrite(rewrite, Box::new(nested)))
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
        b"Type3" => parse_type3_font(document, dict, page_index),
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

fn parse_type3_font(
    document: &Document,
    dict: &Dictionary,
    page_index: u32,
) -> Result<FontMetrics> {
    let matrix_values = dict
        .get(b"FontMatrix")
        .map_err(|_| unsupported(page_index, "Type3 font has no /FontMatrix"))?;
    let matrix_values = deref(document, matrix_values)?
        .as_array()
        .map_err(|_| unsupported(page_index, "Type3 /FontMatrix is not an array"))?;
    let values = number_array::<6>(document, matrix_values, page_index, "Type3 /FontMatrix")?;
    let matrix = Matrix {
        a: values[0],
        b: values[1],
        c: values[2],
        d: values[3],
        e: values[4],
        f: values[5],
    };
    if !matrix.is_finite() || matrix.a <= 0.0 || matrix.b.abs() > 1e-9 {
        return Err(unsupported(
            page_index,
            "Type3 /FontMatrix has unsupported vertical or non-positive text advance",
        ));
    }

    let font_bbox = dict
        .get(b"FontBBox")
        .map_err(|_| unsupported(page_index, "Type3 font has no /FontBBox"))?;
    let font_bbox = deref(document, font_bbox)?
        .as_array()
        .map_err(|_| unsupported(page_index, "Type3 /FontBBox is not an array"))?;
    let font_bbox = number_array::<4>(document, font_bbox, page_index, "Type3 /FontBBox")?;
    let mut extents = type3_metric_box(matrix, font_bbox, page_index)?;

    let first = dict
        .get(b"FirstChar")
        .map_err(|_| unsupported(page_index, "Type3 font has no /FirstChar"))?;
    let first = deref(document, first)?
        .as_i64()
        .map_err(|_| unsupported(page_index, "Type3 /FirstChar is not an integer"))?;
    let last = dict
        .get(b"LastChar")
        .map_err(|_| unsupported(page_index, "Type3 font has no /LastChar"))?;
    let last = deref(document, last)?
        .as_i64()
        .map_err(|_| unsupported(page_index, "Type3 /LastChar is not an integer"))?;
    if !(0..=255).contains(&first) || !(first..=255).contains(&last) {
        return Err(unsupported(page_index, "Type3 character range is invalid"));
    }
    let width_objects = dict
        .get(b"Widths")
        .map_err(|_| unsupported(page_index, "Type3 font has no /Widths"))?;
    let width_objects = deref(document, width_objects)?
        .as_array()
        .map_err(|_| unsupported(page_index, "Type3 /Widths is not an array"))?;
    let expected_widths = usize::try_from(last - first + 1)
        .map_err(|_| unsupported(page_index, "Type3 /Widths length overflowed"))?;
    if width_objects.len() != expected_widths {
        return Err(unsupported(
            page_index,
            "Type3 /Widths length does not match /FirstChar../LastChar",
        ));
    }
    let mut raw_widths = Box::new([None; 256]);
    let mut widths = Box::new([None; 256]);
    for (offset, object) in width_objects.iter().enumerate() {
        let code = usize::try_from(first)
            .ok()
            .and_then(|first| first.checked_add(offset))
            .ok_or_else(|| unsupported(page_index, "Type3 character code overflowed"))?;
        let raw = number(document, object, page_index, "Type3 width")?;
        raw_widths[code] = Some(raw);
        widths[code] = Some(raw * matrix.a * 1000.0);
    }

    // The code -> glyph-name map: a named base encoding (bare or via /BaseEncoding),
    // then any /Differences layered on top. A Differences-only /Encoding dictionary
    // stays valid, which is the shape most Type 3 producers emit.
    let encoding = dict
        .get(b"Encoding")
        .map_err(|_| unsupported(page_index, "Type3 font has no /Encoding"))?;
    let encoding = deref(document, encoding)?;
    let mut glyph_names: Vec<Option<Vec<u8>>> = vec![None; 256];
    let mut apply_base_encoding = |name: &[u8]| -> Result<()> {
        if !is_known_base_encoding(name) {
            return Err(unsupported(
                page_index,
                format!(
                    "Type3 /Encoding /{} is not a supported base encoding",
                    String::from_utf8_lossy(name)
                ),
            ));
        }
        for code in 0u16..256 {
            let code = code as u8;
            if let Some(glyph) = base_encoding_glyph_name(name, code) {
                glyph_names[usize::from(code)] = Some(glyph.as_bytes().to_vec());
            }
        }
        Ok(())
    };
    let differences = match encoding {
        Object::Name(name) => {
            apply_base_encoding(name)?;
            None
        }
        Object::Dictionary(encoding) => {
            if let Ok(base) = encoding.get(b"BaseEncoding") {
                let base = deref(document, base)?
                    .as_name()
                    .map_err(|_| unsupported(page_index, "Type3 /BaseEncoding is not a name"))?
                    .to_vec();
                apply_base_encoding(&base)?;
            }
            match encoding.get(b"Differences") {
                Ok(differences) => Some(
                    deref(document, differences)?
                        .as_array()
                        .map_err(|_| unsupported(page_index, "Type3 /Differences is not an array"))?
                        .clone(),
                ),
                Err(_) => None,
            }
        }
        _ => {
            return Err(unsupported(
                page_index,
                "Type3 /Encoding is neither a name nor a dictionary",
            ));
        }
    };
    if let Some(differences) = &differences {
        let mut next_code: Option<i64> = None;
        for item in differences {
            match deref(document, item)? {
                Object::Integer(code) => {
                    if !(0..=255).contains(code) {
                        return Err(unsupported(
                            page_index,
                            "Type3 /Differences code is invalid",
                        ));
                    }
                    next_code = Some(*code);
                }
                Object::Name(name) => {
                    let code = next_code.ok_or_else(|| {
                        unsupported(page_index, "Type3 /Differences starts with a glyph name")
                    })?;
                    if code > 255 {
                        return Err(unsupported(
                            page_index,
                            "Type3 /Differences exceeds code 255",
                        ));
                    }
                    glyph_names[code as usize] = Some(name.clone());
                    next_code = Some(code + 1);
                }
                _ => {
                    return Err(unsupported(
                        page_index,
                        "Type3 /Differences contains an invalid object",
                    ));
                }
            }
        }
    }
    if differences.is_none()
        && !matches!(encoding, Object::Name(_))
        && glyph_names.iter().all(Option::is_none)
    {
        return Err(unsupported(
            page_index,
            "Type3 /Encoding has neither a base encoding nor /Differences",
        ));
    }

    let charprocs = dict
        .get(b"CharProcs")
        .map_err(|_| unsupported(page_index, "Type3 font has no /CharProcs"))?;
    let charprocs = deref(document, charprocs)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "Type3 /CharProcs is not a dictionary"))?;
    let mut valid_codes = Box::new([false; 256]);
    let mut glyph_boxes: Box<[Option<[f64; 4]>; 256]> = Box::new([None; 256]);
    let mut parsed_charprocs = BTreeMap::<Vec<u8>, (Option<[f64; 4]>, f64)>::new();
    for code in 0usize..256 {
        let Some(name) = glyph_names[code].clone() else {
            continue;
        };
        // A base encoding names every ASCII code, but a Type 3 font only draws the
        // subset it declares. Codes without a width or a CharProc simply stay invalid;
        // text that uses one still refuses typed at show time.
        let Some(raw_width) = raw_widths[code] else {
            continue;
        };
        if !charprocs.has(&name) {
            continue;
        }
        let (glyph_bbox, glyph_width) = if let Some(metrics) = parsed_charprocs.get(&name) {
            *metrics
        } else {
            let charproc = charprocs
                .get(&name)
                .map_err(|_| unsupported(page_index, "Type3 CharProc disappeared"))?;
            let charproc = deref(document, charproc)?
                .as_stream()
                .map_err(|_| unsupported(page_index, "Type3 CharProc is not a stream"))?;
            let bytes = strict_stream_content(
                charproc,
                page_index,
                "Type3 CharProc",
                MAX_DECODED_CONTENT_BYTES,
            )?;
            let _operations =
                decode_content_strict(&bytes, page_index, "Type3 CharProc content stream")?
                    .operations;
            let metrics = parse_type3_glyph_prefix(&bytes, page_index)?;
            if metrics.vertical_displacement.abs() > 1e-9 {
                return Err(unsupported(
                    page_index,
                    "Type3 CharProc declares a vertical displacement",
                ));
            }
            let parsed = (metrics.bbox, metrics.width);
            parsed_charprocs.insert(name.clone(), parsed);
            parsed
        };
        if (glyph_width - raw_width).abs() > 0.01 {
            return Err(unsupported(
                page_index,
                format!("Type3 code {code} /Widths and CharProc metrics disagree"),
            ));
        }
        // `d0` leaves the glyph shape unbounded, so the font's /FontBBox is the only
        // conservative extent available for it.
        let metric_box = glyph_bbox.unwrap_or(font_bbox);
        let transformed = type3_metric_box(matrix, metric_box, page_index)?;
        // The /FontMatrix can translate and scale the glyph away from the pen origin,
        // so the advance cell alone does not describe where a Type 3 glyph is painted.
        // Record the transformed box per code and let intersection use it.
        glyph_boxes[code] = Some([
            f64::from(transformed.left),
            f64::from(transformed.bottom),
            f64::from(transformed.right),
            f64::from(transformed.top),
        ]);
        extents = union_box(extents, transformed);
        valid_codes[code] = true;
    }

    Ok(FontMetrics {
        widths: FontWidths::Simple {
            widths,
            missing: None,
            standard: None,
            valid_codes: Some(valid_codes),
        },
        ascent: f64::from(extents.top),
        descent: f64::from(extents.bottom),
        glyph_boxes: Some(glyph_boxes),
        cid_glyph_boxes: BTreeMap::new(),
    })
}

/// Glyph names for codes 32..=126, shared by the standard Latin text encodings.
const ASCII_GLYPH_NAMES: [&str; 95] = [
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quotesingle",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "grave",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
];

/// The code -> glyph-name mapping of a named base encoding.
///
/// Only the printable ASCII range is modelled. A code outside it resolves to `None`,
/// which leaves the code invalid: text that actually uses it then refuses typed rather
/// than being redacted against a guessed glyph.
fn base_encoding_glyph_name(encoding: &[u8], code: u8) -> Option<&'static str> {
    if !(32..=126).contains(&code) {
        return None;
    }
    let name = ASCII_GLYPH_NAMES[usize::from(code) - 32];
    // StandardEncoding is the only one of the three that spells the two quote codes
    // as quoteright/quoteleft rather than quotesingle/grave.
    Some(match (encoding, code) {
        (b"StandardEncoding", 39) => "quoteright",
        (b"StandardEncoding", 96) => "quoteleft",
        _ => name,
    })
}

fn is_known_base_encoding(name: &[u8]) -> bool {
    matches!(
        name,
        b"StandardEncoding" | b"WinAnsiEncoding" | b"MacRomanEncoding"
    )
}

/// The metrics a Type 3 CharProc declares in its mandatory leading operator.
///
/// `d1` declares the advance *and* a glyph bounding box; `d0` declares only the
/// advance and explicitly leaves the shape unbounded, so the caller must fall back to
/// the font's `/FontBBox` for a conservative extent.
struct Type3GlyphMetrics {
    width: f64,
    vertical_displacement: f64,
    bbox: Option<[f64; 4]>,
}

fn parse_type3_glyph_prefix(bytes: &[u8], page_index: u32) -> Result<Type3GlyphMetrics> {
    let mut tokens: Vec<&[u8]> = Vec::with_capacity(7);
    let mut offset = 0usize;
    while tokens.len() < 7 {
        offset = skip_content_space_and_comments(bytes, offset);
        if offset >= bytes.len() {
            return Err(unsupported(
                page_index,
                "Type3 CharProc ends before its required leading d0/d1 operator",
            ));
        }
        let end = skip_regular(bytes, offset);
        if end == offset {
            return Err(unsupported(
                page_index,
                "Type3 CharProc glyph metrics contain a delimiter",
            ));
        }
        tokens.push(&bytes[offset..end]);
        offset = end;
        // `wx wy d0` is complete after three tokens; `d1` needs the four bbox numbers.
        if tokens.len() == 3 && tokens[2] == b"d0" {
            let width = parse_type3_number(tokens[0], page_index)?;
            let vertical_displacement = parse_type3_number(tokens[1], page_index)?;
            return Ok(Type3GlyphMetrics {
                width,
                vertical_displacement,
                bbox: None,
            });
        }
    }
    if tokens[6] != b"d1" {
        return Err(unsupported(
            page_index,
            "Type3 CharProc does not begin with `wx wy d0` or six numbers and d1",
        ));
    }
    let mut values = [0.0; 6];
    for (index, token) in tokens[..6].iter().enumerate() {
        values[index] = parse_type3_number(token, page_index)?;
    }
    Ok(Type3GlyphMetrics {
        width: values[0],
        vertical_displacement: values[1],
        bbox: Some([values[2], values[3], values[4], values[5]]),
    })
}

fn parse_type3_number(token: &[u8], page_index: u32) -> Result<f64> {
    let text = std::str::from_utf8(token)
        .map_err(|_| unsupported(page_index, "Type3 CharProc metric is not ASCII"))?;
    let value = text
        .parse::<f64>()
        .map_err(|_| unsupported(page_index, "Type3 CharProc has an invalid metric number"))?;
    if !value.is_finite() {
        return Err(unsupported(
            page_index,
            "Type3 CharProc has a non-finite metric number",
        ));
    }
    Ok(value)
}

fn type3_metric_box(matrix: Matrix, values: [f64; 4], page_index: u32) -> Result<BoxPts> {
    if !values.iter().all(|value| value.is_finite()) {
        return Err(unsupported(page_index, "Type3 metric box is non-finite"));
    }
    let points = [
        matrix.point(values[0], values[1]),
        matrix.point(values[0], values[3]),
        matrix.point(values[2], values[1]),
        matrix.point(values[2], values[3]),
    ];
    let bounds = BoxPts {
        left: (points
            .iter()
            .map(|point| point.0)
            .fold(f64::INFINITY, f64::min)
            * 1000.0) as f32,
        right: (points
            .iter()
            .map(|point| point.0)
            .fold(f64::NEG_INFINITY, f64::max)
            * 1000.0) as f32,
        bottom: (points
            .iter()
            .map(|point| point.1)
            .fold(f64::INFINITY, f64::min)
            * 1000.0) as f32,
        top: (points
            .iter()
            .map(|point| point.1)
            .fold(f64::NEG_INFINITY, f64::max)
            * 1000.0) as f32,
    };
    if !bounds.is_finite_and_nonempty() {
        return Err(unsupported(page_index, "Type3 metric box is degenerate"));
    }
    Ok(bounds)
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
    let (ascent, descent, _x_min, _x_max) = font_extents(document, descriptor, page_index)?;
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
            valid_codes: None,
        },
        ascent,
        descent,
        glyph_boxes: None,
        cid_glyph_boxes: BTreeMap::new(),
    })
}

/// The `/Ordering` of a Type0 font's descendant `/CIDSystemInfo`, e.g. `Japan1`.
fn cid_system_ordering(
    document: &Document,
    dict: &Dictionary,
    page_index: u32,
) -> Result<Option<Vec<u8>>> {
    let Ok(descendants) = dict.get(b"DescendantFonts") else {
        return Ok(None);
    };
    let descendants = deref(document, descendants)?
        .as_array()
        .map_err(|_| unsupported(page_index, "Type0 /DescendantFonts is not an array"))?;
    let Some(descendant) = descendants.first() else {
        return Ok(None);
    };
    let descendant = deref(document, descendant)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "Type0 descendant font is not a dictionary"))?;
    let Ok(system_info) = descendant.get(b"CIDSystemInfo") else {
        return Ok(None);
    };
    let system_info = deref(document, system_info)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "/CIDSystemInfo is not a dictionary"))?;
    let Ok(ordering) = system_info.get(b"Ordering") else {
        return Ok(None);
    };
    match deref(document, ordering)? {
        Object::String(bytes, _) => Ok(Some(bytes.clone())),
        _ => Err(unsupported(
            page_index,
            "/CIDSystemInfo /Ordering is not a string",
        )),
    }
}

fn parse_cid_font(document: &Document, dict: &Dictionary, page_index: u32) -> Result<FontMetrics> {
    let encoding = dict
        .get(b"Encoding")
        .map_err(|_| unsupported(page_index, "Type0 font has no /Encoding"))?;
    let mut encoding = match deref(document, encoding)? {
        Object::Name(name) if name == b"Identity-H" => CidCodeMap::IdentityH,
        Object::Name(name) if name == b"Identity-V" => CidCodeMap::IdentityV,
        Object::Name(name) => {
            // A predefined CMap only produces the right CIDs for the character
            // collection it was written against, so refuse rather than map a font's
            // codes through a foreign collection.
            let name = name.clone();
            if let Some((ordering, _)) = predefined_cmap(&name) {
                let declared = cid_system_ordering(document, dict, page_index)?;
                if declared.as_deref() != Some(ordering) {
                    return Err(unsupported(
                        page_index,
                        format!(
                            "predefined Type0 CMap /{} expects the {} character collection",
                            String::from_utf8_lossy(&name),
                            String::from_utf8_lossy(ordering)
                        ),
                    ));
                }
            }
            CidCodeMap::Embedded(load_predefined_cmap(&name, page_index, 0)?)
        }
        Object::Stream(stream) => CidCodeMap::Embedded(parse_encoding_cmap(stream, page_index)?),
        _ => {
            return Err(unsupported(
                page_index,
                "Type0 /Encoding is neither an Identity CMap nor an embedded CMap stream",
            ));
        }
    };
    if let CidCodeMap::Embedded(cmap) = &mut encoding {
        match dict.get(b"ToUnicode") {
            Ok(to_unicode) => {
                let to_unicode = deref(document, to_unicode)?.as_stream().map_err(|_| {
                    unsupported(page_index, "Type0 /ToUnicode is not a CMap stream")
                })?;
                cmap.unicode_map = parse_to_unicode_cmap(to_unicode, &cmap.codespaces, page_index)?;
            }
            // A predefined Unicode-keyed CMap already knows the text behind every code
            // (the code *is* the UTF-16 unit), so it needs no /ToUnicode stream. An
            // embedded CMap carries no such guarantee and still requires one.
            Err(_) if !cmap.unicode_map.is_empty() => {}
            Err(_) => {
                return Err(unsupported(
                    page_index,
                    "embedded Type0 CMap has no /ToUnicode",
                ));
            }
        }
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
    let (ascent, descent, _x_min, _x_max) = font_extents(document, descriptor, page_index)?;
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
    let default_vertical = if let Ok(object) = descendant.get(b"DW2") {
        let array = deref(document, object)?
            .as_array()
            .map_err(|_| unsupported(page_index, "CID /DW2 is not an array"))?;
        // /DW2 is [v_y, w1y]; the position component is validated but not carried.
        let [_position_y, displacement_y] =
            number_array::<2>(document, array, page_index, "CID /DW2")?;
        VerticalMetric {
            displacement_y,
            position_x: None,
        }
    } else {
        VerticalMetric {
            displacement_y: -1000.0,
            position_x: None,
        }
    };
    let mut vertical_widths = BTreeMap::new();
    if let Ok(object) = descendant.get(b"W2") {
        let array = deref(document, object)?
            .as_array()
            .map_err(|_| unsupported(page_index, "CID /W2 is not an array"))?;
        parse_cid_vertical_widths(document, array, &mut vertical_widths, page_index)?;
    }
    let cid_glyph_boxes =
        if descendant_subtype == b"CIDFontType2" && matches!(encoding, CidCodeMap::IdentityH) {
            cid_true_type_boxes(document, descendant).unwrap_or_default()
        } else {
            BTreeMap::new()
        };
    Ok(FontMetrics {
        widths: FontWidths::Cid {
            widths,
            default_width,
            vertical_widths,
            default_vertical,
            encoding,
        },
        ascent,
        descent,
        glyph_boxes: None,
        cid_glyph_boxes,
    })
}

// Tighten only a verified embedded TrueType outline with explicit identity CID
// mapping. PDF widths still drive advances; unsupported programs keep the
// conservative metrics used by the existing fail-closed redactor.
fn cid_true_type_boxes(
    document: &Document,
    descendant: &Dictionary,
) -> Option<BTreeMap<u16, [f64; 4]>> {
    if deref(document, descendant.get(b"CIDToGIDMap").ok()?)
        .ok()?
        .as_name()
        .ok()?
        != b"Identity"
    {
        return None;
    }
    let descriptor = deref(document, descendant.get(b"FontDescriptor").ok()?)
        .ok()?
        .as_dict()
        .ok()?;
    let stream = deref(document, descriptor.get(b"FontFile2").ok()?)
        .ok()?
        .as_stream()
        .ok()?;
    let bytes = if stream.dict.get(b"Filter").is_ok() {
        stream.decompressed_content().ok()?
    } else {
        stream.content.clone()
    };
    let face = ttf_parser::Face::parse(&bytes, 0).ok()?;
    if face.is_variable() || face.tables().glyf.is_none() {
        return None;
    }
    let scale = 1000.0 / f64::from(face.units_per_em());
    let mut boxes = BTreeMap::new();
    for gid in 0..face.number_of_glyphs() {
        if let Some(rect) = face.glyph_bounding_box(ttf_parser::GlyphId(gid)) {
            boxes.insert(
                gid,
                [
                    f64::from(rect.x_min) * scale,
                    f64::from(rect.y_min) * scale,
                    f64::from(rect.x_max) * scale,
                    f64::from(rect.y_max) * scale,
                ],
            );
        }
    }
    Some(boxes)
}

#[derive(Debug, Clone)]
enum CMapToken {
    Hex(Vec<u8>),
    Integer(i64),
    Word(Vec<u8>),
    ArrayStart,
    ArrayEnd,
}

/// The predefined Adobe CMaps we carry, with the character collection each belongs to.
///
/// Only the Unicode-keyed families are vendored. Their character *codes* are UCS-2 or
/// UTF-16 code units, so the code-to-Unicode mapping a redaction needs is the identity
/// and no separate ToUnicode table has to be shipped or trusted. The legacy
/// byte-encoded CMaps (RKSJ, EUC, Big5, UHC ...) carry no such property and remain a
/// typed refusal rather than a guess.
const PREDEFINED_CMAPS: &[(&[u8], &[u8], &[u8])] = &[
    (
        b"UniJIS-UCS2-H",
        b"Japan1",
        include_bytes!("../assets/cmaps/UniJIS-UCS2-H"),
    ),
    (
        b"UniJIS-UCS2-V",
        b"Japan1",
        include_bytes!("../assets/cmaps/UniJIS-UCS2-V"),
    ),
    (
        b"UniJIS-UTF16-H",
        b"Japan1",
        include_bytes!("../assets/cmaps/UniJIS-UTF16-H"),
    ),
    (
        b"UniJIS-UTF16-V",
        b"Japan1",
        include_bytes!("../assets/cmaps/UniJIS-UTF16-V"),
    ),
    (
        b"UniGB-UCS2-H",
        b"GB1",
        include_bytes!("../assets/cmaps/UniGB-UCS2-H"),
    ),
    (
        b"UniGB-UCS2-V",
        b"GB1",
        include_bytes!("../assets/cmaps/UniGB-UCS2-V"),
    ),
    (
        b"UniGB-UTF16-H",
        b"GB1",
        include_bytes!("../assets/cmaps/UniGB-UTF16-H"),
    ),
    (
        b"UniGB-UTF16-V",
        b"GB1",
        include_bytes!("../assets/cmaps/UniGB-UTF16-V"),
    ),
    (
        b"UniCNS-UCS2-H",
        b"CNS1",
        include_bytes!("../assets/cmaps/UniCNS-UCS2-H"),
    ),
    (
        b"UniCNS-UCS2-V",
        b"CNS1",
        include_bytes!("../assets/cmaps/UniCNS-UCS2-V"),
    ),
    (
        b"UniCNS-UTF16-H",
        b"CNS1",
        include_bytes!("../assets/cmaps/UniCNS-UTF16-H"),
    ),
    (
        b"UniCNS-UTF16-V",
        b"CNS1",
        include_bytes!("../assets/cmaps/UniCNS-UTF16-V"),
    ),
    (
        b"UniKS-UCS2-H",
        b"Korea1",
        include_bytes!("../assets/cmaps/UniKS-UCS2-H"),
    ),
    (
        b"UniKS-UCS2-V",
        b"Korea1",
        include_bytes!("../assets/cmaps/UniKS-UCS2-V"),
    ),
    (
        b"UniKS-UTF16-H",
        b"Korea1",
        include_bytes!("../assets/cmaps/UniKS-UTF16-H"),
    ),
    (
        b"UniKS-UTF16-V",
        b"Korea1",
        include_bytes!("../assets/cmaps/UniKS-UTF16-V"),
    ),
];

/// `usecmap` chains are one level deep in the shipped resources; the bound keeps a
/// hand-built chain from recursing without limit.
const MAX_USECMAP_DEPTH: usize = 4;

fn predefined_cmap(name: &[u8]) -> Option<(&'static [u8], &'static [u8])> {
    PREDEFINED_CMAPS
        .iter()
        .find(|(cmap, _, _)| *cmap == name)
        .map(|(_, ordering, bytes)| (*ordering, *bytes))
}

/// Builds a CMap from a vendored predefined resource, following its `usecmap` base.
fn load_predefined_cmap(name: &[u8], page_index: u32, depth: usize) -> Result<EmbeddedCMap> {
    if depth > MAX_USECMAP_DEPTH {
        return Err(unsupported(
            page_index,
            "predefined CMap usecmap chain is too deep",
        ));
    }
    let Some((_, bytes)) = predefined_cmap(name) else {
        return Err(unsupported(
            page_index,
            format!(
                "predefined Type0 CMap /{} is not supported",
                String::from_utf8_lossy(name)
            ),
        ));
    };
    let tokens = tokenize_cmap(bytes, page_index)?;

    // A vertical CMap inherits its codespaces and CID mappings from the horizontal one
    // it names, then overrides the entries whose glyphs differ when set vertically.
    let mut base = match find_usecmap_reference(&tokens) {
        Some(base_name) => Some(load_predefined_cmap(&base_name, page_index, depth + 1)?),
        None => None,
    };

    let program_wmode = parse_cmap_wmode(&tokens, page_index)?.unwrap_or(0);
    let writing_mode = match program_wmode {
        0 => WritingMode::Horizontal,
        1 => WritingMode::Vertical,
        _ => {
            return Err(unsupported(
                page_index,
                "predefined CMap /WMode is not 0 or 1",
            ));
        }
    };

    let mut codespaces = parse_cmap_codespaces(&tokens, page_index).unwrap_or_default();
    if codespaces.is_empty() {
        codespaces = base
            .as_ref()
            .map(|base| base.codespaces.clone())
            .unwrap_or_default();
    }
    if codespaces.is_empty() {
        return Err(unsupported(
            page_index,
            "predefined CMap declares no codespace ranges",
        ));
    }

    let mut cid_map = base
        .take()
        .map(|base| base.cid_map)
        .unwrap_or_else(BTreeMap::new);
    // Parse this CMap's own blocks separately, so duplicate detection still applies
    // within one program while a vertical CMap may legitimately *override* the
    // horizontal entries it inherited through `usecmap`.
    let mut overlay = BTreeMap::new();
    parse_cmap_cid_mappings(&tokens, &codespaces, &mut overlay, page_index)?;
    cid_map.extend(overlay);
    if cid_map.is_empty() {
        return Err(unsupported(
            page_index,
            "predefined CMap has no CID mappings",
        ));
    }

    // Codes in these CMaps *are* UTF-16 code units, so the code-to-Unicode mapping is
    // exact without a ToUnicode stream.
    let mut unicode_map = BTreeMap::new();
    for key in cid_map.keys() {
        let units = match key.len {
            4 => vec![(key.value >> 16) as u16, (key.value & 0xFFFF) as u16],
            _ => vec![key.value as u16],
        };
        unicode_map.insert(*key, units);
    }

    Ok(EmbeddedCMap {
        codespaces,
        cid_map,
        unicode_map,
        writing_mode,
    })
}

/// Finds the `/Name usecmap` reference a CMap program declares, if any.
fn find_usecmap_reference(tokens: &[CMapToken]) -> Option<Vec<u8>> {
    tokens.iter().enumerate().find_map(|(index, token)| {
        if token_word(token) != Some(b"usecmap") || index == 0 {
            return None;
        }
        let previous = token_word(&tokens[index - 1])?;
        previous
            .strip_prefix(b"/")
            .map(<[u8]>::to_vec)
            .or_else(|| Some(previous.to_vec()))
    })
}

/// Reads every `begincidchar` / `begincidrange` block into `cid_map`, overriding any
/// entry inherited from a `usecmap` base.
fn parse_cmap_cid_mappings(
    tokens: &[CMapToken],
    codespaces: &[CodeSpace],
    cid_map: &mut BTreeMap<CodeKey, u16>,
    page_index: u32,
) -> Result<()> {
    let mut index = 0usize;
    while index < tokens.len() {
        if token_word(&tokens[index]) == Some(b"begincidchar") {
            let count = cmap_block_count(tokens, index, page_index, "cidchar")?;
            index += 1;
            for _ in 0..count {
                let code = cmap_code_token(tokens.get(index), page_index, "cidchar code")?;
                let cid = cmap_integer(tokens.get(index + 1), page_index, "cidchar CID")?;
                let cid = u16::try_from(cid)
                    .map_err(|_| unsupported(page_index, "cidchar CID exceeds 65535"))?;
                insert_cid_mapping(cid_map, code, cid, codespaces, page_index)?;
                index += 2;
            }
            continue;
        }
        if token_word(&tokens[index]) == Some(b"begincidrange") {
            let count = cmap_block_count(tokens, index, page_index, "cidrange")?;
            index += 1;
            for _ in 0..count {
                let start = cmap_code_token(tokens.get(index), page_index, "cidrange start")?;
                let end = cmap_code_token(tokens.get(index + 1), page_index, "cidrange end")?;
                let first_cid = cmap_integer(tokens.get(index + 2), page_index, "cidrange CID")?;
                let range = cmap_code_range(start, end, page_index, "cidrange")?;
                for (offset, code) in range.into_iter().enumerate() {
                    let cid =
                        first_cid
                            .checked_add(i64::try_from(offset).map_err(|_| {
                                unsupported(page_index, "cidrange offset overflowed")
                            })?)
                            .ok_or_else(|| unsupported(page_index, "cidrange CID overflowed"))?;
                    let cid = u16::try_from(cid)
                        .map_err(|_| unsupported(page_index, "cidrange CID exceeds 65535"))?;
                    insert_cid_mapping(cid_map, code, cid, codespaces, page_index)?;
                }
                index += 3;
            }
            continue;
        }
        index += 1;
    }
    Ok(())
}

fn parse_encoding_cmap(stream: &Stream, page_index: u32) -> Result<EmbeddedCMap> {
    let bytes = strict_stream_content(
        stream,
        page_index,
        "Type0 Encoding CMap",
        MAX_DECODED_CONTENT_BYTES,
    )?;
    let tokens = tokenize_cmap(&bytes, page_index)?;
    reject_usecmap(&tokens, page_index)?;
    let program_wmode = parse_cmap_wmode(&tokens, page_index)?;
    let dictionary_wmode = match stream.dict.get(b"WMode") {
        Ok(Object::Integer(value)) => Some(*value),
        Ok(_) => {
            return Err(unsupported(
                page_index,
                "Encoding CMap dictionary /WMode is not an integer",
            ));
        }
        Err(_) => None,
    };
    let wmode = match (dictionary_wmode, program_wmode) {
        (Some(left), Some(right)) if left != right => {
            return Err(unsupported(
                page_index,
                "Encoding CMap dictionary and program /WMode values contradict",
            ));
        }
        (Some(value), _) | (_, Some(value)) => value,
        (None, None) => 0,
    };
    let writing_mode = match wmode {
        0 => WritingMode::Horizontal,
        1 => WritingMode::Vertical,
        _ => {
            return Err(unsupported(
                page_index,
                "Encoding CMap /WMode is not 0 or 1",
            ));
        }
    };
    let codespaces = parse_cmap_codespaces(&tokens, page_index)?;
    let mut cid_map = BTreeMap::new();
    let mut index = 0usize;
    while index < tokens.len() {
        if token_word(&tokens[index]) == Some(b"begincidchar") {
            let count = cmap_block_count(&tokens, index, page_index, "cidchar")?;
            index += 1;
            for _ in 0..count {
                let code = cmap_code_token(tokens.get(index), page_index, "cidchar code")?;
                let cid = cmap_integer(tokens.get(index + 1), page_index, "cidchar CID")?;
                let cid = u16::try_from(cid)
                    .map_err(|_| unsupported(page_index, "cidchar CID exceeds 65535"))?;
                insert_cid_mapping(&mut cid_map, code, cid, &codespaces, page_index)?;
                index += 2;
            }
            continue;
        }
        if token_word(&tokens[index]) == Some(b"begincidrange") {
            let count = cmap_block_count(&tokens, index, page_index, "cidrange")?;
            index += 1;
            for _ in 0..count {
                let start = cmap_code_token(tokens.get(index), page_index, "cidrange start")?;
                let end = cmap_code_token(tokens.get(index + 1), page_index, "cidrange end")?;
                let first_cid = cmap_integer(tokens.get(index + 2), page_index, "cidrange CID")?;
                let range = cmap_code_range(start, end, page_index, "cidrange")?;
                for (offset, code) in range.into_iter().enumerate() {
                    let cid =
                        first_cid
                            .checked_add(i64::try_from(offset).map_err(|_| {
                                unsupported(page_index, "cidrange offset overflowed")
                            })?)
                            .ok_or_else(|| unsupported(page_index, "cidrange CID overflowed"))?;
                    let cid = u16::try_from(cid)
                        .map_err(|_| unsupported(page_index, "cidrange CID exceeds 65535"))?;
                    insert_cid_mapping(&mut cid_map, code, cid, &codespaces, page_index)?;
                }
                index += 3;
            }
            continue;
        }
        index += 1;
    }
    if cid_map.is_empty() {
        return Err(unsupported(
            page_index,
            "embedded Encoding CMap has no CID mappings",
        ));
    }
    Ok(EmbeddedCMap {
        codespaces,
        cid_map,
        unicode_map: BTreeMap::new(),
        writing_mode,
    })
}

fn parse_cmap_wmode(tokens: &[CMapToken], page_index: u32) -> Result<Option<i64>> {
    let mut value = None;
    for window in tokens.windows(3) {
        if token_word(&window[0]) != Some(b"WMode") || token_word(&window[2]) != Some(b"def") {
            continue;
        }
        let current = cmap_integer(Some(&window[1]), page_index, "CMap WMode")?;
        if value
            .replace(current)
            .is_some_and(|previous| previous != current)
        {
            return Err(unsupported(
                page_index,
                "Encoding CMap defines contradictory WMode values",
            ));
        }
    }
    Ok(value)
}

fn parse_to_unicode_cmap(
    stream: &Stream,
    encoding_spaces: &[CodeSpace],
    page_index: u32,
) -> Result<BTreeMap<CodeKey, Vec<u16>>> {
    let bytes = strict_stream_content(
        stream,
        page_index,
        "Type0 ToUnicode CMap",
        MAX_DECODED_CONTENT_BYTES,
    )?;
    let tokens = tokenize_cmap(&bytes, page_index)?;
    reject_usecmap(&tokens, page_index)?;
    let unicode_spaces = parse_cmap_codespaces(&tokens, page_index)?;
    if !same_codespaces(encoding_spaces, &unicode_spaces) {
        return Err(unsupported(
            page_index,
            "Encoding and ToUnicode CMap codespaces differ",
        ));
    }
    let mut mappings = BTreeMap::new();
    let mut index = 0usize;
    while index < tokens.len() {
        if token_word(&tokens[index]) == Some(b"beginbfchar") {
            let count = cmap_block_count(&tokens, index, page_index, "bfchar")?;
            index += 1;
            for _ in 0..count {
                let code = cmap_code_token(tokens.get(index), page_index, "bfchar code")?;
                let unicode = cmap_unicode_token(tokens.get(index + 1), page_index)?;
                insert_unicode_mapping(&mut mappings, code, unicode, encoding_spaces, page_index)?;
                index += 2;
            }
            continue;
        }
        if token_word(&tokens[index]) == Some(b"beginbfrange") {
            let count = cmap_block_count(&tokens, index, page_index, "bfrange")?;
            index += 1;
            for _ in 0..count {
                let start = cmap_code_token(tokens.get(index), page_index, "bfrange start")?;
                let end = cmap_code_token(tokens.get(index + 1), page_index, "bfrange end")?;
                let codes = cmap_code_range(start, end, page_index, "bfrange")?;
                index += 2;
                match tokens.get(index) {
                    Some(CMapToken::Hex(first)) => {
                        for (offset, code) in codes.into_iter().enumerate() {
                            let destination = increment_big_endian(first, offset, page_index)?;
                            let unicode = decode_utf16be(&destination, page_index)?;
                            insert_unicode_mapping(
                                &mut mappings,
                                code,
                                unicode,
                                encoding_spaces,
                                page_index,
                            )?;
                        }
                        index += 1;
                    }
                    Some(CMapToken::ArrayStart) => {
                        index += 1;
                        for code in codes {
                            let unicode = cmap_unicode_token(tokens.get(index), page_index)?;
                            insert_unicode_mapping(
                                &mut mappings,
                                code,
                                unicode,
                                encoding_spaces,
                                page_index,
                            )?;
                            index += 1;
                        }
                        if !matches!(tokens.get(index), Some(CMapToken::ArrayEnd)) {
                            return Err(unsupported(
                                page_index,
                                "array-form bfrange has the wrong number of destinations",
                            ));
                        }
                        index += 1;
                    }
                    _ => {
                        return Err(unsupported(
                            page_index,
                            "bfrange destination is neither a hex string nor an array",
                        ));
                    }
                }
            }
            continue;
        }
        index += 1;
    }
    if mappings.is_empty() {
        return Err(unsupported(page_index, "ToUnicode CMap has no mappings"));
    }
    Ok(mappings)
}

fn tokenize_cmap(bytes: &[u8], page_index: u32) -> Result<Vec<CMapToken>> {
    let mut tokens = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        offset = skip_content_space_and_comments(bytes, offset);
        if offset >= bytes.len() {
            break;
        }
        match bytes[offset] {
            b'[' => {
                tokens.push(CMapToken::ArrayStart);
                offset += 1;
            }
            b']' => {
                tokens.push(CMapToken::ArrayEnd);
                offset += 1;
            }
            b'<' if bytes.get(offset + 1) != Some(&b'<') => {
                let end = bytes[offset + 1..]
                    .iter()
                    .position(|byte| *byte == b'>')
                    .map(|relative| offset + 1 + relative)
                    .ok_or_else(|| unsupported(page_index, "CMap hex string is unterminated"))?;
                let hex: Vec<u8> = bytes[offset + 1..end]
                    .iter()
                    .copied()
                    .filter(|byte| !byte.is_ascii_whitespace())
                    .collect();
                if hex.is_empty() || !hex.len().is_multiple_of(2) {
                    return Err(unsupported(
                        page_index,
                        "CMap hex string has invalid length",
                    ));
                }
                let mut decoded = Vec::with_capacity(hex.len() / 2);
                for pair in hex.chunks_exact(2) {
                    decoded.push(
                        (hex_nibble(pair[0], page_index)? << 4) | hex_nibble(pair[1], page_index)?,
                    );
                }
                tokens.push(CMapToken::Hex(decoded));
                offset = end + 1;
            }
            _ => {
                let end = skip_regular(bytes, offset);
                if end == offset {
                    offset += 1;
                    continue;
                }
                let word = &bytes[offset..end];
                if let Ok(word) = std::str::from_utf8(word) {
                    if let Ok(value) = word.parse::<i64>() {
                        tokens.push(CMapToken::Integer(value));
                    } else {
                        tokens.push(CMapToken::Word(word.as_bytes().to_vec()));
                    }
                } else {
                    return Err(unsupported(page_index, "CMap token is not ASCII"));
                }
                offset = end;
            }
        }
        if tokens.len() > MAX_CONTENT_TOKENS {
            return Err(unsupported(page_index, "CMap exceeds safe token limits"));
        }
    }
    Ok(tokens)
}

fn hex_nibble(byte: u8, page_index: u32) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(unsupported(
            page_index,
            "CMap hex string contains a non-hex digit",
        )),
    }
}

fn token_word(token: &CMapToken) -> Option<&[u8]> {
    match token {
        CMapToken::Word(word) => Some(word),
        _ => None,
    }
}

fn reject_usecmap(tokens: &[CMapToken], page_index: u32) -> Result<()> {
    if tokens
        .iter()
        .any(|token| token_word(token) == Some(b"usecmap"))
    {
        return Err(unsupported(
            page_index,
            "CMap usecmap inheritance is outside the safe embedded envelope",
        ));
    }
    Ok(())
}

fn cmap_block_count(
    tokens: &[CMapToken],
    begin: usize,
    page_index: u32,
    label: &str,
) -> Result<usize> {
    let count = begin
        .checked_sub(1)
        .and_then(|index| tokens.get(index))
        .and_then(|token| match token {
            CMapToken::Integer(value) => Some(*value),
            _ => None,
        })
        .ok_or_else(|| unsupported(page_index, format!("{label} has no entry count")))?;
    let count = usize::try_from(count)
        .map_err(|_| unsupported(page_index, format!("{label} count is negative")))?;
    if count > 100_000 {
        return Err(unsupported(
            page_index,
            format!("{label} exceeds safe mapping limits"),
        ));
    }
    Ok(count)
}

fn parse_cmap_codespaces(tokens: &[CMapToken], page_index: u32) -> Result<Vec<CodeSpace>> {
    let mut spaces = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        if token_word(&tokens[index]) != Some(b"begincodespacerange") {
            index += 1;
            continue;
        }
        let count = cmap_block_count(tokens, index, page_index, "codespacerange")?;
        index += 1;
        for _ in 0..count {
            let start = cmap_code_token(tokens.get(index), page_index, "codespace start")?;
            let end = cmap_code_token(tokens.get(index + 1), page_index, "codespace end")?;
            if start.len != end.len || start.value > end.value {
                return Err(unsupported(page_index, "CMap codespace range is invalid"));
            }
            spaces.push(CodeSpace {
                len: start.len,
                start: start.value,
                end: end.value,
            });
            index += 2;
        }
    }
    if spaces.is_empty() || spaces.len() > 256 {
        return Err(unsupported(
            page_index,
            "CMap must contain 1..=256 codespace ranges",
        ));
    }
    Ok(spaces)
}

fn cmap_code_token(token: Option<&CMapToken>, page_index: u32, label: &str) -> Result<CodeKey> {
    let bytes = match token {
        Some(CMapToken::Hex(bytes)) if (1..=4).contains(&bytes.len()) => bytes,
        _ => {
            return Err(unsupported(
                page_index,
                format!("{label} is not a 1..=4-byte hex code"),
            ));
        }
    };
    Ok(CodeKey {
        len: bytes.len() as u8,
        value: code_value(bytes),
    })
}

fn cmap_integer(token: Option<&CMapToken>, page_index: u32, label: &str) -> Result<i64> {
    match token {
        Some(CMapToken::Integer(value)) => Ok(*value),
        _ => Err(unsupported(
            page_index,
            format!("{label} is not an integer"),
        )),
    }
}

fn cmap_code_range(
    start: CodeKey,
    end: CodeKey,
    page_index: u32,
    label: &str,
) -> Result<Vec<CodeKey>> {
    if start.len != end.len || start.value > end.value {
        return Err(unsupported(
            page_index,
            format!("{label} source range is invalid"),
        ));
    }
    let count = u64::from(end.value) - u64::from(start.value) + 1;
    if count > 100_000 {
        return Err(unsupported(
            page_index,
            format!("{label} range exceeds safe limits"),
        ));
    }
    Ok((start.value..=end.value)
        .map(|value| CodeKey {
            len: start.len,
            value,
        })
        .collect())
}

fn code_in_spaces(code: CodeKey, spaces: &[CodeSpace]) -> bool {
    spaces
        .iter()
        .any(|space| space.len == code.len && code.value >= space.start && code.value <= space.end)
}

fn insert_cid_mapping(
    mappings: &mut BTreeMap<CodeKey, u16>,
    code: CodeKey,
    cid: u16,
    spaces: &[CodeSpace],
    page_index: u32,
) -> Result<()> {
    if !code_in_spaces(code, spaces) {
        return Err(unsupported(
            page_index,
            "CID mapping is outside all codespaces",
        ));
    }
    if mappings.insert(code, cid).is_some() {
        return Err(unsupported(
            page_index,
            "CMap contains a duplicate CID mapping",
        ));
    }
    Ok(())
}

fn cmap_unicode_token(token: Option<&CMapToken>, page_index: u32) -> Result<Vec<u16>> {
    match token {
        Some(CMapToken::Hex(bytes)) => decode_utf16be(bytes, page_index),
        _ => Err(unsupported(
            page_index,
            "ToUnicode destination is not a hex string",
        )),
    }
}

fn decode_utf16be(bytes: &[u8], page_index: u32) -> Result<Vec<u16>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(2) {
        return Err(unsupported(
            page_index,
            "ToUnicode destination is not valid UTF-16BE",
        ));
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    if std::char::decode_utf16(units.iter().copied()).any(|value| value.is_err()) {
        return Err(unsupported(
            page_index,
            "ToUnicode destination has invalid surrogates",
        ));
    }
    Ok(units)
}

fn insert_unicode_mapping(
    mappings: &mut BTreeMap<CodeKey, Vec<u16>>,
    code: CodeKey,
    unicode: Vec<u16>,
    spaces: &[CodeSpace],
    page_index: u32,
) -> Result<()> {
    if !code_in_spaces(code, spaces) {
        return Err(unsupported(
            page_index,
            "ToUnicode mapping is outside all codespaces",
        ));
    }
    if mappings.insert(code, unicode).is_some() {
        return Err(unsupported(
            page_index,
            "ToUnicode contains a duplicate mapping",
        ));
    }
    Ok(())
}

fn increment_big_endian(bytes: &[u8], offset: usize, page_index: u32) -> Result<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > 8 {
        return Err(unsupported(
            page_index,
            "bfrange destination width is unsupported",
        ));
    }
    let mut output = bytes.to_vec();
    let mut carry = offset as u64;
    for byte in output.iter_mut().rev() {
        let value = u64::from(*byte) + (carry & 0xFF);
        *byte = value as u8;
        carry = (carry >> 8) + (value >> 8);
    }
    if carry != 0 {
        return Err(unsupported(page_index, "bfrange destination overflowed"));
    }
    Ok(output)
}

fn same_codespaces(left: &[CodeSpace], right: &[CodeSpace]) -> bool {
    let mut left: Vec<_> = left
        .iter()
        .map(|space| (space.len, space.start, space.end))
        .collect();
    let mut right: Vec<_> = right
        .iter()
        .map(|space| (space.len, space.start, space.end))
        .collect();
    left.sort_unstable();
    right.sort_unstable();
    left == right
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

fn parse_cid_vertical_widths(
    document: &Document,
    array: &[Object],
    widths: &mut BTreeMap<u16, VerticalMetric>,
    page_index: u32,
) -> Result<()> {
    let mut index = 0usize;
    while index < array.len() {
        let start = cid_number(document, &array[index], page_index)?;
        index += 1;
        let next = array
            .get(index)
            .ok_or_else(|| unsupported(page_index, "CID /W2 ends after a start CID"))?;
        match deref(document, next)? {
            Object::Array(values) => {
                if values.is_empty() || !values.len().is_multiple_of(3) {
                    return Err(unsupported(
                        page_index,
                        "CID /W2 array-form metrics are not complete triples",
                    ));
                }
                for (offset, triple) in values.chunks_exact(3).enumerate() {
                    let cid = u16::try_from(usize::from(start) + offset)
                        .map_err(|_| unsupported(page_index, "CID /W2 range exceeds 65535"))?;
                    widths.insert(cid, vertical_metric(document, triple, page_index)?);
                }
                index += 1;
            }
            Object::Integer(_) => {
                let end = cid_number(document, next, page_index)?;
                if end < start {
                    return Err(unsupported(page_index, "CID /W2 range is inverted"));
                }
                let triple = array.get(index + 1..index + 4).ok_or_else(|| {
                    unsupported(page_index, "CID /W2 range has incomplete vertical metrics")
                })?;
                let metric = vertical_metric(document, triple, page_index)?;
                for cid in start..=end {
                    widths.insert(cid, metric);
                }
                index += 4;
            }
            _ => return Err(unsupported(page_index, "CID /W2 contains an invalid range")),
        }
    }
    Ok(())
}

fn vertical_metric(
    document: &Document,
    values: &[Object],
    page_index: u32,
) -> Result<VerticalMetric> {
    if values.len() != 3 {
        return Err(unsupported(
            page_index,
            "CID vertical metric is not a triple",
        ));
    }
    // A /W2 entry is the triple (w1y, v_x, v_y); v_y is validated but not carried.
    number(document, &values[2], page_index, "CID vertical position y")?;
    Ok(VerticalMetric {
        displacement_y: number(
            document,
            &values[0],
            page_index,
            "CID vertical displacement",
        )?,
        position_x: Some(number(
            document,
            &values[1],
            page_index,
            "CID vertical position x",
        )?),
    })
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

    let doomed_widget_ids = doomed
        .iter()
        .filter(|index| annotations[**index].widget)
        .map(|index| {
            annotations[*index].id.ok_or_else(|| {
                unsupported(
                    plan.page_index,
                    "intersecting Widget annotation is direct and cannot be pruned safely",
                )
            })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let removed_field_ids = unlink_widget_fields(document, &doomed_widget_ids, plan.page_index)?;
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
            removed.push((
                annotation.bounds,
                if annotation.widget {
                    RedactionObjectKind::Widget
                } else {
                    RedactionObjectKind::Annotation
                },
            ));
            if let Some(id) = annotation.id {
                removed_ids.insert(id);
            }
        } else {
            kept.push(annotation.entry);
        }
    }
    removed_ids.extend(removed_field_ids);
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

#[derive(Debug, Default)]
struct FieldSubtree {
    object_ids: BTreeSet<ObjectId>,
    widget_ids: BTreeSet<ObjectId>,
    sensitive_ids: BTreeSet<ObjectId>,
}

#[derive(Debug, Clone, Copy)]
enum AcroFormLocation {
    Indirect(ObjectId),
    Direct,
}

#[derive(Debug, Clone, Copy)]
enum FieldsLocation {
    Indirect(ObjectId),
    Direct,
}

fn unlink_widget_fields(
    document: &mut Document,
    doomed_widget_ids: &BTreeSet<ObjectId>,
    page_index: u32,
) -> Result<BTreeSet<ObjectId>> {
    if doomed_widget_ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let acroform_object = document
        .catalog()
        .map_err(|error| unsupported(page_index, format!("PDF catalog cannot be read: {error}")))?
        .get(b"AcroForm")
        .map_err(|_| unsupported(page_index, "intersecting Widget has no catalog /AcroForm"))?
        .clone();
    let (acroform_location, mut acroform) = match acroform_object {
        Object::Reference(acroform_id) => (
            AcroFormLocation::Indirect(acroform_id),
            document
                .get_dictionary(acroform_id)
                .map_err(|_| unsupported(page_index, "catalog /AcroForm is not a dictionary"))?
                .clone(),
        ),
        Object::Dictionary(acroform) => (AcroFormLocation::Direct, acroform),
        _ => {
            return Err(unsupported(
                page_index,
                "catalog /AcroForm is not a dictionary",
            ));
        }
    };
    let fields_object = acroform
        .get(b"Fields")
        .map_err(|_| unsupported(page_index, "catalog /AcroForm has no /Fields"))?
        .clone();
    let (fields_location, field_entries) = match fields_object {
        Object::Array(fields) => (FieldsLocation::Direct, fields),
        Object::Reference(fields_id) => (
            FieldsLocation::Indirect(fields_id),
            document
                .get_object(fields_id)
                .map_err(|_| unsupported(page_index, "/AcroForm /Fields cannot be resolved"))?
                .as_array()
                .map_err(|_| unsupported(page_index, "/AcroForm /Fields is not an array"))?
                .clone(),
        ),
        _ => {
            return Err(unsupported(page_index, "/AcroForm /Fields is not an array"));
        }
    };

    let mut roots = Vec::with_capacity(field_entries.len());
    let mut globally_seen = BTreeSet::new();
    for entry in &field_entries {
        let root_id = entry.as_reference().map_err(|_| {
            unsupported(
                page_index,
                "/AcroForm /Fields contains a direct field that cannot be pruned safely",
            )
        })?;
        let mut subtree = FieldSubtree::default();
        collect_field_subtree(document, root_id, None, &mut subtree, page_index)?;
        if subtree
            .object_ids
            .iter()
            .any(|object_id| !globally_seen.insert(*object_id))
        {
            return Err(unsupported(
                page_index,
                "AcroForm field objects are shared across root fields",
            ));
        }
        roots.push((root_id, subtree));
    }

    let mut matched_widgets = BTreeSet::new();
    let mut removed_roots = BTreeSet::new();
    let mut removed_ids = BTreeSet::new();
    for (root_id, subtree) in &roots {
        if subtree.widget_ids.is_disjoint(doomed_widget_ids) {
            continue;
        }
        if !subtree.widget_ids.is_subset(doomed_widget_ids) {
            return Err(unsupported(
                page_index,
                "redacted AcroForm field also has a widget outside the requested area or page",
            ));
        }
        matched_widgets.extend(subtree.widget_ids.iter().copied());
        removed_roots.insert(*root_id);
        removed_ids.extend(subtree.object_ids.iter().copied());
        removed_ids.extend(subtree.sensitive_ids.iter().copied());
    }
    if matched_widgets != *doomed_widget_ids {
        return Err(unsupported(
            page_index,
            "intersecting Widget is not uniquely reachable from /AcroForm /Fields",
        ));
    }

    for object_id in &removed_ids {
        let Ok(dictionary) = document.get_dictionary_mut(*object_id) else {
            continue;
        };
        dictionary.remove(b"V");
        dictionary.remove(b"DV");
        dictionary.remove(b"AP");
    }
    let kept_fields: Vec<_> = field_entries
        .into_iter()
        .filter(|entry| {
            entry
                .as_reference()
                .is_ok_and(|field_id| !removed_roots.contains(&field_id))
        })
        .collect();
    match fields_location {
        FieldsLocation::Indirect(fields_id) => {
            document
                .objects
                .insert(fields_id, Object::Array(kept_fields));
        }
        FieldsLocation::Direct => acroform.set("Fields", Object::Array(kept_fields)),
    }
    match acroform_location {
        AcroFormLocation::Indirect(acroform_id) => {
            document
                .objects
                .insert(acroform_id, Object::Dictionary(acroform));
        }
        AcroFormLocation::Direct => {
            document
                .catalog_mut()
                .map_err(|_| unsupported(page_index, "catalog /AcroForm cannot be updated"))?
                .set("AcroForm", Object::Dictionary(acroform));
        }
    }
    Ok(removed_ids)
}

fn collect_field_subtree(
    document: &Document,
    field_id: ObjectId,
    expected_parent: Option<ObjectId>,
    subtree: &mut FieldSubtree,
    page_index: u32,
) -> Result<()> {
    if !subtree.object_ids.insert(field_id) {
        return Err(unsupported(
            page_index,
            "AcroForm field tree contains a cycle or shared child",
        ));
    }
    if subtree.object_ids.len() > 100_000 {
        return Err(unsupported(
            page_index,
            "AcroForm field tree exceeds safe inspection complexity limits",
        ));
    }
    let dictionary = document
        .get_dictionary(field_id)
        .map_err(|_| unsupported(page_index, "AcroForm field reference is not a dictionary"))?;
    match (expected_parent, dictionary.get(b"Parent")) {
        (Some(expected), Ok(parent)) if parent.as_reference().ok() == Some(expected) => {}
        (Some(_), _) => {
            return Err(unsupported(
                page_index,
                "AcroForm child field has an inconsistent /Parent",
            ));
        }
        (None, Ok(_)) => {
            return Err(unsupported(
                page_index,
                "AcroForm root field unexpectedly has a /Parent",
            ));
        }
        (None, Err(_)) => {}
    }

    if dictionary
        .get(b"Subtype")
        .ok()
        .and_then(|object| deref(document, object).ok())
        .and_then(|object| object.as_name().ok())
        == Some(b"Widget")
    {
        subtree.widget_ids.insert(field_id);
    }
    for key in [b"V".as_slice(), b"DV"] {
        if let Ok(Object::Reference(value_id)) = dictionary.get(key) {
            subtree.sensitive_ids.insert(*value_id);
        }
    }
    if let Ok(appearance) = dictionary.get(b"AP") {
        collect_appearance_ids(
            document,
            appearance,
            &mut subtree.sensitive_ids,
            page_index,
            0,
        )?;
    }
    let Ok(kids) = dictionary.get(b"Kids") else {
        return Ok(());
    };
    let kids = deref(document, kids)?
        .as_array()
        .map_err(|_| unsupported(page_index, "AcroForm field /Kids is not an array"))?;
    for kid in kids {
        let kid_id = kid
            .as_reference()
            .map_err(|_| unsupported(page_index, "AcroForm field /Kids contains a direct child"))?;
        collect_field_subtree(document, kid_id, Some(field_id), subtree, page_index)?;
    }
    Ok(())
}

fn collect_appearance_ids(
    document: &Document,
    object: &Object,
    ids: &mut BTreeSet<ObjectId>,
    page_index: u32,
    depth: usize,
) -> Result<()> {
    if depth > 64 {
        return Err(unsupported(
            page_index,
            "Widget appearance dictionary exceeds safe nesting limits",
        ));
    }
    match object {
        Object::Reference(id) => {
            if !ids.insert(*id) {
                return Ok(());
            }
            let resolved = document.objects.get(id).ok_or_else(|| {
                unsupported(
                    page_index,
                    format!(
                        "Widget appearance reference {} {} cannot be resolved",
                        id.0, id.1
                    ),
                )
            })?;
            if !matches!(resolved, Object::Stream(_)) {
                collect_appearance_ids(document, resolved, ids, page_index, depth + 1)?;
            }
        }
        Object::Array(values) => {
            for value in values {
                collect_appearance_ids(document, value, ids, page_index, depth + 1)?;
            }
        }
        Object::Dictionary(dictionary) => {
            for (_, value) in dictionary.iter() {
                collect_appearance_ids(document, value, ids, page_index, depth + 1)?;
            }
        }
        Object::Stream(_) => {}
        _ => {}
    }
    Ok(())
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

fn verify_acroform_fields(document: &Document, plan: &PagePlan) -> Result<()> {
    let catalog = document.catalog().map_err(|error| {
        unsupported(
            plan.page_index,
            format!("PDF catalog cannot be inspected for AcroForm fields: {error}"),
        )
    })?;
    let Ok(acroform) = catalog.get(b"AcroForm") else {
        return Ok(());
    };
    let acroform = deref(document, acroform)?
        .as_dict()
        .map_err(|_| unsupported(plan.page_index, "catalog /AcroForm is not a dictionary"))?;
    let Ok(fields) = acroform.get(b"Fields") else {
        return Ok(());
    };
    let fields = deref(document, fields)?
        .as_array()
        .map_err(|_| unsupported(plan.page_index, "/AcroForm /Fields is not an array"))?;
    let mut pending = fields.clone();
    let mut visited = BTreeSet::new();
    let mut inspected = 0usize;
    while let Some(field) = pending.pop() {
        inspected += 1;
        if inspected > 100_000 {
            return Err(unsupported(
                plan.page_index,
                "AcroForm field tree exceeds safe verification complexity limits",
            ));
        }
        if let Object::Reference(id) = field {
            if !visited.insert(id) {
                continue;
            }
            let resolved = document.objects.get(&id).ok_or_else(|| {
                unsupported(
                    plan.page_index,
                    format!(
                        "AcroForm field reference {} {} cannot be resolved",
                        id.0, id.1
                    ),
                )
            })?;
            pending.push(resolved.clone());
            continue;
        }
        let dictionary = field
            .as_dict()
            .map_err(|_| unsupported(plan.page_index, "AcroForm field is not a dictionary"))?;
        if dictionary
            .get(b"Subtype")
            .ok()
            .and_then(|object| deref(document, object).ok())
            .and_then(|object| object.as_name().ok())
            == Some(b"Widget")
        {
            let belongs_to_page = dictionary
                .get(b"P")
                .ok()
                .and_then(|object| object.as_reference().ok())
                .is_none_or(|page_id| page_id == plan.page_id);
            if belongs_to_page
                && intersects_any(
                    annotation_rect(document, dictionary, plan.page_index)?,
                    &plan.boxes,
                )
            {
                return Err(unsupported(
                    plan.page_index,
                    "output verification found an intersecting Widget or form-field value",
                ));
            }
        }
        if let Ok(kids) = dictionary.get(b"Kids") {
            pending.extend(
                deref(document, kids)?
                    .as_array()
                    .map_err(|_| {
                        unsupported(plan.page_index, "AcroForm field /Kids is not an array")
                    })?
                    .iter()
                    .cloned(),
            );
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

fn merge_resources(
    document: &Document,
    inherited: Option<&Dictionary>,
    local: Option<&Dictionary>,
    page_index: u32,
) -> Result<Dictionary> {
    let mut merged = inherited.cloned().unwrap_or_default();
    let Some(local) = local else {
        return Ok(merged);
    };
    for (key, local_value) in local.iter() {
        let category = merged
            .get(key)
            .ok()
            .map(|inherited_value| {
                let inherited_dict = deref(document, inherited_value)?.as_dict().ok();
                let local_dict = deref(document, local_value)?.as_dict().ok();
                Ok::<_, PdfCoreError>(match (inherited_dict, local_dict) {
                    (Some(inherited_dict), Some(local_dict)) => {
                        let mut category = inherited_dict.clone();
                        for (name, value) in local_dict.iter() {
                            category.set(name.clone(), value.clone());
                        }
                        Some(Object::Dictionary(category))
                    }
                    _ => None,
                })
            })
            .transpose()?;
        merged.set(
            key.clone(),
            category.flatten().unwrap_or_else(|| local_value.clone()),
        );
    }
    if merged.len() > 100_000 {
        return Err(unsupported(
            page_index,
            "merged Form XObject resources exceed safe complexity limits",
        ));
    }
    Ok(merged)
}

fn remove_self_form_binding(
    document: &Document,
    resources: &mut Dictionary,
    name: &[u8],
    form_id: ObjectId,
    page_index: u32,
) -> Result<()> {
    let Ok(xobjects_object) = resources.get(b"XObject") else {
        return Ok(());
    };
    let mut xobjects = deref(document, xobjects_object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "Form /Resources/XObject is not a dictionary"))?
        .clone();
    let is_self = xobjects
        .get(name)
        .ok()
        .and_then(|object| document.dereference(object).ok())
        .and_then(|(id, _)| id)
        == Some(form_id);
    if !is_self {
        return Ok(());
    }
    xobjects.remove(name);
    if xobjects.is_empty() {
        resources.remove(b"XObject");
    } else {
        resources.set("XObject", Object::Dictionary(xobjects));
    }
    Ok(())
}

fn prune_unused_resource_names(
    document: &Document,
    resources: &mut Dictionary,
    operations: &[Operation],
    category: &[u8],
    operator: &str,
    removed_names: &BTreeSet<Vec<u8>>,
    page_index: u32,
) -> Result<()> {
    let remaining: BTreeSet<&[u8]> = operations
        .iter()
        .filter(|operation| operation.operator == operator)
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
    let category_object = resources.get(category).map_err(|_| {
        unsupported(
            page_index,
            format!(
                "removed resource has no /{} dictionary",
                String::from_utf8_lossy(category)
            ),
        )
    })?;
    let mut category_dict = deref(document, category_object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "resource category is not a dictionary"))?
        .clone();
    for name in unused {
        category_dict.remove(name);
    }
    if category_dict.is_empty() {
        resources.remove(category);
    } else {
        resources.set(category.to_vec(), Object::Dictionary(category_dict));
    }
    Ok(())
}

fn materialize_form_rewrite(
    document: &mut Document,
    mut rewrite: FormRewrite,
    page_index: u32,
) -> Result<ObjectId> {
    if !rewrite.nested_forms.is_empty() {
        let xobjects_object = rewrite
            .resources
            .get(b"XObject")
            .map_err(|_| unsupported(page_index, "rewritten Form has no /XObject resources"))?;
        let mut xobjects = deref(document, xobjects_object)?
            .as_dict()
            .map_err(|_| unsupported(page_index, "rewritten Form /XObject is not a dictionary"))?
            .clone();
        for (name, nested) in rewrite.nested_forms {
            let nested_id = materialize_form_rewrite(document, nested, page_index)?;
            xobjects.set(name, Object::Reference(nested_id));
        }
        rewrite
            .resources
            .set("XObject", Object::Dictionary(xobjects));
    }
    let bytes = encode_operations(&rewrite.operations, page_index)?;
    rewrite
        .stream
        .dict
        .set("Resources", Object::Dictionary(rewrite.resources));
    rewrite.stream.set_plain_content(bytes);
    Ok(document.add_object(rewrite.stream))
}

fn attach_rewritten_forms(
    document: &mut Document,
    page_id: ObjectId,
    rewritten_forms: BTreeMap<Vec<u8>, FormRewrite>,
    page_index: u32,
) -> Result<()> {
    if rewritten_forms.is_empty() {
        return Ok(());
    }
    let inherited = find_inherited(document, page_id, b"Resources")?
        .ok_or_else(|| unsupported(page_index, "rewritten Form has no page /Resources"))?;
    let inherited = deref(document, inherited)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "page /Resources is not a dictionary"))?
        .clone();
    let mut resources = merge_resources(document, Some(&inherited), None, page_index)?;
    let xobjects_object = resources
        .get(b"XObject")
        .map_err(|_| unsupported(page_index, "rewritten Form has no /XObject dictionary"))?;
    let mut xobjects = deref(document, xobjects_object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "page /Resources/XObject is not a dictionary"))?
        .clone();
    for (name, rewrite) in rewritten_forms {
        let form_id = materialize_form_rewrite(document, rewrite, page_index)?;
        xobjects.set(name, Object::Reference(form_id));
    }
    resources.set("XObject", Object::Dictionary(xobjects));
    document
        .get_dictionary_mut(page_id)
        .map_err(|error| {
            unsupported(
                page_index,
                format!("page resources cannot be updated for rewritten Form: {error}"),
            )
        })?
        .set("Resources", Object::Dictionary(resources));
    Ok(())
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

fn detach_unused_shadings(
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
        .filter(|operation| operation.operator == "sh")
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
        .ok_or_else(|| unsupported(page_index, "removed shading has no page /Resources"))?;
    let mut resources = deref(document, resources_object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "page /Resources is not a dictionary"))?
        .clone();
    let shadings_object = resources
        .get(b"Shading")
        .map_err(|_| unsupported(page_index, "removed shading has no resource dictionary"))?;
    let mut shadings = deref(document, shadings_object)?
        .as_dict()
        .map_err(|_| unsupported(page_index, "page /Resources/Shading is not a dictionary"))?
        .clone();
    for name in unused {
        shadings.remove(name);
    }
    if shadings.is_empty() {
        resources.remove(b"Shading");
    } else {
        resources.set("Shading", Object::Dictionary(shadings));
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

fn transform_box(matrix: Matrix, values: [f64; 4], page_index: u32) -> Result<BoxPts> {
    transform_box_with_margin(matrix, values, BBOX_MARGIN_PT, page_index)
}

fn transform_glyph_box(matrix: Matrix, values: [f64; 4], page_index: u32) -> Result<BoxPts> {
    transform_box_with_margin(matrix, values, 0.01, page_index)
}

fn transform_box_with_margin(
    matrix: Matrix,
    values: [f64; 4],
    margin: f64,
    page_index: u32,
) -> Result<BoxPts> {
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
        - margin;
    let max_x = points
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max)
        + margin;
    let min_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min)
        - margin;
    let max_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max)
        + margin;
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

fn inflate_stroked_text_bounds(
    bounds: BoxPts,
    graphics: &GraphicsState,
    page_index: u32,
) -> Result<BoxPts> {
    let radius = graphics.line_width / 2.0;
    let x_margin = radius * (graphics.ctm.a.abs() + graphics.ctm.c.abs());
    let y_margin = radius * (graphics.ctm.b.abs() + graphics.ctm.d.abs());
    inflate_bounds_by_margins(bounds, x_margin, y_margin, page_index)
}

fn inflate_stroked_path_bounds(
    bounds: BoxPts,
    graphics: &GraphicsState,
    path: &PathGeometry,
    page_index: u32,
) -> Result<BoxPts> {
    let radius = graphics.line_width * graphics.miter_limit / 2.0;
    let x_scale = path
        .stroke_scale_x
        .max(graphics.ctm.a.abs() + graphics.ctm.c.abs());
    let y_scale = path
        .stroke_scale_y
        .max(graphics.ctm.b.abs() + graphics.ctm.d.abs());
    inflate_bounds_by_margins(bounds, radius * x_scale, radius * y_scale, page_index)
}

fn inflate_bounds_by_margins(
    bounds: BoxPts,
    x_margin: f64,
    y_margin: f64,
    page_index: u32,
) -> Result<BoxPts> {
    if ![x_margin, y_margin].iter().all(|value| value.is_finite()) {
        return Err(unsupported(page_index, "stroked bounds are non-finite"));
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
            "stroked bounds exceed supported precision",
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

fn intersect_box(a: BoxPts, b: BoxPts) -> Option<BoxPts> {
    let bounds = BoxPts {
        left: a.left.max(b.left),
        right: a.right.min(b.right),
        bottom: a.bottom.max(b.bottom),
        top: a.top.min(b.top),
    };
    bounds.is_finite_and_nonempty().then_some(bounds)
}

fn visible_graphics_bounds(
    bounds: BoxPts,
    clip: ActiveClip,
    page_bounds: BoxPts,
) -> Option<BoxPts> {
    clip.conservative_bounds(page_bounds)
        .and_then(|clip_bounds| intersect_box(bounds, clip_bounds))
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

/// True when the operator paints at least one glyph.
fn shows_glyphs(operation: &Operation) -> bool {
    let has_glyphs =
        |object: &Object| matches!(object, Object::String(bytes, _) if !bytes.is_empty());
    match operation.operator.as_str() {
        "Tj" | "'" | "\"" => operation.operands.iter().any(has_glyphs),
        "TJ" => operation
            .operands
            .first()
            .and_then(|operand| operand.as_array().ok())
            .is_some_and(|items| items.iter().any(has_glyphs)),
        _ => false,
    }
}

/// A show operator left glyph-free by run-level rewriting: it paints nothing and
/// only carries the displacement of the glyphs that were removed.
fn is_glyph_free_show(operation: &Operation) -> bool {
    matches!(operation.operator.as_str(), "Tj" | "TJ" | "'" | "\"") && !shows_glyphs(operation)
}

/// Operators that re-seat the text position from the line matrix, making any
/// displacement accumulated by earlier show operators unobservable. `'` and `"`
/// begin with an implicit `T*`, so they re-seat before painting.
fn reseats_text_position(operation: &Operation) -> bool {
    matches!(
        operation.operator.as_str(),
        "Td" | "TD" | "Tm" | "T*" | "'" | "\""
    )
}

/// Drops glyph-free show operators whose displacement cannot reach a later
/// glyph. Removing every glyph of a run leaves `[-advance] TJ`, which paints
/// nothing; if no subsequent glyph in the same `BT..ET` is positioned relative
/// to it, the operator has no observable effect and must not survive into the
/// redacted page.
fn prune_unobservable_shows(operations: &mut Vec<Operation>) {
    let mut keep = vec![true; operations.len()];
    for index in 0..operations.len() {
        if !is_glyph_free_show(&operations[index]) {
            continue;
        }
        let mut observable = false;
        for candidate in &operations[index + 1..] {
            if reseats_text_position(candidate) || candidate.operator == "ET" {
                break;
            }
            if shows_glyphs(candidate) {
                observable = true;
                break;
            }
        }
        keep[index] = observable;
    }
    let mut index = 0;
    operations.retain(|_| {
        let retained = keep[index];
        index += 1;
        retained
    });
}

fn rewrite_glyph_bytes_as_tj(
    bytes: &[u8],
    boundaries: &[(usize, usize)],
    removed: &[bool],
    displacement_milli: &[f64],
    page_index: u32,
) -> Result<Operation> {
    if boundaries.len() != removed.len() || boundaries.len() != displacement_milli.len() {
        return Err(unsupported(
            page_index,
            "glyph rewrite inputs have inconsistent lengths",
        ));
    }
    let mut expected_start = 0usize;
    for &(start, end) in boundaries {
        if start != expected_start || end <= start || end > bytes.len() {
            return Err(unsupported(
                page_index,
                "glyph rewrite boundaries do not partition the text string",
            ));
        }
        expected_start = end;
    }
    if expected_start != bytes.len() {
        return Err(unsupported(
            page_index,
            "glyph rewrite boundaries do not cover the text string",
        ));
    }

    let mut items = Vec::new();
    let mut kept_start = 0usize;
    let mut index = 0usize;
    while index < boundaries.len() {
        if !removed[index] {
            index += 1;
            continue;
        }
        let removed_start = boundaries[index].0;
        if kept_start < removed_start {
            items.push(Object::string_literal(
                bytes[kept_start..removed_start].to_vec(),
            ));
        }
        let mut displacement = 0.0;
        while index < boundaries.len() && removed[index] {
            displacement += displacement_milli[index];
            index += 1;
        }
        if !displacement.is_finite() || displacement.abs() > f64::from(f32::MAX) {
            return Err(unsupported(
                page_index,
                "glyph rewrite displacement is outside supported numeric precision",
            ));
        }
        items.push(Object::Real((-displacement) as f32));
        kept_start = boundaries[index - 1].1;
    }
    if kept_start < bytes.len() {
        items.push(Object::string_literal(bytes[kept_start..].to_vec()));
    }
    if items.is_empty() {
        items.push(Object::string_literal(bytes.to_vec()));
    }
    Ok(Operation::new("TJ", vec![Object::Array(items)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    #[test]
    fn embedded_cid_true_type_uses_outline_bounds_without_changing_advance() {
        let bytes =
            include_bytes!("../../packages/doc-yantra-fonts/fonts/LiberationSans-Regular.ttf");
        let face = ttf_parser::Face::parse(bytes, 0).unwrap();
        let glyph = face.glyph_index('i').unwrap();
        let rect = face.glyph_bounding_box(glyph).unwrap();
        let mut document = Document::with_version("1.7");
        let program = document.add_object(Stream::new(Dictionary::new(), bytes.to_vec()));
        let descriptor = document.add_object(lopdf::dictionary! {
            "FontFile2" => program, "Ascent" => 1000, "Descent" => -300,
            "FontBBox" => vec![0.into(), (-300).into(), 1200.into(), 1000.into()],
        });
        let descendant = document.add_object(lopdf::dictionary! {
            "Subtype" => "CIDFontType2", "FontDescriptor" => descriptor,
            "CIDToGIDMap" => "Identity", "DW" => 1000,
        });
        let font = lopdf::dictionary! {
            "Subtype" => "Type0", "Encoding" => "Identity-H",
            "DescendantFonts" => vec![descendant.into()],
        };
        let metrics = parse_cid_font(&document, &font, 0).unwrap();
        metrics
            .for_each_code(&glyph.0.to_be_bytes(), 0, |code| {
                let scale = 1000.0 / f64::from(face.units_per_em());
                assert_eq!(code.width, 1000.0, "PDF advance must remain unchanged");
                assert_eq!(
                    code.glyph_box,
                    [
                        f64::from(rect.x_min) * scale,
                        f64::from(rect.y_min) * scale,
                        f64::from(rect.x_max) * scale,
                        f64::from(rect.y_max) * scale
                    ]
                );
                Ok(())
            })
            .unwrap();
        // Vertical mapping has not been validated for the tighter envelope.
        let mut vertical = font.clone();
        vertical.set("Encoding", "Identity-V");
        parse_cid_font(&document, &vertical, 0)
            .unwrap()
            .for_each_code(&glyph.0.to_be_bytes(), 0, |code| {
                assert_eq!(code.glyph_box, [0.0, -300.0, 1000.0, 1000.0]);
                Ok(())
            })
            .unwrap();
        // Missing/invalid font programs never justify shrinking conservative bounds.
        let mut invalid_document = document.clone();
        invalid_document.objects.insert(
            program,
            Object::Stream(Stream::new(Dictionary::new(), b"invalid font".to_vec())),
        );
        parse_cid_font(&invalid_document, &font, 0)
            .unwrap()
            .for_each_code(&glyph.0.to_be_bytes(), 0, |code| {
                assert_eq!(code.glyph_box, [0.0, -300.0, 1000.0, 1000.0]);
                Ok(())
            })
            .unwrap();
        // The neighbor's advance box overlaps the selection; its ink does not.
        // Both glyphs deliberately share the embedded font resource.
        let secret = face.glyph_index('m').unwrap().0.to_be_bytes();
        let retained = glyph.0.to_be_bytes();
        let font_id = document.add_object(font);
        let pages_id = document.new_object_id();
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 20.into()]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        50.into(),
                        100.into(),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal(retained.to_vec())]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        60.into(),
                        100.into(),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal(secret.to_vec())]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap();
        let stream = document.add_object(Stream::new(Dictionary::new(), content));
        let page = document.add_object(dictionary! { "Type"=>"Page", "Parent"=>pages_id,
            "MediaBox"=>vec![0.into(),0.into(),200.into(),200.into()], "Contents"=>stream,
            "Resources"=>dictionary! {"Font"=>dictionary! {"F1"=>font_id}},
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(
                dictionary! {"Type"=>"Pages", "Count"=>1, "Kids"=>vec![page.into()]},
            ),
        );
        let catalog = document.add_object(dictionary! {"Type"=>"Catalog", "Pages"=>pages_id});
        document.trailer.set("Root", catalog);
        let mut input = Vec::new();
        document.save_to(&mut input).unwrap();
        let result = redact_pure(
            &input,
            &[RedactArea {
                page_index: 0,
                left: 30.5,
                top: 43.0,
                width: 2.0,
                height: 8.0,
            }],
        )
        .unwrap();
        let saved = Document::load_mem(&result.pdf).unwrap();
        let page = *saved.get_pages().get(&1).unwrap();
        let operations = Content::decode(&saved.get_page_content(page).unwrap())
            .unwrap()
            .operations;
        let mut strings = Vec::new();
        for op in &operations {
            for operand in &op.operands {
                if let Object::String(bytes, _) = operand {
                    strings.push(bytes.clone());
                }
                if let Object::Array(parts) = operand {
                    for p in parts {
                        if let Object::String(bytes, _) = p {
                            strings.push(bytes.clone());
                        }
                    }
                }
            }
        }
        assert!(
            strings.iter().any(|s| s.windows(2).any(|p| p == retained)),
            "neighbor glyph bytes removed"
        );
        assert!(
            !strings.iter().any(|s| s.windows(2).any(|p| p == secret)),
            "selected glyph bytes survived"
        );
        assert!(
            operations.iter().any(|op| op.operator == "Tm"
                && op.operands
                    == vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        50.into(),
                        100.into()
                    ]),
            "neighbor transform changed"
        );
    }

    #[test]
    fn empty_glyph_removal_rewrite_is_an_algebraic_noop() {
        let bytes = b"ALPHA BETA";
        let boundaries = (0..bytes.len())
            .map(|start| (start, start + 1))
            .collect::<Vec<_>>();
        let removed = vec![false; boundaries.len()];
        let displacement_milli = vec![500.0; boundaries.len()];

        let rewritten =
            rewrite_glyph_bytes_as_tj(bytes, &boundaries, &removed, &displacement_milli, 0)
                .expect("empty removal set is rewritable");

        assert_eq!(rewritten.operator, "TJ");
        assert_eq!(
            rewritten.operands,
            vec![Object::Array(vec![Object::string_literal(bytes.to_vec())])]
        );
    }

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

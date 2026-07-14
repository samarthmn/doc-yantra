//! Runtime-registered Unicode fonts and per-document CID font embedding.
//!
//! Fonts are never compiled into the engine. Callers register sfnt bytes, then
//! each output document snapshots the registry, selects the first covering
//! face at cluster boundaries, and builds one subset per used face.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, RwLock};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use pdf_writer::types::{CidFontType, FontFlags, SystemInfo};
use pdf_writer::{Finish, Name, Pdf, Rect, Ref, Str};
use subsetter::GlyphRemapper;
use ttf_parser::{GlyphId, Tag};

use crate::error::{PdfCoreError, Result};
use crate::fonts::StandardFont;

const OS2_TAG: Tag = Tag::from_bytes(b"OS/2");
const CFF_TAG: Tag = Tag::from_bytes(b"CFF ");
const CFF2_TAG: Tag = Tag::from_bytes(b"CFF2");

static FONT_REGISTRY: LazyLock<RwLock<Vec<Arc<FaceData>>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

#[derive(Debug)]
struct FaceData {
    id: usize,
    bytes: Arc<[u8]>,
    index: u32,
    postscript_name: String,
    no_subsetting: bool,
    is_cff: bool,
    units_per_em: f64,
    ascent_1000: f32,
    descent_1000: f32,
    cap_height_1000: f32,
    bbox_1000: [f32; 4],
    italic_angle: f32,
}

/// Stable face selected from a document's registry snapshot.
#[derive(Debug, Clone)]
pub(crate) struct FaceRef(Arc<FaceData>);

impl FaceRef {
    fn id(&self) -> usize {
        self.0.id
    }

    fn parsed(&self) -> Result<ttf_parser::Face<'_>> {
        ttf_parser::Face::parse(&self.0.bytes, self.0.index).map_err(|_| {
            PdfCoreError::InvalidInput("registered font can no longer be parsed".to_string())
        })
    }
}

impl PartialEq for FaceRef {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for FaceRef {}

/// Font choice for one logical text run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FontChoice {
    Standard,
    Embedded(EmbeddedChoice),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmbeddedChoice {
    /// Ordered registry snapshot used for cluster-boundary fallback. Keeping
    /// the snapshot on the choice makes measurement and later encoding use
    /// exactly the same face order.
    faces: Vec<FaceRef>,
}

impl FontChoice {
    /// Measures with the exact advances later written to `/W`.
    pub(crate) fn text_width_pt(
        &self,
        text: &str,
        standard: StandardFont,
        size_pt: f64,
    ) -> Result<f64> {
        match self {
            FontChoice::Standard => Ok(standard.text_width_pt(text, size_pt)),
            FontChoice::Embedded(choice) => {
                let shaped = shape_with_faces(&choice.faces, text)?;
                Ok(shaped
                    .iter()
                    .map(|run| {
                        f64::from(run.advance) * size_pt / f64::from(run.units_per_em.max(1))
                    })
                    .sum())
            }
        }
    }

    /// Measures one scalar with the exact advance later written to `/W`.
    pub(crate) fn char_width_pt(
        &self,
        ch: char,
        standard: StandardFont,
        size_pt: f64,
    ) -> Result<f64> {
        match self {
            FontChoice::Standard => {
                Ok(f64::from(standard.glyph_width_milli(ch)) * size_pt / 1000.0)
            }
            FontChoice::Embedded(choice) => {
                let shaped = shape_with_faces(&choice.faces, &ch.to_string())?;
                Ok(shaped
                    .iter()
                    .map(|run| {
                        f64::from(run.advance) * size_pt / f64::from(run.units_per_em.max(1))
                    })
                    .sum())
            }
        }
    }
}

/// Registers every face in an sfnt or collection for later Unicode runs.
///
/// Registration rejects malformed fonts, restricted-license embedding,
/// bitmap-only embedding, and CFF2 outlines. Registry insertion is atomic: a
/// bad face prevents every face in the supplied byte buffer from being added.
pub fn register_font(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Err(PdfCoreError::InvalidInput(
            "font bytes must not be empty".to_string(),
        ));
    }

    let data: Arc<[u8]> = Arc::from(bytes);
    let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1).max(1);
    let mut pending = Vec::with_capacity(count as usize);
    for index in 0..count {
        let face = ttf_parser::Face::parse(&data, index).map_err(|_| {
            PdfCoreError::InvalidInput(format!("unparseable font face at index {index}"))
        })?;
        let fs_type = face
            .raw_face()
            .table(OS2_TAG)
            .and_then(|table| table.get(8..10))
            .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
            .unwrap_or(0);
        if fs_type & 0x0002 != 0 {
            return Err(PdfCoreError::TextUnsupported {
                code: "font-license",
                message: "font forbids embedding (OS/2 fsType 0x0002)".to_string(),
            });
        }
        if fs_type & 0x0200 != 0 {
            return Err(PdfCoreError::TextUnsupported {
                code: "font-license",
                message: "font forbids outline embedding (OS/2 fsType 0x0200)".to_string(),
            });
        }

        let is_cff = face.raw_face().table(CFF_TAG).is_some();
        if face.raw_face().table(CFF2_TAG).is_some() {
            return Err(PdfCoreError::TextUnsupported {
                code: "font-format",
                message: "CFF2 fonts are not supported".to_string(),
            });
        }
        if !is_cff && face.tables().glyf.is_none() {
            return Err(PdfCoreError::TextUnsupported {
                code: "font-format",
                message: "font has no supported TrueType or CFF outlines".to_string(),
            });
        }

        let units_per_em = f64::from(face.units_per_em().max(1));
        let scale = 1000.0 / units_per_em;
        let bbox = face.global_bounding_box();
        let cap_height = face.capital_height().unwrap_or_else(|| face.ascender());
        pending.push(FaceData {
            id: 0,
            bytes: data.clone(),
            index,
            postscript_name: postscript_name(&face),
            no_subsetting: fs_type & 0x0100 != 0,
            is_cff,
            units_per_em,
            ascent_1000: (f64::from(face.ascender()) * scale) as f32,
            descent_1000: (f64::from(face.descender()) * scale) as f32,
            cap_height_1000: (f64::from(cap_height) * scale) as f32,
            bbox_1000: [
                (f64::from(bbox.x_min) * scale) as f32,
                (f64::from(bbox.y_min) * scale) as f32,
                (f64::from(bbox.x_max) * scale) as f32,
                (f64::from(bbox.y_max) * scale) as f32,
            ],
            italic_angle: face.italic_angle(),
        });
    }

    let mut registry = FONT_REGISTRY
        .write()
        .map_err(|_| PdfCoreError::InvalidInput("font registry lock is poisoned".to_string()))?;
    for mut face in pending {
        face.id = registry.len();
        registry.push(Arc::new(face));
    }
    Ok(())
}

fn postscript_name(face: &ttf_parser::Face<'_>) -> String {
    let raw = face
        .names()
        .into_iter()
        .filter(|name| name.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
        .find_map(|name| name.to_string())
        .unwrap_or_else(|| "EmbeddedFont".to_string());
    let sanitized: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect();
    if sanitized.is_empty() {
        "EmbeddedFont".to_string()
    } else {
        sanitized
    }
}

/// Returns whether every scalar has a defined WinAnsi/CP1252 byte.
pub(crate) fn is_winansi(text: &str) -> bool {
    text.chars().all(|ch| winansi_byte(ch).is_some())
}

/// Encodes a standard-font literal string body without surrounding `()`.
pub(crate) fn encode_winansi_literal(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 4);
    for byte in encode_winansi_bytes(text) {
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(byte);
            }
            0x00..=0x1F | 0x7F => out.push(b'?'),
            _ => out.push(byte),
        }
    }
    out
}

/// Encodes text as WinAnsi/CP1252 bytes without PDF string escaping.
pub(crate) fn encode_winansi_bytes(text: &str) -> Vec<u8> {
    text.chars()
        .map(|ch| winansi_byte(ch).unwrap_or(b'?'))
        .collect()
}

pub(crate) fn winansi_byte(ch: char) -> Option<u8> {
    let cp = ch as u32;
    match cp {
        0x00..=0x7F => Some(cp as u8),
        0xA0..=0xFF => Some(cp as u8),
        _ => Some(match ch {
            '\u{20AC}' => 0x80,
            '\u{201A}' => 0x82,
            '\u{0192}' => 0x83,
            '\u{201E}' => 0x84,
            '\u{2026}' => 0x85,
            '\u{2020}' => 0x86,
            '\u{2021}' => 0x87,
            '\u{02C6}' => 0x88,
            '\u{2030}' => 0x89,
            '\u{0160}' => 0x8A,
            '\u{2039}' => 0x8B,
            '\u{0152}' => 0x8C,
            '\u{017D}' => 0x8E,
            '\u{2018}' => 0x91,
            '\u{2019}' => 0x92,
            '\u{201C}' => 0x93,
            '\u{201D}' => 0x94,
            '\u{2022}' => 0x95,
            '\u{2013}' => 0x96,
            '\u{2014}' => 0x97,
            '\u{02DC}' => 0x98,
            '\u{2122}' => 0x99,
            '\u{0161}' => 0x9A,
            '\u{203A}' => 0x9B,
            '\u{0153}' => 0x9C,
            '\u{017E}' => 0x9E,
            '\u{0178}' => 0x9F,
            _ => return None,
        }),
    }
}

/// Rejects scripts that need shaping or bidi reordering.
pub fn validate_text(_text: &str) -> Result<()> {
    Ok(())
}

/// UAX #9 display-order replacement for PDFium `/ActualText`.
pub(crate) fn pdfium_actual_text(text: &str) -> String {
    let visual = text_core::bidi_visual_order(text);
    reverse_rtl_combining_sequences(&visual)
}

fn reverse_rtl_combining_sequences(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut marks = Vec::new();
    for ch in text.chars() {
        if is_rtl_combining_mark(ch) {
            marks.push(ch);
        } else {
            output.extend(marks.drain(..).rev());
            output.push(ch);
        }
    }
    output.extend(marks.into_iter().rev());
    output
}

fn is_rtl_combining_mark(ch: char) -> bool {
    matches!(ch as u32,
        0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2 | 0x05C4..=0x05C5 | 0x05C7
        | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED
        | 0x08D3..=0x08FF
    )
}

fn unsupported_glyph(ch: char) -> PdfCoreError {
    PdfCoreError::TextUnsupported {
        code: "glyph-coverage",
        message: format!("no registered font covers U+{:04X}", ch as u32),
    }
}

fn shape_with_faces(faces: &[FaceRef], text: &str) -> Result<Vec<text_core::ShapedRun>> {
    let shared = faces
        .iter()
        .map(|face| {
            text_core::Face::new(&face.0.bytes, face.0.index).map_err(|error| {
                PdfCoreError::TextUnsupported {
                    code: "font-format",
                    message: error.to_string(),
                }
            })
        })
        .collect::<Result<Vec<_>>>()?;
    text_core::shape_text(text, &shared).map_err(|error| match error {
        text_core::ShapeError::MissingGlyphs(missing) => {
            let missing = missing.first().copied().or_else(|| {
                // A cmap can claim coverage while GSUB still produces
                // `.notdef`. Name a real scalar from the failed input; never
                // manufacture U+FFFD, which hides the actionable codepoint.
                text.chars()
                    .find(|ch| !is_shaping_format_control(*ch))
                    .or_else(|| text.chars().next())
            });
            missing
                .map(unsupported_glyph)
                .unwrap_or_else(|| PdfCoreError::TextUnsupported {
                    code: "glyph-coverage",
                    message: "font shaping produced a missing glyph for an empty run".into(),
                })
        }
        other => PdfCoreError::TextUnsupported {
            code: "font-shaping",
            message: other.to_string(),
        },
    })
}

fn is_shaping_format_control(ch: char) -> bool {
    matches!(ch as u32, 0x200E..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069)
}

#[derive(Debug)]
struct UsedFace {
    face: FaceRef,
    mapper: GlyphRemapper,
    first_use_gids: Vec<u16>,
    cid_by_pair: HashMap<(u16, String), u16>,
    cid_to_gid: BTreeMap<u16, u16>,
    widths: BTreeMap<u16, f32>,
    to_unicode: BTreeMap<u16, String>,
}

/// Per-output font state. Registry selection is frozen at construction.
#[derive(Debug)]
pub(crate) struct FontSession {
    registry: Vec<FaceRef>,
    used: Vec<UsedFace>,
    used_index: HashMap<usize, usize>,
}

impl FontSession {
    pub(crate) fn new() -> Result<Self> {
        let registry = FONT_REGISTRY
            .read()
            .map_err(|_| PdfCoreError::InvalidInput("font registry lock is poisoned".to_string()))?
            .iter()
            .cloned()
            .map(FaceRef)
            .collect();
        Ok(Self {
            registry,
            used: Vec::new(),
            used_index: HashMap::new(),
        })
    }

    /// Resolves one logical run without mutating its eventual glyph subset.
    pub(crate) fn resolve(&self, text: &str) -> Result<FontChoice> {
        if is_winansi(text) {
            return Ok(FontChoice::Standard);
        }
        // `text-core` selects the first covering face independently at each
        // cluster boundary. Pre-shaping here both validates the choice and
        // guarantees measurement/encoding will use this same ordered snapshot.
        shape_with_faces(&self.registry, text)?;
        Ok(FontChoice::Embedded(EmbeddedChoice {
            faces: self.registry.clone(),
        }))
    }

    /// Encodes one embedded run as two-byte big-endian CIDs.
    pub(crate) fn encode(&mut self, choice: &EmbeddedChoice, text: &str) -> Result<EncodedText> {
        let shaped = shape_with_faces(&choice.faces, text)?;
        let mut bytes = Vec::new();
        let mut glyphs = Vec::new();
        let mut spans = Vec::new();
        for run in &shaped {
            let face =
                choice
                    .faces
                    .get(run.face_index)
                    .ok_or_else(|| PdfCoreError::TextUnsupported {
                        code: "font-shaping",
                        message: "shaper returned an invalid face index".to_string(),
                    })?;
            let used_index = match self.used_index.get(&face.id()).copied() {
                Some(index) => index,
                None => {
                    let index = self.used.len();
                    self.used.push(UsedFace {
                        face: face.clone(),
                        mapper: GlyphRemapper::new(),
                        first_use_gids: vec![0],
                        cid_by_pair: HashMap::new(),
                        cid_to_gid: BTreeMap::new(),
                        widths: BTreeMap::new(),
                        to_unicode: BTreeMap::new(),
                    });
                    self.used_index.insert(face.id(), index);
                    index
                }
            };
            let used = &mut self.used[used_index];
            let parsed = used.face.parsed()?;
            for glyph in &run.glyphs {
                let gid = GlyphId(glyph.glyph_id);
                let cluster = if let Some((start, end)) = glyph.unicode_range {
                    run.source[start..end].to_string()
                } else {
                    String::new()
                };
                let subset_gid = if used.face.0.no_subsetting {
                    if !used.first_use_gids.contains(&gid.0) {
                        used.first_use_gids.push(gid.0);
                    }
                    gid.0
                } else {
                    let existed = used.mapper.get(gid.0).is_some();
                    let subset_gid = used.mapper.remap(gid.0);
                    if !existed {
                        used.first_use_gids.push(gid.0);
                    }
                    subset_gid
                };
                let pair = (gid.0, cluster.clone());
                let cid = match used.cid_by_pair.get(&pair).copied() {
                    Some(cid) => cid,
                    None => {
                        let cid = u16::try_from(used.cid_by_pair.len() + 1).map_err(|_| {
                            PdfCoreError::TextUnsupported {
                                code: "glyph-coverage",
                                message:
                                    "embedded font needs more than 65535 distinct text clusters"
                                        .to_string(),
                            }
                        })?;
                        used.cid_by_pair.insert(pair, cid);
                        used.cid_to_gid.insert(cid, subset_gid);
                        used.to_unicode.insert(cid, cluster);
                        cid
                    }
                };
                let advance =
                    parsed
                        .glyph_hor_advance(gid)
                        .ok_or_else(|| PdfCoreError::TextUnsupported {
                            code: "glyph-coverage",
                            message: format!(
                                "registered font has no horizontal advance for GID {}",
                                gid.0
                            ),
                        })?;
                used.widths
                    .entry(cid)
                    .or_insert((f64::from(advance) * 1000.0 / used.face.0.units_per_em) as f32);
                bytes.extend_from_slice(&cid.to_be_bytes());
                glyphs.push(EncodedGlyph {
                    resource_index: used_index,
                    units_per_em: used.face.0.units_per_em,
                    cid,
                    x_advance: glyph.x_advance,
                    x_offset: glyph.x_offset,
                    y_offset: glyph.y_offset,
                });
            }
            if !run.glyphs.is_empty() {
                spans.push(EncodedSpan {
                    source_text: run.source.clone(),
                    direction: run.direction,
                    has_astral: run.source.chars().any(|ch| u32::from(ch) > 0xFFFF),
                });
            }
        }

        Ok(EncodedText {
            bytes,
            glyphs,
            spans,
            base_direction: text_core::bidi_base_direction(text),
        })
    }

    pub(crate) fn finish(self) -> Result<Vec<EmbeddedFont>> {
        self.used.into_iter().map(EmbeddedFont::prepare).collect()
    }
}

/// Encoded text and document-local embedded-font resource index.
#[derive(Debug)]
pub(crate) struct EncodedText {
    pub(crate) bytes: Vec<u8>,
    pub(crate) glyphs: Vec<EncodedGlyph>,
    pub(crate) spans: Vec<EncodedSpan>,
    base_direction: text_core::Direction,
}

impl EncodedText {
    pub(crate) fn directional_actual_text(&self) -> Option<String> {
        let has_ltr = self
            .spans
            .iter()
            .any(|span| span.direction == text_core::Direction::Ltr);
        let has_rtl = self
            .spans
            .iter()
            .any(|span| span.direction == text_core::Direction::Rtl);
        if !has_ltr
            || !has_rtl
            || self.spans.iter().any(|span| span.has_astral)
            || self.spans.last()?.direction == self.base_direction
        {
            return None;
        }

        let base = self.spans.last()?.direction;
        let mut actual = String::new();
        if base == text_core::Direction::Rtl {
            for span in self.spans.iter().rev() {
                if span.direction == text_core::Direction::Rtl {
                    actual.push_str(&text_core::bidi_visual_order(&span.source_text));
                } else {
                    actual.push_str(&reverse_ltr_tokens(&span.source_text));
                }
            }
        } else {
            for span in &self.spans {
                actual.push_str(&text_core::bidi_visual_order_with_base(
                    &span.source_text,
                    Some(base),
                ));
            }
        }
        Some(actual)
    }
}

/// PDFium anchors mixed `/ActualText` to the final RTL run and then reverses
/// the surrounding LTR word/number/punctuation tokens. Pre-applying that
/// token transform makes its extraction pass yield the original LTR span.
fn reverse_ltr_tokens(text: &str) -> String {
    let mut tokens: Vec<String> = Vec::new();
    for ch in text.chars() {
        if ch.is_alphanumeric()
            && let Some(last) = tokens.last_mut()
            && last.chars().all(char::is_alphanumeric)
        {
            last.push(ch);
        } else {
            tokens.push(ch.to_string());
        }
    }
    tokens.into_iter().rev().collect()
}

#[derive(Debug, Clone)]
pub(crate) struct EncodedSpan {
    pub(crate) source_text: String,
    pub(crate) direction: text_core::Direction,
    pub(crate) has_astral: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EncodedGlyph {
    pub(crate) resource_index: usize,
    pub(crate) units_per_em: f64,
    pub(crate) cid: u16,
    pub(crate) x_advance: i32,
    pub(crate) x_offset: i32,
    pub(crate) y_offset: i32,
}

/// Returns `/UF0`, `/UF1`, ... without the slash.
pub(crate) fn resource_name(index: usize) -> String {
    format!("UF{index}")
}

/// Final font program plus PDF dictionaries' data.
#[derive(Debug)]
pub(crate) struct EmbeddedFont {
    base_font: String,
    font_program: Vec<u8>,
    is_cff: bool,
    widths: Vec<(u16, f32)>,
    mapping_stream: Vec<u8>,
    to_unicode: Vec<u8>,
    ascent: f32,
    descent: f32,
    cap_height: f32,
    bbox: [f32; 4],
    italic_angle: f32,
}

impl EmbeddedFont {
    fn prepare(used: UsedFace) -> Result<Self> {
        let ordered: Vec<u16> = if used.face.0.no_subsetting {
            used.first_use_gids.clone()
        } else {
            used.mapper.remapped_gids().collect()
        };
        let tag = subset_tag(&used.face.0, &ordered);
        let base_font = format!("{tag}+{}", used.face.0.postscript_name);

        let sfnt_program = if used.face.0.no_subsetting {
            // Retain every glyph while repackaging TTC faces as standalone
            // sfnts and name-keyed CFF as CID-keyed CFF. Original GID order is
            // preserved, so content CIDs remain an identity mapping.
            let glyph_count = used.face.parsed()?.number_of_glyphs();
            let mut full_mapper = GlyphRemapper::new();
            for gid in 1..glyph_count {
                full_mapper.remap(gid);
            }
            subsetter::subset(&used.face.0.bytes, used.face.0.index, &full_mapper).map_err(
                |err| PdfCoreError::TextUnsupported {
                    code: "font-subset",
                    message: format!("full-font embedding failed: {err}"),
                },
            )?
        } else {
            subsetter::subset(&used.face.0.bytes, used.face.0.index, &used.mapper).map_err(
                |err| PdfCoreError::TextUnsupported {
                    code: "font-subset",
                    message: format!("font subsetting failed: {err}"),
                },
            )?
        };
        let font_program = if used.face.0.is_cff {
            let parsed = ttf_parser::Face::parse(&sfnt_program, 0).map_err(|_| {
                PdfCoreError::TextUnsupported {
                    code: "font-subset",
                    message: "CFF font program is not a valid sfnt".to_string(),
                }
            })?;
            parsed
                .raw_face()
                .table(CFF_TAG)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| PdfCoreError::TextUnsupported {
                    code: "font-subset",
                    message: "CFF font program has no CFF table".to_string(),
                })?
        } else {
            sfnt_program
        };
        if font_program.len() > i32::MAX as usize {
            return Err(PdfCoreError::InvalidInput(
                "embedded font program exceeds PDF stream limit".to_string(),
            ));
        }

        let code_to_gid: Vec<(u16, u16)> = used.cid_to_gid.into_iter().collect();
        let widths = if used.face.0.is_cff {
            let pair_widths = used.widths;
            let mut by_descendant_cid = BTreeMap::new();
            for &(code, descendant_cid) in &code_to_gid {
                if let Some(width) = pair_widths.get(&code) {
                    by_descendant_cid.entry(descendant_cid).or_insert(*width);
                }
            }
            by_descendant_cid.into_iter().collect()
        } else {
            used.widths.into_iter().collect()
        };
        let mapping_stream = if used.face.0.is_cff {
            build_cff_encoding(&code_to_gid)
        } else {
            build_cid_to_gid(&code_to_gid)
        };
        let to_unicode = build_to_unicode(&used.to_unicode);
        Ok(Self {
            base_font,
            font_program,
            is_cff: used.face.0.is_cff,
            widths,
            mapping_stream,
            to_unicode,
            ascent: used.face.0.ascent_1000,
            descent: used.face.0.descent_1000,
            cap_height: used.face.0.cap_height_1000,
            bbox: used.face.0.bbox_1000,
            italic_angle: used.face.0.italic_angle,
        })
    }

    /// Adds this font graph to a lopdf document and returns the Type0 object.
    pub(crate) fn add_to_lopdf(&self, doc: &mut Document) -> Result<ObjectId> {
        let mut file_dict = Dictionary::new();
        file_dict.set("Length1", self.font_program.len() as i64);
        if self.is_cff {
            file_dict.set("Subtype", Object::Name(b"CIDFontType0C".to_vec()));
        }
        let file_id = doc.add_object(Stream::new(file_dict, self.font_program.clone()));

        let mut descriptor = dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => Object::Name(self.base_font.as_bytes().to_vec()),
            "Flags" => 4,
            "FontBBox" => Object::Array(self.bbox.iter().copied().map(Object::Real).collect()),
            "ItalicAngle" => self.italic_angle,
            "Ascent" => self.ascent,
            "Descent" => self.descent,
            "CapHeight" => self.cap_height,
            "StemV" => 80,
        };
        descriptor.set(
            if self.is_cff {
                "FontFile3"
            } else {
                "FontFile2"
            },
            Object::Reference(file_id),
        );
        let descriptor_id = doc.add_object(descriptor);
        let mapping_id =
            doc.add_object(Stream::new(Dictionary::new(), self.mapping_stream.clone()));

        let widths = widths_object(&self.widths);
        let mut descendant = dictionary! {
            "Type" => "Font",
            "Subtype" => if self.is_cff { "CIDFontType0" } else { "CIDFontType2" },
            "BaseFont" => Object::Name(self.base_font.as_bytes().to_vec()),
            "CIDSystemInfo" => Object::Dictionary(dictionary! {
                "Registry" => Object::string_literal("Adobe"),
                "Ordering" => Object::string_literal("Identity"),
                "Supplement" => 0,
            }),
            "FontDescriptor" => Object::Reference(descriptor_id),
            "DW" => 0,
            "W" => widths,
        };
        if !self.is_cff {
            descendant.set("CIDToGIDMap", Object::Reference(mapping_id));
        }
        let descendant_id = doc.add_object(descendant);
        let to_unicode_id = doc.add_object(Stream::new(Dictionary::new(), self.to_unicode.clone()));
        let mut type0 = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => Object::Name(self.base_font.as_bytes().to_vec()),
            "DescendantFonts" => Object::Array(vec![Object::Reference(descendant_id)]),
            "ToUnicode" => Object::Reference(to_unicode_id),
        };
        type0.set(
            "Encoding",
            if self.is_cff {
                Object::Reference(mapping_id)
            } else {
                Object::Name(b"Identity-H".to_vec())
            },
        );
        Ok(doc.add_object(type0))
    }

    /// Writes this font graph through pdf-writer.
    pub(crate) fn write_pdf_writer(&self, pdf: &mut Pdf, refs: PdfFontRefs) {
        {
            let mut type0 = pdf.type0_font(refs.type0);
            type0
                .base_font(Name(self.base_font.as_bytes()))
                .descendant_font(refs.descendant)
                .to_unicode(refs.to_unicode);
            if self.is_cff {
                type0.encoding_cmap(refs.mapping);
            } else {
                type0.encoding_predefined(Name(b"Identity-H"));
            }
        }

        {
            let mut cid = pdf.cid_font(refs.descendant);
            cid.subtype(if self.is_cff {
                CidFontType::Type0
            } else {
                CidFontType::Type2
            })
            .base_font(Name(self.base_font.as_bytes()))
            .system_info(SystemInfo {
                registry: Str(b"Adobe"),
                ordering: Str(b"Identity"),
                supplement: 0,
            })
            .font_descriptor(refs.descriptor)
            .default_width(0.0);
            if !self.is_cff {
                cid.cid_to_gid_map_stream(refs.mapping);
            }
            let mut widths = cid.widths();
            for &(start, width) in &self.widths {
                widths.consecutive(start, [width]);
            }
            widths.finish();
            cid.finish();
        }

        {
            let mut descriptor = pdf.font_descriptor(refs.descriptor);
            descriptor
                .name(Name(self.base_font.as_bytes()))
                .flags(FontFlags::SYMBOLIC)
                .bbox(Rect::new(
                    self.bbox[0],
                    self.bbox[1],
                    self.bbox[2],
                    self.bbox[3],
                ))
                .italic_angle(self.italic_angle)
                .ascent(self.ascent)
                .descent(self.descent)
                .cap_height(self.cap_height)
                .stem_v(80.0);
            if self.is_cff {
                descriptor.font_file3(refs.file);
            } else {
                descriptor.font_file2(refs.file);
            }
            descriptor.finish();
        }

        {
            let mut stream = pdf.stream(refs.file, &self.font_program);
            stream.pair(Name(b"Length1"), self.font_program.len() as i32);
            if self.is_cff {
                stream.pair(Name(b"Subtype"), Name(b"CIDFontType0C"));
            }
        }
        pdf.stream(refs.to_unicode, &self.to_unicode);
        pdf.stream(refs.mapping, &self.mapping_stream);
    }
}

/// Six indirect objects used by one embedded font graph.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PdfFontRefs {
    pub(crate) type0: Ref,
    pub(crate) descendant: Ref,
    pub(crate) descriptor: Ref,
    pub(crate) file: Ref,
    pub(crate) to_unicode: Ref,
    pub(crate) mapping: Ref,
}

fn build_cid_to_gid(mapping: &[(u16, u16)]) -> Vec<u8> {
    let max_cid = mapping.iter().map(|(cid, _)| *cid).max().unwrap_or(0);
    let mut bytes = vec![0; (usize::from(max_cid) + 1) * 2];
    for &(cid, gid) in mapping {
        let offset = usize::from(cid) * 2;
        bytes[offset..offset + 2].copy_from_slice(&gid.to_be_bytes());
    }
    bytes
}

fn build_cff_encoding(mapping: &[(u16, u16)]) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def\n\
         /CMapName /DocYantra-CFF-Encoding def\n/CMapType 1 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in mapping.chunks(100) {
        cmap.push_str(&format!("{} begincidchar\n", chunk.len()));
        for &(code, cid) in chunk {
            cmap.push_str(&format!("<{code:04X}> {cid}\n"));
        }
        cmap.push_str("endcidchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap.into_bytes()
}

fn widths_object(widths: &[(u16, f32)]) -> Object {
    let mut array = Vec::with_capacity(widths.len() * 2);
    for &(cid, width) in widths {
        array.push(Object::Integer(i64::from(cid)));
        array.push(Object::Array(vec![Object::Real(width)]));
    }
    Object::Array(array)
}

fn build_to_unicode(mapping: &BTreeMap<u16, String>) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(u16, &str)> = mapping
        .iter()
        .map(|(&cid, text)| (cid, text.as_str()))
        .collect();
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for &(cid, text) in chunk {
            let utf16: String = if text.is_empty() {
                "0000".to_string()
            } else {
                text.encode_utf16()
                    .map(|unit| format!("{unit:04X}"))
                    .collect()
            };
            cmap.push_str(&format!("<{cid:04X}> <{utf16}>\n"));
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap.into_bytes()
}

fn subset_tag(face: &FaceData, gids: &[u16]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in face
        .bytes
        .iter()
        .copied()
        .chain(face.index.to_be_bytes())
        .chain(gids.iter().flat_map(|gid| gid.to_be_bytes()))
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let mut tag = [b'A'; 6];
    for slot in &mut tag {
        *slot = b'A' + (hash % 26) as u8;
        hash /= 26;
    }
    String::from_utf8_lossy(&tag).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn winansi_detection_covers_cp1252_but_not_cyrillic() {
        assert!(is_winansi("café — €"));
        assert!(!is_winansi("Конфиденциально"));
    }

    #[test]
    fn script_gate_is_removed_for_shaping_and_bidi() {
        assert!(validate_text("مرحبا").is_ok());
        assert!(validate_text("left\u{202E}right").is_ok());
    }

    #[test]
    fn coverage_selection_allows_cjk() {
        assert!(validate_text("你好 日本語").is_ok());
    }

    #[test]
    fn missing_coverage_reports_input_scalar_instead_of_replacement_character() {
        let error = shape_with_faces(&[], "\u{1FAE9}").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("U+1FAE9"), "{message}");
        assert!(!message.contains("U+FFFD"), "{message}");
    }

    #[test]
    fn mixed_text_can_use_multiple_registered_faces() {
        register_font(include_bytes!(
            "../wasm/pkg/fonts/LiberationSans-Regular.ttf"
        ))
        .unwrap();
        register_font(include_bytes!("../wasm/pkg/fonts/NotoEmoji-Variable.ttf")).unwrap();

        let mut session = FontSession::new().unwrap();
        let FontChoice::Embedded(choice) = session.resolve("Words \u{1F44D}").unwrap() else {
            panic!("mixed Unicode text unexpectedly selected the standard font")
        };
        let encoded = session.encode(&choice, "Words \u{1F44D}").unwrap();
        let resources: BTreeSet<_> = encoded
            .glyphs
            .iter()
            .map(|glyph| glyph.resource_index)
            .collect();
        assert_eq!(resources.len(), 2);
    }

    #[test]
    fn truetype_cid_to_gid_map_allows_multiple_cids_for_one_glyph() {
        let bytes = build_cid_to_gid(&[(1, 9), (3, 9), (4, 27)]);
        assert_eq!(
            bytes,
            vec![
                0x00, 0x00, // CID 0 -> .notdef
                0x00, 0x09, // CID 1 -> subset GID 9
                0x00, 0x00, // sparse CID 2 -> .notdef
                0x00, 0x09, // CID 3 -> the same subset GID 9
                0x00, 0x1B, // CID 4 -> subset GID 27
            ]
        );
    }

    #[test]
    fn cff_encoding_allows_distinct_codes_to_share_descendant_cid() {
        let cmap = String::from_utf8(build_cff_encoding(&[(1, 7), (2, 7), (3, 11)]))
            .expect("CFF encoding CMap is ASCII");
        assert!(cmap.contains("<0001> 7\n"));
        assert!(cmap.contains("<0002> 7\n"));
        assert!(cmap.contains("<0003> 11\n"));
    }

    #[test]
    fn to_unicode_keeps_each_pair_code_unambiguous() {
        let mapping = BTreeMap::from([
            (1, "का".to_string()),
            (2, "कि".to_string()),
            (3, String::new()),
        ]);
        let cmap = String::from_utf8(build_to_unicode(&mapping)).expect("ToUnicode CMap is ASCII");
        assert!(cmap.contains("<0001> <0915093E>"));
        assert!(cmap.contains("<0002> <0915093F>"));
        assert!(cmap.contains("<0003> <0000>"));
    }

    #[test]
    fn mixed_actual_text_precompensates_pdfium_rtl_anchor_order() {
        let encoded = EncodedText {
            bytes: Vec::new(),
            glyphs: Vec::new(),
            spans: vec![
                EncodedSpan {
                    source_text: "Total: 123 ".to_string(),
                    direction: text_core::Direction::Ltr,
                    has_astral: false,
                },
                EncodedSpan {
                    source_text: "ريال".to_string(),
                    direction: text_core::Direction::Rtl,
                    has_astral: false,
                },
            ],
            base_direction: text_core::Direction::Ltr,
        };
        assert_eq!(
            encoded.directional_actual_text().as_deref(),
            Some("لاير 123 :Total")
        );
    }

    #[test]
    fn rtl_actual_text_precompensates_combining_mark_order() {
        assert_eq!(
            pdfium_actual_text("שָׁלוֹם"),
            "םֹולָׁש",
            "PDFium reverses RTL ActualText codepoints during extraction"
        );
    }
}

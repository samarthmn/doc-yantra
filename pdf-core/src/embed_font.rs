//! Runtime-registered Unicode fonts and per-document CID font embedding.
//!
//! Fonts are never compiled into the engine. Callers register sfnt bytes, then
//! each output document snapshots the registry, selects one covering face per
//! non-WinAnsi run, and builds one subset per used face.

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

    fn gid(&self, ch: char) -> Result<GlyphId> {
        let gid = self.parsed()?.glyph_index(ch).filter(|gid| gid.0 != 0);
        gid.ok_or_else(|| unsupported_glyph(ch))
    }

    fn advance_1000(&self, gid: GlyphId) -> Result<f32> {
        let advance =
            self.parsed()?
                .glyph_hor_advance(gid)
                .ok_or_else(|| PdfCoreError::TextUnsupported {
                    code: "glyph-coverage",
                    message: format!(
                        "registered font has no horizontal advance for GID {}",
                        gid.0
                    ),
                })?;
        Ok((f64::from(advance) * 1000.0 / self.0.units_per_em) as f32)
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
    Embedded(FaceRef),
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
            FontChoice::Embedded(face) => {
                let mut width_1000 = 0.0_f64;
                for ch in text.chars() {
                    width_1000 += f64::from(face.advance_1000(face.gid(ch)?)?);
                }
                Ok(width_1000 * size_pt / 1000.0)
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
            FontChoice::Embedded(face) => {
                Ok(f64::from(face.advance_1000(face.gid(ch)?)?) * size_pt / 1000.0)
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
    for ch in text.chars() {
        let byte = winansi_byte(ch).unwrap_or(b'?');
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

fn winansi_byte(ch: char) -> Option<u8> {
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
pub fn validate_text(text: &str) -> Result<()> {
    if text.chars().any(|ch| {
        let cp = ch as u32;
        is_complex_script(cp) || is_rtl(cp) || is_rtl_control(cp)
    }) {
        return Err(PdfCoreError::TextUnsupported {
            code: "script",
            message: "text contains complex-script or RTL characters".to_string(),
        });
    }
    Ok(())
}

fn is_complex_script(cp: u32) -> bool {
    matches!(cp,
        0x0590..=0x05FF
        | 0x0600..=0x06FF | 0x0750..=0x077F
        | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF
        | 0x0900..=0x0DFF
        | 0x0E00..=0x0EFF
        | 0x1780..=0x17FF
        | 0x1000..=0x109F
    )
}

fn is_rtl(cp: u32) -> bool {
    matches!(cp,
        0x0590..=0x05FF
        | 0x0600..=0x08FF
        | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF
        | 0x10800..=0x10FFF
        | 0x1E800..=0x1E8DF
        | 0x1E900..=0x1E95F
        | 0x1EE00..=0x1EEFF
    )
}

fn is_rtl_control(cp: u32) -> bool {
    matches!(cp, 0x200E..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069)
}

fn unsupported_glyph(ch: char) -> PdfCoreError {
    PdfCoreError::TextUnsupported {
        code: "glyph-coverage",
        message: format!("no registered font covers U+{:04X}", ch as u32),
    }
}

#[derive(Debug)]
struct UsedFace {
    face: FaceRef,
    mapper: GlyphRemapper,
    first_use_gids: Vec<u16>,
    widths: BTreeMap<u16, f32>,
    to_unicode: BTreeMap<u16, char>,
    unicode_by_gid: HashMap<u16, char>,
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
        validate_text(text)?;
        if is_winansi(text) {
            return Ok(FontChoice::Standard);
        }
        for face in &self.registry {
            let parsed = face.parsed()?;
            if text
                .chars()
                .all(|ch| parsed.glyph_index(ch).is_some_and(|gid| gid.0 != 0))
            {
                return Ok(FontChoice::Embedded(face.clone()));
            }
        }

        let missing = text
            .chars()
            .filter(|ch| winansi_byte(*ch).is_none())
            .find(|&ch| {
                !self.registry.iter().any(|face| {
                    face.parsed()
                        .ok()
                        .and_then(|parsed| parsed.glyph_index(ch))
                        .is_some_and(|gid| gid.0 != 0)
                })
            })
            .or_else(|| {
                text.chars().find(|&ch| {
                    !self.registry.iter().any(|face| {
                        face.parsed()
                            .ok()
                            .and_then(|parsed| parsed.glyph_index(ch))
                            .is_some_and(|gid| gid.0 != 0)
                    })
                })
            })
            .unwrap_or('\u{FFFD}');
        Err(unsupported_glyph(missing))
    }

    /// Encodes one embedded run as two-byte big-endian CIDs.
    pub(crate) fn encode(&mut self, face: &FaceRef, text: &str) -> Result<EncodedText> {
        let used_index = match self.used_index.get(&face.id()).copied() {
            Some(index) => index,
            None => {
                let index = self.used.len();
                self.used.push(UsedFace {
                    face: face.clone(),
                    mapper: GlyphRemapper::new(),
                    first_use_gids: vec![0],
                    widths: BTreeMap::new(),
                    to_unicode: BTreeMap::new(),
                    unicode_by_gid: HashMap::new(),
                });
                self.used_index.insert(face.id(), index);
                index
            }
        };

        let used = &mut self.used[used_index];
        let parsed = used.face.parsed()?;
        let mut bytes = Vec::with_capacity(text.chars().count() * 2);
        for ch in text.chars() {
            let gid = parsed
                .glyph_index(ch)
                .filter(|gid| gid.0 != 0)
                .ok_or_else(|| unsupported_glyph(ch))?;
            if let Some(previous) = used.unicode_by_gid.get(&gid.0)
                && *previous != ch
            {
                return Err(PdfCoreError::TextUnsupported {
                    code: "glyph-coverage",
                    message: format!(
                        "font maps U+{:04X} and U+{:04X} to one glyph",
                        *previous as u32, ch as u32
                    ),
                });
            }

            let cid = if used.face.0.no_subsetting {
                if !used.unicode_by_gid.contains_key(&gid.0) {
                    used.first_use_gids.push(gid.0);
                }
                gid.0
            } else {
                let existed = used.mapper.get(gid.0).is_some();
                let cid = used.mapper.remap(gid.0);
                if !existed {
                    used.first_use_gids.push(gid.0);
                }
                cid
            };
            used.unicode_by_gid.entry(gid.0).or_insert(ch);
            used.to_unicode.entry(cid).or_insert(ch);
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
        }

        Ok(EncodedText {
            resource_index: used_index,
            bytes,
        })
    }

    pub(crate) fn finish(self) -> Result<Vec<EmbeddedFont>> {
        self.used.into_iter().map(EmbeddedFont::prepare).collect()
    }
}

/// Encoded text and document-local embedded-font resource index.
#[derive(Debug)]
pub(crate) struct EncodedText {
    pub(crate) resource_index: usize,
    pub(crate) bytes: Vec<u8>,
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

        let widths = used.widths.into_iter().collect();
        let to_unicode = build_to_unicode(&used.to_unicode);
        Ok(Self {
            base_font,
            font_program,
            is_cff: used.face.0.is_cff,
            widths,
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
            descendant.set("CIDToGIDMap", Object::Name(b"Identity".to_vec()));
        }
        let descendant_id = doc.add_object(descendant);
        let to_unicode_id = doc.add_object(Stream::new(Dictionary::new(), self.to_unicode.clone()));
        Ok(doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => Object::Name(self.base_font.as_bytes().to_vec()),
            "Encoding" => "Identity-H",
            "DescendantFonts" => Object::Array(vec![Object::Reference(descendant_id)]),
            "ToUnicode" => Object::Reference(to_unicode_id),
        }))
    }

    /// Writes this font graph through pdf-writer.
    pub(crate) fn write_pdf_writer(&self, pdf: &mut Pdf, refs: PdfFontRefs) {
        pdf.type0_font(refs.type0)
            .base_font(Name(self.base_font.as_bytes()))
            .encoding_predefined(Name(b"Identity-H"))
            .descendant_font(refs.descendant)
            .to_unicode(refs.to_unicode);

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
                cid.cid_to_gid_map_predefined(Name(b"Identity"));
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
    }
}

/// Five indirect objects used by one embedded font graph.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PdfFontRefs {
    pub(crate) type0: Ref,
    pub(crate) descendant: Ref,
    pub(crate) descriptor: Ref,
    pub(crate) file: Ref,
    pub(crate) to_unicode: Ref,
}

fn widths_object(widths: &[(u16, f32)]) -> Object {
    let mut array = Vec::with_capacity(widths.len() * 2);
    for &(cid, width) in widths {
        array.push(Object::Integer(i64::from(cid)));
        array.push(Object::Array(vec![Object::Real(width)]));
    }
    Object::Array(array)
}

fn build_to_unicode(mapping: &BTreeMap<u16, char>) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(u16, char)> = mapping.iter().map(|(&cid, &ch)| (cid, ch)).collect();
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for &(cid, ch) in chunk {
            let mut units = [0; 2];
            let utf16: String = ch
                .encode_utf16(&mut units)
                .iter()
                .map(|unit| format!("{unit:04X}"))
                .collect();
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

    #[test]
    fn winansi_detection_covers_cp1252_but_not_cyrillic() {
        assert!(is_winansi("café — €"));
        assert!(!is_winansi("Конфиденциально"));
    }

    #[test]
    fn script_gate_rejects_arabic_and_rtl_controls() {
        assert!(
            validate_text("مرحبا")
                .unwrap_err()
                .to_string()
                .starts_with("unsupported:script:")
        );
        assert!(
            validate_text("left\u{202E}right")
                .unwrap_err()
                .to_string()
                .starts_with("unsupported:script:")
        );
    }

    #[test]
    fn script_gate_allows_cjk_for_coverage_selection() {
        assert!(validate_text("你好 日本語").is_ok());
    }
}

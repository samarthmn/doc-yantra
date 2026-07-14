//! Zone 1 — metadata, validation, and optimization. Pure lopdf.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use lopdf::{Dictionary, Document, Object, StringFormat};

use crate::error::{PdfCoreError, Result};

/// Five million indirect objects already implies hundreds of MiB of parser
/// state; valid business PDFs remain orders of magnitude below this ceiling.
pub(crate) const MAX_PDF_OBJECTS: usize = 5_000_000;

pub(crate) fn guard_object_count(doc: &Document) -> Result<()> {
    if doc.objects.len() > MAX_PDF_OBJECTS {
        return Err(PdfCoreError::ResourceLimit(format!(
            "PDF object count {} exceeds limit {MAX_PDF_OBJECTS}",
            doc.objects.len()
        )));
    }
    Ok(())
}

/// Result of structural validation.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfInfo {
    pub page_count: u32,
    /// PDF version from the header, e.g. "1.7".
    pub version: String,
    pub encrypted: bool,
}

/// Document information dictionary fields (all optional).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PdfMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
}

/// Returns the number of pages. Errors on invalid/unparseable PDFs.
pub fn page_count(pdf: &[u8]) -> Result<u32> {
    let doc = load_doc(pdf)?;
    ensure_not_encrypted(pdf, &doc, false)?;
    count_pages(&doc)
}

/// Parses and structurally validates `pdf` (must have >= 1 page).
pub fn validate(pdf: &[u8]) -> Result<PdfInfo> {
    let doc = match load_doc(pdf) {
        Ok(doc) => doc,
        Err(PdfCoreError::Lopdf(err)) if is_encryption_error(&err) => {
            return Err(PdfCoreError::PasswordProtected(
                "encrypted or corrupt PDF".to_string(),
            ));
        }
        Err(err) => return Err(err),
    };

    let encrypted = is_encrypted_doc(pdf, &doc);
    let pages = doc.get_pages().len();
    if pages == 0 {
        // Encrypted documents whose objects could not be decrypted (missing or
        // wrong password) load with an empty object table in lopdf.
        if encrypted {
            return Err(PdfCoreError::PasswordProtected(
                "encrypted or corrupt PDF".to_string(),
            ));
        }
        return Err(PdfCoreError::InvalidInput("PDF has zero pages".to_string()));
    }

    Ok(PdfInfo {
        page_count: pages as u32,
        version: doc.version.clone(),
        encrypted,
    })
}

/// Media box sizes (width_pt, height_pt) for each page, in order.
pub fn page_sizes(pdf: &[u8]) -> Result<Vec<(f64, f64)>> {
    let doc = load_doc(pdf)?;
    ensure_not_encrypted(pdf, &doc, false)?;
    count_pages(&doc)?;

    let mut sizes = Vec::new();
    for (page_no, page_id) in doc.get_pages() {
        let media_box = resolve_media_box(&doc, page_id)?.ok_or_else(|| {
            PdfCoreError::InvalidInput(format!("page {page_no} has no MediaBox (own or inherited)"))
        })?;
        sizes.push(media_box);
    }
    Ok(sizes)
}

/// Reads the document information dictionary.
pub fn get_metadata(pdf: &[u8]) -> Result<PdfMetadata> {
    let doc = load_doc(pdf)?;
    ensure_not_encrypted(pdf, &doc, false)?;
    let mut meta = PdfMetadata::default();

    let info = match doc.trailer.get(b"Info") {
        Ok(obj) => match doc.dereference(obj) {
            Ok((_, Object::Dictionary(dict))) => Some(dict),
            _ => None,
        },
        Err(_) => None,
    };

    if let Some(info) = info {
        meta.title = read_text_value(&doc, info, b"Title");
        meta.author = read_text_value(&doc, info, b"Author");
        meta.subject = read_text_value(&doc, info, b"Subject");
        meta.keywords = read_text_value(&doc, info, b"Keywords");
        meta.creator = read_text_value(&doc, info, b"Creator");
        meta.producer = read_text_value(&doc, info, b"Producer");
    }

    Ok(meta)
}

/// Writes the document information dictionary (None fields are removed).
///
/// Errors with [`PdfCoreError::Unsupported`] on encrypted input (any flavor,
/// including owner-locked files with an empty user password): re-saving via
/// lopdf would either destroy the document or silently strip its encryption.
pub fn set_metadata(pdf: &[u8], metadata: &PdfMetadata) -> Result<Vec<u8>> {
    let mut doc = load_doc(pdf)?;
    ensure_never_encrypted(pdf, &doc, true)?;

    // Locate the existing Info dictionary (reference or inline) if any.
    let info_ref = match doc.trailer.get(b"Info") {
        Ok(Object::Reference(id)) => Some(*id),
        _ => None,
    };

    if let Some(id) = info_ref {
        match doc.objects.get_mut(&id) {
            Some(Object::Dictionary(dict)) => apply_metadata(dict, metadata),
            _ => {
                // Dangling or non-dictionary target: replace with a fresh dict.
                let mut dict = Dictionary::new();
                apply_metadata(&mut dict, metadata);
                doc.objects.insert(id, Object::Dictionary(dict));
            }
        }
    } else if matches!(doc.trailer.get(b"Info"), Ok(Object::Dictionary(_))) {
        if let Ok(Object::Dictionary(dict)) = doc.trailer.get_mut(b"Info") {
            apply_metadata(dict, metadata);
        }
    } else {
        let mut dict = Dictionary::new();
        apply_metadata(&mut dict, metadata);
        let id = doc.add_object(Object::Dictionary(dict));
        doc.trailer.set("Info", Object::Reference(id));
    }

    save_doc(&mut doc)
}

/// Recompresses streams and prunes unreferenced objects. Returns bytes that
/// are typically smaller than the input (never corrupted, occasionally equal).
///
/// Errors with [`PdfCoreError::Unsupported`] on encrypted input (any flavor,
/// including owner-locked files with an empty user password): pruning an
/// undecrypted object table would discard every page, and re-saving a
/// transparently-decrypted one would strip its encryption.
pub fn compress(pdf: &[u8]) -> Result<Vec<u8>> {
    let mut doc = load_doc(pdf)?;
    ensure_never_encrypted(pdf, &doc, true)?;
    // Flate-compress stream contents (lopdf only applies the filter when the
    // compressed form is actually smaller).
    doc.compress();
    // Drop objects unreachable from the trailer, then renumber densely.
    doc.prune_objects();
    doc.renumber_objects();
    save_doc(&mut doc)
}

/// Encrypts `pdf` with RC4-128 (standard security handler, V2/R3).
///
/// The output opens with either password; without a password lopdf refuses to
/// expose the page tree.
pub fn encrypt(pdf: &[u8], user_pw: &str, owner_pw: &str) -> Result<Vec<u8>> {
    let mut doc = load_doc(pdf)?;
    if is_encrypted_doc(pdf, &doc) {
        return Err(PdfCoreError::InvalidInput(
            "PDF is already encrypted".to_string(),
        ));
    }
    count_pages(&doc)?;

    // The RC4 key derivation hashes the first element of the trailer /ID
    // array; lopdf's writer never generates one, so synthesize it here.
    if doc.trailer.get(b"ID").is_err() {
        let file_id = derive_file_id(pdf);
        doc.trailer.set(
            "ID",
            Object::Array(vec![
                Object::String(file_id.clone(), StringFormat::Hexadecimal),
                Object::String(file_id, StringFormat::Hexadecimal),
            ]),
        );
    }

    let state = lopdf::EncryptionState::try_from(lopdf::EncryptionVersion::V2 {
        document: &doc,
        owner_password: owner_pw,
        user_password: user_pw,
        key_length: 128,
        permissions: lopdf::Permissions::default(),
    })?;
    doc.encrypt(&state)?;
    save_doc(&mut doc)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Loads `pdf` with lopdf, retrying once on parse failure with the buffer
/// truncated to just after the LAST `%%EOF` marker. Real-world files gain
/// trailing bytes (email gateways, HTTP appends, analytics stamps); lopdf
/// only scans the final ~512 bytes for `startxref` and hard-fails when the
/// junk pushes the marker out of that window, while pdfium/Acrobat tolerate
/// at least 1024 bytes of trailing data.
fn load_doc(pdf: &[u8]) -> Result<Document> {
    let doc = match Document::load_mem(pdf) {
        Ok(doc) => doc,
        Err(err) => {
            if let Some(end) = find_last(pdf, b"%%EOF").map(|pos| pos + b"%%EOF".len())
                && end < pdf.len()
                && let Ok(doc) = Document::load_mem(&pdf[..end])
            {
                guard_object_count(&doc)?;
                return Ok(doc);
            }
            return Err(err.into());
        }
    };
    guard_object_count(&doc)?;
    Ok(doc)
}

/// True when the document is encrypted in any flavor. lopdf transparently
/// decrypts owner-locked files (empty user password) at load time and strips
/// `/Encrypt` from its post-load trailer view, so the post-load state alone
/// is not trustworthy; combine it with a raw-bytes scan of the trailer
/// region(s).
fn is_encrypted_doc(pdf: &[u8], doc: &Document) -> bool {
    doc.is_encrypted() || doc.was_encrypted() || raw_has_encrypt(pdf)
}

/// Rejects password-protected input with a uniform error. lopdf marks a
/// successfully empty-user-password-decrypted document as `was_encrypted`
/// while clearing its live encryption state; that owner-locked case is safe
/// to process and intentionally emits an unencrypted result.
/// `write_op` selects the error
/// flavor: [`PdfCoreError::Unsupported`] for operations that would re-save the
/// document (and thereby corrupt it or strip its protection), otherwise
/// [`PdfCoreError::InvalidInput`] for read-only queries whose results would be
/// misleading (zero pages / all-`None` metadata).
pub(crate) fn ensure_not_encrypted(pdf: &[u8], doc: &Document, write_op: bool) -> Result<()> {
    const MSG: &str = "encrypted PDF; decrypt before processing";
    let empty_user_password_decrypted = doc.was_encrypted() && !doc.is_encrypted();
    if is_encrypted_doc(pdf, doc) && !empty_user_password_decrypted {
        return Err(if write_op {
            PdfCoreError::Unsupported(MSG.to_string())
        } else {
            PdfCoreError::InvalidInput(MSG.to_string())
        });
    }
    Ok(())
}

fn ensure_never_encrypted(pdf: &[u8], doc: &Document, write_op: bool) -> Result<()> {
    const MSG: &str = "encrypted PDF; decrypt before processing";
    if is_encrypted_doc(pdf, doc) {
        return Err(if write_op {
            PdfCoreError::Unsupported(MSG.to_string())
        } else {
            PdfCoreError::InvalidInput(MSG.to_string())
        });
    }
    Ok(())
}

/// Scans the raw bytes for a live `/Encrypt` entry in a trailer dictionary.
/// Handles classic `trailer << ... >>` sections and cross-reference streams
/// (whose trailer dictionary sits at the last `startxref` offset). False
/// positives are conservative (we refuse to process); false negatives are
/// caught by the lopdf-side checks in [`is_encrypted_doc`].
fn raw_has_encrypt(pdf: &[u8]) -> bool {
    const WINDOW: usize = 4096;

    // Classic trailers: check each `trailer` keyword's dictionary region,
    // bounded by the following `startxref` (or WINDOW bytes).
    let mut from = 0;
    while let Some(rel) = find(&pdf[from..], b"trailer") {
        let start = from + rel + b"trailer".len();
        let end = pdf.len().min(start + WINDOW);
        let end = find(&pdf[start..end], b"startxref").map_or(end, |i| start + i);
        if find(&pdf[start..end], b"/Encrypt").is_some() {
            return true;
        }
        from = start;
    }

    // Cross-reference streams: the trailer dictionary is the stream dict at
    // the byte offset recorded after the last `startxref`.
    if let Some(pos) = find_last(pdf, b"startxref") {
        let tail = &pdf[pos + b"startxref".len()..];
        let digits: Vec<u8> = tail
            .iter()
            .copied()
            .skip_while(u8::is_ascii_whitespace)
            .take_while(u8::is_ascii_digit)
            .collect();
        if let Some(offset) = std::str::from_utf8(&digits)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            && offset < pdf.len()
        {
            let region = &pdf[offset..pdf.len().min(offset + WINDOW)];
            let dict_end = find(region, b"stream").unwrap_or(region.len());
            if find(&region[..dict_end], b"/Encrypt").is_some() {
                return true;
            }
        }
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

fn count_pages(doc: &Document) -> Result<u32> {
    let pages = doc.get_pages().len();
    if pages == 0 {
        return Err(PdfCoreError::InvalidInput("PDF has zero pages".to_string()));
    }
    Ok(pages as u32)
}

fn is_encryption_error(err: &lopdf::Error) -> bool {
    matches!(
        err,
        lopdf::Error::InvalidPassword
            | lopdf::Error::Decryption(_)
            | lopdf::Error::AlreadyEncrypted
    )
}

fn save_doc(doc: &mut Document) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    doc.save_to(&mut out)?;
    Ok(out)
}

/// Resolves the /MediaBox for `page_id`, walking the /Parent chain when the
/// page dictionary itself has none (MediaBox is inheritable).
fn resolve_media_box(doc: &Document, page_id: lopdf::ObjectId) -> Result<Option<(f64, f64)>> {
    const MAX_DEPTH: usize = 64; // guards against /Parent cycles
    let mut dict = doc.get_dictionary(page_id)?;
    for _ in 0..MAX_DEPTH {
        if let Ok(obj) = dict.get(b"MediaBox") {
            let (_, resolved) = doc.dereference(obj)?;
            let rect = resolved
                .as_array()
                .map_err(|_| PdfCoreError::InvalidInput("MediaBox is not an array".to_string()))?;
            if rect.len() != 4 {
                return Err(PdfCoreError::InvalidInput(format!(
                    "MediaBox must have 4 numbers, found {}",
                    rect.len()
                )));
            }
            let mut nums = [0.0f64; 4];
            for (i, item) in rect.iter().enumerate() {
                let (_, value) = doc.dereference(item)?;
                nums[i] = value.as_float().map_err(|_| {
                    PdfCoreError::InvalidInput("MediaBox entry is not numeric".to_string())
                })? as f64;
            }
            // Rectangle corners may be written in any order (ISO 32000-1
            // §7.9.5); normalize so width/height are always non-negative.
            return Ok(Some(((nums[2] - nums[0]).abs(), (nums[3] - nums[1]).abs())));
        }
        match dict.get(b"Parent") {
            Ok(parent) => {
                let parent_id = parent.as_reference().map_err(|_| {
                    PdfCoreError::InvalidInput("/Parent is not a reference".to_string())
                })?;
                dict = doc.get_dictionary(parent_id)?;
            }
            Err(_) => return Ok(None),
        }
    }
    Err(PdfCoreError::InvalidInput(
        "page tree /Parent chain too deep (cycle?)".to_string(),
    ))
}

/// Reads a text value from `dict`, dereferencing and decoding tolerantly.
fn read_text_value(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<String> {
    let obj = dict.get(key).ok()?;
    let (_, resolved) = doc.dereference(obj).ok()?;
    match resolved {
        Object::String(bytes, _) => Some(decode_pdf_text(bytes)),
        _ => None,
    }
}

/// Decodes PDF text-string bytes per ISO 32000-1 §7.9.2.2: UTF-16BE when the
/// FE FF BOM is present, UTF-8 when the EF BB BF BOM is present (PDF 2.0),
/// otherwise PDFDocEncoding. BOM-less bytes are NOT UTF-8 — Word/Acrobat/
/// LaTeX write accented Info strings in PDFDocEncoding.
fn decode_pdf_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|c| {
                if c.len() == 2 {
                    u16::from_be_bytes([c[0], c[1]])
                } else {
                    u16::from(c[0])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| pdf_doc_char(b)).collect()
    }
}

/// PDFDocEncoding → Unicode per ISO 32000-1 Annex D: Latin-1 identity except
/// 0x18–0x1F (spacing accents), 0x80–0x9E (punctuation and ligatures), and
/// 0xA0 (Euro sign).
fn pdf_doc_char(b: u8) -> char {
    const ACCENTS: [char; 8] = [
        '\u{02D8}', '\u{02C7}', '\u{02C6}', '\u{02D9}', '\u{02DD}', '\u{02DB}', '\u{02DA}',
        '\u{02DC}',
    ];
    const PUNCT: [char; 31] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}',
        '\u{2044}', '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}',
        '\u{201D}', '\u{2018}', '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}',
        '\u{0141}', '\u{0152}', '\u{0160}', '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}',
        '\u{0153}', '\u{0161}', '\u{017E}',
    ];
    match b {
        0x18..=0x1F => ACCENTS[(b - 0x18) as usize],
        0x80..=0x9E => PUNCT[(b - 0x80) as usize],
        0xA0 => '\u{20AC}',
        _ => b as char, // Latin-1 identity
    }
}

fn apply_metadata(dict: &mut Dictionary, metadata: &PdfMetadata) {
    apply_field(dict, "Title", &metadata.title);
    apply_field(dict, "Author", &metadata.author);
    apply_field(dict, "Subject", &metadata.subject);
    apply_field(dict, "Keywords", &metadata.keywords);
    apply_field(dict, "Creator", &metadata.creator);
    apply_field(dict, "Producer", &metadata.producer);
}

fn apply_field(dict: &mut Dictionary, key: &str, value: &Option<String>) {
    match value {
        Some(text) => dict.set(key, encode_pdf_text(text)),
        None => {
            dict.remove(key.as_bytes());
        }
    }
}

/// Encodes a metadata value as a PDF text string (ISO 32000-1 §7.9.2.2):
/// printable-ASCII values as literal strings (byte-identical under
/// PDFDocEncoding), everything else as UTF-16BE with a FE FF BOM — the form
/// every conforming reader understands. Raw BOM-less UTF-8 is NOT valid and
/// renders as mojibake in Acrobat/Preview/pdfium.
fn encode_pdf_text(text: &str) -> Object {
    let pdf_doc_safe = text
        .bytes()
        .all(|b| matches!(b, b'\t' | b'\n' | b'\r') || (0x20..=0x7E).contains(&b));
    if pdf_doc_safe {
        Object::string_literal(text)
    } else {
        let mut bytes = Vec::with_capacity(2 + text.len() * 2);
        bytes.extend_from_slice(b"\xFE\xFF");
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        Object::String(bytes, StringFormat::Hexadecimal)
    }
}

/// Derives a deterministic 16-byte file identifier from the input bytes.
/// (Not cryptographic — /ID only needs to be a stable unique-ish token.)
fn derive_file_id(pdf: &[u8]) -> Vec<u8> {
    let mut hasher = DefaultHasher::new();
    pdf.hash(&mut hasher);
    let a = hasher.finish();
    (pdf.len() as u64).hash(&mut hasher);
    a.hash(&mut hasher);
    let b = hasher.finish();
    let mut id = Vec::with_capacity(16);
    id.extend_from_slice(&a.to_be_bytes());
    id.extend_from_slice(&b.to_be_bytes());
    id
}

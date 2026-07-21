//! # office-core — DocKosha's pure-Rust Office → PDF engine
//!
//! Own OOXML parsing, own layout, own PDF emission. **No LibreOffice, Chromium,
//! pdfium, or Typst** in the conversion path — only small, permissive, pure-Rust
//! leaf crates. Mirrors `pdf-core`'s conventions (`&[u8]` in / `Vec<u8>` out,
//! `thiserror` errors, doc-comments on every public item).
//!
//! Two contracts anchor everything (see `hyper-agent/office-engine/README.md`):
//!
//! 1. **Fail-closed envelope.** [`convert`] returns
//!    [`OfficeError::Unsupported`] for anything outside the documented envelope,
//!    never a degraded page.
//! 2. **DocProfile.** [`profile`] predicts the same verdict *without* layout so
//!    the router can decide cheaply before converting.
#![deny(missing_docs)]

use std::sync::{LazyLock, RwLock};

#[cfg(feature = "fidelity-diagnostics")]
pub mod diagnostics;
pub mod drawing;
pub mod emit;
pub mod error;
pub mod fonts;
pub mod layout;
pub mod opc;
pub mod output_limit;
pub mod oxml;
pub mod pptx;
pub mod profile;
pub mod resource_limits;
pub mod text;
pub mod xlsx;

/// Fixture builders shared by unit and integration tests (docx zip assembly).
pub mod testkit;

pub use error::{OfficeError, Result, UnsupportedCode};
pub use fonts::{
    EmbeddingPermission, FontExactness, FontExpectation, FontIdentity, FontRequest, FontResolution,
    FontResolutionEvent, FontResolutionSource, FontSlot, SelectedFontIdentity, VariationCoordinate,
};
pub use profile::DocProfile;

#[derive(Clone)]
struct RuntimeFont {
    bytes: Vec<u8>,
    face_index: u32,
    hash_pinned: bool,
    sha256: String,
}

static RUNTIME_FONTS: LazyLock<RwLock<Vec<RuntimeFont>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

fn guard_page_count(count: usize) -> Result<()> {
    if count > resource_limits::MAX_PAGES {
        return Err(OfficeError::unsupported(
            UnsupportedCode::ResourceLimit,
            format!(
                "page emission count {count} exceeds limit {}",
                resource_limits::MAX_PAGES
            ),
        ));
    }
    Ok(())
}

/// Registers the first face in an SFNT or collection for subsequent conversions
/// and profiles in this process. Use [`register_font_validated`] with a
/// [`FontExpectation::face_index`] to select a different collection face.
pub fn register_font(bytes: &[u8]) -> Result<()> {
    register_font_validated(bytes, None).map(|_| ())
}

/// Validates and registers one SFNT/OTF/TTC face for subsequent profiles and
/// conversions. The registry changes only after every identity, digest,
/// coverage, variation-axis, and embedding-policy check succeeds.
pub fn register_font_validated(
    bytes: &[u8],
    expectation: Option<FontExpectation>,
) -> Result<FontIdentity> {
    let identity = fonts::validate_font(bytes, expectation.as_ref())?;
    let hash_pinned = expectation
        .as_ref()
        .is_some_and(|expectation| expectation.sha256.is_some());
    let mut registry = RUNTIME_FONTS
        .write()
        .map_err(|_| OfficeError::Internal("font registry lock is poisoned".into()))?;
    if let Some(existing) = registry
        .iter_mut()
        .find(|font| font.sha256 == identity.sha256 && font.face_index == identity.face_index)
    {
        existing.hash_pinned |= hash_pinned;
        return Ok(identity);
    }
    registry.push(RuntimeFont {
        bytes: bytes.to_vec(),
        face_index: identity.face_index,
        hash_pinned,
        sha256: identity.sha256.clone(),
    });
    Ok(identity)
}

fn effective_options(opts: &ConvertOptions) -> ConvertOptions {
    opts.clone()
}

pub(crate) fn register_runtime_fonts(db: &mut fonts::FontDb) -> Result<()> {
    let registry = RUNTIME_FONTS
        .read()
        .map_err(|_| OfficeError::Internal("font registry lock is poisoned".into()))?;
    for font in registry.iter() {
        db.register_face(font.bytes.clone(), font.face_index)?;
    }
    let pinned = registry
        .iter()
        .filter(|font| font.hash_pinned)
        .map(|font| font.sha256.clone())
        .collect::<Vec<_>>();
    db.mark_hash_pinned(&pinned);
    Ok(())
}

/// The Office document formats the engine understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeFormat {
    /// WordprocessingML (`.docx`).
    Docx,
    /// PresentationML (`.pptx`).
    Pptx,
    /// SpreadsheetML (`.xlsx` / `.xlsm`).
    Xlsx,
}

impl OfficeFormat {
    /// Maps a lowercase file extension to a format. `xlsm` is [`OfficeFormat::Xlsx`]
    /// (macros never render). Returns `None` for anything else (legacy binary
    /// formats included — those are the router's concern, not the engine's).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "docx" => Some(OfficeFormat::Docx),
            "pptx" => Some(OfficeFormat::Pptx),
            "xlsx" | "xlsm" => Some(OfficeFormat::Xlsx),
            _ => None,
        }
    }
}

/// Optional inputs to [`convert`].
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    /// Extra font directories to scan (native only; ignored on wasm).
    pub font_dirs: Vec<std::path::PathBuf>,
    /// Raw font files to register (brand fonts; the wasm `register_font` path).
    pub font_bytes: Vec<Vec<u8>>,
    /// SHA-256 digests whose registered identities were caller-pinned. Ordinary
    /// consumers use [`register_font_validated`]; this field preserves metadata
    /// when hosts construct per-call font sets directly.
    pub font_hash_pins: Vec<String>,
    /// Whether native conversion may scan OS-managed font directories. Defaults
    /// to false; explicit `font_dirs` remain caller-authorized inputs.
    pub include_system_fonts: bool,
    /// Exact-only or compatible font resolution policy.
    pub resolution_mode: ResolutionMode,
    /// The original input file name, used by fields such as XLSX `&F`.
    pub source_name: Option<String>,
    /// Restrict XLSX conversion to these sheet names (by exact name). `None` = all.
    pub sheet_filter: Option<Vec<String>>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        ConvertOptions {
            font_dirs: Vec::new(),
            font_bytes: Vec::new(),
            font_hash_pins: Vec::new(),
            include_system_fonts: false,
            resolution_mode: ResolutionMode::Compatible,
            source_name: None,
            sheet_filter: None,
        }
    }
}

/// Font-resolution behavior after embedded and runtime-registered exact faces
/// have been considered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionMode {
    /// Exact embedded/registered faces only; unresolved faces fail closed.
    Strict,
    /// Allow the documented substitution and family-class fallback ladder.
    #[default]
    Compatible,
}

/// The successful result of [`convert`].
#[derive(Debug)]
pub struct ConvertOutput {
    /// The emitted PDF bytes.
    pub pdf: Vec<u8>,
    /// Number of pages in the emitted PDF (from layout, never re-parsed).
    pub page_count: u32,
    /// Non-fatal, documented-lossy warnings accumulated during conversion.
    pub warnings: Vec<profile::Warning>,
    /// Font resolution telemetry, byte-for-byte equivalent to profile state
    /// under the same registry and options.
    pub fonts: Vec<FontResolution>,
    /// Feature-gated DOCX line trace used only by fidelity diagnostics.
    #[cfg(feature = "fidelity-diagnostics")]
    pub diagnostics: Option<diagnostics::LayoutTrace>,
}

/// Convert an Office document to PDF.
///
/// Fail-closed: anything outside the documented envelope returns
/// [`OfficeError::Unsupported`] (the same verdict [`profile`] predicts). `format`
/// is the caller's declared format; the container is still validated.
pub fn convert(bytes: &[u8], format: OfficeFormat, opts: &ConvertOptions) -> Result<ConvertOutput> {
    let effective = effective_options(opts);
    match format {
        OfficeFormat::Docx => convert_docx(bytes, &effective),
        OfficeFormat::Pptx => convert_pptx(bytes, &effective),
        OfficeFormat::Xlsx => convert_xlsx(bytes, &effective),
    }
}

/// Converts a `.xlsx`/`.xlsm`: enforce the envelope, then render + emit.
fn convert_xlsx(bytes: &[u8], opts: &ConvertOptions) -> Result<ConvertOutput> {
    let prof = profile::run_with_options(bytes, OfficeFormat::Xlsx, opts)?;
    if !prof.supported {
        let (code, detail) = prof
            .reasons
            .first()
            .map(|r| (r.code, r.detail.clone()))
            .unwrap_or((
                UnsupportedCode::UnknownElement,
                "unsupported document".into(),
            ));
        return Err(OfficeError::unsupported(code, detail));
    }
    let doc = xlsx::XlsxDoc::open(bytes, opts)?;
    let (laid, mut warnings) = xlsx::render_xlsx(&doc)?;
    guard_page_count(laid.pages.len())?;
    let page_count = laid.pages.len() as u32;
    let pdf = emit::pdf::emit(&laid, &doc.fonts, document_id(bytes), &mut warnings)?;
    Ok(ConvertOutput {
        pdf,
        page_count,
        warnings,
        fonts: prof.fonts,
        #[cfg(feature = "fidelity-diagnostics")]
        diagnostics: None,
    })
}

/// Converts a `.pptx`: enforce the envelope, then render + emit.
fn convert_pptx(bytes: &[u8], opts: &ConvertOptions) -> Result<ConvertOutput> {
    let prof = profile::run_with_options(bytes, OfficeFormat::Pptx, opts)?;
    if !prof.supported {
        let (code, detail) = prof
            .reasons
            .first()
            .map(|r| (r.code, r.detail.clone()))
            .unwrap_or((
                UnsupportedCode::UnknownElement,
                "unsupported document".into(),
            ));
        return Err(OfficeError::unsupported(code, detail));
    }
    let doc = pptx::PptxDoc::open(bytes, opts)?;
    let (laid, mut warnings) = pptx::render_pptx(&doc)?;
    guard_page_count(laid.pages.len())?;
    let page_count = laid.pages.len() as u32;
    let pdf = emit::pdf::emit(&laid, &doc.fonts, document_id(bytes), &mut warnings)?;
    Ok(ConvertOutput {
        pdf,
        page_count,
        warnings,
        fonts: prof.fonts,
        #[cfg(feature = "fidelity-diagnostics")]
        diagnostics: None,
    })
}

/// Converts a `.docx`: enforce the envelope (via [`profile`]), then layout + emit.
fn convert_docx(bytes: &[u8], opts: &ConvertOptions) -> Result<ConvertOutput> {
    // Fail closed: the profile is the single source of the envelope verdict.
    let prof = profile::run_with_options(bytes, OfficeFormat::Docx, opts)?;
    if !prof.supported {
        let (code, detail) = prof
            .reasons
            .first()
            .map(|r| (r.code, r.detail.clone()))
            .unwrap_or((
                UnsupportedCode::UnknownElement,
                "unsupported document".into(),
            ));
        return Err(OfficeError::unsupported(code, detail));
    }

    let doc = layout::WordDoc::open(bytes, opts)?;
    let (laid, mut warnings, internal_links) = layout::flow::layout_docx_with_internal_links(&doc)?;
    guard_page_count(laid.pages.len())?;
    let page_count = laid.pages.len() as u32;
    #[cfg(feature = "fidelity-diagnostics")]
    let diagnostics = laid.diagnostics.clone();
    let pdf = emit::pdf::emit_with_internal_links(
        &laid,
        &doc.fonts,
        document_id(bytes),
        &mut warnings,
        &internal_links,
    )?;
    Ok(ConvertOutput {
        pdf,
        page_count,
        warnings,
        fonts: prof.fonts,
        #[cfg(feature = "fidelity-diagnostics")]
        diagnostics: Some(diagnostics),
    })
}

/// Stable PDF-ID schema. Package patch versions are deliberately excluded so a
/// release-only version bump does not alter otherwise identical PDF bytes.
const PDF_DOCUMENT_ID_SCHEMA: &[u8] = b"office-core-pdf-id-v1";

/// Deterministic 16-byte document id: first half of `SHA-256(input ‖ schema)`.
fn document_id(bytes: &[u8]) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.update(PDF_DOCUMENT_ID_SCHEMA);
    let digest = hasher.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

/// Cheap envelope check *without* layout — the router contract.
///
/// See the plan README for the DocProfile JSON schema. Never returns `Err` for a
/// well-formed file (reasons carry the verdict); `Err` is reserved for
/// container-level failures (invalid/encrypted/legacy), which the wasm boundary
/// maps to JSON via [`DocProfile::from_error`].
pub fn profile(bytes: &[u8], format: OfficeFormat) -> Result<DocProfile> {
    profile_with_options(bytes, format, &ConvertOptions::default())
}

/// Cheap envelope check using explicit font-resolution and host-access options.
pub fn profile_with_options(
    bytes: &[u8],
    format: OfficeFormat,
    opts: &ConvertOptions,
) -> Result<DocProfile> {
    let opts = effective_options(opts);
    profile::run_with_options(bytes, format, &opts)
}

/// Performs the layout-free profile walk and returns deduplicated, canonical
/// font metadata for host resolution. Document text is never included, and
/// exact document-embedded or already-registered faces are omitted.
pub fn font_requests(
    bytes: &[u8],
    format: OfficeFormat,
    opts: &ConvertOptions,
) -> Result<Vec<FontRequest>> {
    let opts = effective_options(opts);
    profile::run_with_options(bytes, format, &opts).map(|profile| profile.font_requests)
}

#[cfg(test)]
mod runtime_font_tests {
    use super::*;

    #[test]
    fn document_id_is_stable_across_package_patch_releases() {
        assert_eq!(
            document_id(b"release-stable"),
            [
                0xb3, 0xfa, 0x4a, 0x13, 0xaf, 0x5c, 0x4e, 0x9c, 0xeb, 0x6d, 0x4a, 0x64, 0xc1, 0x08,
                0xd1, 0xa8,
            ]
        );
    }

    #[test]
    fn public_runtime_font_registration_validates_bytes() {
        assert!(register_font(b"not a font").is_err());
        register_font(include_bytes!(
            "../fonts/liberation/LiberationSans-Regular.ttf"
        ))
        .expect("register a valid runtime face");
    }

    #[test]
    fn final_page_guard_uses_shared_resource_limit() {
        let error = guard_page_count(resource_limits::MAX_PAGES + 1).unwrap_err();
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ));
    }
}

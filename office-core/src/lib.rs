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

use std::sync::{Arc, LazyLock, RwLock};

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
    bytes: Arc<Vec<u8>>,
    face_index: u32,
    hash_pinned: bool,
    sha256: String,
}

// The supported host boot set is six ~16.5 MiB pan-CJK/Symbols faces plus the
// much smaller metric-compatible Office families. 128 MiB admits that reviewed
// set while excluding a Wasm working set that the package cannot safely serve.
const MAX_RUNTIME_FONT_BYTES: usize = 128 * 1024 * 1024;
// 256 selected faces leaves ample room for all supported weights/styles while
// keeping every profile/conversion's matching scan bounded.
const MAX_RUNTIME_FONT_FACES: usize = 256;

struct RuntimeFontRegistry {
    fonts: Vec<RuntimeFont>,
    aggregate_bytes: usize,
    max_faces: usize,
    max_bytes: usize,
}

impl RuntimeFontRegistry {
    const fn with_limits(max_faces: usize, max_bytes: usize) -> Self {
        Self {
            fonts: Vec::new(),
            aggregate_bytes: 0,
            max_faces,
            max_bytes,
        }
    }

    const fn production() -> Self {
        Self::with_limits(MAX_RUNTIME_FONT_FACES, MAX_RUNTIME_FONT_BYTES)
    }

    fn insert(
        &mut self,
        bytes: &[u8],
        face_index: u32,
        hash_pinned: bool,
        sha256: &str,
    ) -> Result<bool> {
        let insert_at = match self.fonts.binary_search_by(|font| {
            font.sha256
                .as_str()
                .cmp(sha256)
                .then_with(|| font.face_index.cmp(&face_index))
        }) {
            Ok(existing) => {
                self.fonts[existing].hash_pinned |= hash_pinned;
                return Ok(false);
            }
            Err(insert_at) => insert_at,
        };

        let next_face_count =
            resource_limits::checked_count_add("runtime font faces", self.fonts.len(), 1)?;
        resource_limits::ensure_cardinality("runtime font faces", next_face_count, self.max_faces)?;

        let shared_bytes = self
            .fonts
            .iter()
            .find(|font| font.sha256 == sha256)
            .map(|font| font.bytes.clone());
        let next_aggregate_bytes = if shared_bytes.is_some() {
            self.aggregate_bytes
        } else {
            let next = resource_limits::checked_bytes_add(
                "runtime fonts",
                self.aggregate_bytes,
                bytes.len(),
            )?;
            if next > self.max_bytes {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::ResourceLimit,
                    format!("runtime font bytes {next} exceeds limit {}", self.max_bytes),
                ));
            }
            next
        };

        resource_limits::try_reserve(&mut self.fonts, 1, "runtime font registry")?;
        let stored_bytes = match shared_bytes {
            Some(bytes) => bytes,
            None => {
                let mut owned = Vec::new();
                resource_limits::try_reserve(&mut owned, bytes.len(), "runtime font bytes")?;
                owned.extend_from_slice(bytes);
                Arc::new(owned)
            }
        };
        self.fonts.insert(
            insert_at,
            RuntimeFont {
                bytes: stored_bytes,
                face_index,
                hash_pinned,
                sha256: sha256.to_owned(),
            },
        );
        self.aggregate_bytes = next_aggregate_bytes;
        Ok(true)
    }
}

static RUNTIME_FONTS: LazyLock<RwLock<RuntimeFontRegistry>> =
    LazyLock::new(|| RwLock::new(RuntimeFontRegistry::production()));

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
    registry.insert(bytes, identity.face_index, hash_pinned, &identity.sha256)?;
    Ok(identity)
}

fn effective_options(opts: &ConvertOptions) -> ConvertOptions {
    opts.clone()
}

pub(crate) fn register_runtime_fonts(db: &mut fonts::FontDb) -> Result<()> {
    let registry = RUNTIME_FONTS
        .read()
        .map_err(|_| OfficeError::Internal("font registry lock is poisoned".into()))?;
    for font in &registry.fonts {
        db.register_shared_face(font.bytes.clone(), font.face_index)?;
    }
    let pinned = registry
        .fonts
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

    fn assert_resource_limit(error: OfficeError) {
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ));
    }

    #[test]
    fn local_runtime_font_registry_accepts_exact_face_and_byte_boundaries() {
        let mut registry = RuntimeFontRegistry::with_limits(2, 5);

        assert!(
            registry
                .insert(b"ab", 0, false, "first")
                .expect("first face")
        );
        assert!(
            registry
                .insert(b"cde", 0, false, "second")
                .expect("second face at exact limits")
        );

        assert_eq!(registry.fonts.len(), 2);
        assert_eq!(registry.aggregate_bytes, 5);
    }

    #[test]
    fn local_runtime_font_registry_refuses_one_face_over_limit_without_mutation() {
        let mut registry = RuntimeFontRegistry::with_limits(1, 8);
        registry.insert(b"abc", 0, false, "first").unwrap();

        let error = registry
            .insert(b"def", 0, false, "second")
            .expect_err("second face must exceed the face budget");

        assert_resource_limit(error);
        assert_eq!(registry.fonts.len(), 1);
        assert_eq!(registry.aggregate_bytes, 3);
    }

    #[test]
    fn local_runtime_font_registry_refuses_one_byte_over_limit_without_mutation() {
        let mut registry = RuntimeFontRegistry::with_limits(2, 5);
        registry.insert(b"abcd", 0, false, "first").unwrap();

        let error = registry
            .insert(b"ef", 0, false, "second")
            .expect_err("aggregate bytes must not exceed the byte budget");

        assert_resource_limit(error);
        assert_eq!(registry.fonts.len(), 1);
        assert_eq!(registry.aggregate_bytes, 4);
    }

    #[test]
    fn local_runtime_font_registry_maps_byte_accounting_overflow_to_resource_limit() {
        let mut registry = RuntimeFontRegistry::with_limits(2, usize::MAX);
        registry.aggregate_bytes = usize::MAX;

        let error = registry
            .insert(b"x", 0, false, "overflow")
            .expect_err("checked byte addition must refuse overflow");

        assert_resource_limit(error);
        assert!(registry.fonts.is_empty());
        assert_eq!(registry.aggregate_bytes, usize::MAX);
    }

    #[test]
    fn local_runtime_font_registry_deduplicates_before_charging_or_copying() {
        let mut registry = RuntimeFontRegistry::with_limits(1, 3);
        assert!(registry.insert(b"abc", 0, false, "same").unwrap());
        let original_bytes = registry.fonts[0].bytes.clone();

        assert!(!registry.insert(b"abc", 0, true, "same").unwrap());

        assert_eq!(registry.fonts.len(), 1);
        assert_eq!(registry.aggregate_bytes, 3);
        assert!(registry.fonts[0].hash_pinned);
        assert!(std::sync::Arc::ptr_eq(
            &original_bytes,
            &registry.fonts[0].bytes
        ));
    }

    #[test]
    fn local_runtime_font_registry_order_is_canonical() {
        let mut registry = RuntimeFontRegistry::with_limits(3, 3);
        registry.insert(b"z", 0, false, "zeta").unwrap();
        registry.insert(b"a", 2, false, "alpha").unwrap();
        registry.insert(b"a", 0, false, "alpha").unwrap();

        let identities = registry
            .fonts
            .iter()
            .map(|font| (font.sha256.as_str(), font.face_index))
            .collect::<Vec<_>>();

        // FontDb assigns FaceId in registration order, and the PDF emitter uses
        // FaceId order for font resources. Canonical registry order therefore
        // keeps identical inputs byte-stable after unrelated warm-up converts.
        assert_eq!(identities, vec![("alpha", 0), ("alpha", 2), ("zeta", 0)]);
    }

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
            "../../packages/doc-yantra-fonts/fonts/LiberationSans-Regular.ttf"
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

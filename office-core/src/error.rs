//! Error and result types for the office-core engine.
//!
//! The engine is **fail-closed**: any construct outside the documented envelope
//! (see `hyper-agent/office-engine/README.md`) surfaces as
//! [`OfficeError::Unsupported`] carrying a stable [`UnsupportedCode`], never a
//! silently-degraded page. `profile()` predicts the same verdict cheaply so the
//! router can decide before converting.

/// Stable machine-readable reason codes for out-of-envelope constructs.
///
/// These serialize to `snake_case` strings that appear in the DocProfile JSON
/// `reasons[].code` field and in `Err(Unsupported)` messages. They are an API:
/// **add variants, never rename** existing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedCode {
    /// A recognized format whose converter is unavailable in this engine build.
    FormatNotImplemented,
    /// A legacy binary format (`.doc`/`.ppt`/`.xls`, CFB container).
    LegacyFormat,
    /// The package is encrypted (`EncryptedPackage` part).
    Encrypted,
    /// Text uses a complex script (Arabic/Indic/Thai/…) needing shaping we do not do v1.
    ComplexScript,
    /// Right-to-left text (Hebrew/Arabic); no bidi reordering v1.
    RtlText,
    /// Footnotes are present.
    Footnotes,
    /// Endnotes are present.
    Endnotes,
    /// A text box / framed paragraph (`w:framePr`).
    TextBox,
    /// A floating/anchored drawing (`wp:anchor`).
    AnchoredDrawing,
    /// A floating table (`w:tblpPr`).
    FloatingTable,
    /// A multi-column section.
    MultiColumn,
    /// A chart (`c:chart`).
    Chart,
    /// SmartArt / a diagram.
    SmartArt,
    /// An embedded OLE object or media.
    OleObject,
    /// An OLE preview exists only as unsupported EMF vector content.
    OlePreviewEmf,
    /// An OLE preview exists only as unsupported WMF vector content.
    OlePreviewWmf,
    /// WordArt / preset text warp.
    WordArt,
    /// A shape preset, custom-path command, or guide formula outside the
    /// implemented DrawingML geometry subset.
    ShapeGeometry,
    /// An equation (`m:oMath`).
    Equation,
    /// A field instruction we neither evaluate nor have a cached result for.
    FieldUnsupported,
    /// Automatic hyphenation is enabled in settings.
    AutoHyphenation,
    /// An image with a source crop (`a:srcRect`).
    ImageCrop,
    /// A hyperlink target outside the allowed URL schemes or slide destinations.
    Hyperlink,
    /// A DrawingML fill preset this engine cannot render faithfully.
    PatternFill,
    /// An authored DrawingML fill is malformed or has no resolvable color.
    InvalidFill,
    /// A DrawingML gradient is malformed, degenerate, or uses an unsupported path mode.
    GradientFill,
    /// A DrawingML picture fill has an invalid tile/stretch placement contract.
    InvalidPictureFill,
    /// A visible image is relationship-linked to an external resource. The
    /// in-process engine never fetches network assets.
    ExternalImage,
    /// Conditional formatting whose rules we cannot evaluate.
    ConditionalFormatting,
    /// A formula cell has no usable cached result. The engine never evaluates
    /// formulas or guesses their displayed value.
    FormulaCacheMissing,
    /// A visible object references a relationship id or internal target part
    /// that does not exist in the package.
    MissingRelationshipTarget,
    /// A required image declares or contains an encoding outside the PNG/JPEG
    /// raster envelope.
    UnsupportedImageEncoding,
    /// A required PNG/JPEG part is corrupt or cannot be decoded completely.
    ImageDecodeFailure,
    /// A required image has no OPC content type, so its encoding cannot be
    /// validated against the package declaration.
    ImageContentTypeMissing,
    /// Decoded image dimensions exceed the per-image pixel/allocation limit.
    ImageDimensionsExceedLimit,
    /// Aggregate image count or decoded allocation exceeds document limits.
    ImageAllocationExceedsLimit,
    /// DrawingML source-crop values are malformed or remove the full image.
    InvalidImageCrop,
    /// A visible PPTX table style is missing or uses constructs outside the
    /// faithfully implemented table-style cascade.
    TableStyle,
    /// Autofit text that would need Word's iterative recompute.
    NeedsAutofitRecompute,
    /// An element (or number format / symbol) outside the enumerated envelope.
    UnknownElement,
    /// No available font face maps one or more required Unicode code points.
    MissingGlyph,
    /// One or more requested font faces could not be resolved under the active
    /// strict/compatible policy.
    FontUnresolved,
    /// A supplied font's name/style metadata does not match the requested face.
    FontIdentityMismatch,
    /// A supplied font does not match its caller-pinned SHA-256 digest.
    FontHashMismatch,
    /// A supplied font does not cover a required script or code point.
    FontCoverageMismatch,
    /// A supplied font's embedding permission forbids safe PDF embedding.
    FontLicenseRestricted,
    /// A supplied font format or collection face is outside the SFNT contract.
    FontFormatUnsupported,
    /// The input exceeds the in-process size guard.
    InputTooLarge,
    /// The package decompresses past the entry/part/total budgets. A local
    /// envelope miss to route to the compatibility service — legitimate large
    /// documents can trip it, so it must never read as file corruption.
    DecompressionLimit,
    /// Valid document structure or generated work exceeds an in-process
    /// cardinality, byte, or page budget.
    ResourceLimit,
    /// The serialized conversion output exceeds the engine output budget.
    /// The input was valid; a resource refusal, never file corruption.
    OutputLimit,
    /// The bytes are not a valid OOXML package.
    InvalidFile,
}

/// Unified error type for all office-core operations.
#[derive(Debug, thiserror::Error)]
pub enum OfficeError {
    /// A construct outside the documented, fail-closed envelope.
    #[error("unsupported: {code:?}: {detail}")]
    Unsupported {
        /// The stable reason code.
        code: UnsupportedCode,
        /// Human-readable detail (e.g. the offending element name).
        detail: String,
    },
    /// The input is malformed (not a zip, corrupt XML). Decompression-budget
    /// hits are `Unsupported(DecompressionLimit)`, not `Invalid` — they are an
    /// envelope miss, not corruption.
    #[error("invalid input: {0}")]
    Invalid(String),
    /// A font could not be parsed or resolved.
    #[error("font error: {0}")]
    Font(String),
    /// An internal invariant was violated (a bug).
    #[error("internal: {0}")]
    Internal(String),
}

impl OfficeError {
    /// Convenience constructor for [`OfficeError::Unsupported`].
    pub fn unsupported(code: UnsupportedCode, detail: impl Into<String>) -> Self {
        OfficeError::Unsupported {
            code,
            detail: detail.into(),
        }
    }

    /// Adds part/slide/sheet context without erasing the stable error class.
    pub(crate) fn context(self, prefix: impl std::fmt::Display) -> Self {
        match self {
            OfficeError::Unsupported { code, detail } => OfficeError::Unsupported {
                code,
                detail: format!("{prefix}: {detail}"),
            },
            OfficeError::Invalid(detail) => OfficeError::Invalid(format!("{prefix}: {detail}")),
            OfficeError::Font(detail) => OfficeError::Font(format!("{prefix}: {detail}")),
            OfficeError::Internal(detail) => OfficeError::Internal(format!("{prefix}: {detail}")),
        }
    }
}

/// The crate's result alias.
pub type Result<T> = std::result::Result<T, OfficeError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_limit_has_stable_snake_case_slug() {
        assert_eq!(
            serde_json::to_string(&UnsupportedCode::ResourceLimit).unwrap(),
            r#""resource_limit""#,
        );
    }
}

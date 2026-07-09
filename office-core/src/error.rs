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
    /// The format's converter is not implemented yet (pptx/xlsx before their tasks land).
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
    /// WordArt / preset text warp.
    WordArt,
    /// A non-rectangular shape geometry (`prstGeom` other than `rect`, or
    /// `custGeom`). v1 renders shape fills against their bounding box, so any
    /// non-rect geometry would draw wrong; fail closed rather than degrade.
    ShapeGeometry,
    /// An equation (`m:oMath`).
    Equation,
    /// A field instruction we neither evaluate nor have a cached result for.
    FieldUnsupported,
    /// Automatic hyphenation is enabled in settings.
    AutoHyphenation,
    /// An image with a source crop (`a:srcRect`).
    ImageCrop,
    /// Conditional formatting whose rules we cannot evaluate.
    ConditionalFormatting,
    /// Autofit text that would need Word's iterative recompute.
    NeedsAutofitRecompute,
    /// An element (or number format / symbol) outside the enumerated envelope.
    UnknownElement,
    /// The input exceeds the in-process size guard.
    InputTooLarge,
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
    /// The input is malformed (not a zip, corrupt XML, zip-bomb guard tripped).
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
}

/// The crate's result alias.
pub type Result<T> = std::result::Result<T, OfficeError>;

use thiserror::Error;

/// Unified error type for all pdf-core operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PdfCoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("PDF structure error: {0}")]
    Lopdf(#[from] lopdf::Error),

    #[error("pdfium error: {0}")]
    Pdfium(String),

    #[error("image error: {0}")]
    Image(#[from] image::ImageError),

    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("unsupported operation: {0}")]
    Unsupported(String),

    /// Machine-readable fail-closed text/font rejection.
    #[error("unsupported:{code}: {message}")]
    TextUnsupported { code: &'static str, message: String },

    #[error("encoding error: {0}")]
    Encoding(String),

    /// The document is encrypted with a password we cannot satisfy. Carries the
    /// complete legacy message so existing message-matching callers are stable.
    #[error("{0}")]
    PasswordProtected(String),

    /// A structural or allocation budget was exceeded (page counts, cell
    /// counts, pixel counts, grid anchors). Carries the complete legacy message.
    #[error("{0}")]
    ResourceLimit(String),

    /// The engine produced output that failed its own post-write validation.
    /// Carries the complete legacy message.
    #[error("{0}")]
    OutputValidation(String),
}

impl PdfCoreError {
    /// The stable machine code for this error, used verbatim across the wasm
    /// boundary (`{"dy":1,"code":…}` envelope). These are an API: add codes,
    /// never rename. Downstream consumers must branch on THIS, never on
    /// message prose.
    pub fn stable_code(&self) -> &'static str {
        match self {
            PdfCoreError::Io(_) | PdfCoreError::Pdfium(_) | PdfCoreError::Encoding(_) => {
                "internal_error"
            }
            // lopdf decryption failures are a password problem, not corruption.
            PdfCoreError::Lopdf(
                lopdf::Error::InvalidPassword
                | lopdf::Error::Decryption(_)
                | lopdf::Error::AlreadyEncrypted,
            ) => "password_protected",
            PdfCoreError::Lopdf(_) => "malformed_container",
            PdfCoreError::Image(_) | PdfCoreError::Csv(_) | PdfCoreError::InvalidInput(_) => {
                "invalid_input"
            }
            PdfCoreError::Unsupported(_) => "unsupported_feature",
            PdfCoreError::TextUnsupported { code, .. } => match *code {
                "glyph-coverage" => "missing_glyph",
                _ => "unsupported_feature",
            },
            PdfCoreError::PasswordProtected(_) => "password_protected",
            PdfCoreError::ResourceLimit(_) => "resource_limit",
            PdfCoreError::OutputValidation(_) => "invalid_output",
        }
    }

    /// The crate-internal machine sub-code, when one exists (e.g. the
    /// `TextUnsupported` code). Travels in the envelope `detail` field.
    pub fn machine_detail(&self) -> Option<&'static str> {
        match self {
            PdfCoreError::TextUnsupported { code, .. } => Some(code),
            _ => None,
        }
    }
}

#[cfg(feature = "pdfium")]
impl From<pdfium_render::prelude::PdfiumError> for PdfCoreError {
    fn from(err: pdfium_render::prelude::PdfiumError) -> Self {
        PdfCoreError::Pdfium(format!("{err:?}"))
    }
}

pub type Result<T> = std::result::Result<T, PdfCoreError>;

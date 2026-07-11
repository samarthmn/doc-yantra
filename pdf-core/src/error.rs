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
}

#[cfg(feature = "pdfium")]
impl From<pdfium_render::prelude::PdfiumError> for PdfCoreError {
    fn from(err: pdfium_render::prelude::PdfiumError) -> Self {
        PdfCoreError::Pdfium(format!("{err:?}"))
    }
}

pub type Result<T> = std::result::Result<T, PdfCoreError>;

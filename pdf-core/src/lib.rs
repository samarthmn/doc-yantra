//! # pdf-core — DocKosha's native PDF engine
//!
//! Zone 1 (pure Rust, lopdf/pdf-writer): assembly, watermarking, metadata,
//! generation. Zone 2 (pdfium, BSD-licensed): rasterization, text extraction,
//! redaction, forms.
//!
//! All operations take and return `&[u8]`/`Vec<u8>` PDF bytes so the crate
//! stays IO-agnostic and embeddable (service, N-API, or WASM for Zone 1).

pub mod assembly;
pub mod error;
#[cfg(feature = "pdfium")]
pub mod extract;
pub mod fonts;
#[cfg(feature = "pdfium")]
pub mod forms;
pub mod generate;
pub mod meta;
#[cfg(feature = "pdfium")]
pub mod pdfium;
#[cfg(feature = "pdfium")]
pub mod raster;
#[cfg(feature = "pdfium")]
pub mod redact;
pub mod testkit;
pub mod watermark;

pub use error::{PdfCoreError, Result};

//! XLSX print model: the number-format engine, MDW-based column widths, and
//! print-area pagination that reproduces Excel's "Save As PDF" output.

pub mod numfmt;
pub mod paginate;
pub mod render;
pub mod widths;

pub use paginate::XlsxDoc;
pub use render::render_xlsx;

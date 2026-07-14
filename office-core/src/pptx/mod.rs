//! PPTX layout: master/layout/slide inheritance, shape geometry, text bodies,
//! fills, and tables. One PDF page per slide.
//!
//! Covers common business-deck preset geometry plus DrawingML custom paths,
//! placeholder text, fills, pictures, and tables. Unknown presets and guide
//! formulas fail closed through the profile/convert contract.

pub mod assemble;
pub mod geometry;
pub mod inherit;
pub mod render;
pub(crate) mod table_style;
pub mod textbody;

pub use assemble::PptxDoc;
pub use render::render_pptx;

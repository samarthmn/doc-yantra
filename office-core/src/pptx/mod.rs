//! PPTX layout: master/layout/slide inheritance, shape geometry, text bodies,
//! fills, and tables. One PDF page per slide.
//!
//! v1 covers the common business-deck envelope (placeholder text, fills,
//! pictures, tables). The full DrawingML preset-geometry evaluator and
//! `custGeom` path rendering are a follow-up gated on vendoring ECMA-376's
//! `presetShapeDefinitions.xml` (see [`geometry`]).

pub mod assemble;
pub mod geometry;
pub mod inherit;
pub mod render;
pub mod textbody;

pub use assemble::PptxDoc;
pub use render::render_pptx;

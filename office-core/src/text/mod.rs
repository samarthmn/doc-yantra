//! Text measurement and line breaking: shaping-lite (cmap + kern + GPOS pair
//! adjustment), UAX-14 line breaking, and the complex-script gate.

pub mod linebreak;
pub mod script;
pub mod semantic;
pub mod shape;

pub use script::{ScriptScan, scan_scripts};
pub use shape::{Glyph, ShapedRun, shape};

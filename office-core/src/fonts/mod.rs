//! Font database, matching/substitution, metrics, subsetting, and ODTTF
//! deobfuscation.
//!
//! Bundled fallback fonts live under `office-core/fonts/` and are vendored by a
//! human (see the plan's human task H2); the `bundled-fonts` feature embeds
//! whatever is present via `include_bytes!`. The loader is tolerant of an empty
//! directory — the crate compiles and non-font tests pass without them.

pub mod bundled;
pub mod db;
pub mod matching;
pub mod metrics;
pub mod odttf;
pub mod subset;

pub use db::{Face, FaceId, FontDb};
pub use matching::{Resolved, map_symbol_char, resolve};
pub use metrics::{FaceMetrics, GlyphId};
pub use subset::{SubsetFont, subset_for_pdf};

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
pub mod registration;
pub mod reporting;
pub mod requests;
pub mod subset;

pub use db::{Face, FaceId, FontDb};
pub use matching::{
    Resolved, map_symbol_char, resolve, resolve_exact, resolve_exact_with_stretch,
    resolve_with_stretch, wml_run_family,
};
pub use metrics::{FaceMetrics, GlyphId};
pub use registration::{
    EmbeddingPermission, FontExpectation, FontIdentity, VariationCoordinate, validate_font,
    validate_font_with_limit,
};
pub use reporting::{
    FontExactness, FontResolution, FontResolutionEvent, FontResolutionSource, SelectedFontIdentity,
};
pub use requests::{FontRequest, FontSlot};
pub use subset::{SubsetFont, subset_for_pdf};

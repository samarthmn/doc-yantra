//! Font database, matching/substitution, metrics, subsetting, and ODTTF
//! deobfuscation.
//!
//! Bundled fallback fonts live under `office-core/fonts/` and are vendored by a
//! human (see the plan's human task H2); the `bundled-fonts` feature embeds
//! whatever is present via `include_bytes!`. The loader is tolerant of an empty
//! directory — the crate compiles and non-font tests pass without them.

pub mod bundled;
pub(crate) mod cff_check;
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
    Resolved, family_key, map_symbol_char, pan_cjk_substitution_target, resolve, resolve_exact,
    resolve_exact_or_substitute_with_stretch, resolve_exact_with_stretch, resolve_with_stretch,
    wml_run_family,
};
pub use metrics::{FaceMetrics, GlyphId, LegacyCjkClass};
pub use registration::{
    EmbeddingPermission, FontExpectation, FontIdentity, VariationCoordinate, validate_font,
    validate_font_with_limit,
};
pub use reporting::{
    FontExactness, FontResolution, FontResolutionEvent, FontResolutionSource, SelectedFontIdentity,
};
pub use requests::{
    FontClassing, FontRequest, FontSlot, FontSubstitutionClass, classify_font_metadata,
    classify_font_request, slot_for_character,
};
pub use subset::{SubsetFont, subset_for_pdf};

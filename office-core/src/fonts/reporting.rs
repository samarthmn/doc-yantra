//! Stable profile/convert font-resolution telemetry contracts.

use super::{EmbeddingPermission, FontSlot};
use serde::{Deserialize, Serialize};

/// How closely the selected artifact matches the authored request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontExactness {
    /// Exact face embedded in the source Office document.
    EmbeddedExact,
    /// Exact identity and caller-pinned artifact digest.
    HashPinnedExact,
    /// Exact family/style from a catalog, but the document records no version.
    CatalogVersionAssumed,
    /// Same family with a compatible but non-identical style.
    FamilyStyleCompatible,
    /// A substitution or family-class fallback.
    Fallback,
    /// No usable face was selected.
    Unresolved,
}

/// Layer that supplied the selected font artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontResolutionSource {
    /// Document-embedded face.
    Embedded,
    /// Runtime-registered or explicitly host-authorized native face.
    Registered,
    /// Host memory cache.
    MemoryCache,
    /// Host persistent content-addressed cache.
    PersistentCache,
    /// Configured host provider.
    Provider,
    /// Compatible substitution/fallback ladder.
    Fallback,
    /// No source supplied a usable face.
    None,
}

/// Stable event name emitted for a font-resolution outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontResolutionEvent {
    /// A document-embedded face was selected.
    FontEmbedded,
    /// A runtime-registered face was selected.
    FontRuntimeRegistered,
    /// Host memory cache hit.
    FontMemoryCacheHit,
    /// Host persistent cache hit.
    FontPersistentCacheHit,
    /// Provider fetch succeeded.
    FontFetched,
    /// Provider fetch failed temporarily.
    FontFetchFailed,
    /// Provider reported not found.
    FontNotFound,
    /// Provider identity metadata did not match the artifact.
    FontIdentityMismatch,
    /// Provider digest did not match the artifact.
    FontHashMismatch,
    /// Artifact embedding policy denied use.
    FontLicenseRestricted,
    /// Exact family/style version is document-unspecified.
    FontVersionAssumed,
    /// Compatible substitution/fallback was used.
    FontFallbackUsed,
}

/// Reproducible identity of the face selected by the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedFontIdentity {
    /// Primary parsed family.
    pub family: String,
    /// Full face name.
    pub full_name: Option<String>,
    /// PostScript face name.
    pub postscript_name: Option<String>,
    /// Name-table version string.
    pub version: Option<String>,
    /// SHA-256 of the complete registered artifact.
    pub sha256: String,
    /// Selected face index for a collection.
    pub face_index: u32,
    /// OS/2 weight.
    pub weight: u16,
    /// Italic/oblique state.
    pub italic: bool,
    /// OS/2 width class.
    pub stretch: u16,
}

/// One text-free, per-face resolution outcome shared by profile and convert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontResolution {
    /// Authored requested family.
    pub requested_family: String,
    /// Requested weight.
    pub requested_weight: u16,
    /// Requested italic state.
    pub requested_italic: bool,
    /// Requested width class.
    pub requested_stretch: u16,
    /// Effective OOXML slots.
    pub slots: Vec<FontSlot>,
    /// Selected identity, absent when unresolved.
    pub selected: Option<SelectedFontIdentity>,
    /// Source layer.
    pub source: FontResolutionSource,
    /// Exactness category.
    pub exactness: FontExactness,
    /// Decoded embedding policy for the selected face.
    pub embedding_permission: Option<EmbeddingPermission>,
    /// Whether selected/fallback faces cover all required code points.
    pub coverage_ok: bool,
    /// Stable failure detail when unresolved or rejected.
    pub failure_reason: Option<String>,
    /// Selected fallback identity when exactness is `fallback`.
    pub fallback_identity: Option<SelectedFontIdentity>,
    /// Host-filled cache status; absent in the network-free core.
    pub cache_status: Option<String>,
    /// Host-filled provider duration; absent in the network-free core.
    pub fetch_duration_ms: Option<u64>,
    /// Stable event name for this outcome.
    pub event: FontResolutionEvent,
}

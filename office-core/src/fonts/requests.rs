//! Canonical, text-free font requests emitted by Office preflight.

use serde::{Deserialize, Serialize};

/// OOXML script slot that selected a requested family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontSlot {
    /// `w:rFonts/@w:ascii`.
    Ascii,
    /// `w:rFonts/@w:hAnsi`.
    HAnsi,
    /// `w:rFonts/@w:eastAsia`.
    EastAsia,
    /// `w:rFonts/@w:cs`.
    Cs,
}

impl FontSlot {
    /// Stable snake-case provider wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            FontSlot::Ascii => "ascii",
            FontSlot::HAnsi => "h_ansi",
            FontSlot::EastAsia => "east_asia",
            FontSlot::Cs => "cs",
        }
    }
}

/// One deduplicated family/style request sent to a host font provider.
///
/// This is metadata only: document text and exact per-document character
/// strings are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontRequest {
    /// Canonical authored family name.
    pub family: String,
    /// Font-table aliases, in deterministic order.
    pub alt_families: Vec<String>,
    /// Requested weight class.
    pub weight: u16,
    /// Requested italic/oblique state.
    pub italic: bool,
    /// Requested stretch/width class (1 through 9).
    pub stretch: u16,
    /// Effective OOXML slots represented by this request.
    pub slots: Vec<FontSlot>,
    /// Optional 10-byte PANOSE value as uppercase hexadecimal.
    pub panose: Option<String>,
    /// Optional OOXML family classification (`swiss`, `roman`, and so on).
    pub family_class: Option<String>,
    /// Optional OOXML pitch classification.
    pub pitch: Option<String>,
    /// Optional OOXML charset byte as hexadecimal.
    pub charset: Option<String>,
    /// OS/2 Unicode-range bitfields declared by `fontTable.xml`.
    pub unicode_ranges: Vec<u32>,
    /// Script classes required by visible content, never the content itself.
    pub scripts: Vec<String>,
    /// Requested variable-font coordinates as stable `axis=value` strings.
    pub vf_coords: Vec<String>,
}

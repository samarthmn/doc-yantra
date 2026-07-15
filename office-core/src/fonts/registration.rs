//! Hardened SFNT/TTC validation and reproducible runtime-font identity.

use crate::{OfficeError, Result, UnsupportedCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use ttf_parser::{Face, Tag, name_id};

/// Default maximum size accepted by validated runtime registration (10 MiB).
pub const DEFAULT_MAX_FONT_BYTES: usize = 10 * 1024 * 1024;

/// A requested variable-font axis coordinate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariationCoordinate {
    /// Four-character OpenType axis tag such as `wght`.
    pub axis: String,
    /// Requested design-space value.
    pub value: f32,
}

/// Caller-supplied identity and coverage constraints for a font artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontExpectation {
    /// Expected family name.
    pub family: String,
    /// Expected OS/2 weight class.
    pub weight: u16,
    /// Whether an italic or oblique face is expected.
    pub italic: bool,
    /// Expected OS/2 width class (1 through 9; 5 is normal).
    pub stretch: u16,
    /// Optional exact name-table version string.
    pub version: Option<String>,
    /// Optional lowercase or uppercase SHA-256 hex digest.
    pub sha256: Option<String>,
    /// Face index for an SFNT collection; defaults to zero.
    pub face_index: Option<u32>,
    /// Script names whose representative characters must be covered.
    #[serde(default)]
    pub required_scripts: Vec<String>,
    /// Exact Unicode scalar values that must be covered.
    #[serde(default)]
    pub required_codepoints: Vec<u32>,
    /// Variable-font coordinates the selected face must admit.
    #[serde(default)]
    pub vf_coords: Vec<VariationCoordinate>,
}

impl Default for FontExpectation {
    fn default() -> Self {
        FontExpectation {
            family: String::new(),
            weight: 400,
            italic: false,
            stretch: 5,
            version: None,
            sha256: None,
            face_index: None,
            required_scripts: Vec::new(),
            required_codepoints: Vec::new(),
            vf_coords: Vec::new(),
        }
    }
}

/// Effective OS/2 embedding policy for the selected face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingPermission {
    /// Installable embedding; subsetting allowed.
    Installable,
    /// Editable embedding; subsetting allowed.
    Editable,
    /// Preview-and-print embedding; subsetting allowed.
    PreviewPrint,
    /// Installable embedding but the font must be embedded whole.
    InstallableNoSubsetting,
    /// Editable embedding but the font must be embedded whole.
    EditableNoSubsetting,
    /// Preview-and-print embedding but the font must be embedded whole.
    PreviewPrintNoSubsetting,
    /// Restricted-license embedding.
    Restricted,
    /// Only bitmap strikes may be embedded; outline PDF embedding is forbidden.
    BitmapOnly,
    /// The OS/2 table or its policy bits are absent/malformed.
    Unknown,
    /// Mutually exclusive permission bits were combined.
    Contradictory,
}

impl EmbeddingPermission {
    /// Whether the outline face may be embedded in a PDF.
    pub fn allows_outline_embedding(self) -> bool {
        matches!(
            self,
            EmbeddingPermission::Installable
                | EmbeddingPermission::Editable
                | EmbeddingPermission::PreviewPrint
                | EmbeddingPermission::InstallableNoSubsetting
                | EmbeddingPermission::EditableNoSubsetting
                | EmbeddingPermission::PreviewPrintNoSubsetting
        )
    }

    /// Whether the font license permits subsetting.
    pub fn allows_subsetting(self) -> bool {
        matches!(
            self,
            EmbeddingPermission::Installable
                | EmbeddingPermission::Editable
                | EmbeddingPermission::PreviewPrint
        )
    }
}

/// Validated, reproducible identity of one selected face.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontIdentity {
    /// Unique family names present on the selected face.
    pub families: Vec<String>,
    /// Name-table full face name.
    pub full_name: Option<String>,
    /// Name-table PostScript name.
    pub postscript_name: Option<String>,
    /// Name-table subfamily/style name.
    pub subfamily: Option<String>,
    /// OS/2 weight class.
    pub weight: u16,
    /// Whether the selected face is italic or oblique.
    pub italic: bool,
    /// OS/2 width class.
    pub stretch: u16,
    /// Name-table version string.
    pub version: Option<String>,
    /// Number of faces in the supplied SFNT/TTC.
    pub face_count: u32,
    /// Selected face index.
    pub face_index: u32,
    /// SHA-256 digest of the complete supplied artifact.
    pub sha256: String,
    /// Decoded OS/2 embedding policy.
    pub fs_type: EmbeddingPermission,
    /// Whether every requested script/codepoint is covered.
    pub coverage_ok: bool,
}

/// Validates one SFNT/TTC artifact using the default 10 MiB cap.
pub fn validate_font(bytes: &[u8], expectation: Option<&FontExpectation>) -> Result<FontIdentity> {
    validate_font_with_limit(bytes, expectation, DEFAULT_MAX_FONT_BYTES)
}

/// Validates one SFNT/TTC artifact with a caller-selected byte cap.
pub fn validate_font_with_limit(
    bytes: &[u8],
    expectation: Option<&FontExpectation>,
    max_bytes: usize,
) -> Result<FontIdentity> {
    validate_font_impl(bytes, expectation, max_bytes, None)
}

pub(crate) fn validate_font_index(bytes: &[u8], face_index: u32) -> Result<FontIdentity> {
    validate_font_impl(bytes, None, DEFAULT_MAX_FONT_BYTES, Some(face_index))
}

fn validate_font_impl(
    bytes: &[u8],
    expectation: Option<&FontExpectation>,
    max_bytes: usize,
    selected_face_index: Option<u32>,
) -> Result<FontIdentity> {
    if bytes.len() > max_bytes {
        return Err(OfficeError::unsupported(
            UnsupportedCode::DecompressionLimit,
            format!(
                "font is {} bytes, over the {max_bytes} byte registration limit",
                bytes.len()
            ),
        ));
    }
    if matches!(bytes.get(..4), Some(b"wOFF") | Some(b"wOF2")) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::FontFormatUnsupported,
            "WOFF/WOFF2 must be decoded to SFNT by the host",
        ));
    }

    let face_count = ttf_parser::fonts_in_collection(bytes).unwrap_or(1).max(1);
    let face_index = selected_face_index
        .or_else(|| expectation.and_then(|value| value.face_index))
        .unwrap_or(0);
    if face_index >= face_count {
        return Err(OfficeError::unsupported(
            UnsupportedCode::FontFormatUnsupported,
            format!("font face index {face_index} is outside collection size {face_count}"),
        ));
    }
    let face = Face::parse(bytes, face_index).map_err(|_| {
        OfficeError::unsupported(
            UnsupportedCode::FontFormatUnsupported,
            "unparseable SFNT/OTF/TTC bytes",
        )
    })?;
    let sha256 = format!("{:x}", Sha256::digest(bytes));
    let families = face_names(&face, &[name_id::TYPOGRAPHIC_FAMILY, name_id::FAMILY]);
    let identity = FontIdentity {
        families,
        full_name: face_name(&face, name_id::FULL_NAME),
        postscript_name: face_name(&face, name_id::POST_SCRIPT_NAME),
        subfamily: face_name(&face, name_id::SUBFAMILY),
        weight: face.weight().to_number(),
        italic: face.is_italic() || face.is_oblique(),
        stretch: face.width().to_number(),
        version: face_name(&face, name_id::VERSION),
        face_count,
        face_index,
        sha256,
        fs_type: embedding_permission(&face),
        coverage_ok: expectation.is_none_or(|expected| coverage_matches(&face, expected)),
    };

    if let Some(expected) = expectation {
        validate_expectation(&identity, &face, expected)?;
    }
    if !identity.fs_type.allows_outline_embedding() {
        return Err(OfficeError::unsupported(
            UnsupportedCode::FontLicenseRestricted,
            format!("font embedding policy is {:?}", identity.fs_type),
        ));
    }
    Ok(identity)
}

fn validate_expectation(
    identity: &FontIdentity,
    face: &Face<'_>,
    expected: &FontExpectation,
) -> Result<()> {
    if let Some(pin) = &expected.sha256
        && !pin.eq_ignore_ascii_case(&identity.sha256)
    {
        return Err(OfficeError::unsupported(
            UnsupportedCode::FontHashMismatch,
            format!("expected SHA-256 {pin}, received {}", identity.sha256),
        ));
    }

    let family_matches = identity
        .families
        .iter()
        .chain(identity.full_name.iter())
        .chain(identity.postscript_name.iter())
        .any(|name| normalized_name(name) == normalized_name(&expected.family));
    if !family_matches
        || identity.weight != expected.weight
        || identity.italic != expected.italic
        || identity.stretch != expected.stretch
        || expected
            .version
            .as_ref()
            .is_some_and(|version| identity.version.as_ref() != Some(version))
    {
        return Err(OfficeError::unsupported(
            UnsupportedCode::FontIdentityMismatch,
            format!(
                "expected {} weight={} italic={} stretch={}, received families={:?} weight={} italic={} stretch={}",
                expected.family,
                expected.weight,
                expected.italic,
                expected.stretch,
                identity.families,
                identity.weight,
                identity.italic,
                identity.stretch
            ),
        ));
    }
    if !identity.coverage_ok {
        return Err(OfficeError::unsupported(
            UnsupportedCode::FontCoverageMismatch,
            format!(
                "font {} does not cover the requested scripts",
                expected.family
            ),
        ));
    }
    validate_variations(face, expected)
}

fn validate_variations(face: &Face<'_>, expected: &FontExpectation) -> Result<()> {
    if expected.vf_coords.is_empty() {
        return Ok(());
    }
    let axes: Vec<_> = face.variation_axes().into_iter().collect();
    for coordinate in &expected.vf_coords {
        let Some(axis) = axes
            .iter()
            .find(|axis| axis.tag.to_string() == coordinate.axis)
        else {
            return Err(OfficeError::unsupported(
                UnsupportedCode::FontIdentityMismatch,
                format!("font has no variable axis {}", coordinate.axis),
            ));
        };
        if coordinate.value < axis.min_value || coordinate.value > axis.max_value {
            return Err(OfficeError::unsupported(
                UnsupportedCode::FontIdentityMismatch,
                format!(
                    "axis {} value {} is outside {}..={}",
                    coordinate.axis, coordinate.value, axis.min_value, axis.max_value
                ),
            ));
        }
    }
    Ok(())
}

fn coverage_matches(face: &Face<'_>, expected: &FontExpectation) -> bool {
    expected
        .required_codepoints
        .iter()
        .filter_map(|codepoint| char::from_u32(*codepoint))
        .chain(
            expected
                .required_scripts
                .iter()
                .filter_map(|script| representative_for_script(script)),
        )
        .all(|character| face.glyph_index(character).is_some())
}

fn representative_for_script(script: &str) -> Option<char> {
    match script.trim().to_ascii_lowercase().as_str() {
        "latin" => Some('A'),
        "cyrillic" => Some('\u{0416}'),
        "greek" => Some('\u{03A9}'),
        "arabic" => Some('\u{0634}'),
        "hebrew" => Some('\u{05D0}'),
        "devanagari" => Some('\u{0915}'),
        "thai" => Some('\u{0E01}'),
        "cjk" | "han" | "east_asia" => Some('\u{65E5}'),
        "hangul" => Some('\u{D55C}'),
        "hiragana" => Some('\u{3042}'),
        "katakana" => Some('\u{30A2}'),
        "symbols" => Some('\u{25CF}'),
        _ => None,
    }
}

pub(crate) fn embedding_permission(face: &Face<'_>) -> EmbeddingPermission {
    let Some(os2) = face.raw_face().table(Tag::from_bytes(b"OS/2")) else {
        return EmbeddingPermission::Unknown;
    };
    let Some(raw) = os2.get(8..10) else {
        return EmbeddingPermission::Unknown;
    };
    let fs_type = u16::from_be_bytes([raw[0], raw[1]]);
    let policy = fs_type & 0x000f;
    if policy.count_ones() > 1 {
        return EmbeddingPermission::Contradictory;
    }
    if fs_type & 0x0200 != 0 {
        return EmbeddingPermission::BitmapOnly;
    }
    let no_subsetting = fs_type & 0x0100 != 0;
    match (policy, no_subsetting) {
        (0, false) => EmbeddingPermission::Installable,
        (0, true) => EmbeddingPermission::InstallableNoSubsetting,
        (2, _) => EmbeddingPermission::Restricted,
        (4, false) => EmbeddingPermission::PreviewPrint,
        (4, true) => EmbeddingPermission::PreviewPrintNoSubsetting,
        (8, false) => EmbeddingPermission::Editable,
        (8, true) => EmbeddingPermission::EditableNoSubsetting,
        _ => EmbeddingPermission::Unknown,
    }
}

fn face_names(face: &Face<'_>, ids: &[u16]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for id in ids {
        for name in face.names().into_iter().filter(|name| name.name_id == *id) {
            if let Some(value) = name.to_string()
                && !value.is_empty()
            {
                names.insert(value);
            }
        }
    }
    names.into_iter().collect()
}

fn face_name(face: &Face<'_>, id: u16) -> Option<String> {
    face.names()
        .into_iter()
        .filter(|name| name.name_id == id)
        .find_map(|name| name.to_string())
        .filter(|value| !value.is_empty())
}

fn normalized_name(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|character| {
            !character.is_ascii_whitespace() && *character != '-' && *character != '_'
        })
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARLITO: &[u8] = include_bytes!("../../fonts/carlito/Carlito-Regular.ttf");

    #[test]
    fn script_representatives_are_metadata_only_and_stable() {
        assert_eq!(representative_for_script("Arabic"), Some('\u{0634}'));
        assert_eq!(representative_for_script("unknown"), None);
    }

    #[test]
    fn fs_type_policy_matrix_fails_closed_and_preserves_no_subsetting() {
        for (fs_type, expected, subsettable) in [
            (0x0000, EmbeddingPermission::Installable, true),
            (0x0004, EmbeddingPermission::PreviewPrint, true),
            (0x0008, EmbeddingPermission::Editable, true),
            (0x0100, EmbeddingPermission::InstallableNoSubsetting, false),
            (0x0104, EmbeddingPermission::PreviewPrintNoSubsetting, false),
            (0x0108, EmbeddingPermission::EditableNoSubsetting, false),
        ] {
            let bytes = with_fs_type(CARLITO, fs_type);
            let identity = validate_font(&bytes, None).expect("embeddable policy");
            assert_eq!(identity.fs_type, expected);
            assert_eq!(identity.fs_type.allows_subsetting(), subsettable);
        }

        for (fs_type, expected) in [
            (0x0002, EmbeddingPermission::Restricted),
            (0x0200, EmbeddingPermission::BitmapOnly),
            (0x0006, EmbeddingPermission::Contradictory),
            (0x0001, EmbeddingPermission::Unknown),
        ] {
            let bytes = with_fs_type(CARLITO, fs_type);
            let error = validate_font(&bytes, None).expect_err("policy must reject outlines");
            assert!(matches!(
                error,
                OfficeError::Unsupported {
                    code: UnsupportedCode::FontLicenseRestricted,
                    ..
                }
            ));
            let face = Face::parse(&bytes, 0).unwrap();
            assert_eq!(embedding_permission(&face), expected);
        }
    }

    fn with_fs_type(source: &[u8], fs_type: u16) -> Vec<u8> {
        let mut bytes = source.to_vec();
        let table_count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
        let offset = (0..table_count)
            .find_map(|index| {
                let record = 12 + index * 16;
                (&bytes[record..record + 4] == b"OS/2").then(|| {
                    u32::from_be_bytes([
                        bytes[record + 8],
                        bytes[record + 9],
                        bytes[record + 10],
                        bytes[record + 11],
                    ]) as usize
                })
            })
            .expect("OS/2 table");
        bytes[offset + 8..offset + 10].copy_from_slice(&fs_type.to_be_bytes());
        bytes
    }
}

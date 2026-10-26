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

/// Word's OOXML font slot for one character's script.
///
/// Preflight coverage collection and layout share this classifier so
/// `profile()` and `convert()` resolve the same face for the same character.
/// Word uses the ASCII slot for ASCII, the East Asian slot for Han/kana/
/// Hangul/CJK punctuation/fullwidth forms, and the complex-script slot for
/// scripts that need shaping or bidi.
pub fn slot_for_character(character: char) -> FontSlot {
    let codepoint = character as u32;
    if character.is_ascii() {
        FontSlot::Ascii
    } else if matches!(
        codepoint,
        0x1100..=0x11FF
            | 0x2E80..=0x2FFF
            | 0x3000..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7FF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFFEF
            | 0x20000..=0x3134F
    ) {
        FontSlot::EastAsia
    } else if matches!(
        codepoint,
        0x0590..=0x08FF
            | 0x0900..=0x0EFF
            | 0x1000..=0x109F
            | 0x1780..=0x17FF
            | 0xFB1D..=0xFEFF
    ) {
        FontSlot::Cs
    } else {
        FontSlot::HAnsi
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

/// Coarse metric-compatible family selected from OOXML font metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontSubstitutionClass {
    /// Proportional serif text face.
    Serif,
    /// Proportional sans-serif text face.
    Sans,
    /// Fixed-pitch text face.
    Mono,
}

/// A metadata-derived substitution class and the signal that selected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontClassing {
    /// Selected coarse family class.
    pub class: FontSubstitutionClass,
    /// Stable, text-free diagnostic basis for substitution warnings.
    pub basis: String,
}

/// Classifies a preflight request from `fontTable.xml` metadata.
pub fn classify_font_request(request: &FontRequest) -> Option<FontClassing> {
    classify_font_metadata(
        request.panose.as_deref(),
        request.family_class.as_deref(),
        request.pitch.as_deref(),
    )
}

/// Classifies OOXML PANOSE/family/pitch metadata without using the family name.
pub fn classify_font_metadata(
    panose: Option<&str>,
    family_class: Option<&str>,
    pitch: Option<&str>,
) -> Option<FontClassing> {
    if pitch.is_some_and(|pitch| {
        matches!(
            pitch.trim().to_ascii_lowercase().as_str(),
            "fixed" | "fixedpitch" | "mono" | "monospace" | "monospaced"
        )
    }) {
        return Some(FontClassing {
            class: FontSubstitutionClass::Mono,
            basis: "pitch=fixed".into(),
        });
    }

    if let Some(bytes) = panose.and_then(parse_panose) {
        if bytes[3] == 9 {
            return Some(FontClassing {
                class: FontSubstitutionClass::Mono,
                basis: "panose proportion=monospaced".into(),
            });
        }
        if bytes[0] == 2 {
            let class = match bytes[1] {
                2..=10 => Some(FontSubstitutionClass::Serif),
                11..=15 => Some(FontSubstitutionClass::Sans),
                _ => None,
            };
            if let Some(class) = class {
                return Some(FontClassing {
                    class,
                    basis: format!("panose serif-style={}", bytes[1]),
                });
            }
        }
    }

    let family = family_class?.trim().to_ascii_lowercase();
    let class = match family.as_str() {
        "roman" => FontSubstitutionClass::Serif,
        "swiss" => FontSubstitutionClass::Sans,
        "modern" => FontSubstitutionClass::Mono,
        _ => return None,
    };
    Some(FontClassing {
        class,
        basis: format!("family={family}"),
    })
}

fn parse_panose(value: &str) -> Option<[u8; 10]> {
    let value = value.trim();
    if value.len() != 20 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = [0; 10];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_classifier_matches_word_script_slots() {
        assert_eq!(slot_for_character('A'), FontSlot::Ascii);
        assert_eq!(slot_for_character('é'), FontSlot::HAnsi);
        assert_eq!(slot_for_character('汉'), FontSlot::EastAsia);
        assert_eq!(slot_for_character('あ'), FontSlot::EastAsia);
        assert_eq!(slot_for_character('한'), FontSlot::EastAsia);
        assert_eq!(slot_for_character('。'), FontSlot::EastAsia);
        assert_eq!(slot_for_character('ｱ'), FontSlot::EastAsia);
        assert_eq!(slot_for_character('ㄱ'), FontSlot::EastAsia);
        assert_eq!(slot_for_character('㈱'), FontSlot::EastAsia);
        assert_eq!(slot_for_character('㎡'), FontSlot::EastAsia);
        assert_eq!(slot_for_character('﹐'), FontSlot::EastAsia);
        assert_eq!(slot_for_character('“'), FontSlot::HAnsi);
        assert_eq!(slot_for_character('ש'), FontSlot::Cs);
    }

    #[test]
    fn classifies_font_table_signals_without_family_name_guessing() {
        assert_eq!(
            classify_font_metadata(None, Some("roman"), None),
            Some(FontClassing {
                class: FontSubstitutionClass::Serif,
                basis: "family=roman".into(),
            })
        );
        assert_eq!(
            classify_font_metadata(Some("020B0604020202020204"), None, None)
                .map(|classing| classing.class),
            Some(FontSubstitutionClass::Sans)
        );
        assert_eq!(
            classify_font_metadata(Some("02020609020202020204"), None, None)
                .map(|classing| classing.class),
            Some(FontSubstitutionClass::Mono)
        );
        assert_eq!(
            classify_font_metadata(None, Some("swiss"), Some("fixed"))
                .map(|classing| classing.class),
            Some(FontSubstitutionClass::Mono)
        );
    }
}

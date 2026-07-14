//! Font matching and substitution: resolve a requested `(family, weight,
//! italic)` to a concrete face, applying the metric-compatible substitution map
//! and family-class fallback, plus symbol-font bullet mapping.

use super::db::{FaceId, FontDb};
use crate::profile::Warning;

/// The result of resolving a font request.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    /// The chosen face, or `None` if the database is empty (fonts not vendored).
    pub face: Option<FaceId>,
    /// Warnings accumulated (substitution, fallback, synthetic-style).
    pub warnings: Vec<Warning>,
    /// The requested family, recorded when it had no exact/compatible match.
    pub missing: Option<String>,
}

/// Resolves `(family, weight, italic)` following the documented order:
/// embedded → exact → metric-compatible substitution → family-class fallback.
pub fn resolve(db: &FontDb, family: &str, weight: u16, italic: bool) -> Resolved {
    let mut out = Resolved::default();
    if db.is_empty() {
        out.missing = Some(family.to_string());
        return out;
    }

    // 1 & 2: exact family match (embedded faces sort first via `pick_in_family`).
    if let Some(pick) = pick_in_family(db, family, weight, italic) {
        out.face = Some(pick.face);
        if pick.synthetic_italic {
            out.warnings.push(Warning::new(
                "synthetic_style_unavailable",
                family.to_string(),
            ));
        }
        return out;
    }

    // 3: metric-compatible substitution.
    if let Some(target) = substitute(family)
        && let Some(pick) = pick_in_family(db, target, weight, italic)
    {
        out.face = Some(pick.face);
        out.warnings.push(Warning::new(
            "font_substituted",
            format!("{family} -> {target}"),
        ));
        if pick.synthetic_italic {
            out.warnings.push(Warning::new(
                "synthetic_style_unavailable",
                family.to_string(),
            ));
        }
        return out;
    }

    // 4: family-class fallback by name heuristic.
    let fallback = fallback_class(family);
    if let Some(pick) = pick_in_family(db, fallback, weight, italic) {
        out.face = Some(pick.face);
        out.warnings.push(Warning::new(
            "font_fallback",
            format!("{family} -> {fallback}"),
        ));
        out.missing = Some(family.to_string());
        return out;
    }

    // Nothing in the database covers even the fallback class.
    out.missing = Some(family.to_string());
    out
}

/// Returns deterministic coverage-fallback candidates after `primary`.
/// Embedded document faces win, then packaged Noto faces, then metric-compatible
/// bundled faces, with style distance breaking ties inside each class.
pub fn coverage_fallbacks(db: &FontDb, primary: FaceId, weight: u16, italic: bool) -> Vec<FaceId> {
    let mut ids: Vec<FaceId> = (0..db.len()).map(FaceId).collect();
    ids.retain(|id| {
        *id == primary || !family_key(&db.faces()[id.0].family_name).contains("lastresort")
    });
    ids.sort_by_key(|id| {
        let face = &db.faces()[id.0];
        let family = family_key(&face.family_name);
        let class = if *id == primary {
            0
        } else if face.embedded {
            1
        } else if family.starts_with("noto") {
            2
        } else if matches!(
            family.as_str(),
            "carlito" | "caladea" | "liberation sans" | "liberation serif" | "liberation mono"
        ) {
            3
        } else {
            4
        };
        (
            class,
            u8::from(face.italic != italic),
            face.weight.abs_diff(weight),
            id.0,
        )
    });
    ids
}

/// Resolves a bullet-only documented substitution when the authored glyph is
/// absent from every selected coverage fallback. Unknown characters are not
/// substituted, preserving the fail-closed behavior for arbitrary PUA bullets.
pub fn substitute_bullet_char(db: &FontDb, faces: &[FaceId], ch: char) -> Option<char> {
    let covered = |candidate: char| {
        faces.iter().any(|face_id| {
            db.face(*face_id)
                .and_then(|face| face.parsed())
                .and_then(|face| face.glyph_index(candidate))
                .is_some()
        })
    };
    if covered(ch) {
        return Some(ch);
    }
    let candidates: &[char] = match ch {
        // Google/Office arrowhead bullets -> black right-pointing pointer.
        '\u{2B9A}' | '\u{27A2}' | '\u{27A4}' => &['\u{25BA}', '\u{2022}'],
        // Diamond ornaments retain filled/outlined intent before generic bullet.
        '\u{2756}' => &['\u{2666}', '\u{2022}'],
        '\u{2726}' => &['\u{25C6}', '\u{2022}'],
        '\u{2727}' => &['\u{25C7}', '\u{2022}'],
        // Dot operators and middle dots normalize to the ordinary bullet.
        '\u{2219}' | '\u{00B7}' => &['\u{2022}'],
        _ => &[],
    };
    candidates
        .iter()
        .copied()
        .find(|candidate| covered(*candidate))
}

struct Pick {
    face: FaceId,
    synthetic_italic: bool,
}

/// Picks the best face in `family` (case-insensitive, matching family / full /
/// name), scored by italic match then weight distance. Embedded faces win ties.
fn pick_in_family(db: &FontDb, family: &str, weight: u16, italic: bool) -> Option<Pick> {
    let key = family_key(family);
    let mut best: Option<(FaceId, i64, bool)> = None; // (id, score, italic_match)
    for (i, face) in db.faces().iter().enumerate() {
        if family_key(&face.family_name) != key
            && face
                .full_name
                .as_deref()
                .map(|n| family_key(n) != key)
                .unwrap_or(true)
        {
            continue;
        }
        let italic_match = face.italic == italic;
        let weight_dist = (face.weight as i64 - weight as i64).abs();
        // Lower score is better: prioritise italic match, then weight distance,
        // then prefer embedded faces.
        let score = (if italic_match { 0 } else { 100_000 })
            + weight_dist
            + if face.embedded { -1 } else { 0 };
        let id = FaceId(i);
        match &best {
            Some((_, best_score, _)) if *best_score <= score => {}
            _ => best = Some((id, score, italic_match)),
        }
    }
    best.map(|(face, _, italic_match)| Pick {
        face,
        synthetic_italic: italic && !italic_match,
    })
}

/// Normalizes a family name for case-insensitive comparison.
fn family_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// The metric-compatible substitution target for a Microsoft family, if any.
fn substitute(family: &str) -> Option<&'static str> {
    match family_key(family).as_str() {
        "calibri" | "calibri light" => Some("Carlito"),
        "cambria" => Some("Caladea"),
        "arial" | "helvetica" | "segoe ui" => Some("Liberation Sans"),
        "times new roman" | "times" => Some("Liberation Serif"),
        "courier new" | "courier" => Some("Liberation Mono"),
        _ => None,
    }
}

/// The family-class fallback for an unknown family, by name heuristic.
fn fallback_class(family: &str) -> &'static str {
    let key = family_key(family);
    if key.contains("mono") || key.contains("consol") || key.contains("courier") {
        "Liberation Mono"
    } else if [
        "times", "georgia", "garamond", "book", "serif", "cambria", "minion",
    ]
    .iter()
    .any(|s| key.contains(s))
    {
        "Liberation Serif"
    } else {
        "Liberation Sans"
    }
}

/// Exact OOXML numbering-label family cascade shared by profile and layout.
/// Symbol normalization must observe the same authored face in both paths.
pub fn numbering_label_family(
    ascii: Option<&str>,
    h_ansi: Option<&str>,
    theme_minor_latin: Option<&str>,
) -> String {
    ascii
        .or(h_ansi)
        .or(theme_minor_latin)
        .unwrap_or("Calibri")
        .to_string()
}

/// Maps a symbol-font code point (Symbol/Wingdings/Webdings, commonly in the
/// U+F0xx PUA range) to a Unicode equivalent for bullets. Returns `None` for a
/// non-symbol font or an unmapped code (caller fails closed).
pub fn map_symbol_char(font: &str, code: u32) -> Option<char> {
    let is_symbol_font = matches!(
        family_key(font).as_str(),
        "symbol" | "wingdings" | "wingdings 2" | "wingdings 3" | "webdings"
    );
    if !is_symbol_font && code < 0xF000 {
        return None;
    }
    let low = (code & 0xFF) as u8;
    let mapped: u32 = match low {
        0xB7 => 0x2022, // • bullet (Symbol)
        0xA7 => 0x25AA, // ▪ small black square (Wingdings)
        0xD8 => 0x27A2, // ➢ three-D arrowhead
        0xFC => 0x2713, // ✓ check mark
        0x6E => 0x25A0, // ■ black square
        0x75 => 0x25C6, // ◆ black diamond
        0xA8 => 0x25A1, // □ white square
        _ => return None,
    };
    char::from_u32(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_map_covers_core_families() {
        assert_eq!(substitute("Calibri"), Some("Carlito"));
        assert_eq!(substitute("CALIBRI LIGHT"), Some("Carlito"));
        assert_eq!(substitute("Cambria"), Some("Caladea"));
        assert_eq!(substitute("Arial"), Some("Liberation Sans"));
        assert_eq!(substitute("Times New Roman"), Some("Liberation Serif"));
        assert_eq!(substitute("Courier New"), Some("Liberation Mono"));
        assert_eq!(substitute("Comic Sans MS"), None);
    }

    #[test]
    fn fallback_classes() {
        assert_eq!(fallback_class("Consolas"), "Liberation Mono");
        assert_eq!(fallback_class("Georgia"), "Liberation Serif");
        assert_eq!(fallback_class("Verdana"), "Liberation Sans");
    }

    #[test]
    fn symbol_bullets_map() {
        assert_eq!(map_symbol_char("Symbol", 0xF0B7), Some('\u{2022}'));
        assert_eq!(map_symbol_char("Wingdings", 0xF06E), Some('\u{25A0}'));
        // Same code without the F0 offset.
        assert_eq!(map_symbol_char("Symbol", 0xB7), Some('\u{2022}'));
        // Non-symbol font, ordinary code → not remapped.
        assert_eq!(map_symbol_char("Calibri", 0x41), None);
        // Symbol font, unmapped code → None (caller fails closed).
        assert_eq!(map_symbol_char("Wingdings", 0xF041), None);
    }

    #[test]
    fn empty_db_reports_missing() {
        let db = FontDb::new();
        let r = resolve(&db, "Calibri", 400, false);
        assert!(r.face.is_none());
        assert_eq!(r.missing.as_deref(), Some("Calibri"));
    }

    /// With bundled fonts vendored (H2), Calibri resolves to Carlito with a
    /// `font_substituted` warning. Skips before the fonts are present.
    #[test]
    fn resolves_calibri_to_carlito_with_bundled_fonts() {
        let db = FontDb::with_bundled();
        if db.is_empty() {
            eprintln!("skipping: no bundled fonts vendored (human task H2)");
            return;
        }
        let r = resolve(&db, "Calibri", 400, false);
        assert!(r.face.is_some(), "Calibri should resolve via Carlito");
        assert!(
            r.warnings.iter().any(|w| w.code == "font_substituted"),
            "expected a font_substituted warning, got {:?}",
            r.warnings
        );
    }

    /// Bold Arial resolves to the bold Liberation Sans face.
    #[test]
    fn resolves_bold_weight() {
        let db = FontDb::with_bundled();
        if db.is_empty() {
            return;
        }
        let r = resolve(&db, "Arial", 700, false);
        let face = r.face.expect("Arial resolves");
        assert_eq!(db.face(face).unwrap().weight, 700);
    }
}

//! Font matching and substitution: resolve a requested `(family, weight,
//! italic)` to a concrete face, applying the metric-compatible substitution map
//! and family-class fallback, plus symbol-font bullet mapping.

use super::db::{FaceId, FontDb};
use super::requests::FontSlot;
use crate::oxml::theme::Theme;
use crate::oxml::wml::RunFonts;
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
    resolve_with_stretch(db, family, weight, italic, 5)
}

/// Resolves a face while preserving the requested OS/2 width class.
pub fn resolve_with_stretch(
    db: &FontDb,
    family: &str,
    weight: u16,
    italic: bool,
    stretch: u16,
) -> Resolved {
    let mut out = Resolved::default();
    if db.is_empty() {
        out.missing = Some(family.to_string());
        return out;
    }

    if db.resolution_mode() == crate::ResolutionMode::Strict {
        out.face = resolve_exact_with_stretch(db, family, weight, italic, stretch);
        if out.face.is_none() {
            out.missing = Some(family.to_string());
        }
        return out;
    }

    // 1 & 2: family match (embedded faces sort first via `pick_in_family`).
    if let Some(pick) = pick_in_family(db, family, weight, italic, stretch) {
        out.face = Some(pick.face);
        push_style_warning(&mut out, db, family, weight, italic, stretch, &pick);
        if pick.synthetic_italic {
            out.warnings.push(Warning::new(
                "synthetic_style_unavailable",
                family.to_string(),
            ));
        }
        return out;
    }

    // 3: metric-compatible substitution.
    for target in substitution_targets(family) {
        if let Some(pick) = pick_in_family(db, target, weight, italic, stretch) {
            out.face = Some(pick.face);
            out.warnings.push(Warning::new(
                "font_substituted",
                format!("{family} -> {target}"),
            ));
            push_style_warning(&mut out, db, target, weight, italic, stretch, &pick);
            if pick.synthetic_italic {
                out.warnings.push(Warning::new(
                    "synthetic_style_unavailable",
                    family.to_string(),
                ));
            }
            return out;
        }
    }

    // 4: metadata-guided classing from the document's font table. The
    // provider-class family wins when registered; bundled Liberation remains
    // the deterministic offline fallback.
    if let Some(classing) = db.classing_hint(family) {
        for target in metadata_targets(classing.class) {
            if let Some(pick) = pick_in_family(db, target, weight, italic, stretch) {
                out.face = Some(pick.face);
                out.warnings.push(Warning::new(
                    "font_substituted",
                    format!("{family} -> {target} (fontTable {})", classing.basis),
                ));
                push_style_warning(&mut out, db, target, weight, italic, stretch, &pick);
                if pick.synthetic_italic {
                    out.warnings.push(Warning::new(
                        "synthetic_style_unavailable",
                        family.to_string(),
                    ));
                }
                return out;
            }
        }
    }

    // 5: family-class fallback by name heuristic.
    let fallback = fallback_class(family);
    if let Some(pick) = pick_in_family(db, fallback, weight, italic, stretch) {
        out.face = Some(pick.face);
        out.warnings.push(Warning::new(
            "font_fallback",
            format!("{family} -> {fallback}"),
        ));
        out.missing = Some(family.to_string());
        return out;
    }

    // The fontless worker's only boot family is Noto Sans. It is a visible,
    // reported last resort in compatible mode and is never considered by the
    // strict branch above.
    if let Some(pick) = pick_in_family(db, "Noto Sans", weight, italic, stretch) {
        out.face = Some(pick.face);
        out.warnings.push(Warning::new(
            "font_fallback_used",
            format!("{family} -> Noto Sans"),
        ));
        out.missing = Some(family.to_string());
        return out;
    }

    // Nothing in the database covers even the fallback class.
    out.missing = Some(family.to_string());
    out
}

fn metadata_targets(class: super::requests::FontSubstitutionClass) -> &'static [&'static str] {
    use super::requests::FontSubstitutionClass::{Mono, Sans, Serif};
    match class {
        Serif => &["Tinos", "Gelasio", "Liberation Serif"],
        Sans => &["Arimo", "Carlito", "Liberation Sans"],
        Mono => &["Cousine", "Liberation Mono"],
    }
}

/// Returns an exact family, weight, and italic face without substitution or
/// synthetic styling. Embedded faces win deterministic ties.
pub fn resolve_exact(db: &FontDb, family: &str, weight: u16, italic: bool) -> Option<FaceId> {
    resolve_exact_with_stretch(db, family, weight, italic, 5)
}

/// Returns an exact family, weight, italic, and width-class face.
pub fn resolve_exact_with_stretch(
    db: &FontDb,
    family: &str,
    weight: u16,
    italic: bool,
    stretch: u16,
) -> Option<FaceId> {
    let key = family_key(family);
    db.faces()
        .iter()
        .enumerate()
        .filter(|(_, face)| {
            (family_key(&face.family_name) == key
                || face
                    .full_name
                    .as_deref()
                    .is_some_and(|name| family_key(name) == key))
                && face.weight == weight
                && face.italic == italic
                && face.stretch == stretch
        })
        .min_by_key(|(index, face)| (u8::from(!face.embedded), *index))
        .map(|(index, _)| FaceId(index))
}

/// Returns an exact face or a registered metric-compatible substitute without
/// falling through to the generic family-class or Noto fallbacks.
///
/// Host-side font providers use this narrower query before fetching an alias:
/// a registered application-owned substitute (for example Liberation Sans for
/// Arial) must win over a network-provided last resort (Arimo).
pub fn resolve_exact_or_substitute_with_stretch(
    db: &FontDb,
    family: &str,
    weight: u16,
    italic: bool,
    stretch: u16,
) -> Option<FaceId> {
    if let Some(pick) = pick_in_family(db, family, weight, italic, stretch) {
        return Some(pick.face);
    }
    if db.resolution_mode() == crate::ResolutionMode::Strict {
        return None;
    }
    substitution_targets(family).iter().find_map(|target| {
        pick_in_family(db, target, weight, italic, stretch).map(|pick| pick.face)
    })
}

/// Returns deterministic coverage-fallback candidates after `primary`.
/// Sibling faces from the primary family win first so split Unicode-subset
/// artifacts preserve the requested typeface. Other embedded faces, Noto, and
/// metric-compatible faces follow, with style distance breaking ties.
pub fn coverage_fallbacks(db: &FontDb, primary: FaceId, weight: u16, italic: bool) -> Vec<FaceId> {
    let primary_face = &db.faces()[primary.0];
    let primary_family = family_key(&primary_face.family_name);
    if db.resolution_mode() == crate::ResolutionMode::Strict {
        let mut exact_subset_faces = (0..db.len())
            .map(FaceId)
            .filter(|id| {
                let face = &db.faces()[id.0];
                *id == primary
                    || (family_key(&face.family_name) == primary_family
                        && face.weight == primary_face.weight
                        && face.italic == primary_face.italic
                        && face.stretch == primary_face.stretch)
            })
            .collect::<Vec<_>>();
        exact_subset_faces.sort_by_key(|id| (u8::from(*id != primary), id.0));
        return exact_subset_faces;
    }
    let mut ids: Vec<FaceId> = (0..db.len()).map(FaceId).collect();
    ids.retain(|id| {
        *id == primary || !family_key(&db.faces()[id.0].family_name).contains("lastresort")
    });
    ids.sort_by_key(|id| {
        let face = &db.faces()[id.0];
        let family = family_key(&face.family_name);
        let class = if *id == primary {
            0
        } else if family == primary_family {
            1
        } else if face.embedded {
            2
        } else if matches!(
            family.as_str(),
            "arimo"
                | "caladea"
                | "carlito"
                | "cousine"
                | "liberation sans"
                | "liberation serif"
                | "liberation mono"
                | "tinos"
        ) {
            // Word draws a glyph from the requested font whenever that font
            // covers it, so the metric-compatible substitute outranks the
            // Noto coverage faces; Noto (CJK, Math, Symbols) fills only the
            // glyphs the text faces genuinely lack.
            3
        } else if family.starts_with("noto") {
            4
        } else {
            5
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
        '\u{2B9A}' | '\u{27A2}' | '\u{27A4}' => &[
            '\u{25BA}', '\u{25CF}', '\u{25A0}', '\u{25CB}', '\u{25AA}', '\u{2192}', '\u{2022}',
        ],
        // Diamond ornaments retain filled/outlined intent before generic bullet.
        '\u{2756}' => &[
            '\u{2666}', '\u{25CF}', '\u{25A0}', '\u{25CB}', '\u{25AA}', '\u{2192}', '\u{2022}',
        ],
        '\u{2726}' => &[
            '\u{25C6}', '\u{25CF}', '\u{25A0}', '\u{25CB}', '\u{25AA}', '\u{2192}', '\u{2022}',
        ],
        '\u{2727}' => &[
            '\u{25C7}', '\u{25CF}', '\u{25A0}', '\u{25CB}', '\u{25AA}', '\u{2192}', '\u{2022}',
        ],
        // Dot operators and middle dots normalize to the ordinary bullet.
        '\u{2219}' | '\u{00B7}' => &[
            '\u{25CF}', '\u{25A0}', '\u{25CB}', '\u{25AA}', '\u{2192}', '\u{2022}',
        ],
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
    style_compatible: bool,
}

/// Picks the best face in `family` (case-insensitive, matching family / full /
/// name), scored by italic match then weight distance. Embedded faces win ties.
fn pick_in_family(
    db: &FontDb,
    family: &str,
    weight: u16,
    italic: bool,
    stretch: u16,
) -> Option<Pick> {
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
        // then prefer embedded faces. Compatible mode deliberately preserves
        // the pre-fontless face tie-break; strict/exact matching enforces the
        // requested stretch above without moving legacy native fidelity.
        let score = (if italic_match { 0 } else { 100_000 })
            + weight_dist
            + if face.embedded { -1 } else { 0 };
        let id = FaceId(i);
        match &best {
            Some((_, best_score, _)) if *best_score <= score => {}
            _ => best = Some((id, score, italic_match)),
        }
    }
    best.map(|(face, _, italic_match)| {
        let selected = &db.faces()[face.0];
        Pick {
            face,
            synthetic_italic: italic && !italic_match,
            style_compatible: selected.weight != weight
                || selected.italic != italic
                || selected.stretch != stretch,
        }
    })
}

fn push_style_warning(
    out: &mut Resolved,
    db: &FontDb,
    family: &str,
    weight: u16,
    italic: bool,
    stretch: u16,
    pick: &Pick,
) {
    if !pick.style_compatible {
        return;
    }
    let selected = &db.faces()[pick.face.0];
    out.warnings.push(Warning::new(
        "font_style_compatible",
        format!(
            "{family} weight={weight} italic={italic} stretch={stretch} -> weight={} italic={} stretch={}",
            selected.weight, selected.italic, selected.stretch
        ),
    ));
}

/// Normalizes a family name for case-insensitive comparison.
/// Normalizes a family name for case- and spacing-insensitive matching.
pub fn family_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// App-owned metric-compatible targets followed by provider-only fallbacks.
fn substitution_targets(family: &str) -> &'static [&'static str] {
    match family_key(family).as_str() {
        "calibri" | "calibri light" => &["Carlito"],
        // Cambria Math shares Cambria's letterform metrics; Caladea is the
        // metric-compatible clone for both. Operator coverage still comes
        // from the symbol/math coverage fallbacks.
        "cambria" | "cambria math" => &["Caladea"],
        "arial" | "helvetica" | "segoe ui" => &["Liberation Sans", "Arimo"],
        "times new roman" | "times" => &["Liberation Serif", "Tinos"],
        "courier new" | "courier" => &["Liberation Mono", "Cousine"],
        "liberation sans" => &["Arimo"],
        "liberation serif" => &["Tinos"],
        "liberation mono" => &["Cousine"],
        "georgia" => &["Gelasio"],
        // Symbol families substitute for each other before falling back to a
        // Latin text face for metrics; glyph coverage for symbol codepoints
        // comes from whichever registered face covers them (see
        // `coverage_fallbacks`), so Carlito stays last as the metric anchor.
        "noto sans symbols" => &["Noto Sans Symbols 2", "Noto Sans Math", "Carlito"],
        "noto sans symbols 2" => &["Noto Sans Symbols", "Noto Sans Math", "Carlito"],
        "noto sans math" => &["Noto Sans Symbols 2", "Carlito"],
        // Common CJK faces have no metric-compatible OFL clone; the pan-CJK
        // Noto regional faces are the practical substitutes (the same policy
        // the retired LibreOffice backend applied). Hosts supply these via a
        // font provider; nothing is bundled. Serif legacy families (SimSun,
        // MingLiU, Mincho, Batang, and the Kai/FangSong brush faces) prefer
        // the Noto Serif CJK face and fall back to the Sans face when only
        // that is registered, so Sans-only hosts keep converting.
        // Localized names alias the classic Windows face first (registered by
        // hosts with the real fonts, e.g. the fidelity harness on a machine
        // with Office), then the free substitution chain.
        "simsun" | "宋体" => &["SimSun", "FandolSong", "Noto Serif CJK SC", "Noto Sans CJK SC"],
        "nsimsun" | "simsun-extb" | "新宋体" => {
            &["NSimSun", "SimSun", "FandolSong", "Noto Serif CJK SC", "Noto Sans CJK SC"]
        }
        "kaiti" | "楷体" => &["KaiTi", "Noto Serif CJK SC", "Noto Sans CJK SC"],
        "fangsong" | "仿宋" => &["FangSong", "Noto Serif CJK SC", "Noto Sans CJK SC"],
        "simhei" | "黑体" => &["SimHei", "Noto Sans CJK SC"],
        "microsoft yahei" | "microsoft yahei ui" | "微软雅黑" => {
            &["Microsoft YaHei", "Noto Sans CJK SC"]
        }
        "dengxian" | "等线" => &["DengXian", "Noto Sans CJK SC"],
        "mingliu" | "mingliu-extb" | "細明體" => {
            &["MingLiU", "細明體", "Noto Serif CJK TC", "Noto Sans CJK TC"]
        }
        "pmingliu" | "pmingliu-extb" | "新細明體" => {
            &["PMingLiU", "新細明體", "MingLiU", "細明體", "Noto Serif CJK TC", "Noto Sans CJK TC"]
        }
        "dfkai-sb" | "標楷體" => &["DFKai-SB", "Noto Serif CJK TC", "Noto Sans CJK TC"],
        "microsoft jhenghei" | "microsoft jhenghei ui" | "微軟正黑體" => {
            &["Microsoft JhengHei", "Noto Sans CJK TC"]
        }
        "ms mincho" | "ms 明朝" | "ＭＳ 明朝" => {
            &["MS Mincho", "Noto Serif CJK JP", "Noto Sans CJK JP"]
        }
        "ms pmincho" | "ms ｐ明朝" | "ＭＳ Ｐ明朝" => {
            &["MS PMincho", "MS Mincho", "Noto Serif CJK JP", "Noto Sans CJK JP"]
        }
        "yu mincho" | "游明朝" => &["Yu Mincho", "Noto Serif CJK JP", "Noto Sans CJK JP"],
        "ms gothic" | "ms ui gothic" | "ms ゴシック" | "ＭＳ ゴシック" => {
            &["MS Gothic", "IPAexGothic", "Noto Sans CJK JP"]
        }
        "ms pgothic" | "ms ｐゴシック" | "ＭＳ Ｐゴシック" => {
            &["MS PGothic", "MS Gothic", "IPAexGothic", "Noto Sans CJK JP"]
        }
        "yu gothic" | "yu gothic ui" | "meiryo" | "meiryo ui" | "游ゴシック" | "メイリオ" => {
            &["Noto Sans CJK JP"]
        }
        "batang" | "바탕" => &["Batang", "Noto Serif CJK KR", "Noto Sans CJK KR"],
        "batangche" | "바탕체" => &["BatangChe", "Batang", "Noto Serif CJK KR", "Noto Sans CJK KR"],
        "gungsuh" | "궁서" => &["Gungsuh", "Batang", "Noto Serif CJK KR", "Noto Sans CJK KR"],
        "gulim" | "굴림" => &["Gulim", "Noto Sans CJK KR"],
        "gulimche" | "굴림체" => &["GulimChe", "Gulim", "Noto Sans CJK KR"],
        "dotum" | "돋움" => &["Dotum", "Gulim", "Noto Sans CJK KR"],
        "dotumche" | "돋움체" => &["DotumChe", "Dotum", "Noto Sans CJK KR"],
        "malgun gothic" | "malgun gothic semilight" | "맑은 고딕" => {
            &["Malgun Gothic", "Noto Sans CJK KR"]
        }
        _ => &[],
    }
}

/// The pan-CJK Noto face a family substitutes to, if any. These faces have
/// no CDN catalog artifacts: hosts and the fidelity harness provision the
/// repo-shipped OTFs (`pdf-core/wasm/fonts/NotoSansCJK*.otf`) instead, so
/// providers need to recognize the mapping without duplicating the table.
pub fn pan_cjk_substitution_target(family: &str) -> Option<&'static str> {
    const PAN_CJK: [&str; 10] = [
        "Noto Sans CJK SC",
        "Noto Sans CJK TC",
        "Noto Sans CJK JP",
        "Noto Sans CJK KR",
        "Noto Serif CJK SC",
        "Noto Serif CJK TC",
        "Noto Serif CJK JP",
        "Noto Serif CJK KR",
        "IPAexGothic",
        "FandolSong",
    ];
    let key = family_key(family);
    PAN_CJK
        .iter()
        .copied()
        .find(|target| family_key(target) == key)
        .or_else(|| {
            substitution_targets(family)
                .iter()
                .copied()
                .find(|target| PAN_CJK.contains(target))
        })
}

/// Whether a family is one of the pan-CJK substitution targets (the Noto
/// CJK faces plus the metric-compatible legacy clones like IPAexGothic).
pub fn pan_cjk_face(family: &str) -> bool {
    let key = family_key(family);
    key.starts_with("noto sans cjk")
        || key.starts_with("noto serif cjk")
        || key == "ipaexgothic"
        || key == "fandolsong"
}

/// Legacy fixed-pitch CJK families whose ASCII glyphs advance exactly half
/// an em in Word (SimSun, MingLiU, MS Mincho/Gothic, and the Korean -che
/// faces). Substituted rendering must keep those advances or every Latin
/// fragment — and any AutoFit table measured from it — drifts wide. The
/// proportional variants (MS P*, PMingLiU, Batang, Gulim, Dotum) keep their
/// substitute's own Latin advances.
pub fn halfwidth_ascii_legacy_family(family: &str) -> bool {
    matches!(
        family_key(family).as_str(),
        "simsun"
            | "nsimsun"
            | "simsun-extb"
            | "simhei"
            | "kaiti"
            | "fangsong"
            | "mingliu"
            | "mingliu-extb"
            | "ms mincho"
            | "ms gothic"
            | "ms 明朝"
            | "ms ゴシック"
            | "ＭＳ 明朝"
            | "ＭＳ ゴシック"
            | "batangche"
            | "gulimche"
            | "dotumche"
            | "gungsuhche"
    )
}

/// The classic Windows East Asian faces whose line boxes Word builds with
/// the hhea line gap centered around the em square (baseline at
/// `winAscent + lineGap/2` from the line top). Applies when the real font
/// is registered; substituted rendering models the same convention through
/// [`legacy_cjk_line_metrics_em`].
pub fn legacy_cjk_centered_family(family: &str) -> bool {
    matches!(
        family_key(family).as_str(),
        "simsun"
            | "nsimsun"
            | "simsun-extb"
            | "simhei"
            | "kaiti"
            | "fangsong"
            | "mingliu"
            | "pmingliu"
            | "mingliu-extb"
            | "pmingliu-extb"
            | "dfkai-sb"
            | "ms mincho"
            | "ms pmincho"
            | "ms gothic"
            | "ms pgothic"
            | "ms ui gothic"
            | "batang"
            | "batangche"
            | "gungsuh"
            | "gungsuhche"
            | "gulim"
            | "gulimche"
            | "dotum"
            | "dotumche"
    )
}

/// The legacy East Asian class of a family (real face or substitute), used
/// to apply Word's measured per-size line-box splits.
pub fn legacy_cjk_class(family: &str) -> Option<crate::fonts::LegacyCjkClass> {
    use crate::fonts::LegacyCjkClass::*;
    Some(match family_key(family).as_str() {
        "simsun" | "nsimsun" | "simsun-extb" | "fandolsong" | "noto sans cjk sc"
        | "noto serif cjk sc" => Sc,
        "mingliu" | "pmingliu" | "mingliu-extb" | "pmingliu-extb" | "細明體" | "新細明體"
        | "noto sans cjk tc" | "noto serif cjk tc" => Tc,
        "ms gothic" | "ms pgothic" | "ms ui gothic" | "ms mincho" | "ms pmincho"
        | "ipaexgothic" | "noto sans cjk jp" | "noto serif cjk jp" => Jp,
        "batang" | "batangche" | "gungsuh" | "gungsuhche" | "noto sans cjk kr"
        | "noto serif cjk kr" => Kr,
        _ => return None,
    })
}

/// Line metrics (ascent, descent, line gap in em) presented for a pan-CJK
/// face. The legacy CJK families they replace (SimSun, MS Mincho, Batang,
/// MingLiU, …) design to a 1.0em body that Word grows with the growth
/// centered around the glyph box (baseline at `winAscent + gap/2` from the
/// line top — calibrated against Word-exported CJK ground truths,
/// 2026-07-17). Noto Sans CJK's own 1.48em win metrics would reflow every
/// substituted document, so rendering through a pan-CJK face keeps the
/// legacy convention: the below-baseline half of the growth is folded into
/// the descent so `ascent + gap` stays the baseline offset. The MingLiU
/// lineage behind the TC target carries a larger hhea gap than the
/// SimSun-class 1.30em faces, measured as a ~1.316em line with a lower
/// baseline share.
pub fn legacy_cjk_line_metrics_em(family: &str) -> (f64, f64, f64) {
    // Encoded under the GDI convention (baseline at plain ascent, gap
    // below): identical box ascents and totals as the earlier gap-above
    // tuples they replace.
    matches!(
        family_key(family).as_str(),
        "noto sans cjk tc" | "noto serif cjk tc"
    )
    .then_some((1.1006, 0.215, 0.0))
    .unwrap_or((1.009375, 0.290625, 0.0))
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

/// Resolves one effective WML script slot through direct and theme-backed
/// `w:rFonts` values, using the same cascade in profile and layout.
pub fn wml_run_family(fonts: &RunFonts, slot: FontSlot, theme: &Theme) -> String {
    let direct = match slot {
        FontSlot::Ascii => fonts.ascii.clone().or_else(|| fonts.h_ansi.clone()),
        FontSlot::HAnsi => fonts.h_ansi.clone().or_else(|| fonts.ascii.clone()),
        FontSlot::EastAsia => fonts
            .east_asia
            .clone()
            .or_else(|| fonts.h_ansi.clone())
            .or_else(|| fonts.ascii.clone()),
        FontSlot::Cs => fonts
            .cs
            .clone()
            .or_else(|| fonts.ascii.clone())
            .or_else(|| fonts.h_ansi.clone()),
    };
    if let Some(family) = direct {
        return family;
    }
    let reference = match slot {
        FontSlot::Ascii => fonts
            .ascii_theme
            .as_deref()
            .or(fonts.h_ansi_theme.as_deref()),
        FontSlot::HAnsi => fonts
            .h_ansi_theme
            .as_deref()
            .or(fonts.ascii_theme.as_deref()),
        FontSlot::EastAsia => fonts
            .east_asia_theme
            .as_deref()
            .or(fonts.h_ansi_theme.as_deref())
            .or(fonts.ascii_theme.as_deref()),
        FontSlot::Cs => fonts
            .cs_theme
            .as_deref()
            .or(fonts.ascii_theme.as_deref())
            .or(fonts.h_ansi_theme.as_deref()),
    };
    reference
        .and_then(|reference| theme.resolve_font_reference(reference))
        .or_else(|| theme.minor_latin.clone())
        .unwrap_or_else(|| "Calibri".to_string())
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
        assert_eq!(substitution_targets("Calibri"), &["Carlito"]);
        assert_eq!(substitution_targets("CALIBRI LIGHT"), &["Carlito"]);
        assert_eq!(substitution_targets("Cambria"), &["Caladea"]);
        assert_eq!(substitution_targets("Arial"), &["Liberation Sans", "Arimo"]);
        assert_eq!(
            substitution_targets("Times New Roman"),
            &["Liberation Serif", "Tinos"]
        );
        assert_eq!(
            substitution_targets("Courier New"),
            &["Liberation Mono", "Cousine"]
        );
        assert_eq!(substitution_targets("Georgia"), &["Gelasio"]);
        assert_eq!(
            substitution_targets("Noto Sans Symbols"),
            &["Noto Sans Symbols 2", "Noto Sans Math", "Carlito"]
        );
        assert_eq!(
            substitution_targets("Noto Sans Symbols 2"),
            &["Noto Sans Symbols", "Noto Sans Math", "Carlito"]
        );
        assert_eq!(
            substitution_targets("Noto Sans Math"),
            &["Noto Sans Symbols 2", "Carlito"]
        );
        assert!(substitution_targets("Comic Sans MS").is_empty());
    }

    #[test]
    fn substitution_map_covers_common_cjk_families() {
        assert_eq!(
            substitution_targets("SimSun"),
            &["SimSun", "FandolSong", "Noto Serif CJK SC", "Noto Sans CJK SC"]
        );
        assert_eq!(
            substitution_targets("Microsoft YaHei"),
            &["Microsoft YaHei", "Noto Sans CJK SC"]
        );
        assert_eq!(
            substitution_targets("PMingLiU"),
            &[
                "PMingLiU",
                "新細明體",
                "MingLiU",
                "細明體",
                "Noto Serif CJK TC",
                "Noto Sans CJK TC"
            ]
        );
        assert_eq!(
            substitution_targets("Microsoft JhengHei"),
            &["Microsoft JhengHei", "Noto Sans CJK TC"]
        );
        assert_eq!(
            substitution_targets("MS Mincho"),
            &["MS Mincho", "Noto Serif CJK JP", "Noto Sans CJK JP"]
        );
        assert_eq!(substitution_targets("Yu Gothic"), &["Noto Sans CJK JP"]);
        assert_eq!(
            substitution_targets("Malgun Gothic"),
            &["Malgun Gothic", "Noto Sans CJK KR"]
        );
        assert_eq!(
            substitution_targets("Batang"),
            &["Batang", "Noto Serif CJK KR", "Noto Sans CJK KR"]
        );
    }

    #[test]
    fn pan_cjk_targets_are_exposed_for_providers() {
        assert_eq!(pan_cjk_substitution_target("SimSun"), Some("FandolSong"));
        assert_eq!(
            pan_cjk_substitution_target("MS Mincho"),
            Some("Noto Serif CJK JP")
        );
        assert_eq!(
            pan_cjk_substitution_target("MS Gothic"),
            Some("IPAexGothic")
        );
        assert_eq!(
            pan_cjk_substitution_target("batang"),
            Some("Noto Serif CJK KR")
        );
        assert_eq!(
            pan_cjk_substitution_target("noto sans cjk tc"),
            Some("Noto Sans CJK TC")
        );
        assert_eq!(
            pan_cjk_substitution_target("Noto Serif CJK KR"),
            Some("Noto Serif CJK KR")
        );
        assert_eq!(pan_cjk_substitution_target("Calibri"), None);
        assert_eq!(pan_cjk_substitution_target("Noto Sans Math"), None);
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

    #[test]
    fn arial_prefers_liberation_over_the_provider_alias_when_both_are_registered() {
        let mut db = FontDb::new();
        db.register_embedded(
            "Arimo",
            include_bytes!("../../fonts/carlito/Carlito-Regular.ttf").to_vec(),
            false,
            false,
        );
        db.register(include_bytes!("../../fonts/liberation/LiberationSans-Regular.ttf").to_vec())
            .expect("register Liberation Sans");

        let resolved = resolve(&db, "Arial", 400, false);
        let face = resolved.face.expect("Arial resolves");
        assert_eq!(db.face(face).unwrap().family_name, "Liberation Sans");
    }

    #[test]
    fn arial_uses_arimo_as_a_reported_last_resort_without_liberation() {
        let mut db = FontDb::new();
        db.register_embedded(
            "Arimo",
            include_bytes!("../../fonts/carlito/Carlito-Regular.ttf").to_vec(),
            false,
            false,
        );

        let resolved = resolve(&db, "Arial", 400, false);
        let face = resolved.face.expect("Arial resolves through Arimo");
        assert_eq!(db.face(face).unwrap().family_name, "Arimo");
        assert!(resolved.warnings.iter().any(|warning| {
            warning.code == "font_substituted" && warning.detail == "Arial -> Arimo"
        }));
    }

    #[test]
    fn metadata_classing_selects_the_provider_family_and_names_its_basis() {
        let mut db = FontDb::new();
        db.register_embedded(
            "Tinos",
            include_bytes!("../../fonts/carlito/Carlito-Regular.ttf").to_vec(),
            false,
            false,
        );
        db.register(include_bytes!("../../fonts/liberation/LiberationSans-Regular.ttf").to_vec())
            .expect("register unrelated sans fallback");
        db.set_classing_hint(
            "Mystery Text",
            crate::fonts::FontClassing {
                class: crate::fonts::FontSubstitutionClass::Serif,
                basis: "family=roman".into(),
            },
        );

        let resolved = resolve(&db, "Mystery Text", 400, false);
        let face = resolved.face.expect("metadata-classed family resolves");
        assert_eq!(db.face(face).unwrap().family_name, "Tinos");
        assert!(resolved.warnings.iter().any(|warning| {
            warning.code == "font_substituted" && warning.detail.contains("fontTable family=roman")
        }));
    }

    #[test]
    fn coverage_fallbacks_keep_exact_family_siblings_ahead_of_other_families() {
        let mut db = FontDb::new();
        db.register(include_bytes!("../../fonts/liberation/LiberationSans-Regular.ttf").to_vec())
            .expect("register unrelated fallback");
        db.register(include_bytes!("../../fonts/carlito/Carlito-Regular.ttf").to_vec())
            .expect("register primary subset face");
        db.register(include_bytes!("../../fonts/carlito/Carlito-Regular.ttf").to_vec())
            .expect("register exact-family sibling subset face");

        let fallbacks = coverage_fallbacks(&db, FaceId(1), 400, false);
        assert_eq!(fallbacks[..3], [FaceId(1), FaceId(2), FaceId(0)]);
    }

    #[test]
    fn strict_coverage_keeps_only_exact_style_siblings() {
        let mut db = FontDb::new();
        db.register(include_bytes!("../../fonts/liberation/LiberationSans-Regular.ttf").to_vec())
            .expect("register unrelated fallback");
        db.register(include_bytes!("../../fonts/carlito/Carlito-Regular.ttf").to_vec())
            .expect("register primary subset face");
        db.register(include_bytes!("../../fonts/carlito/Carlito-Regular.ttf").to_vec())
            .expect("register exact-family sibling subset face");
        db.register(include_bytes!("../../fonts/carlito/Carlito-Bold.ttf").to_vec())
            .expect("register different-style family face");
        db.set_resolution_mode(crate::ResolutionMode::Strict);

        assert_eq!(
            coverage_fallbacks(&db, FaceId(1), 400, false),
            [FaceId(1), FaceId(2)]
        );
    }
}

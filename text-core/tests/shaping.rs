use std::path::{Path, PathBuf};

use text_core::{
    Direction, Face, FeatureConfig, ShapeError, bidi_base_direction, bidi_visual_order, itemize,
    preflight_coverage, shape_text, shape_text_with_features,
};

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(path)
}

fn face(path: impl AsRef<Path>) -> (Vec<u8>, u32) {
    (std::fs::read(path).expect("read test font"), 0)
}

fn semantic_text(runs: &[text_core::ShapedRun]) -> String {
    let mut out = String::new();
    for run in runs {
        for glyph in &run.glyphs {
            if let Some((start, end)) = glyph.unicode_range {
                out.push_str(&run.source[start..end]);
            }
        }
    }
    out
}

#[test]
fn bidi_itemization_places_hebrew_run_rtl_in_mixed_line() {
    let runs = itemize("abc שלום 123");
    assert!(
        runs.iter()
            .any(|run| run.direction == Direction::Rtl && run.text.contains("שלום"))
    );
    let (bytes, index) = face(repo(
        "office-core/fonts/liberation/LiberationSans-Regular.ttf",
    ));
    let faces = [Face::new(&bytes, index).unwrap()];
    let shaped = shape_text("abc שלום 123", &faces).unwrap();
    let hebrew = shaped
        .iter()
        .find(|run| run.source.contains("שלום"))
        .unwrap();
    assert!(
        hebrew
            .glyphs
            .windows(2)
            .all(|pair| pair[0].cluster >= pair[1].cluster)
    );
}

#[test]
fn bidi_visual_order_reorders_arabic_sentence_by_uax9_levels() {
    let logical = "هذا نص تجريبي باللغة العربية. يحتوي على أرقام عربية ٠١٢٣ وحروف متصلة.";
    assert_eq!(
        bidi_visual_order(logical),
        ".ةلصتم فورحو ٠١٢٣ ةيبرع ماقرأ ىلع يوتحي .ةيبرعلا ةغللاب يبيرجت صن اذه"
    );
}

#[test]
fn bidi_visual_order_preserves_ltr_prefix_and_digits_in_mixed_line() {
    let logical = "Total: 123 ريال";
    assert_eq!(bidi_visual_order(logical), "Total: 123 لاير");
}

#[test]
fn bidi_base_direction_uses_the_first_strong_paragraph_character() {
    assert_eq!(bidi_base_direction("Total: 123 ريال"), Direction::Ltr);
    assert_eq!(bidi_base_direction("ريال 123 total"), Direction::Rtl);
    assert_eq!(bidi_base_direction(""), Direction::Ltr);
}

#[test]
fn coverage_reports_exact_uncovered_scalars_in_codepoint_order() {
    let (bytes, index) = face(repo(
        "office-core/fonts/liberation/LiberationSans-Regular.ttf",
    ));
    let faces = [Face::new(&bytes, index).unwrap()];
    let missing = preflight_coverage("Latin 中 🔒", &faces);
    assert_eq!(missing, vec!['中', '🔒']);
}

#[test]
fn fallback_never_switches_inside_combining_cluster() {
    let (bytes, index) = face(repo(
        "office-core/fonts/liberation/LiberationSans-Regular.ttf",
    ));
    let faces = [Face::new(&bytes, index).unwrap()];
    let shaped = shape_text("a\u{0301}b", &faces).unwrap();
    assert_eq!(shaped.len(), 1);
    assert!(shaped[0].glyphs.iter().all(|glyph| glyph.face_index == 0));
    assert!(shaped[0].glyphs.iter().any(|glyph| glyph.cluster == 0));
}

#[test]
fn pdf_defaults_keep_kerning_while_office_can_disable_it() {
    let (bytes, index) = face(repo(
        "office-core/fonts/liberation/LiberationSans-Regular.ttf",
    ));
    let faces = [Face::new(&bytes, index).unwrap()];
    let defaults: i32 = shape_text("AV", &faces)
        .unwrap()
        .iter()
        .map(|run| run.advance)
        .sum();
    let unkerned: i32 = shape_text_with_features(
        "AV",
        &faces,
        FeatureConfig::default().kern(false).liga(false),
    )
    .unwrap()
    .iter()
    .map(|run| run.advance)
    .sum();

    assert!(
        unkerned > defaults,
        "AV must be wider when kern is disabled"
    );
}

#[test]
fn astral_lock_shapes_without_replacement_character() {
    let (bytes, index) = face(repo("pdf-core/wasm/pkg/fonts/NotoEmoji-Variable.ttf"));
    let faces = [Face::new(&bytes, index).unwrap()];
    let shaped = shape_text("🔒", &faces).unwrap();
    assert_eq!(shaped.len(), 1);
    assert!(!shaped[0].glyphs.is_empty());
    assert_eq!(shaped[0].source, "🔒");
    assert_eq!(shaped[0].glyphs[0].cluster, 0);
}

#[test]
fn intact_emoji_sequence_uses_later_covering_face_before_degrading() {
    let (cjk_bytes, cjk_index) = face(repo("pdf-core/wasm/pkg/fonts/NotoSansCJKsc-Regular.otf"));
    let (emoji_bytes, emoji_index) = face(repo("pdf-core/wasm/pkg/fonts/NotoEmoji-Variable.ttf"));
    let faces = [
        Face::new(&cjk_bytes, cjk_index).unwrap(),
        Face::new(&emoji_bytes, emoji_index).unwrap(),
    ];

    let shaped = shape_text("☝🏽", &faces).unwrap();

    assert_eq!(shaped.iter().map(|run| run.glyphs.len()).sum::<usize>(), 1);
    assert!(
        shaped
            .iter()
            .flat_map(|run| &run.glyphs)
            .all(|glyph| glyph.face_index == 1)
    );
    assert_eq!(semantic_text(&shaped), "☝🏽");
}

#[test]
fn emoji_fallback_drops_vs16_but_preserves_original_semantics() {
    let (bytes, index) = face(repo(
        "office-core/fonts/liberation/LiberationSans-Regular.ttf",
    ));
    let faces = [Face::new(&bytes, index).unwrap()];

    let shaped = shape_text("©️", &faces).unwrap();

    assert!(
        shaped
            .iter()
            .flat_map(|run| &run.glyphs)
            .all(|glyph| glyph.glyph_id != 0)
    );
    assert_eq!(semantic_text(&shaped), "©️");
    assert!(shaped.iter().all(|run| run.source == "©️"));
}

#[test]
fn emoji_fallback_drops_skin_tone_but_preserves_original_semantics() {
    let (bytes, index) = face(repo("pdf-core/wasm/pkg/fonts/NotoSansCJKsc-Regular.otf"));
    let faces = [Face::new(&bytes, index).unwrap()];

    let shaped = shape_text("☝🏽", &faces).unwrap();

    assert!(
        shaped
            .iter()
            .flat_map(|run| &run.glyphs)
            .all(|glyph| glyph.glyph_id != 0)
    );
    assert_eq!(semantic_text(&shaped), "☝🏽");
    assert!(shaped.iter().all(|run| run.source == "☝🏽"));
}

#[test]
fn emoji_fallback_splits_zwj_components_and_maps_original_once() {
    let (bytes, index) = face(repo("pdf-core/wasm/pkg/fonts/NotoSansCJKsc-Regular.otf"));
    let faces = [Face::new(&bytes, index).unwrap()];

    let shaped = shape_text("☝‍☝", &faces).unwrap();
    let glyphs: Vec<_> = shaped.iter().flat_map(|run| &run.glyphs).collect();

    assert_eq!(glyphs.iter().filter(|glyph| glyph.x_advance > 0).count(), 2);
    assert!(glyphs.iter().all(|glyph| glyph.glyph_id != 0));
    assert_eq!(
        glyphs
            .iter()
            .filter(|glyph| glyph.unicode_range.is_some())
            .count(),
        1
    );
    assert_eq!(semantic_text(&shaped), "☝‍☝");
    assert!(shaped.iter().all(|run| run.source == "☝‍☝"));
}

#[test]
fn absent_extended_pictograph_reports_real_codepoint() {
    let (bytes, index) = face(repo("pdf-core/wasm/pkg/fonts/NotoEmoji-Variable.ttf"));
    let faces = [Face::new(&bytes, index).unwrap()];

    assert_eq!(
        shape_text("🫩", &faces).unwrap_err(),
        ShapeError::MissingGlyphs(vec!['🫩'])
    );
}

#[test]
fn missing_coverage_is_typed() {
    let (bytes, index) = face(repo(
        "office-core/fonts/liberation/LiberationSans-Regular.ttf",
    ));
    let faces = [Face::new(&bytes, index).unwrap()];
    assert_eq!(
        shape_text("مرحبا", &faces).unwrap_err(),
        ShapeError::MissingGlyphs(vec!['م', 'ر', 'ح', 'ب', 'ا'])
    );
}

#[test]
fn office_feature_profile_keeps_required_arabic_substitutions() {
    let (bytes, index) = face(repo("pdf-core/wasm/pkg/fonts/NotoSansArabic-Variable.ttf"));
    let faces = [Face::new(&bytes, index).unwrap()];
    let text = "لا";
    let shaped = shape_text_with_features(
        text,
        &faces,
        FeatureConfig::default().kern(false).liga(false),
    )
    .unwrap();
    let parsed = ttf_parser::Face::parse(&bytes, 0).unwrap();
    let cmap_gids: Vec<_> = text
        .chars()
        .map(|ch| parsed.glyph_index(ch).unwrap().0)
        .collect();
    let shaped_gids: Vec<_> = shaped
        .iter()
        .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.glyph_id))
        .collect();
    assert_ne!(
        shaped_gids, cmap_gids,
        "required Arabic substitutions must remain active"
    );
}

#[test]
fn office_feature_profile_keeps_devanagari_conjunct_shaping() {
    let (bytes, index) = face(repo(
        "pdf-core/wasm/pkg/fonts/NotoSansDevanagari-Variable.ttf",
    ));
    let faces = [Face::new(&bytes, index).unwrap()];
    let text = "क्ष";
    let shaped = shape_text_with_features(
        text,
        &faces,
        FeatureConfig::default().kern(false).liga(false),
    )
    .unwrap();
    assert!(shaped.iter().map(|run| run.glyphs.len()).sum::<usize>() < text.chars().count());
}

#[test]
fn office_feature_profile_keeps_thai_mark_positioning() {
    let (bytes, index) = face(repo("pdf-core/wasm/pkg/fonts/NotoSansThai-Variable.ttf"));
    let faces = [Face::new(&bytes, index).unwrap()];
    let shaped = shape_text_with_features(
        "กิ",
        &faces,
        FeatureConfig::default().kern(false).liga(false),
    )
    .unwrap();
    let clusters: Vec<_> = shaped[0].glyphs.iter().map(|glyph| glyph.cluster).collect();
    assert!(clusters.windows(2).any(|pair| pair[0] == pair[1]));
    assert!(shaped[0].glyphs.iter().any(|glyph| glyph.x_advance == 0));
}

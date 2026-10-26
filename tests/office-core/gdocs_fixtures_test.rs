use std::path::Path;

use office_core::layout::{Item, WordDoc, layout_docx};
use office_core::{ConvertOptions, OfficeFormat, convert, profile};

const DOCUMENTED_WARNING_CODES: &[&str] = &[
    "bullet_substituted",
    "floating_table_block_fallback",
    "font_fallback",
    "font_substituted",
];

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/corpus/docx")
        .join(name)
        .join("source.docx");
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn gdocs_anchors_toc_has_exact_word_page_count() {
    let bytes = fixture("gdocs-anchors-toc");
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile anchors/TOC fixture");
    assert!(
        predicted.supported && predicted.reasons.is_empty(),
        "anchors/TOC profile reasons: {:?}",
        predicted.reasons
    );
    assert!(
        predicted
            .warnings
            .iter()
            .all(|warning| DOCUMENTED_WARNING_CODES.contains(&warning.code)),
        "undocumented anchors/TOC warnings: {:?}",
        predicted.warnings
    );

    let output = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("convert anchors/TOC fixture");
    assert_eq!(output.page_count, 10, "must match the Word export");
}

#[test]
fn gdocs_anchors_toc_retains_numbering_levels_and_page_ten_origins() {
    let bytes = fixture("gdocs-anchors-toc");
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).expect("open anchors fixture");
    for (ilvl, expected_left, expected_hanging) in [
        (0, 36.0, 18.0),
        (1, 57.6, 21.6),
        (2, 79.2, 25.2),
        (3, 104.4, 32.4),
    ] {
        let level = doc
            .numbering
            .resolve(3, ilvl)
            .unwrap_or_else(|| panic!("resolve numId=3 ilvl={ilvl}"));
        assert_eq!(level.indent.left_pt, Some(expected_left));
        assert_eq!(level.indent.hanging_pt, Some(expected_hanging));
    }
    let roman = doc.numbering.resolve(5, 0).expect("resolve Roman level");
    assert_eq!(roman.indent.left_pt, Some(57.6));
    assert_eq!(roman.indent.hanging_pt, Some(18.0));

    let (laid, _) = layout_docx(&doc).expect("layout anchors fixture");
    let runs = laid.pages[9]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => Some((
                run.glyphs.iter().map(|glyph| glyph.ch).collect::<String>(),
                positioned.x_pt,
                run.glyphs.iter().map(|glyph| glyph.advance_pt).sum::<f64>(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let roman_index = runs
        .iter()
        .position(|(text, ..)| text == "i.")
        .expect("page-10 i. label");
    let (_, roman_x, roman_width) = &runs[roman_index];
    let (_, roman_body_x, _) = runs
        .iter()
        .skip(roman_index + 1)
        .find(|(text, ..)| text == "One")
        .expect("page-10 Roman body");
    assert!(
        roman_body_x - (roman_x + roman_width) >= 17.8,
        "Roman label must retain the truth's tab gap: {runs:?}"
    );

    let label_111 = runs
        .iter()
        .position(|(text, ..)| text == "1.1.1.")
        .expect("1.1.1 label");
    let label_112 = runs
        .iter()
        .position(|(text, ..)| text == "1.1.2.")
        .expect("1.1.2 label");
    let body_111_x = runs[label_111 + 1].1;
    let body_112_x = runs[label_112 + 1].1;
    let continuation_112_x = runs[label_112 + 2].1;
    assert!(
        (body_111_x - 180.0).abs() <= 0.15
            && (body_112_x - 180.0).abs() <= 0.15
            && (continuation_112_x - 151.2).abs() <= 0.15,
        "level-2 labels must use the overflow tab while continuations retain the level-2 text indent: {runs:?}"
    );
}

#[test]
fn gdocs_tables_longdoc_has_exact_word_page_count() {
    let bytes = fixture("gdocs-tables-longdoc");
    let output = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("convert tables/longdoc fixture");
    assert_eq!(output.page_count, 44, "must match the Word export");
}

#[test]
fn gdocs_columns_has_exact_word_page_count() {
    let bytes = fixture("gdocs-columns");
    let output = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("convert columns fixture");
    assert_eq!(output.page_count, 3, "must match the Word export");
}

#[test]
fn japanese_inline_image_transition_matches_word_logical_object_box() {
    let bytes = fixture("japanese");
    let mut options = ConvertOptions::default();
    let word_fonts = Path::new("/Applications/Microsoft Word.app/Contents/Resources/DFonts");
    if word_fonts.is_dir() {
        options.font_dirs.push(word_fonts.to_path_buf());
    }
    options.font_bytes.push(
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../packages/doc-yantra-fonts/fonts/NotoSansCJKjp-Regular.otf"),
        )
        .expect("read committed Japanese fallback face"),
    );
    let doc = WordDoc::open(&bytes, &options).expect("open Japanese fixture");
    let (laid, _) = layout_docx(&doc).expect("layout Japanese fixture");

    let mut baselines = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| {
            matches!(positioned.item, Item::Glyphs(_)).then_some(positioned.y_pt)
        })
        .collect::<Vec<_>>();
    baselines.sort_by(f64::total_cmp);
    baselines.dedup_by(|left, right| (*left - *right).abs() <= 0.01);

    let largest_gap = baselines
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max_by(f64::total_cmp)
        .expect("at least two Japanese page-one baselines");
    assert!(
        (largest_gap - 68.77).abs() <= 0.25,
        "inline-image transition must match Word's 68.77pt gap; got {largest_gap:.4}pt from {baselines:?}"
    );
}

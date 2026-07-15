use std::path::Path;

use office_core::{ConvertOptions, OfficeFormat, convert, profile};

const DOCUMENTED_WARNING_CODES: &[&str] = &[
    "bullet_substituted",
    "floating_table_block_fallback",
    "font_fallback",
    "font_substituted",
    "superscript_synthesized",
];

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fidelity-corpus/docx")
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

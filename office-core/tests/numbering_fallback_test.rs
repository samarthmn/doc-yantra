//! Regression coverage for font fallback in DOCX numbering labels.

use office_core::layout::{Item, WordDoc, layout_docx};
use office_core::testkit::{DOC_NAMESPACES, zip_package};
use office_core::{ConvertOptions, OfficeFormat, UnsupportedCode, convert, profile};

fn numbering_docx() -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p></w:body></w:document>"#
    );
    let numbering = format!(
        r#"<?xml version="1.0"?><w:numbering{DOC_NAMESPACES}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="१%1."/><w:rPr><w:rFonts w:ascii="Calibri"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
    ])
}

fn options() -> ConvertOptions {
    ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..ConvertOptions::default()
    }
}

fn bullet_docx_with_fonts(label: char, items: usize, r_fonts: &str) -> Vec<u8> {
    let paragraphs = (0..items)
        .map(|index| {
            format!(r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>item {index}</w:t></w:r></w:p>"#)
        })
        .collect::<String>();
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>{paragraphs}</w:body></w:document>"#
    );
    let numbering = format!(
        r#"<?xml version="1.0"?><w:numbering{DOC_NAMESPACES}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="{label}"/><w:rPr><w:rFonts {r_fonts}/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
    ])
}

fn bullet_docx(label: char, items: usize) -> Vec<u8> {
    bullet_docx_with_fonts(label, items, r#"w:ascii="Noto Sans Symbols""#)
}

fn extracted_layout_text(bytes: &[u8]) -> String {
    let doc = WordDoc::open(bytes, &ConvertOptions::default()).expect("open bullet fixture");
    let (laid, _) = layout_docx(&doc).expect("layout bullet fixture");
    laid.pages
        .iter()
        .flat_map(|page| &page.items)
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => Some(run.glyphs.iter().map(|glyph| glyph.ch).collect::<String>()),
            _ => None,
        })
        .collect()
}

#[test]
fn numbering_label_splits_mixed_primary_and_fallback_faces() {
    let bytes = numbering_docx();
    let options = options();
    let converted = convert(&bytes, OfficeFormat::Docx, &options);
    assert!(converted.is_ok(), "numbering conversion: {converted:?}");

    let doc = WordDoc::open(&bytes, &options).expect("open numbering fixture");
    let (laid, _) = layout_docx(&doc).expect("layout numbering fixture");
    let extracted: String = laid
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => Some(run.glyphs.iter().map(|glyph| glyph.ch).collect::<String>()),
            _ => None,
        })
        .collect();
    assert!(
        extracted.contains("१1."),
        "missing numbering label: {extracted:?}"
    );
}

#[test]
fn exotic_bullet_substitution_warns_in_profile_and_convert_once() {
    let bytes = bullet_docx('\u{2B9A}', 2);
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile exotic bullet");
    assert!(
        predicted.supported,
        "profile reasons: {:?}",
        predicted.reasons
    );
    let output = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("substituted bullet converts");
    let expected_detail = "U+2B9A → U+25BA";
    let profile_warnings: Vec<_> = predicted
        .warnings
        .iter()
        .filter(|warning| warning.code == "bullet_substituted")
        .collect();
    let convert_warnings: Vec<_> = output
        .warnings
        .iter()
        .filter(|warning| warning.code == "bullet_substituted")
        .collect();
    assert_eq!(
        profile_warnings.len(),
        1,
        "profile: {:?}",
        predicted.warnings
    );
    assert_eq!(convert_warnings.len(), 1, "convert: {:?}", output.warnings);
    assert_eq!(profile_warnings[0].detail, expected_detail);
    assert_eq!(convert_warnings[0], profile_warnings[0]);

    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).expect("open exotic bullet");
    let (laid, _) = layout_docx(&doc).expect("layout exotic bullet");
    let extracted: String = laid
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => Some(run.glyphs.iter().map(|glyph| glyph.ch).collect::<String>()),
            _ => None,
        })
        .collect();
    assert!(
        extracted.contains('\u{25BA}'),
        "substituted label: {extracted:?}"
    );
    assert!(
        !extracted.contains('\u{2B9A}'),
        "raw label survived: {extracted:?}"
    );
}

#[test]
fn hansi_only_symbol_and_wingdings_bullets_use_the_same_family_in_profile_and_layout() {
    for (family, authored, expected) in [
        ("Symbol", '\u{00B7}', '\u{2022}'),
        ("Wingdings", '\u{00A7}', '\u{25AA}'),
    ] {
        let bytes = bullet_docx_with_fonts(authored, 1, &format!(r#"w:hAnsi="{family}""#));
        let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile hAnsi bullet");
        assert!(
            predicted.supported,
            "{family} reasons: {:?}",
            predicted.reasons
        );
        let output = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
            .expect("convert hAnsi bullet");
        assert_eq!(
            output.warnings, predicted.warnings,
            "{family} profile/layout warning parity"
        );
        let extracted = extracted_layout_text(&bytes);
        assert!(
            extracted.contains(expected),
            "{family} expected U+{:04X}: {extracted:?}",
            expected as u32
        );
        assert!(
            !extracted.contains(authored),
            "{family} raw U+{:04X} survived: {extracted:?}",
            authored as u32
        );
    }
}

#[test]
fn hansi_only_exotic_bullet_has_exact_profile_convert_warning_parity() {
    let bytes = bullet_docx_with_fonts('\u{2B9A}', 2, r#"w:hAnsi="Noto Sans Symbols""#);
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile hAnsi exotic bullet");
    let output = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("convert hAnsi exotic bullet");
    assert_eq!(output.warnings, predicted.warnings);
    assert!(
        predicted
            .warnings
            .iter()
            .any(|warning| warning.code == "bullet_substituted")
    );
    let extracted = extracted_layout_text(&bytes);
    assert!(
        extracted.contains('\u{25BA}'),
        "substituted label: {extracted:?}"
    );
    assert!(
        !extracted.contains('\u{2B9A}'),
        "raw label survived: {extracted:?}"
    );
}

#[test]
fn unmappable_private_use_bullet_remains_fail_closed() {
    let bytes = bullet_docx('\u{10FFFD}', 1);
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile PUA bullet");
    assert!(predicted.reasons.iter().any(|reason| {
        reason.code == UnsupportedCode::MissingGlyph && reason.detail.contains("U+10FFFD")
    }));
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::MissingGlyph,
            ..
        })
    ));
}

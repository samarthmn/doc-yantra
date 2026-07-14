//! Regression coverage for font fallback in DOCX numbering labels.

use office_core::layout::{Item, WordDoc, layout_docx};
use office_core::testkit::{DOC_NAMESPACES, zip_package};
use office_core::{ConvertOptions, OfficeFormat, convert};

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

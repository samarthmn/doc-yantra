#![cfg(feature = "fidelity-diagnostics")]

use office_core::testkit::{DOC_NAMESPACES, minimal_docx, zip_package};
use office_core::{ConvertOptions, OfficeFormat, convert};

#[test]
fn diagnostic_conversion_returns_final_line_geometry_and_property_provenance() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:ind w:left="720" w:hanging="360"/><w:tabs><w:tab w:val="right" w:pos="1440"/></w:tabs></w:pPr><w:r><w:t>Trace me</w:t></w:r></w:p>"#,
    );
    let output = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("diagnostic DOCX conversion");
    let trace = output
        .diagnostics
        .expect("feature must return a DOCX trace");
    let line = trace.lines.first().expect("paragraph line trace");

    assert!(
        line.page == 1
            && line.baseline_pt > line.line_box_top_pt
            && line.paragraph.direct_indent.left_pt == Some(36.0)
            && line.paragraph.direct_tabs[0].pos_pt == 72.0
            && !line.glyph_origins.is_empty(),
        "incomplete trace: {}",
        serde_json::to_string(line).expect("serialize line trace")
    );
}

#[test]
fn numbering_trace_exposes_effective_tabs_and_label_body_geometry() {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Trace body</w:t></w:r></w:p></w:body></w:document>"#
    );
    let numbering = format!(
        r#"<?xml version="1.0"?><w:numbering{DOC_NAMESPACES}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="lowerRoman"/><w:lvlText w:val="%1."/><w:lvlJc w:val="right"/><w:pPr><w:tabs><w:tab w:val="left" w:pos="1440"/></w:tabs><w:ind w:left="1152" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
    ]);
    let output = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("diagnostic numbered conversion");
    let trace = output.diagnostics.expect("numbering trace");
    let line = trace.lines.first().expect("numbered line");
    let paragraph = serde_json::to_value(&line.paragraph).expect("serialize paragraph trace");
    assert_eq!(line.paragraph.numbering_tabs[0].pos_pt, 72.0);
    assert_eq!(line.paragraph.effective_tabs[0].pos_pt, 72.0);
    assert_eq!(
        paragraph["numbering_layout"]["following_text_start_pt"],
        serde_json::json!(72.0)
    );
    assert_eq!(
        paragraph["numbering_layout"]["left_origin"],
        serde_json::json!("numbering_level")
    );
    assert!(
        paragraph["numbering_layout"]["label_ink_end_pt"]
            .as_f64()
            .is_some_and(|end| end < 57.6),
        "right-justified Roman label must end before the 57.6pt body: {paragraph}"
    );
}

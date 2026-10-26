#![cfg(feature = "fidelity-diagnostics")]

use office_core::testkit::{DOC_NAMESPACES, minimal_docx, wrap_document, wrap_styles, zip_package};
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

#[test]
fn spacing_trace_exposes_default_style_direct_effective_and_natural_metrics() {
    let document = wrap_document(
        r#"<w:p><w:pPr><w:pStyle w:val="Boundary"/><w:spacing w:before="80"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Liberation Sans" w:hAnsi="Liberation Sans"/><w:sz w:val="24"/></w:rPr><w:t>Boundary trace</w:t></w:r></w:p>"#,
    );
    let styles = wrap_styles(
        r#"<w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:before="40" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:styleId="Boundary"><w:pPr><w:spacing w:after="120" w:line="360" w:lineRule="auto"/></w:pPr></w:style>"#,
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
    ]);
    let output = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("diagnostic spacing conversion");
    let trace = output.diagnostics.expect("spacing trace");
    let line = serde_json::to_value(trace.lines.first().expect("spacing line"))
        .expect("serialize spacing line");

    assert_eq!(line["paragraph"]["default_spacing"]["before_pt"], 2.0);
    assert_eq!(line["paragraph"]["style_spacing"]["after_pt"], 6.0);
    assert_eq!(line["paragraph"]["direct_spacing"]["before_pt"], 4.0);
    assert_eq!(line["paragraph"]["effective_spacing"]["before_pt"], 4.0);
    assert_eq!(line["paragraph"]["effective_spacing"]["after_pt"], 6.0);
    assert_eq!(line["paragraph"]["effective_spacing"]["line_rule"], "auto");
    assert_eq!(line["paragraph"]["effective_spacing"]["line_value"], 1.5);
    assert!(
        line["natural_line_height_pt"]
            .as_f64()
            .is_some_and(|v| v > 0.0)
            && line["text_natural_height_pt"]
                .as_f64()
                .is_some_and(|v| v > 0.0)
            && line["run_metrics"].as_array().is_some_and(|runs| {
                runs.iter().any(|run| {
                    run["size_pt"] == 12.0
                        && run["line_box_height_pt"].as_f64().is_some_and(|v| v > 0.0)
                })
            }),
        "missing line natural/run metrics: {line}"
    );
}

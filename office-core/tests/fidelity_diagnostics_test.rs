#![cfg(feature = "fidelity-diagnostics")]

use office_core::testkit::minimal_docx;
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

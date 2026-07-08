//! DOCX layout + emission integration tests (agent task T3.9).

use office_core::layout::{Item, LaidOutDocument, Page, WordDoc, layout_docx};
use office_core::testkit::{minimal_docx, wrap_document, zip_package};
use office_core::{ConvertOptions, OfficeFormat, UnsupportedCode, convert, profile};

fn opts() -> ConvertOptions {
    ConvertOptions::default()
}

/// Lays out a docx body and returns the laid-out document.
fn layout_body(body: &str) -> LaidOutDocument {
    let bytes = minimal_docx(body);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    layout_docx(&doc).0
}

/// All glyph text on a page, in item (paint) order.
fn page_text(page: &Page) -> String {
    let mut s = String::new();
    for pos in &page.items {
        if let Item::Glyphs(run) = &pos.item {
            for g in &run.glyphs {
                s.push(g.ch);
            }
        }
    }
    s
}

/// Distinct glyph-run baselines on a page (≈ number of text lines).
fn text_rows(page: &Page) -> usize {
    let mut ys: Vec<i64> = page
        .items
        .iter()
        .filter(|p| matches!(p.item, Item::Glyphs(_)))
        .map(|p| (p.y_pt * 4.0) as i64)
        .collect();
    ys.sort_unstable();
    ys.dedup();
    ys.len()
}

#[test]
fn converts_real_mock_docx_to_valid_pdf() {
    let bytes = std::fs::read("../mock-files/docx/file1.docx").unwrap();
    let out = convert(&bytes, OfficeFormat::Docx, &opts()).expect("convert");
    assert!(out.pdf.starts_with(b"%PDF-"));
    assert!(out.page_count >= 1);
    assert!(out.pdf.len() > 500);
}

#[test]
fn conversion_is_deterministic() {
    let bytes = minimal_docx(r#"<w:p><w:r><w:t>Deterministic output test.</w:t></w:r></w:p>"#);
    let a = convert(&bytes, OfficeFormat::Docx, &opts()).unwrap();
    let b = convert(&bytes, OfficeFormat::Docx, &opts()).unwrap();
    assert_eq!(a.pdf, b.pdf, "same input must yield byte-identical PDF");
}

#[test]
fn simple_text_lays_out_and_is_selectable() {
    let laid = layout_body(r#"<w:p><w:r><w:t>Hello world</w:t></w:r></w:p>"#);
    assert_eq!(laid.pages.len(), 1);
    let text = page_text(&laid.pages[0]);
    assert!(text.contains("Hello world"), "got {text:?}");
}

#[test]
fn long_text_wraps_to_multiple_lines() {
    let words = "word ".repeat(200);
    let body = format!("<w:p><w:r><w:t xml:space=\"preserve\">{words}</w:t></w:r></w:p>");
    let laid = layout_body(&body);
    let rows: usize = laid.pages.iter().map(text_rows).sum();
    assert!(rows > 5, "expected many wrapped lines, got {rows}");
}

#[test]
fn explicit_page_break_starts_new_page() {
    let body = r#"<w:p><w:r><w:t>Page one</w:t></w:r></w:p>
        <w:p><w:r><w:br w:type="page"/></w:r></w:p>
        <w:p><w:r><w:t>Page two</w:t></w:r></w:p>"#;
    let laid = layout_body(body);
    assert!(laid.pages.len() >= 2, "page break should add a page");
    assert!(page_text(&laid.pages[0]).contains("Page one"));
    assert!(page_text(laid.pages.last().unwrap()).contains("Page two"));
}

#[test]
fn bordered_table_draws_borders_and_text() {
    let body = r#"
        <w:tbl>
          <w:tblPr>
            <w:tblW w:w="6000" w:type="dxa"/>
            <w:tblBorders>
              <w:top w:val="single" w:sz="8" w:color="000000"/>
              <w:left w:val="single" w:sz="8" w:color="000000"/>
              <w:bottom w:val="single" w:sz="8" w:color="000000"/>
              <w:right w:val="single" w:sz="8" w:color="000000"/>
              <w:insideH w:val="single" w:sz="8" w:color="000000"/>
              <w:insideV w:val="single" w:sz="8" w:color="000000"/>
            </w:tblBorders>
          </w:tblPr>
          <w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>
          <w:tr>
            <w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc>
            <w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc>
          </w:tr>
          <w:tr>
            <w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc>
            <w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc>
          </w:tr>
        </w:tbl>"#;
    let laid = layout_body(body);
    let page = &laid.pages[0];
    let lines = page
        .items
        .iter()
        .filter(|p| matches!(p.item, Item::Line { .. }))
        .count();
    assert!(lines >= 4, "expected table border segments, got {lines}");
    let text = page_text(page);
    for cell in ["A1", "B1", "A2", "B2"] {
        assert!(text.contains(cell), "cell {cell} missing from {text:?}");
    }
}

#[test]
fn nested_table_renders_inside_cell() {
    let body = r#"
        <w:tbl>
          <w:tblGrid><w:gridCol w:w="6000"/></w:tblGrid>
          <w:tr><w:tc>
            <w:tbl>
              <w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid>
              <w:tr><w:tc><w:p><w:r><w:t>Nested</w:t></w:r></w:p></w:tc></w:tr>
            </w:tbl>
          </w:tc></w:tr>
        </w:tbl>"#;
    let laid = layout_body(body);
    assert!(page_text(&laid.pages[0]).contains("Nested"));
}

#[test]
fn header_footer_page_field_shows_page_of_total() {
    // A package with a footer that has PAGE / NUMPAGES fields and enough body
    // content to span multiple pages.
    let paras = "<w:p><w:r><w:t>line</w:t></w:r></w:p>".repeat(160);
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body>{paras}
        <w:sectPr><w:footerReference w:type="default" r:id="rId1"/>
        <w:pgSz w:w="12240" w:h="15840"/>
        <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720"/>
        </w:sectPr></w:body></w:document>"#,
        ns = office_core::testkit::DOC_NAMESPACES,
    );
    let footer = format!(
        r#"<?xml version="1.0"?><w:ftr{ns}><w:p>
        <w:r><w:t xml:space="preserve">Page </w:t></w:r>
        <w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple>
        <w:r><w:t xml:space="preserve"> of </w:t></w:r>
        <w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>1</w:t></w:r></w:fldSimple>
        </w:p></w:ftr>"#,
        ns = office_core::testkit::DOC_NAMESPACES,
    );
    let content_types = r#"<?xml version="1.0"?>
        <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
        <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
        <Default Extension="xml" ContentType="application/xml"/>
        <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
        <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>
        </Types>"#;
    let root_rels = r#"<?xml version="1.0"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
        </Relationships>"#;
    let doc_rels = r#"<?xml version="1.0"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>
        </Relationships>"#;

    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/footer1.xml", footer.as_bytes()),
    ]);

    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).0;
    let total = laid.pages.len();
    assert!(total >= 2, "expected multiple pages, got {total}");
    let first = page_text(&laid.pages[0]);
    assert!(first.contains("Page 1 of"), "footer on p1: {first:?}");
    assert!(
        first.contains(&total.to_string()),
        "total {total} in footer: {first:?}"
    );
    let last = page_text(laid.pages.last().unwrap());
    assert!(
        last.contains(&format!("Page {total} of {total}")),
        "footer on last page: {last:?}"
    );
}

/// Contract lock: `profile().supported == convert().is_ok()` and, when
/// unsupported, the error code equals the first profile reason code.
#[test]
fn profile_predicts_convert_verdict() {
    let cases: &[(&str, bool)] = &[
        (r#"<w:p><w:r><w:t>plain supported</w:t></w:r></w:p>"#, true),
        (
            r#"<w:p><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#,
            false,
        ),
        (r#"<w:p><w:r><m:oMath/></w:r></w:p>"#, false),
        (
            r#"<w:p><w:pPr><w:framePr w:w="1000"/></w:pPr><w:r><w:t>tb</w:t></w:r></w:p>"#,
            false,
        ),
    ];
    for (body, expect_supported) in cases {
        let bytes = minimal_docx(body);
        let prof = profile(&bytes, OfficeFormat::Docx).expect("profile");
        let conv = convert(&bytes, OfficeFormat::Docx, &opts());
        assert_eq!(
            prof.supported,
            conv.is_ok(),
            "verdict mismatch for {body:?}"
        );
        assert_eq!(
            prof.supported, *expect_supported,
            "expectation for {body:?}"
        );
        if let Err(office_core::OfficeError::Unsupported { code, .. }) = &conv {
            assert_eq!(
                Some(*code),
                prof.reasons.first().map(|r| r.code),
                "code mismatch for {body:?}"
            );
        }
    }
}

#[test]
fn multicolumn_section_fails_closed() {
    let body = r#"<w:p><w:r><w:t>x</w:t></w:r></w:p>
        <w:sectPr><w:cols w:num="2"/></w:sectPr>"#;
    let bytes = minimal_docx(body);
    let err = convert(&bytes, OfficeFormat::Docx, &opts()).unwrap_err();
    assert!(matches!(
        err,
        office_core::OfficeError::Unsupported {
            code: UnsupportedCode::MultiColumn,
            ..
        }
    ));
    // Keep the wrap_document helper exercised.
    assert!(wrap_document("<w:p/>").contains("w:body"));
}

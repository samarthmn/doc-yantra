//! Continuous-section and stacked column-band regression tests.

use office_core::layout::{Item, Page, WordDoc, layout_docx};
use office_core::testkit::{DOC_NAMESPACES, minimal_docx, zip_package};
use office_core::{ConvertOptions, OfficeFormat, UnsupportedCode, convert, profile};

const LETTER_SECTION: &str = r#"<w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1800" w:bottom="1440" w:left="1800" w:header="720" w:footer="720"/>"#;
const SHORT_SECTION: &str = r#"<w:pgSz w:w="12240" w:h="5760"/><w:pgMar w:top="720" w:right="1800" w:bottom="720" w:left="1800" w:header="360" w:footer="360"/>"#;
const TWO_COLUMNS: &str = r#"<w:cols w:num="2" w:equalWidth="0" w:space="0"><w:col w:w="4320" w:space="0"/><w:col w:w="4320" w:space="0"/></w:cols>"#;

fn open_layout(bytes: &[u8]) -> office_core::layout::LaidOutDocument {
    let doc = WordDoc::open(bytes, &ConvertOptions::default()).expect("open DOCX");
    layout_docx(&doc).expect("layout DOCX").0
}

fn page_text(page: &Page) -> String {
    page.items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => Some(run.glyphs.iter().map(|glyph| glyph.ch).collect::<String>()),
            Item::SemanticText(run) => Some(
                run.clusters
                    .iter()
                    .map(|cluster| cluster.source.as_str())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

fn text_position(page: &Page, needle: &str) -> (f64, f64) {
    page.items
        .iter()
        .find_map(|positioned| match &positioned.item {
            Item::Glyphs(run)
                if run.glyphs.iter().map(|glyph| glyph.ch).collect::<String>() == needle =>
            {
                Some((positioned.x_pt, positioned.y_pt))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing {needle:?} on page: {:?}", page_text(page)))
}

fn paragraph(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"#)
}

fn exact_paragraph(text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:spacing w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
    )
}

fn section_end(text: &str, properties: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:sectPr>{properties}</w:sectPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
    )
}

fn footer_docx(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>{body}</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/word/footer2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdF1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/><Relationship Id="rIdF2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/></Relationships>"#;
    let inherited = format!(
        r#"<?xml version="1.0"?><w:ftr{DOC_NAMESPACES}><w:p><w:r><w:t>INHERITED</w:t></w:r></w:p></w:ftr>"#
    );
    let overridden = format!(
        r#"<?xml version="1.0"?><w:ftr{DOC_NAMESPACES}><w:p><w:r><w:t>OVERRIDE</w:t></w:r></w:p></w:ftr>"#
    );
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels.as_bytes()),
        ("word/footer1.xml", inherited.as_bytes()),
        ("word/footer2.xml", overridden.as_bytes()),
    ])
}

#[test]
fn five_continuous_sections_stack_balanced_column_bands_on_one_page() {
    let mut body = section_end("S0", LETTER_SECTION);
    for label in ["S1A", "S1B", "S1C"] {
        body.push_str(&paragraph(label));
    }
    body.push_str(&section_end(
        "S1D",
        &format!(r#"<w:type w:val="continuous"/>{LETTER_SECTION}{TWO_COLUMNS}"#),
    ));
    body.push_str(&section_end(
        "S2",
        &format!(r#"<w:type w:val="continuous"/>{LETTER_SECTION}"#),
    ));
    for label in ["S3A", "S3B", "S3C"] {
        body.push_str(&paragraph(label));
    }
    body.push_str(&section_end(
        "S3D",
        &format!(r#"<w:type w:val="continuous"/>{LETTER_SECTION}{TWO_COLUMNS}"#),
    ));
    body.push_str(&paragraph("S4"));
    body.push_str(&format!(
        r#"<w:sectPr><w:type w:val="continuous"/>{LETTER_SECTION}</w:sectPr>"#
    ));

    let laid = open_layout(&minimal_docx(&body));
    assert_eq!(laid.pages.len(), 1);
    let page = &laid.pages[0];
    let (s1a_x, s1a_y) = text_position(page, "S1A");
    let (s1b_x, _) = text_position(page, "S1B");
    let (s1c_x, _) = text_position(page, "S1C");
    let (s2_x, s2_y) = text_position(page, "S2");
    assert!((s1a_x - 90.0).abs() < 0.1, "first column x={s1a_x}");
    assert!(
        (s1b_x - 90.0).abs() < 0.1,
        "balanced first column x={s1b_x}"
    );
    assert!((s1c_x - 306.0).abs() < 0.1, "second column x={s1c_x}");
    assert!((s2_x - 90.0).abs() < 0.1, "single-column x={s2_x}");
    assert!(
        s2_y > s1a_y,
        "single-column band must follow the two-column band"
    );
}

#[test]
fn next_page_is_the_transition_into_the_second_run() {
    let body = format!(
        "{}{}<w:sectPr>{LETTER_SECTION}</w:sectPr>",
        section_end("FIRST", LETTER_SECTION),
        paragraph("SECOND")
    );
    let laid = open_layout(&minimal_docx(&body));
    assert_eq!(laid.pages.len(), 2);
    assert!(page_text(&laid.pages[0]).contains("FIRST"));
    assert!(page_text(&laid.pages[1]).contains("SECOND"));
}

#[test]
fn next_page_preserves_an_empty_paragraph_section_page() {
    let body = format!(
        r#"<w:p><w:pPr><w:sectPr>{LETTER_SECTION}</w:sectPr></w:pPr></w:p>{}<w:sectPr>{LETTER_SECTION}</w:sectPr>"#,
        paragraph("SECOND")
    );
    let laid = open_layout(&minimal_docx(&body));
    assert_eq!(laid.pages.len(), 2);
    assert!(page_text(&laid.pages[0]).is_empty());
    assert!(page_text(&laid.pages[1]).contains("SECOND"));
}

#[test]
fn next_page_preserves_an_empty_trailing_section_page() {
    let body = format!(
        "{}<w:sectPr>{LETTER_SECTION}</w:sectPr>",
        section_end("FIRST", LETTER_SECTION)
    );
    let laid = open_layout(&minimal_docx(&body));
    assert_eq!(laid.pages.len(), 2);
    assert!(page_text(&laid.pages[0]).contains("FIRST"));
    assert!(page_text(&laid.pages[1]).is_empty());
}

#[test]
fn even_and_odd_transitions_insert_only_required_parity_pages() {
    let body = format!(
        "{}{}{}<w:sectPr><w:type w:val=\"oddPage\"/>{LETTER_SECTION}</w:sectPr>",
        section_end("ONE", LETTER_SECTION),
        section_end(
            "TWO",
            &format!(r#"<w:type w:val="evenPage"/>{LETTER_SECTION}"#)
        ),
        paragraph("THREE")
    );
    let laid = open_layout(&minimal_docx(&body));
    assert_eq!(laid.pages.len(), 3);
    assert!(page_text(&laid.pages[0]).contains("ONE"));
    assert!(page_text(&laid.pages[1]).contains("TWO"));
    assert!(page_text(&laid.pages[2]).contains("THREE"));
}

#[test]
fn footer_refs_inherit_and_later_refs_override_the_chain() {
    let body = format!(
        "{}{}{}{}<w:sectPr>{LETTER_SECTION}</w:sectPr>",
        section_end(
            "ONE",
            &format!(r#"{LETTER_SECTION}<w:footerReference w:type="default" r:id="rIdF1"/>"#)
        ),
        section_end("TWO", LETTER_SECTION),
        section_end(
            "THREE",
            &format!(r#"{LETTER_SECTION}<w:footerReference w:type="default" r:id="rIdF2"/>"#)
        ),
        paragraph("FOUR")
    );
    let laid = open_layout(&footer_docx(&body));
    assert_eq!(laid.pages.len(), 4);
    assert!(page_text(&laid.pages[0]).contains("INHERITED"));
    assert!(page_text(&laid.pages[1]).contains("INHERITED"));
    assert!(page_text(&laid.pages[2]).contains("OVERRIDE"));
    assert!(page_text(&laid.pages[3]).contains("OVERRIDE"));
}

#[test]
fn tall_two_column_run_continues_at_next_page_top_and_balances_last_band() {
    let mut body = section_end("LEAD", SHORT_SECTION);
    for index in 0..21 {
        body.push_str(&exact_paragraph(&format!("C{index:02}")));
    }
    body.push_str(&section_end(
        "C21",
        &format!(r#"<w:type w:val="continuous"/>{SHORT_SECTION}{TWO_COLUMNS}"#),
    ));
    body.push_str(&paragraph("AFTER"));
    body.push_str(&format!(
        r#"<w:sectPr><w:type w:val="continuous"/>{SHORT_SECTION}</w:sectPr>"#
    ));
    let laid = open_layout(&minimal_docx(&body));
    assert_eq!(
        laid.pages.len(),
        2,
        "page texts: {:?}",
        laid.pages.iter().map(page_text).collect::<Vec<_>>()
    );
    let page_two = &laid.pages[1];
    let (_, first_y) = text_position(page_two, "C16");
    let (left_x, _) = text_position(page_two, "C16");
    let (right_x, _) = text_position(page_two, "C19");
    let (_, after_y) = text_position(page_two, "AFTER");
    assert!(first_y < 61.0, "continuation y={first_y}");
    assert!((left_x - 90.0).abs() < 0.1, "left x={left_x}");
    assert!((right_x - 306.0).abs() < 0.1, "right x={right_x}");
    assert!(
        after_y > first_y,
        "following band y={after_y}, first y={first_y}"
    );
}

#[test]
fn continuous_geometry_change_has_profile_convert_parity() {
    let body = format!(
        "{}{}<w:sectPr><w:type w:val=\"continuous\"/><w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr>",
        section_end("LETTER", LETTER_SECTION),
        paragraph("A4")
    );
    let bytes = minimal_docx(&body);
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(predicted.reasons.iter().any(|reason| {
        reason.code == UnsupportedCode::UnknownElement
            && reason.detail == "continuous section break changes page geometry"
    }));
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::UnknownElement,
            detail,
        }) if detail == "continuous section break changes page geometry"
    ));
}

#[test]
fn real_gdocs_columns_fixture_profiles_and_converts_to_three_pages() {
    let bytes = std::fs::read("../fidelity-corpus/docx/gdocs-columns/source.docx")
        .expect("gdocs columns fixture");
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile fixture");
    assert!(
        predicted.supported,
        "profile reasons: {:?}",
        predicted.reasons
    );
    let converted =
        convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()).expect("convert fixture");
    assert_eq!(converted.page_count, 3);
}

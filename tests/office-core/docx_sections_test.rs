//! Continuous-section and stacked column-band regression tests.

use office_core::layout::{Item, Page, WordDoc, layout_docx};
use office_core::testkit::{DOC_NAMESPACES, minimal_docx, zip_package};
use office_core::{ConvertOptions, OfficeError, OfficeFormat, UnsupportedCode, convert, profile};

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

fn text_positions(page: &Page, needle: &str) -> Vec<(f64, f64)> {
    page.items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run)
                if run
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.ch)
                    .collect::<String>()
                    .contains(needle) =>
            {
                Some((positioned.x_pt, positioned.y_pt))
            }
            _ => None,
        })
        .collect()
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

fn package_with_parts(
    body: &str,
    document_rels: &str,
    extra_parts: Vec<(&str, Vec<u8>)>,
) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>{body}</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut owned = vec![
        ("[Content_Types].xml", content_types.as_bytes().to_vec()),
        ("_rels/.rels", root_rels.as_bytes().to_vec()),
        ("word/document.xml", document.into_bytes()),
        (
            "word/_rels/document.xml.rels",
            document_rels.as_bytes().to_vec(),
        ),
    ];
    owned.extend(extra_parts);
    let borrowed = owned
        .iter()
        .map(|(name, bytes)| (*name, bytes.as_slice()))
        .collect::<Vec<_>>();
    zip_package(&borrowed)
}

fn page_float(width_emu: i64, height_emu: i64) -> String {
    format!(
        r#"<w:drawing><wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1" distT="0" distB="114300" distL="0" distR="114300"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>left</wp:align></wp:positionH><wp:positionV relativeFrom="margin"><wp:align>top</wp:align></wp:positionV><wp:extent cx="{width_emu}" cy="{height_emu}"/><wp:wrapSquare wrapText="bothSides"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor></w:drawing>"#
    )
}

fn image_docx(body: &str) -> Vec<u8> {
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    package_with_parts(
        body,
        rels,
        vec![(
            "word/media/image1.png",
            include_bytes!("../../tests/fixtures/samples/png/file1.png").to_vec(),
        )],
    )
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
fn a_mid_page_column_band_applies_its_leading_gap_to_every_column() {
    let body = format!(
        r#"{}<w:p><w:pPr><w:spacing w:before="200" w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>LEFT TOP</w:t></w:r></w:p>{}{}{}<w:sectPr><w:type w:val="continuous"/>{LETTER_SECTION}{TWO_COLUMNS}</w:sectPr>"#,
        section_end("LEAD", LETTER_SECTION),
        exact_paragraph("LEFT SECOND"),
        exact_paragraph("RIGHT TOP"),
        exact_paragraph("RIGHT SECOND"),
    );
    let laid = open_layout(&minimal_docx(&body));
    let left_top = text_position(&laid.pages[0], "LEFT TOP");
    let right_top = text_position(&laid.pages[0], "RIGHT TOP");
    assert!(left_top.0 < 200.0 && right_top.0 > 200.0);
    assert!(
        (left_top.1 - right_top.1).abs() < 0.1,
        "column tops must share the band's leading gap: left={left_top:?}, right={right_top:?}"
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
fn next_page_section_preserves_collapsed_spacing_before_its_first_paragraph() {
    let first_baseline = |before_twips: u32| {
        let body = format!(
            r#"<w:p><w:pPr><w:spacing w:after="200"/><w:sectPr>{LETTER_SECTION}</w:sectPr></w:pPr></w:p><w:p><w:pPr><w:spacing w:before="{before_twips}"/></w:pPr><w:r><w:t>SECOND</w:t></w:r></w:p><w:sectPr>{LETTER_SECTION}</w:sectPr>"#
        );
        let laid = open_layout(&minimal_docx(&body));
        assert_eq!(laid.pages.len(), 2);
        text_position(&laid.pages[1], "SECOND").1
    };

    let after_sized_before = first_baseline(200);
    let larger_before = first_baseline(480);

    // Word desktop product-brief page 3 starts the first heading baseline at
    // 99.53pt versus 85.25pt when section-top spacing is suppressed. The
    // authored 24pt before collapses with the section mark's 10pt after, so
    // the general transition contribution is their 14pt excess.
    assert!(
        ((larger_before - after_sized_before) - 14.0).abs() < 0.01,
        "section-top spacing must preserve the collapsed before/after excess: \
         after-sized={after_sized_before}, larger={larger_before}"
    );
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
fn entering_odd_and_even_sections_insert_required_physical_parity_blanks() {
    let body = format!(
        "{}{}{}{}<w:sectPr><w:type w:val=\"evenPage\"/>{LETTER_SECTION}</w:sectPr>",
        section_end("ONE", LETTER_SECTION),
        section_end(
            "ODD",
            &format!(r#"<w:type w:val="oddPage"/>{LETTER_SECTION}"#)
        ),
        section_end("NEXT", LETTER_SECTION),
        paragraph("EVEN")
    );
    let laid = open_layout(&minimal_docx(&body));
    assert_eq!(laid.pages.len(), 6);
    assert!(page_text(&laid.pages[0]).contains("ONE"));
    assert!(page_text(&laid.pages[1]).is_empty());
    assert!(page_text(&laid.pages[2]).contains("ODD"));
    assert!(page_text(&laid.pages[3]).contains("NEXT"));
    assert!(page_text(&laid.pages[4]).is_empty());
    assert!(page_text(&laid.pages[5]).contains("EVEN"));
}

#[test]
fn only_the_page_with_a_footnote_reserves_the_note_band() {
    let note_page = format!(
        r#"<w:p><w:pPr><w:sectPr>{SHORT_SECTION}</w:sectPr></w:pPr><w:r><w:t>NOTE PAGE</w:t><w:footnoteReference w:id="1"/></w:r></w:p>"#
    );
    let later_body = (0..8)
        .map(|index| exact_paragraph(&format!("FULL HEIGHT {index}")))
        .collect::<String>();
    let body = format!("{note_page}{later_body}<w:sectPr>{SHORT_SECTION}</w:sectPr>");
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="1"><w:p><w:r><w:t>PAGE ONE NOTE</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = package_with_parts(
        &body,
        rels,
        vec![("word/footnotes.xml", footnotes.into_bytes())],
    );
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile normal footnote");
    assert!(
        predicted.supported,
        "normal footnote profile reasons: {:?}",
        predicted.reasons
    );
    convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("normal footnote converts");
    let laid = open_layout(&bytes);
    assert_eq!(
        laid.pages.len(),
        2,
        "page texts: {:?}",
        laid.pages.iter().map(page_text).collect::<Vec<_>>()
    );
    assert!(page_text(&laid.pages[0]).contains("PAGE ONE NOTE"));
    assert!(page_text(&laid.pages[1]).contains("FULL HEIGHT 7"));
    assert!(
        text_position(&laid.pages[1], "FULL HEIGHT 7").1 < 252.0,
        "later page must retain the full body height"
    );
}

#[test]
fn measured_footnote_reservation_converges_with_profile_convert_parity() {
    let leading = (0..10)
        .map(|index| exact_paragraph(&format!("OSCILLATION LEAD {index}")))
        .collect::<String>();
    let reference = r#"<w:p><w:pPr><w:spacing w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>OSCILLATING NOTE</w:t><w:footnoteReference w:id="1"/></w:r></w:p>"#;
    let body = format!("{leading}{reference}<w:sectPr>{SHORT_SECTION}</w:sectPr>");
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="1"><w:p><w:r><w:t>OSCILLATING NOTE TEXT A</w:t><w:br/><w:t>OSCILLATING NOTE TEXT B</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = package_with_parts(
        &body,
        rels,
        vec![("word/footnotes.xml", footnotes.into_bytes())],
    );

    let converted = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("measured reservation converges");
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile measured footnote");
    assert!(
        predicted.supported,
        "profile reasons: {:?}",
        predicted.reasons
    );

    let laid = open_layout(&bytes);
    assert_eq!(converted.page_count as usize, laid.pages.len());
    let reference_page = laid
        .pages
        .iter()
        .position(|page| page_text(page).contains("OSCILLATING NOTE"))
        .expect("reference page");
    let note_page = laid
        .pages
        .iter()
        .position(|page| page_text(page).contains("OSCILLATING NOTE TEXT A"))
        .expect("note page");
    assert_eq!(reference_page, note_page);
    let separator_y = laid.pages[note_page]
        .items
        .iter()
        .find_map(|positioned| match &positioned.item {
            Item::Line { dx_pt, dy_pt, .. }
                if (*dx_pt - 36.0).abs() < 0.01 && dy_pt.abs() < 0.01 =>
            {
                Some(positioned.y_pt)
            }
            _ => None,
        })
        .expect("footnote separator");
    assert!(
        text_position(&laid.pages[reference_page], "OSCILLATING NOTE").1 < separator_y,
        "reference must stay above the measured note reservation"
    );
}

#[test]
fn late_second_column_footnote_reserves_the_entire_physical_page() {
    let first_column = (0..9)
        .map(|index| exact_paragraph(&format!("COLUMN BODY {index}")))
        .collect::<String>();
    let note_boundary = format!(
        r#"<w:p><w:pPr><w:spacing w:line="480" w:lineRule="exact"/><w:sectPr>{SHORT_SECTION}{TWO_COLUMNS}</w:sectPr></w:pPr><w:r><w:t>LATE COLUMN NOTE</w:t><w:footnoteReference w:id="1"/></w:r></w:p>"#
    );
    let body = format!(
        "{first_column}{note_boundary}{}<w:sectPr>{SHORT_SECTION}</w:sectPr>",
        exact_paragraph("AFTER NOTE PAGE")
    );
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="1"><w:p><w:r><w:t>LATE NOTE TEXT</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let laid = open_layout(&package_with_parts(
        &body,
        rels,
        vec![("word/footnotes.xml", footnotes.into_bytes())],
    ));

    assert_eq!(
        laid.pages.len(),
        2,
        "page texts: {:?}",
        laid.pages.iter().map(page_text).collect::<Vec<_>>()
    );
    assert!(page_text(&laid.pages[0]).contains("LATE NOTE TEXT"));
    assert!(page_text(&laid.pages[1]).contains("AFTER NOTE PAGE"));
    let separator_y = laid.pages[0]
        .items
        .iter()
        .find_map(|positioned| match &positioned.item {
            Item::Line { dx_pt, dy_pt, .. }
                if (*dx_pt - 36.0).abs() < 0.01 && dy_pt.abs() < 0.01 =>
            {
                Some(positioned.y_pt)
            }
            _ => None,
        })
        .expect("footnote separator");
    assert!(
        text_position(&laid.pages[0], "LATE COLUMN NOTE").0 > 200.0,
        "the late note reference must remain in the second column"
    );
    for index in 0..9 {
        let position = text_position(&laid.pages[0], &format!("COLUMN BODY {index}"));
        assert!(
            position.1 < separator_y,
            "COLUMN BODY {index} overlaps the measured page-local note reservation: y={}, separator={separator_y}",
            position.1
        );
    }
}

#[test]
fn balanced_columns_include_each_repeated_table_header_height() {
    let row = |label: &str, line_twips: u32, header: bool| {
        let header = if header { "<w:tblHeader/>" } else { "" };
        format!(
            r#"<w:tr><w:trPr>{header}</w:trPr><w:tc><w:p><w:pPr><w:spacing w:line="{line_twips}" w:lineRule="exact"/></w:pPr><w:r><w:t>{label}</w:t></w:r></w:p></w:tc></w:tr>"#
        )
    };
    let mut table = r#"<w:tbl><w:tblPr><w:tblW w:w="4320" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="4320"/></w:tblGrid>"#.to_string();
    table.push_str(&row("REPEATED HEADER", 960, true));
    for index in 0..4 {
        table.push_str(&row(&format!("BODY ROW {index}"), 480, false));
    }
    table.push_str("</w:tbl>");
    table.push_str(&format!(
        r#"<w:sectPr>{SHORT_SECTION}{TWO_COLUMNS}</w:sectPr>"#
    ));
    let laid = open_layout(&minimal_docx(&table));
    assert_eq!(laid.pages.len(), 1);
    assert_eq!(text_positions(&laid.pages[0], "REPEATED HEADER").len(), 2);
    let left_rows = (0..4)
        .filter(|index| text_position(&laid.pages[0], &format!("BODY ROW {index}")).0 < 200.0)
        .count();
    assert_eq!(
        left_rows, 2,
        "repeated headers must participate in balancing"
    );
}

#[test]
fn balancing_does_not_orphan_a_header_before_one_large_atomic_row() {
    let table = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="4320" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="4320"/></w:tblGrid><w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:p><w:pPr><w:spacing w:line="960" w:lineRule="exact"/></w:pPr><w:r><w:t>ATOMIC HEADER</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:pPr><w:keepLines/><w:spacing w:line="2400" w:lineRule="exact"/></w:pPr><w:r><w:t>LARGE ATOMIC ROW</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr>{SHORT_SECTION}{TWO_COLUMNS}</w:sectPr>"#
    );
    let laid = open_layout(&minimal_docx(&table));
    assert_eq!(laid.pages.len(), 1);
    assert_eq!(text_positions(&laid.pages[0], "ATOMIC HEADER").len(), 1);
    assert!(
        text_position(&laid.pages[0], "LARGE ATOMIC ROW").0 < 200.0,
        "the header and its only atomic row must stay together in the first column"
    );
}

#[test]
fn balanced_columns_keep_keep_next_groups_atomic() {
    let exact = |label: &str, keep_next: bool| {
        let keep = if keep_next { "<w:keepNext/>" } else { "" };
        format!(
            r#"<w:p><w:pPr>{keep}<w:spacing w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>{label}</w:t></w:r></w:p>"#
        )
    };
    let body = format!(
        "{}{}{}{}<w:sectPr>{SHORT_SECTION}{TWO_COLUMNS}</w:sectPr>",
        exact("LEAD", false),
        exact("KEEP HEADING", true),
        exact("KEEP FOLLOW", false),
        exact("TAIL", false),
    );
    let laid = open_layout(&minimal_docx(&body));
    let heading = text_position(&laid.pages[0], "KEEP HEADING");
    let following = text_position(&laid.pages[0], "KEEP FOLLOW");
    let tail = text_position(&laid.pages[0], "TAIL");
    assert!((heading.0 - following.0).abs() < 0.1);
    assert!((tail.0 - following.0).abs() > 100.0);
}

#[test]
fn oversized_keep_next_chain_splits_at_widow_groups() {
    let words = "bounded keep chain content ".repeat(120);
    let body = format!(
        r#"<w:p><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>OVERSIZED KEEP HEADING</w:t></w:r></w:p><w:p><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>OVERSIZED KEEP MARK</w:t></w:r></w:p><w:p><w:pPr><w:widowControl/><w:spacing w:line="240" w:lineRule="exact"/></w:pPr><w:r><w:t>{words}</w:t></w:r></w:p><w:sectPr>{SHORT_SECTION}</w:sectPr>"#
    );
    let laid = open_layout(&minimal_docx(&body));

    assert!(
        laid.pages.len() >= 2,
        "a keep-next chain taller than a page must split safely"
    );
    assert!(page_text(&laid.pages[0]).contains("OVERSIZED KEEP HEADING"));
}

#[test]
fn transitive_keep_next_moves_the_widow_group_but_splits_the_long_body_remainder() {
    let body_lines = (0..20)
        .map(|index| {
            let br = if index != 19 { "<w:br/>" } else { "" };
            format!(r#"<w:t>BODY {index:02}</w:t>{br}"#)
        })
        .collect::<String>();
    let body = format!(
        r#"<w:p><w:pPr><w:spacing w:line="3360" w:lineRule="exact"/></w:pPr><w:r><w:t>LEAD</w:t></w:r></w:p>
        <w:p><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>CHAIN HEADING</w:t></w:r></w:p>
        <w:p><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>CHAIN MARK</w:t></w:r></w:p>
        <w:p><w:pPr><w:widowControl/><w:spacing w:line="240" w:lineRule="exact"/></w:pPr><w:r>{body_lines}</w:r></w:p>
        <w:sectPr>{SHORT_SECTION}</w:sectPr>"#
    );
    let laid = open_layout(&minimal_docx(&body));

    assert_eq!(
        laid.pages.len(),
        3,
        "page text: {:?}",
        laid.pages.iter().map(page_text).collect::<Vec<_>>()
    );
    assert!(!page_text(&laid.pages[0]).contains("CHAIN HEADING"));
    let chain_page = page_text(&laid.pages[1]);
    assert!(chain_page.contains("CHAIN HEADING"), "{chain_page:?}");
    assert!(chain_page.contains("CHAIN MARK"), "{chain_page:?}");
    assert!(chain_page.contains("BODY 00"), "{chain_page:?}");
    assert!(
        !chain_page.contains("BODY 19"),
        "body remainder must split: {chain_page:?}"
    );
    assert!(page_text(&laid.pages[2]).contains("BODY 19"));
}

#[test]
fn keep_next_heading_stays_with_the_first_table_row() {
    let body = format!(
        r#"<w:p><w:pPr><w:spacing w:line="3600" w:lineRule="exact"/></w:pPr><w:r><w:t>LEAD</w:t></w:r></w:p>
<w:p><w:pPr><w:keepNext/><w:spacing w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>TABLE HEADING</w:t></w:r></w:p>
<w:tbl><w:tblGrid><w:gridCol w:w="4320"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>FIRST TABLE ROW</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:sectPr>{SHORT_SECTION}</w:sectPr>"#
    );
    let laid = open_layout(&minimal_docx(&body));
    assert_eq!(laid.pages.len(), 2);
    assert!(!page_text(&laid.pages[0]).contains("TABLE HEADING"));
    let second = page_text(&laid.pages[1]);
    assert!(second.contains("TABLE HEADING"), "second page: {second:?}");
    assert!(
        second.contains("FIRST TABLE ROW"),
        "second page: {second:?}"
    );
}

#[test]
fn later_next_page_geometry_drives_page_float_reflow_bounds() {
    let first = section_end("SHORT FIRST", SHORT_SECTION);
    let anchor = page_float(3_175_000, 5_080_000);
    let wrapped = "WRAP GEOMETRY words must remain beside the later-section float. ".repeat(8);
    let body = format!(
        r#"{first}<w:p><w:r>{anchor}</w:r></w:p><w:p><w:r><w:t>{wrapped}</w:t></w:r></w:p><w:sectPr>{LETTER_SECTION}</w:sectPr>"#
    );
    let laid = open_layout(&image_docx(&body));
    assert!(laid.pages.len() >= 2);
    let wrapped_under_float = text_positions(&laid.pages[1], "WRAP GEOMETRY")
        .into_iter()
        .filter(|(_, y)| *y < 484.0)
        .collect::<Vec<_>>();
    assert!(!wrapped_under_float.is_empty());
    assert!(
        wrapped_under_float.iter().all(|(x, _)| *x > 330.0),
        "later-section lines escaped the float window: {wrapped_under_float:?}"
    );
}

#[test]
fn continuous_page_float_under_prior_non_top_alignment_fails_with_profile_parity() {
    let first = section_end(
        "CENTERED FIRST",
        &format!(r#"{LETTER_SECTION}<w:vAlign w:val="center"/>"#),
    );
    let anchor = page_float(977_900, 977_900);
    let body = format!(
        r#"{first}<w:p><w:r><w:t>LATER FLOAT</w:t>{anchor}</w:r></w:p><w:sectPr><w:type w:val="continuous"/>{LETTER_SECTION}<w:vAlign w:val="top"/></w:sectPr>"#
    );
    let bytes = image_docx(&body);
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(predicted.reasons.iter().any(|reason| {
        reason.code == UnsupportedCode::AnchoredDrawing
            && reason.detail == "page-anchored drawing with non-top body alignment"
    }));
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(OfficeError::Unsupported {
            code: UnsupportedCode::AnchoredDrawing,
            detail,
        }) if detail == "page-anchored drawing with non-top body alignment"
    ));
}

#[test]
fn centered_continuous_run_float_rejects_after_spilling_to_its_governing_page() {
    let first = section_end("TOP FIRST", SHORT_SECTION);
    let spill = (0..9)
        .map(|index| exact_paragraph(&format!("CENTERED SPILL {index}")))
        .collect::<String>();
    let anchor = page_float(977_900, 977_900);
    let body = format!(
        r#"{first}{spill}<w:p><w:r>{anchor}</w:r></w:p><w:sectPr><w:type w:val="continuous"/>{SHORT_SECTION}<w:vAlign w:val="center"/></w:sectPr>"#
    );
    let bytes = image_docx(&body);

    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(predicted.reasons.iter().any(|reason| {
        reason.code == UnsupportedCode::AnchoredDrawing
            && reason.detail == "page-anchored drawing with non-top body alignment"
    }));
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(OfficeError::Unsupported {
            code: UnsupportedCode::AnchoredDrawing,
            detail,
        }) if detail == "page-anchored drawing with non-top body alignment"
    ));
}

#[test]
fn top_continuous_run_float_is_allowed_after_spilling_off_a_centered_page() {
    let first = section_end(
        "CENTERED FIRST",
        &format!(r#"{SHORT_SECTION}<w:vAlign w:val="center"/>"#),
    );
    let spill = (0..9)
        .map(|index| exact_paragraph(&format!("TOP SPILL {index}")))
        .collect::<String>();
    let anchor = page_float(977_900, 977_900);
    let body = format!(
        r#"{first}{spill}<w:p><w:r>{anchor}</w:r></w:p><w:sectPr><w:type w:val="continuous"/>{SHORT_SECTION}<w:vAlign w:val="top"/></w:sectPr>"#
    );
    let bytes = image_docx(&body);

    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(
        predicted.supported,
        "profile reasons: {:?}",
        predicted.reasons
    );
    convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("top-governed float must convert");
    let laid = open_layout(&bytes);
    assert!(laid.pages.len() >= 2);
    assert!(
        !laid.pages[0]
            .items
            .iter()
            .any(|positioned| matches!(positioned.item, Item::Image { .. }))
    );
    assert!(
        laid.pages[1]
            .items
            .iter()
            .any(|positioned| matches!(positioned.item, Item::Image { .. }))
    );
}

#[test]
fn appended_endnote_pages_use_the_final_continuous_section_properties() {
    let first = section_end(
        "FIRST BODY",
        &format!(
            r#"{LETTER_SECTION}<w:headerReference w:type="default" r:id="rIdH1"/><w:footerReference w:type="default" r:id="rIdF1"/><w:pgNumType w:start="1"/>"#
        ),
    );
    let body = format!(
        r#"{first}<w:p><w:r><w:t>FINAL BODY</w:t><w:endnoteReference w:id="2"/></w:r></w:p><w:sectPr><w:type w:val="continuous"/>{LETTER_SECTION}<w:headerReference w:type="default" r:id="rIdH2"/><w:footerReference w:type="default" r:id="rIdF2"/><w:pgNumType w:start="9" w:fmt="upperRoman"/><w:pgBorders><w:top w:val="single" w:sz="8" w:color="000000"/><w:bottom w:val="single" w:sz="8" w:color="000000"/><w:left w:val="single" w:sz="8" w:color="000000"/><w:right w:val="single" w:sz="8" w:color="000000"/></w:pgBorders></w:sectPr>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rIdH2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/><Relationship Id="rIdF1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/><Relationship Id="rIdF2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer2.xml"/></Relationships>"#;
    let header = |text: &str| {
        format!(
            r#"<?xml version="1.0"?><w:hdr{DOC_NAMESPACES}><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:hdr>"#
        )
        .into_bytes()
    };
    let footer = |text: &str| {
        format!(
            r#"<?xml version="1.0"?><w:ftr{DOC_NAMESPACES}><w:p><w:r><w:t>{text} </w:t></w:r><w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p></w:ftr>"#
        )
        .into_bytes()
    };
    let endnotes = format!(
        r#"<?xml version="1.0"?><w:endnotes{DOC_NAMESPACES}><w:endnote w:id="2"><w:p><w:r><w:t>FINAL ENDNOTE</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    );
    let laid = open_layout(&package_with_parts(
        &body,
        rels,
        vec![
            ("word/header1.xml", header("HEADER ONE")),
            ("word/header2.xml", header("HEADER FINAL")),
            ("word/footer1.xml", footer("FOOTER ONE")),
            ("word/footer2.xml", footer("FOOTER FINAL")),
            ("word/endnotes.xml", endnotes.into_bytes()),
        ],
    ));
    assert_eq!(laid.pages.len(), 2);
    assert!(page_text(&laid.pages[0]).contains("HEADER ONE"));
    let last = &laid.pages[1];
    let last_text = page_text(last);
    assert!(last_text.contains("FINAL ENDNOTE"));
    assert!(last_text.contains("HEADER FINAL"));
    assert!(last_text.contains("FOOTER FINAL"));
    assert!(last_text.contains("IX"), "last page text: {last_text:?}");
    assert!(
        last.items
            .iter()
            .filter(|item| matches!(item.item, Item::Line { .. }))
            .count()
            >= 4
    );
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
fn explicit_page_break_at_a_continuous_run_boundary_starts_a_new_page() {
    let body = format!(
        r#"<w:p><w:pPr><w:sectPr>{SHORT_SECTION}</w:sectPr></w:pPr><w:r><w:br w:type="page"/></w:r></w:p><w:p><w:pPr><w:spacing w:before="200" w:line="480" w:lineRule="exact"/></w:pPr><w:r><w:t>AFTER RUN BOUNDARY</w:t></w:r></w:p><w:sectPr><w:type w:val="continuous"/>{SHORT_SECTION}{TWO_COLUMNS}</w:sectPr>"#
    );

    let laid = open_layout(&minimal_docx(&body));
    assert_eq!(laid.pages.len(), 2);
    assert!(page_text(&laid.pages[0]).is_empty());
    assert!(page_text(&laid.pages[1]).contains("AFTER RUN BOUNDARY"));
    assert!(
        text_position(&laid.pages[1], "AFTER RUN BOUNDARY").1 > 55.0,
        "the section-ending break must preserve the following paragraph's page-top leading"
    );
}

#[test]
fn negative_decimal_table_indent_positions_the_painted_edge_from_the_margin() {
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="2160.0" w:type="dxa"/><w:tblInd w:w="-108.0" w:type="dxa"/><w:tblBorders><w:top w:val="single" w:sz="8"/><w:left w:val="single" w:sz="8"/><w:bottom w:val="single" w:sz="8"/><w:right w:val="single" w:sz="8"/></w:tblBorders><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="2160.0"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>INDENTED</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr>{LETTER_SECTION}</w:sectPr>"#
    );
    let laid = open_layout(&minimal_docx(&body));
    let (left_center, left_width) = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Line {
                dx_pt,
                dy_pt,
                stroke,
            } if dx_pt.abs() < 0.01 && dy_pt.abs() > 0.01 => {
                Some((positioned.x_pt, stroke.width_pt))
            }
            _ => None,
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .expect("left table edge");
    let painted_left = left_center - left_width / 2.0;

    assert!(
        (painted_left - 84.6).abs() < 0.1,
        "-108 twips must put the painted table edge at 90 - 5.4 = 84.6pt, got {painted_left}"
    );
}

#[test]
fn table_cell_wrapping_prefers_a_hyphen_before_emergency_glyph_breaks() {
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="2880" w:type="dxa"/><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>application/vnd.openxmlformats-officedocument.spreadsheetml.sheet</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr>{LETTER_SECTION}</w:sectPr>"#
    );
    let laid = open_layout(&minimal_docx(&body));
    let line_count = laid.pages[0]
        .items
        .iter()
        .filter(|positioned| matches!(positioned.item, Item::Glyphs(_)))
        .count();

    assert_eq!(
        line_count, 4,
        "Word breaks after the hyphen before emergency-breaking the remaining token"
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
    let bytes = std::fs::read("../tests/fixtures/corpus/docx/gdocs-columns/source.docx")
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
    let laid = open_layout(&bytes);
    assert!(!page_text(&laid.pages[1]).contains("Part 3: Two-Column Reference"));
    assert!(page_text(&laid.pages[2]).contains("Part 3: Two-Column Reference"));
}

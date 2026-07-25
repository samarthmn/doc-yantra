//! Word parity for numbering on content-less paragraphs, locked against the
//! chinese fidelity ground truth ([0024]/[0026]/[0029]): an EMPTY numbered
//! paragraph still displays its label on its own line, while a numbered
//! paragraph whose only content is a page break consumes its number without
//! displaying it.

use office_core::ConvertOptions;
use office_core::layout::{Item, WordDoc, layout_docx};
use office_core::testkit::{DOC_NAMESPACES, zip_package};

fn numbered_docx(body_paragraphs: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>{body_paragraphs}</w:body></w:document>"#
    );
    let numbering = format!(
        r#"<?xml version="1.0"?><w:numbering{DOC_NAMESPACES}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="[%1]"/><w:rPr><w:rFonts w:ascii="Calibri"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
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

const NUM_PPR: &str =
    r#"<w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>"#;

fn extracted_text(bytes: &[u8]) -> String {
    let doc = WordDoc::open(bytes, &ConvertOptions::default()).expect("open numbering fixture");
    let (laid, _) = layout_docx(&doc).expect("layout numbering fixture");
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
fn empty_numbered_paragraph_still_displays_its_label() {
    let body = format!(
        "<w:p>{NUM_PPR}<w:r><w:t>one</w:t></w:r></w:p>\
         <w:p>{NUM_PPR}</w:p>\
         <w:p>{NUM_PPR}<w:r><w:t>two</w:t></w:r></w:p>"
    );
    let extracted = extracted_text(&numbered_docx(&body));
    for label in ["[1]", "[2]", "[3]"] {
        assert!(
            extracted.contains(label),
            "Word shows the bare number of an empty numbered paragraph; \
             missing {label}: {extracted:?}"
        );
    }
}

#[test]
fn empty_numbered_section_mark_paragraph_hides_its_label() {
    // The empty paragraph that carries a section break is the section mark
    // itself: Word never displays its number (chinese ground truth ends at
    // [0035] even though the final empty AppBody paragraph would be [0036]).
    let body = format!(
        "<w:p>{NUM_PPR}<w:r><w:t>one</w:t></w:r></w:p>\
         <w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr>\
         <w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr></w:pPr></w:p>\
         <w:p>{NUM_PPR}<w:r><w:t>two</w:t></w:r></w:p>"
    );
    let extracted = extracted_text(&numbered_docx(&body));
    assert!(extracted.contains("[1]"), "{extracted:?}");
    assert!(
        !extracted.contains("[2]"),
        "a section-mark paragraph must not display its number: {extracted:?}"
    );
}

#[test]
fn page_break_only_numbered_paragraph_consumes_but_hides_its_label() {
    let body = format!(
        "<w:p>{NUM_PPR}<w:r><w:t>one</w:t></w:r></w:p>\
         <w:p>{NUM_PPR}<w:r><w:br w:type=\"page\"/></w:r></w:p>\
         <w:p>{NUM_PPR}<w:r><w:t>two</w:t></w:r></w:p>"
    );
    let extracted = extracted_text(&numbered_docx(&body));
    assert!(extracted.contains("[1]"), "{extracted:?}");
    assert!(
        extracted.contains("[3]"),
        "the page-break-only paragraph must still consume its number: {extracted:?}"
    );
    assert!(
        !extracted.contains("[2]"),
        "Word hides the label of a page-break-only numbered paragraph: {extracted:?}"
    );
}

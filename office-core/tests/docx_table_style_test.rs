//! DOCX table styles (`w:tblStyle`) with conditional formatting.
//!
//! Word writes a table style reference on essentially every non-plain table, and
//! stamps `w:tblLook` plus cached `w:cnfStyle` markers alongside it. The engine
//! must resolve the style out of `word/styles.xml`, decide which conditional
//! blocks apply from the table's position grid, and paint the result — refusing
//! the document is not parity.

use office_core::layout::{Item, Page, WordDoc, layout_docx};
use office_core::oxml::Rgb;
use office_core::testkit::{DOC_NAMESPACES, wrap_styles, zip_package};
use office_core::{ConvertOptions, OfficeFormat, convert, profile};

/// A table style close to Word's built-in `LightGrid-Accent1`: a bordered whole
/// table, a filled bold header row, and shaded odd horizontal bands.
const TABLE_STYLES: &str = r#"
<w:style w:type="table" w:default="1" w:styleId="TableNormal">
  <w:tblPr><w:tblCellMar>
    <w:top w:w="0" w:type="dxa"/><w:left w:w="108" w:type="dxa"/>
    <w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/>
  </w:tblCellMar></w:tblPr>
</w:style>
<w:style w:type="table" w:styleId="LightGrid-Accent1">
  <w:basedOn w:val="TableNormal"/>
  <w:tblPr>
    <w:tblStyleRowBandSize w:val="1"/>
    <w:tblBorders>
      <w:top w:val="single" w:sz="8" w:color="4472C4"/>
      <w:left w:val="single" w:sz="8" w:color="4472C4"/>
      <w:bottom w:val="single" w:sz="8" w:color="4472C4"/>
      <w:right w:val="single" w:sz="8" w:color="4472C4"/>
      <w:insideH w:val="single" w:sz="8" w:color="4472C4"/>
      <w:insideV w:val="single" w:sz="8" w:color="4472C4"/>
    </w:tblBorders>
  </w:tblPr>
  <w:tblStylePr w:type="firstRow">
    <w:rPr><w:b/><w:color w:val="FFFFFF"/></w:rPr>
    <w:tcPr><w:shd w:val="clear" w:fill="4472C4"/></w:tcPr>
  </w:tblStylePr>
  <w:tblStylePr w:type="band1Horz">
    <w:tcPr><w:shd w:val="clear" w:fill="D9E2F3"/></w:tcPr>
  </w:tblStylePr>
</w:style>
"#;

/// Two columns, four rows: a header, then three body rows. With `firstRow` on
/// and horizontal banding on, body row 1 is band1 (shaded), row 2 is band2
/// (bare), row 3 is band1 again. The `w:cnfStyle` markers are exactly what Word
/// emits; they are redundant with the positional computation and must not be
/// refused. The last cell overrides the band fill directly.
const TABLE_BODY: &str = r#"
<w:tbl>
  <w:tblPr>
    <w:tblStyle w:val="LightGrid-Accent1"/>
    <w:tblW w:w="0" w:type="auto"/>
    <w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/>
  </w:tblPr>
  <w:tblGrid><w:gridCol w:w="4000"/><w:gridCol w:w="4000"/></w:tblGrid>
  <w:tr>
    <w:trPr><w:cnfStyle w:val="100000000000" w:firstRow="1"/></w:trPr>
    <w:tc><w:tcPr><w:cnfStyle w:val="100000000000" w:firstRow="1"/></w:tcPr><w:p><w:pPr><w:cnfStyle w:val="100000000000" w:firstRow="1"/></w:pPr><w:r><w:t>HEADA</w:t></w:r></w:p></w:tc>
    <w:tc><w:p><w:r><w:t>HEADB</w:t></w:r></w:p></w:tc>
  </w:tr>
  <w:tr>
    <w:trPr><w:cnfStyle w:val="000000100000" w:oddHBand="1"/></w:trPr>
    <w:tc><w:p><w:r><w:t>BAND1</w:t></w:r></w:p></w:tc>
    <w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>
  </w:tr>
  <w:tr>
    <w:tc><w:p><w:r><w:t>PLAIN</w:t></w:r></w:p></w:tc>
    <w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>
  </w:tr>
  <w:tr>
    <w:tc><w:p><w:r><w:t>BAND1</w:t></w:r></w:p></w:tc>
    <w:tc><w:tcPr><w:shd w:val="clear" w:fill="FF0000"/></w:tcPr><w:p><w:r><w:t>DIRECT</w:t></w:r></w:p></w:tc>
  </w:tr>
</w:tbl>
<w:p><w:r><w:t>after</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:docGrid w:linePitch="360"/></w:sectPr>
"#;

fn styled_table_docx() -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>{TABLE_BODY}</w:body></w:document>"#
    );
    let styles = wrap_styles(TABLE_STYLES);
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
    ])
}

/// Every fill painted on the page, flattened out of nested clips/transforms.
fn fills(page: &Page) -> Vec<Rgb> {
    fn walk(item: &Item, out: &mut Vec<Rgb>) {
        match item {
            Item::Rect { fill: Some(f), .. } => out.push(f.rgb()),
            Item::Clip { items, .. } => {
                for nested in items {
                    walk(&nested.item, out);
                }
            }
            Item::Transform { item, .. } => walk(item, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for positioned in &page.items {
        walk(&positioned.item, &mut out);
    }
    out
}

/// Every stroke colour painted as a line (table borders).
fn line_colors(page: &Page) -> Vec<Rgb> {
    fn walk(item: &Item, out: &mut Vec<Rgb>) {
        match item {
            Item::Line { stroke, .. } => out.push(stroke.color.rgb()),
            Item::Clip { items, .. } => {
                for nested in items {
                    walk(&nested.item, out);
                }
            }
            Item::Transform { item, .. } => walk(item, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for positioned in &page.items {
        walk(&positioned.item, &mut out);
    }
    out
}

/// The fill colour of the glyph run whose text is `needle`.
fn text_color(page: &Page, needle: &str) -> Option<Rgb> {
    fn walk(item: &Item, needle: &str, out: &mut Option<Rgb>) {
        match item {
            Item::Glyphs(run) => {
                let text: String = run.glyphs.iter().map(|glyph| glyph.ch).collect();
                if text.contains(needle) {
                    *out = Some(run.color.rgb());
                }
            }
            Item::Clip { items, .. } => {
                for nested in items {
                    walk(&nested.item, needle, out);
                }
            }
            Item::Transform { item, .. } => walk(item, needle, out),
            _ => {}
        }
    }
    let mut out = None;
    for positioned in &page.items {
        walk(&positioned.item, needle, &mut out);
    }
    out
}

#[test]
fn table_style_reference_with_conditional_formatting_converts_and_paints() {
    let bytes = styled_table_docx();

    let doc_profile = profile(&bytes, OfficeFormat::Docx).expect("profile a styled table");
    assert!(
        doc_profile.supported,
        "a table style defined in styles.xml is ordinary Word output, not an unsupported feature; \
         reasons: {:?}",
        doc_profile.reasons
    );

    convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("convert a document whose table carries a built-in Word table style");

    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).expect("open");
    let (laid_out, _warnings) = layout_docx(&doc).expect("lay out");
    let page = laid_out.pages.first().expect("one page");

    let accent = Rgb::new(0x44, 0x72, 0xC4);
    let band = Rgb::new(0xD9, 0xE2, 0xF3);

    let painted = fills(page);
    assert!(
        painted.contains(&accent),
        "the firstRow conditional fills the header cells with {accent:?}; painted fills: {painted:?}"
    );
    assert_eq!(
        painted.iter().filter(|fill| **fill == band).count(),
        3,
        "band1Horz shades body rows 1 and 3 (2 cells each) minus the one cell that overrides the \
         fill directly; painted fills: {painted:?}"
    );
    assert!(
        painted.contains(&Rgb::new(0xFF, 0x00, 0x00)),
        "direct cell shading outranks the table style's conditional band"
    );

    assert!(
        line_colors(page).contains(&accent),
        "the whole-table conditional draws {accent:?} borders; drawn: {:?}",
        line_colors(page)
    );

    assert_eq!(
        text_color(page, "HEADA"),
        Some(Rgb::new(0xFF, 0xFF, 0xFF)),
        "the firstRow conditional's rPr must reach the run cascade (white header text)"
    );
    assert_eq!(
        text_color(page, "PLAIN"),
        Some(Rgb::new(0x00, 0x00, 0x00)),
        "body rows keep the default text colour"
    );
}

#[test]
fn unknown_table_style_id_still_lays_the_table_out() {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>
        <w:tbl><w:tblPr><w:tblStyle w:val="StyleThatIsNotDefined"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid>
        <w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
        </w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels.as_bytes()),
    ]);

    let doc_profile = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(
        doc_profile.supported,
        "a dangling style id carries no formatting; it is not an unsupported feature: {:?}",
        doc_profile.reasons
    );
    convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("a table whose style id resolves to nothing still lays out");
}

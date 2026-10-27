//! A direct `w:numPr` that references a different list than the paragraph
//! style's own numbering promotes the referenced level's indent above the
//! style's own `w:ind` (korean fidelity ground truth: the style binds a
//! margin-hugging label list, and its multi-level list paragraphs override
//! with a numbering whose levels carry real indents Word honors). The
//! style's own indent still wins while its own list is in effect.

use office_core::layout::{Item, WordDoc, layout_docx};
use office_core::testkit::{DOC_NAMESPACES, zip_package};
use office_core::ConvertOptions;

/// Style `Lbl` binds numId 1 (label list, style ind 357/357). The document
/// has one paragraph on the style's own list and one overriding with numId 2
/// (ilvl 0 ind left=1260 hanging=360).
fn override_docx() -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body><w:p><w:pPr><w:pStyle w:val="Lbl"/></w:pPr><w:r><w:t>style list</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Lbl"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>direct list</w:t></w:r></w:p></w:body></w:document>"#
    );
    let styles = format!(
        r#"<?xml version="1.0"?><w:styles{DOC_NAMESPACES}><w:style w:type="paragraph" w:styleId="Lbl"><w:name w:val="Lbl"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr><w:ind w:left="357" w:hanging="357"/></w:pPr></w:style></w:styles>"#
    );
    let numbering = format!(
        r#"<?xml version="1.0"?><w:numbering{DOC_NAMESPACES}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="[%1]"/><w:pPr><w:ind w:left="360" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:pPr><w:ind w:left="1260" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
    ])
}

/// Left x of the first glyph run of each of the two list paragraphs.
fn paragraph_label_xs(bytes: &[u8]) -> Vec<f64> {
    let doc = WordDoc::open(bytes, &ConvertOptions::default()).expect("open fixture");
    let (laid, _) = layout_docx(&doc).expect("layout fixture");
    let mut rows: Vec<(f64, f64)> = Vec::new();
    for page in &laid.pages {
        for item in &page.items {
            if let Item::Glyphs(_) = &item.item {
                let row = rows
                    .iter_mut()
                    .find(|(y, _)| (*y - item.y_pt).abs() < 2.0);
                match row {
                    Some((_, x)) => *x = x.min(item.x_pt),
                    None => rows.push((item.y_pt, item.x_pt)),
                }
            }
        }
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|(_, x)| x).collect()
}

#[test]
fn direct_numbering_reference_promotes_its_level_indent_over_the_styles_own_ind() {
    let xs = paragraph_label_xs(&override_docx());
    assert_eq!(xs.len(), 2, "two list lines expected");
    // Paragraph 1 stays on the style's list: style ind 357/357 → label at the
    // margin (72pt).
    assert!(
        (xs[0] - 72.0).abs() < 0.75,
        "style-list label must honor the style's own ind: x = {}",
        xs[0]
    );
    // Paragraph 2 overrides with numId 2: level ind 1260/360 twips → label at
    // 72 + (1260-360)/20 = 117pt.
    assert!(
        (xs[1] - 117.0).abs() < 0.75,
        "direct-list label must honor the referenced level's ind: x = {}",
        xs[1]
    );
}

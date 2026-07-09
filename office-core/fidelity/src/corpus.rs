//! The fidelity corpus: agent-authored synthetic `source.docx` cases (never
//! customer content) plus their manifests. `seed` (re)generates the sources;
//! a human then exports Word ground truths (human task H3).

use office_core::testkit::{DOC_NAMESPACES, zip_package};
use std::path::Path;

/// One corpus case.
pub struct Case {
    /// Directory / case name.
    pub name: &'static str,
    /// Human title (manifest).
    pub title: &'static str,
    /// `core` cases must pass 100%; `extended` ≥ 90%.
    pub tier: &'static str,
    /// Per-case SSIM threshold.
    pub ssim_threshold: f64,
    /// The source `.docx` bytes.
    pub docx: Vec<u8>,
}

/// A4 page (210×297 mm) with 1" margins, pinned on every case so office-core and
/// Word render the same page geometry rather than each using its own default
/// (office-core → US Letter; an A4-locale Word → A4). Twips: 11906×16838.
const A4_PGSZ_MAR: &str = r#"<w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720"/>"#;

/// `word/styles.xml` pinning document defaults (font, size, paragraph spacing) so
/// neither renderer falls back to its built-in Normal style — Word's is 1.08 line
/// with 8pt after, office-core's is single with 0. Pinning explicit single
/// spacing / 0 after makes both agree (and keeps office-core's output stable).
fn styles_xml() -> String {
    format!(
        r#"<?xml version="1.0"?><w:styles{ns}><w:docDefaults>
<w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:cs="Calibri"/><w:sz w:val="22"/><w:szCs w:val="22"/></w:rPr></w:rPrDefault>
<w:pPrDefault><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr></w:pPrDefault>
</w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/>
<w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr>
<w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:cs="Calibri"/><w:sz w:val="22"/><w:szCs w:val="22"/></w:rPr></w:style>
</w:styles>"#,
        ns = DOC_NAMESPACES
    )
}

/// `[Content_Types].xml` registering the styles part, plus any extra `<Override>`
/// lines a case needs (footer, numbering).
fn content_types(extra_overrides: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
{extra_overrides}</Types>"#
    )
}

/// `word/_rels/document.xml.rels` linking the styles part (id `rIdS`) plus any
/// extra relationships. Word requires these to load styles/numbering/footer;
/// office-core resolves styles by convention but valid rels keep the package sound.
fn document_rels(extra_rels: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rIdS" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
{extra_rels}</Relationships>"#
    )
}

/// A deterministic single-body `.docx`: the body XML followed by an explicit A4
/// section, with a pinned `styles.xml`. Replaces `minimal_docx` for corpus cases
/// so fixtures never depend on a renderer's implicit page-size / spacing defaults.
fn det_docx(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body>{body}<w:sectPr>{sect}</w:sectPr></w:body></w:document>"#,
        ns = DOC_NAMESPACES,
        sect = A4_PGSZ_MAR
    );
    zip_package(&[
        ("[Content_Types].xml", content_types("").as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels("").as_bytes()),
        ("word/styles.xml", styles_xml().as_bytes()),
    ])
}

/// Wraps body XML in a section with a footer that carries PAGE/NUMPAGES fields
/// (A4, with the pinned `styles.xml`).
fn docx_with_footer(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body>{body}
        <w:sectPr><w:footerReference w:type="default" r:id="rId1"/>{sect}</w:sectPr></w:body></w:document>"#,
        ns = DOC_NAMESPACES,
        sect = A4_PGSZ_MAR
    );
    let footer = format!(
        r#"<?xml version="1.0"?><w:ftr{ns}><w:p><w:pPr><w:jc w:val="center"/></w:pPr>
        <w:r><w:t xml:space="preserve">Page </w:t></w:r>
        <w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple>
        <w:r><w:t xml:space="preserve"> of </w:t></w:r>
        <w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>1</w:t></w:r></w:fldSimple>
        </w:p></w:ftr>"#,
        ns = DOC_NAMESPACES
    );
    let ct = content_types(
        r#"<Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>"#,
    );
    let rels = document_rels(
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>"#,
    );
    zip_package(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", rels.as_bytes()),
        ("word/styles.xml", styles_xml().as_bytes()),
        ("word/footer1.xml", footer.as_bytes()),
    ])
}

const ROOT_RELS: &str = r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

/// Builds every corpus case.
pub fn cases() -> Vec<Case> {
    let para = |t: &str| format!("<w:p><w:r><w:t xml:space=\"preserve\">{t}</w:t></w:r></w:p>");
    let lorem = "The quick brown fox jumps over the lazy dog. ".repeat(6);

    let mut cases = Vec::new();
    let mut add = |name, title, tier, thr, docx| {
        cases.push(Case {
            name,
            title,
            tier,
            ssim_threshold: thr,
            docx,
        })
    };

    add(
        "plain-paragraphs",
        "Plain paragraphs",
        "core",
        0.97,
        det_docx(&format!("{}{}{}", para(&lorem), para(&lorem), para(&lorem))),
    );
    add(
        "run-formatting-matrix",
        "Run formatting: bold/italic/underline/strike/color/size",
        "core",
        0.96,
        det_docx(
            r#"<w:p>
              <w:r><w:rPr><w:b/></w:rPr><w:t>Bold </w:t></w:r>
              <w:r><w:rPr><w:i/></w:rPr><w:t>Italic </w:t></w:r>
              <w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>Underline </w:t></w:r>
              <w:r><w:rPr><w:strike/></w:rPr><w:t>Strike </w:t></w:r>
              <w:r><w:rPr><w:color w:val="CC0000"/><w:sz w:val="32"/></w:rPr><w:t>Red16</w:t></w:r>
            </w:p>"#,
        ),
    );
    add(
        "justify-long-text",
        "Justified paragraph",
        "core",
        0.95,
        det_docx(&format!(
            r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
            lorem.repeat(3)
        )),
    );
    add(
        "fonts-substitution",
        "Font substitution: Calibri/Cambria/Arial/Times",
        "core",
        0.94,
        det_docx(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Calibri"/></w:rPr><w:t>Calibri </w:t></w:r>
              <w:r><w:rPr><w:rFonts w:ascii="Cambria"/></w:rPr><w:t>Cambria </w:t></w:r>
              <w:r><w:rPr><w:rFonts w:ascii="Arial"/></w:rPr><w:t>Arial </w:t></w:r>
              <w:r><w:rPr><w:rFonts w:ascii="Times New Roman"/></w:rPr><w:t>Times</w:t></w:r></w:p>"#,
        ),
    );
    add(
        "table-fixed",
        "Fixed-layout table with borders",
        "core",
        0.95,
        det_docx(&bordered_table(3, 3)),
    );
    add(
        "table-merges",
        "Table with gridSpan and vMerge",
        "core",
        0.94,
        det_docx(MERGE_TABLE),
    );
    add(
        "table-nested",
        "Nested table inside a cell",
        "extended",
        0.93,
        det_docx(NESTED_TABLE),
    );
    add(
        "lists-multilevel",
        "Multi-level numbered list",
        "extended",
        0.93,
        numbered_list_docx(),
    );
    add(
        "hyperlinks",
        "External hyperlink",
        "extended",
        0.95,
        hyperlink_docx(),
    );
    add(
        "tabs-leaders",
        "Tab stops",
        "extended",
        0.94,
        det_docx(
            r#"<w:p><w:pPr><w:tabs><w:tab w:val="right" w:pos="8640"/></w:tabs></w:pPr>
              <w:r><w:t>Left</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>Right</w:t></w:r></w:p>"#,
        ),
    );
    add(
        "spacing-exact-atleast",
        "Exact and at-least line spacing",
        "extended",
        0.94,
        det_docx(
            r#"<w:p><w:pPr><w:spacing w:line="480" w:lineRule="auto"/></w:pPr><w:r><w:t>Double spaced line one and a fairly long continuation to force wrapping across the page width.</w:t></w:r></w:p>"#,
        ),
    );
    add(
        "page-fields",
        "Footer with PAGE / NUMPAGES fields",
        "core",
        0.95,
        docx_with_footer(&para(&lorem).repeat(180)),
    );
    add(
        "headers-footers",
        "Footer present on every page",
        "core",
        0.95,
        docx_with_footer(&para("Body content with a running footer.").repeat(4)),
    );
    add(
        "long-doc-pagination",
        "Long document pagination stability",
        "core",
        0.95,
        det_docx(&para(&lorem).repeat(120)),
    );

    cases
}

fn bordered_table(rows: usize, cols: usize) -> String {
    let grid: String = (0..cols).map(|_| "<w:gridCol w:w=\"2600\"/>").collect();
    let mut body = String::from(
        r#"<w:tbl><w:tblPr><w:tblBorders>
          <w:top w:val="single" w:sz="6"/><w:left w:val="single" w:sz="6"/>
          <w:bottom w:val="single" w:sz="6"/><w:right w:val="single" w:sz="6"/>
          <w:insideH w:val="single" w:sz="6"/><w:insideV w:val="single" w:sz="6"/>
        </w:tblBorders></w:tblPr>"#,
    );
    body.push_str(&format!("<w:tblGrid>{grid}</w:tblGrid>"));
    for r in 0..rows {
        body.push_str("<w:tr>");
        for c in 0..cols {
            body.push_str(&format!(
                "<w:tc><w:p><w:r><w:t>R{r}C{c}</w:t></w:r></w:p></w:tc>"
            ));
        }
        body.push_str("</w:tr>");
    }
    body.push_str("</w:tbl>");
    body
}

const MERGE_TABLE: &str = r#"<w:tbl>
  <w:tblPr><w:tblBorders><w:top w:val="single" w:sz="6"/><w:left w:val="single" w:sz="6"/>
    <w:bottom w:val="single" w:sz="6"/><w:right w:val="single" w:sz="6"/>
    <w:insideH w:val="single" w:sz="6"/><w:insideV w:val="single" w:sz="6"/></w:tblBorders></w:tblPr>
  <w:tblGrid><w:gridCol w:w="2600"/><w:gridCol w:w="2600"/><w:gridCol w:w="2600"/></w:tblGrid>
  <w:tr>
    <w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>Merged</w:t></w:r></w:p></w:tc>
    <w:tc><w:p><w:r><w:t>C</w:t></w:r></w:p></w:tc>
  </w:tr>
  <w:tr>
    <w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge/></w:tcPr><w:p/></w:tc>
    <w:tc><w:p><w:r><w:t>F</w:t></w:r></w:p></w:tc>
  </w:tr>
</w:tbl>"#;

const NESTED_TABLE: &str = r#"<w:tbl>
  <w:tblPr><w:tblBorders><w:top w:val="single" w:sz="6"/><w:left w:val="single" w:sz="6"/>
    <w:bottom w:val="single" w:sz="6"/><w:right w:val="single" w:sz="6"/></w:tblBorders></w:tblPr>
  <w:tblGrid><w:gridCol w:w="6000"/></w:tblGrid>
  <w:tr><w:tc>
    <w:p><w:r><w:t>Outer</w:t></w:r></w:p>
    <w:tbl><w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid>
      <w:tr><w:tc><w:p><w:r><w:t>Inner</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>
  </w:tc></w:tr>
</w:tbl>"#;

fn numbered_list_docx() -> Vec<u8> {
    let numbering = format!(
        r#"<?xml version="1.0"?><w:numbering{ns}>
        <w:abstractNum w:abstractNumId="0">
          <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>
            <w:pPr><w:ind w:left="360" w:hanging="360"/></w:pPr></w:lvl>
          <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%2)"/>
            <w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl>
        </w:abstractNum>
        <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#,
        ns = DOC_NAMESPACES
    );
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Second</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Sub a</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Sub b</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Third</w:t></w:r></w:p>
        <w:sectPr>{sect}</w:sectPr></w:body></w:document>"#,
        ns = DOC_NAMESPACES,
        sect = A4_PGSZ_MAR
    );
    let ct = content_types(
        r#"<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#,
    );
    let rels = document_rels(
        r#"<Relationship Id="rIdN" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#,
    );
    zip_package(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", rels.as_bytes()),
        ("word/styles.xml", styles_xml().as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
    ])
}

fn hyperlink_docx() -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body>
        <w:p><w:r><w:t xml:space="preserve">Visit </w:t></w:r>
        <w:hyperlink r:id="rId2"><w:r><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr><w:t>example.com</w:t></w:r></w:hyperlink></w:p>
        <w:sectPr>{sect}</w:sectPr></w:body></w:document>"#,
        ns = DOC_NAMESPACES,
        sect = A4_PGSZ_MAR
    );
    let rels = document_rels(
        r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com" TargetMode="External"/>"#,
    );
    zip_package(&[
        ("[Content_Types].xml", content_types("").as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", rels.as_bytes()),
        ("word/styles.xml", styles_xml().as_bytes()),
    ])
}

/// Writes every case's `source.docx` + `manifest.toml` under `corpus_root/docx/`.
pub fn seed(corpus_root: &Path) -> anyhow::Result<usize> {
    let mut count = 0;
    for case in cases() {
        let dir = corpus_root.join("docx").join(case.name);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("source.docx"), &case.docx)?;
        let manifest = format!(
            "title = {:?}\ntier = {:?}\nssim_threshold = {}\nmax_page_count_delta = 0\nword_version = \"TBD (fill on export)\"\nnotes = \"Synthetic fixture — export ground-truth.pdf from Word (human task H3).\"\n",
            case.title, case.tier, case.ssim_threshold
        );
        std::fs::write(dir.join("manifest.toml"), manifest)?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use office_core::{ConvertOptions, OfficeFormat, convert};

    /// Every corpus case must convert inside the envelope (an intentionally
    /// unsupported case belongs in unit tests, not the corpus).
    #[test]
    fn every_case_is_in_envelope() {
        for case in cases() {
            let out = convert(&case.docx, OfficeFormat::Docx, &ConvertOptions::default())
                .unwrap_or_else(|e| panic!("case {} is out of envelope: {e}", case.name));
            assert!(out.page_count >= 1, "case {} produced 0 pages", case.name);
        }
    }
}

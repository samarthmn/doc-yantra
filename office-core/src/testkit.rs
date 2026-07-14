//! Fixture builders shared by unit and integration tests. Compiled into the
//! library so `tests/*.rs` can use them; not part of the stable public API.
//!
//! Fixtures are assembled as real OPC (ZIP) packages so they exercise the same
//! [`crate::opc`] path production files take.

use std::io::{Cursor, Write};
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

/// The namespace declarations placed on the root of every generated
/// `document.xml` so fixtures can use any common OOXML prefix.
pub const DOC_NAMESPACES: &str = concat!(
    r#" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#,
    r#" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#,
    r#" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#,
    r#" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#,
    r#" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing""#,
    r#" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#,
    r#" xmlns:v="urn:schemas-microsoft-com:vml""#,
);

/// Zips the given `(part_name, bytes)` entries into an OPC package (Deflate).
pub fn zip_package(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zw = ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in parts {
            zw.start_file(*name, opts).expect("start_file");
            zw.write_all(data).expect("write part");
        }
        zw.finish().expect("finish zip");
    }
    buf
}

/// Wraps body-level XML in a complete `word/document.xml` (with a final section).
pub fn wrap_document(body_inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document{ns}><w:body>{body}</w:body></w:document>"#,
        ns = DOC_NAMESPACES,
        body = body_inner,
    )
}

/// Wraps run/paragraph XML in a complete `word/styles.xml` document.
pub fn wrap_styles(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles{ns}>{inner}</w:styles>"#,
        ns = DOC_NAMESPACES,
    )
}

/// Wraps level XML in a complete `word/numbering.xml` document.
pub fn wrap_numbering(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering{ns}>{inner}</w:numbering>"#,
        ns = DOC_NAMESPACES,
    )
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

/// Builds a complete, openable minimal `.docx` package around body XML.
pub fn minimal_docx(body_inner: &str) -> Vec<u8> {
    let document = wrap_document(body_inner);
    zip_package(&[
        ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
    ])
}

/// Builds a compact Google-Docs-style DOCX conformance fixture.
///
/// The package intentionally combines producer idioms that are individually
/// invisible or supported in final view, so parser regressions cannot silently
/// shrink the profile envelope again.
pub fn gdocs_paragraph_docx() -> Vec<u8> {
    let document = wrap_document(
        r#"
        <w:p><w:pPr><w:rPr/></w:pPr><w:r><w:t>GDocs paragraph probe</w:t></w:r></w:p>
        <w:p>
          <w:pPr>
            <w:jc w:val="left"/>
            <w:ind w:firstLine="192.00000000000003"/>
            <w:rPr><w:color w:val="336699"/><w:sz w:val="22.0"/><w:vertAlign w:val="baseline"/></w:rPr>
          </w:pPr>
          <w:r>
            <w:rPr>
              <w:b w:val="0"/><w:i w:val="0"/><w:smallCaps w:val="0"/>
              <w:strike w:val="0"/><w:u w:val="none"/><w:rtl w:val="0"/>
            </w:rPr>
            <w:t>GDocs toggles probe</w:t>
          </w:r>
        </w:p>
        <w:tbl>
          <w:tblPr><w:tblInd w:w="-108.0" w:type="dxa"/></w:tblPr>
          <w:tblGrid>
            <w:gridCol w:w="2400.5"/>
            <w:tblGridChange w:id="1"><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid></w:tblGridChange>
          </w:tblGrid>
          <w:tr>
            <w:trPr><w:tblHeader w:val="0"/><w:cantSplit w:val="0"/></w:trPr>
            <w:tc><w:tcPr/><w:p><w:r><w:t>GDocs table probe</w:t></w:r></w:p></w:tc>
          </w:tr>
        </w:tbl>
        <w:sectPr>
          <w:footerReference w:type="default" r:id="rIdFooter"/>
          <w:pgNumType w:start="1" w:fmt="decimal"/>
        </w:sectPr>
        "#,
    );
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>
</Types>"#;
    let document_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rIdFooter" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>
</Relationships>"#;
    let footer = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr{ns}><w:p><w:r><w:t>GDocs footer probe</w:t></w:r></w:p></w:ftr>"#,
        ns = DOC_NAMESPACES,
    );
    zip_package(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels),
        ("word/footer1.xml", footer.as_bytes()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opc::Package;

    #[test]
    fn minimal_docx_opens_and_finds_main_part() {
        let bytes = minimal_docx(r#"<w:p><w:r><w:t>Hi</w:t></w:r></w:p>"#);
        let pkg = Package::open(&bytes).expect("open");
        assert_eq!(pkg.main_part().unwrap(), "word/document.xml");
        assert!(pkg.part("word/document.xml").is_some());
    }
}

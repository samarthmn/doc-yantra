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

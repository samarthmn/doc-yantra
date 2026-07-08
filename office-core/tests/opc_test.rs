//! OPC container integration tests (agent task T1.2).

use office_core::opc::Package;
use office_core::testkit;
use office_core::{OfficeError, UnsupportedCode};
use std::path::Path;

/// Every real OOXML fixture under `../mock-files/` opens and exposes a main part.
#[test]
fn opens_every_mock_office_file() {
    let mut checked = 0;
    for sub in ["docx", "pptx", "xlsx", "xlsm"] {
        let dir = Path::new("../mock-files").join(sub);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(sub) {
                continue;
            }
            let bytes = std::fs::read(&path).expect("read fixture");
            let pkg =
                Package::open(&bytes).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
            let main = pkg
                .main_part()
                .unwrap_or_else(|e| panic!("main_part {}: {e}", path.display()));
            assert!(
                pkg.part(&main).is_some(),
                "main part {main} missing bytes in {}",
                path.display()
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "expected at least one mock office file to check"
    );
}

/// A non-zip payload (e.g. a `.bin` renamed to `.docx`) is `Invalid`.
#[test]
fn non_zip_payload_is_invalid() {
    let bytes = b"\x89PNG\r\n\x1a\n not a zip at all just some bytes here";
    match Package::open(bytes) {
        Err(OfficeError::Invalid(_)) => {}
        other => panic!("expected Invalid, got {other:?}"),
    }
}

/// A legacy binary `.doc` (CFB container) is `Unsupported{LegacyFormat}`.
#[test]
fn legacy_doc_is_unsupported() {
    let path = Path::new("../mock-files/doc/file1.doc");
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("skipping: {} not present", path.display());
        return;
    };
    // Only assert the CFB verdict when the fixture really is a CFB container.
    if bytes.len() >= 4 && bytes[0..4] == [0xD0, 0xCF, 0x11, 0xE0] {
        match Package::open(&bytes) {
            Err(OfficeError::Unsupported {
                code: UnsupportedCode::LegacyFormat | UnsupportedCode::Encrypted,
                ..
            }) => {}
            other => panic!("expected LegacyFormat/Encrypted, got {other:?}"),
        }
    }
}

/// Content-type lookup resolves overrides and extension defaults.
#[test]
fn content_types_resolve() {
    let bytes = testkit::minimal_docx(r#"<w:p><w:r><w:t>hi</w:t></w:r></w:p>"#);
    let pkg = Package::open(&bytes).expect("open");
    assert_eq!(
        pkg.content_type("word/document.xml"),
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml")
    );
    // `.rels` resolves via a Default extension entry.
    assert_eq!(
        pkg.content_type("_rels/.rels"),
        Some("application/vnd.openxmlformats-package.relationships+xml")
    );
}

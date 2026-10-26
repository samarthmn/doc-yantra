//! RK-7: owner-locked PDFs with an empty user password remain processable.

use lopdf::Document;
use pdf_core::assembly::{extract_pages, merge};
use pdf_core::meta::page_count;
use pdf_core::redact_pure::redact_pure;
use pdf_core::redact_types::RedactArea;
use pdf_core::testkit::sample_pdf;

// Produced with pypdf 6.x from tests/fixtures/samples/pdf/file1.pdf using an empty user
// password and a non-empty owner password. Old PyMuPDF shipped decrypted output.
const OWNER_LOCKED: &[u8] = include_bytes!("../../tests/fixtures/samples/pdf/owner-locked-empty-user-pw.pdf");

#[test]
fn owner_locked_page_count_and_extract_succeed() {
    assert_eq!(page_count(OWNER_LOCKED).expect("page count"), 1);
    let extracted = extract_pages(OWNER_LOCKED, &[1]).expect("extract page");
    let doc = Document::load_mem(&extracted).expect("reload extracted output");
    assert_eq!(doc.get_pages().len(), 1);
    assert!(!doc.is_encrypted());
}

#[test]
fn owner_locked_merge_succeeds_and_emits_unencrypted_pdf() {
    let plain = sample_pdf(&["PLAIN-MERGE-PAGE"]);
    let merged = merge(&[OWNER_LOCKED, &plain]).expect("merge owner-locked and plain");
    let doc = Document::load_mem(&merged).expect("reload merged output");
    assert_eq!(doc.get_pages().len(), 2);
    assert!(!doc.is_encrypted());
}

#[test]
fn owner_locked_redaction_succeeds_and_removes_original_text() {
    let input = Document::load_mem(OWNER_LOCKED).expect("load owner-locked fixture");
    let original_text = input.extract_text(&[1]).expect("extract fixture text");
    assert!(
        !original_text.trim().is_empty(),
        "fixture must contain target text"
    );

    let output = redact_pure(
        OWNER_LOCKED,
        &[RedactArea {
            page_index: 0,
            left: 0.0,
            top: 0.0,
            width: 100.0,
            height: 100.0,
        }],
    )
    .expect("redact owner-locked PDF");
    let doc = Document::load_mem(&output.pdf).expect("reload redacted output");
    assert!(!doc.is_encrypted());
    let redacted_text = doc.extract_text(&[1]).expect("extract redacted text");
    assert!(
        redacted_text.trim().is_empty(),
        "secret text survived: {redacted_text:?}"
    );
}

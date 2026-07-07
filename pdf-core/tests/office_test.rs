//! Office → PDF (pure-Rust office2pdf path) exercised against the SHARED
//! workspace mock corpus (`../mock-files`), not crate-local synthetic
//! fixtures. Assertions are content-ROBUST: structural validity via
//! pdf-core's own parser, `page_count >= 1`, and warnings tracked — so
//! refreshing the mock corpus never breaks the suite on brittle exact-string
//! matches. Light content probes assert the words plainly present in each
//! mock document survived the round-trip.

#![cfg(all(feature = "office", feature = "pdfium"))]

use pdf_core::office::{OfficeFormat, OfficeOutput, office_to_pdf, office_to_pdf_by_extension};

/// Reads a file from the workspace-shared mock corpus (sibling of the crate),
/// the same corpus DocKosha's own suites run against.
fn mock(rel: &str) -> Vec<u8> {
    let path = format!("{}/../mock-files/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read mock {path}: {e}"))
}

fn extract_all_text(pdf: &[u8]) -> String {
    pdf_core::extract::extract_text(pdf).expect("extract text from converted pdf")
}

/// The structural contract every successful conversion must satisfy,
/// regardless of the specific document: real PDF bytes, at least one page
/// that our own parser accepts, and no fidelity warnings.
fn assert_sound_conversion(out: &OfficeOutput, ctx: &str) {
    assert!(out.pdf.starts_with(b"%PDF"), "{ctx}: output must be a PDF");
    let pages = pdf_core::meta::page_count(&out.pdf)
        .unwrap_or_else(|e| panic!("{ctx}: converted pdf must re-parse: {e}"));
    assert!(pages >= 1, "{ctx}: expected >= 1 page, got {pages}");
    assert!(
        out.warnings.is_empty(),
        "{ctx}: should convert warning-free, got: {:?}",
        out.warnings
    );
}

#[test]
fn docx_converts_with_content_intact() {
    let out = office_to_pdf(&mock("docx/file1.docx"), OfficeFormat::Docx).expect("docx converts");
    assert_sound_conversion(&out, "docx/file1");
    let text = extract_all_text(&out.pdf);
    assert!(text.contains("Dummy"), "body word missing: {text}");
    assert!(text.contains("Document"), "body word missing: {text}");
}

#[test]
fn pptx_converts_with_slide_text_intact() {
    let out = office_to_pdf(&mock("pptx/file1.pptx"), OfficeFormat::Pptx).expect("pptx converts");
    assert_sound_conversion(&out, "pptx/file1");
    let text = extract_all_text(&out.pdf);
    assert!(text.contains("Dummy"), "slide text missing: {text}");
}

#[test]
fn xlsx_converts_with_cell_data_intact() {
    let out = office_to_pdf(&mock("xlsx/file1.xlsx"), OfficeFormat::Xlsx).expect("xlsx converts");
    assert_sound_conversion(&out, "xlsx/file1");
    let text = extract_all_text(&out.pdf);
    assert!(text.contains("Column"), "header cell missing: {text}");
}

/// The second variant of every supported format must also convert soundly —
/// proves the pipeline is not overfit to a single hand-picked document.
#[test]
fn second_variants_all_convert_soundly() {
    for (rel, fmt) in [
        ("docx/file2.docx", OfficeFormat::Docx),
        ("pptx/file2.pptx", OfficeFormat::Pptx),
        ("xlsx/file2.xlsx", OfficeFormat::Xlsx),
    ] {
        let out = office_to_pdf(&mock(rel), fmt).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_sound_conversion(&out, rel);
    }
}

#[test]
fn extension_routing_matches_scope() {
    // Modern OOXML maps to the pure-Rust engine…
    assert_eq!(
        OfficeFormat::from_extension("docx"),
        Some(OfficeFormat::Docx)
    );
    assert_eq!(
        OfficeFormat::from_extension("PPTX"),
        Some(OfficeFormat::Pptx)
    );
    assert_eq!(
        OfficeFormat::from_extension(" xlsx "),
        Some(OfficeFormat::Xlsx)
    );
    // …legacy binary formats and macro workbooks must NOT (LibreOffice territory).
    for legacy in ["doc", "ppt", "xls", "xlsm", "csv", "pdf", ""] {
        assert_eq!(OfficeFormat::from_extension(legacy), None, "{legacy}");
    }
}

#[test]
fn unsupported_extension_errors_as_unsupported() {
    let err = office_to_pdf_by_extension(&mock("docx/file1.docx"), "xlsm").unwrap_err();
    assert!(
        matches!(err, pdf_core::PdfCoreError::Unsupported(_)),
        "xlsm must route to LibreOffice, got: {err}"
    );
}

#[test]
fn corrupt_bytes_error_cleanly() {
    let err = office_to_pdf(&[0x50, 0x4B, 0x03, 0x04, 1, 2, 3], OfficeFormat::Docx).unwrap_err();
    assert!(
        matches!(err, pdf_core::PdfCoreError::InvalidInput(_)),
        "corrupt zip must be InvalidInput, got: {err}"
    );
}

#[test]
fn empty_input_errors_cleanly() {
    let err = office_to_pdf(&[], OfficeFormat::Docx).unwrap_err();
    assert!(matches!(err, pdf_core::PdfCoreError::InvalidInput(_)));
}

#[test]
fn wrong_format_for_bytes_errors_fast() {
    // Feed the xlsx bytes through the docx path (a renamed file): must be
    // rejected by the content sniff in MILLISECONDS — the underlying parser
    // can hang for minutes on mismatched bytes, which a synchronous
    // in-process engine cannot afford.
    let start = std::time::Instant::now();
    let result = office_to_pdf(&mock("xlsx/file1.xlsx"), OfficeFormat::Docx);
    assert!(result.is_err(), "xlsx-as-docx must fail cleanly");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "mismatch rejection must be fast, took {:?}",
        start.elapsed()
    );
    // And every correct pairing still passes the sniff (no false rejects) —
    // covered by the conversion tests above.
}

#[test]
fn converted_docx_flows_through_downstream_ops() {
    // The converted PDF must be a first-class citizen for the rest of the
    // engine: watermark + merge + page ops.
    let out = office_to_pdf(&mock("docx/file1.docx"), OfficeFormat::Docx).unwrap();
    let spec = pdf_core::watermark::WatermarkSpec {
        pattern: pdf_core::watermark::Pattern::Grid,
        rotation_deg: -40.0,
        lines: vec!["CONFIDENTIAL".into()],
        ..pdf_core::watermark::WatermarkSpec::default()
    };
    let stamped = pdf_core::watermark::apply_watermark(&out.pdf, &spec).expect("watermarkable");
    let merged = pdf_core::assembly::merge(&[&stamped, &out.pdf]).expect("mergeable with itself");
    let text = extract_all_text(&merged);
    assert!(text.contains("CONFIDENTIAL"), "watermark mark missing");
    assert!(text.contains("Dummy"), "source content missing after merge");
}

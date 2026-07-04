//! Office → PDF (pure-Rust office2pdf path). Verifies real behavior on the
//! committed fixtures: structural validity via pdf-core's own parser and
//! content presence via pdfium text extraction.

#![cfg(all(feature = "office", feature = "pdfium"))]

use pdf_core::office::{OfficeFormat, office_to_pdf, office_to_pdf_by_extension};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
}

fn extract_all_text(pdf: &[u8]) -> String {
    pdf_core::extract::extract_text(pdf).expect("extract text from converted pdf")
}

#[test]
fn docx_converts_with_content_intact() {
    let out = office_to_pdf(&fixture("sample.docx"), OfficeFormat::Docx).expect("docx converts");
    assert!(out.pdf.starts_with(b"%PDF"), "output must be a PDF");
    assert!(
        pdf_core::meta::page_count(&out.pdf).expect("valid pdf") >= 1,
        "at least one page"
    );
    let text = extract_all_text(&out.pdf);
    for expected in [
        "Quarterly Report",
        "18.4 percent",
        "North America",
        "125000",
        "Closing summary paragraph.",
    ] {
        assert!(text.contains(expected), "missing '{expected}' in: {text}");
    }
    assert!(
        out.warnings.is_empty(),
        "fixture should convert warning-free, got: {:?}",
        out.warnings
    );
}

#[test]
fn xlsx_converts_with_cell_data_intact() {
    let out = office_to_pdf(&fixture("sample.xlsx"), OfficeFormat::Xlsx).expect("xlsx converts");
    let text = extract_all_text(&out.pdf);
    for expected in ["Region", "Revenue", "125000", "Europe", "98000"] {
        assert!(text.contains(expected), "missing '{expected}' in: {text}");
    }
}

#[test]
fn pptx_converts_with_slide_text_intact() {
    let out = office_to_pdf(&fixture("sample.pptx"), OfficeFormat::Pptx).expect("pptx converts");
    let text = extract_all_text(&out.pdf);
    assert!(
        text.contains("Board Deck Q2"),
        "slide title present: {text}"
    );
    assert!(
        text.contains("Growth highlights"),
        "slide body present: {text}"
    );
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
    let err = office_to_pdf_by_extension(&fixture("sample.docx"), "xlsm").unwrap_err();
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
    let result = office_to_pdf(&fixture("sample.xlsx"), OfficeFormat::Docx);
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
    let out = office_to_pdf(&fixture("sample.docx"), OfficeFormat::Docx).unwrap();
    let spec = pdf_core::watermark::WatermarkSpec {
        pattern: pdf_core::watermark::Pattern::Grid,
        rotation_deg: -40.0,
        lines: vec!["CONFIDENTIAL".into()],
        ..pdf_core::watermark::WatermarkSpec::default()
    };
    let stamped = pdf_core::watermark::apply_watermark(&out.pdf, &spec).expect("watermarkable");
    let merged = pdf_core::assembly::merge(&[&stamped, &out.pdf]).expect("mergeable with itself");
    let text = extract_all_text(&merged);
    assert!(text.contains("CONFIDENTIAL"));
    assert!(text.contains("Quarterly Report"));
}

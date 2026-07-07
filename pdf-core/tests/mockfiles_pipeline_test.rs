//! End-to-end pipeline over the SHARED workspace mock corpus
//! (`../mock-files`). Proves the engine's features compose on REAL inputs:
//! every convertible source (docx / pptx / xlsx via office2pdf, csv / md via
//! the generator) is driven through the full downstream chain —
//! convert → watermark → merge → extract → page_count → raster — with each
//! stage asserted. This complements the per-module unit tests (which use
//! synthetic fixtures) by catching integration gaps that only surface on
//! real documents.

#![cfg(all(feature = "office", feature = "pdfium"))]

use pdf_core::generate::{self, DocOptions};
use pdf_core::office::{OfficeFormat, office_to_pdf};
use pdf_core::watermark::{Pattern, WatermarkSpec};
use pdf_core::{assembly, extract, meta, raster};
use serial_test::serial;

/// Reads a file from the workspace-shared mock corpus (sibling of the crate).
fn mock(rel: &str) -> Vec<u8> {
    let path = format!("{}/../mock-files/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read mock {path}: {e}"))
}

/// A distinctive mark so an incidental collision with source content cannot
/// mask a broken watermark stage.
const MARK: &str = "PIPELINE-MARK-9Q7";

/// Runs one real base PDF through the complete downstream feature chain,
/// asserting each stage produced sound output.
fn assert_full_pipeline(base: Vec<u8>, label: &str) {
    // page_count on the freshly-produced base
    let base_pages =
        meta::page_count(&base).unwrap_or_else(|e| panic!("{label}: page_count(base): {e}"));
    assert!(base_pages >= 1, "{label}: base must have >= 1 page");
    assert!(
        !extract::extract_text(&base)
            .unwrap_or_default()
            .contains(MARK),
        "{label}: sanity — the mark must not pre-exist in the source"
    );

    // watermark
    let spec = WatermarkSpec {
        pattern: Pattern::Grid,
        rotation_deg: -40.0,
        lines: vec![MARK.into()],
        ..WatermarkSpec::default()
    };
    let stamped = pdf_core::watermark::apply_watermark(&base, &spec)
        .unwrap_or_else(|e| panic!("{label}: watermark: {e}"));
    assert_ne!(stamped, base, "{label}: watermark must change the bytes");
    assert_eq!(
        meta::page_count(&stamped).unwrap(),
        base_pages,
        "{label}: watermark preserves the page count"
    );
    assert!(
        extract::extract_text(&stamped).unwrap().contains(MARK),
        "{label}: the drawn mark must be extractable after watermarking"
    );

    // merge (stamped + original) — page counts are additive
    let merged =
        assembly::merge(&[&stamped, &base]).unwrap_or_else(|e| panic!("{label}: merge: {e}"));
    assert_eq!(
        meta::page_count(&merged).unwrap(),
        base_pages * 2,
        "{label}: merged page count is the sum of inputs"
    );

    // extract over the merged document still finds the mark
    assert!(
        extract::extract_text(&merged).unwrap().contains(MARK),
        "{label}: the mark must survive the merge"
    );

    // search API locates the mark too
    let hits =
        extract::search(&merged, MARK, false).unwrap_or_else(|e| panic!("{label}: search: {e}"));
    assert!(!hits.is_empty(), "{label}: search must find the mark");

    // raster: the first merged page renders to a real PNG
    let thumbs = raster::render_thumbnails(&merged, Some(&[0]), 128)
        .unwrap_or_else(|e| panic!("{label}: raster: {e}"));
    assert_eq!(thumbs.len(), 1, "{label}: exactly one thumbnail requested");
    assert!(
        thumbs[0].starts_with(&[0x89, b'P', b'N', b'G']),
        "{label}: thumbnail must be PNG-encoded"
    );
}

#[test]
#[serial]
fn docx_real_file_pipeline() {
    let base = office_to_pdf(&mock("docx/file1.docx"), OfficeFormat::Docx)
        .expect("docx converts")
        .pdf;
    assert_full_pipeline(base, "docx");
}

#[test]
#[serial]
fn pptx_real_file_pipeline() {
    let base = office_to_pdf(&mock("pptx/file1.pptx"), OfficeFormat::Pptx)
        .expect("pptx converts")
        .pdf;
    assert_full_pipeline(base, "pptx");
}

#[test]
#[serial]
fn xlsx_real_file_pipeline() {
    let base = office_to_pdf(&mock("xlsx/file1.xlsx"), OfficeFormat::Xlsx)
        .expect("xlsx converts")
        .pdf;
    assert_full_pipeline(base, "xlsx");
}

#[test]
#[serial]
fn csv_real_file_pipeline() {
    let base =
        generate::csv_to_pdf(&mock("csv/file1.csv"), &DocOptions::default()).expect("csv renders");
    assert_full_pipeline(base, "csv");
}

#[test]
#[serial]
fn markdown_real_file_pipeline() {
    let md = String::from_utf8(mock("md/file1.md")).expect("md is utf-8");
    let base = generate::markdown_to_pdf(&md, &DocOptions::default()).expect("md renders");
    assert_full_pipeline(base, "md");
}

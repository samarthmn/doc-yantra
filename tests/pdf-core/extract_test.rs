//! Integration tests for `pdf_core::extract` — real behavior asserted against
//! pdfium-extracted text and search geometry on generated fixture PDFs.

#![cfg(feature = "pdfium")]

use pdf_core::assembly::rotate_pages;
use pdf_core::error::PdfCoreError;
use pdf_core::extract::{extract_text, extract_text_pages, search};
use pdf_core::testkit::sample_pdf;

#[test]
fn extract_text_contains_all_page_strings() {
    let pdf = sample_pdf(&["Alpha bravo", "Charlie delta"]);
    let text = extract_text(&pdf).expect("extract_text");
    assert!(
        text.contains("Alpha bravo"),
        "missing page 1 text: {text:?}"
    );
    assert!(
        text.contains("Charlie delta"),
        "missing page 2 text: {text:?}"
    );
}

#[test]
fn extract_text_joins_pages_with_blank_line() {
    let pdf = sample_pdf(&["Alpha bravo", "Charlie delta"]);
    let text = extract_text(&pdf).expect("extract_text");
    let separator = text.find("\n\n").expect("pages joined with \\n\\n");
    let (first, second) = text.split_at(separator);
    assert!(first.contains("Alpha bravo"));
    assert!(second.contains("Charlie delta"));
}

#[test]
fn extract_text_pages_returns_one_entry_per_page_in_order() {
    let pdf = sample_pdf(&["Alpha bravo", "Charlie delta"]);
    let pages = extract_text_pages(&pdf).expect("extract_text_pages");
    assert_eq!(pages.len(), 2);
    assert!(
        pages[0].contains("Alpha bravo"),
        "page 0 text wrong: {:?}",
        pages[0]
    );
    assert!(
        !pages[0].contains("Charlie delta"),
        "page 0 leaked page 1 text: {:?}",
        pages[0]
    );
    assert!(
        pages[1].contains("Charlie delta"),
        "page 1 text wrong: {:?}",
        pages[1]
    );
    assert!(
        !pages[1].contains("Alpha bravo"),
        "page 1 leaked page 0 text: {:?}",
        pages[1]
    );
}

#[test]
fn search_finds_single_hit_with_plausible_geometry() {
    let pdf = sample_pdf(&["Alpha bravo", "Charlie delta"]);
    let hits = search(&pdf, "bravo", true).expect("search");
    assert_eq!(hits.len(), 1, "expected exactly one hit: {hits:?}");

    let hit = &hits[0];
    assert_eq!(hit.page_index, 0);

    let (x, y, w, h) = hit.rect;
    assert!(w > 0.0, "width must be positive: {w}");
    assert!(h > 0.0, "height must be positive: {h}");
    // Within a US Letter page (612 x 792 points).
    assert!(x >= 0.0 && x + w <= 612.0, "x out of page: {:?}", hit.rect);
    assert!(y >= 0.0 && y + h <= 792.0, "y out of page: {:?}", hit.rect);
    // Near the fixture draw position: text starts at (72, 720), 24pt.
    assert!(x >= 60.0, "x too far left: {x}");
    assert!(y >= 600.0, "y too low: {y}");
}

#[test]
fn search_case_insensitive_matches_different_case() {
    let pdf = sample_pdf(&["Alpha bravo", "Charlie delta"]);
    let hits = search(&pdf, "BRAVO", false).expect("search");
    assert_eq!(
        hits.len(),
        1,
        "case-insensitive should find bravo: {hits:?}"
    );
    assert_eq!(hits[0].page_index, 0);
}

#[test]
fn search_case_sensitive_rejects_different_case() {
    let pdf = sample_pdf(&["Alpha bravo", "Charlie delta"]);
    let hits = search(&pdf, "BRAVO", true).expect("search");
    assert!(
        hits.is_empty(),
        "case-sensitive must not match BRAVO: {hits:?}"
    );
}

#[test]
fn search_finds_all_occurrences_across_pages() {
    let pdf = sample_pdf(&["dup dup", "dup"]);
    let hits = search(&pdf, "dup", true).expect("search");
    assert_eq!(hits.len(), 3, "expected 3 hits: {hits:?}");

    let page0: Vec<_> = hits.iter().filter(|h| h.page_index == 0).collect();
    let page1: Vec<_> = hits.iter().filter(|h| h.page_index == 1).collect();
    assert_eq!(page0.len(), 2, "two hits on page 0: {hits:?}");
    assert_eq!(page1.len(), 1, "one hit on page 1: {hits:?}");

    // The two page-0 occurrences are distinct horizontal spans on one line.
    assert!(
        (page0[0].rect.0 - page0[1].rect.0).abs() > 1.0,
        "page-0 hits should not overlap exactly: {hits:?}"
    );
    for hit in &hits {
        assert!(
            hit.rect.2 > 0.0 && hit.rect.3 > 0.0,
            "degenerate rect: {hit:?}"
        );
    }
}

#[test]
fn search_empty_query_is_invalid_input() {
    let pdf = sample_pdf(&["Alpha bravo"]);
    let err = search(&pdf, "", true).expect_err("empty query must error");
    assert!(
        matches!(err, PdfCoreError::InvalidInput(_)),
        "expected InvalidInput, got: {err:?}"
    );
}

#[test]
fn search_no_matches_returns_empty_vec() {
    let pdf = sample_pdf(&["Alpha bravo"]);
    let hits = search(&pdf, "zulu", false).expect("search");
    assert!(hits.is_empty(), "no matches expected: {hits:?}");
}

#[test]
fn extract_text_pages_empty_page_yields_empty_string() {
    let pdf = sample_pdf(&["", "Charlie delta"]);
    let pages = extract_text_pages(&pdf).expect("extract_text_pages");
    assert_eq!(pages.len(), 2);
    assert!(
        pages[0].trim().is_empty(),
        "empty page should extract to empty text: {:?}",
        pages[0]
    );
    assert!(pages[1].contains("Charlie delta"));
}

// --- Rotated-page regressions ---
//
// pdfium-render's `PdfPageText::all()` clips to the rotation-transformed page
// box while `FPDFText_GetBoundedText` expects unrotated user-space coords, so
// `/Rotate 90/270` pages silently lost text. extract.rs now iterates the
// text-page characters instead, which is rotation-independent.

#[test]
fn extract_text_survives_all_page_rotations() {
    let pdf = sample_pdf(&["Rotate payload"]);
    for degrees in [90, 180, 270] {
        let rotated = rotate_pages(&pdf, None, degrees).expect("rotate_pages");
        let text = extract_text(&rotated).expect("extract_text");
        assert!(
            text.contains("Rotate payload"),
            "text lost on /Rotate {degrees} page: {text:?}"
        );
    }
}

#[test]
fn extract_text_hand_built_rotate_90_fixture() {
    // Minimal hand-built PDF whose page dict carries /Rotate 90, with text at
    // y=720 on a 612x792 page — exactly the band `all()` used to clip away.
    let pdf = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/pdf-core/extract-rot90.pdf"
    ))
    .expect("read extract-rot90.pdf fixture");
    let text = extract_text(&pdf).expect("extract_text");
    assert!(
        text.contains("Rotate payload"),
        "rotated fixture text lost: {text:?}"
    );
}

#[test]
fn search_finds_hits_on_rotated_pages_in_unrotated_space() {
    let pdf = sample_pdf(&["Rotate payload"]);
    let baseline = search(&pdf, "payload", true).expect("search unrotated");
    assert_eq!(baseline.len(), 1, "baseline hit expected: {baseline:?}");

    for degrees in [90, 180, 270] {
        let rotated = rotate_pages(&pdf, None, degrees).expect("rotate_pages");
        let hits = search(&rotated, "payload", true).expect("search rotated");
        assert_eq!(
            hits.len(),
            1,
            "hit must be found on /Rotate {degrees} page: {hits:?}"
        );
        assert_eq!(hits[0].page_index, 0);
        // Documented behavior: hit rects are in UNROTATED page user space —
        // identical to the unrotated page's rect regardless of /Rotate.
        let (bx, by, bw, bh) = baseline[0].rect;
        let (x, y, w, h) = hits[0].rect;
        assert!(
            (x - bx).abs() < 0.01
                && (y - by).abs() < 0.01
                && (w - bw).abs() < 0.01
                && (h - bh).abs() < 0.01,
            "/Rotate {degrees} rect {:?} diverged from unrotated-space rect {:?}",
            hits[0].rect,
            baseline[0].rect
        );
    }
}

#[test]
fn extract_text_invalid_pdf_errors() {
    let err = extract_text(b"not a pdf at all").expect_err("garbage must error");
    assert!(
        matches!(err, PdfCoreError::Pdfium(_)),
        "expected Pdfium error, got: {err:?}"
    );
}

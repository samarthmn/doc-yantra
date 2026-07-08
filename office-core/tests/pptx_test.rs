//! PPTX conversion integration tests (agent task 5).

use office_core::layout::{Item, Page};
use office_core::pptx::{PptxDoc, render_pptx};
use office_core::{ConvertOptions, OfficeFormat, convert, profile};

fn opts() -> ConvertOptions {
    ConvertOptions::default()
}

fn page_text(page: &Page) -> String {
    let mut s = String::new();
    for pos in &page.items {
        if let Item::Glyphs(run) = &pos.item {
            for g in &run.glyphs {
                s.push(g.ch);
            }
        }
    }
    s
}

#[test]
fn converts_every_mock_pptx_to_valid_pdf() {
    let dir = std::path::Path::new("../mock-files/pptx");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut checked = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pptx") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let out = convert(&bytes, OfficeFormat::Pptx, &opts())
            .unwrap_or_else(|e| panic!("convert {}: {e}", path.display()));
        assert!(out.pdf.starts_with(b"%PDF-"));
        assert!(out.page_count >= 1, "{} had 0 pages", path.display());
        checked += 1;
    }
    assert!(checked > 0);
}

#[test]
fn slide_text_is_rendered() {
    let bytes = std::fs::read("../mock-files/pptx/file1.pptx").unwrap();
    let doc = PptxDoc::open(&bytes, &opts()).expect("open");
    let (laid, _) = render_pptx(&doc);
    assert_eq!(laid.pages.len(), 1);
    let text = page_text(&laid.pages[0]);
    assert!(text.contains("Dummy Document 1"), "title missing: {text:?}");
    assert!(text.contains("first dummy"), "body missing: {text:?}");
}

#[test]
fn pptx_conversion_is_deterministic() {
    let bytes = std::fs::read("../mock-files/pptx/file1.pptx").unwrap();
    let a = convert(&bytes, OfficeFormat::Pptx, &opts()).unwrap();
    let b = convert(&bytes, OfficeFormat::Pptx, &opts()).unwrap();
    assert_eq!(a.pdf, b.pdf, "pptx output must be deterministic");
}

#[test]
fn pptx_profile_reports_slides() {
    let bytes = std::fs::read("../mock-files/pptx/file1.pptx").unwrap();
    let prof = profile(&bytes, OfficeFormat::Pptx).expect("profile");
    assert_eq!(prof.format, "pptx");
    assert!(prof.supported);
    assert!(prof.stats.slides >= 1);
}

/// Router contract for PPTX: `profile().supported == convert().is_ok()` on every
/// real mock file. `convert_pptx` profiles first and fails closed, so the
/// unsupported→unsupported direction is structural; the risk this guards is a
/// profile *false positive* — a file profile passes that then fails in
/// `PptxDoc::open`/render/emit. DocKosha's engine-first router depends on this.
#[test]
fn pptx_profile_agrees_with_convert_on_mock_corpus() {
    let dir = std::path::Path::new("../mock-files/pptx");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut checked = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pptx") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let prof = profile(&bytes, OfficeFormat::Pptx).expect("profile");
        let conv = convert(&bytes, OfficeFormat::Pptx, &opts());
        assert_eq!(
            prof.supported,
            conv.is_ok(),
            "profile/convert verdict mismatch for {}: reasons={:?} err={:?}",
            path.display(),
            prof.reasons,
            conv.err(),
        );
        checked += 1;
    }
    assert!(checked > 0, "no mock pptx files found");
}

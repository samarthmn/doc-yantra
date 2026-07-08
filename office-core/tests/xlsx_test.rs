//! XLSX conversion integration tests (agent task 6).

use office_core::layout::{Item, Page};
use office_core::xlsx::{XlsxDoc, render_xlsx};
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
fn converts_every_mock_xlsx_to_valid_pdf() {
    for sub in ["xlsx", "xlsm"] {
        let dir = std::path::Path::new("../mock-files").join(sub);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(sub) {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let out = convert(&bytes, OfficeFormat::Xlsx, &opts())
                .unwrap_or_else(|e| panic!("convert {}: {e}", path.display()));
            assert!(out.pdf.starts_with(b"%PDF-"));
            assert!(out.page_count >= 1, "{} had 0 pages", path.display());
        }
    }
}

#[test]
fn sheet_cells_render_with_values() {
    let bytes = std::fs::read("../mock-files/xlsx/file1.xlsx").unwrap();
    let doc = XlsxDoc::open(&bytes, &opts()).expect("open");
    let (laid, _) = render_xlsx(&doc);
    assert!(!laid.pages.is_empty());
    let text: String = laid.pages.iter().map(page_text).collect();
    assert!(text.contains("Column A"), "header missing: {text:?}");
    assert!(text.contains("one"), "shared string missing: {text:?}");
    // Booleans render as TRUE/FALSE.
    assert!(text.contains("TRUE") || text.contains("FALSE"));
}

#[test]
fn xlsx_conversion_is_deterministic() {
    let bytes = std::fs::read("../mock-files/xlsx/file1.xlsx").unwrap();
    let a = convert(&bytes, OfficeFormat::Xlsx, &opts()).unwrap();
    let b = convert(&bytes, OfficeFormat::Xlsx, &opts()).unwrap();
    assert_eq!(a.pdf, b.pdf, "xlsx output must be deterministic");
}

#[test]
fn xlsx_profile_reports_sheets() {
    let bytes = std::fs::read("../mock-files/xlsx/file1.xlsx").unwrap();
    let prof = profile(&bytes, OfficeFormat::Xlsx).expect("profile");
    assert_eq!(prof.format, "xlsx");
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    assert!(prof.stats.sheets >= 1);
}

//! PPTX conversion integration tests (agent task 5).

use office_core::layout::{Item, Page};
use office_core::pptx::{PptxDoc, render_pptx};
use office_core::testkit::zip_package;
use office_core::{ConvertOptions, OfficeFormat, UnsupportedCode, convert, profile};

fn opts() -> ConvertOptions {
    ConvertOptions::default()
}

/// Builds a minimal one-slide `.pptx` whose slide's shape tree is `shapes`.
fn minimal_pptx(shapes: &str) -> Vec<u8> {
    let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;
    let pres = r#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst></p:presentation>"#;
    let pres_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#;
    let slide = format!(
        r#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:sld>"#
    );
    zip_package(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("ppt/presentation.xml", pres.as_bytes()),
        ("ppt/_rels/presentation.xml.rels", pres_rels.as_bytes()),
        ("ppt/slides/slide1.xml", slide.as_bytes()),
    ])
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

/// Y-B1: non-rectangular geometry must fail closed. v1 draws shape fills against
/// the bounding box, so an arrow/star/callout would render as a rectangle. A
/// plain rect passes; every other preset and custGeom is unsupported, and since
/// the router trusts profile(), convert() must fail closed too.
#[test]
fn pptx_non_rect_geometry_fails_closed() {
    let rect = minimal_pptx(r#"<p:sp><p:spPr><a:prstGeom prst="rect"/></p:spPr></p:sp>"#);
    assert!(
        profile(&rect, OfficeFormat::Pptx).unwrap().supported,
        "a rect-only deck must be supported"
    );

    for geom in [
        r#"<a:prstGeom prst="star5"/>"#,
        r#"<a:prstGeom prst="rightArrow"/>"#,
        "<a:custGeom/>",
    ] {
        let deck = minimal_pptx(&format!("<p:sp><p:spPr>{geom}</p:spPr></p:sp>"));
        let prof = profile(&deck, OfficeFormat::Pptx).unwrap();
        assert!(!prof.supported, "{geom} should be unsupported");
        assert!(
            prof.reasons
                .iter()
                .any(|r| r.code == UnsupportedCode::ShapeGeometry),
            "{geom} should report ShapeGeometry, got {:?}",
            prof.reasons
        );
        assert!(
            convert(&deck, OfficeFormat::Pptx, &opts()).is_err(),
            "{geom}: convert must fail closed to match profile"
        );
    }
}

//! Integration coverage for runtime Unicode font registration and CID embedding.

use std::path::PathBuf;

use lopdf::Document;
use pdf_core::assembly::merge;
use pdf_core::embed_font::{register_font, validate_text};
use pdf_core::generate::{DocOptions, csv_to_pdf, markdown_to_pdf};
use pdf_core::meta::{page_count, validate};
use pdf_core::testkit::sample_pdf;
use pdf_core::watermark::{WatermarkSpec, apply_watermark};
use serial_test::serial;
use ttf_parser::Tag;

#[cfg(feature = "pdfium")]
use pdf_core::extract::extract_text;
#[cfg(feature = "pdfium")]
use pdf_core::raster::{RenderOptions, render_page};

const CYRILLIC: &str = "Конфиденциально";
const GREEK: &str = "Εμπιστευτικό";

fn liberation_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../office-core/fonts/liberation")
        .join(file)
}

fn read_liberation(file: &str) -> Vec<u8> {
    let path = liberation_path(file);
    std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn register_liberation_faces() {
    for file in [
        "LiberationSans-Regular.ttf",
        "LiberationSans-Bold.ttf",
        "LiberationMono-Regular.ttf",
    ] {
        register_font(&read_liberation(file))
            .unwrap_or_else(|err| panic!("register {file}: {err}"));
    }
}

fn with_fs_type(fs_type: u16) -> Vec<u8> {
    let mut bytes = read_liberation("LiberationSans-Regular.ttf");
    let table_offset = {
        let face = ttf_parser::Face::parse(&bytes, 0).expect("parse Liberation Sans");
        let table = face
            .raw_face()
            .table(Tag::from_bytes(b"OS/2"))
            .expect("Liberation Sans has OS/2");
        assert!(table.len() >= 10, "OS/2 table contains fsType");
        table.as_ptr() as usize - bytes.as_ptr() as usize
    };
    bytes[table_offset + 8..table_offset + 10].copy_from_slice(&fs_type.to_be_bytes());
    bytes
}

fn unicode_watermark(line: &str) -> WatermarkSpec {
    WatermarkSpec {
        rotation_deg: 0.0,
        lines: vec![line.to_string()],
        font_size_pt: 28.0,
        opacity: 1.0,
        ..WatermarkSpec::default()
    }
}

fn assert_valid_page_count(bytes: &[u8], expected: u32) {
    let info = validate(bytes).expect("output must validate");
    assert_eq!(info.page_count, expected, "validated page count");
    assert_eq!(page_count(bytes).expect("page_count"), expected);
}

fn embedded_font_program_count(bytes: &[u8]) -> usize {
    let doc = Document::load_mem(bytes).expect("load generated PDF");
    doc.objects
        .values()
        .filter(|object| {
            object
                .as_dict()
                .is_ok_and(|dict| dict.has(b"FontFile2") || dict.has(b"FontFile3"))
        })
        .count()
}

#[test]
#[serial(unicode_fonts)]
fn register_font_rejects_unparseable_bytes() {
    let err = register_font(b"not an sfnt").expect_err("invalid font must fail");
    assert!(
        err.to_string().starts_with("invalid input:"),
        "unexpected error: {err}"
    );
}

#[test]
#[serial(unicode_fonts)]
fn register_font_accepts_liberation_sfnt() {
    register_font(&read_liberation("LiberationSans-Regular.ttf"))
        .expect("valid Liberation face registers");
}

#[test]
#[serial(unicode_fonts)]
fn register_font_rejects_restricted_embedding_fs_type() {
    let err = register_font(&with_fs_type(0x0002)).expect_err("restricted face must fail");
    assert!(
        err.to_string().starts_with("unsupported:font-license:"),
        "unexpected error: {err}"
    );
}

#[test]
#[serial(unicode_fonts)]
fn validate_text_rejects_arabic_as_complex_script() {
    let err = validate_text("مرحبا").expect_err("Arabic needs shaping");
    assert!(
        err.to_string().starts_with("unsupported:script:"),
        "unexpected error: {err}"
    );
}

#[test]
#[serial(unicode_fonts)]
fn validate_text_rejects_hebrew_as_rtl() {
    let err = validate_text("שלום").expect_err("RTL text must fail");
    assert!(
        err.to_string().starts_with("unsupported:script:"),
        "unexpected error: {err}"
    );
}

#[test]
#[serial(unicode_fonts)]
fn cjk_without_covering_registered_face_fails_closed() {
    register_liberation_faces();
    let source = sample_pdf(&["source"]);
    let err = apply_watermark(&source, &unicode_watermark("你好"))
        .expect_err("Liberation does not cover CJK");
    assert!(
        err.to_string().starts_with("unsupported:glyph-coverage:"),
        "unexpected error: {err}"
    );
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn cyrillic_watermark_validates_and_extracts_exact_text() {
    register_liberation_faces();
    let source = sample_pdf(&["source page"]);
    let output = apply_watermark(&source, &unicode_watermark(CYRILLIC))
        .expect("Cyrillic watermark succeeds");

    assert_valid_page_count(&output, 1);
    let extracted = extract_text(&output).expect("extract watermarked PDF");
    assert!(
        extracted.contains(CYRILLIC),
        "missing exact Cyrillic text: {extracted:?}"
    );
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn unicode_csv_validates_and_extracts_greek_and_cyrillic() {
    register_liberation_faces();
    let csv = format!("Language,Label\nGreek,{GREEK}\nRussian,{CYRILLIC}\n");
    let output = csv_to_pdf(csv.as_bytes(), &DocOptions::default()).expect("render Unicode CSV");

    assert_valid_page_count(&output, 1);
    let extracted = extract_text(&output).expect("extract CSV PDF");
    assert!(extracted.contains(GREEK), "missing Greek: {extracted:?}");
    assert!(
        extracted.contains(CYRILLIC),
        "missing Cyrillic: {extracted:?}"
    );
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn unicode_markdown_validates_and_extracts_greek_and_cyrillic() {
    register_liberation_faces();
    let markdown = format!("# Unicode\n\n{GREEK}\n\n{CYRILLIC}\n");
    let output =
        markdown_to_pdf(&markdown, &DocOptions::default()).expect("render Unicode Markdown");

    assert_valid_page_count(&output, 1);
    let extracted = extract_text(&output).expect("extract Markdown PDF");
    assert!(extracted.contains(GREEK), "missing Greek: {extracted:?}");
    assert!(
        extracted.contains(CYRILLIC),
        "missing Cyrillic: {extracted:?}"
    );
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn cyrillic_watermark_rasterizes_non_blank_center_region() {
    register_liberation_faces();
    let source = sample_pdf(&[""]);
    let output = apply_watermark(&source, &unicode_watermark(CYRILLIC))
        .expect("Cyrillic watermark succeeds");
    let png = render_page(&output, 0, &RenderOptions::default()).expect("rasterize watermark");
    let gray = image::load_from_memory(&png)
        .expect("decode raster")
        .to_luma8();

    let center_x = gray.width() / 2;
    let center_y = gray.height() / 2;
    let x_start = center_x.saturating_sub(180);
    let x_end = (center_x + 180).min(gray.width());
    let y_start = center_y.saturating_sub(50);
    let y_end = (center_y + 50).min(gray.height());
    let non_blank = (y_start..y_end)
        .flat_map(|y| (x_start..x_end).map(move |x| (x, y)))
        .filter(|&(x, y)| gray.get_pixel(x, y)[0] < 245)
        .count();

    assert!(
        non_blank > 20,
        "watermark region should contain rendered glyph pixels, found {non_blank}"
    );
}

#[test]
#[serial(unicode_fonts)]
fn ascii_watermark_contains_no_embedded_font_program() {
    register_liberation_faces();
    let source = sample_pdf(&["source"]);
    let output = apply_watermark(&source, &unicode_watermark("CONFIDENTIAL"))
        .expect("ASCII watermark succeeds");

    assert_eq!(embedded_font_program_count(&output), 0);
}

#[test]
#[serial(unicode_fonts)]
fn ascii_csv_contains_no_embedded_font_program() {
    register_liberation_faces();
    let output = csv_to_pdf(b"Name,Value\nAlpha,Bravo\n", &DocOptions::default())
        .expect("ASCII CSV succeeds");

    assert_eq!(embedded_font_program_count(&output), 0);
}

#[test]
#[serial(unicode_fonts)]
fn merged_document_watermark_contains_one_embedded_font_graph() {
    register_liberation_faces();
    let first = sample_pdf(&["first"]);
    let second = sample_pdf(&["second"]);
    let merged = merge(&[first.as_slice(), second.as_slice()]).expect("merge source PDFs");
    let output =
        apply_watermark(&merged, &unicode_watermark(CYRILLIC)).expect("watermark merged PDF");

    assert_valid_page_count(&output, 2);
    assert_eq!(
        embedded_font_program_count(&output),
        1,
        "one shared embedded font graph expected"
    );
}

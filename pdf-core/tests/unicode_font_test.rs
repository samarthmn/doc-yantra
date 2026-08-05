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

fn packaged_font_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../packages/doc-yantra-fonts/fonts")
        .join(file)
}

fn read_liberation(file: &str) -> Vec<u8> {
    let path = packaged_font_path(file);
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

fn register_packaged_font(file: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../packages/doc-yantra-fonts/fonts")
        .join(file);
    register_font(
        &std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("register {file}: {error}"));
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
fn validate_text_accepts_arabic_for_coverage_and_shaping() {
    validate_text("مرحبا").expect("script gate is removed");
}

#[test]
#[serial(unicode_fonts)]
fn validate_text_accepts_hebrew_for_bidi_shaping() {
    validate_text("שלום").expect("RTL gate is removed");
}

#[test]
#[serial(unicode_fonts)]
fn hebrew_csv_shapes_with_liberation() {
    register_liberation_faces();
    let output = csv_to_pdf("label,value\nעברית,שלום".as_bytes(), &DocOptions::default())
        .expect("Liberation Sans covers and shapes Hebrew");
    assert_valid_page_count(&output, 1);
    #[cfg(feature = "pdfium")]
    {
        let extracted = extract_text(&output).expect("extract Hebrew CSV");
        assert!(
            extracted.contains("שלום"),
            "expected logical-order Hebrew, got {extracted:?}"
        );
    }
}

#[cfg(feature = "pdfium")]
fn assert_markdown_line_round_trips(line: &str, font: &str) {
    register_liberation_faces();
    register_packaged_font(font);
    let output = markdown_to_pdf(line, &DocOptions::default())
        .unwrap_or_else(|error| panic!("render {line:?}: {error}"));
    let extracted = extract_text(&output).expect("extract shaped Markdown");
    assert!(
        extracted.contains(line),
        "expected logical-order {line:?}, got {extracted:?}"
    );
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn long_arabic_sentence_round_trips_reused_glyphs() {
    assert_markdown_line_round_trips(
        "هذا نص تجريبي باللغة العربية. يحتوي على أرقام عربية ٠١٢٣ وحروف متصلة.",
        "NotoSansArabic-Variable.ttf",
    );
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn long_devanagari_sentence_round_trips_reused_glyphs() {
    assert_markdown_line_round_trips(
        "यह देवनागरी में एक परीक्षण वाक्य है। संयुक्ताक्षर: क्ष त्र ज्ञ।",
        "NotoSansDevanagari-Variable.ttf",
    );
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn long_thai_sentence_round_trips_reused_glyphs() {
    assert_markdown_line_round_trips(
        "นี่คือข้อความทดสอบภาษาไทยที่ไม่มีการเว้นวรรคระหว่างคำและมีวรรณยุกต์ซ้อนกัน",
        "NotoSansThai-Variable.ttf",
    );
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn arabic_watermark_extracts_in_logical_order() {
    let line = "هذا نص تجريبي باللغة العربية";
    register_packaged_font("NotoSansArabic-Variable.ttf");
    let source = sample_pdf(&["source"]);
    let output =
        apply_watermark(&source, &unicode_watermark(line)).expect("render shaped Arabic watermark");
    let extracted = extract_text(&output).expect("extract Arabic watermark");
    assert!(
        extracted.contains(line),
        "expected logical-order Arabic, got {extracted:?}"
    );
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn mixed_ltr_rtl_line_round_trips_logical_order() {
    register_liberation_faces();
    register_packaged_font("NotoSansArabic-Variable.ttf");
    for line in ["Total: 123 ريال", "ريال 123 Total"] {
        let output = markdown_to_pdf(line, &DocOptions::default()).expect("render mixed bidi line");
        let extracted = extract_text(&output).expect("extract mixed bidi line");
        assert!(
            extracted.contains(line),
            "expected logical-order mixed line {line:?}, got {extracted:?}"
        );
    }
}

#[cfg(feature = "pdfium")]
#[test]
#[serial(unicode_fonts)]
fn emoji_modifier_and_zwj_line_round_trips_original_sequences() {
    let line = "Thumbs up 👍🏽 and a family 👨‍👩‍👧‍👦 plus a heart ❤️.";
    register_liberation_faces();
    register_packaged_font("NotoEmoji-Variable.ttf");
    let output = markdown_to_pdf(line, &DocOptions::default())
        .unwrap_or_else(|error| panic!("render emoji sequences: {error}"));
    let extracted = extract_text(&output).expect("extract emoji sequences");
    assert!(
        extracted.contains(line),
        "expected original emoji sequences, got {extracted:?}"
    );
}

#[test]
#[serial(unicode_fonts)]
fn absent_pictograph_names_real_codepoint_not_replacement_character() {
    register_packaged_font("NotoEmoji-Variable.ttf");
    let err = markdown_to_pdf("🫩", &DocOptions::default())
        .expect_err("Noto Emoji does not cover U+1FAE9");
    let message = err.to_string();
    assert!(
        message.starts_with("unsupported:glyph-coverage:"),
        "unexpected error: {message}"
    );
    assert!(message.contains("U+1FAE9"), "unexpected error: {message}");
    assert!(!message.contains("U+FFFD"), "unexpected error: {message}");
}

#[test]
#[serial(unicode_fonts)]
fn astral_emoji_watermark_uses_variable_noto_face() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../packages/doc-yantra-fonts/fonts/NotoEmoji-Variable.ttf");
    register_font(&std::fs::read(path).expect("read Noto Emoji")).expect("register Noto Emoji");
    let source = sample_pdf(&["source"]);
    let output = apply_watermark(&source, &unicode_watermark("🔒"))
        .expect("astral emoji shapes without U+FFFD folding");
    assert_valid_page_count(&output, 1);
    #[cfg(feature = "pdfium")]
    assert!(
        extract_text(&output)
            .expect("extract astral emoji watermark")
            .contains('🔒')
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
fn markdown_mixed_winansi_and_cyrillic_extracts_both_runs() {
    register_liberation_faces();
    let output = markdown_to_pdf("café Москва", &DocOptions::default())
        .expect("render mixed WinAnsi and Cyrillic Markdown");
    let extracted = extract_text(&output).expect("extract mixed-script Markdown");

    assert!(
        extracted.contains("café"),
        "missing WinAnsi run: {extracted:?}"
    );
    assert!(
        extracted.contains("Москва"),
        "missing Cyrillic run: {extracted:?}"
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

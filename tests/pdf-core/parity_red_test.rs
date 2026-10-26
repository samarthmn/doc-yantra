//! Parity fixtures that describe required behavior, including unresolved ignored gaps.

#![cfg(feature = "pdfium")]

use std::path::PathBuf;

use lopdf::{Document, Object};
use pdf_core::embed_font::register_font;
use pdf_core::error::PdfCoreError;
use pdf_core::extract::extract_text;
use pdf_core::generate::{DocOptions, csv_to_pdf, markdown_to_pdf};
use pdf_core::redact_pure::redact_pure;
use pdf_core::redact_types::RedactArea;
use pdf_core::testkit::sample_pdf;
use serial_test::serial;

fn area(page_index: u32) -> RedactArea {
    RedactArea {
        page_index,
        left: 10.0,
        top: 10.0,
        width: 20.0,
        height: 20.0,
    }
}

fn register_liberation_faces() {
    let font_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../packages/doc-yantra-fonts/fonts");
    for file in [
        "LiberationSans-Regular.ttf",
        "LiberationSans-Bold.ttf",
        "LiberationMono-Regular.ttf",
    ] {
        let path = font_dir.join(file);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        register_font(&bytes).unwrap_or_else(|error| panic!("register {file}: {error}"));
    }
}

fn markdown_text(markdown: &str) -> String {
    let pdf = markdown_to_pdf(markdown, &DocOptions::default()).expect("render Markdown fixture");
    extract_text(&pdf).expect("extract Markdown fixture")
}

fn dereference<'a>(doc: &'a Document, object: &'a Object) -> &'a Object {
    doc.dereference(object)
        .map(|(_, resolved)| resolved)
        .expect("resolve PDF object")
}

fn has_uri_link_annotation(pdf: &[u8], expected_uri: &[u8]) -> bool {
    let doc = Document::load_mem(pdf).expect("load Markdown PDF");
    doc.get_pages().values().any(|page_id| {
        let page = doc
            .get_object(*page_id)
            .and_then(Object::as_dict)
            .expect("page dictionary");
        let Ok(annots) = page.get(b"Annots") else {
            return false;
        };
        let Ok(annots) = dereference(&doc, annots).as_array() else {
            return false;
        };
        annots.iter().any(|annot| {
            let Ok(dict) = dereference(&doc, annot).as_dict() else {
                return false;
            };
            if !matches!(dict.get(b"Subtype"), Ok(Object::Name(name)) if name == b"Link") {
                return false;
            }
            let Ok(action) = dict.get(b"A") else {
                return false;
            };
            let Ok(action) = dereference(&doc, action).as_dict() else {
                return false;
            };
            matches!(action.get(b"S"), Ok(Object::Name(name)) if name == b"URI")
                && matches!(action.get(b"URI"), Ok(Object::String(uri, _)) if uri == expected_uri)
        })
    })
}

#[test]
fn out_of_range_redaction_page_must_be_a_typed_error() {
    let input = sample_pdf(&["one page"]);
    let error = redact_pure(&input, &[area(5)]).expect_err("out-of-range page must fail");

    match error {
        PdfCoreError::InvalidInput(message) => assert_eq!(
            message,
            "redaction page index 5 is out of range (document has 1 page)"
        ),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn partially_out_of_range_redaction_must_be_a_typed_error() {
    let input = sample_pdf(&["one page"]);
    let error = redact_pure(&input, &[area(0), area(7)]).expect_err("mixed page indices must fail");

    match error {
        PdfCoreError::InvalidInput(message) => assert_eq!(
            message,
            "redaction page index 7 is out of range (document has 1 page)"
        ),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn non_empty_redaction_success_must_change_bytes() {
    let input = sample_pdf(&["one page"]);
    let no_text_area = RedactArea {
        page_index: 0,
        left: 90.0,
        top: 90.0,
        width: 5.0,
        height: 5.0,
    };
    let output = redact_pure(&input, &[no_text_area]).expect("valid redaction must succeed");

    assert_ne!(
        output.pdf, input,
        "non-empty redaction returned unchanged bytes"
    );
}

#[test]
#[serial(unicode_fonts)]
fn csv_latin1_must_extract_exact_accents() {
    register_liberation_faces();
    let pdf = csv_to_pdf("café,Zürich\n1,2".as_bytes(), &DocOptions::default())
        .expect("render Latin-1 CSV");
    let text = extract_text(&pdf).expect("extract Latin-1 CSV");

    assert!(
        text.contains("café") && text.contains("Zürich"),
        "expected café and Zürich, got {text:?}"
    );
}

#[test]
#[serial(unicode_fonts)]
fn markdown_latin1_must_extract_exact_accents() {
    register_liberation_faces();
    let text = markdown_text("# Café menu\n\nZürich café");

    assert!(
        text.contains("Café") && text.contains("Zürich") && text.contains("café"),
        "expected Café, Zürich, and café, got {text:?}"
    );
}

#[test]
fn markdown_link_must_emit_uri_annotation() {
    let uri = b"https://example.com/x";
    let pdf = markdown_to_pdf("[DocKosha](https://example.com/x)", &DocOptions::default())
        .expect("render Markdown link");

    assert!(
        has_uri_link_annotation(&pdf, uri),
        "expected Link annotation with URI https://example.com/x"
    );
}

#[test]
fn markdown_image_must_emit_secure_placeholder() {
    let text = markdown_text("before ![diagram](x.png) after");

    assert!(
        text.contains("[Image omitted for secure conversion]"),
        "expected secure image placeholder, got {text:?}"
    );
}

#[test]
fn markdown_strikethrough_must_not_render_literal_delimiters() {
    let text = markdown_text("a ~~gone~~ b");

    assert!(
        !text.contains("~~"),
        "literal strikethrough delimiters remain: {text:?}"
    );
}

#[test]
fn markdown_task_lists_must_not_render_literal_brackets() {
    let text = markdown_text("- [x] done\n- [ ] open");

    assert!(
        !text.contains("[x]") && !text.contains("[ ]"),
        "literal task-list markers remain: {text:?}"
    );
}

#[test]
fn markdown_footnotes_must_not_render_literal_reference() {
    let text = markdown_text("body[^1]\n\n[^1]: note");

    assert!(
        !text.contains("[^1]") && text.contains("note") && text.contains('1'),
        "expected marker 1 and definition text without literal syntax: {text:?}"
    );
}

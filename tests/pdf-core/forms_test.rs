//! Integration tests for `pdf_core::forms` — list_fields, fill_fields, flatten.

#![cfg(feature = "pdfium")]

use std::sync::{Mutex, MutexGuard, PoisonError};

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use pdf_core::error::PdfCoreError;
use pdf_core::forms::{FieldKind, fill_fields, flatten, list_fields};
use pdf_core::pdfium::lock;
use pdf_core::testkit::sample_pdf;

/// pdfium is not safe to drive from multiple threads at once (pdfium-render
/// 0.9.2's `thread_safe` feature only adds Send/Sync markers, no locking).
/// Some tests below call pdfium directly for verification, so serialize every
/// test in this binary behind one lock.
static PDFIUM_TEST_LOCK: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    PDFIUM_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Builds a minimal AcroForm PDF with one text field named "fullname"
/// (value "initial") and, optionally, a checkbox field named "agree".
///
/// The text widget carries an explicit /AP /N appearance stream drawing the
/// value so pdfium's FPDFPage_Flatten has something to paint into the page.
fn form_pdf(include_checkbox: bool) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    // Static page content so the page is not empty.
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("Form page")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("encode content"),
    ));

    // Appearance stream for the text widget: draws the initial value inside
    // the widget rect (Rect is 200x30, so BBox [0 0 200 30]).
    let ap_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 200.into(), 30.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "Helv" => font_id },
            },
        },
        b"/Tx BMC q BT /Helv 12 Tf 0 g 2 8 Td (initial) Tj ET Q EMC".to_vec(),
    ));

    let text_field_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "T" => Object::string_literal("fullname"),
        "V" => Object::string_literal("initial"),
        "DA" => Object::string_literal("/Helv 12 Tf 0 g"),
        "F" => 4,
        "Rect" => vec![100.into(), 600.into(), 300.into(), 630.into()],
        "AP" => dictionary! { "N" => ap_id },
    });

    let mut annots: Vec<Object> = vec![text_field_id.into()];
    let mut fields: Vec<Object> = vec![text_field_id.into()];

    if include_checkbox {
        let checkbox_id = doc.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Widget",
            "FT" => "Btn",
            "T" => Object::string_literal("agree"),
            "V" => "Off",
            "AS" => "Off",
            "F" => 4,
            "Rect" => vec![100.into(), 550.into(), 120.into(), 570.into()],
        });
        annots.push(checkbox_id.into());
        fields.push(checkbox_id.into());
    }

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
        "Annots" => annots,
    });

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );

    let acroform_id = doc.add_object(dictionary! {
        "Fields" => fields,
        "NeedAppearances" => true,
        "DA" => Object::string_literal("/Helv 12 Tf 0 g"),
        "DR" => dictionary! {
            "Font" => dictionary! { "Helv" => font_id },
        },
    });

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "AcroForm" => acroform_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("serialize form pdf");
    buf
}

#[test]
fn list_fields_on_formless_pdf_is_empty() {
    let _guard = serial();
    let bytes = sample_pdf(&["No forms here"]);
    let fields = list_fields(&bytes).expect("list_fields on formless pdf");
    assert!(fields.is_empty(), "expected no fields, got {fields:?}");
}

#[test]
fn list_fields_reads_text_field_from_fixture() {
    let _guard = serial();
    let bytes = form_pdf(false);
    let fields = list_fields(&bytes).expect("list_fields on fixture");
    assert_eq!(fields.len(), 1, "expected exactly one field: {fields:?}");
    assert_eq!(fields[0].name, "fullname");
    assert_eq!(fields[0].kind, FieldKind::Text);
    assert_eq!(fields[0].value.as_deref(), Some("initial"));
}

#[test]
fn list_fields_reports_checkbox_kind() {
    let _guard = serial();
    let bytes = form_pdf(true);
    let mut fields = list_fields(&bytes).expect("list_fields on mixed fixture");
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(fields.len(), 2, "expected two fields: {fields:?}");
    assert_eq!(fields[0].name, "agree");
    assert_eq!(fields[0].kind, FieldKind::Checkbox);
    assert_eq!(fields[0].value.as_deref(), Some("false"));
    assert_eq!(fields[1].name, "fullname");
    assert_eq!(fields[1].kind, FieldKind::Text);
}

#[test]
fn fill_fields_roundtrips_through_list_fields() {
    let _guard = serial();
    let bytes = form_pdf(false);
    let filled = fill_fields(
        &bytes,
        &[("fullname".to_string(), "Jane Q. Public".to_string())],
    )
    .expect("fill_fields should succeed on a text field");

    // The output must be a loadable PDF whose field now carries the new value.
    let fields = list_fields(&filled).expect("list_fields on filled output");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "fullname");
    assert_eq!(fields[0].kind, FieldKind::Text);
    assert_eq!(fields[0].value.as_deref(), Some("Jane Q. Public"));

    // And it must still be structurally valid per lopdf with one page.
    let reloaded = Document::load_mem(&filled).expect("lopdf reload of filled output");
    assert_eq!(reloaded.get_pages().len(), 1);
}

#[test]
fn fill_fields_unknown_name_is_invalid_input() {
    let _guard = serial();
    let bytes = form_pdf(false);
    let err = fill_fields(
        &bytes,
        &[("no_such_field".to_string(), "value".to_string())],
    )
    .expect_err("unknown field name must error");
    match err {
        PdfCoreError::InvalidInput(msg) => {
            assert!(
                msg.contains("no_such_field"),
                "error should name the missing field: {msg}"
            );
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn fill_fields_on_formless_pdf_is_invalid_input() {
    let _guard = serial();
    let bytes = sample_pdf(&["No forms here"]);
    let err = fill_fields(&bytes, &[("fullname".to_string(), "x".to_string())])
        .expect_err("filling a formless pdf must error");
    match err {
        PdfCoreError::InvalidInput(msg) => {
            assert!(
                msg.contains("fullname"),
                "error should name the missing field: {msg}"
            );
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn fill_fields_non_text_target_is_unsupported() {
    let _guard = serial();
    let bytes = form_pdf(true);
    let err = fill_fields(&bytes, &[("agree".to_string(), "true".to_string())])
        .expect_err("filling a checkbox must error");
    match err {
        PdfCoreError::Unsupported(msg) => {
            assert!(
                msg.contains("agree"),
                "error should name the non-text field: {msg}"
            );
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn flatten_removes_widgets_and_keeps_value_text() {
    let _guard = serial();
    let bytes = form_pdf(false);
    let flat = flatten(&bytes).expect("flatten fixture");

    // Structurally valid, page count preserved.
    let reloaded = Document::load_mem(&flat).expect("lopdf reload of flattened output");
    assert_eq!(reloaded.get_pages().len(), 1);

    // The widget is gone: no fields remain.
    let fields = list_fields(&flat).expect("list_fields on flattened output");
    assert!(
        fields.is_empty(),
        "expected no fields after flatten: {fields:?}"
    );

    // The appearance-stream text was painted into the page content.
    let pdfium = lock().expect("bind pdfium");
    let doc = pdfium
        .load_pdf_from_byte_slice(&flat, None)
        .expect("pdfium reload of flattened output");
    assert_eq!(doc.pages().len(), 1);
    let page = doc.pages().get(0).expect("first page");
    let text = page.text().expect("page text").all();
    assert!(
        text.contains("initial"),
        "flattened page text should contain the field value; got: {text:?}"
    );
    assert!(
        text.contains("Form page"),
        "flattened page text should keep the static content; got: {text:?}"
    );
}

#[test]
fn flatten_is_a_noop_on_annotation_free_pdf() {
    let _guard = serial();
    let bytes = sample_pdf(&["Page one", "Page two"]);
    let flat = flatten(&bytes).expect("flatten formless pdf");

    let reloaded = Document::load_mem(&flat).expect("lopdf reload");
    assert_eq!(reloaded.get_pages().len(), 2);

    let pdfium = lock().expect("bind pdfium");
    let doc = pdfium
        .load_pdf_from_byte_slice(&flat, None)
        .expect("pdfium reload");
    let text = doc
        .pages()
        .get(0)
        .expect("first page")
        .text()
        .expect("text")
        .all();
    assert!(
        text.contains("Page one"),
        "original text must survive flatten; got: {text:?}"
    );
}

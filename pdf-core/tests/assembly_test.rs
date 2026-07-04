//! Integration tests for `pdf_core::assembly` — every public function is
//! exercised against real output bytes, reloaded with lopdf.

#![cfg(feature = "pdfium")]

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use pdf_core::assembly::{delete_pages, extract_pages, merge, reorder_pages, rotate_pages, split};
use pdf_core::error::PdfCoreError;
use pdf_core::meta::{PdfMetadata, get_metadata, set_metadata};
use pdf_core::testkit::sample_pdf;

/// Reloads output bytes and asserts they parse as a valid PDF.
fn load(bytes: &[u8]) -> Document {
    Document::load_mem(bytes).expect("output must be a loadable PDF")
}

/// Extracted text of a 1-based page in the reloaded output.
fn page_text(doc: &Document, page: u32) -> String {
    doc.extract_text(&[page]).expect("extract text")
}

/// The /Rotate value of a 1-based page (resolving inheritance is deliberately
/// NOT done here: rotate_pages must write the value onto the page itself).
fn page_rotation(doc: &Document, page: u32) -> i64 {
    let page_id = doc.get_pages()[&page];
    let dict = doc.get_dictionary(page_id).expect("page dict");
    match dict.get(b"Rotate") {
        Ok(object) => object.as_i64().expect("Rotate is an integer"),
        Err(_) => 0,
    }
}

fn assert_invalid_input<T: std::fmt::Debug>(result: pdf_core::Result<T>) {
    match result {
        Err(PdfCoreError::InvalidInput(_)) => {}
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

/// A PDF with a NESTED page tree: root Pages -> intermediate Pages node that
/// holds Resources + MediaBox, with two child pages that omit both and rely on
/// inheritance. Used to prove flattening materializes inherited attributes.
fn nested_tree_pdf(texts: [&str; 2]) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let root_pages_id = doc.new_object_id();
    let mid_pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut kids: Vec<Object> = Vec::new();
    for text in texts {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("encode content"),
        ));
        // No Resources / MediaBox here: inherited from the intermediate node.
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => mid_pages_id,
            "Contents" => content_id,
        });
        kids.push(page_id.into());
    }

    doc.objects.insert(
        mid_pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Parent" => root_pages_id,
            "Kids" => kids,
            "Count" => 2,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
        }),
    );
    doc.objects.insert(
        root_pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![mid_pages_id.into()],
            "Count" => 2,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => root_pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("serialize nested pdf");
    buf
}

/// Minimal AcroForm PDF: one page, one text field (merged field/widget dict)
/// registered in the catalog's /AcroForm /Fields. The AcroForm carries a /DR
/// with one font under `dr_font_key`, plus /NeedAppearances if requested.
fn form_pdf(field_name: &str, value: &str, dr_font_key: &str, need_appearances: bool) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let field_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "T" => Object::string_literal(field_name),
        "V" => Object::string_literal(value),
        "Rect" => vec![100.into(), 600.into(), 300.into(), 630.into()],
        "F" => 4,
    });
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Annots" => vec![Object::Reference(field_id)],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let mut acroform = dictionary! {
        "Fields" => vec![Object::Reference(field_id)],
        "DA" => Object::string_literal("/Helv 0 Tf 0 g"),
        "DR" => dictionary! { "Font" => dictionary! { dr_font_key => font_id } },
    };
    if need_appearances {
        acroform.set("NeedAppearances", true);
    }
    let acroform_id = doc.add_object(acroform);
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "AcroForm" => Object::Reference(acroform_id),
    });
    doc.trailer.set("Root", catalog_id);
    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("serialize form pdf");
    buf
}

/// The document's /AcroForm dictionary, resolving an indirect reference.
fn acroform_dict(doc: &Document) -> Dictionary {
    let object = doc
        .catalog()
        .expect("catalog")
        .get(b"AcroForm")
        .expect("catalog must have /AcroForm");
    match doc.dereference(object) {
        Ok((_, Object::Dictionary(dict))) => dict.clone(),
        other => panic!("/AcroForm must resolve to a dictionary, got {other:?}"),
    }
}

/// A malformed-but-real PDF whose root /Kids lists the SAME page object twice
/// (the shape this crate itself used to emit for duplicate extract_pages).
fn shared_page_pdf() -> Vec<u8> {
    let mut doc = Document::load_mem(&sample_pdf(&["Shared page"])).expect("load");
    let page_id = doc.get_pages()[&1];
    let pages_id = doc
        .catalog()
        .expect("catalog")
        .get(b"Pages")
        .and_then(Object::as_reference)
        .expect("Pages ref");
    let pages = doc.get_dictionary_mut(pages_id).expect("pages dict");
    pages.set(
        "Kids",
        vec![Object::Reference(page_id), Object::Reference(page_id)],
    );
    pages.set("Count", 2);
    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("serialize shared-page pdf");
    buf
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

#[test]
fn merge_two_docs_concatenates_pages_and_content() {
    let a = sample_pdf(&["Alpha text"]);
    let b = sample_pdf(&["Bravo text", "Charlie text"]);

    let merged = merge(&[&a, &b]).expect("merge");
    let doc = load(&merged);

    assert_eq!(doc.get_pages().len(), 3, "page count must be the sum");
    assert!(page_text(&doc, 1).contains("Alpha"));
    assert!(page_text(&doc, 2).contains("Bravo"));
    assert!(page_text(&doc, 3).contains("Charlie"));
}

#[test]
fn merge_single_doc_roundtrips() {
    let a = sample_pdf(&["Solo page"]);
    let merged = merge(&[&a]).expect("merge one doc");
    let doc = load(&merged);
    assert_eq!(doc.get_pages().len(), 1);
    assert!(page_text(&doc, 1).contains("Solo"));
}

#[test]
fn merge_preserves_page_dimensions() {
    let a = sample_pdf(&["Alpha"]);
    let merged = merge(&[&a, &a]).expect("merge");
    let doc = load(&merged);
    for page_id in doc.get_pages().values() {
        let dict = doc.get_dictionary(*page_id).expect("page dict");
        let media_box = dict.get(b"MediaBox").expect("MediaBox present");
        let values: Vec<i64> = media_box
            .as_array()
            .expect("array")
            .iter()
            .map(|v| v.as_i64().expect("int"))
            .collect();
        assert_eq!(values, vec![0, 0, 612, 792]);
    }
}

#[test]
fn merge_empty_doc_list_is_invalid() {
    assert_invalid_input(merge(&[]));
}

#[test]
fn merge_with_empty_byte_slice_is_invalid() {
    let a = sample_pdf(&["Alpha"]);
    assert_invalid_input(merge(&[&a, &[]]));
}

#[test]
fn merge_two_form_docs_keeps_all_fields() {
    let a = form_pdf("fieldA", "valueA", "FA", false);
    let b = form_pdf("fieldB", "valueB", "FB", false);

    // Sanity: each input alone exposes its field via pdfium.
    let fa = pdf_core::forms::list_fields(&a).expect("list fields of a");
    assert_eq!(fa.len(), 1);
    assert_eq!(fa[0].name, "fieldA");

    // pdfium is the arbiter: fields of BOTH inputs must stay enumerable (and
    // fillable) through the merged document's form-fill environment.
    let merged = merge(&[&a, &b]).expect("merge form docs");
    let fields = pdf_core::forms::list_fields(&merged).expect("list merged fields");
    let mut names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["fieldA", "fieldB"],
        "merge must union /AcroForm /Fields of all inputs"
    );
    for field in &fields {
        match field.name.as_str() {
            "fieldA" => assert_eq!(field.value.as_deref(), Some("valueA")),
            "fieldB" => assert_eq!(field.value.as_deref(), Some("valueB")),
            other => panic!("unexpected field {other}"),
        }
    }
}

#[test]
fn merge_creates_acroform_when_first_doc_has_none() {
    let plain = sample_pdf(&["No form here"]);
    let b = form_pdf("fieldB", "valueB", "FB", false);

    let merged = merge(&[&plain, &b]).expect("merge plain + form");
    let fields = pdf_core::forms::list_fields(&merged).expect("list merged fields");
    assert_eq!(fields.len(), 1, "second doc's field must survive");
    assert_eq!(fields[0].name, "fieldB");
}

#[test]
fn merge_unions_acroform_dr_and_preserves_need_appearances() {
    let a = form_pdf("fieldA", "valueA", "FA", false);
    let b = form_pdf("fieldB", "valueB", "FB", true);

    let merged = merge(&[&a, &b]).expect("merge form docs");
    let doc = load(&merged);
    let acroform = acroform_dict(&doc);

    let fields = match doc.dereference(acroform.get(b"Fields").expect("Fields")) {
        Ok((_, Object::Array(array))) => array.clone(),
        other => panic!("/Fields must be an array, got {other:?}"),
    };
    assert_eq!(fields.len(), 2, "merged /Fields lists both fields");

    assert!(
        acroform
            .get(b"NeedAppearances")
            .and_then(Object::as_bool)
            .expect("NeedAppearances"),
        "NeedAppearances set by ANY input must be preserved"
    );

    // /DR font resources of both inputs are unioned (first-wins on name
    // collisions; here the names are distinct so both must be present).
    let dr = match doc.dereference(acroform.get(b"DR").expect("DR")) {
        Ok((_, Object::Dictionary(dict))) => dict.clone(),
        other => panic!("/DR must be a dictionary, got {other:?}"),
    };
    let fonts = match doc.dereference(dr.get(b"Font").expect("DR Font")) {
        Ok((_, Object::Dictionary(dict))) => dict.clone(),
        other => panic!("/DR /Font must be a dictionary, got {other:?}"),
    };
    assert!(fonts.has(b"FA"), "first doc's DR font kept");
    assert!(fonts.has(b"FB"), "second doc's DR font merged in");
}

#[test]
fn merge_preserves_first_docs_info_metadata() {
    let a = set_metadata(
        &sample_pdf(&["Meta page"]),
        &PdfMetadata {
            title: Some("KeepMe".into()),
            author: Some("DocKosha".into()),
            ..PdfMetadata::default()
        },
    )
    .expect("set metadata");

    // A single-input merge must round-trip document metadata.
    let merged = merge(&[&a]).expect("merge single");
    let meta = get_metadata(&merged).expect("get metadata");
    assert_eq!(meta.title.as_deref(), Some("KeepMe"));
    assert_eq!(meta.author.as_deref(), Some("DocKosha"));

    // A multi-input merge keeps the FIRST document's /Info.
    let b = set_metadata(
        &sample_pdf(&["Other page"]),
        &PdfMetadata {
            title: Some("LoseMe".into()),
            ..PdfMetadata::default()
        },
    )
    .expect("set metadata");
    let merged = merge(&[&a, &b]).expect("merge two");
    assert_eq!(
        get_metadata(&merged)
            .expect("get metadata")
            .title
            .as_deref(),
        Some("KeepMe"),
        "first input's Info wins"
    );
}

// ---------------------------------------------------------------------------
// split
// ---------------------------------------------------------------------------

#[test]
fn split_three_page_doc_yields_three_single_page_docs() {
    let src = sample_pdf(&["Alpha text", "Bravo text", "Charlie text"]);
    let parts = split(&src).expect("split");
    assert_eq!(parts.len(), 3);

    let expected = ["Alpha", "Bravo", "Charlie"];
    for (part, expected_word) in parts.iter().zip(expected) {
        let doc = load(part);
        assert_eq!(doc.get_pages().len(), 1, "each part has exactly one page");
        assert!(
            page_text(&doc, 1).contains(expected_word),
            "part must contain {expected_word}"
        );
    }
}

#[test]
fn split_empty_input_is_invalid() {
    assert_invalid_input(split(&[]));
}

// ---------------------------------------------------------------------------
// extract_pages
// ---------------------------------------------------------------------------

#[test]
fn extract_pages_honors_requested_order() {
    let src = sample_pdf(&["Alpha text", "Bravo text", "Charlie text"]);
    let out = extract_pages(&src, &[3, 1]).expect("extract");
    let doc = load(&out);

    assert_eq!(doc.get_pages().len(), 2);
    assert!(
        page_text(&doc, 1).contains("Charlie"),
        "first output page must be source page 3"
    );
    assert!(
        page_text(&doc, 2).contains("Alpha"),
        "second output page must be source page 1"
    );
}

#[test]
fn extract_pages_duplicates_become_distinct_page_objects() {
    let src = sample_pdf(&["Dup me once"]);
    let out = extract_pages(&src, &[1, 1]).expect("extract [1,1]");
    let doc = load(&out);

    let pages = doc.get_pages();
    assert_eq!(pages.len(), 2, "duplicate selection yields two pages");
    let ids: Vec<ObjectId> = pages.values().copied().collect();
    assert_ne!(
        ids[0], ids[1],
        "each occurrence must be a DISTINCT page object (Kids must stay a tree)"
    );
    assert!(page_text(&doc, 1).contains("Dup"));
    assert!(page_text(&doc, 2).contains("Dup"));
}

#[test]
fn rotate_all_after_duplicate_extract_rotates_each_page_once() {
    let src = sample_pdf(&["Dup rotate"]);
    let out = extract_pages(&src, &[1, 1]).expect("extract [1,1]");
    let rotated = rotate_pages(&out, None, 90).expect("rotate all");
    let doc = load(&rotated);

    assert_eq!(page_rotation(&doc, 1), 90, "must not double-rotate");
    assert_eq!(page_rotation(&doc, 2), 90, "must not double-rotate");
}

#[test]
fn extract_pages_out_of_range_is_invalid() {
    let src = sample_pdf(&["Alpha", "Bravo"]);
    assert_invalid_input(extract_pages(&src, &[3]));
    assert_invalid_input(extract_pages(&src, &[0]));
}

#[test]
fn extract_pages_empty_selection_is_invalid() {
    let src = sample_pdf(&["Alpha"]);
    assert_invalid_input(extract_pages(&src, &[]));
}

#[test]
fn extract_pages_empty_pdf_is_invalid() {
    assert_invalid_input(extract_pages(&[], &[1]));
}

// ---------------------------------------------------------------------------
// delete_pages
// ---------------------------------------------------------------------------

#[test]
fn delete_middle_page_keeps_outer_pages_in_order() {
    let src = sample_pdf(&["Alpha text", "Bravo text", "Charlie text"]);
    let out = delete_pages(&src, &[2]).expect("delete");
    let doc = load(&out);

    assert_eq!(doc.get_pages().len(), 2);
    assert!(page_text(&doc, 1).contains("Alpha"));
    assert!(page_text(&doc, 2).contains("Charlie"));
    let all_text = doc.extract_text(&[1, 2]).expect("text");
    assert!(
        !all_text.contains("Bravo"),
        "deleted page text must be gone"
    );
}

#[test]
fn delete_all_pages_is_invalid() {
    let src = sample_pdf(&["Alpha", "Bravo"]);
    assert_invalid_input(delete_pages(&src, &[1, 2]));
}

#[test]
fn delete_pages_out_of_range_is_invalid() {
    let src = sample_pdf(&["Alpha", "Bravo"]);
    assert_invalid_input(delete_pages(&src, &[5]));
}

#[test]
fn delete_pages_empty_selection_is_invalid() {
    let src = sample_pdf(&["Alpha"]);
    assert_invalid_input(delete_pages(&src, &[]));
}

// ---------------------------------------------------------------------------
// reorder_pages
// ---------------------------------------------------------------------------

#[test]
fn reorder_pages_reverse() {
    let src = sample_pdf(&["Alpha text", "Bravo text", "Charlie text"]);
    let out = reorder_pages(&src, &[3, 2, 1]).expect("reorder");
    let doc = load(&out);

    assert_eq!(doc.get_pages().len(), 3);
    assert!(page_text(&doc, 1).contains("Charlie"));
    assert!(page_text(&doc, 2).contains("Bravo"));
    assert!(page_text(&doc, 3).contains("Alpha"));
}

#[test]
fn reorder_pages_flattens_nested_tree_and_keeps_inherited_attributes() {
    let src = nested_tree_pdf(["Delta text", "Echo text"]);
    let out = reorder_pages(&src, &[2, 1]).expect("reorder nested");
    let doc = load(&out);

    assert_eq!(doc.get_pages().len(), 2);
    assert!(page_text(&doc, 1).contains("Echo"));
    assert!(page_text(&doc, 2).contains("Delta"));

    // Inherited MediaBox and Resources must have been materialized onto the
    // pages, since their old intermediate parent node is gone.
    for page_id in doc.get_pages().values() {
        let dict = doc.get_dictionary(*page_id).expect("page dict");
        assert!(dict.has(b"MediaBox"), "page must own a MediaBox");
        assert!(dict.has(b"Resources"), "page must own Resources");
        let media_box: Vec<i64> = dict
            .get(b"MediaBox")
            .expect("MediaBox")
            .as_array()
            .expect("array")
            .iter()
            .map(|v| v.as_i64().expect("int"))
            .collect();
        assert_eq!(media_box, vec![0, 0, 612, 792]);
    }
}

#[test]
fn reorder_pages_rejects_non_permutations() {
    let src = sample_pdf(&["Alpha", "Bravo", "Charlie"]);
    assert_invalid_input(reorder_pages(&src, &[1, 2])); // wrong length
    assert_invalid_input(reorder_pages(&src, &[1, 1, 2])); // duplicate
    assert_invalid_input(reorder_pages(&src, &[1, 2, 4])); // out of range
    assert_invalid_input(reorder_pages(&src, &[0, 1, 2])); // zero page
}

// ---------------------------------------------------------------------------
// rotate_pages
// ---------------------------------------------------------------------------

#[test]
fn rotate_all_pages_by_90() {
    let src = sample_pdf(&["Alpha", "Bravo"]);
    let out = rotate_pages(&src, None, 90).expect("rotate");
    let doc = load(&out);

    assert_eq!(doc.get_pages().len(), 2);
    assert_eq!(page_rotation(&doc, 1), 90);
    assert_eq!(page_rotation(&doc, 2), 90);
    // Content untouched.
    assert!(page_text(&doc, 1).contains("Alpha"));
}

#[test]
fn rotate_specific_page_only() {
    let src = sample_pdf(&["Alpha", "Bravo"]);
    let out = rotate_pages(&src, Some(&[2]), 90).expect("rotate");
    let doc = load(&out);

    assert_eq!(page_rotation(&doc, 1), 0);
    assert_eq!(page_rotation(&doc, 2), 90);
}

#[test]
fn rotate_negative_degrees_normalizes_to_positive() {
    let src = sample_pdf(&["Alpha"]);
    let out = rotate_pages(&src, None, -90).expect("rotate");
    let doc = load(&out);
    assert_eq!(page_rotation(&doc, 1), 270);
}

#[test]
fn rotate_accumulates_with_existing_rotation() {
    let src = sample_pdf(&["Alpha"]);
    let once = rotate_pages(&src, None, 90).expect("rotate once");
    let twice = rotate_pages(&once, None, 90).expect("rotate twice");
    let doc = load(&twice);
    assert_eq!(page_rotation(&doc, 1), 180);

    // A full extra turn lands back on the same orientation.
    let full_turn = rotate_pages(&twice, None, 360).expect("full turn");
    assert_eq!(page_rotation(&load(&full_turn), 1), 180);
}

#[test]
fn rotate_duplicate_page_numbers_rotates_once() {
    let src = sample_pdf(&["Alpha"]);
    let out = rotate_pages(&src, Some(&[1, 1]), 90).expect("rotate");
    assert_eq!(page_rotation(&load(&out), 1), 90);
}

#[test]
fn rotate_all_on_shared_page_object_rotates_once() {
    // Two page NUMBERS backed by ONE object: rotation must dedup by object
    // id, otherwise the shared dict is rotated once per page number.
    let src = shared_page_pdf();
    let rotated = rotate_pages(&src, None, 90).expect("rotate all");
    let doc = load(&rotated);
    assert_eq!(page_rotation(&doc, 1), 90);
    assert_eq!(page_rotation(&doc, 2), 90);
}

#[test]
fn rotate_non_multiple_of_90_is_invalid() {
    let src = sample_pdf(&["Alpha"]);
    assert_invalid_input(rotate_pages(&src, None, 45));
    assert_invalid_input(rotate_pages(&src, None, -30));
}

#[test]
fn rotate_out_of_range_page_is_invalid() {
    let src = sample_pdf(&["Alpha"]);
    assert_invalid_input(rotate_pages(&src, Some(&[2]), 90));
    assert_invalid_input(rotate_pages(&src, Some(&[0]), 90));
}

#[test]
fn rotate_empty_pdf_is_invalid() {
    assert_invalid_input(rotate_pages(&[], None, 90));
}

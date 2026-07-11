//! Integration and pdfium cross-validation tests for Zone-1 redaction.

#![cfg(feature = "pdfium")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, Stream, StringFormat, dictionary};
use serial_test::serial;

use pdf_core::error::PdfCoreError;
use pdf_core::extract::{extract_text, extract_text_pages};
use pdf_core::meta::{self, page_count, validate};
use pdf_core::raster::{RenderOptions, render_page};
use pdf_core::redact::{RedactionRect, redact};
use pdf_core::redact_pure::redact_pure;
use pdf_core::redact_types::RedactArea;
use pdf_core::testkit::sample_pdf;

const PAGE_W: f64 = 612.0;
const PAGE_H: f64 = 792.0;

fn area(page_index: u32, left: f64, top: f64, width: f64, height: f64) -> RedactArea {
    RedactArea {
        page_index,
        left,
        top,
        width,
        height,
    }
}

fn full_page_area(page_index: u32) -> RedactArea {
    area(page_index, 0.0, 0.0, 100.0, 100.0)
}

fn top_text_area(page_index: u32) -> RedactArea {
    area_for_region(page_index, 0, 60.0, 340.0, 700.0, 750.0)
}

fn finish_doc(mut doc: Document, pages_id: lopdf::ObjectId, kids: Vec<Object>) -> Vec<u8> {
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize fixture");
    bytes
}

fn pdf_with_texts(texts: &[(&str, i64, i64)], rotate: Option<i64>) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let operations: Vec<Operation> = texts
        .iter()
        .flat_map(|(text, x, y)| {
            [
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![(*x).into(), (*y).into()]),
                Operation::new("Tj", vec![Object::string_literal(*text)]),
                Operation::new("ET", vec![]),
            ]
        })
        .collect();
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().expect("encode content"),
    ));
    let mut page = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
    };
    if let Some(rotation) = rotate {
        page.set("Rotate", rotation);
    }
    let page_id = doc.add_object(page);
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn pdf_with_operations(operations: Vec<Operation>) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn pdf_with_raw_content(
    content: Vec<u8>,
    stream_dictionary: Dictionary,
    resources: Dictionary,
) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content_id =
        doc.add_object(Stream::new(stream_dictionary, content).with_compression(false));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources,
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn replace_first_page_contents(pdf: &[u8], contents: Object) -> Vec<u8> {
    let mut doc = Document::load_mem(pdf).expect("load fixture for /Contents replacement");
    let page_id = *doc.get_pages().values().next().expect("first page");
    doc.get_object_mut(page_id)
        .expect("page object")
        .as_dict_mut()
        .expect("page dictionary")
        .set("Contents", contents);
    let mut output = Vec::new();
    doc.save_to(&mut output)
        .expect("serialize fixture with replaced /Contents");
    output
}

fn pdf_with_offset_crop_box() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![172.into(), 920.into()]),
        Operation::new("Tj", vec![Object::string_literal("OFFSET-ROTATE-SECRET")]),
        Operation::new("ET", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![172.into(), 320.into()]),
        Operation::new("Tj", vec![Object::string_literal("OFFSET-ROTATE-KEEP")]),
        Operation::new("ET", vec![]),
    ];
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![50.into(), 150.into(), 762.into(), 1042.into()],
        "CropBox" => vec![100.into(), 200.into(), 712.into(), 992.into()],
        "Rotate" => 90,
        "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn stroked_text_extgstate_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let graphics_state_id = doc.add_object(dictionary! {
        "Type" => "ExtGState",
        "LW" => 20,
        "ML" => 1,
    });
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![200.into(), 720.into()]),
        Operation::new("Tr", vec![1.into()]),
        Operation::new("gs", vec!["GS1".into()]),
        Operation::new("Tj", vec![Object::string_literal("I")]),
        Operation::new("ET", vec![]),
    ];
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "ExtGState" => dictionary! { "GS1" => graphics_state_id },
        },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn explicit_width_pdf(subtype: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "MetricFixture",
        "Flags" => 32,
        "FontBBox" => vec![0.into(), (-200).into(), 1000.into(), 800.into()],
        "ItalicAngle" => 0,
        "Ascent" => 800,
        "Descent" => -200,
        "CapHeight" => 700,
        "StemV" => 80,
        "MissingWidth" => 400,
    });
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => Object::Name(subtype.as_bytes().to_vec()),
        "BaseFont" => "MetricFixture",
        "FirstChar" => 65,
        "LastChar" => 65,
        "Widths" => vec![500.into()],
        "FontDescriptor" => descriptor_id,
    });
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new("Tj", vec![Object::string_literal("AA")]),
        Operation::new("ET", vec![]),
    ];
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn cid_pdf(encoding: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "CidFixture",
        "Flags" => 4,
        "FontBBox" => vec![0.into(), (-200).into(), 1000.into(), 900.into()],
        "ItalicAngle" => 0,
        "Ascent" => 880,
        "Descent" => -120,
        "CapHeight" => 700,
        "StemV" => 80,
    });
    let descendant_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "CidFixture",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 1000,
        "W" => vec![1.into(), Object::Array(vec![500.into()])],
    });
    let type0_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "CidFixture",
        "Encoding" => Object::Name(encoding.as_bytes().to_vec()),
        "DescendantFonts" => vec![descendant_id.into()],
    });
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F0".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new(
            "Tj",
            vec![Object::String(vec![0, 1], StringFormat::Hexadecimal)],
        ),
        Operation::new("ET", vec![]),
    ];
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F0" => type0_id } },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn type3_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let charproc_id = doc.add_object(Stream::new(
        dictionary! {},
        b"750 0 0 0 750 750 d1 0 0 750 750 re f".to_vec(),
    ));
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "FontBBox" => vec![0.into(), 0.into(), 750.into(), 750.into()],
        "FontMatrix" => vec![0.001.into(), 0.into(), 0.into(), 0.001.into(), 0.into(), 0.into()],
        "CharProcs" => dictionary! { "square" => charproc_id },
        "Encoding" => dictionary! {
            "Type" => "Encoding",
            "Differences" => vec![97.into(), "square".into()],
        },
        "FirstChar" => 97,
        "LastChar" => 97,
        "Widths" => vec![750.into()],
    });
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F3".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new("Tj", vec![Object::string_literal("aaa")]),
        Operation::new("ET", vec![]),
    ];
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F3" => font_id } },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn form_pdf(secret: &str, include_bbox: bool) -> Vec<u8> {
    form_pdf_with_matrix(secret, include_bbox, None)
}

fn form_pdf_with_matrix(secret: &str, include_bbox: bool, matrix: Option<[i64; 6]>) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let form_content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![20.into(), 20.into()]),
            Operation::new("Tj", vec![Object::string_literal(secret)]),
            Operation::new("ET", vec![]),
        ],
    };
    let mut form_dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
    };
    if include_bbox {
        form_dict.set(
            "BBox",
            Object::Array(vec![0.into(), 0.into(), 320.into(), 60.into()]),
        );
    }
    if let Some(matrix) = matrix {
        form_dict.set(
            "Matrix",
            Object::Array(matrix.into_iter().map(Object::from).collect()),
        );
    }
    let form_id = doc.add_object(Stream::new(
        form_dict,
        form_content.encode().expect("encode form"),
    ));
    let page_content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    50.into(),
                    680.into(),
                ],
            ),
            Operation::new("Do", vec!["X1".into()]),
            Operation::new("Q", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        page_content.encode().expect("encode page"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! { "XObject" => dictionary! { "X1" => form_id } },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn form_pdf_with_degenerate_bbox() -> Vec<u8> {
    let mut doc =
        Document::load_mem(&form_pdf("DEGENERATE-BBOX", true)).expect("load form fixture");
    for object in doc.objects.values_mut() {
        let Ok(stream) = object.as_stream_mut() else {
            continue;
        };
        if stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|object| object.as_name().ok())
            == Some(b"Form".as_slice())
        {
            stream
                .dict
                .set("BBox", vec![0.into(), 0.into(), 0.into(), 60.into()]);
        }
    }
    let mut output = Vec::new();
    doc.save_to(&mut output)
        .expect("save degenerate form fixture");
    output
}

fn annotation_pdf(secret: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let annotation_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "FreeText",
        "Rect" => vec![72.into(), 690.into(), 300.into(), 740.into()],
        "Contents" => Object::string_literal(secret),
        "DA" => Object::string_literal("/Helv 12 Tf 0 g"),
    });
    let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Annots" => vec![annotation_id.into()],
        "Resources" => dictionary! {},
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn inline_image_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut content = b"q\n100 0 0 100 72 650 cm\nBI /W 1 /H 1 /CS /RGB /BPC 8\nID\n".to_vec();
    content.extend_from_slice(&[255, 0, 0]);
    content.extend_from_slice(b"\nEI\nQ");
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {},
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn area_for_region(
    page_index: u32,
    rotation: i64,
    left: f64,
    right: f64,
    bottom: f64,
    top: f64,
) -> RedactArea {
    let (x0, y0, display_w, display_h) = to_display(rotation, left, bottom);
    let (x1, y1, _, _) = to_display(rotation, right, top);
    let display_left = x0.min(x1);
    let display_right = x0.max(x1);
    let display_bottom = y0.min(y1);
    let display_top = y0.max(y1);
    area(
        page_index,
        display_left / display_w * 100.0,
        (display_h - display_top) / display_h * 100.0,
        (display_right - display_left) / display_w * 100.0,
        (display_top - display_bottom) / display_h * 100.0,
    )
}

fn to_display(rotation: i64, x: f64, y: f64) -> (f64, f64, f64, f64) {
    match rotation {
        0 => (x, y, PAGE_W, PAGE_H),
        90 => (y, PAGE_W - x, PAGE_H, PAGE_W),
        180 => (PAGE_W - x, PAGE_H - y, PAGE_W, PAGE_H),
        270 => (PAGE_H - y, x, PAGE_H, PAGE_W),
        _ => unreachable!("unsupported fixture rotation {rotation}"),
    }
}

fn page_operations(pdf: &[u8]) -> Vec<Operation> {
    let doc = Document::load_mem(pdf).expect("load PDF");
    let page_id = *doc.get_pages().values().next().expect("first page");
    let bytes = doc.get_page_content(page_id).expect("page content");
    Content::decode(&bytes)
        .expect("decode page content")
        .operations
}

fn page_show_count(pdf: &[u8]) -> usize {
    page_operations(pdf)
        .iter()
        .filter(|operation| matches!(operation.operator.as_str(), "Tj" | "TJ" | "'" | "\""))
        .count()
}

fn bytes_contain(pdf: &[u8], needle: &str) -> bool {
    fn contains(bytes: &[u8], needle: &[u8]) -> bool {
        bytes.windows(needle.len()).any(|window| window == needle)
    }

    if contains(pdf, needle.as_bytes()) {
        return true;
    }
    let Ok(doc) = Document::load_mem(pdf) else {
        return true;
    };
    doc.objects.values().any(|object| match object {
        Object::String(bytes, _) => contains(bytes, needle.as_bytes()),
        Object::Stream(stream) => {
            contains(&stream.content, needle.as_bytes())
                || stream
                    .decompressed_content()
                    .is_ok_and(|bytes| contains(&bytes, needle.as_bytes()))
        }
        _ => false,
    })
}

fn assert_unsupported(result: pdf_core::Result<pdf_core::redact_types::RedactOutput>) {
    let error = result.expect_err("fixture must fail closed");
    assert!(
        matches!(error, PdfCoreError::Unsupported(_)),
        "expected Unsupported, got {error:?}"
    );
}

#[test]
#[serial(pdfium)]
fn targeted_redaction_removes_secret_preserves_other_text_and_structure() {
    let input = pdf_with_texts(
        &[("PURE-SECRET-7fd", 72, 720), ("KEEP-OUTSIDE-4aa", 72, 120)],
        None,
    );
    let output = redact_pure(&input, &[top_text_area(0)]).expect("pure redact");
    let extracted = extract_text(&output.pdf).expect("extract redacted output");

    assert!(!extracted.contains("PURE-SECRET-7fd"), "got {extracted:?}");
    assert!(extracted.contains("KEEP-OUTSIDE-4aa"), "got {extracted:?}");
    assert_eq!(output.page_count, 1);
    assert_eq!(page_count(&output.pdf).expect("page count"), 1);
    assert_eq!(validate(&output.pdf).expect("validate").page_count, 1);
    assert!(!bytes_contain(&output.pdf, "PURE-SECRET-7fd"));
}

#[test]
#[serial(pdfium)]
fn redaction_draws_opaque_black_box_without_covering_unselected_page() {
    let input = pdf_with_texts(&[("PAINT-ME", 72, 720)], None);
    let output = redact_pure(&input, &[top_text_area(0)]).expect("pure redact");
    let png = render_page(&output.pdf, 0, &RenderOptions::default()).expect("rasterize");
    let image = image::load_from_memory(&png).expect("decode PNG").to_rgb8();
    let (_, height) = image.dimensions();
    let inside = image.get_pixel(150, height - 1 - 725).0;
    let outside = image.get_pixel(450, height - 1 - 650).0;

    assert!(
        inside.iter().all(|channel| *channel < 40),
        "inside={inside:?}"
    );
    assert!(
        outside.iter().all(|channel| *channel > 200),
        "outside={outside:?}"
    );
}

#[test]
#[serial(pdfium)]
fn displayed_percent_rect_maps_correctly_for_every_page_rotation() {
    for rotation in [0, 90, 180, 270] {
        let input = pdf_with_texts(
            &[("ROTATE-SECRET", 72, 720), ("ROTATE-KEEP", 72, 120)],
            Some(rotation),
        );
        let selected = area_for_region(0, rotation, 60.0, 340.0, 700.0, 750.0);
        let output = redact_pure(&input, &[selected]).expect("pure redact rotated page");
        let extracted = extract_text(&output.pdf).expect("extract rotated output");

        assert!(
            !extracted.contains("ROTATE-SECRET"),
            "rotation {rotation}: {extracted:?}"
        );
        assert!(
            extracted.contains("ROTATE-KEEP"),
            "rotation {rotation}: {extracted:?}"
        );
    }
}

#[test]
#[serial(pdfium)]
fn rotated_crop_box_with_nonzero_origin_maps_displayed_rect_to_page_space() {
    let input = pdf_with_offset_crop_box();
    let selected = area_for_region(0, 90, 60.0, 340.0, 700.0, 750.0);
    let output = redact_pure(&input, &[selected]).expect("redact offset rotated page");
    let extracted = extract_text(&output.pdf).expect("extract offset rotated output");

    assert!(
        !extracted.contains("OFFSET-ROTATE-SECRET") && extracted.contains("OFFSET-ROTATE-KEEP"),
        "got {extracted:?}"
    );
}

#[test]
#[serial(pdfium)]
fn chained_noncommuting_cm_operators_place_text_using_pdf_concatenation_order() {
    let input = pdf_with_operations(vec![
        Operation::new("q", vec![]),
        Operation::new(
            "cm",
            vec![2.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()],
        ),
        Operation::new(
            "cm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), 100.into(), 0.into()],
        ),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![10.into(), 720.into()]),
        Operation::new("Tj", vec![Object::string_literal("CHAINED-CM-SECRET")]),
        Operation::new("ET", vec![]),
        Operation::new("Q", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 120.into()]),
        Operation::new("Tj", vec![Object::string_literal("CHAINED-CM-KEEP")]),
        Operation::new("ET", vec![]),
    ]);
    let selected = area_for_region(0, 0, 215.0, 245.0, 715.0, 735.0);
    let output = redact_pure(&input, &[selected]).expect("redact text under chained cm");
    let extracted = extract_text(&output.pdf).expect("extract chained-cm output");

    assert!(
        !extracted.contains("CHAINED-CM-SECRET") && extracted.contains("CHAINED-CM-KEEP"),
        "got {extracted:?}"
    );
}

#[test]
fn nonidentity_form_matrix_is_applied_before_intersection_testing() {
    let secret = "FORM-MATRIX-SECRET";
    let input = form_pdf_with_matrix(secret, true, Some([1, 0, 0, 1, 300, -100]));
    let selected = area_for_region(0, 0, 360.0, 400.0, 590.0, 630.0);
    let output = redact_pure(&input, &[selected]).expect("redact transformed form");

    assert!(!bytes_contain(&output.pdf, secret));
}

#[test]
fn clipping_render_mode_used_before_reset_fails_closed_for_removed_text_object() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 120.into()]),
        Operation::new("Tr", vec![4.into()]),
        Operation::new("Tj", vec![Object::string_literal("CLIPPING-TEXT")]),
        Operation::new("Tr", vec![0.into()]),
        Operation::new("Td", vec![0.into(), 600.into()]),
        Operation::new("Tj", vec![Object::string_literal("SELECTED-TEXT")]),
        Operation::new("ET", vec![]),
    ]);

    assert_unsupported(redact_pure(&input, &[top_text_area(0)]));
}

#[test]
fn negative_character_spacing_bounds_every_intermediate_glyph_origin() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![200.into(), 720.into()]),
        Operation::new("Tc", vec![(-10).into()]),
        Operation::new("Tj", vec![Object::string_literal("Wi")]),
        Operation::new("ET", vec![]),
    ]);
    let selected = area_for_region(0, 0, 236.0, 238.0, 715.0, 735.0);
    let output = redact_pure(&input, &[selected]).expect("redact intermediate glyph extent");

    assert_eq!(page_show_count(&output.pdf), 0);
}

#[test]
fn extgstate_line_width_inflates_stroked_text_bounds() {
    let input = stroked_text_extgstate_pdf();
    let selected = area_for_region(0, 0, 181.0, 183.0, 715.0, 735.0);
    let output = redact_pure(&input, &[selected]).expect("redact stroked text extent");

    assert_eq!(page_show_count(&output.pdf), 0);
}

#[test]
fn explicit_widths_are_resolved_for_type1_truetype_and_mmtype1_fonts() {
    for subtype in ["Type1", "TrueType", "MMType1"] {
        let input = explicit_width_pdf(subtype);
        let selected = area_for_region(0, 0, 88.0, 94.0, 715.0, 740.0);
        let output = redact_pure(&input, &[selected]).expect("redact explicit-width text");
        assert_eq!(
            page_show_count(&output.pdf),
            0,
            "{subtype} width must place AA through x=96"
        );
    }
}

#[test]
fn identity_h_cid_widths_are_resolved() {
    let input = cid_pdf("Identity-H");
    let selected = area_for_region(0, 0, 78.0, 81.0, 714.0, 742.0);
    let output = redact_pure(&input, &[selected]).expect("redact Identity-H text");
    assert_eq!(page_show_count(&output.pdf), 0);
}

#[test]
fn all_text_showing_and_state_operators_are_analyzed_in_invisible_mode() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new(
            "Tm",
            vec![
                1.into(),
                0.into(),
                0.into(),
                1.into(),
                72.into(),
                720.into(),
            ],
        ),
        Operation::new("Tc", vec![1.into()]),
        Operation::new("Tw", vec![2.into()]),
        Operation::new("Tz", vec![90.into()]),
        Operation::new("TL", vec![24.into()]),
        Operation::new("Ts", vec![2.into()]),
        Operation::new("Tr", vec![3.into()]),
        Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("INVISIBLE-TJ"),
                (-120).into(),
                Object::string_literal("-SECRET"),
            ])],
        ),
        Operation::new("TD", vec![0.into(), (-24).into()]),
        Operation::new("T*", vec![]),
        Operation::new("'", vec![Object::string_literal("QUOTE-SECRET")]),
        Operation::new(
            "\"",
            vec![
                2.into(),
                1.into(),
                Object::string_literal("DOUBLE-QUOTE-SECRET"),
            ],
        ),
        Operation::new("ET", vec![]),
    ]);
    let output = redact_pure(&input, &[full_page_area(0)]).expect("redact all show operators");

    assert_eq!(page_show_count(&output.pdf), 0);
    assert!(!bytes_contain(&output.pdf, "INVISIBLE-TJ"));
    assert!(!bytes_contain(&output.pdf, "QUOTE-SECRET"));
    assert!(!bytes_contain(&output.pdf, "DOUBLE-QUOTE-SECRET"));
}

#[test]
fn type3_font_fails_closed() {
    assert_unsupported(redact_pure(&type3_pdf(), &[full_page_area(0)]));
}

#[test]
fn non_identity_cmap_fails_closed() {
    assert_unsupported(redact_pure(
        &cid_pdf("UniJIS-UTF16-H"),
        &[full_page_area(0)],
    ));
}

#[test]
fn unresolved_and_non_stream_page_contents_fail_closed() {
    let base = pdf_with_texts(&[("CONTENTS-SECRET", 72, 720)], None);
    let fixtures = [
        (
            "unresolved reference",
            replace_first_page_contents(&base, Object::Reference((999_999, 0))),
        ),
        (
            "non-stream object",
            replace_first_page_contents(&base, Object::Integer(7)),
        ),
    ];

    for (label, input) in fixtures {
        let result = redact_pure(&input, &[full_page_area(0)]);
        let error = result.expect_err(label);
        assert!(
            matches!(error, PdfCoreError::Unsupported(_)),
            "{label}: expected Unsupported, got {error:?}"
        );
    }
}

#[test]
fn malformed_and_truncated_flate_page_contents_fail_closed() {
    let fixtures = [
        ("invalid zlib header", vec![0, 0, 0, 0, 0, 0]),
        ("truncated DEFLATE payload", vec![0x78, 0x9c, 0, 0, 0, 0]),
    ];

    for (label, content) in fixtures {
        let input = pdf_with_raw_content(
            content,
            dictionary! { "Filter" => "FlateDecode" },
            dictionary! {},
        );
        let result = redact_pure(&input, &[full_page_area(0)]);
        let error = result.expect_err(label);
        assert!(
            matches!(error, PdfCoreError::Unsupported(_)),
            "{label}: expected Unsupported, got {error:?}"
        );
    }
}

#[test]
fn marked_content_scope_crossing_text_object_boundary_fails_closed() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new("Tj", vec![Object::string_literal("MARKED-SECRET")]),
        Operation::new("BMC", vec![Object::Name(b"Span".to_vec())]),
        Operation::new("ET", vec![]),
        Operation::new("EMC", vec![]),
    ]);

    assert_unsupported(redact_pure(&input, &[full_page_area(0)]));
}

#[test]
fn intersecting_actual_text_marked_content_fails_closed() {
    let input = pdf_with_operations(vec![
        Operation::new(
            "BDC",
            vec![
                Object::Name(b"Span".to_vec()),
                Object::Dictionary(dictionary! {
                    "ActualText" => Object::string_literal("REPLACEMENT-SECRET"),
                }),
            ],
        ),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new("Tj", vec![Object::string_literal("VISIBLE-SECRET")]),
        Operation::new("ET", vec![]),
        Operation::new("EMC", vec![]),
    ]);

    assert_unsupported(redact_pure(&input, &[full_page_area(0)]));
}

#[test]
fn used_extgstate_soft_mask_fails_closed() {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mask_group_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "Group" => dictionary! { "S" => "Transparency" },
            "Resources" => dictionary! {},
        },
        b"0 0 10 10 re f".to_vec(),
    ));
    let extgstate_id = doc.add_object(dictionary! {
        "Type" => "ExtGState",
        "SMask" => dictionary! {
            "S" => "Alpha",
            "G" => mask_group_id,
        },
    });
    let operations = vec![
        Operation::new("q", vec![]),
        Operation::new("gs", vec![Object::Name(b"GS1".to_vec())]),
        Operation::new("re", vec![0.into(), 0.into(), 100.into(), 100.into()]),
        Operation::new("f", vec![]),
        Operation::new("Q", vec![]),
    ];
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "ExtGState" => dictionary! { "GS1" => extgstate_id },
        },
    });
    let input = finish_doc(doc, pages_id, vec![page_id.into()]);

    assert_unsupported(redact_pure(&input, &[full_page_area(0)]));
}

#[test]
fn used_pattern_scn_color_fails_closed() {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let pattern_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "Pattern",
            "PatternType" => 1,
            "PaintType" => 1,
            "TilingType" => 1,
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "XStep" => 10,
            "YStep" => 10,
            "Resources" => dictionary! {},
        },
        b"0 0 10 10 re f".to_vec(),
    ));
    let operations = vec![
        Operation::new("CS", vec![Object::Name(b"Pattern".to_vec())]),
        Operation::new("SCN", vec![Object::Name(b"P1".to_vec())]),
        Operation::new("re", vec![0.into(), 0.into(), 100.into(), 100.into()]),
        Operation::new("S", vec![]),
    ];
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Pattern" => dictionary! { "P1" => pattern_id },
        },
    });
    let input = finish_doc(doc, pages_id, vec![page_id.into()]);

    assert_unsupported(redact_pure(&input, &[full_page_area(0)]));
}

#[test]
fn hostile_inline_image_dimensions_fail_closed_without_panicking() {
    let input = pdf_with_raw_content(
        b"q\nBI /W 9223372036854775807 /H 9223372036854775807 /CS /RGB /BPC 8\nID\nEI\nQ\n"
            .to_vec(),
        dictionary! {},
        dictionary! {},
    );
    let result = std::panic::catch_unwind(|| redact_pure(&input, &[full_page_area(0)]));

    assert!(result.is_ok(), "hostile inline image caused a panic");
    assert_unsupported(result.expect("checked above"));
}

#[test]
fn form_without_bbox_fails_closed() {
    assert_unsupported(redact_pure(
        &form_pdf("NO-BBOX-SECRET", false),
        &[full_page_area(0)],
    ));
}

#[test]
fn form_with_degenerate_bbox_fails_closed() {
    assert_unsupported(redact_pure(
        &form_pdf_with_degenerate_bbox(),
        &[full_page_area(0)],
    ));
}

#[test]
fn encrypted_pdf_fails_closed() {
    let encrypted = meta::encrypt(&sample_pdf(&["ENCRYPTED-SECRET"]), "", "owner-only")
        .expect("encrypt fixture");
    assert_unsupported(redact_pure(&encrypted, &[full_page_area(0)]));
}

#[test]
fn intersecting_form_is_removed_in_full_and_pruned() {
    let secret = "FORM-SECRET-prune";
    let input = form_pdf(secret, true);
    let output = redact_pure(&input, &[top_text_area(0)]).expect("redact form");

    assert!(!bytes_contain(&output.pdf, secret));
    assert!(
        output
            .warnings
            .iter()
            .any(|warning| warning.contains("Form")),
        "expected excess-coverage warning, got {:?}",
        output.warnings
    );
}

#[test]
fn intersecting_annotation_is_deleted_and_pruned() {
    let secret = "ANNOTATION-SECRET-prune";
    let input = annotation_pdf(secret);
    let output = redact_pure(&input, &[top_text_area(0)]).expect("redact annotation");
    let doc = Document::load_mem(&output.pdf).expect("load output");
    let page_id = *doc.get_pages().values().next().expect("first page");
    let page = doc.get_dictionary(page_id).expect("page dictionary");
    let annotations_are_empty = match page.get(b"Annots") {
        Err(_) => true,
        Ok(Object::Array(annotations)) => annotations.is_empty(),
        Ok(other) => panic!("unexpected Annots value: {other:?}"),
    };

    assert!(!bytes_contain(&output.pdf, secret));
    assert!(annotations_are_empty, "intersecting annotation survived");
}

#[test]
fn intersecting_inline_image_is_preserved_with_its_exact_samples() {
    let input = inline_image_pdf();
    let output = redact_pure(&input, &[top_text_area(0)]).expect("redact inline image");
    let operations = page_operations(&output.pdf);
    let inline = operations
        .iter()
        .find(|operation| operation.operator == "BI")
        .expect("inline image operation survives");
    let Object::Stream(stream) = &inline.operands[0] else {
        panic!(
            "inline image operand is not a stream: {:?}",
            inline.operands
        );
    };

    assert_eq!(stream.content, [255, 0, 0]);
}

#[test]
fn metadata_survives_content_rewrite_and_pruning() {
    let input = meta::set_metadata(
        &sample_pdf(&["METADATA SECRET"]),
        &meta::PdfMetadata {
            title: Some("Keep this title".to_string()),
            author: Some("DocYantra tests".to_string()),
            ..meta::PdfMetadata::default()
        },
    )
    .expect("set fixture metadata");
    let output = redact_pure(&input, &[full_page_area(0)]).expect("redact metadata fixture");
    let metadata = meta::get_metadata(&output.pdf).expect("read output metadata");

    assert_eq!(metadata.title.as_deref(), Some("Keep this title"));
    assert_eq!(metadata.author.as_deref(), Some("DocYantra tests"));
}

#[test]
fn partial_text_object_selection_warns_and_covers_whole_removed_bounds() {
    let input = pdf_with_texts(&[("ALPHA SECRET OMEGA", 72, 720)], None);
    let selected = area_for_region(0, 0, 145.0, 220.0, 715.0, 740.0);
    let output = redact_pure(&input, &[selected]).expect("redact partial text run");

    assert!(!output.warnings.is_empty(), "whole-object excess must warn");
    assert_eq!(page_show_count(&output.pdf), 0);
}

fn mock_pdf_paths() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../mock-files");
    let mut paths = Vec::new();
    for directory in [root.join("pdf"), root.join("pdf_ppt")] {
        for entry in fs::read_dir(&directory).expect("read mock PDF directory") {
            let path = entry.expect("directory entry").path();
            if path.extension().is_some_and(|extension| extension == "pdf") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
}

fn words(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect()
}

fn pdfium_redactions(selected: RedactArea) -> BTreeMap<u32, Vec<RedactionRect>> {
    BTreeMap::from([(
        selected.page_index,
        vec![RedactionRect {
            left_pct: selected.left,
            top_pct: selected.top,
            width_pct: selected.width,
            height_pct: selected.height,
        }],
    )])
}

#[test]
#[serial(pdfium)]
fn pure_redaction_removes_at_least_everything_removed_by_pdfium() {
    let mut corpus: Vec<(String, Vec<u8>)> = mock_pdf_paths()
        .into_iter()
        .map(|path| {
            let label = path
                .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
                .unwrap_or(&path)
                .display()
                .to_string();
            let bytes = fs::read(&path).expect("read corpus PDF");
            (label, bytes)
        })
        .collect();
    corpus.extend([
        (
            "generated/sample".to_string(),
            sample_pdf(&["SAMPLE ALPHA BETA"]),
        ),
        (
            "generated/two-runs".to_string(),
            pdf_with_texts(&[("TOP RUN", 72, 720), ("BOTTOM RUN", 72, 120)], None),
        ),
        (
            "generated/form".to_string(),
            form_pdf("FORM CORPUS SECRET", true),
        ),
        ("generated/type3".to_string(), type3_pdf()),
        (
            "generated/non-identity-cmap".to_string(),
            cid_pdf("UniJIS-UTF16-H"),
        ),
    ]);

    let corpus_size = corpus.len();
    let cases = [
        ("full-page", full_page_area(0)),
        ("top-half", area(0, 0.0, 0.0, 100.0, 50.0)),
    ];
    let mut compared_pdfs = BTreeSet::new();
    let mut comparison_cases = 0usize;
    let mut pure_failed_closed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut pdfium_failed = Vec::new();

    for (label, input) in &corpus {
        let input_pages = page_count(input).expect("corpus page count");
        let input_text = extract_text_pages(input).expect("extract corpus input");
        for (case_label, selected) in cases {
            let pure = match redact_pure(input, &[selected]) {
                Ok(output) => output,
                Err(PdfCoreError::Unsupported(reason)) => {
                    pure_failed_closed
                        .entry(label.clone())
                        .or_default()
                        .push(format!("{case_label}: {reason}"));
                    continue;
                }
                Err(error) => panic!(
                    "{label}/{case_label}: pure redactor returned non-fail-closed error: {error}"
                ),
            };
            assert_eq!(
                pure.page_count, input_pages,
                "{label}/{case_label}: output page count"
            );
            assert_eq!(
                validate(&pure.pdf)
                    .expect("validate pure output")
                    .page_count,
                input_pages,
                "{label}/{case_label}: validated page count"
            );
            let baseline = match redact(input, &pdfium_redactions(selected)) {
                Ok(output) => output,
                Err(error) => {
                    pdfium_failed.push(format!("{label}/{case_label}: {error}"));
                    continue;
                }
            };
            assert_eq!(
                baseline.page_count, input_pages,
                "{label}/{case_label}: pdfium output page count"
            );
            assert_eq!(
                validate(&baseline.pdf)
                    .expect("validate pdfium output")
                    .page_count,
                input_pages,
                "{label}/{case_label}: validated pdfium page count"
            );

            let pure_text = extract_text_pages(&pure.pdf).expect("extract pure output");
            let baseline_text = extract_text_pages(&baseline.pdf).expect("extract pdfium output");
            let input_words = words(&input_text[0]);
            let removed_by_pdfium: BTreeSet<String> = input_words
                .difference(&words(&baseline_text[0]))
                .cloned()
                .collect();
            let pure_words = words(&pure_text[0]);
            let leaked: Vec<&String> = removed_by_pdfium.intersection(&pure_words).collect();

            assert!(
                leaked.is_empty(),
                "{label}/{case_label}: pure output retained {leaked:?}"
            );
            compared_pdfs.insert(label.clone());
            comparison_cases += 1;
        }
    }

    let pure_failed_closed_cases: usize = pure_failed_closed.values().map(Vec::len).sum();
    eprintln!(
        "redact cross-validation: corpus_pdfs={corpus_size}, compared_pdfs={}, comparison_cases={comparison_cases}, pure_failed_closed_pdfs={} {:?}, pure_failed_closed_cases={pure_failed_closed_cases}, pdfium_failed_cases={} {:?}",
        compared_pdfs.len(),
        pure_failed_closed.len(),
        pure_failed_closed,
        pdfium_failed.len(),
        pdfium_failed,
    );

    assert_eq!(
        corpus_size, 10,
        "cross-validation corpus unexpectedly changed"
    );
    assert_eq!(compared_pdfs.len(), 8, "unexpected comparable PDF count");
    assert_eq!(comparison_cases, 16, "each supported PDF needs two cases");
    assert_eq!(
        pure_failed_closed.len(),
        2,
        "only the deliberate Type3 and non-Identity CMap fixtures should fail closed"
    );
    assert_eq!(
        pure_failed_closed_cases, 4,
        "each deliberate unsupported PDF must fail closed for both cases"
    );
    assert!(
        pure_failed_closed
            .keys()
            .any(|label| label == "generated/type3"),
        "Type3 fixture did not fail closed: {pure_failed_closed:?}"
    );
    assert!(
        pure_failed_closed
            .keys()
            .any(|label| label == "generated/non-identity-cmap"),
        "non-Identity CMap fixture did not fail closed: {pure_failed_closed:?}"
    );
    assert!(
        pdfium_failed.is_empty(),
        "pdfium cross-validation failures: {pdfium_failed:?}"
    );
    assert_eq!(
        comparison_cases + pure_failed_closed_cases + pdfium_failed.len(),
        corpus_size * cases.len(),
        "every PDF/case pair must be compared or explicitly fail closed"
    );
}

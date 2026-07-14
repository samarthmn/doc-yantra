//! Integration tests for `pdf_core::assembly` — every public function is
//! exercised against real output bytes, reloaded with lopdf.

#![cfg(feature = "pdfium")]

use std::collections::BTreeSet;

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

fn assert_stable_code<T: std::fmt::Debug>(result: pdf_core::Result<T>, expected: &str) {
    match result {
        Err(error) => assert_eq!(error.stable_code(), expected, "unexpected error: {error}"),
        Ok(_) => panic!("expected {expected}, got success"),
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

fn collision_form_pdf(source: &str, javascript: bool) -> Vec<u8> {
    collision_form_pdf_with_property_options(source, javascript, "MC0", false, false)
}

fn collision_form_pdf_with_property_options(
    source: &str,
    javascript: bool,
    property_name: &str,
    inline_bdc: bool,
    short_dp: bool,
) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let source_object = || Object::string_literal(source);
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => if source == "A" { "Helvetica" } else { "Courier" },
        "DYSource" => source_object(),
    });
    let xobject_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "Resources" => Dictionary::new(),
            "DYSource" => source_object(),
        },
        b"0 0 m 10 10 l S".to_vec(),
    ));
    let ext_gstate_id = doc.add_object(dictionary! {
        "Type" => "ExtGState",
        "CA" => if source == "A" { 0.25 } else { 0.75 },
        "ca" => if source == "A" { 0.25 } else { 0.75 },
        "DYSource" => source_object(),
    });
    let pattern_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "Pattern",
            "PatternType" => 1,
            "PaintType" => 1,
            "TilingType" => 1,
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "XStep" => 10,
            "YStep" => 10,
            "Resources" => Dictionary::new(),
            "DYSource" => source_object(),
        },
        b"0 0 10 10 re f".to_vec(),
    ));
    let function_id = doc.add_object(dictionary! {
        "FunctionType" => 2,
        "Domain" => vec![0.into(), 1.into()],
        "C0" => vec![0.into(), 0.into(), 0.into()],
        "C1" => vec![1.into(), 1.into(), 1.into()],
        "N" => 1,
    });
    let shading_id = doc.add_object(dictionary! {
        "ShadingType" => 2,
        "ColorSpace" => "DeviceRGB",
        "Coords" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        "Function" => function_id,
        "Extend" => vec![true.into(), true.into()],
        "DYSource" => source_object(),
    });
    let profile_id = doc.add_object(Stream::new(
        dictionary! {
            "N" => if source == "A" { 3 } else { 4 },
            "Alternate" => if source == "A" { "DeviceRGB" } else { "DeviceCMYK" },
            "DYSource" => source_object(),
        },
        vec![0; 16],
    ));
    let color_space_id = doc.add_object(Object::Array(vec![
        Object::Name(b"ICCBased".to_vec()),
        Object::Reference(profile_id),
    ]));
    let property_id = doc.add_object(dictionary! {
        "Type" => "Metadata",
        "DYSource" => source_object(),
    });
    let bdc_property = if inline_bdc {
        Object::Dictionary(dictionary! { "MCID" => 0 })
    } else {
        Object::Name(property_name.as_bytes().to_vec())
    };
    let dp_operands = if short_dp {
        vec![Object::Name(b"Span".to_vec())]
    } else {
        vec![
            Object::Name(b"Span".to_vec()),
            Object::Name(property_name.as_bytes().to_vec()),
        ]
    };
    let appearance = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new("BDC", vec![Object::Name(b"Span".to_vec()), bdc_property]),
            Operation::new("EMC", vec![]),
            Operation::new("DP", dp_operands),
            Operation::new("gs", vec!["GS1".into()]),
            Operation::new("CS", vec!["CS1".into()]),
            Operation::new("cs", vec!["Pattern".into()]),
            Operation::new("scn", vec!["P1".into()]),
            Operation::new("sh", vec!["Sh1".into()]),
            Operation::new("Do", vec!["Im1".into()]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Tj", vec![Object::string_literal(source)]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
        ],
    };
    let appearance_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 200.into(), 30.into()],
        },
        appearance.encode().expect("encode appearance"),
    ));
    let mut field = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "T" => Object::string_literal(format!("field{source}")),
        "V" => Object::string_literal(source),
        "Rect" => vec![100.into(), 600.into(), 300.into(), 630.into()],
        "AP" => dictionary! { "N" => appearance_id },
    };
    if source == "A" {
        field.set("DA", Object::string_literal("/F1 12 Tf 0 g"));
    }
    if javascript {
        field.set(
            "AA",
            dictionary! {
                "K" => dictionary! {
                    "S" => "JavaScript",
                    "JS" => Object::string_literal("event.value = event.value"),
                },
            },
        );
    }
    let field_id = doc.add_object(field);
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Annots" => vec![Object::Reference(field_id)],
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
        "Fields" => vec![Object::Reference(field_id)],
        "DA" => Object::string_literal("/F1 12 Tf 0 g"),
        "DR" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "Im1" => xobject_id },
            "ExtGState" => dictionary! { "GS1" => ext_gstate_id },
            "Pattern" => dictionary! { "P1" => pattern_id },
            "Shading" => dictionary! { "Sh1" => shading_id },
            "ColorSpace" => dictionary! { "CS1" => color_space_id },
            "Properties" => dictionary! { property_name => property_id },
        },
    });
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "AcroForm" => acroform_id,
    });
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save collision form PDF");
    bytes
}

fn collision_form_pdf_with_opaque_appearance(source: &str) -> Vec<u8> {
    let mut doc = Document::load_mem(&collision_form_pdf(source, false)).expect("load form");
    let acroform = acroform_dict(&doc);
    let field_id = acroform
        .get(b"Fields")
        .and_then(Object::as_array)
        .expect("Fields")[0]
        .as_reference()
        .expect("field reference");
    let appearance_id = doc
        .get_dictionary(field_id)
        .expect("field")
        .get(b"AP")
        .and_then(Object::as_dict)
        .expect("AP")
        .get(b"N")
        .and_then(Object::as_reference)
        .expect("normal appearance");
    let stream = doc
        .get_object_mut(appearance_id)
        .expect("appearance")
        .as_stream_mut()
        .expect("appearance stream");
    stream.dict.set("Filter", "DCTDecode");
    stream.set_content(b"opaque viewer-managed appearance".to_vec());
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .expect("save opaque appearance form");
    bytes
}

fn collision_form_pdf_with_procset(source: &str, names: &[&str]) -> Vec<u8> {
    let mut doc = Document::load_mem(&collision_form_pdf(source, false)).expect("load form");
    let acroform_id = doc
        .catalog()
        .expect("catalog")
        .get(b"AcroForm")
        .and_then(Object::as_reference)
        .expect("AcroForm reference");
    let dr = doc
        .get_dictionary_mut(acroform_id)
        .expect("AcroForm")
        .get_mut(b"DR")
        .and_then(Object::as_dict_mut)
        .expect("DR");
    dr.set(
        "ProcSet",
        Object::Array(
            names
                .iter()
                .map(|name| Object::Name(name.as_bytes().to_vec()))
                .collect(),
        ),
    );
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save form with ProcSet");
    bytes
}

fn annotation_bomb_pdf(entries: usize) -> Vec<u8> {
    let mut doc = Document::load_mem(&sample_pdf(&["annotation bomb"])).expect("load sample");
    let page_id = doc.get_pages()[&1];
    let annotation_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Text",
        "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
    });
    doc.get_dictionary_mut(page_id)
        .expect("page")
        .set("Annots", vec![Object::Reference(annotation_id); entries]);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save annotation bomb");
    bytes
}

fn resource_source(doc: &Document, object: &Object) -> Vec<u8> {
    match doc.dereference(object).expect("resource dereference").1 {
        Object::Dictionary(dict) => dict
            .get(b"DYSource")
            .and_then(Object::as_str)
            .expect("dictionary source marker")
            .to_vec(),
        Object::Stream(stream) => stream
            .dict
            .get(b"DYSource")
            .and_then(Object::as_str)
            .expect("stream source marker")
            .to_vec(),
        Object::Array(array) => {
            let profile = array.get(1).expect("ICCBased profile");
            resource_source(doc, profile)
        }
        other => panic!("unexpected resource object: {other:?}"),
    }
}

fn operation_resource(operation: &Operation) -> Option<(&[u8], &[u8])> {
    let category = match operation.operator.as_str() {
        "Tf" => b"Font".as_slice(),
        "Do" => b"XObject".as_slice(),
        "gs" => b"ExtGState".as_slice(),
        "scn" | "SCN" => b"Pattern".as_slice(),
        "sh" => b"Shading".as_slice(),
        "cs" | "CS" => b"ColorSpace".as_slice(),
        "BDC" | "DP" => b"Properties".as_slice(),
        _ => return None,
    };
    let operand = if matches!(operation.operator.as_str(), "scn" | "SCN") {
        operation.operands.last()
    } else if matches!(operation.operator.as_str(), "BDC" | "DP") {
        operation.operands.get(1)
    } else {
        operation.operands.first()
    }?;
    let name = operand.as_name().ok()?;
    if category == b"ColorSpace" && name == b"Pattern" {
        return None;
    }
    Some((category, name))
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

/// Two-page PDF carrying every navigation structure merge must preserve.
fn navigation_pdf(source: &str, closed: bool) -> Vec<u8> {
    let mut doc = Document::load_mem(&sample_pdf(&[
        &format!("{source} page one"),
        &format!("{source} page two"),
    ]))
    .expect("load sample");
    let pages = doc.get_pages();
    let first_page = pages[&1];
    let second_page = pages[&2];

    let named_destination = Object::Array(vec![
        Object::Reference(second_page),
        Object::Name(b"Fit".to_vec()),
    ]);
    let dests_tree_id = doc.add_object(dictionary! {
        "Names" => vec![Object::string_literal("shared"), named_destination.clone()],
    });
    let names_id = doc.add_object(dictionary! { "Dests" => dests_tree_id });

    let child_id = doc.new_object_id();
    let top_id = doc.new_object_id();
    let outlines_id = doc.new_object_id();
    doc.objects.insert(
        child_id,
        Object::Dictionary(dictionary! {
            "Title" => Object::string_literal(format!("{source} child")),
            "Parent" => top_id,
            "Dest" => vec![Object::Reference(second_page), Object::Name(b"Fit".to_vec())],
        }),
    );
    doc.objects.insert(
        top_id,
        Object::Dictionary(dictionary! {
            "Title" => Object::string_literal(format!("{source} top")),
            "Parent" => outlines_id,
            "First" => child_id,
            "Last" => child_id,
            "Count" => if closed { -1 } else { 1 },
            "A" => dictionary! {
                "S" => "GoTo",
                "D" => Object::string_literal("shared"),
            },
        }),
    );
    doc.objects.insert(
        outlines_id,
        Object::Dictionary(dictionary! {
            "Type" => "Outlines",
            "First" => top_id,
            "Last" => top_id,
            "Count" => 1,
        }),
    );

    let direct_link_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => vec![10.into(), 10.into(), 100.into(), 30.into()],
        "A" => dictionary! {
            "S" => "GoTo",
            "D" => vec![Object::Reference(second_page), Object::Name(b"Fit".to_vec())],
        },
    });
    let named_link_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => vec![110.into(), 10.into(), 200.into(), 30.into()],
        "Dest" => Object::string_literal("shared"),
    });
    doc.get_dictionary_mut(first_page).expect("page").set(
        "Annots",
        vec![
            Object::Reference(direct_link_id),
            Object::Reference(named_link_id),
        ],
    );

    let style = if source == "A" { "r" } else { "D" };
    let prefix = format!("{source}-");
    let labels_id = doc.add_object(dictionary! {
        "Nums" => vec![0.into(), Object::Dictionary(dictionary! {
            "S" => style,
            "P" => Object::string_literal(prefix),
        })],
    });

    let catalog = doc.catalog_mut().expect("catalog");
    catalog.set("Names", names_id);
    // Also exercise legacy /Dests alongside the name tree.
    catalog.set(
        "Dests",
        Object::Dictionary(dictionary! { format!("legacy-{source}") => named_destination }),
    );
    catalog.set("Outlines", outlines_id);
    catalog.set("PageLabels", labels_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save navigation PDF");
    bytes
}

fn deep_outline_pdf(depth: usize) -> Vec<u8> {
    let mut doc = Document::load_mem(&sample_pdf(&["deep outline"])).expect("load sample");
    let outline_root_id = doc.new_object_id();
    let item_ids: Vec<ObjectId> = (0..depth).map(|_| doc.new_object_id()).collect();
    for (index, item_id) in item_ids.iter().copied().enumerate() {
        let parent_id = if index == 0 {
            outline_root_id
        } else {
            item_ids[index - 1]
        };
        let mut item = dictionary! {
            "Title" => Object::string_literal(format!("level {index}")),
            "Parent" => parent_id,
        };
        if let Some(child_id) = item_ids.get(index + 1) {
            item.set("First", *child_id);
            item.set("Last", *child_id);
            item.set("Count", 1);
        }
        doc.objects.insert(item_id, Object::Dictionary(item));
    }
    doc.objects.insert(
        outline_root_id,
        Object::Dictionary(dictionary! {
            "Type" => "Outlines",
            "First" => item_ids[0],
            "Last" => item_ids[0],
            "Count" => 1,
        }),
    );
    doc.catalog_mut()
        .expect("catalog")
        .set("Outlines", outline_root_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save deep outline PDF");
    bytes
}

fn cyclic_page_tree_pdf() -> Vec<u8> {
    let mut doc = Document::load_mem(&sample_pdf(&["valid before cycle"])).expect("load sample");
    let page_id = doc.get_pages()[&1];
    let pages_id = doc
        .catalog()
        .expect("catalog")
        .get(b"Pages")
        .and_then(Object::as_reference)
        .expect("Pages");
    doc.get_dictionary_mut(pages_id)
        .expect("Pages dictionary")
        .set(
            "Kids",
            vec![Object::Reference(page_id), Object::Reference(pages_id)],
        );
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save cyclic page tree");
    bytes
}

fn cyclic_fields_pdf() -> Vec<u8> {
    let mut doc =
        Document::load_mem(&form_pdf("cycle", "value", "F1", false)).expect("load form PDF");
    let acroform = acroform_dict(&doc);
    let field_id = acroform
        .get(b"Fields")
        .and_then(Object::as_array)
        .expect("Fields")[0]
        .as_reference()
        .expect("field reference");
    doc.get_dictionary_mut(field_id)
        .expect("field")
        .set("Kids", vec![Object::Reference(field_id)]);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save cyclic field tree");
    bytes
}

fn huge_destination_name_pdf(name_len: usize) -> Vec<u8> {
    let mut doc = Document::load_mem(&sample_pdf(&["huge destination name"])).expect("load sample");
    let page_id = doc.get_pages()[&1];
    let dests_id = doc.add_object(dictionary! {
        "Names" => vec![
            Object::string_literal(vec![b'n'; name_len]),
            Object::Array(vec![Object::Reference(page_id), Object::Name(b"Fit".to_vec())]),
        ],
    });
    let names_id = doc.add_object(dictionary! { "Dests" => dests_id });
    doc.catalog_mut().expect("catalog").set("Names", names_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .expect("save huge destination name PDF");
    bytes
}

fn page_label_bomb_pdf(entries: usize) -> Vec<u8> {
    let mut doc = Document::load_mem(&sample_pdf(&["label bomb"])).expect("load sample");
    let mut nums = Vec::with_capacity(entries * 2);
    for _ in 0..entries {
        nums.push(Object::Integer(0));
        nums.push(Object::Dictionary(Dictionary::new()));
    }
    let labels_id = doc.add_object(dictionary! { "Nums" => nums });
    doc.catalog_mut()
        .expect("catalog")
        .set("PageLabels", labels_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save page-label bomb");
    bytes
}

fn resolved_dict<'a>(doc: &'a Document, object: &'a Object) -> &'a Dictionary {
    match doc.dereference(object).expect("dereference").1 {
        Object::Dictionary(dict) => dict,
        other => panic!("expected dictionary, got {other:?}"),
    }
}

fn destination_page(doc: &Document, object: &Object) -> ObjectId {
    let resolved = doc.dereference(object).expect("destination dereference").1;
    let array = match resolved {
        Object::Array(array) => array,
        Object::Dictionary(dict) => dict
            .get(b"D")
            .expect("destination D")
            .as_array()
            .expect("D array"),
        Object::String(name, _) | Object::Name(name) => {
            let names = resolved_dict(
                doc,
                doc.catalog()
                    .expect("catalog")
                    .get(b"Names")
                    .expect("Names"),
            );
            let dests = resolved_dict(doc, names.get(b"Dests").expect("Dests"));
            let entries = dests
                .get(b"Names")
                .and_then(Object::as_array)
                .expect("destination entries");
            let destination = entries
                .chunks_exact(2)
                .find(|pair| pair[0].as_str().ok() == Some(name.as_slice()))
                .map(|pair| &pair[1])
                .expect("named destination resolves");
            return destination_page(doc, destination);
        }
        other => panic!("expected destination array/dict, got {other:?}"),
    };
    array[0].as_reference().expect("destination page reference")
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
fn merge_renames_and_rebinds_all_colliding_acroform_resource_categories() {
    let a = collision_form_pdf("A", false);
    let b = collision_form_pdf("B", false);
    let merged = merge(&[&a, &b]).expect("merge colliding forms");
    let doc = load(&merged);
    let acroform = acroform_dict(&doc);
    let dr = resolved_dict(&doc, acroform.get(b"DR").expect("DR"));
    let categories: [(&[u8], &[u8]); 7] = [
        (b"Font", b"F1"),
        (b"XObject", b"Im1"),
        (b"ExtGState", b"GS1"),
        (b"Pattern", b"P1"),
        (b"Shading", b"Sh1"),
        (b"ColorSpace", b"CS1"),
        (b"Properties", b"MC0"),
    ];
    for (category, original_name) in categories {
        let entries = resolved_dict(&doc, dr.get(category).expect("resource category"));
        assert_eq!(
            resource_source(&doc, entries.get(original_name).expect("original binding")),
            b"A"
        );
        let mut renamed = original_name.to_vec();
        renamed.extend_from_slice(b"~2");
        assert_eq!(
            resource_source(&doc, entries.get(&renamed).expect("renamed binding")),
            b"B"
        );
    }

    let fields = acroform
        .get(b"Fields")
        .and_then(Object::as_array)
        .expect("Fields");
    for field_object in fields {
        let field = resolved_dict(&doc, field_object);
        let source = field
            .get(b"T")
            .and_then(Object::as_str)
            .expect("field name")
            .last()
            .copied()
            .expect("source suffix");
        let suffix = if source == b'A' {
            b"".as_slice()
        } else {
            b"~2"
        };
        let da = field.get(b"DA").and_then(Object::as_str).expect("field DA");
        let da_content = Content::decode_strict(da).expect("parse DA");
        let da_font = da_content
            .operations
            .iter()
            .find(|operation| operation.operator == "Tf")
            .and_then(|operation| operation.operands.first())
            .and_then(|operand| operand.as_name().ok())
            .expect("DA font");
        assert_eq!(da_font, [b"F1".as_slice(), suffix].concat());

        let appearance = resolved_dict(&doc, field.get(b"AP").expect("AP"));
        let stream = match doc
            .dereference(appearance.get(b"N").expect("normal appearance"))
            .expect("appearance dereference")
            .1
        {
            Object::Stream(stream) => stream,
            other => panic!("normal appearance is not a stream: {other:?}"),
        };
        let resources = match stream.dict.get(b"Resources") {
            Ok(resources) => resolved_dict(&doc, resources),
            Err(_) => dr,
        };
        if source == b'B' {
            assert!(
                stream.dict.has(b"Resources"),
                "colliding appearance isolated"
            );
        }
        let content = Content::decode_strict(
            &stream
                .get_plain_content()
                .expect("decode appearance filters"),
        )
        .expect("parse appearance");
        let expected_property = [b"MC0".as_slice(), suffix].concat();
        let marked_content_properties: Vec<&[u8]> = content
            .operations
            .iter()
            .filter(|operation| matches!(operation.operator.as_str(), "BDC" | "DP"))
            .map(|operation| {
                operation
                    .operands
                    .get(1)
                    .and_then(|operand| operand.as_name().ok())
                    .expect("named BDC/DP property operand")
            })
            .collect();
        assert_eq!(
            marked_content_properties,
            vec![expected_property.as_slice(), expected_property.as_slice()]
        );
        let mut bound_categories = BTreeSet::new();
        for operation in &content.operations {
            let Some((category, name)) = operation_resource(operation) else {
                continue;
            };
            bound_categories.insert(category.to_vec());
            let entries = resolved_dict(
                &doc,
                resources
                    .get(category)
                    .expect("appearance resource category"),
            );
            assert_eq!(
                resource_source(&doc, entries.get(name).expect("appearance binding")),
                [source]
            );
            let original_name = categories
                .iter()
                .find(|(candidate, _)| *candidate == category)
                .map(|(_, name)| *name)
                .expect("known category");
            assert_eq!(name, [original_name, suffix].concat());
        }
        assert_eq!(bound_categories.len(), categories.len());
    }
}

#[test]
fn merge_preserves_distinct_acroform_property_names_without_rewriting() {
    let a = collision_form_pdf_with_property_options("A", false, "MC0", false, false);
    let b = collision_form_pdf_with_property_options("B", false, "MC1", false, false);
    let merged = merge(&[&a, &b]).expect("merge distinct property names");
    let doc = load(&merged);
    let acroform = acroform_dict(&doc);
    let dr = resolved_dict(&doc, acroform.get(b"DR").expect("DR"));
    let properties = resolved_dict(&doc, dr.get(b"Properties").expect("Properties"));
    assert_eq!(
        resource_source(&doc, properties.get(b"MC0").expect("MC0")),
        b"A"
    );
    assert_eq!(
        resource_source(&doc, properties.get(b"MC1").expect("MC1")),
        b"B"
    );
    assert!(!properties.has(b"MC0~2"));

    for field_object in acroform
        .get(b"Fields")
        .and_then(Object::as_array)
        .expect("Fields")
    {
        let field = resolved_dict(&doc, field_object);
        let source = field.get(b"V").and_then(Object::as_str).expect("value");
        let expected = if source == b"A" { b"MC0" } else { b"MC1" };
        let appearance = resolved_dict(&doc, field.get(b"AP").expect("AP"));
        let stream = doc
            .dereference(appearance.get(b"N").expect("N"))
            .expect("appearance")
            .1
            .as_stream()
            .expect("stream");
        let content = Content::decode_strict(&stream.get_plain_content().expect("content"))
            .expect("parse content");
        let names: Vec<&[u8]> = content
            .operations
            .iter()
            .filter(|operation| matches!(operation.operator.as_str(), "BDC" | "DP"))
            .map(|operation| operation.operands[1].as_name().expect("property name"))
            .collect();
        assert_eq!(names, vec![expected.as_slice(), expected.as_slice()]);
    }
}

#[test]
fn merge_unions_and_deduplicates_acroform_procsets() {
    let a = collision_form_pdf_with_procset("A", &["PDF", "Text"]);
    let b = collision_form_pdf_with_procset("B", &["Text", "ImageB"]);
    let merged = merge(&[&a, &b]).expect("merge ProcSet arrays");
    let doc = load(&merged);
    let acroform = acroform_dict(&doc);
    let dr = resolved_dict(&doc, acroform.get(b"DR").expect("DR"));
    let procset = dr
        .get(b"ProcSet")
        .and_then(Object::as_array)
        .expect("ProcSet array");
    let names: Vec<&[u8]> = procset
        .iter()
        .map(|entry| entry.as_name().expect("ProcSet name"))
        .collect();
    assert_eq!(names, vec![b"PDF".as_slice(), b"Text", b"ImageB"]);
}

#[test]
fn merge_leaves_inline_bdc_and_short_dp_operands_untouched() {
    let a = collision_form_pdf("A", false);
    let b = collision_form_pdf_with_property_options("B", false, "MC0", true, true);
    let merged = merge(&[&a, &b]).expect("merge inline and short property operands");
    let doc = load(&merged);
    let acroform = acroform_dict(&doc);
    let field = acroform
        .get(b"Fields")
        .and_then(Object::as_array)
        .expect("Fields")
        .iter()
        .map(|field| resolved_dict(&doc, field))
        .find(|field| field.get(b"V").and_then(Object::as_str).ok() == Some(b"B"))
        .expect("field B");
    let appearance = resolved_dict(&doc, field.get(b"AP").expect("AP"));
    let stream = doc
        .dereference(appearance.get(b"N").expect("N"))
        .expect("appearance")
        .1
        .as_stream()
        .expect("stream");
    let content = Content::decode_strict(&stream.get_plain_content().expect("content"))
        .expect("parse content");
    let bdc = content
        .operations
        .iter()
        .find(|operation| operation.operator == "BDC")
        .expect("BDC");
    assert!(matches!(bdc.operands.get(1), Some(Object::Dictionary(_))));
    let dp = content
        .operations
        .iter()
        .find(|operation| operation.operator == "DP")
        .expect("DP");
    assert_eq!(dp.operands, vec![Object::Name(b"Span".to_vec())]);
}

#[test]
fn merge_refuses_resource_collision_when_field_javascript_makes_rewrite_unsafe() {
    let a = collision_form_pdf("A", false);
    let b = collision_form_pdf("B", true);
    assert_stable_code(merge(&[&a, &b]), "unsupported_feature");
}

#[test]
fn merge_isolates_opaque_appearance_resources_under_original_names() {
    let a = collision_form_pdf("A", false);
    let b = collision_form_pdf_with_opaque_appearance("B");
    let merged = merge(&[&a, &b]).expect("merge opaque appearance");
    let doc = load(&merged);
    let acroform = acroform_dict(&doc);
    let field = acroform
        .get(b"Fields")
        .and_then(Object::as_array)
        .expect("Fields")
        .iter()
        .map(|field| resolved_dict(&doc, field))
        .find(|field| field.get(b"T").and_then(Object::as_str).ok() == Some(b"fieldB"))
        .expect("fieldB");
    let appearance = resolved_dict(&doc, field.get(b"AP").expect("AP"));
    let stream = match doc
        .dereference(appearance.get(b"N").expect("normal appearance"))
        .expect("appearance dereference")
        .1
    {
        Object::Stream(stream) => stream,
        other => panic!("normal appearance is not a stream: {other:?}"),
    };
    let resources = resolved_dict(
        &doc,
        stream.dict.get(b"Resources").expect("private Resources"),
    );
    for (category, name) in [
        (b"Font".as_slice(), b"F1".as_slice()),
        (b"XObject".as_slice(), b"Im1".as_slice()),
        (b"ExtGState".as_slice(), b"GS1".as_slice()),
        (b"Pattern".as_slice(), b"P1".as_slice()),
        (b"Shading".as_slice(), b"Sh1".as_slice()),
        (b"ColorSpace".as_slice(), b"CS1".as_slice()),
        (b"Properties".as_slice(), b"MC0".as_slice()),
    ] {
        let entries = resolved_dict(&doc, resources.get(category).expect("category"));
        assert_eq!(
            resource_source(&doc, entries.get(name).expect("isolated binding")),
            b"B"
        );
    }
}

#[test]
fn merge_rejects_annotation_entry_bomb_as_resource_limit() {
    let pdf = annotation_bomb_pdf(100_001);
    assert_stable_code(merge(&[&pdf]), "resource_limit");
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

#[test]
fn merge_preserves_outlines_named_destinations_links_and_page_labels() {
    let a = navigation_pdf("A", false);
    let b = navigation_pdf("B", true);
    let merged = merge(&[&a, &b]).expect("merge navigation PDFs");
    let doc = load(&merged);
    let pages = doc.get_pages();

    let catalog = doc.catalog().expect("catalog");
    let outlines = resolved_dict(&doc, catalog.get(b"Outlines").expect("Outlines"));
    assert_eq!(
        outlines.get(b"Count").and_then(Object::as_i64).unwrap(),
        3,
        "two top-level items plus source A's open child are visible"
    );
    let first_id = outlines
        .get(b"First")
        .and_then(Object::as_reference)
        .expect("first outline");
    let first = doc.get_dictionary(first_id).expect("first outline dict");
    assert_eq!(
        first.get(b"Title").and_then(Object::as_str).unwrap(),
        b"A top"
    );
    assert_eq!(first.get(b"Count").and_then(Object::as_i64).unwrap(), 1);
    assert_eq!(
        destination_page(
            &doc,
            first
                .get(b"A")
                .expect("action")
                .as_dict()
                .expect("action dict")
                .get(b"D")
                .expect("action destination")
        ),
        pages[&2],
        "source A named outline destination resolves to merged page 2"
    );
    let second_id = first
        .get(b"Next")
        .and_then(Object::as_reference)
        .expect("source B top outline follows source A");
    let second = doc.get_dictionary(second_id).expect("second outline dict");
    assert_eq!(
        second.get(b"Title").and_then(Object::as_str).unwrap(),
        b"B top"
    );
    assert_eq!(second.get(b"Count").and_then(Object::as_i64).unwrap(), -1);
    assert_eq!(
        destination_page(
            &doc,
            second
                .get(b"A")
                .expect("action")
                .as_dict()
                .expect("action dict")
                .get(b"D")
                .expect("action destination")
        ),
        pages[&4],
        "source B renamed outline destination resolves to merged page 4"
    );
    let child_id = second
        .get(b"First")
        .and_then(Object::as_reference)
        .expect("nested child");
    let child = doc.get_dictionary(child_id).expect("child");
    assert_eq!(
        child.get(b"Title").and_then(Object::as_str).unwrap(),
        b"B child"
    );
    assert_eq!(
        child.get(b"Parent").and_then(Object::as_reference).unwrap(),
        second_id
    );
    assert_eq!(
        destination_page(&doc, child.get(b"Dest").expect("direct child destination")),
        pages[&4]
    );

    let names = resolved_dict(&doc, catalog.get(b"Names").expect("Names"));
    let dest_tree = resolved_dict(&doc, names.get(b"Dests").expect("Names/Dests"));
    let entries = dest_tree
        .get(b"Names")
        .and_then(Object::as_array)
        .expect("flat destination name tree");
    let keys: Vec<Vec<u8>> = entries
        .chunks_exact(2)
        .map(|pair| pair[0].as_str().expect("destination key").to_vec())
        .collect();
    assert_eq!(
        keys,
        [
            b"legacy-A".to_vec(),
            b"legacy-B".to_vec(),
            b"shared".to_vec(),
            b"shared~2".to_vec(),
        ]
    );
    assert_eq!(destination_page(&doc, &entries[5]), pages[&2]);
    assert_eq!(destination_page(&doc, &entries[7]), pages[&4]);

    let b_first_page = doc.get_dictionary(pages[&3]).expect("B first page");
    let annots = b_first_page
        .get(b"Annots")
        .and_then(Object::as_array)
        .expect("link annotations");
    let link = resolved_dict(&doc, &annots[0]);
    let action = link
        .get(b"A")
        .and_then(Object::as_dict)
        .expect("GoTo action");
    assert_eq!(
        destination_page(&doc, action.get(b"D").expect("D")),
        pages[&4]
    );
    let named_link = resolved_dict(&doc, &annots[1]);
    assert_eq!(
        destination_page(&doc, named_link.get(b"Dest").expect("Dest")),
        pages[&4]
    );

    let labels = resolved_dict(&doc, catalog.get(b"PageLabels").expect("PageLabels"));
    let nums = labels
        .get(b"Nums")
        .and_then(Object::as_array)
        .expect("Nums");
    assert_eq!(nums[0].as_i64().unwrap(), 0);
    assert_eq!(nums[2].as_i64().unwrap(), 2, "source B label offset");
    assert_eq!(
        nums[1]
            .as_dict()
            .unwrap()
            .get(b"P")
            .and_then(Object::as_str)
            .unwrap(),
        b"A-"
    );
    assert_eq!(
        nums[3]
            .as_dict()
            .unwrap()
            .get(b"P")
            .and_then(Object::as_str)
            .unwrap(),
        b"B-"
    );
}

#[test]
fn merge_inserts_default_page_label_run_only_when_prior_custom_run_would_leak() {
    let labeled = navigation_pdf("A", false);
    let plain = sample_pdf(&["plain"]);
    let merged = merge(&[&labeled, &plain]).expect("merge labeled + plain");
    let doc = load(&merged);
    let labels = resolved_dict(
        &doc,
        doc.catalog()
            .expect("catalog")
            .get(b"PageLabels")
            .expect("PageLabels"),
    );
    let nums = labels.get(b"Nums").and_then(Object::as_array).unwrap();
    assert_eq!(nums.len(), 4, "one source run plus one needed reset");
    assert_eq!(nums[0].as_i64().unwrap(), 0);
    assert_eq!(nums[2].as_i64().unwrap(), 2);
    assert!(nums[3].as_dict().expect("default label dict").is_empty());
}

#[test]
fn merge_rejects_dangling_outline_destination_with_stable_code() {
    let mut broken = Document::load_mem(&navigation_pdf("broken", false)).expect("load");
    let outlines = broken
        .catalog()
        .expect("catalog")
        .get(b"Outlines")
        .and_then(Object::as_reference)
        .expect("outlines");
    let top = broken
        .get_dictionary(outlines)
        .expect("root")
        .get(b"First")
        .and_then(Object::as_reference)
        .expect("top");
    broken
        .get_dictionary_mut(top)
        .expect("top")
        .get_mut(b"A")
        .expect("action")
        .as_dict_mut()
        .expect("action dict")
        .set("D", Object::string_literal("missing"));
    let mut bytes = Vec::new();
    broken.save_to(&mut bytes).expect("save broken PDF");

    let error = match merge(&[&bytes]) {
        Err(error) => error,
        Ok(_) => panic!("dangling outline merge unexpectedly succeeded"),
    };
    assert!(matches!(error, PdfCoreError::InvalidInput(_)));
    assert_eq!(error.stable_code(), "invalid_input");
}

#[test]
fn merge_rejects_outline_depth_over_limit_as_resource_limit() {
    let pdf = deep_outline_pdf(65);
    assert_stable_code(merge(&[&pdf]), "resource_limit");
}

#[test]
fn merge_rejects_page_tree_cycle_even_after_a_valid_page() {
    let pdf = cyclic_page_tree_pdf();
    assert_stable_code(merge(&[&pdf]), "invalid_input");
}

#[test]
fn merge_rejects_cyclic_field_kids() {
    let pdf = cyclic_fields_pdf();
    assert_stable_code(merge(&[&pdf]), "invalid_input");
}

#[test]
fn merge_rejects_aggregate_destination_name_bytes_over_limit() {
    let pdf = huge_destination_name_pdf(4 * 1024 * 1024 + 1);
    assert_stable_code(merge(&[&pdf]), "resource_limit");
}

#[test]
fn merge_rejects_page_label_entry_bomb_as_resource_limit() {
    let pdf = page_label_bomb_pdf(10_001);
    assert_stable_code(merge(&[&pdf]), "resource_limit");
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

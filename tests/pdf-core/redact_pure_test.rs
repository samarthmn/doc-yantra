//! Integration and pdfium cross-validation tests for Zone-1 redaction.

#![cfg(feature = "pdfium")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, Stream, StringFormat, dictionary};
use serial_test::serial;

use pdf_core::error::PdfCoreError;
use pdf_core::extract::{extract_text, extract_text_pages, search};
use pdf_core::meta::{self, page_count, page_sizes, validate};
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

fn embedded_cmap_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "EmbeddedCMapFixture",
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
        "BaseFont" => "EmbeddedCMapFixture",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 1000,
        "W" => vec![
            1.into(),
            Object::Array(vec![500.into(), 600.into(), 700.into(), 800.into()]),
        ],
    });
    let encoding_id = doc.add_object(Stream::new(
        dictionary! {},
        br#"begincmap
4 begincodespacerange
<01> <01>
<0201> <0201>
<030001> <030001>
<04000001> <04000001>
endcodespacerange
1 begincidchar
<01> 1
endcidchar
3 begincidrange
<0201> <0201> 2
<030001> <030001> 3
<04000001> <04000001> 4
endcidrange
endcmap"#
            .to_vec(),
    ));
    let to_unicode_id = doc.add_object(Stream::new(
        dictionary! {},
        br#"begincmap
4 begincodespacerange
<01> <01>
<0201> <0201>
<030001> <030001>
<04000001> <04000001>
endcodespacerange
2 beginbfchar
<01> <0041>
<04000001> <006600660069>
endbfchar
1 beginbfrange
<0201> <0201> <0042>
endbfrange
1 beginbfrange
<030001> <030001> [<00650301>]
endbfrange
endcmap"#
            .to_vec(),
    ));
    let type0_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "EmbeddedCMapFixture",
        "Encoding" => encoding_id,
        "ToUnicode" => to_unicode_id,
        "DescendantFonts" => vec![descendant_id.into()],
    });
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F0".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new(
            "Tj",
            vec![Object::String(
                vec![1, 2, 1, 3, 0, 1, 4, 0, 0, 1],
                StringFormat::Hexadecimal,
            )],
        ),
        Operation::new("ET", vec![]),
    ];
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }
            .encode()
            .expect("encode CMap content"),
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

fn fixed_two_byte_cmap_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "TwoByteCMapFixture",
        "Flags" => 4,
        "FontBBox" => vec![0.into(), (-200).into(), 500.into(), 800.into()],
        "ItalicAngle" => 0,
        "Ascent" => 800,
        "Descent" => -200,
        "CapHeight" => 700,
        "StemV" => 80,
    });
    let descendant_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "TwoByteCMapFixture",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 500,
    });
    let encoding_id = doc.add_object(Stream::new(
        dictionary! {},
        br#"begincmap
1 begincodespacerange
<0001> <0005>
endcodespacerange
1 begincidrange
<0001> <0005> 1
endcidrange
endcmap"#
            .to_vec(),
    ));
    let to_unicode_id = doc.add_object(Stream::new(
        dictionary! {},
        br#"begincmap
1 begincodespacerange
<0001> <0005>
endcodespacerange
1 beginbfrange
<0001> <0005> <0041>
endbfrange
endcmap"#
            .to_vec(),
    ));
    let type0_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "TwoByteCMapFixture",
        "Encoding" => encoding_id,
        "ToUnicode" => to_unicode_id,
        "DescendantFonts" => vec![descendant_id.into()],
    });
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F0".into(), 20.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::String(
                        vec![0, 1, 0, 2, 0, 3, 0, 4, 0, 5],
                        StringFormat::Hexadecimal,
                    )],
                ),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .expect("encode two-byte CMap content"),
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

fn embedded_vertical_cmap_pdf(dictionary_wmode: i64, program_wmode: i64) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "VerticalCMapFixture",
        "Flags" => 4,
        "FontBBox" => vec![0.into(), (-200).into(), 500.into(), 800.into()],
        "ItalicAngle" => 0,
        "Ascent" => 800,
        "Descent" => -200,
        "CapHeight" => 700,
        "StemV" => 80,
    });
    let descendant_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "VerticalCMapFixture",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 500,
        "DW2" => vec![880.into(), (-1000).into()],
        "W2" => vec![
            1.into(),
            Object::Array(vec![
                (-1000).into(), 250.into(), 880.into(),
                (-900).into(), 300.into(), 850.into(),
                (-1100).into(), 200.into(), 900.into(),
            ]),
            4.into(), 5.into(), (-1000).into(), 250.into(), 880.into(),
        ],
    });
    let encoding = format!(
        "begincmap\n/WMode {program_wmode} def\n1 begincodespacerange\n<0001> <0005>\nendcodespacerange\n1 begincidrange\n<0001> <0005> 1\nendcidrange\nendcmap"
    );
    let encoding_id = doc.add_object(Stream::new(
        dictionary! { "WMode" => dictionary_wmode },
        encoding.into_bytes(),
    ));
    let to_unicode_id = doc.add_object(Stream::new(
        dictionary! {},
        br#"begincmap
1 begincodespacerange
<0001> <0005>
endcodespacerange
1 beginbfrange
<0001> <0005> <0041>
endbfrange
endcmap"#
            .to_vec(),
    ));
    let type0_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "VerticalCMapFixture",
        "Encoding" => encoding_id,
        "ToUnicode" => to_unicode_id,
        "DescendantFonts" => vec![descendant_id.into()],
    });
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F0".into(), 20.into()]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        300.into(),
                        700.into(),
                    ],
                ),
                Operation::new(
                    "Tj",
                    vec![Object::String(
                        vec![0, 1, 0, 2, 0, 3, 0, 4, 0, 5],
                        StringFormat::Hexadecimal,
                    )],
                ),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .expect("encode vertical CMap content"),
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

fn predefined_japan1_cmap_pdf(encoding: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "PredefinedJapan1Fixture",
        "Flags" => 4,
        "FontBBox" => vec![0.into(), (-200).into(), 500.into(), 800.into()],
        "ItalicAngle" => 0,
        "Ascent" => 800,
        "Descent" => -200,
        "CapHeight" => 700,
        "StemV" => 80,
    });
    let descendant_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "PredefinedJapan1Fixture",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Japan1"),
            "Supplement" => 7,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 500,
        "DW2" => vec![880.into(), (-1000).into()],
    });
    let type0_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "PredefinedJapan1Fixture",
        "Encoding" => Object::Name(encoding.as_bytes().to_vec()),
        "DescendantFonts" => vec![descendant_id.into()],
    });
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F0".into(), 20.into()]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        300.into(),
                        700.into(),
                    ],
                ),
                Operation::new(
                    "Tj",
                    vec![Object::String(
                        vec![0, 65, 0, 66, 0, 67, 0, 68, 0, 69],
                        StringFormat::Hexadecimal,
                    )],
                ),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .expect("encode predefined Japan1 content"),
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

fn embedded_cmap_misleading_space_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "EmbeddedCMapSpaceFixture",
        "Flags" => 4,
        "FontBBox" => vec![0.into(), (-200).into(), 500.into(), 800.into()],
        "ItalicAngle" => 0,
        "Ascent" => 800,
        "Descent" => -200,
        "CapHeight" => 700,
        "StemV" => 80,
    });
    let descendant_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "EmbeddedCMapSpaceFixture",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 500,
        "W" => vec![1.into(), Object::Array(vec![500.into(), 500.into()])],
    });
    let encoding_id = doc.add_object(Stream::new(
        dictionary! {},
        br#"begincmap
1 begincodespacerange
<01> <02>
endcodespacerange
2 begincidchar
<01> 1
<02> 2
endcidchar
endcmap"#
            .to_vec(),
    ));
    let to_unicode_id = doc.add_object(Stream::new(
        dictionary! {},
        br#"begincmap
1 begincodespacerange
<01> <02>
endcodespacerange
2 beginbfchar
<01> <0020>
<02> <0058>
endbfchar
endcmap"#
            .to_vec(),
    ));
    let type0_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "EmbeddedCMapSpaceFixture",
        "Encoding" => encoding_id,
        "ToUnicode" => to_unicode_id,
        "DescendantFonts" => vec![descendant_id.into()],
    });
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F0".into(), 20.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tw", vec![(-100).into()]),
                Operation::new(
                    "Tj",
                    vec![Object::String(vec![1, 1, 1, 2], StringFormat::Hexadecimal)],
                ),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .expect("encode misleading-space CMap content"),
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

fn identity_unicode_sequences_pdf() -> Vec<u8> {
    let mut doc = Document::load_mem(&cid_pdf("Identity-H")).expect("load Identity-H fixture");
    let to_unicode_id = doc.add_object(Stream::new(
        dictionary! {},
        br#"begincmap
1 begincodespacerange
<0001> <0002>
endcodespacerange
2 beginbfchar
<0001> <00650301>
<0002> <006600660069>
endbfchar
endcmap"#
            .to_vec(),
    ));
    for object in doc.objects.values_mut() {
        let Ok(dict) = object.as_dict_mut() else {
            continue;
        };
        if dict
            .get(b"Subtype")
            .ok()
            .and_then(|object| object.as_name().ok())
            == Some(b"Type0".as_slice())
        {
            dict.set("ToUnicode", Object::Reference(to_unicode_id));
        }
    }
    let page_id = *doc.get_pages().values().next().expect("Identity-H page");
    let content_id = doc
        .get_dictionary(page_id)
        .expect("Identity-H page dictionary")
        .get(b"Contents")
        .expect("Identity-H page content")
        .as_reference()
        .expect("Identity-H indirect content");
    let content = doc
        .get_object_mut(content_id)
        .expect("Identity-H content object")
        .as_stream_mut()
        .expect("Identity-H content stream");
    let mut operations = Content::decode(&content.content)
        .expect("decode Identity-H content")
        .operations;
    let text = operations
        .iter_mut()
        .find(|operation| operation.operator == "Tj")
        .expect("Identity-H text operation");
    text.operands[0] = Object::String(vec![0, 1, 0, 2], StringFormat::Hexadecimal);
    content.set_plain_content(
        Content { operations }
            .encode()
            .expect("encode Unicode sequences"),
    );
    let mut output = Vec::new();
    doc.save_to(&mut output)
        .expect("serialize Unicode sequence fixture");
    output
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

fn type3_d0_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let charproc_id = doc.add_object(Stream::new(
        dictionary! {},
        b"750 0 d0 1 0 0 rg 0 0 750 750 re f".to_vec(),
    ));
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "FontBBox" => vec![0.into(), 0.into(), 750.into(), 750.into()],
        "FontMatrix" => vec![0.001.into(), 0.into(), 0.into(), 0.001.into(), 0.into(), 0.into()],
        "CharProcs" => dictionary! { "square" => charproc_id },
        "Encoding" => dictionary! { "Differences" => vec![97.into(), "square".into()] },
        "FirstChar" => 97,
        "LastChar" => 97,
        "Widths" => vec![750.into()],
    });
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F3".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal("aaa")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .expect("encode Type3 d0 content"),
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

fn type3_encoding_pdf(encoding: Object) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let charproc_id = doc.add_object(Stream::new(
        dictionary! {},
        b"500 0 0 0 500 700 d1 0 0 500 700 re f".to_vec(),
    ));
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "FontBBox" => vec![0.into(), 0.into(), 500.into(), 700.into()],
        "FontMatrix" => vec![0.001.into(), 0.into(), 0.into(), 0.001.into(), 0.into(), 0.into()],
        "CharProcs" => dictionary! { "A" => charproc_id },
        "Encoding" => encoding,
        "FirstChar" => 65,
        "LastChar" => 65,
        "Widths" => vec![500.into()],
    });
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F3".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal("A")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .expect("encode Type3 encoding content"),
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

fn transformed_type3_run_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let charproc_id = doc.add_object(Stream::new(
        dictionary! {},
        b"500 0 0 0 500 700 d1 0 0 500 700 re f".to_vec(),
    ));
    let to_unicode_id = doc.add_object(Stream::new(
        dictionary! {},
        br#"begincmap
1 begincodespacerange
<61> <61>
endcodespacerange
1 beginbfchar
<61> <0061>
endbfchar
endcmap"#
            .to_vec(),
    ));
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "FontBBox" => vec![0.into(), 0.into(), 500.into(), 700.into()],
        "FontMatrix" => vec![0.002.into(), 0.into(), 0.into(), 0.002.into(), 5.into(), 10.into()],
        "CharProcs" => dictionary! { "a" => charproc_id },
        "Encoding" => dictionary! { "Differences" => vec![97.into(), "a".into()] },
        "ToUnicode" => to_unicode_id,
        "FirstChar" => 97,
        "LastChar" => 97,
        "Widths" => vec![500.into()],
    });
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        0.5.into(),
                        0.into(),
                        0.into(),
                        0.5.into(),
                        200.into(),
                        300.into(),
                    ],
                ),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F3".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 120.into()]),
                Operation::new("Tj", vec![Object::string_literal("aaaaa")]),
                Operation::new("ET", vec![]),
                Operation::new("Q", vec![]),
            ],
        }
        .encode()
        .expect("encode transformed Type3 content"),
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

fn compressed_stream(dictionary: Dictionary, content: &[u8]) -> Stream {
    let mut stream = Stream::new(dictionary, content.to_vec());
    stream.compress().expect("compress fixture stream");
    stream
}

fn image_xobject_pdf(include_second_page: bool) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let soft_mask_id = doc.add_object(compressed_stream(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        b"IMAGE-SMASK-SECRET-7e19",
    ));
    let explicit_mask_id = doc.add_object(compressed_stream(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ImageMask" => true,
            "BitsPerComponent" => 1,
        },
        b"IMAGE-MASK-SECRET-3b52",
    ));
    let image_id = doc.add_object(compressed_stream(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "SMask" => soft_mask_id,
            "Mask" => explicit_mask_id,
        },
        b"IMAGE-XOBJECT-SECRET-a4c8",
    ));
    let page_content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    100.into(),
                    0.into(),
                    0.into(),
                    100.into(),
                    72.into(),
                    650.into(),
                ],
            ),
            Operation::new("Do", vec!["ImSecret".into()]),
            Operation::new("Q", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        page_content.encode().expect("encode image page"),
    ));
    let first_page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "XObject" => dictionary! { "ImSecret" => image_id },
        },
    });
    let mut kids = vec![first_page_id.into()];
    if include_second_page {
        let second_content_id = doc.add_object(Stream::new(
            dictionary! {},
            page_content.encode().expect("encode shared image page"),
        ));
        let second_page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => second_content_id,
            "Resources" => dictionary! {
                "XObject" => dictionary! { "ImSecret" => image_id },
            },
        });
        kids.push(second_page_id.into());
    }
    finish_doc(doc, pages_id, kids)
}

fn nested_form_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let inner_id = doc.add_object(compressed_stream(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 200.into(), 50.into()],
            "Resources" => dictionary! {},
        },
        b"123.125 10 m 190 10 l 190 40 l h f % NESTED-FORM-SECRET-c921",
    ));
    let outer_id = doc.add_object(compressed_stream(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 220.into(), 60.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Inner" => inner_id },
            },
        },
        b"/Inner Do",
    ));
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        b"q 1 0 0 1 72 680 cm /Outer Do Q".to_vec(),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "XObject" => dictionary! { "Outer" => outer_id },
        },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn widget_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let appearance_id = doc.add_object(compressed_stream(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 228.into(), 50.into()],
            "Resources" => dictionary! {},
        },
        b"0 0 228 50 re f % WIDGET-AP-SECRET-5d60",
    ));
    let field_id = doc.new_object_id();
    let widget_id = doc.new_object_id();
    doc.objects.insert(
        widget_id,
        Object::Dictionary(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "Rect" => vec![72.into(), 690.into(), 300.into(), 740.into()],
        "AP" => dictionary! { "N" => appearance_id },
            "Parent" => field_id,
        }),
    );
    doc.objects.insert(
        field_id,
        Object::Dictionary(dictionary! {
            "FT" => "Tx",
            "T" => Object::string_literal("secure-field"),
            "V" => Object::string_literal("WIDGET-V-SECRET-03ef"),
            "Kids" => vec![widget_id.into()],
        }),
    );
    let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Annots" => vec![widget_id.into()],
        "Resources" => dictionary! {},
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
        "Fields" => vec![field_id.into()],
    });
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "AcroForm" => acroform_id,
    });
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize widget fixture");
    bytes
}

fn struct_tree_without_marked_content_pdf() -> Vec<u8> {
    let mut doc = Document::load_mem(&pdf_with_texts(
        &[("STRUCT-VISIBLE-SECRET-f112", 72, 720)],
        None,
    ))
    .expect("load StructTree fixture");
    let page_id = *doc.get_pages().values().next().expect("StructTree page");
    let element_id = doc.add_object(dictionary! {
        "Type" => "StructElem",
        "S" => "Span",
        "Pg" => page_id,
        "ActualText" => Object::string_literal("STRUCT-ACTUALTEXT-SECRET-8a77"),
    });
    let tree_id = doc.add_object(dictionary! {
        "Type" => "StructTreeRoot",
        "K" => vec![element_id.into()],
    });
    doc.catalog_mut()
        .expect("StructTree catalog")
        .set("StructTreeRoot", tree_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .expect("serialize StructTree fixture");
    bytes
}

fn shading_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let shading_id = doc.add_object(dictionary! {
        "ShadingType" => 2,
        "ColorSpace" => "DeviceRGB",
        "Coords" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Function" => dictionary! {
            "FunctionType" => 2,
            "Domain" => vec![0.into(), 1.into()],
            "C0" => vec![0.into(), 0.into(), 0.into()],
            "C1" => vec![1.into(), 1.into(), 1.into()],
            "N" => 1,
            "AuditMarker" => Object::string_literal("SHADING-DICT-SECRET-61aa"),
        },
        "Extend" => vec![true.into(), true.into()],
    });
    let content_id = doc.add_object(Stream::new(dictionary! {}, b"/ShadeSecret sh".to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Shading" => dictionary! { "ShadeSecret" => shading_id },
        },
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

fn form_operations(pdf: &[u8]) -> Vec<Operation> {
    let doc = Document::load_mem(pdf).expect("load PDF");
    let form = doc
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .find(|stream| {
            stream
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|object| object.as_name().ok())
                == Some(b"Form".as_slice())
        })
        .expect("Form XObject survives");
    Content::decode(&form.decompressed_content().expect("decode Form XObject"))
        .expect("parse Form XObject content")
        .operations
}

fn assert_operator_pair(operations: &[Operation], first: &str, second: &str) {
    assert!(
        operations
            .windows(2)
            .any(|window| window[0].operator == first && window[1].operator == second),
        "expected {first} followed by {second}, got {:?}",
        operations
            .iter()
            .map(|operation| operation.operator.as_str())
            .collect::<Vec<_>>()
    );
}

fn pixel_is_dark(pdf: &[u8], x: u32, y_from_bottom: u32) -> bool {
    let png = render_page(pdf, 0, &RenderOptions::default()).expect("rasterize clipping fixture");
    let image = image::load_from_memory(&png)
        .expect("decode clipping fixture")
        .to_luma8();
    image.get_pixel(x, image.height() - 1 - y_from_bottom)[0] < 64
}

fn clipping_form_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let form_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
            "Resources" => dictionary! {},
        },
        Content {
            operations: vec![
                Operation::new("re", vec![0.into(), 0.into(), 100.into(), 100.into()]),
                Operation::new("W", vec![]),
                Operation::new("S", vec![]),
                Operation::new("re", vec![150.into(), 150.into(), 50.into(), 50.into()]),
                Operation::new("f", vec![]),
            ],
        }
        .encode()
        .expect("encode clipping form"),
    ));
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        100.into(),
                        400.into(),
                    ],
                ),
                Operation::new("Do", vec!["ClipForm".into()]),
                Operation::new("Q", vec![]),
            ],
        }
        .encode()
        .expect("encode clipping form page"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "XObject" => dictionary! { "ClipForm" => form_id },
        },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn inherited_state_clipping_form_pdf(
    page_state: Vec<Operation>,
    form_body: Vec<Operation>,
) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let mut form_operations = vec![
        Operation::new("re", vec![0.into(), 0.into(), 300.into(), 100.into()]),
        Operation::new("W", vec![]),
        Operation::new("n", vec![]),
    ];
    form_operations.extend(form_body);
    let form_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 300.into(), 100.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
        },
        Content {
            operations: form_operations,
        }
        .encode()
        .expect("encode inherited-state clipping form"),
    ));
    let mut page_operations = page_state;
    page_operations.extend([
        Operation::new("q", vec![]),
        Operation::new(
            "cm",
            vec![
                1.into(),
                0.into(),
                0.into(),
                1.into(),
                100.into(),
                400.into(),
            ],
        ),
        Operation::new("Do", vec!["ClipForm".into()]),
        Operation::new("Q", vec![]),
    ]);
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: page_operations,
        }
        .encode()
        .expect("encode inherited-state clipping page"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "ClipForm" => form_id },
        },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn clipped_shading_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let shading_id = doc.add_object(dictionary! {
        "ShadingType" => 2,
        "ColorSpace" => "DeviceRGB",
        "Coords" => vec![20.into(), 20.into(), 70.into(), 70.into()],
        "Function" => dictionary! {
            "FunctionType" => 2,
            "Domain" => vec![0.into(), 1.into()],
            "C0" => vec![0.into(), 0.into(), 0.into()],
            "C1" => vec![1.into(), 0.into(), 0.into()],
            "N" => 1,
            "AuditMarker" => Object::string_literal("CLIPPED-SHADING-KEEP-a8d3"),
        },
        "Extend" => vec![true.into(), true.into()],
    });
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new("re", vec![20.into(), 20.into(), 50.into(), 50.into()]),
                Operation::new("W", vec![]),
                Operation::new("n", vec![]),
                Operation::new("sh", vec!["ShadeClipped".into()]),
                Operation::new("Q", vec![]),
            ],
        }
        .encode()
        .expect("encode clipped shading content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Shading" => dictionary! { "ShadeClipped" => shading_id },
        },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn disjoint_clipped_graphics_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let image_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "AuditMarker" => Object::string_literal("CLIPPED-IMAGE-KEEP-b1e4"),
        },
        vec![255, 0, 0],
    ));
    let form_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 50.into(), 50.into()],
            "Resources" => dictionary! {},
            "AuditMarker" => Object::string_literal("CLIPPED-FORM-KEEP-c7a2"),
        },
        b"0 0 50 50 re f".to_vec(),
    ));
    let mut content = b"q\n20 20 50 50 re W n\n400 400 50 50 re f\nq\n50 0 0 50 400 400 cm\nBI /W 1 /H 1 /CS /RGB /BPC 8\nID\n"
        .to_vec();
    content.extend_from_slice(&[0, 255, 0]);
    content.extend_from_slice(
        b"\nEI\nQ\nq\n50 0 0 50 400 400 cm\n/ImClipped Do\nQ\nq\n1 0 0 1 400 400 cm\n/FormClipped Do\nQ\nQ",
    );
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "XObject" => dictionary! {
                "ImClipped" => image_id,
                "FormClipped" => form_id,
            },
        },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn nested_clipping_form_resources_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let inherited_font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let local_font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica-Bold",
    });
    let inner_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 250.into(), 160.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "FLocal" => local_font_id },
            },
        },
        Content {
            operations: vec![
                Operation::new("re", vec![0.into(), 0.into(), 250.into(), 160.into()]),
                Operation::new("W", vec![]),
                Operation::new("n", vec![]),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["FParent".into(), 24.into()]),
                Operation::new("Td", vec![20.into(), 20.into()]),
                Operation::new("Tj", vec![Object::string_literal("NESTED-RESOURCE-SECRET")]),
                Operation::new("ET", vec![]),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["FLocal".into(), 20.into()]),
                Operation::new("Td", vec![20.into(), 120.into()]),
                Operation::new("Tj", vec![Object::string_literal("NESTED-RESOURCE-KEEP")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .expect("encode inner clipping form"),
    ));
    let outer_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Inner" => inner_id },
            },
        },
        Content {
            operations: vec![
                Operation::new("re", vec![0.into(), 0.into(), 300.into(), 200.into()]),
                Operation::new("W", vec![]),
                Operation::new("n", vec![]),
                Operation::new("Do", vec!["Inner".into()]),
            ],
        }
        .encode()
        .expect("encode outer clipping form"),
    ));
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        100.into(),
                        400.into(),
                    ],
                ),
                Operation::new("Do", vec!["Outer".into()]),
                Operation::new("Q", vec![]),
            ],
        }
        .encode()
        .expect("encode nested clipping page"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! { "FParent" => inherited_font_id },
            "XObject" => dictionary! { "Outer" => outer_id },
        },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

fn page_show_count(pdf: &[u8]) -> usize {
    page_operations(pdf)
        .iter()
        .filter(|operation| matches!(operation.operator.as_str(), "Tj" | "TJ" | "'" | "\""))
        .count()
}

/// Every glyph byte still painted by the page, in show order.
///
/// Run-level rewriting keeps the surviving glyphs of a partially covered run, so
/// asserting on the exact remaining bytes is stronger than counting operators: it
/// proves both that the covered glyphs are gone and that the untouched ones stayed.
fn page_shown_bytes(pdf: &[u8]) -> Vec<u8> {
    let mut shown = Vec::new();
    for operation in page_operations(pdf) {
        match operation.operator.as_str() {
            "Tj" | "'" | "\"" => {
                for operand in &operation.operands {
                    if let Ok(bytes) = operand.as_str() {
                        shown.extend_from_slice(bytes);
                    }
                }
            }
            "TJ" => {
                if let Some(items) = operation.operands.first().and_then(|o| o.as_array().ok()) {
                    for item in items {
                        if let Ok(bytes) = item.as_str() {
                            shown.extend_from_slice(bytes);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    shown
}

fn unique_search_rect(pdf: &[u8], query: &str) -> (f64, f64, f64, f64) {
    let hits = search(pdf, query, true).unwrap_or_else(|error| panic!("search {query:?}: {error}"));
    assert_eq!(hits.len(), 1, "expected one {query:?} hit, got {hits:?}");
    hits[0].rect
}

fn area_for_query(pdf: &[u8], query: &str) -> RedactArea {
    let (left, bottom, width, height) = unique_search_rect(pdf, query);
    area_for_region(
        0,
        0,
        left + 0.1,
        left + width - 0.1,
        bottom + 0.1,
        bottom + height - 0.1,
    )
}

fn assert_query_geometry_unchanged(input: &[u8], output: &[u8], query: &str) {
    let before = unique_search_rect(input, query);
    let after = unique_search_rect(output, query);
    for (before, after) in [before.0, before.1, before.2, before.3]
        .into_iter()
        .zip([after.0, after.1, after.2, after.3])
    {
        assert!(
            (before - after).abs() <= 0.01,
            "{query:?} geometry moved: before={before}, after={after}"
        );
    }
}

fn bytes_contain(pdf: &[u8], needle: &str) -> bool {
    fn contains(bytes: &[u8], needle: &[u8]) -> bool {
        bytes.windows(needle.len()).any(|window| window == needle)
    }

    fn object_contains(object: &Object, needle: &[u8]) -> bool {
        match object {
            Object::String(bytes, _) => contains(bytes, needle),
            Object::Array(values) => values.iter().any(|value| object_contains(value, needle)),
            Object::Dictionary(dictionary) => dictionary
                .iter()
                .any(|(_, value)| object_contains(value, needle)),
            Object::Stream(stream) => {
                stream
                    .dict
                    .iter()
                    .any(|(_, value)| object_contains(value, needle))
                    || contains(&stream.content, needle)
                    || stream
                        .decompressed_content()
                        .is_ok_and(|bytes| contains(&bytes, needle))
            }
            _ => false,
        }
    }

    if contains(pdf, needle.as_bytes()) {
        return true;
    }
    let Ok(doc) = Document::load_mem(pdf) else {
        return true;
    };
    doc.objects
        .values()
        .any(|object| object_contains(object, needle.as_bytes()))
}

fn decoded_graph_content_contains(pdf: &[u8], needle: &str) -> bool {
    fn contains_word(bytes: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || needle.len() > bytes.len() {
            return false;
        }
        bytes
            .windows(needle.len())
            .enumerate()
            .any(|(index, window)| {
                window == needle
                    && index
                        .checked_sub(1)
                        .and_then(|previous| bytes.get(previous))
                        .is_none_or(|byte| !byte.is_ascii_alphanumeric())
                    && bytes
                        .get(index + needle.len())
                        .is_none_or(|byte| !byte.is_ascii_alphanumeric())
            })
    }

    fn object_content_contains(object: &Object, needle: &[u8]) -> bool {
        match object {
            Object::String(bytes, _) => contains_word(bytes, needle),
            Object::Array(values) => values
                .iter()
                .any(|value| object_content_contains(value, needle)),
            Object::Dictionary(dictionary) => dictionary
                .iter()
                .any(|(_, value)| object_content_contains(value, needle)),
            Object::Stream(stream) => {
                stream
                    .dict
                    .iter()
                    .any(|(_, value)| object_content_contains(value, needle))
                    || stream.decompressed_content().is_ok_and(|bytes| {
                        std::str::from_utf8(&bytes)
                            .is_ok_and(|text| contains_word(text.as_bytes(), needle))
                    })
            }
            Object::Name(_) | Object::Reference(_) => false,
            _ => false,
        }
    }

    let document = Document::load_mem(pdf).expect("load decoded-object-graph PDF");
    document
        .objects
        .values()
        .any(|object| object_content_contains(object, needle.as_bytes()))
}

#[test]
fn decoded_graph_content_search_excludes_names_and_keys_but_reads_content() {
    let mut document =
        Document::load_mem(&sample_pdf(&["SEARCHABLE BODY"])).expect("load search fixture");
    document.add_object(dictionary! {
        "structural-needle" => Object::Name(b"structural-needle".to_vec()),
        "StringValue" => Object::string_literal("STRING-CONTENT-SECRET"),
    });
    document.add_object(compressed_stream(dictionary! {}, b"DECODED-STREAM-SECRET"));
    document.add_object(compressed_stream(
        dictionary! { "Length1" => 22 },
        b"\xffBINARY-CONTENT-SECRET",
    ));
    let mut pdf = Vec::new();
    document.save_to(&mut pdf).expect("save search fixture");

    assert!(!decoded_graph_content_contains(&pdf, "structural-needle"));
    assert!(decoded_graph_content_contains(
        &pdf,
        "STRING-CONTENT-SECRET"
    ));
    assert!(decoded_graph_content_contains(
        &pdf,
        "DECODED-STREAM-SECRET"
    ));
    assert!(!decoded_graph_content_contains(
        &pdf,
        "BINARY-CONTENT-SECRET"
    ));
}

fn assert_single_page_with_requested_coverage(pdf: &[u8]) {
    assert_eq!(page_count(pdf).expect("coverage page count"), 1);
    let operations = page_operations(pdf);
    let has_requested_box = operations.windows(2).any(|window| {
        if window[0].operator != "re" || window[1].operator != "f" {
            return false;
        }
        let values = window[0]
            .operands
            .iter()
            .map(|value| value.as_float().ok().map(f64::from))
            .collect::<Option<Vec<_>>>();
        values.is_some_and(|values| {
            values.len() == 4
                && (values[0] - 60.0).abs() < 0.01
                && (values[1] - 700.0).abs() < 0.01
                && (values[2] - 280.0).abs() < 0.01
                && (values[3] - 50.0).abs() < 0.01
        })
    });
    assert!(has_requested_box, "requested black coverage box is absent");
}

fn assert_unsupported(result: pdf_core::Result<pdf_core::redact_types::RedactOutput>) {
    let error = result.expect_err("fixture must fail closed");
    assert_eq!(error.stable_code(), "unsupported_feature");
    assert!(
        matches!(error, PdfCoreError::Unsupported(_)),
        "expected Unsupported, got {error:?}"
    );
}

#[test]
fn mixed_valid_and_invalid_page_indices_return_invalid_input_without_changing_input() {
    let input = sample_pdf(&["MIXED-INDEX-SECRET"]);
    let original = input.clone();
    let error = redact_pure(&input, &[full_page_area(0), full_page_area(99)])
        .expect_err("mixed valid and invalid page indices must fail");

    match error {
        PdfCoreError::InvalidInput(message) => assert_eq!(
            message,
            "redaction page index 99 is out of range (document has 1 page)"
        ),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    assert_eq!(input, original, "failed redaction mutated its input bytes");
}

#[test]
fn empty_redaction_areas_return_invalid_input() {
    let input = sample_pdf(&["EMPTY-AREA-SECRET"]);
    let error = redact_pure(&input, &[]).expect_err("empty redaction request must fail");

    match error {
        PdfCoreError::InvalidInput(message) => {
            assert_eq!(message, "redaction request contains no areas");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn valid_single_area_redaction_succeeds_with_changed_output() {
    let input = pdf_with_texts(&[("CHANGED-OUTPUT-SECRET", 72, 720)], None);
    let output = redact_pure(&input, &[top_text_area(0)]).expect("valid redaction must succeed");

    assert_ne!(
        output.pdf, input,
        "successful redaction returned input bytes"
    );
}

#[test]
fn out_of_range_page_error_names_index_and_exact_page_count() {
    let input = sample_pdf(&["page zero", "page one"]);
    let error =
        redact_pure(&input, &[full_page_area(42)]).expect_err("out-of-range page index must fail");

    match error {
        PdfCoreError::InvalidInput(message) => assert_eq!(
            message,
            "redaction page index 42 is out of range (document has 2 pages)"
        ),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
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
fn rotated_text_matrix_intersects_in_device_space() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new(
            "Tm",
            vec![
                0.into(),
                1.into(),
                (-1).into(),
                0.into(),
                300.into(),
                500.into(),
            ],
        ),
        Operation::new("Tj", vec![Object::string_literal("ROTATED-TM-SECRET")]),
        Operation::new("ET", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 120.into()]),
        Operation::new("Tj", vec![Object::string_literal("ROTATED-TM-KEEP")]),
        Operation::new("ET", vec![]),
    ]);
    let selected = area_for_region(0, 0, 278.0, 310.0, 500.0, 700.0);
    let output = redact_pure(&input, &[selected]).expect("redact rotated Tm text");
    let extracted = extract_text(&output.pdf).expect("extract rotated Tm output");

    assert!(!bytes_contain(&output.pdf, "ROTATED-TM-SECRET"));
    assert!(!extracted.contains("ROTATED-TM-SECRET"));
    assert!(extracted.contains("ROTATED-TM-KEEP"));
}

#[test]
fn text_object_bounds_span_all_show_operator_forms() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new("Tj", vec![Object::string_literal("HEAD-SEGMENT-")]),
        Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("OPERATOR-"),
                (-40).into(),
                Object::string_literal("SECRET"),
            ])],
        ),
        Operation::new("TL", vec![24.into()]),
        Operation::new("'", vec![Object::string_literal("QUOTE-SEGMENT")]),
        Operation::new(
            "\"",
            vec![
                0.into(),
                0.into(),
                Object::string_literal("DOUBLE-QUOTE-SEGMENT"),
            ],
        ),
        Operation::new("ET", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 120.into()]),
        Operation::new("Tj", vec![Object::string_literal("BOUNDARY-KEEP")]),
        Operation::new("ET", vec![]),
    ]);
    // The rectangle spans every line of the first text object, so each show-operator
    // form (Tj, TJ, ' and ") must be analysed and rewritten. A form that went
    // unanalysed would leave its glyphs behind. The second text object sits far below
    // the rectangle and must survive intact — removal is per glyph, not per object.
    let selected = area_for_region(0, 0, 60.0, 430.0, 650.0, 750.0);
    let output = redact_pure(&input, &[selected]).expect("redact cross-operator text object");
    let extracted = extract_text(&output.pdf).expect("extract cross-operator output");

    for removed in [
        "HEAD-SEGMENT-",
        "OPERATOR-",
        "SECRET",
        "QUOTE-SEGMENT",
        "DOUBLE-QUOTE-SEGMENT",
    ] {
        assert!(!bytes_contain(&output.pdf, removed), "survived: {removed}");
    }
    assert_eq!(
        page_shown_bytes(&output.pdf),
        b"BOUNDARY-KEEP".to_vec(),
        "only the text object outside the rectangle may keep its glyphs"
    );
    assert!(extracted.contains("BOUNDARY-KEEP"));
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
    // A text object that builds a clip (Tr 4) with one run and paints ordinary text
    // (Tr 0) with another. Removing a clip-*building* glyph would shrink the clipping
    // path and could reveal content that was previously clipped away, so that must
    // fail closed. Removing only the ordinary run leaves the clip intact and is safe:
    // per-glyph rewriting no longer has to destroy the whole object to redact it.
    let fixture = || {
        pdf_with_operations(vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 20.into()]),
            Operation::new("Td", vec![72.into(), 120.into()]),
            Operation::new("Tr", vec![4.into()]),
            Operation::new("Tj", vec![Object::string_literal("CLIPPING-TEXT")]),
            Operation::new("Tr", vec![0.into()]),
            Operation::new("Td", vec![0.into(), 600.into()]),
            Operation::new("Tj", vec![Object::string_literal("SELECTED-TEXT")]),
            Operation::new("ET", vec![]),
        ])
    };

    // The clip-building run is at y=120 and must never be rewritten.
    let over_clipping_run = area_for_region(0, 0, 60.0, 260.0, 108.0, 145.0);
    assert_unsupported(redact_pure(&fixture(), &[over_clipping_run]));

    // The ordinary run at y=720 can be removed without touching the clip.
    let output = redact_pure(&fixture(), &[top_text_area(0)]).expect("redact non-clipping run");
    assert!(!bytes_contain(&output.pdf, "SELECTED-TEXT"));
    assert_eq!(
        page_shown_bytes(&output.pdf),
        b"CLIPPING-TEXT".to_vec(),
        "the clip-building run must survive verbatim"
    );
}

#[test]
fn negative_character_spacing_bounds_every_intermediate_glyph_origin() {
    // Helvetica `W` (944/1000) then `i` (222/1000) at 20pt from x=200, with Tc=-10.
    // The negative char spacing pulls `i` back: its origin is 200+18.88-10 = 208.88
    // and its cell ends at ≈213.3, so nothing is painted at or beyond x=219. Ignoring
    // Tc would instead place `i` at 218.88..223.3 — directly inside the rectangle.
    let fixture = || {
        pdf_with_operations(vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 20.into()]),
            Operation::new("Td", vec![200.into(), 720.into()]),
            Operation::new("Tc", vec![(-10).into()]),
            Operation::new("Tj", vec![Object::string_literal("Wi")]),
            Operation::new("ET", vec![]),
        ])
    };

    let beyond_run = area_for_region(0, 0, 219.0, 222.0, 715.0, 735.0);
    let untouched = redact_pure(&fixture(), &[beyond_run]).expect("redact beyond the run");
    assert_eq!(
        page_shown_bytes(&untouched.pdf),
        b"Wi".to_vec(),
        "negative char spacing must pull the intermediate origin left of x=219"
    );

    let over_run = area_for_region(0, 0, 210.0, 212.0, 715.0, 735.0);
    let removed = redact_pure(&fixture(), &[over_run]).expect("redact the overlapping cells");
    assert!(
        page_shown_bytes(&removed.pdf).is_empty(),
        "both overlapping glyph cells cover x=210..212 and must be removed"
    );
}

#[test]
fn extgstate_line_width_inflates_stroked_text_bounds() {
    // Helvetica `I` at 20pt from x=200 advances 278/1000 em, so its glyph cell ends
    // at x≈205.6. The ExtGState sets LW=20, and a stroked glyph paints half the line
    // width beyond its outline, so the painted extent reaches back to x≈190. The
    // rectangle below sits in that stroke halo and *outside* the glyph cell: it can
    // only intersect if the ExtGState line width is applied to the text bounds.
    let input = stroked_text_extgstate_pdf();
    let selected = area_for_region(0, 0, 191.0, 193.0, 715.0, 735.0);
    let output = redact_pure(&input, &[selected]).expect("redact stroked text extent");

    assert_eq!(page_show_count(&output.pdf), 0);
    assert!(page_shown_bytes(&output.pdf).is_empty());
}

#[test]
fn explicit_widths_are_resolved_for_type1_truetype_and_mmtype1_fonts() {
    // `AA` at 24pt from x=72 with an explicit /Widths of 500: the first glyph owns
    // x=72..84 and the second x=84..96. The rectangle covers only the second cell, so
    // exactly one `A` may be removed — and it can only be reached if the explicit
    // width (not a default) placed it there.
    for subtype in ["Type1", "TrueType", "MMType1"] {
        let input = explicit_width_pdf(subtype);
        let selected = area_for_region(0, 0, 88.0, 94.0, 715.0, 740.0);
        let output = redact_pure(&input, &[selected]).expect("redact explicit-width text");
        assert_eq!(
            page_shown_bytes(&output.pdf),
            b"A".to_vec(),
            "{subtype} width must place the second A through x=96 and keep the first"
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
fn partial_tj_removes_one_word_and_keeps_five_word_run_geometry() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new(
            "Tj",
            vec![Object::string_literal("ONE TWO TARGET FOUR FIVE")],
        ),
        Operation::new("ET", vec![]),
    ]);
    let output =
        redact_pure(&input, &[area_for_query(&input, "TARGET")]).expect("rewrite one word from Tj");

    assert!(!extract_text(&output.pdf).unwrap().contains("TARGET"));
    for survivor in ["ONE", "TWO", "FOUR", "FIVE"] {
        assert_query_geometry_unchanged(&input, &output.pdf, survivor);
    }
    assert!(output.warnings.is_empty());
    assert!(!bytes_contain(&output.pdf, "TARGET"));
}

#[test]
fn partial_tj_array_preserves_existing_mid_array_adjustments() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("ONE "),
                (-120).into(),
                Object::string_literal("TWO TARGET "),
                80.into(),
                Object::string_literal("FOUR FIVE"),
            ])],
        ),
        Operation::new("ET", vec![]),
    ]);
    let output = redact_pure(&input, &[area_for_query(&input, "TARGET")])
        .expect("rewrite one word inside TJ array");

    for survivor in ["ONE", "TWO", "FOUR", "FIVE"] {
        assert_query_geometry_unchanged(&input, &output.pdf, survivor);
    }
    assert!(!bytes_contain(&output.pdf, "TARGET"));
}

#[test]
fn partial_multibyte_show_never_splits_a_two_byte_code() {
    let input = fixed_two_byte_cmap_pdf();
    let (left, bottom, width, height) = unique_search_rect(&input, "C");
    let selected = area_for_region(
        0,
        0,
        left + width * 0.25,
        left + width * 0.55,
        bottom + 0.1,
        bottom + height - 0.1,
    );
    let output = redact_pure(&input, &[selected]).expect("rewrite one two-byte CMap code");
    let remaining = page_operations(&output.pdf)
        .iter()
        .filter_map(operation_show_bytes)
        .flatten()
        .collect::<Vec<_>>();

    assert_eq!(remaining, vec![0, 1, 0, 2, 0, 4, 0, 5]);
    for survivor in ["A", "B", "D", "E"] {
        assert_query_geometry_unchanged(&input, &output.pdf, survivor);
    }
}

#[test]
fn vertical_wmode_one_redacts_middle_glyph_and_keeps_column_origins() {
    let input = embedded_vertical_cmap_pdf(1, 1);
    let output = redact_pure(&input, &[area_for_query(&input, "C")])
        .expect("redact vertical WMode-1 middle glyph");

    assert!(!extract_text(&output.pdf).unwrap().contains('C'));
    for survivor in ["A", "B", "D", "E"] {
        assert_query_geometry_unchanged(&input, &output.pdf, survivor);
    }
}

#[test]
fn contradictory_vertical_wmode_sources_fail_with_typed_error() {
    let error = redact_pure(&embedded_vertical_cmap_pdf(0, 1), &[full_page_area(0)])
        .expect_err("contradictory WMode must fail closed");

    assert!(matches!(error, PdfCoreError::Unsupported(_)));
    assert_eq!(error.stable_code(), "unsupported_feature");
}

#[test]
fn partial_show_preserves_nonzero_character_and_word_spacing() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new("Tc", vec![2.into()]),
        Operation::new("Tw", vec![5.into()]),
        Operation::new(
            "Tj",
            vec![Object::string_literal("ONE TWO TARGET FOUR FIVE")],
        ),
        Operation::new("ET", vec![]),
    ]);
    let output = redact_pure(&input, &[area_for_query(&input, "TARGET")])
        .expect("rewrite text with Tc and Tw");

    for survivor in ["ONE", "TWO", "FOUR", "FIVE"] {
        assert_query_geometry_unchanged(&input, &output.pdf, survivor);
    }
}

#[test]
fn partial_show_preserves_nondefault_horizontal_scale() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new("Tz", vec![75.into()]),
        Operation::new(
            "Tj",
            vec![Object::string_literal("ONE TWO TARGET FOUR FIVE")],
        ),
        Operation::new("ET", vec![]),
    ]);
    let output =
        redact_pure(&input, &[area_for_query(&input, "TARGET")]).expect("rewrite text with Tz");

    for survivor in ["ONE", "TWO", "FOUR", "FIVE"] {
        assert_query_geometry_unchanged(&input, &output.pdf, survivor);
    }
}

#[test]
fn partial_quote_operators_preserve_line_move_and_spacing_side_effects() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 18.into()]),
        Operation::new("Td", vec![72.into(), 740.into()]),
        Operation::new("TL", vec![28.into()]),
        Operation::new("Tj", vec![Object::string_literal("HEADER KEEP")]),
        Operation::new(
            "'",
            vec![Object::string_literal("ONE TWO QUOTESECRET FOUR FIVE")],
        ),
        Operation::new(
            "\"",
            vec![
                4.into(),
                2.into(),
                Object::string_literal("SIX SEVEN DOUBLESECRET NINE TEN"),
            ],
        ),
        Operation::new("ET", vec![]),
    ]);
    let areas = [
        area_for_query(&input, "QUOTESECRET"),
        area_for_query(&input, "DOUBLESECRET"),
    ];
    let output = redact_pure(&input, &areas).expect("rewrite quote operators");

    for survivor in ["HEADER", "ONE", "FOUR", "SIX", "NINE", "TEN"] {
        assert_query_geometry_unchanged(&input, &output.pdf, survivor);
    }
    assert!(!bytes_contain(&output.pdf, "QUOTESECRET"));
    assert!(!bytes_contain(&output.pdf, "DOUBLESECRET"));
}

#[test]
fn partial_stroked_text_modes_keep_unselected_glyphs() {
    for rendering_mode in [1, 2] {
        let input = pdf_with_operations(vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 20.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("w", vec![6.into()]),
            Operation::new("Tr", vec![rendering_mode.into()]),
            Operation::new(
                "Tj",
                vec![Object::string_literal("ONE TWO TARGET FOUR FIVE")],
            ),
            Operation::new("ET", vec![]),
        ]);
        let output = redact_pure(&input, &[area_for_query(&input, "TARGET")])
            .unwrap_or_else(|error| panic!("rewrite Tr {rendering_mode}: {error}"));

        for survivor in ["ONE", "TWO", "FOUR", "FIVE"] {
            assert_query_geometry_unchanged(&input, &output.pdf, survivor);
        }
        assert!(!bytes_contain(&output.pdf, "TARGET"));
    }
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
fn type3_font_metrics_allow_irreversible_redaction() {
    let output = redact_pure(&type3_pdf(), &[full_page_area(0)])
        .expect("Type3 FontMatrix, Widths, Encoding, and CharProcs are analyzable");

    assert_eq!(page_show_count(&output.pdf), 0);
    assert!(!bytes_contain(&output.pdf, "aaa"));
    assert_eq!(
        validate(&output.pdf)
            .expect("validate Type3 output")
            .page_count,
        1
    );
}

#[test]
fn type3_d0_charproc_allows_irreversible_redaction() {
    let output = redact_pure(&type3_d0_pdf(), &[full_page_area(0)])
        .expect("Type3 d0 CharProc uses FontBBox for conservative geometry");

    assert_eq!(page_show_count(&output.pdf), 0);
    assert!(!bytes_contain(&output.pdf, "aaa"));
}

#[test]
fn type3_bare_standard_encoding_names_are_supported() {
    for encoding in ["WinAnsiEncoding", "MacRomanEncoding", "StandardEncoding"] {
        let input = type3_encoding_pdf(Object::Name(encoding.as_bytes().to_vec()));
        let output = redact_pure(&input, &[full_page_area(0)])
            .unwrap_or_else(|error| panic!("Type3 /Encoding /{encoding}: {error}"));
        assert_eq!(page_show_count(&output.pdf), 0, "{encoding}");
    }
}

#[test]
fn type3_base_encoding_and_differences_are_layered() {
    let fixtures = [
        dictionary! { "BaseEncoding" => "WinAnsiEncoding" },
        dictionary! {
            "BaseEncoding" => "StandardEncoding",
            "Differences" => vec![65.into(), "A".into()],
        },
    ];
    for encoding in fixtures {
        let output = redact_pure(
            &type3_encoding_pdf(Object::Dictionary(encoding)),
            &[full_page_area(0)],
        )
        .expect("Type3 BaseEncoding with optional Differences");
        assert_eq!(page_show_count(&output.pdf), 0);
    }
}

#[test]
fn transformed_type3_partial_glyph_redaction_keeps_other_origins() {
    let input = transformed_type3_run_pdf();
    let before = search(&input, "a", true).expect("search transformed Type3 input");
    assert_eq!(before.len(), 5, "transformed Type3 fixture extraction");
    // The fixture's /FontMatrix translates every glyph by (5, 10) glyph-space units
    // before scaling, so the ink is painted well away from the pen. pdfium's character
    // box ignores that translation (it reports the pen cell), while a renderer honours
    // it: rasterising this page puts the five glyphs at x=296..356, y=480..497, twelve
    // points apart. Redaction geometry must follow the ink, not pdfium's box, so the
    // rectangle below is placed over the *painted* middle glyph (x=320..332).
    let selected = area_for_region(0, 0, 321.0, 331.0, 482.0, 495.0);
    let output = redact_pure(&input, &[selected]).expect("redact transformed Type3 middle glyph");
    let after = search(&output.pdf, "a", true).expect("search transformed Type3 output");
    let expected = [&before[0], &before[1], &before[3], &before[4]];

    assert_eq!(after.len(), expected.len());
    for (actual, expected) in after.iter().zip(expected) {
        for (actual, expected) in [actual.rect.0, actual.rect.1, actual.rect.2, actual.rect.3]
            .into_iter()
            .zip([
                expected.rect.0,
                expected.rect.1,
                expected.rect.2,
                expected.rect.3,
            ])
        {
            assert!((actual - expected).abs() <= 0.01);
        }
    }
}

#[test]
fn predefined_unijis_ucs2_h_redacts_without_embedded_tounicode() {
    let input = predefined_japan1_cmap_pdf("UniJIS-UCS2-H");
    // The fixture embeds no font program, so pdfium substitutes one whose ink is wider
    // than the declared DW=500 (10pt at this size) and its reported character boxes
    // overlap their neighbours. Selecting by declared metrics instead: the five glyphs
    // sit on 10pt pens from x=300, so `C` owns x=320..330 and its neighbours do not.
    let selected = area_for_region(0, 0, 321.0, 329.0, 702.0, 712.0);
    let output = redact_pure(&input, &[selected]).expect("redact predefined UniJIS-UCS2-H");

    assert!(!extract_text(&output.pdf).unwrap().contains('C'));
    for survivor in ["A", "B", "D", "E"] {
        assert_query_geometry_unchanged(&input, &output.pdf, survivor);
    }
}

#[test]
fn predefined_unijis_ucs2_v_resolves_usecmap_and_vertical_metrics() {
    let input = predefined_japan1_cmap_pdf("UniJIS-UCS2-V");
    // This assertion harness locates glyphs through pdfium text search, and
    // not every platform's pdfium build resolves the vertical usecmap chain:
    // the Linux chromium/148 dylib extracts empty text for UniJIS-UCS2-V
    // (UniJIS-UCS2-H works), while macOS builds extract it fine. redact_pure
    // itself is pure Rust; without a baseline extraction there is nothing the
    // harness can select or verify, so skip loudly instead of failing on an
    // engine gap outside this crate.
    if search(&input, "C", true)
        .map(|hits| hits.is_empty())
        .unwrap_or(true)
    {
        eprintln!(
            "skipping: this pdfium build cannot extract text through the \
             predefined UniJIS-UCS2-V usecmap chain"
        );
        return;
    }
    let output = redact_pure(&input, &[area_for_query(&input, "C")])
        .expect("redact usecmap-chained predefined UniJIS-UCS2-V");

    assert!(!extract_text(&output.pdf).unwrap().contains('C'));
    for survivor in ["A", "B", "D", "E"] {
        assert_query_geometry_unchanged(&input, &output.pdf, survivor);
    }
}

#[test]
fn non_identity_cmap_fails_closed() {
    assert_unsupported(redact_pure(
        &cid_pdf("UniJIS-UTF16-H"),
        &[full_page_area(0)],
    ));
}

#[test]
fn embedded_multibyte_cmap_and_tounicode_allow_irreversible_redaction() {
    let input = embedded_cmap_pdf();
    let output = redact_pure(&input, &[full_page_area(0)])
        .expect("embedded multibyte Encoding and ToUnicode CMaps are analyzable");

    assert_eq!(page_show_count(&output.pdf), 0);
    assert!(
        !output
            .pdf
            .windows(10)
            .any(|window| window == [1, 2, 1, 3, 0, 1, 4, 0, 0, 1]),
        "encoded multibyte text survived"
    );
    assert_eq!(
        validate(&output.pdf)
            .expect("validate embedded CMap output")
            .page_count,
        1
    );
}

#[test]
fn embedded_cmap_word_spacing_uses_source_code_not_tounicode_text() {
    let input = embedded_cmap_misleading_space_pdf();
    let selected = area_for_region(0, 0, 103.0, 111.0, 716.0, 738.0);
    let output = redact_pure(&input, &[selected]).expect("redact final rendered CID glyph");

    // Tw applies only to single-byte code 0x20. Here ToUnicode maps CID code 0x01 to
    // U+0020, so an engine that keyed word spacing off the *decoded text* would apply
    // Tw=-100 to the leading glyphs and shift the rendered `X` far left of the
    // rectangle. Keying off the source code keeps `X` in x=102..112, so exactly that
    // glyph is removed and the three preceding codes survive untouched.
    assert_eq!(page_shown_bytes(&output.pdf), vec![1, 1, 1]);
    assert!(
        !extract_text(&output.pdf)
            .expect("extract misleading-space CMap output")
            .contains('X'),
        "rendered CID text survived because Tw followed ToUnicode instead of source code 0x20"
    );
    assert!(
        !output.pdf.windows(4).any(|window| window == [1, 1, 1, 2]),
        "encoded CID text survived the rewrite"
    );
}

#[test]
fn combining_and_shaped_tounicode_sequences_are_removed() {
    let input = identity_unicode_sequences_pdf();
    let extracted = extract_text(&input).expect("extract Unicode sequence input");
    assert!(
        extracted.contains("e\u{301}"),
        "combining mapping was not extracted: {extracted:?}"
    );
    assert!(
        extracted.contains("ffi"),
        "shaped mapping was not extracted: {extracted:?}"
    );

    let output =
        redact_pure(&input, &[full_page_area(0)]).expect("redact combining and shaped sequences");
    assert_eq!(
        extract_text(&output.pdf).expect("extract Unicode output"),
        ""
    );
    assert_eq!(page_show_count(&output.pdf), 0);
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
    let encrypted = meta::encrypt(&sample_pdf(&["ENCRYPTED-SECRET"]), "user-pw", "owner-only")
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
            .any(|warning| warning.kind.as_str() == "form_xobject"),
        "expected excess-coverage warning, got {:?}",
        output.warnings
    );
    assert_single_page_with_requested_coverage(&output.pdf);
}

#[test]
fn intersecting_nested_form_is_removed_from_decoded_object_graph() {
    let input = nested_form_pdf();
    let output = redact_pure(&input, &[top_text_area(0)]).expect("redact nested form");

    assert!(!bytes_contain(&output.pdf, "NESTED-FORM-SECRET-c921"));
    assert_single_page_with_requested_coverage(&output.pdf);
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
    assert_single_page_with_requested_coverage(&output.pdf);
}

#[test]
fn intersecting_inline_image_is_removed_with_its_exact_samples() {
    let input = inline_image_pdf();
    let output = redact_pure(&input, &[top_text_area(0)]).expect("redact inline image");
    let operations = page_operations(&output.pdf);

    assert!(
        operations
            .iter()
            .all(|operation| operation.operator != "BI"),
        "intersecting inline image operation survived"
    );
    assert!(!output.pdf.windows(3).any(|window| window == [255, 0, 0]));
    assert!(
        output
            .warnings
            .iter()
            .any(|warning| warning.kind.as_str() == "inline_image"),
        "partial inline-image removal did not warn: {:?}",
        output.warnings
    );
    assert_single_page_with_requested_coverage(&output.pdf);
}

#[test]
fn intersecting_image_xobject_and_masks_are_removed_and_pruned() {
    let input = image_xobject_pdf(false);
    let output = redact_pure(&input, &[top_text_area(0)]).expect("redact image XObject");

    for marker in [
        "IMAGE-XOBJECT-SECRET-a4c8",
        "IMAGE-SMASK-SECRET-7e19",
        "IMAGE-MASK-SECRET-3b52",
    ] {
        assert!(
            !bytes_contain(&output.pdf, marker),
            "decoded image marker survived: {marker}"
        );
    }
    assert!(
        output
            .warnings
            .iter()
            .any(|warning| warning.kind.as_str() == "image_xobject"),
        "partial image removal did not warn: {:?}",
        output.warnings
    );
    assert_single_page_with_requested_coverage(&output.pdf);
}

#[test]
fn image_xobject_shared_with_unredacted_page_fails_closed() {
    let input = image_xobject_pdf(true);

    assert_unsupported(redact_pure(&input, &[top_text_area(0)]));
}

#[test]
fn intersecting_vector_path_is_removed_from_decoded_content() {
    let input = pdf_with_raw_content(
        b"123.456 710 m 333.789 710 l 333.789 740 l 123.456 740 l h f".to_vec(),
        dictionary! {},
        dictionary! {},
    );
    let output = redact_pure(&input, &[top_text_area(0)]).expect("redact vector path");

    assert!(!bytes_contain(&output.pdf, "123.456"));
    assert_single_page_with_requested_coverage(&output.pdf);
}

#[test]
#[serial(pdfium)]
fn intersecting_painted_clip_keeps_clip_and_suppresses_only_paint() {
    let input = pdf_with_operations(vec![
        Operation::new("re", vec![72.into(), 600.into(), 160.into(), 130.into()]),
        Operation::new("W", vec![]),
        Operation::new("S", vec![]),
        Operation::new("re", vec![350.into(), 450.into(), 80.into(), 80.into()]),
        Operation::new("f", vec![]),
    ]);
    let selected = area_for_region(0, 0, 70.0, 85.0, 680.0, 700.0);
    let output = redact_pure(&input, &[selected]).expect("rewrite painted clipping path");
    let operations = page_operations(&output.pdf);

    assert_operator_pair(&operations, "W", "n");
    assert!(
        !pixel_is_dark(&output.pdf, 390, 490),
        "content outside the retained clip became visible"
    );
    assert_eq!(
        extract_text(&output.pdf).expect("extract clipped output"),
        ""
    );
}

#[test]
#[serial(pdfium)]
fn clipping_q_q_scope_is_retained_without_escaping() {
    let input = pdf_with_operations(vec![
        Operation::new("q", vec![]),
        Operation::new("re", vec![72.into(), 600.into(), 160.into(), 130.into()]),
        Operation::new("W", vec![]),
        Operation::new("S", vec![]),
        Operation::new("Q", vec![]),
        Operation::new("re", vec![350.into(), 450.into(), 80.into(), 80.into()]),
        Operation::new("f", vec![]),
    ]);
    let selected = area_for_region(0, 0, 70.0, 85.0, 680.0, 700.0);
    let output = redact_pure(&input, &[selected]).expect("rewrite scoped clipping path");
    let operations = page_operations(&output.pdf);

    assert_operator_pair(&operations, "W", "n");
    let retained_clip = operations
        .iter()
        .position(|operation| operation.operator == "W")
        .expect("retained clip");
    let restored = operations
        .iter()
        .skip(retained_clip)
        .position(|operation| operation.operator == "Q")
        .map(|offset| retained_clip + offset)
        .expect("clip graphics state is restored");
    let later_fill = operations
        .iter()
        .skip(restored + 1)
        .any(|operation| operation.operator == "f");
    assert!(later_fill, "content after Q was incorrectly clipped away");
    assert!(
        pixel_is_dark(&output.pdf, 390, 490),
        "the retained clip escaped its q/Q scope"
    );
}

#[test]
#[serial(pdfium)]
fn intersecting_even_odd_clip_retains_winding_rule() {
    let input = pdf_with_operations(vec![
        Operation::new("re", vec![72.into(), 580.into(), 220.into(), 170.into()]),
        Operation::new("re", vec![140.into(), 640.into(), 60.into(), 50.into()]),
        Operation::new("W*", vec![]),
        Operation::new("S", vec![]),
        Operation::new("re", vec![72.into(), 580.into(), 220.into(), 170.into()]),
        Operation::new("f", vec![]),
    ]);
    let selected = area_for_region(0, 0, 70.0, 85.0, 680.0, 700.0);
    let output = redact_pure(&input, &[selected]).expect("rewrite even-odd clipping path");
    let operations = page_operations(&output.pdf);

    assert_operator_pair(&operations, "W*", "n");
    assert!(
        operations
            .iter()
            .skip_while(|operation| operation.operator != "W*")
            .skip(2)
            .any(|operation| operation.operator == "f"),
        "content painted through the retained even-odd clip disappeared"
    );
}

#[test]
#[serial(pdfium)]
fn intersecting_fill_and_clip_replaces_fill_with_end_path() {
    let input = pdf_with_operations(vec![
        Operation::new("re", vec![72.into(), 600.into(), 160.into(), 130.into()]),
        Operation::new("W", vec![]),
        Operation::new("f", vec![]),
        Operation::new("re", vec![350.into(), 450.into(), 80.into(), 80.into()]),
        Operation::new("f", vec![]),
    ]);
    let selected = area_for_region(0, 0, 70.0, 85.0, 680.0, 700.0);
    let output = redact_pure(&input, &[selected]).expect("rewrite fill-and-clip path");
    let operations = page_operations(&output.pdf);

    assert_operator_pair(&operations, "W", "n");
    assert!(
        !pixel_is_dark(&output.pdf, 390, 490),
        "fill-and-clip suppression widened visibility"
    );
}

#[test]
#[serial(pdfium)]
fn transformed_painted_clip_is_retained_in_device_space() {
    let input = pdf_with_operations(vec![
        Operation::new("q", vec![]),
        Operation::new(
            "cm",
            vec![
                2.into(),
                0.into(),
                0.into(),
                2.into(),
                100.into(),
                300.into(),
            ],
        ),
        Operation::new("re", vec![0.into(), 100.into(), 80.into(), 65.into()]),
        Operation::new("W", vec![]),
        Operation::new("S", vec![]),
        Operation::new("re", vec![150.into(), 20.into(), 40.into(), 40.into()]),
        Operation::new("f", vec![]),
        Operation::new("Q", vec![]),
    ]);
    let selected = area_for_region(0, 0, 98.0, 112.0, 500.0, 520.0);
    let output = redact_pure(&input, &[selected]).expect("rewrite transformed clipping path");
    let operations = page_operations(&output.pdf);

    assert_operator_pair(&operations, "W", "n");
    assert!(
        !pixel_is_dark(&output.pdf, 420, 380),
        "content outside the transformed clip became visible"
    );
}

#[test]
#[serial(pdfium)]
fn painted_clip_inside_form_is_rewritten_without_widening_visibility() {
    let input = clipping_form_pdf();
    let selected = area_for_region(0, 0, 98.0, 112.0, 440.0, 460.0);
    let output = redact_pure(&input, &[selected]).expect("rewrite clipping path in Form XObject");
    let page = page_operations(&output.pdf);
    let form = form_operations(&output.pdf);

    assert!(
        page.iter().any(|operation| operation.operator == "Do"),
        "safe form invocation was removed instead of rewritten"
    );
    assert_operator_pair(&form, "W", "n");
    assert!(
        !pixel_is_dark(&output.pdf, 275, 575),
        "content outside the form's retained clip became visible"
    );
}

#[test]
fn clipping_form_inherits_invoking_word_spacing_for_text_geometry() {
    let input = inherited_state_clipping_form_pdf(
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 20.into()]),
            Operation::new("Tw", vec![80.into()]),
            Operation::new("ET", vec![]),
        ],
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 20.into()]),
            Operation::new("Td", vec![20.into(), 40.into()]),
            Operation::new("Tj", vec![Object::string_literal("  X")]),
            Operation::new("ET", vec![]),
        ],
    );
    let selected = area_for_region(0, 0, 294.0, 310.0, 430.0, 465.0);
    let output = redact_pure(&input, &[selected]).expect("redact spaced text inside clipped Form");

    assert!(
        !extract_text(&output.pdf)
            .expect("extract inherited-Tw Form output")
            .contains('X'),
        "text positioned by the invoking Tw survived"
    );
    assert!(!bytes_contain(&output.pdf, "  X"));
}

#[test]
fn clipping_form_inherits_invoking_line_width_for_path_geometry() {
    let input = inherited_state_clipping_form_pdf(
        vec![Operation::new("w", vec![40.into()])],
        vec![
            Operation::new("m", vec![100.into(), 50.into()]),
            Operation::new("l", vec![200.into(), 50.into()]),
            Operation::new("S", vec![]),
        ],
    );
    let selected = area_for_region(0, 0, 220.0, 280.0, 464.0, 469.0);
    let output = redact_pure(&input, &[selected]).expect("redact inherited-width Form stroke");
    let operations = form_operations(&output.pdf);

    assert!(
        operations.iter().all(|operation| operation.operator != "S"),
        "path enlarged by the invoking line width survived: {operations:?}"
    );
}

#[test]
fn nested_form_resources_merge_inherited_and_local_entries() {
    let input = nested_clipping_form_resources_pdf();
    let selected = area_for_region(0, 0, 115.0, 360.0, 410.0, 455.0);
    let output = redact_pure(&input, &[selected]).expect("rewrite nested Form resources");
    let extracted = extract_text(&output.pdf).expect("extract nested Form resource output");

    assert!(!bytes_contain(&output.pdf, "NESTED-RESOURCE-SECRET"));
    assert!(!extracted.contains("NESTED-RESOURCE-SECRET"));
    assert!(
        extracted.contains("NESTED-RESOURCE-KEEP"),
        "local nested font content disappeared: {extracted:?}"
    );
    assert_eq!(
        validate(&output.pdf)
            .expect("validate nested Form output")
            .page_count,
        1
    );
}

#[test]
fn intersecting_widget_field_value_and_appearance_are_removed_and_pruned() {
    let input = widget_pdf();
    let output = redact_pure(&input, &[top_text_area(0)]).expect("redact widget field");

    assert!(!bytes_contain(&output.pdf, "WIDGET-V-SECRET-03ef"));
    assert!(!bytes_contain(&output.pdf, "WIDGET-AP-SECRET-5d60"));
    assert_single_page_with_requested_coverage(&output.pdf);
}

#[test]
fn struct_tree_actual_text_without_marked_content_fails_closed() {
    let input = struct_tree_without_marked_content_pdf();

    assert_unsupported(redact_pure(&input, &[top_text_area(0)]));
}

#[test]
fn shading_operation_and_resource_are_removed_and_pruned() {
    let input = shading_pdf();
    let output = redact_pure(&input, &[top_text_area(0)]).expect("redact shading");

    assert!(!bytes_contain(&output.pdf, "SHADING-DICT-SECRET-61aa"));
    assert!(
        page_operations(&output.pdf)
            .iter()
            .all(|operation| operation.operator != "sh"),
        "shading operation survived"
    );
    assert!(
        output
            .warnings
            .iter()
            .any(|warning| warning.kind.as_str() == "shading"),
        "whole-page shading removal did not warn: {:?}",
        output.warnings
    );
    assert_single_page_with_requested_coverage(&output.pdf);
}

#[test]
fn shading_disjoint_from_redaction_under_active_clip_is_preserved() {
    let input = clipped_shading_pdf();
    let selected = area_for_region(0, 0, 400.0, 430.0, 700.0, 730.0);
    let output = redact_pure(&input, &[selected]).expect("preserve disjoint clipped shading");

    assert!(
        page_operations(&output.pdf)
            .iter()
            .any(|operation| operation.operator == "sh"),
        "shading outside the redaction through an active clip was removed"
    );
    assert!(bytes_contain(&output.pdf, "CLIPPED-SHADING-KEEP-a8d3"));
}

#[test]
#[serial(pdfium)]
fn intersecting_clipped_shading_coverage_is_bounded_by_active_clip() {
    let input = clipped_shading_pdf();
    let selected = area_for_region(0, 0, 30.0, 40.0, 30.0, 40.0);
    let output = redact_pure(&input, &[selected]).expect("remove intersecting clipped shading");

    assert!(
        page_operations(&output.pdf)
            .iter()
            .all(|operation| operation.operator != "sh")
    );
    assert!(!bytes_contain(&output.pdf, "CLIPPED-SHADING-KEEP-a8d3"));
    assert!(
        !pixel_is_dark(&output.pdf, 300, 300),
        "clipped shading removal blackened the unrelated page"
    );
}

#[test]
#[serial(pdfium)]
fn graphics_disjoint_from_redaction_under_active_clip_are_preserved() {
    let input = disjoint_clipped_graphics_pdf();
    let selected = area_for_region(0, 0, 410.0, 420.0, 410.0, 420.0);
    let output = redact_pure(&input, &[selected]).expect("preserve disjoint clipped graphics");
    let operations = page_operations(&output.pdf);

    assert!(
        operations
            .iter()
            .any(|operation| operation.operator == "BI"),
        "disjoint clipped inline image was removed"
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.operator == "Do")
            .count(),
        2,
        "disjoint clipped image/Form XObjects were removed"
    );
    assert!(
        operations
            .iter()
            .filter(|operation| operation.operator == "f")
            .count()
            >= 1,
        "disjoint clipped vector path was removed"
    );
    assert!(bytes_contain(&output.pdf, "CLIPPED-IMAGE-KEEP-b1e4"));
    assert!(bytes_contain(&output.pdf, "CLIPPED-FORM-KEEP-c7a2"));
    assert!(
        !pixel_is_dark(&output.pdf, 445, 445),
        "raw bounds from invisible clipped graphics blackened unrelated content"
    );
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
fn partial_text_object_selection_rewrites_only_intersecting_glyphs() {
    let input = pdf_with_texts(&[("ALPHA SECRET OMEGA", 72, 720)], None);
    let selected = area_for_query(&input, "SECRET");
    let output = redact_pure(&input, &[selected]).expect("redact partial text run");

    assert!(output.warnings.is_empty());
    assert!(!extract_text(&output.pdf).unwrap().contains("SECRET"));
    assert_query_geometry_unchanged(&input, &output.pdf, "ALPHA");
    assert_query_geometry_unchanged(&input, &output.pdf, "OMEGA");
    assert_eq!(page_show_count(&output.pdf), 1);
}

#[test]
fn warning_output_is_deterministic_and_stably_sorted() {
    let input = pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new("Tj", vec![Object::string_literal("WARNING-TEXT-SECRET")]),
        Operation::new("ET", vec![]),
        Operation::new("re", vec![72.into(), 680.into(), 280.into(), 60.into()]),
        Operation::new("f", vec![]),
    ]);
    let selected = area_for_region(0, 0, 150.0, 170.0, 710.0, 730.0);
    let first = redact_pure(&input, &[selected]).expect("first deterministic redaction");
    let second = redact_pure(&input, &[selected]).expect("second deterministic redaction");

    assert_eq!(first.warnings, second.warnings);
    assert!(
        first.warnings.windows(2).all(|pair| (
            pair[0].page,
            pair[0].code,
            pair[0].kind,
            pair[0].message.as_str()
        ) <= (
            pair[1].page,
            pair[1].code,
            pair[1].kind,
            pair[1].message.as_str()
        )),
        "warnings are not in stable contract order: {:?}",
        first.warnings
    );
    assert!(
        first
            .warnings
            .iter()
            .all(|warning| warning.code.as_str() == "over_redaction")
    );
}

#[test]
fn redaction_preserves_geometry_link_text_and_unrelated_vector_graphics() {
    let mut doc = Document::load_mem(&pdf_with_operations(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 720.into()]),
        Operation::new("Tj", vec![Object::string_literal("PRESERVE-SECRET-7ad1")]),
        Operation::new("ET", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 20.into()]),
        Operation::new("Td", vec![72.into(), 120.into()]),
        Operation::new("Tj", vec![Object::string_literal("PRESERVE-KEEP-18ce")]),
        Operation::new("ET", vec![]),
        Operation::new("re", vec![401.125.into(), 100.into(), 50.into(), 30.into()]),
        Operation::new("S", vec![]),
    ]))
    .expect("load preservation fixture");
    let page_id = *doc.get_pages().values().next().expect("preservation page");
    let link_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => vec![400.into(), 90.into(), 500.into(), 140.into()],
        "A" => dictionary! {
            "S" => "URI",
            "URI" => Object::string_literal("https://example.test/kept-link"),
        },
    });
    doc.get_dictionary_mut(page_id)
        .expect("preservation page dictionary")
        .set("Annots", vec![link_id.into()]);
    let mut input = Vec::new();
    doc.save_to(&mut input)
        .expect("serialize preservation fixture");
    let selected = area_for_region(0, 0, 60.0, 330.0, 700.0, 750.0);
    let output = redact_pure(&input, &[selected]).expect("redact preservation fixture");
    let extracted = extract_text(&output.pdf).expect("extract preservation output");
    let reopened = Document::load_mem(&output.pdf).expect("reopen preservation output");
    let output_page_id = *reopened.get_pages().values().next().expect("output page");
    let output_page = reopened
        .get_dictionary(output_page_id)
        .expect("output page dictionary");

    assert!(!extracted.contains("PRESERVE-SECRET-7ad1"));
    assert!(!bytes_contain(&output.pdf, "PRESERVE-SECRET-7ad1"));
    assert!(extracted.contains("PRESERVE-KEEP-18ce"));
    assert!(bytes_contain(&output.pdf, "https://example.test/kept-link"));
    assert!(bytes_contain(&output.pdf, "401.125"));
    assert_eq!(
        output_page.get(b"MediaBox").expect("output MediaBox"),
        &Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()])
    );
    assert!(
        output_page.get(b"Annots").is_ok(),
        "unrelated link disappeared"
    );
    assert_eq!(
        validate(&output.pdf)
            .expect("validate preservation output")
            .page_count,
        1
    );
}

fn mock_pdf_paths() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/samples");
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

fn generated_coverage_count(pdf: &[u8]) -> usize {
    let operations = page_operations(pdf);
    let black = operations
        .iter()
        .rposition(|operation| operation.operator == "rg")
        .expect("generated black coverage color");
    operations[black + 1..]
        .windows(2)
        .filter(|window| window[0].operator == "re" && window[1].operator == "f")
        .count()
}

#[derive(Debug, PartialEq, Eq)]
enum RedactionJsonError {
    Syntax,
    InvalidPage,
    InvalidNumber,
    DuplicateField,
    MissingField,
    UnknownField,
    TrailingData,
}

struct JsonCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> JsonCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_whitespace();
        self.bytes.get(self.offset).copied()
    }

    fn expect(&mut self, expected: u8) -> Result<(), RedactionJsonError> {
        self.skip_whitespace();
        if self.bytes.get(self.offset) != Some(&expected) {
            return Err(RedactionJsonError::Syntax);
        }
        self.offset += 1;
        Ok(())
    }

    fn string(&mut self) -> Result<&'a str, RedactionJsonError> {
        self.expect(b'"')?;
        let start = self.offset;
        while let Some(byte) = self.bytes.get(self.offset) {
            match byte {
                b'"' => {
                    let value = std::str::from_utf8(&self.bytes[start..self.offset])
                        .map_err(|_| RedactionJsonError::Syntax)?;
                    self.offset += 1;
                    return Ok(value);
                }
                b'\\' | 0..=0x1f => return Err(RedactionJsonError::Syntax),
                _ => self.offset += 1,
            }
        }
        Err(RedactionJsonError::Syntax)
    }

    fn number(&mut self) -> Result<f64, RedactionJsonError> {
        self.skip_whitespace();
        let start = self.offset;
        while self.bytes.get(self.offset).is_some_and(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
        }) {
            self.offset += 1;
        }
        if start == self.offset {
            return Err(RedactionJsonError::InvalidNumber);
        }
        std::str::from_utf8(&self.bytes[start..self.offset])
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .ok_or(RedactionJsonError::InvalidNumber)
    }
}

fn parse_redaction_rect(
    cursor: &mut JsonCursor<'_>,
    page_index: u32,
) -> Result<RedactArea, RedactionJsonError> {
    cursor.expect(b'{')?;
    let mut values = BTreeMap::new();
    loop {
        if cursor.peek() == Some(b'}') {
            cursor.expect(b'}')?;
            break;
        }
        let field = cursor.string()?;
        cursor.expect(b':')?;
        let value = cursor.number()?;
        if !matches!(field, "left" | "top" | "width" | "height") {
            return Err(RedactionJsonError::UnknownField);
        }
        if values.insert(field, value).is_some() {
            return Err(RedactionJsonError::DuplicateField);
        }
        match cursor.peek() {
            Some(b',') => cursor.expect(b',')?,
            Some(b'}') => {}
            _ => return Err(RedactionJsonError::Syntax),
        }
    }
    let value = |field| {
        values
            .get(field)
            .copied()
            .ok_or(RedactionJsonError::MissingField)
    };
    Ok(area(
        page_index,
        value("left")?,
        value("top")?,
        value("width")?,
        value("height")?,
    ))
}

fn parse_legacy_redactions(bytes: &[u8]) -> Result<Vec<RedactArea>, RedactionJsonError> {
    let mut cursor = JsonCursor::new(bytes);
    let mut areas = Vec::new();
    let mut pages = BTreeSet::new();
    cursor.expect(b'{')?;
    loop {
        if cursor.peek() == Some(b'}') {
            cursor.expect(b'}')?;
            break;
        }
        let page_index = cursor
            .string()?
            .parse::<u32>()
            .map_err(|_| RedactionJsonError::InvalidPage)?;
        if !pages.insert(page_index) {
            return Err(RedactionJsonError::InvalidPage);
        }
        cursor.expect(b':')?;
        cursor.expect(b'[')?;
        loop {
            if cursor.peek() == Some(b']') {
                cursor.expect(b']')?;
                break;
            }
            areas.push(parse_redaction_rect(&mut cursor, page_index)?);
            match cursor.peek() {
                Some(b',') => cursor.expect(b',')?,
                Some(b']') => {}
                _ => return Err(RedactionJsonError::Syntax),
            }
        }
        match cursor.peek() {
            Some(b',') => cursor.expect(b',')?,
            Some(b'}') => {}
            _ => return Err(RedactionJsonError::Syntax),
        }
    }
    cursor.skip_whitespace();
    if cursor.offset != cursor.bytes.len() {
        return Err(RedactionJsonError::TrailingData);
    }
    if areas.is_empty() {
        return Err(RedactionJsonError::MissingField);
    }
    Ok(areas)
}

#[derive(Debug)]
struct LegacyParityBlocker {
    fixture_id: String,
    reason_code: String,
    justification: String,
    user_visible_effect: String,
}

fn parse_legacy_parity_blocker(
    cursor: &mut JsonCursor<'_>,
) -> Result<LegacyParityBlocker, RedactionJsonError> {
    cursor.expect(b'{')?;
    let mut values = BTreeMap::new();
    loop {
        if cursor.peek() == Some(b'}') {
            cursor.expect(b'}')?;
            break;
        }
        let field = cursor.string()?;
        cursor.expect(b':')?;
        let value = cursor.string()?.to_string();
        if !matches!(
            field,
            "fixture_id"
                | "reason_code"
                | "justification"
                | "user_visible_effect"
                | "tracking_note"
        ) {
            return Err(RedactionJsonError::UnknownField);
        }
        if values.insert(field, value).is_some() {
            return Err(RedactionJsonError::DuplicateField);
        }
        match cursor.peek() {
            Some(b',') => cursor.expect(b',')?,
            Some(b'}') => {}
            _ => return Err(RedactionJsonError::Syntax),
        }
    }
    let mut take = |field| values.remove(field).ok_or(RedactionJsonError::MissingField);
    Ok(LegacyParityBlocker {
        fixture_id: take("fixture_id")?,
        reason_code: take("reason_code")?,
        justification: take("justification")?,
        user_visible_effect: take("user_visible_effect")?,
    })
}

fn parse_legacy_parity_allowlist(
    bytes: &[u8],
) -> Result<Vec<LegacyParityBlocker>, RedactionJsonError> {
    let mut cursor = JsonCursor::new(bytes);
    let mut schema_version = None;
    let mut blockers = Vec::new();
    let mut saw_blocker_list = false;
    cursor.expect(b'{')?;
    loop {
        if cursor.peek() == Some(b'}') {
            cursor.expect(b'}')?;
            break;
        }
        let field = cursor.string()?;
        cursor.expect(b':')?;
        match field {
            "schema_version" => {
                if schema_version.replace(cursor.number()?).is_some() {
                    return Err(RedactionJsonError::DuplicateField);
                }
            }
            "blockers" | "allowlisted" | "known_failures" => {
                if field == "blockers" && saw_blocker_list {
                    return Err(RedactionJsonError::DuplicateField);
                }
                saw_blocker_list = true;
                cursor.expect(b'[')?;
                loop {
                    if cursor.peek() == Some(b']') {
                        cursor.expect(b']')?;
                        break;
                    }
                    blockers.push(parse_legacy_parity_blocker(&mut cursor)?);
                    match cursor.peek() {
                        Some(b',') => cursor.expect(b',')?,
                        Some(b']') => {}
                        _ => return Err(RedactionJsonError::Syntax),
                    }
                }
            }
            _ => return Err(RedactionJsonError::UnknownField),
        }
        match cursor.peek() {
            Some(b',') => cursor.expect(b',')?,
            Some(b'}') => {}
            _ => return Err(RedactionJsonError::Syntax),
        }
    }
    cursor.skip_whitespace();
    if cursor.offset != cursor.bytes.len() {
        return Err(RedactionJsonError::TrailingData);
    }
    if !matches!(schema_version, Some(1.0 | 2.0)) {
        return Err(RedactionJsonError::InvalidNumber);
    }
    saw_blocker_list
        .then_some(blockers)
        .ok_or(RedactionJsonError::MissingField)
}

fn source_lexemes_removed_by_oracle(
    input_text: &str,
    removed_by_oracle: &BTreeSet<String>,
) -> BTreeSet<String> {
    input_text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 4)
        .filter(|word| removed_by_oracle.contains(&word.to_lowercase()))
        .map(str::to_string)
        .collect()
}

fn operation_show_bytes(operation: &Operation) -> Option<Vec<u8>> {
    fn append_strings(object: &Object, bytes: &mut Vec<u8>) {
        match object {
            Object::String(value, _) => bytes.extend_from_slice(value),
            Object::Array(values) => {
                for value in values {
                    append_strings(value, bytes);
                }
            }
            _ => {}
        }
    }

    let operands = match operation.operator.as_str() {
        "Tj" | "TJ" | "'" => operation.operands.as_slice(),
        "\"" => operation.operands.get(2..).unwrap_or_default(),
        _ => return None,
    };
    let mut bytes = Vec::new();
    for operand in operands {
        append_strings(operand, &mut bytes);
    }
    Some(bytes)
}

fn decoded_graph_show_sequences(pdf: &[u8]) -> Vec<Vec<u8>> {
    let document = Document::load_mem(pdf).expect("load decoded-graph PDF");
    document
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .filter_map(|stream| stream.decompressed_content().ok())
        .filter_map(|bytes| Content::decode(&bytes).ok())
        .flat_map(|content| content.operations)
        .filter_map(|operation| operation_show_bytes(&operation))
        .filter(|bytes| !bytes.is_empty())
        .collect()
}

fn pdfium_page_text_objects(pdf: &[u8]) -> Vec<String> {
    let pdfium = pdf_core::pdfium::lock().expect("lock pdfium for text objects");
    let document = pdfium
        .load_pdf_from_byte_slice(pdf, None)
        .expect("load text-object PDF");
    let mut result = Vec::new();
    for page_index in document.pages().as_range() {
        let page = document.pages().get(page_index).expect("load page");
        let text_objects = page
            .objects()
            .create_group(|object| object.as_text_object().is_some())
            .expect("collect text objects");
        result.extend(text_objects.iter().filter_map(|object| {
            object
                .as_text_object()
                .map(|text_object| text_object.text())
        }));
    }
    result
}

fn encoded_graph_probes_removed_by_oracle(
    input_pdf: &[u8],
    removed_by_oracle: &BTreeSet<String>,
) -> Vec<Vec<u8>> {
    let text_objects = pdfium_page_text_objects(input_pdf);
    let show_sequences: Vec<Vec<u8>> = page_operations(input_pdf)
        .iter()
        .filter_map(operation_show_bytes)
        .filter(|bytes| !bytes.is_empty())
        .collect();
    assert_eq!(
        text_objects.len(),
        show_sequences.len(),
        "fixture text-object/show-operation mapping changed"
    );
    text_objects
        .into_iter()
        .zip(show_sequences)
        .filter(|(text, _)| {
            words(text)
                .iter()
                .any(|word| removed_by_oracle.contains(word))
        })
        .map(|(_, sequence)| sequence)
        .collect()
}

#[test]
fn legacy_redaction_json_schema_maps_percent_rectangles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let cases = [
        (
            "file1.redactions.json",
            vec![
                area(0, 10.0, 12.0, 35.0, 6.0),
                area(0, 55.0, 40.0, 25.0, 8.0),
            ],
        ),
        (
            "file2.redactions.json",
            vec![
                area(0, 20.0, 20.0, 40.0, 10.0),
                area(0, 5.0, 80.0, 90.0, 10.0),
            ],
        ),
        (
            "pdf_ppt-file1.redactions.json",
            vec![area(0, 25.0, 30.0, 50.0, 20.0)],
        ),
    ];
    for (file, expected) in cases {
        let bytes = fs::read(
            root.join("tests/fixtures/corpus/legacy-goldens/redaction")
                .join(file),
        )
        .unwrap_or_else(|error| panic!("read {file}: {error}"));
        assert_eq!(parse_legacy_redactions(&bytes), Ok(expected), "map {file}");
    }
}

#[test]
#[serial(pdfium)]
fn frozen_legacy_redaction_fixtures_match_pymupdf_text_semantics() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let allowlist = parse_legacy_parity_allowlist(
        &fs::read(root.join("tests/fixtures/corpus/legacy-parity-allowlist.json"))
            .expect("read legacy parity allowlist"),
    )
    .expect("parse legacy parity allowlist");
    for blocker in &allowlist {
        assert!(!blocker.fixture_id.trim().is_empty());
        assert!(!blocker.reason_code.trim().is_empty());
        assert!(!blocker.justification.trim().is_empty());
        assert!(!blocker.user_visible_effect.trim().is_empty());
    }
    let redaction_blockers: Vec<&LegacyParityBlocker> = allowlist
        .iter()
        .filter(|blocker| blocker.fixture_id.starts_with("redaction/"))
        .collect();
    assert!(
        redaction_blockers.is_empty(),
        "redaction parity must have no allowlisted blockers: {redaction_blockers:?}"
    );
    let cases = [
        (
            "file1",
            "tests/fixtures/samples/pdf/file1.pdf",
            "tests/fixtures/corpus/legacy-goldens/redaction/file1.pymupdf-redacted.pdf",
            "tests/fixtures/corpus/legacy-goldens/redaction/file1.redactions.json",
        ),
        (
            "file2",
            "tests/fixtures/samples/pdf/file2.pdf",
            "tests/fixtures/corpus/legacy-goldens/redaction/file2.pymupdf-redacted.pdf",
            "tests/fixtures/corpus/legacy-goldens/redaction/file2.redactions.json",
        ),
        (
            "pdf_ppt-file1",
            "tests/fixtures/samples/pdf_ppt/file1.pdf",
            "tests/fixtures/corpus/legacy-goldens/redaction/pdf_ppt-file1.pymupdf-redacted.pdf",
            "tests/fixtures/corpus/legacy-goldens/redaction/pdf_ppt-file1.redactions.json",
        ),
    ];

    let mut parity_failures = Vec::new();
    for (label, input_path, oracle_path, redactions_path) in cases {
        let input = fs::read(root.join(input_path)).expect("read legacy input");
        let oracle = fs::read(root.join(oracle_path)).expect("read PyMuPDF oracle");
        let redactions = fs::read(root.join(redactions_path)).expect("read legacy redactions");
        let areas = parse_legacy_redactions(&redactions)
            .unwrap_or_else(|error| panic!("{label}: map legacy redaction JSON schema: {error:?}"));
        let input_text = extract_text(&input).expect("extract legacy input");
        let input_words = words(&input_text);
        let oracle_words = words(&extract_text(&oracle).expect("extract legacy oracle"));
        let removed_by_oracle: BTreeSet<String> =
            input_words.difference(&oracle_words).cloned().collect();
        let oracle_source_survivors: BTreeSet<String> =
            input_words.intersection(&oracle_words).cloned().collect();

        let output = redact_pure(&input, &areas).unwrap_or_else(|error| {
            panic!("{label}: typed blocker {}: {error}", error.stable_code())
        });
        let output_words = words(&extract_text(&output.pdf).expect("extract pure legacy output"));
        let leaked: Vec<&String> = removed_by_oracle.intersection(&output_words).collect();
        let over_removed: Vec<&String> =
            oracle_source_survivors.difference(&output_words).collect();
        let matched_removals = removed_by_oracle.difference(&output_words).count();
        let surviving_matches = oracle_source_survivors.intersection(&output_words).count();
        let surviving_coverage = if oracle_source_survivors.is_empty() {
            1.0
        } else {
            surviving_matches as f64 / oracle_source_survivors.len() as f64
        };

        assert!(
            leaked.is_empty(),
            "{label}: oracle-removed words survived: {leaked:?}"
        );
        assert_eq!(
            matched_removals,
            removed_by_oracle.len(),
            "{label}: semantic match-count parity"
        );
        assert!(
            !oracle_source_survivors.is_empty() && !output_words.is_empty(),
            "{label}: unrelated searchable text did not survive"
        );
        // Every source word the oracle kept must survive (no over-removal), and the
        // output must carry no word the oracle does not also carry (no under-removal
        // and no invented text). Comparing to `input ∩ oracle` instead of the oracle
        // itself would ignore the partial-word fragments PyMuPDF leaves when a
        // rectangle bisects a word, which the engine is required to reproduce.
        let oracle_words_all = words(&extract_text(&oracle).expect("extract legacy oracle"));
        if !over_removed.is_empty() || output_words != oracle_words_all {
            let unexpected: Vec<&String> = output_words.difference(&oracle_words_all).collect();
            parity_failures.push(format!(
                "{label}: survivor set {surviving_matches}/{}; over-removed={over_removed:?}; not-in-oracle={unexpected:?}",
                oracle_source_survivors.len()
            ));
        }
        let input_page_text = extract_text_pages(&input).expect("extract legacy input pages");
        let oracle_page_text = extract_text_pages(&oracle).expect("extract legacy oracle pages");
        let output_page_text =
            extract_text_pages(&output.pdf).expect("extract pure legacy output pages");
        assert_eq!(output_page_text.len(), input_page_text.len());
        assert_eq!(oracle_page_text.len(), input_page_text.len());
        for page_index in 0..input_page_text.len() {
            // Exact page-level parity with the oracle. Comparing against
            // `input ∩ oracle` would discard the partial-word fragments PyMuPDF
            // leaves behind when a rectangle bisects a word (e.g. `slightly` ->
            // `slig`): those fragments exist in the oracle but not in the source,
            // so they must be reproduced, not treated as over-removal.
            let oracle_page_words = words(&oracle_page_text[page_index]);
            let actual_page_survivors = words(&output_page_text[page_index]);
            if actual_page_survivors != oracle_page_words {
                parity_failures.push(format!(
                    "{label}: survivor placement differs on page {page_index}; actual={actual_page_survivors:?}; expected={oracle_page_words:?}"
                ));
            }
        }
        let graph_probe_candidates =
            source_lexemes_removed_by_oracle(&input_text, &removed_by_oracle);
        let graph_probes: BTreeSet<String> = graph_probe_candidates
            .into_iter()
            .filter(|probe| decoded_graph_content_contains(&input, probe))
            .collect();
        for probe in &graph_probes {
            assert!(
                decoded_graph_content_contains(&input, probe),
                "{label}: input-side decoded graph does not contain probe {probe:?}"
            );
        }
        let encoded_graph_probes =
            encoded_graph_probes_removed_by_oracle(&input, &removed_by_oracle);
        assert!(
            !encoded_graph_probes.is_empty(),
            "{label}: no decoded-graph removal probes derived from oracle"
        );
        let input_show_sequences = decoded_graph_show_sequences(&input);
        let missing_input_encoded_probes = encoded_graph_probes
            .iter()
            .filter(|probe| !input_show_sequences.contains(probe))
            .count();
        assert_eq!(
            missing_input_encoded_probes, 0,
            "{label}: encoded probes must be present in the input decoded graph"
        );
        let graph_leaks: Vec<&String> = graph_probes
            .iter()
            .filter(|probe| decoded_graph_content_contains(&output.pdf, probe))
            .collect();
        if !graph_leaks.is_empty() {
            parity_failures.push(format!(
                "{label}: removed lexemes survived decoded object graph content: {graph_leaks:?}"
            ));
        }
        let output_show_sequences = decoded_graph_show_sequences(&output.pdf);
        let encoded_graph_leaks = encoded_graph_probes
            .iter()
            .filter(|probe| output_show_sequences.contains(probe))
            .count();
        if encoded_graph_leaks != 0 {
            parity_failures.push(format!(
                "{label}: {encoded_graph_leaks} oracle-selected encoded text objects survived decoded object graph"
            ));
        }
        assert!(
            generated_coverage_count(&output.pdf) >= areas.len(),
            "{label}: not every requested area produced black coverage"
        );
        let input_sizes = page_sizes(&input).expect("input page geometry");
        let output_sizes = page_sizes(&output.pdf).expect("output page geometry");
        let oracle_sizes = page_sizes(&oracle).expect("oracle page geometry");
        assert_eq!(
            output_sizes, input_sizes,
            "{label}: pure redaction changed page geometry"
        );
        assert_eq!(
            oracle_sizes, input_sizes,
            "{label}: frozen oracle changed page geometry"
        );
        assert_eq!(
            validate(&output.pdf)
                .expect("validate legacy output")
                .page_count,
            1,
            "{label}: output must reopen with one page"
        );
        assert_eq!(
            validate(&oracle)
                .expect("validate legacy oracle")
                .page_count,
            validate(&input).expect("validate legacy input").page_count,
            "{label}: oracle page-count parity"
        );
        eprintln!(
            "LEGACY REDACTION {label}: areas={}; matched-removals={}/{}; surviving-source-words={}/{} ({surviving_coverage:.4}); graph-probes={}+{}; pages=1; geometry={:?}; warnings={}",
            areas.len(),
            matched_removals,
            removed_by_oracle.len(),
            surviving_matches,
            oracle_source_survivors.len(),
            graph_probes.len(),
            encoded_graph_probes.len(),
            output_sizes[0],
            output.warnings.len(),
        );
    }
    assert!(
        parity_failures.is_empty(),
        "legacy redaction parity failures:\n{}",
        parity_failures.join("\n")
    );
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
        corpus_size, 11,
        "cross-validation corpus unexpectedly changed"
    );
    assert_eq!(compared_pdfs.len(), 10, "unexpected comparable PDF count");
    assert_eq!(comparison_cases, 20, "each supported PDF needs two cases");
    assert_eq!(
        pure_failed_closed.len(),
        1,
        "only the deliberate predefined non-Identity CMap fixture should fail closed"
    );
    assert_eq!(
        pure_failed_closed_cases, 2,
        "the deliberate predefined CMap must fail closed for both cases"
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

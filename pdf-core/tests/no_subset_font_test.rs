#![cfg(feature = "pdfium")]

use lopdf::Document;
use pdf_core::embed_font::register_font;
use pdf_core::extract::extract_text;
use pdf_core::generate::{DocOptions, text_to_pdf};
use pdf_core::meta::validate;
use ttf_parser::Tag;

const CYRILLIC: &str = "Конфиденциально";

#[test]
fn no_subsetting_license_retains_full_font_and_roundtrips_text() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../packages/doc-yantra-fonts/fonts/LiberationSans-Regular.ttf"
    );
    let mut bytes = std::fs::read(path).expect("read Liberation Sans");
    let table_offset = {
        let face = ttf_parser::Face::parse(&bytes, 0).expect("parse Liberation Sans");
        let table = face
            .raw_face()
            .table(Tag::from_bytes(b"OS/2"))
            .expect("Liberation Sans has OS/2");
        table.as_ptr() as usize - bytes.as_ptr() as usize
    };
    bytes[table_offset + 8..table_offset + 10].copy_from_slice(&0x0100_u16.to_be_bytes());
    register_font(&bytes).expect("no-subsetting license permits full-font embedding");

    let output = text_to_pdf(CYRILLIC, &DocOptions::default()).expect("render Cyrillic");
    validate(&output).expect("full-font output validates");
    assert!(
        extract_text(&output)
            .expect("extract full-font PDF")
            .contains(CYRILLIC)
    );

    let document = Document::load_mem(&output).expect("load full-font PDF");
    let font_programs = document
        .objects
        .values()
        .filter(|object| object.as_dict().is_ok_and(|dict| dict.has(b"FontFile2")))
        .count();
    assert_eq!(font_programs, 1);
}

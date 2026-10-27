//! Embedded PDF font-family inspection for fidelity assertions.

use anyhow::{Context, Result};
use lopdf::{Document, Object};
use std::collections::BTreeSet;

pub(crate) fn embedded_font_families(pdf: &[u8]) -> Result<BTreeSet<String>> {
    let document = Document::load_mem(pdf).context("parse PDF for embedded-font inspection")?;
    let mut families = BTreeSet::new();
    for object in document.objects.values() {
        let Ok(descriptor) = object.as_dict() else {
            continue;
        };
        let Some(font_file) = [b"FontFile".as_slice(), b"FontFile2", b"FontFile3"]
            .into_iter()
            .find_map(|key| descriptor.get(key).ok())
        else {
            continue;
        };
        let bytes = resolve_object(&document, font_file)
            .and_then(|object| object.as_stream().ok())
            .and_then(|stream| stream.decompressed_content().ok());
        let family = bytes
            .as_deref()
            .and_then(sfnt_family)
            .or_else(|| descriptor_name(descriptor));
        if let Some(family) = family.and_then(|family| normalize_family(&family)) {
            families.insert(family);
        }
    }
    Ok(families)
}

fn resolve_object<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => document.get_object(*id).ok(),
        _ => Some(object),
    }
}

fn descriptor_name(descriptor: &lopdf::Dictionary) -> Option<String> {
    descriptor
        .get(b"FontFamily")
        .ok()
        .and_then(pdf_string)
        .or_else(|| descriptor.get(b"FontName").ok().and_then(pdf_string))
}

fn pdf_string(object: &Object) -> Option<String> {
    match object {
        Object::Name(value) | Object::String(value, _) => {
            Some(String::from_utf8_lossy(value).into_owned())
        }
        _ => None,
    }
}

fn sfnt_family(bytes: &[u8]) -> Option<String> {
    let face_count = ttf_parser::fonts_in_collection(bytes).unwrap_or(1);
    (0..face_count).find_map(|index| {
        let face = ttf_parser::Face::parse(bytes, index).ok()?;
        [
            ttf_parser::name_id::TYPOGRAPHIC_FAMILY,
            ttf_parser::name_id::WWS_FAMILY,
            ttf_parser::name_id::FAMILY,
            ttf_parser::name_id::FULL_NAME,
            ttf_parser::name_id::POST_SCRIPT_NAME,
        ]
        .into_iter()
        .find_map(|name_id| {
            face.names()
                .into_iter()
                .filter(|name| name.name_id == name_id)
                .filter_map(|name| name.to_string())
                .find(|name| !name.trim().is_empty())
        })
    })
}

fn normalize_family(value: &str) -> Option<String> {
    let mut value = value.trim().trim_start_matches('/');
    if value.len() > 7
        && value.as_bytes()[6] == b'+'
        && value.as_bytes()[..6]
            .iter()
            .all(|byte| byte.is_ascii_uppercase())
    {
        value = &value[7..];
    }
    value = value
        .split_once([',', '-'])
        .map_or(value, |(family, _)| family);
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Document, Object, Stream, dictionary};

    const CARLITO: &[u8] = include_bytes!("../../fonts/carlito/Carlito-Regular.ttf");

    fn pdf_with_embedded_font(base_font: &str, bytes: Option<&[u8]>) -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        let mut descriptor = dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => Object::Name(base_font.as_bytes().to_vec()),
        };
        if let Some(bytes) = bytes {
            let file = document.add_object(Stream::new(dictionary! {}, bytes.to_vec()));
            descriptor.set("FontFile2", file);
        }
        let descriptor = document.add_object(descriptor);
        let font = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => Object::Name(base_font.as_bytes().to_vec()),
            "FontDescriptor" => descriptor,
        });
        let resources = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font },
        });
        let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
        let pages = document.new_object_id();
        let page = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages,
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => resources,
            "Contents" => content,
        });
        document.objects.insert(
            pages,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page.into()],
                "Count" => 1,
            }),
        );
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let mut pdf = Vec::new();
        document.save_to(&mut pdf).expect("save font fixture PDF");
        pdf
    }

    #[test]
    fn strips_subset_and_style_suffixes_from_pdf_font_names() {
        let pdf = pdf_with_embedded_font("ABCDEF+Carlito-BoldItalic", Some(b"not-an-sfnt"));
        assert_eq!(
            embedded_font_families(&pdf).expect("inspect font fixture"),
            BTreeSet::from(["Carlito".to_string()])
        );
    }

    #[test]
    fn reads_the_real_family_from_an_embedded_sfnt_name_table() {
        let pdf = pdf_with_embedded_font("OfficeCoreSubset", Some(CARLITO));
        assert_eq!(
            embedded_font_families(&pdf).expect("inspect embedded Carlito"),
            BTreeSet::from(["Carlito".to_string()])
        );
    }
}

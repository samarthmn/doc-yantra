//! Fixture builders shared by unit and integration tests. Compiled into the
//! library so `tests/*.rs` can use them; not part of the stable public API.

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};

/// Builds a minimal valid PDF (US Letter, 612x792) with one page per entry in
/// `page_texts`, each drawing its text in Helvetica 24pt at (72, 720).
pub fn sample_pdf(page_texts: &[&str]) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut kids: Vec<Object> = Vec::new();
    for text in page_texts {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(*text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("encode content"),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
        });
        kids.push(page_id.into());
    }

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

    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("serialize sample pdf");
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_pdf_is_loadable() {
        let bytes = sample_pdf(&["Page one", "Page two"]);
        let doc = Document::load_mem(&bytes).expect("load sample");
        assert_eq!(doc.get_pages().len(), 2);
    }
}

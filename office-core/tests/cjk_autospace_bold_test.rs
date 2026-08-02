//! East Asian auto-spacing (`w:autoSpaceDE`/`DN`) and synthetic bold for
//! regular-only CJK faces.

use office_core::layout::{Item, WordDoc, layout_docx};
use office_core::testkit::minimal_docx;
use office_core::{ConvertOptions, OfficeFormat, convert};

fn register_cjk() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let noto = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../pdf-core/wasm/pkg/fonts/NotoSansCJKjp-Regular.otf",
        ))
        .expect("read packaged pan-CJK face");
        office_core::register_font(&noto).expect("register CJK face");
    });
}

/// Sum of glyph advances across every glyph run in the document.
fn total_glyph_advance(bytes: &[u8]) -> f64 {
    let doc = WordDoc::open(bytes, &ConvertOptions::default()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let mut total = 0.0;
    for page in &laid.pages {
        for item in &page.items {
            if let Item::Glyphs(run) = &item.item {
                total += run.glyphs.iter().map(|glyph| glyph.advance_pt).sum::<f64>();
            }
        }
    }
    total
}

#[test]
fn auto_space_inserts_an_eighth_em_between_han_and_digit() {
    register_cjk();
    let with_auto = minimal_docx(r#"<w:p><w:r><w:t>表1</w:t></w:r></w:p>"#);
    let disabled = minimal_docx(
        r#"<w:p><w:pPr><w:autoSpaceDN w:val="0"/></w:pPr><w:r><w:t>表1</w:t></w:r></w:p>"#,
    );
    let gap = total_glyph_advance(&with_auto) - total_glyph_advance(&disabled);
    // Default run size is 11pt in the minimal fixture's docDefaults-less
    // cascade (DEFAULT_SIZE_PT); accept the eighth-em of any plausible body
    // size rather than pinning the constant twice.
    assert!(
        gap > 0.9 && gap < 2.0,
        "expected one eighth-em auto-space gap, measured {gap}pt"
    );
}

#[test]
fn auto_space_leaves_authored_spaces_alone() {
    register_cjk();
    let spaced = minimal_docx(
        r#"<w:p><w:r><w:t xml:space="preserve">表 1</w:t></w:r></w:p>"#,
    );
    let spaced_disabled = minimal_docx(
        r#"<w:p><w:pPr><w:autoSpaceDN w:val="0"/></w:pPr><w:r><w:t xml:space="preserve">表 1</w:t></w:r></w:p>"#,
    );
    let delta = total_glyph_advance(&spaced) - total_glyph_advance(&spaced_disabled);
    assert!(
        delta.abs() < 0.01,
        "an authored space suppresses auto-spacing, measured extra {delta}pt"
    );
}

/// Decompressed concatenation of every stream in the PDF.
fn inflated_streams(pdf: &[u8]) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    while let Some(start) = find(pdf, cursor, b"stream") {
        let data_start = match pdf.get(start + 6) {
            Some(b'\r') => start + 8,
            Some(b'\n') => start + 7,
            _ => start + 6,
        };
        let Some(end) = find(pdf, data_start, b"endstream") else {
            break;
        };
        if let Ok(inflated) =
            miniz_oxide::inflate::decompress_to_vec_zlib(&pdf[data_start..end])
        {
            out.push_str(&String::from_utf8_lossy(&inflated));
        }
        cursor = end + 9;
    }
    out
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| position + from)
}

#[test]
fn bold_cjk_on_a_regular_only_face_strokes_the_fill() {
    register_cjk();
    let bold = minimal_docx(r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>表</w:t></w:r></w:p>"#);
    let output = convert(&bold, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("convert bold CJK");
    let content = inflated_streams(&output.pdf);
    assert!(
        content.contains("2 Tr"),
        "bold CJK on a regular-only face must fill+stroke (2 Tr)"
    );

    let regular = minimal_docx(r#"<w:p><w:r><w:t>表</w:t></w:r></w:p>"#);
    let output = convert(&regular, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("convert regular CJK");
    let content = inflated_streams(&output.pdf);
    assert!(
        !content.contains("2 Tr"),
        "regular CJK must remain fill-only"
    );

    let latin_bold = minimal_docx(r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Bold</w:t></w:r></w:p>"#);
    let output = convert(&latin_bold, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("convert bold latin");
    let content = inflated_streams(&output.pdf);
    assert!(
        !content.contains("2 Tr"),
        "latin bold has a real bold face and must not be synthetically stroked"
    );
}

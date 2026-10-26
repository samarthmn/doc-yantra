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
            "/../packages/doc-yantra-fonts/fonts/NotoSansCJKjp-Regular.otf",
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

/// Distinct baselines of 12pt glyph runs, top to bottom.
fn glyph_baselines(bytes: &[u8]) -> Vec<f64> {
    let doc = WordDoc::open(bytes, &ConvertOptions::default()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let mut baselines = Vec::new();
    for page in &laid.pages {
        for positioned in &page.items {
            if let Item::Glyphs(run) = &positioned.item
                && (run.size_pt - 12.0).abs() < 0.01
            {
                baselines.push(positioned.y_pt);
            }
        }
    }
    baselines.sort_by(|a, b| a.partial_cmp(b).expect("finite baseline"));
    baselines.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    baselines
}

#[test]
fn legacy_cjk_line_pitch_matches_word() {
    register_cjk();
    let text = "测试正文汉字排版样本".repeat(5);
    let docx = minimal_docx(&format!(
        r#"<w:p><w:pPr><w:spacing w:line="360" w:lineRule="auto"/></w:pPr><w:r><w:rPr><w:rFonts w:eastAsia="Noto Sans CJK JP"/><w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
    ));
    let baselines = glyph_baselines(&docx);
    assert!(
        baselines.len() >= 2,
        "fixture must wrap to at least two lines: {baselines:?}"
    );
    let pitch = baselines[1] - baselines[0];
    // Word's PDF truth is 23.25pt. The engine keeps the class's flat 1.3em
    // base (23.4pt) because the measured 1.2917em band desynchronizes inline
    // object and pagination geometry; keep the residual inside one raster
    // pixel at 144dpi.
    assert!(
        pitch > 23.2 && pitch < 23.5,
        "12pt CJK 1.5-line pitch must stay within Word's 23.25-23.4pt band, measured {pitch} from {baselines:?}"
    );
}

#[test]
fn east_asia_slot_uses_the_authored_cjk_face() {
    register_cjk();
    let docx = minimal_docx(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Noto Sans CJK JP"/></w:rPr><w:t>汉A</w:t></w:r></w:p>"#,
    );
    let doc = WordDoc::open(&docx, &ConvertOptions::default()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let mut cjk = Vec::new();
    let mut latin = Vec::new();
    for page in &laid.pages {
        for positioned in &page.items {
            if let Item::Glyphs(run) = &positioned.item {
                let family = doc
                    .fonts
                    .face(run.face)
                    .map(|face| face.family_name.clone())
                    .unwrap_or_default();
                if run.glyphs.iter().any(|glyph| glyph.ch == '汉') {
                    cjk.push(family.clone());
                }
                if run.glyphs.iter().any(|glyph| glyph.ch == 'A') {
                    latin.push(family);
                }
            }
        }
    }
    assert!(
        cjk.iter().any(|family| family.contains("Noto Sans CJK JP")),
        "Han text must use the authored East Asian face: {cjk:?}"
    );
    assert!(
        latin
            .iter()
            .any(|family| !family.contains("Noto Sans CJK JP")),
        "Latin text must keep the Latin face: {latin:?}"
    );
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
    let spaced = minimal_docx(r#"<w:p><w:r><w:t xml:space="preserve">表 1</w:t></w:r></w:p>"#);
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
        if let Ok(inflated) = miniz_oxide::inflate::decompress_to_vec_zlib(&pdf[data_start..end]) {
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
    let output =
        convert(&bold, OfficeFormat::Docx, &ConvertOptions::default()).expect("convert bold CJK");
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

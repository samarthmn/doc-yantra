//! Integration tests for `pdf_core::redact` — true redaction semantics.
//!
//! Verification uses pdfium text extraction (`page.text().all()`) and pixel
//! sampling directly in this file; the `extract` module is owned elsewhere.

#![cfg(feature = "pdfium")]

use std::collections::BTreeMap;

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use pdfium_render::prelude::*;
use serial_test::serial;

use pdf_core::error::PdfCoreError;
use pdf_core::pdfium::lock;
use pdf_core::redact::{RedactionRect, redact};
use pdf_core::testkit::sample_pdf;

/// The rect from the task spec: covers the sample text at (72, 720) 24pt on a
/// 612x792 page (x 30.6..397.8 pts, y 657.36..776.16 pts).
fn covering_rect() -> RedactionRect {
    RedactionRect {
        left_pct: 5.0,
        top_pct: 2.0,
        width_pct: 60.0,
        height_pct: 15.0,
    }
}

fn extract_page_text(pdf: &[u8], page_index: i32) -> String {
    let pdfium = lock().expect("bind pdfium");
    let doc = pdfium
        .load_pdf_from_byte_slice(pdf, None)
        .expect("load pdf");
    let page = doc.pages().get(page_index).expect("get page");
    page.text().expect("page text").all()
}

/// Leak walker: scans raw file bytes, every string value in every object, and
/// every stream's raw AND decompressed content for `needle`. Returns a
/// description of each place the needle survives.
fn leak_hits(pdf: &[u8], needle: &str) -> Vec<String> {
    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }
    fn scan(obj: &Object, needle: &str, path: &str, hits: &mut Vec<String>) {
        match obj {
            Object::String(bytes, _) => {
                if contains(bytes, needle) {
                    hits.push(format!("{path} <string>"));
                }
            }
            Object::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    scan(item, needle, &format!("{path}[{i}]"), hits);
                }
            }
            Object::Dictionary(dict) => {
                for (key, value) in dict.iter() {
                    scan(
                        value,
                        needle,
                        &format!("{path}/{}", String::from_utf8_lossy(key)),
                        hits,
                    );
                }
            }
            Object::Stream(stream) => {
                for (key, value) in stream.dict.iter() {
                    scan(
                        value,
                        needle,
                        &format!("{path}/{}", String::from_utf8_lossy(key)),
                        hits,
                    );
                }
                if contains(&stream.content, needle) {
                    hits.push(format!("{path} <raw stream>"));
                }
                if let Ok(data) = stream.decompressed_content()
                    && contains(&data, needle)
                {
                    hits.push(format!("{path} <decompressed stream>"));
                }
            }
            _ => {}
        }
    }

    let mut hits = Vec::new();
    if contains(pdf, needle) {
        hits.push("RAW FILE BYTES".to_string());
    }
    let Ok(doc) = Document::load_mem(pdf) else {
        hits.push("UNPARSEABLE OUTPUT".to_string());
        return hits;
    };
    for (id, obj) in &doc.objects {
        scan(obj, needle, &format!("obj {} {}", id.0, id.1), &mut hits);
    }
    hits
}

/// Finishes a lopdf document that already has its pages under `pages_id`.
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
    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("serialize fixture");
    buf
}

/// One US-Letter page drawing each `(text, x, y)` in Helvetica 24pt, with an
/// optional /Rotate value on the page dictionary.
fn pdf_with_texts(texts: &[(&str, i64, i64)], rotate: Option<i64>) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let mut operations = vec![];
    for (text, x, y) in texts {
        operations.extend([
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![(*x).into(), (*y).into()]),
            Operation::new("Tj", vec![Object::string_literal(*text)]),
            Operation::new("ET", vec![]),
        ]);
    }
    let content = Content { operations };
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("encode content"),
    ));
    let mut page_dict = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
    };
    if let Some(deg) = rotate {
        page_dict.set("Rotate", deg);
    }
    let page_id = doc.add_object(page_dict);
    finish_doc(doc, pages_id, vec![page_id.into()])
}

/// One page whose only content is `/X1 Do` invoking a Form XObject that draws
/// the secret text at (72, 720) in Helvetica 24pt.
fn xobject_pdf(secret: &str) -> Vec<u8> {
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
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal(secret)]),
            Operation::new("ET", vec![]),
        ],
    };
    let form_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
        },
        form_content.encode().expect("encode form"),
    ));
    let page_content = Content {
        operations: vec![
            Operation::new("q", vec![]),
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
        "Resources" => dictionary! {
            "XObject" => dictionary! { "X1" => form_id },
        },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

/// One page with an empty content stream and a FreeText annotation at
/// /Rect [72 690 300 740] whose /Contents holds the secret.
fn annot_pdf(secret: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let annot_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "FreeText",
        "Rect" => vec![72.into(), 690.into(), 300.into(), 740.into()],
        "Contents" => Object::string_literal(secret),
        "DA" => Object::string_literal("/Helv 12 Tf 0 g"),
    });
    let content_id = doc.add_object(Stream::new(dictionary! {}, b"".to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Annots" => vec![annot_id.into()],
        "Resources" => dictionary! {},
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

/// Page with a Helvetica secret at (72, 720) and an untouched Type3-font line
/// at (72, 400) — pdfium's content regeneration cannot re-serialize Type3
/// text, so this exercises the post-save integrity check.
fn type3_pdf(secret: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let helv_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let charproc_id = doc.add_object(Stream::new(
        dictionary! {},
        b"750 0 0 0 750 750 d1 0 0 750 750 re f".to_vec(),
    ));
    let t3_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "FontBBox" => vec![0.into(), 0.into(), 750.into(), 750.into()],
        "FontMatrix" => vec![
            0.001.into(),
            0.into(),
            0.into(),
            0.001.into(),
            0.into(),
            0.into(),
        ],
        "CharProcs" => dictionary! { "square" => charproc_id },
        "Encoding" => dictionary! {
            "Type" => "Encoding",
            "Differences" => vec![97.into(), "square".into()],
        },
        "FirstChar" => 97,
        "LastChar" => 97,
        "Widths" => vec![750.into()],
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal(secret)]),
            Operation::new("ET", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F3".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 400.into()]),
            Operation::new("Tj", vec![Object::string_literal("aaa")]),
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
            "Font" => dictionary! { "F1" => helv_id, "F3" => t3_id },
        },
    });
    finish_doc(doc, pages_id, vec![page_id.into()])
}

const UW: f64 = 612.0; // unrotated US-Letter width
const UH: f64 = 792.0; // unrotated US-Letter height

/// Forward-maps an unrotated user-space point to displayed coordinates
/// (bottom-left origin), returning (dx, dy, disp_w, disp_h). Matches pdfium's
/// rendering of /Rotate, verified empirically (90 deg: display = (y, W - x)).
fn to_display(rot_deg: i64, x: f64, y: f64) -> (f64, f64, f64, f64) {
    match rot_deg {
        0 => (x, y, UW, UH),
        90 => (y, UW - x, UH, UW),
        180 => (UW - x, UH - y, UW, UH),
        270 => (UH - y, x, UH, UW),
        _ => unreachable!("unsupported test rotation {rot_deg}"),
    }
}

/// Builds the percent rect a UI would send for the unrotated user-space
/// region (l..r, b..t) selected on the page AS DISPLAYED at `rot_deg`.
fn displayed_rect_for_region(rot_deg: i64, l: f64, r: f64, b: f64, t: f64) -> RedactionRect {
    let (dx0, dy0, dw, dh) = to_display(rot_deg, l, b);
    let (dx1, dy1, _, _) = to_display(rot_deg, r, t);
    let (dl, dr) = (dx0.min(dx1), dx0.max(dx1));
    let (db, dt) = (dy0.min(dy1), dy0.max(dy1));
    RedactionRect {
        left_pct: dl / dw * 100.0,
        top_pct: (dh - dt) / dh * 100.0,
        width_pct: (dr - dl) / dw * 100.0,
        height_pct: (dt - db) / dh * 100.0,
    }
}

/// Renders page 0 at 1pt = 1px in DISPLAYED orientation and returns the RGB
/// at the displayed position of unrotated user-space point (x, y).
fn rendered_rgb_at(pdf: &[u8], rot_deg: i64, x: f64, y: f64) -> (u8, u8, u8) {
    let (dx, dy, dw, dh) = to_display(rot_deg, x, y);
    let pdfium = lock().expect("bind pdfium");
    let doc = pdfium
        .load_pdf_from_byte_slice(pdf, None)
        .expect("load pdf");
    let page = doc.pages().get(0).expect("page 0");
    let bitmap = page
        .render(dw as i32, dh as i32, None)
        .expect("render page");
    assert_eq!(bitmap.width() as f64, dw, "render width follows display");
    assert_eq!(bitmap.height() as f64, dh, "render height follows display");
    let rgba = bitmap.as_rgba_bytes();
    let (px, py) = (dx as usize, (dh - dy) as usize);
    let offset = (py * bitmap.width() as usize + px) * 4;
    (rgba[offset], rgba[offset + 1], rgba[offset + 2])
}

/// Returns the unrotated (left, right, bottom, top) bounds of every top-level
/// text object on page 0.
fn text_object_bounds(pdf: &[u8]) -> Vec<(f32, f32, f32, f32)> {
    let pdfium = lock().expect("bind pdfium");
    let doc = pdfium
        .load_pdf_from_byte_slice(pdf, None)
        .expect("load pdf");
    let page = doc.pages().get(0).expect("page 0");
    let mut all = Vec::new();
    for index in 0..page.objects().len() {
        let object = page.objects().get(index).expect("object");
        if object.object_type() == PdfPageObjectType::Text {
            let bounds = object.bounds().expect("text bounds");
            all.push((
                bounds.left().value,
                bounds.right().value,
                bounds.bottom().value,
                bounds.top().value,
            ));
        }
    }
    all
}

#[test]
#[serial]
fn covering_rect_removes_text_and_preserves_page_count() {
    let secret = "TOPSECRET-alpha";
    let input = sample_pdf(&[secret]);
    // Sanity: the fixture text is extractable before redaction.
    assert!(
        extract_page_text(&input, 0).contains(secret),
        "fixture text must be extractable pre-redaction"
    );

    let mut redactions = BTreeMap::new();
    redactions.insert(0u32, vec![covering_rect()]);
    let out = redact(&input, &redactions).expect("redact");

    assert_eq!(out.page_count, 1);
    // The rect fully contains the removed text bounds: no excess warnings.
    assert!(
        out.warnings.is_empty(),
        "fully-covered removal must not warn, got: {:?}",
        out.warnings
    );

    // Output loads as a structurally valid PDF with the same page count.
    let lodoc = lopdf::Document::load_mem(&out.pdf).expect("output loads in lopdf");
    assert_eq!(lodoc.get_pages().len(), 1);

    // TRUE redaction: the text is no longer extractable.
    let text_after = extract_page_text(&out.pdf, 0);
    assert!(
        !text_after.contains(secret),
        "redacted text must be unextractable, got: {text_after:?}"
    );
    // The raw bytes should not leak the literal either (fixture writes it as
    // an unencrypted, uncompressed string literal).
    assert!(
        !out.pdf
            .windows(secret.len())
            .any(|w| w == secret.as_bytes()),
        "raw output bytes must not contain the redacted string"
    );
}

#[test]
#[serial]
fn rect_over_empty_region_leaves_text_extractable() {
    let secret = "KEEP-ME-visible";
    let input = sample_pdf(&[secret]);

    // Bottom-right corner: far from the text at (72, 720).
    let mut redactions = BTreeMap::new();
    redactions.insert(
        0u32,
        vec![RedactionRect {
            left_pct: 70.0,
            top_pct: 80.0,
            width_pct: 20.0,
            height_pct: 15.0,
        }],
    );
    let out = redact(&input, &redactions).expect("redact");

    assert_eq!(out.page_count, 1);
    let text_after = extract_page_text(&out.pdf, 0);
    assert!(
        text_after.contains(secret),
        "text outside the redaction rect must survive, got: {text_after:?}"
    );
}

#[test]
#[serial]
fn multi_page_redacts_only_targeted_page() {
    let input = sample_pdf(&["Alpha page zero", "Bravo page one"]);

    let mut redactions = BTreeMap::new();
    redactions.insert(1u32, vec![covering_rect()]);
    let out = redact(&input, &redactions).expect("redact");

    assert_eq!(out.page_count, 2);
    let lodoc = lopdf::Document::load_mem(&out.pdf).expect("output loads");
    assert_eq!(lodoc.get_pages().len(), 2);

    let page0_text = extract_page_text(&out.pdf, 0);
    assert!(
        page0_text.contains("Alpha page zero"),
        "page 0 text must remain, got: {page0_text:?}"
    );
    let page1_text = extract_page_text(&out.pdf, 1);
    assert!(
        !page1_text.contains("Bravo page one"),
        "page 1 text must be gone, got: {page1_text:?}"
    );
}

#[test]
#[serial]
fn out_of_range_page_index_is_skipped_silently() {
    let secret = "STILL-here";
    let input = sample_pdf(&[secret]);

    let mut redactions = BTreeMap::new();
    redactions.insert(99u32, vec![covering_rect()]);
    let out = redact(&input, &redactions).expect("redact must not error");

    assert_eq!(out.page_count, 1);
    let lodoc = lopdf::Document::load_mem(&out.pdf).expect("output loads");
    assert_eq!(lodoc.get_pages().len(), 1);
    assert!(
        extract_page_text(&out.pdf, 0).contains(secret),
        "document must be otherwise unchanged"
    );
}

#[test]
#[serial]
fn degenerate_rect_is_skipped() {
    let secret = "ZERO-width-safe";
    let input = sample_pdf(&[secret]);

    let mut redactions = BTreeMap::new();
    redactions.insert(
        0u32,
        vec![RedactionRect {
            left_pct: 5.0,
            top_pct: 2.0,
            width_pct: 0.0,
            height_pct: 15.0,
        }],
    );
    let out = redact(&input, &redactions).expect("redact");

    assert_eq!(out.page_count, 1);
    assert!(
        extract_page_text(&out.pdf, 0).contains(secret),
        "degenerate rect must be a no-op"
    );
}

#[test]
#[serial]
fn black_box_is_drawn_over_redacted_area() {
    let input = sample_pdf(&["PAINT-over-me"]);

    let mut redactions = BTreeMap::new();
    redactions.insert(0u32, vec![covering_rect()]);
    let out = redact(&input, &redactions).expect("redact");

    // Rasterize at quarter scale (153 x 198) and sample inside the box.
    let pdfium = lock().expect("bind pdfium");
    let doc = pdfium
        .load_pdf_from_byte_slice(&out.pdf, None)
        .expect("load redacted pdf");
    let page = doc.pages().get(0).expect("page 0");
    let bitmap = page.render(153, 198, None).expect("render");
    let (bw, bh) = (bitmap.width() as f32, bitmap.height() as f32);
    let rgba = bitmap.as_rgba_bytes();

    // Redaction box in page points: x 30.6..397.8, y_top 776.16, y_bottom 657.36
    // on a 612x792 page. Convert to bitmap pixels (top-left origin).
    let sx = bw / 612.0;
    let sy = bh / 792.0;
    let px0 = (30.6 * sx).ceil() as usize + 2;
    let px1 = (397.8 * sx).floor() as usize - 2;
    let py0 = ((792.0 - 776.16) * sy).ceil() as usize + 2;
    let py1 = ((792.0 - 657.36) * sy).floor() as usize - 2;
    assert!(px0 < px1 && py0 < py1, "sampling window must be non-empty");

    let width_px = bitmap.width() as usize;
    let mut samples = 0usize;
    for py in [py0, (py0 + py1) / 2, py1] {
        for px in [px0, (px0 + px1) / 2, px1] {
            let offset = (py * width_px + px) * 4;
            let (r, g, b) = (rgba[offset], rgba[offset + 1], rgba[offset + 2]);
            assert!(
                r < 40 && g < 40 && b < 40,
                "pixel ({px},{py}) inside redaction must be near-black, got rgb({r},{g},{b})"
            );
            samples += 1;
        }
    }
    assert_eq!(samples, 9);

    // Control: a pixel well outside the box (bottom of page) stays light.
    let outside_offset = (((bh * 0.9) as usize) * width_px + (bw * 0.5) as usize) * 4;
    let (r, g, b) = (
        rgba[outside_offset],
        rgba[outside_offset + 1],
        rgba[outside_offset + 2],
    );
    assert!(
        r > 200 && g > 200 && b > 200,
        "pixel outside redaction must stay light, got rgb({r},{g},{b})"
    );
}

/// CRITICAL regression (findings/redact.txt, findings/parity.txt): percent
/// rects refer to the page AS DISPLAYED. For all four rotations the same
/// displayed selection must (a) remove the intended text from the file,
/// (b) paint the black box over the right displayed region, and (c) behave
/// identically to the unrotated case (the untouched text object survives).
#[test]
#[serial]
fn rotated_pages_redact_the_displayed_selection() {
    let secret = "ROT-SECRET-p3nd";
    let keep = "KEEP-ME-visible";
    // Unrotated region covering the secret at (72, 720) 24pt: bounds are
    // x 73.9..287.7, y 715.2..737.5 (measured via pdfium).
    let (rl, rr, rb, rt) = (66.0, 330.0, 710.0, 742.0);

    for rot in [0i64, 90, 180, 270] {
        let input = pdf_with_texts(&[(secret, 72, 720), (keep, 72, 120)], Some(rot));
        assert!(
            !leak_hits(&input, secret).is_empty(),
            "rot {rot}: walker must see the fixture secret pre-redaction"
        );

        let mut redactions = BTreeMap::new();
        redactions.insert(0u32, vec![displayed_rect_for_region(rot, rl, rr, rb, rt)]);
        let out = redact(&input, &redactions).expect("redact");

        // (a) The intended text is gone from the file entirely.
        let hits = leak_hits(&out.pdf, secret);
        assert!(
            hits.is_empty(),
            "rot {rot}: secret must not survive anywhere, found: {hits:?}"
        );

        // (c) Parity with the unrotated case: exactly the untouched text
        // object survives (pdfium re-encodes regenerated content streams, so
        // object identity is asserted via bounds, not string literals).
        let survivors = text_object_bounds(&out.pdf);
        assert_eq!(
            survivors.len(),
            1,
            "rot {rot}: only the KEEP text object must survive"
        );
        let (_, _, sb, st) = survivors[0];
        assert!(
            (110.0..=120.0).contains(&sb) && (135.0..=145.0).contains(&st),
            "rot {rot}: survivor must be the KEEP text at y 115..138, got {sb}..{st}"
        );
        assert!(
            out.warnings.is_empty(),
            "rot {rot}: rect fully covers the text, no warnings expected, got: {:?}",
            out.warnings
        );

        // (b) The black box renders over the displayed selection...
        let (r, g, b) = rendered_rgb_at(&out.pdf, rot, 150.0, 726.0);
        assert!(
            r < 40 && g < 40 && b < 40,
            "rot {rot}: displayed selection must be near-black, got rgb({r},{g},{b})"
        );
        // ...and NOT over unselected areas (box placement precision).
        let (r, g, b) = rendered_rgb_at(&out.pdf, rot, 450.0, 726.0);
        assert!(
            r > 200 && g > 200 && b > 200,
            "rot {rot}: area right of the selection must stay light, got rgb({r},{g},{b})"
        );
    }
}

/// CRITICAL regression (findings/redact.txt): text living inside a Form
/// XObject must not survive a redaction that covers it — either the engine
/// removes it (bytes + extraction clean) or it hard-errors. Never a silent
/// Ok with the secret still extractable.
#[test]
#[serial]
fn xobject_text_never_silently_survives() {
    let secret = "XOBJ-SECRET-77zq";
    let input = xobject_pdf(secret);
    assert!(
        extract_page_text(&input, 0).contains(secret),
        "fixture secret must be extractable pre-redaction"
    );

    let mut redactions = BTreeMap::new();
    redactions.insert(
        0u32,
        vec![RedactionRect {
            left_pct: 0.0,
            top_pct: 0.0,
            width_pct: 100.0,
            height_pct: 100.0,
        }],
    );
    match redact(&input, &redactions) {
        Ok(out) => {
            let hits = leak_hits(&out.pdf, secret);
            assert!(
                hits.is_empty(),
                "Ok() must mean the secret is gone from the file, found: {hits:?}"
            );
            let text_after = extract_page_text(&out.pdf, 0);
            assert!(
                !text_after.contains(secret),
                "Ok() must mean the secret is unextractable, got: {text_after:?}"
            );
        }
        Err(PdfCoreError::Unsupported(msg)) => {
            assert!(
                msg.contains("page 0"),
                "hard error must name the page, got: {msg}"
            );
        }
        Err(other) => panic!("unexpected error kind: {other}"),
    }
}

/// Companion: a redaction rect that does NOT touch the XObject's text leaves
/// the form (and its text) intact — no over-erroring, no over-removal.
#[test]
#[serial]
fn xobject_text_outside_rect_survives() {
    let secret = "XOBJ-KEEP-31ab";
    let input = xobject_pdf(secret);

    // Bottom third of the page; the form's text sits at y 715..738.
    let mut redactions = BTreeMap::new();
    redactions.insert(
        0u32,
        vec![RedactionRect {
            left_pct: 0.0,
            top_pct: 70.0,
            width_pct: 100.0,
            height_pct: 30.0,
        }],
    );
    let out = redact(&input, &redactions).expect("redact");
    assert!(
        extract_page_text(&out.pdf, 0).contains(secret),
        "form text outside the rect must survive"
    );
    assert!(out.warnings.is_empty(), "nothing removed, no warnings");
}

/// CRITICAL regression (findings/redact.txt): annotation text (/Contents of
/// FreeText etc.) must not survive a redaction that intersects the
/// annotation — either deleted with clean bytes or a hard error.
#[test]
#[serial]
fn annotation_secret_never_silently_survives() {
    let secret = "ANNOT-SECRET-4fkq";
    let input = annot_pdf(secret);
    assert!(
        !leak_hits(&input, secret).is_empty(),
        "walker must see the fixture secret pre-redaction"
    );

    let mut redactions = BTreeMap::new();
    redactions.insert(
        0u32,
        vec![RedactionRect {
            left_pct: 0.0,
            top_pct: 0.0,
            width_pct: 100.0,
            height_pct: 100.0,
        }],
    );
    match redact(&input, &redactions) {
        Ok(out) => {
            let hits = leak_hits(&out.pdf, secret);
            assert!(
                hits.is_empty(),
                "Ok() must mean the annotation secret is gone, found: {hits:?}"
            );
            // The annotation itself is deleted, not merely emptied.
            let pdfium = lock().expect("bind pdfium");
            let doc = pdfium
                .load_pdf_from_byte_slice(&out.pdf, None)
                .expect("load redacted pdf");
            assert_eq!(
                doc.pages().get(0).expect("page 0").annotations().len(),
                0,
                "intersecting annotation must be deleted"
            );
        }
        Err(PdfCoreError::Unsupported(msg)) => {
            assert!(
                msg.contains("page 0"),
                "hard error must name the page, got: {msg}"
            );
        }
        Err(other) => panic!("unexpected error kind: {other}"),
    }
}

/// Companion: an annotation the rects never touch survives untouched.
#[test]
#[serial]
fn annotation_outside_rect_survives() {
    let secret = "ANNOT-KEEP-92rt";
    let input = annot_pdf(secret); // /Rect [72 690 300 740]

    // Bottom-right corner, far from the annotation.
    let mut redactions = BTreeMap::new();
    redactions.insert(
        0u32,
        vec![RedactionRect {
            left_pct: 70.0,
            top_pct: 80.0,
            width_pct: 25.0,
            height_pct: 15.0,
        }],
    );
    let out = redact(&input, &redactions).expect("redact");
    assert!(
        !leak_hits(&out.pdf, secret).is_empty(),
        "annotation outside the rects must survive"
    );
    let pdfium = lock().expect("bind pdfium");
    let doc = pdfium
        .load_pdf_from_byte_slice(&out.pdf, None)
        .expect("load redacted pdf");
    assert_eq!(
        doc.pages().get(0).expect("page 0").annotations().len(),
        1,
        "non-intersecting annotation must not be deleted"
    );
}

/// A rect that clips only part of an annotation still deletes the whole
/// annotation (whole-object semantics), covers its full bounds with a black
/// box, and reports the excess in warnings.
#[test]
#[serial]
fn partially_covered_annotation_is_deleted_covered_and_warned() {
    let secret = "ANNOT-HALF-56mv";
    let input = annot_pdf(secret); // /Rect [72 690 300 740]

    // Covers only the left half of the annotation rect.
    let mut redactions = BTreeMap::new();
    redactions.insert(
        0u32,
        vec![RedactionRect {
            left_pct: 10.0,  // x 61.2
            top_pct: 6.0,    // y_top 744.5
            width_pct: 15.0, // x 61.2..153.0
            height_pct: 8.0, // y 681.1..744.5
        }],
    );
    let out = redact(&input, &redactions).expect("redact");
    assert!(
        leak_hits(&out.pdf, secret).is_empty(),
        "partially covered annotation must still be fully removed"
    );
    assert!(
        out.warnings.iter().any(|w| w.contains("page 0")),
        "excess coverage must be reported, got: {:?}",
        out.warnings
    );
    // Union coverage: the annotation area beyond the rect is blacked out.
    let (r, g, b) = rendered_rgb_at(&out.pdf, 0, 280.0, 715.0);
    assert!(
        r < 40 && g < 40 && b < 40,
        "uncovered annotation remainder must be blacked out, got rgb({r},{g},{b})"
    );
}

/// MAJOR regression (findings/parity.txt, findings/realworld.txt): removing a
/// whole text object that extends beyond the rect must cover the object's
/// full bounds with a black box (never silently-absent content) and emit a
/// warning naming the page.
#[test]
#[serial]
fn whole_object_removal_covers_full_bounds_and_warns() {
    let line = "Alpha SECRET Omega";
    let input = pdf_with_texts(&[(line, 72, 720)], None);

    // The word SECRET only: x 138..240, y 712..742 (from the parity finding).
    let mut redactions = BTreeMap::new();
    redactions.insert(
        0u32,
        vec![RedactionRect {
            left_pct: 22.549,
            top_pct: 6.313,
            width_pct: 16.667,
            height_pct: 3.788,
        }],
    );
    let out = redact(&input, &redactions).expect("redact");

    // v1 semantics: the whole show-text run is removed...
    assert!(
        leak_hits(&out.pdf, "SECRET").is_empty(),
        "redacted word must be gone"
    );
    assert!(
        leak_hits(&out.pdf, "Alpha").is_empty(),
        "whole-object removal takes the rest of the line with it (v1)"
    );
    // ...which MUST be reported to the caller...
    assert!(
        out.warnings.iter().any(|w| w.contains("page 0")),
        "removal beyond the rect must warn with the page index, got: {:?}",
        out.warnings
    );
    // ...and the full removed bounds must be covered: 'Alpha' (x~74..128)
    // and 'Omega' (x~250..320) regions render near-black, not absent-white.
    for x in [90.0, 290.0] {
        let (r, g, b) = rendered_rgb_at(&out.pdf, 0, x, 726.0);
        assert!(
            r < 40 && g < 40 && b < 40,
            "removed-but-unrequested region x={x} must be covered, got rgb({r},{g},{b})"
        );
    }
    // Control: below the line stays light (coverage is not page-wide).
    let (r, g, b) = rendered_rgb_at(&out.pdf, 0, 300.0, 650.0);
    assert!(
        r > 200 && g > 200 && b > 200,
        "area below the removed line must stay light, got rgb({r},{g},{b})"
    );
}

/// CRITICAL regression (findings/realworld.txt, Ghostscript/Type3 wipe):
/// pdfium's content regeneration silently drops Type3-font text objects, so
/// redacting any page that carries them used to destroy ALL text on the page
/// while reporting success. The engine must detect the loss after saving and
/// hard-error instead of returning a silently-gutted document.
#[test]
#[serial]
fn type3_regeneration_loss_is_a_hard_error_not_silent_destruction() {
    let secret = "T3-SECRET-11aa";
    let input = type3_pdf(secret);
    assert!(
        extract_page_text(&input, 0).contains(secret),
        "fixture secret must be extractable pre-redaction"
    );

    // Covers only the Helvetica secret at the top; the Type3 line at
    // (72, 400) is nowhere near the rect and must not be harmed.
    let mut redactions = BTreeMap::new();
    redactions.insert(0u32, vec![covering_rect()]);
    match redact(&input, &redactions) {
        Ok(out) => {
            // If pdfium ever learns to re-serialize Type3 text, Ok is fine —
            // but then the untouched Type3 object must actually survive.
            let survivors = text_object_bounds(&out.pdf);
            assert_eq!(
                survivors.len(),
                1,
                "the untouched Type3 text object must survive an Ok() result"
            );
        }
        Err(PdfCoreError::Unsupported(msg)) => {
            assert!(
                msg.contains("page 0"),
                "hard error must name the page, got: {msg}"
            );
        }
        Err(other) => panic!("unexpected error kind: {other}"),
    }
}

#[test]
#[serial]
fn empty_redactions_map_is_a_valid_noop() {
    let secret = "NOOP-untouched";
    let input = sample_pdf(&[secret, "second page"]);

    let redactions = BTreeMap::new();
    let out = redact(&input, &redactions).expect("redact with empty map");

    assert_eq!(out.page_count, 2);
    let lodoc = lopdf::Document::load_mem(&out.pdf).expect("output loads");
    assert_eq!(lodoc.get_pages().len(), 2);
    assert!(
        extract_page_text(&out.pdf, 0).contains(secret),
        "text must be intact after no-op redaction"
    );
}

//! Integration tests for pdf_core::watermark — assert real behavior on the
//! output bytes by reloading with lopdf (and pdfium for a validity check).

#![cfg(feature = "pdfium")]

use lopdf::{Document, Object, Stream, dictionary};
use pdf_core::PdfCoreError;
use pdf_core::testkit::sample_pdf;
use pdf_core::watermark::{
    Pattern, StampPosition, WatermarkImage, WatermarkSpec, apply_watermark, stamp_page_numbers,
};
use std::time::{Duration, Instant};

#[test]
fn hostile_watermark_line_count_returns_typed_limit_without_panic() {
    let input = sample_pdf(&["base"]);
    let spec = WatermarkSpec {
        lines: vec!["x".to_string(); 10_000],
        ..WatermarkSpec::default()
    };
    let result = std::panic::catch_unwind(|| apply_watermark(&input, &spec));
    let error = result.expect("must not panic").expect_err("line limit");
    assert_eq!(error.stable_code(), "resource_limit");
}

#[test]
fn hostile_png_dimensions_return_typed_limit_before_decode() {
    let mut png = tiny_png_rgba();
    png[16..20].copy_from_slice(&100_000_u32.to_be_bytes());
    png[20..24].copy_from_slice(&100_000_u32.to_be_bytes());
    let checksum = crc32(&png[12..29]);
    png[29..33].copy_from_slice(&checksum.to_be_bytes());
    let spec = WatermarkSpec {
        image: Some(WatermarkImage {
            bytes: png,
            width_pt: 100.0,
            height_pt: 100.0,
        }),
        lines: Vec::new(),
        ..WatermarkSpec::default()
    };
    let result = std::panic::catch_unwind(|| apply_watermark(&sample_pdf(&["base"]), &spec));
    let error = result.expect("must not panic").expect_err("pixel limit");
    assert_eq!(error.stable_code(), "resource_limit");
}

#[test]
fn extreme_media_box_grid_returns_resource_limit_before_anchor_allocation() {
    let mut doc = Document::load_mem(&sample_pdf(&["base"])).expect("load");
    let page_id = doc.get_pages()[&1];
    doc.get_dictionary_mut(page_id).expect("page").set(
        "MediaBox",
        vec![0.into(), 0.into(), 1_000_000.into(), 1_000_000.into()],
    );
    let mut input = Vec::new();
    doc.save_to(&mut input).expect("save");
    let spec = WatermarkSpec {
        pattern: Pattern::Grid,
        x_spacing_pt: 72.0,
        y_spacing_pt: 72.0,
        ..WatermarkSpec::default()
    };

    let started = Instant::now();
    let error = match apply_watermark(&input, &spec) {
        Err(error) => error,
        Ok(_) => panic!("oversized watermark grid unexpectedly succeeded"),
    };
    assert!(
        matches!(error, PdfCoreError::ResourceLimit(ref message) if message == "watermark grid anchor resource limit exceeded"),
        "stable typed resource-limit message: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "must reject before allocating anchors: {:?}",
        started.elapsed()
    );
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    crc ^ u32::MAX
}

/// Decompressed, concatenated content stream text of every page, in order.
fn page_contents(bytes: &[u8]) -> Vec<String> {
    let doc = Document::load_mem(bytes).expect("reload output");
    doc.get_pages()
        .values()
        .map(|&pid| {
            String::from_utf8_lossy(&doc.get_page_content(pid).expect("page content")).into_owned()
        })
        .collect()
}

/// Mirrors the library's content-stream number formatting for assertions.
fn fnum(v: f64) -> String {
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

fn tiny_png_rgba() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 128]));
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .expect("encode png");
    bytes
}

fn tiny_jpeg_rgb() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(6, 4, image::Rgb([10, 20, 30]));
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Jpeg,
    )
    .expect("encode jpeg");
    bytes
}

#[test]
fn default_watermark_stamps_every_page_and_still_loads() {
    let src = sample_pdf(&["page 1", "page 2", "page 3"]);
    let out = apply_watermark(&src, &WatermarkSpec::default()).expect("apply");

    let doc = Document::load_mem(&out).expect("reload");
    assert_eq!(doc.get_pages().len(), 3, "page count preserved");

    let contents = page_contents(&out);
    assert_eq!(contents.len(), 3);
    for (i, c) in contents.iter().enumerate() {
        // Sandwich: content begins with the save-state stream.
        assert!(c.trim_start().starts_with('q'), "page {i} starts with q");
        // Original page text survives.
        assert!(
            c.contains(&format!("page {}", i + 1)),
            "page {i} keeps text"
        );
        // Watermark text + opacity gs op on every page.
        assert!(c.contains("CONFIDENTIAL"), "page {i} has watermark text");
        assert!(c.contains("/DKwmGS gs"), "page {i} has gs op");
        assert!(c.contains(" Tm"), "page {i} has a text matrix");
    }

    // pdfium accepts the output (structural validity beyond lopdf's parser).
    let pdfium = pdf_core::pdfium::lock().expect("pdfium");
    let pdoc = pdfium
        .load_pdf_from_byte_slice(&out, None)
        .expect("pdfium load");
    assert_eq!(pdoc.pages().len(), 3);
}

/// Builds a single-page PDF with an arbitrary MediaBox (pt).
fn custom_size_pdf(width: f64, height: f64) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), width.into(), height.into()],
        "Contents" => content_id,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save custom pdf");
    out
}

/// Distinct on-page anchor columns/rows from the emitted glyph `Tm` x/y
/// coordinates, clustered to the nearest 10pt so per-glyph advances collapse
/// into their anchor.
fn visible_stamp_clusters(bytes: &[u8], width: f64, height: f64) -> (usize, usize) {
    let content = page_contents(bytes).remove(0);
    let mut xs: Vec<i64> = Vec::new();
    let mut ys: Vec<i64> = Vec::new();
    for line in content.lines() {
        let Some(rest) = line.strip_suffix(" Tm") else {
            continue;
        };
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if parts.len() != 6 {
            continue;
        }
        let (Ok(x), Ok(y)) = (parts[4].parse::<f64>(), parts[5].parse::<f64>()) else {
            continue;
        };
        if x < 0.0 || x > width || y < 0.0 || y > height {
            continue;
        }
        xs.push((x / 60.0).round() as i64);
        ys.push((y / 60.0).round() as i64);
    }
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    (xs.len(), ys.len())
}

#[test]
fn default_grid_spacing_covers_slide_sized_pages() {
    // 720x405pt (16:9 slide export). The Letter-calibrated fixed 320pt default
    // left exactly one visible row of stamps here; the page-proportional
    // default must yield a real grid (>=2 distinct columns AND rows).
    let input = custom_size_pdf(720.0, 405.0);
    let spec = WatermarkSpec {
        pattern: Pattern::Grid,
        rotation_deg: 0.0,
        lines: vec!["DRAFT".to_string(), "DO NOT COPY".to_string()],
        ..WatermarkSpec::default()
    };
    let out = apply_watermark(&input, &spec).expect("watermark slide page");
    let (cols, rows) = visible_stamp_clusters(&out, 720.0, 405.0);
    assert!(
        cols >= 2 && rows >= 2,
        "expected >=2x2 visible stamp grid on a 720x405 page, got {cols}x{rows}"
    );
}

#[test]
fn default_grid_spacing_is_letter_invariant() {
    // On US-Letter pages the adaptive default must reproduce the historical
    // 320pt lattice byte-for-byte (spacing scale factor is exactly 1).
    let input = sample_pdf(&["letter"]);
    let auto_spec = WatermarkSpec {
        pattern: Pattern::Grid,
        rotation_deg: 0.0,
        lines: vec!["DRAFT".to_string()],
        ..WatermarkSpec::default()
    };
    let explicit_spec = WatermarkSpec {
        x_spacing_pt: 320.0,
        y_spacing_pt: 320.0,
        ..auto_spec.clone()
    };
    let auto_out = apply_watermark(&input, &auto_spec).expect("auto");
    let explicit_out = apply_watermark(&input, &explicit_spec).expect("explicit");
    assert_eq!(
        page_contents(&auto_out),
        page_contents(&explicit_out),
        "default spacing on Letter must equal the historical explicit 320pt lattice"
    );
}

#[test]
fn grid_pattern_emits_strictly_more_anchors_than_single() {
    let src = sample_pdf(&["only page"]);
    let single = apply_watermark(&src, &WatermarkSpec::default()).expect("single");
    let grid = apply_watermark(
        &src,
        &WatermarkSpec {
            pattern: Pattern::Grid,
            ..WatermarkSpec::default()
        },
    )
    .expect("grid");

    let single_tj = page_contents(&single)[0].matches(" Tj").count();
    let grid_tj = page_contents(&grid)[0].matches(" Tj").count();
    assert!(
        grid_tj > single_tj,
        "grid ({grid_tj} Tj) must emit more anchors than single ({single_tj} Tj)"
    );
    // Letter page, 320pt spacing: x in [-612, 1224] -> 6 columns, y in
    // [-792, 1584] -> 8 rows = 48 anchors (+1 original page Tj).
    assert_eq!(grid_tj, 49);
    assert_eq!(single_tj, 2);
}

#[test]
fn multi_line_spec_emits_every_line_at_primary_and_meta_sizes() {
    let src = sample_pdf(&["doc"]);
    let spec = WatermarkSpec {
        lines: vec![
            "ACME CORP".to_string(),
            "user@example.com".to_string(),
            "2026-07-03 12:00".to_string(),
        ],
        ..WatermarkSpec::default()
    };
    let out = apply_watermark(&src, &spec).expect("apply");
    let c = &page_contents(&out)[0];

    assert!(c.contains("(ACME CORP) Tj"));
    assert!(c.contains("(user@example.com) Tj"));
    assert!(c.contains("(2026-07-03 12:00) Tj"));
    // Primary 14pt, meta = clamp(14*0.65, 6, 14) = 9.1pt.
    assert!(c.contains("/DKwmF 14 Tf"), "primary size");
    assert!(c.contains("/DKwmF 9.1 Tf"), "meta size");
    // Meta lines appear twice (lines 2 and 3).
    assert_eq!(c.matches("/DKwmF 9.1 Tf").count(), 2);
}

#[test]
fn opacity_lands_in_an_extgstate_object() {
    let src = sample_pdf(&["doc"]);
    let spec = WatermarkSpec {
        opacity: 0.3,
        ..WatermarkSpec::default()
    };
    let out = apply_watermark(&src, &spec).expect("apply");

    let doc = Document::load_mem(&out).expect("reload");
    let found = doc.objects.values().any(|obj| {
        let Object::Dictionary(d) = obj else {
            return false;
        };
        let is_gs = d
            .get(b"Type")
            .and_then(Object::as_name)
            .map(|n| n == b"ExtGState")
            .unwrap_or(false);
        let ca = d.get(b"ca").and_then(Object::as_float).unwrap_or(0.0);
        let ca_upper = d.get(b"CA").and_then(Object::as_float).unwrap_or(0.0);
        is_gs && (f64::from(ca) - 0.3).abs() < 1e-4 && (f64::from(ca_upper) - 0.3).abs() < 1e-4
    });
    assert!(found, "ExtGState with ca/CA = 0.3 must exist");
    assert!(page_contents(&out)[0].contains("/DKwmGS gs"));

    // Page resources reference the gs under the collision-proof name.
    let page_id = *doc.get_pages().values().next().unwrap();
    let page = doc.get_dictionary(page_id).unwrap();
    let res = page.get(b"Resources").unwrap().as_dict().unwrap();
    let gs = res.get(b"ExtGState").unwrap().as_dict().unwrap();
    assert!(gs.has(b"DKwmGS"));
}

#[test]
fn rotation_produces_tm_with_nonzero_sin_components() {
    let src = sample_pdf(&["doc"]);
    let spec = WatermarkSpec {
        rotation_deg: 30.0,
        ..WatermarkSpec::default()
    };
    let out = apply_watermark(&src, &spec).expect("apply");
    let c = &page_contents(&out)[0];

    // cos 30deg = 0.86603, sin 30deg = 0.5 -> Tm [cos sin -sin cos ox oy].
    assert!(
        c.contains("0.86603 0.5 -0.5 0.86603"),
        "rotated Tm matrix present: {c}"
    );
    let tm_line = c
        .lines()
        .find(|l| l.ends_with(" Tm") && l.starts_with("0.86603"))
        .expect("rotated Tm op line");
    assert!(tm_line.contains(" Tm"));
}

#[test]
fn font_size_is_clamped_to_64() {
    let src = sample_pdf(&["doc"]);
    let spec = WatermarkSpec {
        font_size_pt: 200.0,
        lines: vec!["BIG".to_string(), "meta line".to_string()],
        ..WatermarkSpec::default()
    };
    let out = apply_watermark(&src, &spec).expect("apply");
    let c = &page_contents(&out)[0];

    assert!(c.contains("/DKwmF 64 Tf"), "primary clamped to 64: {c}");
    // Meta: clamp(64*0.65, 6, 64) = 41.6.
    assert!(c.contains("/DKwmF 41.6 Tf"), "meta follows clamped primary");
    assert!(!c.contains("/DKwmF 200 Tf"));
}

#[test]
fn png_image_watermark_adds_xobject_with_smask_and_do_op() {
    let src = sample_pdf(&["doc"]);
    let spec = WatermarkSpec {
        lines: vec![],
        image: Some(WatermarkImage {
            bytes: tiny_png_rgba(),
            width_pt: 100.0,
            height_pt: 50.0,
        }),
        ..WatermarkSpec::default()
    };
    let out = apply_watermark(&src, &spec).expect("apply");
    let c = &page_contents(&out)[0];

    assert!(c.contains("/DKwmImg Do"), "image draw op present");
    // Image-only: no text fallback.
    assert!(!c.contains("CONFIDENTIAL"));
    // cm scales the unit square: rotation 0 -> [100 0 0 50 ox oy].
    assert!(c.contains("100 0 0 50 "), "cm carries image size: {c}");

    let doc = Document::load_mem(&out).expect("reload");
    let image_stream = doc
        .objects
        .values()
        .find_map(|obj| {
            let Object::Stream(s) = obj else { return None };
            let is_image = s
                .dict
                .get(b"Subtype")
                .and_then(Object::as_name)
                .map(|n| n == b"Image")
                .unwrap_or(false);
            (is_image && s.dict.has(b"SMask")).then_some(s)
        })
        .expect("RGB image XObject with SMask");
    assert_eq!(
        image_stream.dict.get(b"Width").unwrap().as_i64().unwrap(),
        8
    );
    assert_eq!(
        image_stream.dict.get(b"Height").unwrap().as_i64().unwrap(),
        8
    );
    assert_eq!(
        image_stream
            .dict
            .get(b"ColorSpace")
            .unwrap()
            .as_name()
            .unwrap(),
        b"DeviceRGB"
    );

    // The SMask itself is a DeviceGray image of the same dimensions.
    let smask_id = image_stream
        .dict
        .get(b"SMask")
        .unwrap()
        .as_reference()
        .unwrap();
    let smask = doc.get_object(smask_id).unwrap().as_stream().unwrap();
    assert_eq!(
        smask.dict.get(b"ColorSpace").unwrap().as_name().unwrap(),
        b"DeviceGray"
    );
    let alpha = smask.decompressed_content().unwrap();
    assert_eq!(alpha.len(), 8 * 8);
    assert!(alpha.iter().all(|&a| a == 128), "alpha channel preserved");

    // Page resources expose the XObject under the collision-proof name.
    let page_id = *doc.get_pages().values().next().unwrap();
    let res = doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_dict()
        .unwrap();
    assert!(
        res.get(b"XObject")
            .unwrap()
            .as_dict()
            .unwrap()
            .has(b"DKwmImg")
    );
}

#[test]
fn jpeg_image_passes_through_with_dctdecode() {
    let src = sample_pdf(&["doc"]);
    let jpeg = tiny_jpeg_rgb();
    let spec = WatermarkSpec {
        lines: vec![],
        image: Some(WatermarkImage {
            bytes: jpeg.clone(),
            width_pt: 60.0,
            height_pt: 40.0,
        }),
        ..WatermarkSpec::default()
    };
    let out = apply_watermark(&src, &spec).expect("apply");

    let doc = Document::load_mem(&out).expect("reload");
    let stream = doc
        .objects
        .values()
        .find_map(|obj| {
            let Object::Stream(s) = obj else { return None };
            let dct = s
                .dict
                .get(b"Filter")
                .and_then(Object::as_name)
                .map(|n| n == b"DCTDecode")
                .unwrap_or(false);
            dct.then_some(s)
        })
        .expect("DCTDecode image XObject");
    assert_eq!(stream.content, jpeg, "JPEG bytes pass through unmodified");
    assert_eq!(stream.dict.get(b"Width").unwrap().as_i64().unwrap(), 6);
    assert_eq!(stream.dict.get(b"Height").unwrap().as_i64().unwrap(), 4);
    assert!(page_contents(&out)[0].contains("/DKwmImg Do"));
}

#[test]
fn hybrid_emits_both_image_and_text() {
    let src = sample_pdf(&["doc"]);
    let spec = WatermarkSpec {
        lines: vec!["DRAFT".to_string()],
        image: Some(WatermarkImage {
            bytes: tiny_png_rgba(),
            width_pt: 80.0,
            height_pt: 80.0,
        }),
        ..WatermarkSpec::default()
    };
    let out = apply_watermark(&src, &spec).expect("apply");
    let c = &page_contents(&out)[0];

    assert!(c.contains("/DKwmImg Do"), "hybrid draws image");
    assert!(c.contains("(DRAFT) Tj"), "hybrid draws text");
    // Hybrid layout: image sits above text, so the image cm y-origin must be
    // higher than the text Tm baseline. hybrid_gap = clamp(14*0.6,4,96)=8.4;
    // total_text_height = 14*1.2 = 16.8; image center = (16.8+8.4)/2 = 12.6
    // above anchor; text center = -(80+8.4)/2 = -44.2 below anchor.
    let cy = 792.0 / 2.0;
    let img_oy = cy + 12.6 - 40.0; // image bottom-left y
    assert!(
        c.contains(&format!(
            "80 0 0 80 {} {} cm",
            fnum(612.0 / 2.0 - 40.0),
            fnum(img_oy)
        )),
        "image placed per hybrid layout: {c}"
    );
}

#[test]
fn empty_lines_without_image_fall_back_to_confidential() {
    let src = sample_pdf(&["doc"]);
    let spec = WatermarkSpec {
        lines: vec!["   ".to_string(), String::new()],
        ..WatermarkSpec::default()
    };
    let out = apply_watermark(&src, &spec).expect("apply");
    let c = &page_contents(&out)[0];
    assert!(c.contains("(CONFIDENTIAL) Tj"), "fallback text: {c}");
}

#[test]
fn watermark_survives_inherited_resources_and_double_stamping() {
    // Build a PDF whose /Resources live on the Pages node (inherited).
    let bytes = pdf_with_inherited_resources("inherited page");
    let out = apply_watermark(&bytes, &WatermarkSpec::default()).expect("apply");

    let doc = Document::load_mem(&out).expect("reload");
    let page_id = *doc.get_pages().values().next().unwrap();
    let page = doc.get_dictionary(page_id).unwrap();
    let res = page.get(b"Resources").unwrap().as_dict().unwrap();
    let fonts = res.get(b"Font").unwrap().as_dict().unwrap();
    assert!(fonts.has(b"F1"), "inherited font cloned onto the page");
    assert!(fonts.has(b"DKwmF"), "watermark font added");
    assert!(page_contents(&out)[0].contains("CONFIDENTIAL"));

    // Second stamping exercises the Contents-array path.
    let spec2 = WatermarkSpec {
        lines: vec!["SECOND PASS".to_string()],
        ..WatermarkSpec::default()
    };
    let twice = apply_watermark(&out, &spec2).expect("apply twice");
    let c = &page_contents(&twice)[0];
    assert!(c.contains("CONFIDENTIAL"));
    assert!(c.contains("SECOND PASS"));
    assert!(c.contains("inherited page"));
}

#[test]
fn stamp_page_numbers_labels_every_page() {
    let src = sample_pdf(&["one", "two", "three"]);
    let out = stamp_page_numbers(
        &src,
        "{page} of {total}",
        StampPosition::BottomCenter,
        10.0,
        1,
    )
    .expect("stamp");

    let doc = Document::load_mem(&out).expect("reload");
    assert_eq!(doc.get_pages().len(), 3);

    let contents = page_contents(&out);
    for (i, c) in contents.iter().enumerate() {
        assert!(
            c.contains(&format!("({} of 3) Tj", i + 1)),
            "page {i} label: {c}"
        );
        // Bottom baseline 36pt from the bottom edge, centered horizontally.
        let label = format!("{} of 3", i + 1);
        let tw = pdf_core::fonts::StandardFont::Helvetica.text_width_pt(&label, 10.0);
        let x = (612.0 - tw) / 2.0;
        assert!(
            c.contains(&format!("1 0 0 1 {} 36 Tm", fnum(x))),
            "page {i} position: {c}"
        );
        assert!(c.contains("/DKwmF 10 Tf"));
        assert!(c.contains("0 0 0 rg"), "black text");
    }
}

#[test]
fn stamp_page_numbers_zero_pads_and_offsets_start() {
    let src = sample_pdf(&["a", "b"]);
    let out =
        stamp_page_numbers(&src, "DK-{page:03}", StampPosition::TopRight, 9.0, 7).expect("stamp");
    let contents = page_contents(&out);

    assert!(contents[0].contains("(DK-007) Tj"), "{}", contents[0]);
    assert!(contents[1].contains("(DK-008) Tj"), "{}", contents[1]);

    // Top-right: baseline 36pt below the top edge, right edge 36pt margin.
    let tw = pdf_core::fonts::StandardFont::Helvetica.text_width_pt("DK-007", 9.0);
    let x = 612.0 - 36.0 - tw;
    assert!(
        contents[0].contains(&format!("1 0 0 1 {} 756 Tm", fnum(x))),
        "{}",
        contents[0]
    );

    // Wider Bates-style padding.
    let bates = stamp_page_numbers(&src, "DK-{page:06}", StampPosition::BottomRight, 9.0, 1)
        .expect("stamp bates");
    assert!(page_contents(&bates)[0].contains("(DK-000001) Tj"));
}

#[test]
fn rewatermark_uses_fresh_resource_names_and_keeps_pass1_bindings() {
    // Two passes with different opacities AND different images: pass 2 must
    // not rebind pass 1's /DKwmGS //DKwmF //DKwmImg names, or pass 1's ops
    // would silently render with pass 2's opacity/image.
    let src = sample_pdf(&["doc"]);
    let pass1 = apply_watermark(
        &src,
        &WatermarkSpec {
            opacity: 0.3,
            image: Some(WatermarkImage {
                bytes: tiny_png_rgba(),
                width_pt: 100.0,
                height_pt: 50.0,
            }),
            ..WatermarkSpec::default()
        },
    )
    .expect("pass 1");
    let pass2 = apply_watermark(
        &pass1,
        &WatermarkSpec {
            opacity: 0.8,
            lines: vec!["SECOND".to_string()],
            image: Some(WatermarkImage {
                bytes: tiny_jpeg_rgb(),
                width_pt: 60.0,
                height_pt: 40.0,
            }),
            ..WatermarkSpec::default()
        },
    )
    .expect("pass 2");

    // Both passes' ops reference their own names.
    let c = &page_contents(&pass2)[0];
    assert!(c.contains("/DKwmGS gs"), "pass-1 gs op intact: {c}");
    assert!(c.contains("/DKwmGS2 gs"), "pass-2 gets suffixed gs: {c}");
    assert!(c.contains("/DKwmImg Do"), "pass-1 image op intact");
    assert!(c.contains("/DKwmImg2 Do"), "pass-2 gets suffixed image");
    assert!(c.contains("(CONFIDENTIAL) Tj") && c.contains("(SECOND) Tj"));

    let doc = Document::load_mem(&pass2).expect("reload");
    let page_id = *doc.get_pages().values().next().unwrap();
    let res = doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_dict()
        .unwrap();

    // Both ExtGStates exist and each name still resolves to its own opacity.
    let gs = res.get(b"ExtGState").unwrap().as_dict().unwrap();
    let ca_of = |name: &[u8]| {
        let id = gs.get(name).unwrap().as_reference().unwrap();
        let d = doc.get_dictionary(id).unwrap();
        f64::from(d.get(b"ca").and_then(Object::as_float).unwrap())
    };
    assert!(
        (ca_of(b"DKwmGS") - 0.3).abs() < 1e-4,
        "pass-1 name keeps pass-1 opacity"
    );
    assert!(
        (ca_of(b"DKwmGS2") - 0.8).abs() < 1e-4,
        "pass-2 name carries pass-2 opacity"
    );

    // Both fonts exist; both images exist and each name resolves to its own
    // stream (pass 1 = flate PNG, pass 2 = DCTDecode JPEG).
    let fonts = res.get(b"Font").unwrap().as_dict().unwrap();
    assert!(fonts.has(b"DKwmF") && fonts.has(b"DKwmF2"));
    let xobj = res.get(b"XObject").unwrap().as_dict().unwrap();
    let filter_of = |name: &[u8]| {
        let id = xobj.get(name).unwrap().as_reference().unwrap();
        let s = doc.get_object(id).unwrap().as_stream().unwrap();
        s.dict
            .get(b"Filter")
            .and_then(Object::as_name)
            .map(|n| n.to_vec())
            .unwrap_or_default()
    };
    assert_eq!(filter_of(b"DKwmImg"), b"FlateDecode".to_vec());
    assert_eq!(filter_of(b"DKwmImg2"), b"DCTDecode".to_vec());

    // pdfium still accepts the double-watermarked output.
    let pdfium = pdf_core::pdfium::lock().expect("pdfium");
    pdfium
        .load_pdf_from_byte_slice(&pass2, None)
        .expect("pdfium load");
}

#[test]
fn grid_with_huge_negative_offsets_stays_bounded_and_fast() {
    // offset_x = -1e9 used to iterate millions of off-page columns (26s+,
    // multi-GB output). Offsets are periodic modulo the spacing, so the
    // normalized grid must be identical to the offset-0 grid (-1e9 % 320 = 0)
    // and finish immediately.
    let src = sample_pdf(&["only page"]);
    let t0 = std::time::Instant::now();
    let out = apply_watermark(
        &src,
        &WatermarkSpec {
            pattern: Pattern::Grid,
            offset_x_pt: -1e9,
            offset_y_pt: -1e9,
            ..WatermarkSpec::default()
        },
    )
    .expect("grid with huge offsets");
    let elapsed = t0.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "bounded grid loop must finish fast, took {elapsed:?}"
    );

    // Same 48-anchor lattice as offset 0 (+1 Tj for the original page text).
    let tj = page_contents(&out)[0].matches(" Tj").count();
    assert_eq!(tj, 49);
}

#[test]
fn nonzero_mediabox_origin_shifts_stamp_coordinates() {
    // MediaBox [100 50 712 842]: visible page is 612x792 with lower-left at
    // (100, 50). All stamp math must be offset by that origin.
    let bytes = pdf_with_media_box(100.0, 50.0, 712.0, 842.0);

    // Single watermark: anchor at the true page center (406, 446).
    let out = apply_watermark(&bytes, &WatermarkSpec::default()).expect("watermark");
    let c = &page_contents(&out)[0];
    let tw = pdf_core::fonts::StandardFont::Helvetica.text_width_pt("CONFIDENTIAL", 14.0);
    let ox = 406.0 - tw / 2.0;
    let oy = 446.0 - 14.0 * 1.2 / 2.0; // cursor drops one line height from +8.4
    assert!(
        c.contains(&format!("1 0 0 1 {} {} Tm", fnum(ox), fnum(oy))),
        "watermark Tm shifted by MediaBox origin: {c}"
    );

    // Grid loop bounds shift too: anchor x spans [x0 - w, x0 + 2w].
    let grid = apply_watermark(
        &bytes,
        &WatermarkSpec {
            pattern: Pattern::Grid,
            ..WatermarkSpec::default()
        },
    )
    .expect("grid");
    let gc = &page_contents(&grid)[0];
    let xs: Vec<f64> = gc
        .lines()
        .filter(|l| l.ends_with(" Tm"))
        .filter_map(|l| l.split_whitespace().nth(4)?.parse().ok())
        .collect();
    assert!(!xs.is_empty());
    let min_x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    // Columns at -512, -192, ..., 1088 (next would exceed 100 + 2*612).
    assert!(
        (min_x - (-512.0 - tw / 2.0)).abs() < 0.01,
        "min_x = {min_x}"
    );
    assert!(
        (max_x - (1088.0 - tw / 2.0)).abs() < 0.01,
        "max_x = {max_x}"
    );

    // Page numbers: margins are relative to the box corners, not (0, 0).
    let nums = stamp_page_numbers(&bytes, "{page}", StampPosition::BottomLeft, 10.0, 1)
        .expect("page numbers");
    assert!(
        page_contents(&nums)[0].contains("1 0 0 1 136 86 Tm"),
        "bottom-left = (100+36, 50+36): {}",
        page_contents(&nums)[0]
    );
    let top = stamp_page_numbers(&bytes, "{page}", StampPosition::TopRight, 10.0, 1)
        .expect("page numbers top");
    let ltw = pdf_core::fonts::StandardFont::Helvetica.text_width_pt("1", 10.0);
    assert!(
        page_contents(&top)[0].contains(&format!(
            "1 0 0 1 {} 806 Tm",
            fnum(100.0 + 612.0 - 36.0 - ltw)
        )),
        "top-right shifted by origin: {}",
        page_contents(&top)[0]
    );
}

#[test]
fn nonzero_mediabox_origin_watermark_rasters_inside_the_page() {
    // With a far-offset MediaBox, ignoring the origin would stamp at
    // (306, 396) — entirely outside [1000..1612]x[1000..1792] — and the
    // rendered page would be blank.
    let bytes = pdf_with_media_box(1000.0, 1000.0, 1612.0, 1792.0);
    let out = apply_watermark(
        &bytes,
        &WatermarkSpec {
            opacity: 1.0,
            color: [0, 0, 0],
            font_size_pt: 64.0,
            ..WatermarkSpec::default()
        },
    )
    .expect("watermark");

    let png = pdf_core::raster::render_page(&out, 0, &pdf_core::raster::RenderOptions::default())
        .expect("render");
    let img = image::load_from_memory(&png)
        .expect("decode png")
        .to_luma8();
    assert_eq!((img.width(), img.height()), (612, 792));
    let dark = img.pixels().filter(|p| p.0[0] < 128).count();
    assert!(
        dark > 100,
        "stamp must be visible inside the page box (dark pixels = {dark})"
    );
}

#[test]
fn page_number_pad_width_is_clamped_not_panicking() {
    let src = sample_pdf(&["a"]);
    // Rust format! panics on widths >= 65536; huge N must clamp to 16.
    for template in [
        "{page:099999}",
        "{page:065536}",
        "{page:018446744073709551616000}",
    ] {
        let out = stamp_page_numbers(&src, template, StampPosition::BottomRight, 9.0, 7)
            .unwrap_or_else(|e| panic!("{template} must not fail: {e}"));
        assert!(
            page_contents(&out)[0].contains("(0000000000000007) Tj"),
            "{template} clamps to 16-wide pad: {}",
            page_contents(&out)[0]
        );
    }
    // Sane widths are untouched.
    let out = stamp_page_numbers(&src, "{page:03}", StampPosition::BottomRight, 9.0, 7)
        .expect("small pad");
    assert!(page_contents(&out)[0].contains("(007) Tj"));
}

#[test]
fn image_dimensions_clamp_to_docyantra_range() {
    // parseWatermark clamps image widthPt/heightPt to 24..=1440 (default
    // 180); a 2000pt request must render 1440pt wide like DocYantra.
    let src = sample_pdf(&["doc"]);
    let case = |w: f64, h: f64| {
        let out = apply_watermark(
            &src,
            &WatermarkSpec {
                lines: vec![],
                image: Some(WatermarkImage {
                    bytes: tiny_png_rgba(),
                    width_pt: w,
                    height_pt: h,
                }),
                ..WatermarkSpec::default()
            },
        )
        .expect("apply");
        page_contents(&out)[0].clone()
    };

    // Matches DocYantra's CTM for the same payload: [1440 0 0 100 -414 346].
    assert!(case(2000.0, 100.0).contains("1440 0 0 100 -414 346 cm"));
    // Tiny dims clamp up to 24.
    assert!(case(5.0, 5.0).contains("24 0 0 24 294 384 cm"));
    // Non-finite dims fall back to the TS default of 180.
    assert!(case(f64::NAN, f64::NAN).contains("180 0 0 180 216 306 cm"));
}

/// Minimal one-page PDF with an arbitrary MediaBox and empty content.
fn pdf_with_media_box(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<u8> {
    use lopdf::{Stream, dictionary};

    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content_id = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![x0.into(), y0.into(), x1.into(), y1.into()],
        "Contents" => content_id,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("save");
    buf
}

/// Minimal PDF with /Resources on the Pages node instead of the page.
fn pdf_with_inherited_resources(text: &str) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Stream, dictionary};

    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
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
        content.encode().expect("encode"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("save");
    buf
}

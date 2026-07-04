//! Integration tests for src/raster.rs (render_page, render_thumbnails).

#![cfg(feature = "pdfium")]

use image::GenericImageView;
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use pdf_core::raster::{ImageFormat, RenderOptions, render_page, render_thumbnails};
use pdf_core::testkit::sample_pdf;
use serial_test::serial;

/// Letter fixture dimensions in points (== pixels at scale 1.0 / 72 dpi).
const LETTER_W: u32 = 612;
const LETTER_H: u32 = 792;

fn assert_close(actual: u32, expected: u32, tolerance: u32, what: &str) {
    let diff = actual.abs_diff(expected);
    assert!(
        diff <= tolerance,
        "{what}: expected ~{expected} (±{tolerance}), got {actual}"
    );
}

/// Builds a single-page PDF with a tiny page (100 x 50 pt) so upscaling
/// behavior can be observed.
fn tiny_page_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content = Content {
        operations: vec![
            Operation::new("re", vec![10.into(), 10.into(), 30.into(), 20.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 50.into()],
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
    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("serialize tiny pdf");
    buf
}

/// Minimum luminance across all pixels — used to prove glyphs were rendered.
fn min_luminance(img: &image::DynamicImage) -> u8 {
    img.to_luma8().pixels().map(|p| p[0]).min().unwrap()
}

#[test]
#[serial(pdfium)]
fn render_page_default_png_has_native_size_and_aspect() {
    let pdf = sample_pdf(&["Hello raster"]);
    let png = render_page(&pdf, 0, &RenderOptions::default()).unwrap();

    assert_eq!(
        image::guess_format(&png).unwrap(),
        image::ImageFormat::Png,
        "default output must be PNG"
    );
    let img = image::load_from_memory(&png).unwrap();
    let (w, h) = img.dimensions();
    assert_close(w, LETTER_W, 1, "native width");
    assert_close(h, LETTER_H, 1, "native height");

    // Aspect ratio ~612:792 within ±1 px rounding.
    let expected_h = (f64::from(w) * f64::from(LETTER_H) / f64::from(LETTER_W)).round() as u32;
    assert_close(h, expected_h, 1, "aspect ratio height");
}

#[test]
#[serial(pdfium)]
fn render_page_is_not_blank() {
    let pdf = sample_pdf(&["THE QUICK BROWN FOX"]);
    let png = render_page(&pdf, 0, &RenderOptions::default()).unwrap();
    let img = image::load_from_memory(&png).unwrap();
    let darkest = min_luminance(&img);
    assert!(
        darkest < 128,
        "expected black Helvetica glyphs; darkest pixel luminance was {darkest}"
    );
}

#[test]
#[serial(pdfium)]
fn render_page_honors_target_width_exactly() {
    let pdf = sample_pdf(&["target width"]);
    let opts = RenderOptions {
        target_width_px: Some(200),
        // scale must be ignored when target_width_px is set
        scale: 3.0,
        format: ImageFormat::Png,
    };
    let png = render_page(&pdf, 0, &opts).unwrap();
    let img = image::load_from_memory(&png).unwrap();
    let (w, h) = img.dimensions();
    assert_eq!(w, 200, "target_width_px must be honored exactly");
    let expected_h = (200.0 * f64::from(LETTER_H) / f64::from(LETTER_W)).round() as u32;
    assert_close(h, expected_h, 1, "height at target width");
}

#[test]
#[serial(pdfium)]
fn render_page_scale_half_halves_dimensions() {
    let pdf = sample_pdf(&["half scale"]);
    let opts = RenderOptions {
        target_width_px: None,
        scale: 0.5,
        format: ImageFormat::Png,
    };
    let png = render_page(&pdf, 0, &opts).unwrap();
    let (w, h) = image::load_from_memory(&png).unwrap().dimensions();
    assert_close(w, LETTER_W / 2, 1, "half-scale width");
    assert_close(h, LETTER_H / 2, 1, "half-scale height");
}

#[test]
#[serial(pdfium)]
fn render_page_jpeg_decodes_as_jpeg_and_respects_sizing() {
    let pdf = sample_pdf(&["jpeg page"]);
    let opts = RenderOptions {
        target_width_px: Some(180),
        scale: 1.0,
        format: ImageFormat::Jpeg(80),
    };
    let bytes = render_page(&pdf, 0, &opts).unwrap();
    assert_eq!(
        image::guess_format(&bytes).unwrap(),
        image::ImageFormat::Jpeg,
        "output must be a real JPEG stream"
    );
    let img = image::load_from_memory(&bytes).unwrap();
    let (w, h) = img.dimensions();
    assert_eq!(w, 180, "JPEG must honor target_width_px exactly");
    let expected_h = (180.0 * f64::from(LETTER_H) / f64::from(LETTER_W)).round() as u32;
    assert_close(h, expected_h, 1, "JPEG height at target width");
    // Content survives the round trip (glyphs still darker than paper).
    assert!(min_luminance(&img) < 160, "JPEG output should not be blank");
}

#[test]
#[serial(pdfium)]
fn render_page_jpeg_quality_is_clamped() {
    let pdf = sample_pdf(&["quality clamp"]);
    for quality in [0u8, 255u8] {
        let opts = RenderOptions {
            target_width_px: Some(120),
            scale: 1.0,
            format: ImageFormat::Jpeg(quality),
        };
        let bytes = render_page(&pdf, 0, &opts).unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
        image::load_from_memory(&bytes).unwrap();
    }
}

#[test]
#[serial(pdfium)]
fn render_page_rejects_out_of_range_index() {
    let pdf = sample_pdf(&["only page"]);
    let err = render_page(&pdf, 1, &RenderOptions::default()).unwrap_err();
    assert!(
        matches!(err, pdf_core::error::PdfCoreError::InvalidInput(_)),
        "expected InvalidInput, got {err:?}"
    );

    // Far beyond pdfium's index type as well.
    let err = render_page(&pdf, u32::MAX, &RenderOptions::default()).unwrap_err();
    assert!(matches!(
        err,
        pdf_core::error::PdfCoreError::InvalidInput(_)
    ));
}

#[test]
#[serial(pdfium)]
fn render_page_rejects_bad_scale_and_zero_width() {
    let pdf = sample_pdf(&["opts"]);
    for bad_scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let opts = RenderOptions {
            target_width_px: None,
            scale: bad_scale,
            format: ImageFormat::Png,
        };
        let err = render_page(&pdf, 0, &opts).unwrap_err();
        assert!(matches!(
            err,
            pdf_core::error::PdfCoreError::InvalidInput(_)
        ));
    }
    let opts = RenderOptions {
        target_width_px: Some(0),
        scale: 1.0,
        format: ImageFormat::Png,
    };
    let err = render_page(&pdf, 0, &opts).unwrap_err();
    assert!(matches!(
        err,
        pdf_core::error::PdfCoreError::InvalidInput(_)
    ));
}

#[test]
#[serial(pdfium)]
fn render_thumbnails_all_pages_in_order() {
    let pdf = sample_pdf(&["1", "TWO WIDE TEXT LINE", "iiiii iiiii iiiii"]);
    let thumbs = render_thumbnails(&pdf, None, 150).unwrap();
    assert_eq!(thumbs.len(), 3, "one thumbnail per page");

    let mut decoded = Vec::new();
    for (i, bytes) in thumbs.iter().enumerate() {
        assert_eq!(
            image::guess_format(bytes).unwrap(),
            image::ImageFormat::Png,
            "thumbnail {i} must be PNG"
        );
        let img = image::load_from_memory(bytes).unwrap();
        let (w, h) = img.dimensions();
        assert!(w <= 150, "thumbnail {i} width {w} exceeds max_width_px");
        assert_eq!(w, 150, "Letter page is wider than 150 so width caps at max");
        let expected_h = (150.0 * f64::from(LETTER_H) / f64::from(LETTER_W)).round() as u32;
        assert_close(h, expected_h, 1, "thumbnail height");
        decoded.push(img);
    }

    // Pages carry different text, so rendered pixels must differ — proves the
    // thumbnails are distinct pages, in order.
    let luma: Vec<Vec<u8>> = decoded
        .iter()
        .map(|img| img.to_luma8().into_raw())
        .collect();
    assert_ne!(luma[0], luma[1], "page 0 and 1 must render differently");
    assert_ne!(luma[1], luma[2], "page 1 and 2 must render differently");
    assert_ne!(luma[0], luma[2], "page 0 and 2 must render differently");
}

#[test]
#[serial(pdfium)]
fn render_thumbnails_subset_returns_given_order() {
    let pdf = sample_pdf(&["page zero", "page one", "page two LONG"]);
    let all = render_thumbnails(&pdf, None, 150).unwrap();
    let subset = render_thumbnails(&pdf, Some(&[2, 0]), 150).unwrap();

    assert_eq!(subset.len(), 2);
    // PNG encoding is deterministic for identical pixels, so byte equality
    // against the full render pins both selection and ordering.
    assert_eq!(subset[0], all[2], "first thumbnail must be page 2");
    assert_eq!(subset[1], all[0], "second thumbnail must be page 0");
    assert_ne!(all[0], all[2], "pages must be distinguishable");
}

#[test]
#[serial(pdfium)]
fn render_thumbnails_never_upscales_tiny_pages() {
    let pdf = tiny_page_pdf(); // 100 x 50 pt page
    let thumbs = render_thumbnails(&pdf, None, 200).unwrap();
    assert_eq!(thumbs.len(), 1);
    let (w, h) = image::load_from_memory(&thumbs[0]).unwrap().dimensions();
    assert_eq!(w, 100, "tiny page must keep natural width, not upscale");
    assert_close(h, 50, 1, "tiny page height");
}

#[test]
#[serial(pdfium)]
fn render_thumbnails_rejects_out_of_range_and_zero_width() {
    let pdf = sample_pdf(&["a", "b", "c"]);
    let err = render_thumbnails(&pdf, Some(&[3]), 100).unwrap_err();
    assert!(matches!(
        err,
        pdf_core::error::PdfCoreError::InvalidInput(_)
    ));

    let err = render_thumbnails(&pdf, Some(&[0, u32::MAX]), 100).unwrap_err();
    assert!(matches!(
        err,
        pdf_core::error::PdfCoreError::InvalidInput(_)
    ));

    let err = render_thumbnails(&pdf, None, 0).unwrap_err();
    assert!(matches!(
        err,
        pdf_core::error::PdfCoreError::InvalidInput(_)
    ));
}

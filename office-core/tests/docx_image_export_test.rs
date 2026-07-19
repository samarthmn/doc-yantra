//! Word-compatible DOCX image-export integration tests.

use std::sync::OnceLock;

use office_core::{ConvertOptions, OfficeFormat, convert};

const COMPLEX_DOCX: &[u8] = include_bytes!("../../fidelity-corpus/docx/complex/source.docx");
const COMPLEX_TRUTH: &[u8] = include_bytes!("../../fidelity-corpus/docx/complex/ground-truth.pdf");
const ANCHORS_DOCX: &[u8] =
    include_bytes!("../../fidelity-corpus/docx/gdocs-anchors-toc/source.docx");

fn complex_pdf() -> &'static [u8] {
    static PDF: OnceLock<Vec<u8>> = OnceLock::new();
    PDF.get_or_init(|| {
        convert(COMPLEX_DOCX, OfficeFormat::Docx, &ConvertOptions::default())
            .expect("convert complex image fixture")
            .pdf
    })
}

fn dct_streams(pdf: &[u8]) -> Vec<&[u8]> {
    let mut streams = Vec::new();
    let mut cursor = 0;
    while let Some(relative_filter) = find_bytes(&pdf[cursor..], b"DCTDecode") {
        let filter = cursor + relative_filter;
        let (stream_marker, marker_len) = find_bytes(&pdf[filter..], b"stream\r\n")
            .map(|position| (position, b"stream\r\n".len()))
            .or_else(|| {
                find_bytes(&pdf[filter..], b"stream\n")
                    .map(|position| (position, b"stream\n".len()))
            })
            .expect("DCT stream marker");
        let start = filter + stream_marker + marker_len;
        let end = find_bytes(&pdf[start..], b"\r\nendstream")
            .or_else(|| find_bytes(&pdf[start..], b"\nendstream"))
            .map(|relative| start + relative)
            .expect("DCT stream end");
        streams.push(&pdf[start..end]);
        cursor = end + b"\nendstream".len();
    }
    streams
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[derive(Debug, PartialEq, Eq)]
struct JpegMetadata {
    baseline: bool,
    width: u16,
    height: u16,
    sampling: Vec<(u8, u8, u8)>,
    quantization_tables: Vec<Vec<u8>>,
}

fn jpeg_metadata(jpeg: &[u8]) -> JpegMetadata {
    assert_eq!(&jpeg[..2], &[0xff, 0xd8], "JPEG SOI");
    let mut cursor = 2;
    let mut baseline = false;
    let mut width = 0;
    let mut height = 0;
    let mut sampling = Vec::new();
    let mut quantization_tables = Vec::new();
    while cursor + 4 <= jpeg.len() {
        assert_eq!(jpeg[cursor], 0xff, "JPEG marker prefix at {cursor}");
        let marker = jpeg[cursor + 1];
        cursor += 2;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        let length = usize::from(u16::from_be_bytes([jpeg[cursor], jpeg[cursor + 1]]));
        let payload = &jpeg[cursor + 2..cursor + length];
        match marker {
            0xc0 => {
                baseline = true;
                height = u16::from_be_bytes([payload[1], payload[2]]);
                width = u16::from_be_bytes([payload[3], payload[4]]);
                let components = usize::from(payload[5]);
                sampling = payload[6..6 + components * 3]
                    .chunks_exact(3)
                    .map(|component| (component[0], component[1] >> 4, component[1] & 0x0f))
                    .collect();
            }
            0xdb => {
                let mut offset = 0;
                while offset < payload.len() {
                    assert_eq!(payload[offset] >> 4, 0, "8-bit DQT");
                    quantization_tables.push(payload[offset + 1..offset + 65].to_vec());
                    offset += 65;
                }
            }
            _ => {}
        }
        cursor += length;
    }
    JpegMetadata {
        baseline,
        width,
        height,
        sampling,
        quantization_tables,
    }
}

#[test]
fn complex_opaque_pngs_export_as_quality_75_baseline_420_jpegs() {
    let streams = dct_streams(complex_pdf());
    let expected_luma = vec![
        8, 6, 6, 7, 6, 5, 8, 7, 7, 7, 9, 9, 8, 10, 12, 20, 13, 12, 11, 11, 12, 25, 18, 19, 15, 20,
        29, 26, 31, 30, 29, 26, 28, 28, 32, 36, 46, 39, 32, 34, 44, 35, 28, 28, 40, 55, 41, 44, 48,
        49, 52, 52, 52, 31, 39, 57, 61, 56, 50, 60, 46, 51, 52, 50,
    ];
    let expected_chroma = vec![
        9, 9, 9, 12, 11, 12, 24, 13, 13, 24, 50, 33, 28, 33, 50, 50, 50, 50, 50, 50, 50, 50, 50,
        50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
        50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
    ];
    let metadata: Vec<_> = streams.iter().map(|stream| jpeg_metadata(stream)).collect();

    assert!(
        metadata.len() == 5
            && metadata.iter().all(|jpeg| {
                jpeg.baseline
                    && jpeg.sampling == vec![(1, 2, 2), (2, 1, 1), (3, 1, 1)]
                    && jpeg.quantization_tables
                        == vec![expected_luma.clone(), expected_chroma.clone()]
            }),
        "metadata={metadata:?}"
    );
}

#[test]
fn complex_original_size_jpegs_decode_pixel_identically_to_word() {
    let ours: Vec<_> = dct_streams(complex_pdf())
        .into_iter()
        .filter(|jpeg| {
            let metadata = jpeg_metadata(jpeg);
            (metadata.width, metadata.height) == (1536, 1024)
        })
        .map(|jpeg| {
            image::load_from_memory(jpeg)
                .expect("decode ours JPEG")
                .into_rgb8()
        })
        .collect();
    let truth: Vec<_> = dct_streams(COMPLEX_TRUTH)
        .into_iter()
        .filter(|jpeg| {
            let metadata = jpeg_metadata(jpeg);
            (metadata.width, metadata.height) == (1536, 1024)
        })
        .map(|jpeg| {
            image::load_from_memory(jpeg)
                .expect("decode truth JPEG")
                .into_rgb8()
        })
        .collect();

    assert_eq!(ours.len(), 2);
    assert_eq!(truth.len(), 2);
    for (index, (ours, truth)) in ours.iter().zip(&truth).enumerate() {
        let differing = ours
            .as_raw()
            .iter()
            .zip(truth.as_raw())
            .filter(|(ours, truth)| ours != truth)
            .count();
        assert_eq!(differing, 0, "original-size JPEG {index}");
    }
}

#[test]
fn complex_pdf_contains_only_placed_image_resources() {
    let pdf = String::from_utf8_lossy(complex_pdf());

    // Five JPEGs plus the transparent page-6 color image and its soft mask.
    assert_eq!(pdf.matches("/Subtype /Image").count(), 7);
}

#[test]
fn transparent_gdocs_pngs_remain_lossless_with_soft_masks() {
    let pdf = convert(ANCHORS_DOCX, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("convert transparent image fixture")
        .pdf;
    let text = String::from_utf8_lossy(&pdf);

    assert!(dct_streams(&pdf).is_empty() && text.matches("/SMask").count() == 3);
}

#[test]
fn word_jpeg_export_is_deterministic() {
    let second = convert(COMPLEX_DOCX, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("repeat complex conversion")
        .pdf;

    assert_eq!(complex_pdf(), second);
}

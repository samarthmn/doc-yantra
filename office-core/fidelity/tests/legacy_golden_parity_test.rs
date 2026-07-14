//! Structural parity against frozen LibreOffice office-conversion goldens.
//! The oracle files are read-only; this runner invokes only local Rust engines.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fidelity::evidence::verify_sha256;
use image::{DynamicImage, RgbImage};
use office_core::{ConvertOptions, OfficeError, OfficeFormat, convert};
use pdf_core::extract::extract_text_pages;
use pdf_core::meta::{page_sizes, validate};
use pdf_core::raster::{RenderOptions, render_page};
use serde::Deserialize;

const RASTER_DPI: u32 = 144;
const RASTER_SCALE: f64 = RASTER_DPI as f64 / 72.0;
const RASTER_MEAN_ABS_LIMIT: f64 = 0.02;
const RASTER_MAX_REGION_ABS_LIMIT: f64 = 0.12;
const RASTER_REGION_GRID: usize = 12;
const COLOR_PROBE_CELL_PX: usize = 32;
const COLOR_PROBE_DELTA_LIMIT: f64 = 0.12;
const COLOR_PROBE_MATCH_FLOOR: f64 = 0.95;
const TEXT_POSITION_P95_LIMIT_PT: f64 = 2.0;
const TEXT_POSITION_MAX_LIMIT_PT: f64 = 5.0;
const IMAGE_BOUNDS_TOLERANCE_PT: f64 = 0.5;

const OFFICE_CASE_IDS: [&str; 22] = [
    "office/docx/file1.docx",
    "office/docx/file2.docx",
    "office/pptx/file1.pptx",
    "office/pptx/file2.pptx",
    "office/xlsx/file1.xlsx",
    "office/xlsx/file2.xlsx",
    "office/xlsm/file1.xlsm",
    "office/xlsm/file2.xlsm",
    "office/fidelity-docx/fonts-substitution",
    "office/fidelity-docx/headers-footers",
    "office/fidelity-docx/hyperlinks",
    "office/fidelity-docx/justify-long-text",
    "office/fidelity-docx/lists-multilevel",
    "office/fidelity-docx/long-doc-pagination",
    "office/fidelity-docx/page-fields",
    "office/fidelity-docx/plain-paragraphs",
    "office/fidelity-docx/run-formatting-matrix",
    "office/fidelity-docx/spacing-exact-atleast",
    "office/fidelity-docx/table-fixed",
    "office/fidelity-docx/table-merges",
    "office/fidelity-docx/table-nested",
    "office/fidelity-docx/tabs-leaders",
];

#[derive(Debug, Deserialize)]
struct Manifest {
    goldens: Vec<GoldenRow>,
}

#[derive(Debug, Deserialize)]
struct GoldenRow {
    input_path: String,
    input_sha256: String,
    golden_path: String,
    golden_sha256: String,
}

#[derive(Debug, Deserialize)]
struct Allowlist {
    schema_version: u32,
    allowlisted: Vec<Disposition>,
    known_failures: Vec<KnownFailure>,
}

#[derive(Debug, Deserialize)]
struct Disposition {
    fixture_id: String,
    reason_code: String,
    justification: String,
    user_visible_effect: String,
}

#[derive(Debug, Deserialize)]
struct KnownFailure {
    fixture_id: String,
    reason_code: String,
    justification: String,
    user_visible_effect: String,
    tracking_note: String,
}

#[derive(Debug)]
struct Finding {
    fixture_id: String,
    reason_code: &'static str,
    detail: String,
}

#[derive(Clone, Copy)]
struct CasePolicy {
    word_coverage_floor: f64,
    page_window_radius: usize,
    dimension_tolerance_pt: f64,
    required_phrases: &'static [&'static str],
    minimum_page_objects: usize,
    minimum_images: usize,
}

#[derive(Debug)]
struct PdfiumMetrics {
    page_objects: usize,
    image_bounds: Vec<PositionedRect>,
    uris: BTreeSet<String>,
    word_positions: Vec<Vec<WordPosition>>,
}

#[derive(Clone, Copy, Debug)]
struct PositionedRect {
    page: usize,
    left: f64,
    bottom: f64,
    right: f64,
    top: f64,
}

#[derive(Debug)]
struct WordPosition {
    word: String,
    center_x: f64,
    center_y: f64,
}

#[derive(Debug)]
struct RasterPageMetrics {
    mean_abs: f64,
    max_region_abs: f64,
    color_probe_match: f64,
    color_probes_matched: usize,
    color_probes_total: usize,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(root: &Path, relative: &str) -> Vec<u8> {
    std::fs::read(root.join(relative)).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn read_json<T: for<'de> Deserialize<'de>>(root: &Path, relative: &str) -> T {
    serde_json::from_slice(&read(root, relative))
        .unwrap_or_else(|error| panic!("parse {relative}: {error}"))
}

fn fixture_id(golden_path: &str) -> String {
    golden_path
        .strip_prefix("fidelity-corpus/legacy-goldens/")
        .and_then(|path| path.strip_suffix(".libreoffice.pdf"))
        .unwrap_or_else(|| panic!("unexpected office golden path: {golden_path}"))
        .to_string()
}

fn office_format(input_path: &str) -> OfficeFormat {
    let extension = Path::new(input_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_else(|| panic!("missing extension: {input_path}"));
    OfficeFormat::from_extension(extension)
        .unwrap_or_else(|| panic!("unsupported office extension: {input_path}"))
}

fn policy(fixture_id: &str) -> CasePolicy {
    let required_phrases: &[&str] = match fixture_id {
        "office/fidelity-docx/headers-footers" => {
            &["Body content with a running footer", "Page 1 of 1"]
        }
        "office/fidelity-docx/hyperlinks" => &["Visit example com"],
        "office/fidelity-docx/lists-multilevel" => &["First", "Second", "Sub a", "Sub b", "Third"],
        "office/fidelity-docx/page-fields" => &[
            "The quick brown fox jumps over the lazy dog",
            "Page 1 of 11",
            "Page 11 of 11",
        ],
        "office/fidelity-docx/table-fixed" => &["R0C0", "R1C1", "R2C2"],
        "office/fidelity-docx/table-merges" => &["Merged", "C", "F"],
        "office/fidelity-docx/table-nested" => &["Outer", "Inner"],
        "office/xlsx/file1.xlsx" | "office/xlsm/file1.xlsm" => {
            &["Column A Column B Column C", "1 one TRUE", "5 five TRUE"]
        }
        "office/xlsx/file2.xlsx" | "office/xlsm/file2.xlsm" => &["X Y", "0 0", "9 81"],
        _ => &[],
    };

    CasePolicy {
        word_coverage_floor: 1.0,
        page_window_radius: 0,
        dimension_tolerance_pt: 0.05,
        required_phrases,
        minimum_page_objects: 1,
        minimum_images: 0,
    }
}

fn split_compound_word(word: &str) -> Vec<String> {
    let characters: Vec<char> = word.chars().collect();
    if characters.len() <= 3 && characters.iter().all(|character| character.is_uppercase()) {
        return characters
            .into_iter()
            .map(|character| character.to_lowercase().collect())
            .collect();
    }

    let mut result = Vec::new();
    let mut current = String::new();
    let mut previous = None;
    for character in characters {
        let boundary = previous.is_some_and(|previous: char| {
            (previous.is_lowercase() && character.is_uppercase())
                || (previous.is_alphabetic() && character.is_numeric())
                || (previous.is_numeric() && character.is_alphabetic())
        });
        if boundary && !current.is_empty() {
            result.push(current.to_lowercase());
            current.clear();
        }
        current.push(character);
        previous = Some(character);
    }
    if !current.is_empty() {
        result.push(current.to_lowercase());
    }
    result
}

fn words(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .flat_map(split_compound_word)
        .collect()
}

fn contains_sequence(haystack: &[String], needle: &[String]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn lcs_len(left: &[String], right: &[String]) -> usize {
    let mut previous = vec![0_usize; right.len() + 1];
    let mut current = vec![0_usize; right.len() + 1];
    for left_word in left {
        for (index, right_word) in right.iter().enumerate() {
            current[index + 1] = if left_word == right_word {
                previous[index] + 1
            } else {
                current[index].max(previous[index + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
        current.fill(0);
    }
    previous[right.len()]
}

fn page_window_coverages(
    golden_pages: &[String],
    current_pages: &[String],
    radius: usize,
) -> Vec<f64> {
    golden_pages
        .iter()
        .enumerate()
        .map(|(page_index, golden)| {
            let golden_words = words(golden);
            if golden_words.is_empty() {
                return 1.0;
            }
            let start = page_index.saturating_sub(radius);
            let end = (page_index + radius + 1).min(current_pages.len());
            let current_words: Vec<String> = current_pages[start..end]
                .iter()
                .flat_map(|page| words(page))
                .collect();
            lcs_len(&golden_words, &current_words) as f64 / golden_words.len() as f64
        })
        .collect()
}

fn non_latin_words(pages: &[String]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|page| words(page))
        .filter(|word| !word.is_ascii())
        .collect()
}

fn first_word_mismatch(golden: &[String], current: &[String]) -> Option<usize> {
    golden
        .iter()
        .zip(current)
        .position(|(golden, current)| golden != current)
        .or_else(|| (golden.len() != current.len()).then_some(golden.len().min(current.len())))
}

fn append_word_position(
    positions: &mut Vec<WordPosition>,
    raw_word: &mut String,
    bounds: &mut Option<(f64, f64, f64, f64)>,
) {
    let Some((left, bottom, right, top)) = bounds.take() else {
        raw_word.clear();
        return;
    };
    for word in split_compound_word(raw_word) {
        positions.push(WordPosition {
            word,
            center_x: (left + right) / 2.0,
            center_y: (bottom + top) / 2.0,
        });
    }
    raw_word.clear();
}

fn pdfium_metrics(pdf: &[u8]) -> Result<PdfiumMetrics, String> {
    let pdfium = pdf_core::pdfium::lock().map_err(|error| error.to_string())?;
    let document = pdfium
        .load_pdf_from_byte_slice(pdf, None)
        .map_err(|error| error.to_string())?;
    let mut page_objects = 0;
    let mut image_bounds = Vec::new();
    let mut uris = BTreeSet::new();
    let mut word_positions = Vec::new();
    for page_index in document.pages().as_range() {
        let page = document
            .pages()
            .get(page_index)
            .map_err(|error| error.to_string())?;
        page_objects += page
            .objects()
            .create_group(|_| true)
            .map_err(|error| error.to_string())?
            .len();
        let objects = page.objects();
        let images = objects
            .create_group(|object| object.as_image_object().is_some())
            .map_err(|error| error.to_string())?;
        for image_index in 0..images.len() {
            let mut image = images.get(image_index).map_err(|error| error.to_string())?;
            let mut singleton = objects.create_empty_group();
            singleton
                .push(&mut image)
                .map_err(|error| error.to_string())?;
            let bounds = singleton.bounds().map_err(|error| error.to_string())?;
            image_bounds.push(PositionedRect {
                page: page_index as usize,
                left: bounds.left().value as f64,
                bottom: bounds.bottom().value as f64,
                right: bounds.right().value as f64,
                top: bounds.top().value as f64,
            });
        }

        let text = page.text().map_err(|error| error.to_string())?;
        let mut page_word_positions = Vec::new();
        let mut raw_word = String::new();
        let mut word_bounds: Option<(f64, f64, f64, f64)> = None;
        for character in text.chars().iter() {
            let Some(value) = character.unicode_char() else {
                continue;
            };
            if value.is_alphanumeric() {
                raw_word.push(value);
                if let Ok(bounds) = character.loose_bounds() {
                    let character_bounds = (
                        bounds.left().value as f64,
                        bounds.bottom().value as f64,
                        bounds.right().value as f64,
                        bounds.top().value as f64,
                    );
                    word_bounds = Some(match word_bounds {
                        Some((left, bottom, right, top)) => (
                            left.min(character_bounds.0),
                            bottom.min(character_bounds.1),
                            right.max(character_bounds.2),
                            top.max(character_bounds.3),
                        ),
                        None => character_bounds,
                    });
                }
            } else {
                append_word_position(&mut page_word_positions, &mut raw_word, &mut word_bounds);
            }
        }
        append_word_position(&mut page_word_positions, &mut raw_word, &mut word_bounds);
        word_positions.push(page_word_positions);

        for link in page.links().iter() {
            if let Some(action) = link.action()
                && let Some(uri_action) = action.as_uri_action()
            {
                uris.insert(uri_action.uri().map_err(|error| error.to_string())?);
            }
        }
    }
    Ok(PdfiumMetrics {
        page_objects,
        image_bounds,
        uris,
        word_positions,
    })
}

fn normalize_navigation_uri(uri: &str) -> String {
    let Some((scheme, remainder)) = uri.split_once("://") else {
        return uri.to_string();
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return uri.to_string();
    }
    if let Some(authority) = remainder.strip_suffix('/')
        && !authority.contains('/')
        && !authority.contains('?')
        && !authority.contains('#')
    {
        format!(
            "{}://{}",
            scheme.to_ascii_lowercase(),
            authority.to_ascii_lowercase()
        )
    } else {
        uri.to_string()
    }
}

fn normalized_uri_set(uris: &BTreeSet<String>) -> BTreeSet<String> {
    uris.iter()
        .map(|uri| normalize_navigation_uri(uri))
        .collect()
}

fn render_raster(pdf: &[u8], page: u32) -> Result<DynamicImage, String> {
    let png = render_page(
        pdf,
        page,
        &RenderOptions {
            scale: RASTER_SCALE,
            ..Default::default()
        },
    )
    .map_err(|error| error.to_string())?;
    image::load_from_memory(&png).map_err(|error| error.to_string())
}

fn white_padded_rgb(image: &DynamicImage, width: u32, height: u32) -> RgbImage {
    let source = image.to_rgb8();
    let mut padded = RgbImage::from_pixel(width, height, image::Rgb([255, 255, 255]));
    for (x, y, pixel) in source.enumerate_pixels() {
        padded.put_pixel(x, y, *pixel);
    }
    padded
}

fn mean_abs_region(
    golden: &RgbImage,
    current: &RgbImage,
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
) -> f64 {
    let mut total = 0_u64;
    let mut channels = 0_usize;
    for y in top..bottom {
        for x in left..right {
            let golden_pixel = golden.get_pixel(x as u32, y as u32).0;
            let current_pixel = current.get_pixel(x as u32, y as u32).0;
            for channel in 0..3 {
                total += golden_pixel[channel].abs_diff(current_pixel[channel]) as u64;
                channels += 1;
            }
        }
    }
    total as f64 / (channels.max(1) as f64 * 255.0)
}

fn color_probe_metrics(golden: &RgbImage, current: &RgbImage) -> (usize, usize, f64) {
    let (width, height) = golden.dimensions();
    let mut matched = 0_usize;
    let mut total = 0_usize;
    for top in (0..height as usize).step_by(COLOR_PROBE_CELL_PX) {
        for left in (0..width as usize).step_by(COLOR_PROBE_CELL_PX) {
            let bottom = (top + COLOR_PROBE_CELL_PX).min(height as usize);
            let right = (left + COLOR_PROBE_CELL_PX).min(width as usize);
            let mut strongest: Option<(u16, usize, usize)> = None;
            for y in top..bottom {
                for x in left..right {
                    let pixel = golden.get_pixel(x as u32, y as u32).0;
                    let distance = pixel.iter().map(|channel| 255_u16 - *channel as u16).sum();
                    if strongest.is_none_or(|(best, _, _)| distance > best) {
                        strongest = Some((distance, x, y));
                    }
                }
            }
            let Some((distance, x, y)) = strongest else {
                continue;
            };
            if distance < 48 {
                continue;
            }
            total += 1;
            let golden_pixel = golden.get_pixel(x as u32, y as u32).0;
            let current_pixel = current.get_pixel(x as u32, y as u32).0;
            let delta = (0..3)
                .map(|channel| golden_pixel[channel].abs_diff(current_pixel[channel]) as f64)
                .sum::<f64>()
                / (3.0 * 255.0);
            if delta <= COLOR_PROBE_DELTA_LIMIT {
                matched += 1;
            }
        }
    }
    let ratio = if total == 0 {
        1.0
    } else {
        matched as f64 / total as f64
    };
    (matched, total, ratio)
}

fn compare_rasters(golden: &DynamicImage, current: &DynamicImage) -> RasterPageMetrics {
    let width = golden.width().max(current.width());
    let height = golden.height().max(current.height());
    let golden = white_padded_rgb(golden, width, height);
    let current = white_padded_rgb(current, width, height);
    let mean_abs = mean_abs_region(&golden, &current, 0, 0, width as usize, height as usize);
    let mut max_region_abs = 0.0_f64;
    for row in 0..RASTER_REGION_GRID {
        for column in 0..RASTER_REGION_GRID {
            let left = column * width as usize / RASTER_REGION_GRID;
            let right = (column + 1) * width as usize / RASTER_REGION_GRID;
            let top = row * height as usize / RASTER_REGION_GRID;
            let bottom = (row + 1) * height as usize / RASTER_REGION_GRID;
            max_region_abs =
                max_region_abs.max(mean_abs_region(&golden, &current, left, top, right, bottom));
        }
    }
    let (color_probes_matched, color_probes_total, color_probe_match) =
        color_probe_metrics(&golden, &current);
    RasterPageMetrics {
        mean_abs,
        max_region_abs,
        color_probe_match,
        color_probes_matched,
        color_probes_total,
    }
}

fn percentile_95(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[((values.len() * 95).div_ceil(100)).saturating_sub(1)]
}

fn text_position_metrics(
    golden_pages: &[Vec<WordPosition>],
    current_pages: &[Vec<WordPosition>],
) -> Result<(f64, f64, usize), String> {
    if golden_pages.len() != current_pages.len() {
        return Err(format!(
            "page count differs: golden={} current={}",
            golden_pages.len(),
            current_pages.len()
        ));
    }
    let mut deltas = Vec::new();
    for (page_index, (golden, current)) in golden_pages.iter().zip(current_pages).enumerate() {
        let golden_words: Vec<&str> = golden.iter().map(|word| word.word.as_str()).collect();
        let current_words: Vec<&str> = current.iter().map(|word| word.word.as_str()).collect();
        if golden_words != current_words {
            let mismatch = golden_words
                .iter()
                .zip(&current_words)
                .position(|(golden, current)| golden != current)
                .or_else(|| {
                    (golden_words.len() != current_words.len())
                        .then_some(golden_words.len().min(current_words.len()))
                });
            return Err(format!(
                "page={} word-box alignment differs at {mismatch:?}; golden-words={} current-words={}",
                page_index + 1,
                golden_words.len(),
                current_words.len()
            ));
        }
        deltas.extend(golden.iter().zip(current).map(|(golden, current)| {
            (golden.center_x - current.center_x).hypot(golden.center_y - current.center_y)
        }));
    }
    let maximum = deltas.iter().copied().fold(0.0_f64, f64::max);
    let count = deltas.len();
    let p95 = percentile_95(&mut deltas);
    Ok((p95, maximum, count))
}

fn maximum_image_bounds_delta(golden: &[PositionedRect], current: &[PositionedRect]) -> f64 {
    golden
        .iter()
        .zip(current)
        .map(|(golden, current)| {
            if golden.page != current.page {
                return f64::INFINITY;
            }
            (golden.left - current.left)
                .abs()
                .max((golden.bottom - current.bottom).abs())
                .max((golden.right - current.right).abs())
                .max((golden.top - current.top).abs())
        })
        .fold(0.0_f64, f64::max)
}

fn finding(fixture_id: &str, reason_code: &'static str, detail: impl Into<String>) -> Finding {
    Finding {
        fixture_id: fixture_id.to_string(),
        reason_code,
        detail: detail.into(),
    }
}

fn conversion_error_finding(fixture_id: &str, error: &OfficeError) -> Finding {
    match error {
        OfficeError::Unsupported { code, detail } => finding(
            fixture_id,
            "OFFICE_CONVERSION_UNSUPPORTED",
            format!(
                "code={} detail={detail}",
                serde_json::to_string(code).expect("serialize stable UnsupportedCode")
            ),
        ),
        OfficeError::Invalid(detail) => {
            finding(fixture_id, "OFFICE_CONVERSION_INVALID", detail.clone())
        }
        OfficeError::Font(detail) => finding(fixture_id, "OFFICE_CONVERSION_FONT", detail.clone()),
        OfficeError::Internal(detail) => {
            finding(fixture_id, "OFFICE_CONVERSION_INTERNAL", detail.clone())
        }
    }
}

fn evaluate_case(root: &Path, row: &GoldenRow) -> Vec<Finding> {
    let fixture_id = fixture_id(&row.golden_path);
    let policy = policy(&fixture_id);
    let input = read(root, &row.input_path);
    let golden = read(root, &row.golden_path);
    verify_sha256(&root.join(&row.input_path), &input, &row.input_sha256)
        .unwrap_or_else(|error| panic!("{error}"));
    verify_sha256(&root.join(&row.golden_path), &golden, &row.golden_sha256)
        .unwrap_or_else(|error| panic!("{error}"));
    let options = ConvertOptions {
        source_name: Path::new(&row.input_path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string),
        ..ConvertOptions::default()
    };
    let output = match convert(&input, office_format(&row.input_path), &options) {
        Ok(output) => output,
        Err(error) => return vec![conversion_error_finding(&fixture_id, &error)],
    };

    let mut findings = Vec::new();
    let golden_info = validate(&golden).expect("frozen office golden must validate");
    match validate(&output.pdf) {
        Ok(current_info) if current_info.page_count == output.page_count => {}
        Ok(current_info) => findings.push(finding(
            &fixture_id,
            "OFFICE_PDF_VALIDATION",
            format!(
                "layout page_count={} reopened page_count={}",
                output.page_count, current_info.page_count
            ),
        )),
        Err(error) => findings.push(finding(
            &fixture_id,
            "OFFICE_PDF_VALIDATION",
            error.to_string(),
        )),
    }
    if output.page_count != golden_info.page_count {
        findings.push(finding(
            &fixture_id,
            "OFFICE_PAGE_COUNT",
            format!(
                "golden={} current={}",
                golden_info.page_count, output.page_count
            ),
        ));
    }

    let golden_sizes = page_sizes(&golden).expect("read golden page sizes");
    let current_sizes = page_sizes(&output.pdf).expect("read current page sizes");
    let mut maximum_dimension_delta = 0.0_f64;
    for (page_index, (golden_size, current_size)) in
        golden_sizes.iter().zip(&current_sizes).enumerate()
    {
        let same_orientation = (golden_size.0 > golden_size.1) == (current_size.0 > current_size.1);
        if !same_orientation {
            findings.push(finding(
                &fixture_id,
                "OFFICE_PAGE_ORIENTATION",
                format!(
                    "page={} golden={golden_size:?} current={current_size:?}",
                    page_index + 1
                ),
            ));
        }
        let width_delta = (golden_size.0 - current_size.0).abs();
        let height_delta = (golden_size.1 - current_size.1).abs();
        maximum_dimension_delta = maximum_dimension_delta.max(width_delta).max(height_delta);
        if width_delta > policy.dimension_tolerance_pt
            || height_delta > policy.dimension_tolerance_pt
        {
            findings.push(finding(
                &fixture_id,
                "OFFICE_PAGE_DIMENSIONS",
                format!(
                    "page={} golden={golden_size:?} current={current_size:?} tolerance={}pt",
                    page_index + 1,
                    policy.dimension_tolerance_pt
                ),
            ));
        }
    }

    let golden_pages = extract_text_pages(&golden).expect("extract golden pages");
    let current_pages = extract_text_pages(&output.pdf).expect("extract current pages");
    for (page_index, (golden_page, current_page)) in
        golden_pages.iter().zip(&current_pages).enumerate()
    {
        let golden_words = words(golden_page);
        let current_words = words(current_page);
        if golden_words != current_words {
            findings.push(finding(
                &fixture_id,
                "OFFICE_PAGE_WORD_ORDER",
                format!(
                    "page={} mismatch={:?} golden-words={} current-words={}",
                    page_index + 1,
                    first_word_mismatch(&golden_words, &current_words),
                    golden_words.len(),
                    current_words.len()
                ),
            ));
        }
    }
    let coverages = page_window_coverages(&golden_pages, &current_pages, policy.page_window_radius);
    let minimum_coverage = coverages.iter().copied().fold(1.0_f64, f64::min);
    if minimum_coverage + f64::EPSILON < policy.word_coverage_floor {
        findings.push(finding(
            &fixture_id,
            "OFFICE_WORD_SEQUENCE_COVERAGE",
            format!(
                "minimum={minimum_coverage:.4} floor={:.4} pages={coverages:?} radius={}",
                policy.word_coverage_floor, policy.page_window_radius
            ),
        ));
    }
    let current_words: Vec<String> = current_pages.iter().flat_map(|page| words(page)).collect();
    for phrase in policy.required_phrases {
        let phrase_words = words(phrase);
        if !contains_sequence(&current_words, &phrase_words) {
            findings.push(finding(
                &fixture_id,
                "OFFICE_REQUIRED_STRUCTURE_TEXT",
                format!("missing phrase {phrase:?}"),
            ));
        }
    }
    let unicode_words = non_latin_words(&golden_pages);
    if !contains_sequence(&current_words, &unicode_words) {
        findings.push(finding(
            &fixture_id,
            "OFFICE_UNICODE_LOGICAL_ORDER",
            format!("missing ordered Unicode words {unicode_words:?}"),
        ));
    }

    let golden_metrics = pdfium_metrics(&golden).expect("inspect golden with pdfium");
    let current_metrics = pdfium_metrics(&output.pdf).expect("inspect current with pdfium");
    if current_metrics.page_objects < policy.minimum_page_objects {
        findings.push(finding(
            &fixture_id,
            "OFFICE_PAGE_OBJECT_PRESENCE",
            format!(
                "golden={} current={} expected-at-least={}",
                golden_metrics.page_objects,
                current_metrics.page_objects,
                policy.minimum_page_objects
            ),
        ));
    }
    let expected_image_count = policy.minimum_images.max(golden_metrics.image_bounds.len());
    if current_metrics.image_bounds.len() != expected_image_count {
        findings.push(finding(
            &fixture_id,
            "OFFICE_IMAGE_COUNT",
            format!(
                "golden={} current={} expected={expected_image_count}",
                golden_metrics.image_bounds.len(),
                current_metrics.image_bounds.len()
            ),
        ));
    }
    let image_bounds_delta =
        if golden_metrics.image_bounds.len() == current_metrics.image_bounds.len() {
            maximum_image_bounds_delta(&golden_metrics.image_bounds, &current_metrics.image_bounds)
        } else {
            f64::INFINITY
        };
    if image_bounds_delta > IMAGE_BOUNDS_TOLERANCE_PT {
        findings.push(finding(
            &fixture_id,
            "OFFICE_IMAGE_BOUNDS",
            format!(
                "max-delta={image_bounds_delta:.4}pt tolerance={IMAGE_BOUNDS_TOLERANCE_PT:.4}pt golden={:?} current={:?}",
                golden_metrics.image_bounds, current_metrics.image_bounds
            ),
        ));
    }

    let golden_navigation_targets = normalized_uri_set(&golden_metrics.uris);
    let current_navigation_targets = normalized_uri_set(&current_metrics.uris);
    if golden_navigation_targets != current_navigation_targets {
        findings.push(finding(
            &fixture_id,
            "OFFICE_LINK_URI_TARGET_SET",
            format!("golden={golden_navigation_targets:?} current={current_navigation_targets:?}"),
        ));
    } else if golden_metrics.uris != current_metrics.uris {
        findings.push(finding(
            &fixture_id,
            "OFFICE_LINK_URI_SERIALIZATION",
            format!(
                "navigation targets match; golden={:?} current={:?}",
                golden_metrics.uris, current_metrics.uris
            ),
        ));
    }

    let mut visual_reds = Vec::new();
    let (text_position_p95, text_position_max, positioned_words) = match text_position_metrics(
        &golden_metrics.word_positions,
        &current_metrics.word_positions,
    ) {
        Ok(metrics) => metrics,
        Err(detail) => {
            visual_reds.push(format!("text-position-alignment: {detail}"));
            (f64::INFINITY, f64::INFINITY, 0)
        }
    };
    if text_position_p95 > TEXT_POSITION_P95_LIMIT_PT
        || text_position_max > TEXT_POSITION_MAX_LIMIT_PT
    {
        visual_reds.push(format!(
                "words={positioned_words} p95={text_position_p95:.4}pt limit={TEXT_POSITION_P95_LIMIT_PT:.4}pt max={text_position_max:.4}pt limit={TEXT_POSITION_MAX_LIMIT_PT:.4}pt"
            ));
    }

    let mut maximum_mean_abs = 0.0_f64;
    let mut maximum_region_abs = 0.0_f64;
    let mut minimum_color_probe_match = 1.0_f64;
    let mut color_probes_matched = 0_usize;
    let mut color_probes_total = 0_usize;
    for page_index in 0..golden_info.page_count.min(output.page_count) {
        let golden_raster = render_raster(&golden, page_index).unwrap_or_else(|error| {
            panic!(
                "render golden {fixture_id} page {}: {error}",
                page_index + 1
            )
        });
        let current_raster = render_raster(&output.pdf, page_index).unwrap_or_else(|error| {
            panic!(
                "render current {fixture_id} page {}: {error}",
                page_index + 1
            )
        });
        let metrics = compare_rasters(&golden_raster, &current_raster);
        maximum_mean_abs = maximum_mean_abs.max(metrics.mean_abs);
        maximum_region_abs = maximum_region_abs.max(metrics.max_region_abs);
        minimum_color_probe_match = minimum_color_probe_match.min(metrics.color_probe_match);
        color_probes_matched += metrics.color_probes_matched;
        color_probes_total += metrics.color_probes_total;
        if metrics.mean_abs > RASTER_MEAN_ABS_LIMIT
            || metrics.max_region_abs > RASTER_MAX_REGION_ABS_LIMIT
        {
            visual_reds.push(format!(
                    "page={} mean-abs={:.6} limit={RASTER_MEAN_ABS_LIMIT:.6} max-{}x{}-region-abs={:.6} limit={RASTER_MAX_REGION_ABS_LIMIT:.6}",
                    page_index + 1,
                    metrics.mean_abs,
                    RASTER_REGION_GRID,
                    RASTER_REGION_GRID,
                    metrics.max_region_abs
                ));
        }
        if metrics.color_probe_match < COLOR_PROBE_MATCH_FLOOR {
            visual_reds.push(format!(
                    "page={} matched={}/{} ratio={:.4} floor={COLOR_PROBE_MATCH_FLOOR:.4} per-probe-delta-limit={COLOR_PROBE_DELTA_LIMIT:.4}",
                    page_index + 1,
                    metrics.color_probes_matched,
                    metrics.color_probes_total,
                    metrics.color_probe_match
                ));
        }
    }
    if !visual_reds.is_empty() {
        findings.push(finding(
            &fixture_id,
            "OFFICE_VISUAL_PARITY",
            visual_reds.join("; "),
        ));
    }

    eprintln!(
        "LEGACY OFFICE {fixture_id}: pages={}->{}; min-word-coverage={minimum_coverage:.4}; max-dimension-delta={maximum_dimension_delta:.4}pt/{}pt; raster@{RASTER_DPI}dpi mean-abs={maximum_mean_abs:.6}/{RASTER_MEAN_ABS_LIMIT:.6} max-region-abs={maximum_region_abs:.6}/{RASTER_MAX_REGION_ABS_LIMIT:.6}; color-probes={color_probes_matched}/{color_probes_total} min-page-ratio={minimum_color_probe_match:.4}/{COLOR_PROBE_MATCH_FLOOR:.4}; text-position-p95={text_position_p95:.4}/{TEXT_POSITION_P95_LIMIT_PT:.4}pt max={text_position_max:.4}/{TEXT_POSITION_MAX_LIMIT_PT:.4}pt words={positioned_words}; objects={}->{}; images={}->{} bounds-max-delta={image_bounds_delta:.4}/{IMAGE_BOUNDS_TOLERANCE_PT:.4}pt; uris={:?}->{:?}",
        golden_info.page_count,
        output.page_count,
        policy.dimension_tolerance_pt,
        golden_metrics.page_objects,
        current_metrics.page_objects,
        golden_metrics.image_bounds.len(),
        current_metrics.image_bounds.len(),
        golden_metrics.uris,
        current_metrics.uris,
    );
    findings
}

#[test]
fn every_frozen_office_golden_has_an_honest_typed_disposition() {
    let root = root();
    let manifest: Manifest = read_json(&root, "fidelity-corpus/legacy-goldens/manifest.json");
    let allowlist: Allowlist = read_json(&root, "fidelity-corpus/legacy-parity-allowlist.json");
    assert_eq!(allowlist.schema_version, 2, "unknown allowlist schema");

    let office_rows: Vec<&GoldenRow> = manifest
        .goldens
        .iter()
        .filter(|row| row.golden_path.contains("/office/"))
        .collect();
    assert_eq!(
        manifest.goldens.len(),
        34,
        "legacy manifest row-count drift"
    );
    assert_eq!(office_rows.len(), 22, "office manifest row-count drift");
    assert_eq!(
        manifest
            .goldens
            .iter()
            .filter(|row| row.golden_path.contains("/redaction/"))
            .count(),
        3,
        "redaction manifest row-count drift"
    );
    let manifest_ids: BTreeSet<String> = office_rows
        .iter()
        .map(|row| fixture_id(&row.golden_path))
        .collect();
    let policy_ids: BTreeSet<String> = OFFICE_CASE_IDS.iter().map(|id| (*id).to_string()).collect();
    assert_eq!(
        manifest_ids, policy_ids,
        "office manifest/policy coverage drift"
    );

    for disposition in &allowlist.allowlisted {
        assert!(
            disposition.fixture_id.starts_with("office/")
                || disposition.fixture_id.starts_with("redaction/"),
            "unknown typed allowlist fixture group: {}",
            disposition.fixture_id
        );
        assert!(!disposition.reason_code.trim().is_empty());
        assert!(!disposition.justification.trim().is_empty());
        assert!(!disposition.user_visible_effect.trim().is_empty());
    }
    for failure in &allowlist.known_failures {
        assert!(!failure.fixture_id.trim().is_empty());
        assert!(!failure.reason_code.trim().is_empty());
        assert!(!failure.justification.trim().is_empty());
        assert!(!failure.user_visible_effect.trim().is_empty());
        assert!(!failure.tracking_note.trim().is_empty());
    }
    let office_allowlisted: Vec<&Disposition> = allowlist
        .allowlisted
        .iter()
        .filter(|entry| entry.fixture_id.starts_with("office/"))
        .collect();
    let allowlisted_keys: BTreeSet<(&str, &str)> = office_allowlisted
        .iter()
        .map(|entry| (entry.fixture_id.as_str(), entry.reason_code.as_str()))
        .collect();
    assert_eq!(
        allowlisted_keys.len(),
        office_allowlisted.len(),
        "duplicate typed allowlist entries"
    );
    let office_known_failures: Vec<&KnownFailure> = allowlist
        .known_failures
        .iter()
        .filter(|entry| entry.fixture_id.starts_with("office/"))
        .collect();
    let known_failure_keys: BTreeSet<(&str, &str)> = office_known_failures
        .iter()
        .map(|entry| (entry.fixture_id.as_str(), entry.reason_code.as_str()))
        .collect();
    assert_eq!(
        known_failure_keys.len(),
        office_known_failures.len(),
        "duplicate typed known-failure entries"
    );
    assert!(
        allowlisted_keys.is_disjoint(&known_failure_keys),
        "same office metric cannot be both allowlisted and red"
    );

    let mut observed_allowlisted = BTreeSet::new();
    let mut observed_known_failures = BTreeSet::new();
    let mut unexpected = Vec::new();
    let mut green_rows = 0_usize;
    let mut allowlisted_rows = 0_usize;
    let mut red_rows = 0_usize;
    for row in office_rows {
        let findings = evaluate_case(&root, row);
        let mut row_allowlisted = false;
        let mut row_red = false;
        for finding in findings {
            let key = (finding.fixture_id.as_str(), finding.reason_code);
            if allowlisted_keys.contains(&key) {
                eprintln!(
                    "ALLOWLISTED {} {}: {}",
                    finding.fixture_id, finding.reason_code, finding.detail
                );
                row_allowlisted = true;
                observed_allowlisted.insert((finding.fixture_id, finding.reason_code.to_string()));
            } else if known_failure_keys.contains(&key) {
                eprintln!(
                    "RED TRACKED {} {}: {}",
                    finding.fixture_id, finding.reason_code, finding.detail
                );
                row_red = true;
                observed_known_failures
                    .insert((finding.fixture_id, finding.reason_code.to_string()));
            } else {
                unexpected.push(format!(
                    "{} {}: {}",
                    finding.fixture_id, finding.reason_code, finding.detail
                ));
            }
        }
        if row_red {
            red_rows += 1;
        } else if row_allowlisted {
            allowlisted_rows += 1;
        } else {
            green_rows += 1;
        }
    }

    let expected_allowlisted: BTreeSet<(String, String)> = allowlisted_keys
        .iter()
        .map(|(fixture, code)| ((*fixture).to_string(), (*code).to_string()))
        .collect();
    assert_eq!(
        observed_allowlisted, expected_allowlisted,
        "typed office allowlist is stale or unexercised"
    );
    let expected_known_failures: BTreeSet<(String, String)> = known_failure_keys
        .iter()
        .map(|(fixture, code)| ((*fixture).to_string(), (*code).to_string()))
        .collect();
    assert_eq!(
        observed_known_failures, expected_known_failures,
        "typed office known-failure registry is stale or unexercised"
    );
    assert!(
        unexpected.is_empty(),
        "legacy office parity failures:\n{}",
        unexpected.join("\n")
    );
    eprintln!(
        "LEGACY OFFICE DISPOSITIONS: green={green_rows} allowlisted={allowlisted_rows} red={red_rows}"
    );
}

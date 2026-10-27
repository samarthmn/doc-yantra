//! Dev-only vector geometry probe for pairing engine and Word PDF elements.

use anyhow::{Context, Result, anyhow, bail};
use fidelity::evidence::sha256_hex;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const PAIR_DISTANCE_PT: f64 = 3.0;
const DUPLICATE_DISTANCE_PT: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl Orientation {
    fn class(self) -> &'static str {
        match self {
            Orientation::Horizontal => "h_border",
            Orientation::Vertical => "v_border",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BorderElement {
    pub orientation: Orientation,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub width: f64,
}

impl BorderElement {
    fn center(&self) -> Point {
        Point {
            x: (self.x0 + self.x1) / 2.0,
            y: (self.y0 + self.y1) / 2.0,
        }
    }

    fn length(&self) -> f64 {
        match self.orientation {
            Orientation::Horizontal => (self.x1 - self.x0).abs(),
            Orientation::Vertical => (self.y1 - self.y0).abs(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TextOrigin {
    pub x: f64,
    pub y: f64,
    pub font_size: f64,
    pub first_character: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchRecord {
    pub class: String,
    pub ours_index: usize,
    pub truth_index: usize,
    pub dx: f64,
    pub dy: f64,
    pub y_band: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_character: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeltaCluster {
    pub class: String,
    pub y_band: u8,
    pub dx: f64,
    pub dy: f64,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnmatchedReport {
    pub ours_borders: Vec<usize>,
    pub truth_borders: Vec<usize>,
    pub ours_text: Vec<usize>,
    pub truth_text: Vec<usize>,
    pub elements: Vec<UnmatchedElement>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnmatchedElement {
    pub source: String,
    pub class: String,
    pub index: usize,
    pub x0: f64,
    pub y0: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x1: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y1: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_character: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateRecord {
    pub first_index: usize,
    pub second_index: usize,
    pub orientation: Orientation,
    pub separation: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub page: u32,
    pub page_width: f64,
    pub page_height: f64,
    pub ours_borders: usize,
    pub truth_borders: usize,
    pub ours_text: usize,
    pub truth_text: usize,
    /// Raw text origins on both sides, for calibration diagnostics.
    pub ours_text_origins: Vec<TextOrigin>,
    pub truth_text_origins: Vec<TextOrigin>,
    pub matches: Vec<MatchRecord>,
    pub clusters: Vec<DeltaCluster>,
    pub unmatched: UnmatchedReport,
    pub duplicates: Vec<DuplicateRecord>,
    /// Lossless text/image content records from our page.
    pub ours_content: PdfPageContent,
    /// Lossless text/image content records from the Word page.
    pub truth_content: PdfPageContent,
}

/// Text and image records decoded from one PDF page's content streams.
#[derive(Debug, Clone, Serialize)]
pub struct PdfPageContent {
    pub page: u32,
    pub text: Vec<TextShowRecord>,
    pub images: Vec<ImageXObjectRecord>,
}

/// One text-showing operator with its exact PDF font and advance inputs.
#[derive(Debug, Clone, Serialize)]
pub struct TextShowRecord {
    pub operator_index: usize,
    pub operator: String,
    pub resource_name: String,
    pub font_base_name: Option<String>,
    pub font_family: Option<String>,
    pub font_style: Option<String>,
    pub font_subtype: Option<String>,
    pub descendant_subtype: Option<String>,
    pub font_encoding: Option<String>,
    pub tf_pt: f64,
    pub tm: [f64; 6],
    pub ctm: [f64; 6],
    pub origin: [f64; 2],
    pub unicode: String,
    pub encoded_hex: String,
    pub glyphs: Vec<PdfGlyphRecord>,
    pub tj_adjustments: Vec<f64>,
    pub resulting_advance_text_units: f64,
    pub resulting_advance_pt: f64,
}

/// One encoded PDF character code and its nominal/placed advance.
#[derive(Debug, Clone, Serialize)]
pub struct PdfGlyphRecord {
    pub source_code: u32,
    pub source_cid: Option<u32>,
    pub source_gid: Option<u16>,
    pub unicode: String,
    pub nominal_width_text_units: f64,
    pub following_tj: f64,
    pub resulting_advance_text_units: f64,
}

/// One placed image XObject and the metadata that affects PDF rendering.
#[derive(Debug, Clone, Serialize)]
pub struct ImageXObjectRecord {
    pub resource_name: String,
    pub object_id: Option<[u32; 2]>,
    pub encoded_sha256: String,
    pub encoded_length: usize,
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
    pub filters: Vec<String>,
    pub color_space: Option<String>,
    pub decode_parameters: Option<String>,
    pub placement_matrix: [f64; 6],
    pub clip_rectangle: Option<PdfRectangle>,
    pub jpeg: Option<JpegMetadata>,
}

/// Axis-aligned PDF-user-space rectangle.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PdfRectangle {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// JPEG frame, quantization, and chroma-sampling metadata.
#[derive(Debug, Clone, Serialize)]
pub struct JpegMetadata {
    pub frame_marker: String,
    pub precision_bits: u8,
    pub components: Vec<JpegComponent>,
    pub quantization_tables: Vec<JpegQuantizationTable>,
}

/// JPEG component sampling factors and quantization-table selector.
#[derive(Debug, Clone, Serialize)]
pub struct JpegComponent {
    pub component_id: u8,
    pub horizontal_sampling: u8,
    pub vertical_sampling: u8,
    pub quantization_table: u8,
}

/// One JPEG DQT table in stored zig-zag order.
#[derive(Debug, Clone, Serialize)]
pub struct JpegQuantizationTable {
    pub table_id: u8,
    pub precision_bits: u8,
    pub values: Vec<u16>,
}

/// Per-case vector-geometry bounds. Values are in PDF points except the
/// unmatched fraction, which is a percentage.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct GeometryLimits {
    pub median_pt: f64,
    pub p95_pt: f64,
    pub max_unmatched_pct: f64,
}

impl Default for GeometryLimits {
    fn default() -> Self {
        Self {
            // Tuned below the 0.5pt displacement produced by a one-pixel shift
            // at the harness's 144dpi raster scale.
            median_pt: 0.4,
            p95_pt: 1.5,
            max_unmatched_pct: 5.0,
        }
    }
}

/// Aggregate paired-origin statistics for one vector element class.
#[derive(Debug, Clone, Serialize)]
pub struct GeometryClassMetrics {
    pub element_class: String,
    pub ours: usize,
    pub truth: usize,
    pub matched: usize,
    pub median_dx: f64,
    pub median_dy: f64,
    pub p95_dx: f64,
    pub p95_dy: f64,
    pub unmatched_pct: f64,
}

/// Blocking vector-geometry result for one page.
#[derive(Debug, Clone, Serialize)]
pub struct GeometryPageMetrics {
    pub page: u32,
    pub borders: GeometryClassMetrics,
    pub text: GeometryClassMetrics,
    pub pass: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub violation: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn distance(self, other: Point) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

/// PDF affine matrix `[a b c d e f]`, mapping `(x,y)` to
/// `(a*x+c*y+e, b*x+d*y+f)`.
#[derive(Debug, Clone, Copy)]
struct Affine {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Default for Affine {
    fn default() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
}

impl Affine {
    fn from_operands(operands: &[Object]) -> Option<Self> {
        Some(Self {
            a: number(operands.first()?)?,
            b: number(operands.get(1)?)?,
            c: number(operands.get(2)?)?,
            d: number(operands.get(3)?)?,
            e: number(operands.get(4)?)?,
            f: number(operands.get(5)?)?,
        })
    }

    /// Composition `self ∘ local`: local coordinates pass through `local`
    /// before the current transform in `self`.
    fn concat(self, local: Self) -> Self {
        Self {
            a: self.a * local.a + self.c * local.b,
            b: self.b * local.a + self.d * local.b,
            c: self.a * local.c + self.c * local.d,
            d: self.b * local.c + self.d * local.d,
            e: self.a * local.e + self.c * local.f + self.e,
            f: self.b * local.e + self.d * local.f + self.f,
        }
    }

    fn translation(x: f64, y: f64) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::default()
        }
    }

    fn transform(self, point: Point) -> Point {
        Point {
            x: self.a * point.x + self.c * point.y + self.e,
            y: self.b * point.x + self.d * point.y + self.f,
        }
    }

    fn x_scale(self) -> f64 {
        self.a.hypot(self.b)
    }

    fn y_scale(self) -> f64 {
        self.c.hypot(self.d)
    }

    fn as_array(self) -> [f64; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }
}

#[derive(Debug, Clone)]
enum PathItem {
    Line(Point, Point),
    Rect([Point; 4]),
}

#[derive(Debug, Clone)]
struct GraphicsState {
    ctm: Affine,
    line_width: f64,
    path: Vec<PathItem>,
    current_point: Option<Point>,
    subpath_start: Option<Point>,
    text_matrix: Affine,
    text_line_matrix: Affine,
    text_leading: f64,
    character_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    font_name: Option<Vec<u8>>,
    font_size: f64,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Affine::default(),
            line_width: 1.0,
            path: Vec::new(),
            current_point: None,
            subpath_start: None,
            text_matrix: Affine::default(),
            text_line_matrix: Affine::default(),
            text_leading: 0.0,
            character_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            font_name: None,
            font_size: 0.0,
        }
    }
}

#[derive(Debug)]
struct ExtractedPage {
    width: f64,
    height: f64,
    media_x0: f64,
    media_y1: f64,
    borders: Vec<BorderElement>,
    text: Vec<TextOrigin>,
}

impl ExtractedPage {
    fn normalize_point(&self, point: Point) -> Point {
        Point {
            x: round2(point.x - self.media_x0),
            y: round2(self.media_y1 - point.y),
        }
    }

    fn push_line(&mut self, first: Point, second: Point, width: f64) {
        let first = self.normalize_point(first);
        let second = self.normalize_point(second);
        let dx = (second.x - first.x).abs();
        let dy = (second.y - first.y).abs();
        let (orientation, x0, y0, x1, y1) = if dy <= 0.05 && dx > 0.05 {
            (
                Orientation::Horizontal,
                first.x.min(second.x),
                (first.y + second.y) / 2.0,
                first.x.max(second.x),
                (first.y + second.y) / 2.0,
            )
        } else if dx <= 0.05 && dy > 0.05 {
            (
                Orientation::Vertical,
                (first.x + second.x) / 2.0,
                first.y.min(second.y),
                (first.x + second.x) / 2.0,
                first.y.max(second.y),
            )
        } else {
            return;
        };
        self.borders.push(BorderElement {
            orientation,
            x0: round2(x0),
            y0: round2(y0),
            x1: round2(x1),
            y1: round2(y1),
            width: round2(width.abs()),
        });
    }

    fn push_thin_rect(&mut self, corners: [Point; 4]) {
        let corners = corners.map(|point| self.normalize_point(point));
        let min_x = corners
            .iter()
            .map(|point| point.x)
            .fold(f64::INFINITY, f64::min);
        let max_x = corners
            .iter()
            .map(|point| point.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = corners
            .iter()
            .map(|point| point.y)
            .fold(f64::INFINITY, f64::min);
        let max_y = corners
            .iter()
            .map(|point| point.y)
            .fold(f64::NEG_INFINITY, f64::max);
        let width = max_x - min_x;
        let height = max_y - min_y;
        if width <= 0.0 || height <= 0.0 || width.min(height) > 2.0 {
            return;
        }
        if width >= height {
            self.borders.push(BorderElement {
                orientation: Orientation::Horizontal,
                x0: round2(min_x),
                y0: round2((min_y + max_y) / 2.0),
                x1: round2(max_x),
                y1: round2((min_y + max_y) / 2.0),
                width: round2(height),
            });
        } else {
            self.borders.push(BorderElement {
                orientation: Orientation::Vertical,
                x0: round2((min_x + max_x) / 2.0),
                y0: round2(min_y),
                x1: round2((min_x + max_x) / 2.0),
                y1: round2(max_y),
                width: round2(width),
            });
        }
    }
}

pub fn compare_page(ours: &[u8], truth: &[u8], page: u32) -> Result<ProbeReport> {
    let ours_content = extract_content_page(ours, page).context("extract our PDF content")?;
    let truth_content = extract_content_page(truth, page).context("extract truth PDF content")?;
    let mut ours = extract_page(ours, page).context("extract our PDF geometry")?;
    let mut truth = extract_page(truth, page).context("extract truth PDF geometry")?;
    canonicalize_page(&mut ours);
    canonicalize_page(&mut truth);
    let (ours_borders, truth_borders) = canonicalize_comparable_borders(
        std::mem::take(&mut ours.borders),
        std::mem::take(&mut truth.borders),
    );
    ours.borders = ours_borders;
    truth.borders = truth_borders;
    // PDF producers commonly round A4's fractional point dimensions to
    // nearby representations (Word exports 595.25pt for a 11907-twip
    // 595.35pt section). This tolerance is far below the probe's geometry
    // matching thresholds but avoids rejecting equivalent page boxes.
    const PAGE_BOX_TOLERANCE_PT: f64 = 0.15;
    if (ours.width - truth.width).abs() > PAGE_BOX_TOLERANCE_PT
        || (ours.height - truth.height).abs() > PAGE_BOX_TOLERANCE_PT
    {
        bail!(
            "page geometry differs: ours {}x{}, truth {}x{}",
            ours.width,
            ours.height,
            truth.width,
            truth.height
        );
    }

    let (mut matches, used_ours_borders, used_truth_borders) =
        pair_borders(&ours.borders, &truth.borders, ours.height);
    let (text_matches, used_ours_text, used_truth_text) =
        pair_text(&ours.text, &truth.text, ours.height);
    matches.extend(text_matches);
    matches.sort_by(|left, right| {
        left.class
            .cmp(&right.class)
            .then(left.y_band.cmp(&right.y_band))
            .then(left.ours_index.cmp(&right.ours_index))
    });

    let mut cluster_counts: BTreeMap<(String, u8, i64, i64), usize> = BTreeMap::new();
    for paired in &matches {
        *cluster_counts
            .entry((
                paired.class.clone(),
                paired.y_band,
                (paired.dx * 100.0).round() as i64,
                (paired.dy * 100.0).round() as i64,
            ))
            .or_default() += 1;
    }
    let mut clusters: Vec<DeltaCluster> = cluster_counts
        .into_iter()
        .map(|((class, y_band, dx, dy), count)| DeltaCluster {
            class,
            y_band,
            dx: dx as f64 / 100.0,
            dy: dy as f64 / 100.0,
            count,
        })
        .collect();
    clusters.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then(left.class.cmp(&right.class))
            .then(left.y_band.cmp(&right.y_band))
    });
    let ours_borders = unmatched_indices(ours.borders.len(), &used_ours_borders);
    let truth_borders = unmatched_indices(truth.borders.len(), &used_truth_borders);
    let ours_text = unmatched_indices(ours.text.len(), &used_ours_text);
    let truth_text = unmatched_indices(truth.text.len(), &used_truth_text);
    let elements = unmatched_element_records(
        &ours,
        &truth,
        &ours_borders,
        &truth_borders,
        &ours_text,
        &truth_text,
    );

    Ok(ProbeReport {
        page,
        page_width: ours.width,
        page_height: ours.height,
        ours_borders: ours.borders.len(),
        truth_borders: truth.borders.len(),
        ours_text: ours.text.len(),
        truth_text: truth.text.len(),
        ours_text_origins: ours.text.clone(),
        truth_text_origins: truth.text.clone(),
        matches,
        clusters,
        unmatched: UnmatchedReport {
            ours_borders,
            truth_borders,
            ours_text,
            truth_text,
            elements,
        },
        duplicates: duplicate_segments(&ours.borders),
        ours_content,
        truth_content,
    })
}

/// Reduces a detailed probe report to the blocking per-page geometry metrics.
pub fn geometry_metrics(report: &ProbeReport, limits: &GeometryLimits) -> GeometryPageMetrics {
    let borders = class_metrics(report, false);
    let text = class_metrics(report, true);
    let violation = class_violation(report.page, &borders, limits)
        .or_else(|| class_violation(report.page, &text, limits));
    GeometryPageMetrics {
        page: report.page,
        borders,
        text,
        pass: violation.is_none(),
        violation,
    }
}

fn class_metrics(report: &ProbeReport, text: bool) -> GeometryClassMetrics {
    let matches = report
        .matches
        .iter()
        .filter(|record| (record.class == "text") == text)
        .collect::<Vec<_>>();
    let (ours, truth, unmatched) = if text {
        (
            report.ours_text,
            report.truth_text,
            report.unmatched.ours_text.len() + report.unmatched.truth_text.len(),
        )
    } else {
        (
            report.ours_borders,
            report.truth_borders,
            report.unmatched.ours_borders.len() + report.unmatched.truth_borders.len(),
        )
    };
    let dx = matches.iter().map(|record| record.dx).collect::<Vec<_>>();
    let dy = matches.iter().map(|record| record.dy).collect::<Vec<_>>();
    let total = ours + truth;
    GeometryClassMetrics {
        element_class: if text { "text" } else { "borders" }.into(),
        ours,
        truth,
        matched: matches.len(),
        median_dx: round2(median(&dx)),
        median_dy: round2(median(&dy)),
        p95_dx: round2(percentile95_abs(&dx)),
        p95_dy: round2(percentile95_abs(&dy)),
        unmatched_pct: if total == 0 {
            0.0
        } else {
            round2(unmatched as f64 / total as f64 * 100.0)
        },
    }
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn percentile95_abs(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut values = values.iter().map(|value| value.abs()).collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    let rank = ((values.len() as f64 * 0.95).ceil() as usize).clamp(1, values.len());
    values[rank - 1]
}

fn class_violation(
    page: u32,
    metrics: &GeometryClassMetrics,
    limits: &GeometryLimits,
) -> Option<String> {
    for (label, value) in [
        ("median dx", metrics.median_dx),
        ("median dy", metrics.median_dy),
    ] {
        if value.abs() > limits.median_pt {
            return Some(format!(
                "page {page} {} {label} {value:+.2}pt",
                metrics.element_class
            ));
        }
    }
    for (label, value) in [("p95 dx", metrics.p95_dx), ("p95 dy", metrics.p95_dy)] {
        if value > limits.p95_pt {
            return Some(format!(
                "page {page} {} {label} {value:.2}pt",
                metrics.element_class
            ));
        }
    }
    (metrics.unmatched_pct > limits.max_unmatched_pct).then(|| {
        format!(
            "page {page} {} unmatched {:.2}%",
            metrics.element_class, metrics.unmatched_pct
        )
    })
}

pub fn print_report(report: &ProbeReport) {
    println!(
        "page {} ({:.2}×{:.2} pt): borders ours/truth {}/{}, text {}/{}",
        report.page,
        report.page_width,
        report.page_height,
        report.ours_borders,
        report.truth_borders,
        report.ours_text,
        report.truth_text
    );
    println!("class       band     dx     dy  count");
    for cluster in report.clusters.iter().take(40) {
        println!(
            "{:<11} {:>4} {:+6.2} {:+6.2} {:>6}",
            cluster.class, cluster.y_band, cluster.dx, cluster.dy, cluster.count
        );
    }
    println!(
        "unmatched: borders ours/truth {}/{}, text {}/{}; duplicate segments: {}",
        report.unmatched.ours_borders.len(),
        report.unmatched.truth_borders.len(),
        report.unmatched.ours_text.len(),
        report.unmatched.truth_text.len(),
        report.duplicates.len()
    );
    if !report.unmatched.elements.is_empty() {
        println!("unmatched element details (first 40):");
        println!("source class      idx       x0       y0       x1       y1 metric char");
        for element in report.unmatched.elements.iter().take(40) {
            let metric = element.width.or(element.font_size).unwrap_or(0.0);
            println!(
                "{:<6} {:<10} {:>4} {:>8.2} {:>8.2} {:>8} {:>8} {:>6.2} {}",
                element.source,
                element.class,
                element.index,
                element.x0,
                element.y0,
                element
                    .x1
                    .map_or_else(|| "-".into(), |value| format!("{value:.2}")),
                element
                    .y1
                    .map_or_else(|| "-".into(), |value| format!("{value:.2}")),
                metric,
                element.first_character.as_deref().unwrap_or("-"),
            );
        }
        if report.unmatched.elements.len() > 40 {
            println!(
                "... {} more unmatched elements",
                report.unmatched.elements.len() - 40
            );
        }
    }
}

fn extract_page(bytes: &[u8], page_number: u32) -> Result<ExtractedPage> {
    if page_number == 0 {
        bail!("probe page is 1-based and must be at least 1");
    }
    let document = Document::load_mem(bytes).context("parse PDF")?;
    let page_id = document
        .get_pages()
        .get(&page_number)
        .copied()
        .ok_or_else(|| anyhow!("PDF has no page {page_number}"))?;
    let media_box = inherited_array(&document, page_id, b"MediaBox")?;
    if media_box.len() != 4 {
        bail!("page {page_number} MediaBox must have four numbers");
    }
    let x0 = number(&media_box[0]).ok_or_else(|| anyhow!("invalid MediaBox x0"))?;
    let y0 = number(&media_box[1]).ok_or_else(|| anyhow!("invalid MediaBox y0"))?;
    let x1 = number(&media_box[2]).ok_or_else(|| anyhow!("invalid MediaBox x1"))?;
    let y1 = number(&media_box[3]).ok_or_else(|| anyhow!("invalid MediaBox y1"))?;
    let resources = inherited_dict(&document, page_id, b"Resources")?;
    let data = document
        .get_page_content(page_id)
        .context("read page content streams")?;
    let content = Content::decode(&data).context("decode page content")?;
    let mut extracted = ExtractedPage {
        width: round2((x1 - x0).abs()),
        height: round2((y1 - y0).abs()),
        media_x0: x0.min(x1),
        media_y1: y0.max(y1),
        borders: Vec::new(),
        text: Vec::new(),
    };
    interpret(
        &document,
        &content.operations,
        resources,
        GraphicsState::default(),
        0,
        &mut extracted,
    )?;
    Ok(extracted)
}

#[derive(Clone, Default)]
struct ContentState {
    graphics: GraphicsState,
    clip: Option<PdfRectangle>,
    path_bounds: Option<PdfRectangle>,
    pending_clip: bool,
}

pub(crate) struct ExtractedImageXObject {
    pub record: ImageXObjectRecord,
    pub bytes: Vec<u8>,
}

/// Extracts exact text-show and image-XObject records from one PDF page.
pub fn extract_content_page(bytes: &[u8], page_number: u32) -> Result<PdfPageContent> {
    let (content, _) = extract_content_page_with_bytes(bytes, page_number)?;
    Ok(content)
}

pub(crate) fn extract_content_page_with_bytes(
    bytes: &[u8],
    page_number: u32,
) -> Result<(PdfPageContent, Vec<ExtractedImageXObject>)> {
    if page_number == 0 {
        bail!("probe page is 1-based and must be at least 1");
    }
    let document = Document::load_mem(bytes).context("parse PDF")?;
    extract_content_page_from_document(&document, page_number)
}

pub(crate) fn extract_content_page_from_document(
    document: &Document,
    page_number: u32,
) -> Result<(PdfPageContent, Vec<ExtractedImageXObject>)> {
    if page_number == 0 {
        bail!("probe page is 1-based and must be at least 1");
    }
    let page_id = document
        .get_pages()
        .get(&page_number)
        .copied()
        .ok_or_else(|| anyhow!("PDF has no page {page_number}"))?;
    let resources = inherited_dict(document, page_id, b"Resources")?;
    let data = document
        .get_page_content(page_id)
        .context("read page content streams")?;
    let content = Content::decode(&data).context("decode page content")?;
    let mut text = Vec::new();
    let mut images = Vec::new();
    let mut operator_index = 0usize;
    record_operations(
        document,
        &content.operations,
        resources,
        ContentState::default(),
        0,
        &mut operator_index,
        &mut text,
        &mut images,
    )?;
    let records = images.iter().map(|image| image.record.clone()).collect();
    Ok((
        PdfPageContent {
            page: page_number,
            text,
            images: records,
        },
        images,
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "PDF interpreter state is explicit at the recursion boundary"
)]
fn record_operations(
    document: &Document,
    operations: &[Operation],
    resources: Option<&Dictionary>,
    mut state: ContentState,
    form_depth: u8,
    operator_index: &mut usize,
    text: &mut Vec<TextShowRecord>,
    images: &mut Vec<ExtractedImageXObject>,
) -> Result<()> {
    if form_depth > 16 {
        bail!("PDF form nesting exceeds diagnostic limit");
    }
    let mut stack = Vec::new();
    for operation in operations {
        let index = *operator_index;
        *operator_index += 1;
        match operation.operator.as_str() {
            "q" => stack.push(state.clone()),
            "Q" => {
                if let Some(saved) = stack.pop() {
                    state = saved;
                }
            }
            "cm" => {
                if let Some(matrix) = Affine::from_operands(&operation.operands) {
                    state.graphics.ctm = state.graphics.ctm.concat(matrix);
                }
            }
            "m" | "l" => {
                if let Some(point) = transformed_point(&state.graphics, &operation.operands) {
                    include_path_point(&mut state.path_bounds, point);
                }
            }
            "c" => {
                for pair in operation.operands.chunks_exact(2) {
                    if let (Some(x), Some(y)) = (number(&pair[0]), number(&pair[1])) {
                        include_path_point(
                            &mut state.path_bounds,
                            state.graphics.ctm.transform(Point { x, y }),
                        );
                    }
                }
            }
            "v" | "y" => {
                for pair in operation.operands.chunks_exact(2) {
                    if let (Some(x), Some(y)) = (number(&pair[0]), number(&pair[1])) {
                        include_path_point(
                            &mut state.path_bounds,
                            state.graphics.ctm.transform(Point { x, y }),
                        );
                    }
                }
            }
            "re" => include_rectangle_path(&mut state, operation),
            "W" | "W*" => state.pending_clip = true,
            "n" | "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                apply_pending_clip(&mut state);
                state.path_bounds = None;
            }
            "BT" => {
                state.graphics.text_matrix = Affine::default();
                state.graphics.text_line_matrix = Affine::default();
            }
            "Tf" => {
                state.graphics.font_name = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    .map(ToOwned::to_owned);
                if let Some(size) = operation.operands.get(1).and_then(number) {
                    state.graphics.font_size = size.abs();
                }
            }
            "TL" => {
                if let Some(leading) = operation.operands.first().and_then(number) {
                    state.graphics.text_leading = leading;
                }
            }
            "Tc" => {
                if let Some(spacing) = operation.operands.first().and_then(number) {
                    state.graphics.character_spacing = spacing;
                }
            }
            "Tw" => {
                if let Some(spacing) = operation.operands.first().and_then(number) {
                    state.graphics.word_spacing = spacing;
                }
            }
            "Tz" => {
                if let Some(scale) = operation.operands.first().and_then(number) {
                    state.graphics.horizontal_scale = scale / 100.0;
                }
            }
            "Tm" => {
                if let Some(matrix) = Affine::from_operands(&operation.operands) {
                    state.graphics.text_matrix = matrix;
                    state.graphics.text_line_matrix = matrix;
                }
            }
            "Td" | "TD" => apply_text_position(operation, &mut state.graphics),
            "T*" => next_text_line(&mut state.graphics),
            "Tj" | "TJ" => record_text_show(
                document,
                resources,
                operation,
                index,
                &mut state.graphics,
                text,
            ),
            "'" => {
                next_text_line(&mut state.graphics);
                record_text_show(
                    document,
                    resources,
                    operation,
                    index,
                    &mut state.graphics,
                    text,
                );
            }
            "\"" => {
                if let Some(spacing) = operation.operands.first().and_then(number) {
                    state.graphics.word_spacing = spacing;
                }
                if let Some(spacing) = operation.operands.get(1).and_then(number) {
                    state.graphics.character_spacing = spacing;
                }
                next_text_line(&mut state.graphics);
                record_text_show(
                    document,
                    resources,
                    operation,
                    index,
                    &mut state.graphics,
                    text,
                );
            }
            "Do" => record_xobject(
                document,
                resources,
                operation,
                state.clone(),
                form_depth,
                operator_index,
                text,
                images,
            )?,
            _ => {}
        }
    }
    Ok(())
}

fn include_rectangle_path(state: &mut ContentState, operation: &Operation) {
    if operation.operands.len() < 4 {
        return;
    }
    let (Some(x), Some(y), Some(width), Some(height)) = (
        number(&operation.operands[0]),
        number(&operation.operands[1]),
        number(&operation.operands[2]),
        number(&operation.operands[3]),
    ) else {
        return;
    };
    for point in [
        Point { x, y },
        Point { x: x + width, y },
        Point {
            x: x + width,
            y: y + height,
        },
        Point { x, y: y + height },
    ] {
        include_path_point(&mut state.path_bounds, state.graphics.ctm.transform(point));
    }
}

fn include_path_point(bounds: &mut Option<PdfRectangle>, point: Point) {
    match bounds {
        Some(bounds) => {
            bounds.x0 = bounds.x0.min(point.x);
            bounds.y0 = bounds.y0.min(point.y);
            bounds.x1 = bounds.x1.max(point.x);
            bounds.y1 = bounds.y1.max(point.y);
        }
        None => {
            *bounds = Some(PdfRectangle {
                x0: point.x,
                y0: point.y,
                x1: point.x,
                y1: point.y,
            });
        }
    }
}

fn apply_pending_clip(state: &mut ContentState) {
    if !state.pending_clip {
        return;
    }
    state.clip = match (state.clip, state.path_bounds) {
        (Some(first), Some(second)) => intersect_rectangles(first, second),
        (None, bounds) => bounds,
        (clip, None) => clip,
    };
    state.pending_clip = false;
}

fn intersect_rectangles(first: PdfRectangle, second: PdfRectangle) -> Option<PdfRectangle> {
    let rectangle = PdfRectangle {
        x0: first.x0.max(second.x0),
        y0: first.y0.max(second.y0),
        x1: first.x1.min(second.x1),
        y1: first.y1.min(second.y1),
    };
    (rectangle.x0 <= rectangle.x1 && rectangle.y0 <= rectangle.y1).then_some(rectangle)
}

fn apply_text_position(operation: &Operation, state: &mut GraphicsState) {
    let (Some(tx), Some(ty)) = (
        operation.operands.first().and_then(number),
        operation.operands.get(1).and_then(number),
    ) else {
        return;
    };
    if operation.operator == "TD" {
        state.text_leading = -ty;
    }
    state.text_line_matrix = state.text_line_matrix.concat(Affine::translation(tx, ty));
    state.text_matrix = state.text_line_matrix;
}

fn record_text_show(
    document: &Document,
    resources: Option<&Dictionary>,
    operation: &Operation,
    operator_index: usize,
    state: &mut GraphicsState,
    records: &mut Vec<TextShowRecord>,
) {
    let Some(font) = selected_font(document, resources, state.font_name.as_deref()) else {
        return;
    };
    let type0 = font.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Type0");
    let font_details = pdf_font_details(document, font);
    let tm = state.text_matrix;
    let device_origin = state.ctm.transform(tm.transform(Point::default()));
    let mut glyphs = Vec::new();
    let mut unicode = String::new();
    let mut encoded = Vec::new();
    let mut adjustments = Vec::new();
    let mut advance_pt = 0.0;

    match operation.operator.as_str() {
        "Tj" | "'" => {
            if let Some(bytes) = operation
                .operands
                .last()
                .and_then(|value| value.as_str().ok())
            {
                append_text_bytes_record(
                    document,
                    font,
                    type0,
                    bytes,
                    state,
                    &mut unicode,
                    &mut encoded,
                    &mut glyphs,
                    &mut advance_pt,
                );
            }
        }
        "\"" => {
            if let Some(bytes) = operation
                .operands
                .get(2)
                .and_then(|value| value.as_str().ok())
            {
                append_text_bytes_record(
                    document,
                    font,
                    type0,
                    bytes,
                    state,
                    &mut unicode,
                    &mut encoded,
                    &mut glyphs,
                    &mut advance_pt,
                );
            }
        }
        "TJ" => {
            if let Some(items) = operation
                .operands
                .first()
                .and_then(|value| value.as_array().ok())
            {
                for item in items {
                    if let Ok(bytes) = item.as_str() {
                        append_text_bytes_record(
                            document,
                            font,
                            type0,
                            bytes,
                            state,
                            &mut unicode,
                            &mut encoded,
                            &mut glyphs,
                            &mut advance_pt,
                        );
                    } else if let Some(adjustment) = number(item) {
                        adjustments.push(adjustment);
                        if let Some(glyph) = glyphs.last_mut() {
                            glyph.following_tj += adjustment;
                            glyph.resulting_advance_text_units -= adjustment;
                        }
                        let delta =
                            -adjustment / 1_000.0 * state.font_size * state.horizontal_scale;
                        advance_pt += delta;
                        advance_text_matrix(state, delta);
                    }
                }
            }
        }
        _ => return,
    }
    let resulting_advance_text_units = glyphs
        .iter()
        .map(|glyph| glyph.resulting_advance_text_units)
        .sum();
    records.push(TextShowRecord {
        operator_index,
        operator: operation.operator.clone(),
        resource_name: state
            .font_name
            .as_deref()
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .unwrap_or_default(),
        font_base_name: font_details.base_name,
        font_family: font_details.family,
        font_style: font_details.style,
        font_subtype: font_details.subtype,
        descendant_subtype: font_details.descendant_subtype,
        font_encoding: font_details.encoding,
        tf_pt: state.font_size,
        tm: tm.as_array(),
        ctm: state.ctm.as_array(),
        origin: [device_origin.x, device_origin.y],
        unicode,
        encoded_hex: hex_bytes(&encoded),
        glyphs,
        tj_adjustments: adjustments,
        resulting_advance_text_units,
        resulting_advance_pt: advance_pt,
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "the helper mutates one in-progress PDF text record"
)]
fn append_text_bytes_record(
    document: &Document,
    font: &Dictionary,
    type0: bool,
    bytes: &[u8],
    state: &mut GraphicsState,
    unicode: &mut String,
    encoded: &mut Vec<u8>,
    glyphs: &mut Vec<PdfGlyphRecord>,
    advance_pt: &mut f64,
) {
    encoded.extend_from_slice(bytes);
    for (code, code_bytes) in encoded_codes(type0, bytes) {
        let decoded = decode_text(document, font, code_bytes).unwrap_or_default();
        unicode.push_str(&decoded);
        let nominal_width = if type0 {
            type0_width(document, font, code)
        } else {
            simple_font_width(document, font, code)
        };
        let spacing_pt = state.character_spacing
            + if !type0 && code == 0x20 {
                state.word_spacing
            } else {
                0.0
            };
        let scale = state.font_size * state.horizontal_scale;
        let spacing_units = if scale.abs() > f64::EPSILON {
            spacing_pt * state.horizontal_scale / scale * 1_000.0
        } else {
            0.0
        };
        let glyph_advance_units = nominal_width + spacing_units;
        let glyph_advance_pt = glyph_advance_units / 1_000.0 * scale;
        *advance_pt += glyph_advance_pt;
        advance_text_matrix(state, glyph_advance_pt);
        glyphs.push(PdfGlyphRecord {
            source_code: code,
            source_cid: type0.then_some(code),
            source_gid: source_gid(document, font, type0, code, &decoded),
            unicode: decoded,
            nominal_width_text_units: nominal_width,
            following_tj: 0.0,
            resulting_advance_text_units: glyph_advance_units,
        });
    }
}

struct PdfFontDetails {
    base_name: Option<String>,
    family: Option<String>,
    style: Option<String>,
    subtype: Option<String>,
    descendant_subtype: Option<String>,
    encoding: Option<String>,
}

fn pdf_font_details(document: &Document, font: &Dictionary) -> PdfFontDetails {
    let base_name = font
        .get(b"BaseFont")
        .and_then(Object::as_name)
        .ok()
        .map(|name| String::from_utf8_lossy(name).into_owned());
    let normalized = base_name.as_deref().map(strip_pdf_subset_prefix);
    let (family, style) = normalized.map(split_font_family_style).unwrap_or_default();
    let descendant = font
        .get(b"DescendantFonts")
        .ok()
        .and_then(|object| resolve_object(document, object).ok())
        .and_then(|object| object.as_array().ok())
        .and_then(|fonts| fonts.first())
        .and_then(|object| resolve_object(document, object).ok())
        .and_then(|object| object.as_dict().ok());
    PdfFontDetails {
        base_name,
        family,
        style,
        subtype: dictionary_name(font, b"Subtype"),
        descendant_subtype: descendant.and_then(|font| dictionary_name(font, b"Subtype")),
        encoding: font.get(b"Encoding").ok().map(pdf_object_label),
    }
}

fn strip_pdf_subset_prefix(name: &str) -> &str {
    if name.as_bytes().get(6) == Some(&b'+')
        && name.as_bytes()[..6].iter().all(u8::is_ascii_uppercase)
    {
        &name[7..]
    } else {
        name
    }
}

fn split_font_family_style(name: &str) -> (Option<String>, Option<String>) {
    let normalized = name.replace(',', "-");
    for suffix in [
        "-BoldItalic",
        "-BoldOblique",
        "-Italic",
        "-Oblique",
        "-Bold",
    ] {
        if let Some(family) = normalized.strip_suffix(suffix) {
            return (
                Some(family.to_string()),
                Some(suffix.trim_start_matches('-').to_string()),
            );
        }
    }
    (Some(normalized), Some("Regular".to_string()))
}

fn dictionary_name(dictionary: &Dictionary, key: &[u8]) -> Option<String> {
    dictionary
        .get(key)
        .and_then(Object::as_name)
        .ok()
        .map(|name| String::from_utf8_lossy(name).into_owned())
}

fn encoded_codes(type0: bool, bytes: &[u8]) -> Vec<(u32, &[u8])> {
    if type0 {
        bytes
            .chunks(2)
            .map(|chunk| {
                let code = if chunk.len() == 2 {
                    u16::from_be_bytes([chunk[0], chunk[1]]) as u32
                } else {
                    u32::from(chunk[0])
                };
                (code, chunk)
            })
            .collect()
    } else {
        bytes
            .iter()
            .enumerate()
            .map(|(index, byte)| (u32::from(*byte), &bytes[index..index + 1]))
            .collect()
    }
}

fn decode_text(document: &Document, font: &Dictionary, bytes: &[u8]) -> Option<String> {
    let mut to_unicode_font = font.clone();
    let decoding_font = if to_unicode_font.get(b"ToUnicode").is_ok() {
        to_unicode_font.remove(b"Encoding");
        &to_unicode_font
    } else {
        font
    };
    decoding_font
        .get_font_encoding(document)
        .ok()?
        .bytes_to_string(bytes)
        .ok()
}

fn source_gid(
    document: &Document,
    font: &Dictionary,
    type0: bool,
    code: u32,
    unicode: &str,
) -> Option<u16> {
    if type0 {
        let descendant = font
            .get(b"DescendantFonts")
            .ok()
            .and_then(|object| resolve_object(document, object).ok())
            .and_then(|object| object.as_array().ok())
            .and_then(|fonts| fonts.first())
            .and_then(|object| resolve_object(document, object).ok())
            .and_then(|object| object.as_dict().ok())?;
        return match descendant.get(b"CIDToGIDMap") {
            Ok(Object::Name(name)) if name == b"Identity" => u16::try_from(code).ok(),
            Ok(object) => resolve_object(document, object)
                .ok()
                .and_then(|object| object.as_stream().ok())
                .and_then(|stream| {
                    let offset = usize::try_from(code).ok()?.checked_mul(2)?;
                    let bytes = stream.content.get(offset..offset + 2)?;
                    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
                }),
            Err(_) => u16::try_from(code).ok(),
        };
    }
    if dictionary_name(font, b"Subtype").as_deref() != Some("TrueType") {
        return None;
    }
    let descriptor = font
        .get(b"FontDescriptor")
        .ok()
        .and_then(|object| resolve_object(document, object).ok())
        .and_then(|object| object.as_dict().ok())?;
    let stream = descriptor
        .get(b"FontFile2")
        .ok()
        .and_then(|object| resolve_object(document, object).ok())
        .and_then(|object| object.as_stream().ok())?;
    let bytes = stream
        .decompressed_content()
        .unwrap_or_else(|_| stream.content.clone());
    let face = ttf_parser::Face::parse(&bytes, 0).ok()?;
    face.glyph_index(unicode.chars().next()?).map(|gid| gid.0)
}

#[expect(
    clippy::too_many_arguments,
    reason = "recursive XObject interpretation mirrors the PDF graphics stack"
)]
fn record_xobject(
    document: &Document,
    resources: Option<&Dictionary>,
    operation: &Operation,
    state: ContentState,
    form_depth: u8,
    operator_index: &mut usize,
    text: &mut Vec<TextShowRecord>,
    images: &mut Vec<ExtractedImageXObject>,
) -> Result<()> {
    let Some(name) = operation
        .operands
        .first()
        .and_then(|operand| operand.as_name().ok())
    else {
        return Ok(());
    };
    let Some(resources) = resources else {
        return Ok(());
    };
    let Ok(xobjects_object) = resources.get(b"XObject") else {
        return Ok(());
    };
    let xobjects = resolve_object(document, xobjects_object)
        .context("resolve XObject resources")?
        .as_dict()
        .context("XObject resources are not a dictionary")?;
    let Ok(object) = xobjects.get(name) else {
        return Ok(());
    };
    let object_id = object.as_reference().ok();
    let stream = resolve_object(document, object)
        .context("resolve XObject")?
        .as_stream()
        .context("XObject is not a stream")?;
    match stream.dict.get(b"Subtype").and_then(Object::as_name).ok() {
        Some(b"Image") => {
            let bytes = stream.content.clone();
            images.push(ExtractedImageXObject {
                record: image_record(document, resources, name, object_id, stream, &bytes, &state),
                bytes,
            });
        }
        Some(b"Form") => {
            let mut form_state = state;
            if let Ok(matrix) = stream.dict.get(b"Matrix")
                && let Ok(array) = matrix.as_array()
                && let Some(matrix) = Affine::from_operands(array)
            {
                form_state.graphics.ctm = form_state.graphics.ctm.concat(matrix);
            }
            if let Ok(bbox) = stream.dict.get(b"BBox")
                && let Ok(values) = bbox.as_array()
                && values.len() == 4
                && let (Some(x0), Some(y0), Some(x1), Some(y1)) = (
                    number(&values[0]),
                    number(&values[1]),
                    number(&values[2]),
                    number(&values[3]),
                )
            {
                let first = form_state.graphics.ctm.transform(Point { x: x0, y: y0 });
                let second = form_state.graphics.ctm.transform(Point { x: x1, y: y1 });
                let bbox = PdfRectangle {
                    x0: first.x.min(second.x),
                    y0: first.y.min(second.y),
                    x1: first.x.max(second.x),
                    y1: first.y.max(second.y),
                };
                form_state.clip = match form_state.clip {
                    Some(clip) => intersect_rectangles(clip, bbox),
                    None => Some(bbox),
                };
            }
            let form_resources = stream
                .dict
                .get(b"Resources")
                .ok()
                .and_then(|object| resolve_object(document, object).ok())
                .and_then(|object| object.as_dict().ok())
                .or(Some(resources));
            let data = stream
                .decompressed_content()
                .context("decompress form XObject")?;
            let content = Content::decode(&data).context("decode form XObject content")?;
            record_operations(
                document,
                &content.operations,
                form_resources,
                form_state,
                form_depth + 1,
                operator_index,
                text,
                images,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn image_record(
    document: &Document,
    resources: &Dictionary,
    name: &[u8],
    object_id: Option<ObjectId>,
    stream: &Stream,
    bytes: &[u8],
    state: &ContentState,
) -> ImageXObjectRecord {
    ImageXObjectRecord {
        resource_name: String::from_utf8_lossy(name).into_owned(),
        object_id: object_id.map(|(number, generation)| [number, u32::from(generation)]),
        encoded_sha256: sha256_hex(bytes),
        encoded_length: bytes.len(),
        width_px: dictionary_u32(&stream.dict, b"Width"),
        height_px: dictionary_u32(&stream.dict, b"Height"),
        filters: filter_names(&stream.dict),
        color_space: stream
            .dict
            .get(b"ColorSpace")
            .ok()
            .map(|object| resolved_color_space_label(document, resources, object)),
        decode_parameters: stream.dict.get(b"DecodeParms").ok().map(pdf_object_label),
        placement_matrix: state.graphics.ctm.as_array(),
        clip_rectangle: state.clip,
        jpeg: parse_jpeg_metadata(bytes),
    }
}

fn dictionary_u32(dictionary: &Dictionary, key: &[u8]) -> Option<u32> {
    dictionary
        .get(key)
        .and_then(Object::as_i64)
        .ok()
        .and_then(|value| u32::try_from(value).ok())
}

fn filter_names(dictionary: &Dictionary) -> Vec<String> {
    let Ok(filter) = dictionary.get(b"Filter") else {
        return Vec::new();
    };
    if let Ok(name) = filter.as_name() {
        return vec![String::from_utf8_lossy(name).into_owned()];
    }
    filter
        .as_array()
        .map(|filters| filters.iter().map(pdf_object_label).collect::<Vec<_>>())
        .unwrap_or_default()
}

fn resolved_color_space_label(
    document: &Document,
    resources: &Dictionary,
    object: &Object,
) -> String {
    if let Ok(name) = object.as_name()
        && let Ok(color_spaces) = resources.get(b"ColorSpace")
        && let Ok(color_spaces) = resolve_object(document, color_spaces)
        && let Ok(color_spaces) = color_spaces.as_dict()
        && let Ok(color_space) = color_spaces.get(name)
    {
        return pdf_object_label(color_space);
    }
    pdf_object_label(object)
}

fn pdf_object_label(object: &Object) -> String {
    match object {
        Object::Null => "null".to_string(),
        Object::Boolean(value) => value.to_string(),
        Object::Integer(value) => value.to_string(),
        Object::Real(value) => value.to_string(),
        Object::Name(value) => format!("/{}", String::from_utf8_lossy(value)),
        Object::String(value, _) => format!("<{}>", hex_bytes(value)),
        Object::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(pdf_object_label)
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Object::Dictionary(dictionary) => {
            let mut entries = dictionary
                .iter()
                .map(|(key, value)| {
                    format!(
                        "/{} {}",
                        String::from_utf8_lossy(key),
                        pdf_object_label(value)
                    )
                })
                .collect::<Vec<_>>();
            entries.sort();
            format!("<<{}>>", entries.join(" "))
        }
        Object::Stream(stream) => format!("stream({})", stream.content.len()),
        Object::Reference((number, generation)) => format!("{number} {generation} R"),
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn parse_jpeg_metadata(bytes: &[u8]) -> Option<JpegMetadata> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut offset = 2usize;
    let mut frame = None;
    let mut tables = Vec::new();
    while offset + 1 < bytes.len() {
        while offset < bytes.len() && bytes[offset] != 0xff {
            offset += 1;
        }
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        let marker = *bytes.get(offset)?;
        offset += 1;
        if matches!(marker, 0xd8 | 0xd9) {
            continue;
        }
        if marker == 0xda {
            break;
        }
        let length = usize::from(u16::from_be_bytes([
            *bytes.get(offset)?,
            *bytes.get(offset + 1)?,
        ]));
        if length < 2 || offset + length > bytes.len() {
            return None;
        }
        let segment = &bytes[offset + 2..offset + length];
        match marker {
            0xdb => parse_dqt_segment(segment, &mut tables)?,
            0xc0..=0xc2 => frame = parse_sof_segment(marker, segment),
            _ => {}
        }
        offset += length;
    }
    let (frame_marker, precision_bits, components) = frame?;
    Some(JpegMetadata {
        frame_marker,
        precision_bits,
        components,
        quantization_tables: tables,
    })
}

fn parse_dqt_segment(segment: &[u8], tables: &mut Vec<JpegQuantizationTable>) -> Option<()> {
    let mut offset = 0usize;
    while offset < segment.len() {
        let selector = *segment.get(offset)?;
        offset += 1;
        let precision_bits = if selector >> 4 == 0 { 8 } else { 16 };
        let bytes_per_value = usize::from(precision_bits / 8);
        let byte_len = 64usize.checked_mul(bytes_per_value)?;
        let values_bytes = segment.get(offset..offset + byte_len)?;
        let values = if precision_bits == 8 {
            values_bytes.iter().map(|value| u16::from(*value)).collect()
        } else {
            values_bytes
                .chunks_exact(2)
                .map(|value| u16::from_be_bytes([value[0], value[1]]))
                .collect()
        };
        tables.push(JpegQuantizationTable {
            table_id: selector & 0x0f,
            precision_bits,
            values,
        });
        offset += byte_len;
    }
    Some(())
}

fn parse_sof_segment(marker: u8, segment: &[u8]) -> Option<(String, u8, Vec<JpegComponent>)> {
    let precision_bits = *segment.first()?;
    let count = usize::from(*segment.get(5)?);
    let mut components = Vec::with_capacity(count);
    for component in segment.get(6..)?.chunks_exact(3).take(count) {
        components.push(JpegComponent {
            component_id: component[0],
            horizontal_sampling: component[1] >> 4,
            vertical_sampling: component[1] & 0x0f,
            quantization_table: component[2],
        });
    }
    (components.len() == count).then(|| {
        (
            match marker {
                0xc0 => "sof0_baseline",
                0xc1 => "sof1_extended_sequential",
                0xc2 => "sof2_progressive",
                _ => "sof_unknown",
            }
            .to_string(),
            precision_bits,
            components,
        )
    })
}

fn canonicalize_page(page: &mut ExtractedPage) {
    page.text = canonicalize_text_runs(std::mem::take(&mut page.text));
}

/// Keeps only border geometry both PDF producers expose comparably. Word can
/// encode one side of an L-shaped table-cell decoration as a stroked path and
/// the other side as a glyph/clip, while pdf-writer emits both as paths. A
/// segment is comparable when it has a same-orientation shadow in the other
/// PDF and at least one side exposes it as part of an orthogonal structure.
fn canonicalize_comparable_borders(
    ours: Vec<BorderElement>,
    truth: Vec<BorderElement>,
) -> (Vec<BorderElement>, Vec<BorderElement>) {
    let ours = canonicalize_borders(ours);
    let truth = canonicalize_borders(truth);
    let ours_structural = structural_border_flags(&ours);
    let truth_structural = structural_border_flags(&truth);

    let keep_ours = ours
        .iter()
        .enumerate()
        .map(|(index, border)| {
            comparable_border_is_visible(border, ours_structural[index], &truth, &truth_structural)
        })
        .collect::<Vec<_>>();
    let keep_truth = truth
        .iter()
        .enumerate()
        .map(|(index, border)| {
            comparable_border_is_visible(border, truth_structural[index], &ours, &ours_structural)
        })
        .collect::<Vec<_>>();
    let ours = ours
        .into_iter()
        .zip(keep_ours)
        .filter(|(_, keep)| *keep)
        .map(|(border, _)| border)
        .collect();
    let truth = truth
        .into_iter()
        .zip(keep_truth)
        .filter(|(_, keep)| *keep)
        .map(|(border, _)| border)
        .collect();
    (ours, truth)
}

fn comparable_border_is_visible(
    border: &BorderElement,
    structurally_visible: bool,
    peers: &[BorderElement],
    peer_structural: &[bool],
) -> bool {
    let shadows = peers
        .iter()
        .enumerate()
        .filter(|(_, peer)| border_has_comparable_shadow(border, peer))
        .collect::<Vec<_>>();
    !shadows.is_empty()
        && (structurally_visible
            || shadows
                .iter()
                .any(|(peer_index, _)| peer_structural[*peer_index]))
}

fn border_has_comparable_shadow(first: &BorderElement, second: &BorderElement) -> bool {
    // Wide enough to retain deliberately displaced table edges as unmatched
    // failures, while requiring overlap on the segment's projected extent so
    // unrelated structures elsewhere on the page cannot make it comparable.
    const SHADOW_AXIS_TOLERANCE_PT: f64 = 12.0;
    const EXTENT_TOLERANCE_PT: f64 = 1.5;
    first.orientation == second.orientation
        && (border_axis(first) - border_axis(second)).abs() <= SHADOW_AXIS_TOLERANCE_PT
        && border_start(first) <= border_end(second) + EXTENT_TOLERANCE_PT
        && border_start(second) <= border_end(first) + EXTENT_TOLERANCE_PT
}

fn canonicalize_borders(mut borders: Vec<BorderElement>) -> Vec<BorderElement> {
    // Word and pdf-writer may quantize the same table edge to coordinates that
    // differ by a few hundredths of a point. Keep this below the 0.10pt
    // near-duplicate threshold so a genuinely doubled rule remains visible.
    const AXIS_TOLERANCE_PT: f64 = 0.09;
    const JOIN_TOLERANCE_PT: f64 = 3.0;
    borders.sort_by(|left, right| {
        left.orientation
            .cmp(&right.orientation)
            .then(border_axis(left).total_cmp(&border_axis(right)))
            .then(border_start(left).total_cmp(&border_start(right)))
            .then(border_end(left).total_cmp(&border_end(right)))
    });
    let mut canonical: Vec<BorderElement> = Vec::new();
    for border in borders {
        if let Some(previous) = canonical.last_mut()
            && previous.orientation == border.orientation
            && (border_axis(previous) - border_axis(&border)).abs() <= AXIS_TOLERANCE_PT
            && border_start(&border) <= border_end(previous) + JOIN_TOLERANCE_PT
        {
            let start = border_start(previous).min(border_start(&border));
            let end = border_end(previous).max(border_end(&border));
            previous.width = previous.width.max(border.width);
            match previous.orientation {
                Orientation::Horizontal => {
                    previous.x0 = start;
                    previous.x1 = end;
                }
                Orientation::Vertical => {
                    previous.y0 = start;
                    previous.y1 = end;
                }
            }
            continue;
        }
        canonical.push(border);
    }
    canonical
}

#[cfg(test)]
fn structural_borders(borders: Vec<BorderElement>) -> Vec<BorderElement> {
    let structural = structural_border_flags(&borders);
    borders
        .into_iter()
        .zip(structural)
        .filter(|(_, structural)| *structural)
        .map(|(border, _)| border)
        .collect()
}

fn structural_border_flags(borders: &[BorderElement]) -> Vec<bool> {
    const INTERSECTION_TOLERANCE_PT: f64 = 1.5;
    // Sub-point stubs are stroke-join/cap artifacts emitted by some PDF
    // producers, not independently meaningful table geometry.
    const MIN_STRUCTURAL_LENGTH_PT: f64 = 1.0;
    borders
        .iter()
        .enumerate()
        .map(|(index, border)| {
            border.length() >= MIN_STRUCTURAL_LENGTH_PT
                && borders.iter().enumerate().any(|(other_index, other)| {
                    index != other_index
                        && border.orientation != other.orientation
                        && borders_intersect(border, other, INTERSECTION_TOLERANCE_PT)
                })
        })
        .collect()
}

fn borders_intersect(first: &BorderElement, second: &BorderElement, tolerance: f64) -> bool {
    let (horizontal, vertical) = match first.orientation {
        Orientation::Horizontal => (first, second),
        Orientation::Vertical => (second, first),
    };
    let x = border_axis(vertical);
    let y = border_axis(horizontal);
    x + tolerance >= border_start(horizontal)
        && x <= border_end(horizontal) + tolerance
        && y + tolerance >= border_start(vertical)
        && y <= border_end(vertical) + tolerance
}

fn border_axis(border: &BorderElement) -> f64 {
    match border.orientation {
        Orientation::Horizontal => (border.y0 + border.y1) / 2.0,
        Orientation::Vertical => (border.x0 + border.x1) / 2.0,
    }
}

fn border_start(border: &BorderElement) -> f64 {
    match border.orientation {
        Orientation::Horizontal => border.x0.min(border.x1),
        Orientation::Vertical => border.y0.min(border.y1),
    }
}

fn border_end(border: &BorderElement) -> f64 {
    match border.orientation {
        Orientation::Horizontal => border.x0.max(border.x1),
        Orientation::Vertical => border.y0.max(border.y1),
    }
}

fn canonicalize_text_runs(text: Vec<TextOrigin>) -> Vec<TextOrigin> {
    const BASELINE_TOLERANCE_PT: f64 = 0.25;
    const BACKTRACK_TOLERANCE_PT: f64 = 0.25;
    let mut runs: Vec<TextOrigin> = Vec::new();
    for origin in text {
        if origin
            .first_character
            .as_deref()
            .is_some_and(|value| value.chars().all(char::is_whitespace))
        {
            continue;
        }
        if let Some(previous) = runs.last()
            && (previous.y - origin.y).abs() <= BASELINE_TOLERANCE_PT
            && origin.x + BACKTRACK_TOLERANCE_PT >= previous.x
        {
            continue;
        }
        runs.push(origin);
    }
    runs
}

fn interpret(
    document: &Document,
    operations: &[Operation],
    resources: Option<&Dictionary>,
    mut state: GraphicsState,
    form_depth: u8,
    extracted: &mut ExtractedPage,
) -> Result<()> {
    let mut stack = Vec::new();
    for operation in operations {
        match operation.operator.as_str() {
            "q" => stack.push(state.clone()),
            "Q" => {
                if let Some(saved) = stack.pop() {
                    state = saved;
                }
            }
            "cm" => {
                if let Some(matrix) = Affine::from_operands(&operation.operands) {
                    state.ctm = state.ctm.concat(matrix);
                }
            }
            "w" => {
                if let Some(width) = operation.operands.first().and_then(number) {
                    state.line_width = width.abs();
                }
            }
            "m" => {
                if let Some(point) = transformed_point(&state, &operation.operands) {
                    state.current_point = Some(point);
                    state.subpath_start = Some(point);
                }
            }
            "l" => {
                if let (Some(first), Some(second)) = (
                    state.current_point,
                    transformed_point(&state, &operation.operands),
                ) {
                    state.path.push(PathItem::Line(first, second));
                    state.current_point = Some(second);
                }
            }
            "h" => {
                close_path(&mut state);
            }
            "re" => {
                if operation.operands.len() >= 4
                    && let (Some(x), Some(y), Some(width), Some(height)) = (
                        number(&operation.operands[0]),
                        number(&operation.operands[1]),
                        number(&operation.operands[2]),
                        number(&operation.operands[3]),
                    )
                {
                    state.path.push(PathItem::Rect([
                        state.ctm.transform(Point { x, y }),
                        state.ctm.transform(Point { x: x + width, y }),
                        state.ctm.transform(Point {
                            x: x + width,
                            y: y + height,
                        }),
                        state.ctm.transform(Point { x, y: y + height }),
                    ]));
                }
            }
            "S" => paint_path(&mut state, true, false, extracted),
            "s" => {
                close_path(&mut state);
                paint_path(&mut state, true, false, extracted);
            }
            "f" | "F" | "f*" => paint_path(&mut state, false, true, extracted),
            "B" | "B*" => paint_path(&mut state, true, true, extracted),
            "b" | "b*" => {
                close_path(&mut state);
                paint_path(&mut state, true, true, extracted);
            }
            "n" => clear_path(&mut state),
            "BT" => {
                state.text_matrix = Affine::default();
                state.text_line_matrix = Affine::default();
            }
            "Tf" => {
                state.font_name = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    .map(ToOwned::to_owned);
                if let Some(size) = operation.operands.get(1).and_then(number) {
                    state.font_size = size.abs();
                }
            }
            "TL" => {
                if let Some(leading) = operation.operands.first().and_then(number) {
                    state.text_leading = leading;
                }
            }
            "Tc" => {
                if let Some(spacing) = operation.operands.first().and_then(number) {
                    state.character_spacing = spacing;
                }
            }
            "Tw" => {
                if let Some(spacing) = operation.operands.first().and_then(number) {
                    state.word_spacing = spacing;
                }
            }
            "Tz" => {
                if let Some(scale) = operation.operands.first().and_then(number) {
                    state.horizontal_scale = scale / 100.0;
                }
            }
            "Tm" => {
                if let Some(matrix) = Affine::from_operands(&operation.operands) {
                    state.text_matrix = matrix;
                    state.text_line_matrix = matrix;
                }
            }
            "Td" | "TD" => {
                if let (Some(tx), Some(ty)) = (
                    operation.operands.first().and_then(number),
                    operation.operands.get(1).and_then(number),
                ) {
                    if operation.operator == "TD" {
                        state.text_leading = -ty;
                    }
                    state.text_line_matrix =
                        state.text_line_matrix.concat(Affine::translation(tx, ty));
                    state.text_matrix = state.text_line_matrix;
                }
            }
            "T*" => next_text_line(&mut state),
            "Tj" | "TJ" => show_text(document, resources, operation, &mut state, extracted),
            "'" => {
                next_text_line(&mut state);
                show_text(document, resources, operation, &mut state, extracted);
            }
            "\"" => {
                if let Some(spacing) = operation.operands.first().and_then(number) {
                    state.word_spacing = spacing;
                }
                if let Some(spacing) = operation.operands.get(1).and_then(number) {
                    state.character_spacing = spacing;
                }
                next_text_line(&mut state);
                show_text(document, resources, operation, &mut state, extracted);
            }
            "Do" if form_depth == 0 => {
                interpret_form(document, resources, operation, &state, extracted)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn paint_path(state: &mut GraphicsState, stroke: bool, fill: bool, extracted: &mut ExtractedPage) {
    for item in &state.path {
        match item {
            PathItem::Line(first, second) if stroke => {
                let horizontal = (first.y - second.y).abs() <= (first.x - second.x).abs();
                let scale = if horizontal {
                    state.ctm.y_scale()
                } else {
                    state.ctm.x_scale()
                };
                extracted.push_line(*first, *second, state.line_width * scale);
            }
            PathItem::Rect(corners) if fill || stroke => extracted.push_thin_rect(*corners),
            _ => {}
        }
    }
    clear_path(state);
}

fn clear_path(state: &mut GraphicsState) {
    state.path.clear();
    state.current_point = None;
    state.subpath_start = None;
}

fn close_path(state: &mut GraphicsState) {
    if let (Some(first), Some(second)) = (state.current_point, state.subpath_start)
        && first.distance(second) > 0.001
    {
        state.path.push(PathItem::Line(first, second));
        state.current_point = Some(second);
    }
}

fn next_text_line(state: &mut GraphicsState) {
    state.text_line_matrix = state
        .text_line_matrix
        .concat(Affine::translation(0.0, -state.text_leading));
    state.text_matrix = state.text_line_matrix;
}

fn show_text(
    document: &Document,
    resources: Option<&Dictionary>,
    operation: &Operation,
    state: &mut GraphicsState,
    extracted: &mut ExtractedPage,
) {
    match operation.operator.as_str() {
        "Tj" | "'" => {
            if let Some(bytes) = operation
                .operands
                .last()
                .and_then(|value| value.as_str().ok())
            {
                show_text_bytes(document, resources, bytes, state, extracted);
            }
        }
        "\"" => {
            if let Some(bytes) = operation
                .operands
                .get(2)
                .and_then(|value| value.as_str().ok())
            {
                show_text_bytes(document, resources, bytes, state, extracted);
            }
        }
        "TJ" => {
            let Some(items) = operation
                .operands
                .first()
                .and_then(|value| value.as_array().ok())
            else {
                return;
            };
            for item in items {
                if let Ok(bytes) = item.as_str() {
                    show_text_bytes(document, resources, bytes, state, extracted);
                } else if let Some(adjustment) = number(item) {
                    advance_text_matrix(
                        state,
                        -adjustment / 1_000.0 * state.font_size * state.horizontal_scale,
                    );
                }
            }
        }
        _ => {}
    }
}

fn show_text_bytes(
    document: &Document,
    resources: Option<&Dictionary>,
    bytes: &[u8],
    state: &mut GraphicsState,
    extracted: &mut ExtractedPage,
) {
    if bytes.is_empty() {
        return;
    }
    let device = state
        .ctm
        .transform(state.text_matrix.transform(Point::default()));
    let point = extracted.normalize_point(device);
    extracted.text.push(TextOrigin {
        x: point.x,
        y: point.y,
        font_size: round2(state.font_size * state.ctm.y_scale()),
        first_character: decode_first_character(
            document,
            resources,
            state.font_name.as_deref(),
            bytes,
        ),
    });
    let advance = text_advance(document, resources, state, bytes);
    advance_text_matrix(state, advance);
}

fn advance_text_matrix(state: &mut GraphicsState, advance: f64) {
    state.text_matrix = state.text_matrix.concat(Affine::translation(advance, 0.0));
}

fn text_advance(
    document: &Document,
    resources: Option<&Dictionary>,
    state: &GraphicsState,
    bytes: &[u8],
) -> f64 {
    let font = selected_font(document, resources, state.font_name.as_deref());
    let type0 = font
        .is_some_and(|font| font.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Type0"));
    let codes = if type0 {
        bytes
            .chunks(2)
            .map(|chunk| {
                if chunk.len() == 2 {
                    u16::from_be_bytes([chunk[0], chunk[1]]) as u32
                } else {
                    u32::from(chunk[0])
                }
            })
            .collect::<Vec<_>>()
    } else {
        bytes.iter().copied().map(u32::from).collect::<Vec<_>>()
    };
    codes
        .into_iter()
        .map(|code| {
            let width = font.map_or(0.0, |font| {
                if type0 {
                    type0_width(document, font, code)
                } else {
                    simple_font_width(document, font, code)
                }
            });
            (width / 1_000.0 * state.font_size
                + state.character_spacing
                + if !type0 && code == 0x20 {
                    state.word_spacing
                } else {
                    0.0
                })
                * state.horizontal_scale
        })
        .sum()
}

fn selected_font<'a>(
    document: &'a Document,
    resources: Option<&'a Dictionary>,
    font_name: Option<&[u8]>,
) -> Option<&'a Dictionary> {
    let resources = resources?;
    let font_name = font_name?;
    let fonts = resolve_object(document, resources.get(b"Font").ok()?)
        .ok()?
        .as_dict()
        .ok()?;
    resolve_object(document, fonts.get(font_name).ok()?)
        .ok()?
        .as_dict()
        .ok()
}

fn simple_font_width(document: &Document, font: &Dictionary, code: u32) -> f64 {
    let first = font
        .get(b"FirstChar")
        .and_then(Object::as_i64)
        .ok()
        .and_then(|value| u32::try_from(value).ok());
    let widths = font
        .get(b"Widths")
        .ok()
        .and_then(|value| resolve_object(document, value).ok())
        .and_then(|value| value.as_array().ok());
    if let (Some(first), Some(widths)) = (first, widths)
        && let Some(offset) = code
            .checked_sub(first)
            .and_then(|value| usize::try_from(value).ok())
        && let Some(width) = widths.get(offset).and_then(number)
    {
        return width;
    }
    font.get(b"FontDescriptor")
        .ok()
        .and_then(|value| resolve_object(document, value).ok())
        .and_then(|value| value.as_dict().ok())
        .and_then(|descriptor| descriptor.get(b"MissingWidth").ok())
        .and_then(number)
        .unwrap_or(0.0)
}

fn type0_width(document: &Document, font: &Dictionary, code: u32) -> f64 {
    let descendant = font
        .get(b"DescendantFonts")
        .ok()
        .and_then(|value| resolve_object(document, value).ok())
        .and_then(|value| value.as_array().ok())
        .and_then(|fonts| fonts.first())
        .and_then(|value| resolve_object(document, value).ok())
        .and_then(|value| value.as_dict().ok());
    let Some(descendant) = descendant else {
        return 0.0;
    };
    if let Some(widths) = descendant
        .get(b"W")
        .ok()
        .and_then(|value| resolve_object(document, value).ok())
        .and_then(|value| value.as_array().ok())
        && let Some(width) = cid_width(widths, code)
    {
        return width;
    }
    descendant
        .get(b"DW")
        .ok()
        .and_then(number)
        .unwrap_or(1_000.0)
}

fn cid_width(widths: &[Object], code: u32) -> Option<f64> {
    let mut index = 0usize;
    while index + 1 < widths.len() {
        let start = widths
            .get(index)?
            .as_i64()
            .ok()
            .and_then(|value| u32::try_from(value).ok())?;
        if let Ok(array) = widths[index + 1].as_array() {
            if let Some(offset) = code
                .checked_sub(start)
                .and_then(|value| usize::try_from(value).ok())
                && let Some(width) = array.get(offset).and_then(number)
            {
                return Some(width);
            }
            index += 2;
            continue;
        }
        if index + 2 >= widths.len() {
            break;
        }
        let end = widths[index + 1]
            .as_i64()
            .ok()
            .and_then(|value| u32::try_from(value).ok())?;
        if (start..=end).contains(&code) {
            return number(&widths[index + 2]);
        }
        index += 3;
    }
    None
}

fn decode_first_character(
    document: &Document,
    resources: Option<&Dictionary>,
    font_name: Option<&[u8]>,
    bytes: &[u8],
) -> Option<String> {
    let font = selected_font(document, resources, font_name)?;
    // lopdf prefers `/Encoding` over `/ToUnicode`; Type0 Word subsets carry
    // both (`Identity-H` plus the actual Unicode CMap). Remove the transport
    // encoding from a diagnostic clone so character labels follow ToUnicode.
    let mut to_unicode_font = font.clone();
    let decoding_font = if to_unicode_font.get(b"ToUnicode").is_ok() {
        to_unicode_font.remove(b"Encoding");
        &to_unicode_font
    } else {
        font
    };
    decoding_font
        .get_font_encoding(document)
        .ok()?
        .bytes_to_string(bytes)
        .ok()?
        .chars()
        .next()
        .map(|character| character.to_string())
}

fn interpret_form(
    document: &Document,
    resources: Option<&Dictionary>,
    operation: &Operation,
    state: &GraphicsState,
    extracted: &mut ExtractedPage,
) -> Result<()> {
    let Some(name) = operation
        .operands
        .first()
        .and_then(|operand| operand.as_name().ok())
    else {
        return Ok(());
    };
    let Some(resources) = resources else {
        return Ok(());
    };
    let Ok(xobjects) = resources.get(b"XObject") else {
        return Ok(());
    };
    let xobjects = resolve_object(document, xobjects)?.as_dict()?;
    let Ok(xobject) = xobjects.get(name) else {
        return Ok(());
    };
    let stream = resolve_object(document, xobject)?.as_stream()?;
    if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Form") {
        return Ok(());
    }
    let matrix = stream
        .dict
        .get(b"Matrix")
        .ok()
        .and_then(|object| resolve_object(document, object).ok())
        .and_then(|object| object.as_array().ok())
        .and_then(|array| Affine::from_operands(array))
        .unwrap_or_default();
    let form_resources = stream
        .dict
        .get(b"Resources")
        .ok()
        .and_then(|object| resolve_object(document, object).ok())
        .and_then(|object| object.as_dict().ok())
        .or(Some(resources));
    let content = Content::decode(&stream.get_plain_content()?)?;
    let mut form_state = state.clone();
    form_state.ctm = state.ctm.concat(matrix);
    clear_path(&mut form_state);
    form_state.text_matrix = Affine::default();
    form_state.text_line_matrix = Affine::default();
    interpret(
        document,
        &content.operations,
        form_resources,
        form_state,
        1,
        extracted,
    )
}

fn pair_borders(
    ours: &[BorderElement],
    truth: &[BorderElement],
    page_height: f64,
) -> (Vec<MatchRecord>, BTreeSet<usize>, BTreeSet<usize>) {
    let mut candidates = Vec::new();
    for (ours_index, ours_element) in ours.iter().enumerate() {
        for (truth_index, truth_element) in truth.iter().enumerate() {
            if ours_element.orientation != truth_element.orientation {
                continue;
            }
            let distance = ours_element.center().distance(truth_element.center());
            if distance <= PAIR_DISTANCE_PT {
                let length_penalty = (ours_element.length() - truth_element.length()).abs() * 0.02;
                candidates.push((distance + length_penalty, ours_index, truth_index));
            }
        }
    }
    candidates.sort_by(|left, right| left.0.total_cmp(&right.0));
    let mut used_ours = BTreeSet::new();
    let mut used_truth = BTreeSet::new();
    let mut matches = Vec::new();
    for (_, ours_index, truth_index) in candidates {
        if used_ours.contains(&ours_index) || used_truth.contains(&truth_index) {
            continue;
        }
        used_ours.insert(ours_index);
        used_truth.insert(truth_index);
        let ours_element = &ours[ours_index];
        let truth_element = &truth[truth_index];
        let ours_center = ours_element.center();
        let truth_center = truth_element.center();
        matches.push(MatchRecord {
            class: ours_element.orientation.class().to_string(),
            ours_index,
            truth_index,
            dx: round2(truth_center.x - ours_center.x),
            dy: round2(truth_center.y - ours_center.y),
            y_band: y_band((ours_center.y + truth_center.y) / 2.0, page_height),
            first_character: None,
        });
    }
    (matches, used_ours, used_truth)
}

fn pair_text(
    ours: &[TextOrigin],
    truth: &[TextOrigin],
    page_height: f64,
) -> (Vec<MatchRecord>, BTreeSet<usize>, BTreeSet<usize>) {
    let mut candidates = Vec::new();
    for (ours_index, ours_element) in ours.iter().enumerate() {
        for (truth_index, truth_element) in truth.iter().enumerate() {
            let distance = Point {
                x: ours_element.x,
                y: ours_element.y,
            }
            .distance(Point {
                x: truth_element.x,
                y: truth_element.y,
            });
            if distance <= PAIR_DISTANCE_PT {
                let character_penalty = if ours_element.first_character.is_some()
                    && truth_element.first_character.is_some()
                    && ours_element.first_character != truth_element.first_character
                {
                    1.5
                } else {
                    0.0
                };
                candidates.push((distance + character_penalty, ours_index, truth_index));
            }
        }
    }
    candidates.sort_by(|left, right| left.0.total_cmp(&right.0));
    let mut used_ours = BTreeSet::new();
    let mut used_truth = BTreeSet::new();
    let mut matches = Vec::new();
    for (_, ours_index, truth_index) in candidates {
        if used_ours.contains(&ours_index) || used_truth.contains(&truth_index) {
            continue;
        }
        used_ours.insert(ours_index);
        used_truth.insert(truth_index);
        matches.push(text_match_record(
            ours,
            truth,
            ours_index,
            truth_index,
            page_height,
        ));
    }

    // Equal canonical run counts are strong evidence that both producers
    // exposed the same lines, even when a real layout displacement exceeds the
    // ordinary 3pt local-pair radius. Preserve the already reliable local
    // matches, then pair only the remaining runs by global spatial proximity.
    // This reports the actual large delta instead of disguising it as missing
    // content, while avoiding content-stream index assumptions.
    if ours.len() == truth.len() && used_ours.len() < ours.len() {
        let mut remaining = Vec::new();
        for (ours_index, ours_element) in ours.iter().enumerate() {
            if used_ours.contains(&ours_index) {
                continue;
            }
            for (truth_index, truth_element) in truth.iter().enumerate() {
                if used_truth.contains(&truth_index) {
                    continue;
                }
                let distance = Point {
                    x: ours_element.x,
                    y: ours_element.y,
                }
                .distance(Point {
                    x: truth_element.x,
                    y: truth_element.y,
                });
                let character_penalty = if ours_element.first_character.is_some()
                    && truth_element.first_character.is_some()
                    && ours_element.first_character != truth_element.first_character
                {
                    12.0
                } else {
                    0.0
                };
                remaining.push((distance + character_penalty, ours_index, truth_index));
            }
        }
        remaining.sort_by(|left, right| left.0.total_cmp(&right.0));
        for (_, ours_index, truth_index) in remaining {
            if used_ours.contains(&ours_index) || used_truth.contains(&truth_index) {
                continue;
            }
            used_ours.insert(ours_index);
            used_truth.insert(truth_index);
            matches.push(text_match_record(
                ours,
                truth,
                ours_index,
                truth_index,
                page_height,
            ));
        }
    }
    (matches, used_ours, used_truth)
}

fn text_match_record(
    ours: &[TextOrigin],
    truth: &[TextOrigin],
    ours_index: usize,
    truth_index: usize,
    page_height: f64,
) -> MatchRecord {
    let ours_element = &ours[ours_index];
    let truth_element = &truth[truth_index];
    MatchRecord {
        class: "text".to_string(),
        ours_index,
        truth_index,
        dx: round2(truth_element.x - ours_element.x),
        dy: round2(truth_element.y - ours_element.y),
        y_band: y_band((ours_element.y + truth_element.y) / 2.0, page_height),
        first_character: ours_element
            .first_character
            .clone()
            .or_else(|| truth_element.first_character.clone()),
    }
}

fn duplicate_segments(borders: &[BorderElement]) -> Vec<DuplicateRecord> {
    let mut duplicates = Vec::new();
    for first_index in 0..borders.len() {
        for second_index in first_index + 1..borders.len() {
            let first = &borders[first_index];
            let second = &borders[second_index];
            if first.orientation != second.orientation
                || (first.length() - second.length()).abs() > 0.1
            {
                continue;
            }
            let aligned = match first.orientation {
                Orientation::Horizontal => {
                    (first.x0 - second.x0).abs() <= 0.1 && (first.x1 - second.x1).abs() <= 0.1
                }
                Orientation::Vertical => {
                    (first.y0 - second.y0).abs() <= 0.1 && (first.y1 - second.y1).abs() <= 0.1
                }
            };
            let separation = match first.orientation {
                Orientation::Horizontal => (first.y0 - second.y0).abs(),
                Orientation::Vertical => (first.x0 - second.x0).abs(),
            };
            if aligned && separation <= DUPLICATE_DISTANCE_PT {
                duplicates.push(DuplicateRecord {
                    first_index,
                    second_index,
                    orientation: first.orientation,
                    separation: round2(separation),
                });
            }
        }
    }
    duplicates
}

fn unmatched_indices(length: usize, used: &BTreeSet<usize>) -> Vec<usize> {
    (0..length).filter(|index| !used.contains(index)).collect()
}

fn unmatched_element_records(
    ours: &ExtractedPage,
    truth: &ExtractedPage,
    ours_borders: &[usize],
    truth_borders: &[usize],
    ours_text: &[usize],
    truth_text: &[usize],
) -> Vec<UnmatchedElement> {
    let mut records = Vec::new();
    for (source, page, indices) in [
        ("ours", ours, ours_borders),
        ("truth", truth, truth_borders),
    ] {
        records.extend(indices.iter().map(|index| {
            let border = &page.borders[*index];
            UnmatchedElement {
                source: source.to_string(),
                class: border.orientation.class().to_string(),
                index: *index,
                x0: border.x0,
                y0: border.y0,
                x1: Some(border.x1),
                y1: Some(border.y1),
                width: Some(border.width),
                font_size: None,
                first_character: None,
            }
        }));
    }
    for (source, page, indices) in [("ours", ours, ours_text), ("truth", truth, truth_text)] {
        records.extend(indices.iter().map(|index| {
            let text = &page.text[*index];
            UnmatchedElement {
                source: source.to_string(),
                class: "text".to_string(),
                index: *index,
                x0: text.x,
                y0: text.y,
                x1: None,
                y1: None,
                width: None,
                font_size: Some(text.font_size),
                first_character: text.first_character.clone(),
            }
        }));
    }
    records
}

fn y_band(y: f64, page_height: f64) -> u8 {
    if page_height <= 0.0 {
        return 0;
    }
    ((y / page_height * 5.0).floor() as i32).clamp(0, 4) as u8 + 1
}

fn transformed_point(state: &GraphicsState, operands: &[Object]) -> Option<Point> {
    Some(state.ctm.transform(Point {
        x: number(operands.first()?)?,
        y: number(operands.get(1)?)?,
    }))
}

fn number(object: &Object) -> Option<f64> {
    object.as_float().ok().map(f64::from)
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn resolve_object<'a>(document: &'a Document, object: &'a Object) -> lopdf::Result<&'a Object> {
    document.dereference(object).map(|(_, object)| object)
}

fn inherited_object<'a>(
    document: &'a Document,
    mut object_id: ObjectId,
    key: &[u8],
) -> Result<&'a Object> {
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(object_id) {
            bail!("cycle in inherited page dictionaries");
        }
        let dictionary = document.get_dictionary(object_id)?;
        if let Ok(value) = dictionary.get(key) {
            return resolve_object(document, value).map_err(Into::into);
        }
        object_id = dictionary
            .get(b"Parent")
            .and_then(Object::as_reference)
            .map_err(|_| {
                anyhow!(
                    "page tree has no inherited {}",
                    String::from_utf8_lossy(key)
                )
            })?;
    }
}

fn inherited_array<'a>(
    document: &'a Document,
    object_id: ObjectId,
    key: &[u8],
) -> Result<&'a Vec<Object>> {
    inherited_object(document, object_id, key)?
        .as_array()
        .map_err(Into::into)
}

fn inherited_dict<'a>(
    document: &'a Document,
    object_id: ObjectId,
    key: &[u8],
) -> Result<Option<&'a Dictionary>> {
    match inherited_object(document, object_id, key) {
        Ok(object) => Ok(Some(object.as_dict()?)),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Document, Object, Stream, dictionary};

    fn probe_pdf(dx: f32, dy: f32, duplicate: bool) -> Vec<u8> {
        probe_pdf_parts(dx, dy, dx, dy, duplicate, "Helvetica")
    }

    fn probe_pdf_parts(
        border_dx: f32,
        border_dy: f32,
        text_dx: f32,
        text_dy: f32,
        duplicate: bool,
        base_font: &str,
    ) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => Object::Name(base_font.as_bytes().to_vec()),
            "Encoding" => "WinAnsiEncoding",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut operations = vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    border_dx.into(),
                    border_dy.into(),
                ],
            ),
            Operation::new("w", vec![0.5.into()]),
            Operation::new("m", vec![10.into(), 80.into()]),
            Operation::new("l", vec![90.into(), 80.into()]),
            Operation::new("S", vec![]),
            Operation::new("m", vec![10.into(), 40.into()]),
            Operation::new("l", vec![10.into(), 80.into()]),
            Operation::new("S", vec![]),
            Operation::new("re", vec![20.into(), 40.into(), 60.into(), 1.into()]),
            Operation::new("f", vec![]),
        ];
        if duplicate {
            operations.extend([
                Operation::new("m", vec![10.into(), 80.1.into()]),
                Operation::new("l", vec![90.into(), 80.1.into()]),
                Operation::new("S", vec![]),
            ]);
        }
        operations.extend([
            Operation::new("Q", vec![]),
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    text_dx.into(),
                    text_dy.into(),
                ],
            ),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 15.into(), 60.into()],
            ),
            Operation::new("Tj", vec![Object::string_literal("A")]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
        ]);
        let content = Content { operations }.encode().expect("encode content");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => resources_id,
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
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save probe PDF");
        bytes
    }

    fn text_advancement_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let mut widths = vec![Object::Integer(500); 35];
        widths[0] = Object::Integer(250); // space
        widths[33] = Object::Integer(500); // A
        widths[34] = Object::Integer(600); // B
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 32,
            "LastChar" => 66,
            "Widths" => widths,
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let operations = vec![
            Operation::new("m", vec![10.into(), 10.into()]),
            Operation::new("l", vec![20.into(), 10.into()]),
            Operation::new("l", vec![20.into(), 20.into()]),
            Operation::new("l", vec![10.into(), 20.into()]),
            Operation::new("s", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
            Operation::new("TL", vec![12.into()]),
            Operation::new("Tc", vec![1.into()]),
            Operation::new("Tz", vec![50.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 10.into(), 60.into()],
            ),
            Operation::new("Tj", vec![Object::string_literal("A")]),
            Operation::new("Tj", vec![Object::string_literal("B")]),
            Operation::new(
                "TJ",
                vec![Object::Array(vec![
                    Object::string_literal("A"),
                    Object::Integer(-100),
                    Object::string_literal("B"),
                    Object::Integer(50),
                    Object::string_literal("A"),
                ])],
            ),
            Operation::new("\"", vec![2.into(), 1.into(), Object::string_literal(" ")]),
            Operation::new("Tj", vec![Object::string_literal("A")]),
            Operation::new("ET", vec![]),
        ];
        let content = Content { operations }.encode().expect("encode content");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => resources_id,
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
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save text probe PDF");
        bytes
    }

    #[test]
    fn type0_character_labels_prefer_to_unicode_over_identity_h() {
        let mut doc = Document::with_version("1.7");
        let cmap = br#"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfchar
<0001> <0041>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end"#
            .to_vec();
        let cmap_id = doc.add_object(Stream::new(dictionary! {}, cmap));
        let descendant_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "CIDFontType2",
            "BaseFont" => "Fixture",
            "CIDSystemInfo" => dictionary! {
                "Registry" => Object::string_literal("Adobe"),
                "Ordering" => Object::string_literal("Identity"),
                "Supplement" => 0,
            },
            "DW" => 1000,
        });
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "Fixture",
            "Encoding" => "Identity-H",
            "DescendantFonts" => vec![descendant_id.into()],
            "ToUnicode" => cmap_id,
        });
        let resources = dictionary! { "Font" => dictionary! { "F1" => font_id } };

        assert_eq!(
            decode_first_character(&doc, Some(&resources), Some(b"F1"), &[0, 1]),
            Some("A".to_string())
        );
    }

    #[test]
    fn pairs_stroked_lines_filled_rules_and_text_under_ctm() {
        let ours = probe_pdf(0.0, 0.0, false);
        let truth = probe_pdf(0.5, 0.5, false);
        let report = compare_page(&ours, &truth, 1).expect("compare probe PDFs");
        assert_eq!(report.ours_borders, 2);
        assert_eq!(report.truth_borders, 2);
        assert_eq!(report.ours_text, 1);
        assert_eq!(report.truth_text, 1);
        assert_eq!(report.matches.len(), 3);
        assert!(
            report
                .clusters
                .iter()
                .all(|cluster| cluster.dx == 0.5 && cluster.dy == -0.5)
        );
    }

    #[test]
    fn one_pixel_translation_fails_the_default_geometry_gate() {
        let ours = probe_pdf(0.0, 0.0, false);
        let translated = probe_pdf(0.5, 0.0, false);
        let report = compare_page(&ours, &translated, 1).expect("compare translated page");
        let metrics = geometry_metrics(&report, &GeometryLimits::default());

        assert!(!metrics.pass);
        assert_eq!(
            metrics.violation.as_deref(),
            Some("page 1 borders median dx +0.50pt")
        );
    }

    #[test]
    fn pre_autofit_table_border_displacement_fails_geometry() {
        let ours = probe_pdf_parts(0.0, 0.0, 0.0, 0.0, false, "Helvetica");
        let pre_autofit = probe_pdf_parts(2.0, 0.0, 0.0, 0.0, false, "Helvetica");
        let report = compare_page(&ours, &pre_autofit, 1).expect("compare table geometry");
        let metrics = geometry_metrics(&report, &GeometryLimits::default());

        assert!(!metrics.pass);
        assert!(
            metrics
                .violation
                .as_deref()
                .is_some_and(|note| { note.contains("page 1 borders") && note.contains("dx") })
        );
    }

    #[test]
    fn substituted_font_with_identical_origins_passes_geometry() {
        let ours = probe_pdf_parts(0.0, 0.0, 0.0, 0.0, false, "Helvetica");
        let substituted = probe_pdf_parts(0.0, 0.0, 0.0, 0.0, false, "Times-Roman");
        let report = compare_page(&ours, &substituted, 1).expect("compare substituted page");
        let metrics = geometry_metrics(&report, &GeometryLimits::default());

        assert!(metrics.pass, "{:?}", metrics.violation);
    }

    #[test]
    fn equal_text_run_sequences_report_large_deltas_instead_of_unmatched_runs() {
        let ours = probe_pdf_parts(0.0, 0.0, 0.0, 0.0, false, "Helvetica");
        let shifted = probe_pdf_parts(0.0, 0.0, 10.0, 0.0, false, "Helvetica");
        let report = compare_page(&ours, &shifted, 1).expect("compare shifted text");
        let text_matches = report
            .matches
            .iter()
            .filter(|record| record.class == "text")
            .collect::<Vec<_>>();

        assert_eq!(text_matches.len(), 1);
        assert_eq!(text_matches[0].dx, 10.0);
        assert!(report.unmatched.ours_text.is_empty());
        assert!(report.unmatched.truth_text.is_empty());
    }

    #[test]
    fn equal_text_counts_do_not_force_content_stream_index_pairing() {
        let ours = vec![
            TextOrigin {
                x: 10.0,
                y: 20.0,
                font_size: 10.0,
                first_character: Some("A".into()),
            },
            TextOrigin {
                x: 90.0,
                y: 80.0,
                font_size: 10.0,
                first_character: Some("B".into()),
            },
        ];
        let truth = vec![
            TextOrigin {
                x: 90.0,
                y: 80.0,
                font_size: 10.0,
                first_character: Some("B".into()),
            },
            TextOrigin {
                x: 10.0,
                y: 20.0,
                font_size: 10.0,
                first_character: Some("A".into()),
            },
        ];

        let (matches, _, _) = pair_text(&ours, &truth, 100.0);

        assert_eq!(matches.len(), 2);
        assert!(
            matches
                .iter()
                .all(|record| record.dx == 0.0 && record.dy == 0.0)
        );
    }

    #[test]
    fn detects_near_coincident_duplicate_segments() {
        let ours = probe_pdf(0.0, 0.0, true);
        let truth = probe_pdf(0.0, 0.0, false);
        let report = compare_page(&ours, &truth, 1).expect("compare probe PDFs");
        assert_eq!(report.ours_borders, 3);
        assert_eq!(report.duplicates.len(), 1);
        assert_eq!(report.unmatched.elements.len(), 1);
        assert_eq!(report.unmatched.elements[0].class, "h_border");
        assert_eq!(report.unmatched.elements[0].x0, 10.0);
    }

    #[test]
    fn canonicalizes_sub_tenth_point_pdf_axis_rounding() {
        let borders = canonicalize_borders(vec![
            BorderElement {
                orientation: Orientation::Horizontal,
                x0: 10.0,
                y0: 20.0,
                x1: 30.0,
                y1: 20.0,
                width: 0.5,
            },
            BorderElement {
                orientation: Orientation::Horizontal,
                x0: 30.0,
                y0: 20.08,
                x1: 40.0,
                y1: 20.08,
                width: 0.5,
            },
        ]);

        assert_eq!(borders.len(), 1);
        assert_eq!(borders[0].x0, 10.0);
        assert_eq!(borders[0].x1, 40.0);
    }

    #[test]
    fn structural_borders_ignore_subpoint_stroke_join_fragments() {
        let borders = structural_borders(vec![
            BorderElement {
                orientation: Orientation::Horizontal,
                x0: 10.0,
                y0: 20.0,
                x1: 30.0,
                y1: 20.0,
                width: 0.5,
            },
            BorderElement {
                orientation: Orientation::Vertical,
                x0: 20.0,
                y0: 10.0,
                x1: 20.0,
                y1: 30.0,
                width: 0.5,
            },
            BorderElement {
                orientation: Orientation::Horizontal,
                x0: 19.64,
                y0: 30.0,
                x1: 20.36,
                y1: 30.0,
                width: 0.5,
            },
        ]);

        assert_eq!(borders.len(), 2);
        assert!(borders.iter().all(|border| border.length() >= 1.0));
    }

    #[test]
    fn comparable_borders_tolerate_producer_specific_l_shaped_strokes() {
        let ours = vec![
            BorderElement {
                orientation: Orientation::Horizontal,
                x0: 10.0,
                y0: 20.0,
                x1: 30.0,
                y1: 20.0,
                width: 0.5,
            },
            BorderElement {
                orientation: Orientation::Vertical,
                x0: 30.0,
                y0: 20.0,
                x1: 30.0,
                y1: 40.0,
                width: 0.5,
            },
        ];
        let truth = vec![BorderElement {
            orientation: Orientation::Horizontal,
            x0: 10.5,
            y0: 20.2,
            x1: 30.5,
            y1: 20.2,
            width: 0.5,
        }];

        let (ours, truth) = canonicalize_comparable_borders(ours, truth);

        assert_eq!(ours.len(), 1);
        assert_eq!(truth.len(), 1);
        assert_eq!(ours[0].orientation, Orientation::Horizontal);
        assert_eq!(truth[0].orientation, Orientation::Horizontal);
    }

    #[test]
    fn accepts_equivalent_page_boxes_with_pdf_rounding_noise() {
        let ours = probe_pdf(0.0, 0.0, false);
        let mut truth = Document::load_mem(&probe_pdf(0.0, 0.0, false)).expect("load probe PDF");
        let page_id = truth.get_pages()[&1];
        truth
            .get_object_mut(page_id)
            .expect("page object")
            .as_dict_mut()
            .expect("page dictionary")
            .set(
                "MediaBox",
                vec![
                    0.into(),
                    0.into(),
                    Object::Real(100.02),
                    Object::Real(99.98),
                ],
            );
        let mut truth_bytes = Vec::new();
        truth
            .save_to(&mut truth_bytes)
            .expect("save rounded probe PDF");

        compare_page(&ours, &truth_bytes, 1)
            .expect("0.02pt producer rounding must not block geometry probing");
    }

    #[test]
    fn text_show_operators_advance_origins_with_widths_spacing_and_tj_adjustments() {
        let page = extract_page(&text_advancement_pdf(), 1).expect("extract text probe");
        assert_eq!(
            page.borders.len(),
            4,
            "s must close and paint the fourth edge"
        );
        let origins = page
            .text
            .iter()
            .map(|origin| (origin.x, origin.y))
            .collect::<Vec<_>>();
        assert_eq!(
            origins,
            [
                (10.0, 40.0),
                (13.0, 40.0),
                (16.5, 40.0),
                (20.0, 40.0),
                (23.25, 40.0),
                (10.0, 52.0),
                (12.75, 52.0)
            ]
        );
    }

    #[test]
    fn comparison_serializes_lossless_content_stream_records() {
        let pdf = text_advancement_pdf();
        let report = compare_page(&pdf, &pdf, 1).expect("compare text probe PDF");
        let json = serde_json::to_value(report).expect("serialize probe report");

        assert!(
            json.pointer("/ours_content/text/0/resource_name").is_some()
                && json.pointer("/ours_content/text/0/font_subtype").is_some()
                && json.pointer("/ours_content/text/0/tf_pt").is_some()
                && json.pointer("/ours_content/text/0/tm").is_some()
                && json
                    .pointer("/ours_content/text/0/glyphs/0/source_code")
                    .is_some()
                && json
                    .pointer("/ours_content/text/2/tj_adjustments/0")
                    .is_some(),
            "content-stream diagnostic fields are absent: {json}"
        );
    }

    #[test]
    fn content_stream_records_positive_and_negative_tj_values_exactly() {
        let content =
            extract_content_page(&text_advancement_pdf(), 1).expect("extract text content");

        assert_eq!(content.text[2].tj_adjustments, [-100.0, 50.0]);
    }

    #[test]
    fn content_stream_keeps_strings_split_across_showing_operators() {
        let content =
            extract_content_page(&text_advancement_pdf(), 1).expect("extract text content");
        let text = content
            .text
            .iter()
            .map(|record| record.unicode.as_str())
            .collect::<Vec<_>>();

        assert_eq!(text, ["A", "B", "ABA", " ", "A"]);
    }

    #[test]
    fn compact_cid_to_gid_map_is_reported_without_identity_assumptions() {
        let mut doc = Document::with_version("1.7");
        let map_id = doc.add_object(Stream::new(dictionary! {}, vec![0, 0, 0, 37]));
        let descendant = Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "CIDFontType2",
            "CIDToGIDMap" => map_id,
        });
        let font = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "Encoding" => "Identity-H",
            "DescendantFonts" => vec![descendant],
        };

        assert_eq!(source_gid(&doc, &font, true, 1, "A"), Some(37));
    }

    #[test]
    fn image_content_records_placement_clip_and_jpeg_sampling() {
        let content = extract_content_page(&image_content_pdf(), 1).expect("extract image content");
        let image = &content.images[0];

        assert!(
            image.width_px == Some(2)
                && image.height_px == Some(1)
                && image.filters == ["DCTDecode"]
                && image.placement_matrix == [20.0, 0.0, 0.0, 10.0, 5.0, 7.0]
                && image.clip_rectangle.is_some_and(|clip| {
                    clip.x0 == 5.0 && clip.y0 == 7.0 && clip.x1 == 25.0 && clip.y1 == 17.0
                })
                && image.jpeg.as_ref().is_some_and(|jpeg| {
                    jpeg.components[0].horizontal_sampling == 2
                        && jpeg.components[0].vertical_sampling == 2
                }),
            "incomplete image record: {image:?}"
        );
    }

    fn image_content_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 2,
                "Height" => 1,
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            },
            minimal_jpeg_420(),
        ));
        let resources_id = doc.add_object(dictionary! {
            "XObject" => dictionary! { "Im1" => image_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![20.into(), 0.into(), 0.into(), 10.into(), 5.into(), 7.into()],
                ),
                Operation::new("re", vec![0.into(), 0.into(), 1.into(), 1.into()]),
                Operation::new("W", vec![]),
                Operation::new("n", vec![]),
                Operation::new("Do", vec![Object::Name(b"Im1".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        }
        .encode()
        .expect("encode image content");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => resources_id,
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
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save image probe PDF");
        bytes
    }

    fn minimal_jpeg_420() -> Vec<u8> {
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00];
        jpeg.extend([1; 64]);
        jpeg.extend([
            0xff, 0xc0, 0x00, 0x11, 8, 0, 1, 0, 2, 3, 1, 0x22, 0, 2, 0x11, 0, 3, 0x11, 0, 0xff,
            0xda,
        ]);
        jpeg
    }
}

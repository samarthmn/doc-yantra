//! Dev-only vector geometry probe for pairing engine and Word PDF elements.

use anyhow::{Context, Result, anyhow, bail};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};
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
    pub matches: Vec<MatchRecord>,
    pub clusters: Vec<DeltaCluster>,
    pub unmatched: UnmatchedReport,
    pub duplicates: Vec<DuplicateRecord>,
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
    let ours = extract_page(ours, page).context("extract our PDF geometry")?;
    let truth = extract_page(truth, page).context("extract truth PDF geometry")?;
    // PDF producers commonly round A4's fractional point dimensions to two
    // nearby decimal representations. This tolerance is far below the probe's
    // geometry matching thresholds but avoids rejecting equivalent page boxes.
    const PAGE_BOX_TOLERANCE_PT: f64 = 0.05;
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
        let ours_element = &ours[ours_index];
        let truth_element = &truth[truth_index];
        matches.push(MatchRecord {
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
        });
    }
    (matches, used_ours, used_truth)
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
        let mut doc = Document::with_version("1.7");
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut operations = vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), dx.into(), dy.into()],
            ),
            Operation::new("w", vec![0.5.into()]),
            Operation::new("m", vec![10.into(), 80.into()]),
            Operation::new("l", vec![90.into(), 80.into()]),
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
            "Subtype" => "Type1",
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
                (10.0, 52.0),
                (12.75, 52.0)
            ]
        );
    }
}

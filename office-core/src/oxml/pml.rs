//! PresentationML model: `presentation.xml`, slide masters, layouts, and
//! slides. Parse-only (no layout). The shape tree is modelled generically
//! enough for the pptx layout engine (agent task 5).

use super::theme::{ColorRef, ColorTransform, parse_color_element};
use super::{A, Node, P, R, attr, child, children, emu_to_pt, is, parse_f64, parse_i64};
use crate::{OfficeError, Result};

/// Slide dimensions in points.
#[derive(Debug, Clone, Copy)]
pub struct SlideSize {
    /// Width in points.
    pub w_pt: f64,
    /// Height in points.
    pub h_pt: f64,
}

impl Default for SlideSize {
    fn default() -> Self {
        // 16:9 default (13.333 × 7.5 in).
        SlideSize {
            w_pt: 960.0,
            h_pt: 540.0,
        }
    }
}

/// The presentation: slide size and the ordered slide relationship ids.
#[derive(Debug, Clone, Default)]
pub struct Presentation {
    /// Slide size.
    pub size: SlideSize,
    /// Slide part relationship ids, in order.
    pub slide_rel_ids: Vec<String>,
}

impl Presentation {
    /// Parses `ppt/presentation.xml`.
    pub fn parse(xml: &str) -> Result<Presentation> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| OfficeError::Invalid(format!("presentation.xml: {e}")))?;
        let root = doc.root_element();
        let mut pres = Presentation::default();
        if let Some(sz) = child(root, P, "sldSz") {
            let cx = parse_i64(attr(sz, P, "cx")).or_else(|| parse_i64(attr(sz, A, "cx")));
            let cy = parse_i64(attr(sz, P, "cy")).or_else(|| parse_i64(attr(sz, A, "cy")));
            if let (Some(cx), Some(cy)) = (cx, cy) {
                pres.size = SlideSize {
                    w_pt: emu_to_pt(cx),
                    h_pt: emu_to_pt(cy),
                };
            }
        }
        if let Some(lst) = child(root, P, "sldIdLst") {
            for sld in children(lst) {
                if is(sld, P, "sldId")
                    && let Some(rid) = attr(sld, R, "id")
                {
                    pres.slide_rel_ids.push(rid.to_string());
                }
            }
        }
        Ok(pres)
    }
}

/// A 2D transform from `a:xfrm`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Xfrm {
    /// Offset x in points.
    pub x_pt: f64,
    /// Offset y in points.
    pub y_pt: f64,
    /// Extent width in points.
    pub w_pt: f64,
    /// Extent height in points.
    pub h_pt: f64,
    /// Rotation in degrees (clockwise).
    pub rot_deg: f64,
    /// Horizontal flip.
    pub flip_h: bool,
    /// Vertical flip.
    pub flip_v: bool,
    /// Whether any of the above was actually present.
    pub present: bool,
}

impl Xfrm {
    fn parse(node: Node) -> Xfrm {
        let mut x = Xfrm::default();
        if let Some(off) = child(node, A, "off") {
            x.x_pt = parse_i64(attr(off, A, "x")).map(emu_to_pt).unwrap_or(0.0);
            x.y_pt = parse_i64(attr(off, A, "y")).map(emu_to_pt).unwrap_or(0.0);
        }
        if let Some(ext) = child(node, A, "ext") {
            x.w_pt = parse_i64(attr(ext, A, "cx")).map(emu_to_pt).unwrap_or(0.0);
            x.h_pt = parse_i64(attr(ext, A, "cy")).map(emu_to_pt).unwrap_or(0.0);
        }
        x.rot_deg = parse_f64(attr(node, A, "rot"))
            .map(|r| r / 60_000.0)
            .unwrap_or(0.0);
        x.flip_h = attr(node, A, "flipH") == Some("1");
        x.flip_v = attr(node, A, "flipV") == Some("1");
        x.present = true;
        x
    }
}

/// A placeholder reference (`p:ph`).
#[derive(Debug, Clone, Default)]
pub struct Placeholder {
    /// Placeholder type (`title`, `body`, `ctrTitle`, `subTitle`, …).
    pub ph_type: Option<String>,
    /// Placeholder index.
    pub idx: Option<u32>,
}

/// A fill.
#[derive(Debug, Clone)]
pub enum Fill {
    /// No fill.
    None,
    /// A solid color.
    Solid(ColorSpec),
    /// A linear gradient (angle degrees, stops).
    LinearGradient {
        /// Gradient angle in degrees.
        angle_deg: f64,
        /// Color stops as `(position 0..1, color)`.
        stops: Vec<(f64, ColorSpec)>,
    },
    /// A picture fill (relationship id).
    Picture(String),
}

/// A color reference plus its transforms (resolved against a theme later).
#[derive(Debug, Clone)]
pub struct ColorSpec {
    /// Base color reference.
    pub base: ColorRef,
    /// Transforms applied in order.
    pub transforms: Vec<ColorTransform>,
}

/// A shape's visual properties (`p:spPr`).
#[derive(Debug, Clone, Default)]
pub struct ShapeProps {
    /// Transform, if present.
    pub xfrm: Option<Xfrm>,
    /// Preset geometry name (`prstGeom @prst`), if any.
    pub preset: Option<String>,
    /// Whether a `custGeom` is present.
    pub custom_geom: bool,
    /// Fill.
    pub fill: Option<Fill>,
    /// Line (outline) fill + width in points.
    pub line: Option<(ColorSpec, f64)>,
}

/// A text run in a text body.
#[derive(Debug, Clone, Default)]
pub struct TextRun {
    /// The run text.
    pub text: String,
    /// Bold.
    pub bold: Option<bool>,
    /// Italic.
    pub italic: Option<bool>,
    /// Underline.
    pub underline: bool,
    /// Font size in points (from `sz`, hundredths of a point).
    pub size_pt: Option<f64>,
    /// Latin typeface (`a:latin @typeface`, resolving `+mj-lt`/`+mn-lt`).
    pub typeface: Option<String>,
    /// Solid fill color.
    pub color: Option<ColorSpec>,
    /// Hyperlink relationship id.
    pub hlink: Option<String>,
}

/// A text-body paragraph.
#[derive(Debug, Clone, Default)]
pub struct TextParagraph {
    /// Indent level (0-based).
    pub level: u8,
    /// Alignment (`l`/`ctr`/`r`/`just`).
    pub align: Option<String>,
    /// Whether a bullet is suppressed (`a:buNone`).
    pub bullet_none: bool,
    /// A literal bullet char (`a:buChar @char`).
    pub bullet_char: Option<String>,
    /// Runs.
    pub runs: Vec<TextRun>,
}

/// A text body (`p:txBody`).
#[derive(Debug, Clone, Default)]
pub struct TextBody {
    /// Paragraphs.
    pub paragraphs: Vec<TextParagraph>,
    /// Vertical anchor (`t`/`ctr`/`b`).
    pub anchor: Option<String>,
    /// Autofit font scale (thousandths of a percent) if `normAutofit`.
    pub font_scale: Option<f64>,
    /// Left/right inset in points.
    pub inset_lr: (f64, f64),
    /// Top/bottom inset in points.
    pub inset_tb: (f64, f64),
}

/// A shape-tree element.
#[derive(Debug, Clone)]
pub enum ShapeElem {
    /// A shape (`p:sp`).
    Shape {
        /// Placeholder reference.
        ph: Option<Placeholder>,
        /// Visual properties.
        props: ShapeProps,
        /// Text body.
        text: Option<TextBody>,
    },
    /// A picture (`p:pic`).
    Picture {
        /// Visual properties (xfrm).
        props: ShapeProps,
        /// The image drawing (blip rel id, crop).
        drawing: PictureRef,
    },
    /// A group (`p:grpSp`) — children with a composed transform.
    Group {
        /// Group transform.
        xfrm: Option<Xfrm>,
        /// Child-space offset/extent for composition.
        child_space: Option<(Xfrm, Xfrm)>,
        /// Children.
        children: Vec<ShapeElem>,
    },
    /// A table (`p:graphicFrame` with `a:tbl`).
    Table {
        /// Frame transform.
        xfrm: Option<Xfrm>,
        /// The table.
        table: super::pml::PmlTable,
    },
    /// An unsupported graphic-frame (chart/diagram/ole) — carries a reason detail.
    Unsupported(String),
}

/// A minimal picture reference.
#[derive(Debug, Clone, Default)]
pub struct PictureRef {
    /// Blip relationship id.
    pub blip_rel_id: Option<String>,
    /// Whether a source crop is present.
    pub cropped: bool,
}

/// A PPTX table (`a:tbl`).
#[derive(Debug, Clone, Default)]
pub struct PmlTable {
    /// Column widths in points.
    pub col_widths: Vec<f64>,
    /// Rows.
    pub rows: Vec<PmlRow>,
}

/// A PPTX table row.
#[derive(Debug, Clone, Default)]
pub struct PmlRow {
    /// Row height (minimum) in points.
    pub height_pt: f64,
    /// Cells.
    pub cells: Vec<PmlCell>,
}

/// A PPTX table cell.
#[derive(Debug, Clone, Default)]
pub struct PmlCell {
    /// Horizontal span.
    pub grid_span: u32,
    /// Whether merged horizontally into a prior cell.
    pub h_merge: bool,
    /// Cell text.
    pub text: Option<TextBody>,
    /// Fill.
    pub fill: Option<Fill>,
}

/// A parsed slide / layout / master (they share the shape-tree shape).
#[derive(Debug, Clone, Default)]
pub struct Slide {
    /// Top-level shapes.
    pub shapes: Vec<ShapeElem>,
    /// Slide background fill.
    pub background: Option<Fill>,
    /// The layout relationship id (slides only).
    pub layout_rel_id: Option<String>,
}

impl Slide {
    /// Parses a slide/layout/master part (`p:sld`/`p:sldLayout`/`p:sldMaster`).
    pub fn parse(xml: &str) -> Result<Slide> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| OfficeError::Invalid(format!("slide XML: {e}")))?;
        let root = doc.root_element();
        let mut slide = Slide::default();
        let Some(csld) = child(root, P, "cSld") else {
            return Ok(slide);
        };
        if let Some(bg) = child(csld, P, "bg") {
            slide.background = parse_bg(bg);
        }
        if let Some(tree) = child(csld, P, "spTree") {
            for node in children(tree) {
                if let Some(elem) = parse_shape_elem(node) {
                    slide.shapes.push(elem);
                }
            }
        }
        Ok(slide)
    }
}

fn parse_bg(bg: Node) -> Option<Fill> {
    let pr = child(bg, P, "bgPr")?;
    parse_fill_children(pr)
}

fn parse_shape_elem(node: Node) -> Option<ShapeElem> {
    match node.tag_name().name() {
        "sp" => Some(parse_sp(node)),
        "pic" => Some(parse_pic(node)),
        "grpSp" => Some(parse_grp(node)),
        "graphicFrame" => Some(parse_graphic_frame(node)),
        "cxnSp" => Some(parse_sp(node)), // connector: treat as a shape with a line
        _ => None,
    }
}

fn parse_sp(node: Node) -> ShapeElem {
    let ph = child(node, P, "nvSpPr")
        .and_then(|nv| child(nv, P, "nvPr"))
        .and_then(|nvpr| child(nvpr, P, "ph"))
        .map(parse_ph);
    let props = child(node, P, "spPr").map(parse_sp_pr).unwrap_or_default();
    let text = child(node, P, "txBody").map(parse_txbody);
    ShapeElem::Shape { ph, props, text }
}

fn parse_ph(ph: Node) -> Placeholder {
    Placeholder {
        ph_type: attr(ph, P, "type").map(str::to_string),
        idx: parse_i64(attr(ph, P, "idx")).map(|v| v.max(0) as u32),
    }
}

fn parse_sp_pr(node: Node) -> ShapeProps {
    let mut props = ShapeProps::default();
    for c in children(node) {
        match c.tag_name().name() {
            "xfrm" => props.xfrm = Some(Xfrm::parse(c)),
            "prstGeom" => props.preset = attr(c, A, "prst").map(str::to_string),
            "custGeom" => props.custom_geom = true,
            "solidFill" | "gradFill" | "blipFill" | "noFill" | "pattFill" => {
                props.fill = parse_one_fill(c);
            }
            "ln" => props.line = parse_line(c),
            _ => {}
        }
    }
    props
}

fn parse_line(node: Node) -> Option<(ColorSpec, f64)> {
    let width = parse_i64(attr(node, A, "w")).map(emu_to_pt).unwrap_or(1.0);
    let color = child(node, A, "solidFill").and_then(|sf| color_from_solid(sf))?;
    Some((color, width))
}

fn parse_fill_children(node: Node) -> Option<Fill> {
    children(node).find_map(parse_one_fill)
}

fn parse_one_fill(node: Node) -> Option<Fill> {
    match node.tag_name().name() {
        "noFill" => Some(Fill::None),
        "solidFill" => color_from_solid(node).map(Fill::Solid),
        "gradFill" => Some(parse_gradient(node)),
        "blipFill" => child(node, A, "blip")
            .and_then(|b| attr(b, R, "embed"))
            .map(|id| Fill::Picture(id.to_string())),
        _ => None,
    }
}

fn color_from_solid(node: Node) -> Option<ColorSpec> {
    children(node).find_map(|c| {
        parse_color_element(c).map(|(base, transforms)| ColorSpec { base, transforms })
    })
}

fn parse_gradient(node: Node) -> Fill {
    let mut stops = Vec::new();
    if let Some(list) = child(node, A, "gsLst") {
        for gs in children(list) {
            if is(gs, A, "gs") {
                let pos = parse_f64(attr(gs, A, "pos"))
                    .map(|p| p / 100_000.0)
                    .unwrap_or(0.0);
                if let Some(color) = color_from_solid_like(gs) {
                    stops.push((pos, color));
                }
            }
        }
    }
    let angle = child(node, A, "lin")
        .and_then(|l| parse_f64(attr(l, A, "ang")))
        .map(|a| a / 60_000.0)
        .unwrap_or(0.0);
    Fill::LinearGradient {
        angle_deg: angle,
        stops,
    }
}

fn color_from_solid_like(node: Node) -> Option<ColorSpec> {
    children(node).find_map(|c| {
        parse_color_element(c).map(|(base, transforms)| ColorSpec { base, transforms })
    })
}

fn parse_txbody(node: Node) -> TextBody {
    // Defaults: 91440 EMU (7.2pt) l/r, 45720 EMU (3.6pt) t/b insets.
    let mut body = TextBody {
        inset_lr: (7.2, 7.2),
        inset_tb: (3.6, 3.6),
        ..Default::default()
    };
    for c in children(node) {
        match c.tag_name().name() {
            "bodyPr" => {
                body.anchor = attr(c, A, "anchor").map(str::to_string);
                if let Some(l) = parse_i64(attr(c, A, "lIns")) {
                    body.inset_lr.0 = emu_to_pt(l);
                }
                if let Some(r) = parse_i64(attr(c, A, "rIns")) {
                    body.inset_lr.1 = emu_to_pt(r);
                }
                if let Some(t) = parse_i64(attr(c, A, "tIns")) {
                    body.inset_tb.0 = emu_to_pt(t);
                }
                if let Some(b) = parse_i64(attr(c, A, "bIns")) {
                    body.inset_tb.1 = emu_to_pt(b);
                }
                if let Some(na) = child(c, A, "normAutofit") {
                    body.font_scale = parse_f64(attr(na, A, "fontScale")).map(|f| f / 100_000.0);
                }
            }
            "p" => body.paragraphs.push(parse_text_paragraph(c)),
            _ => {}
        }
    }
    body
}

fn parse_text_paragraph(node: Node) -> TextParagraph {
    let mut para = TextParagraph::default();
    for c in children(node) {
        match c.tag_name().name() {
            "pPr" => {
                para.level = parse_i64(attr(c, A, "lvl")).unwrap_or(0).clamp(0, 8) as u8;
                para.align = attr(c, A, "algn").map(str::to_string);
                for b in children(c) {
                    match b.tag_name().name() {
                        "buNone" => para.bullet_none = true,
                        "buChar" => para.bullet_char = attr(b, A, "char").map(str::to_string),
                        _ => {}
                    }
                }
            }
            "r" => para.runs.push(parse_text_run(c)),
            "br" => para.runs.push(TextRun {
                text: "\n".to_string(),
                ..Default::default()
            }),
            "fld" => {
                if let Some(t) = child(c, A, "t") {
                    para.runs.push(TextRun {
                        text: t.text().unwrap_or_default().to_string(),
                        ..Default::default()
                    });
                }
            }
            _ => {}
        }
    }
    para
}

fn parse_text_run(node: Node) -> TextRun {
    let mut run = TextRun::default();
    if let Some(t) = child(node, A, "t") {
        run.text = t.text().unwrap_or_default().to_string();
    }
    if let Some(rpr) = child(node, A, "rPr") {
        run.bold = attr(rpr, A, "b").map(|v| v == "1");
        run.italic = attr(rpr, A, "i").map(|v| v == "1");
        run.underline = attr(rpr, A, "u").map(|v| v != "none").unwrap_or(false);
        run.size_pt = parse_f64(attr(rpr, A, "sz")).map(|s| s / 100.0);
        if let Some(latin) = child(rpr, A, "latin") {
            run.typeface = attr(latin, A, "typeface").map(str::to_string);
        }
        run.color = child(rpr, A, "solidFill").and_then(color_from_solid);
        if let Some(hl) = child(rpr, A, "hlinkClick") {
            run.hlink = attr(hl, R, "id").map(str::to_string);
        }
    }
    run
}

fn parse_pic(node: Node) -> ShapeElem {
    let props = child(node, P, "spPr").map(parse_sp_pr).unwrap_or_default();
    let mut drawing = PictureRef::default();
    if let Some(bf) = child(node, P, "blipFill") {
        drawing.blip_rel_id = child(bf, A, "blip")
            .and_then(|b| attr(b, R, "embed"))
            .map(str::to_string);
        drawing.cropped = child(bf, A, "srcRect").is_some();
    }
    ShapeElem::Picture { props, drawing }
}

fn parse_grp(node: Node) -> ShapeElem {
    let grp_pr = child(node, P, "grpSpPr");
    let xfrm = grp_pr.and_then(|pr| child(pr, A, "xfrm")).map(Xfrm::parse);
    let child_space = grp_pr.and_then(|pr| child(pr, A, "xfrm")).and_then(|x| {
        let ch_off = child(x, A, "chOff").map(Xfrm::parse);
        let ch_ext = child(x, A, "chExt").map(Xfrm::parse);
        // chOff/chExt are stored on the same xfrm; approximate via a second parse.
        ch_off.zip(ch_ext)
    });
    let mut kids = Vec::new();
    for c in children(node) {
        if let Some(elem) = parse_shape_elem(c) {
            kids.push(elem);
        }
    }
    ShapeElem::Group {
        xfrm,
        child_space,
        children: kids,
    }
}

fn parse_graphic_frame(node: Node) -> ShapeElem {
    let xfrm = child(node, P, "xfrm").map(Xfrm::parse);
    let Some(graphic) = child(node, A, "graphic") else {
        return ShapeElem::Unsupported("empty graphicFrame".into());
    };
    let Some(data) = child(graphic, A, "graphicData") else {
        return ShapeElem::Unsupported("graphicFrame without data".into());
    };
    let uri = attr(data, A, "uri").unwrap_or_default();
    if let Some(tbl) = child(data, A, "tbl") {
        return ShapeElem::Table {
            xfrm,
            table: parse_table(tbl),
        };
    }
    if uri.contains("chart") {
        ShapeElem::Unsupported("chart".into())
    } else if uri.contains("diagram") {
        ShapeElem::Unsupported("smartart".into())
    } else if uri.contains("ole") {
        ShapeElem::Unsupported("ole".into())
    } else {
        ShapeElem::Unsupported(format!("graphic:{uri}"))
    }
}

fn parse_table(tbl: Node) -> PmlTable {
    let mut table = PmlTable::default();
    for c in children(tbl) {
        match c.tag_name().name() {
            "tblGrid" => {
                for gc in children(c) {
                    if is(gc, A, "gridCol") {
                        table
                            .col_widths
                            .push(parse_i64(attr(gc, A, "w")).map(emu_to_pt).unwrap_or(72.0));
                    }
                }
            }
            "tr" => {
                let mut row = PmlRow {
                    height_pt: parse_i64(attr(c, A, "h")).map(emu_to_pt).unwrap_or(0.0),
                    cells: Vec::new(),
                };
                for tc in children(c) {
                    if is(tc, A, "tc") {
                        row.cells.push(parse_table_cell(tc));
                    }
                }
                table.rows.push(row);
            }
            _ => {}
        }
    }
    table
}

fn parse_table_cell(tc: Node) -> PmlCell {
    let mut cell = PmlCell {
        grid_span: parse_i64(attr(tc, A, "gridSpan")).unwrap_or(1).max(1) as u32,
        h_merge: attr(tc, A, "hMerge") == Some("1"),
        text: child(tc, A, "txBody").map(parse_txbody),
        fill: None,
    };
    if let Some(pr) = child(tc, A, "tcPr") {
        cell.fill = parse_fill_children(pr);
    }
    cell
}

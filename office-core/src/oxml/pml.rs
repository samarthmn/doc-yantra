//! PresentationML model: `presentation.xml`, slide masters, layouts, and
//! slides. Parse-only (no layout). The shape tree is modelled generically
//! enough for the pptx layout engine.

use super::theme::{ColorRef, ColorTransform, Theme, parse_color_element};
use super::{
    A, MC, Node, P, R, alternate_content_fallback, attr, child, children, emu_to_pt, is, parse_f64,
    parse_i64,
};
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

const DSP: &str = "http://schemas.microsoft.com/office/drawing/2008/diagram";

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
        const MAX_SLIDES: usize = 2_000;
        let doc = super::parse_xml(xml, "presentation.xml")?;
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
                    if pres.slide_rel_ids.len() >= MAX_SLIDES {
                        return Err(OfficeError::unsupported(
                            crate::UnsupportedCode::DecompressionLimit,
                            format!("slide count exceeds limit {MAX_SLIDES}"),
                        ));
                    }
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
    /// A recognized fill that must fail closed.
    Unsupported(FillUnsupported),
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
    /// A DrawingML pattern fill.
    Pattern {
        /// Preset pattern name.
        preset: String,
        /// Foreground color.
        foreground: ColorSpec,
        /// Background color.
        background: ColorSpec,
    },
    /// A picture fill.
    Picture(PictureFill),
}

/// A typed, structured fill parse failure. This is stored in the model so
/// profiling and direct rendering return the same stable reason.
#[derive(Debug, Clone)]
pub struct FillUnsupported {
    /// Stable typed refusal reason.
    pub code: UnsupportedCode,
    /// Context for diagnostics; never used to classify the failure.
    pub detail: String,
}

impl FillUnsupported {
    fn new(code: UnsupportedCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl Fill {
    /// Returns the first scheme color that the effective slide theme cannot resolve.
    pub fn unresolved_scheme<'a>(&'a self, theme: &Theme) -> Option<&'a str> {
        fn unresolved<'a>(color: &'a ColorSpec, theme: &Theme) -> Option<&'a str> {
            match &color.base {
                ColorRef::Scheme(slot) if theme.color(slot).is_none() => Some(slot),
                _ => None,
            }
        }
        match self {
            Fill::Solid(color) => unresolved(color, theme),
            Fill::LinearGradient { stops, .. } => {
                stops.iter().find_map(|(_, color)| unresolved(color, theme))
            }
            Fill::Pattern {
                foreground,
                background,
                ..
            } => unresolved(foreground, theme).or_else(|| unresolved(background, theme)),
            Fill::Unsupported(_) | Fill::None | Fill::Picture(_) => None,
        }
    }
}

/// Source crop percentages in DrawingML's 1/1000-percent integer units.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CropRect {
    /// Left crop.
    pub left: i32,
    /// Top crop.
    pub top: i32,
    /// Right crop.
    pub right: i32,
    /// Bottom crop.
    pub bottom: i32,
}

/// Picture-fill placement.
#[derive(Debug, Clone)]
pub enum PictureMode {
    /// Stretch to fill the target box.
    Stretch,
    /// Repeat a tile; scale values are percentages where 100_000 is 100%.
    Tile {
        /// Horizontal tile scale.
        scale_x: f64,
        /// Vertical tile scale.
        scale_y: f64,
    },
}

/// A resolved-at-render-time picture fill.
#[derive(Debug, Clone)]
pub struct PictureFill {
    /// Image relationship id.
    pub rel_id: String,
    /// Optional source crop.
    pub crop: Option<CropRect>,
    /// Stretch or tile placement.
    pub mode: PictureMode,
}

/// One DrawingML guide formula.
#[derive(Debug, Clone)]
pub struct GeometryGuide {
    /// Guide name.
    pub name: String,
    /// Formula tokens.
    pub formula: String,
}

/// A coordinate used by custom geometry commands.
#[derive(Debug, Clone)]
pub struct GeometryPoint {
    /// X expression.
    pub x: String,
    /// Y expression.
    pub y: String,
}

/// One custom-geometry path command.
#[derive(Debug, Clone)]
pub enum GeometryCommand {
    /// Move current point.
    Move(GeometryPoint),
    /// Straight segment.
    Line(GeometryPoint),
    /// Cubic Bézier segment.
    Cubic(GeometryPoint, GeometryPoint, GeometryPoint),
    /// Quadratic Bézier segment.
    Quad(GeometryPoint, GeometryPoint),
    /// Elliptic arc.
    Arc {
        /// Horizontal radius expression.
        width_radius: String,
        /// Vertical radius expression.
        height_radius: String,
        /// Start angle expression (1/60,000 degree).
        start_angle: String,
        /// Sweep angle expression (1/60,000 degree).
        sweep_angle: String,
    },
    /// Close current contour.
    Close,
}

/// One custom-geometry path.
#[derive(Debug, Clone, Default)]
pub struct GeometryPath {
    /// Coordinate-space width.
    pub width: Option<f64>,
    /// Coordinate-space height.
    pub height: Option<f64>,
    /// Commands.
    pub commands: Vec<GeometryCommand>,
    /// Recognized path children outside implemented command subset.
    pub unsupported: Vec<String>,
}

/// Parsed `a:custGeom`.
#[derive(Debug, Clone, Default)]
pub struct CustomGeometry {
    /// Guide formulas.
    pub guides: Vec<GeometryGuide>,
    /// Paths.
    pub paths: Vec<GeometryPath>,
}

/// Parsed preset geometry and adjustment formulas.
#[derive(Debug, Clone, Default)]
pub struct PresetGeometry {
    /// Preset name.
    pub name: String,
    /// Adjustment formulas keyed by guide name.
    pub adjustments: Vec<GeometryGuide>,
}

/// Shape line properties.
#[derive(Debug, Clone)]
pub struct LineSpec {
    /// Stroke color.
    pub color: ColorSpec,
    /// Width in points.
    pub width_pt: f64,
    /// Optional head arrow type.
    pub head_end: Option<String>,
    /// Optional tail arrow type.
    pub tail_end: Option<String>,
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
    pub preset: Option<PresetGeometry>,
    /// Custom geometry.
    pub custom_geom: Option<CustomGeometry>,
    /// Fill.
    pub fill: Option<Fill>,
    /// Line (outline) fill + width in points.
    pub line: Option<LineSpec>,
    /// Preset text-warp name.
    pub text_warp: Option<String>,
    /// Visible properties recognized but not renderable.
    pub unsupported: Vec<String>,
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
    /// Visible run properties recognized but not renderable.
    pub unsupported: Vec<String>,
}

/// A text-body paragraph.
#[derive(Debug, Clone, Default)]
pub struct TextParagraph {
    /// Indent level (0-based).
    pub level: u8,
    /// Alignment (`l`/`ctr`/`r`/`just`).
    pub align: Option<String>,
    /// Left margin in points (`a:pPr @marL`).
    pub mar_l_pt: Option<f64>,
    /// First-line indent in points (`a:pPr @indent`; negative = hanging).
    pub indent_pt: Option<f64>,
    /// Space before this paragraph.
    pub space_before: Option<TextSpacing>,
    /// Space after this paragraph.
    pub space_after: Option<TextSpacing>,
    /// Right-to-left paragraph base direction.
    pub rtl: bool,
    /// Whether a bullet is suppressed (`a:buNone`).
    pub bullet_none: bool,
    /// A literal bullet char (`a:buChar @char`).
    pub bullet_char: Option<String>,
    /// Automatic numbering `(format, start)` from `a:buAutoNum`.
    pub bullet_auto_num: Option<(String, u32)>,
    /// Runs.
    pub runs: Vec<TextRun>,
    /// Visible paragraph constructs recognized but not renderable.
    pub unsupported: Vec<String>,
}

/// A bullet declaration inherited from an `a:lstStyle` level.
#[derive(Debug, Clone)]
pub enum TextBulletStyle {
    /// Explicitly suppress bullets.
    None,
    /// Literal bullet character.
    Character(String),
    /// Automatic numbering format and start value.
    AutoNumber(String, u32),
}

/// DrawingML paragraph spacing, either relative to the paragraph line box or
/// an absolute point value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextSpacing {
    /// A fraction of the paragraph line box (`a:spcPct`, 100000 = 100%).
    Percent(f64),
    /// Absolute points (`a:spcPts`, hundredths of a point in the XML).
    Points(f64),
}

/// One paragraph level from `a:lstStyle`.
#[derive(Debug, Clone, Default)]
pub struct TextListStyleLevel {
    /// Paragraph alignment.
    pub align: Option<String>,
    /// Left margin in points (`@marL`).
    pub mar_l_pt: Option<f64>,
    /// First-line indent in points (`@indent`; negative = hanging).
    pub indent_pt: Option<f64>,
    /// Space before paragraphs at this level.
    pub space_before: Option<TextSpacing>,
    /// Space after paragraphs at this level.
    pub space_after: Option<TextSpacing>,
    /// Bullet declaration.
    pub bullet: Option<TextBulletStyle>,
    /// Default run properties for this level.
    pub default_run: TextRun,
}

/// A text body (`p:txBody`).
#[derive(Debug, Clone, Default)]
pub struct TextBody {
    /// Paragraphs.
    pub paragraphs: Vec<TextParagraph>,
    /// Level styles (0–8) declared by `a:lstStyle`.
    pub list_style: Vec<Option<TextListStyleLevel>>,
    /// Vertical anchor (`t`/`ctr`/`b`).
    pub anchor: Option<String>,
    /// Autofit font scale (thousandths of a percent) if `normAutofit`.
    pub font_scale: Option<f64>,
    /// Explicit autofit choice: normalized (`true`), disabled (`false`), or
    /// inherited (`None`).
    #[doc(hidden)]
    pub norm_autofit: Option<bool>,
    /// Left/right inset in points.
    pub inset_lr: (f64, f64),
    /// Top/bottom inset in points.
    pub inset_tb: (f64, f64),
    /// Which left/right inset values were explicitly authored.
    #[doc(hidden)]
    pub inset_lr_explicit: (bool, bool),
    /// Which top/bottom inset values were explicitly authored.
    #[doc(hidden)]
    pub inset_tb_explicit: (bool, bool),
    /// Visible body constructs recognized but not renderable.
    pub unsupported: Vec<String>,
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
    /// A relationship-backed DrawingML chart frame.
    Chart {
        /// Frame transform.
        xfrm: Option<Xfrm>,
        /// Relationship id of the chart part.
        rel_id: String,
    },
    /// A SmartArt frame whose data part may resolve to a pre-rendered drawing.
    SmartArt {
        /// Frame transform.
        xfrm: Option<Xfrm>,
        /// Relationship id of the diagram data part.
        data_rel_id: String,
    },
    /// An OLE frame represented by its embedded raster preview.
    OlePreview {
        /// Frame transform.
        xfrm: Option<Xfrm>,
        /// Relationship id of the preview image.
        preview_rel_id: String,
    },
    /// An unsupported graphic-frame (chart/diagram/ole) — carries a reason detail.
    Unsupported(String),
}

/// A minimal picture reference.
#[derive(Debug, Clone, Default)]
pub struct PictureRef {
    /// Blip relationship id.
    pub blip_rel_id: Option<String>,
    /// Source crop.
    pub crop: Option<CropRect>,
    /// Typed parse failure for a visible picture source/placement.
    pub unsupported: Option<FillUnsupported>,
}

/// A PPTX table (`a:tbl`).
#[derive(Debug, Clone, Default)]
pub struct PmlTable {
    /// Column widths in points.
    pub col_widths: Vec<f64>,
    /// Rows.
    pub rows: Vec<PmlRow>,
    /// Referenced table-style id, when present.
    pub style_id: Option<String>,
    /// Table-wide background fill from `a:tblPr`.
    pub fill: Option<Fill>,
    /// Emphasize first row.
    pub first_row: bool,
    /// Emphasize last row.
    pub last_row: bool,
    /// Alternate row bands.
    pub band_rows: bool,
    /// Emphasize first column.
    pub first_col: bool,
    /// Emphasize last column.
    pub last_col: bool,
    /// Alternate column bands.
    pub band_cols: bool,
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
    /// Vertical span.
    pub row_span: u32,
    /// Whether merged vertically into a cell above.
    pub v_merge: bool,
    /// Cell text.
    pub text: Option<TextBody>,
    /// Fill.
    pub fill: Option<Fill>,
    /// Top/right/bottom/left borders.
    pub borders: [Option<LineSpec>; 4],
    /// Whether each top/right/bottom/left border was explicitly specified;
    /// distinguishes inherited borders from an explicit `a:noFill`.
    pub border_specified: [bool; 4],
    /// Visible cell properties outside implemented subset.
    pub unsupported: Vec<String>,
    /// Left/right text insets in points.
    pub inset_lr: (f64, f64),
    /// Top/bottom text insets in points.
    pub inset_tb: (f64, f64),
    /// Vertical text anchor.
    pub anchor: Option<String>,
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
    /// Color-map aliases declared by master or override parts.
    pub color_map: HashMap<String, String>,
    /// Layout/slide color-map override semantics.
    pub color_map_override: ColorMapOverride,
    /// Whether master static shapes should be shown (`showMasterSp`, default true).
    pub show_master_shapes: Option<bool>,
}

/// The `p:clrMapOvr` choice on a layout or slide.
#[derive(Debug, Clone, Default)]
pub enum ColorMapOverride {
    /// No override, or explicit `a:masterClrMapping` inheritance.
    #[default]
    Inherit,
    /// Explicit `a:overrideClrMapping` values.
    Override(HashMap<String, String>),
}

impl Slide {
    /// Parses a slide/layout/master part (`p:sld`/`p:sldLayout`/`p:sldMaster`).
    pub fn parse(xml: &str) -> Result<Slide> {
        let doc = super::parse_xml(xml, "slide")?;
        let root = doc.root_element();
        let mut slide = Slide {
            color_map: child(root, P, "clrMap")
                .map(parse_color_mapping)
                .unwrap_or_default(),
            color_map_override: parse_color_map_override(root),
            show_master_shapes: attr(root, P, "showMasterSp").map(xml_bool_value),
            ..Slide::default()
        };
        let Some(csld) = child(root, P, "cSld") else {
            return Ok(slide);
        };
        if let Some(bg) = child(csld, P, "bg") {
            slide.background = parse_bg(bg);
        }
        if let Some(tree) = child(csld, P, "spTree") {
            for node in children(tree) {
                if let Some(elem) = parse_shape_elem(node, 0)? {
                    slide.shapes.push(elem);
                }
            }
        }
        Ok(slide)
    }
}

fn parse_color_map_override(root: Node) -> ColorMapOverride {
    let Some(override_node) = child(root, P, "clrMapOvr") else {
        return ColorMapOverride::Inherit;
    };
    child(override_node, A, "overrideClrMapping")
        .map(parse_color_mapping)
        .map(ColorMapOverride::Override)
        .unwrap_or(ColorMapOverride::Inherit)
}

fn parse_color_mapping(mapping: Node) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for name in [
        "bg1", "tx1", "bg2", "tx2", "accent1", "accent2", "accent3", "accent4", "accent5",
        "accent6", "hlink", "folHlink",
    ] {
        if let Some(value) = attr(mapping, A, name).or_else(|| attr(mapping, P, name)) {
            out.insert(name.to_string(), value.to_string());
        }
    }
    out
}

fn parse_bg(bg: Node) -> Option<Fill> {
    if let Some(pr) = child(bg, P, "bgPr") {
        return Some(match parse_fill_children(pr) {
            Ok(Some(fill)) => fill,
            Ok(None) => Fill::Unsupported(FillUnsupported::new(
                UnsupportedCode::InvalidFill,
                "p:bg/p:bgPr has no fill",
            )),
            Err(error) => Fill::Unsupported(error),
        });
    }
    child(bg, P, "bgRef").map(|background_ref| {
        parse_fill_color(background_ref, UnsupportedCode::InvalidFill, "p:bg/p:bgRef")
            .map(Fill::Solid)
            .unwrap_or_else(Fill::Unsupported)
    })
}

const MAX_GROUP_NESTING: usize = 32;

fn parse_shape_elem(node: Node, depth: usize) -> Result<Option<ShapeElem>> {
    if is(node, MC, "AlternateContent") {
        let mut fallback = Vec::new();
        for child in alternate_content_fallback(node)? {
            if let Some(elem) = parse_shape_elem(child, depth)? {
                fallback.push(elem);
            }
        }
        return Ok(match fallback.len() {
            0 => None,
            1 => fallback.pop(),
            _ => Some(ShapeElem::Group {
                xfrm: None,
                child_space: None,
                children: fallback,
            }),
        });
    }
    Ok(match node.tag_name().name() {
        "sp" => Some(parse_sp(node)),
        "pic" => Some(parse_pic(node)),
        "grpSp" => Some(parse_grp(node, depth + 1)?),
        "graphicFrame" => Some(parse_graphic_frame(node)),
        "cxnSp" => Some(parse_sp(node)), // connector: treat as a shape with a line
        "nvGrpSpPr" | "grpSpPr" => None,
        other => Some(ShapeElem::Unsupported(format!("shape-tree:{other}"))),
    })
}

fn parse_sp(node: Node) -> ShapeElem {
    let ph = child(node, P, "nvSpPr")
        .and_then(|nv| child(nv, P, "nvPr"))
        .and_then(|nvpr| child(nvpr, P, "ph"))
        .map(parse_ph);
    let mut props = child(node, P, "spPr").map(parse_sp_pr).unwrap_or_default();
    if child(node, P, "style").is_some() {
        props.unsupported.push("p:style theme refs".to_string());
    }
    if child(node, P, "nvSpPr").is_some_and(|nv| {
        nv.descendants()
            .any(|descendant| is(descendant, A, "hlinkClick"))
    }) {
        props.unsupported.push("shape-level hyperlink".to_string());
    }
    let text = child(node, P, "txBody").map(parse_txbody);
    props.text_warp = child(node, P, "txBody")
        .and_then(|body| child(body, A, "bodyPr"))
        .and_then(|body_pr| child(body_pr, A, "prstTxWarp"))
        .and_then(|warp| attr(warp, A, "prst"))
        .map(str::to_string);
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
            "prstGeom" => props.preset = Some(parse_preset_geometry(c)),
            "custGeom" => props.custom_geom = Some(parse_custom_geometry(c)),
            "solidFill" | "gradFill" | "blipFill" | "noFill" | "pattFill" => {
                props.fill = Some(if props.fill.is_some() {
                    Fill::Unsupported(FillUnsupported::new(
                        UnsupportedCode::InvalidFill,
                        "a:spPr declares more than one fill",
                    ))
                } else {
                    parse_one_fill(c).unwrap_or_else(Fill::Unsupported)
                });
            }
            "ln" => {
                props.line = parse_line(c);
                if props.line.is_none() && child(c, A, "noFill").is_none() {
                    props.unsupported.push("a:ln without solidFill".to_string());
                }
                if line_has_unsupported_style(c) {
                    props
                        .unsupported
                        .push("a:ln dash/compound/join".to_string());
                }
            }
            "effectLst" | "effectDag" | "scene3d" | "sp3d" | "grpFill" => {
                props.unsupported.push(format!("a:{}", c.tag_name().name()));
            }
            _ => {}
        }
    }
    props
}

fn xml_bool(value: Option<&str>) -> bool {
    value.is_some_and(xml_bool_value)
}

fn xml_bool_value(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true")
}

pub(crate) fn parse_line(node: Node) -> Option<LineSpec> {
    let width = parse_i64(attr(node, A, "w")).map(emu_to_pt).unwrap_or(1.0);
    let color = child(node, A, "solidFill").and_then(|sf| color_from_solid(sf))?;
    Some(LineSpec {
        color,
        width_pt: width,
        head_end: child(node, A, "headEnd")
            .and_then(|end| attr(end, A, "type"))
            .filter(|kind| *kind != "none")
            .map(str::to_string),
        tail_end: child(node, A, "tailEnd")
            .and_then(|end| attr(end, A, "type"))
            .filter(|kind| *kind != "none")
            .map(str::to_string),
    })
}

pub(crate) fn line_has_unsupported_style(node: Node) -> bool {
    attr(node, A, "cmpd").is_some()
        || attr(node, A, "cap").is_some()
        || child(node, A, "prstDash").is_some()
        || child(node, A, "custDash").is_some()
        || child(node, A, "round").is_some()
        || child(node, A, "bevel").is_some()
        || child(node, A, "miter").is_some()
}

type FillParseResult<T> = std::result::Result<T, FillUnsupported>;

pub(crate) fn parse_fill_children(node: Node) -> FillParseResult<Option<Fill>> {
    let mut parsed = None;
    for fill_node in children(node).filter(|child| {
        matches!(
            child.tag_name().name(),
            "noFill" | "solidFill" | "gradFill" | "pattFill" | "blipFill"
        )
    }) {
        if parsed.is_some() {
            return Err(FillUnsupported::new(
                UnsupportedCode::InvalidFill,
                "fill container declares more than one fill",
            ));
        }
        parsed = Some(parse_one_fill(fill_node)?);
    }
    Ok(parsed)
}

fn parse_one_fill(node: Node) -> FillParseResult<Fill> {
    match node.tag_name().name() {
        "noFill" => Ok(Fill::None),
        "solidFill" => {
            parse_fill_color(node, UnsupportedCode::InvalidFill, "a:solidFill").map(Fill::Solid)
        }
        "gradFill" => parse_gradient(node),
        "pattFill" => parse_pattern_fill(node),
        "blipFill" => parse_picture_fill(node).map(Fill::Picture),
        other => Err(FillUnsupported::new(
            UnsupportedCode::InvalidFill,
            format!("unsupported fill a:{other}"),
        )),
    }
}

fn parse_pattern_fill(node: Node) -> FillParseResult<Fill> {
    let preset = attr(node, A, "prst").ok_or_else(|| {
        FillUnsupported::new(UnsupportedCode::PatternFill, "a:pattFill is missing prst")
    })?;
    if !supports_pattern(preset) {
        return Err(FillUnsupported::new(
            UnsupportedCode::PatternFill,
            format!("unsupported a:pattFill preset {preset}"),
        ));
    }
    let foreground_node = child(node, A, "fgClr").ok_or_else(|| {
        FillUnsupported::new(
            UnsupportedCode::PatternFill,
            "a:pattFill is missing a:fgClr",
        )
    })?;
    let background_node = child(node, A, "bgClr").ok_or_else(|| {
        FillUnsupported::new(
            UnsupportedCode::PatternFill,
            "a:pattFill is missing a:bgClr",
        )
    })?;
    let foreground = parse_fill_color(
        foreground_node,
        UnsupportedCode::PatternFill,
        "a:pattFill/a:fgClr",
    )?;
    let background = parse_fill_color(
        background_node,
        UnsupportedCode::PatternFill,
        "a:pattFill/a:bgClr",
    )?;
    Ok(Fill::Pattern {
        preset: preset.to_string(),
        foreground,
        background,
    })
}

fn parse_picture_fill(node: Node) -> FillParseResult<PictureFill> {
    let blip = child(node, A, "blip").ok_or_else(|| {
        FillUnsupported::new(
            UnsupportedCode::MissingRelationshipTarget,
            "a:blipFill is missing a:blip",
        )
    })?;
    if let Some(link) = attr(blip, R, "link") {
        return Err(FillUnsupported::new(
            UnsupportedCode::ExternalImage,
            format!("external image relationship {link} is not fetched"),
        ));
    }
    let rel_id = attr(blip, R, "embed")
        .filter(|rel_id| !rel_id.is_empty())
        .ok_or_else(|| {
            FillUnsupported::new(
                UnsupportedCode::MissingRelationshipTarget,
                "a:blipFill/a:blip is missing r:embed",
            )
        })?
        .to_string();
    if children(blip).next().is_some() {
        return Err(FillUnsupported::new(
            UnsupportedCode::InvalidPictureFill,
            "a:blip effects are unsupported",
        ));
    }
    let crop = child(node, A, "srcRect").map(parse_crop);
    let tile = child(node, A, "tile");
    let stretch = child(node, A, "stretch");
    let mode = match (tile, stretch) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(FillUnsupported::new(
                UnsupportedCode::InvalidPictureFill,
                "a:blipFill must declare exactly one of a:tile or a:stretch",
            ));
        }
        (Some(tile), None) => {
            if ["tx", "ty", "algn", "flip"]
                .iter()
                .any(|name| attr(tile, A, name).is_some())
            {
                return Err(FillUnsupported::new(
                    UnsupportedCode::InvalidPictureFill,
                    "a:tile offset/alignment/flip is unsupported",
                ));
            }
            let scale = |name| -> FillParseResult<f64> {
                let value = match attr(tile, A, name) {
                    Some(value) => value.parse::<f64>().ok(),
                    None => Some(100_000.0),
                }
                .filter(|value| value.is_finite() && *value > 0.0)
                .ok_or_else(|| {
                    FillUnsupported::new(
                        UnsupportedCode::InvalidPictureFill,
                        format!("a:tile@{name} must be a positive finite percentage"),
                    )
                })?;
                Ok(value / 100_000.0)
            };
            PictureMode::Tile {
                scale_x: scale("sx")?,
                scale_y: scale("sy")?,
            }
        }
        (None, Some(stretch)) => {
            let fill_rect = child(stretch, A, "fillRect").ok_or_else(|| {
                FillUnsupported::new(
                    UnsupportedCode::InvalidPictureFill,
                    "a:stretch is missing a:fillRect",
                )
            })?;
            if ["l", "t", "r", "b"]
                .iter()
                .any(|name| attr(fill_rect, A, name).is_some_and(|value| value != "0"))
            {
                return Err(FillUnsupported::new(
                    UnsupportedCode::InvalidPictureFill,
                    "non-zero a:stretch/a:fillRect offsets are unsupported",
                ));
            }
            PictureMode::Stretch
        }
    };
    for child in children(node) {
        if !matches!(
            child.tag_name().name(),
            "blip" | "srcRect" | "tile" | "stretch"
        ) {
            return Err(FillUnsupported::new(
                UnsupportedCode::InvalidPictureFill,
                format!("unsupported a:blipFill/a:{}", child.tag_name().name()),
            ));
        }
    }
    Ok(PictureFill { rel_id, crop, mode })
}

fn parse_crop(node: Node) -> CropRect {
    let value = |name| match attr(node, A, name) {
        Some(value) => value
            .parse::<i64>()
            .ok()
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(i32::MIN),
        None => 0,
    };
    CropRect {
        left: value("l"),
        top: value("t"),
        right: value("r"),
        bottom: value("b"),
    }
}

fn parse_preset_geometry(node: Node) -> PresetGeometry {
    PresetGeometry {
        name: attr(node, A, "prst").unwrap_or_default().to_string(),
        adjustments: child(node, A, "avLst")
            .into_iter()
            .flat_map(children)
            .filter_map(parse_guide)
            .collect(),
    }
}

fn parse_guide(node: Node) -> Option<GeometryGuide> {
    if !is(node, A, "gd") {
        return None;
    }
    Some(GeometryGuide {
        name: attr(node, A, "name")?.to_string(),
        formula: attr(node, A, "fmla")?.to_string(),
    })
}

fn parse_custom_geometry(node: Node) -> CustomGeometry {
    let guides = child(node, A, "gdLst")
        .into_iter()
        .flat_map(children)
        .filter_map(parse_guide)
        .collect();
    let paths = child(node, A, "pathLst")
        .into_iter()
        .flat_map(children)
        .filter(|path| is(*path, A, "path"))
        .map(parse_geometry_path)
        .collect();
    CustomGeometry { guides, paths }
}

fn parse_geometry_path(node: Node) -> GeometryPath {
    let mut path = GeometryPath {
        width: parse_f64(attr(node, A, "w")),
        height: parse_f64(attr(node, A, "h")),
        commands: Vec::new(),
        unsupported: Vec::new(),
    };
    for command in children(node) {
        let points: Vec<GeometryPoint> = children(command)
            .filter(|point| is(*point, A, "pt"))
            .filter_map(|point| {
                Some(GeometryPoint {
                    x: attr(point, A, "x")?.to_string(),
                    y: attr(point, A, "y")?.to_string(),
                })
            })
            .collect();
        let parsed = match command.tag_name().name() {
            "moveTo" => points.first().cloned().map(GeometryCommand::Move),
            "lnTo" => points.first().cloned().map(GeometryCommand::Line),
            "cubicBezTo" if points.len() == 3 => Some(GeometryCommand::Cubic(
                points[0].clone(),
                points[1].clone(),
                points[2].clone(),
            )),
            "quadBezTo" if points.len() == 2 => {
                Some(GeometryCommand::Quad(points[0].clone(), points[1].clone()))
            }
            "arcTo" => Some(GeometryCommand::Arc {
                width_radius: attr(command, A, "wR").unwrap_or("0").to_string(),
                height_radius: attr(command, A, "hR").unwrap_or("0").to_string(),
                start_angle: attr(command, A, "stAng").unwrap_or("0").to_string(),
                sweep_angle: attr(command, A, "swAng").unwrap_or("0").to_string(),
            }),
            "close" => Some(GeometryCommand::Close),
            other => {
                path.unsupported.push(format!("a:custGeom/a:path/{other}"));
                None
            }
        };
        if let Some(parsed) = parsed {
            path.commands.push(parsed);
        }
    }
    path
}

pub(crate) fn color_from_solid(node: Node) -> Option<ColorSpec> {
    children(node).find_map(|c| {
        parse_color_element(c).map(|(base, transforms)| ColorSpec { base, transforms })
    })
}

fn parse_fill_color(
    node: Node,
    code: UnsupportedCode,
    context: &str,
) -> FillParseResult<ColorSpec> {
    let mut color_nodes = children(node);
    let color = color_nodes
        .next()
        .ok_or_else(|| FillUnsupported::new(code, format!("{context} has no color element")))?;
    if color_nodes.next().is_some() {
        return Err(FillUnsupported::new(
            code,
            format!("{context} has more than one color element"),
        ));
    }
    let Some((base, transforms)) = parse_color_element(color) else {
        return Err(FillUnsupported::new(
            code,
            format!(
                "{context} has invalid or unsupported a:{}",
                color.tag_name().name()
            ),
        ));
    };
    for transform in children(color) {
        if !matches!(
            transform.tag_name().name(),
            "tint" | "shade" | "lumMod" | "lumOff" | "satMod" | "alpha"
        ) {
            return Err(FillUnsupported::new(
                code,
                format!(
                    "{context} has unsupported color transform a:{}",
                    transform.tag_name().name()
                ),
            ));
        }
        let valid = attr(transform, A, "val")
            .and_then(|value| value.parse::<f64>().ok())
            .is_some_and(|value| value.is_finite() && (0.0..=100_000.0).contains(&value));
        if !valid {
            return Err(FillUnsupported::new(
                code,
                format!(
                    "{context} has invalid a:{}@val",
                    transform.tag_name().name()
                ),
            ));
        }
    }
    Ok(ColorSpec { base, transforms })
}

fn parse_gradient(node: Node) -> FillParseResult<Fill> {
    if child(node, A, "path").is_some() {
        return Err(FillUnsupported::new(
            UnsupportedCode::GradientFill,
            "path gradients are outside the rendered gradient envelope",
        ));
    }
    let list = child(node, A, "gsLst").ok_or_else(|| {
        FillUnsupported::new(
            UnsupportedCode::GradientFill,
            "a:gradFill is missing a:gsLst",
        )
    })?;
    let mut stops = Vec::new();
    for gs in children(list) {
        if !is(gs, A, "gs") {
            return Err(FillUnsupported::new(
                UnsupportedCode::GradientFill,
                format!("unsupported a:gsLst/a:{}", gs.tag_name().name()),
            ));
        }
        let pos = parse_f64(attr(gs, A, "pos"))
            .filter(|position| position.is_finite() && (0.0..=100_000.0).contains(position))
            .ok_or_else(|| {
                FillUnsupported::new(
                    UnsupportedCode::GradientFill,
                    "a:gs has an invalid or missing position",
                )
            })?
            / 100_000.0;
        let color = parse_fill_color(gs, UnsupportedCode::GradientFill, "a:gradFill/a:gs")?;
        stops.push((pos, color));
    }
    if stops.len() < 2 {
        return Err(FillUnsupported::new(
            UnsupportedCode::GradientFill,
            "a:gradFill needs at least two color stops",
        ));
    }
    let (min_position, max_position) = stops
        .iter()
        .map(|(position, _)| *position)
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
            (min.min(value), max.max(value))
        });
    if (max_position - min_position).abs() <= f64::EPSILON {
        return Err(FillUnsupported::new(
            UnsupportedCode::GradientFill,
            "a:gradFill color-stop positions are degenerate",
        ));
    }
    let linear = child(node, A, "lin").ok_or_else(|| {
        FillUnsupported::new(
            UnsupportedCode::GradientFill,
            "a:gradFill has neither a:lin nor a:path geometry",
        )
    })?;
    let angle = match attr(linear, A, "ang") {
        Some(value) => value.parse::<f64>().ok(),
        None => Some(0.0),
    }
    .filter(|angle| angle.is_finite())
    .ok_or_else(|| {
        FillUnsupported::new(
            UnsupportedCode::GradientFill,
            "a:gradFill/a:lin has an invalid angle",
        )
    })? / 60_000.0;
    Ok(Fill::LinearGradient {
        angle_deg: angle,
        stops,
    })
}

pub(crate) fn supports_pattern(preset: &str) -> bool {
    matches!(
        preset,
        "pct5"
            | "pct10"
            | "pct20"
            | "pct25"
            | "pct30"
            | "pct40"
            | "pct50"
            | "pct60"
            | "pct70"
            | "pct75"
            | "pct80"
            | "pct90"
            | "horz"
            | "vert"
            | "ltHorz"
            | "ltVert"
            | "dkHorz"
            | "dkVert"
            | "narHorz"
            | "narVert"
            | "dashHorz"
            | "dashVert"
            | "cross"
            | "dnDiag"
            | "upDiag"
            | "ltDnDiag"
            | "ltUpDiag"
            | "dkDnDiag"
            | "dkUpDiag"
            | "wdDnDiag"
            | "wdUpDiag"
            | "dashDnDiag"
            | "dashUpDiag"
            | "diagCross"
            | "smCheck"
            | "lgCheck"
            | "smGrid"
            | "lgGrid"
            | "dotGrid"
            | "smConfetti"
            | "lgConfetti"
            | "horzBrick"
            | "diagBrick"
            | "solidDmnd"
            | "openDmnd"
            | "dotDmnd"
            | "plaid"
            | "sphere"
            | "weave"
            | "divot"
            | "shingle"
            | "wave"
            | "trellis"
            | "zigZag"
    )
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
                if let Some(anchor) = attr(c, A, "anchor") {
                    if matches!(anchor, "t" | "ctr" | "b") {
                        body.anchor = Some(anchor.to_string());
                    } else {
                        body.unsupported.push(format!("a:bodyPr@anchor={anchor}"));
                    }
                }
                for attribute in ["vert", "rot", "numCol", "spcCol", "wrap"] {
                    if let Some(value) = attr(c, A, attribute) {
                        let neutral = matches!(
                            (attribute, value),
                            ("vert", "horz")
                                | ("rot", "0")
                                | ("numCol", "1")
                                | ("spcCol", "0")
                                | ("wrap", "square")
                        );
                        if !neutral {
                            body.unsupported
                                .push(format!("a:bodyPr@{attribute}={value}"));
                        }
                    }
                }
                parse_text_body_inset(
                    c,
                    "lIns",
                    &mut body.inset_lr.0,
                    &mut body.inset_lr_explicit.0,
                    &mut body.unsupported,
                );
                parse_text_body_inset(
                    c,
                    "rIns",
                    &mut body.inset_lr.1,
                    &mut body.inset_lr_explicit.1,
                    &mut body.unsupported,
                );
                parse_text_body_inset(
                    c,
                    "tIns",
                    &mut body.inset_tb.0,
                    &mut body.inset_tb_explicit.0,
                    &mut body.unsupported,
                );
                parse_text_body_inset(
                    c,
                    "bIns",
                    &mut body.inset_tb.1,
                    &mut body.inset_tb_explicit.1,
                    &mut body.unsupported,
                );
                let normalized = child(c, A, "normAutofit");
                let disabled = child(c, A, "noAutofit");
                if normalized.is_some() && disabled.is_some() {
                    body.unsupported
                        .push("a:bodyPr/a:autofit-choice".to_string());
                }
                if let Some(na) = normalized {
                    body.norm_autofit = Some(true);
                    if let Some(raw_scale) = attr(na, A, "fontScale") {
                        match parse_i64(Some(raw_scale))
                            .filter(|scale| (1_000..=100_000).contains(scale))
                        {
                            Some(scale) => body.font_scale = Some(scale as f64 / 100_000.0),
                            None => body
                                .unsupported
                                .push("a:bodyPr/a:normAutofit@fontScale".to_string()),
                        }
                    }
                    if attr(na, A, "lnSpcReduction")
                        .is_some_and(|raw| parse_i64(Some(raw)) != Some(0))
                    {
                        body.unsupported
                            .push("a:bodyPr/a:normAutofit@lnSpcReduction".to_string());
                    }
                } else if disabled.is_some() {
                    body.norm_autofit = Some(false);
                }
                for visible_child in ["spAutoFit", "scene3d", "flatTx"] {
                    if child(c, A, visible_child).is_some() {
                        body.unsupported.push(format!("a:bodyPr/a:{visible_child}"));
                    }
                }
            }
            "p" => body.paragraphs.push(parse_text_paragraph(c)),
            "lstStyle" => {
                let (levels, unsupported) = parse_list_style(c);
                body.list_style = levels;
                body.unsupported.extend(unsupported);
            }
            "extLst" => {}
            other => body.unsupported.push(format!("a:txBody/{other}")),
        }
    }
    body
}

fn parse_text_body_inset(
    body_pr: Node,
    name: &str,
    value: &mut f64,
    explicit: &mut bool,
    unsupported: &mut Vec<String>,
) {
    let Some(raw) = attr(body_pr, A, name) else {
        return;
    };
    match parse_i64(Some(raw))
        .and_then(|parsed| i32::try_from(parsed).ok())
        .filter(|parsed| *parsed >= 0)
    {
        Some(parsed) => {
            *value = emu_to_pt(i64::from(parsed));
            *explicit = true;
        }
        None => unsupported.push(format!("a:bodyPr@{name}={raw}")),
    }
}

fn parse_list_style(node: Node) -> (Vec<Option<TextListStyleLevel>>, Vec<String>) {
    let mut levels = vec![None; 9];
    let mut unsupported = Vec::new();
    for level_node in children(node) {
        let name = level_node.tag_name().name();
        let Some(level) = name
            .strip_prefix("lvl")
            .and_then(|value| value.strip_suffix("pPr"))
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| (1..=9).contains(value))
            .map(|value| value - 1)
        else {
            unsupported.push(format!("a:lstStyle/a:{name}"));
            continue;
        };
        let mut style = TextListStyleLevel {
            align: attr(level_node, A, "algn").map(str::to_string),
            mar_l_pt: parse_i64(attr(level_node, A, "marL")).map(emu_to_pt),
            indent_pt: parse_i64(attr(level_node, A, "indent")).map(emu_to_pt),
            ..TextListStyleLevel::default()
        };
        for property in children(level_node) {
            match property.tag_name().name() {
                "buNone" => style.bullet = Some(TextBulletStyle::None),
                "buChar" => {
                    style.bullet = attr(property, A, "char")
                        .map(str::to_string)
                        .map(TextBulletStyle::Character)
                }
                "buAutoNum" => {
                    style.bullet = attr(property, A, "type").map(|format| {
                        TextBulletStyle::AutoNumber(
                            format.to_string(),
                            parse_i64(attr(property, A, "startAt")).unwrap_or(1).max(1) as u32,
                        )
                    })
                }
                "spcBef" => match parse_text_spacing(property) {
                    Some(spacing) => style.space_before = Some(spacing),
                    None => unsupported.push(format!("a:lstStyle/a:{name}/a:spcBef")),
                },
                "spcAft" => match parse_text_spacing(property) {
                    Some(spacing) => style.space_after = Some(spacing),
                    None => unsupported.push(format!("a:lstStyle/a:{name}/a:spcAft")),
                },
                "defRPr" => parse_list_default_run(property, &mut style.default_run),
                "buClr" | "buClrTx" | "buFont" | "buFontTx" | "buSzPct" | "buSzPts" | "buSzTx"
                | "tabLst" => unsupported.push(format!(
                    "a:lstStyle/a:{name}/a:{}",
                    property.tag_name().name()
                )),
                other => unsupported.push(format!("a:lstStyle/a:{name}/a:{other}")),
            }
        }
        levels[level] = Some(style);
    }
    (levels, unsupported)
}

/// Parses a DrawingML `a:spcBef`/`a:spcAft` value. The schema values are
/// integers; parsing them as bounded `i64`s also prevents exponent-sized
/// untrusted values from reaching layout arithmetic. Negative, missing, or
/// structurally ambiguous values stay outside the supported envelope.
pub(crate) fn parse_text_spacing(node: Node) -> Option<TextSpacing> {
    let mut values = children(node);
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let parsed = parse_i64(attr(value, A, "val"))
        .and_then(|parsed| i32::try_from(parsed).ok())
        .filter(|value| *value >= 0)?;
    match (value.tag_name().namespace(), value.tag_name().name()) {
        (Some(A), "spcPct") => Some(TextSpacing::Percent(f64::from(parsed) / 100_000.0)),
        (Some(A), "spcPts") => Some(TextSpacing::Points(f64::from(parsed) / 100.0)),
        _ => None,
    }
}

fn parse_list_default_run(node: Node, run: &mut TextRun) {
    run.bold = attr(node, A, "b").map(xml_bool_value);
    run.italic = attr(node, A, "i").map(xml_bool_value);
    run.underline = attr(node, A, "u").is_some_and(|value| value != "none");
    run.size_pt = parse_f64(attr(node, A, "sz")).map(|size| size / 100.0);
    run.typeface = child(node, A, "latin")
        .and_then(|latin| attr(latin, A, "typeface"))
        .map(str::to_string);
    run.color = child(node, A, "solidFill").and_then(color_from_solid);
}

fn parse_text_paragraph(node: Node) -> TextParagraph {
    let mut para = TextParagraph::default();
    for c in children(node) {
        match c.tag_name().name() {
            "pPr" => {
                para.level = parse_i64(attr(c, A, "lvl")).unwrap_or(0).clamp(0, 8) as u8;
                para.align = attr(c, A, "algn").map(str::to_string);
                para.rtl = attr(c, A, "rtl").is_some_and(|value| value == "1" || value == "true");
                para.mar_l_pt = parse_i64(attr(c, A, "marL")).map(emu_to_pt);
                para.indent_pt = parse_i64(attr(c, A, "indent")).map(emu_to_pt);
                if parse_i64(attr(c, A, "defTabSz")).is_some_and(|value| value != 0) {
                    para.unsupported.push("a:pPr@defTabSz".to_string());
                }
                if attr(c, A, "fontAlgn").is_some() {
                    para.unsupported.push("a:pPr@fontAlgn".to_string());
                }
                for b in children(c) {
                    match b.tag_name().name() {
                        "buNone" => para.bullet_none = true,
                        "buChar" => para.bullet_char = attr(b, A, "char").map(str::to_string),
                        "buAutoNum" => {
                            para.bullet_auto_num = Some((
                                attr(b, A, "type").unwrap_or_default().to_string(),
                                parse_i64(attr(b, A, "startAt")).unwrap_or(1).max(1) as u32,
                            ));
                        }
                        "spcBef" => match parse_text_spacing(b) {
                            Some(spacing) => para.space_before = Some(spacing),
                            None => para.unsupported.push("a:pPr/a:spcBef".to_string()),
                        },
                        "spcAft" => match parse_text_spacing(b) {
                            Some(spacing) => para.space_after = Some(spacing),
                            None => para.unsupported.push("a:pPr/a:spcAft".to_string()),
                        },
                        "buBlip" => para.unsupported.push("a:buBlip".to_string()),
                        "extLst" => {}
                        other => para.unsupported.push(format!("a:pPr/a:{other}")),
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
                    let mut run = parse_text_run(c);
                    run.text = t.text().unwrap_or_default().to_string();
                    para.runs.push(run);
                } else {
                    para.unsupported
                        .push("a:fld without cached text".to_string());
                }
            }
            "endParaRPr" => {
                if c.attributes().len() > 0 || children(c).next().is_some() {
                    para.unsupported.push("a:endParaRPr".to_string());
                }
            }
            "extLst" => {}
            other => para.unsupported.push(format!("a:p/{other}")),
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
        if run.typeface.is_none() {
            run.typeface = ["ea", "cs"]
                .into_iter()
                .find_map(|name| child(rpr, A, name))
                .and_then(|font| attr(font, A, "typeface"))
                .filter(|typeface| !typeface.is_empty() && !typeface.starts_with('+'))
                .map(str::to_string);
        }
        run.color = child(rpr, A, "solidFill").and_then(color_from_solid);
        if let Some(hl) = child(rpr, A, "hlinkClick") {
            run.hlink = attr(hl, R, "id").map(str::to_string);
        }
        for visible in ["strike", "highlight", "spc", "baseline", "cap", "kern"] {
            if attr(rpr, A, visible).is_some() || child(rpr, A, visible).is_some() {
                run.unsupported.push(format!("a:rPr@{visible}"));
            }
        }
        for visible_child in ["gradFill", "pattFill", "blipFill", "ln", "effectLst"] {
            if child(rpr, A, visible_child).is_some() {
                run.unsupported.push(format!("a:rPr/a:{visible_child}"));
            }
        }
    }
    run
}

fn parse_pic(node: Node) -> ShapeElem {
    let props = child(node, P, "spPr").map(parse_sp_pr).unwrap_or_default();
    let mut drawing = PictureRef::default();
    if let Some(bf) = child(node, P, "blipFill") {
        match parse_picture_fill(bf) {
            Ok(picture) => {
                drawing.blip_rel_id = Some(picture.rel_id);
                drawing.crop = picture.crop;
            }
            Err(error) => drawing.unsupported = Some(error),
        }
    } else {
        drawing.unsupported = Some(FillUnsupported::new(
            UnsupportedCode::MissingRelationshipTarget,
            "p:pic is missing p:blipFill",
        ));
    }
    ShapeElem::Picture { props, drawing }
}

fn parse_grp(node: Node, depth: usize) -> Result<ShapeElem> {
    if depth > MAX_GROUP_NESTING {
        return Err(OfficeError::unsupported(
            crate::UnsupportedCode::DecompressionLimit,
            format!("DrawingML group nesting exceeds limit {MAX_GROUP_NESTING}"),
        ));
    }
    let grp_pr = child(node, P, "grpSpPr");
    let xfrm = grp_pr.and_then(|pr| child(pr, A, "xfrm")).map(Xfrm::parse);
    let child_space = grp_pr.and_then(|pr| child(pr, A, "xfrm")).and_then(|x| {
        let ch_off = child(x, A, "chOff").map(|node| Xfrm {
            x_pt: parse_i64(attr(node, A, "x")).map(emu_to_pt).unwrap_or(0.0),
            y_pt: parse_i64(attr(node, A, "y")).map(emu_to_pt).unwrap_or(0.0),
            present: true,
            ..Default::default()
        });
        let ch_ext = child(x, A, "chExt").map(|node| Xfrm {
            w_pt: parse_i64(attr(node, A, "cx")).map(emu_to_pt).unwrap_or(0.0),
            h_pt: parse_i64(attr(node, A, "cy")).map(emu_to_pt).unwrap_or(0.0),
            present: true,
            ..Default::default()
        });
        ch_off.zip(ch_ext)
    });
    let mut kids = Vec::new();
    for c in children(node) {
        if let Some(elem) = parse_shape_elem(c, depth)? {
            kids.push(elem);
        }
    }
    Ok(ShapeElem::Group {
        xfrm,
        child_space,
        children: kids,
    })
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
        data.descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().namespace()
                        == Some("http://schemas.openxmlformats.org/drawingml/2006/chart")
                    && node.tag_name().name() == "chart"
            })
            .and_then(|chart| attr(chart, R, "id"))
            .map(|rel_id| ShapeElem::Chart {
                xfrm,
                rel_id: rel_id.to_string(),
            })
            .unwrap_or_else(|| ShapeElem::Unsupported("chart missing relationship".into()))
    } else if uri.contains("diagram") {
        data.descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().namespace()
                        == Some("http://schemas.openxmlformats.org/drawingml/2006/diagram")
                    && node.tag_name().name() == "relIds"
            })
            .and_then(|rel_ids| attr(rel_ids, R, "dm"))
            .filter(|rel_id| !rel_id.is_empty())
            .map(|data_rel_id| ShapeElem::SmartArt {
                xfrm,
                data_rel_id: data_rel_id.to_string(),
            })
            .unwrap_or_else(|| ShapeElem::Unsupported("smartart missing data relationship".into()))
    } else if uri.contains("ole") {
        child(data, P, "oleObj")
            .and_then(|ole| child(ole, P, "pic"))
            .and_then(|picture| child(picture, P, "blipFill"))
            .and_then(|fill| child(fill, A, "blip"))
            .and_then(|blip| attr(blip, R, "embed"))
            .map(|preview_rel_id| ShapeElem::OlePreview {
                xfrm,
                preview_rel_id: preview_rel_id.to_string(),
            })
            .unwrap_or_else(|| ShapeElem::Unsupported("ole preview missing".into()))
    } else {
        ShapeElem::Unsupported(format!("graphic:{uri}"))
    }
}

/// Parses the pre-rendered `dsp:drawing` fallback carried by SmartArt.
pub fn parse_diagram_drawing(xml: &str) -> Result<Vec<ShapeElem>> {
    let doc = super::parse_xml(xml, "diagram drawing")?;
    let tree = doc
        .descendants()
        .find(|node| {
            node.is_element()
                && node.tag_name().namespace() == Some(DSP)
                && node.tag_name().name() == "spTree"
        })
        .ok_or_else(|| {
            OfficeError::unsupported(
                crate::UnsupportedCode::SmartArt,
                "diagram drawing has no dsp:spTree",
            )
        })?;
    let mut shapes = Vec::new();
    for node in children(tree) {
        if let Some(shape) = parse_diagram_shape(node, 0)? {
            shapes.push(shape);
        }
    }
    if shapes.is_empty() {
        return Err(OfficeError::unsupported(
            crate::UnsupportedCode::SmartArt,
            "diagram drawing fallback has no visible shapes",
        ));
    }
    Ok(shapes)
}

fn parse_diagram_shape(node: Node, depth: usize) -> Result<Option<ShapeElem>> {
    if node.tag_name().namespace() != Some(DSP) {
        return Ok(None);
    }
    Ok(match node.tag_name().name() {
        "sp" | "cxnSp" => {
            let props = child_local(node, "spPr")
                .map(parse_sp_pr)
                .unwrap_or_default();
            let text = child_local(node, "txBody").map(parse_txbody);
            Some(ShapeElem::Shape {
                ph: None,
                props,
                text,
            })
        }
        "grpSp" => {
            if depth >= MAX_GROUP_NESTING {
                return Err(OfficeError::unsupported(
                    crate::UnsupportedCode::DecompressionLimit,
                    format!("SmartArt group nesting exceeds limit {MAX_GROUP_NESTING}"),
                ));
            }
            let group_props = child_local(node, "grpSpPr");
            let xfrm_node = group_props.and_then(|props| child(props, A, "xfrm"));
            let xfrm = xfrm_node.map(Xfrm::parse);
            let child_space = xfrm_node.and_then(|xfrm| {
                let offset = child(xfrm, A, "chOff").map(|value| Xfrm {
                    x_pt: parse_i64(attr(value, A, "x")).map(emu_to_pt).unwrap_or(0.0),
                    y_pt: parse_i64(attr(value, A, "y")).map(emu_to_pt).unwrap_or(0.0),
                    present: true,
                    ..Xfrm::default()
                });
                let extent = child(xfrm, A, "chExt").map(|value| Xfrm {
                    w_pt: parse_i64(attr(value, A, "cx"))
                        .map(emu_to_pt)
                        .unwrap_or(0.0),
                    h_pt: parse_i64(attr(value, A, "cy"))
                        .map(emu_to_pt)
                        .unwrap_or(0.0),
                    present: true,
                    ..Xfrm::default()
                });
                offset.zip(extent)
            });
            let mut children_out = Vec::new();
            for child_node in children(node) {
                if let Some(child_shape) = parse_diagram_shape(child_node, depth + 1)? {
                    children_out.push(child_shape);
                }
            }
            Some(ShapeElem::Group {
                xfrm,
                child_space,
                children: children_out,
            })
        }
        "nvGrpSpPr" | "grpSpPr" => None,
        other => Some(ShapeElem::Unsupported(format!("smartart:dsp:{other}"))),
    })
}

fn child_local<'a>(node: Node<'a, 'a>, local: &str) -> Option<Node<'a, 'a>> {
    children(node).find(|child| child.tag_name().name() == local)
}

fn parse_table(tbl: Node) -> PmlTable {
    let mut table = PmlTable::default();
    for c in children(tbl) {
        match c.tag_name().name() {
            "tblPr" => {
                table.fill = match parse_fill_children(c) {
                    Ok(fill) => fill,
                    Err(error) => Some(Fill::Unsupported(error)),
                };
                table.first_row = xml_bool(attr(c, A, "firstRow"));
                table.last_row = xml_bool(attr(c, A, "lastRow"));
                table.band_rows = xml_bool(attr(c, A, "bandRow"));
                table.first_col = xml_bool(attr(c, A, "firstCol"));
                table.last_col = xml_bool(attr(c, A, "lastCol"));
                table.band_cols = xml_bool(attr(c, A, "bandCol"));
                table.style_id = child(c, A, "tableStyleId")
                    .and_then(|style| style.text())
                    .map(str::to_string);
            }
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
    let mut unsupported = Vec::new();
    let grid_span = parse_table_merge_span(tc, "gridSpan", &mut unsupported);
    let h_merge = parse_table_merge_flag(tc, "hMerge", &mut unsupported);
    let row_span = parse_table_merge_span(tc, "rowSpan", &mut unsupported);
    let v_merge = parse_table_merge_flag(tc, "vMerge", &mut unsupported);
    let mut cell = PmlCell {
        grid_span,
        h_merge,
        row_span,
        v_merge,
        text: child(tc, A, "txBody").map(parse_txbody),
        fill: None,
        borders: Default::default(),
        border_specified: [false; 4],
        unsupported,
        inset_lr: (7.2, 7.2),
        inset_tb: (3.6, 3.6),
        anchor: None,
    };
    if let Some(pr) = child(tc, A, "tcPr") {
        cell.fill = match parse_fill_children(pr) {
            Ok(fill) => fill,
            Err(error) => Some(Fill::Unsupported(error)),
        };
        if let Some(value) = parse_i64(attr(pr, A, "marL")) {
            cell.inset_lr.0 = emu_to_pt(value);
        }
        if let Some(value) = parse_i64(attr(pr, A, "marR")) {
            cell.inset_lr.1 = emu_to_pt(value);
        }
        if let Some(value) = parse_i64(attr(pr, A, "marT")) {
            cell.inset_tb.0 = emu_to_pt(value);
        }
        if let Some(value) = parse_i64(attr(pr, A, "marB")) {
            cell.inset_tb.1 = emu_to_pt(value);
        }
        cell.anchor = attr(pr, A, "anchor").map(str::to_string);
        for (index, name) in ["lnT", "lnR", "lnB", "lnL"].iter().enumerate() {
            if let Some(line) = child(pr, A, name) {
                cell.border_specified[index] = true;
                cell.borders[index] = parse_line(line);
                if cell.borders[index].is_none() && child(line, A, "noFill").is_none() {
                    cell.unsupported.push(format!("a:tcPr/a:{name}"));
                }
                if line_has_unsupported_style(line) {
                    cell.unsupported
                        .push(format!("a:tcPr/a:{name} dash/compound/join"));
                }
            }
        }
    }
    cell
}

fn parse_table_merge_span(tc: Node, name: &str, unsupported: &mut Vec<String>) -> u32 {
    let Some(raw) = attr(tc, A, name) else {
        return 1;
    };
    if let Some(value) = parse_i64(Some(raw))
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
    {
        value
    } else {
        unsupported.push(format!("a:tc@{name} malformed value"));
        1
    }
}

fn parse_table_merge_flag(tc: Node, name: &str, unsupported: &mut Vec<String>) -> bool {
    match attr(tc, A, name) {
        None | Some("0") | Some("false") => false,
        Some("1") | Some("true") => true,
        Some(_) => {
            unsupported.push(format!("a:tc@{name} malformed boolean"));
            false
        }
    }
}

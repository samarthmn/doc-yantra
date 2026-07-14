//! The layout engine (the moat): turns parsed OOXML models into positioned PDF
//! primitives. Layout emits ONLY the primitives in [`Item`]; the emitter has
//! zero OOXML knowledge. Coordinates here are top-left origin (`y` grows down);
//! the emitter flips to PDF's bottom-left origin.

pub mod assemble;
pub mod flow;
pub mod math;
pub mod numbering_layout;
pub mod objects;
pub mod page;
pub mod paragraph;
pub mod table;

pub use assemble::WordDoc;
pub use flow::layout_docx;

use crate::fonts::FaceId;
use crate::oxml::Rgba;

/// A fully laid-out document: pages plus the resources the emitter needs.
#[derive(Debug, Default)]
pub struct LaidOutDocument {
    /// Positioned pages, in order.
    pub pages: Vec<Page>,
    /// Decoded images referenced by [`ImageId`].
    pub images: Vec<EncodedImage>,
    /// Outline (bookmark) entries from heading paragraphs.
    pub outline: Vec<OutlineEntry>,
    /// `(created, modified)` PDF date strings for deterministic metadata.
    pub dates: DocDates,
}

/// PDF date strings (`D:YYYYMMDDHHmmSSZ`) for `CreationDate`/`ModDate`.
#[derive(Debug, Clone)]
pub struct DocDates {
    /// Creation date literal.
    pub created: String,
    /// Modification date literal.
    pub modified: String,
}

impl Default for DocDates {
    fn default() -> Self {
        // Fixed literal when the document declares no core dates (determinism).
        DocDates {
            created: "D:20000101000000Z".to_string(),
            modified: "D:20000101000000Z".to_string(),
        }
    }
}

/// One laid-out page.
#[derive(Debug, Default)]
pub struct Page {
    /// Page width in points.
    pub width_pt: f64,
    /// Page height in points.
    pub height_pt: f64,
    /// Positioned items in paint order.
    pub items: Vec<Positioned>,
}

/// An [`Item`] placed at a top-left-origin coordinate.
#[derive(Debug, Clone)]
pub struct Positioned {
    /// X from the page left edge.
    pub x_pt: f64,
    /// Y from the page top edge (glyph `y` is the baseline).
    pub y_pt: f64,
    /// The primitive.
    pub item: Item,
}

/// A layout primitive the emitter can render without OOXML knowledge.
#[derive(Debug, Clone)]
pub enum Item {
    /// An item transformed by an affine matrix in top-left page coordinates.
    Transform {
        /// Matrix `[a, b, c, d, e, f]`: `(x, y) -> (a*x+c*y+e, b*x+d*y+f)`.
        matrix: [f64; 6],
        /// Primitive to transform.
        item: Box<Item>,
    },
    /// A run of glyphs sharing a face, size, and color.
    Glyphs(GlyphRun),
    /// Invisible logical text for visible clusters owned by one wrapped line.
    SemanticText(SemanticRun),
    /// A filled/stroked rectangle (borders, shading, highlights).
    Rect {
        /// Width in points.
        w_pt: f64,
        /// Height in points (extends downward from `y`).
        h_pt: f64,
        /// Fill color, if any.
        fill: Option<Rgba>,
        /// Stroke, if any.
        stroke: Option<Stroke>,
    },
    /// A straight line from `(x, y)` to `(x + dx, y + dy)`.
    Line {
        /// Delta-x to the endpoint.
        dx_pt: f64,
        /// Delta-y to the endpoint.
        dy_pt: f64,
        /// Line stroke.
        stroke: Stroke,
    },
    /// A general vector path painted with native PDF path operators.
    Path {
        /// Commands in coordinates relative to the containing [`Positioned`].
        commands: Vec<PathCommand>,
        /// Fill/stroke operation applied after the path is constructed.
        paint: PathPaint,
    },
    /// A bounded group whose children are clipped to a vector path.
    Clip {
        /// Clip path in coordinates relative to the containing [`Positioned`].
        commands: Vec<PathCommand>,
        /// Winding rule used to establish the clipping region.
        fill_rule: FillRule,
        /// Children positioned relative to the containing [`Positioned`].
        items: Vec<Positioned>,
    },
    /// A raster image occupying `(w, h)` with top-left at `(x, y)`.
    Image {
        /// Width in points.
        w_pt: f64,
        /// Height in points.
        h_pt: f64,
        /// Index into [`LaidOutDocument::images`].
        image_id: ImageId,
    },
    /// A raster image with an OOXML source rectangle.
    CroppedImage {
        /// Width in points.
        w_pt: f64,
        /// Height in points.
        h_pt: f64,
        /// Index into [`LaidOutDocument::images`].
        image_id: ImageId,
        /// Cropped source fractions (`left, top, right, bottom`).
        crop: [f64; 4],
    },
    /// A clickable link rectangle (top-left at `(x, y)`).
    Link {
        /// Width in points.
        w_pt: f64,
        /// Height in points.
        h_pt: f64,
        /// The link destination.
        target: LinkTarget,
    },
}

/// One command in a general vector path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCommand {
    /// Starts a new subpath.
    MoveTo {
        /// Horizontal coordinate.
        x: f64,
        /// Vertical coordinate.
        y: f64,
    },
    /// Adds a straight segment.
    LineTo {
        /// Horizontal coordinate.
        x: f64,
        /// Vertical coordinate.
        y: f64,
    },
    /// Adds a cubic Bézier segment.
    CubicTo {
        /// First control-point x coordinate.
        x1: f64,
        /// First control-point y coordinate.
        y1: f64,
        /// Second control-point x coordinate.
        x2: f64,
        /// Second control-point y coordinate.
        y2: f64,
        /// End-point x coordinate.
        x: f64,
        /// End-point y coordinate.
        y: f64,
    },
    /// Closes the current subpath.
    Close,
}

/// PDF path winding rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    /// Nonzero winding rule.
    NonZero,
    /// Even-odd winding rule.
    EvenOdd,
}

/// Fill and stroke state for a vector path.
#[derive(Debug, Clone)]
pub struct PathPaint {
    /// Fill color, if the path is filled.
    pub fill: Option<Rgba>,
    /// Stroke style, if the path is stroked.
    pub stroke: Option<Stroke>,
    /// Winding rule used by fill operations.
    pub fill_rule: FillRule,
}

/// A run of positioned glyphs.
#[derive(Debug, Clone)]
pub struct GlyphRun {
    /// The face these glyphs are drawn with.
    pub face: FaceId,
    /// Font size in points.
    pub size_pt: f64,
    /// Fill color.
    pub color: Rgba,
    /// Explicit paragraph base direction. `None` uses Unicode first-strong
    /// detection; Office bidi containers set this to RTL.
    pub base_direction: Option<text_core::Direction>,
    /// Glyphs, flowing from `(x, y)` by each glyph's advance.
    pub glyphs: Vec<PlacedGlyph>,
    /// Underline decoration, if any.
    pub underline: Option<Decor>,
    /// Strikethrough decoration, if any.
    pub strike: Option<Decor>,
}

/// One line-local logical text run used for copy, search, and selection.
#[derive(Debug, Clone)]
pub struct SemanticRun {
    /// Face whose subset supplies the invisible semantic CIDs.
    pub face: FaceId,
    /// Font size in points.
    pub size_pt: f64,
    /// Office paragraph base direction, explicit at this layout boundary.
    pub base_direction: text_core::Direction,
    /// Logical clusters owned by this physical line, in source order.
    pub clusters: Vec<SemanticCluster>,
}

/// Complete logical source for one shaped cluster and its physical advance.
#[derive(Debug, Clone)]
pub struct SemanticCluster {
    /// Backing glyph in [`SemanticRun::face`] used for the invisible CID.
    pub gid: u16,
    /// Complete source scalar sequence, exactly once.
    pub source: String,
    /// Actual line-local advance after justification.
    pub advance_pt: f64,
}

/// A single glyph within a [`GlyphRun`].
#[derive(Debug, Clone, Copy)]
pub struct PlacedGlyph {
    /// Original glyph id in the face (remapped to a CID at emit time).
    pub gid: u16,
    /// Advance width in points (kerning + spacing already applied).
    pub advance_pt: f64,
    /// Horizontal GPOS offset from the run cursor in points.
    pub x_offset_pt: f64,
    /// Vertical GPOS offset from the baseline in points (positive is up).
    pub y_offset_pt: f64,
    /// The source character (for ToUnicode).
    pub ch: char,
}

/// A stroke style.
#[derive(Debug, Clone)]
pub struct Stroke {
    /// Line width in points.
    pub width_pt: f64,
    /// Stroke color.
    pub color: Rgba,
    /// Dash pattern (points on/off), if dashed.
    pub dash: Option<Vec<f64>>,
}

/// A text decoration line (underline/strike), offset from the baseline.
#[derive(Debug, Clone, Copy)]
pub struct Decor {
    /// Line thickness in points.
    pub thickness_pt: f64,
    /// Y offset from the baseline (positive = below).
    pub y_offset_pt: f64,
}

/// The destination of a [`Item::Link`].
#[derive(Debug, Clone)]
pub enum LinkTarget {
    /// An external URL.
    Url(String),
    /// An internal page (0-based).
    Page(u32),
}

/// Fully resolved DOCX-internal annotation metadata. It remains private so the
/// legacy public layout primitives keep their exhaustive shape.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedInternalLink {
    pub(crate) source_page: usize,
    pub(crate) x_pt: f64,
    pub(crate) y_pt: f64,
    pub(crate) w_pt: f64,
    pub(crate) h_pt: f64,
    pub(crate) target_page: usize,
    pub(crate) target_y_pt: f64,
}

/// An index into [`LaidOutDocument::images`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageId(pub usize);

/// A decoded image ready for PDF embedding.
#[derive(Debug, Clone)]
pub enum EncodedImage {
    /// A JPEG, passed through with `DCTDecode`.
    Jpeg {
        /// Raw JPEG bytes.
        data: Vec<u8>,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
    },
    /// Decoded RGB8 samples.
    Rgb8 {
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// `width*height*3` samples.
        data: Vec<u8>,
    },
    /// Decoded RGBA8 samples (alpha becomes an SMask).
    Rgba8 {
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// `width*height*4` samples.
        data: Vec<u8>,
    },
}

/// A PDF outline (bookmark) entry.
#[derive(Debug, Clone)]
pub struct OutlineEntry {
    /// Heading text.
    pub title: String,
    /// Outline level 0–2 (Heading 1–3).
    pub level: u8,
    /// Target page (0-based).
    pub page: u32,
    /// Y position on the page (top-origin) for the destination.
    pub y_pt: f64,
}

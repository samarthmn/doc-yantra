//! Shared DrawingML parsing for images (`w:drawing` in WML, `p:pic` in PML).
//!
//! Task 1 parses only what the profile and later layout need: inline-vs-anchor
//! disposition, extents (EMU), the blip relationship id, source-crop presence,
//! and the `a:graphicData` URI (used to classify charts/diagrams/pictures).
//! Preset-geometry evaluation lives in the pptx task.

use super::{A, Node, R, WP, attr, children, emu_to_pt, parse_i64};

/// Whether a drawing flows inline with text or floats (anchored).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawingKind {
    /// `wp:inline` — participates in line layout.
    Inline,
    /// `wp:anchor` — floating; unsupported v1 (surfaced as `AnchoredDrawing`).
    Anchor,
}

/// A parsed `w:drawing` (or DrawingML picture) reference.
#[derive(Debug, Clone)]
pub struct Drawing {
    /// Inline vs anchored disposition.
    pub kind: DrawingKind,
    /// Extent in points `(width, height)`, from `wp:extent` (EMU → pt).
    pub extent_pt: Option<(f64, f64)>,
    /// The image part relationship id (`a:blip @r:embed`), if any.
    pub blip_rel_id: Option<String>,
    /// True when a source crop (`a:srcRect`) is present → `ImageCrop`.
    pub cropped: bool,
    /// The `a:graphicData @uri` — distinguishes picture / chart / diagram / ole.
    pub graphic_uri: Option<String>,
}

/// The DrawingML picture graphic-data URI.
pub const URI_PICTURE: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
/// The chart graphic-data URI.
pub const URI_CHART: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
/// The SmartArt/diagram graphic-data URI.
pub const URI_DIAGRAM: &str = "http://schemas.openxmlformats.org/drawingml/2006/diagram";

impl Drawing {
    /// Parses a `w:drawing` element into a [`Drawing`] model. Never errors —
    /// missing sub-parts leave their fields `None`; the profile/layout decide
    /// what is in-envelope.
    pub fn parse_w_drawing(node: Node) -> Drawing {
        let (kind, container) = if let Some(inline) = children(node).find(|c| is_wp(*c, "inline")) {
            (DrawingKind::Inline, Some(inline))
        } else if let Some(anchor) = children(node).find(|c| is_wp(*c, "anchor")) {
            (DrawingKind::Anchor, Some(anchor))
        } else {
            (DrawingKind::Inline, None)
        };

        let mut extent_pt = None;
        let mut blip_rel_id = None;
        let mut cropped = false;
        let mut graphic_uri = None;

        if let Some(container) = container {
            if let Some(ext) = children(container).find(|c| is_wp(*c, "extent")) {
                let cx = parse_i64(attr(ext, WP, "cx").or_else(|| attr(ext, A, "cx")));
                let cy = parse_i64(attr(ext, WP, "cy").or_else(|| attr(ext, A, "cy")));
                if let (Some(cx), Some(cy)) = (cx, cy) {
                    extent_pt = Some((emu_to_pt(cx), emu_to_pt(cy)));
                }
            }
            for d in container.descendants() {
                if super::is(d, A, "graphicData") {
                    graphic_uri = attr(d, A, "uri").map(str::to_string);
                }
                if super::is(d, A, "blip") {
                    blip_rel_id = attr(d, R, "embed").map(str::to_string);
                }
                if super::is(d, A, "srcRect") {
                    cropped = true;
                }
            }
        }

        Drawing {
            kind,
            extent_pt,
            blip_rel_id,
            cropped,
            graphic_uri,
        }
    }
}

fn is_wp(node: Node, local: &str) -> bool {
    super::is(node, WP, local)
}

//! Shared redaction request, result, geometry, and page-rotation types.
//!
//! Coordinates supplied by callers use percentages of the displayed page,
//! with a top-left origin. [`resolve_rect()`] converts them to displayed page
//! points and [`derotate_box()`] maps those points into unrotated PDF user
//! space.

/// One redaction area from the DocYantra service-compatible payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RedactArea {
    /// Zero-based page index.
    pub page_index: u32,
    /// Left edge as a percentage of the displayed page width.
    pub left: f64,
    /// Top edge as a percentage of the displayed page height.
    pub top: f64,
    /// Width as a percentage of the displayed page width.
    pub width: f64,
    /// Height as a percentage of the displayed page height.
    pub height: f64,
}

impl RedactArea {
    /// Returns this area's page-relative rectangle without its page index.
    pub fn rect(self) -> RedactionRect {
        RedactionRect {
            left_pct: self.left,
            top_pct: self.top,
            width_pct: self.width,
            height_pct: self.height,
        }
    }
}

impl From<&RedactArea> for RedactionRect {
    fn from(area: &RedactArea) -> Self {
        area.rect()
    }
}

/// One redaction rectangle as percentages of the displayed page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RedactionRect {
    /// Left edge as a percentage of the displayed page width.
    pub left_pct: f64,
    /// Top edge as a percentage of the displayed page height.
    pub top_pct: f64,
    /// Width as a percentage of the displayed page width.
    pub width_pct: f64,
    /// Height as a percentage of the displayed page height.
    pub height_pct: f64,
}

/// Result of applying redactions to a PDF.
#[derive(Debug, Clone)]
pub struct RedactOutput {
    /// Rewritten PDF bytes.
    pub pdf: Vec<u8>,
    /// Number of pages in the rewritten PDF.
    pub page_count: u32,
    /// Notices for whole-object removals that exceeded requested rectangles.
    pub warnings: Vec<RedactionWarning>,
}

/// Closed machine-readable warning codes emitted by redaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RedactionWarningCode {
    /// A whole PDF object exceeded the requested rectangle and was removed.
    OverRedaction,
}

impl RedactionWarningCode {
    /// Stable snake_case code for API consumers.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OverRedaction => "over_redaction",
        }
    }
}

/// Stable object kinds carried by redaction warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RedactionObjectKind {
    /// Text content inside one `BT..ET` object.
    Text,
    /// An inline image (`BI..EI`).
    InlineImage,
    /// An image XObject and any attached masks.
    ImageXObject,
    /// A Form XObject.
    FormXObject,
    /// A painted vector path.
    VectorPath,
    /// A shading operation and resource.
    Shading,
    /// A non-widget annotation.
    Annotation,
    /// A widget annotation and its field subtree.
    Widget,
}

impl RedactionObjectKind {
    /// Stable snake_case kind for API consumers.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::InlineImage => "inline_image",
            Self::ImageXObject => "image_xobject",
            Self::FormXObject => "form_xobject",
            Self::VectorPath => "vector_path",
            Self::Shading => "shading",
            Self::Annotation => "annotation",
            Self::Widget => "widget",
        }
    }

    pub(crate) const fn message_label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::InlineImage => "inline image",
            Self::ImageXObject => "image XObject",
            Self::FormXObject => "Form XObject",
            Self::VectorPath => "vector path",
            Self::Shading => "shading",
            Self::Annotation => "annotation",
            Self::Widget => "widget",
        }
    }
}

/// Structured notice for a conservative redaction decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionWarning {
    /// Closed machine code; consumers branch on this field.
    pub code: RedactionWarningCode,
    /// Zero-based page index.
    pub page: u32,
    /// Stable kind of object that caused the warning.
    pub kind: RedactionObjectKind,
    /// Human-readable context; consumers must not branch on this prose.
    pub message: String,
}

impl std::fmt::Display for RedactionWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Axis-aligned box in PDF points, using a bottom-left origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxPts {
    /// Left edge in points.
    pub left: f32,
    /// Right edge in points.
    pub right: f32,
    /// Bottom edge in points.
    pub bottom: f32,
    /// Top edge in points.
    pub top: f32,
}

impl BoxPts {
    /// Reports whether all coordinates are finite and both dimensions are positive.
    pub fn is_finite_and_nonempty(&self) -> bool {
        [self.left, self.right, self.bottom, self.top]
            .iter()
            .all(|value| value.is_finite())
            && self.right > self.left
            && self.top > self.bottom
    }

    /// Reports overlap with strictly positive area.
    pub fn intersects(&self, left: f32, right: f32, bottom: f32, top: f32) -> bool {
        self.left < right && self.right > left && self.bottom < top && self.top > bottom
    }

    /// Returns the box area, treating inverted dimensions as zero.
    pub fn area(&self) -> f32 {
        (self.right - self.left).max(0.0) * (self.top - self.bottom).max(0.0)
    }

    /// Returns overlap area with another box.
    pub fn overlap_area(&self, other: &Self) -> f32 {
        let width = self.right.min(other.right) - self.left.max(other.left);
        let height = self.top.min(other.top) - self.bottom.max(other.bottom);
        width.max(0.0) * height.max(0.0)
    }
}

/// Supported normalized PDF page rotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRotation {
    /// No rotation.
    None,
    /// 90 degrees clockwise.
    Degrees90,
    /// 180 degrees clockwise.
    Degrees180,
    /// 270 degrees clockwise.
    Degrees270,
}

impl PageRotation {
    /// Normalizes any multiple of 90 degrees into the corresponding rotation.
    ///
    /// Returns `None` when `degrees` is not a multiple of 90.
    pub fn from_degrees(degrees: i64) -> Option<Self> {
        if degrees % 90 != 0 {
            return None;
        }

        match degrees.rem_euclid(360) {
            0 => Some(Self::None),
            90 => Some(Self::Degrees90),
            180 => Some(Self::Degrees180),
            270 => Some(Self::Degrees270),
            _ => None,
        }
    }
}

/// Resolves a top-left-origin percentage rectangle into displayed page points.
///
/// Percentages are clamped to `0..=100`. Degenerate and non-finite rectangles
/// return `None`, matching the existing DocYantra behavior.
pub fn resolve_rect(rect: &RedactionRect, page_width: f64, page_height: f64) -> Option<BoxPts> {
    let left_pct = rect.left_pct.clamp(0.0, 100.0);
    let right_pct = (rect.left_pct + rect.width_pct).clamp(0.0, 100.0);
    let top_pct = rect.top_pct.clamp(0.0, 100.0);
    let bottom_pct = (rect.top_pct + rect.height_pct).clamp(0.0, 100.0);

    let left = left_pct / 100.0 * page_width;
    let right = right_pct / 100.0 * page_width;
    let top = page_height - (top_pct / 100.0 * page_height);
    let bottom = page_height - (bottom_pct / 100.0 * page_height);

    if !(left.is_finite() && right.is_finite() && top.is_finite() && bottom.is_finite()) {
        return None;
    }
    if right <= left || top <= bottom {
        return None;
    }

    let bounds = BoxPts {
        left: left as f32,
        right: right as f32,
        bottom: bottom as f32,
        top: top as f32,
    };
    bounds.is_finite_and_nonempty().then_some(bounds)
}

/// Maps a displayed-space box into unrotated PDF user space.
///
/// `displayed_width` and `displayed_height` are page dimensions after applying
/// [`PageRotation`].
pub fn derotate_box(
    bounds: BoxPts,
    rotation: PageRotation,
    displayed_width: f32,
    displayed_height: f32,
) -> BoxPts {
    match rotation {
        PageRotation::None => bounds,
        PageRotation::Degrees90 => BoxPts {
            left: displayed_height - bounds.top,
            right: displayed_height - bounds.bottom,
            bottom: bounds.left,
            top: bounds.right,
        },
        PageRotation::Degrees180 => BoxPts {
            left: displayed_width - bounds.right,
            right: displayed_width - bounds.left,
            bottom: displayed_height - bounds.top,
            top: displayed_height - bounds.bottom,
        },
        PageRotation::Degrees270 => BoxPts {
            left: bounds.bottom,
            right: bounds.top,
            bottom: displayed_width - bounds.right,
            top: displayed_width - bounds.left,
        },
    }
}

/// Reports whether removed bounds meaningfully exceed the best-overlapping
/// requested rectangle.
///
/// Bounds exceed the request when their area is more than 1.5 times the
/// request area, or an edge extends more than 2% of the page dimension beyond
/// the request. No requested rectangles always means excess.
pub fn exceeds_requested(
    bounds: &BoxPts,
    requested: &[BoxPts],
    page_width: f32,
    page_height: f32,
) -> bool {
    let Some(best) = requested
        .iter()
        .max_by(|a, b| bounds.overlap_area(a).total_cmp(&bounds.overlap_area(b)))
        .copied()
    else {
        return true;
    };

    let epsilon_x = 0.02 * page_width;
    let epsilon_y = 0.02 * page_height;
    bounds.area() > 1.5 * best.area()
        || bounds.left < best.left - epsilon_x
        || bounds.right > best.right + epsilon_x
        || bounds.bottom < best.bottom - epsilon_y
        || bounds.top > best.top + epsilon_y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derotation_maps_displayed_boxes_for_all_page_rotations() {
        let expected = BoxPts {
            left: 10.0,
            right: 30.0,
            bottom: 20.0,
            top: 40.0,
        };
        let cases = [
            (PageRotation::None, expected, 100.0, 200.0),
            (
                PageRotation::Degrees90,
                BoxPts {
                    left: 20.0,
                    right: 40.0,
                    bottom: 70.0,
                    top: 90.0,
                },
                200.0,
                100.0,
            ),
            (
                PageRotation::Degrees180,
                BoxPts {
                    left: 70.0,
                    right: 90.0,
                    bottom: 160.0,
                    top: 180.0,
                },
                100.0,
                200.0,
            ),
            (
                PageRotation::Degrees270,
                BoxPts {
                    left: 160.0,
                    right: 180.0,
                    bottom: 10.0,
                    top: 30.0,
                },
                200.0,
                100.0,
            ),
        ];

        for (rotation, displayed, width, height) in cases {
            assert_eq!(derotate_box(displayed, rotation, width, height), expected);
        }
    }

    #[test]
    fn page_rotation_normalizes_positive_and_negative_multiples() {
        assert_eq!(
            PageRotation::from_degrees(-90),
            Some(PageRotation::Degrees270)
        );
        assert_eq!(
            PageRotation::from_degrees(450),
            Some(PageRotation::Degrees90)
        );
        assert_eq!(PageRotation::from_degrees(45), None);
    }
}

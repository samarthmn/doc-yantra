//! Page geometry: content area and header/footer bands derived from a section's
//! page size and margins.

use crate::oxml::wml::SectPr;

/// Resolved page geometry in points (top-left origin).
#[derive(Debug, Clone, Copy)]
pub struct PageGeometry {
    /// Page width.
    pub width_pt: f64,
    /// Page height.
    pub height_pt: f64,
    /// Left edge of the content area (left margin + gutter).
    pub content_left_pt: f64,
    /// Top edge of the content area (top margin).
    pub content_top_pt: f64,
    /// Right edge of the content area.
    pub content_right_pt: f64,
    /// Bottom edge of the content area.
    pub content_bottom_pt: f64,
    /// Header band top (distance from page top).
    pub header_top_pt: f64,
    /// Footer band bottom (distance from page top).
    pub footer_top_pt: f64,
}

impl PageGeometry {
    /// Derives geometry from a section.
    pub fn from_section(sect: &SectPr) -> PageGeometry {
        let m = sect.margins;
        PageGeometry {
            width_pt: sect.page_w_pt,
            height_pt: sect.page_h_pt,
            content_left_pt: m.left + m.gutter,
            content_top_pt: m.top,
            content_right_pt: sect.page_w_pt - m.right,
            content_bottom_pt: sect.page_h_pt - m.bottom,
            header_top_pt: m.header,
            footer_top_pt: sect.page_h_pt - m.footer,
        }
    }

    /// The width of the content area.
    pub fn content_width_pt(&self) -> f64 {
        (self.content_right_pt - self.content_left_pt).max(1.0)
    }

    /// The height of the content area.
    pub fn content_height_pt(&self) -> f64 {
        (self.content_bottom_pt - self.content_top_pt).max(1.0)
    }
}

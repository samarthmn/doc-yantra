//! Column-width geometry. Excel stores widths in "characters" of the workbook's
//! normal font (Calibri 11 → maximum digit width MDW = 7px at 96 DPI).

/// Maximum digit width in pixels for the normal font (Calibri 11).
const MDW: f64 = 7.0;

/// Converts a column width in "characters" to points. Excel's displayed width
/// maps `chars` to `chars·MDW + 5px` of cell padding (so the default 8.43 chars
/// → 64px → 48pt at 96 DPI).
pub fn width_chars_to_pt(chars: f64) -> f64 {
    let width_px = (chars * MDW + 5.0).round();
    width_px * 72.0 / 96.0
}

/// The default column width (8.43 chars) in points (≈ 48pt).
pub fn default_width_pt() -> f64 {
    width_chars_to_pt(8.43)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_width_is_48pt() {
        // 8.43 chars → 64px → 48pt.
        assert!(
            (default_width_pt() - 48.0).abs() < 0.01,
            "got {}",
            default_width_pt()
        );
    }

    #[test]
    fn wider_columns_scale() {
        assert!(width_chars_to_pt(20.0) > width_chars_to_pt(10.0));
    }
}

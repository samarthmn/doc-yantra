//! Column-width geometry. Excel stores widths in "characters" of the workbook's
//! normal font (Calibri 11 → Excel-web maximum digit width MDW = 7.5234375px at
//! 96 DPI).

/// Maximum digit width in pixels for the normal font (Calibri 11).
const MDW: f64 = 7.5234375;

/// Converts a column width in "characters" to points. Excel's displayed width
/// already includes the cell-padding allowance in its stored character unit;
/// Excel for the web therefore maps it directly to `chars * MDW`, without
/// adding padding once per column.
pub fn width_chars_to_pt(chars: f64) -> f64 {
    chars * MDW * 72.0 / 96.0
}

/// The default column width (8.43 chars) in points (≈ 47.55pt).
pub fn default_width_pt() -> f64 {
    width_chars_to_pt(8.43)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_width_uses_excel_web_calibri_digit_advance() {
        // Excel-web truth: 8.43 stored characters use the same 7.5234375px/char
        // advance as authored columns, with no extra per-column padding.
        assert!(
            (default_width_pt() - 47.5669335938).abs() < 0.0001,
            "got {}",
            default_width_pt()
        );
    }

    #[test]
    fn measured_excel_web_width_has_no_per_column_padding() {
        // The authoritative inventory PDF advances 4.5145pt per stored
        // character at 80% print scale. Jointly solving the three truth-table
        // bounds pins the logical advance to 7.5234375px per character.
        assert!((width_chars_to_pt(12.0) - 67.7109375).abs() < 0.0001);
        assert!((width_chars_to_pt(34.0) - 191.84765625).abs() < 0.0001);
        assert!(
            (width_chars_to_pt(34.0) / 34.0 - width_chars_to_pt(12.0) / 12.0).abs() < f64::EPSILON
        );
    }
}

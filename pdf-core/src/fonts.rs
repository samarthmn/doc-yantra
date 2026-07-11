//! Standard-14 font metrics (Adobe AFM widths, 1/1000 em units).
//!
//! Only ASCII 32..=126 is tabulated; other characters fall back to an average
//! width. WinAnsi runs keep this legacy path; other supported scripts use a
//! runtime-registered CID font through [`crate::embed_font`].

/// PDF standard-14 fonts supported by pdf-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardFont {
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    HelveticaBoldOblique,
    Courier,
    CourierBold,
}

impl StandardFont {
    /// PostScript base font name for use in a /Font dictionary.
    pub fn base_name(self) -> &'static str {
        match self {
            StandardFont::Helvetica => "Helvetica",
            StandardFont::HelveticaBold => "Helvetica-Bold",
            StandardFont::HelveticaOblique => "Helvetica-Oblique",
            StandardFont::HelveticaBoldOblique => "Helvetica-BoldOblique",
            StandardFont::Courier => "Courier",
            StandardFont::CourierBold => "Courier-Bold",
        }
    }

    /// Glyph advance width in 1/1000 em for `c`.
    pub fn glyph_width_milli(self, c: char) -> u32 {
        match self {
            StandardFont::Courier | StandardFont::CourierBold => 600,
            StandardFont::Helvetica | StandardFont::HelveticaOblique => {
                ascii_width(c, &HELVETICA_WIDTHS, 556)
            }
            StandardFont::HelveticaBold | StandardFont::HelveticaBoldOblique => {
                ascii_width(c, &HELVETICA_BOLD_WIDTHS, 556)
            }
        }
    }

    /// Width of `text` in points when set at `size_pt`.
    pub fn text_width_pt(self, text: &str, size_pt: f64) -> f64 {
        let milli: u32 = text.chars().map(|c| self.glyph_width_milli(c)).sum();
        (milli as f64) * size_pt / 1000.0
    }
}

fn ascii_width(c: char, table: &[u16; 95], fallback: u16) -> u32 {
    let code = c as u32;
    if (32..=126).contains(&code) {
        table[(code - 32) as usize] as u32
    } else {
        fallback as u32
    }
}

/// Helvetica AFM widths for ASCII 32..=126.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278,
    278, // ' '..'/'
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, // '0'..'9'
    278, 278, 584, 584, 584, 556, 1015, // ':'..'@'
    667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, // 'A'..'P'
    778, 722, 667, 611, 722, 667, 944, 667, 667, 611, // 'Q'..'Z'
    278, 278, 278, 469, 556, 333, // '['..'`'
    556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    556, // 'a'..'p'
    556, 333, 500, 278, 556, 500, 722, 500, 500, 500, // 'q'..'z'
    334, 260, 334, 584, // '{'..'~'
];

/// Helvetica-Bold AFM widths for ASCII 32..=126.
const HELVETICA_BOLD_WIDTHS: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278,
    278, // ' '..'/'
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, // '0'..'9'
    333, 333, 584, 584, 584, 611, 975, // ':'..'@'
    722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778,
    667, // 'A'..'P'
    778, 722, 667, 611, 722, 667, 944, 667, 667, 611, // 'Q'..'Z'
    333, 278, 333, 584, 556, 333, // '['..'`'
    556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611,
    611, // 'a'..'p'
    611, 389, 556, 333, 611, 556, 778, 556, 556, 500, // 'q'..'z'
    389, 280, 389, 584, // '{'..'~'
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helvetica_space_and_cap_a() {
        assert_eq!(StandardFont::Helvetica.glyph_width_milli(' '), 278);
        assert_eq!(StandardFont::Helvetica.glyph_width_milli('A'), 667);
        assert_eq!(StandardFont::HelveticaBold.glyph_width_milli('A'), 722);
    }

    #[test]
    fn courier_is_monospace() {
        assert_eq!(StandardFont::Courier.glyph_width_milli('i'), 600);
        assert_eq!(StandardFont::Courier.glyph_width_milli('W'), 600);
    }

    #[test]
    fn text_width_scales_with_size() {
        let w12 = StandardFont::Helvetica.text_width_pt("CONFIDENTIAL", 12.0);
        let w24 = StandardFont::Helvetica.text_width_pt("CONFIDENTIAL", 24.0);
        assert!((w24 - 2.0 * w12).abs() < 1e-9);
    }
}

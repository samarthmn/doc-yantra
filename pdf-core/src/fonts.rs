//! Standard-14 font metrics (Adobe AFM widths, 1/1000 em units).
//!
//! Latin fonts use their WinAnsi-encoded AFM widths. Symbol and ZapfDingbats
//! use their built-in encoded AFM widths. Undefined codes conservatively use
//! each face's widest AFM advance.

/// PDF standard-14 fonts supported by pdf-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardFont {
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    HelveticaBoldOblique,
    Courier,
    CourierBold,
    CourierOblique,
    CourierBoldOblique,
    TimesRoman,
    TimesBold,
    TimesItalic,
    TimesBoldItalic,
    Symbol,
    ZapfDingbats,
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
            StandardFont::CourierOblique => "Courier-Oblique",
            StandardFont::CourierBoldOblique => "Courier-BoldOblique",
            StandardFont::TimesRoman => "Times-Roman",
            StandardFont::TimesBold => "Times-Bold",
            StandardFont::TimesItalic => "Times-Italic",
            StandardFont::TimesBoldItalic => "Times-BoldItalic",
            StandardFont::Symbol => "Symbol",
            StandardFont::ZapfDingbats => "ZapfDingbats",
        }
    }

    /// Resolves an exact Standard-14 `/BaseFont` name.
    ///
    /// A leading PDF name slash and a valid six-letter subset prefix are
    /// accepted because callers commonly pass names at different parse stages.
    pub fn from_base_name(base_name: &[u8]) -> Option<Self> {
        let name = base_name.strip_prefix(b"/").unwrap_or(base_name);
        let name = strip_subset_prefix(name);
        match name {
            b"Helvetica" => Some(Self::Helvetica),
            b"Helvetica-Bold" => Some(Self::HelveticaBold),
            b"Helvetica-Oblique" => Some(Self::HelveticaOblique),
            b"Helvetica-BoldOblique" => Some(Self::HelveticaBoldOblique),
            b"Courier" => Some(Self::Courier),
            b"Courier-Bold" => Some(Self::CourierBold),
            b"Courier-Oblique" => Some(Self::CourierOblique),
            b"Courier-BoldOblique" => Some(Self::CourierBoldOblique),
            b"Times-Roman" => Some(Self::TimesRoman),
            b"Times-Bold" => Some(Self::TimesBold),
            b"Times-Italic" => Some(Self::TimesItalic),
            b"Times-BoldItalic" => Some(Self::TimesBoldItalic),
            b"Symbol" => Some(Self::Symbol),
            b"ZapfDingbats" => Some(Self::ZapfDingbats),
            _ => None,
        }
    }

    /// Encoded glyph advance width in 1/1000 em.
    ///
    /// Latin faces interpret `code` as WinAnsi. Symbol and ZapfDingbats use
    /// their built-in encodings. Unassigned codes return a conservative width.
    pub fn width_for_code_milli(self, code: u8) -> u32 {
        if matches!(
            self,
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique
        ) {
            return 600;
        }

        let table = self.width_table();
        let width = table[usize::from(code)];
        u32::from(if width == 0 {
            self.conservative_fallback_milli()
        } else {
            width
        })
    }

    /// Built-in-encoding glyph advance width in 1/1000 em.
    ///
    /// Use this for a Standard-14 Type1 font without an `/Encoding` entry.
    /// Latin faces then use Adobe StandardEncoding; symbolic faces retain
    /// their own built-in encodings. Unassigned codes return a conservative
    /// width rather than borrowing a potentially narrower WinAnsi metric.
    pub fn width_for_builtin_code_milli(self, code: u8) -> u32 {
        if matches!(
            self,
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique
        ) {
            return 600;
        }

        let table = self.builtin_width_table();
        let width = table[usize::from(code)];
        u32::from(if width == 0 {
            self.conservative_fallback_milli()
        } else {
            width
        })
    }

    /// Glyph advance width in 1/1000 em for `c`.
    pub fn glyph_width_milli(self, c: char) -> u32 {
        match self {
            Self::Symbol | Self::ZapfDingbats => u8::try_from(u32::from(c)).map_or_else(
                |_| self.conservative_fallback_milli().into(),
                |code| self.width_for_code_milli(code),
            ),
            _ => winansi_byte(c).map_or_else(
                || self.conservative_fallback_milli().into(),
                |code| self.width_for_code_milli(code),
            ),
        }
    }

    /// Width of `text` in points when set at `size_pt`.
    pub fn text_width_pt(self, text: &str, size_pt: f64) -> f64 {
        let milli: u64 = text
            .chars()
            .map(|c| u64::from(self.glyph_width_milli(c)))
            .sum();
        (milli as f64) * size_pt / 1000.0
    }

    fn width_table(self) -> &'static [u16; 256] {
        match self {
            Self::Helvetica | Self::HelveticaOblique => &HELVETICA_WIDTHS,
            Self::HelveticaBold | Self::HelveticaBoldOblique => &HELVETICA_BOLD_WIDTHS,
            Self::TimesRoman => &TIMES_ROMAN_WIDTHS,
            Self::TimesBold => &TIMES_BOLD_WIDTHS,
            Self::TimesItalic => &TIMES_ITALIC_WIDTHS,
            Self::TimesBoldItalic => &TIMES_BOLD_ITALIC_WIDTHS,
            Self::Symbol => &SYMBOL_WIDTHS,
            Self::ZapfDingbats => &ZAPF_DINGBATS_WIDTHS,
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                &COURIER_WIDTHS
            }
        }
    }

    fn builtin_width_table(self) -> &'static [u16; 256] {
        match self {
            Self::Helvetica | Self::HelveticaOblique => &HELVETICA_BUILTIN_WIDTHS,
            Self::HelveticaBold | Self::HelveticaBoldOblique => &HELVETICA_BOLD_BUILTIN_WIDTHS,
            Self::TimesRoman => &TIMES_ROMAN_BUILTIN_WIDTHS,
            Self::TimesBold => &TIMES_BOLD_BUILTIN_WIDTHS,
            Self::TimesItalic => &TIMES_ITALIC_BUILTIN_WIDTHS,
            Self::TimesBoldItalic => &TIMES_BOLD_ITALIC_BUILTIN_WIDTHS,
            Self::Symbol => &SYMBOL_WIDTHS,
            Self::ZapfDingbats => &ZAPF_DINGBATS_WIDTHS,
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                &COURIER_WIDTHS
            }
        }
    }

    const fn conservative_fallback_milli(self) -> u16 {
        match self {
            Self::Helvetica | Self::HelveticaOblique => 1015,
            Self::HelveticaBold | Self::HelveticaBoldOblique => 1000,
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                600
            }
            Self::TimesRoman | Self::TimesBold | Self::TimesItalic | Self::TimesBoldItalic => 1000,
            Self::Symbol => 1042,
            Self::ZapfDingbats => 1016,
        }
    }
}

fn strip_subset_prefix(name: &[u8]) -> &[u8] {
    if name.len() > 7 && name[6] == b'+' && name[..6].iter().all(|byte| byte.is_ascii_uppercase()) {
        &name[7..]
    } else {
        name
    }
}

fn winansi_byte(c: char) -> Option<u8> {
    let cp = u32::from(c);
    match cp {
        0x00..=0x7F | 0xA0..=0xFF => Some(cp as u8),
        _ => Some(match c {
            '\u{20AC}' => 0x80,
            '\u{201A}' => 0x82,
            '\u{0192}' => 0x83,
            '\u{201E}' => 0x84,
            '\u{2026}' => 0x85,
            '\u{2020}' => 0x86,
            '\u{2021}' => 0x87,
            '\u{02C6}' => 0x88,
            '\u{2030}' => 0x89,
            '\u{0160}' => 0x8A,
            '\u{2039}' => 0x8B,
            '\u{0152}' => 0x8C,
            '\u{017D}' => 0x8E,
            '\u{2018}' => 0x91,
            '\u{2019}' => 0x92,
            '\u{201C}' => 0x93,
            '\u{201D}' => 0x94,
            '\u{2022}' => 0x95,
            '\u{2013}' => 0x96,
            '\u{2014}' => 0x97,
            '\u{02DC}' => 0x98,
            '\u{2122}' => 0x99,
            '\u{0161}' => 0x9A,
            '\u{203A}' => 0x9B,
            '\u{0153}' => 0x9C,
            '\u{017E}' => 0x9E,
            '\u{0178}' => 0x9F,
            _ => return None,
        }),
    }
}

const COURIER_WIDTHS: [u16; 256] = [600; 256];

// Built-in Latin tables preserve Adobe StandardEncoding code assignments.
const HELVETICA_BUILTIN_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 222, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 333, 556, 556, 167, 556, 556, 556, 556, 191, 333, 556, 333, 333, 500, 500, 0, 556, 556,
    556, 278, 0, 537, 350, 222, 333, 333, 556, 1000, 1000, 0, 611, 0, 333, 333, 333, 333, 333, 333,
    333, 333, 0, 333, 333, 0, 333, 333, 333, 1000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1000, 0, 370, 0, 0, 0, 0, 556, 778, 1000, 365, 0, 0, 0, 0, 0, 889, 0, 0, 0, 278, 0, 0, 222,
    611, 944, 611, 0, 0, 0, 0,
];

const HELVETICA_BOLD_BUILTIN_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    278, 333, 474, 556, 556, 889, 722, 278, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 278, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 333, 556, 556, 167, 556, 556, 556, 556, 238, 500, 556, 333, 333, 611, 611, 0, 556, 556,
    556, 278, 0, 556, 350, 278, 500, 500, 556, 1000, 1000, 0, 611, 0, 333, 333, 333, 333, 333, 333,
    333, 333, 0, 333, 333, 0, 333, 333, 333, 1000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1000, 0, 370, 0, 0, 0, 0, 611, 778, 1000, 365, 0, 0, 0, 0, 0, 889, 0, 0, 0, 278, 0, 0, 278,
    611, 944, 611, 0, 0, 0, 0,
];

const TIMES_ROMAN_BUILTIN_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 408, 500, 500, 833, 778, 333, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 333, 500, 500, 167, 500, 500, 500, 500, 180, 444, 500, 333, 333, 556, 556, 0, 500, 500,
    500, 250, 0, 453, 350, 333, 444, 444, 500, 1000, 1000, 0, 444, 0, 333, 333, 333, 333, 333, 333,
    333, 333, 0, 333, 333, 0, 333, 333, 333, 1000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    889, 0, 276, 0, 0, 0, 0, 611, 722, 889, 310, 0, 0, 0, 0, 0, 667, 0, 0, 0, 278, 0, 0, 278, 500,
    722, 500, 0, 0, 0, 0,
];

const TIMES_ITALIC_BUILTIN_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 420, 500, 500, 833, 778, 333, 333, 333, 500, 675, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 675, 675, 675, 500, 920, 611, 611, 667, 722, 611,
    611, 722, 722, 333, 444, 667, 556, 833, 667, 722, 611, 722, 611, 500, 556, 722, 611, 833, 611,
    556, 556, 389, 278, 389, 422, 500, 333, 500, 500, 444, 500, 444, 278, 500, 500, 278, 278, 444,
    278, 722, 500, 500, 500, 500, 389, 389, 278, 500, 444, 667, 444, 444, 389, 400, 275, 400, 541,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 389, 500, 500, 167, 500, 500, 500, 500, 214, 556, 500, 333, 333, 500, 500, 0, 500, 500,
    500, 250, 0, 523, 350, 333, 556, 556, 500, 889, 1000, 0, 500, 0, 333, 333, 333, 333, 333, 333,
    333, 333, 0, 333, 333, 0, 333, 333, 333, 889, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    889, 0, 276, 0, 0, 0, 0, 556, 722, 944, 310, 0, 0, 0, 0, 0, 667, 0, 0, 0, 278, 0, 0, 278, 500,
    667, 500, 0, 0, 0, 0,
];

const TIMES_BOLD_BUILTIN_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 555, 500, 500, 1000, 833, 333, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 930, 722, 667, 722, 722, 667,
    611, 778, 778, 389, 500, 778, 667, 944, 722, 778, 611, 778, 722, 556, 667, 722, 722, 1000, 722,
    722, 667, 333, 278, 333, 581, 500, 333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556,
    278, 833, 556, 500, 556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 333, 500, 500, 167, 500, 500, 500, 500, 278, 500, 500, 333, 333, 556, 556, 0, 500, 500,
    500, 250, 0, 540, 350, 333, 500, 500, 500, 1000, 1000, 0, 500, 0, 333, 333, 333, 333, 333, 333,
    333, 333, 0, 333, 333, 0, 333, 333, 333, 1000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1000, 0, 300, 0, 0, 0, 0, 667, 778, 1000, 330, 0, 0, 0, 0, 0, 722, 0, 0, 0, 278, 0, 0, 278,
    500, 722, 556, 0, 0, 0, 0,
];

const TIMES_BOLD_ITALIC_BUILTIN_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 389, 555, 500, 500, 833, 778, 333, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 832, 667, 667, 667, 722, 667,
    667, 722, 778, 389, 500, 667, 611, 889, 722, 722, 611, 722, 667, 556, 611, 722, 667, 889, 667,
    611, 611, 333, 278, 333, 570, 500, 333, 500, 500, 444, 500, 444, 333, 500, 556, 278, 278, 500,
    278, 778, 556, 500, 500, 500, 389, 389, 278, 556, 444, 667, 500, 444, 389, 348, 220, 348, 570,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 389, 500, 500, 167, 500, 500, 500, 500, 278, 500, 500, 333, 333, 556, 556, 0, 500, 500,
    500, 250, 0, 500, 350, 333, 500, 500, 500, 1000, 1000, 0, 500, 0, 333, 333, 333, 333, 333, 333,
    333, 333, 0, 333, 333, 0, 333, 333, 333, 1000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    944, 0, 266, 0, 0, 0, 0, 611, 722, 944, 300, 0, 0, 0, 0, 0, 722, 0, 0, 0, 278, 0, 0, 278, 500,
    722, 500, 0, 0, 0, 0,
];

// Latin tables map WinAnsi codes to public Adobe Core 14 AFM metrics.
const HELVETICA_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
    0, 556, 0, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0, 0, 222, 222,
    333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 0, 500, 667, 278, 333, 556, 556, 556, 556,
    260, 556, 333, 737, 370, 556, 584, 333, 737, 333, 400, 584, 333, 333, 333, 556, 537, 278, 333,
    333, 365, 556, 834, 834, 834, 611, 667, 667, 667, 667, 667, 667, 1000, 722, 667, 667, 667, 667,
    278, 278, 278, 278, 722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667,
    611, 556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278, 278, 556, 556,
    556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500,
];

const HELVETICA_BOLD_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
    0, 556, 0, 278, 556, 500, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0, 0, 278, 278,
    500, 500, 350, 556, 1000, 333, 1000, 556, 333, 944, 0, 500, 667, 278, 333, 556, 556, 556, 556,
    280, 556, 333, 737, 370, 556, 584, 333, 737, 333, 400, 584, 333, 333, 333, 611, 556, 278, 333,
    333, 365, 556, 834, 834, 834, 611, 722, 722, 722, 722, 722, 722, 1000, 722, 667, 667, 667, 667,
    278, 278, 278, 278, 722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667,
    611, 556, 556, 556, 556, 556, 556, 889, 556, 556, 556, 556, 556, 278, 278, 278, 278, 611, 611,
    611, 611, 611, 611, 611, 584, 611, 611, 611, 611, 611, 556, 611, 556,
];

const TIMES_ROMAN_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
    0, 500, 0, 333, 500, 444, 1000, 500, 500, 333, 1000, 556, 333, 889, 0, 611, 0, 0, 333, 333,
    444, 444, 350, 500, 1000, 333, 980, 389, 333, 722, 0, 444, 722, 250, 333, 500, 500, 500, 500,
    200, 500, 333, 760, 276, 500, 564, 333, 760, 333, 400, 564, 300, 300, 333, 500, 453, 250, 333,
    300, 310, 500, 750, 750, 750, 444, 722, 722, 722, 722, 722, 722, 889, 667, 611, 611, 611, 611,
    333, 333, 333, 333, 722, 722, 722, 722, 722, 722, 722, 564, 722, 722, 722, 722, 722, 722, 556,
    500, 444, 444, 444, 444, 444, 444, 667, 444, 444, 444, 444, 444, 278, 278, 278, 278, 500, 500,
    500, 500, 500, 500, 500, 564, 500, 500, 500, 500, 500, 500, 500, 500,
];

const TIMES_ITALIC_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 420, 500, 500, 833, 778, 214, 333, 333, 500, 675, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 675, 675, 675, 500, 920, 611, 611, 667, 722, 611,
    611, 722, 722, 333, 444, 667, 556, 833, 667, 722, 611, 722, 611, 500, 556, 722, 611, 833, 611,
    556, 556, 389, 278, 389, 422, 500, 333, 500, 500, 444, 500, 444, 278, 500, 500, 278, 278, 444,
    278, 722, 500, 500, 500, 500, 389, 389, 278, 500, 444, 667, 444, 444, 389, 400, 275, 400, 541,
    0, 500, 0, 333, 500, 556, 889, 500, 500, 333, 1000, 500, 333, 944, 0, 556, 0, 0, 333, 333, 556,
    556, 350, 500, 889, 333, 980, 389, 333, 667, 0, 389, 556, 250, 389, 500, 500, 500, 500, 275,
    500, 333, 760, 276, 500, 675, 333, 760, 333, 400, 675, 300, 300, 333, 500, 523, 250, 333, 300,
    310, 500, 750, 750, 750, 500, 611, 611, 611, 611, 611, 611, 889, 667, 611, 611, 611, 611, 333,
    333, 333, 333, 722, 667, 722, 722, 722, 722, 722, 675, 722, 722, 722, 722, 722, 556, 611, 500,
    500, 500, 500, 500, 500, 500, 667, 444, 444, 444, 444, 444, 278, 278, 278, 278, 500, 500, 500,
    500, 500, 500, 500, 675, 500, 500, 500, 500, 500, 444, 500, 444,
];

const TIMES_BOLD_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 930, 722, 667, 722, 722, 667,
    611, 778, 778, 389, 500, 778, 667, 944, 722, 778, 611, 778, 722, 556, 667, 722, 722, 1000, 722,
    722, 667, 333, 278, 333, 581, 500, 333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556,
    278, 833, 556, 500, 556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520,
    0, 500, 0, 333, 500, 500, 1000, 500, 500, 333, 1000, 556, 333, 1000, 0, 667, 0, 0, 333, 333,
    500, 500, 350, 500, 1000, 333, 1000, 389, 333, 722, 0, 444, 722, 250, 333, 500, 500, 500, 500,
    220, 500, 333, 747, 300, 500, 570, 333, 747, 333, 400, 570, 300, 300, 333, 556, 540, 250, 333,
    300, 330, 500, 750, 750, 750, 500, 722, 722, 722, 722, 722, 722, 1000, 722, 667, 667, 667, 667,
    389, 389, 389, 389, 722, 722, 778, 778, 778, 778, 778, 570, 778, 722, 722, 722, 722, 722, 611,
    556, 500, 500, 500, 500, 500, 500, 722, 444, 444, 444, 444, 444, 278, 278, 278, 278, 500, 556,
    500, 500, 500, 500, 500, 570, 500, 556, 556, 556, 556, 500, 556, 500,
];

const TIMES_BOLD_ITALIC_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 389, 555, 500, 500, 833, 778, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 832, 667, 667, 667, 722, 667,
    667, 722, 778, 389, 500, 667, 611, 889, 722, 722, 611, 722, 667, 556, 611, 722, 667, 889, 667,
    611, 611, 333, 278, 333, 570, 500, 333, 500, 500, 444, 500, 444, 333, 500, 556, 278, 278, 500,
    278, 778, 556, 500, 500, 500, 389, 389, 278, 556, 444, 667, 500, 444, 389, 348, 220, 348, 570,
    0, 500, 0, 333, 500, 500, 1000, 500, 500, 333, 1000, 556, 333, 944, 0, 611, 0, 0, 333, 333,
    500, 500, 350, 500, 1000, 333, 1000, 389, 333, 722, 0, 389, 611, 250, 389, 500, 500, 500, 500,
    220, 500, 333, 747, 266, 500, 606, 333, 747, 333, 400, 570, 300, 300, 333, 576, 500, 250, 333,
    300, 300, 500, 750, 750, 750, 500, 667, 667, 667, 667, 667, 667, 944, 667, 667, 667, 667, 667,
    389, 389, 389, 389, 722, 722, 722, 722, 722, 722, 722, 570, 722, 722, 722, 722, 722, 611, 611,
    500, 500, 500, 500, 500, 500, 500, 722, 444, 444, 444, 444, 444, 278, 278, 278, 278, 500, 556,
    500, 500, 500, 500, 500, 570, 500, 556, 556, 556, 556, 444, 500, 444,
];

const SYMBOL_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    250, 333, 713, 500, 549, 833, 778, 439, 333, 333, 500, 549, 250, 549, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 549, 549, 549, 444, 549, 722, 667, 722, 612, 611,
    763, 603, 722, 333, 631, 722, 686, 889, 722, 722, 768, 741, 556, 592, 611, 690, 439, 768, 645,
    795, 611, 333, 863, 333, 658, 500, 500, 631, 549, 549, 494, 439, 521, 411, 603, 329, 603, 549,
    549, 576, 521, 549, 549, 521, 549, 603, 439, 576, 713, 686, 493, 686, 494, 480, 200, 480, 549,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 750, 620, 247, 549, 167, 713, 500, 753, 753, 753, 753, 1042, 987, 603, 987, 603, 400, 549,
    411, 549, 549, 713, 494, 460, 549, 549, 549, 549, 1000, 603, 1000, 658, 823, 686, 795, 987,
    768, 768, 823, 768, 768, 713, 713, 713, 713, 713, 713, 713, 768, 713, 790, 790, 890, 823, 549,
    250, 713, 603, 603, 1042, 987, 603, 987, 603, 494, 329, 790, 790, 786, 713, 384, 384, 384, 384,
    384, 384, 494, 494, 494, 494, 0, 329, 274, 686, 686, 686, 384, 384, 384, 384, 384, 384, 494,
    494, 494, 0,
];

const ZAPF_DINGBATS_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    278, 974, 961, 974, 980, 719, 789, 790, 791, 690, 960, 939, 549, 855, 911, 933, 911, 945, 974,
    755, 846, 762, 761, 571, 677, 763, 760, 759, 754, 494, 552, 537, 577, 692, 786, 788, 788, 790,
    793, 794, 816, 823, 789, 841, 823, 833, 816, 831, 923, 744, 723, 749, 790, 792, 695, 776, 768,
    792, 759, 707, 708, 682, 701, 826, 815, 789, 789, 707, 687, 696, 689, 786, 787, 713, 791, 785,
    791, 873, 761, 762, 762, 759, 759, 892, 892, 788, 784, 438, 138, 277, 415, 392, 392, 668, 668,
    0, 390, 390, 317, 317, 276, 276, 509, 509, 410, 410, 234, 234, 334, 334, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 732, 544, 544, 910, 667, 760, 760, 776, 595, 694, 626, 788,
    788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788,
    788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788,
    788, 894, 838, 1016, 458, 748, 924, 748, 918, 927, 928, 928, 834, 873, 828, 924, 924, 917, 930,
    931, 463, 883, 836, 836, 867, 867, 696, 696, 874, 0, 874, 760, 946, 771, 865, 771, 888, 967,
    888, 831, 873, 927, 970, 918, 0,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helvetica_faces_use_winansi_afm_widths() {
        assert_eq!(StandardFont::Helvetica.glyph_width_milli('\u{20ac}'), 556);
        assert_eq!(
            StandardFont::HelveticaBold.glyph_width_milli('\u{0152}'),
            1000
        );
        assert_eq!(StandardFont::HelveticaOblique.glyph_width_milli('é'), 556);
        assert_eq!(
            StandardFont::HelveticaBoldOblique.glyph_width_milli('A'),
            722
        );
    }

    #[test]
    fn all_courier_faces_are_monospace() {
        for face in [
            StandardFont::Courier,
            StandardFont::CourierBold,
            StandardFont::CourierOblique,
            StandardFont::CourierBoldOblique,
        ] {
            assert_eq!(face.glyph_width_milli('i'), 600);
            assert_eq!(face.glyph_width_milli('\u{20ac}'), 600);
        }
    }

    #[test]
    fn times_faces_use_distinct_afm_widths() {
        assert_eq!(StandardFont::TimesRoman.glyph_width_milli('A'), 722);
        assert_eq!(StandardFont::TimesItalic.glyph_width_milli('A'), 611);
        assert_eq!(StandardFont::TimesBold.glyph_width_milli('A'), 722);
        assert_eq!(StandardFont::TimesBoldItalic.glyph_width_milli('A'), 667);
    }

    #[test]
    fn symbolic_faces_use_builtin_code_metrics() {
        assert_eq!(StandardFont::Symbol.width_for_builtin_code_milli(b'A'), 722);
        assert_eq!(
            StandardFont::ZapfDingbats.width_for_builtin_code_milli(b'!'),
            974
        );
    }

    #[test]
    fn builtin_standard_encoding_differs_safely_from_winansi() {
        assert_eq!(StandardFont::Helvetica.width_for_code_milli(b'\''), 191);
        assert_eq!(
            StandardFont::Helvetica.width_for_builtin_code_milli(b'\''),
            222
        );
        assert_eq!(
            StandardFont::TimesBold.width_for_builtin_code_milli(0xae),
            556
        );
    }

    #[test]
    fn unmapped_codes_use_face_maximum_for_conservative_bounds() {
        assert_eq!(StandardFont::Helvetica.width_for_code_milli(0x81), 1015);
        assert_eq!(StandardFont::Symbol.width_for_code_milli(0x80), 1042);
        assert_eq!(StandardFont::ZapfDingbats.width_for_code_milli(0xff), 1016);
    }

    #[test]
    fn base_name_lookup_accepts_pdf_and_subset_forms() {
        assert_eq!(
            StandardFont::from_base_name(b"/Times-BoldItalic"),
            Some(StandardFont::TimesBoldItalic)
        );
        assert_eq!(
            StandardFont::from_base_name(b"ABCDEF+ZapfDingbats"),
            Some(StandardFont::ZapfDingbats)
        );
    }

    #[test]
    fn base_name_lookup_rejects_nonstandard_aliases() {
        assert_eq!(StandardFont::from_base_name(b"ArialMT"), None);
    }

    #[test]
    fn winansi_tables_cover_every_defined_byte() {
        for table in [
            &HELVETICA_WIDTHS,
            &HELVETICA_BOLD_WIDTHS,
            &TIMES_ROMAN_WIDTHS,
            &TIMES_ITALIC_WIDTHS,
            &TIMES_BOLD_WIDTHS,
            &TIMES_BOLD_ITALIC_WIDTHS,
        ] {
            assert_eq!(table.iter().filter(|width| **width != 0).count(), 218);
        }
    }

    #[test]
    fn builtin_tables_cover_every_afm_encoded_glyph() {
        for table in [
            &HELVETICA_BUILTIN_WIDTHS,
            &HELVETICA_BOLD_BUILTIN_WIDTHS,
            &TIMES_ROMAN_BUILTIN_WIDTHS,
            &TIMES_ITALIC_BUILTIN_WIDTHS,
            &TIMES_BOLD_BUILTIN_WIDTHS,
            &TIMES_BOLD_ITALIC_BUILTIN_WIDTHS,
        ] {
            assert_eq!(table.iter().filter(|width| **width != 0).count(), 149);
        }
        assert_eq!(
            SYMBOL_WIDTHS.iter().filter(|width| **width != 0).count(),
            189
        );
        assert_eq!(
            ZAPF_DINGBATS_WIDTHS
                .iter()
                .filter(|width| **width != 0)
                .count(),
            202
        );
    }

    #[test]
    fn text_width_scales_with_size() {
        let w12 = StandardFont::Helvetica.text_width_pt("CONFIDENTIAL", 12.0);
        let w24 = StandardFont::Helvetica.text_width_pt("CONFIDENTIAL", 24.0);
        assert!((w24 - 2.0 * w12).abs() < 1e-9);
    }
}

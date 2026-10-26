//! The complex-script gate (fail-closed). Scans text for scripts that need
//! shaping (Arabic/Indic/Thai/…) or bidi reordering, which v1 does not do. CJK
//! is allowed (1:1 mapping is correct) but flagged so font coverage can be
//! checked.

/// The result of scanning a document's text for script coverage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScriptScan {
    /// A complex script (needs shaping) is present → `ComplexScript`.
    pub complex_script: bool,
    /// Right-to-left text is present → `RtlText`.
    pub rtl: bool,
    /// CJK text is present (allowed, but needs a covering font).
    pub cjk: bool,
}

impl ScriptScan {
    /// Merges another scan into this one (logical OR of every flag).
    pub fn merge(&mut self, other: ScriptScan) {
        self.complex_script |= other.complex_script;
        self.rtl |= other.rtl;
        self.cjk |= other.cjk;
    }
}

/// Scans `text` for complex scripts, RTL, and CJK.
pub fn scan_scripts(text: &str) -> ScriptScan {
    let mut scan = ScriptScan::default();
    for ch in text.chars() {
        let c = ch as u32;
        if is_complex(c) {
            scan.complex_script = true;
        }
        if is_rtl_range(c) {
            scan.rtl = true;
        }
        if is_cjk(c) {
            scan.cjk = true;
        }
    }
    // Corroborate RTL via the bidi algorithm (catches marks/isolates too).
    if !scan.rtl && text.chars().any(|c| c as u32 >= 0x0590) {
        scan.rtl = unicode_bidi::BidiInfo::new(text, None).has_rtl();
    }
    scan
}

/// Complex scripts that require shaping we do not implement v1.
fn is_complex(c: u32) -> bool {
    matches!(c,
        0x0590..=0x05FF                       // Hebrew
        | 0x0600..=0x06FF | 0x0750..=0x077F   // Arabic
        | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF   // Arabic presentation forms
        | 0x0900..=0x0DFF                     // Devanagari + Indic
        | 0x0E00..=0x0EFF                     // Thai / Lao
        | 0x1780..=0x17FF                     // Khmer
        | 0x1000..=0x109F                     // Myanmar
    )
}

/// RTL script ranges (Hebrew, Arabic).
fn is_rtl_range(c: u32) -> bool {
    matches!(c,
        0x0590..=0x05FF
        | 0x0600..=0x06FF | 0x0750..=0x077F
        | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF
    )
}

/// CJK ranges (allowed with a covering font).
fn is_cjk(c: u32) -> bool {
    matches!(c,
        0x4E00..=0x9FFF                       // CJK unified ideographs
        | 0x3040..=0x30FF                     // Hiragana + Katakana
        | 0xAC00..=0xD7AF                     // Hangul syllables
        | 0x3000..=0x303F                     // CJK symbols/punctuation
        | 0xFF00..=0xFFEF                     // Halfwidth/Fullwidth forms
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_is_plain() {
        let s = scan_scripts("Hello, world! 123");
        assert_eq!(s, ScriptScan::default());
    }

    #[test]
    fn arabic_is_complex_and_rtl() {
        let s = scan_scripts("مرحبا");
        assert!(s.complex_script);
        assert!(s.rtl);
    }

    #[test]
    fn hebrew_is_complex_and_rtl() {
        let s = scan_scripts("שלום");
        assert!(s.complex_script);
        assert!(s.rtl);
    }

    #[test]
    fn devanagari_is_complex_not_rtl() {
        let s = scan_scripts("नमस्ते");
        assert!(s.complex_script);
        assert!(!s.rtl);
    }

    #[test]
    fn cjk_allowed_not_complex() {
        let s = scan_scripts("日本語のテスト 한국어");
        assert!(s.cjk);
        assert!(!s.complex_script);
        assert!(!s.rtl);
    }
}

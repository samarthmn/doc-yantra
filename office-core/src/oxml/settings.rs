//! `word/settings.xml`: the handful of document-wide settings that affect
//! layout or the profile verdict.

use super::{W, attr, children, parse_i64, toggle, twip_to_pt};
use crate::{OfficeError, Result};

/// Document-wide settings.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Default tab-stop interval in points (Word default 36pt = 720 twips).
    pub default_tab_stop_pt: f64,
    /// Whether even/odd pages use different headers/footers.
    pub even_and_odd_headers: bool,
    /// Whether automatic hyphenation is enabled (out of envelope v1).
    pub auto_hyphenation: bool,
    /// Compatibility mode (`w:compatSetting` `compatibilityMode`; default 15).
    pub compat_mode: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            default_tab_stop_pt: 36.0,
            even_and_odd_headers: false,
            auto_hyphenation: false,
            compat_mode: 15,
        }
    }
}

impl Settings {
    /// Parses `word/settings.xml`. An absent part yields [`Settings::default`].
    pub fn parse(xml: &str) -> Result<Settings> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| OfficeError::Invalid(format!("settings.xml XML: {e}")))?;
        let root = doc.root_element();
        let mut settings = Settings::default();

        for c in children(root) {
            match c.tag_name().name() {
                "defaultTabStop" => {
                    if let Some(v) = parse_i64(attr(c, W, "val")) {
                        settings.default_tab_stop_pt = twip_to_pt(v);
                    }
                }
                "evenAndOddHeaders" => settings.even_and_odd_headers = toggle(c),
                "autoHyphenation" => settings.auto_hyphenation = toggle(c),
                "compat" => {
                    for cs in children(c) {
                        if super::is(cs, W, "compatSetting")
                            && attr(cs, W, "name") == Some("compatibilityMode")
                            && let Some(v) = parse_i64(attr(cs, W, "val"))
                        {
                            settings.compat_mode = v.clamp(0, 255) as u8;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(settings)
    }
}

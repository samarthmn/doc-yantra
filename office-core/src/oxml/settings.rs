//! `word/settings.xml`: the handful of document-wide settings that affect
//! layout or the profile verdict.

use super::{M, W, attr, children, measure_twip_to_pt, parse_i64, toggle};
use crate::Result;

/// Document-wide settings.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Default tab-stop interval in points (Word default 36pt = 720 twips).
    pub default_tab_stop_pt: f64,
    /// Whether even/odd pages use different headers/footers.
    pub even_and_odd_headers: bool,
    /// Whether automatic hyphenation is enabled (out of envelope v1).
    pub auto_hyphenation: bool,
    /// Compatibility mode (`w:compatSetting` `compatibilityMode`; default 15).
    pub compat_mode: u8,
    /// Visible settings deliberately kept fail-closed.
    pub visible_unsupported: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            default_tab_stop_pt: 36.0,
            even_and_odd_headers: false,
            auto_hyphenation: false,
            compat_mode: 15,
            visible_unsupported: Vec::new(),
        }
    }
}

impl Settings {
    /// Parses `word/settings.xml`. An absent part yields [`Settings::default`].
    pub fn parse(xml: &str) -> Result<Settings> {
        let doc = super::parse_xml(xml, "settings.xml")?;
        let root = doc.root_element();
        let mut settings = Settings::default();

        for c in children(root) {
            if c.tag_name().namespace() == Some(M) && c.tag_name().name() == "mathPr" {
                for property in children(c) {
                    let supported_default_font = property.tag_name().namespace() == Some(M)
                        && property.tag_name().name() == "mathFont"
                        && attr(property, M, "val").is_none_or(|font| font == "Cambria Math");
                    if !supported_default_font {
                        settings
                            .visible_unsupported
                            .push(format!("m:mathPr/m:{}", property.tag_name().name()));
                    }
                }
                continue;
            }
            match c.tag_name().name() {
                "defaultTabStop" => {
                    if let Some(value) = measure_twip_to_pt(attr(c, W, "val")) {
                        settings.default_tab_stop_pt = value;
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
                "mirrorMargins" | "gutterAtTop" | "displayBackgroundShape" => settings
                    .visible_unsupported
                    .push(format!("w:{}", c.tag_name().name())),
                // Proofing, view, revision, locale, and save metadata do not paint.
                _ => {}
            }
        }
        Ok(settings)
    }
}

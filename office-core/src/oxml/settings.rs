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
    /// Whether the file carries Word's revision-save-id table (`w:rsids`) —
    /// a reliable marker that Word itself authored/saved the document, and
    /// therefore that AutoFit results were written back into `tblGrid`.
    pub word_authored: bool,
    /// `w:themeFontLang/@w:eastAsia`: selects the theme's East Asian script
    /// face (see [`super::theme::Theme::east_asia_lang`]).
    pub theme_font_lang_east_asia: Option<String>,
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
            word_authored: false,
            theme_font_lang_east_asia: None,
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
                    let accepted = property.tag_name().namespace() == Some(M)
                        && math_preference_is_renderable(property);
                    if !accepted {
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
                "themeFontLang" => {
                    settings.theme_font_lang_east_asia =
                        attr(c, W, "eastAsia").map(str::to_string)
                }
                "autoHyphenation" => settings.auto_hyphenation = toggle(c),
                "rsids" => settings.word_authored = true,
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

/// Whether one `m:mathPr` preference leaves the supported equation
/// rendering unchanged. Word stamps its defaults into every document that
/// contains math, so the defaults must stay in envelope; values that would
/// change output we cannot produce stay fail-closed.
fn math_preference_is_renderable(property: super::Node) -> bool {
    let value = attr(property, M, "val");
    match property.tag_name().name() {
        "mathFont" => value.is_none_or(|font| font == "Cambria Math"),
        // Equations lay out as unbroken boxes, so preferences that only
        // select where an over-wide equation would break cannot change the
        // rendered output.
        "brkBin" | "brkBinSub" | "wrapIndent" => true,
        "smallFrac" => matches!(value, Some("0" | "false" | "off")),
        "dispDef" => value.is_none_or(|toggle| matches!(toggle, "1" | "true" | "on")),
        "lMargin" | "rMargin" => value == Some("0"),
        // `centerGroup` and `center` both match the centered display
        // default; left/right defaults would move every equation.
        "defJc" => matches!(value, Some("center" | "centerGroup")),
        // Limits always render under/over. For integrals Word's `subSup`
        // default is the frozen baseline divergence; `undOvr` matches the
        // rendering exactly. A `subSup` n-ary default would move Σ limits
        // beyond that baseline, so it stays out of envelope.
        "intLim" => matches!(value, Some("subSup" | "undOvr")),
        "naryLim" => value == Some("undOvr"),
        _ => false,
    }
}

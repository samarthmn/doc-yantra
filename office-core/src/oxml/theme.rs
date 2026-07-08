//! `theme1.xml`: the color scheme, the major/minor Latin fonts, and DrawingML
//! color resolution (`resolve_color`) shared by WML `themeColor` attributes and
//! PPTX `a:schemeClr`.

use super::{A, Node, Rgb, Rgba, attr, child, children, parse_f64};
use crate::{OfficeError, Result};
use std::collections::HashMap;

/// A theme: its named color slots and the major/minor Latin typefaces.
#[derive(Debug, Clone, Default)]
pub struct Theme {
    colors: HashMap<String, Rgb>,
    /// The major (heading) Latin typeface (`+mj-lt`).
    pub major_latin: Option<String>,
    /// The minor (body) Latin typeface (`+mn-lt`).
    pub minor_latin: Option<String>,
}

impl Theme {
    /// Parses `word/theme/theme1.xml` (or a slide theme). An absent part yields
    /// an empty theme (color lookups miss; fonts fall back to Calibri).
    pub fn parse(xml: &str) -> Result<Theme> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| OfficeError::Invalid(format!("theme XML: {e}")))?;
        let root = doc.root_element();
        let mut theme = Theme::default();

        let Some(elements) = child(root, A, "themeElements") else {
            return Ok(theme);
        };
        if let Some(clr_scheme) = child(elements, A, "clrScheme") {
            for slot in children(clr_scheme) {
                let name = canonical_slot(slot.tag_name().name());
                if let Some(rgb) = parse_scheme_color_value(slot) {
                    theme.colors.insert(name, rgb);
                }
            }
        }
        if let Some(font_scheme) = child(elements, A, "fontScheme") {
            theme.major_latin = latin_typeface(font_scheme, "majorFont");
            theme.minor_latin = latin_typeface(font_scheme, "minorFont");
        }
        Ok(theme)
    }

    /// Looks up a theme color slot (`accent1`, `dk1`, `hlink`, …). Handles the
    /// `window`/`windowText` aliases (→ `lt1`/`dk1`) and the `tx1`/`bg1` style
    /// scheme-map names.
    pub fn color(&self, slot: &str) -> Option<Rgb> {
        self.colors.get(&canonical_slot(slot)).copied()
    }
}

/// Canonicalizes a color-slot name to the `clrScheme` element name.
fn canonical_slot(name: &str) -> String {
    let n = match name {
        "window" => "lt1",
        "windowText" => "dk1",
        "tx1" => "dk1",
        "bg1" => "lt1",
        "tx2" => "dk2",
        "bg2" => "lt2",
        "hyperlink" => "hlink",
        "followedHyperlink" => "folHlink",
        other => other,
    };
    n.to_string()
}

/// Reads the RGB value from a `clrScheme` slot (`a:srgbClr`/`a:sysClr`).
fn parse_scheme_color_value(slot: Node) -> Option<Rgb> {
    for c in children(slot) {
        if super::is(c, A, "srgbClr") {
            return attr(c, A, "val").and_then(Rgb::from_hex);
        }
        if super::is(c, A, "sysClr") {
            return attr(c, A, "lastClr")
                .and_then(Rgb::from_hex)
                .or_else(|| attr(c, A, "val").and_then(Rgb::from_hex));
        }
    }
    None
}

fn latin_typeface(font_scheme: Node, bucket: &str) -> Option<String> {
    child(font_scheme, A, bucket)
        .and_then(|b| child(b, A, "latin"))
        .and_then(|l| attr(l, A, "typeface").map(str::to_string))
        .filter(|s| !s.is_empty())
}

/// A base color reference for [`resolve_color`].
#[derive(Debug, Clone)]
pub enum ColorRef {
    /// An explicit sRGB value.
    Srgb(Rgb),
    /// A theme slot name (resolved through [`Theme::color`]).
    Scheme(String),
}

/// A DrawingML color transform, applied in order of appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorTransform {
    /// Lightens toward white: `c' = c·t + (1−t)·255`.
    Tint(f64),
    /// Darkens toward black: `c' = c·s`.
    Shade(f64),
    /// Scales HSL lightness: `L' = L·m`.
    LumMod(f64),
    /// Offsets HSL lightness: `L' = L + o`.
    LumOff(f64),
    /// Scales HSL saturation: `S' = S·m`.
    SatMod(f64),
    /// Sets opacity (0.0–1.0).
    Alpha(f64),
}

/// Resolves a base color plus its transforms into a concrete [`Rgba`].
///
/// Transforms apply in listed order. `tint`/`shade` operate in sRGB;
/// `lumMod`/`lumOff`/`satMod` convert to HSL for that one op and back; `alpha`
/// records opacity. An unresolved scheme color falls back to black.
pub fn resolve_color(base: ColorRef, transforms: &[ColorTransform], theme: &Theme) -> Rgba {
    let mut rgb = match base {
        ColorRef::Srgb(c) => c,
        ColorRef::Scheme(slot) => theme.color(&slot).unwrap_or(Rgb::new(0, 0, 0)),
    };
    let mut alpha = 1.0_f32;

    for t in transforms {
        match *t {
            ColorTransform::Tint(v) => {
                rgb = Rgb::new(tint(rgb.r, v), tint(rgb.g, v), tint(rgb.b, v));
            }
            ColorTransform::Shade(v) => {
                rgb = Rgb::new(shade(rgb.r, v), shade(rgb.g, v), shade(rgb.b, v));
            }
            ColorTransform::LumMod(m) => {
                let (h, s, l) = rgb_to_hsl(rgb);
                rgb = hsl_to_rgb(h, s, (l * m).clamp(0.0, 1.0));
            }
            ColorTransform::LumOff(o) => {
                let (h, s, l) = rgb_to_hsl(rgb);
                rgb = hsl_to_rgb(h, s, (l + o).clamp(0.0, 1.0));
            }
            ColorTransform::SatMod(m) => {
                let (h, s, l) = rgb_to_hsl(rgb);
                rgb = hsl_to_rgb(h, (s * m).clamp(0.0, 1.0), l);
            }
            ColorTransform::Alpha(a) => alpha = a.clamp(0.0, 1.0) as f32,
        }
    }
    Rgba {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
        a: alpha,
    }
}

/// Parses a DrawingML color element (`a:srgbClr`, `a:schemeClr`, `a:sysClr`)
/// into a base ref and its child transforms. Returns `None` for other elements.
pub fn parse_color_element(node: Node) -> Option<(ColorRef, Vec<ColorTransform>)> {
    let base = match node.tag_name().name() {
        "srgbClr" => ColorRef::Srgb(attr(node, A, "val").and_then(Rgb::from_hex)?),
        "schemeClr" => ColorRef::Scheme(attr(node, A, "val")?.to_string()),
        "sysClr" => ColorRef::Srgb(
            attr(node, A, "lastClr")
                .and_then(Rgb::from_hex)
                .unwrap_or(Rgb::new(0, 0, 0)),
        ),
        _ => return None,
    };
    let mut transforms = Vec::new();
    for c in children(node) {
        let pct = |a: &str| parse_f64(attr(c, A, a)).map(|v| v / 100_000.0);
        match c.tag_name().name() {
            "tint" => transforms.extend(pct("val").map(ColorTransform::Tint)),
            "shade" => transforms.extend(pct("val").map(ColorTransform::Shade)),
            "lumMod" => transforms.extend(pct("val").map(ColorTransform::LumMod)),
            "lumOff" => transforms.extend(pct("val").map(ColorTransform::LumOff)),
            "satMod" => transforms.extend(pct("val").map(ColorTransform::SatMod)),
            "alpha" => transforms.extend(pct("val").map(ColorTransform::Alpha)),
            _ => {}
        }
    }
    Some((base, transforms))
}

fn tint(c: u8, t: f64) -> u8 {
    (c as f64 * t + (1.0 - t) * 255.0).round().clamp(0.0, 255.0) as u8
}

fn shade(c: u8, s: f64) -> u8 {
    (c as f64 * s).round().clamp(0.0, 255.0) as u8
}

/// sRGB → HSL, all outputs in 0.0–1.0 (hue normalized to a fraction of a turn).
fn rgb_to_hsl(c: Rgb) -> (f64, f64, f64) {
    let r = c.r as f64 / 255.0;
    let g = c.g as f64 / 255.0;
    let b = c.b as f64 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f64::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

/// HSL (0.0–1.0) → sRGB.
fn hsl_to_rgb(h: f64, s: f64, l: f64) -> Rgb {
    if s.abs() < f64::EPSILON {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return Rgb::new(v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let r = hue_to_channel(p, q, h + 1.0 / 3.0);
    let g = hue_to_channel(p, q, h);
    let b = hue_to_channel(p, q, h - 1.0 / 3.0);
    Rgb::new(to_u8(r), to_u8(g), to_u8(b))
}

fn hue_to_channel(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

fn to_u8(v: f64) -> u8 {
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme_with(slot: &str, rgb: Rgb) -> Theme {
        let mut t = Theme::default();
        t.colors.insert(slot.to_string(), rgb);
        t
    }

    #[test]
    fn shade_halves_channels() {
        // 4472C4 shaded 50% → each channel halved: 22 39 62.
        let out = resolve_color(
            ColorRef::Srgb(Rgb::new(0x44, 0x72, 0xC4)),
            &[ColorTransform::Shade(0.5)],
            &Theme::default(),
        );
        assert_eq!(out.rgb(), Rgb::new(0x22, 0x39, 0x62));
    }

    #[test]
    fn tint_lightens_toward_white() {
        // 4472C4 tinted 60%: c*0.6 + 0.4*255 → 8F AA DC (rounded).
        let out = resolve_color(
            ColorRef::Srgb(Rgb::new(0x44, 0x72, 0xC4)),
            &[ColorTransform::Tint(0.6)],
            &Theme::default(),
        );
        assert_eq!(out.rgb(), Rgb::new(0x8F, 0xAA, 0xDC));
    }

    #[test]
    fn scheme_color_resolves_through_theme() {
        let theme = theme_with("accent1", Rgb::new(0x44, 0x72, 0xC4));
        let out = resolve_color(ColorRef::Scheme("accent1".into()), &[], &theme);
        assert_eq!(out.rgb(), Rgb::new(0x44, 0x72, 0xC4));
    }

    #[test]
    fn lummod_darkens() {
        // accent1 4472C4 with lumMod 60%: darker than the source (L·0.6),
        // and near the ~2A4C86 region PowerPoint produces. Assert darkening
        // plus a tolerance band rather than an exact byte match.
        let theme = theme_with("accent1", Rgb::new(0x44, 0x72, 0xC4));
        let out = resolve_color(
            ColorRef::Scheme("accent1".into()),
            &[ColorTransform::LumMod(0.6)],
            &theme,
        )
        .rgb();
        let (_, _, l_in) = rgb_to_hsl(Rgb::new(0x44, 0x72, 0xC4));
        let (_, _, l_out) = rgb_to_hsl(out);
        assert!(l_out < l_in, "lumMod 0.6 must lower lightness");
        assert!((l_out - l_in * 0.6).abs() < 0.02, "L' ≈ L·0.6");
    }

    #[test]
    fn lummod_100_lumoff_0_is_identity() {
        let src = Rgb::new(0x44, 0x72, 0xC4);
        let out = resolve_color(
            ColorRef::Srgb(src),
            &[ColorTransform::LumMod(1.0), ColorTransform::LumOff(0.0)],
            &Theme::default(),
        )
        .rgb();
        for (a, b) in [(out.r, src.r), (out.g, src.g), (out.b, src.b)] {
            assert!((a as i32 - b as i32).abs() <= 1, "hsl round-trip within 1");
        }
    }

    #[test]
    fn window_aliases_to_lt1() {
        let theme = theme_with("lt1", Rgb::new(255, 255, 255));
        assert_eq!(theme.color("window"), Some(Rgb::new(255, 255, 255)));
        assert_eq!(theme.color("bg1"), Some(Rgb::new(255, 255, 255)));
    }
}

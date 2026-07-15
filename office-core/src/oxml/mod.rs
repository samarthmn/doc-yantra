//! Typed OOXML models (parse only — no layout) plus the shared XML conventions
//! every model in this crate follows.
//!
//! Parsing uses `roxmltree` DOM (documents are size-capped by [`crate::opc`] so
//! DOM is safe). **Namespaces are matched on `(namespace_uri, local_name)`,
//! never on prefixes** — the URI constants below are the single source.
//!
//! ## Fail-closed walker rule (applies to every model here)
//!
//! When iterating the children of a content element: known elements are handled,
//! elements on that model's IGNORE list are skipped, and **anything else is an
//! [`OfficeError::Unsupported`]`{UnknownElement}`**. `mc:AlternateContent` takes
//! its `mc:Fallback` branch when present, else it is unsupported.

use crate::{OfficeError, Result, UnsupportedCode};

/// Large enough for complex Office parts; bounds roxmltree DOM allocation and walks.
pub const MAX_XML_NODES: usize = 1_000_000;
/// Office markup is shallow in practice; deeper input is hostile or malformed.
pub const MAX_XML_DEPTH: usize = 256;

/// Parses one size-capped OPC part and applies structural DOM budgets.
pub fn parse_xml<'a>(xml: &'a str, label: &str) -> Result<roxmltree::Document<'a>> {
    guard_xml_budget(xml, label)?;
    let doc = roxmltree::Document::parse(xml)
        .map_err(|error| OfficeError::Invalid(format!("{label} XML: {error}")))?;
    Ok(doc)
}

/// Checks budgets without claiming malformed auxiliary XML; callers that know
/// part context remain responsible for producing the eventual parse error.
pub(crate) fn guard_xml_budget(xml: &str, label: &str) -> Result<()> {
    guard_xml_depth(xml.as_bytes(), label)?;
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Ok(());
    };
    let mut nodes = 0usize;
    for _node in doc.descendants() {
        nodes += 1;
        if nodes > MAX_XML_NODES {
            return Err(OfficeError::unsupported(
                UnsupportedCode::DecompressionLimit,
                format!("{label} XML node count exceeds limit {MAX_XML_NODES}"),
            ));
        }
    }
    Ok(())
}

fn guard_xml_depth(bytes: &[u8], label: &str) -> Result<()> {
    let mut depth = 0usize;
    let mut cursor = 0usize;
    while let Some(relative) = bytes[cursor..].iter().position(|byte| *byte == b'<') {
        let start = cursor + relative;
        let mut end = start + 1;
        let mut quote = None;
        while end < bytes.len() {
            let byte = bytes[end];
            if let Some(delimiter) = quote {
                if byte == delimiter {
                    quote = None;
                }
            } else if byte == b'\'' || byte == b'"' {
                quote = Some(byte);
            } else if byte == b'>' {
                break;
            }
            end += 1;
        }
        if end == bytes.len() {
            break;
        }
        let body = &bytes[start + 1..end];
        if body.starts_with(b"/") {
            depth = depth.saturating_sub(1);
        } else if !body.starts_with(b"!")
            && !body.starts_with(b"?")
            && body
                .iter()
                .rev()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_none_or(|byte| *byte != b'/')
        {
            depth += 1;
            if depth > MAX_XML_DEPTH {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::DecompressionLimit,
                    format!("{label} XML depth exceeds limit {MAX_XML_DEPTH}"),
                ));
            }
        }
        cursor = end + 1;
    }
    Ok(())
}

pub mod dml;
pub mod font_table;
pub mod numbering;
pub mod omml;
pub mod pml;
pub mod settings;
pub mod sml;
pub mod styles;
pub mod table_style;
pub mod theme;
pub mod wml;

/// WordprocessingML main namespace.
pub const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
/// DrawingML main namespace.
pub const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
/// PresentationML main namespace.
pub const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
/// SpreadsheetML main namespace.
pub const S: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
/// Relationship references inside document parts (`r:id`, `r:embed`).
pub const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
/// WordprocessingML drawing wrapper namespace (`wp:inline`, `wp:anchor`).
pub const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
/// Office Math namespace.
pub const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
/// Markup-compatibility namespace (`mc:AlternateContent`).
pub const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
/// Package relationships namespace (used inside `.rels` parts).
pub const PKG_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
/// Content-types namespace (used in `[Content_Types].xml`).
pub const CONTENT_TYPES: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
/// Legacy VML namespace (`w:pict` shapes — unsupported v1).
pub const VML: &str = "urn:schemas-microsoft-com:vml";

#[cfg(test)]
mod resource_budget_tests {
    use super::*;

    #[test]
    fn deeply_nested_xml_fails_with_typed_limit() {
        let started = std::time::Instant::now();
        let xml = format!("{}x{}", "<n>".repeat(5_000), "</n>".repeat(5_000));
        let error = parse_xml(&xml, "hostile").expect_err("depth limit");
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::DecompressionLimit,
                ..
            }
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn hundred_nested_drawing_groups_fail_with_typed_limit() {
        let groups = "<p:grpSp><p:grpSpPr/>".repeat(100);
        let closes = "</p:grpSp>".repeat(100);
        let xml = format!(
            r#"<p:sld xmlns:p="{P}" xmlns:a="{A}"><p:cSld><p:spTree>{groups}{closes}</p:spTree></p:cSld></p:sld>"#
        );
        let error = pml::Slide::parse(&xml).expect_err("group limit");
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::DecompressionLimit,
                ..
            }
        ));
    }
}

// --- Unit conversions -------------------------------------------------------

/// English Metric Units → points (914_400 EMU per inch, 12_700 per point).
pub fn emu_to_pt(emu: i64) -> f64 {
    emu as f64 / 12_700.0
}

/// Twips (twentieths of a point) → points.
pub fn twip_to_pt(twips: i64) -> f64 {
    twips as f64 / 20.0
}

/// Half-points (the `w:sz` font-size unit) → points.
pub fn half_point_to_pt(v: i64) -> f64 {
    v as f64 / 2.0
}

/// Eighth-points (the border `w:sz` unit) → points.
pub fn eighth_point_to_pt(v: i64) -> f64 {
    v as f64 / 8.0
}

// --- Table width units ------------------------------------------------------

/// A `w:tblW` / `w:tcW` measurement, discriminated by its `w:type`.
///
/// `dxa` = twips, `pct` = fiftieths of a percent (5000 = 100%), `auto`/`nil`
/// carry no explicit width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TblWidth {
    /// Absolute width in points (from `dxa` twips).
    Pt(f64),
    /// Percentage of the available width, 0.0–1.0 (from `pct`).
    Pct(f64),
    /// `auto` — width determined by content/grid.
    Auto,
    /// `nil` / absent — no preferred width.
    Nil,
}

impl TblWidth {
    /// Parses a `(w:w, w:type)` pair. Absent type defaults to `dxa`.
    pub fn parse(w: Option<&str>, ty: Option<&str>) -> TblWidth {
        let raw = w.and_then(|s| s.trim().parse::<f64>().ok());
        match ty.unwrap_or("dxa") {
            "dxa" => raw.map(|v| TblWidth::Pt(v / 20.0)).unwrap_or(TblWidth::Nil),
            "pct" => raw
                .map(|v| TblWidth::Pct(v / 5000.0))
                .unwrap_or(TblWidth::Nil),
            "auto" => TblWidth::Auto,
            _ => TblWidth::Nil,
        }
    }
}

// --- Colors -----------------------------------------------------------------

/// An opaque sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// Constructs an opaque color.
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// Parses a 6-hex-digit `RRGGBB` string (case-insensitive). Returns `None`
    /// for `auto` or malformed input.
    pub fn from_hex(s: &str) -> Option<Rgb> {
        let s = s.trim();
        if s.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some(Rgb { r, g, b })
    }
}

/// An sRGB color with an alpha (opacity) channel, 0.0–1.0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Opacity, 0.0 (transparent) – 1.0 (opaque).
    pub a: f32,
}

impl Rgba {
    /// Opaque `Rgba` from an [`Rgb`].
    pub const fn opaque(c: Rgb) -> Rgba {
        Rgba {
            r: c.r,
            g: c.g,
            b: c.b,
            a: 1.0,
        }
    }

    /// The opaque [`Rgb`] part (drops alpha).
    pub const fn rgb(self) -> Rgb {
        Rgb {
            r: self.r,
            g: self.g,
            b: self.b,
        }
    }
}

// --- roxmltree helpers ------------------------------------------------------

/// Node/attribute types this crate uses from `roxmltree`.
pub use roxmltree::{Document as XmlDocument, Node};

/// Returns the value of the attribute with local name `local`, **preferring**
/// the one in namespace `ns` and falling back to an unqualified attribute only
/// when no namespaced one exists. The preference matters when an element carries
/// both (e.g. `p:sldId` has an unqualified `id` and a namespaced `r:id`).
pub fn attr<'a>(node: Node<'a, '_>, ns: &str, local: &str) -> Option<&'a str> {
    node.attributes()
        .find_map(|a| (a.name() == local && a.namespace() == Some(ns)).then_some(a.value()))
        .or_else(|| {
            node.attributes()
                .find_map(|a| (a.name() == local && a.namespace().is_none()).then_some(a.value()))
        })
}

/// Returns the first attribute matching `local` regardless of namespace.
pub fn attr_local<'a>(node: Node<'a, '_>, local: &str) -> Option<&'a str> {
    node.attributes()
        .find_map(|a| (a.name() == local).then_some(a.value()))
}

/// Iterates the element children of `node` (skips text/comment nodes).
pub fn children<'a, 'input>(node: Node<'a, 'input>) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children().filter(|c| c.is_element())
}

/// Finds the first element child in namespace `ns` with local name `local`.
pub fn child<'a, 'input>(
    node: Node<'a, 'input>,
    ns: &str,
    local: &str,
) -> Option<Node<'a, 'input>> {
    children(node).find(|c| is(*c, ns, local))
}

/// True when `node` is an element in namespace `ns` with local name `local`.
pub fn is(node: Node, ns: &str, local: &str) -> bool {
    let n = node.tag_name();
    n.name() == local && n.namespace() == Some(ns)
}

/// Best-effort `prefix:local` display name for diagnostics (`<w:foo>`).
pub fn qname(node: Node) -> String {
    let name = node.tag_name();
    let prefix = match name.namespace() {
        Some(W) => "w",
        Some(A) => "a",
        Some(P) => "p",
        Some(S) => "s",
        Some(R) => "r",
        Some(WP) => "wp",
        Some(M) => "m",
        Some(MC) => "mc",
        Some(VML) => "v",
        _ => "",
    };
    if prefix.is_empty() {
        name.name().to_string()
    } else {
        format!("{prefix}:{}", name.name())
    }
}

/// The fail-closed error for an element that is neither known nor ignorable.
pub fn unknown_child(parent: &str, child: Node) -> OfficeError {
    OfficeError::unsupported(
        UnsupportedCode::UnknownElement,
        format!("<{}> in <{parent}>", qname(child)),
    )
}

/// Parses an OOXML on/off toggle value (`w:val` semantics): `None`/absent means
/// the toggle is present-and-on; `"0"`, `"false"`, `"off"` mean off; anything
/// else means on.
pub fn on_off(val: Option<&str>) -> bool {
    match val {
        None => true,
        Some(v) => !matches!(v.trim(), "0" | "false" | "off"),
    }
}

/// Reads a `w:val` toggle off an element (e.g. `<w:b/>` or `<w:b w:val="0"/>`).
pub fn toggle(node: Node) -> bool {
    on_off(attr(node, W, "val"))
}

/// Parses a signed integer attribute value, ignoring surrounding whitespace.
pub fn parse_i64(v: Option<&str>) -> Option<i64> {
    v.and_then(|s| s.trim().parse::<i64>().ok())
}

/// Parses a signed measurement attribute that mainstream producers may write
/// with a fractional part (Google Docs: `w:w="-108.0"`, `w:firstLine=
/// "192.00000000000003"`). Counts and ids stay on [`parse_i64`].
pub fn parse_measure(v: Option<&str>) -> Option<f64> {
    v.and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

/// [`parse_measure`] in twips converted to points.
pub fn measure_twip_to_pt(v: Option<&str>) -> Option<f64> {
    parse_measure(v).map(|twips| twips / 20.0)
}

/// Parses a floating-point attribute value.
pub fn parse_f64(v: Option<&str>) -> Option<f64> {
    v.and_then(|s| s.trim().parse::<f64>().ok())
}

/// Resolves an `mc:AlternateContent` node to the children of its `mc:Fallback`
/// branch, or returns `Unsupported` when no fallback exists.
pub fn alternate_content_fallback<'a, 'input>(
    node: Node<'a, 'input>,
) -> Result<Vec<Node<'a, 'input>>> {
    if let Some(fallback) = child(node, MC, "Fallback") {
        Ok(children(fallback).collect())
    } else {
        Err(OfficeError::unsupported(
            UnsupportedCode::UnknownElement,
            "mc:AlternateContent without mc:Fallback",
        ))
    }
}

/// Local names of WML elements that carry no printable content and are safely
/// skipped during a fail-closed walk (invisible bookkeeping, revision markers).
///
/// Note: `w:ins` (inserted tracked text) and `w:sdt` are **not** here — they are
/// unwrapped by the walker, not skipped. `w:del`/`moveFrom` (deleted text) are
/// skipped because Word prints the final view.
pub const WML_IGNORE: &[&str] = &[
    "bookmarkStart",
    "bookmarkEnd",
    "proofErr",
    "permStart",
    "permEnd",
    "commentRangeStart",
    "commentRangeEnd",
    "commentReference",
    "lastRenderedPageBreak",
    "sectPrChange",
    "pPrChange",
    "rPrChange",
    "tblPrChange",
    "tcPrChange",
    "trPrChange",
    "del",
    "moveFrom",
    "moveFromRangeStart",
    "moveFromRangeEnd",
    "moveToRangeStart",
    "moveToRangeEnd",
    "noProof",
    "webHidden",
    "snapToGrid",
    "softHyphen",
];

/// True when a WML element (matched by local name, in the `w` namespace) is on
/// the [`WML_IGNORE`] list.
pub fn wml_ignorable(node: Node) -> bool {
    node.tag_name().namespace() == Some(W) && WML_IGNORE.contains(&node.tag_name().name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_conversions() {
        assert_eq!(emu_to_pt(914_400), 72.0);
        assert_eq!(emu_to_pt(12_700), 1.0);
        assert_eq!(twip_to_pt(1440), 72.0);
        assert_eq!(twip_to_pt(720), 36.0);
        assert_eq!(half_point_to_pt(22), 11.0);
        assert_eq!(eighth_point_to_pt(8), 1.0);
    }

    #[test]
    fn decimal_measurements_parse_to_points_and_reject_non_finite_values() {
        let positive = measure_twip_to_pt(Some("192.00000000000003")).unwrap();
        assert!((positive - 9.6).abs() < 1e-12, "positive={positive}");
        assert_eq!(measure_twip_to_pt(Some("-108.0")), Some(-5.4));
        for invalid in ["abc", "NaN", "inf", "-inf"] {
            assert_eq!(parse_measure(Some(invalid)), None, "invalid={invalid}");
        }
    }

    #[test]
    fn tbl_width_parsing() {
        assert_eq!(
            TblWidth::parse(Some("1440"), Some("dxa")),
            TblWidth::Pt(72.0)
        );
        assert_eq!(
            TblWidth::parse(Some("5000"), Some("pct")),
            TblWidth::Pct(1.0)
        );
        assert_eq!(
            TblWidth::parse(Some("2500"), Some("pct")),
            TblWidth::Pct(0.5)
        );
        assert_eq!(TblWidth::parse(None, Some("auto")), TblWidth::Auto);
        assert_eq!(TblWidth::parse(Some("100"), None), TblWidth::Pt(5.0)); // default dxa
    }

    #[test]
    fn hex_colors() {
        assert_eq!(Rgb::from_hex("4472C4"), Some(Rgb::new(0x44, 0x72, 0xC4)));
        assert_eq!(Rgb::from_hex("ffffff"), Some(Rgb::new(255, 255, 255)));
        assert_eq!(Rgb::from_hex("auto"), None);
        assert_eq!(Rgb::from_hex("12345"), None);
    }

    #[test]
    fn on_off_semantics() {
        assert!(on_off(None));
        assert!(on_off(Some("1")));
        assert!(on_off(Some("true")));
        assert!(!on_off(Some("0")));
        assert!(!on_off(Some("false")));
        assert!(!on_off(Some("off")));
    }
}

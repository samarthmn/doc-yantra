//! `word/numbering.xml`: abstract numbering definitions, concrete `num`
//! instances (with `lvlOverride`), and the number-format helper the list layout
//! uses to render counters.

use super::wml::{Indent, RunProps, TabStop, parse_run_props, parse_tabs};
use super::{Node, W, attr, child, children, measure_twip_to_pt, parse_i64};
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

/// Word numbering levels are indexed from zero through eight.
pub(crate) const MAX_NUMBERING_LEVEL: u32 = 8;
/// Existing fixtures start at 21 or below; one million leaves more than four
/// orders of magnitude of headroom while bounding counter-controlled work.
pub(crate) const MAX_NUMBERING_VALUE: i64 = 1_000_000;
pub(crate) const MIN_NUMBERING_VALUE: i64 = -MAX_NUMBERING_VALUE;

fn numbering_resource_limit(detail: impl Into<String>) -> OfficeError {
    OfficeError::unsupported(UnsupportedCode::ResourceLimit, detail)
}

pub(crate) fn parse_numbering_level(raw: Option<&str>, label: &str) -> Result<u32> {
    let value = parse_bounded_number(raw, 0, 0, i64::from(MAX_NUMBERING_LEVEL), label)?;
    u32::try_from(value).map_err(|_| numbering_resource_limit(format!("{label} exceeds u32")))
}

fn parse_numbering_value(raw: Option<&str>, default: i64, label: &str) -> Result<i64> {
    parse_bounded_number(
        raw,
        default,
        MIN_NUMBERING_VALUE,
        MAX_NUMBERING_VALUE,
        label,
    )
}

fn parse_bounded_number(
    raw: Option<&str>,
    default: i64,
    minimum: i64,
    maximum: i64,
    label: &str,
) -> Result<i64> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    let digits = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    let integer_syntax = !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
    let value = match raw.parse::<i64>() {
        Ok(value) => value,
        Err(_) if integer_syntax => {
            return Err(numbering_resource_limit(format!(
                "{label} exceeds the integer range"
            )));
        }
        Err(_) => {
            return Err(OfficeError::Invalid(format!(
                "{label} is not an integer: {raw}"
            )));
        }
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(numbering_resource_limit(format!(
            "{label} {value} is outside {minimum}..={maximum}"
        )));
    }
    Ok(value)
}

/// A number format (`w:numFmt`). Unlisted formats become [`NumFmt::Other`] and
/// fail closed only if a paragraph actually uses that level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumFmt {
    /// `1, 2, 3, …`
    Decimal,
    /// `01, 02, …` (single leading zero).
    DecimalZero,
    /// Zero-padded decimal from a `w14` custom format (`0001, 0002, …`).
    DecimalPadded(usize),
    /// A bullet — the label is the `lvlText` literal.
    Bullet,
    /// `a, b, …, z, aa, …`
    LowerLetter,
    /// `A, B, …`
    UpperLetter,
    /// `i, ii, iii, …`
    LowerRoman,
    /// `I, II, III, …`
    UpperRoman,
    /// `1st, 2nd, 3rd, …`
    Ordinal,
    /// `one, two, three, …`
    CardinalText,
    /// `first, second, third, …`
    OrdinalText,
    /// Suppress the label.
    None,
    /// An unsupported format; carries the raw `w:val`.
    Other(String),
}

impl NumFmt {
    pub(crate) fn parse(v: &str) -> NumFmt {
        match v {
            "decimal" => NumFmt::Decimal,
            "decimalZero" => NumFmt::DecimalZero,
            "bullet" => NumFmt::Bullet,
            "lowerLetter" => NumFmt::LowerLetter,
            "upperLetter" => NumFmt::UpperLetter,
            "lowerRoman" => NumFmt::LowerRoman,
            "upperRoman" => NumFmt::UpperRoman,
            "ordinal" => NumFmt::Ordinal,
            "cardinalText" => NumFmt::CardinalText,
            "ordinalText" => NumFmt::OrdinalText,
            "none" => NumFmt::None,
            other => NumFmt::Other(other.to_string()),
        }
    }
}

/// The character(s) between a list label and the paragraph text (`w:suff`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LevelSuffix {
    /// Advance to the text indent via a tab (Word default).
    #[default]
    Tab,
    /// A single space.
    Space,
    /// Nothing (label abuts text).
    Nothing,
}

impl LevelSuffix {
    fn parse(v: Option<&str>) -> LevelSuffix {
        match v {
            Some("space") => LevelSuffix::Space,
            Some("nothing") => LevelSuffix::Nothing,
            _ => LevelSuffix::Tab,
        }
    }
}

/// A resolved numbering level definition.
#[derive(Debug, Clone)]
pub struct LevelDef {
    /// The number format.
    pub num_fmt: NumFmt,
    /// The label pattern (`%1.`, `%1.%2`, a bullet glyph, …).
    pub lvl_text: String,
    /// The starting counter value.
    pub start: i64,
    /// The separator after the label.
    pub suffix: LevelSuffix,
    /// Indentation from the level's `pPr`.
    pub indent: Indent,
    /// Typed tab stops from the level's `pPr`.
    pub tabs: Vec<TabStop>,
    /// Provenance of authored left/hanging indentation after level overrides.
    pub indent_provenance: LevelIndentProvenance,
    /// The label's run properties (`lvl` `rPr`).
    pub rpr: RunProps,
    /// Label justification within the hanging-indent area.
    pub justification: LabelJustification,
    /// Legal numbering forces referenced levels to decimal.
    pub legal: bool,
}

/// Source of a numbering level's authored indentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelPropertyOrigin {
    /// The property came from the abstract numbering level.
    AbstractLevel,
    /// The property came from a concrete `w:lvlOverride/w:lvl`.
    LevelOverride,
}

/// Per-property provenance retained with a resolved numbering level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LevelIndentProvenance {
    /// Origin of `w:ind/@w:left` or `@w:start`.
    pub left: Option<LevelPropertyOrigin>,
    /// Origin of `w:ind/@w:hanging`.
    pub hanging: Option<LevelPropertyOrigin>,
}

/// Number-label justification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LabelJustification {
    /// Left/start aligned.
    #[default]
    Start,
    /// Centered.
    Center,
    /// Right/end aligned.
    End,
}

impl Default for LevelDef {
    fn default() -> Self {
        LevelDef {
            num_fmt: NumFmt::Decimal,
            lvl_text: "%1.".to_string(),
            start: 1,
            suffix: LevelSuffix::Tab,
            indent: Indent::default(),
            tabs: Vec::new(),
            indent_provenance: LevelIndentProvenance::default(),
            rpr: RunProps::default(),
            justification: LabelJustification::Start,
            legal: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct AbstractNum {
    levels: HashMap<u32, LevelDef>,
}

#[derive(Debug, Clone)]
struct NumInstance {
    abstract_id: i64,
    start_overrides: HashMap<u32, i64>,
    level_overrides: HashMap<u32, LevelDef>,
}

/// All numbering definitions in a document.
#[derive(Debug, Clone, Default)]
pub struct Numbering {
    abstract_nums: HashMap<i64, AbstractNum>,
    nums: HashMap<i64, NumInstance>,
}

impl Numbering {
    /// Parses `word/numbering.xml`. An absent part yields an empty set.
    pub fn parse(xml: &str) -> Result<Numbering> {
        let doc = super::parse_xml(xml, "numbering.xml")?;
        let root = doc.root_element();
        let mut numbering = Numbering::default();

        for node in children(root) {
            if super::is(node, W, "abstractNum") {
                let Some(id) = parse_i64(attr(node, W, "abstractNumId")) else {
                    continue;
                };
                numbering
                    .abstract_nums
                    .insert(id, parse_abstract_num(node)?);
            } else if super::is(node, W, "num") {
                let Some(id) = parse_i64(attr(node, W, "numId")) else {
                    continue;
                };
                if let Some(inst) = parse_num_instance(node)? {
                    numbering.nums.insert(id, inst);
                }
            }
        }
        Ok(numbering)
    }

    /// Resolves the effective level definition for `(num_id, ilvl)`, applying
    /// any `lvlOverride`. Returns `None` when the reference is dangling.
    pub fn resolve(&self, num_id: i64, ilvl: u32) -> Option<LevelDef> {
        let inst = self.nums.get(&num_id)?;
        let abstract_num = self.abstract_nums.get(&inst.abstract_id)?;
        let mut level = inst
            .level_overrides
            .get(&ilvl)
            .or_else(|| abstract_num.levels.get(&ilvl))
            .cloned()?;
        if let Some(start) = inst.start_overrides.get(&ilvl) {
            level.start = *start;
        }
        Some(level)
    }

    #[cfg(test)]
    pub(crate) fn test_with_levels(levels: &[(i64, u32, LevelDef)]) -> Numbering {
        let mut numbering = Numbering::default();
        for (num_id, ilvl, level) in levels {
            numbering
                .abstract_nums
                .entry(*num_id)
                .or_default()
                .levels
                .insert(*ilvl, level.clone());
            numbering.nums.entry(*num_id).or_insert(NumInstance {
                abstract_id: *num_id,
                start_overrides: HashMap::new(),
                level_overrides: HashMap::new(),
            });
        }
        numbering
    }
}

fn parse_abstract_num(node: Node) -> Result<AbstractNum> {
    let mut abstract_num = AbstractNum::default();
    for lvl in children(node) {
        if !super::is(lvl, W, "lvl") {
            continue;
        }
        let ilvl = parse_numbering_level(attr(lvl, W, "ilvl"), "w:lvl@w:ilvl")?;
        abstract_num
            .levels
            .insert(ilvl, parse_level(lvl, LevelPropertyOrigin::AbstractLevel)?);
    }
    Ok(abstract_num)
}

fn parse_level(lvl: Node, origin: LevelPropertyOrigin) -> Result<LevelDef> {
    let mut def = LevelDef::default();
    for c in children(lvl) {
        match c.tag_name().name() {
            "start" => def.start = parse_numbering_value(attr(c, W, "val"), 1, "w:start@w:val")?,
            "numFmt" => {
                def.num_fmt = attr(c, W, "val")
                    .map(NumFmt::parse)
                    .unwrap_or(NumFmt::Decimal)
            }
            "lvlText" => def.lvl_text = attr(c, W, "val").unwrap_or_default().to_string(),
            "suff" => def.suffix = LevelSuffix::parse(attr(c, W, "val")),
            "pPr" => {
                def.indent = parse_level_indent(c);
                def.tabs = child(c, W, "tabs").map(parse_tabs).unwrap_or_default();
                def.indent_provenance = LevelIndentProvenance {
                    left: def.indent.left_pt.map(|_| origin),
                    hanging: def.indent.hanging_pt.map(|_| origin),
                };
            }
            "rPr" => def.rpr = parse_run_props(c),
            "lvlJc" => {
                def.justification = match attr(c, W, "val") {
                    Some("center") => LabelJustification::Center,
                    Some("right" | "end") => LabelJustification::End,
                    _ => LabelJustification::Start,
                }
            }
            "isLgl" => def.legal = super::toggle(c),
            // Word ≥2010 stores extended formats as `mc:Choice Requires=
            // "w14"` with a plain-decimal `mc:Fallback`. Real Word renders
            // the Choice, so honor the custom formats we can reproduce and
            // fail closed (via `NumFmt::Other`) on the ones we cannot.
            "AlternateContent" => {
                if let Some(fmt) = alternate_num_fmt(c) {
                    def.num_fmt = fmt;
                }
            }
            _ => {}
        }
    }
    Ok(def)
}

fn alternate_num_fmt(node: Node) -> Option<NumFmt> {
    for choice in children(node).filter(|c| c.tag_name().name() == "Choice") {
        let Some(num_fmt) = child(choice, W, "numFmt") else {
            continue;
        };
        return Some(match attr(num_fmt, W, "val") {
            Some("custom") => {
                let format = attr(num_fmt, W, "format").unwrap_or_default();
                custom_num_fmt(format)
            }
            Some(value) => NumFmt::parse(value),
            None => NumFmt::Decimal,
        });
    }
    children(node)
        .find(|c| c.tag_name().name() == "Fallback")
        .and_then(|fallback| child(fallback, W, "numFmt"))
        .and_then(|num_fmt| attr(num_fmt, W, "val"))
        .map(NumFmt::parse)
}

/// Maps a `w14` custom format string to a supported format. The zero-padded
/// decimal family (`"0001, 0002, 0003, ..."`) is the only one reproduced;
/// everything else carries the raw format and fails closed if used.
fn custom_num_fmt(format: &str) -> NumFmt {
    let first = format.split(',').next().unwrap_or_default().trim();
    if first.len() > 1
        && first.ends_with('1')
        && first[..first.len() - 1].bytes().all(|byte| byte == b'0')
    {
        NumFmt::DecimalPadded(first.len())
    } else {
        NumFmt::Other(format!("custom:{format}"))
    }
}

fn parse_level_indent(ppr: Node) -> Indent {
    let mut indent = Indent::default();
    if let Some(ind) = child(ppr, W, "ind") {
        let g = |a: &str| measure_twip_to_pt(attr(ind, W, a));
        indent.left_pt = g("left").or_else(|| g("start"));
        indent.right_pt = g("right").or_else(|| g("end"));
        indent.first_line_pt = g("firstLine");
        indent.hanging_pt = g("hanging");
    }
    indent
}

fn parse_num_instance(node: Node) -> Result<Option<NumInstance>> {
    let Some(abstract_id) =
        child(node, W, "abstractNumId").and_then(|n| parse_i64(attr(n, W, "val")))
    else {
        return Ok(None);
    };
    let mut inst = NumInstance {
        abstract_id,
        start_overrides: HashMap::new(),
        level_overrides: HashMap::new(),
    };
    for ov in children(node) {
        if !super::is(ov, W, "lvlOverride") {
            continue;
        }
        let ilvl = parse_numbering_level(attr(ov, W, "ilvl"), "w:lvlOverride@w:ilvl")?;
        if let Some(start_node) = child(ov, W, "startOverride") {
            let start =
                parse_numbering_value(attr(start_node, W, "val"), 1, "w:startOverride@w:val")?;
            inst.start_overrides.insert(ilvl, start);
        }
        if let Some(lvl) = child(ov, W, "lvl") {
            let nested_ilvl =
                parse_numbering_level(attr(lvl, W, "ilvl"), "w:lvlOverride/w:lvl@w:ilvl")?;
            if nested_ilvl != ilvl {
                return Err(OfficeError::Invalid(format!(
                    "w:lvlOverride/w:lvl@w:ilvl {nested_ilvl} does not match w:lvlOverride@w:ilvl {ilvl}"
                )));
            }
            inst.level_overrides
                .insert(ilvl, parse_level(lvl, LevelPropertyOrigin::LevelOverride)?);
        }
    }
    Ok(Some(inst))
}

/// Formats a counter `n` under a numeric [`NumFmt`].
///
/// Bullets and unsupported formats return `Unsupported{UnknownElement}` — the
/// caller uses the `lvlText` literal for bullets and fails closed otherwise.
pub fn format_number(fmt: &NumFmt, n: i64) -> Result<String> {
    match fmt {
        NumFmt::Decimal => Ok(n.to_string()),
        NumFmt::DecimalZero => Ok(if (0..10).contains(&n) {
            format!("0{n}")
        } else {
            n.to_string()
        }),
        NumFmt::DecimalPadded(width) => Ok(if n >= 0 {
            format!("{n:0width$}", width = *width)
        } else {
            n.to_string()
        }),
        NumFmt::LowerLetter => Ok(bijective_base26(n, false)),
        NumFmt::UpperLetter => Ok(bijective_base26(n, true)),
        NumFmt::LowerRoman => Ok(to_roman(n, false)),
        NumFmt::UpperRoman => Ok(to_roman(n, true)),
        NumFmt::Ordinal => Ok(ordinal_decimal(n)),
        NumFmt::CardinalText => Ok(cardinal_text(n)),
        NumFmt::OrdinalText => Ok(ordinal_text(n)),
        NumFmt::None => Ok(String::new()),
        NumFmt::Bullet => Err(OfficeError::unsupported(
            UnsupportedCode::UnknownElement,
            "format_number called on bullet",
        )),
        NumFmt::Other(v) => Err(OfficeError::unsupported(
            UnsupportedCode::UnknownElement,
            format!("numFmt:{v}"),
        )),
    }
}

fn ordinal_decimal(n: i64) -> String {
    let magnitude = n.unsigned_abs();
    let suffix = if (11..=13).contains(&(magnitude % 100)) {
        "th"
    } else {
        match magnitude % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

fn cardinal_text(n: i64) -> String {
    let magnitude = n.unsigned_abs();
    let formatted = if magnitude > 999_999 {
        magnitude.to_string()
    } else {
        cardinal_under_million(magnitude)
    };
    if n < 0 {
        format!("minus {formatted}")
    } else {
        formatted
    }
}

fn cardinal_under_million(n: u64) -> String {
    if n >= 1_000 {
        let rest = n % 1_000;
        let head = format!("{} thousand", cardinal_under_thousand(n / 1_000));
        return if rest == 0 {
            head
        } else {
            format!("{head} {}", cardinal_under_thousand(rest))
        };
    }
    cardinal_under_thousand(n)
}

fn cardinal_under_thousand(n: u64) -> String {
    const SMALL: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    if n < 20 {
        return SMALL[n as usize].to_string();
    }
    if n < 100 {
        let rest = n % 10;
        return if rest == 0 {
            TENS[(n / 10) as usize].to_string()
        } else {
            format!("{}-{}", TENS[(n / 10) as usize], SMALL[rest as usize])
        };
    }
    let rest = n % 100;
    let head = format!("{} hundred", SMALL[(n / 100) as usize]);
    if rest == 0 {
        head
    } else {
        format!("{head} {}", cardinal_under_thousand(rest))
    }
}

fn ordinal_text(n: i64) -> String {
    const DIRECT: [&str; 20] = [
        "zeroth",
        "first",
        "second",
        "third",
        "fourth",
        "fifth",
        "sixth",
        "seventh",
        "eighth",
        "ninth",
        "tenth",
        "eleventh",
        "twelfth",
        "thirteenth",
        "fourteenth",
        "fifteenth",
        "sixteenth",
        "seventeenth",
        "eighteenth",
        "nineteenth",
    ];
    if !(0..=999_999).contains(&n) {
        return ordinal_decimal(n);
    }
    if n < 20 {
        return DIRECT[n as usize].to_string();
    }
    let cardinal = cardinal_text(n);
    let (stem, suffix) = cardinal
        .rsplit_once([' ', '-'])
        .unwrap_or(("", cardinal.as_str()));
    let last = match suffix {
        "one" => "first".to_string(),
        "two" => "second".to_string(),
        "three" => "third".to_string(),
        "five" => "fifth".to_string(),
        "eight" => "eighth".to_string(),
        "nine" => "ninth".to_string(),
        "twelve" => "twelfth".to_string(),
        word if word.ends_with('y') => format!("{}ieth", &word[..word.len() - 1]),
        word => format!("{word}th"),
    };
    if stem.is_empty() {
        last
    } else {
        let sep = if cardinal.as_bytes().get(stem.len()) == Some(&b'-') {
            '-'
        } else {
            ' '
        };
        format!("{stem}{sep}{last}")
    }
}

/// Bijective base-26: 1→a, 26→z, 27→aa (Word's `lowerLetter`/`upperLetter`).
fn bijective_base26(n: i64, upper: bool) -> String {
    if n <= 0 {
        return n.to_string();
    }
    let base = if upper { b'A' } else { b'a' };
    let mut n = n;
    let mut out = Vec::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        out.push(base + rem);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Roman numerals for 1..=3999; out-of-range falls back to decimal.
fn to_roman(n: i64, upper: bool) -> String {
    if n <= 0 || n > 3999 {
        return n.to_string();
    }
    const VALUES: [(i64, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut n = n;
    let mut out = String::new();
    for (val, sym) in VALUES {
        while n >= val {
            out.push_str(sym);
            n -= val;
        }
    }
    if upper { out.to_ascii_uppercase() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formats() {
        assert_eq!(format_number(&NumFmt::Decimal, 7).unwrap(), "7");
        assert_eq!(format_number(&NumFmt::DecimalZero, 3).unwrap(), "03");
        assert_eq!(format_number(&NumFmt::DecimalZero, 12).unwrap(), "12");
        assert_eq!(format_number(&NumFmt::LowerLetter, 1).unwrap(), "a");
        assert_eq!(format_number(&NumFmt::LowerLetter, 26).unwrap(), "z");
        assert_eq!(format_number(&NumFmt::LowerLetter, 27).unwrap(), "aa");
        assert_eq!(format_number(&NumFmt::UpperLetter, 28).unwrap(), "AB");
        assert_eq!(format_number(&NumFmt::LowerRoman, 4).unwrap(), "iv");
        assert_eq!(format_number(&NumFmt::UpperRoman, 2024).unwrap(), "MMXXIV");
        assert!(format_number(&NumFmt::Bullet, 1).is_err());
        assert!(format_number(&NumFmt::Other("aiueoFullWidth".into()), 1).is_err());
    }

    #[test]
    fn wave3_number_formats_render_instead_of_failing_closed() {
        assert_eq!(format_number(&NumFmt::parse("ordinal"), 1).unwrap(), "1st");
        assert_eq!(
            format_number(&NumFmt::parse("ordinal"), 12).unwrap(),
            "12th"
        );
        assert_eq!(
            format_number(&NumFmt::parse("cardinalText"), 42).unwrap(),
            "forty-two"
        );
        assert_eq!(
            format_number(&NumFmt::parse("ordinalText"), 21).unwrap(),
            "twenty-first"
        );
        assert_eq!(format_number(&NumFmt::parse("none"), 9).unwrap(), "");
    }

    #[test]
    fn ordinal_decimal_formats_i64_min_without_overflow() {
        assert_eq!(
            format_number(&NumFmt::Ordinal, i64::MIN).expect("format ordinal"),
            "-9223372036854775808th"
        );
    }

    #[test]
    fn cardinal_text_formats_i64_min_without_recursive_negation() {
        assert_eq!(
            format_number(&NumFmt::CardinalText, i64::MIN).expect("format cardinal text"),
            "minus 9223372036854775808"
        );
    }

    #[test]
    fn ordinal_text_formats_i64_min_without_overflow() {
        assert_eq!(
            format_number(&NumFmt::OrdinalText, i64::MIN).expect("format ordinal text"),
            "-9223372036854775808th"
        );
    }

    #[test]
    fn numbering_rejects_levels_above_eight() {
        let xml = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="9"/></w:abstractNum></w:numbering>"#;

        let error = Numbering::parse(xml).expect_err("reject out-of-range ilvl");

        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ));
    }

    #[test]
    fn numbering_rejects_start_above_engine_range() {
        let xml = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1000001"/></w:lvl></w:abstractNum></w:numbering>"#;

        let error = Numbering::parse(xml).expect_err("reject excessive start");

        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ));
    }

    #[test]
    fn numbering_rejects_start_override_above_engine_range() {
        let xml = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"/></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="1000001"/></w:lvlOverride></w:num></w:numbering>"#;

        let error = Numbering::parse(xml).expect_err("reject excessive restart");

        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ));
    }

    #[test]
    fn numbering_rejects_out_of_range_nested_override_level() {
        let xml = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"/></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:lvl w:ilvl="9"/></w:lvlOverride></w:num></w:numbering>"#;

        let error = Numbering::parse(xml).expect_err("reject out-of-range nested override level");

        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ));
    }

    #[test]
    fn numbering_rejects_mismatched_nested_override_level() {
        let xml = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"/></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:lvl w:ilvl="1"/></w:lvlOverride></w:num></w:numbering>"#;

        let error = Numbering::parse(xml).expect_err("reject mismatched nested override level");

        assert!(matches!(error, OfficeError::Invalid(_)));
    }

    #[test]
    fn levels_zero_through_eight_retain_tabs_suffix_and_indent_provenance() {
        let levels = (0..=8)
            .map(|ilvl| {
                let left = 720 + ilvl * 360;
                format!(
                    r#"<w:lvl w:ilvl="{ilvl}"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:pPr><w:tabs><w:tab w:val="left" w:pos="{left}"/></w:tabs><w:ind w:left="{left}" w:hanging="360"/></w:pPr></w:lvl>"#
                )
            })
            .collect::<String>();
        let xml = format!(
            r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0">{levels}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
        );
        let numbering = Numbering::parse(&xml).expect("parse nine numbering levels");
        for ilvl in 0..=8 {
            let level = numbering.resolve(1, ilvl).expect("resolve numbering level");
            let expected = (720.0 + f64::from(ilvl) * 360.0) / 20.0;
            assert_eq!(level.suffix, LevelSuffix::Tab, "omitted w:suff at {ilvl}");
            assert_eq!(level.indent.left_pt, Some(expected));
            assert_eq!(level.indent.hanging_pt, Some(18.0));
            assert_eq!(level.tabs.len(), 1);
            assert_eq!(level.tabs[0].pos_pt, expected);
            assert_eq!(
                level.indent_provenance.left,
                Some(LevelPropertyOrigin::AbstractLevel)
            );
            assert_eq!(
                level.indent_provenance.hanging,
                Some(LevelPropertyOrigin::AbstractLevel)
            );
        }
    }
}

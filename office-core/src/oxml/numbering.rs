//! `word/numbering.xml`: abstract numbering definitions, concrete `num`
//! instances (with `lvlOverride`), and the number-format helper the list layout
//! uses to render counters.

use super::wml::{Indent, RunProps, parse_run_props};
use super::{Node, W, attr, child, children, measure_twip_to_pt, parse_i64};
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

/// A number format (`w:numFmt`). Unlisted formats become [`NumFmt::Other`] and
/// fail closed only if a paragraph actually uses that level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumFmt {
    /// `1, 2, 3, …`
    Decimal,
    /// `01, 02, …` (single leading zero).
    DecimalZero,
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
    /// The label's run properties (`lvl` `rPr`).
    pub rpr: RunProps,
    /// Label justification within the hanging-indent area.
    pub justification: LabelJustification,
    /// Legal numbering forces referenced levels to decimal.
    pub legal: bool,
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
                numbering.abstract_nums.insert(id, parse_abstract_num(node));
            } else if super::is(node, W, "num") {
                let Some(id) = parse_i64(attr(node, W, "numId")) else {
                    continue;
                };
                if let Some(inst) = parse_num_instance(node) {
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
}

fn parse_abstract_num(node: Node) -> AbstractNum {
    let mut abstract_num = AbstractNum::default();
    for lvl in children(node) {
        if !super::is(lvl, W, "lvl") {
            continue;
        }
        let ilvl = parse_i64(attr(lvl, W, "ilvl")).unwrap_or(0).max(0) as u32;
        abstract_num.levels.insert(ilvl, parse_level(lvl));
    }
    abstract_num
}

fn parse_level(lvl: Node) -> LevelDef {
    let mut def = LevelDef::default();
    for c in children(lvl) {
        match c.tag_name().name() {
            "start" => def.start = parse_i64(attr(c, W, "val")).unwrap_or(1),
            "numFmt" => {
                def.num_fmt = attr(c, W, "val")
                    .map(NumFmt::parse)
                    .unwrap_or(NumFmt::Decimal)
            }
            "lvlText" => def.lvl_text = attr(c, W, "val").unwrap_or_default().to_string(),
            "suff" => def.suffix = LevelSuffix::parse(attr(c, W, "val")),
            "pPr" => def.indent = parse_level_indent(c),
            "rPr" => def.rpr = parse_run_props(c),
            "lvlJc" => {
                def.justification = match attr(c, W, "val") {
                    Some("center") => LabelJustification::Center,
                    Some("right" | "end") => LabelJustification::End,
                    _ => LabelJustification::Start,
                }
            }
            "isLgl" => def.legal = super::toggle(c),
            _ => {}
        }
    }
    def
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

fn parse_num_instance(node: Node) -> Option<NumInstance> {
    let abstract_id = child(node, W, "abstractNumId").and_then(|n| parse_i64(attr(n, W, "val")))?;
    let mut inst = NumInstance {
        abstract_id,
        start_overrides: HashMap::new(),
        level_overrides: HashMap::new(),
    };
    for ov in children(node) {
        if !super::is(ov, W, "lvlOverride") {
            continue;
        }
        let ilvl = parse_i64(attr(ov, W, "ilvl")).unwrap_or(0).max(0) as u32;
        if let Some(start) =
            child(ov, W, "startOverride").and_then(|n| parse_i64(attr(n, W, "val")))
        {
            inst.start_overrides.insert(ilvl, start);
        }
        if let Some(lvl) = child(ov, W, "lvl") {
            inst.level_overrides.insert(ilvl, parse_level(lvl));
        }
    }
    Some(inst)
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
    let suffix = if (11..=13).contains(&(n.abs() % 100)) {
        "th"
    } else {
        match n.abs() % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

fn cardinal_text(n: i64) -> String {
    if n < 0 {
        return format!("minus {}", cardinal_text(-n));
    }
    if n > 999_999 {
        return n.to_string();
    }
    cardinal_under_million(n)
}

fn cardinal_under_million(n: i64) -> String {
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

fn cardinal_under_thousand(n: i64) -> String {
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
}

//! The Excel number-format engine. The formatted string is not stored in the
//! file, so this is mandatory for correctness. Implements the builtin format
//! table, common custom patterns (digit placeholders, thousands, percent,
//! scientific, literals), and date/time formatting (1900 epoch with the
//! phantom-leap-day bug reproduced).

use crate::oxml::Rgb;

/// A formatted value plus any color the format applies.
#[derive(Debug, Clone, PartialEq)]
pub struct Formatted {
    /// The display string.
    pub text: String,
    /// A color modifier (`[Red]` etc.), if any.
    pub color: Option<Rgb>,
}

impl Formatted {
    fn plain(text: String) -> Formatted {
        Formatted { text, color: None }
    }
}

/// The builtin format code for a format id (< 164). `None` → treat as `General`.
pub fn builtin_code(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        5 => "$#,##0;($#,##0)",
        6 => "$#,##0;[Red]($#,##0)",
        7 => "$#,##0.00;($#,##0.00)",
        8 => "$#,##0.00;[Red]($#,##0.00)",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        37 => "#,##0;(#,##0)",
        38 => "#,##0;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        41 => "#,##0;(#,##0);-",
        42 => "$#,##0;($#,##0);$-",
        43 => "#,##0.00;(#,##0.00);-",
        44 => "$#,##0.00;($#,##0.00);$-",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

/// Formats a numeric cell value with `code`.
pub fn format_number(value: f64, code: &str, date1904: bool) -> Formatted {
    if code.is_empty() || code.eq_ignore_ascii_case("general") {
        return Formatted::plain(format_general(value));
    }
    let sections: Vec<&str> = split_sections(code);
    let (section, use_abs) = pick_section(value, &sections);
    let (color, body) = strip_color(section);

    if is_fraction_pattern(body) {
        return Formatted {
            text: format_fraction(if use_abs { value.abs() } else { value }, body),
            color,
        };
    }
    if is_elapsed_time_pattern(body) {
        return Formatted {
            text: format_elapsed_time(value, body),
            color,
        };
    }
    if is_date_pattern(body) {
        return Formatted {
            text: format_datetime(value, body, date1904),
            color,
        };
    }
    let n = if use_abs { value.abs() } else { value };
    Formatted {
        text: format_numeric(n, body),
        color,
    }
}

/// Formats a text cell value with `code`. Text uses the 4th section, or any
/// section containing `@`; otherwise it is shown as-is.
pub fn format_text(text: &str, code: &str) -> String {
    let sections = split_sections(code);
    let section = sections
        .get(3)
        .copied()
        .or_else(|| sections.iter().copied().find(|s| s.contains('@')));
    match section {
        Some(s) if s.contains('@') => apply_text_section(s, text),
        _ => text.to_string(),
    }
}

/// Applies a text section: `@` → the value, with quoted literals and `\` escapes.
fn apply_text_section(section: &str, text: &str) -> String {
    let chars: Vec<char> = section.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '@' => {
                out.push_str(text);
                i += 1;
            }
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    out.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                }
                i += 2;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Splits a format code into sections on top-level `;`.
fn split_sections(code: &str) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut start = 0;
    let mut in_quote = false;
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_quote = !in_quote,
            b'\\' => i += 1,
            b';' if !in_quote => {
                sections.push(&code[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    sections.push(&code[start..]);
    sections
}

/// Picks the section for a value's sign; returns whether to use `abs`.
fn pick_section<'a>(value: f64, sections: &[&'a str]) -> (&'a str, bool) {
    match sections.len() {
        0 => ("General", false),
        1 => (sections[0], false),
        2 => {
            if value < 0.0 {
                (sections[1], true)
            } else {
                (sections[0], false)
            }
        }
        _ => {
            if value < 0.0 {
                (sections[1], true)
            } else if value == 0.0 {
                (sections[2], false)
            } else {
                (sections[0], false)
            }
        }
    }
}

/// Strips a leading `[Color]` modifier, returning `(color, remaining)`.
fn strip_color(section: &str) -> (Option<Rgb>, &str) {
    let s = section.trim_start();
    if let Some(rest) = s.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let name = &rest[..end];
        let color = match name.to_ascii_lowercase().as_str() {
            "red" => Some(Rgb::new(255, 0, 0)),
            "blue" => Some(Rgb::new(0, 0, 255)),
            "green" => Some(Rgb::new(0, 128, 0)),
            "black" => Some(Rgb::new(0, 0, 0)),
            "white" => Some(Rgb::new(255, 255, 255)),
            "magenta" => Some(Rgb::new(255, 0, 255)),
            "cyan" => Some(Rgb::new(0, 255, 255)),
            "yellow" => Some(Rgb::new(255, 255, 0)),
            _ => None,
        };
        if color.is_some() {
            return (color, &rest[end + 1..]);
        }
    }
    (None, section)
}

/// True when a pattern contains date/time tokens (outside quotes).
fn is_date_pattern(code: &str) -> bool {
    let mut in_quote = false;
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' => in_quote = !in_quote,
            '\\' => i += 1,
            'y' | 'd' | 'h' | 's' if !in_quote => return true,
            'm' if !in_quote => return true,
            'A' | 'a' if !in_quote && upcoming(&chars, i, "am/pm") => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn upcoming(chars: &[char], i: usize, pat: &str) -> bool {
    let s: String = chars[i..(i + pat.len()).min(chars.len())]
        .iter()
        .collect::<String>()
        .to_ascii_lowercase();
    s == pat
}

/// General format: integers plain; otherwise trimmed decimals, scientific for
/// very large/small magnitudes.
fn format_general(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    let abs = value.abs();
    if !(1e-9..1e11).contains(&abs) {
        let mut s = format!("{value:E}");
        // Normalize Rust's E notation toward Excel's (best effort).
        s = s.replace('E', "E+").replace("E+-", "E-");
        return s;
    }
    // Up to 11 significant digits, trailing zeros trimmed.
    let mut s = format!("{value:.9}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    s
}

/// Formats a non-negative number with a numeric pattern.
fn format_numeric(value: f64, pattern: &str) -> String {
    // A section with no digit placeholders is literal text; an `@` there takes
    // the value's General form (e.g. a number under the text format `@`).
    if !pattern.contains(['0', '#', '?']) {
        return apply_text_section(pattern, &format_general(value));
    }
    // Count trailing commas (scale by 1000 each) and detect features.
    let percent = pattern.contains('%');
    let scientific = pattern.to_ascii_uppercase().contains('E');
    let mut value = value;
    if percent {
        value *= 100.0;
    }
    // Trailing comma scaling (between end of digits): count commas right before
    // a non-digit tail.
    let scale_commas = trailing_comma_scale(pattern);
    value /= 1000f64.powi(scale_commas as i32);

    let (minimum_decimals, decimals) = decimal_places(pattern);
    let thousands = pattern.contains(",0") || pattern.contains("#,#") || pattern.contains(",##");

    let mut out = String::new();
    // Leading literal (e.g. "$").
    out.push_str(&leading_literal(pattern));

    if scientific {
        let exp_digits = 2;
        out.push_str(&format_scientific(value, decimals, exp_digits));
    } else {
        out.push_str(&format_fixed(value, minimum_decimals, decimals, thousands));
    }
    out.push_str(&trailing_literal(pattern, percent));
    out
}

fn trailing_comma_scale(pattern: &str) -> usize {
    // Count commas immediately after the last digit placeholder.
    let last_digit = pattern.rfind(['0', '#', '?']);
    match last_digit {
        Some(i) => pattern[i + 1..].chars().take_while(|&c| c == ',').count(),
        None => 0,
    }
}

fn decimal_places(pattern: &str) -> (usize, usize) {
    if let Some(dot) = pattern.find('.') {
        let placeholders: Vec<_> = pattern[dot + 1..]
            .chars()
            .take_while(|&c| c == '0' || c == '#' || c == '?')
            .collect();
        (
            placeholders.iter().filter(|&&value| value == '0').count(),
            placeholders.len(),
        )
    } else {
        (0, 0)
    }
}

fn leading_literal(pattern: &str) -> String {
    let mut out = String::new();
    for ch in pattern.chars() {
        match ch {
            '0' | '#' | '?' => break,
            '$' | '(' | '-' | '+' => out.push(ch),
            _ => {}
        }
    }
    out
}

fn trailing_literal(pattern: &str, percent: bool) -> String {
    let mut out = String::new();
    if percent {
        out.push('%');
    }
    // Trailing ")" for accounting negatives.
    if pattern.trim_end().ends_with(')') {
        out.push(')');
    }
    out
}

/// Rounds `value` to `decimals` places **half away from zero** — Excel's rule.
/// (Rust's `{:.N}` formatting rounds half to even, which mis-rounds `2.5` → `2`.)
fn round_half_away(value: f64, decimals: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let factor = 10f64.powi(decimals as i32);
    let scaled = value * factor;
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5).floor()
    } else {
        (scaled - 0.5).ceil()
    };
    rounded / factor
}

fn format_fixed(value: f64, minimum_decimals: usize, decimals: usize, thousands: bool) -> String {
    let s = format!("{:.*}", decimals, round_half_away(value, decimals));
    let (int_part, frac) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s, None),
    };
    let int_grouped = if thousands {
        group_thousands(&int_part)
    } else {
        int_part
    };
    match frac {
        Some(mut f) => {
            while f.len() > minimum_decimals && f.ends_with('0') {
                f.pop();
            }
            if f.is_empty() {
                int_grouped
            } else {
                format!("{int_grouped}.{f}")
            }
        }
        None => int_grouped,
    }
}

fn group_thousands(int_part: &str) -> String {
    let (sign, digits) = int_part
        .strip_prefix('-')
        .map_or(("", int_part), |digits| ("-", digits));
    let digits: Vec<char> = digits.chars().collect();
    let mut out = String::new();
    out.push_str(sign);
    let n = digits.len();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

fn is_fraction_pattern(pattern: &str) -> bool {
    pattern.contains('/') && pattern.contains('?')
}

fn format_fraction(value: f64, pattern: &str) -> String {
    let denominator_digits = pattern
        .split_once('/')
        .map(|(_, denominator)| denominator.chars().filter(|ch| *ch == '?').count())
        .unwrap_or(1)
        .clamp(1, 3);
    let max_denominator = 10u32.pow(denominator_digits as u32) - 1;
    let sign = if value < 0.0 { "-" } else { "" };
    let value = value.abs();
    let whole = value.floor() as u64;
    let fraction = value.fract();
    let mut best_numerator = 0u32;
    let mut best_denominator = 1u32;
    let mut best_error = f64::INFINITY;
    for denominator in 1..=max_denominator {
        let numerator = (fraction * f64::from(denominator)).round() as u32;
        let error = (fraction - f64::from(numerator) / f64::from(denominator)).abs();
        if error < best_error {
            best_error = error;
            best_numerator = numerator;
            best_denominator = denominator;
        }
    }
    if best_numerator == 0 {
        return format!("{sign}{whole}");
    }
    let divisor = gcd(best_numerator, best_denominator);
    best_numerator /= divisor;
    best_denominator /= divisor;
    if whole == 0 {
        format!("{sign}{best_numerator}/{best_denominator}")
    } else {
        format!("{sign}{whole} {best_numerator}/{best_denominator}")
    }
}

fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

fn is_elapsed_time_pattern(pattern: &str) -> bool {
    ["[h]", "[m]", "[s]"]
        .iter()
        .any(|token| pattern.to_ascii_lowercase().contains(token))
}

fn format_elapsed_time(value: f64, pattern: &str) -> String {
    let total_seconds = (value.abs() * 86_400.0).round() as u64;
    let lower = pattern.to_ascii_lowercase();
    let sign = if value < 0.0 { "-" } else { "" };
    if lower.contains("[h]") {
        let hours = total_seconds / 3_600;
        let minutes = (total_seconds / 60) % 60;
        let seconds = total_seconds % 60;
        return format!("{sign}{hours}:{minutes:02}:{seconds:02}");
    }
    if lower.contains("[m]") {
        let minutes = total_seconds / 60;
        return format!("{sign}{minutes}:{:02}", total_seconds % 60);
    }
    format!("{sign}{total_seconds}")
}

fn format_scientific(value: f64, decimals: usize, exp_digits: usize) -> String {
    if value == 0.0 {
        return format!("{:.*}E+{:0width$}", decimals, 0, 0, width = exp_digits);
    }
    let exp = value.abs().log10().floor() as i32;
    let mantissa = round_half_away(value / 10f64.powi(exp), decimals);
    let sign = if exp < 0 { '-' } else { '+' };
    format!(
        "{:.*}E{}{:0width$}",
        decimals,
        mantissa,
        sign,
        exp.abs(),
        width = exp_digits
    )
}

// --- Date/time --------------------------------------------------------------

/// Formats an Excel serial date/time with a date pattern.
fn format_datetime(serial: f64, pattern: &str, date1904: bool) -> String {
    let (y, mo, d, h, mi, s) = serial_to_datetime(serial, date1904);
    let has_ampm = pattern.to_ascii_lowercase().contains("am/pm");
    let display_h = if has_ampm {
        let h12 = h % 12;
        if h12 == 0 { 12 } else { h12 }
    } else {
        h
    };
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month_full = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];

    let mut out = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let run = run_len(&chars, i);
        let lower = c.to_ascii_lowercase();
        match lower {
            'y' => {
                out.push_str(&if run >= 3 {
                    format!("{y:04}")
                } else {
                    format!("{:02}", y % 100)
                });
                i += run;
            }
            'm' => {
                // Minutes if adjacent to h/s (handled by token order); month otherwise.
                let is_minute = adjacent_time(&chars, i);
                if is_minute {
                    out.push_str(&format!("{:0width$}", mi, width = run.min(2)));
                } else if run >= 4 {
                    out.push_str(month_full[(mo - 1) as usize]);
                } else if run == 3 {
                    out.push_str(months[(mo - 1) as usize]);
                } else {
                    out.push_str(&format!("{:0width$}", mo, width = run));
                }
                i += run;
            }
            'd' => {
                out.push_str(&format!("{:0width$}", d, width = run.min(2)));
                i += run;
            }
            'h' => {
                out.push_str(&format!("{:0width$}", display_h, width = run.min(2)));
                i += run;
            }
            's' => {
                out.push_str(&format!("{:0width$}", s, width = run.min(2)));
                i += run;
            }
            'a' if upcoming(&chars, i, "am/pm") => {
                out.push_str(if h < 12 { "AM" } else { "PM" });
                i += 5;
            }
            '\\' => {
                if i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                }
                i += 2;
            }
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    out.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn run_len(chars: &[char], i: usize) -> usize {
    let c = chars[i].to_ascii_lowercase();
    let mut n = 0;
    while i + n < chars.len() && chars[i + n].to_ascii_lowercase() == c {
        n += 1;
    }
    n
}

/// Whether an `m` token is a minute (adjacent to an `h` before or `s` after).
fn adjacent_time(chars: &[char], i: usize) -> bool {
    let before = chars[..i]
        .iter()
        .rev()
        .find(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase());
    let after = chars[i + run_len(chars, i)..]
        .iter()
        .find(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase());
    before == Some('h') || after == Some('s')
}

/// Converts an Excel serial to `(year, month, day, hour, minute, second)`.
/// The 1900 system reproduces the phantom 1900-02-29 (serial 60): serials 1–59
/// predate it, 61+ follow it, so the unix-epoch offset differs across it.
fn serial_to_datetime(serial: f64, date1904: bool) -> (i64, u32, u32, u32, u32, u32) {
    let whole = serial.trunc() as i64;
    let frac = serial - serial.trunc();
    let total_secs = (frac * 86400.0).round() as i64;
    let h = ((total_secs / 3600) % 24) as u32;
    let mi = ((total_secs / 60) % 60) as u32;
    let s = (total_secs % 60) as u32;

    if !date1904 && whole == 60 {
        return (1900, 2, 29, h, mi, s); // Excel's phantom leap day
    }
    // 25569 = serial of 1970-01-01 (1900 sys, post-phantom); 24107 for 1904.
    let unix_days = if date1904 {
        whole - 24107
    } else if whole < 60 {
        whole - 25568 // before the phantom day: one fewer offset
    } else {
        whole - 25569
    };
    let (y, mo, d) = civil_from_days(unix_days);
    (y, mo, d, h, mi, s)
}

/// Howard Hinnant's days-from-civil inverse (days since 1970-01-01 → y/m/d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(value: f64, code: &str) -> String {
        format_number(value, code, false).text
    }

    #[test]
    fn number_formats_table() {
        // (value, code, expected) — verified against Excel semantics.
        let cases: &[(f64, &str, &str)] = &[
            (0.0, "General", "0"),
            (42.0, "General", "42"),
            (1.23456, "General", "1.23456"),
            (-7.0, "General", "-7"),
            (1234.5, "General", "1234.5"),
            (5.0, "0", "5"),
            (5.7, "0", "6"),
            (5.0, "0.00", "5.00"),
            (1.23456, "0.00", "1.23"),
            (1234.0, "#,##0", "1,234"),
            (1234567.0, "#,##0", "1,234,567"),
            (1234.5, "#,##0.00", "1,234.50"),
            (0.5, "0%", "50%"),
            (0.1234, "0.00%", "12.34%"),
            (1.0, "0%", "100%"),
            (12000.0, "#,##0,", "12"),
            (0.0, "0.00", "0.00"),
            (999.999, "0.00", "1000.00"),
            (-1234.0, "#,##0;(#,##0)", "(1,234)"),
            (1234.0, "#,##0;(#,##0)", "1,234"),
            (2.0, "0.0", "2.0"),
            (100.0, "0.000", "100.000"),
            (0.005, "0.00", "0.01"),
            (0.004, "0.00", "0.00"),
            (42.0, "@", "42"),
            // Excel rounds half AWAY from zero (Rust's {:.N} rounds to even).
            (2.5, "0", "3"),
            (3.5, "0", "4"),
            (-2.5, "0", "-3"),
            (0.125, "0.00", "0.13"),
            (0.375, "0.00", "0.38"),
            // Negatives, single-section.
            (-3.5, "0.00", "-3.50"),
            (-1234.5, "#,##0.00", "-1,234.50"),
            // Larger grouping + zero.
            (1000000.0, "#,##0", "1,000,000"),
            (0.0, "0", "0"),
            (0.0, "#,##0", "0"),
            // Percent edge cases.
            (0.0, "0%", "0%"),
            (-0.5, "0%", "-50%"),
            (1.5, "0.0%", "150.0%"),
            // Trailing-comma scaling (thousands).
            (1500000.0, "#,##0,", "1,500"),
            (2500000.0, "0.0,,", "2.5"),
            // Two-section: zero uses the positive section.
            (0.0, "#,##0;(#,##0)", "0"),
            // Three-section pos;neg;zero.
            (0.0, "0.0;-0.0;\"zero\"", "zero"),
            (-5.0, "0.0;-0.0;\"zero\"", "-5.0"),
            // Text section.
            (0.0, "@", "0"),
        ];
        for (v, code, expect) in cases {
            assert_eq!(&f(*v, code), expect, "value {v} code {code:?}");
        }
    }

    #[test]
    fn scientific_format() {
        assert_eq!(f(12345.0, "0.00E+00"), "1.23E+04");
        assert_eq!(f(0.00012, "0.00E+00"), "1.20E-04");
    }

    #[test]
    fn date_formats_table() {
        // Serial 1 = 1900-01-01; 44927 = 2023-01-01; 45658 = 2025-01-01.
        let cases: &[(f64, &str, &str)] = &[
            (44927.0, "m/d/yyyy", "1/1/2023"),
            (44927.0, "yyyy-mm-dd", "2023-01-01"),
            (44927.0, "d-mmm-yy", "1-Jan-23"),
            (44927.0, "mmm-yy", "Jan-23"),
            (44927.0, "mmmm d, yyyy", "January 1, 2023"),
            (45658.0, "m/d/yyyy", "1/1/2025"),
            // The 1900 phantom-leap-day boundary (Excel's famous bug): serial 60
            // is the non-existent 1900-02-29; 59 and 61 straddle it.
            (59.0, "m/d/yyyy", "2/28/1900"),
            (60.0, "m/d/yyyy", "2/29/1900"),
            (61.0, "m/d/yyyy", "3/1/1900"),
            (1.0, "m/d/yyyy", "1/1/1900"),
            // A real Gregorian leap day.
            (45351.0, "yyyy-mm-dd", "2024-02-29"),
            (45350.0, "yyyy-mm-dd", "2024-02-28"),
        ];
        for (v, code, expect) in cases {
            assert_eq!(&f(*v, code), expect, "serial {v} code {code:?}");
        }
    }

    #[test]
    fn time_formats() {
        // 0.5 = noon; 0.75 = 6 PM.
        assert_eq!(f(0.5, "h:mm"), "12:00");
        assert_eq!(f(0.75, "h:mm AM/PM"), "6:00 PM");
        assert_eq!(f(0.25, "h:mm AM/PM"), "6:00 AM");
        assert_eq!(f(0.5, "h:mm:ss"), "12:00:00");
    }

    #[test]
    fn builtin_codes_resolve() {
        assert_eq!(builtin_code(0), Some("General"));
        assert_eq!(builtin_code(2), Some("0.00"));
        assert_eq!(builtin_code(14), Some("m/d/yyyy"));
        assert_eq!(builtin_code(200), None);
    }

    #[test]
    fn text_section() {
        assert_eq!(format_text("hi", "General"), "hi");
        assert_eq!(format_text("hi", "@"), "hi");
        assert_eq!(format_text("hi", "\"[\"@\"]\""), "[hi]");
    }
}

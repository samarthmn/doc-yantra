//! XLSX assembly: opens a workbook and resolves each visible sheet's model,
//! shared strings, styles, theme, fonts, and dates.

use crate::fonts::FontDb;
use crate::layout::DocDates;
use crate::opc::Package;
use crate::oxml::sml::{SharedStrings, Styles, Workbook, Worksheet};
use crate::oxml::theme::Theme;
use crate::{ConvertOptions, OfficeError, Result};
use std::collections::HashMap;

/// A resolved sheet.
pub struct ResolvedSheet {
    /// Sheet name.
    pub name: String,
    /// The worksheet model.
    pub sheet: Worksheet,
}

/// A fully-resolved workbook.
pub struct XlsxDoc {
    /// The workbook (date system, sheet order).
    pub workbook: Workbook,
    /// Visible (and filtered) sheets in order.
    pub sheets: Vec<ResolvedSheet>,
    /// Cell styles + custom number formats.
    pub styles: Styles,
    /// The theme.
    pub theme: Theme,
    /// Fonts.
    pub fonts: FontDb,
    /// PDF dates.
    pub dates: DocDates,
}

impl XlsxDoc {
    /// Opens and resolves a `.xlsx`/`.xlsm` package.
    pub fn open(bytes: &[u8], opts: &ConvertOptions) -> Result<XlsxDoc> {
        let pkg = Package::open(bytes)?;
        let wb_part = pkg
            .relationships("")?
            .into_iter()
            .find(|r| r.rel_type.ends_with("/officeDocument"))
            .map(|r| r.target)
            .unwrap_or_else(|| "xl/workbook.xml".to_string());
        let workbook = parse(&pkg, &wb_part, Workbook::parse)?;
        let wb_rels = rel_map(&pkg, &wb_part);

        let shared =
            optional(&pkg, "xl/sharedStrings.xml", SharedStrings::parse)?.unwrap_or_default();
        let styles = optional(&pkg, "xl/styles.xml", Styles::parse)?.unwrap_or_default();
        let theme = optional(&pkg, "xl/theme/theme1.xml", Theme::parse)?.unwrap_or_default();

        let filter = opts.sheet_filter.as_ref();
        let mut sheets = Vec::new();
        for sr in &workbook.sheets {
            if !sr.visible {
                continue;
            }
            if let Some(filter) = filter
                && !filter.iter().any(|n| n == &sr.name)
            {
                continue;
            }
            let Some(part) = wb_rels.get(&sr.rel_id) else {
                continue;
            };
            let Ok(sheet) = parse(&pkg, part, |xml| Worksheet::parse(xml, &shared)) else {
                continue;
            };
            sheets.push(ResolvedSheet {
                name: sr.name.clone(),
                sheet,
            });
        }

        let mut fonts = FontDb::with_system(&opts.font_dirs);
        for fb in &opts.font_bytes {
            fonts.register(fb.clone()).ok();
        }

        Ok(XlsxDoc {
            workbook,
            sheets,
            styles,
            theme,
            fonts,
            dates: core_dates(&pkg),
        })
    }
}

fn parse<T>(pkg: &Package, part: &str, f: impl Fn(&str) -> Result<T>) -> Result<T> {
    let bytes = pkg
        .part(part)
        .ok_or_else(|| OfficeError::Invalid(format!("missing part {part}")))?;
    let text = std::str::from_utf8(bytes)
        .map_err(|_| OfficeError::Invalid(format!("{part} is not UTF-8")))?;
    f(text)
}

fn optional<T>(pkg: &Package, part: &str, f: impl Fn(&str) -> Result<T>) -> Result<Option<T>> {
    match pkg.part(part) {
        Some(_) => parse(pkg, part, f).map(Some),
        None => Ok(None),
    }
}

fn rel_map(pkg: &Package, source_part: &str) -> HashMap<String, String> {
    pkg.relationships(source_part)
        .map(|rels| rels.into_iter().map(|r| (r.id, r.target)).collect())
        .unwrap_or_default()
}

fn core_dates(pkg: &Package) -> DocDates {
    let mut dates = DocDates::default();
    let Some(bytes) = pkg.part("docprops/core.xml") else {
        return dates;
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return dates;
    };
    let Ok(doc) = roxmltree::Document::parse(text) else {
        return dates;
    };
    for node in doc.root_element().descendants() {
        let d = node.text().and_then(iso_to_pdf_date);
        match node.tag_name().name() {
            "created" => {
                if let Some(d) = d {
                    dates.created = d;
                }
            }
            "modified" => {
                if let Some(d) = d {
                    dates.modified = d;
                }
            }
            _ => {}
        }
    }
    dates
}

fn iso_to_pdf_date(s: &str) -> Option<String> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    let mut out = String::from("D:");
    out.push_str(&digits[..digits.len().min(14)]);
    while out.len() < 16 {
        out.push('0');
    }
    out.push('Z');
    Some(out)
}

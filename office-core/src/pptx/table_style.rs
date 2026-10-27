//! DrawingML table-style parsing and conditional-region resolution.

use crate::oxml::pml::{
    ColorSpec, Fill, LineSpec, PmlTable, TextBody, line_has_unsupported_style, parse_fill_children,
    parse_line,
};
use crate::oxml::theme::parse_color_element;
use crate::oxml::{A, Node, attr, child, children, is, parse_xml};
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

const TOP: usize = 0;
const RIGHT: usize = 1;
const BOTTOM: usize = 2;
const LEFT: usize = 3;
const INSIDE_H: usize = 4;
const INSIDE_V: usize = 5;

/// Parsed `ppt/tableStyles.xml`.
#[derive(Debug, Clone, Default)]
pub(crate) struct TableStyleList {
    default_id: Option<String>,
    styles: HashMap<String, TableStyle>,
}

#[derive(Debug, Clone, Default)]
struct TableStyle {
    parts: HashMap<Region, TablePartStyle>,
    unsupported: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Region {
    Whole,
    FirstRow,
    LastRow,
    FirstCol,
    LastCol,
    Band1H,
    Band2H,
    Band1V,
    Band2V,
    Ne,
    Nw,
    Se,
    Sw,
}

#[derive(Debug, Clone, Default)]
struct TablePartStyle {
    fill: Option<Fill>,
    borders: [BorderValue; 6],
    text: TableTextStyle,
    unsupported: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct BorderValue {
    specified: bool,
    line: Option<LineSpec>,
}

#[derive(Debug, Clone, Default)]
struct TableTextStyle {
    bold: Option<bool>,
    italic: Option<bool>,
    color: Option<ColorSpec>,
    typeface: Option<String>,
}

/// Fully cascaded style for one visible cell.
#[derive(Debug, Clone, Default)]
pub(crate) struct TableCellStyle {
    /// Cascaded cell fill.
    pub fill: Option<Fill>,
    /// Whether omitted borders inherit PowerPoint's Medium Style 2 base grid.
    pub inherited_base_grid: bool,
    borders: [BorderValue; 6],
    text: TableTextStyle,
}

/// Physical row/column bounds of a possibly merged table cell.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CellBounds {
    /// First physical row occupied by the cell.
    pub row_start: usize,
    /// Last physical row occupied by the cell.
    pub row_end: usize,
    /// First physical column occupied by the cell.
    pub col_start: usize,
    /// Last physical column occupied by the cell.
    pub col_end: usize,
    /// Number of physical rows in the table.
    pub row_count: usize,
    /// Number of physical columns in the table.
    pub col_count: usize,
}

impl TableStyleList {
    pub(crate) fn parse(xml: &str) -> Result<Self> {
        let document = parse_xml(xml, "PowerPoint table styles")?;
        let root = document.root_element();
        if !is(root, A, "tblStyleLst") {
            return Err(OfficeError::Invalid(
                "ppt/tableStyles.xml has no a:tblStyleLst root".to_string(),
            ));
        }
        let mut list = TableStyleList {
            default_id: attr(root, A, "def").map(str::to_string),
            styles: HashMap::new(),
        };
        for node in children(root) {
            if is(node, A, "extLst") {
                continue;
            }
            if !is(node, A, "tblStyle") {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::TableStyle,
                    format!("a:tblStyleLst/a:{}", node.tag_name().name()),
                ));
            }
            let id = attr(node, A, "styleId")
                .ok_or_else(|| OfficeError::Invalid("a:tblStyle is missing styleId".to_string()))?;
            list.styles.insert(id.to_string(), parse_style(node));
        }
        Ok(list)
    }

    pub(crate) fn resolve_cell(
        &self,
        table: &PmlTable,
        row: usize,
        col: usize,
    ) -> Result<Option<TableCellStyle>> {
        let Some(id) = table.style_id.as_deref().or(self.default_id.as_deref()) else {
            return Ok(None);
        };
        let style = self.styles.get(id).ok_or_else(|| {
            OfficeError::unsupported(
                UnsupportedCode::TableStyle,
                format!("table style {id} is not defined in ppt/tableStyles.xml"),
            )
        })?;
        if let Some(detail) = style.unsupported.first() {
            return Err(OfficeError::unsupported(
                UnsupportedCode::TableStyle,
                detail.clone(),
            ));
        }
        let row_count = table.rows.len();
        let col_count = table.col_widths.len();
        let mut resolved = TableCellStyle {
            inherited_base_grid: is_medium_style_2(id),
            ..TableCellStyle::default()
        };
        apply_region(style, Region::Whole, &mut resolved)?;
        if table.band_rows {
            let band_row = row.saturating_sub(usize::from(table.first_row));
            apply_region(
                style,
                if band_row.is_multiple_of(2) {
                    Region::Band1H
                } else {
                    Region::Band2H
                },
                &mut resolved,
            )?;
        }
        if table.band_cols {
            let band_col = col.saturating_sub(usize::from(table.first_col));
            apply_region(
                style,
                if band_col.is_multiple_of(2) {
                    Region::Band1V
                } else {
                    Region::Band2V
                },
                &mut resolved,
            )?;
        }
        let is_first_row = table.first_row && row == 0;
        let is_last_row = table.last_row && row + 1 == row_count;
        let is_first_col = table.first_col && col == 0;
        let is_last_col = table.last_col && col + 1 == col_count;
        // ISO/IEC 29500's table-style cascade is ordered, rather than a
        // specificity contest. The later region wins for every property it
        // specifies. This matters for one-row/one-column tables, where several
        // conditional regions apply to the same cell.
        if is_last_col {
            apply_region(style, Region::LastCol, &mut resolved)?;
        }
        if is_first_col {
            apply_region(style, Region::FirstCol, &mut resolved)?;
        }
        if is_last_row {
            apply_region(style, Region::LastRow, &mut resolved)?;
        }
        if is_last_row && is_last_col {
            apply_region(style, Region::Se, &mut resolved)?;
        }
        if is_last_row && is_first_col {
            apply_region(style, Region::Sw, &mut resolved)?;
        }
        if is_first_row {
            apply_region(style, Region::FirstRow, &mut resolved)?;
        }
        if is_first_row && is_last_col {
            apply_region(style, Region::Ne, &mut resolved)?;
        }
        if is_first_row && is_first_col {
            apply_region(style, Region::Nw, &mut resolved)?;
        }
        Ok(Some(resolved))
    }
}

fn is_medium_style_2(id: &str) -> bool {
    const IDS: [&str; 7] = [
        "{073A0DAA-6AF3-43AB-8588-CEC1D06C72B9}",
        "{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}",
        "{21E4AEA4-8DFA-4A89-87EB-49C32662AFE0}",
        "{F5AB1C69-6EDB-4FF4-983F-18BD219EF322}",
        "{00A15C55-8517-42AA-B614-E9B94910E393}",
        "{7DF18680-E054-41AD-8BC1-D1AEF772440D}",
        "{93296810-A885-4BE3-A3E7-6D5BEEA58F35}",
    ];
    IDS.iter()
        .any(|candidate| id.eq_ignore_ascii_case(candidate))
}

impl TableCellStyle {
    /// Returns whether the style cascade specified this edge and, when it did,
    /// its visible line. `true, None` is an explicit `a:noFill`; keeping that
    /// distinct from omission is required before applying the table default.
    pub(crate) fn border(&self, edge: usize, bounds: CellBounds) -> (bool, Option<&LineSpec>) {
        let direct = &self.borders[edge];
        if direct.specified {
            return (true, direct.line.as_ref());
        }
        let inside = match edge {
            TOP if bounds.row_start > 0 => Some(INSIDE_H),
            BOTTOM if bounds.row_end + 1 < bounds.row_count => Some(INSIDE_H),
            LEFT if bounds.col_start > 0 => Some(INSIDE_V),
            RIGHT if bounds.col_end + 1 < bounds.col_count => Some(INSIDE_V),
            _ => None,
        };
        let Some(index) = inside else {
            return (false, None);
        };
        let value = &self.borders[index];
        (value.specified, value.line.as_ref())
    }

    pub(crate) fn apply_text(&self, body: &TextBody) -> TextBody {
        let mut body = body.clone();
        for paragraph in &mut body.paragraphs {
            for run in &mut paragraph.runs {
                if run.bold.is_none() {
                    run.bold = self.text.bold;
                }
                if run.italic.is_none() {
                    run.italic = self.text.italic;
                }
                if run.color.is_none() {
                    run.color.clone_from(&self.text.color);
                }
                if run.typeface.is_none() {
                    run.typeface.clone_from(&self.text.typeface);
                }
            }
        }
        body
    }
}

fn apply_region(style: &TableStyle, region: Region, resolved: &mut TableCellStyle) -> Result<()> {
    let Some(part) = style.parts.get(&region) else {
        return Ok(());
    };
    if let Some(detail) = part.unsupported.first() {
        return Err(OfficeError::unsupported(
            UnsupportedCode::TableStyle,
            detail.clone(),
        ));
    }
    if let Some(fill) = &part.fill {
        resolved.fill = Some(fill.clone());
    }
    for (target, source) in resolved.borders.iter_mut().zip(&part.borders) {
        if source.specified {
            target.clone_from(source);
        }
    }
    if part.text.bold.is_some() {
        resolved.text.bold = part.text.bold;
    }
    if part.text.italic.is_some() {
        resolved.text.italic = part.text.italic;
    }
    if part.text.color.is_some() {
        resolved.text.color.clone_from(&part.text.color);
    }
    if part.text.typeface.is_some() {
        resolved.text.typeface.clone_from(&part.text.typeface);
    }
    Ok(())
}

fn parse_style(node: Node) -> TableStyle {
    let mut style = TableStyle::default();
    for part in children(node) {
        let region = match part.tag_name().name() {
            "wholeTbl" => Some(Region::Whole),
            "firstRow" => Some(Region::FirstRow),
            "lastRow" => Some(Region::LastRow),
            "firstCol" => Some(Region::FirstCol),
            "lastCol" => Some(Region::LastCol),
            "band1H" => Some(Region::Band1H),
            "band2H" => Some(Region::Band2H),
            "band1V" => Some(Region::Band1V),
            "band2V" => Some(Region::Band2V),
            "neCell" => Some(Region::Ne),
            "nwCell" => Some(Region::Nw),
            "seCell" => Some(Region::Se),
            "swCell" => Some(Region::Sw),
            "extLst" => None,
            other => {
                style.unsupported.push(format!("a:tblStyle/a:{other}"));
                None
            }
        };
        if let Some(region) = region {
            style.parts.insert(region, parse_part(part));
        }
    }
    style
}

fn parse_part(node: Node) -> TablePartStyle {
    let mut part = TablePartStyle::default();
    for child_node in children(node) {
        match child_node.tag_name().name() {
            "tcTxStyle" => parse_text_style(child_node, &mut part),
            "tcStyle" => parse_cell_style(child_node, &mut part),
            other => part
                .unsupported
                .push(format!("a:{}/a:{other}", node.tag_name().name())),
        }
    }
    part
}

fn parse_text_style(node: Node, part: &mut TablePartStyle) {
    part.text.bold = parse_optional_bool(attr(node, A, "b"), "b", &mut part.unsupported);
    part.text.italic = parse_optional_bool(attr(node, A, "i"), "i", &mut part.unsupported);
    if let Some(font) = child(node, A, "fontRef") {
        part.text.typeface = match attr(font, A, "idx") {
            Some("major") => Some("+mj-lt".to_string()),
            Some("minor") => Some("+mn-lt".to_string()),
            Some(other) => {
                part.unsupported
                    .push(format!("a:tcTxStyle/a:fontRef@idx={other}"));
                None
            }
            None => {
                part.unsupported
                    .push("a:tcTxStyle/a:fontRef missing idx".to_string());
                None
            }
        };
    }
    part.text.color = children(node).find_map(|color| {
        parse_color_element(color).map(|(base, transforms)| ColorSpec { base, transforms })
    });
    for child_node in children(node) {
        if !is(child_node, A, "fontRef") && parse_color_element(child_node).is_none() {
            part.unsupported
                .push(format!("a:tcTxStyle/a:{}", child_node.tag_name().name()));
        }
    }
}

fn parse_optional_bool(
    value: Option<&str>,
    name: &str,
    unsupported: &mut Vec<String>,
) -> Option<bool> {
    match value {
        None => None,
        Some("1" | "true" | "on") => Some(true),
        Some("0" | "false" | "off") => Some(false),
        Some(other) => {
            unsupported.push(format!("a:tcTxStyle@{name}={other}"));
            None
        }
    }
}

fn parse_cell_style(node: Node, part: &mut TablePartStyle) {
    for child_node in children(node) {
        match child_node.tag_name().name() {
            "tcBdr" => parse_borders(child_node, part),
            "fill" => {
                part.fill = match parse_fill_children(child_node) {
                    Ok(Some(fill)) => Some(fill),
                    Ok(None) => {
                        part.unsupported
                            .push("a:tcStyle/a:fill has no fill".to_string());
                        None
                    }
                    Err(error) => Some(Fill::Unsupported(error)),
                };
                if matches!(part.fill, Some(Fill::Picture(_))) {
                    part.unsupported.push("a:tcStyle picture fill".to_string());
                }
            }
            "fillRef" | "cell3D" => part
                .unsupported
                .push(format!("a:tcStyle/a:{}", child_node.tag_name().name())),
            other => part.unsupported.push(format!("a:tcStyle/a:{other}")),
        }
    }
}

fn parse_borders(node: Node, part: &mut TablePartStyle) {
    for edge in children(node) {
        let index = match edge.tag_name().name() {
            "top" => Some(TOP),
            "right" => Some(RIGHT),
            "bottom" => Some(BOTTOM),
            "left" => Some(LEFT),
            "insideH" => Some(INSIDE_H),
            "insideV" => Some(INSIDE_V),
            "tl2br" | "tr2bl" => {
                part.unsupported
                    .push(format!("a:tcBdr/a:{}", edge.tag_name().name()));
                None
            }
            other => {
                part.unsupported.push(format!("a:tcBdr/a:{other}"));
                None
            }
        };
        let Some(index) = index else { continue };
        let value = &mut part.borders[index];
        value.specified = true;
        let Some(line) = child(edge, A, "ln") else {
            part.unsupported
                .push(format!("a:tcBdr/a:{} has no a:ln", edge.tag_name().name()));
            continue;
        };
        if line_has_unsupported_style(line) {
            part.unsupported.push(format!(
                "a:tcBdr/a:{}/a:ln dash/compound/join",
                edge.tag_name().name()
            ));
        }
        value.line = parse_line(line);
        if value.line.is_none() && child(line, A, "noFill").is_none() {
            part.unsupported
                .push(format!("a:tcBdr/a:{}/a:ln fill", edge.tag_name().name()));
        }
    }
}

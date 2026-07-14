//! Applies a DOCX table style (`w:tblStyle`) to the table it is referenced from.
//!
//! A table style in `word/styles.xml` carries a whole-table block plus up to
//! twelve *conditional* blocks (`w:tblStylePr`) — first/last row, first/last
//! column, alternating row and column bands, and the four corner cells. Which
//! of them apply to a given cell follows from the table's `w:tblLook` opt-ins
//! and the cell's position in the grid; the slots are then layered least- to
//! most-specific (ECMA-376 §17.7.6), and the table's own direct properties
//! outrank all of them.
//!
//! Word also caches its own answer to that question in `w:cnfStyle` markers on
//! each row, cell, and paragraph. Those are redundant with the positional
//! computation here and carry no formatting, so they are parsed and dropped.
//!
//! Text formatting cannot simply be folded into the paragraph's direct
//! properties: in the cascade a table style sits *below* the paragraph style,
//! so it rides on [`ParaProps::tbl_style`] and is slotted in at the right level
//! by [`super::styles::StyleResolver`].

use super::styles::{StyleResolver, TableStyleFormat, TblStyleSlot};
use super::wml::{BlockContent, NotesPart, Table, TblLook, TblStyleText};
use std::rc::Rc;

/// Resolves and applies the table style of every table in `blocks`, recursing
/// into cells so nested tables pick up their own style.
pub fn apply_table_styles(styles: &StyleResolver, blocks: &mut [BlockContent]) {
    for block in blocks {
        let BlockContent::Table(table) = block else {
            continue;
        };
        apply_to_table(styles, table);
    }
}

/// Applies table styles to every note in a footnote/endnote part.
pub fn apply_notes_table_styles(styles: &StyleResolver, notes: &mut NotesPart) {
    for blocks in notes.notes.values_mut() {
        apply_table_styles(styles, blocks);
    }
    apply_table_styles(styles, &mut notes.separator);
    apply_table_styles(styles, &mut notes.continuation_separator);
}

fn apply_to_table(styles: &StyleResolver, table: &mut Table) {
    // A table with no `w:tblStyle` keeps exactly the properties it declares.
    // (Word would also layer the `w:default` table style underneath; that style
    // is `TableNormal`, whose cell margins the layout engine already assumes.)
    let style = table
        .props
        .style_id
        .as_deref()
        .and_then(|id| styles.table_style(id));

    if let Some(style) = &style {
        let look = table.props.look;
        table.props = table.props.merge_over(&style.whole.tbl);

        let row_band = table.props.row_band_size.unwrap_or(1).max(1) as usize;
        let col_band = table.props.col_band_size.unwrap_or(1).max(1) as usize;
        let row_count = table.rows.len();
        let col_count = table.grid.len().max(
            table
                .rows
                .iter()
                .map(|row| {
                    row.cells
                        .iter()
                        .map(|cell| cell.props.grid_span.max(1) as usize)
                        .sum::<usize>()
                })
                .max()
                .unwrap_or(0),
        );

        for row_index in 0..row_count {
            let row_slots = row_slots(look, row_index, row_count, row_band);

            let mut row_format = style.whole.clone();
            for slot in TblStyleSlot::ORDER {
                if row_slots.contains(&slot)
                    && let Some(format) = style.slot(slot)
                {
                    row_format = format.merge_over(&row_format);
                }
            }
            table.rows[row_index].props = table.rows[row_index].props.merge_over(&row_format.row);

            let mut column = 0usize;
            for cell_index in 0..table.rows[row_index].cells.len() {
                let span = table.rows[row_index].cells[cell_index]
                    .props
                    .grid_span
                    .max(1) as usize;
                let mut slots = row_slots.clone();
                slots.extend(column_slots(look, column, col_count, col_band));
                slots.extend(corner_slots(&slots));

                let mut format = style.whole.clone();
                for slot in TblStyleSlot::ORDER {
                    if slots.contains(&slot)
                        && let Some(defined) = style.slot(slot)
                    {
                        format = defined.merge_over(&format);
                    }
                }

                let cell = &mut table.rows[row_index].cells[cell_index];
                cell.props = cell.props.merge_over(&format.cell);
                apply_text_format(&format, &mut cell.content);
                column += span;
            }
        }
    }

    // Nested tables resolve their own style, not the enclosing table's.
    for row in &mut table.rows {
        for cell in &mut row.cells {
            apply_table_styles(styles, &mut cell.content);
        }
    }
}

/// Attaches the table style's paragraph/run formatting to the cell's own
/// paragraphs. Paragraphs inside a *nested* table belong to that table's style
/// and are left alone here.
fn apply_text_format(format: &TableStyleFormat, content: &mut [BlockContent]) {
    let text = Rc::new(TblStyleText {
        ppr: format.ppr.clone(),
        rpr: format.rpr.clone(),
    });
    for block in content {
        if let BlockContent::Paragraph(paragraph) = block {
            paragraph.props.tbl_style = Some(Rc::clone(&text));
        }
    }
}

/// The slots a whole row falls into: first/last row plus horizontal banding.
fn row_slots(look: TblLook, index: usize, count: usize, band: usize) -> Vec<TblStyleSlot> {
    let mut slots = Vec::new();
    if look.first_row && index == 0 {
        slots.push(TblStyleSlot::FirstRow);
    }
    if look.last_row && count > 0 && index == count - 1 {
        slots.push(TblStyleSlot::LastRow);
    }
    if slots.is_empty()
        && !look.no_h_band
        && let Some(slot) = band_slot(
            index,
            count,
            band,
            look.first_row,
            look.last_row,
            TblStyleSlot::Band1Horz,
            TblStyleSlot::Band2Horz,
        )
    {
        slots.push(slot);
    }
    slots
}

/// The slots a column falls into: first/last column plus vertical banding.
fn column_slots(look: TblLook, index: usize, count: usize, band: usize) -> Vec<TblStyleSlot> {
    let mut slots = Vec::new();
    if look.first_col && index == 0 {
        slots.push(TblStyleSlot::FirstCol);
    }
    if look.last_col && count > 0 && index == count - 1 {
        slots.push(TblStyleSlot::LastCol);
    }
    if slots.is_empty()
        && !look.no_v_band
        && let Some(slot) = band_slot(
            index,
            count,
            band,
            look.first_col,
            look.last_col,
            TblStyleSlot::Band1Vert,
            TblStyleSlot::Band2Vert,
        )
    {
        slots.push(slot);
    }
    slots
}

/// Banding runs over the rows/columns left once an emphasised first and last
/// one are taken out, so the first body row is always band 1.
fn band_slot(
    index: usize,
    count: usize,
    band: usize,
    skip_first: bool,
    skip_last: bool,
    odd: TblStyleSlot,
    even: TblStyleSlot,
) -> Option<TblStyleSlot> {
    let start = usize::from(skip_first);
    let end = count.saturating_sub(usize::from(skip_last));
    if index < start || index >= end {
        return None;
    }
    let ordinal = (index - start) / band.max(1);
    Some(if ordinal.is_multiple_of(2) { odd } else { even })
}

/// Corner slots apply where an emphasised first/last row meets an emphasised
/// first/last column.
fn corner_slots(slots: &[TblStyleSlot]) -> Vec<TblStyleSlot> {
    let has = |slot: TblStyleSlot| slots.contains(&slot);
    let mut corners = Vec::new();
    if has(TblStyleSlot::FirstRow) && has(TblStyleSlot::FirstCol) {
        corners.push(TblStyleSlot::NwCell);
    }
    if has(TblStyleSlot::FirstRow) && has(TblStyleSlot::LastCol) {
        corners.push(TblStyleSlot::NeCell);
    }
    if has(TblStyleSlot::LastRow) && has(TblStyleSlot::FirstCol) {
        corners.push(TblStyleSlot::SwCell);
    }
    if has(TblStyleSlot::LastRow) && has(TblStyleSlot::LastCol) {
        corners.push(TblStyleSlot::SeCell);
    }
    corners
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_BANDS: TblLook = TblLook {
        first_row: true,
        last_row: true,
        first_col: true,
        last_col: true,
        no_h_band: false,
        no_v_band: false,
    };

    #[test]
    fn banding_skips_the_emphasised_first_and_last_rows() {
        // Six rows: header, four banded body rows, footer.
        let slots: Vec<_> = (0..6)
            .map(|index| row_slots(ALL_BANDS, index, 6, 1))
            .collect();
        assert_eq!(slots[0], vec![TblStyleSlot::FirstRow]);
        assert_eq!(slots[1], vec![TblStyleSlot::Band1Horz]);
        assert_eq!(slots[2], vec![TblStyleSlot::Band2Horz]);
        assert_eq!(slots[3], vec![TblStyleSlot::Band1Horz]);
        assert_eq!(slots[4], vec![TblStyleSlot::Band2Horz]);
        assert_eq!(slots[5], vec![TblStyleSlot::LastRow]);
    }

    #[test]
    fn a_band_size_above_one_groups_consecutive_rows() {
        let look = TblLook {
            first_row: false,
            last_row: false,
            ..ALL_BANDS
        };
        let slots: Vec<_> = (0..4).map(|index| row_slots(look, index, 4, 2)).collect();
        assert_eq!(slots[0], vec![TblStyleSlot::Band1Horz]);
        assert_eq!(slots[1], vec![TblStyleSlot::Band1Horz]);
        assert_eq!(slots[2], vec![TblStyleSlot::Band2Horz]);
        assert_eq!(slots[3], vec![TblStyleSlot::Band2Horz]);
    }

    #[test]
    fn opting_out_of_banding_leaves_body_rows_bare() {
        let look = TblLook {
            no_h_band: true,
            ..ALL_BANDS
        };
        assert!(row_slots(look, 1, 4, 1).is_empty());
    }

    #[test]
    fn corners_need_both_an_emphasised_row_and_column() {
        let mut slots = row_slots(ALL_BANDS, 0, 3, 1);
        slots.extend(column_slots(ALL_BANDS, 0, 3, 1));
        assert_eq!(corner_slots(&slots), vec![TblStyleSlot::NwCell]);

        let banded_only = vec![TblStyleSlot::Band1Horz, TblStyleSlot::Band1Vert];
        assert!(corner_slots(&banded_only).is_empty());
    }
}

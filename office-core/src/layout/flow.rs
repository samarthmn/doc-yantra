//! Flow: builds flow units from body blocks, paginates them (keep/widow groups,
//! table-header repeat, explicit and section breaks), renders headers/footers
//! per page with page-number field substitution, and collects the outline.
//!
//! v1 uses the final section's geometry for every page (the common single-section
//! case); mid-document geometry changes are a fidelity-harness refinement.

use super::assemble::WordDoc;
use super::numbering_layout::{ListCounters, PlacedLabel, build_label};
use super::page::PageGeometry;
use super::paragraph::{
    AnchoredLineItem, ExclusionBand, FieldContext, LineItem, PageFloat, ParaLayout, WrapExclusion,
    layout_paragraph, layout_paragraph_with_wrap,
};
use super::table::layout_table;
use super::{EncodedImage, FillRule, LaidOutDocument, OutlineEntry, Page, PathCommand, Positioned};
use crate::oxml::numbering::{NumFmt, format_number};
use crate::oxml::wml::{
    AnchorOrigin, BlockContent, ColumnSpec, FrameAlign, FrameHeightRule, FrameProps, HdrFtrKind,
    InlineContent, NoteKind, PageVAlign, Paragraph, Run, RunContent, RunProps, SectPr, SectType,
    VertAlign, WrapMode,
};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

const MIN_TEXT_WINDOW_PT: f64 = 36.0;
const PAGE_FLOAT_REFLOW_LIMIT: usize = 3;

/// A single flow unit (one paragraph line or one table row).
struct Unit {
    height: f64,
    /// Authored paragraph spacing, suppressed at a fresh page/column top.
    gap_before: f64,
    /// Float clearance that remains active even at a fresh page/column top.
    float_leading: f64,
    items: Vec<LineItem>,
    anchored_items: Vec<AnchoredLineItem>,
    page_floats: Vec<PageFloat>,
    break_before: bool,
    column_break_before: bool,
    group: usize,
    table_id: Option<usize>,
    is_header_row: bool,
    outline: Option<(String, u8)>,
    note_refs: Vec<(crate::oxml::wml::NoteKind, i64)>,
    frame: Option<FramePlacement>,
    block_index: usize,
    floating_table: Option<FloatingTablePlacement>,
}

#[derive(Debug, Clone, Copy)]
struct FramePlacement {
    props: FrameProps,
    width_pt: f64,
    height_pt: f64,
}

#[derive(Debug, Clone, Copy)]
struct FloatingTablePlacement {
    horizontal: AnchorOrigin,
    vertical: AnchorOrigin,
    x_pt: f64,
    y_pt: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageBandSide {
    Left,
    Right,
    Full,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedPageBand {
    side: PageBandSide,
    top_pt: f64,
    bottom_pt: f64,
    inset_pt: f64,
    uses_text_baseline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FloatCarrier {
    block_index: usize,
    float_index: usize,
}

#[derive(Default)]
struct PageBandSchedule {
    block_placements: HashMap<usize, (usize, f64)>,
    bands: HashMap<usize, Vec<ResolvedPageBand>>,
}

struct SectionPass {
    units: Vec<Unit>,
    pages: Vec<WorkPage>,
    counters: ListCounters,
    warnings: Vec<Warning>,
}

impl PageBandSchedule {
    fn wrap_for_block(&self, block_index: usize, content_width: f64) -> Option<WrapExclusion> {
        let (page_index, unit_top) = self.block_placements.get(&block_index).copied()?;
        let mut left_band = None;
        let mut right_band = None;
        for band in self.bands.get(&page_index).into_iter().flatten() {
            if band.bottom_pt <= unit_top + 0.01 {
                continue;
            }
            let edge = ExclusionBand {
                top_remaining_pt: (band.top_pt - unit_top).max(0.0),
                remaining_pt: band.bottom_pt - unit_top,
                inset_pt: if band.side == PageBandSide::Full {
                    content_width
                } else {
                    band.inset_pt
                },
                uses_text_baseline: band.uses_text_baseline,
            };
            match band.side {
                PageBandSide::Left | PageBandSide::Full => {
                    if left_band.is_none() {
                        left_band = Some(edge);
                    }
                }
                PageBandSide::Right => {
                    if right_band.is_none() {
                        right_band = Some(edge);
                    }
                }
            }
        }
        (left_band.is_some() || right_band.is_some()).then_some(WrapExclusion {
            left_band,
            right_band,
        })
    }
}

fn advance_wrap(wrap: WrapExclusion, distance: f64) -> Option<WrapExclusion> {
    let advance = |band: Option<ExclusionBand>| {
        band.and_then(|mut band| {
            band.top_remaining_pt = (band.top_remaining_pt - distance).max(0.0);
            band.remaining_pt -= distance;
            (band.remaining_pt > 0.01).then_some(band)
        })
    };
    let advanced = WrapExclusion {
        left_band: advance(wrap.left_band),
        right_band: advance(wrap.right_band),
    };
    (advanced.left_band.is_some() || advanced.right_band.is_some()).then_some(advanced)
}

fn combine_wraps(
    active: Option<WrapExclusion>,
    scheduled: Option<WrapExclusion>,
) -> Result<Option<WrapExclusion>> {
    match (active, scheduled) {
        (None, other) | (other, None) => Ok(other),
        (Some(active), Some(scheduled)) => {
            let merge_edge = |active: Option<ExclusionBand>,
                              scheduled: Option<ExclusionBand>|
             -> Result<Option<ExclusionBand>> {
                match (active, scheduled) {
                    (None, other) | (other, None) => Ok(other),
                    (Some(active), Some(scheduled))
                        if (active.inset_pt - scheduled.inset_pt).abs() <= 0.01
                            && active.uses_text_baseline == scheduled.uses_text_baseline =>
                    {
                        Ok(Some(scheduled))
                    }
                    (Some(_), Some(_)) => Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        "overlapping anchored drawing exclusions",
                    )),
                }
            };
            Ok(Some(WrapExclusion {
                left_band: merge_edge(active.left_band, scheduled.left_band)?,
                right_band: merge_edge(active.right_band, scheduled.right_band)?,
            }))
        }
    }
}

fn wrap_remaining(wrap: WrapExclusion) -> f64 {
    wrap.left_band
        .into_iter()
        .chain(wrap.right_band)
        .map(|band| band.remaining_pt)
        .fold(0.0, f64::max)
}

/// Lays out a resolved document into pages plus emitter resources.
pub fn layout_docx(doc: &WordDoc) -> Result<(LaidOutDocument, Vec<Warning>)> {
    let mut warnings = Vec::new();
    let mut counters = ListCounters::default();
    let boundaries: Vec<(usize, &SectPr)> = doc
        .document
        .body
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            BlockContent::Paragraph(paragraph) => paragraph
                .sect_break
                .as_ref()
                .map(|section| (index, section)),
            BlockContent::Table(_) => None,
        })
        .collect();
    let mut all_sections: Vec<&SectPr> = boundaries.iter().map(|(_, section)| *section).collect();
    all_sections.push(&doc.document.final_sect);
    for pair in all_sections.windows(2) {
        let current = pair[0];
        let next = pair[1];
        if current.sect_type == SectType::Continuous && geometry_differs(current, next) {
            return Err(OfficeError::unsupported(
                UnsupportedCode::UnknownElement,
                "continuous section break changes page geometry",
            ));
        }
    }

    let mut sections = Vec::new();
    let mut start = 0usize;
    for (boundary_index, (block_index, section)) in boundaries.iter().enumerate() {
        let next = boundaries
            .get(boundary_index + 1)
            .map(|(_, section)| *section)
            .unwrap_or(&doc.document.final_sect);
        if section.sect_type == SectType::Continuous && !geometry_differs(section, next) {
            continue;
        }
        sections.push((&doc.document.body[start..=*block_index], *section));
        start = *block_index + 1;
    }
    sections.push((&doc.document.body[start..], &doc.document.final_sect));

    let mut batches = Vec::new();
    let mut pages_before = 0usize;
    for (blocks, section) in sections {
        let geo = PageGeometry::from_section(section);
        let (column_widths, column_offsets) = section_columns(&geo, &section.cols);
        let layout_width = column_widths.iter().copied().fold(f64::INFINITY, f64::min);
        if section.cols.is_multi() && blocks_contain_anchors(blocks) {
            return Err(OfficeError::unsupported(
                UnsupportedCode::AnchoredDrawing,
                "anchored drawing in a multi-column section",
            ));
        }
        let counter_start = counters.clone();
        let mut pass = run_section_pass(
            doc,
            blocks,
            &geo,
            layout_width,
            &column_widths,
            &column_offsets,
            &counter_start,
            None,
        )?;
        if pass.units.iter().any(|unit| !unit.page_floats.is_empty()) {
            let mut previous_carriers = float_carriers(&pass.pages);
            let mut converged = false;
            for _ in 0..PAGE_FLOAT_REFLOW_LIMIT {
                let schedule = page_band_schedule(&pass.units, &pass.pages);
                let next = run_section_pass(
                    doc,
                    blocks,
                    &geo,
                    layout_width,
                    &column_widths,
                    &column_offsets,
                    &counter_start,
                    Some(&schedule),
                )?;
                let next_carriers = float_carriers(&next.pages);
                pass = next;
                if next_carriers == previous_carriers {
                    converged = true;
                    break;
                }
                previous_carriers = next_carriers;
            }
            if !converged {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::AnchoredDrawing,
                    "page-anchored drawing did not converge",
                ));
            }
        }
        counters = pass.counters;
        warnings.extend(pass.warnings);
        let units = pass.units;
        let mut pages = pass.pages;
        align_body_vertically(section.vertical_align, &geo, &units, &mut pages);
        place_footnotes(doc, &mut warnings, &geo, &mut pages)?;
        let next_page_number = pages_before + 1;
        let needs_blank = match section.sect_type {
            SectType::OddPage => next_page_number.is_multiple_of(2),
            SectType::EvenPage => !next_page_number.is_multiple_of(2),
            _ => false,
        };
        if needs_blank {
            pages.insert(0, new_page(&geo));
        }
        pages_before += pages.len();
        batches.push((section, geo, units, pages));
    }

    let mut endnote_refs = Vec::new();
    for (_, _, units, _) in &batches {
        for unit in units {
            for note_ref in &unit.note_refs {
                if note_ref.0 == NoteKind::Endnote && !endnote_refs.contains(note_ref) {
                    endnote_refs.push(*note_ref);
                }
            }
        }
    }
    if !endnote_refs.is_empty()
        && let Some((_, geo, _, pages)) = batches.last_mut()
    {
        append_endnotes(doc, &mut warnings, geo, &endnote_refs, pages)?;
    }

    let total_pages: u32 = batches.iter().map(|batch| batch.3.len() as u32).sum();
    let mut page_offset = 0u32;
    let mut outline = Vec::new();
    let mut pages = Vec::new();
    for (section, geo, units, mut batch_pages) in batches {
        render_headers_footers(
            doc,
            &mut warnings,
            section,
            &geo,
            &mut batch_pages,
            total_pages,
            page_offset,
        )?;
        let mut batch_outline = collect_outline(&units, &batch_pages, &geo);
        for entry in &mut batch_outline {
            entry.page += page_offset;
        }
        outline.extend(batch_outline);
        page_offset += batch_pages.len() as u32;
        pages.extend(batch_pages);
    }

    let laid = LaidOutDocument {
        pages: pages.into_iter().map(finish_work_page).collect(),
        images: clone_images(&doc.images),
        outline,
        dates: doc.dates.clone(),
    };
    Ok((laid, warnings))
}

fn align_body_vertically(
    alignment: PageVAlign,
    geo: &PageGeometry,
    units: &[Unit],
    pages: &mut [WorkPage],
) {
    if alignment == PageVAlign::Top {
        return;
    }
    for page in pages {
        let body_bottom = page
            .unit_ys
            .iter()
            .filter_map(|(index, y)| units.get(*index).map(|unit| y + unit.height))
            .fold(geo.content_top_pt, f64::max);
        let free = (geo.content_bottom_pt - body_bottom).max(0.0);
        let shift = match alignment {
            PageVAlign::Center => free / 2.0,
            PageVAlign::Bottom => free,
            PageVAlign::Top => 0.0,
        };
        for item in &mut page.page.items {
            item.y_pt += shift;
        }
        for (_, item) in &mut page.underlays {
            item.y_pt += shift;
        }
        for (_, item) in &mut page.overlays {
            item.y_pt += shift;
        }
        for (_, y) in &mut page.unit_ys {
            *y += shift;
        }
    }
}

fn section_columns(geo: &PageGeometry, spec: &ColumnSpec) -> (Vec<f64>, Vec<f64>) {
    let count = (spec.num as usize).max(spec.cols.len()).max(1);
    let space = spec.space_pt.unwrap_or(36.0).max(0.0);
    let available = (geo.content_width_pt() - space * count.saturating_sub(1) as f64).max(1.0);
    let widths = if spec.cols.len() == count && spec.cols.iter().all(|width| *width > 0.0) {
        spec.cols.clone()
    } else {
        vec![available / count as f64; count]
    };
    let mut offsets = Vec::with_capacity(count);
    let mut x = 0.0;
    for (index, width) in widths.iter().enumerate() {
        offsets.push(x);
        x += *width + spec.spaces.get(index).copied().unwrap_or(space);
    }
    (widths, offsets)
}

fn geometry_differs(left: &SectPr, right: &SectPr) -> bool {
    left.page_w_pt != right.page_w_pt
        || left.page_h_pt != right.page_h_pt
        || left.orientation != right.orientation
        || left.margins.top != right.margins.top
        || left.margins.right != right.margins.right
        || left.margins.bottom != right.margins.bottom
        || left.margins.left != right.margins.left
        || left.margins.gutter != right.margins.gutter
}

fn clone_images(images: &[EncodedImage]) -> Vec<EncodedImage> {
    images.to_vec()
}

/// A page under construction plus the y where body content ended.
struct WorkPage {
    page: Page,
    underlays: Vec<(u32, Positioned)>,
    overlays: Vec<(u32, Positioned)>,
    page_underlays: Vec<(u32, Positioned)>,
    page_overlays: Vec<(u32, Positioned)>,
    /// The set of (unit index) placed, for outline mapping.
    unit_ys: Vec<(usize, f64)>,
    note_refs: Vec<(crate::oxml::wml::NoteKind, i64)>,
    float_bands: Vec<ResolvedPageBand>,
    float_carriers: Vec<FloatCarrier>,
}

// --- Unit construction ------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_section_pass(
    doc: &WordDoc,
    blocks: &[BlockContent],
    geo: &PageGeometry,
    layout_width: f64,
    column_widths: &[f64],
    column_offsets: &[f64],
    counter_start: &ListCounters,
    schedule: Option<&PageBandSchedule>,
) -> Result<SectionPass> {
    let mut warnings = Vec::new();
    let mut counters = counter_start.clone();
    let mut table_headers = HashMap::new();
    let units = build_units(
        doc,
        &mut warnings,
        blocks,
        doc.main_rels(),
        layout_width,
        &mut counters,
        &mut table_headers,
        schedule,
    )?;
    let pages = paginate(&units, geo, &table_headers, column_widths, column_offsets);
    validate_page_bands(&pages, geo.content_width_pt())?;
    Ok(SectionPass {
        units,
        pages,
        counters,
        warnings,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_units(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    blocks: &[BlockContent],
    rels: &HashMap<String, String>,
    content_w: f64,
    counters: &mut ListCounters,
    table_headers: &mut HashMap<usize, Vec<Unit>>,
    page_schedule: Option<&PageBandSchedule>,
) -> Result<Vec<Unit>> {
    let mut units = Vec::new();
    let mut group_counter = 0usize;
    let mut next_group = || {
        group_counter += 1;
        group_counter
    };
    let mut keep_next_group: Option<usize> = None;
    let mut prev_after = 0.0_f64;
    let mut table_counter = 0usize;
    let mut pending_break = false;
    let mut pending_column_break = false;
    let mut active_wrap: Option<WrapExclusion> = None;
    let mut previous_scheduled_page = None;

    for (block_index, block) in blocks.iter().enumerate() {
        let scheduled_page = page_schedule.and_then(|schedule| {
            schedule
                .block_placements
                .get(&block_index)
                .map(|(page, _)| *page)
        });
        if previous_scheduled_page.is_some()
            && scheduled_page.is_some()
            && previous_scheduled_page != scheduled_page
        {
            active_wrap = None;
        }
        if scheduled_page.is_some() {
            previous_scheduled_page = scheduled_page;
        }
        match block {
            BlockContent::Paragraph(p) => {
                let label = counter_label(doc, warnings, p, counters)?;
                let effective_para = doc.styles.para_props(&p.props);
                let frame = effective_para.frame;
                if let Some(detail) = frame.and_then(|value| value.unsupported) {
                    return Err(OfficeError::unsupported(UnsupportedCode::TextBox, detail));
                }
                let frame_width = frame
                    .and_then(|value| value.width_pt)
                    .unwrap_or(content_w)
                    .max(1.0);
                if pending_break || pending_column_break || effective_para.page_break_before {
                    active_wrap = None;
                } else if let Some(wrap) = active_wrap {
                    let gap =
                        prev_after.max(effective_para.spacing.before_pt.unwrap_or(0.0).max(0.0));
                    active_wrap = advance_wrap(wrap, gap);
                }
                let scheduled_wrap = page_schedule
                    .and_then(|schedule| schedule.wrap_for_block(block_index, frame_width));
                let paragraph_wrap = combine_wraps(active_wrap, scheduled_wrap)?;
                let layout = layout_paragraph_with_wrap(
                    doc,
                    warnings,
                    p,
                    rels,
                    frame_width,
                    label,
                    None,
                    paragraph_wrap,
                )?;
                active_wrap = layout.wrap_after;
                if let Some(frame) = frame {
                    push_frame_unit(
                        layout,
                        frame,
                        frame_width,
                        &mut units,
                        &mut next_group,
                        &mut keep_next_group,
                        &mut prev_after,
                        &mut pending_break,
                        &mut pending_column_break,
                        block_index,
                    )?;
                } else {
                    push_paragraph_units(
                        layout,
                        &mut units,
                        &mut next_group,
                        &mut keep_next_group,
                        &mut prev_after,
                        &mut pending_break,
                        &mut pending_column_break,
                        block_index,
                    );
                }
            }
            BlockContent::Table(t) => {
                let scheduled_wrap = page_schedule
                    .and_then(|schedule| schedule.wrap_for_block(block_index, content_w));
                active_wrap = combine_wraps(active_wrap, scheduled_wrap)?;
                // A table cannot occupy the single text interval beside an
                // active float. Clear it below the exclusion before laying out
                // the table, matching Word's block-flow behavior.
                let table_clearance = active_wrap
                    .take()
                    .map(|wrap| wrap_remaining(wrap).max(0.0))
                    .unwrap_or(0.0);
                let table_gap = prev_after.max(table_clearance);
                let table_id = table_counter;
                table_counter += 1;
                keep_next_group = None;
                let table_layout = layout_table(doc, warnings, t, rels, content_w, 0.0, counters)?;
                if let Some(floating) = t.props.floating {
                    let mut items = Vec::new();
                    let mut row_y = 0.0;
                    for row in table_layout.rows {
                        for item in row.items {
                            items.push(LineItem {
                                x_pt: item.x_pt,
                                y_pt: row_y + item.y_pt,
                                item: item.item,
                            });
                        }
                        row_y += row.height_pt;
                    }
                    let [_, dist_bottom, dist_left, dist_right] = floating.distances_pt;
                    let left_hugging = floating.x_pt <= 0.01;
                    let right_hugging = floating.x_pt + table_layout.width_pt >= content_w - 0.01;
                    let (side_wrap, text_window) = if left_hugging {
                        (
                            Some(WrapExclusion {
                                left_band: Some(ExclusionBand {
                                    top_remaining_pt: floating.y_pt.max(0.0),
                                    remaining_pt: floating.y_pt + row_y + dist_bottom,
                                    inset_pt: floating.x_pt + table_layout.width_pt + dist_right,
                                    uses_text_baseline: false,
                                }),
                                right_band: None,
                            }),
                            content_w - floating.x_pt - table_layout.width_pt - dist_right,
                        )
                    } else if right_hugging {
                        (
                            Some(WrapExclusion {
                                left_band: None,
                                right_band: Some(ExclusionBand {
                                    top_remaining_pt: floating.y_pt.max(0.0),
                                    remaining_pt: floating.y_pt + row_y + dist_bottom,
                                    inset_pt: content_w - floating.x_pt + dist_left,
                                    uses_text_baseline: false,
                                }),
                            }),
                            floating.x_pt - dist_left,
                        )
                    } else {
                        (None, 0.0)
                    };
                    let wraps_text = floating.vertical == AnchorOrigin::Paragraph
                        && side_wrap.is_some()
                        && text_window >= MIN_TEXT_WINDOW_PT;
                    if floating.vertical == AnchorOrigin::Paragraph
                        && side_wrap.is_some()
                        && text_window < MIN_TEXT_WINDOW_PT
                    {
                        warnings.push(Warning::new(
                            "floating_table_block_fallback",
                            "floating table leaves less than 36pt for side-wrapped text",
                        ));
                    }
                    units.push(Unit {
                        height: if wraps_text {
                            0.0
                        } else {
                            floating.y_pt.max(0.0) + row_y + dist_bottom
                        },
                        gap_before: table_gap,
                        float_leading: 0.0,
                        items,
                        anchored_items: Vec::new(),
                        page_floats: Vec::new(),
                        break_before: std::mem::take(&mut pending_break),
                        column_break_before: std::mem::take(&mut pending_column_break),
                        group: next_group(),
                        table_id: None,
                        is_header_row: false,
                        outline: None,
                        note_refs: Vec::new(),
                        frame: None,
                        block_index,
                        floating_table: Some(FloatingTablePlacement {
                            horizontal: floating.horizontal,
                            vertical: floating.vertical,
                            x_pt: floating.x_pt,
                            y_pt: floating.y_pt,
                        }),
                    });
                    active_wrap = if wraps_text { side_wrap } else { None };
                    prev_after = 0.0;
                    continue;
                }
                let mut headers = Vec::new();
                let mut first_row = true;
                for row in table_layout.rows {
                    let unit = Unit {
                        height: row.height_pt,
                        gap_before: if first_row { table_gap } else { 0.0 },
                        float_leading: 0.0,
                        items: row.items,
                        anchored_items: Vec::new(),
                        page_floats: Vec::new(),
                        break_before: first_row && std::mem::take(&mut pending_break),
                        column_break_before: first_row && std::mem::take(&mut pending_column_break),
                        group: next_group(),
                        table_id: Some(table_id),
                        is_header_row: row.is_header,
                        outline: None,
                        note_refs: Vec::new(),
                        frame: None,
                        block_index,
                        floating_table: None,
                    };
                    if row.is_header {
                        headers.push(clone_unit(&unit));
                    }
                    units.push(unit);
                    first_row = false;
                }
                if !headers.is_empty() {
                    table_headers.insert(table_id, headers);
                }
                prev_after = 0.0;
            }
        }
    }
    Ok(units)
}

fn blocks_contain_anchors(blocks: &[BlockContent]) -> bool {
    fn run_has_anchor(run: &Run) -> bool {
        run.content.iter().any(|content| {
            matches!(content, RunContent::Drawing(drawing) if drawing.anchor.is_some())
                || matches!(content, RunContent::VmlShape(_))
        })
    }

    fn inline_has_anchor(inline: &InlineContent) -> bool {
        match inline {
            InlineContent::Run(run) => run_has_anchor(run),
            InlineContent::Hyperlink { runs, .. } => runs.iter().any(run_has_anchor),
            InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                field.content.iter().any(inline_has_anchor)
            }
            InlineContent::Math(_) => false,
        }
    }

    blocks.iter().any(|block| match block {
        BlockContent::Paragraph(paragraph) => paragraph.runs.iter().any(inline_has_anchor),
        BlockContent::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .any(|cell| blocks_contain_anchors(&cell.content)),
    })
}

fn clone_unit(u: &Unit) -> Unit {
    Unit {
        height: u.height,
        gap_before: u.gap_before,
        float_leading: u.float_leading,
        items: u.items.iter().map(clone_line_item).collect(),
        anchored_items: u
            .anchored_items
            .iter()
            .map(clone_anchored_line_item)
            .collect(),
        page_floats: u.page_floats.iter().map(clone_page_float).collect(),
        break_before: u.break_before,
        column_break_before: u.column_break_before,
        group: u.group,
        table_id: u.table_id,
        is_header_row: u.is_header_row,
        outline: u.outline.clone(),
        note_refs: u.note_refs.clone(),
        frame: u.frame,
        block_index: u.block_index,
        floating_table: u.floating_table,
    }
}

fn clone_line_item(li: &LineItem) -> LineItem {
    LineItem {
        x_pt: li.x_pt,
        y_pt: li.y_pt,
        item: li.item.clone(),
    }
}

fn clone_anchored_line_item(item: &AnchoredLineItem) -> AnchoredLineItem {
    AnchoredLineItem {
        x_pt: item.x_pt,
        y_pt: item.y_pt,
        behind_doc: item.behind_doc,
        relative_height: item.relative_height,
        item: item.item.clone(),
    }
}

fn clone_page_float(float: &PageFloat) -> PageFloat {
    PageFloat {
        items: float.items.iter().map(clone_line_item).collect(),
        w_pt: float.w_pt,
        h_pt: float.h_pt,
        x_origin: float.x_origin,
        x_pt: float.x_pt,
        x_align: float.x_align,
        y_origin: float.y_origin,
        y_pt: float.y_pt,
        y_align: float.y_align,
        wrap: float.wrap,
        distances_pt: float.distances_pt,
        behind_doc: float.behind_doc,
        relative_height: float.relative_height,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_paragraph_units(
    layout: ParaLayout,
    units: &mut Vec<Unit>,
    next_group: &mut impl FnMut() -> usize,
    keep_next_group: &mut Option<usize>,
    prev_after: &mut f64,
    pending_break: &mut bool,
    pending_column_break: &mut bool,
    block_index: usize,
) {
    let l = layout.lines.len();
    let mut ids = paragraph_group_ids(&layout, l, next_group);
    if let Some(previous_group) = keep_next_group.take()
        && let Some(first_group) = ids.first().copied()
    {
        for group in &mut ids {
            if *group == first_group {
                *group = previous_group;
            }
        }
    }
    let gap = (*prev_after).max(layout.space_before_pt);
    let mut brk = *pending_break;
    let mut column_brk = *pending_column_break;
    let note_refs = layout.note_refs.clone();
    let mut page_floats = Some(layout.page_floats);

    for (i, line) in layout.lines.into_iter().enumerate() {
        let group = ids[i];
        let outline = (i == 0)
            .then(|| layout.outline_level.map(|lvl| (layout.text.clone(), lvl)))
            .flatten();
        let break_before = brk || (i == 0 && layout.page_break_before);
        let column_break_before = column_brk;
        brk = line.page_break_after;
        column_brk = line.column_break_after;
        units.push(Unit {
            height: line.height_pt,
            gap_before: if i == 0 { gap } else { 0.0 },
            float_leading: if i == 0 { layout.float_leading_pt } else { 0.0 },
            items: line.items,
            anchored_items: line.anchored_items,
            page_floats: if i == 0 {
                page_floats.take().unwrap_or_default()
            } else {
                Vec::new()
            },
            break_before,
            column_break_before,
            group,
            table_id: None,
            is_header_row: false,
            outline,
            note_refs: if i == 0 {
                note_refs.clone()
            } else {
                Vec::new()
            },
            frame: None,
            block_index,
            floating_table: None,
        });
    }
    *pending_break = brk;
    *pending_column_break = column_brk;
    *prev_after = layout.space_after_pt;
    if layout.keep_next {
        *keep_next_group = Some(ids[l.saturating_sub(1)]);
    } else {
        *keep_next_group = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn push_frame_unit(
    layout: ParaLayout,
    frame: FrameProps,
    width_pt: f64,
    units: &mut Vec<Unit>,
    next_group: &mut impl FnMut() -> usize,
    keep_next_group: &mut Option<usize>,
    prev_after: &mut f64,
    pending_break: &mut bool,
    pending_column_break: &mut bool,
    block_index: usize,
) -> Result<()> {
    if !layout.page_floats.is_empty()
        || layout
            .lines
            .iter()
            .any(|line| !line.anchored_items.is_empty())
    {
        return Err(OfficeError::unsupported(
            UnsupportedCode::AnchoredDrawing,
            "anchored drawing in a framed paragraph",
        ));
    }
    let mut items = Vec::new();
    let mut content_height = 0.0;
    for line in layout.lines {
        for item in line.items {
            items.push(LineItem {
                x_pt: item.x_pt,
                y_pt: content_height + item.y_pt,
                item: item.item,
            });
        }
        content_height += line.height_pt;
    }
    let height_pt = match (frame.height_rule, frame.height_pt) {
        (FrameHeightRule::Exact, Some(height)) => height,
        (_, Some(height)) => height.max(content_height),
        (_, None) => content_height,
    };
    if frame.height_rule == FrameHeightRule::Exact {
        // Word preserves the authored frame box and clips painted overflow. Keep
        // every child for faithful partial-line clipping and text extraction.
        // Link annotations are not PDF content-stream items, so intersect their
        // rectangles with the same box and leave them at page level.
        let mut clipped_items = Vec::new();
        let mut clipped_links = Vec::new();
        for item in items {
            match item.item {
                crate::layout::Item::Link { w_pt, h_pt, target } => {
                    let left = item.x_pt.max(0.0);
                    let top = item.y_pt.max(0.0);
                    let right = (item.x_pt + w_pt).min(width_pt);
                    let bottom = (item.y_pt + h_pt).min(height_pt);
                    if right > left && bottom > top {
                        clipped_links.push(LineItem {
                            x_pt: left,
                            y_pt: top,
                            item: crate::layout::Item::Link {
                                w_pt: right - left,
                                h_pt: bottom - top,
                                target,
                            },
                        });
                    }
                }
                other => {
                    clipped_items.push(Positioned {
                        x_pt: item.x_pt,
                        y_pt: item.y_pt,
                        item: other,
                    });
                }
            }
        }
        items = vec![LineItem {
            x_pt: 0.0,
            y_pt: 0.0,
            item: crate::layout::Item::Clip {
                commands: vec![
                    PathCommand::MoveTo { x: 0.0, y: 0.0 },
                    PathCommand::LineTo {
                        x: width_pt,
                        y: 0.0,
                    },
                    PathCommand::LineTo {
                        x: width_pt,
                        y: height_pt,
                    },
                    PathCommand::LineTo {
                        x: 0.0,
                        y: height_pt,
                    },
                    PathCommand::Close,
                ],
                fill_rule: FillRule::NonZero,
                items: clipped_items,
            },
        }];
        items.extend(clipped_links);
    }
    let offset_bottom = frame.y_pt.unwrap_or(0.0).max(0.0) + height_pt;
    let exclusion_height = if frame.wrap == WrapMode::None {
        0.0
    } else {
        offset_bottom
    };
    let group = next_group();
    units.push(Unit {
        height: exclusion_height,
        gap_before: (*prev_after).max(layout.space_before_pt),
        float_leading: layout.float_leading_pt,
        items,
        anchored_items: Vec::new(),
        page_floats: Vec::new(),
        break_before: std::mem::take(pending_break) || layout.page_break_before,
        column_break_before: std::mem::take(pending_column_break),
        group,
        table_id: None,
        is_header_row: false,
        outline: layout
            .outline_level
            .map(|level| (layout.text.clone(), level)),
        note_refs: layout.note_refs,
        frame: Some(FramePlacement {
            props: frame,
            width_pt,
            height_pt,
        }),
        block_index,
        floating_table: None,
    });
    *prev_after = layout.space_after_pt;
    *keep_next_group = layout.keep_next.then_some(group);
    Ok(())
}

/// Assigns keep-group ids to a paragraph's lines (keep_lines / widow rules).
fn paragraph_group_ids(
    layout: &ParaLayout,
    l: usize,
    next_group: &mut impl FnMut() -> usize,
) -> Vec<usize> {
    if l == 0 {
        return Vec::new();
    }
    if layout.keep_lines || l <= 1 {
        let g = next_group();
        return vec![g; l];
    }
    let mut ids: Vec<usize> = (0..l).map(|_| next_group()).collect();
    if layout.widow_control {
        if l <= 3 {
            let g = ids[0];
            for id in ids.iter_mut() {
                *id = g;
            }
        } else {
            ids[1] = ids[0]; // no orphan
            ids[l - 1] = ids[l - 2]; // no widow
        }
    }
    ids
}

fn counter_label(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    p: &Paragraph,
    counters: &mut ListCounters,
) -> Result<Option<PlacedLabel>> {
    let eff = doc.styles.para_props(&p.props);
    let Some(num_ref) = eff.numbering else {
        return Ok(None);
    };
    let Some((text, level)) = counters.advance(doc, num_ref) else {
        return Ok(None);
    };
    build_label(doc, warnings, p, &text, &level)
}

// --- Pagination -------------------------------------------------------------

fn paginate(
    units: &[Unit],
    geo: &PageGeometry,
    table_headers: &HashMap<usize, Vec<Unit>>,
    column_widths: &[f64],
    column_offsets: &[f64],
) -> Vec<WorkPage> {
    let mut pages: Vec<WorkPage> = Vec::new();
    let mut page = new_page(geo);
    let mut y = geo.content_top_pt;
    let mut column = 0usize;
    let has_footnotes = units.iter().any(|unit| {
        unit.note_refs
            .iter()
            .any(|(kind, _)| *kind == NoteKind::Footnote)
    });
    let full_bottom = if has_footnotes {
        geo.content_bottom_pt - geo.content_height_pt() * 0.25
    } else {
        geo.content_bottom_pt
    };
    let total_height: f64 = units
        .iter()
        .map(|unit| unit.gap_before + unit.float_leading + unit.height)
        .sum();
    let column_height = full_bottom - geo.content_top_pt;
    let content_bottom = if column_widths.len() > 1
        && total_height > 0.0
        && total_height <= column_height * column_widths.len() as f64
    {
        geo.content_top_pt + (total_height / column_widths.len() as f64).ceil() + 0.5
    } else {
        full_bottom
    };
    let mut active_table: Option<usize> = None;
    let mut table_first_page: Option<usize> = None;

    // Coalesce contiguous same-group units into atomic runs.
    let runs = coalesce_runs(units);

    for run in runs {
        let mut probe_y = y;
        for &i in &run {
            probe_y += effective_gap_before(&units[i], probe_y, geo.content_top_pt);
            probe_y += units[i].height;
        }
        let break_before = units[run[0]].break_before;
        let fits = probe_y <= content_bottom + 0.01;

        if break_before && (y > geo.content_top_pt || column > 0) {
            pages.push(std::mem::replace(&mut page, new_page(geo)));
            y = geo.content_top_pt;
            column = 0;
        } else if units[run[0]].column_break_before || !fits {
            if column + 1 < column_widths.len() {
                column += 1;
                y = geo.content_top_pt;
            } else if y > geo.content_top_pt || column > 0 {
                pages.push(std::mem::replace(&mut page, new_page(geo)));
                y = geo.content_top_pt;
                column = 0;
            }
            // Re-emit table headers on a continuation page.
            if let Some(t) = active_table
                && table_first_page.is_some()
                && let Some(headers) = table_headers.get(&t)
            {
                for h in headers {
                    place_unit(
                        &mut page,
                        &mut y,
                        geo,
                        column_offsets[column],
                        h,
                        usize::MAX,
                    );
                }
            }
        }

        for &i in &run {
            let u = &units[i];
            if let Some(t) = u.table_id {
                if active_table != Some(t) {
                    active_table = Some(t);
                    table_first_page = Some(pages.len());
                }
            } else {
                active_table = None;
                table_first_page = None;
            }
            // A single oversized unit taller than a page is placed anyway
            // (content simply overflows the bottom margin — rare).
            place_unit(&mut page, &mut y, geo, column_offsets[column], u, i);
        }
    }
    if !work_page_is_empty(&page) || pages.is_empty() {
        pages.push(page);
    }
    pages
}

fn float_carriers(pages: &[WorkPage]) -> Vec<(FloatCarrier, usize)> {
    let mut carriers = pages
        .iter()
        .enumerate()
        .flat_map(|(page_index, page)| {
            page.float_carriers
                .iter()
                .copied()
                .map(move |carrier| (carrier, page_index))
        })
        .collect::<Vec<_>>();
    carriers.sort_unstable();
    carriers
}

fn page_band_schedule(units: &[Unit], pages: &[WorkPage]) -> PageBandSchedule {
    let mut schedule = PageBandSchedule::default();
    for (page_index, page) in pages.iter().enumerate() {
        schedule.bands.insert(page_index, page.float_bands.clone());
        for (unit_index, unit_top) in &page.unit_ys {
            if let Some(unit) = units.get(*unit_index) {
                schedule
                    .block_placements
                    .entry(unit.block_index)
                    .or_insert((page_index, *unit_top));
            }
        }
    }
    schedule
}

fn validate_page_bands(pages: &[WorkPage], content_width: f64) -> Result<()> {
    for page in pages {
        for band in &page.float_bands {
            if band.side != PageBandSide::Full
                && content_width - band.inset_pt < MIN_TEXT_WINDOW_PT - 0.01
            {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::AnchoredDrawing,
                    "anchored drawings leave no text window",
                ));
            }
        }
        for (index, left) in page.float_bands.iter().enumerate() {
            for right in &page.float_bands[index + 1..] {
                if left.top_pt >= right.bottom_pt - 0.01 || right.top_pt >= left.bottom_pt - 0.01 {
                    continue;
                }
                let opposite = matches!(
                    (left.side, right.side),
                    (PageBandSide::Left, PageBandSide::Right)
                        | (PageBandSide::Right, PageBandSide::Left)
                );
                if !opposite {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        "overlapping anchored drawing exclusions",
                    ));
                }
                if content_width - left.inset_pt - right.inset_pt < MIN_TEXT_WINDOW_PT - 0.01 {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        "anchored drawings leave no text window",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Groups consecutive units sharing a group id into atomic runs.
fn coalesce_runs(units: &[Unit]) -> Vec<Vec<usize>> {
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for i in 0..units.len() {
        if let Some(last) = runs.last_mut()
            && units[*last.last().unwrap()].group == units[i].group
            && !units[i].break_before
            && !units[i].column_break_before
        {
            last.push(i);
        } else {
            runs.push(vec![i]);
        }
    }
    runs
}

fn place_unit(
    page: &mut WorkPage,
    y: &mut f64,
    geo: &PageGeometry,
    column_offset: f64,
    unit: &Unit,
    index: usize,
) {
    *y += effective_gap_before(unit, *y, geo.content_top_pt);
    let unit_top = *y;
    let (frame_x, frame_y) = if let Some(floating) = unit.floating_table {
        floating_table_origin(floating, geo, column_offset, unit_top)
    } else {
        unit.frame
            .map(|frame| frame_origin(frame, geo, column_offset, unit_top))
            .unwrap_or((geo.content_left_pt + column_offset, unit_top))
    };
    for li in &unit.items {
        page.page.items.push(Positioned {
            x_pt: frame_x + li.x_pt,
            y_pt: frame_y + li.y_pt,
            item: li.item.clone(),
        });
    }
    for item in &unit.anchored_items {
        let anchored = Positioned {
            x_pt: frame_x + item.x_pt,
            y_pt: frame_y + item.y_pt,
            item: item.item.clone(),
        };
        if item.behind_doc {
            page.underlays.push((item.relative_height, anchored));
        } else {
            page.overlays.push((item.relative_height, anchored));
        }
    }
    for (float_index, page_float) in unit.page_floats.iter().enumerate() {
        place_page_float(
            page,
            geo,
            column_offset,
            unit.block_index,
            float_index,
            page_float,
        );
    }
    if index != usize::MAX {
        page.unit_ys.push((index, unit_top));
    }
    for note_ref in &unit.note_refs {
        if !page.note_refs.contains(note_ref) {
            page.note_refs.push(*note_ref);
        }
    }
    *y += unit.height;
}

fn place_page_float(
    page: &mut WorkPage,
    geo: &PageGeometry,
    column_offset: f64,
    block_index: usize,
    float_index: usize,
    page_float: &PageFloat,
) {
    let (anchor_x, anchor_width) = match page_float.x_origin {
        AnchorOrigin::Page => (0.0, geo.width_pt),
        AnchorOrigin::Margin => (geo.content_left_pt, geo.content_width_pt()),
        AnchorOrigin::Paragraph => (
            geo.content_left_pt + column_offset,
            geo.content_width_pt() - column_offset,
        ),
    };
    let x = page_float
        .x_align
        .map_or(anchor_x + page_float.x_pt, |align| {
            anchor_x
                + match align {
                    FrameAlign::Start => 0.0,
                    FrameAlign::Center => (anchor_width - page_float.w_pt).max(0.0) / 2.0,
                    FrameAlign::End => (anchor_width - page_float.w_pt).max(0.0),
                }
        });
    let (anchor_y, anchor_height) = match page_float.y_origin {
        AnchorOrigin::Page => (0.0, geo.height_pt),
        AnchorOrigin::Margin => (geo.content_top_pt, geo.content_height_pt()),
        AnchorOrigin::Paragraph => (geo.content_top_pt, geo.content_height_pt()),
    };
    let y = page_float
        .y_align
        .map_or(anchor_y + page_float.y_pt, |align| {
            anchor_y
                + match align {
                    FrameAlign::Start => 0.0,
                    FrameAlign::Center => (anchor_height - page_float.h_pt).max(0.0) / 2.0,
                    FrameAlign::End => (anchor_height - page_float.h_pt).max(0.0),
                }
        });
    for item in &page_float.items {
        let positioned = Positioned {
            x_pt: x + item.x_pt,
            y_pt: y + item.y_pt,
            item: item.item.clone(),
        };
        if page_float.behind_doc {
            page.page_underlays
                .push((page_float.relative_height, positioned));
        } else {
            page.page_overlays
                .push((page_float.relative_height, positioned));
        }
    }
    page.float_carriers.push(FloatCarrier {
        block_index,
        float_index,
    });
    let [dist_top, dist_bottom, dist_left, dist_right] = page_float.distances_pt;
    let top_pt = y - dist_top;
    let bottom_pt = y + page_float.h_pt + dist_bottom;
    let expanded_left = x - dist_left;
    let expanded_right = x + page_float.w_pt + dist_right;
    let band = match page_float.wrap {
        WrapMode::None => None,
        WrapMode::TopAndBottom => Some(ResolvedPageBand {
            side: PageBandSide::Full,
            top_pt,
            bottom_pt,
            inset_pt: geo.content_width_pt(),
            uses_text_baseline: false,
        }),
        WrapMode::Square | WrapMode::Tight => {
            let left_room = (expanded_left - geo.content_left_pt).max(0.0);
            let right_room = (geo.content_right_pt - expanded_right).max(0.0);
            let (side, inset_pt) =
                if expanded_left <= geo.content_left_pt + 0.01 || right_room >= left_room {
                    (
                        PageBandSide::Left,
                        (expanded_right - geo.content_left_pt).max(0.0),
                    )
                } else {
                    (
                        PageBandSide::Right,
                        (geo.content_right_pt - expanded_left).max(0.0),
                    )
                };
            Some(ResolvedPageBand {
                side,
                top_pt,
                bottom_pt,
                inset_pt,
                uses_text_baseline: page_float.wrap == WrapMode::Tight,
            })
        }
    };
    if let Some(band) = band {
        page.float_bands.push(band);
    }
}

fn effective_gap_before(unit: &Unit, y: f64, content_top: f64) -> f64 {
    if (y - content_top).abs() <= 0.01 {
        unit.float_leading
    } else {
        unit.gap_before + unit.float_leading
    }
}

fn frame_origin(
    frame: FramePlacement,
    geo: &PageGeometry,
    column_offset: f64,
    unit_top: f64,
) -> (f64, f64) {
    let (anchor_x, anchor_width) = match frame.props.horizontal {
        AnchorOrigin::Page => (0.0, geo.width_pt),
        AnchorOrigin::Margin => (geo.content_left_pt, geo.content_width_pt()),
        AnchorOrigin::Paragraph => (
            geo.content_left_pt + column_offset,
            geo.content_width_pt() - column_offset,
        ),
    };
    let x = frame.props.x_pt.map_or_else(
        || match frame.props.x_align {
            FrameAlign::Start => anchor_x,
            FrameAlign::Center => anchor_x + (anchor_width - frame.width_pt).max(0.0) / 2.0,
            FrameAlign::End => anchor_x + (anchor_width - frame.width_pt).max(0.0),
        },
        |offset| anchor_x + offset,
    );
    let (anchor_y, anchor_height) = match frame.props.vertical {
        AnchorOrigin::Page => (0.0, geo.height_pt),
        AnchorOrigin::Margin => (geo.content_top_pt, geo.content_height_pt()),
        AnchorOrigin::Paragraph => (unit_top, geo.content_bottom_pt - unit_top),
    };
    let y = frame.props.y_pt.map_or_else(
        || match frame.props.y_align {
            FrameAlign::Start => anchor_y,
            FrameAlign::Center => anchor_y + (anchor_height - frame.height_pt).max(0.0) / 2.0,
            FrameAlign::End => anchor_y + (anchor_height - frame.height_pt).max(0.0),
        },
        |offset| anchor_y + offset,
    );
    (x, y)
}

fn floating_table_origin(
    table: FloatingTablePlacement,
    geo: &PageGeometry,
    column_offset: f64,
    unit_top: f64,
) -> (f64, f64) {
    let x = match table.horizontal {
        AnchorOrigin::Page => 0.0,
        AnchorOrigin::Margin => geo.content_left_pt,
        AnchorOrigin::Paragraph => geo.content_left_pt + column_offset,
    } + table.x_pt;
    let y = match table.vertical {
        AnchorOrigin::Page => 0.0,
        AnchorOrigin::Margin => geo.content_top_pt,
        AnchorOrigin::Paragraph => unit_top,
    } + table.y_pt;
    (x, y)
}

fn new_page(geo: &PageGeometry) -> WorkPage {
    WorkPage {
        page: Page {
            width_pt: geo.width_pt,
            height_pt: geo.height_pt,
            items: Vec::new(),
        },
        underlays: Vec::new(),
        overlays: Vec::new(),
        page_underlays: Vec::new(),
        page_overlays: Vec::new(),
        unit_ys: Vec::new(),
        note_refs: Vec::new(),
        float_bands: Vec::new(),
        float_carriers: Vec::new(),
    }
}

fn work_page_is_empty(page: &WorkPage) -> bool {
    page.page.items.is_empty()
        && page.underlays.is_empty()
        && page.overlays.is_empty()
        && page.page_underlays.is_empty()
        && page.page_overlays.is_empty()
}

fn finish_work_page(mut work: WorkPage) -> Page {
    work.underlays.append(&mut work.page_underlays);
    work.overlays.append(&mut work.page_overlays);
    work.underlays.sort_by_key(|(height, _)| *height);
    work.overlays.sort_by_key(|(height, _)| *height);
    let mut items =
        Vec::with_capacity(work.underlays.len() + work.page.items.len() + work.overlays.len());
    items.extend(work.underlays.into_iter().map(|(_, item)| item));
    items.append(&mut work.page.items);
    items.extend(work.overlays.into_iter().map(|(_, item)| item));
    work.page.items = items;
    work.page
}

fn place_footnotes(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    geo: &PageGeometry,
    pages: &mut Vec<WorkPage>,
) -> Result<()> {
    let mut page_index = 0usize;
    while page_index < pages.len() {
        let refs: Vec<_> = pages[page_index]
            .note_refs
            .iter()
            .copied()
            .filter(|(kind, _)| *kind == NoteKind::Footnote)
            .collect();
        if refs.is_empty() {
            page_index += 1;
            continue;
        }

        let mut note_units = Vec::new();
        let mut counters = ListCounters::default();
        let mut table_headers = HashMap::new();
        for (kind, id) in refs {
            let Some((blocks, rels)) = doc.note_content(kind, id) else {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::Footnotes,
                    format!("missing footnote {id}"),
                ));
            };
            if blocks_contain_anchors(blocks) {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::AnchoredDrawing,
                    "anchored drawing in a footnote or endnote",
                ));
            }
            let marker = Paragraph {
                runs: vec![InlineContent::Run(Run {
                    props: RunProps {
                        vert_align: Some(VertAlign::Super),
                        ..RunProps::default()
                    },
                    content: vec![RunContent::Text(format!("{id} "))],
                })],
                ..Paragraph::default()
            };
            let mut blocks_with_marker = vec![BlockContent::Paragraph(marker)];
            blocks_with_marker.extend_from_slice(blocks);
            note_units.extend(build_units(
                doc,
                warnings,
                &blocks_with_marker,
                rels,
                geo.content_width_pt(),
                &mut counters,
                &mut table_headers,
                None,
            )?);
        }

        let note_top = geo.content_bottom_pt - geo.content_height_pt() * 0.25 + 8.0;
        let mut target = page_index;
        let mut y = note_top;
        add_note_separator(&mut pages[target], geo, false);
        for unit in &note_units {
            let needed = effective_gap_before(unit, y, note_top) + unit.height;
            if y + needed > geo.content_bottom_pt && y > note_top {
                target += 1;
                pages.insert(target, new_page(geo));
                y = note_top;
                add_note_separator(&mut pages[target], geo, true);
            }
            place_unit(&mut pages[target], &mut y, geo, 0.0, unit, usize::MAX);
        }
        page_index = target + 1;
    }
    Ok(())
}

fn add_note_separator(page: &mut WorkPage, geo: &PageGeometry, continuation: bool) {
    let y = geo.content_bottom_pt - geo.content_height_pt() * 0.25;
    page.page.items.push(Positioned {
        x_pt: geo.content_left_pt,
        y_pt: y,
        item: crate::layout::Item::Line {
            dx_pt: if continuation { 72.0 } else { 36.0 },
            dy_pt: 0.0,
            stroke: crate::layout::Stroke {
                width_pt: 0.75,
                color: Rgba::opaque(Rgb::new(0, 0, 0)),
                dash: None,
            },
        },
    });
}

fn append_endnotes(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    geo: &PageGeometry,
    refs: &[(NoteKind, i64)],
    pages: &mut Vec<WorkPage>,
) -> Result<()> {
    let mut counters = ListCounters::default();
    let mut table_headers = HashMap::new();
    let mut units = Vec::new();
    for &(kind, id) in refs {
        let Some((blocks, rels)) = doc.note_content(kind, id) else {
            return Err(OfficeError::unsupported(
                UnsupportedCode::Endnotes,
                format!("missing endnote {id}"),
            ));
        };
        if blocks_contain_anchors(blocks) {
            return Err(OfficeError::unsupported(
                UnsupportedCode::AnchoredDrawing,
                "anchored drawing in a footnote or endnote",
            ));
        }
        let marker = Paragraph {
            runs: vec![InlineContent::Run(Run {
                props: RunProps {
                    vert_align: Some(VertAlign::Super),
                    ..RunProps::default()
                },
                content: vec![RunContent::Text(format!("{id} "))],
            })],
            ..Paragraph::default()
        };
        let mut note_blocks = vec![BlockContent::Paragraph(marker)];
        note_blocks.extend_from_slice(blocks);
        units.extend(build_units(
            doc,
            warnings,
            &note_blocks,
            rels,
            geo.content_width_pt(),
            &mut counters,
            &mut table_headers,
            None,
        )?);
    }
    let mut page = new_page(geo);
    let mut y = geo.content_top_pt;
    for unit in &units {
        if y + effective_gap_before(unit, y, geo.content_top_pt) + unit.height
            > geo.content_bottom_pt
            && !work_page_is_empty(&page)
        {
            pages.push(std::mem::replace(&mut page, new_page(geo)));
            y = geo.content_top_pt;
        }
        place_unit(&mut page, &mut y, geo, 0.0, unit, usize::MAX);
    }
    pages.push(page);
    Ok(())
}

// --- Headers / footers ------------------------------------------------------

fn render_headers_footers(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    sect: &SectPr,
    geo: &PageGeometry,
    pages: &mut [WorkPage],
    total: u32,
    page_offset: u32,
) -> Result<()> {
    let content_w = geo.content_width_pt();
    for (idx, wp) in pages.iter_mut().enumerate() {
        let body_item_count = wp.page.items.len();
        let page_no = sect.page_num_start.unwrap_or(page_offset + 1) + idx as u32;
        let page_label = format_number(&NumFmt::parse(&sect.page_num_fmt), i64::from(page_no))?;
        let ctx = FieldContext {
            page: page_no,
            total,
            page_label,
        };

        // Header band (top).
        if let Some(rel_id) = select_hf(
            &sect.header_refs,
            sect,
            idx,
            doc.settings.even_and_odd_headers,
        ) {
            place_hf(
                doc,
                warnings,
                rel_id,
                ctx.clone(),
                content_w,
                geo,
                geo.header_top_pt,
                wp,
            )?;
        }
        // Footer band (bottom).
        if let Some(rel_id) = select_hf(
            &sect.footer_refs,
            sect,
            idx,
            doc.settings.even_and_odd_headers,
        ) {
            place_hf(
                doc,
                warnings,
                rel_id,
                ctx,
                content_w,
                geo,
                geo.footer_top_pt,
                wp,
            )?;
        }
        let header_footer_items = wp.page.items.split_off(body_item_count);
        wp.page.items.splice(0..0, header_footer_items);
        place_page_borders(sect, geo, wp);
    }
    Ok(())
}

fn place_page_borders(sect: &SectPr, geo: &PageGeometry, page: &mut WorkPage) {
    let edges = [
        (
            sect.page_borders.top.as_ref(),
            geo.content_left_pt,
            geo.content_top_pt,
            geo.content_width_pt(),
            0.0,
        ),
        (
            sect.page_borders.bottom.as_ref(),
            geo.content_left_pt,
            geo.content_bottom_pt,
            geo.content_width_pt(),
            0.0,
        ),
        (
            sect.page_borders.left.as_ref(),
            geo.content_left_pt,
            geo.content_top_pt,
            0.0,
            geo.content_height_pt(),
        ),
        (
            sect.page_borders.right.as_ref(),
            geo.content_right_pt,
            geo.content_top_pt,
            0.0,
            geo.content_height_pt(),
        ),
    ];
    for (border, x, y, dx, dy) in edges {
        if let Some(border) = border {
            page.page.items.push(Positioned {
                x_pt: x,
                y_pt: y,
                item: crate::layout::Item::Line {
                    dx_pt: dx,
                    dy_pt: dy,
                    stroke: crate::layout::Stroke {
                        width_pt: border.size_pt.max(0.25),
                        color: Rgba::opaque(border.color.unwrap_or(Rgb::new(0, 0, 0))),
                        dash: None,
                    },
                },
            });
        }
    }
}

/// Selects the header/footer ref for a page kind (first/even/default).
fn select_hf<'a>(
    refs: &'a [crate::oxml::wml::HdrFtrRef],
    sect: &SectPr,
    page_idx: usize,
    even_and_odd_headers: bool,
) -> Option<&'a str> {
    let is_first = page_idx == 0 && sect.title_pg;
    let is_even = (page_idx + 1).is_multiple_of(2);
    let pick = |kind: HdrFtrKind| {
        refs.iter()
            .find(|r| r.kind == kind)
            .map(|r| r.rel_id.as_str())
    };
    if is_first {
        pick(HdrFtrKind::First).or_else(|| pick(HdrFtrKind::Default))
    } else if is_even && even_and_odd_headers {
        pick(HdrFtrKind::Even).or_else(|| pick(HdrFtrKind::Default))
    } else {
        pick(HdrFtrKind::Default)
    }
}

#[allow(clippy::too_many_arguments)]
fn place_hf(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    rel_id: &str,
    ctx: FieldContext,
    content_w: f64,
    geo: &PageGeometry,
    band_top: f64,
    wp: &mut WorkPage,
) -> Result<()> {
    let Some((blocks, rels)) = doc.header_content(rel_id) else {
        return Ok(());
    };
    let mut placed: Vec<(f64, LineItem)> = Vec::new();
    let mut block_y = 0.0;
    let mut counters = ListCounters::default();
    for block in blocks {
        match block {
            BlockContent::Paragraph(p) => {
                let layout =
                    layout_paragraph(doc, warnings, p, rels, content_w, None, Some(ctx.clone()))?;
                if !layout.page_floats.is_empty()
                    || layout
                        .lines
                        .iter()
                        .any(|line| !line.anchored_items.is_empty())
                {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        "anchored drawing in a header or footer",
                    ));
                }
                block_y += layout.space_before_pt;
                for line in layout.lines {
                    for item in line.items {
                        placed.push((block_y + item.y_pt, item));
                    }
                    block_y += line.height_pt;
                }
                block_y += layout.space_after_pt;
            }
            BlockContent::Table(table) => {
                for row in
                    layout_table(doc, warnings, table, rels, content_w, 0.0, &mut counters)?.rows
                {
                    for item in row.items {
                        placed.push((block_y + item.y_pt, item));
                    }
                    block_y += row.height_pt;
                }
            }
        }
    }
    if band_top <= geo.content_top_pt {
        let overlap = (band_top + block_y - geo.content_top_pt).max(0.0);
        if overlap > 0.0 {
            for positioned in &mut wp.page.items {
                positioned.y_pt += overlap;
            }
            for (_, positioned) in &mut wp.underlays {
                positioned.y_pt += overlap;
            }
            for (_, positioned) in &mut wp.overlays {
                positioned.y_pt += overlap;
            }
            for (_, y) in &mut wp.unit_ys {
                *y += overlap;
            }
        }
    }
    for (y, item) in placed {
        wp.page.items.push(Positioned {
            x_pt: geo.content_left_pt + item.x_pt,
            y_pt: band_top + y,
            item: item.item,
        });
    }
    Ok(())
}

// --- Outline ----------------------------------------------------------------

fn collect_outline(units: &[Unit], pages: &[WorkPage], _geo: &PageGeometry) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();
    for (page_idx, wp) in pages.iter().enumerate() {
        for &(unit_idx, y) in &wp.unit_ys {
            if let Some((title, level)) = &units[unit_idx].outline {
                entries.push(OutlineEntry {
                    title: title.clone(),
                    level: *level,
                    page: page_idx as u32,
                    y_pt: y,
                });
            }
        }
    }
    entries
}

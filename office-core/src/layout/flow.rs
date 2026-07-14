//! Flow: builds flow units from body blocks, paginates them (keep/widow groups,
//! table-header repeat, explicit and section breaks), renders headers/footers
//! per page with page-number field substitution, and collects the outline.
//!
//! Section properties are self-describing: each section run carries the properties
//! that govern its blocks and the transition into that run.

use super::assemble::WordDoc;
use super::numbering_layout::{ListCounters, PlacedLabel, build_label};
use super::page::PageGeometry;
use super::paragraph::{
    AnchoredLineItem, ExclusionBand, FieldContext, LineItem, PageFloat, PageHorizontalContext,
    ParaLayout, WrapExclusion, layout_paragraph, layout_paragraph_with_page_wrap,
};
use super::table::{layout_table, layout_table_with_width};
use super::{EncodedImage, FillRule, LaidOutDocument, OutlineEntry, Page, PathCommand, Positioned};
use crate::oxml::numbering::{NumFmt, format_number};
use crate::oxml::wml::{
    AnchorOrigin, BlockContent, ColumnSpec, FrameAlign, FrameHeightRule, FrameProps, HdrFtrKind,
    HdrFtrRef, InlineContent, NoteKind, PageVAlign, Paragraph, Run, RunContent, RunProps, SectPr,
    SectType, VertAlign, WrapMode,
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
    /// Stable ordinal of this unit within its source body block.
    block_unit_index: usize,
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
    carrier: FloatCarrier,
    /// The one placed line/unit responsible for clearing a full-width band.
    /// Side bands apply independently to every intersecting placed unit.
    clearance_owner: Option<(usize, usize)>,
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

#[derive(Clone, Default)]
struct PageBandSchedule {
    unit_placements: HashMap<(usize, usize), ScheduledUnitPlacement>,
    bands: HashMap<usize, Vec<ResolvedPageBand>>,
    page_bounds: HashMap<usize, (f64, f64)>,
}

#[derive(Debug, Clone, Copy)]
struct ScheduledUnitPlacement {
    page_index: usize,
    top_pt: f64,
    height_pt: f64,
}

struct SectionRun<'a> {
    blocks: &'a [BlockContent],
    sect: &'a SectPr,
    body_block_offset: usize,
    effective_header_refs: &'a [HdrFtrRef],
    effective_footer_refs: &'a [HdrFtrRef],
}

struct RunLayout {
    unit_start: usize,
    unit_end: usize,
    geo: PageGeometry,
    column_widths: Vec<f64>,
    column_offsets: Vec<f64>,
}

struct DocumentPass {
    units: Vec<Unit>,
    runs: Vec<RunLayout>,
    pages: Vec<WorkPage>,
    warnings: Vec<Warning>,
}

#[derive(Default)]
struct UnitBuildState {
    group_counter: usize,
    keep_next_group: Option<usize>,
    prev_after: f64,
    table_counter: usize,
    pending_break: bool,
    pending_column_break: bool,
    active_wrap: Option<WrapExclusion>,
    previous_scheduled_page: Option<usize>,
}

impl UnitBuildState {
    fn reset_for_page_transition(&mut self) {
        self.keep_next_group = None;
        self.prev_after = 0.0;
        self.pending_break = false;
        self.pending_column_break = false;
        self.active_wrap = None;
        self.previous_scheduled_page = None;
    }
}

impl PageBandSchedule {
    fn wrap_for_position(
        &self,
        page_index: usize,
        unit_top: f64,
        owner: (usize, usize),
        content_width: f64,
    ) -> Option<WrapExclusion> {
        let (block_index, block_unit_index) = owner;
        let mut left_bands = Vec::new();
        let mut right_bands = Vec::new();
        for band in self.bands.get(&page_index).into_iter().flatten() {
            if band.bottom_pt <= unit_top + 0.01 {
                continue;
            }
            if band.side == PageBandSide::Full
                && band.clearance_owner != Some((block_index, block_unit_index))
            {
                continue;
            }
            let edge = ExclusionBand {
                source: Some((band.carrier.block_index, band.carrier.float_index)),
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
                PageBandSide::Left | PageBandSide::Full => left_bands.push(edge),
                PageBandSide::Right => right_bands.push(edge),
            }
        }
        (!left_bands.is_empty() || !right_bands.is_empty()).then_some(WrapExclusion {
            left_bands,
            right_bands,
        })
    }

    fn wrap_for_unit(
        &self,
        block_index: usize,
        block_unit_index: usize,
        content_width: f64,
    ) -> Option<WrapExclusion> {
        let placement = self
            .unit_placements
            .get(&(block_index, block_unit_index))
            .copied()?;
        self.wrap_for_position(
            placement.page_index,
            placement.top_pt,
            (block_index, block_unit_index),
            content_width,
        )
    }

    fn line_wraps_for_block(
        &self,
        block_index: usize,
        content_width: f64,
    ) -> Vec<Option<WrapExclusion>> {
        let Some(max_ordinal) = self
            .unit_placements
            .keys()
            .filter_map(|(block, ordinal)| (*block == block_index).then_some(*ordinal))
            .max()
        else {
            return Vec::new();
        };
        let mut wraps = (0..=max_ordinal)
            .map(|ordinal| self.wrap_for_unit(block_index, ordinal, content_width))
            .collect::<Vec<_>>();
        let Some((last_ordinal, mut placement)) = self
            .unit_placements
            .iter()
            .filter_map(|((block, ordinal), placement)| {
                (*block == block_index).then_some((*ordinal, *placement))
            })
            .max_by_key(|(ordinal, _)| *ordinal)
        else {
            return wraps;
        };
        let Some(last_band_page) = self
            .bands
            .iter()
            .filter_map(|(page, bands)| (!bands.is_empty()).then_some(*page))
            .max()
        else {
            return wraps;
        };
        let step = placement.height_pt.max(1.0);
        let mut ordinal = last_ordinal + 1;
        while placement.page_index <= last_band_page {
            let Some((_, content_bottom_pt)) = self.page_bounds.get(&placement.page_index) else {
                break;
            };
            placement.top_pt += step;
            if placement.top_pt + step > *content_bottom_pt + 0.01 {
                placement.page_index += 1;
                let Some((content_top_pt, _)) = self.page_bounds.get(&placement.page_index) else {
                    break;
                };
                placement.top_pt = *content_top_pt;
            }
            if placement.page_index > last_band_page {
                break;
            }
            wraps.push(self.wrap_for_position(
                placement.page_index,
                placement.top_pt,
                (block_index, ordinal),
                content_width,
            ));
            ordinal += 1;
        }
        wraps
    }

    fn first_page_for_block(&self, block_index: usize) -> Option<usize> {
        self.unit_placements
            .get(&(block_index, 0))
            .map(|placement| placement.page_index)
    }
}

fn advance_wrap(wrap: WrapExclusion, distance: f64) -> Option<WrapExclusion> {
    let advance = |bands: Vec<ExclusionBand>| {
        bands
            .into_iter()
            .filter_map(|mut band| {
                band.top_remaining_pt = (band.top_remaining_pt - distance).max(0.0);
                band.remaining_pt -= distance;
                (band.remaining_pt > 0.01).then_some(band)
            })
            .collect::<Vec<_>>()
    };
    let advanced = WrapExclusion {
        left_bands: advance(wrap.left_bands),
        right_bands: advance(wrap.right_bands),
    };
    (!advanced.left_bands.is_empty() || !advanced.right_bands.is_empty()).then_some(advanced)
}

fn combine_wraps(
    active: Option<WrapExclusion>,
    scheduled: Option<WrapExclusion>,
) -> Result<Option<WrapExclusion>> {
    match (active, scheduled) {
        (None, other) | (other, None) => Ok(other),
        (Some(active), Some(scheduled)) => {
            let merge_edge = |mut active: Vec<ExclusionBand>,
                              scheduled: Vec<ExclusionBand>|
             -> Result<Vec<ExclusionBand>> {
                for candidate in scheduled {
                    if candidate.source.is_some()
                        && active.iter().any(|band| band.source == candidate.source)
                    {
                        // `active` has already been advanced using the current
                        // pass's real preceding block geometry. Keep it rather
                        // than replacing it with the previous pass's schedule.
                        continue;
                    }
                    if active.iter().any(|band| {
                        band.top_remaining_pt < candidate.remaining_pt - 0.01
                            && candidate.top_remaining_pt < band.remaining_pt - 0.01
                    }) {
                        return Err(OfficeError::unsupported(
                            UnsupportedCode::AnchoredDrawing,
                            "overlapping anchored drawing exclusions",
                        ));
                    }
                    active.push(candidate);
                }
                active.sort_by(|left, right| {
                    left.top_remaining_pt
                        .partial_cmp(&right.top_remaining_pt)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                Ok(active)
            };
            Ok(Some(WrapExclusion {
                left_bands: merge_edge(active.left_bands, scheduled.left_bands)?,
                right_bands: merge_edge(active.right_bands, scheduled.right_bands)?,
            }))
        }
    }
}

fn wrap_remaining(wrap: WrapExclusion) -> f64 {
    wrap.left_bands
        .into_iter()
        .chain(wrap.right_bands)
        .map(|band| band.remaining_pt)
        .fold(0.0, f64::max)
}

/// Lays out a resolved document into pages plus emitter resources.
pub fn layout_docx(doc: &WordDoc) -> Result<(LaidOutDocument, Vec<Warning>)> {
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
    validate_anchor_section_envelope(&doc.document.body, &boundaries, &doc.document.final_sect)?;
    let section_runs = build_section_runs(doc, &boundaries);
    for pair in section_runs.windows(2) {
        let previous = pair[0].sect;
        let current = pair[1].sect;
        if current.sect_type == SectType::Continuous && geometry_differs(previous, current) {
            return Err(OfficeError::unsupported(
                UnsupportedCode::UnknownElement,
                "continuous section break changes page geometry",
            ));
        }
    }

    let mut pass = run_document_pass(doc, &section_runs, None)?;
    if pass.units.iter().any(|unit| !unit.page_floats.is_empty()) {
        let mut schedule = page_band_schedule(&pass.units, &pass.pages, &pass.runs);
        let mut converged = false;
        for _ in 0..PAGE_FLOAT_REFLOW_LIMIT {
            let next = run_document_pass(doc, &section_runs, Some(&schedule))?;
            let next_schedule = page_band_schedule(&next.units, &next.pages, &next.runs);
            let stable = page_band_schedules_equivalent(&schedule, &next_schedule);
            pass = next;
            if stable {
                converged = true;
                break;
            }
            schedule = next_schedule;
        }
        if !converged {
            return Err(OfficeError::unsupported(
                UnsupportedCode::AnchoredDrawing,
                "page-anchored drawing did not converge",
            ));
        }
    }

    let mut warnings = pass.warnings;
    let units = pass.units;
    let run_layouts = pass.runs;
    let mut pages = pass.pages;
    align_body_vertically(&section_runs, &run_layouts, &units, &mut pages);
    place_footnotes(doc, &mut warnings, &run_layouts, &mut pages)?;

    let mut endnote_refs = Vec::new();
    for unit in &units {
        for note_ref in &unit.note_refs {
            if note_ref.0 == NoteKind::Endnote && !endnote_refs.contains(note_ref) {
                endnote_refs.push(*note_ref);
            }
        }
    }
    if !endnote_refs.is_empty() {
        append_endnotes(doc, &mut warnings, &run_layouts, &endnote_refs, &mut pages)?;
    }

    let total_pages = pages.len() as u32;
    render_headers_footers(
        doc,
        &mut warnings,
        &section_runs,
        &run_layouts,
        &mut pages,
        total_pages,
    )?;
    let outline = collect_outline(&units, &pages);

    let laid = LaidOutDocument {
        pages: pages.into_iter().map(finish_work_page).collect(),
        images: clone_images(&doc.images),
        outline,
        dates: doc.dates.clone(),
    };
    Ok((laid, warnings))
}

fn build_section_runs<'a>(
    doc: &'a WordDoc,
    boundaries: &[(usize, &'a SectPr)],
) -> Vec<SectionRun<'a>> {
    let mut runs = Vec::with_capacity(boundaries.len() + 1);
    let mut start = 0usize;
    let mut effective_header_refs: &[HdrFtrRef] = &[];
    let mut effective_footer_refs: &[HdrFtrRef] = &[];
    for &(block_index, sect) in boundaries {
        if !sect.header_refs.is_empty() {
            effective_header_refs = &sect.header_refs;
        }
        if !sect.footer_refs.is_empty() {
            effective_footer_refs = &sect.footer_refs;
        }
        runs.push(SectionRun {
            blocks: &doc.document.body[start..=block_index],
            sect,
            body_block_offset: start,
            effective_header_refs,
            effective_footer_refs,
        });
        start = block_index + 1;
    }
    let sect = &doc.document.final_sect;
    if !sect.header_refs.is_empty() {
        effective_header_refs = &sect.header_refs;
    }
    if !sect.footer_refs.is_empty() {
        effective_footer_refs = &sect.footer_refs;
    }
    runs.push(SectionRun {
        blocks: &doc.document.body[start..],
        sect,
        body_block_offset: start,
        effective_header_refs,
        effective_footer_refs,
    });
    runs
}

fn align_body_vertically(
    section_runs: &[SectionRun<'_>],
    run_layouts: &[RunLayout],
    units: &[Unit],
    pages: &mut [WorkPage],
) {
    for page in pages {
        let run_index = page.governing_run;
        let alignment = section_runs[run_index].sect.vertical_align;
        if alignment == PageVAlign::Top {
            continue;
        }
        let geo = &run_layouts[run_index].geo;
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

fn validate_anchor_section_envelope(
    blocks: &[BlockContent],
    boundaries: &[(usize, &SectPr)],
    final_section: &SectPr,
) -> Result<()> {
    let mut start = 0usize;
    let mut governing_alignment = None;
    for (run_index, (block_index, section)) in boundaries.iter().enumerate() {
        if run_index == 0 || section.sect_type != SectType::Continuous {
            governing_alignment = Some(section.vertical_align);
        }
        let section_blocks = &blocks[start..=*block_index];
        if section.cols.is_multi() && blocks_contain_anchors(section_blocks) {
            return Err(OfficeError::unsupported(
                UnsupportedCode::AnchoredDrawing,
                "anchored drawing in a multi-column section",
            ));
        }
        if governing_alignment != Some(PageVAlign::Top)
            && blocks_contain_page_floats(section_blocks)
        {
            return Err(OfficeError::unsupported(
                UnsupportedCode::AnchoredDrawing,
                "page-anchored drawing with non-top body alignment",
            ));
        }
        start = *block_index + 1;
    }
    if boundaries.is_empty() || final_section.sect_type != SectType::Continuous {
        governing_alignment = Some(final_section.vertical_align);
    }
    if final_section.cols.is_multi() && blocks_contain_anchors(&blocks[start..]) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::AnchoredDrawing,
            "anchored drawing in a multi-column section",
        ));
    }
    if governing_alignment != Some(PageVAlign::Top) && blocks_contain_page_floats(&blocks[start..])
    {
        return Err(OfficeError::unsupported(
            UnsupportedCode::AnchoredDrawing,
            "page-anchored drawing with non-top body alignment",
        ));
    }
    Ok(())
}

fn clone_images(images: &[EncodedImage]) -> Vec<EncodedImage> {
    images.to_vec()
}

/// A page under construction plus the y where body content ended.
struct WorkPage {
    governing_run: usize,
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

fn run_document_pass(
    doc: &WordDoc,
    section_runs: &[SectionRun<'_>],
    schedule: Option<&PageBandSchedule>,
) -> Result<DocumentPass> {
    let mut warnings = Vec::new();
    let mut counters = ListCounters::default();
    let mut build_state = UnitBuildState::default();
    let mut table_headers = HashMap::new();
    let mut units = Vec::new();
    let mut runs = Vec::with_capacity(section_runs.len());
    for (run_index, run) in section_runs.iter().enumerate() {
        if run_index > 0 && run.sect.sect_type != SectType::Continuous {
            build_state.reset_for_page_transition();
        }
        let geo = PageGeometry::from_section(run.sect);
        let (column_widths, column_offsets) = section_columns(&geo, &run.sect.cols);
        let layout_width = column_widths.iter().copied().fold(f64::INFINITY, f64::min);
        let unit_start = units.len();
        units.extend(build_units(
            doc,
            &mut warnings,
            run.blocks,
            doc.main_rels(),
            layout_width,
            &mut counters,
            &mut table_headers,
            PageHorizontalContext {
                content_left_pt: geo.content_left_pt,
                page_width_pt: geo.width_pt,
            },
            Some(run.body_block_offset),
            schedule,
            &mut build_state,
        )?);
        runs.push(RunLayout {
            unit_start,
            unit_end: units.len(),
            geo,
            column_widths,
            column_offsets,
        });
    }
    let pages = paginate(&units, section_runs, &runs, &table_headers);
    for page in &pages {
        validate_page_bands(
            std::slice::from_ref(page),
            runs[page.governing_run].geo.content_width_pt(),
        )?;
    }
    Ok(DocumentPass {
        units,
        runs,
        pages,
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
    page_horizontal: PageHorizontalContext,
    body_block_offset: Option<usize>,
    page_schedule: Option<&PageBandSchedule>,
    state: &mut UnitBuildState,
) -> Result<Vec<Unit>> {
    let mut units = Vec::new();
    let mut group_counter = state.group_counter;
    let mut next_group = || {
        group_counter += 1;
        group_counter
    };
    let mut keep_next_group = state.keep_next_group;
    let mut prev_after = state.prev_after;
    let mut table_counter = state.table_counter;
    let mut pending_break = state.pending_break;
    let mut pending_column_break = state.pending_column_break;
    let mut active_wrap = state.active_wrap.take();
    let mut previous_scheduled_page = state.previous_scheduled_page;

    for (block_index, block) in blocks.iter().enumerate() {
        let source_block_index =
            body_block_offset.map_or(block_index, |offset| offset + block_index);
        let scheduled_page =
            page_schedule.and_then(|schedule| schedule.first_page_for_block(source_block_index));
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
                } else if let Some(wrap) = active_wrap.take() {
                    let gap =
                        prev_after.max(effective_para.spacing.before_pt.unwrap_or(0.0).max(0.0));
                    active_wrap = advance_wrap(wrap, gap);
                }
                let scheduled_line_wraps = page_schedule
                    .map(|schedule| schedule.line_wraps_for_block(source_block_index, frame_width))
                    .unwrap_or_default();
                let mut layout = layout_paragraph_with_page_wrap(
                    doc,
                    warnings,
                    p,
                    rels,
                    frame_width,
                    label,
                    None,
                    active_wrap.take(),
                    page_horizontal,
                    scheduled_line_wraps,
                )?;
                active_wrap = layout.wrap_after.take();
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
                        source_block_index,
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
                        source_block_index,
                    );
                }
            }
            BlockContent::Table(t) => {
                let scheduled_wrap = page_schedule
                    .and_then(|schedule| schedule.wrap_for_unit(source_block_index, 0, content_w));
                active_wrap = combine_wraps(active_wrap.take(), scheduled_wrap)?;
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
                let table_layout =
                    layout_table_with_width(doc, warnings, t, rels, content_w, 0.0, counters)?;
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
                    let distances = doc.floating_table_distances(Some(source_block_index), t);
                    // Resolve the authored origin into content-relative physical
                    // coordinates before deciding whether the table hugs a text
                    // edge. Page-edge tables therefore cannot masquerade as
                    // content-edge tables merely because their raw offset is 0.
                    let resolved_x = match floating.horizontal {
                        AnchorOrigin::Page => floating.x_pt - page_horizontal.content_left_pt,
                        AnchorOrigin::Margin | AnchorOrigin::Paragraph => floating.x_pt,
                    };
                    let left_hugging = resolved_x.abs() <= 0.01;
                    let right_hugging =
                        (resolved_x + table_layout.width_pt - content_w).abs() <= 0.01;
                    let (side_wrap, text_window) = if left_hugging {
                        (
                            Some(WrapExclusion {
                                left_bands: vec![ExclusionBand {
                                    source: None,
                                    top_remaining_pt: floating.y_pt.max(0.0),
                                    remaining_pt: floating.y_pt + row_y + distances.bottom_pt,
                                    inset_pt: resolved_x
                                        + table_layout.width_pt
                                        + distances.right_pt,
                                    uses_text_baseline: false,
                                }],
                                right_bands: Vec::new(),
                            }),
                            content_w - resolved_x - table_layout.width_pt - distances.right_pt,
                        )
                    } else if right_hugging {
                        (
                            Some(WrapExclusion {
                                left_bands: Vec::new(),
                                right_bands: vec![ExclusionBand {
                                    source: None,
                                    top_remaining_pt: floating.y_pt.max(0.0),
                                    remaining_pt: floating.y_pt + row_y + distances.bottom_pt,
                                    inset_pt: content_w - resolved_x + distances.left_pt,
                                    uses_text_baseline: false,
                                }],
                            }),
                            resolved_x - distances.left_pt,
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
                            floating.y_pt + row_y + floating.wrap_padding_pt
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
                        block_index: source_block_index,
                        block_unit_index: 0,
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
                for (row_index, row) in table_layout.rows.into_iter().enumerate() {
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
                        block_index: source_block_index,
                        block_unit_index: row_index,
                        floating_table: None,
                    };
                    if row.is_header {
                        let mut repeated = clone_unit(&unit);
                        repeated.gap_before = 0.0;
                        repeated.break_before = false;
                        repeated.column_break_before = false;
                        headers.push(repeated);
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
    state.group_counter = group_counter;
    state.keep_next_group = keep_next_group;
    state.prev_after = prev_after;
    state.table_counter = table_counter;
    state.pending_break = pending_break;
    state.pending_column_break = pending_column_break;
    state.active_wrap = active_wrap;
    state.previous_scheduled_page = previous_scheduled_page;
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

fn blocks_contain_page_floats(blocks: &[BlockContent]) -> bool {
    fn run_has_page_float(run: &Run) -> bool {
        run.content.iter().any(|content| {
            matches!(
                content,
                RunContent::Drawing(drawing)
                    if drawing.anchor.is_some_and(|anchor| {
                        anchor.relative_from_v != AnchorOrigin::Paragraph
                    })
            )
        })
    }

    fn inline_has_page_float(inline: &InlineContent) -> bool {
        match inline {
            InlineContent::Run(run) => run_has_page_float(run),
            InlineContent::Hyperlink { runs, .. } => runs.iter().any(run_has_page_float),
            InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                field.content.iter().any(inline_has_page_float)
            }
            InlineContent::Math(_) => false,
        }
    }

    blocks.iter().any(|block| match block {
        BlockContent::Paragraph(paragraph) => paragraph.runs.iter().any(inline_has_page_float),
        BlockContent::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .any(|cell| blocks_contain_page_floats(&cell.content)),
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
        block_unit_index: u.block_unit_index,
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
            block_unit_index: i,
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
        block_unit_index: 0,
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
    section_runs: &[SectionRun<'_>],
    run_layouts: &[RunLayout],
    table_headers: &HashMap<usize, Vec<Unit>>,
) -> Vec<WorkPage> {
    let first_geo = &run_layouts[0].geo;
    let mut pages: Vec<WorkPage> = Vec::new();
    let mut page = new_page(first_geo, 0);
    let mut y = first_geo.content_top_pt;

    for (run_index, (section_run, run_layout)) in section_runs.iter().zip(run_layouts).enumerate() {
        let geo = &run_layout.geo;
        if run_index == 0 {
            y = geo.content_top_pt;
        } else if section_run.sect.sect_type != SectType::Continuous {
            if !work_page_is_empty(&page) {
                pages.push(std::mem::replace(&mut page, new_page(geo, run_index)));
            } else {
                page = new_page(geo, run_index);
            }
            let next_page_number = pages.len() + 1;
            if parity_blank_needed(next_page_number, section_run.sect.sect_type) {
                pages.push(std::mem::replace(&mut page, new_page(geo, run_index)));
            }
            y = geo.content_top_pt;
        }

        let mut column = 0usize;
        let mut band_top = y;
        let mut band_extent = y;
        let mut full_bottom = body_content_bottom(geo, page_has_footnotes(&page));
        let should_balance = run_layout.column_widths.len() > 1
            && (run_index + 1 == section_runs.len()
                || section_runs[run_index + 1].sect.sect_type == SectType::Continuous);
        let (mut content_bottom, mut band_is_balanced) = column_band_bottom(
            units,
            run_layout.unit_start,
            run_layout.unit_end,
            band_top,
            full_bottom,
            run_layout.column_widths.len(),
            should_balance,
            table_headers,
        );
        let mut active_table: Option<usize> = None;
        let mut table_first_page: Option<usize> = None;

        for unit_run in coalesce_runs(units, run_layout.unit_start, run_layout.unit_end) {
            let first_unit_index = unit_run[0];
            let unit_run_has_footnotes = unit_run.iter().any(|index| {
                units[*index]
                    .note_refs
                    .iter()
                    .any(|(kind, _)| *kind == NoteKind::Footnote)
            });
            let candidate_has_footnotes = page_has_footnotes(&page) || unit_run_has_footnotes;
            let candidate_full_bottom = body_content_bottom(geo, candidate_has_footnotes);
            if (candidate_full_bottom - full_bottom).abs() > 0.01 {
                full_bottom = candidate_full_bottom;
                (content_bottom, band_is_balanced) = column_band_bottom(
                    units,
                    run_layout.unit_start,
                    run_layout.unit_end,
                    band_top,
                    full_bottom,
                    run_layout.column_widths.len(),
                    should_balance,
                    table_headers,
                );
            }
            let mut probe_y = y;
            for &unit_index in &unit_run {
                probe_y += effective_gap_before(&units[unit_index], probe_y, band_top);
                probe_y += units[unit_index].height;
            }
            let break_before = units[first_unit_index].break_before;
            let fits = probe_y <= content_bottom + 0.01;
            let mut advanced = false;

            if break_before && (y > band_top + 0.01 || column > 0) {
                pages.push(std::mem::replace(&mut page, new_page(geo, run_index)));
                y = geo.content_top_pt;
                band_top = y;
                band_extent = y;
                column = 0;
                full_bottom = body_content_bottom(geo, unit_run_has_footnotes);
                (content_bottom, band_is_balanced) = column_band_bottom(
                    units,
                    first_unit_index,
                    run_layout.unit_end,
                    band_top,
                    full_bottom,
                    run_layout.column_widths.len(),
                    should_balance,
                    table_headers,
                );
                advanced = true;
            } else if units[first_unit_index].column_break_before || !fits {
                if column + 1 < run_layout.column_widths.len() {
                    column += 1;
                    y = band_top;
                    advanced = true;
                } else if y > band_top + 0.01 || column > 0 {
                    pages.push(std::mem::replace(&mut page, new_page(geo, run_index)));
                    y = geo.content_top_pt;
                    band_top = y;
                    band_extent = y;
                    column = 0;
                    full_bottom = body_content_bottom(geo, unit_run_has_footnotes);
                    (content_bottom, band_is_balanced) = column_band_bottom(
                        units,
                        first_unit_index,
                        run_layout.unit_end,
                        band_top,
                        full_bottom,
                        run_layout.column_widths.len(),
                        should_balance,
                        table_headers,
                    );
                    advanced = true;
                }
            }

            if advanced
                && let Some(table_id) = active_table
                && table_first_page.is_some()
                && let Some(headers) = table_headers.get(&table_id)
            {
                for header in headers {
                    place_unit(
                        &mut page,
                        &mut y,
                        geo,
                        run_layout.column_offsets[column],
                        header,
                        usize::MAX,
                    );
                }
            }

            for &unit_index in &unit_run {
                let unit = &units[unit_index];
                if let Some(table_id) = unit.table_id {
                    if active_table != Some(table_id) {
                        active_table = Some(table_id);
                        table_first_page = Some(pages.len());
                    }
                } else {
                    active_table = None;
                    table_first_page = None;
                }
                place_unit(
                    &mut page,
                    &mut y,
                    geo,
                    run_layout.column_offsets[column],
                    unit,
                    unit_index,
                );
                band_extent = band_extent.max(y);
            }
        }
        y = band_extent;
        if band_is_balanced {
            y = y.max(content_bottom.min(full_bottom));
        }
    }
    let final_run = &run_layouts[run_layouts.len() - 1];
    let empty_final_run_starts_page = section_runs.len() > 1
        && final_run.unit_start == final_run.unit_end
        && section_runs[section_runs.len() - 1].sect.sect_type != SectType::Continuous;
    if !work_page_is_empty(&page) || pages.is_empty() || empty_final_run_starts_page {
        pages.push(page);
    }
    pages
}

fn page_has_footnotes(page: &WorkPage) -> bool {
    page.note_refs
        .iter()
        .any(|(kind, _)| *kind == NoteKind::Footnote)
}

fn body_content_bottom(geo: &PageGeometry, reserves_footnotes: bool) -> f64 {
    if reserves_footnotes {
        geo.content_bottom_pt - geo.content_height_pt() * 0.25
    } else {
        geo.content_bottom_pt
    }
}

fn parity_blank_needed(next_page_number: usize, sect_type: SectType) -> bool {
    match sect_type {
        SectType::OddPage => next_page_number.is_multiple_of(2),
        SectType::EvenPage => !next_page_number.is_multiple_of(2),
        SectType::NextPage | SectType::Continuous => false,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the balancing probe needs the complete run slice and its page-band constraints"
)]
fn column_band_bottom(
    units: &[Unit],
    start: usize,
    end: usize,
    band_top: f64,
    full_bottom: f64,
    column_count: usize,
    should_balance: bool,
    table_headers: &HashMap<usize, Vec<Unit>>,
) -> (f64, bool) {
    let total_height: f64 = units[start..end]
        .iter()
        .map(|unit| unit.gap_before + unit.float_leading + unit.height)
        .sum();
    let available_height = (full_bottom - band_top).max(0.0);
    let balanced_height = balanced_column_height(units, start, end, column_count, table_headers);
    let balanced = should_balance && total_height > 0.0 && balanced_height <= available_height;
    if balanced {
        (band_top + balanced_height.ceil() + 0.5, true)
    } else {
        (full_bottom, false)
    }
}

fn balanced_column_height(
    units: &[Unit],
    start: usize,
    end: usize,
    column_count: usize,
    table_headers: &HashMap<usize, Vec<Unit>>,
) -> f64 {
    let groups = coalesce_runs(units, start, end);
    let group_heights = groups
        .iter()
        .map(|run| {
            run.iter()
                .map(|index| {
                    let unit = &units[*index];
                    unit.gap_before + unit.float_leading + unit.height
                })
                .sum::<f64>()
        })
        .collect::<Vec<_>>();
    let total = group_heights.iter().sum::<f64>();
    let largest = group_heights.iter().copied().fold(0.0, f64::max);
    let mut low = (total / column_count as f64).max(largest);
    let repeat_allowance = table_headers
        .values()
        .map(|headers| repeated_header_height(headers))
        .sum::<f64>()
        * column_count.saturating_sub(1) as f64;
    let mut high = (total + repeat_allowance).max(low);
    for _ in 0..32 {
        let candidate = (low + high) / 2.0;
        let mut used_columns = 1usize;
        let mut column_height = 0.0;
        let mut active_table = None;
        for (group, height) in groups.iter().zip(&group_heights) {
            if column_height > 0.0 && column_height + height > candidate + 0.001 {
                used_columns += 1;
                column_height = 0.0;
                if let Some(table_id) = active_table
                    && units[group[0]].table_id == Some(table_id)
                    && let Some(headers) = table_headers.get(&table_id)
                {
                    column_height += repeated_header_height(headers);
                }
                if column_height + height > candidate + 0.001 {
                    used_columns = column_count + 1;
                    break;
                }
            }
            column_height += height;
            active_table = units[group[0]].table_id;
        }
        if used_columns <= column_count {
            high = candidate;
        } else {
            low = candidate;
        }
    }
    high
}

fn repeated_header_height(headers: &[Unit]) -> f64 {
    headers
        .iter()
        .map(|header| header.float_leading + header.height)
        .sum()
}

fn page_band_schedule(
    units: &[Unit],
    pages: &[WorkPage],
    run_layouts: &[RunLayout],
) -> PageBandSchedule {
    let mut schedule = PageBandSchedule::default();
    for (page_index, page) in pages.iter().enumerate() {
        let geo = &run_layouts[page.governing_run].geo;
        schedule
            .page_bounds
            .insert(page_index, (geo.content_top_pt, geo.content_bottom_pt));
        let mut bands = page.float_bands.clone();
        for band in &mut bands {
            if band.side != PageBandSide::Full {
                continue;
            }
            band.clearance_owner = page.unit_ys.iter().find_map(|(unit_index, unit_top)| {
                let unit = units.get(*unit_index)?;
                (*unit_top < band.bottom_pt - 0.01 && *unit_top + unit.height > band.top_pt + 0.01)
                    .then_some((unit.block_index, unit.block_unit_index))
            });
        }
        schedule.bands.insert(page_index, bands);
        for (unit_index, unit_top) in &page.unit_ys {
            if let Some(unit) = units.get(*unit_index) {
                schedule.unit_placements.insert(
                    (unit.block_index, unit.block_unit_index),
                    ScheduledUnitPlacement {
                        page_index,
                        top_pt: *unit_top,
                        height_pt: unit.height,
                    },
                );
            }
        }
    }
    schedule
}

fn page_band_schedules_equivalent(previous: &PageBandSchedule, next: &PageBandSchedule) -> bool {
    if previous.unit_placements.len() != next.unit_placements.len()
        || previous.bands.len() != next.bands.len()
        || previous.page_bounds.len() != next.page_bounds.len()
    {
        return false;
    }
    let same_bounds = previous.page_bounds.iter().all(|(page, (top, bottom))| {
        next.page_bounds
            .get(page)
            .is_some_and(|(next_top, next_bottom)| {
                (top - next_top).abs() <= 0.01 && (bottom - next_bottom).abs() <= 0.01
            })
    });
    if !same_bounds {
        return false;
    }
    let same_units = previous.unit_placements.iter().all(|(unit, placement)| {
        next.unit_placements
            .get(unit)
            .is_some_and(|next_placement| {
                placement.page_index == next_placement.page_index
                    && (placement.top_pt - next_placement.top_pt).abs() <= 0.01
                    && (placement.height_pt - next_placement.height_pt).abs() <= 0.01
            })
    });
    if !same_units {
        return false;
    }
    previous.bands.iter().all(|(page, bands)| {
        let Some(next_bands) = next.bands.get(page) else {
            return false;
        };
        bands.len() == next_bands.len()
            && bands.iter().zip(next_bands).all(|(left, right)| {
                left.carrier == right.carrier
                    && left.clearance_owner == right.clearance_owner
                    && left.side == right.side
                    && (left.top_pt - right.top_pt).abs() <= 0.01
                    && (left.bottom_pt - right.bottom_pt).abs() <= 0.01
                    && (left.inset_pt - right.inset_pt).abs() <= 0.01
                    && left.uses_text_baseline == right.uses_text_baseline
            })
    })
}

fn validate_page_bands(pages: &[WorkPage], content_width: f64) -> Result<()> {
    for page in pages {
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
fn coalesce_runs(units: &[Unit], start: usize, end: usize) -> Vec<Vec<usize>> {
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for i in start..end {
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
    let carrier = FloatCarrier {
        block_index,
        float_index,
    };
    page.float_carriers.push(carrier);
    let [dist_top, dist_bottom, dist_left, dist_right] = page_float.distances_pt;
    let top_pt = y - dist_top;
    let bottom_pt = y + page_float.h_pt + dist_bottom;
    let expanded_left = x - dist_left;
    let expanded_right = x + page_float.w_pt + dist_right;
    let band = match page_float.wrap {
        WrapMode::None => None,
        WrapMode::TopAndBottom => Some(ResolvedPageBand {
            carrier,
            clearance_owner: None,
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
                carrier,
                clearance_owner: None,
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

fn new_page(geo: &PageGeometry, governing_run: usize) -> WorkPage {
    WorkPage {
        governing_run,
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
        && page.unit_ys.is_empty()
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
    run_layouts: &[RunLayout],
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
        let governing_run = pages[page_index].governing_run;
        let geo = &run_layouts[governing_run].geo;

        let mut note_units = Vec::new();
        let mut counters = ListCounters::default();
        let mut table_headers = HashMap::new();
        let mut build_state = UnitBuildState::default();
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
                PageHorizontalContext {
                    content_left_pt: geo.content_left_pt,
                    page_width_pt: geo.width_pt,
                },
                None,
                None,
                &mut build_state,
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
                pages.insert(target, new_page(geo, governing_run));
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
    run_layouts: &[RunLayout],
    refs: &[(NoteKind, i64)],
    pages: &mut Vec<WorkPage>,
) -> Result<()> {
    let governing_run = run_layouts.len() - 1;
    let geo = &run_layouts[governing_run].geo;
    let mut counters = ListCounters::default();
    let mut table_headers = HashMap::new();
    let mut build_state = UnitBuildState::default();
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
            PageHorizontalContext {
                content_left_pt: geo.content_left_pt,
                page_width_pt: geo.width_pt,
            },
            None,
            None,
            &mut build_state,
        )?);
    }
    let mut page = new_page(geo, governing_run);
    let mut y = geo.content_top_pt;
    for unit in &units {
        if y + effective_gap_before(unit, y, geo.content_top_pt) + unit.height
            > geo.content_bottom_pt
            && !work_page_is_empty(&page)
        {
            pages.push(std::mem::replace(&mut page, new_page(geo, governing_run)));
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
    section_runs: &[SectionRun<'_>],
    run_layouts: &[RunLayout],
    pages: &mut [WorkPage],
    total: u32,
) -> Result<()> {
    let mut page_number = 0u32;
    let mut previous_run = None;
    let mut pages_per_run = vec![0usize; section_runs.len()];
    for (idx, wp) in pages.iter_mut().enumerate() {
        let run_index = wp.governing_run;
        let run = &section_runs[run_index];
        let sect = run.sect;
        let geo = &run_layouts[run_index].geo;
        let content_w = geo.content_width_pt();
        let body_item_count = wp.page.items.len();
        if idx == 0 {
            page_number = sect.page_num_start.unwrap_or(1);
        } else if previous_run != Some(run_index) {
            page_number = sect.page_num_start.unwrap_or(page_number + 1);
        } else {
            page_number += 1;
        }
        previous_run = Some(run_index);
        let section_page_index = pages_per_run[run_index];
        pages_per_run[run_index] += 1;
        let page_label = format_number(&NumFmt::parse(&sect.page_num_fmt), i64::from(page_number))?;
        let ctx = FieldContext {
            page: page_number,
            total,
            page_label,
        };

        // Header band (top).
        if let Some(rel_id) = select_hf(
            run.effective_header_refs,
            sect,
            section_page_index,
            idx + 1,
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
            run.effective_footer_refs,
            sect,
            section_page_index,
            idx + 1,
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
    section_page_idx: usize,
    physical_page_number: usize,
    even_and_odd_headers: bool,
) -> Option<&'a str> {
    let is_first = section_page_idx == 0 && sect.title_pg;
    let is_even = physical_page_number.is_multiple_of(2);
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
                for row in layout_table(doc, warnings, table, rels, content_w, 0.0, &mut counters)?
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

fn collect_outline(units: &[Unit], pages: &[WorkPage]) -> Vec<OutlineEntry> {
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

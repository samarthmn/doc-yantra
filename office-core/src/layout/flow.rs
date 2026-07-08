//! Flow: builds flow units from body blocks, paginates them (keep/widow groups,
//! table-header repeat, explicit and section breaks), renders headers/footers
//! per page with page-number field substitution, and collects the outline.
//!
//! v1 uses the final section's geometry for every page (the common single-section
//! case); mid-document geometry changes are a fidelity-harness refinement.

use super::assemble::WordDoc;
use super::numbering_layout::{ListCounters, PlacedLabel, build_label};
use super::page::PageGeometry;
use super::paragraph::{FieldContext, Line, LineItem, ParaLayout, layout_paragraph};
use super::table::layout_table;
use super::{EncodedImage, LaidOutDocument, OutlineEntry, Page, Positioned};
use crate::oxml::wml::{BlockContent, HdrFtrKind, Paragraph, SectPr};
use crate::profile::Warning;
use std::collections::HashMap;

/// A single flow unit (one paragraph line or one table row).
struct Unit {
    height: f64,
    gap_before: f64,
    items: Vec<LineItem>,
    break_before: bool,
    group: usize,
    table_id: Option<usize>,
    is_header_row: bool,
    outline: Option<(String, u8)>,
}

/// Lays out a resolved document into pages plus emitter resources.
pub fn layout_docx(doc: &WordDoc) -> (LaidOutDocument, Vec<Warning>) {
    let mut warnings = Vec::new();
    let sect = &doc.document.final_sect;
    let geo = PageGeometry::from_section(sect);
    let content_w = geo.content_width_pt();

    let mut counters = ListCounters::default();
    let mut table_headers: HashMap<usize, Vec<Unit>> = HashMap::new();
    let units = build_units(
        doc,
        &mut warnings,
        &doc.document.body,
        doc.main_rels(),
        content_w,
        &mut counters,
        &mut table_headers,
    );

    let mut pages = paginate(&units, &geo, &table_headers);

    // Headers/footers per page, now that the page count is known.
    render_headers_footers(doc, &mut warnings, sect, &geo, &mut pages);

    let outline = collect_outline(&units, &pages, &geo);

    let laid = LaidOutDocument {
        pages: pages.into_iter().map(|p| p.page).collect(),
        images: clone_images(&doc.images),
        outline,
        dates: doc.dates.clone(),
    };
    (laid, warnings)
}

fn clone_images(images: &[EncodedImage]) -> Vec<EncodedImage> {
    images.to_vec()
}

/// A page under construction plus the y where body content ended.
struct WorkPage {
    page: Page,
    /// The set of (unit index) placed, for outline mapping.
    unit_ys: Vec<(usize, f64)>,
}

// --- Unit construction ------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn build_units(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    blocks: &[BlockContent],
    rels: &HashMap<String, String>,
    content_w: f64,
    counters: &mut ListCounters,
    table_headers: &mut HashMap<usize, Vec<Unit>>,
) -> Vec<Unit> {
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

    for block in blocks {
        match block {
            BlockContent::Paragraph(p) => {
                let label = counter_label(doc, warnings, p, counters);
                let layout = layout_paragraph(doc, warnings, p, rels, content_w, label, None);
                push_paragraph_units(
                    layout,
                    &mut units,
                    &mut next_group,
                    &mut keep_next_group,
                    &mut prev_after,
                    &mut pending_break,
                );
            }
            BlockContent::Table(t) => {
                let table_id = table_counter;
                table_counter += 1;
                keep_next_group = None;
                let rows = layout_table(doc, warnings, t, rels, content_w, 0.0, counters);
                let mut headers = Vec::new();
                let mut first_row = true;
                for row in rows {
                    let unit = Unit {
                        height: row.height_pt,
                        gap_before: 0.0,
                        items: row.items,
                        break_before: first_row && std::mem::take(&mut pending_break),
                        group: next_group(),
                        table_id: Some(table_id),
                        is_header_row: row.is_header,
                        outline: None,
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
    units
}

fn clone_unit(u: &Unit) -> Unit {
    Unit {
        height: u.height,
        gap_before: u.gap_before,
        items: u.items.iter().map(clone_line_item).collect(),
        break_before: u.break_before,
        group: u.group,
        table_id: u.table_id,
        is_header_row: u.is_header_row,
        outline: u.outline.clone(),
    }
}

fn clone_line_item(li: &LineItem) -> LineItem {
    LineItem {
        x_pt: li.x_pt,
        y_pt: li.y_pt,
        item: li.item.clone(),
    }
}

fn push_paragraph_units(
    layout: ParaLayout,
    units: &mut Vec<Unit>,
    next_group: &mut impl FnMut() -> usize,
    keep_next_group: &mut Option<usize>,
    prev_after: &mut f64,
    pending_break: &mut bool,
) {
    let l = layout.lines.len();
    let ids = paragraph_group_ids(&layout, l, next_group);
    let gap = (*prev_after).max(layout.space_before_pt);
    let mut brk = *pending_break;

    for (i, line) in layout.lines.into_iter().enumerate() {
        let mut group = ids[i];
        // keep_next merges the first line with the previous paragraph's group.
        if i == 0
            && let Some(g) = keep_next_group.take()
        {
            group = g;
        }
        let outline = (i == 0)
            .then(|| layout.outline_level.map(|lvl| (layout.text.clone(), lvl)))
            .flatten();
        let break_before = brk || (i == 0 && layout.page_break_before);
        brk = line.page_break_after;
        units.push(Unit {
            height: line.height_pt,
            gap_before: if i == 0 { gap } else { 0.0 },
            items: line.items,
            break_before,
            group,
            table_id: None,
            is_header_row: false,
            outline,
        });
    }
    *pending_break = brk;
    *prev_after = layout.space_after_pt;
    if layout.keep_next {
        *keep_next_group = Some(ids[l.saturating_sub(1)]);
    } else {
        *keep_next_group = None;
    }
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
) -> Option<PlacedLabel> {
    let eff = doc.styles.para_props(&p.props);
    let num_ref = eff.numbering?;
    let (text, level) = counters.advance(doc, num_ref)?;
    build_label(doc, warnings, p, &text, &level)
}

// --- Pagination -------------------------------------------------------------

fn paginate(
    units: &[Unit],
    geo: &PageGeometry,
    table_headers: &HashMap<usize, Vec<Unit>>,
) -> Vec<WorkPage> {
    let mut pages: Vec<WorkPage> = Vec::new();
    let mut page = new_page(geo);
    let mut y = geo.content_top_pt;
    let content_bottom = geo.content_bottom_pt;
    let mut active_table: Option<usize> = None;
    let mut table_first_page: Option<usize> = None;

    // Coalesce contiguous same-group units into atomic runs.
    let runs = coalesce_runs(units);

    for run in runs {
        let run_height: f64 = run
            .iter()
            .map(|&i| units[i].gap_before + units[i].height)
            .sum();
        let break_before = units[run[0]].break_before;
        let fits = y + run_height <= content_bottom + 0.01;

        if (break_before || !fits) && y > geo.content_top_pt {
            pages.push(std::mem::replace(&mut page, new_page(geo)));
            y = geo.content_top_pt;
            // Re-emit table headers on a continuation page.
            if let Some(t) = active_table
                && table_first_page.is_some()
                && let Some(headers) = table_headers.get(&t)
            {
                for h in headers {
                    place_unit(&mut page, &mut y, geo, h, usize::MAX);
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
            place_unit(&mut page, &mut y, geo, u, i);
        }
    }
    if !page.page.items.is_empty() || pages.is_empty() {
        pages.push(page);
    }
    pages
}

/// Groups consecutive units sharing a group id into atomic runs.
fn coalesce_runs(units: &[Unit]) -> Vec<Vec<usize>> {
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for i in 0..units.len() {
        if let Some(last) = runs.last_mut()
            && units[*last.last().unwrap()].group == units[i].group
        {
            last.push(i);
        } else {
            runs.push(vec![i]);
        }
    }
    runs
}

fn place_unit(page: &mut WorkPage, y: &mut f64, geo: &PageGeometry, unit: &Unit, index: usize) {
    *y += unit.gap_before;
    let unit_top = *y;
    for li in &unit.items {
        page.page.items.push(Positioned {
            x_pt: geo.content_left_pt + li.x_pt,
            y_pt: unit_top + li.y_pt,
            item: li.item.clone(),
        });
    }
    if index != usize::MAX {
        page.unit_ys.push((index, unit_top));
    }
    *y += unit.height;
}

fn new_page(geo: &PageGeometry) -> WorkPage {
    WorkPage {
        page: Page {
            width_pt: geo.width_pt,
            height_pt: geo.height_pt,
            items: Vec::new(),
        },
        unit_ys: Vec::new(),
    }
}

// --- Headers / footers ------------------------------------------------------

fn render_headers_footers(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    sect: &SectPr,
    geo: &PageGeometry,
    pages: &mut [WorkPage],
) {
    let total = pages.len() as u32;
    let content_w = geo.content_width_pt();
    for (idx, wp) in pages.iter_mut().enumerate() {
        let page_no = sect.page_num_start.unwrap_or(1) + idx as u32;
        let ctx = FieldContext {
            page: page_no,
            total,
        };

        // Header band (top).
        if let Some(rel_id) = select_hf(&sect.header_refs, sect, idx) {
            place_hf(
                doc,
                warnings,
                rel_id,
                ctx,
                content_w,
                geo,
                geo.header_top_pt,
                wp,
            );
        }
        // Footer band (bottom).
        if let Some(rel_id) = select_hf(&sect.footer_refs, sect, idx) {
            place_hf(
                doc,
                warnings,
                rel_id,
                ctx,
                content_w,
                geo,
                geo.footer_top_pt,
                wp,
            );
        }
    }
}

/// Selects the header/footer ref for a page kind (first/even/default).
fn select_hf<'a>(
    refs: &'a [crate::oxml::wml::HdrFtrRef],
    sect: &SectPr,
    page_idx: usize,
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
    } else if is_even {
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
) {
    let Some(blocks) = doc.header_blocks(rel_id) else {
        return;
    };
    // Lay out the header/footer blocks and measure total height.
    let mut lines: Vec<Line> = Vec::new();
    for block in blocks {
        if let BlockContent::Paragraph(p) = block {
            let layout = layout_paragraph(
                doc,
                warnings,
                p,
                doc.main_rels(),
                content_w,
                None,
                Some(ctx),
            );
            lines.extend(layout.lines);
        }
    }
    let mut y = band_top;
    for line in lines {
        for li in line.items {
            wp.page.items.push(Positioned {
                x_pt: geo.content_left_pt + li.x_pt,
                y_pt: y + li.y_pt,
                item: li.item,
            });
        }
        y += line.height_pt;
    }
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

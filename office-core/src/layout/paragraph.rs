//! Paragraph line layout: resolve fonts, shape runs, break into lines, align,
//! and decorate. Produces a [`ParaLayout`] of lines that [`super::flow`] places
//! into pages. Line height uses real OS/2 Windows metrics (the parity lever);
//! super/sub factors and decoration offsets are tuned by the fidelity harness.

use super::assemble::WordDoc;
use super::numbering_layout::{self, PlacedLabel};
use super::{Decor, GlyphRun, ImageId, Item, LinkTarget, PlacedGlyph};
use crate::fonts::metrics::FaceMetrics;
use crate::fonts::{FaceId, matching};
use crate::oxml::wml::{
    BreakKind, Field, InlineContent, Jc, LineSpacing, Paragraph, Run, RunColor, RunContent,
    RunProps, UnderlineKind, VertAlign,
};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::text::shape;
use std::collections::HashMap;
use std::rc::Rc;

/// Field substitution context (page numbering) for headers/footers.
#[derive(Debug, Clone, Copy)]
pub struct FieldContext {
    /// The current 1-based page number.
    pub page: u32,
    /// The total page count.
    pub total: u32,
}

/// Word's default size when unresolved (points).
const DEFAULT_SIZE_PT: f64 = 11.0;
/// Super/subscript size factor.
const SCRIPT_SCALE: f64 = 0.65;
/// Super/subscript baseline rise (fraction of size).
const SCRIPT_RISE: f64 = 0.35;

/// The laid-out lines of one paragraph, plus flow-control flags.
#[derive(Debug, Default)]
pub struct ParaLayout {
    /// Space above the paragraph.
    pub space_before_pt: f64,
    /// Space below the paragraph.
    pub space_after_pt: f64,
    /// Laid-out lines.
    pub lines: Vec<Line>,
    /// Keep with the next paragraph.
    pub keep_next: bool,
    /// Keep all lines together.
    pub keep_lines: bool,
    /// Widow/orphan control (default on).
    pub widow_control: bool,
    /// Force a page break before.
    pub page_break_before: bool,
    /// Outline level (0–2) for bookmarks.
    pub outline_level: Option<u8>,
    /// Style id (to detect contextual-spacing neighbours).
    pub style_id: Option<String>,
    /// Contextual spacing suppresses space between same-style paragraphs.
    pub contextual_spacing: bool,
    /// First-line text (for the outline title).
    pub text: String,
}

/// One laid-out line: items positioned relative to the line's top-left.
#[derive(Debug, Default)]
pub struct Line {
    /// Line box height.
    pub height_pt: f64,
    /// Items (`x` relative to content-left, `y` relative to line-top; glyph `y`
    /// is the baseline).
    pub items: Vec<LineItem>,
    /// An explicit page break (`<w:br w:type="page"/>`) ends this line — the flow
    /// must start the next line on a new page.
    pub page_break_after: bool,
}

/// An item within a [`Line`], positioned relative to the line's top-left.
#[derive(Debug)]
pub struct LineItem {
    /// X relative to the content-left edge.
    pub x_pt: f64,
    /// Y relative to the line top (baseline for glyph runs).
    pub y_pt: f64,
    /// The primitive.
    pub item: Item,
}

/// Shared per-run style. Glyphs from one run share an `Rc` so coalescing is a
/// pointer comparison and hyperlink strings are not cloned per glyph.
#[derive(Debug)]
struct RunStyle {
    face: FaceId,
    size_pt: f64,
    color: Rgba,
    underline: Option<Decor>,
    strike: Option<Decor>,
    rise_pt: f64,
    ascent_pt: f64,
    line_height_pt: f64,
    link: Option<LinkTarget>,
}

/// An atom in the pre-break stream.
enum Atom {
    Glyph {
        gid: u16,
        advance_pt: f64,
        ch: char,
        is_space: bool,
        style: Rc<RunStyle>,
    },
    /// A tab (advances to the next stop; alignment/leaders are v1-simplified).
    Tab,
    Break(BreakKind),
    Image {
        image_id: ImageId,
        w_pt: f64,
        h_pt: f64,
        link: Option<LinkTarget>,
    },
}

impl Atom {
    fn advance(&self) -> f64 {
        match self {
            Atom::Glyph { advance_pt, .. } => *advance_pt,
            Atom::Image { w_pt, .. } => *w_pt,
            _ => 0.0,
        }
    }
    fn is_space(&self) -> bool {
        matches!(self, Atom::Glyph { is_space: true, .. })
    }
}

/// Lays out `para` into lines within `avail_width_pt`.
pub fn layout_paragraph(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    avail_width_pt: f64,
    label_override: Option<PlacedLabel>,
    field_ctx: Option<FieldContext>,
) -> ParaLayout {
    let eff_para = doc.styles.para_props(&para.props);
    // Flow supplies a counter-accurate label; otherwise fall back to a
    // first-item approximation.
    let label =
        label_override.or_else(|| numbering_layout::resolve_label(doc, warnings, &eff_para, para));

    // List levels contribute indentation when the paragraph specifies none.
    let label_left = label.as_ref().and_then(|l| l.left_pt);
    let label_hanging = label.as_ref().and_then(|l| l.hanging_pt);
    let left = eff_para
        .indent
        .left_pt
        .or(label_left)
        .unwrap_or(0.0)
        .max(0.0);
    let right = eff_para.indent.right_pt.unwrap_or(0.0).max(0.0);
    let hanging = eff_para.indent.hanging_pt.or(label_hanging).unwrap_or(0.0);
    let first_line = eff_para.indent.first_line_pt.unwrap_or(0.0);
    let first_offset = (first_line - hanging).max(-left);

    let content_first = (avail_width_pt - left - right - first_offset).max(1.0);
    let content_rest = (avail_width_pt - left - right).max(1.0);
    let atoms = flatten(doc, warnings, para, rels, field_ctx);
    let text: String = atoms
        .iter()
        .filter_map(|a| match a {
            Atom::Glyph { ch, .. } => Some(*ch),
            _ => None,
        })
        .collect();

    let alignment = eff_para.alignment.unwrap_or(Jc::Start);
    let line_spacing = eff_para.spacing.line;
    let default_tab = doc.settings.default_tab_stop_pt;
    let mut tab_stops: Vec<f64> = eff_para.tabs.iter().map(|t| t.pos_pt).collect();
    tab_stops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let broken = break_lines(&atoms, content_first, content_rest, default_tab, &tab_stops);
    let n = broken.len();
    let mut lines = Vec::new();
    for (i, line_atoms) in broken.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i + 1 == n;
        let content_w = if is_first {
            content_first
        } else {
            content_rest
        };
        let base_x = left + if is_first { first_offset.max(0.0) } else { 0.0 };
        let this_label = if is_first { label.as_ref() } else { None };
        lines.push(place_line(
            &atoms,
            line_atoms,
            content_w,
            base_x,
            left,
            hanging,
            alignment,
            line_spacing,
            is_last,
            default_tab,
            &tab_stops,
            this_label,
        ));
    }

    if lines.is_empty() {
        let h = para_mark_height(doc, para);
        lines.push(Line {
            height_pt: apply_line_spacing(line_spacing, h),
            items: Vec::new(),
            page_break_after: false,
        });
    }

    ParaLayout {
        space_before_pt: eff_para.spacing.before_pt.unwrap_or(0.0).max(0.0),
        space_after_pt: eff_para.spacing.after_pt.unwrap_or(0.0).max(0.0),
        lines,
        keep_next: eff_para.keep_next,
        keep_lines: eff_para.keep_lines,
        widow_control: eff_para.widow_control.unwrap_or(true),
        page_break_before: eff_para.page_break_before,
        outline_level: eff_para.outline_lvl.filter(|&l| l <= 2),
        style_id: eff_para.style_id.clone(),
        contextual_spacing: eff_para.contextual_spacing,
        text,
    }
}

/// Applies a [`LineSpacing`] rule to a natural line height.
pub fn apply_line_spacing(spacing: Option<LineSpacing>, natural: f64) -> f64 {
    match spacing {
        Some(LineSpacing::Auto(m)) => m * natural,
        Some(LineSpacing::AtLeast(v)) => v.max(natural),
        Some(LineSpacing::Exact(v)) => v,
        None => natural,
    }
}

/// Resolves a run's font to a face id, warning as needed.
fn resolve_face(doc: &WordDoc, warnings: &mut Vec<Warning>, props: &RunProps) -> Option<FaceId> {
    let family = props
        .fonts
        .ascii
        .clone()
        .or_else(|| props.fonts.h_ansi.clone())
        .or_else(|| doc.theme.minor_latin.clone())
        .unwrap_or_else(|| "Calibri".to_string());
    let weight = if props.bold == Some(true) { 700 } else { 400 };
    let italic = props.italic == Some(true);
    let resolved = matching::resolve(&doc.fonts, &family, weight, italic);
    for w in resolved.warnings {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    resolved.face
}

fn flatten(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    field_ctx: Option<FieldContext>,
) -> Vec<Atom> {
    let mut atoms = Vec::new();
    for inline in &para.runs {
        match inline {
            InlineContent::Run(r) => flatten_run(doc, warnings, para, r, None, rels, &mut atoms),
            InlineContent::Hyperlink {
                rel_id,
                anchor,
                runs,
            } => {
                let target = link_target(doc, rels, rel_id.as_deref(), anchor.as_deref());
                for r in runs {
                    flatten_run(doc, warnings, para, r, target.clone(), rels, &mut atoms);
                }
            }
            InlineContent::SimpleField(f) | InlineContent::ComplexFieldRegion(f) => {
                for r in &field_runs(f, field_ctx) {
                    flatten_run(doc, warnings, para, r, None, rels, &mut atoms);
                }
            }
        }
    }
    atoms
}

/// Resolves a field to the runs to lay out: a substituted PAGE/NUMPAGES number
/// (when a field context is present), else the cached result runs.
fn field_runs(field: &Field, ctx: Option<FieldContext>) -> Vec<Run> {
    let first = field
        .instruction
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if let Some(ctx) = ctx {
        let num = match first.as_str() {
            "PAGE" => Some(ctx.page.to_string()),
            "NUMPAGES" => Some(ctx.total.to_string()),
            _ => None,
        };
        if let Some(num) = num {
            let props = field
                .cached_runs
                .first()
                .map(|r| r.props.clone())
                .unwrap_or_default();
            return vec![Run {
                props,
                content: vec![RunContent::Text(num)],
            }];
        }
    }
    field.cached_runs.clone()
}

fn link_target(
    doc: &WordDoc,
    rels: &HashMap<String, String>,
    rel_id: Option<&str>,
    anchor: Option<&str>,
) -> Option<LinkTarget> {
    match rel_id {
        Some(rid) => doc.hyperlink_url(rels, rid).map(LinkTarget::Url),
        None => anchor.map(|_| LinkTarget::Page(0)),
    }
}

fn flatten_run(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    run: &Run,
    link: Option<LinkTarget>,
    rels: &HashMap<String, String>,
    atoms: &mut Vec<Atom>,
) {
    let eff = doc.styles.run_props(&para.props, &run.props);
    if eff.vanish == Some(true) {
        return;
    }
    let Some(face) = resolve_face(doc, warnings, &eff) else {
        return;
    };
    let Some(parsed) = doc.fonts.face(face).and_then(|f| f.parsed()) else {
        return;
    };
    let metrics = FaceMetrics::new(parsed);

    let base_size = eff.size_pt.unwrap_or(DEFAULT_SIZE_PT);
    let (size, rise) = match eff.vert_align {
        Some(VertAlign::Super) => (base_size * SCRIPT_SCALE, SCRIPT_RISE * base_size),
        Some(VertAlign::Sub) => (base_size * SCRIPT_SCALE, -SCRIPT_RISE * base_size),
        _ => (base_size, 0.0),
    };
    let (u_pos, u_thick) = metrics.underline_pt(size);
    let style = Rc::new(RunStyle {
        face,
        size_pt: size,
        color: resolve_color(&eff),
        underline: matches!(eff.underline, Some(u) if u != UnderlineKind::None).then(|| Decor {
            thickness_pt: u_thick.abs().max(0.4),
            y_offset_pt: (-u_pos).max(0.5),
        }),
        strike: (eff.strike == Some(true) || eff.dstrike == Some(true)).then(|| Decor {
            thickness_pt: u_thick.abs().max(0.4),
            y_offset_pt: -metrics.ascent_pt(size) * 0.32,
        }),
        rise_pt: rise,
        ascent_pt: metrics.ascent_pt(size),
        line_height_pt: metrics.line_height_pt(size),
        link,
    });

    let char_spacing = eff.spacing_pt.unwrap_or(0.0);
    for content in &run.content {
        match content {
            RunContent::Text(t) => shape_text(&metrics, t, size, char_spacing, &style, atoms),
            RunContent::Tab => atoms.push(Atom::Tab),
            RunContent::Break(k) => atoms.push(Atom::Break(*k)),
            RunContent::Symbol { font, char_code } => {
                if let Some(ch) = matching::map_symbol_char(font, *char_code) {
                    shape_text(&metrics, &ch.to_string(), size, char_spacing, &style, atoms);
                }
            }
            RunContent::Drawing(d) => {
                if let Some((w, h)) = d.extent_pt
                    && let Some(rel_id) = &d.blip_rel_id
                    && let Some(image_id) = doc.image_id(rels, rel_id)
                {
                    atoms.push(Atom::Image {
                        image_id,
                        w_pt: w,
                        h_pt: h,
                        link: style.link.clone(),
                    });
                }
            }
        }
    }
}

fn shape_text(
    metrics: &FaceMetrics,
    text: &str,
    size: f64,
    char_spacing: f64,
    style: &Rc<RunStyle>,
    atoms: &mut Vec<Atom>,
) {
    let shaped = shape::shape(text, metrics, size, char_spacing);
    let chars: HashMap<usize, char> = text.char_indices().collect();
    for g in &shaped.glyphs {
        let ch = chars.get(&g.cluster).copied().unwrap_or(' ');
        atoms.push(Atom::Glyph {
            gid: g.id.0,
            advance_pt: g.advance_pt,
            ch,
            is_space: ch == ' ',
            style: Rc::clone(style),
        });
    }
}

fn resolve_color(props: &RunProps) -> Rgba {
    match props.color {
        Some(RunColor::Rgb(c)) => Rgba::opaque(c),
        _ => Rgba::opaque(Rgb::new(0, 0, 0)),
    }
}

/// Greedy line breaking on spaces and mandatory breaks; emergency-breaks a word
/// longer than the line at glyph granularity.
fn break_lines(
    atoms: &[Atom],
    width_first: f64,
    width_rest: f64,
    default_tab: f64,
    tab_stops: &[f64],
) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut width_used = 0.0_f64;
    let mut last_break: Option<usize> = None;

    let width_of = |lines: &[Vec<usize>]| {
        if lines.is_empty() {
            width_first
        } else {
            width_rest
        }
    };

    let mut i = 0;
    while i < atoms.len() {
        match &atoms[i] {
            Atom::Break(_) => {
                current.push(i);
                lines.push(std::mem::take(&mut current));
                width_used = 0.0;
                last_break = None;
                i += 1;
                continue;
            }
            Atom::Tab => {
                width_used = next_tab_stop(width_used, default_tab, tab_stops);
                current.push(i);
                last_break = Some(current.len());
                i += 1;
                continue;
            }
            _ => {}
        }

        let adv = atoms[i].advance();
        let avail = width_of(&lines);
        let is_space = atoms[i].is_space();

        if width_used + adv > avail && !current.is_empty() && !is_space {
            if let Some(bp) = last_break.filter(|&bp| bp < current.len()) {
                let rest = current.split_off(bp);
                lines.push(std::mem::take(&mut current));
                width_used = rest.iter().map(|&k| atoms[k].advance()).sum();
                current = rest;
                last_break = None;
                continue;
            }
            lines.push(std::mem::take(&mut current));
            width_used = 0.0;
            last_break = None;
            continue;
        }

        current.push(i);
        width_used += adv;
        if is_space {
            last_break = Some(current.len());
        }
        i += 1;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The next tab stop position ≥ `x` (explicit stops first, else the default grid).
fn next_tab_stop(x: f64, default_tab: f64, tab_stops: &[f64]) -> f64 {
    for &pos in tab_stops {
        if pos > x + 0.01 {
            return pos;
        }
    }
    let n = (x / default_tab).floor() + 1.0;
    n * default_tab
}

#[allow(clippy::too_many_arguments)]
fn place_line(
    atoms: &[Atom],
    indices: &[usize],
    content_w: f64,
    base_x: f64,
    left: f64,
    hanging: f64,
    alignment: Jc,
    line_spacing: Option<LineSpacing>,
    is_last: bool,
    default_tab: f64,
    tab_stops: &[f64],
    label: Option<&PlacedLabel>,
) -> Line {
    // Natural width and expandable space count.
    let mut natural = 0.0_f64;
    let mut spaces = 0usize;
    for &k in indices {
        match &atoms[k] {
            Atom::Tab => natural = next_tab_stop(natural, default_tab, tab_stops),
            Atom::Glyph {
                advance_pt,
                is_space,
                ..
            } => {
                natural += advance_pt;
                if *is_space {
                    spaces += 1;
                }
            }
            Atom::Image { w_pt, .. } => natural += w_pt,
            Atom::Break(_) => {}
        }
    }
    let trailing: f64 = indices
        .iter()
        .rev()
        .map(|&k| &atoms[k])
        .take_while(|a| a.is_space())
        .map(|a| a.advance())
        .sum();
    let effective = natural - trailing;

    let (start_shift, space_extra) = match alignment {
        Jc::Center => (((content_w - effective) / 2.0).max(0.0), 0.0),
        Jc::End => ((content_w - effective).max(0.0), 0.0),
        Jc::Both | Jc::Distribute if !is_last && spaces > 0 && effective < content_w => {
            (0.0, (content_w - effective) / spaces as f64)
        }
        _ => (0.0, 0.0),
    };

    // Line box: max height/ascent over glyphs + label, with line spacing applied.
    let mut natural_h = 0.0_f64;
    let mut ascent = 0.0_f64;
    if let Some(l) = label {
        natural_h = natural_h.max(l.line_height_pt);
        ascent = ascent.max(l.ascent_pt);
    }
    for &k in indices {
        match &atoms[k] {
            Atom::Glyph { style, .. } => {
                natural_h = natural_h.max(style.line_height_pt);
                ascent = ascent.max(style.ascent_pt);
            }
            Atom::Image { h_pt, .. } => {
                natural_h = natural_h.max(*h_pt);
                ascent = ascent.max(*h_pt);
            }
            _ => {}
        }
    }
    if natural_h <= 0.0 {
        natural_h = DEFAULT_SIZE_PT * 1.2;
        ascent = DEFAULT_SIZE_PT;
    }
    let height = apply_line_spacing(line_spacing, natural_h);

    let mut items: Vec<LineItem> = Vec::new();

    // Numbering label at (left - hanging).
    if let Some(l) = label {
        items.push(LineItem {
            x_pt: (left - hanging).max(0.0),
            y_pt: ascent,
            item: Item::Glyphs(l.run.clone()),
        });
    }

    let mut x = base_x + start_shift;
    let mut pending: Vec<PlacedGlyph> = Vec::new();
    let mut pending_style: Option<Rc<RunStyle>> = None;
    let mut pending_x = x;
    let mut link_start: Option<(f64, LinkTarget)> = None;

    let flush = |pending: &mut Vec<PlacedGlyph>,
                 pending_style: &mut Option<Rc<RunStyle>>,
                 pending_x: f64,
                 items: &mut Vec<LineItem>| {
        if let Some(st) = pending_style.take()
            && !pending.is_empty()
        {
            items.push(LineItem {
                x_pt: pending_x,
                y_pt: ascent - st.rise_pt,
                item: Item::Glyphs(build_run(&st, std::mem::take(pending))),
            });
        }
    };

    for &k in indices {
        match &atoms[k] {
            Atom::Glyph {
                gid,
                advance_pt,
                ch,
                is_space,
                style,
            } => {
                let same = pending_style
                    .as_ref()
                    .map(|s| Rc::ptr_eq(s, style))
                    .unwrap_or(false);
                if !same {
                    flush(&mut pending, &mut pending_style, pending_x, &mut items);
                    pending_style = Some(Rc::clone(style));
                    pending_x = x;
                }
                update_link(&mut link_start, style.link.as_ref(), x, height, &mut items);
                let adv = advance_pt + if *is_space { space_extra } else { 0.0 };
                pending.push(PlacedGlyph {
                    gid: *gid,
                    advance_pt: adv,
                    ch: *ch,
                });
                x += adv;
            }
            Atom::Tab => {
                flush(&mut pending, &mut pending_style, pending_x, &mut items);
                x = base_x + next_tab_stop(x - base_x, default_tab, tab_stops);
            }
            Atom::Image {
                image_id,
                w_pt,
                h_pt,
                link,
            } => {
                flush(&mut pending, &mut pending_style, pending_x, &mut items);
                update_link(&mut link_start, link.as_ref(), x, height, &mut items);
                items.push(LineItem {
                    x_pt: x,
                    y_pt: (ascent - h_pt).max(0.0),
                    item: Item::Image {
                        w_pt: *w_pt,
                        h_pt: *h_pt,
                        image_id: *image_id,
                    },
                });
                x += w_pt;
            }
            Atom::Break(_) => {}
        }
    }
    flush(&mut pending, &mut pending_style, pending_x, &mut items);
    update_link(&mut link_start, None, x, height, &mut items);

    let page_break_after = indices
        .iter()
        .any(|&k| matches!(atoms[k], Atom::Break(BreakKind::Page)));
    Line {
        height_pt: height,
        items,
        page_break_after,
    }
}

/// Opens/closes link rectangles as the current link target changes.
fn update_link(
    link_start: &mut Option<(f64, LinkTarget)>,
    now: Option<&LinkTarget>,
    x: f64,
    height: f64,
    items: &mut Vec<LineItem>,
) {
    let changed = link_key(link_start.as_ref().map(|(_, t)| t)) != link_key(now);
    if !changed {
        return;
    }
    if let Some((sx, target)) = link_start.take()
        && x > sx
    {
        items.push(LineItem {
            x_pt: sx,
            y_pt: 0.0,
            item: Item::Link {
                w_pt: x - sx,
                h_pt: height,
                target,
            },
        });
    }
    if let Some(t) = now {
        *link_start = Some((x, t.clone()));
    }
}

fn link_key(l: Option<&LinkTarget>) -> String {
    match l {
        None => String::new(),
        Some(LinkTarget::Url(u)) => format!("u:{u}"),
        Some(LinkTarget::Page(p)) => format!("p:{p}"),
    }
}

fn build_run(style: &RunStyle, glyphs: Vec<PlacedGlyph>) -> GlyphRun {
    GlyphRun {
        face: style.face,
        size_pt: style.size_pt,
        color: style.color,
        glyphs,
        underline: style.underline,
        strike: style.strike,
    }
}

fn para_mark_height(doc: &WordDoc, para: &Paragraph) -> f64 {
    let eff = doc.styles.run_props(&para.props, &RunProps::default());
    let size = eff.size_pt.unwrap_or(DEFAULT_SIZE_PT);
    // Use the resolved face's metrics when available.
    let family = eff
        .fonts
        .ascii
        .clone()
        .or_else(|| doc.theme.minor_latin.clone())
        .unwrap_or_else(|| "Calibri".to_string());
    if let Some(face) = matching::resolve(&doc.fonts, &family, 400, false).face
        && let Some(parsed) = doc.fonts.face(face).and_then(|f| f.parsed())
    {
        return FaceMetrics::new(parsed).line_height_pt(size);
    }
    size * 1.2
}

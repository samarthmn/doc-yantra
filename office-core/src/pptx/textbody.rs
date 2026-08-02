//! PPTX text-body layout: lays a [`TextBody`] into positioned glyph runs within
//! a box, with bullets, alignment, and master/level default sizes.

use super::assemble::{Category, TextDefaults};
use super::geometry::resolve_colorspec;
use crate::Result;
use crate::fonts::metrics::FaceMetrics;
use crate::fonts::{FaceId, FontDb, matching};
use crate::layout::paragraph::LineItem;
use crate::layout::{Decor, GlyphRun, Item, LinkTarget, PlacedGlyph, SemanticCluster, SemanticRun};
use crate::oxml::pml::{TextBody, TextParagraph, TextRun, TextSpacing};
use crate::oxml::theme::Theme;
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::text::shape;
use std::collections::{BTreeMap, HashMap};

/// PowerPoint's normalized-autofit DrawingML line box is 1.20 times the
/// largest font size on the physical line. The frozen PowerPoint-web truths
/// inherit `<a:normAutofit/>` and measure 24.05pt at 20pt. Ordinary text keeps
/// the face's natural line box (Carlito's Win box is 1.220703em).
const NORMALIZED_LINE_HEIGHT_EM: f64 = 1.2;

/// The laid-out text of a body: items (relative to the content box top-left) and
/// total height.
pub struct BodyLayout {
    /// Positioned items.
    pub items: Vec<LineItem>,
    /// Total text height.
    pub height_pt: f64,
}

/// Lays out `body` within `content_w`, using master defaults for `category`.
#[allow(clippy::too_many_arguments)]
pub fn layout_textbody(
    fonts: &FontDb,
    theme: &Theme,
    defaults: &TextDefaults,
    warnings: &mut Vec<Warning>,
    body: &TextBody,
    category: Category,
    content_w: f64,
    link_resolver: impl Fn(&str) -> Option<LinkTarget>,
) -> Result<BodyLayout> {
    let scale = body.font_scale.unwrap_or(1.0);
    let mut items = Vec::new();
    let mut y = 0.0_f64;
    let mut numbering = [0u32; 9];

    for (paragraph_index, para) in body.paragraphs.iter().enumerate() {
        let has_following_paragraph = paragraph_index < body.paragraphs.len().saturating_sub(1);
        let bullet = bullet_prefix(para, &mut numbering);
        let align = para
            .align
            .as_deref()
            .unwrap_or(if para.rtl { "r" } else { "l" });
        let (mar_l, indent) = resolve_indent_geometry(para, body, defaults, category);
        let (space_before, space_after) = resolve_paragraph_spacing(para, body, defaults, category);

        let (mut atoms, bullet_len) = shape_paragraph(
            fonts,
            theme,
            defaults,
            warnings,
            para,
            category,
            scale,
            bullet.as_deref(),
            &link_resolver,
        )?;
        if body.norm_autofit == Some(true) {
            for atom in &mut atoms {
                atom.line_height_pt = normalized_line_height(atom.size_pt);
            }
        }
        let shaped_line_height = atoms
            .iter()
            .map(|atom| atom.line_height_pt)
            .fold(0.0, f64::max);
        let paragraph_line_height = if shaped_line_height > 0.0 {
            shaped_line_height
        } else {
            normalized_line_height(defaults.size(category, para.level) * scale)
        };
        if paragraph_index > 0 {
            y += resolve_spacing(space_before, paragraph_line_height);
        }
        if atoms.is_empty() {
            // Empty paragraph still advances by a default line.
            y += paragraph_line_height;
            if has_following_paragraph {
                y += resolve_spacing(space_after, paragraph_line_height);
            }
            continue;
        }
        // The bullet glyph draws at marL+indent (negative indent = hanging);
        // first-line text starts at marL, pushed right only when the bullet's
        // advance overflows the hang zone. Continuation lines start at marL.
        let bullet_w: f64 = atoms[..bullet_len].iter().map(|a| a.advance_pt).sum();
        let bullet_x = (mar_l + indent).max(0.0);
        let (first_x, rest_x) = if bullet_len > 0 {
            (f64::max(mar_l, bullet_x + bullet_w), mar_l)
        } else {
            ((mar_l + indent).max(0.0), mar_l)
        };
        let avail_first = (content_w - first_x).max(1.0);
        let avail_rest = (content_w - rest_x).max(1.0);
        let bullet_indices: Vec<usize> = (0..bullet_len).collect();

        let mut lines = break_lines(&atoms, bullet_len, avail_first, avail_rest);
        if lines.is_empty() && bullet_len > 0 {
            // A bulleted paragraph with no text still renders its bullet.
            lines.push(Vec::new());
        }
        for (index, line) in lines.iter().enumerate() {
            let (base_x, line_avail) = if index == 0 {
                (first_x, avail_first)
            } else {
                (rest_x, avail_rest)
            };
            let mut segments: Vec<(f64, &[usize])> = Vec::new();
            if index == 0 && bullet_len > 0 {
                segments.push((bullet_x, &bullet_indices));
            }
            segments.push((base_x, line));
            let (line_items, line_h) = place(&atoms, &segments, line_avail, align, y, para.rtl);
            items.extend(line_items);
            y += line_h;
        }
        if has_following_paragraph {
            y += resolve_spacing(space_after, paragraph_line_height);
        }
    }
    Ok(BodyLayout {
        items,
        height_pt: y,
    })
}

fn normalized_line_height(size_pt: f64) -> f64 {
    size_pt * NORMALIZED_LINE_HEIGHT_EM
}

fn resolve_spacing(spacing: Option<TextSpacing>, line_height_pt: f64) -> f64 {
    match spacing {
        Some(TextSpacing::Percent(fraction)) => line_height_pt * fraction,
        Some(TextSpacing::Points(points)) => points,
        None => 0.0,
    }
}

fn resolve_paragraph_spacing(
    para: &TextParagraph,
    body: &TextBody,
    defaults: &TextDefaults,
    category: Category,
) -> (Option<TextSpacing>, Option<TextSpacing>) {
    let level = para.level as usize;
    let own_style = body.list_style.get(level).and_then(Option::as_ref);
    let txstyles_style = defaults.level_style(category, level);
    let before = para
        .space_before
        .or_else(|| own_style.and_then(|style| style.space_before))
        .or_else(|| txstyles_style.and_then(|style| style.space_before));
    let after = para
        .space_after
        .or_else(|| own_style.and_then(|style| style.space_after))
        .or_else(|| txstyles_style.and_then(|style| style.space_after));
    (before, after)
}

/// Resolves a paragraph's `(marL, indent)` in points. Placeholder paragraphs
/// arrive with the own→layout→master→txStyles chain already folded in by
/// `inherit::resolve_text_body`; non-placeholder shapes and table cells skip
/// that pass, so the body's own `a:lstStyle` and the master `p:txStyles`
/// bucket are consulted here as fallbacks. Only when the entire chain is
/// silent does the legacy 18pt-per-level step apply (keeps masters without
/// indent geometry stable).
fn resolve_indent_geometry(
    para: &TextParagraph,
    body: &TextBody,
    defaults: &TextDefaults,
    category: Category,
) -> (f64, f64) {
    let level = para.level as usize;
    let own_style = body.list_style.get(level).and_then(Option::as_ref);
    let txstyles_style = defaults.level_style(category, level);
    let mar_l = para
        .mar_l_pt
        .or_else(|| own_style.and_then(|style| style.mar_l_pt))
        .or_else(|| txstyles_style.and_then(|style| style.mar_l_pt));
    let indent = para
        .indent_pt
        .or_else(|| own_style.and_then(|style| style.indent_pt))
        .or_else(|| txstyles_style.and_then(|style| style.indent_pt));
    match (mar_l, indent) {
        (None, None) => (para.level as f64 * 18.0, 0.0),
        (mar_l, indent) => (mar_l.unwrap_or(0.0).max(0.0), indent.unwrap_or(0.0)),
    }
}

/// A shaped glyph atom.
#[derive(Clone)]
struct AtomSemanticCluster {
    group_id: usize,
    source_order: usize,
    face: FaceId,
    size_pt: f64,
    base_direction: text_core::Direction,
    cluster: SemanticCluster,
}

struct Atom {
    gid: u16,
    advance_pt: f64,
    x_offset_pt: f64,
    y_offset_pt: f64,
    ch: char,
    is_space: bool,
    face: FaceId,
    size_pt: f64,
    color: Rgba,
    underline: bool,
    /// Requested-bold run on a face with no bold style (weight < 600).
    synthetic_bold: bool,
    ascent_pt: f64,
    line_height_pt: f64,
    link: Option<LinkTarget>,
    semantic: Vec<AtomSemanticCluster>,
}

#[allow(clippy::too_many_arguments)]
fn shape_paragraph(
    fonts: &FontDb,
    theme: &Theme,
    defaults: &TextDefaults,
    warnings: &mut Vec<Warning>,
    para: &TextParagraph,
    category: Category,
    scale: f64,
    bullet: Option<&str>,
    link_resolver: &dyn Fn(&str) -> Option<LinkTarget>,
) -> Result<(Vec<Atom>, usize)> {
    let mut atoms = Vec::new();
    // Bullet as a leading run using the first run's style (or defaults). It is
    // positioned independently of the text (at marL+indent), so it is shaped
    // bare — no inlined separator.
    if let Some(b) = bullet {
        let synthetic = TextRun {
            text: b.to_string(),
            ..para.runs.first().cloned().unwrap_or_default()
        };
        shape_run(
            fonts,
            theme,
            defaults,
            warnings,
            &synthetic,
            para,
            category,
            scale,
            link_resolver,
            &mut atoms,
        )?;
    }
    let bullet_len = atoms.len();
    for run in &para.runs {
        shape_run(
            fonts,
            theme,
            defaults,
            warnings,
            run,
            para,
            category,
            scale,
            link_resolver,
            &mut atoms,
        )?;
    }
    Ok((atoms, bullet_len))
}

#[allow(clippy::too_many_arguments)]
fn shape_run(
    fonts: &FontDb,
    theme: &Theme,
    defaults: &TextDefaults,
    warnings: &mut Vec<Warning>,
    run: &TextRun,
    para: &TextParagraph,
    category: Category,
    scale: f64,
    link_resolver: &dyn Fn(&str) -> Option<LinkTarget>,
    atoms: &mut Vec<Atom>,
) -> Result<()> {
    let size = run
        .size_pt
        .unwrap_or_else(|| defaults.size(category, para.level))
        * scale;
    let family = match run.typeface.as_deref() {
        Some("+mj-lt") => theme.major_latin.clone(),
        Some("+mn-lt") => theme.minor_latin.clone(),
        Some(family) if !family.starts_with('+') => Some(family.to_string()),
        _ if category == Category::Title => theme.major_latin.clone(),
        _ => theme.minor_latin.clone(),
    }
    .unwrap_or_else(|| "Calibri".to_string());
    let weight = if run.bold == Some(true) { 700 } else { 400 };
    let resolved = matching::resolve(fonts, &family, weight, run.italic == Some(true));
    for w in resolved.warnings {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    let Some(face) = resolved.face else {
        return Ok(());
    };
    let mut face_ids = matching::coverage_fallbacks(fonts, face, weight, run.italic == Some(true));
    if face_ids.first() != Some(&face) {
        face_ids.insert(0, face);
    }
    let font_faces: Vec<_> = face_ids.iter().filter_map(|id| fonts.face(*id)).collect();
    if font_faces.is_empty() {
        return Ok(());
    }
    let color = run
        .color
        .as_ref()
        .map(|c| resolve_colorspec(theme, c))
        .unwrap_or(Rgba::opaque(Rgb::new(0, 0, 0)));
    let link = run.hlink.as_deref().and_then(link_resolver);

    for piece in run.text.split_inclusive('\n') {
        let text = piece.trim_end_matches('\n');
        // PowerPoint kerns text by default (DrawingML `kern` defaults to on).
        let shaped = shape::require_glyphs(shape::shape_with_fallback(
            text,
            &font_faces,
            size,
            0.0,
            true,
        ))?;
        let base_direction = if para.rtl {
            text_core::Direction::Rtl
        } else {
            text_core::Direction::Ltr
        };
        let mapped =
            crate::text::semantic::map_shaped_run_clusters_with_direct(text, &shaped, !para.rtl);
        let semantic_face = face_ids
            .get(mapped.owner_face_index)
            .copied()
            .unwrap_or(face);
        let group_id = atoms.len();
        let mut semantic_by_owner: HashMap<usize, Vec<AtomSemanticCluster>> = HashMap::new();
        for cluster in mapped.semantic_clusters {
            semantic_by_owner
                .entry(cluster.owner_visible_index)
                .or_default()
                .push(AtomSemanticCluster {
                    group_id,
                    source_order: cluster.source_offset,
                    face: semantic_face,
                    size_pt: size,
                    base_direction,
                    cluster: SemanticCluster {
                        gid: cluster.gid,
                        source: cluster.source,
                        advance_pt: cluster.advance_pt,
                    },
                });
        }
        for (visible_index, ((g, visible), is_space)) in shaped
            .glyphs
            .iter()
            .zip(mapped.visible)
            .zip(mapped.is_space)
            .enumerate()
        {
            let face = face_ids.get(g.face_index).copied().unwrap_or(face);
            let metrics = fonts
                .face(face)
                .and_then(|font| font.parsed())
                .map(FaceMetrics::new);
            atoms.push(Atom {
                gid: visible.gid,
                advance_pt: visible.advance_pt,
                x_offset_pt: visible.x_offset_pt,
                y_offset_pt: visible.y_offset_pt,
                ch: visible.ch,
                is_space,
                face,
                size_pt: size,
                color,
                underline: run.underline,
                synthetic_bold: run.bold == Some(true)
                    && fonts.face(face).is_some_and(|font| font.weight < 600),
                ascent_pt: metrics
                    .as_ref()
                    .map(|metrics| metrics.ascent_pt(size))
                    .unwrap_or(size),
                line_height_pt: metrics
                    .as_ref()
                    .map(|metrics| metrics.line_height_pt(size))
                    .unwrap_or_else(|| normalized_line_height(size)),
                link: link.clone(),
                semantic: semantic_by_owner.remove(&visible_index).unwrap_or_default(),
            });
        }
        if piece.ends_with('\n') {
            atoms.push(Atom {
                gid: 0,
                advance_pt: 0.0,
                x_offset_pt: 0.0,
                y_offset_pt: 0.0,
                ch: '\n',
                is_space: false,
                face,
                size_pt: size,
                color,
                underline: false,
                synthetic_bold: false,
                ascent_pt: fonts
                    .face(face)
                    .and_then(|font| font.parsed())
                    .map(|parsed| FaceMetrics::new(parsed).ascent_pt(size))
                    .unwrap_or(size),
                line_height_pt: fonts
                    .face(face)
                    .and_then(|font| font.parsed())
                    .map(|parsed| FaceMetrics::new(parsed).line_height_pt(size))
                    .unwrap_or_else(|| normalized_line_height(size)),
                link: link.clone(),
                semantic: Vec::new(),
            });
        }
    }
    Ok(())
}

fn bullet_prefix(para: &TextParagraph, numbering: &mut [u32; 9]) -> Option<String> {
    if para.bullet_none {
        return None;
    }
    if let Some((format, start)) = &para.bullet_auto_num {
        let level = para.level as usize;
        if numbering[level] == 0 {
            numbering[level] = *start;
        } else {
            numbering[level] += 1;
        }
        for deeper in &mut numbering[level + 1..] {
            *deeper = 0;
        }
        return match format.as_str() {
            "arabicPeriod" => Some(format!("{}.", numbering[level])),
            _ => None, // profile rejects unsupported formats before layout
        };
    }
    para.bullet_char.clone().map(|c| {
        // Map symbol bullets to Unicode.
        c.chars()
            .map(|ch| matching::map_symbol_char("Wingdings", ch as u32).unwrap_or(ch))
            .collect()
    })
}

/// Breaks the text atoms (past the leading `skip` bullet atoms) into lines.
/// The first line wraps at `first_width` (text starts at marL, or past an
/// overflowing bullet); continuation lines wrap at `rest_width` (marL-based).
fn break_lines(atoms: &[Atom], skip: usize, first_width: f64, rest_width: f64) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current = Vec::new();
    let mut used = 0.0;
    let mut last_space = None;
    for (i, a) in atoms.iter().enumerate().skip(skip) {
        let width = if lines.is_empty() {
            first_width
        } else {
            rest_width
        };
        if a.ch == '\n' {
            lines.push(std::mem::take(&mut current));
            used = 0.0;
            last_space = None;
            continue;
        }
        if used + a.advance_pt > width && !current.is_empty() && !a.is_space {
            if let Some(bp) = last_space.filter(|&bp: &usize| bp < current.len()) {
                let rest = current.split_off(bp);
                lines.push(std::mem::take(&mut current));
                used = rest.iter().map(|&k: &usize| atoms[k].advance_pt).sum();
                current = rest;
            } else {
                lines.push(std::mem::take(&mut current));
                used = 0.0;
            }
            last_space = None;
        }
        current.push(i);
        used += a.advance_pt;
        if a.is_space {
            last_space = Some(current.len());
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Places one line's segments. Each segment is `(base_x, atom indices)`; a
/// bulleted first line carries the bullet as its own segment at marL+indent
/// ahead of the text segment at marL. Alignment offsets are computed from the
/// text (last) segment's natural width against `avail`, the marL-reduced
/// content width, and shift every segment together.
fn place(
    atoms: &[Atom],
    segments: &[(f64, &[usize])],
    avail: f64,
    align: &str,
    y_top: f64,
    rtl: bool,
) -> (Vec<LineItem>, f64) {
    let all_indices = || segments.iter().flat_map(|(_, indices)| indices.iter());
    let natural: f64 = segments
        .last()
        .map(|(_, indices)| indices.iter().map(|&k| atoms[k].advance_pt).sum())
        .unwrap_or(0.0);
    let ascent = all_indices()
        .map(|&k| atoms[k].ascent_pt)
        .fold(0.0, f64::max);
    let height = all_indices()
        .map(|&k| atoms[k].line_height_pt)
        .fold(0.0, f64::max);
    let height = if height <= 0.0 { 14.0 } else { height };
    let align_offset = match align {
        "ctr" => (avail - natural).max(0.0) / 2.0,
        "r" => (avail - natural).max(0.0),
        _ => 0.0,
    };

    let mut items = Vec::new();
    // The secondary key lets a semantic run sort before a visible glyph run
    // when both begin at the same source atom. That tie occurs when a complex
    // cluster starts a same-style run followed by directly mapped text; the
    // visible run otherwise exposes the later direct text first to copy/search.
    let mut text_items: Vec<(usize, u8, LineItem)> = Vec::new();
    let mut x = 0.0;
    let mut pending: Vec<PlacedGlyph> = Vec::new();
    let mut pstyle: Option<(FaceId, f64, Rgba, bool, bool, Option<LinkTarget>)> = None;
    let mut pending_order: Option<usize> = None;
    let mut px = x;
    let mut line_semantics: BTreeMap<usize, Vec<(f64, AtomSemanticCluster)>> = BTreeMap::new();

    let flush = |pending: &mut Vec<PlacedGlyph>,
                 pstyle: &mut Option<(FaceId, f64, Rgba, bool, bool, Option<LinkTarget>)>,
                 pending_order: &mut Option<usize>,
                 px: f64,
                 items: &mut Vec<LineItem>,
                 text_items: &mut Vec<(usize, u8, LineItem)>| {
        if let Some((face, size, color, ul, synthetic_bold, link)) = pstyle.take()
            && !pending.is_empty()
        {
            let underline = ul.then(|| Decor {
                thickness_pt: (size * 0.06).max(0.4),
                y_offset_pt: size * 0.12,
            });
            let width = pending.iter().map(|glyph| glyph.advance_pt).sum();
            text_items.push((
                pending_order.take().unwrap_or(usize::MAX),
                1,
                LineItem {
                    x_pt: px,
                    y_pt: y_top + ascent,
                    item: Item::Glyphs(GlyphRun {
                        face,
                        size_pt: size,
                        color,
                        synthetic_bold,
                        base_direction: Some(if rtl {
                            text_core::Direction::Rtl
                        } else {
                            text_core::Direction::Ltr
                        }),
                        glyphs: std::mem::take(pending),
                        underline,
                        strike: None,
                    }),
                },
            ));
            if let Some(target) = link {
                items.push(LineItem {
                    x_pt: px,
                    y_pt: y_top,
                    item: Item::Link {
                        w_pt: width,
                        h_pt: height,
                        target,
                    },
                });
            }
        }
    };

    for &(segment_x, indices) in segments {
        x = segment_x + align_offset;
        for &k in indices {
            let a = &atoms[k];
            let key = (a.face, a.size_pt, a.color, a.underline, a.synthetic_bold, a.link.clone());
            let same_style = pstyle.as_ref().is_some_and(|style| {
                style.0 == key.0
                    && style.1 == key.1
                    && style.2 == key.2
                    && style.3 == key.3
                    && style.4 == key.4
                    && same_link(style.5.as_ref(), key.5.as_ref())
            });
            if !same_style {
                flush(
                    &mut pending,
                    &mut pstyle,
                    &mut pending_order,
                    px,
                    &mut items,
                    &mut text_items,
                );
                pstyle = Some(key);
                pending_order = Some(k.saturating_add(1));
                px = x;
            } else {
                pending_order = Some(pending_order.unwrap_or(usize::MAX).min(k.saturating_add(1)));
            }
            let advance_end = x + a.advance_pt;
            for owned in &a.semantic {
                line_semantics
                    .entry(owned.group_id)
                    .or_default()
                    .push((advance_end - owned.cluster.advance_pt, owned.clone()));
            }
            pending.push(PlacedGlyph {
                gid: a.gid,
                advance_pt: a.advance_pt,
                x_offset_pt: a.x_offset_pt,
                y_offset_pt: a.y_offset_pt,
                ch: a.ch,
            });
            x += a.advance_pt;
        }
        // A segment boundary is a positioning jump; never merge runs across it.
        flush(
            &mut pending,
            &mut pstyle,
            &mut pending_order,
            px,
            &mut items,
            &mut text_items,
        );
    }
    for (_, mut semantic) in line_semantics {
        semantic.sort_by_key(|(_, owned)| owned.source_order);
        let Some((_, first)) = semantic.first() else {
            continue;
        };
        let origin_x = semantic
            .iter()
            .map(|(origin, _)| *origin)
            .fold(f64::INFINITY, f64::min);
        let face = first.face;
        let size_pt = first.size_pt;
        let base_direction = first.base_direction;
        let group_id = first.group_id;
        text_items.push((
            group_id.saturating_add(1),
            0,
            LineItem {
                x_pt: origin_x,
                y_pt: y_top + ascent,
                item: Item::SemanticText(SemanticRun {
                    face,
                    size_pt,
                    base_direction,
                    clusters: semantic
                        .into_iter()
                        .map(|(_, owned)| owned.cluster)
                        .collect(),
                }),
            },
        ));
    }
    text_items.sort_by_key(|(order, priority, _)| (*order, *priority));
    items.extend(text_items.into_iter().map(|(_, _, item)| item));
    (items, height)
}

fn same_link(left: Option<&LinkTarget>, right: Option<&LinkTarget>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(LinkTarget::Url(left)), Some(LinkTarget::Url(right))) => left == right,
        (Some(LinkTarget::Page(left)), Some(LinkTarget::Page(right))) => left == right,
        _ => false,
    }
}

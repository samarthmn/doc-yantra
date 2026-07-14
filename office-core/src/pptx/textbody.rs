//! PPTX text-body layout: lays a [`TextBody`] into positioned glyph runs within
//! a box, with bullets, alignment, and master/level default sizes.

use super::assemble::{Category, TextDefaults};
use super::geometry::resolve_colorspec;
use crate::Result;
use crate::fonts::metrics::FaceMetrics;
use crate::fonts::{FaceId, FontDb, matching};
use crate::layout::paragraph::LineItem;
use crate::layout::{Decor, GlyphRun, Item, LinkTarget, PlacedGlyph, SemanticCluster, SemanticRun};
use crate::oxml::pml::{TextBody, TextParagraph, TextRun};
use crate::oxml::theme::Theme;
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::text::shape;
use std::collections::{BTreeMap, HashMap};

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

    for para in &body.paragraphs {
        let bullet = bullet_prefix(para, &mut numbering);
        let align = para
            .align
            .as_deref()
            .unwrap_or(if para.rtl { "r" } else { "l" });
        let indent = para.level as f64 * 18.0;
        let avail = (content_w - indent).max(1.0);

        let atoms = shape_paragraph(
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
        if atoms.is_empty() {
            // Empty paragraph still advances by a default line.
            y += defaults.size(category, para.level) * scale * 1.2;
            continue;
        }
        for line in break_lines(&atoms, avail) {
            let (line_items, line_h) = place(&atoms, &line, indent, avail, align, y, para.rtl);
            items.extend(line_items);
            y += line_h;
        }
    }
    Ok(BodyLayout {
        items,
        height_pt: y,
    })
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
) -> Result<Vec<Atom>> {
    let mut atoms = Vec::new();
    // Bullet as a leading run using the first run's style (or defaults).
    if let Some(b) = bullet {
        let synthetic = TextRun {
            text: format!("{b}  "),
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
    Ok(atoms)
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
                ascent_pt: metrics
                    .as_ref()
                    .map(|metrics| metrics.ascent_pt(size))
                    .unwrap_or(size),
                line_height_pt: metrics
                    .as_ref()
                    .map(|metrics| metrics.line_height_pt(size))
                    .unwrap_or(size * 1.2),
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
                ascent_pt: fonts
                    .face(face)
                    .and_then(|font| font.parsed())
                    .map(|parsed| FaceMetrics::new(parsed).ascent_pt(size))
                    .unwrap_or(size),
                line_height_pt: fonts
                    .face(face)
                    .and_then(|font| font.parsed())
                    .map(|parsed| FaceMetrics::new(parsed).line_height_pt(size))
                    .unwrap_or(size * 1.2),
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

fn break_lines(atoms: &[Atom], width: f64) -> Vec<Vec<usize>> {
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut used = 0.0;
    let mut last_space = None;
    for (i, a) in atoms.iter().enumerate() {
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

fn place(
    atoms: &[Atom],
    indices: &[usize],
    indent: f64,
    avail: f64,
    align: &str,
    y_top: f64,
    rtl: bool,
) -> (Vec<LineItem>, f64) {
    let natural: f64 = indices.iter().map(|&k| atoms[k].advance_pt).sum();
    let ascent = indices
        .iter()
        .map(|&k| atoms[k].ascent_pt)
        .fold(0.0, f64::max);
    let height = indices
        .iter()
        .map(|&k| atoms[k].line_height_pt)
        .fold(0.0, f64::max);
    let height = if height <= 0.0 { 14.0 } else { height };
    let start = indent
        + match align {
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
    let mut x = start;
    let mut pending: Vec<PlacedGlyph> = Vec::new();
    let mut pstyle: Option<(FaceId, f64, Rgba, bool, Option<LinkTarget>)> = None;
    let mut pending_order: Option<usize> = None;
    let mut px = x;
    let mut line_semantics: BTreeMap<usize, Vec<(f64, AtomSemanticCluster)>> = BTreeMap::new();

    let flush = |pending: &mut Vec<PlacedGlyph>,
                 pstyle: &mut Option<(FaceId, f64, Rgba, bool, Option<LinkTarget>)>,
                 pending_order: &mut Option<usize>,
                 px: f64,
                 items: &mut Vec<LineItem>,
                 text_items: &mut Vec<(usize, u8, LineItem)>| {
        if let Some((face, size, color, ul, link)) = pstyle.take()
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

    for &k in indices {
        let a = &atoms[k];
        let key = (a.face, a.size_pt, a.color, a.underline, a.link.clone());
        let same_style = pstyle.as_ref().is_some_and(|style| {
            style.0 == key.0
                && style.1 == key.1
                && style.2 == key.2
                && style.3 == key.3
                && same_link(style.4.as_ref(), key.4.as_ref())
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
    flush(
        &mut pending,
        &mut pstyle,
        &mut pending_order,
        px,
        &mut items,
        &mut text_items,
    );
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

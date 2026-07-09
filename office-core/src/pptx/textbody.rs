//! PPTX text-body layout: lays a [`TextBody`] into positioned glyph runs within
//! a box, with bullets, alignment, and master/level default sizes.

use super::assemble::{Category, TextDefaults};
use super::geometry::resolve_colorspec;
use crate::fonts::metrics::FaceMetrics;
use crate::fonts::{FaceId, FontDb, matching};
use crate::layout::paragraph::LineItem;
use crate::layout::{Decor, GlyphRun, Item, PlacedGlyph};
use crate::oxml::pml::{TextBody, TextParagraph, TextRun};
use crate::oxml::theme::Theme;
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::text::shape;

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
) -> BodyLayout {
    let scale = body.font_scale.unwrap_or(1.0);
    let mut items = Vec::new();
    let mut y = 0.0_f64;

    for para in &body.paragraphs {
        let bullet = bullet_prefix(para);
        let align = para.align.as_deref().unwrap_or("l");
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
        );
        if atoms.is_empty() {
            // Empty paragraph still advances by a default line.
            y += defaults.size(category, para.level) * scale * 1.2;
            continue;
        }
        for line in break_lines(&atoms, avail) {
            let (line_items, line_h) = place(&atoms, &line, indent, avail, align, y);
            items.extend(line_items);
            y += line_h;
        }
    }
    BodyLayout {
        items,
        height_pt: y,
    }
}

/// A shaped glyph atom.
struct Atom {
    gid: u16,
    advance_pt: f64,
    ch: char,
    is_space: bool,
    face: FaceId,
    size_pt: f64,
    color: Rgba,
    underline: bool,
    ascent_pt: f64,
    line_height_pt: f64,
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
) -> Vec<Atom> {
    let mut atoms = Vec::new();
    // Bullet as a leading run using the first run's style (or defaults).
    if let Some(b) = bullet {
        let synthetic = TextRun {
            text: format!("{b}  "),
            ..para.runs.first().cloned().unwrap_or_default()
        };
        shape_run(
            fonts, theme, defaults, warnings, &synthetic, para, category, scale, &mut atoms,
        );
    }
    for run in &para.runs {
        shape_run(
            fonts, theme, defaults, warnings, run, para, category, scale, &mut atoms,
        );
    }
    atoms
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
    atoms: &mut Vec<Atom>,
) {
    let size = run
        .size_pt
        .unwrap_or_else(|| defaults.size(category, para.level))
        * scale;
    let family = run
        .typeface
        .clone()
        .filter(|f| !f.starts_with('+'))
        .or_else(|| theme.minor_latin.clone())
        .unwrap_or_else(|| "Calibri".to_string());
    let weight = if run.bold == Some(true) { 700 } else { 400 };
    let resolved = matching::resolve(fonts, &family, weight, run.italic == Some(true));
    for w in resolved.warnings {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    let Some(face) = resolved.face else {
        return;
    };
    let Some(parsed) = fonts.face(face).and_then(|f| f.parsed()) else {
        return;
    };
    let metrics = FaceMetrics::new(parsed);
    let color = run
        .color
        .as_ref()
        .map(|c| resolve_colorspec(theme, c))
        .unwrap_or(Rgba::opaque(Rgb::new(0, 0, 0)));

    for piece in run.text.split_inclusive('\n') {
        let text = piece.trim_end_matches('\n');
        // PowerPoint kerns text by default (DrawingML `kern` defaults to on).
        let shaped = shape::shape(text, &metrics, size, 0.0, true);
        let chars: std::collections::HashMap<usize, char> = text.char_indices().collect();
        for g in &shaped.glyphs {
            let ch = chars.get(&g.cluster).copied().unwrap_or(' ');
            atoms.push(Atom {
                gid: g.id.0,
                advance_pt: g.advance_pt,
                ch,
                is_space: ch == ' ',
                face,
                size_pt: size,
                color,
                underline: run.underline,
                ascent_pt: metrics.ascent_pt(size),
                line_height_pt: metrics.line_height_pt(size),
            });
        }
        if piece.ends_with('\n') {
            atoms.push(Atom {
                gid: 0,
                advance_pt: 0.0,
                ch: '\n',
                is_space: false,
                face,
                size_pt: size,
                color,
                underline: false,
                ascent_pt: metrics.ascent_pt(size),
                line_height_pt: metrics.line_height_pt(size),
            });
        }
    }
}

fn bullet_prefix(para: &TextParagraph) -> Option<String> {
    if para.bullet_none {
        return None;
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
    let mut x = start;
    let mut pending: Vec<PlacedGlyph> = Vec::new();
    let mut pstyle: Option<(FaceId, f64, Rgba, bool)> = None;
    let mut px = x;

    let flush = |pending: &mut Vec<PlacedGlyph>,
                 pstyle: &mut Option<(FaceId, f64, Rgba, bool)>,
                 px: f64,
                 items: &mut Vec<LineItem>| {
        if let Some((face, size, color, ul)) = pstyle.take()
            && !pending.is_empty()
        {
            let underline = ul.then(|| Decor {
                thickness_pt: (size * 0.06).max(0.4),
                y_offset_pt: size * 0.12,
            });
            items.push(LineItem {
                x_pt: px,
                y_pt: y_top + ascent,
                item: Item::Glyphs(GlyphRun {
                    face,
                    size_pt: size,
                    color,
                    glyphs: std::mem::take(pending),
                    underline,
                    strike: None,
                }),
            });
        }
    };

    for &k in indices {
        let a = &atoms[k];
        let key = (a.face, a.size_pt, a.color, a.underline);
        if pstyle != Some(key) {
            flush(&mut pending, &mut pstyle, px, &mut items);
            pstyle = Some(key);
            px = x;
        }
        pending.push(PlacedGlyph {
            gid: a.gid,
            advance_pt: a.advance_pt,
            ch: a.ch,
        });
        x += a.advance_pt;
    }
    flush(&mut pending, &mut pstyle, px, &mut items);
    (items, height)
}

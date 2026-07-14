//! Placeholder inheritance: a slide shape with no geometry inherits its `xfrm`
//! from the matching placeholder on the layout, then the master.

use super::assemble::Category;
use crate::oxml::pml::{
    Placeholder, ShapeElem, Slide, TextBody, TextBulletStyle, TextListStyleLevel, TextRun, Xfrm,
};

/// The placeholder category for a `p:ph @type`.
pub fn category(ph: &Placeholder) -> Category {
    match ph.ph_type.as_deref() {
        Some("title") | Some("ctrTitle") => Category::Title,
        Some("body") | Some("subTitle") | None => Category::Body,
        _ => Category::Other,
    }
}

/// Finds the `xfrm` for a slide placeholder, searching the layout then master.
pub fn resolve_xfrm(ph: &Placeholder, own: Option<Xfrm>, layout: &Slide, master: &Slide) -> Xfrm {
    if let Some(x) = own.filter(|x| x.present && x.w_pt > 0.0) {
        return x;
    }
    if let Some(x) = find_ph_xfrm(ph, layout).or_else(|| find_ph_xfrm(ph, master)) {
        return x;
    }
    default_box(ph)
}

/// Applies the slide placeholder's own list style, then layout, then master.
pub fn resolve_text_body(
    ph: Option<&Placeholder>,
    own: &TextBody,
    layout: &Slide,
    master: &Slide,
) -> TextBody {
    let Some(ph) = ph else { return own.clone() };
    let layout_body = find_ph_text(ph, layout);
    let master_body = find_ph_text(ph, master);
    let mut resolved = own.clone();
    for paragraph in &mut resolved.paragraphs {
        let level = paragraph.level as usize;
        let own_style = own.list_style.get(level).and_then(Option::as_ref);
        let layout_style = layout_body
            .and_then(|body| body.list_style.get(level))
            .and_then(Option::as_ref);
        let master_style = master_body
            .and_then(|body| body.list_style.get(level))
            .and_then(Option::as_ref);
        apply_list_level(paragraph, own_style, layout_style, master_style);
    }
    resolved
}

fn apply_list_level(
    paragraph: &mut crate::oxml::pml::TextParagraph,
    own: Option<&TextListStyleLevel>,
    layout: Option<&TextListStyleLevel>,
    master: Option<&TextListStyleLevel>,
) {
    let styles = [own, layout, master];
    if paragraph.align.is_none() {
        paragraph.align = styles
            .iter()
            .flatten()
            .find_map(|style| style.align.clone());
    }
    if !paragraph.bullet_none
        && paragraph.bullet_char.is_none()
        && paragraph.bullet_auto_num.is_none()
        && let Some(bullet) = styles
            .iter()
            .flatten()
            .find_map(|style| style.bullet.as_ref())
    {
        match bullet {
            TextBulletStyle::None => paragraph.bullet_none = true,
            TextBulletStyle::Character(character) => {
                paragraph.bullet_char = Some(character.clone())
            }
            TextBulletStyle::AutoNumber(format, start) => {
                paragraph.bullet_auto_num = Some((format.clone(), *start))
            }
        }
    }
    for run in &mut paragraph.runs {
        apply_run_defaults(run, styles);
    }
}

fn apply_run_defaults(run: &mut TextRun, styles: [Option<&TextListStyleLevel>; 3]) {
    let defaults = styles.map(|style| style.map(|style| &style.default_run));
    if run.bold.is_none() {
        run.bold = defaults.iter().flatten().find_map(|run| run.bold);
    }
    if run.italic.is_none() {
        run.italic = defaults.iter().flatten().find_map(|run| run.italic);
    }
    if !run.underline {
        run.underline = defaults.iter().flatten().any(|run| run.underline);
    }
    if run.size_pt.is_none() {
        run.size_pt = defaults.iter().flatten().find_map(|run| run.size_pt);
    }
    if run.typeface.is_none() {
        run.typeface = defaults
            .iter()
            .flatten()
            .find_map(|run| run.typeface.clone());
    }
    if run.color.is_none() {
        run.color = defaults.iter().flatten().find_map(|run| run.color.clone());
    }
}

fn find_ph_xfrm(ph: &Placeholder, slide: &Slide) -> Option<Xfrm> {
    // Prefer a same-idx match, then a same-type match.
    let mut by_type = None;
    for elem in &slide.shapes {
        if let ShapeElem::Shape {
            ph: Some(other),
            props,
            ..
        } = elem
            && let Some(x) = props.xfrm.filter(|x| x.present && x.w_pt > 0.0)
        {
            if other.idx.is_some() && other.idx == ph.idx {
                return Some(x);
            }
            if by_type.is_none() && other.ph_type == ph.ph_type {
                by_type = Some(x);
            }
        }
    }
    by_type
}

fn find_ph_text<'a>(ph: &Placeholder, slide: &'a Slide) -> Option<&'a TextBody> {
    let mut by_type = None;
    for elem in &slide.shapes {
        if let ShapeElem::Shape {
            ph: Some(other),
            text: Some(text),
            ..
        } = elem
        {
            if other.idx.is_some() && other.idx == ph.idx {
                return Some(text);
            }
            if by_type.is_none() && other.ph_type == ph.ph_type {
                by_type = Some(text);
            }
        }
    }
    by_type
}

/// A default placeholder box (points) when nothing is inherited — sized for the
/// common 4:3/16:9 slide so text is at least placed sensibly.
fn default_box(ph: &Placeholder) -> Xfrm {
    match category(ph) {
        Category::Title => Xfrm {
            x_pt: 60.0,
            y_pt: 40.0,
            w_pt: 840.0,
            h_pt: 90.0,
            present: true,
            ..Default::default()
        },
        _ => Xfrm {
            x_pt: 60.0,
            y_pt: 150.0,
            w_pt: 840.0,
            h_pt: 340.0,
            present: true,
            ..Default::default()
        },
    }
}

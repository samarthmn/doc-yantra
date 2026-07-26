//! Placeholder inheritance: a slide shape with no geometry inherits its `xfrm`
//! from the matching placeholder on the layout, then the master.

use super::assemble::{Category, TextDefaults};
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

/// Chooses the text-style bucket for a shape. A placeholder with an omitted
/// `@type` is a body placeholder, while a shape with no `p:ph` at all is
/// freeform text and uses the master's `otherStyle`.
pub fn shape_category(ph: Option<&Placeholder>) -> Category {
    ph.map(category).unwrap_or(Category::Other)
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
    defaults: &TextDefaults,
) -> TextBody {
    let Some(ph) = ph else { return own.clone() };
    let layout_body = find_ph_text(ph, layout);
    let master_body = find_ph_text(ph, master);
    let mut resolved = own.clone();
    resolved.anchor = own
        .anchor
        .clone()
        .or_else(|| layout_body.and_then(|body| body.anchor.clone()))
        .or_else(|| master_body.and_then(|body| body.anchor.clone()));
    let autofit_source = [Some(own), layout_body, master_body]
        .into_iter()
        .flatten()
        .find(|body| body.norm_autofit.is_some());
    resolved.norm_autofit = autofit_source.and_then(|body| body.norm_autofit);
    resolved.font_scale = autofit_source
        .filter(|body| body.norm_autofit == Some(true))
        .and_then(|body| body.font_scale);
    inherit_insets(&mut resolved, own, layout_body, master_body);
    for inherited in [layout_body, master_body].into_iter().flatten() {
        for detail in &inherited.unsupported {
            if !resolved.unsupported.contains(detail) {
                resolved.unsupported.push(detail.clone());
            }
        }
    }
    for paragraph in &mut resolved.paragraphs {
        let level = paragraph.level as usize;
        let own_style = own.list_style.get(level).and_then(Option::as_ref);
        let layout_style = layout_body
            .and_then(|body| body.list_style.get(level))
            .and_then(Option::as_ref);
        let master_style = master_body
            .and_then(|body| body.list_style.get(level))
            .and_then(Option::as_ref);
        // The master's `p:txStyles` bucket is the lowest-precedence source —
        // it is where PowerPoint stores the default placeholder bullets.
        let txstyles_style = defaults.level_style(category(ph), level);
        apply_list_level(
            paragraph,
            own_style,
            layout_style,
            master_style,
            txstyles_style,
        );
    }
    resolved
}

fn inherit_insets(
    resolved: &mut TextBody,
    own: &TextBody,
    layout: Option<&TextBody>,
    master: Option<&TextBody>,
) {
    let horizontal = [
        (
            own.inset_lr.0,
            own.inset_lr_explicit.0,
            layout.map(|body| (body.inset_lr.0, body.inset_lr_explicit.0)),
            master.map(|body| (body.inset_lr.0, body.inset_lr_explicit.0)),
        ),
        (
            own.inset_lr.1,
            own.inset_lr_explicit.1,
            layout.map(|body| (body.inset_lr.1, body.inset_lr_explicit.1)),
            master.map(|body| (body.inset_lr.1, body.inset_lr_explicit.1)),
        ),
    ];
    let vertical = [
        (
            own.inset_tb.0,
            own.inset_tb_explicit.0,
            layout.map(|body| (body.inset_tb.0, body.inset_tb_explicit.0)),
            master.map(|body| (body.inset_tb.0, body.inset_tb_explicit.0)),
        ),
        (
            own.inset_tb.1,
            own.inset_tb_explicit.1,
            layout.map(|body| (body.inset_tb.1, body.inset_tb_explicit.1)),
            master.map(|body| (body.inset_tb.1, body.inset_tb_explicit.1)),
        ),
    ];
    resolved.inset_lr = (
        inherited_inset(horizontal[0]),
        inherited_inset(horizontal[1]),
    );
    resolved.inset_tb = (inherited_inset(vertical[0]), inherited_inset(vertical[1]));
    resolved.inset_lr_explicit = (true, true);
    resolved.inset_tb_explicit = (true, true);
}

type InsetValue = (f64, bool);
type InsetInheritance = (f64, bool, Option<InsetValue>, Option<InsetValue>);

fn inherited_inset((own_value, own_explicit, layout, master): InsetInheritance) -> f64 {
    if own_explicit {
        own_value
    } else if let Some((value, true)) = layout {
        value
    } else if let Some((value, true)) = master {
        value
    } else {
        own_value
    }
}

fn apply_list_level(
    paragraph: &mut crate::oxml::pml::TextParagraph,
    own: Option<&TextListStyleLevel>,
    layout: Option<&TextListStyleLevel>,
    master: Option<&TextListStyleLevel>,
    txstyles: Option<&TextListStyleLevel>,
) {
    let styles = [own, layout, master, txstyles];
    if paragraph.align.is_none() {
        paragraph.align = styles
            .iter()
            .flatten()
            .find_map(|style| style.align.clone());
    }
    if paragraph.mar_l_pt.is_none() {
        paragraph.mar_l_pt = styles.iter().flatten().find_map(|style| style.mar_l_pt);
    }
    if paragraph.indent_pt.is_none() {
        paragraph.indent_pt = styles.iter().flatten().find_map(|style| style.indent_pt);
    }
    if paragraph.space_before.is_none() {
        paragraph.space_before = styles.iter().flatten().find_map(|style| style.space_before);
    }
    if paragraph.space_after.is_none() {
        paragraph.space_after = styles.iter().flatten().find_map(|style| style.space_after);
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

fn apply_run_defaults(run: &mut TextRun, styles: [Option<&TextListStyleLevel>; 4]) {
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
            if by_type.is_none() && equivalent_placeholder_type(other, ph) {
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
            if by_type.is_none() && equivalent_placeholder_type(other, ph) {
                by_type = Some(text);
            }
        }
    }
    by_type
}

fn equivalent_placeholder_type(left: &Placeholder, right: &Placeholder) -> bool {
    left.ph_type == right.ph_type
        || matches!(
            (left.ph_type.as_deref(), right.ph_type.as_deref()),
            (Some("title"), Some("ctrTitle")) | (Some("ctrTitle"), Some("title"))
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oxml::pml::ShapeProps;

    fn placeholder_shape(ph_type: &str, anchor: Option<&str>) -> ShapeElem {
        placeholder_shape_with_body(
            ph_type,
            TextBody {
                anchor: anchor.map(str::to_string),
                ..TextBody::default()
            },
        )
    }

    fn placeholder_shape_with_body(ph_type: &str, body: TextBody) -> ShapeElem {
        ShapeElem::Shape {
            ph: Some(Placeholder {
                ph_type: Some(ph_type.to_string()),
                idx: None,
            }),
            props: ShapeProps::default(),
            text: Some(body),
        }
    }

    #[test]
    fn centered_title_placeholder_inherits_equivalent_master_title_anchor() {
        let ph = Placeholder {
            ph_type: Some("ctrTitle".to_string()),
            idx: None,
        };
        let layout = Slide {
            shapes: vec![placeholder_shape("ctrTitle", None)],
            ..Slide::default()
        };
        let master = Slide {
            shapes: vec![ShapeElem::Shape {
                ph: Some(Placeholder {
                    ph_type: Some("title".to_string()),
                    idx: None,
                }),
                props: ShapeProps::default(),
                text: Some(TextBody {
                    anchor: Some("ctr".to_string()),
                    norm_autofit: Some(true),
                    ..TextBody::default()
                }),
            }],
            ..Slide::default()
        };

        let resolved = resolve_text_body(
            Some(&ph),
            &TextBody::default(),
            &layout,
            &master,
            &TextDefaults::default(),
        );

        assert_eq!(
            resolved.anchor.as_deref(),
            Some("ctr"),
            "ctrTitle and title are equivalent placeholder kinds for bodyPr inheritance"
        );
        assert_eq!(
            resolved.norm_autofit,
            Some(true),
            "the equivalent master title's normalized-autofit line rule must inherit"
        );
    }

    #[test]
    fn freeform_shape_uses_other_style_but_typeless_placeholder_uses_body_style() {
        assert_eq!(shape_category(None), Category::Other);
        assert_eq!(
            shape_category(Some(&Placeholder::default())),
            Category::Body
        );
    }

    #[test]
    fn explicit_zero_inset_and_no_autofit_override_inherited_body_properties() {
        let ph = Placeholder {
            ph_type: Some("body".to_string()),
            idx: None,
        };
        let own = TextBody {
            norm_autofit: Some(false),
            inset_lr: (0.0, 7.2),
            inset_tb: (3.6, 3.6),
            inset_lr_explicit: (true, false),
            ..TextBody::default()
        };
        let layout = Slide {
            shapes: vec![placeholder_shape_with_body(
                "body",
                TextBody {
                    inset_lr: (9.0, 10.0),
                    inset_tb: (11.0, 12.0),
                    inset_lr_explicit: (false, true),
                    inset_tb_explicit: (true, false),
                    ..TextBody::default()
                },
            )],
            ..Slide::default()
        };
        let master = Slide {
            shapes: vec![placeholder_shape_with_body(
                "body",
                TextBody {
                    norm_autofit: Some(true),
                    font_scale: Some(0.7),
                    inset_lr: (20.0, 20.0),
                    inset_tb: (20.0, 20.0),
                    inset_lr_explicit: (true, true),
                    inset_tb_explicit: (true, true),
                    ..TextBody::default()
                },
            )],
            ..Slide::default()
        };

        let resolved =
            resolve_text_body(Some(&ph), &own, &layout, &master, &TextDefaults::default());

        assert_eq!(resolved.norm_autofit, Some(false));
        assert_eq!(resolved.font_scale, None);
        assert_eq!(resolved.inset_lr, (0.0, 10.0));
        assert_eq!(resolved.inset_tb, (11.0, 20.0));
    }
}

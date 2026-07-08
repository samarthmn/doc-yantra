//! Placeholder inheritance: a slide shape with no geometry inherits its `xfrm`
//! from the matching placeholder on the layout, then the master.

use super::assemble::Category;
use crate::oxml::pml::{Placeholder, ShapeElem, Slide, Xfrm};

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

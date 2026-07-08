//! Shape geometry and fill helpers.
//!
//! v1 renders shape fills/outlines against their `xfrm` bounding box. The full
//! DrawingML preset-geometry evaluator (all presets + `custGeom` path rendering)
//! is a follow-up gated on vendoring ECMA-376's `presetShapeDefinitions.xml`
//! (a large external asset, like the H2 fonts) — see the plan README.

use crate::oxml::Rgba;
use crate::oxml::pml::{ColorSpec, Fill};
use crate::oxml::theme::{Theme, resolve_color};

/// Resolves a [`ColorSpec`] against the theme.
pub fn resolve_colorspec(theme: &Theme, spec: &ColorSpec) -> Rgba {
    resolve_color(spec.base.clone(), &spec.transforms, theme)
}

/// A solid fill color for a [`Fill`] (gradient → first stop; picture/none → `None`).
pub fn solid_fill_color(theme: &Theme, fill: &Fill) -> Option<Rgba> {
    match fill {
        Fill::Solid(c) => Some(resolve_colorspec(theme, c)),
        Fill::LinearGradient { stops, .. } => {
            stops.first().map(|(_, c)| resolve_colorspec(theme, c))
        }
        _ => None,
    }
}

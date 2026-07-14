//! Paragraph line layout: resolve fonts, shape runs, break into lines, align,
//! and decorate. Produces a [`ParaLayout`] of lines that [`super::flow`] places
//! into pages. Line height uses real OS/2 Windows metrics (the parity lever);
//! super/sub factors and decoration offsets are tuned by the fidelity harness.

use super::assemble::WordDoc;
use super::numbering_layout::{self, PlacedLabel};
use super::{
    Decor, GlyphRun, ImageId, Item, LinkTarget, PlacedGlyph, SemanticCluster, SemanticRun, Stroke,
};
use crate::fonts::metrics::FaceMetrics;
use crate::fonts::{FaceId, FontDb, matching};
use crate::oxml::numbering::LabelJustification;
use crate::oxml::wml::{
    AnchorOrigin, AnchorProps, BreakKind, Field, FrameAlign, InlineContent, Jc, LineSpacing,
    NoteKind, Paragraph, Run, RunColor, RunContent, RunProps, TabAlign, TabLeader, TabStop,
    TextBox, UnderlineKind, VertAlign, VmlShape, WrapMode,
};
use crate::oxml::{Rgb, Rgba};
use crate::profile::Warning;
use crate::text::shape;
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

/// Field substitution context (page numbering) for headers/footers.
#[derive(Debug, Clone)]
pub struct FieldContext {
    /// The current 1-based page number.
    pub page: u32,
    /// The total page count.
    pub total: u32,
    /// PAGE field text after applying section `pgNumType@fmt`.
    pub page_label: String,
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
    /// Notes referenced by this paragraph, in document order.
    pub note_refs: Vec<(NoteKind, i64)>,
    /// Rectangular float exclusion that continues into the next paragraph.
    pub(crate) wrap_after: Option<WrapExclusion>,
    /// Word float-line clearance added before this paragraph's first line.
    pub(crate) float_leading_pt: f64,
    /// Page-scoped floats carried by this paragraph's first flow unit.
    pub(crate) page_floats: Vec<PageFloat>,
}

/// One edge of a rectangular exclusion carried across paragraphs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExclusionBand {
    /// Remaining distance to the top of the exclusion.
    pub top_remaining_pt: f64,
    /// Remaining vertical extent from the next paragraph's content top.
    pub remaining_pt: f64,
    /// Width unavailable from this content edge.
    pub inset_pt: f64,
    /// Tight wrapping follows the text baseline through its polygon, while
    /// square wrapping excludes the complete line box.
    pub uses_text_baseline: bool,
}

/// Edge-aligned rectangular exclusions; both edges may be active together.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WrapExclusion {
    pub(crate) left_band: Option<ExclusionBand>,
    pub(crate) right_band: Option<ExclusionBand>,
}

#[derive(Debug, Clone, Copy)]
struct EdgeWrapBand {
    top_pt: f64,
    bottom_pt: f64,
    inset_pt: f64,
    uses_text_baseline: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct WrapBand {
    left: Option<EdgeWrapBand>,
    right: Option<EdgeWrapBand>,
}

impl EdgeWrapBand {
    fn applies_to_line(
        self,
        line_index: usize,
        line_height_pt: f64,
        baseline_offset_pt: f64,
    ) -> bool {
        let top = line_index as f64 * line_height_pt;
        if self.uses_text_baseline {
            // Tight wrapping evaluates the polygon at the text baseline.
            // Testing the complete line box keeps a line constrained after
            // its visible text has already cleared the polygon's bottom edge.
            let baseline = top + baseline_offset_pt;
            baseline > self.top_pt + 0.01 && baseline < self.bottom_pt - 0.01
        } else {
            let bottom = top + line_height_pt;
            top < self.bottom_pt - 0.01 && bottom > self.top_pt + 0.01
        }
    }
}

/// One laid-out line: items positioned relative to the line's top-left.
#[derive(Debug, Default)]
pub struct Line {
    /// Line box height.
    pub height_pt: f64,
    /// Items (`x` relative to content-left, `y` relative to line-top; glyph `y`
    /// is the baseline).
    pub items: Vec<LineItem>,
    /// Anchored items whose page-level paint order is resolved by flow.
    pub(crate) anchored_items: Vec<AnchoredLineItem>,
    /// An explicit page break (`<w:br w:type="page"/>`) ends this line — the flow
    /// must start the next line on a new page.
    pub page_break_after: bool,
    /// A `w:br type="column"` follows this line.
    pub column_break_after: bool,
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

/// An anchored item within a [`Line`], kept out of the public primitive enum so
/// page-layer metadata cannot leak into a completed [`super::LaidOutDocument`].
#[derive(Debug)]
pub(crate) struct AnchoredLineItem {
    pub(crate) x_pt: f64,
    pub(crate) y_pt: f64,
    pub(crate) behind_doc: bool,
    pub(crate) relative_height: u32,
    pub(crate) item: Item,
}

/// A margin/page-relative float whose placement is resolved during pagination.
#[derive(Debug)]
pub(crate) struct PageFloat {
    pub(crate) items: Vec<LineItem>,
    pub(crate) w_pt: f64,
    pub(crate) h_pt: f64,
    pub(crate) x_origin: AnchorOrigin,
    pub(crate) x_pt: f64,
    pub(crate) x_align: Option<FrameAlign>,
    pub(crate) y_origin: AnchorOrigin,
    pub(crate) y_pt: f64,
    pub(crate) y_align: Option<FrameAlign>,
    pub(crate) wrap: WrapMode,
    pub(crate) distances_pt: [f64; 4],
    pub(crate) behind_doc: bool,
    pub(crate) relative_height: u32,
}

/// Shared per-run style. Glyphs from one run share an `Rc` so coalescing is a
/// pointer comparison and hyperlink strings are not cloned per glyph.
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
struct AtomSemanticCluster {
    group_id: usize,
    source_order: usize,
    style: Rc<RunStyle>,
    base_direction: text_core::Direction,
    cluster: SemanticCluster,
}

/// An atom in the pre-break stream.
enum Atom {
    Glyph {
        gid: u16,
        advance_pt: f64,
        x_offset_pt: f64,
        y_offset_pt: f64,
        ch: char,
        is_space: bool,
        style: Rc<RunStyle>,
        semantic: Vec<AtomSemanticCluster>,
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
    Composite {
        w_pt: f64,
        h_pt: f64,
        baseline_pt: Option<f64>,
        items: Vec<LineItem>,
        anchor: Option<AnchorProps>,
    },
}

impl Atom {
    fn advance(&self) -> f64 {
        match self {
            Atom::Glyph { advance_pt, .. } => *advance_pt,
            Atom::Image { w_pt, .. } => *w_pt,
            Atom::Composite {
                w_pt, anchor: None, ..
            } => *w_pt,
            Atom::Composite { .. } => 0.0,
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
) -> Result<ParaLayout> {
    layout_paragraph_with_wrap(
        doc,
        warnings,
        para,
        rels,
        avail_width_pt,
        label_override,
        field_ctx,
        None,
    )
}

/// Lays out a body paragraph while honoring one active edge float.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_paragraph_with_wrap(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    rels: &HashMap<String, String>,
    avail_width_pt: f64,
    label_override: Option<PlacedLabel>,
    field_ctx: Option<FieldContext>,
    incoming_wrap: Option<WrapExclusion>,
) -> Result<ParaLayout> {
    let eff_para = doc.styles.para_props(&para.props);
    if let Some(detail) = eff_para.borders.unsupported_detail(&doc.theme) {
        return Err(OfficeError::unsupported(
            UnsupportedCode::UnknownElement,
            detail,
        ));
    }
    // Flow supplies a counter-accurate label; otherwise fall back to a
    // first-item approximation.
    let label = match label_override {
        Some(label) => Some(label),
        None => numbering_layout::resolve_label(doc, warnings, &eff_para, para)?,
    };

    // List levels contribute indentation when the paragraph specifies none.
    let label_left = label.as_ref().and_then(|l| l.left_pt);
    let label_hanging = label.as_ref().and_then(|l| l.hanging_pt);
    let left = eff_para
        .indent
        .left_pt
        .or(label_left)
        .unwrap_or(0.0)
        .max(0.0);
    let right_indent = eff_para.indent.right_pt.unwrap_or(0.0).max(0.0);
    let right_border = eff_para
        .borders
        .right
        .as_ref()
        .filter(|border| !matches!(border.val.as_str(), "nil" | "none"));
    let right_border_reserve = right_border
        .map(|border| border.space_pt + border.size_pt)
        .unwrap_or(0.0);
    let right = right_indent + right_border_reserve;
    let hanging = eff_para.indent.hanging_pt.or(label_hanging).unwrap_or(0.0);
    let first_line = eff_para.indent.first_line_pt.unwrap_or(0.0);
    let first_offset = (first_line - hanging).max(-left);

    let content_first = (avail_width_pt - left - right - first_offset).max(1.0);
    let content_rest = (avail_width_pt - left - right).max(1.0);
    let atoms = flatten(doc, warnings, para, rels, field_ctx)?;
    let text: String = atoms
        .iter()
        .filter_map(|a| match a {
            Atom::Glyph { ch: '\0', .. } => None,
            Atom::Glyph { ch, .. } => Some(*ch),
            _ => None,
        })
        .collect();

    let rtl = eff_para.bidi == Some(true);
    let alignment = eff_para
        .alignment
        .unwrap_or(if rtl { Jc::End } else { Jc::Start });
    let line_spacing = eff_para.spacing.line;
    let default_tab = doc.settings.default_tab_stop_pt;
    let mut tab_stops = eff_para.tabs.clone();
    tab_stops.sort_by(|a, b| {
        a.pos_pt
            .partial_cmp(&b.pos_pt)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let nominal_natural_height = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Glyph { style, .. } => Some(style.line_height_pt),
            _ => None,
        })
        .chain(label.as_ref().map(|value| value.line_height_pt))
        .fold(0.0_f64, f64::max)
        .max(para_mark_height(doc, para, false));
    let nominal_line_height = apply_line_spacing(line_spacing, nominal_natural_height).max(0.01);
    let nominal_baseline_offset = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Glyph { style, .. } => Some(style.ascent_pt),
            _ => None,
        })
        .chain(label.as_ref().map(|value| value.ascent_pt))
        .fold(0.0_f64, f64::max);
    let nominal_baseline_offset = if nominal_baseline_offset > 0.0 {
        nominal_baseline_offset.min(nominal_line_height)
    } else {
        nominal_line_height * 0.8
    };

    let anchored: Vec<(f64, f64, AnchorProps)> = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Composite {
                w_pt,
                h_pt,
                anchor: Some(anchor),
                ..
            } if anchor.relative_from_v == AnchorOrigin::Paragraph => Some((*w_pt, *h_pt, *anchor)),
            _ => None,
        })
        .collect();
    if anchored.len() > 1 {
        return Err(OfficeError::unsupported(
            UnsupportedCode::AnchoredDrawing,
            "multiple paragraph-relative drawings in one paragraph",
        ));
    }

    let page_floats = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Composite {
                w_pt,
                h_pt,
                items,
                anchor: Some(anchor),
                ..
            } if anchor.relative_from_v != AnchorOrigin::Paragraph => Some(PageFloat {
                items: items
                    .iter()
                    .map(|item| LineItem {
                        x_pt: item.x_pt,
                        y_pt: item.y_pt,
                        item: item.item.clone(),
                    })
                    .collect(),
                w_pt: *w_pt,
                h_pt: *h_pt,
                x_origin: anchor.relative_from_h,
                x_pt: anchor.x_pt,
                x_align: anchor.x_align,
                y_origin: anchor.relative_from_v,
                y_pt: anchor.y_pt,
                y_align: anchor.y_align,
                wrap: anchor.wrap,
                distances_pt: anchor.distances_pt,
                behind_doc: anchor.behind_doc,
                relative_height: anchor.relative_height,
            }),
            _ => None,
        })
        .collect();

    let mut wrap_band = incoming_wrap
        .map(|wrap| WrapBand {
            left: wrap
                .left_band
                .filter(|band| band.remaining_pt > 0.01)
                .map(|band| EdgeWrapBand {
                    top_pt: band.top_remaining_pt.max(0.0),
                    bottom_pt: band.remaining_pt,
                    inset_pt: band.inset_pt,
                    uses_text_baseline: band.uses_text_baseline,
                }),
            right: wrap
                .right_band
                .filter(|band| band.remaining_pt > 0.01)
                .map(|band| EdgeWrapBand {
                    top_pt: band.top_remaining_pt.max(0.0),
                    bottom_pt: band.remaining_pt,
                    inset_pt: band.inset_pt,
                    uses_text_baseline: band.uses_text_baseline,
                }),
        })
        .filter(|wrap| wrap.left.is_some() || wrap.right.is_some());
    let mut full_width_band = wrap_band.as_mut().and_then(|band| {
        if band.right.is_none()
            && band
                .left
                .is_some_and(|left| left.inset_pt >= avail_width_pt - 0.01)
        {
            band.left.take()
        } else {
            None
        }
    });
    if wrap_band.is_some_and(|band| band.left.is_none() && band.right.is_none()) {
        wrap_band = None;
    }
    let mut top_bottom_reserve = None;
    let mut float_leading_pt = 0.0;
    if let Some((w_pt, h_pt, anchor)) = anchored.first().copied() {
        match anchor.wrap {
            WrapMode::Square | WrapMode::Tight => {
                if anchor.wrap == WrapMode::Square {
                    float_leading_pt = nominal_line_height;
                }
                if wrap_band.is_some() {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        "overlapping anchored drawing exclusions",
                    ));
                }
                let x_pt = match anchor.x_align {
                    Some(crate::oxml::wml::FrameAlign::Start) => 0.0,
                    Some(crate::oxml::wml::FrameAlign::Center) => {
                        (avail_width_pt - w_pt).max(0.0) / 2.0
                    }
                    Some(crate::oxml::wml::FrameAlign::End) => (avail_width_pt - w_pt).max(0.0),
                    None => anchor.x_pt,
                };
                let is_left_edge = x_pt <= 0.01;
                let is_right_edge = x_pt + w_pt >= avail_width_pt - 0.01;
                let [dist_top, dist_bottom, dist_left, dist_right] = anchor.distances_pt;
                let left_available = (x_pt - dist_left).max(0.0);
                let right_start = (x_pt + w_pt + dist_right).max(0.0);
                let right_available = (avail_width_pt - right_start).max(0.0);
                let use_right_interval =
                    !is_right_edge && (is_left_edge || right_available >= left_available);
                let anchor_y_for_wrap = anchor.y_pt
                    - if anchor.wrap == WrapMode::Square {
                        nominal_line_height
                    } else {
                        0.0
                    };
                let edge = EdgeWrapBand {
                    top_pt: (anchor_y_for_wrap - dist_top).max(0.0),
                    bottom_pt: (anchor_y_for_wrap + h_pt + dist_bottom).max(0.0),
                    inset_pt: if use_right_interval {
                        right_start
                    } else {
                        (avail_width_pt - left_available).max(0.0)
                    },
                    uses_text_baseline: anchor.wrap == WrapMode::Tight,
                };
                wrap_band = Some(if use_right_interval {
                    WrapBand {
                        left: Some(edge),
                        right: None,
                    }
                } else {
                    WrapBand {
                        left: None,
                        right: Some(edge),
                    }
                });
            }
            WrapMode::TopAndBottom => {
                top_bottom_reserve = Some((anchor.y_pt + h_pt + anchor.distances_pt[1]).max(0.0));
            }
            WrapMode::None => {}
        }
    }

    let constrained_width = |line_index: usize, base_width: f64, base_x: f64| {
        let left_inset = wrap_band
            .and_then(|band| band.left)
            .filter(|band| {
                band.applies_to_line(line_index, nominal_line_height, nominal_baseline_offset)
            })
            .map_or(0.0, |band| band.inset_pt);
        let right_inset = wrap_band
            .and_then(|band| band.right)
            .filter(|band| {
                band.applies_to_line(line_index, nominal_line_height, nominal_baseline_offset)
            })
            .map_or(0.0, |band| band.inset_pt);
        let left_shift = (left_inset - base_x).max(0.0);
        let right_cut = (right_inset - right).max(0.0);
        (
            (base_width - left_shift - right_cut).max(1.0),
            base_x + left_shift,
        )
    };

    let broken = break_lines(
        &atoms,
        content_first,
        content_rest,
        default_tab,
        &tab_stops,
        |line_index, base_width| {
            let base_x = left
                + if line_index == 0 {
                    first_offset.max(0.0)
                } else {
                    0.0
                };
            constrained_width(line_index, base_width, base_x).0
        },
    );
    let n = broken.len();
    let mut lines = Vec::new();
    for (i, line_atoms) in broken.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i + 1 == n;
        let base_content_w = if is_first {
            content_first
        } else {
            content_rest
        };
        let initial_x = left + if is_first { first_offset.max(0.0) } else { 0.0 };
        let (content_w, base_x) = constrained_width(i, base_content_w, initial_x);
        let this_label = if is_first { label.as_ref() } else { None };
        lines.push(place_line(
            &atoms,
            line_atoms,
            content_w,
            avail_width_pt,
            base_x,
            left,
            hanging,
            alignment,
            line_spacing,
            is_last,
            default_tab,
            &tab_stops,
            this_label,
            rtl,
        ));
    }

    if lines.is_empty() {
        let h = para_mark_height(doc, para, true);
        lines.push(Line {
            height_pt: apply_line_spacing(line_spacing, h),
            items: Vec::new(),
            anchored_items: Vec::new(),
            page_break_after: false,
            column_break_after: false,
        });
    }

    let mut full_width_after = None;
    if let Some(band) = full_width_band.take() {
        let occupied_before: f64 = lines.iter().map(|line| line.height_pt).sum();
        if occupied_before <= band.top_pt + 0.01 {
            full_width_after = Some(ExclusionBand {
                top_remaining_pt: (band.top_pt - occupied_before).max(0.0),
                remaining_pt: band.bottom_pt - occupied_before,
                inset_pt: band.inset_pt,
                uses_text_baseline: false,
            });
        } else if band.top_pt <= 0.01 {
            float_leading_pt = float_leading_pt.max(band.bottom_pt.max(0.0));
        } else {
            let mut before = 0.0;
            let mut lines_before = 0usize;
            for line in &lines {
                if before + line.height_pt > band.top_pt + 0.01 {
                    break;
                }
                before += line.height_pt;
                lines_before += 1;
            }
            if lines_before == 0 {
                float_leading_pt = float_leading_pt.max(band.bottom_pt.max(0.0));
            } else if let Some(previous) = lines.get_mut(lines_before - 1) {
                previous.height_pt += (band.bottom_pt - before).max(0.0);
            }
        }
    }

    if let Some(reserve_bottom) = top_bottom_reserve {
        let occupied: f64 = lines.iter().map(|line| line.height_pt).sum();
        if reserve_bottom > occupied + 0.01
            && let Some(last_line) = lines.last_mut()
        {
            last_line.height_pt += reserve_bottom - occupied;
        }
    }

    if let Some(border) = eff_para
        .borders
        .bottom
        .as_ref()
        .filter(|border| !matches!(border.val.as_str(), "nil" | "none"))
        && let Some(last_line) = lines.last_mut()
    {
        let width_pt = border.size_pt.max(0.25);
        let border_y = last_line.height_pt + border.space_pt + width_pt / 2.0;
        last_line.items.push(LineItem {
            x_pt: left,
            y_pt: border_y,
            item: Item::Line {
                dx_pt: (avail_width_pt - left - right_indent).max(0.0),
                dy_pt: 0.0,
                stroke: Stroke {
                    width_pt,
                    color: Rgba::opaque(
                        border
                            .resolved_color(&doc.theme)
                            .unwrap_or(Rgb::new(0, 0, 0)),
                    ),
                    dash: None,
                },
            },
        });
        last_line.height_pt += border.space_pt + width_pt;
    }

    if let Some(border) = right_border {
        let paragraph_height: f64 = lines.iter().map(|line| line.height_pt).sum();
        let width_pt = border.size_pt;
        let border_x = (avail_width_pt - right_indent - width_pt / 2.0).max(0.0);
        if let Some(first_line) = lines.first_mut() {
            first_line.items.push(LineItem {
                x_pt: border_x,
                y_pt: 0.0,
                item: Item::Line {
                    dx_pt: 0.0,
                    dy_pt: paragraph_height,
                    stroke: Stroke {
                        width_pt,
                        color: Rgba::opaque(
                            border
                                .resolved_color(&doc.theme)
                                .unwrap_or(Rgb::new(0, 0, 0)),
                        ),
                        dash: None,
                    },
                },
            });
        }
    }

    let occupied_height: f64 = lines.iter().map(|line| line.height_pt).sum();
    let wrap_after = wrap_band
        .and_then(|band| {
            let carry = |edge: Option<EdgeWrapBand>| {
                edge.and_then(|edge| {
                    let remaining_pt = edge.bottom_pt - occupied_height;
                    (remaining_pt > 0.01).then_some(ExclusionBand {
                        top_remaining_pt: (edge.top_pt - occupied_height).max(0.0),
                        remaining_pt,
                        inset_pt: edge.inset_pt,
                        uses_text_baseline: edge.uses_text_baseline,
                    })
                })
            };
            let exclusion = WrapExclusion {
                left_band: full_width_after.or_else(|| carry(band.left)),
                right_band: carry(band.right),
            };
            (exclusion.left_band.is_some() || exclusion.right_band.is_some()).then_some(exclusion)
        })
        .or_else(|| {
            full_width_after.map(|left_band| WrapExclusion {
                left_band: Some(left_band),
                right_band: None,
            })
        });

    Ok(ParaLayout {
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
        note_refs: paragraph_note_refs(para),
        wrap_after,
        float_leading_pt,
        page_floats,
    })
}

fn paragraph_note_refs(para: &Paragraph) -> Vec<(NoteKind, i64)> {
    let mut refs = Vec::new();
    let mut scan_run = |run: &Run| {
        for content in &run.content {
            if let RunContent::NoteReference { kind, id } = content {
                refs.push((*kind, *id));
            }
        }
    };
    fn scan_inlines(inlines: &[InlineContent], scan_run: &mut impl FnMut(&Run)) {
        for inline in inlines {
            match inline {
                InlineContent::Run(run) => scan_run(run),
                InlineContent::Hyperlink { runs, .. } => runs.iter().for_each(&mut *scan_run),
                InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                    scan_inlines(&field.content, scan_run);
                }
                InlineContent::Math(_) => {}
            }
        }
    }
    scan_inlines(&para.runs, &mut scan_run);
    refs
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
    let family = (props.rtl == Some(true))
        .then(|| props.fonts.cs.clone())
        .flatten()
        .or_else(|| props.fonts.ascii.clone())
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
) -> Result<Vec<Atom>> {
    let mut atoms = Vec::new();
    flatten_inlines(doc, warnings, para, &para.runs, rels, field_ctx, &mut atoms)?;
    Ok(atoms)
}

fn flatten_inlines(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    para: &Paragraph,
    inlines: &[InlineContent],
    rels: &HashMap<String, String>,
    field_ctx: Option<FieldContext>,
    atoms: &mut Vec<Atom>,
) -> Result<()> {
    for inline in inlines {
        match inline {
            InlineContent::Run(r) => flatten_run(doc, warnings, para, r, None, rels, atoms)?,
            InlineContent::Hyperlink {
                rel_id,
                anchor,
                runs,
            } => {
                let target = link_target(doc, rels, rel_id.as_deref(), anchor.as_deref());
                for r in runs {
                    flatten_run(doc, warnings, para, r, target.clone(), rels, atoms)?;
                }
            }
            InlineContent::SimpleField(f) | InlineContent::ComplexFieldRegion(f) => {
                if let Some(run) = substituted_field_run(f, field_ctx.clone()) {
                    flatten_run(doc, warnings, para, &run, None, rels, atoms)?;
                } else {
                    flatten_inlines(
                        doc,
                        warnings,
                        para,
                        &f.content,
                        rels,
                        field_ctx.clone(),
                        atoms,
                    )?;
                }
            }
            InlineContent::Math(math) => push_math_atom(doc, math, atoms)?,
        }
    }
    Ok(())
}

/// Resolves PAGE/NUMPAGES to a substituted run when a field context is present.
/// Every other field renders its cached inline content directly.
fn substituted_field_run(field: &Field, ctx: Option<FieldContext>) -> Option<Run> {
    let first = field
        .instruction
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let ctx = ctx?;
    let text = match first.as_str() {
        "PAGE" => ctx.page_label,
        "NUMPAGES" => ctx.total.to_string(),
        _ => return None,
    };
    let props = first_field_run(&field.content)
        .map(|run| run.props.clone())
        .unwrap_or_default();
    Some(Run {
        props,
        content: vec![RunContent::Text(text)],
    })
}

fn first_field_run(inlines: &[InlineContent]) -> Option<&Run> {
    for inline in inlines {
        match inline {
            InlineContent::Run(run) => return Some(run),
            InlineContent::Hyperlink { runs, .. } => {
                if let Some(run) = runs.first() {
                    return Some(run);
                }
            }
            InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                if let Some(run) = first_field_run(&field.content) {
                    return Some(run);
                }
            }
            InlineContent::Math(_) => {}
        }
    }
    None
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
) -> Result<()> {
    let eff = doc.styles.run_props(&para.props, &run.props);
    if eff.vanish == Some(true) {
        return Ok(());
    }
    let Some(face) = resolve_face(doc, warnings, &eff) else {
        return Ok(());
    };
    let Some(font_face) = doc.fonts.face(face) else {
        return Ok(());
    };
    let Some(parsed) = font_face.parsed() else {
        return Ok(());
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
    // Word only kerns runs carrying `w:kern` whose size clears the threshold; it is
    // off by default. Kerning unconditionally narrows text and shifts line breaks.
    let kern = eff.kern_min_pt.is_some_and(|min| size >= min);
    let base_direction =
        if eff.rtl == Some(true) || doc.styles.para_props(&para.props).bidi == Some(true) {
            text_core::Direction::Rtl
        } else {
            text_core::Direction::Ltr
        };
    let allow_direct_text = matches!(base_direction, text_core::Direction::Ltr);
    let fallback_ids = matching::coverage_fallbacks(&doc.fonts, face, weight(&eff), italic(&eff));
    for content in &run.content {
        match content {
            RunContent::Text(t) => shape_text(
                &doc.fonts,
                &fallback_ids,
                t,
                size,
                char_spacing,
                kern,
                allow_direct_text,
                base_direction,
                &style,
                atoms,
            )?,
            RunContent::Tab => atoms.push(Atom::Tab),
            RunContent::Break(k) => atoms.push(Atom::Break(*k)),
            RunContent::Symbol { font, char_code } => {
                if let Some(ch) = matching::map_symbol_char(font, *char_code) {
                    shape_text(
                        &doc.fonts,
                        &fallback_ids,
                        &ch.to_string(),
                        size,
                        char_spacing,
                        kern,
                        allow_direct_text,
                        base_direction,
                        &style,
                        atoms,
                    )?;
                }
            }
            RunContent::Drawing(d) => {
                if let Some(detail) = d.anchor_violations.first() {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        detail.clone(),
                    ));
                }
                if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_PICTURE)
                    && !d
                        .drawing
                        .extent_pt
                        .is_some_and(|(width, height)| width > 0.0 && height > 0.0)
                {
                    return Err(crate::OfficeError::unsupported(
                        crate::UnsupportedCode::MissingRelationshipTarget,
                        "picture drawing has no positive extent",
                    ));
                }
                if let Some(text_box) = &d.text_box {
                    atoms.push(Atom::Composite {
                        w_pt: text_box.width_pt,
                        h_pt: text_box.height_pt,
                        baseline_pt: None,
                        items: text_box_items(doc, warnings, text_box, rels)?,
                        anchor: d.anchor,
                    });
                } else if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_DIAGRAM)
                    && let Some((w, h)) = d.drawing.extent_pt
                    && let Some(rel_id) = &d.drawing.graphic_rel_id
                    && let Some(diagram) = doc.diagram(rels, rel_id)
                {
                    atoms.push(Atom::Composite {
                        w_pt: w,
                        h_pt: h,
                        baseline_pt: None,
                        items: diagram
                            .render(&doc.fonts, &doc.theme, w, h)?
                            .into_iter()
                            .map(|item| LineItem {
                                x_pt: item.x_pt,
                                y_pt: item.y_pt,
                                item: item.item,
                            })
                            .collect(),
                        anchor: d.anchor,
                    });
                } else if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_CHART)
                    && let Some((w, h)) = d.drawing.extent_pt
                    && let Some(rel_id) = &d.drawing.graphic_rel_id
                    && let Some(chart) = doc.chart(rels, rel_id)
                {
                    atoms.push(Atom::Composite {
                        w_pt: w,
                        h_pt: h,
                        baseline_pt: None,
                        items: chart
                            .render(&doc.fonts, &doc.theme, w, h)?
                            .into_iter()
                            .map(|item| LineItem {
                                x_pt: item.x_pt,
                                y_pt: item.y_pt,
                                item: item.item,
                            })
                            .collect(),
                        anchor: d.anchor,
                    });
                } else if let Some((w, h)) = d.drawing.extent_pt
                    && let Some(rel_id) = &d.drawing.blip_rel_id
                {
                    let image_id = doc.image_id(rels, rel_id).ok_or_else(|| {
                        crate::OfficeError::unsupported(
                            crate::UnsupportedCode::MissingRelationshipTarget,
                            format!("image relationship {rel_id} or its target is missing"),
                        )
                    })?;
                    if d.anchor.is_some() || d.crop.is_some() {
                        let image = if let Some(crop) = d.crop {
                            crate::drawing::image::validate_crop_fractions(crop)?;
                            Item::CroppedImage {
                                w_pt: w,
                                h_pt: h,
                                image_id,
                                crop,
                            }
                        } else {
                            Item::Image {
                                w_pt: w,
                                h_pt: h,
                                image_id,
                            }
                        };
                        atoms.push(Atom::Composite {
                            w_pt: w,
                            h_pt: h,
                            baseline_pt: None,
                            items: vec![LineItem {
                                x_pt: 0.0,
                                y_pt: 0.0,
                                item: image,
                            }],
                            anchor: d.anchor,
                        });
                    } else {
                        atoms.push(Atom::Image {
                            image_id,
                            w_pt: w,
                            h_pt: h,
                            link: style.link.clone(),
                        });
                    }
                } else if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_PICTURE) {
                    return Err(crate::OfficeError::unsupported(
                        crate::UnsupportedCode::MissingRelationshipTarget,
                        "picture drawing has no image relationship or extent",
                    ));
                }
            }
            RunContent::VmlShape(shape) => {
                atoms.push(Atom::Composite {
                    w_pt: shape.width_pt,
                    h_pt: shape.height_pt,
                    baseline_pt: None,
                    items: vml_items(doc, warnings, shape, rels)?,
                    anchor: Some(AnchorProps {
                        x_pt: shape.x_pt,
                        x_align: None,
                        y_pt: shape.y_pt,
                        y_align: None,
                        relative_from_h: crate::oxml::wml::AnchorOrigin::Paragraph,
                        relative_from_v: crate::oxml::wml::AnchorOrigin::Paragraph,
                        wrap: crate::oxml::wml::WrapMode::None,
                        distances_pt: [0.0; 4],
                        behind_doc: false,
                        relative_height: 0,
                    }),
                });
            }
            RunContent::OlePreview(shape) => {
                let rel_id = shape.image_rel_id.as_deref().ok_or_else(|| {
                    crate::OfficeError::unsupported(
                        crate::UnsupportedCode::OleObject,
                        "OLE object has no preview relationship",
                    )
                })?;
                let image_id = doc.image_id(rels, rel_id).ok_or_else(|| {
                    crate::OfficeError::unsupported(
                        crate::UnsupportedCode::MissingRelationshipTarget,
                        format!("OLE raster preview relationship {rel_id} is unavailable"),
                    )
                })?;
                atoms.push(Atom::Composite {
                    w_pt: shape.width_pt,
                    h_pt: shape.height_pt,
                    baseline_pt: None,
                    items: vec![LineItem {
                        x_pt: 0.0,
                        y_pt: 0.0,
                        item: Item::Image {
                            w_pt: shape.width_pt,
                            h_pt: shape.height_pt,
                            image_id,
                        },
                    }],
                    anchor: None,
                });
            }
            RunContent::Math(math) => push_math_atom(doc, math, atoms)?,
            RunContent::NoteReference { id, .. } => {
                let note_size = size * 0.75;
                let mut note_style = (*style).clone();
                note_style.size_pt = note_size;
                note_style.rise_pt += size * 0.35;
                note_style.line_height_pt = note_style.line_height_pt.max(size);
                shape_text(
                    &doc.fonts,
                    &fallback_ids,
                    &id.to_string(),
                    note_size,
                    char_spacing,
                    kern,
                    allow_direct_text,
                    base_direction,
                    &Rc::new(note_style),
                    atoms,
                )?;
            }
        }
    }
    Ok(())
}

fn push_math_atom(
    doc: &WordDoc,
    math: &crate::oxml::omml::MathContent,
    atoms: &mut Vec<Atom>,
) -> Result<()> {
    let equation = super::math::layout_equation(doc, math, DEFAULT_SIZE_PT)?;
    atoms.push(Atom::Composite {
        w_pt: equation.width,
        h_pt: equation.height,
        baseline_pt: Some(equation.baseline),
        items: equation.items,
        anchor: None,
    });
    Ok(())
}

fn text_box_items(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    text_box: &TextBox,
    rels: &HashMap<String, String>,
) -> Result<Vec<LineItem>> {
    let mut items = vec![LineItem {
        x_pt: 0.0,
        y_pt: 0.0,
        item: Item::Rect {
            w_pt: text_box.width_pt,
            h_pt: text_box.height_pt,
            fill: text_box.fill.map(Rgba::opaque),
            stroke: text_box.outline.map(|color| Stroke {
                width_pt: 0.75,
                color: Rgba::opaque(color),
                dash: None,
            }),
        },
    }];
    let mut y = 4.0;
    let width = (text_box.width_pt - 8.0).max(1.0);
    let mut counters = numbering_layout::ListCounters::default();
    for block in &text_box.content {
        match block {
            crate::oxml::wml::BlockContent::Paragraph(paragraph) => {
                let layout = layout_paragraph(doc, warnings, paragraph, rels, width, None, None)?;
                if layout
                    .lines
                    .iter()
                    .any(|line| !line.anchored_items.is_empty())
                {
                    return Err(OfficeError::unsupported(
                        UnsupportedCode::AnchoredDrawing,
                        "anchored drawing in a text box",
                    ));
                }
                y += layout.space_before_pt;
                for line in layout.lines {
                    for item in line.items {
                        items.push(LineItem {
                            x_pt: 4.0 + item.x_pt,
                            y_pt: y + item.y_pt,
                            item: item.item,
                        });
                    }
                    y += line.height_pt;
                }
                y += layout.space_after_pt;
            }
            crate::oxml::wml::BlockContent::Table(table) => {
                for row in super::table::layout_table(
                    doc,
                    warnings,
                    table,
                    rels,
                    width,
                    0.0,
                    &mut counters,
                )?
                .rows
                {
                    for item in row.items {
                        items.push(LineItem {
                            x_pt: 4.0 + item.x_pt,
                            y_pt: y + item.y_pt,
                            item: item.item,
                        });
                    }
                    y += row.height_pt;
                }
            }
        }
    }
    Ok(items)
}

fn vml_items(
    doc: &WordDoc,
    warnings: &mut Vec<Warning>,
    shape: &VmlShape,
    rels: &HashMap<String, String>,
) -> Result<Vec<LineItem>> {
    let color = Rgba::opaque(shape.outline.unwrap_or(Rgb::new(0, 0, 0)));
    let stroke = Stroke {
        width_pt: 0.75,
        color,
        dash: None,
    };
    let mut items = Vec::new();
    match shape.kind.as_str() {
        "line" => items.push(LineItem {
            x_pt: 0.0,
            y_pt: 0.0,
            item: Item::Line {
                dx_pt: shape.width_pt,
                dy_pt: shape.height_pt,
                stroke: stroke.clone(),
            },
        }),
        "oval" => {
            let segments = 24;
            for index in 0..segments {
                let a = std::f64::consts::TAU * index as f64 / segments as f64;
                let b = std::f64::consts::TAU * (index + 1) as f64 / segments as f64;
                let x1 = shape.width_pt * (0.5 + 0.5 * a.cos());
                let y1 = shape.height_pt * (0.5 + 0.5 * a.sin());
                let x2 = shape.width_pt * (0.5 + 0.5 * b.cos());
                let y2 = shape.height_pt * (0.5 + 0.5 * b.sin());
                items.push(LineItem {
                    x_pt: x1,
                    y_pt: y1,
                    item: Item::Line {
                        dx_pt: x2 - x1,
                        dy_pt: y2 - y1,
                        stroke: stroke.clone(),
                    },
                });
            }
        }
        "shape" if shape.path.is_some() => {
            for (x1, y1, x2, y2) in simple_vml_path(shape.path.as_deref().unwrap_or_default())? {
                items.push(LineItem {
                    x_pt: x1 * shape.width_pt,
                    y_pt: y1 * shape.height_pt,
                    item: Item::Line {
                        dx_pt: (x2 - x1) * shape.width_pt,
                        dy_pt: (y2 - y1) * shape.height_pt,
                        stroke: stroke.clone(),
                    },
                });
            }
        }
        _ => items.push(LineItem {
            x_pt: 0.0,
            y_pt: 0.0,
            item: Item::Rect {
                w_pt: shape.width_pt,
                h_pt: shape.height_pt,
                fill: shape.fill.map(Rgba::opaque),
                stroke: Some(stroke),
            },
        }),
    }
    if let Some(rel_id) = &shape.image_rel_id {
        let image_id = doc.image_id(rels, rel_id).ok_or_else(|| {
            crate::OfficeError::unsupported(
                crate::UnsupportedCode::MissingRelationshipTarget,
                format!("VML image relationship {rel_id} or its target is missing"),
            )
        })?;
        items.push(LineItem {
            x_pt: 0.0,
            y_pt: 0.0,
            item: Item::Image {
                w_pt: shape.width_pt,
                h_pt: shape.height_pt,
                image_id,
            },
        });
    }
    if !shape.text_box.is_empty() {
        items.extend(text_box_items(
            doc,
            warnings,
            &TextBox {
                width_pt: shape.width_pt,
                height_pt: shape.height_pt,
                fill: None,
                outline: None,
                content: shape.text_box.clone(),
            },
            rels,
        )?);
    }
    Ok(items)
}

fn simple_vml_path(path: &str) -> Result<Vec<(f64, f64, f64, f64)>> {
    let normalized = path
        .replace(',', " ")
        .replace('m', " m ")
        .replace('l', " l ")
        .replace('e', " e ");
    let tokens: Vec<_> = normalized.split_whitespace().collect();
    let mut index = 0usize;
    let mut current = (0.0, 0.0);
    let mut segments = Vec::new();
    while index < tokens.len() {
        match tokens[index] {
            "m" => {
                let x = tokens
                    .get(index + 1)
                    .and_then(|value| value.parse::<f64>().ok());
                let y = tokens
                    .get(index + 2)
                    .and_then(|value| value.parse::<f64>().ok());
                let (Some(x), Some(y)) = (x, y) else {
                    break;
                };
                current = (x / 21_600.0, y / 21_600.0);
                index += 3;
            }
            "l" => {
                index += 1;
                while index + 1 < tokens.len() && !matches!(tokens[index], "m" | "l" | "e") {
                    let Some(x) = tokens[index].parse::<f64>().ok() else {
                        break;
                    };
                    let Some(y) = tokens[index + 1].parse::<f64>().ok() else {
                        break;
                    };
                    let next = (x / 21_600.0, y / 21_600.0);
                    segments.push((current.0, current.1, next.0, next.1));
                    current = next;
                    index += 2;
                }
            }
            "e" => break,
            _ => index += 1,
        }
    }
    if segments.is_empty() {
        return Err(crate::OfficeError::unsupported(
            crate::UnsupportedCode::UnknownElement,
            "v:shape path",
        ));
    }
    Ok(segments)
}

#[expect(
    clippy::too_many_arguments,
    reason = "shape inputs mirror resolved Word run properties"
)]
fn shape_text(
    fonts: &FontDb,
    face_ids: &[FaceId],
    text: &str,
    size: f64,
    char_spacing: f64,
    kern: bool,
    allow_direct_text: bool,
    base_direction: text_core::Direction,
    style: &Rc<RunStyle>,
    atoms: &mut Vec<Atom>,
) -> Result<()> {
    let faces: Vec<_> = face_ids.iter().filter_map(|id| fonts.face(*id)).collect();
    let shaped = shape::require_glyphs(shape::shape_with_fallback(
        text,
        &faces,
        size,
        char_spacing,
        kern,
    ))?;
    let styles: Vec<Rc<RunStyle>> = face_ids
        .iter()
        .map(|face_id| fallback_style(fonts, *face_id, style))
        .collect();
    let mapped = crate::text::semantic::map_shaped_run_clusters_with_direct(
        text,
        &shaped,
        allow_direct_text,
    );
    let semantic_style = styles
        .get(mapped.owner_face_index)
        .cloned()
        .unwrap_or_else(|| Rc::clone(style));
    let group_id = atoms.len();
    let mut semantic_by_owner: HashMap<usize, Vec<AtomSemanticCluster>> = HashMap::new();
    for cluster in mapped.semantic_clusters {
        semantic_by_owner
            .entry(cluster.owner_visible_index)
            .or_default()
            .push(AtomSemanticCluster {
                group_id,
                source_order: cluster.source_offset,
                style: Rc::clone(&semantic_style),
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
        atoms.push(Atom::Glyph {
            gid: visible.gid,
            advance_pt: visible.advance_pt,
            x_offset_pt: visible.x_offset_pt,
            y_offset_pt: visible.y_offset_pt,
            ch: visible.ch,
            is_space,
            style: styles
                .get(g.face_index)
                .cloned()
                .unwrap_or_else(|| Rc::clone(style)),
            semantic: semantic_by_owner.remove(&visible_index).unwrap_or_default(),
        });
    }
    Ok(())
}

fn weight(props: &RunProps) -> u16 {
    if props.bold == Some(true) { 700 } else { 400 }
}

fn italic(props: &RunProps) -> bool {
    props.italic == Some(true)
}

fn fallback_style(fonts: &FontDb, face: FaceId, base: &RunStyle) -> Rc<RunStyle> {
    let metrics = fonts
        .face(face)
        .and_then(|font| font.parsed())
        .map(FaceMetrics::new);
    Rc::new(RunStyle {
        face,
        size_pt: base.size_pt,
        color: base.color,
        underline: base.underline,
        strike: base.strike,
        rise_pt: base.rise_pt,
        ascent_pt: metrics
            .as_ref()
            .map_or(base.ascent_pt, |m| m.ascent_pt(base.size_pt)),
        line_height_pt: metrics
            .as_ref()
            .map_or(base.line_height_pt, |m| m.line_height_pt(base.size_pt)),
        link: base.link.clone(),
    })
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
    tab_stops: &[TabStop],
    width_for_line: impl Fn(usize, f64) -> f64,
) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut width_used = 0.0_f64;
    let mut last_break: Option<usize> = None;

    let width_of = |line_index: usize| {
        let base = if line_index == 0 {
            width_first
        } else {
            width_rest
        };
        width_for_line(line_index, base)
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
        let avail = width_of(lines.len());
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
fn next_tab_stop(x: f64, default_tab: f64, tab_stops: &[TabStop]) -> f64 {
    for stop in tab_stops {
        if stop.alignment != TabAlign::Clear && stop.pos_pt > x + 0.01 {
            return stop.pos_pt;
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
    anchor_width_pt: f64,
    base_x: f64,
    left: f64,
    hanging: f64,
    alignment: Jc,
    line_spacing: Option<LineSpacing>,
    is_last: bool,
    default_tab: f64,
    tab_stops: &[TabStop],
    label: Option<&PlacedLabel>,
    rtl: bool,
) -> Line {
    let visual_indices = visual_run_order(atoms, indices, rtl);
    let indices = visual_indices.as_slice();
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
            Atom::Composite { w_pt, anchor, .. } => {
                if anchor.is_none() {
                    natural += w_pt;
                }
            }
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
    let mut text_natural_h = 0.0_f64;
    let mut ascent = 0.0_f64;
    let mut descent = 0.0_f64;
    if let Some(l) = label {
        natural_h = natural_h.max(l.line_height_pt);
        text_natural_h = text_natural_h.max(l.line_height_pt);
        ascent = ascent.max(l.ascent_pt);
        descent = descent.max((l.line_height_pt - l.ascent_pt).max(0.0));
    }
    for &k in indices {
        match &atoms[k] {
            Atom::Glyph { style, .. } => {
                natural_h = natural_h.max(style.line_height_pt);
                text_natural_h = text_natural_h.max(style.line_height_pt);
                ascent = ascent.max(style.ascent_pt);
                descent = descent.max((style.line_height_pt - style.ascent_pt).max(0.0));
            }
            Atom::Image { h_pt, .. } => {
                natural_h = natural_h.max(*h_pt);
                ascent = ascent.max(*h_pt);
            }
            Atom::Composite {
                h_pt,
                baseline_pt,
                anchor,
                ..
            } if anchor.is_none() => {
                natural_h = natural_h.max(*h_pt);
                let baseline = baseline_pt.unwrap_or(*h_pt).clamp(0.0, *h_pt);
                ascent = ascent.max(baseline);
                descent = descent.max(*h_pt - baseline);
            }
            _ => {}
        }
    }
    if natural_h <= 0.0 {
        natural_h = DEFAULT_SIZE_PT * 1.2;
        ascent = DEFAULT_SIZE_PT;
    }
    if text_natural_h <= 0.0 {
        text_natural_h = DEFAULT_SIZE_PT * 1.2;
    }
    natural_h = natural_h.max(ascent + descent);
    let height = match line_spacing {
        Some(LineSpacing::Auto(_)) => {
            natural_h.max(apply_line_spacing(line_spacing, text_natural_h))
        }
        _ => apply_line_spacing(line_spacing, natural_h),
    };

    let mut items: Vec<LineItem> = Vec::new();
    // Semantic text represents complex visible glyphs in logical source order.
    // Prefer it on an equal source key so a complex cluster at the start of a
    // same-style glyph run is exposed before later directly mapped characters.
    let mut text_items: Vec<(usize, u8, LineItem)> = Vec::new();
    let mut anchored_items: Vec<AnchoredLineItem> = Vec::new();

    // Numbering label at (left - hanging).
    if let Some(l) = label {
        let label_width: f64 = l
            .runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .map(|glyph| glyph.advance_pt)
            .sum();
        let area_left = (left - hanging).max(0.0);
        let label_x = match l.justification {
            LabelJustification::Start => area_left,
            LabelJustification::Center => area_left + (hanging - label_width).max(0.0) / 2.0,
            LabelJustification::End => (left - label_width).max(area_left),
        };
        let mut run_x = label_x;
        for run in &l.runs {
            text_items.push((
                0,
                1,
                LineItem {
                    x_pt: run_x,
                    y_pt: ascent,
                    item: Item::Glyphs(run.clone()),
                },
            ));
            run_x += run.glyphs.iter().map(|glyph| glyph.advance_pt).sum::<f64>();
        }
    }

    let mut x = base_x + start_shift;
    let mut pending: Vec<PlacedGlyph> = Vec::new();
    let mut pending_style: Option<Rc<RunStyle>> = None;
    let mut pending_order: Option<usize> = None;
    let mut pending_x = x;
    let mut link_start: Option<(f64, LinkTarget)> = None;
    let mut line_semantics: BTreeMap<usize, Vec<(f64, AtomSemanticCluster)>> = BTreeMap::new();

    let flush = |pending: &mut Vec<PlacedGlyph>,
                 pending_style: &mut Option<Rc<RunStyle>>,
                 pending_order: &mut Option<usize>,
                 pending_x: f64,
                 text_items: &mut Vec<(usize, u8, LineItem)>| {
        if let Some(st) = pending_style.take()
            && !pending.is_empty()
        {
            text_items.push((
                pending_order.take().unwrap_or(usize::MAX),
                1,
                LineItem {
                    x_pt: pending_x,
                    y_pt: ascent - st.rise_pt,
                    item: Item::Glyphs(build_run(&st, std::mem::take(pending), rtl)),
                },
            ));
        }
    };

    for (visual_position, &k) in indices.iter().enumerate() {
        match &atoms[k] {
            Atom::Glyph {
                gid,
                advance_pt,
                x_offset_pt,
                y_offset_pt,
                ch,
                is_space,
                style,
                semantic,
            } => {
                let same = pending_style
                    .as_ref()
                    .map(|s| Rc::ptr_eq(s, style))
                    .unwrap_or(false);
                if !same {
                    flush(
                        &mut pending,
                        &mut pending_style,
                        &mut pending_order,
                        pending_x,
                        &mut text_items,
                    );
                    pending_style = Some(Rc::clone(style));
                    pending_order = Some(k.saturating_add(1));
                    pending_x = x;
                } else {
                    pending_order =
                        Some(pending_order.unwrap_or(usize::MAX).min(k.saturating_add(1)));
                }
                update_link(&mut link_start, style.link.as_ref(), x, height, &mut items);
                let adv = advance_pt + if *is_space { space_extra } else { 0.0 };
                let advance_end = x + adv;
                for owned in semantic {
                    let mut owned = owned.clone();
                    let semantic_advance =
                        owned.cluster.advance_pt + if *is_space { space_extra } else { 0.0 };
                    owned.cluster.advance_pt = semantic_advance;
                    line_semantics
                        .entry(owned.group_id)
                        .or_default()
                        .push((advance_end - semantic_advance, owned));
                }
                pending.push(PlacedGlyph {
                    gid: *gid,
                    advance_pt: adv,
                    x_offset_pt: *x_offset_pt,
                    y_offset_pt: *y_offset_pt,
                    ch: *ch,
                });
                x += adv;
            }
            Atom::Tab => {
                flush(
                    &mut pending,
                    &mut pending_style,
                    &mut pending_order,
                    pending_x,
                    &mut text_items,
                );
                let relative_x = x - base_x;
                let target = next_tab_stop(relative_x, default_tab, tab_stops);
                let stop = tab_stops.iter().find(|stop| {
                    stop.alignment != TabAlign::Clear && (stop.pos_pt - target).abs() < 0.01
                });
                let following = &indices[visual_position + 1..];
                let segment: Vec<_> = following
                    .iter()
                    .copied()
                    .take_while(|index| !matches!(atoms[*index], Atom::Tab | Atom::Break(_)))
                    .collect();
                let segment_width: f64 = segment.iter().map(|index| atoms[*index].advance()).sum();
                let decimal_width: f64 = segment
                    .iter()
                    .take_while(|index| !matches!(atoms[**index], Atom::Glyph { ch: '.', .. }))
                    .map(|index| atoms[*index].advance())
                    .sum();
                let aligned = match stop.map(|stop| stop.alignment).unwrap_or(TabAlign::Left) {
                    TabAlign::Center => target - segment_width / 2.0,
                    TabAlign::Right => target - segment_width,
                    TabAlign::Decimal => target - decimal_width,
                    _ => target,
                }
                .max(relative_x);
                if let Some(stop) = stop {
                    if stop.alignment == TabAlign::Bar {
                        items.push(LineItem {
                            x_pt: base_x + target,
                            y_pt: 0.0,
                            item: Item::Line {
                                dx_pt: 0.0,
                                dy_pt: height,
                                stroke: Stroke {
                                    width_pt: 0.5,
                                    color: Rgba::opaque(Rgb::new(0, 0, 0)),
                                    dash: None,
                                },
                            },
                        });
                    }
                    if stop.leader != TabLeader::None && aligned > relative_x {
                        let dash = match stop.leader {
                            TabLeader::Dot => Some(vec![0.5, 2.0]),
                            TabLeader::Hyphen => Some(vec![3.0, 2.0]),
                            TabLeader::Underscore | TabLeader::None => None,
                        };
                        items.push(LineItem {
                            x_pt: x,
                            y_pt: ascent + 1.0,
                            item: Item::Line {
                                dx_pt: aligned - relative_x,
                                dy_pt: 0.0,
                                stroke: Stroke {
                                    width_pt: 0.5,
                                    color: Rgba::opaque(Rgb::new(0, 0, 0)),
                                    dash,
                                },
                            },
                        });
                    }
                }
                x = base_x + aligned;
            }
            Atom::Image {
                image_id,
                w_pt,
                h_pt,
                link,
            } => {
                flush(
                    &mut pending,
                    &mut pending_style,
                    &mut pending_order,
                    pending_x,
                    &mut text_items,
                );
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
            Atom::Composite {
                w_pt,
                h_pt,
                baseline_pt,
                items: nested,
                anchor,
            } => {
                flush(
                    &mut pending,
                    &mut pending_style,
                    &mut pending_order,
                    pending_x,
                    &mut text_items,
                );
                let (origin_x, origin_y) = anchor
                    .map(|position| {
                        let origin_x = match position.x_align {
                            Some(crate::oxml::wml::FrameAlign::Start) => 0.0,
                            Some(crate::oxml::wml::FrameAlign::Center) => {
                                (anchor_width_pt - w_pt).max(0.0) / 2.0
                            }
                            Some(crate::oxml::wml::FrameAlign::End) => {
                                (anchor_width_pt - w_pt).max(0.0)
                            }
                            None => position.x_pt,
                        };
                        let origin_y = match position.y_align {
                            Some(crate::oxml::wml::FrameAlign::Start) => 0.0,
                            Some(crate::oxml::wml::FrameAlign::Center) => {
                                (height - h_pt).max(0.0) / 2.0
                            }
                            Some(crate::oxml::wml::FrameAlign::End) => (height - h_pt).max(0.0),
                            None if position.wrap == WrapMode::Square => position.y_pt - height,
                            None => position.y_pt,
                        };
                        (origin_x, origin_y)
                    })
                    .unwrap_or((
                        x,
                        (ascent - baseline_pt.unwrap_or(*h_pt).clamp(0.0, *h_pt)).max(0.0),
                    ));
                if let Some(position) = anchor
                    && position.relative_from_v == AnchorOrigin::Paragraph
                {
                    anchored_items.extend(nested.iter().map(|nested_item| AnchoredLineItem {
                        x_pt: origin_x + nested_item.x_pt,
                        y_pt: origin_y + nested_item.y_pt,
                        behind_doc: position.behind_doc,
                        relative_height: position.relative_height,
                        item: nested_item.item.clone(),
                    }));
                } else if anchor.is_none() {
                    items.extend(nested.iter().map(|nested_item| LineItem {
                        x_pt: origin_x + nested_item.x_pt,
                        y_pt: origin_y + nested_item.y_pt,
                        item: nested_item.item.clone(),
                    }));
                    x += w_pt;
                }
            }
            Atom::Break(_) => {}
        }
    }
    flush(
        &mut pending,
        &mut pending_style,
        &mut pending_order,
        pending_x,
        &mut text_items,
    );
    update_link(&mut link_start, None, x, height, &mut items);
    for (_, mut semantic) in line_semantics {
        semantic.sort_by_key(|(_, owned)| owned.source_order);
        let Some((_, first)) = semantic.first() else {
            continue;
        };
        let origin_x = semantic
            .iter()
            .map(|(origin, _)| *origin)
            .fold(f64::INFINITY, f64::min);
        let style = Rc::clone(&first.style);
        let base_direction = first.base_direction;
        let group_id = first.group_id;
        let clusters = semantic
            .into_iter()
            .map(|(_, owned)| owned.cluster)
            .collect();
        text_items.push((
            group_id.saturating_add(1),
            0,
            LineItem {
                x_pt: origin_x,
                y_pt: ascent - style.rise_pt,
                item: Item::SemanticText(SemanticRun {
                    face: style.face,
                    size_pt: style.size_pt,
                    base_direction,
                    clusters,
                }),
            },
        ));
    }
    text_items.sort_by_key(|(order, priority, _)| (*order, *priority));
    items.extend(text_items.into_iter().map(|(_, _, item)| item));

    let page_break_after = indices
        .iter()
        .any(|&k| matches!(atoms[k], Atom::Break(BreakKind::Page)));
    let column_break_after = indices
        .iter()
        .any(|&k| matches!(atoms[k], Atom::Break(BreakKind::Column)));
    Line {
        height_pt: height,
        items,
        anchored_items,
        page_break_after,
        column_break_after,
    }
}

fn visual_run_order(atoms: &[Atom], indices: &[usize], rtl: bool) -> Vec<usize> {
    if !rtl {
        return indices.to_vec();
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for index in indices.iter().copied() {
        let same_style = match (groups.last().and_then(|group| group.first()), &atoms[index]) {
            (Some(previous), Atom::Glyph { style, .. }) => matches!(
                &atoms[*previous],
                Atom::Glyph { style: previous_style, .. } if Rc::ptr_eq(previous_style, style)
            ),
            _ => false,
        };
        if same_style {
            groups.last_mut().expect("group exists").push(index);
        } else {
            groups.push(vec![index]);
        }
    }
    groups.reverse();
    groups.into_iter().flatten().collect()
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

fn build_run(style: &RunStyle, glyphs: Vec<PlacedGlyph>, rtl: bool) -> GlyphRun {
    GlyphRun {
        face: style.face,
        size_pt: style.size_pt,
        color: style.color,
        base_direction: Some(if rtl {
            text_core::Direction::Rtl
        } else {
            text_core::Direction::Ltr
        }),
        glyphs,
        underline: style.underline,
        strike: style.strike,
    }
}

fn para_mark_height(doc: &WordDoc, para: &Paragraph, use_mark_rpr: bool) -> f64 {
    let mark = use_mark_rpr
        .then(|| doc.styles.para_props(&para.props).mark_rpr)
        .flatten()
        .unwrap_or_default();
    let eff = doc.styles.run_props(&para.props, &mark);
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

#[cfg(test)]
mod tests {
    use super::EdgeWrapBand;

    #[test]
    fn tight_wrap_releases_a_line_when_its_baseline_clears_the_polygon() {
        let tight = EdgeWrapBand {
            top_pt: 0.0,
            bottom_pt: 31.0,
            inset_pt: 100.0,
            uses_text_baseline: true,
        };
        assert!(tight.applies_to_line(1, 15.0, 11.0));
        assert!(
            !tight.applies_to_line(2, 15.0, 11.0),
            "line-box top overlaps by 1pt, but the text baseline is below the polygon"
        );

        let square = EdgeWrapBand {
            uses_text_baseline: false,
            ..tight
        };
        assert!(
            square.applies_to_line(2, 15.0, 11.0),
            "square wrapping continues to exclude the complete intersecting line box"
        );
    }
}

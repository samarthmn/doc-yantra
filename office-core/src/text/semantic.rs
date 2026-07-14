//! One shaped-cluster → logical-source mapping used by every Office format.
//!
//! Visible glyphs retain the shaper's exact GIDs, advances, and offsets. Simple
//! monotonic one-scalar clusters carry their source scalar directly on the
//! painted glyph. Complex or visually reordered clusters keep a zero-advance
//! semantic sequence that the PDF emitter maps through dedicated CIDs/ToUnicode
//! entries while painting the visible glyphs as artifacts. This avoids scalar
//! truncation, duplicated combining/ligature source text, and NUL extraction for
//! ordinary text.

use super::shape::ShapedRun;
use crate::layout::PlacedGlyph;

/// Start offset recognized by the PDF emitter as a new invisible semantic
/// source sequence rather than a painted glyph.
pub const SEMANTIC_START_Y: f64 = -1.0e100;
/// Continuation offset for subsequent scalars in the current semantic source
/// sequence.
pub const SEMANTIC_CONTINUATION_Y: f64 = -2.0e100;

/// Shared visible and semantic glyph representation for one shaped source run.
#[derive(Debug, Clone, Default)]
pub struct MappedRun {
    /// Paint-preserving glyphs in the shaper's visual order.
    pub visible: Vec<PlacedGlyph>,
    /// Logical cluster strings, encoded once in ascending source order.
    pub semantic: Vec<PlacedGlyph>,
    /// Face index of the semantic owner glyph.
    pub owner_face_index: usize,
    /// Logical clusters attached to the visible glyph that owns their line break.
    pub semantic_clusters: Vec<MappedSemanticCluster>,
    /// Whether each visible glyph owns a single U+0020 cluster.
    pub is_space: Vec<bool>,
}

/// Line-break ownership for one complex shaped cluster.
#[derive(Debug, Clone)]
pub struct MappedSemanticCluster {
    /// Byte offset in the source run, used to restore logical order after RTL placement.
    pub source_offset: usize,
    /// Visible glyph index after which this semantic cluster is owned.
    pub owner_visible_index: usize,
    /// Backing glyph id in `MappedRun::owner_face_index`.
    pub gid: u16,
    /// Complete logical source sequence for the cluster.
    pub source: String,
    /// Shaped physical advance before line justification.
    pub advance_pt: f64,
}

/// Maps every shaped cluster back to its complete logical source slice.
pub fn map_shaped_run(source: &str, shaped: &ShapedRun) -> MappedRun {
    map_shaped_run_with_options(source, shaped, true, true)
}

/// Maps a shaped run while allowing its caller to keep all painted glyphs
/// extraction-empty inside an explicitly bidi container.
pub fn map_shaped_run_with_direct(
    source: &str,
    shaped: &ShapedRun,
    allow_direct: bool,
) -> MappedRun {
    map_shaped_run_with_options(source, shaped, allow_direct, true)
}

/// Maps a shaped run for line-aware layout consumers that use cluster metadata
/// directly and therefore do not need the legacy per-scalar sentinel copy.
pub fn map_shaped_run_clusters_with_direct(
    source: &str,
    shaped: &ShapedRun,
    allow_direct: bool,
) -> MappedRun {
    map_shaped_run_with_options(source, shaped, allow_direct, false)
}

fn map_shaped_run_with_options(
    source: &str,
    shaped: &ShapedRun,
    allow_direct: bool,
    include_legacy_semantic: bool,
) -> MappedRun {
    let mut visible: Vec<PlacedGlyph> = shaped
        .glyphs
        .iter()
        .map(|glyph| PlacedGlyph {
            gid: glyph.id.0,
            advance_pt: glyph.advance_pt,
            x_offset_pt: glyph.x_offset_pt,
            y_offset_pt: glyph.y_offset_pt,
            ch: '\0',
        })
        .collect();
    let mut starts: Vec<usize> = shaped
        .glyphs
        .iter()
        .map(|glyph| glyph.cluster)
        .filter(|offset| *offset <= source.len() && source.is_char_boundary(*offset))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    if starts.first().copied() != Some(0) && !source.is_empty() {
        starts.insert(0, 0);
    }
    let cluster_source = |start: usize| {
        let end = starts
            .iter()
            .copied()
            .find(|candidate| *candidate > start)
            .unwrap_or(source.len());
        source.get(start..end).unwrap_or_default()
    };
    let owns_visible_source = |glyph: &&crate::text::shape::Glyph| {
        cluster_source(glyph.cluster)
            .chars()
            .any(|character| !character.is_whitespace())
    };
    let Some(owner) = shaped
        .glyphs
        .iter()
        .find(|glyph| owns_visible_source(glyph) && glyph.advance_pt.abs() > f64::EPSILON)
        .or_else(|| shaped.glyphs.iter().find(owns_visible_source))
        .or_else(|| shaped.glyphs.first())
    else {
        return MappedRun {
            visible,
            ..MappedRun::default()
        };
    };

    let direct_characters: Option<Vec<char>> = (allow_direct
        && shaped.glyphs.len() == starts.len()
        && shaped
            .glyphs
            .iter()
            .map(|glyph| glyph.cluster)
            .eq(starts.iter().copied()))
    .then(|| {
        starts
            .iter()
            .copied()
            .map(|start| {
                let mut characters = cluster_source(start).chars();
                let character = characters.next()?;
                characters.next().is_none().then_some(character)
            })
            .collect::<Option<Vec<_>>>()
    })
    .flatten();
    if let Some(characters) = direct_characters {
        let mut is_space = Vec::with_capacity(characters.len());
        for (glyph, character) in visible.iter_mut().zip(characters) {
            glyph.ch = character;
            is_space.push(character == ' ');
        }
        return MappedRun {
            visible,
            semantic: Vec::new(),
            owner_face_index: owner.face_index,
            semantic_clusters: Vec::new(),
            is_space,
        };
    }

    let mut semantic = Vec::new();
    let mut semantic_clusters = Vec::new();
    let mut is_space = vec![false; shaped.glyphs.len()];
    for (position, start) in starts.iter().copied().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(source.len());
        let Some(cluster_source) = source.get(start..end) else {
            continue;
        };
        if cluster_source.is_empty() {
            continue;
        }
        if cluster_source == " "
            && let Some(owner_index) = shaped
                .glyphs
                .iter()
                .position(|glyph| glyph.cluster == start)
        {
            is_space[owner_index] = true;
        }
        // The invisible semantic run must walk the page at the same pace as the painted
        // run, or a viewer's character boxes (selection, search highlighting) collapse
        // onto one another even though the page renders correctly.
        //
        // The advance rides on `x_offset_pt`, not `advance_pt`: these glyphs are never
        // painted and must contribute *no* width to line measurement or table sizing —
        // the painted glyphs already account for that. The emitter reads it back to
        // build the TJ adjustment.
        let cluster_advance: f64 = shaped
            .glyphs
            .iter()
            .filter(|glyph| glyph.cluster == start)
            .map(|glyph| glyph.advance_pt)
            .sum();
        let owner_visible_index = shaped
            .glyphs
            .iter()
            .enumerate()
            .filter_map(|(index, glyph)| (glyph.cluster == start).then_some(index))
            .next_back()
            .unwrap_or_else(|| visible.len().saturating_sub(1));
        semantic_clusters.push(MappedSemanticCluster {
            source_offset: start,
            owner_visible_index,
            gid: owner.id.0,
            source: cluster_source.to_string(),
            advance_pt: cluster_advance,
        });
        if include_legacy_semantic {
            for (index, character) in cluster_source.chars().enumerate() {
                semantic.push(PlacedGlyph {
                    gid: owner.id.0,
                    advance_pt: 0.0,
                    x_offset_pt: if index == 0 { cluster_advance } else { 0.0 },
                    y_offset_pt: if index == 0 {
                        SEMANTIC_START_Y
                    } else {
                        SEMANTIC_CONTINUATION_Y
                    },
                    ch: character,
                });
            }
        }
    }
    MappedRun {
        visible,
        semantic,
        owner_face_index: owner.face_index,
        semantic_clusters,
        is_space,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::{FontDb, matching};
    use crate::text::shape;

    #[test]
    fn complex_clusters_are_serialized_once_in_logical_order_without_changing_appearance() {
        let source = "e\u{301} é لا שָׁ क्षि ก้ 𠀀 ⌚️ 👍🏽 👩‍💻 LTR שלום 123";
        let db = FontDb::with_system(&["../pdf-core/wasm/pkg/fonts".into()]);
        let faces: Vec<_> = db.faces().iter().collect();
        let shaped =
            shape::require_glyphs(shape::shape_with_fallback(source, &faces, 12.0, 0.0, true))
                .unwrap();
        let original_appearance: Vec<_> = shaped
            .glyphs
            .iter()
            .map(|glyph| {
                (
                    glyph.id.0,
                    glyph.advance_pt,
                    glyph.x_offset_pt,
                    glyph.y_offset_pt,
                )
            })
            .collect();
        let mapped = map_shaped_run(source, &shaped);
        let mapped_appearance: Vec<_> = mapped
            .visible
            .iter()
            .map(|glyph| {
                (
                    glyph.gid,
                    glyph.advance_pt,
                    glyph.x_offset_pt,
                    glyph.y_offset_pt,
                )
            })
            .collect();
        assert_eq!(mapped_appearance, original_appearance);

        let mut extracted = String::new();
        for glyph in &mapped.semantic {
            extracted.push(glyph.ch);
        }
        assert_eq!(extracted, source);
        let shaped_clusters: std::collections::BTreeSet<_> =
            shaped.glyphs.iter().map(|glyph| glyph.cluster).collect();
        assert_eq!(
            mapped
                .semantic
                .iter()
                .filter(|glyph| glyph.y_offset_pt == SEMANTIC_START_Y)
                .count(),
            shaped_clusters.len(),
            "every shaped cluster must own exactly one semantic sequence"
        );
    }

    #[test]
    fn simple_monotonic_ltr_maps_source_directly_onto_painted_glyphs() {
        let source = "Alpha Bravo 123";
        let db = FontDb::with_system(&["../pdf-core/wasm/pkg/fonts".into()]);
        let faces: Vec<_> = db.faces().iter().collect();
        let shaped =
            shape::require_glyphs(shape::shape_with_fallback(source, &faces, 12.0, 0.0, false))
                .unwrap();
        let mapped = map_shaped_run(source, &shaped);

        assert!(
            mapped.semantic.is_empty(),
            "simple LTR text must not require a duplicate invisible extraction layer"
        );
        assert_eq!(
            mapped
                .visible
                .iter()
                .map(|glyph| glyph.ch)
                .collect::<String>(),
            source
        );
        assert!(mapped.visible.iter().all(|glyph| glyph.ch != '\0'));
    }

    #[test]
    fn direct_mapping_can_be_disabled_for_an_explicit_bidi_container() {
        let source = "Latin inside bidi";
        let db = FontDb::with_system(&["../pdf-core/wasm/pkg/fonts".into()]);
        let faces: Vec<_> = db.faces().iter().collect();
        let shaped =
            shape::require_glyphs(shape::shape_with_fallback(source, &faces, 12.0, 0.0, false))
                .unwrap();
        let mapped = map_shaped_run_with_direct(source, &shaped, false);

        assert!(mapped.visible.iter().all(|glyph| glyph.ch == '\0'));
        assert_eq!(
            mapped
                .semantic
                .iter()
                .map(|glyph| glyph.ch)
                .collect::<String>(),
            source
        );
    }

    #[test]
    fn combining_cluster_uses_a_nonzero_advance_semantic_owner() {
        let source = "ب\u{08CA}\u{08CB}";
        let db = FontDb::with_system(&["../pdf-core/wasm/pkg/fonts".into()]);
        let faces: Vec<_> = db.faces().iter().collect();
        let shaped =
            shape::require_glyphs(shape::shape_with_fallback(source, &faces, 12.0, 0.0, true))
                .unwrap();
        let owner_gid = shaped
            .glyphs
            .iter()
            .find(|glyph| glyph.advance_pt.abs() > f64::EPSILON)
            .expect("base glyph with a visible advance")
            .id
            .0;
        let mapped = map_shaped_run_with_direct(source, &shaped, false);

        assert!(
            mapped.semantic.iter().all(|glyph| glyph.gid == owner_gid),
            "semantic CIDs must map through a glyph with usable selection metrics"
        );
        assert_eq!(
            mapped
                .semantic_clusters
                .iter()
                .map(|cluster| cluster.source.as_str())
                .collect::<String>(),
            source,
            "the complete Extended-A combining sequence must remain in logical order"
        );
    }

    #[test]
    fn decomposed_latin_stays_on_the_metric_compatible_primary_face() {
        let db = FontDb::with_bundled();
        let primary = matching::resolve(&db, "Calibri", 400, false)
            .face
            .expect("bundled Carlito face");
        let ids = matching::coverage_fallbacks(&db, primary, 400, false);
        let faces: Vec<_> = ids.iter().filter_map(|id| db.face(*id)).collect();
        let shaped = shape::require_glyphs(shape::shape_with_fallback(
            "e\u{301}", &faces, 11.0, 0.0, false,
        ))
        .unwrap();
        let selected: Vec<_> = shaped
            .glyphs
            .iter()
            .map(|glyph| faces[glyph.face_index].family_name.as_str())
            .collect();

        assert!(
            shaped.glyphs.iter().all(|glyph| glyph.face_index == 0),
            "decomposed Latin unexpectedly fell back from {} to {selected:?}",
            db.face(primary).unwrap().family_name
        );
    }
}

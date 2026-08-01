//! PDF emission via `pdf-writer`: Type0/CIDFontType2 subset fonts (Identity-H),
//! images, links, outline, and **deterministic** output (stable object order,
//! `/ID` from a hash of the input, dates from the document or a fixed literal).

use crate::fonts::subset::{SubsetFont, subset_for_pdf};
use crate::fonts::{FaceId, FaceMetrics, FontDb};
use crate::layout::{
    EncodedImage, FillRule, Item, LaidOutDocument, LinkTarget, Page, PathCommand, PathPaint,
    Positioned, ResolvedInternalLink, Stroke,
};
use crate::oxml::Rgba;
use crate::profile::Warning;
use crate::{OfficeError, Result};
use pdf_writer::types::{ActionType, AnnotationType, CidFontType, SystemInfo, TextRenderingMode};
use pdf_writer::writers::WMode;
use pdf_writer::{Content, Date, Filter, Finish, Name, Pdf, Rect, Ref, Str, TextStr};
use std::collections::{BTreeMap, BTreeSet};
use ttf_parser::{GlyphId, OutlineBuilder};

#[derive(Debug, Clone)]
enum AnnotationTarget {
    Public(LinkTarget),
    Internal { page: usize, y_pt: f64 },
}

/// Emits the laid-out document to deterministic PDF bytes.
///
/// `doc_id` is the stable `/ID` (16 bytes; the caller derives it from the input
/// hash). Any subset warnings are appended to `warnings`.
pub fn emit(
    laid: &LaidOutDocument,
    fonts: &FontDb,
    doc_id: [u8; 16],
    warnings: &mut Vec<Warning>,
) -> Result<Vec<u8>> {
    emit_with_internal_links(laid, fonts, doc_id, warnings, &[])
}

pub(crate) fn emit_with_internal_links(
    laid: &LaidOutDocument,
    fonts: &FontDb,
    doc_id: [u8; 16],
    warnings: &mut Vec<Warning>,
    internal_links: &[ResolvedInternalLink],
) -> Result<Vec<u8>> {
    // Defense in depth: a 0-page PDF is invalid per spec and treated as corrupt
    // by viewers. The format converters already reject mismatched inputs before
    // reaching here, so an empty page list is an internal invariant violation.
    if laid.pages.is_empty() {
        return Err(OfficeError::Internal("document produced no pages".into()));
    }

    // 1. Collect used glyphs per face and subset.
    let used = collect_used_glyphs(laid)?;
    let mut subsets: BTreeMap<FaceId, EmbeddedFont> = BTreeMap::new();
    for (face_id, usage) in &used {
        let Some(face) = fonts.face(*face_id) else {
            continue;
        };
        let subset = subset_for_pdf(face, &usage.gids, &usage.chars)?;
        if let Some(w) = &subset.warning
            && !warnings.contains(w)
        {
            warnings.push(w.clone());
        }
        subsets.insert(
            *face_id,
            EmbeddedFont::new(subset, &usage.semantic_pairs, &usage.visible_pairs)?,
        );
    }

    let mut pdf = Pdf::new();
    let mut next = 1i32;
    let mut alloc = || {
        let r = Ref::new(next);
        next += 1;
        r
    };

    let catalog_id = alloc();
    let tree_id = alloc();
    let info_id = alloc();

    // Font object refs.
    let face_order: Vec<FaceId> = subsets.keys().copied().collect();
    for face_id in &face_order {
        let f = subsets.get_mut(face_id).unwrap();
        f.type0 = alloc();
        f.cid = alloc();
        f.descriptor = alloc();
        f.file = alloc();
        f.to_unicode_ref = alloc();
        f.mapping = alloc();
    }

    // Image object refs (+ smask for RGBA).
    let mut image_refs: Vec<(Ref, Option<Ref>)> = Vec::new();
    for img in &laid.images {
        let main = alloc();
        let smask = if matches!(img, EncodedImage::Rgba8 { .. }) {
            Some(alloc())
        } else {
            None
        };
        image_refs.push((main, smask));
    }

    let page_ids: Vec<Ref> = laid.pages.iter().map(|_| alloc()).collect();
    let content_ids: Vec<Ref> = laid.pages.iter().map(|_| alloc()).collect();

    // Link annotations, allocated per page.
    let mut page_annots: Vec<Vec<(Ref, Rect, AnnotationTarget)>> = Vec::new();
    for (page_index, page) in laid.pages.iter().enumerate() {
        let mut annots = Vec::new();
        for pos in &page.items {
            if let Item::Link { w_pt, h_pt, target } = &pos.item {
                let y0 = page.height_pt - (pos.y_pt + h_pt);
                let rect = Rect::new(
                    pos.x_pt as f32,
                    y0 as f32,
                    (pos.x_pt + w_pt) as f32,
                    (page.height_pt - pos.y_pt) as f32,
                );
                annots.push((alloc(), rect, AnnotationTarget::Public(target.clone())));
            }
        }
        for link in internal_links
            .iter()
            .filter(|link| link.source_page == page_index)
        {
            let y0 = page.height_pt - (link.y_pt + link.h_pt);
            annots.push((
                alloc(),
                Rect::new(
                    link.x_pt as f32,
                    y0 as f32,
                    (link.x_pt + link.w_pt) as f32,
                    (page.height_pt - link.y_pt) as f32,
                ),
                AnnotationTarget::Internal {
                    page: link.target_page,
                    y_pt: link.target_y_pt,
                },
            ));
        }
        page_annots.push(annots);
    }

    // Outline refs.
    let outline_id = if laid.outline.is_empty() {
        None
    } else {
        Some(alloc())
    };
    let outline_item_ids: Vec<Ref> = laid.outline.iter().map(|_| alloc()).collect();

    // 2. Catalog + page tree.
    {
        let mut cat = pdf.catalog(catalog_id);
        cat.pages(tree_id);
        if let Some(oid) = outline_id {
            cat.outlines(oid);
        }
        cat.finish();
    }
    pdf.pages(tree_id)
        .kids(page_ids.iter().copied())
        .count(page_ids.len() as i32);

    // 3. Fonts.
    for face_id in &face_order {
        let family = fonts
            .face(*face_id)
            .map(|face| face.family_name.as_str())
            .unwrap_or_default();
        write_font(&mut pdf, subsets.get(face_id).unwrap(), family)?;
    }

    // 4. Images.
    for (img, (main, smask)) in laid.images.iter().zip(&image_refs) {
        write_image(&mut pdf, img, *main, *smask);
    }

    // 5. Pages + content.
    for (i, page) in laid.pages.iter().enumerate() {
        let content = render_page(page, &subsets, &face_order)?;
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&content, 6);

        let mut p = pdf.page(page_ids[i]);
        p.media_box(Rect::new(
            0.0,
            0.0,
            page.width_pt as f32,
            page.height_pt as f32,
        ));
        p.parent(tree_id);
        p.contents(content_ids[i]);
        {
            let mut res = p.resources();
            {
                let mut f = res.fonts();
                for (idx, face_id) in face_order.iter().enumerate() {
                    if subsets.contains_key(face_id) {
                        f.pair(Name(font_res_name(idx).as_bytes()), subsets[face_id].type0);
                    }
                }
                f.finish();
            }
            if !laid.images.is_empty() {
                let mut xo = res.x_objects();
                for (idx, (main, _)) in image_refs.iter().enumerate() {
                    xo.pair(Name(image_res_name(idx).as_bytes()), *main);
                }
                xo.finish();
            }
        }
        if !page_annots[i].is_empty() {
            p.annotations(page_annots[i].iter().map(|(r, _, _)| *r));
        }
        p.finish();

        pdf.stream(content_ids[i], &compressed)
            .filter(Filter::FlateDecode);
    }

    // 6. Link annotation objects.
    for (page_idx, annots) in page_annots.iter().enumerate() {
        for (aref, rect, target) in annots {
            let mut a = pdf.annotation(*aref);
            a.subtype(AnnotationType::Link).rect(*rect);
            // Suppress the PDF 32000-1 §12.5.2 default border (width 1), which
            // viewers otherwise paint as a black box around the link text.
            a.border(0.0, 0.0, 0.0, None);
            match target {
                AnnotationTarget::Public(LinkTarget::Url(url)) => {
                    a.action()
                        .action_type(ActionType::Uri)
                        .uri(Str(url.as_bytes()));
                }
                AnnotationTarget::Public(LinkTarget::Page(p)) => {
                    let target_page = (*p as usize).min(page_ids.len().saturating_sub(1));
                    let _ = page_idx;
                    a.action()
                        .action_type(ActionType::GoTo)
                        .destination()
                        .page(page_ids[target_page])
                        .xyz(0.0, laid.pages[target_page].height_pt as f32, None);
                }
                AnnotationTarget::Internal { page, y_pt } => {
                    let target_page = (*page).min(page_ids.len().saturating_sub(1));
                    let top = laid.pages[target_page].height_pt - y_pt;
                    a.action()
                        .action_type(ActionType::GoTo)
                        .destination()
                        .page(page_ids[target_page])
                        .xyz(0.0, top as f32, None);
                }
            }
            a.finish();
        }
    }

    // 7. Outline.
    if let Some(oid) = outline_id {
        write_outline(&mut pdf, oid, &outline_item_ids, laid, &page_ids);
    }

    // 8. Info + deterministic /ID.
    {
        let mut info = pdf.document_info(info_id);
        info.creator(TextStr("office-core"));
        info.producer(TextStr("office-core"));
        if let Some(d) = parse_pdf_date(&laid.dates.created) {
            info.creation_date(d);
        }
        if let Some(d) = parse_pdf_date(&laid.dates.modified) {
            info.modified_date(d);
        }
        info.finish();
    }
    pdf.set_file_id((doc_id.to_vec(), doc_id.to_vec()));

    Ok(pdf.finish())
}

/// Runs the font subsetting/embedding validation exactly as emission would,
/// without writing a PDF. The DocProfile layout preflight calls this so the
/// emit-time fail-closed verdicts (pdf.js painted-code collisions on shared
/// CFF glyphs, non-identity whole-embed CFF charsets) cannot disagree with
/// `profile()`.
pub(crate) fn preflight_embedded_fonts(laid: &LaidOutDocument, fonts: &FontDb) -> Result<()> {
    let used = collect_used_glyphs(laid)?;
    for (face_id, usage) in &used {
        let Some(face) = fonts.face(*face_id) else {
            continue;
        };
        let subset = subset_for_pdf(face, &usage.gids, &usage.chars)?;
        EmbeddedFont::new(subset, &usage.semantic_pairs, &usage.visible_pairs)?;
    }
    Ok(())
}

/// Per-face embedding state.
#[derive(Debug)]
struct EmbeddedFont {
    subset: SubsetFont,
    /// Bare `CFF ` table for `FontFile3` when the face is CFF-flavoured.
    cff_program: Option<Vec<u8>>,
    cid_widths: BTreeMap<u16, f32>,
    type0: Ref,
    cid: Ref,
    descriptor: Ref,
    file: Ref,
    to_unicode_ref: Ref,
    mapping: Ref,
    semantic_cids: BTreeMap<SemanticKey, u16>,
    painted_cids: BTreeMap<(u16, char), u16>,
    cid_to_gid: BTreeMap<u16, u16>,
    to_unicode: BTreeMap<u16, String>,
}

impl EmbeddedFont {
    fn new(
        subset: SubsetFont,
        semantic_pairs: &BTreeSet<SemanticKey>,
        visible_pairs: &BTreeSet<(u16, char)>,
    ) -> Result<EmbeddedFont> {
        let is_cff = subset.is_cff;
        let cff_program = if is_cff {
            let program = subset.cff_table().ok_or_else(|| {
                OfficeError::Font("CFF font program has no extractable CFF table".into())
            })?;
            // The encoding CMap addresses glyphs as CID == glyph id. The
            // subsetter guarantees that identity charset; whole-font embeds
            // (no-subsetting licenses, subsetter fallback) may not, and a
            // non-identity charset would paint wrong or blank glyphs in every
            // viewer. Fail closed instead.
            crate::fonts::cff_check::ensure_identity_cid_addressing(&program)?;
            Some(program)
        } else {
            None
        };
        let mut cid_widths: BTreeMap<u16, f32> =
            subset.widths.iter().map(|&(c, w)| (c, w as f32)).collect();
        let mut cid_to_gid: BTreeMap<u16, u16> = subset
            .remap
            .values()
            .copied()
            .map(|gid| (gid, gid))
            .collect();
        let mut to_unicode: BTreeMap<u16, String> = subset.to_unicode.iter().cloned().collect();
        to_unicode.retain(|_, source| !source.is_empty());
        let mut semantic_cids = BTreeMap::new();
        let mut painted_cids = BTreeMap::new();
        let mut next_cid = subset
            .remap
            .values()
            .copied()
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| {
                OfficeError::unsupported(
                    crate::UnsupportedCode::DecompressionLimit,
                    "font needs more than 65535 CIDs",
                )
            })?;
        let painted_eligible = |(_, source, owns): &SemanticKey| {
            let mut characters = source.chars();
            characters.next().is_some() && characters.next().is_none() && *owns
        };
        // pdf.js resolves a CIDFontType0C glyph through the LOWEST content code
        // its /Encoding CMap maps to the glyph's CID, so painted codes must be
        // allocated below the semantic-only codes that share their backing
        // glyph. TrueType (Identity-H + CIDToGIDMap stream) resolves each code
        // independently and keeps the original single-pass order (frozen
        // goldens).
        let ordered: Vec<&SemanticKey> = if is_cff {
            semantic_pairs
                .iter()
                .filter(|pair| painted_eligible(pair))
                .chain(semantic_pairs.iter().filter(|pair| !painted_eligible(pair)))
                .collect()
        } else {
            semantic_pairs.iter().collect()
        };
        for (gid, source, owns_advance) in ordered {
            let subset_gid = subset.remap.get(gid).copied().unwrap_or(0);
            semantic_cids.insert((*gid, source.clone(), *owns_advance), next_cid);
            let mut characters = source.chars();
            if let Some(character) = characters.next()
                && characters.next().is_none()
                && *owns_advance
            {
                painted_cids.insert((*gid, character), next_cid);
            }
            to_unicode.insert(next_cid, source.clone());
            let base_width = cid_widths.get(&subset_gid).copied().unwrap_or(0.0);
            // A combining-mark owner can legitimately have a zero font advance even
            // when its complete shaped cluster spans visible base glyphs. Semantic CIDs
            // are invisible, so a nonzero backing glyph from the same subset can safely
            // supply usable viewer metrics while the CID's ToUnicode value remains the
            // complete logical source sequence.
            let (backing_gid, width) = if !*owns_advance {
                (subset_gid, 0.0)
            } else if base_width.abs() > f32::EPSILON {
                (subset_gid, base_width)
            } else {
                subset
                    .remap
                    .values()
                    .find_map(|candidate| {
                        let width = cid_widths.get(candidate).copied().unwrap_or(0.0);
                        (width.abs() > f32::EPSILON).then_some((*candidate, width))
                    })
                    .unwrap_or((subset_gid, 1000.0))
            };
            cid_to_gid.insert(next_cid, backing_gid);
            cid_widths.insert(next_cid, width);
            next_cid = next_cid.checked_add(1).ok_or_else(|| {
                OfficeError::unsupported(
                    crate::UnsupportedCode::DecompressionLimit,
                    "font needs more than 65535 CIDs",
                )
            })?;
        }
        if is_cff {
            // pdf.js paints at most one content code per CFF glyph (it inverts
            // the CMap per CID, lowest code wins). Two painted codes sharing a
            // glyph mean the higher one silently loses its ink; permit that
            // only for glyphs that draw nothing anyway (spaces), otherwise
            // fail closed rather than emit a partially visible PDF. Only pairs
            // that visible glyphs actually paint count: semantic-cluster pairs
            // share the cluster's owner gid but render through the invisible
            // Tr-3 layer, never as ink.
            let mut painted_codes_by_gid: BTreeMap<u16, u32> = BTreeMap::new();
            for (pair, code) in &painted_cids {
                if !visible_pairs.contains(pair) {
                    continue;
                }
                let backing = cid_to_gid.get(code).copied().unwrap_or(0);
                *painted_codes_by_gid.entry(backing).or_insert(0) += 1;
            }
            let shared: Vec<u16> = painted_codes_by_gid
                .iter()
                .filter(|(_, codes)| **codes > 1)
                .map(|(gid, _)| *gid)
                .collect();
            if !shared.is_empty() {
                let face = ttf_parser::Face::parse(&subset.data, 0).map_err(|error| {
                    OfficeError::Font(format!(
                        "embedded font subset could not be parsed for alias checking: {error:?}"
                    ))
                })?;
                for gid in shared {
                    if glyph_has_ink(&face, gid) {
                        return Err(OfficeError::unsupported(
                            crate::UnsupportedCode::MissingGlyph,
                            format!(
                                "CFF glyph {gid} backs multiple painted character codes; \
                                 pdf.js paints only the lowest code per glyph, so the others \
                                 would silently lose their ink"
                            ),
                        ));
                    }
                }
            }
        }
        Ok(EmbeddedFont {
            subset,
            cff_program,
            cid_widths,
            type0: Ref::new(1),
            cid: Ref::new(1),
            descriptor: Ref::new(1),
            file: Ref::new(1),
            to_unicode_ref: Ref::new(1),
            mapping: Ref::new(1),
            semantic_cids,
            painted_cids,
            cid_to_gid,
            to_unicode,
        })
    }
}

#[derive(Default)]
struct UsedFace {
    gids: BTreeSet<GlyphId>,
    chars: BTreeMap<GlyphId, char>,
    semantic_pairs: BTreeSet<SemanticKey>,
    /// Pairs that visible glyphs actually paint through their content code
    /// (`painted_cid`). Semantic-cluster pairs share the cluster's owner gid
    /// and never paint ink themselves, so only these pairs participate in the
    /// pdf.js one-code-per-glyph collision gate.
    visible_pairs: BTreeSet<(u16, char)>,
}

type SemanticKey = (u16, String, bool);

type UsedGlyphs = BTreeMap<FaceId, UsedFace>;

fn collect_used_glyphs(laid: &LaidOutDocument) -> Result<UsedGlyphs> {
    const MAX_GLYPHS: usize = 10_000_000;
    let mut used = UsedGlyphs::new();
    let mut glyph_count = 0usize;
    for page in &laid.pages {
        for pos in &page.items {
            collect_item_glyphs(&pos.item, &mut used, &mut glyph_count, MAX_GLYPHS)?;
        }
    }
    Ok(used)
}

fn collect_item_glyphs(
    item: &Item,
    used: &mut UsedGlyphs,
    glyph_count: &mut usize,
    limit: usize,
) -> Result<()> {
    match item {
        Item::Transform { item, .. } => collect_item_glyphs(item, used, glyph_count, limit)?,
        Item::Clip { items, .. } => {
            for positioned in items {
                collect_item_glyphs(&positioned.item, used, glyph_count, limit)?;
            }
        }
        Item::SemanticText(run) => {
            let semantic_count = run.clusters.iter().fold(0usize, |count, cluster| {
                count.saturating_add(cluster.source.chars().count())
            });
            *glyph_count = glyph_count.saturating_add(semantic_count);
            if *glyph_count > limit {
                return Err(OfficeError::unsupported(
                    crate::UnsupportedCode::DecompressionLimit,
                    format!("text glyph count exceeds limit {limit}"),
                ));
            }
            let entry = used.entry(run.face).or_default();
            let semantic: Vec<_> = run
                .clusters
                .iter()
                .map(|cluster| {
                    entry.gids.insert(GlyphId(cluster.gid));
                    (cluster.gid, cluster.source.clone(), cluster.advance_pt)
                })
                .collect();
            collect_semantic_pairs(entry, &semantic, Some(run.base_direction))?;
        }
        Item::Glyphs(run) => {
            *glyph_count = glyph_count.saturating_add(run.glyphs.len());
            if *glyph_count > limit {
                return Err(OfficeError::unsupported(
                    crate::UnsupportedCode::DecompressionLimit,
                    format!("text glyph count exceeds limit {limit}"),
                ));
            }
            let entry = used.entry(run.face).or_default();
            let semantic_clusters = semantic_clusters(&run.glyphs);
            collect_semantic_pairs(entry, &semantic_clusters, run.base_direction)?;
            for g in &run.glyphs {
                if is_semantic_glyph(g) {
                    continue;
                }
                let gid = GlyphId(g.gid);
                entry.gids.insert(gid);
                if g.ch != '\0' {
                    entry.semantic_pairs.insert((g.gid, g.ch.to_string(), true));
                    entry.visible_pairs.insert((g.gid, g.ch));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_semantic_pairs(
    entry: &mut UsedFace,
    semantic: &[(u16, String, f64)],
    base_direction: Option<text_core::Direction>,
) -> Result<()> {
    if semantic.is_empty() {
        return Ok(());
    }
    let logical: String = semantic
        .iter()
        .map(|(_, source, _)| source.as_str())
        .collect();
    for token in visual_semantic_tokens(semantic, &logical, base_direction)? {
        for (gid, source, advance_pt) in token {
            entry.gids.insert(GlyphId(gid));
            entry
                .semantic_pairs
                .insert((gid, source, advance_pt.abs() > f64::EPSILON));
        }
    }
    Ok(())
}

fn write_font(pdf: &mut Pdf, f: &EmbeddedFont, family: &str) -> Result<()> {
    let is_cff = f.subset.is_cff;

    // Type0. CFF programs have no CIDToGIDMap: codes (including semantic CIDs
    // beyond the glyph range) map to CFF CIDs through an embedded CMap stream
    // instead of Identity-H.
    {
        let mut type0 = pdf.type0_font(f.type0);
        type0.base_font(Name(b"OfficeCoreSubset"));
        if is_cff {
            type0.encoding_cmap(f.mapping);
        } else {
            type0.encoding_predefined(Name(b"Identity-H"));
        }
        type0.descendant_font(f.cid).to_unicode(f.to_unicode_ref);
    }

    // CIDFontType2 (TrueType) or CIDFontType0 (CFF).
    {
        let mut cid = pdf.cid_font(f.cid);
        cid.subtype(if is_cff {
            CidFontType::Type0
        } else {
            CidFontType::Type2
        })
        .base_font(Name(b"OfficeCoreSubset"))
        .system_info(SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"Identity"),
            supplement: 0,
        })
        .font_descriptor(f.descriptor);
        if !is_cff {
            cid.cid_to_gid_map_stream(f.mapping);
        }
        // CIDFontType2 widths are keyed by code (= CID under Identity-H);
        // CIDFontType0 widths are keyed by the CFF CID the encoding CMap maps
        // to (= subset gid), so they are exactly the subset glyph advances.
        let widths_by_cid: BTreeMap<u16, f32> = if is_cff {
            f.subset
                .widths
                .iter()
                .map(|&(gid, width)| (gid, width as f32))
                .collect()
        } else {
            f.cid_widths.clone()
        };
        if let (Some(&min), Some(&max)) = (
            widths_by_cid.keys().next(),
            widths_by_cid.keys().next_back(),
        ) {
            let widths: Vec<f32> = (min..=max)
                .map(|c| widths_by_cid.get(&c).copied().unwrap_or(0.0))
                .collect();
            cid.widths().consecutive(min, widths);
        }
        cid.finish();
    }

    // Descriptor + font file.
    {
        let metrics = font_descriptor_metrics(&f.subset.data);
        let mut descriptor = pdf.font_descriptor(f.descriptor);
        descriptor
            .name(Name(b"OfficeCoreSubset"))
            .flags(pdf_writer::types::FontFlags::NON_SYMBOLIC)
            .bbox(Rect::new(
                metrics.x_min,
                metrics.y_min,
                metrics.x_max,
                metrics.y_max,
            ))
            .italic_angle(0.0)
            .ascent(metrics.ascent)
            .descent(metrics.descent)
            .cap_height(metrics.cap_height)
            .stem_v(80.0);
        if is_cff {
            // A bare CFF program carries no SFNT name table, so the
            // descriptor's FontFamily is the only channel inspectors have
            // for the face identity. The TrueType path stays byte-identical
            // (frozen goldens).
            if !family.is_empty() {
                descriptor.family(Str(family.as_bytes()));
            }
            descriptor.font_file3(f.file);
        } else {
            descriptor.font_file2(f.file);
        }
    }
    let program: &[u8] = f.cff_program.as_deref().unwrap_or(&f.subset.data);
    let compressed_program = miniz_oxide::deflate::compress_to_vec_zlib(program, 6);
    {
        let mut file = pdf.stream(f.file, &compressed_program);
        file.filter(Filter::FlateDecode);
        if is_cff {
            file.pair(Name(b"Subtype"), Name(b"CIDFontType0C"));
        }
    }

    // ToUnicode CMap.
    let cmap = build_to_unicode(&f.to_unicode);
    pdf.stream(
        f.to_unicode_ref,
        &miniz_oxide::deflate::compress_to_vec_zlib(&cmap, 6),
    )
    .filter(Filter::FlateDecode);
    if is_cff {
        // Only content-reachable codes may appear: pdf.js inverts this CMap
        // per glyph (lowest code wins), so an unused identity-prelude entry
        // over the subset glyph ids would shadow every code the content
        // streams actually use. Painted codes were allocated below
        // semantic-only codes for the same reason.
        let mut content_codes: BTreeMap<u16, u16> = BTreeMap::new();
        for code in f.semantic_cids.values() {
            let backing = f.cid_to_gid.get(code).copied().ok_or_else(|| {
                OfficeError::Internal(format!("content code {code} has no backing glyph"))
            })?;
            content_codes.insert(*code, backing);
        }
        let mapping = build_cff_encoding(&content_codes);
        // PDF 32000-1 §9.7.5.3 requires /Type /CMap, /CMapName and
        // /CIDSystemInfo on an embedded CMap stream dictionary.
        let mut cmap = pdf.cmap(f.mapping, &mapping);
        cmap.name(Name(b"DocYantra-CFF-Encoding"));
        cmap.system_info(SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"Identity"),
            supplement: 0,
        });
        cmap.writing_mode(WMode::Horizontal);
    } else {
        pdf.stream(f.mapping, &build_cid_to_gid(&f.cid_to_gid));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct FontDescriptorMetrics {
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
    ascent: f32,
    descent: f32,
    cap_height: f32,
}

/// Returns the embedded face's real 1000-em descriptor metrics. PDF text
/// extraction derives word boxes from these values; synthetic 800/-200 values
/// displaced geometry even though the painted glyph baseline was correct.
fn font_descriptor_metrics(sfnt: &[u8]) -> FontDescriptorMetrics {
    let fallback = FontDescriptorMetrics {
        x_min: -200.0,
        y_min: -200.0,
        x_max: 1000.0,
        y_max: 900.0,
        ascent: 800.0,
        descent: -200.0,
        cap_height: 700.0,
    };
    let Ok(face) = ttf_parser::Face::parse(sfnt, 0) else {
        return fallback;
    };
    let metrics = FaceMetrics::new(face);
    let face = metrics.face();
    let scale = 1000.0 / f32::from(face.units_per_em().max(1));
    let bounds = face.global_bounding_box();
    let descriptor = FontDescriptorMetrics {
        x_min: f32::from(bounds.x_min) * scale,
        y_min: f32::from(bounds.y_min) * scale,
        x_max: f32::from(bounds.x_max) * scale,
        y_max: f32::from(bounds.y_max) * scale,
        ascent: metrics.ascent_pt(1000.0).round() as f32,
        descent: -(metrics.descent_pt(1000.0).round() as f32),
        cap_height: face
            .capital_height()
            .map(|height| (f32::from(height) * scale).round())
            .unwrap_or_else(|| metrics.ascent_pt(1000.0).round() as f32),
    };
    if [
        descriptor.x_min,
        descriptor.y_min,
        descriptor.x_max,
        descriptor.y_max,
        descriptor.ascent,
        descriptor.descent,
        descriptor.cap_height,
    ]
    .iter()
    .all(|value| value.is_finite())
    {
        descriptor
    } else {
        fallback
    }
}

/// True when the glyph's outline draws anything (any contour segment exists).
fn glyph_has_ink(face: &ttf_parser::Face, gid: u16) -> bool {
    struct InkProbe {
        inked: bool,
    }
    impl OutlineBuilder for InkProbe {
        fn move_to(&mut self, _: f32, _: f32) {}
        fn line_to(&mut self, _: f32, _: f32) {
            self.inked = true;
        }
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
            self.inked = true;
        }
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
            self.inked = true;
        }
        fn close(&mut self) {}
    }
    let mut probe = InkProbe { inked: false };
    face.outline_glyph(GlyphId(gid), &mut probe).is_some() && probe.inked
}

fn write_image(pdf: &mut Pdf, img: &EncodedImage, main: Ref, smask: Option<Ref>) {
    match img {
        EncodedImage::Jpeg {
            data,
            width,
            height,
        } => {
            let mut xo = pdf.image_xobject(main, data);
            xo.filter(Filter::DctDecode);
            xo.width(*width as i32).height(*height as i32);
            xo.color_space().device_rgb();
            xo.bits_per_component(8);
            xo.finish();
        }
        EncodedImage::Rgb8 {
            width,
            height,
            data,
        } => {
            let comp = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
            let mut xo = pdf.image_xobject(main, &comp);
            xo.filter(Filter::FlateDecode);
            xo.width(*width as i32).height(*height as i32);
            xo.color_space().device_rgb();
            xo.bits_per_component(8);
            xo.finish();
        }
        EncodedImage::Rgba8 {
            width,
            height,
            data,
        } => {
            let rgb: Vec<u8> = data
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            let alpha: Vec<u8> = data.chunks_exact(4).map(|p| p[3]).collect();
            let comp = miniz_oxide::deflate::compress_to_vec_zlib(&rgb, 6);
            {
                let mut xo = pdf.image_xobject(main, &comp);
                xo.filter(Filter::FlateDecode);
                xo.width(*width as i32).height(*height as i32);
                xo.color_space().device_rgb();
                xo.bits_per_component(8);
                if let Some(sm) = smask {
                    xo.s_mask(sm);
                }
                xo.finish();
            }
            if let Some(sm) = smask {
                let acomp = miniz_oxide::deflate::compress_to_vec_zlib(&alpha, 6);
                let mut xo = pdf.image_xobject(sm, &acomp);
                xo.filter(Filter::FlateDecode);
                xo.width(*width as i32).height(*height as i32);
                xo.color_space().device_gray();
                xo.bits_per_component(8);
                xo.finish();
            }
        }
    }
}

/// Renders a page's items into an (uncompressed) content stream.
fn render_page(
    page: &Page,
    subsets: &BTreeMap<FaceId, EmbeddedFont>,
    face_order: &[FaceId],
) -> Result<Vec<u8>> {
    let mut content = Content::new();
    let h = page.height_pt;
    for pos in &page.items {
        draw_item(&mut content, pos, h, &pos.item, subsets, face_order)?;
    }
    Ok(content.finish().to_vec())
}

fn draw_item(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    item: &Item,
    subsets: &BTreeMap<FaceId, EmbeddedFont>,
    face_order: &[FaceId],
) -> Result<()> {
    match item {
        Item::Transform { matrix, item } => {
            let [a, b, c, d, e, f] = *matrix;
            content.save_state();
            content.transform([
                a as f32,
                -b as f32,
                -c as f32,
                d as f32,
                (e + c * page_h) as f32,
                (page_h - d * page_h - f) as f32,
            ]);
            draw_item(content, pos, page_h, item, subsets, face_order)?;
            content.restore_state();
        }
        Item::Rect {
            w_pt,
            h_pt,
            fill,
            stroke,
        } => draw_rect(content, pos, page_h, *w_pt, *h_pt, fill, stroke),
        Item::Line {
            dx_pt,
            dy_pt,
            stroke,
        } => draw_line(content, pos, page_h, *dx_pt, *dy_pt, stroke),
        Item::Path { commands, paint } => {
            draw_path(content, pos, page_h, commands, paint);
        }
        Item::Clip {
            commands,
            fill_rule,
            items,
        } => {
            content.save_state();
            append_path(content, pos, page_h, commands);
            match fill_rule {
                FillRule::NonZero => content.clip_nonzero(),
                FillRule::EvenOdd => content.clip_even_odd(),
            };
            content.end_path();
            for child in items {
                let child_pos = Positioned {
                    x_pt: pos.x_pt + child.x_pt,
                    y_pt: pos.y_pt + child.y_pt,
                    item: child.item.clone(),
                };
                draw_item(
                    content,
                    &child_pos,
                    page_h,
                    &child_pos.item,
                    subsets,
                    face_order,
                )?;
            }
            content.restore_state();
        }
        Item::Glyphs(run) => draw_glyphs(content, pos, page_h, run, subsets, face_order)?,
        Item::SemanticText(run) => {
            draw_semantic_run(content, pos, page_h, run, subsets, face_order)?
        }
        Item::Image {
            w_pt,
            h_pt,
            image_id,
        } => draw_image(content, pos, page_h, *w_pt, *h_pt, image_id.0),
        Item::CroppedImage {
            w_pt,
            h_pt,
            image_id,
            crop,
        } => draw_cropped_image(content, pos, page_h, *w_pt, *h_pt, image_id.0, *crop),
        Item::Link { .. } => {} // handled as annotations
    }
    Ok(())
}

fn set_fill(content: &mut Content, c: Rgba) {
    content.set_fill_rgb(c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
}
fn set_stroke(content: &mut Content, c: Rgba) {
    content.set_stroke_rgb(c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
}

fn apply_stroke(content: &mut Content, stroke: &Stroke) {
    set_stroke(content, stroke.color);
    content.set_line_width(stroke.width_pt as f32);
    content.set_dash_pattern(
        stroke
            .dash
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|value| *value as f32),
        0.0,
    );
}

fn append_path(content: &mut Content, pos: &Positioned, page_h: f64, commands: &[PathCommand]) {
    for command in commands {
        match *command {
            PathCommand::MoveTo { x, y } => {
                content.move_to((pos.x_pt + x) as f32, (page_h - pos.y_pt - y) as f32);
            }
            PathCommand::LineTo { x, y } => {
                content.line_to((pos.x_pt + x) as f32, (page_h - pos.y_pt - y) as f32);
            }
            PathCommand::CubicTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                content.cubic_to(
                    (pos.x_pt + x1) as f32,
                    (page_h - pos.y_pt - y1) as f32,
                    (pos.x_pt + x2) as f32,
                    (page_h - pos.y_pt - y2) as f32,
                    (pos.x_pt + x) as f32,
                    (page_h - pos.y_pt - y) as f32,
                );
            }
            PathCommand::Close => {
                content.close_path();
            }
        }
    }
}

fn draw_path(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    commands: &[PathCommand],
    paint: &PathPaint,
) {
    append_path(content, pos, page_h, commands);
    if let Some(fill) = paint.fill {
        set_fill(content, fill);
    }
    if let Some(stroke) = &paint.stroke {
        apply_stroke(content, stroke);
    }
    match (&paint.fill, &paint.stroke, paint.fill_rule) {
        (Some(_), Some(_), FillRule::NonZero) => content.fill_nonzero_and_stroke(),
        (Some(_), Some(_), FillRule::EvenOdd) => content.fill_even_odd_and_stroke(),
        (Some(_), None, FillRule::NonZero) => content.fill_nonzero(),
        (Some(_), None, FillRule::EvenOdd) => content.fill_even_odd(),
        (None, Some(_), _) => content.stroke(),
        (None, None, _) => content.end_path(),
    };
}

fn draw_rect(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    w: f64,
    hgt: f64,
    fill: &Option<Rgba>,
    stroke: &Option<crate::layout::Stroke>,
) {
    let y = page_h - (pos.y_pt + hgt);
    content.rect(pos.x_pt as f32, y as f32, w as f32, hgt as f32);
    match (fill, stroke) {
        (Some(f), Some(s)) => {
            set_fill(content, *f);
            apply_stroke(content, s);
            content.fill_nonzero_and_stroke();
        }
        (Some(f), None) => {
            set_fill(content, *f);
            content.fill_nonzero();
        }
        (None, Some(s)) => {
            apply_stroke(content, s);
            content.stroke();
        }
        (None, None) => {}
    }
}

fn draw_line(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    dx: f64,
    dy: f64,
    stroke: &crate::layout::Stroke,
) {
    apply_stroke(content, stroke);
    content.move_to(pos.x_pt as f32, (page_h - pos.y_pt) as f32);
    content.line_to((pos.x_pt + dx) as f32, (page_h - (pos.y_pt + dy)) as f32);
    content.stroke();
}

fn draw_glyphs(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    run: &crate::layout::GlyphRun,
    subsets: &BTreeMap<FaceId, EmbeddedFont>,
    face_order: &[FaceId],
) -> Result<()> {
    draw_glyphs_impl(content, pos, page_h, run, subsets, face_order, true)
}

#[allow(clippy::too_many_arguments)]
fn draw_glyphs_impl(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    run: &crate::layout::GlyphRun,
    subsets: &BTreeMap<FaceId, EmbeddedFont>,
    face_order: &[FaceId],
    mark_artifact: bool,
) -> Result<()> {
    let Some(font) = subsets.get(&run.face) else {
        return Err(OfficeError::Internal(format!(
            "missing embedded font for laid-out face {:?}",
            run.face
        )));
    };
    let idx = face_order.iter().position(|f| *f == run.face).unwrap_or(0);
    let baseline = word_pdf_text_y(page_h, pos.y_pt);
    let semantic = semantic_clusters(&run.glyphs);
    if !semantic.is_empty() {
        draw_semantic_text(content, pos, baseline, run, idx, font, &semantic)?;
        let mut visible = run.clone();
        visible.glyphs.retain(|glyph| !is_semantic_glyph(glyph));
        if visible.glyphs.is_empty() {
            return Ok(());
        }
        draw_glyphs_impl(content, pos, page_h, &visible, subsets, face_order, true)?;
        return Ok(());
    }
    if mark_artifact && run.glyphs.iter().all(|glyph| glyph.ch == '\0') {
        draw_glyph_outlines(content, pos, baseline, run, font)?;
        if let Some(dec) = run.underline {
            draw_decor(content, pos, page_h, run, dec.thickness_pt, dec.y_offset_pt);
        }
        if let Some(dec) = run.strike {
            draw_decor(content, pos, page_h, run, dec.thickness_pt, dec.y_offset_pt);
        }
        return Ok(());
    }

    // CFF faces carry no raw glyph-id fallback codes: the pruned /Encoding
    // CMap lists only painted and semantic codes (a raw code would shadow the
    // glyph's painted code under pdf.js's lowest-code inversion). Paint-only
    // glyphs in mixed runs are drawn as vector outlines instead.
    if font.subset.is_cff
        && run
            .glyphs
            .iter()
            .any(|glyph| painted_code(font, glyph).is_none())
    {
        return draw_cff_mixed_run(content, pos, page_h, baseline, run, idx, font);
    }

    if run
        .glyphs
        .iter()
        .any(|glyph| glyph.x_offset_pt != 0.0 || glyph.y_offset_pt != 0.0)
    {
        let mut cursor = pos.x_pt;
        for glyph in &run.glyphs {
            let cid = painted_cid(font, glyph);
            content.begin_text();
            content.set_text_rendering_mode(TextRenderingMode::Fill);
            set_fill(content, run.color);
            content.set_font(Name(font_res_name(idx).as_bytes()), run.size_pt as f32);
            content.set_text_matrix([
                1.0,
                0.0,
                0.0,
                1.0,
                (cursor + glyph.x_offset_pt) as f32,
                (baseline + glyph.y_offset_pt) as f32,
            ]);
            content.show(Str(&cid.to_be_bytes()));
            content.end_text();
            cursor += glyph.advance_pt;
        }
        return Ok(());
    }

    content.begin_text();
    content.set_text_rendering_mode(TextRenderingMode::Fill);
    set_fill(content, run.color);
    content.set_font(Name(font_res_name(idx).as_bytes()), run.size_pt as f32);
    content.set_text_matrix([1.0, 0.0, 0.0, 1.0, pos.x_pt as f32, baseline as f32]);

    let mut tj = content.show_positioned();
    let mut items = tj.items();
    for g in &run.glyphs {
        let cid = painted_cid(font, g);
        items.show(Str(&cid.to_be_bytes()));
        let font_w = font.cid_widths.get(&cid).copied().unwrap_or(0.0);
        let actual_1000 = (g.advance_pt / run.size_pt.max(0.01) * 1000.0) as f32;
        let adj = font_w - actual_1000;
        if adj.abs() > 0.01 {
            items.adjust(adj);
        }
    }
    items.finish();
    tj.finish();
    content.end_text();

    // Decorations.
    if let Some(dec) = run.underline {
        draw_decor(content, pos, page_h, run, dec.thickness_pt, dec.y_offset_pt);
    }
    if let Some(dec) = run.strike {
        draw_decor(content, pos, page_h, run, dec.thickness_pt, dec.y_offset_pt);
    }
    Ok(())
}

struct PdfGlyphOutline<'a> {
    content: &'a mut Content,
    origin_x: f32,
    origin_y: f32,
    scale: f32,
    current: (f32, f32),
    contour_start: (f32, f32),
}

impl PdfGlyphOutline<'_> {
    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.origin_x + x * self.scale,
            self.origin_y + y * self.scale,
        )
    }
}

impl OutlineBuilder for PdfGlyphOutline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        self.content.move_to(point.0, point.1);
        self.current = point;
        self.contour_start = point;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        self.content.line_to(point.0, point.1);
        self.current = point;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.point(x1, y1);
        let end = self.point(x, y);
        let first = (
            self.current.0 + (control.0 - self.current.0) * (2.0 / 3.0),
            self.current.1 + (control.1 - self.current.1) * (2.0 / 3.0),
        );
        let second = (
            end.0 + (control.0 - end.0) * (2.0 / 3.0),
            end.1 + (control.1 - end.1) * (2.0 / 3.0),
        );
        self.content
            .cubic_to(first.0, first.1, second.0, second.1, end.0, end.1);
        self.current = end;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let first = self.point(x1, y1);
        let second = self.point(x2, y2);
        let end = self.point(x, y);
        self.content
            .cubic_to(first.0, first.1, second.0, second.1, end.0, end.1);
        self.current = end;
    }

    fn close(&mut self) {
        self.content.close_path();
        self.current = self.contour_start;
    }
}

fn draw_glyph_outlines(
    content: &mut Content,
    pos: &Positioned,
    baseline: f64,
    run: &crate::layout::GlyphRun,
    font: &EmbeddedFont,
) -> Result<()> {
    let face = ttf_parser::Face::parse(&font.subset.data, 0).map_err(|error| {
        OfficeError::Font(format!(
            "embedded font subset could not be parsed during glyph painting: {error:?}"
        ))
    })?;
    let scale = run.size_pt as f32 / f32::from(face.units_per_em().max(1));
    let mut cursor = pos.x_pt;
    set_fill(content, run.color);
    for glyph in &run.glyphs {
        let subset_gid = font.subset.remap.get(&glyph.gid).copied().ok_or_else(|| {
            OfficeError::Font(format!(
                "painted glyph {} is absent from the embedded font subset",
                glyph.gid
            ))
        })?;
        let outlined = {
            let mut outline = PdfGlyphOutline {
                content,
                origin_x: (cursor + glyph.x_offset_pt) as f32,
                origin_y: (baseline + glyph.y_offset_pt) as f32,
                scale,
                current: (0.0, 0.0),
                contour_start: (0.0, 0.0),
            };
            let outlined = face
                .outline_glyph(GlyphId(subset_gid), &mut outline)
                .is_some();
            if outlined {
                outline.content.fill_nonzero();
            }
            outlined
        };
        if !outlined && face.glyph_bounding_box(GlyphId(subset_gid)).is_some() {
            return Err(OfficeError::Font(format!(
                "painted glyph {} has bounds but its outline could not be emitted",
                glyph.gid
            )));
        }
        cursor += glyph.advance_pt;
    }
    Ok(())
}

/// A CFF run containing paint-only glyphs mixes text ops (glyphs with a
/// painted content code) with vector outlines (the rest). Raw glyph-id codes
/// must never be shown for CFF faces: they are absent from the pruned
/// /Encoding CMap, and reintroducing them would shadow the painted codes under
/// pdf.js's lowest-code CMap inversion.
fn draw_cff_mixed_run(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    baseline: f64,
    run: &crate::layout::GlyphRun,
    font_index: usize,
    font: &EmbeddedFont,
) -> Result<()> {
    let face = ttf_parser::Face::parse(&font.subset.data, 0).map_err(|error| {
        OfficeError::Font(format!(
            "embedded font subset could not be parsed during glyph painting: {error:?}"
        ))
    })?;
    let scale = run.size_pt as f32 / f32::from(face.units_per_em().max(1));
    let mut cursor = pos.x_pt;
    set_fill(content, run.color);
    for glyph in &run.glyphs {
        if let Some(code) = painted_code(font, glyph) {
            content.begin_text();
            content.set_text_rendering_mode(TextRenderingMode::Fill);
            content.set_font(
                Name(font_res_name(font_index).as_bytes()),
                run.size_pt as f32,
            );
            content.set_text_matrix([
                1.0,
                0.0,
                0.0,
                1.0,
                (cursor + glyph.x_offset_pt) as f32,
                (baseline + glyph.y_offset_pt) as f32,
            ]);
            content.show(Str(&code.to_be_bytes()));
            content.end_text();
        } else {
            let subset_gid = font.subset.remap.get(&glyph.gid).copied().ok_or_else(|| {
                OfficeError::Font(format!(
                    "painted glyph {} is absent from the embedded font subset",
                    glyph.gid
                ))
            })?;
            let outlined = {
                let mut outline = PdfGlyphOutline {
                    content,
                    origin_x: (cursor + glyph.x_offset_pt) as f32,
                    origin_y: (baseline + glyph.y_offset_pt) as f32,
                    scale,
                    current: (0.0, 0.0),
                    contour_start: (0.0, 0.0),
                };
                let outlined = face
                    .outline_glyph(GlyphId(subset_gid), &mut outline)
                    .is_some();
                if outlined {
                    outline.content.fill_nonzero();
                }
                outlined
            };
            if !outlined && face.glyph_bounding_box(GlyphId(subset_gid)).is_some() {
                return Err(OfficeError::Font(format!(
                    "painted glyph {} has bounds but its outline could not be emitted",
                    glyph.gid
                )));
            }
        }
        cursor += glyph.advance_pt;
    }
    if let Some(dec) = run.underline {
        draw_decor(content, pos, page_h, run, dec.thickness_pt, dec.y_offset_pt);
    }
    if let Some(dec) = run.strike {
        draw_decor(content, pos, page_h, run, dec.thickness_pt, dec.y_offset_pt);
    }
    Ok(())
}

fn draw_semantic_text(
    content: &mut Content,
    pos: &Positioned,
    baseline: f64,
    run: &crate::layout::GlyphRun,
    font_index: usize,
    font: &EmbeddedFont,
    semantic: &[(u16, String, f64)],
) -> Result<()> {
    // Semantic clusters carry the complete shaped-run width even when fallback faces
    // split the painted suffix into a separate GlyphRun. Any visible glyphs sharing this
    // run are necessarily that suffix, so backtrack over the remainder to recover the
    // source run's physical origin. Paragraph justification has already been folded into
    // whitespace semantic-cluster advances by line layout.
    let painted_width: f64 = run
        .glyphs
        .iter()
        .filter(|glyph| !is_semantic_glyph(glyph))
        .map(|glyph| glyph.advance_pt)
        .sum();
    let semantic_width: f64 = semantic.iter().map(|(_, _, advance)| *advance).sum();
    let origin_x = pos.x_pt - (semantic_width - painted_width).max(0.0);

    draw_semantic_layer(
        content,
        origin_x,
        baseline,
        run.size_pt,
        run.base_direction,
        font_index,
        font,
        semantic,
    )
}

fn draw_semantic_run(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    run: &crate::layout::SemanticRun,
    subsets: &BTreeMap<FaceId, EmbeddedFont>,
    face_order: &[FaceId],
) -> Result<()> {
    let font = subsets.get(&run.face).ok_or_else(|| {
        OfficeError::Internal(format!(
            "missing embedded font for semantic face {:?}",
            run.face
        ))
    })?;
    let font_index = face_order
        .iter()
        .position(|face| *face == run.face)
        .unwrap_or(0);
    let semantic: Vec<_> = run
        .clusters
        .iter()
        .map(|cluster| (cluster.gid, cluster.source.clone(), cluster.advance_pt))
        .collect();
    draw_semantic_layer(
        content,
        pos.x_pt,
        word_pdf_text_y(page_h, pos.y_pt),
        run.size_pt,
        Some(run.base_direction),
        font_index,
        font,
        &semantic,
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_semantic_layer(
    content: &mut Content,
    origin_x: f64,
    baseline: f64,
    size_pt: f64,
    base_direction: Option<text_core::Direction>,
    font_index: usize,
    font: &EmbeddedFont,
    semantic: &[(u16, String, f64)],
) -> Result<()> {
    if semantic.is_empty() {
        return Ok(());
    }
    let logical: String = semantic.iter().map(|pair| pair.1.as_str()).collect();
    let actual_text = pdfium_actual_text(&logical, base_direction);
    let visual_semantic = visual_semantic_tokens(semantic, &logical, base_direction)?;
    let mut marked = content.begin_marked_content_with_properties(Name(b"Span"));
    marked.properties().actual_text(TextStr(&actual_text));
    marked.finish();

    let semantic_width: f64 = semantic.iter().map(|(_, _, advance)| *advance).sum();

    let rtl = base_direction.unwrap_or_else(|| text_core::bidi_base_direction(&logical))
        == text_core::Direction::Rtl;
    let mut cursor = if rtl {
        origin_x + semantic_width
    } else {
        origin_x
    };
    for token in &visual_semantic {
        let desired_width: f64 = token.iter().map(|(_, _, advance)| *advance).sum();
        let x = if rtl {
            cursor -= desired_width;
            cursor
        } else {
            let x = cursor;
            cursor += desired_width;
            x
        };
        content.begin_text();
        content.set_text_rendering_mode(TextRenderingMode::Invisible);
        content.set_font(Name(font_res_name(font_index).as_bytes()), size_pt as f32);
        let mut encoded = Vec::with_capacity(token.len().saturating_mul(2));
        let mut natural_width_1000 = 0.0_f64;
        for (gid, source, advance_pt) in token {
            let cid = font
                .semantic_cids
                .get(&(*gid, source.clone(), advance_pt.abs() > f64::EPSILON))
                .copied()
                .ok_or_else(|| {
                    OfficeError::Internal(format!(
                        "semantic text CID is missing for glyph {gid} and source {source:?}"
                    ))
                })?;
            encoded.extend_from_slice(&cid.to_be_bytes());
            natural_width_1000 += f64::from(font.cid_widths.get(&cid).copied().unwrap_or(0.0));
        }
        let natural_width_pt = natural_width_1000 / 1000.0 * size_pt.max(0.01);
        if natural_width_pt <= f64::EPSILON && desired_width > f64::EPSILON {
            return Err(OfficeError::Font(
                "semantic text token has no usable embedded-font width".to_string(),
            ));
        }
        let horizontal_scale = if natural_width_pt > f64::EPSILON {
            desired_width / natural_width_pt
        } else {
            1.0
        };
        content.set_text_matrix([
            horizontal_scale as f32,
            0.0,
            0.0,
            1.0,
            x as f32,
            baseline as f32,
        ]);
        content.show(Str(&encoded));
        content.end_text();
    }
    content.end_marked_content();
    Ok(())
}

/// Word's Office-to-PDF path snaps text origins on a quarter-point vertical
/// grid when both page origins share that grid. Keep layout and pagination
/// continuous, and preserve the laid-out baseline on fractional-phase page
/// heights rather than shifting it onto an arbitrary top- or bottom-edge grid.
fn word_pdf_text_y(page_h: f64, layout_y: f64) -> f64 {
    let pdf_y = page_h - layout_y;
    let page_grid_y = page_h * 4.0;
    if (page_grid_y - page_grid_y.round()).abs() <= 1e-6 {
        (pdf_y * 4.0).round() / 4.0
    } else {
        pdf_y
    }
}

fn visual_semantic_tokens(
    semantic: &[(u16, String, f64)],
    logical: &str,
    base_direction: Option<text_core::Direction>,
) -> Result<Vec<Vec<(u16, String, f64)>>> {
    let Some(owner_gid) = semantic.first().map(|(gid, _, _)| *gid) else {
        return Ok(Vec::new());
    };
    let tokens = pdfjs_semantic_tokens(logical, base_direction);
    let advances = semantic_token_advances(semantic);
    let cluster_counts = semantic_token_cluster_counts(semantic);
    if tokens.len() != advances.len() || tokens.len() != cluster_counts.len() {
        return Err(OfficeError::Internal(format!(
            "semantic token count mismatch: {} text tokens, {} advance tokens, {} cluster counts",
            tokens.len(),
            advances.len(),
            cluster_counts.len()
        )));
    }
    Ok(tokens
        .into_iter()
        .zip(advances)
        .zip(cluster_counts)
        .map(|((token, advance), cluster_count)| {
            if cluster_count == 1 && token.chars().count() > 1 {
                let characters: Vec<_> = token.chars().collect();
                let advance_owner = characters
                    .iter()
                    .position(|character| !is_semantic_modifier(*character))
                    .unwrap_or(0);
                characters
                    .into_iter()
                    .enumerate()
                    .map(|(index, character)| {
                        (
                            owner_gid,
                            character.to_string(),
                            if index == advance_owner { advance } else { 0.0 },
                        )
                    })
                    .collect()
            } else {
                vec![(owner_gid, token, advance)]
            }
        })
        .collect())
}

fn semantic_token_cluster_counts(semantic: &[(u16, String, f64)]) -> Vec<usize> {
    let mut counts = Vec::new();
    let mut current_kind = None;
    let mut current_count = 0usize;
    for (_, source, _) in semantic {
        let mut cluster_kind = None;
        for character in source.chars() {
            let kind = character.is_whitespace();
            if cluster_kind == Some(kind) {
                continue;
            }
            cluster_kind = Some(kind);
            if current_kind.is_some_and(|current| current != kind) {
                counts.push(current_count);
                current_count = 0;
            }
            current_kind = Some(kind);
            current_count += 1;
        }
    }
    if current_kind.is_some() {
        counts.push(current_count);
    }
    counts
}

fn semantic_token_advances(semantic: &[(u16, String, f64)]) -> Vec<f64> {
    let mut advances = Vec::new();
    let mut current_kind = None;
    let mut current_advance = 0.0;
    for (_, source, cluster_advance) in semantic {
        let mut segments: Vec<(bool, usize)> = Vec::new();
        for character in source.chars() {
            let whitespace = character.is_whitespace();
            if let Some((kind, count)) = segments.last_mut()
                && *kind == whitespace
            {
                *count += 1;
            } else {
                segments.push((whitespace, 1));
            }
        }
        let scalar_count: usize = segments.iter().map(|(_, count)| *count).sum();
        for (kind, count) in segments {
            if current_kind.is_some_and(|current| current != kind) {
                advances.push(current_advance);
                current_advance = 0.0;
            }
            current_kind = Some(kind);
            current_advance += if scalar_count == count {
                *cluster_advance
            } else {
                *cluster_advance * count as f64 / scalar_count as f64
            };
        }
    }
    if current_kind.is_some() {
        advances.push(current_advance);
    }
    advances
}

/// Pre-compensates semantic text for PDFium's extraction-time bidi pass.
///
/// PDFium reorders RTL `/ActualText` as if it were still in logical order, so
/// serializing UAX #9 display order here makes the extracted result logical.
/// Combining marks inside RTL sequences need the inverse order as well because
/// PDFium reverses them together with their base characters.
fn pdfium_actual_text(text: &str, base_direction: Option<text_core::Direction>) -> String {
    let visual = text_core::bidi_visual_order_with_base(text, base_direction);
    reverse_rtl_combining_sequences(&visual)
}

/// Pre-compensates each PDF.js text item while preserving logical item order.
///
/// PDF.js applies bidi reordering independently to the non-whitespace items it
/// derives from a content stream. A full paragraph visual order would therefore
/// reverse both the characters and the word sequence twice. Keeping whitespace
/// boundaries in logical order while visualizing each token lets PDF.js recover
/// the original source sequence.
#[cfg(test)]
fn pdfjs_semantic_text(text: &str) -> String {
    pdfjs_semantic_tokens(text, None).concat()
}

fn pdfjs_semantic_tokens(text: &str, base_direction: Option<text_core::Direction>) -> Vec<String> {
    let mut output = Vec::new();
    let mut token = String::new();
    let mut token_is_whitespace = None;
    for character in text.chars() {
        let is_whitespace = character.is_whitespace();
        if token_is_whitespace.is_some_and(|current| current != is_whitespace) {
            if token_is_whitespace == Some(true) {
                output.push(std::mem::take(&mut token));
            } else {
                output.push(pdfjs_token_text(&token, base_direction));
                token.clear();
            }
        }
        token_is_whitespace = Some(is_whitespace);
        token.push(character);
    }
    if token_is_whitespace == Some(true) {
        output.push(token);
    } else if !token.is_empty() {
        output.push(pdfjs_token_text(&token, base_direction));
    }
    output
}

fn pdfjs_token_text(text: &str, base_direction: Option<text_core::Direction>) -> String {
    let visual = text_core::bidi_visual_order_with_base(text, base_direction);
    reverse_pdfjs_rtl_combining_sequences(&visual)
}

fn reverse_pdfjs_rtl_combining_sequences(text: &str) -> String {
    // The pinned bidi tables already preserve the logical mark order for the
    // established Hebrew/Arabic ranges. U+08CA..U+08D2 are newer and arrive in
    // reverse order; PDF.js preserves their sequence while moving the base.
    reverse_combining_sequences(text, |character| {
        matches!(character as u32, 0x08CA..=0x08D2)
    })
}

fn reverse_rtl_combining_sequences(text: &str) -> String {
    reverse_combining_sequences(text, is_rtl_combining_mark)
}

fn reverse_combining_sequences(text: &str, is_mark: impl Fn(char) -> bool) -> String {
    let mut output = String::with_capacity(text.len());
    let mut marks = Vec::new();
    for character in text.chars() {
        if is_mark(character) {
            marks.push(character);
        } else {
            output.extend(marks.drain(..).rev());
            output.push(character);
        }
    }
    output.extend(marks.into_iter().rev());
    output
}

fn is_rtl_combining_mark(character: char) -> bool {
    matches!(
        character as u32,
        0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x06DF..=0x06E4
            | 0x06E7..=0x06E8
            | 0x06EA..=0x06ED
            | 0x08D3..=0x08FF
    )
}

fn is_semantic_modifier(character: char) -> bool {
    matches!(
        character as u32,
        0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06ED
            | 0x08CA..=0x08FF
            | 0x0900..=0x0903
            | 0x093A..=0x094D
            | 0x0951..=0x0957
            | 0x0962..=0x0963
            | 0x0E31
            | 0x0E34..=0x0E3A
            | 0x0E47..=0x0E4E
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200D
            | 0x20D0..=0x20FF
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0x1F3FB..=0x1F3FF
            | 0xE0100..=0xE01EF
    )
}

/// Semantic clusters together with the advance the painted run gives each one.
///
/// The invisible text layer has to walk the page at the same pace as the visible
/// glyphs; without the advance it would restack every cluster on the run's origin.
fn semantic_clusters(glyphs: &[crate::layout::PlacedGlyph]) -> Vec<(u16, String, f64)> {
    let mut clusters: Vec<(u16, String, f64)> = Vec::new();
    let mut current: Option<(u16, String, f64)> = None;
    for glyph in glyphs.iter().filter(|glyph| is_semantic_glyph(glyph)) {
        if glyph.y_offset_pt == -1.0e100
            && let Some(cluster) = current.take()
        {
            clusters.push(cluster);
        }
        let entry = current.get_or_insert_with(|| (glyph.gid, String::new(), 0.0));
        entry.1.push(glyph.ch);
        entry.2 += glyph.x_offset_pt;
    }
    if let Some(cluster) = current {
        clusters.push(cluster);
    }
    clusters
}

fn is_semantic_glyph(glyph: &crate::layout::PlacedGlyph) -> bool {
    glyph.y_offset_pt <= crate::text::semantic::SEMANTIC_START_Y
}

/// The painted content code for a visible glyph, when one exists. CFF runs
/// must draw glyphs without one as outlines; TrueType runs may fall back to
/// the raw subset glyph id (see [`painted_cid`]).
fn painted_code(font: &EmbeddedFont, glyph: &crate::layout::PlacedGlyph) -> Option<u16> {
    if glyph.ch != '\0' && !is_semantic_glyph(glyph) {
        font.painted_cids.get(&(glyph.gid, glyph.ch)).copied()
    } else {
        None
    }
}

fn painted_cid(font: &EmbeddedFont, glyph: &crate::layout::PlacedGlyph) -> u16 {
    painted_code(font, glyph)
        .unwrap_or_else(|| font.subset.remap.get(&glyph.gid).copied().unwrap_or(0))
}

fn draw_decor(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    run: &crate::layout::GlyphRun,
    thick: f64,
    offset: f64,
) {
    let width: f64 = run.glyphs.iter().map(|g| g.advance_pt).sum();
    let y = page_h - (pos.y_pt + offset);
    set_stroke(content, run.color);
    content.set_line_width(thick as f32);
    content.move_to(pos.x_pt as f32, y as f32);
    content.line_to((pos.x_pt + width) as f32, y as f32);
    content.stroke();
}

fn draw_image(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    w: f64,
    hgt: f64,
    image_idx: usize,
) {
    let y = page_h - (pos.y_pt + hgt);
    content.save_state();
    content.transform([w as f32, 0.0, 0.0, hgt as f32, pos.x_pt as f32, y as f32]);
    content.x_object(Name(image_res_name(image_idx).as_bytes()));
    content.restore_state();
}

fn draw_cropped_image(
    content: &mut Content,
    pos: &Positioned,
    page_h: f64,
    w: f64,
    hgt: f64,
    image_idx: usize,
    crop: [f64; 4],
) {
    let visible_w = (1.0 - crop[0] - crop[2]).max(0.001);
    let visible_h = (1.0 - crop[1] - crop[3]).max(0.001);
    let full_w = w / visible_w;
    let full_h = hgt / visible_h;
    let clip_y = page_h - (pos.y_pt + hgt);
    content.save_state();
    content.rect(pos.x_pt as f32, clip_y as f32, w as f32, hgt as f32);
    content.clip_nonzero();
    content.end_path();
    content.transform([
        full_w as f32,
        0.0,
        0.0,
        full_h as f32,
        (pos.x_pt - crop[0] * full_w) as f32,
        (clip_y - crop[3] * full_h) as f32,
    ]);
    content.x_object(Name(image_res_name(image_idx).as_bytes()));
    content.restore_state();
}

fn write_outline(
    pdf: &mut Pdf,
    outline_id: Ref,
    item_ids: &[Ref],
    laid: &LaidOutDocument,
    page_ids: &[Ref],
) {
    if item_ids.is_empty() {
        return;
    }
    pdf.outline(outline_id)
        .first(item_ids[0])
        .last(item_ids[item_ids.len() - 1])
        .count(item_ids.len() as i32);
    for (i, entry) in laid.outline.iter().enumerate() {
        let mut item = pdf.outline_item(item_ids[i]);
        item.parent(outline_id);
        item.title(TextStr(&entry.title));
        if i > 0 {
            item.prev(item_ids[i - 1]);
        }
        if i + 1 < item_ids.len() {
            item.next(item_ids[i + 1]);
        }
        let page_ref = page_ids[(entry.page as usize).min(page_ids.len().saturating_sub(1))];
        let top = laid.pages[(entry.page as usize).min(laid.pages.len().saturating_sub(1))]
            .height_pt
            - entry.y_pt;
        item.dest().page(page_ref).xyz(0.0, top as f32, None);
        item.finish();
    }
}

/// Builds a ToUnicode CMap for a subset.
fn build_to_unicode(to_unicode: &BTreeMap<u16, String>) -> Vec<u8> {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let mut entries: Vec<(u16, String)> = to_unicode
        .iter()
        .filter(|(_, text)| !text.is_empty())
        .map(|(cid, text)| {
            let utf16 = text
                .encode_utf16()
                .map(|unit| format!("{unit:04X}"))
                .collect();
            (*cid, utf16)
        })
        .collect();
    entries.sort_by_key(|(c, _)| *c);
    for chunk in entries.chunks(100) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (cid, hex) in chunk {
            s.push_str(&format!("<{cid:04X}> <{hex}>\n"));
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s.into_bytes()
}

fn build_cid_to_gid(mapping: &BTreeMap<u16, u16>) -> Vec<u8> {
    let max_cid = mapping.keys().next_back().copied().unwrap_or(0);
    let mut bytes = vec![0; (usize::from(max_cid) + 1) * 2];
    for (&cid, &gid) in mapping {
        let offset = usize::from(cid) * 2;
        bytes[offset..offset + 2].copy_from_slice(&gid.to_be_bytes());
    }
    bytes
}

/// Embedded `/Encoding` CMap mapping content codes to CFF CIDs; the same
/// format pdf-core emits for its CFF embeds.
fn build_cff_encoding(mapping: &BTreeMap<u16, u16>) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def\n\
         /CMapName /DocYantra-CFF-Encoding def\n/CMapType 1 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(u16, u16)> = mapping.iter().map(|(&code, &cid)| (code, cid)).collect();
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} begincidchar\n", chunk.len()));
        for &(code, cid) in chunk {
            cmap.push_str(&format!("<{code:04X}> {cid}\n"));
        }
        cmap.push_str("endcidchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap.into_bytes()
}

fn parse_pdf_date(s: &str) -> Option<Date> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    let field =
        |a: usize, b: usize| -> Option<u32> { digits.get(a..b).and_then(|s| s.parse().ok()) };
    let year = field(0, 4)? as u16;
    let mut d = Date::new(year);
    if let Some(m) = field(4, 6) {
        d = d.month(m as u8);
    }
    if let Some(day) = field(6, 8) {
        d = d.day(day as u8);
    }
    if let Some(h) = field(8, 10) {
        d = d.hour(h as u8);
    }
    if let Some(mi) = field(10, 12) {
        d = d.minute(mi as u8);
    }
    if let Some(se) = field(12, 14) {
        d = d.second(se as u8);
    }
    Some(d)
}

fn font_res_name(idx: usize) -> String {
    format!("F{idx}")
}
fn image_res_name(idx: usize) -> String {
    format!("Im{idx}")
}

/// A convenience used by the CLI/tests to check the header quickly.
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// Fails with an internal error (used when emission preconditions are violated).
pub fn internal(msg: impl Into<String>) -> OfficeError {
    OfficeError::Internal(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_path_emits_cubic_even_odd_paint_dash_and_bounded_clip() {
        use crate::layout::{FillRule, PathCommand, PathPaint, Stroke};

        let red = Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 1.0,
        };
        let blue = Rgba {
            r: 0,
            g: 0,
            b: 255,
            a: 1.0,
        };
        let commands = vec![
            PathCommand::MoveTo { x: 0.0, y: 0.0 },
            PathCommand::LineTo { x: 20.0, y: 0.0 },
            PathCommand::CubicTo {
                x1: 25.0,
                y1: 0.0,
                x2: 25.0,
                y2: 20.0,
                x: 20.0,
                y: 20.0,
            },
            PathCommand::Close,
        ];
        let page = Page {
            width_pt: 100.0,
            height_pt: 100.0,
            items: vec![
                Positioned {
                    x_pt: 10.0,
                    y_pt: 10.0,
                    item: Item::Path {
                        commands: commands.clone(),
                        paint: PathPaint {
                            fill: Some(red),
                            stroke: Some(Stroke {
                                width_pt: 2.0,
                                color: blue,
                                dash: Some(vec![3.0, 2.0]),
                            }),
                            fill_rule: FillRule::EvenOdd,
                        },
                    },
                },
                Positioned {
                    x_pt: 40.0,
                    y_pt: 10.0,
                    item: Item::Clip {
                        commands,
                        fill_rule: FillRule::EvenOdd,
                        items: vec![Positioned {
                            x_pt: 0.0,
                            y_pt: 0.0,
                            item: Item::Rect {
                                w_pt: 30.0,
                                h_pt: 30.0,
                                fill: Some(red),
                                stroke: None,
                            },
                        }],
                    },
                },
            ],
        };

        let bytes = render_page(&page, &BTreeMap::new(), &[]).expect("render path page");
        let stream = String::from_utf8(bytes).expect("ASCII content stream");
        assert!(stream.contains(" c\n"), "missing cubic operator: {stream}");
        assert!(
            stream.contains("B*\n"),
            "missing even-odd fill+stroke: {stream}"
        );
        assert!(
            stream.contains("[3 2] 0 d"),
            "missing dash pattern: {stream}"
        );
        assert!(
            stream.contains("W*\nn\n"),
            "missing even-odd clip: {stream}"
        );
        assert_eq!(
            stream.lines().filter(|line| *line == "q").count(),
            stream.lines().filter(|line| *line == "Q").count(),
            "unbalanced clipping state: {stream}"
        );
    }

    /// Subsets the repo-committed Noto Sans CJK JP OTF (a CID-keyed CFF face)
    /// down to `chars`, returning the subset plus each char's ORIGINAL gid.
    fn cff_fixture(chars: &[char]) -> (SubsetFont, BTreeMap<char, u16>) {
        use crate::fonts::db::FontDb;
        use crate::fonts::metrics::FaceMetrics;
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../pdf-core/wasm/pkg/fonts/NotoSansCJKjp-Regular.otf"
        ))
        .expect("repo-committed Noto Sans CJK JP OTF");
        let mut db = FontDb::with_bundled();
        db.register(bytes).expect("register CFF face");
        let face = db.faces().last().expect("registered face");
        let metrics = FaceMetrics::new(face.parsed().unwrap());
        let mut used = BTreeSet::new();
        let mut gid_of = BTreeMap::new();
        for &ch in chars {
            let gid = metrics.glyph_id(ch).expect("glyph for fixture char");
            used.insert(gid);
            gid_of.insert(ch, gid.0);
        }
        let subset = subset_for_pdf(face, &used, &BTreeMap::new()).expect("subset CFF face");
        assert!(subset.is_cff, "fixture face must be CFF-flavoured");
        (subset, gid_of)
    }

    /// pdf.js paints a CIDFontType0C glyph only through the LOWEST charcode its
    /// /Encoding CMap maps to the glyph's CID. Painted codes must therefore
    /// sort below any semantic-only code sharing the same backing glyph.
    #[test]
    fn cff_painted_codes_stay_below_semantic_only_codes() {
        let (subset, gid_of) = cff_fixture(&['あ', 'い']);
        let gid = gid_of[&'あ'];
        let pairs = BTreeSet::from([
            // Sorts first in the pair set, but must NOT get the lower code:
            // it is semantic-only (owns no advance) and paints nothing.
            (gid, "あ".to_string(), false),
            // Painted single-char owner of the same backing glyph.
            (gid, "い".to_string(), true),
        ]);
        let embedded =
            EmbeddedFont::new(subset, &pairs, &BTreeSet::new()).expect("embedded font state");
        let painted = embedded.painted_cids[&(gid, 'い')];
        let semantic_only = embedded.semantic_cids[&(gid, "あ".to_string(), false)];
        assert_eq!(
            embedded.cid_to_gid[&painted], embedded.cid_to_gid[&semantic_only],
            "both codes back onto the same glyph"
        );
        assert!(
            painted < semantic_only,
            "painted code {painted} must be the lowest code for its CID or pdf.js \
             blanks it (semantic-only code {semantic_only} shadows it)"
        );
    }

    /// Two painted codes can only share one visible glyph by losing one of
    /// them in pdf.js. That must fail closed instead of shipping a PDF whose
    /// second code silently paints nothing.
    #[test]
    fn cff_visible_glyph_shared_by_two_painted_codes_fails_closed() {
        let (subset, gid_of) = cff_fixture(&['一']);
        let gid = gid_of[&'一'];
        let pairs = BTreeSet::from([
            (gid, "一".to_string(), true),
            (gid, "二".to_string(), true), // second painted owner of the same glyph
        ]);
        let visible = BTreeSet::from([(gid, '\u{4e00}'), (gid, '\u{4e8c}')]);
        let error = EmbeddedFont::new(subset, &pairs, &visible)
            .expect_err("two painted codes on one inked CFF glyph must fail closed");
        assert!(
            error.to_string().starts_with("unsupported:"),
            "collision must surface as a typed unsupported error: {error}"
        );
    }

    /// A CFF run mixing coded glyphs with paint-only glyphs must show exactly
    /// the painted codes as text and draw the rest as vector outlines: a raw
    /// glyph-id code is absent from the pruned /Encoding CMap and would shadow
    /// the glyph's painted code under pdf.js's lowest-code inversion.
    #[test]
    fn cff_mixed_run_draws_paint_only_glyphs_as_outlines() {
        use crate::layout::{GlyphRun, PlacedGlyph};

        let (subset, gid_of) = cff_fixture(&['一', '二']);
        let coded_gid = gid_of[&'一'];
        let outlined_gid = gid_of[&'二'];
        let pairs = BTreeSet::from([(coded_gid, "一".to_string(), true)]);
        let visible = BTreeSet::from([(coded_gid, '一')]);
        let embedded = EmbeddedFont::new(subset, &pairs, &visible).expect("embedded font state");
        let painted_code = embedded.painted_cids[&(coded_gid, '一')];

        let face = FaceId(0);
        let glyph = |gid: u16, ch: char| PlacedGlyph {
            gid,
            advance_pt: 10.0,
            x_offset_pt: 0.0,
            y_offset_pt: 0.0,
            ch,
        };
        let page = Page {
            width_pt: 200.0,
            height_pt: 100.0,
            items: vec![Positioned {
                x_pt: 10.0,
                y_pt: 20.0,
                item: Item::Glyphs(GlyphRun {
                    face,
                    size_pt: 10.0,
                    color: Rgba {
                        r: 0,
                        g: 0,
                        b: 0,
                        a: 1.0,
                    },
                    base_direction: None,
                    glyphs: vec![glyph(coded_gid, '一'), glyph(outlined_gid, '\0')],
                    underline: None,
                    strike: None,
                }),
            }],
        };
        let bytes = render_page(&page, &BTreeMap::from([(face, embedded)]), &[face])
            .expect("render mixed CFF run");
        let stream = String::from_utf8_lossy(&bytes);

        let shows = stream.matches("Tj").count();
        assert_eq!(
            shows, 1,
            "exactly the coded glyph is shown as text: {stream}"
        );
        // pdf-writer octal-escapes non-printable bytes inside literal strings.
        let [hi, lo] = painted_code.to_be_bytes();
        let code_octal = format!("(\\{hi:03o}\\{lo:03o}) Tj");
        assert!(
            stream.contains(&code_octal),
            "the painted content code must be shown (looked for {code_octal}): {stream}"
        );
        assert!(
            stream.contains(" c\n") || stream.contains(" l\n"),
            "paint-only glyph must draw as outline segments: {stream}"
        );
        assert!(
            stream.contains("f\n"),
            "outline contours must be filled: {stream}"
        );
    }

    /// Semantic-cluster pairs share their cluster's owner gid without painting
    /// ink (the visible glyphs are paint-only; extraction rides the invisible
    /// Tr-3 layer). Sharing an inked glyph between such pairs must therefore
    /// stay supported — only pairs painted by visible glyphs can collide.
    #[test]
    fn cff_cluster_pairs_sharing_an_inked_glyph_stay_supported() {
        let (subset, gid_of) = cff_fixture(&['一']);
        let gid = gid_of[&'一'];
        let pairs = BTreeSet::from([(gid, "一".to_string(), true), (gid, "二".to_string(), true)]);
        let embedded = EmbeddedFont::new(subset, &pairs, &BTreeSet::new())
            .expect("cluster-owner aliases never paint through their codes");
        assert_eq!(embedded.painted_cids.len(), 2);
    }

    /// Blank glyphs (spaces) may alias freely: pdf.js skipping the higher code
    /// changes nothing visually, and exact per-code ToUnicode is preserved.
    #[test]
    fn cff_blank_glyph_alias_stays_supported() {
        let (subset, gid_of) = cff_fixture(&[' ']);
        let gid = gid_of[&' '];
        let pairs = BTreeSet::from([
            (gid, " ".to_string(), true),
            (gid, "\u{A0}".to_string(), true),
        ]);
        let visible = BTreeSet::from([(gid, ' '), (gid, '\u{A0}')]);
        let embedded = EmbeddedFont::new(subset, &pairs, &visible)
            .expect("blank-glyph aliases stay supported");
        assert_eq!(embedded.painted_cids.len(), 2);
    }

    #[test]
    fn distinct_source_sequences_get_distinct_cids_for_one_gid() {
        let subset = SubsetFont {
            data: Vec::new(),
            remap: BTreeMap::from([(5, 2)]),
            widths: vec![(2, 500)],
            to_unicode: Vec::new(),
            is_cff: false,
            warning: None,
        };
        let pairs = BTreeSet::from([(5, "का".to_string(), true), (5, "कि".to_string(), true)]);
        let embedded =
            EmbeddedFont::new(subset, &pairs, &BTreeSet::new()).expect("embedded font state");
        let first = embedded.semantic_cids[&(5, "का".to_string(), true)];
        let second = embedded.semantic_cids[&(5, "कि".to_string(), true)];

        assert_ne!(first, second);
        assert_eq!(embedded.cid_to_gid[&first], embedded.cid_to_gid[&second]);
        assert_eq!(embedded.to_unicode[&first], "का");
        assert_eq!(embedded.to_unicode[&second], "कि");
    }

    #[test]
    fn zero_width_semantic_owner_gets_usable_selection_width() {
        let subset = SubsetFont {
            data: Vec::new(),
            remap: BTreeMap::from([(5, 2), (6, 3)]),
            widths: vec![(2, 0), (3, 500)],
            to_unicode: Vec::new(),
            is_cff: false,
            warning: None,
        };
        let source = "ب\u{08CA}\u{08CB}".to_string();
        let embedded = EmbeddedFont::new(
            subset,
            &BTreeSet::from([(
                5,
                pdfium_actual_text(&source, Some(text_core::Direction::Rtl)),
                true,
            )]),
            &BTreeSet::new(),
        )
        .unwrap();
        let cid = *embedded.semantic_cids.values().next().unwrap();

        assert_eq!(embedded.cid_to_gid[&cid], 3);
        assert_eq!(embedded.cid_widths[&cid], 500.0);
    }

    #[test]
    fn semantic_text_matrix_scales_viewer_geometry_to_the_physical_width() {
        use crate::layout::{SemanticCluster, SemanticRun};

        let face = FaceId(0);
        let subset = SubsetFont {
            data: Vec::new(),
            remap: BTreeMap::from([(5, 2)]),
            widths: vec![(2, 500)],
            to_unicode: Vec::new(),
            is_cff: false,
            warning: None,
        };
        let embedded = EmbeddedFont::new(
            subset,
            &BTreeSet::from([(5, "word".to_string(), true)]),
            &BTreeSet::new(),
        )
        .expect("semantic font state");
        let page = Page {
            width_pt: 100.0,
            height_pt: 100.0,
            items: vec![Positioned {
                x_pt: 10.0,
                y_pt: 20.0,
                item: Item::SemanticText(SemanticRun {
                    face,
                    size_pt: 10.0,
                    base_direction: text_core::Direction::Ltr,
                    clusters: "word"
                        .chars()
                        .map(|character| SemanticCluster {
                            gid: 5,
                            source: character.to_string(),
                            advance_pt: 6.25,
                        })
                        .collect(),
                }),
            }],
        };
        let bytes = render_page(&page, &BTreeMap::from([(face, embedded)]), &[face])
            .expect("render semantic page");
        let stream = String::from_utf8(bytes).expect("ASCII content stream");

        assert!(
            stream.contains("5 0 0 1 10 80 Tm"),
            "25pt desired width over a 5pt backing glyph must use 5x horizontal scaling: {stream}"
        );
        assert!(
            !stream.contains("TJ"),
            "trailing TJ adjustments do not produce reliable PDF.js geometry: {stream}"
        );
    }

    #[test]
    fn directly_mapped_painted_glyph_uses_its_unicode_specific_cid() {
        use crate::layout::PlacedGlyph;

        let subset = SubsetFont {
            data: Vec::new(),
            remap: BTreeMap::from([(5, 2)]),
            widths: vec![(2, 500)],
            to_unicode: Vec::new(),
            is_cff: false,
            warning: None,
        };
        let pairs = BTreeSet::from([(5, "A".to_string(), true)]);
        let embedded =
            EmbeddedFont::new(subset, &pairs, &BTreeSet::new()).expect("embedded font state");
        let glyph = PlacedGlyph {
            gid: 5,
            advance_pt: 6.0,
            x_offset_pt: 0.0,
            y_offset_pt: 0.0,
            ch: 'A',
        };

        let cid = painted_cid(&embedded, &glyph);
        assert_ne!(cid, embedded.subset.remap[&5]);
        assert_eq!(embedded.to_unicode[&cid], "A");
        let cmap = String::from_utf8(build_to_unicode(&embedded.to_unicode)).unwrap();
        assert!(cmap.contains(&format!("<{cid:04X}> <0041>")));
    }

    #[test]
    fn visible_text_baseline_is_not_snapped_on_an_off_grid_page_height() {
        use crate::layout::{GlyphRun, PlacedGlyph};

        let face = FaceId(0);
        let subset = SubsetFont {
            data: Vec::new(),
            remap: BTreeMap::from([(5, 2)]),
            widths: vec![(2, 500)],
            to_unicode: Vec::new(),
            is_cff: false,
            warning: None,
        };
        let pairs = BTreeSet::from([(5, "A".to_string(), true)]);
        let visible = BTreeSet::from([(5, 'A')]);
        let embedded = EmbeddedFont::new(subset, &pairs, &visible).expect("embedded font state");
        let page = Page {
            width_pt: 100.0,
            // A non-quarter-point page height distinguishes snapping the
            // top-down layout baseline from snapping the transformed PDF y.
            height_pt: 100.1,
            items: vec![Positioned {
                x_pt: 10.0,
                y_pt: 3.348_388_671_875,
                item: Item::Glyphs(GlyphRun {
                    face,
                    size_pt: 11.0,
                    color: Rgba::opaque(crate::oxml::Rgb::new(0, 0, 0)),
                    base_direction: None,
                    glyphs: vec![PlacedGlyph {
                        gid: 5,
                        advance_pt: 6.0,
                        x_offset_pt: 0.0,
                        y_offset_pt: 0.0,
                        ch: 'A',
                    }],
                    underline: None,
                    strike: None,
                }),
            }],
        };
        let bytes = render_page(&page, &BTreeMap::from([(face, embedded)]), &[face])
            .expect("render visible-text page");
        let stream = String::from_utf8(bytes).expect("ASCII content stream");

        // The top and bottom page origins have different quarter-point phases
        // when the page height is 100.1pt. Preserve the laid-out baseline
        // (100.1 - 3.348388671875 = 96.75161pt) instead of choosing either
        // origin's incompatible snap grid.
        assert!(
            stream.contains("1 0 0 1 10 96.75161 Tm"),
            "an off-grid page height must preserve the laid-out baseline: {stream}"
        );
    }

    #[test]
    fn direct_unicode_pair_does_not_relabel_the_shared_artifact_cid() {
        use crate::layout::{GlyphRun, PlacedGlyph};

        let face = FaceId(0);
        let item = Item::Glyphs(GlyphRun {
            face,
            size_pt: 12.0,
            color: Rgba::opaque(crate::oxml::Rgb::new(0, 0, 0)),
            base_direction: None,
            glyphs: vec![
                PlacedGlyph {
                    gid: 5,
                    advance_pt: 6.0,
                    x_offset_pt: 0.0,
                    y_offset_pt: 0.0,
                    ch: 'A',
                },
                PlacedGlyph {
                    gid: 5,
                    advance_pt: 6.0,
                    x_offset_pt: 0.0,
                    y_offset_pt: 0.0,
                    ch: '\0',
                },
            ],
            underline: None,
            strike: None,
        });
        let mut used = UsedGlyphs::new();
        let mut glyph_count = 0;
        collect_item_glyphs(&item, &mut used, &mut glyph_count, 10).unwrap();
        let usage = &used[&face];

        assert!(
            usage.chars.is_empty(),
            "base CIDs must remain extraction-empty"
        );
        assert!(usage.semantic_pairs.contains(&(5, "A".to_string(), true)));
    }

    #[test]
    fn semantic_actual_text_precompensates_pdfium_rtl_reordering() {
        use crate::layout::{GlyphRun, PlacedGlyph};

        let logical = "مرحبا بالعالم";
        let glyphs: Vec<_> = logical
            .chars()
            .map(|character| PlacedGlyph {
                gid: 5,
                advance_pt: 0.0,
                x_offset_pt: 0.0,
                y_offset_pt: crate::text::semantic::SEMANTIC_START_Y,
                ch: character,
            })
            .collect();
        let semantic = semantic_clusters(&glyphs);
        let pairs: BTreeSet<_> =
            visual_semantic_tokens(&semantic, logical, Some(text_core::Direction::Rtl))
                .unwrap()
                .into_iter()
                .flatten()
                .map(|(gid, source, advance_pt)| (gid, source, advance_pt.abs() > f64::EPSILON))
                .collect();
        let subset = SubsetFont {
            data: Vec::new(),
            remap: BTreeMap::from([(5, 2)]),
            widths: vec![(2, 500)],
            to_unicode: Vec::new(),
            is_cff: false,
            warning: None,
        };
        let embedded = EmbeddedFont::new(subset, &pairs, &BTreeSet::new()).unwrap();
        let face = FaceId(0);
        let page = Page {
            width_pt: 200.0,
            height_pt: 100.0,
            items: vec![Positioned {
                x_pt: 10.0,
                y_pt: 20.0,
                item: Item::Glyphs(GlyphRun {
                    face,
                    size_pt: 12.0,
                    color: Rgba::opaque(crate::oxml::Rgb::new(0, 0, 0)),
                    base_direction: Some(text_core::Direction::Rtl),
                    glyphs,
                    underline: None,
                    strike: None,
                }),
            }],
        };
        let stream = render_page(&page, &BTreeMap::from([(face, embedded)]), &[face])
            .expect("render semantic page");
        let pdfium_precompensated = text_core::bidi_visual_order(logical);
        let utf16 = format!(
            "FEFF{}",
            pdfium_precompensated
                .encode_utf16()
                .map(|unit| format!("{unit:04X}"))
                .collect::<String>()
        )
        .into_bytes();
        assert!(
            stream.windows(utf16.len()).any(|window| window == utf16),
            "PDFium-precompensated /ActualText UTF-16 sequence is absent: {stream:?}"
        );
    }

    #[test]
    fn pdfjs_semantic_order_visualizes_rtl_tokens_without_reversing_words() {
        assert_eq!(pdfjs_semantic_text("مرحبا بالعالم"), "ابحرم ملاعلاب");
        assert_eq!(pdfjs_semantic_text("Total 123 ريال"), "Total 123 لاير");
        assert_eq!(pdfjs_semantic_text("Cafe\u{301} text"), "Cafe\u{301} text");
        assert!(is_rtl_combining_mark('\u{08D3}'));
    }

    #[test]
    fn extended_a_marks_follow_the_bidi_tables_used_for_pdfium_compensation() {
        let source = "ب\u{08CA}\u{08CB}";
        let bidi_visual =
            text_core::bidi_visual_order_with_base(source, Some(text_core::Direction::Rtl));
        let actual_text = pdfium_actual_text(source, Some(text_core::Direction::Rtl));

        assert_eq!(
            actual_text, bidi_visual,
            "U+08CA..U+08D2 are already reversed by the pinned bidi tables"
        );
        assert!(!is_rtl_combining_mark('\u{08CA}'));
        assert!(!is_rtl_combining_mark('\u{08CB}'));
        assert!(is_rtl_combining_mark('\u{08D3}'));
        assert_eq!(
            pdfjs_semantic_tokens(source, Some(text_core::Direction::Rtl)),
            vec!["\u{08CA}\u{08CB}ب"],
            "PDF.js preserves these marks during bidi, so its token needs the inverse order"
        );
    }

    #[test]
    fn semantic_tokens_keep_shaped_cluster_advances() {
        let semantic = vec![
            (5, "e\u{301}".to_string(), 4.5),
            (5, " ".to_string(), 2.0),
            (5, "word".to_string(), 10.0),
        ];
        let tokens = visual_semantic_tokens(&semantic, "e\u{301} word", None).unwrap();

        assert_eq!(
            tokens
                .iter()
                .flatten()
                .map(|(_, source, _)| source.as_str())
                .collect::<Vec<_>>(),
            vec!["e", "\u{301}", " ", "w", "o", "r", "d"]
        );
        assert_eq!(
            tokens
                .iter()
                .flatten()
                .map(|(_, _, advance)| *advance)
                .collect::<Vec<_>>(),
            vec![4.5, 0.0, 2.0, 10.0, 0.0, 0.0, 0.0]
        );

        let standalone = vec![(5, "e\u{301}".to_string(), 4.5)];
        let standalone_tokens = visual_semantic_tokens(&standalone, "e\u{301}", None).unwrap();
        assert_eq!(
            standalone_tokens[0]
                .iter()
                .map(|(_, source, advance)| (source.as_str(), *advance))
                .collect::<Vec<_>>(),
            vec![("e", 4.5), ("\u{301}", 0.0)]
        );
    }

    #[test]
    fn visible_artifact_cids_have_no_to_unicode_mapping() {
        let subset = SubsetFont {
            data: Vec::new(),
            remap: BTreeMap::from([(5, 2)]),
            widths: vec![(2, 500)],
            to_unicode: Vec::new(),
            is_cff: false,
            warning: None,
        };
        let embedded = EmbeddedFont::new(subset, &BTreeSet::new(), &BTreeSet::new()).unwrap();

        assert_eq!(embedded.to_unicode.get(&2), None);
        let cmap = String::from_utf8(build_to_unicode(&embedded.to_unicode)).unwrap();
        assert!(
            !cmap.contains("<0002>"),
            "paint-only artifact CIDs must be absent from ToUnicode: {cmap}"
        );
    }

    #[test]
    fn paint_only_glyphs_fail_closed_when_subset_cannot_be_parsed() {
        use crate::layout::{GlyphRun, PlacedGlyph};

        let face = FaceId(0);
        let subset = SubsetFont {
            data: Vec::new(),
            remap: BTreeMap::from([(5, 2)]),
            widths: vec![(2, 500)],
            to_unicode: Vec::new(),
            is_cff: false,
            warning: None,
        };
        let embedded = EmbeddedFont::new(subset, &BTreeSet::new(), &BTreeSet::new()).unwrap();
        let page = Page {
            width_pt: 100.0,
            height_pt: 100.0,
            items: vec![Positioned {
                x_pt: 10.0,
                y_pt: 20.0,
                item: Item::Glyphs(GlyphRun {
                    face,
                    size_pt: 12.0,
                    color: Rgba::opaque(crate::oxml::Rgb::new(0, 0, 0)),
                    base_direction: None,
                    glyphs: vec![PlacedGlyph {
                        gid: 5,
                        advance_pt: 6.0,
                        x_offset_pt: 0.0,
                        y_offset_pt: 0.0,
                        ch: '\0',
                    }],
                    underline: None,
                    strike: None,
                }),
            }],
        };

        let error = render_page(&page, &BTreeMap::from([(face, embedded)]), &[face])
            .expect_err("invalid paint-only subset must fail conversion");
        assert!(matches!(error, OfficeError::Font(_)));
    }
}

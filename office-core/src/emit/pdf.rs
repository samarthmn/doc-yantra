//! PDF emission via `pdf-writer`: Type0/CIDFontType2 subset fonts (Identity-H),
//! images, links, outline, and **deterministic** output (stable object order,
//! `/ID` from a hash of the input, dates from the document or a fixed literal).

use crate::fonts::subset::{SubsetFont, subset_for_pdf};
use crate::fonts::{FaceId, FontDb};
use crate::layout::{EncodedImage, Item, LaidOutDocument, LinkTarget, Page, Positioned};
use crate::oxml::Rgba;
use crate::profile::Warning;
use crate::{OfficeError, Result};
use pdf_writer::types::{ActionType, AnnotationType, CidFontType, SystemInfo};
use pdf_writer::{Content, Date, Filter, Finish, Name, Pdf, Rect, Ref, Str, TextStr};
use std::collections::{BTreeMap, BTreeSet};
use ttf_parser::GlyphId;

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
    // Defense in depth: a 0-page PDF is invalid per spec and treated as corrupt
    // by viewers. The format converters already reject mismatched inputs before
    // reaching here, so an empty page list is an internal invariant violation.
    if laid.pages.is_empty() {
        return Err(OfficeError::Internal("document produced no pages".into()));
    }

    // 1. Collect used glyphs per face and subset.
    let used = collect_used_glyphs(laid);
    let mut subsets: BTreeMap<FaceId, EmbeddedFont> = BTreeMap::new();
    for (face_id, (gids, chars)) in &used {
        let Some(face) = fonts.face(*face_id) else {
            continue;
        };
        let subset = subset_for_pdf(face, gids, chars)?;
        if let Some(w) = &subset.warning
            && !warnings.contains(w)
        {
            warnings.push(w.clone());
        }
        subsets.insert(*face_id, EmbeddedFont::new(subset));
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
        f.to_unicode = alloc();
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
    let mut page_annots: Vec<Vec<(Ref, Rect, LinkTarget)>> = Vec::new();
    for page in &laid.pages {
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
                annots.push((alloc(), rect, target.clone()));
            }
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
        write_font(&mut pdf, subsets.get(face_id).unwrap());
    }

    // 4. Images.
    for (img, (main, smask)) in laid.images.iter().zip(&image_refs) {
        write_image(&mut pdf, img, *main, *smask);
    }

    // 5. Pages + content.
    for (i, page) in laid.pages.iter().enumerate() {
        let content = render_page(page, &subsets, &face_order);
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
            match target {
                LinkTarget::Url(url) => {
                    a.action()
                        .action_type(ActionType::Uri)
                        .uri(Str(url.as_bytes()));
                }
                LinkTarget::Page(p) => {
                    let target_page = (*p as usize).min(page_ids.len().saturating_sub(1));
                    let _ = page_idx;
                    a.action()
                        .action_type(ActionType::GoTo)
                        .destination()
                        .page(page_ids[target_page])
                        .xyz(0.0, laid.pages[target_page].height_pt as f32, None);
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

/// Per-face embedding state.
struct EmbeddedFont {
    subset: SubsetFont,
    cid_widths: BTreeMap<u16, f32>,
    type0: Ref,
    cid: Ref,
    descriptor: Ref,
    file: Ref,
    to_unicode: Ref,
}

impl EmbeddedFont {
    fn new(subset: SubsetFont) -> EmbeddedFont {
        let cid_widths = subset.widths.iter().map(|&(c, w)| (c, w as f32)).collect();
        EmbeddedFont {
            subset,
            cid_widths,
            type0: Ref::new(1),
            cid: Ref::new(1),
            descriptor: Ref::new(1),
            file: Ref::new(1),
            to_unicode: Ref::new(1),
        }
    }
}

fn collect_used_glyphs(
    laid: &LaidOutDocument,
) -> BTreeMap<FaceId, (BTreeSet<GlyphId>, BTreeMap<GlyphId, char>)> {
    let mut used: BTreeMap<FaceId, (BTreeSet<GlyphId>, BTreeMap<GlyphId, char>)> = BTreeMap::new();
    for page in &laid.pages {
        for pos in &page.items {
            if let Item::Glyphs(run) = &pos.item {
                let entry = used.entry(run.face).or_default();
                for g in &run.glyphs {
                    let gid = GlyphId(g.gid);
                    entry.0.insert(gid);
                    entry.1.entry(gid).or_insert(g.ch);
                }
            }
        }
    }
    used
}

fn write_font(pdf: &mut Pdf, f: &EmbeddedFont) {
    // Type0.
    pdf.type0_font(f.type0)
        .base_font(Name(b"OfficeCoreSubset"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(f.cid)
        .to_unicode(f.to_unicode);

    // CIDFontType2.
    {
        let mut cid = pdf.cid_font(f.cid);
        cid.subtype(CidFontType::Type2)
            .base_font(Name(b"OfficeCoreSubset"))
            .system_info(SystemInfo {
                registry: Str(b"Adobe"),
                ordering: Str(b"Identity"),
                supplement: 0,
            })
            .font_descriptor(f.descriptor)
            .cid_to_gid_map_predefined(Name(b"Identity"));
        if let (Some(&min), Some(&max)) =
            (f.cid_widths.keys().next(), f.cid_widths.keys().next_back())
        {
            let widths: Vec<f32> = (min..=max)
                .map(|c| f.cid_widths.get(&c).copied().unwrap_or(0.0))
                .collect();
            cid.widths().consecutive(min, widths);
        }
        cid.finish();
    }

    // Descriptor + font file.
    pdf.font_descriptor(f.descriptor)
        .name(Name(b"OfficeCoreSubset"))
        .flags(pdf_writer::types::FontFlags::NON_SYMBOLIC)
        .bbox(Rect::new(-200.0, -200.0, 1000.0, 900.0))
        .italic_angle(0.0)
        .ascent(800.0)
        .descent(-200.0)
        .cap_height(700.0)
        .stem_v(80.0)
        .font_file2(f.file);
    pdf.stream(
        f.file,
        &miniz_oxide::deflate::compress_to_vec_zlib(&f.subset.data, 6),
    )
    .filter(Filter::FlateDecode);

    // ToUnicode CMap.
    let cmap = build_to_unicode(&f.subset);
    pdf.stream(
        f.to_unicode,
        &miniz_oxide::deflate::compress_to_vec_zlib(&cmap, 6),
    )
    .filter(Filter::FlateDecode);
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
) -> Vec<u8> {
    let mut content = Content::new();
    let h = page.height_pt;
    for pos in &page.items {
        match &pos.item {
            Item::Rect {
                w_pt,
                h_pt,
                fill,
                stroke,
            } => draw_rect(&mut content, pos, h, *w_pt, *h_pt, fill, stroke),
            Item::Line {
                dx_pt,
                dy_pt,
                stroke,
            } => draw_line(&mut content, pos, h, *dx_pt, *dy_pt, stroke),
            Item::Glyphs(run) => draw_glyphs(&mut content, pos, h, run, subsets, face_order),
            Item::Image {
                w_pt,
                h_pt,
                image_id,
            } => draw_image(&mut content, pos, h, *w_pt, *h_pt, image_id.0),
            Item::Link { .. } => {} // handled as annotations
        }
    }
    content.finish().to_vec()
}

fn set_fill(content: &mut Content, c: Rgba) {
    content.set_fill_rgb(c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
}
fn set_stroke(content: &mut Content, c: Rgba) {
    content.set_stroke_rgb(c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
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
            set_stroke(content, s.color);
            content.set_line_width(s.width_pt as f32);
            content.fill_nonzero_and_stroke();
        }
        (Some(f), None) => {
            set_fill(content, *f);
            content.fill_nonzero();
        }
        (None, Some(s)) => {
            set_stroke(content, s.color);
            content.set_line_width(s.width_pt as f32);
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
    set_stroke(content, stroke.color);
    content.set_line_width(stroke.width_pt as f32);
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
) {
    let Some(font) = subsets.get(&run.face) else {
        return;
    };
    let idx = face_order.iter().position(|f| *f == run.face).unwrap_or(0);
    let baseline = page_h - pos.y_pt;

    content.begin_text();
    set_fill(content, run.color);
    content.set_font(Name(font_res_name(idx).as_bytes()), run.size_pt as f32);
    content.set_text_matrix([1.0, 0.0, 0.0, 1.0, pos.x_pt as f32, baseline as f32]);

    let mut tj = content.show_positioned();
    let mut items = tj.items();
    for g in &run.glyphs {
        let cid = font.subset.remap.get(&g.gid).copied().unwrap_or(0);
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
fn build_to_unicode(subset: &SubsetFont) -> Vec<u8> {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let mut entries: Vec<(u16, String)> = subset
        .to_unicode
        .iter()
        .map(|(cid, txt)| {
            let utf16: String = txt.encode_utf16().map(|u| format!("{u:04X}")).collect();
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

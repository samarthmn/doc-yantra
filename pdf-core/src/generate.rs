//! Zone 1 — PDF generation: CSV tables, Markdown documents, plain text, and
//! image-per-page PDFs. Uses standard-14 fonts for WinAnsi and embedded CID
//! fonts for supported Unicode runs; replaces the Chromium html-to-pdf route.

use std::collections::BTreeSet;

use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::embed_font::{
    EmbeddedFont, FontChoice, FontSession, PdfFontRefs, is_winansi, resource_name,
};
use crate::error::{PdfCoreError, Result};
use crate::fonts::StandardFont;

/// Target page size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageSize {
    /// 612 x 792 pt
    #[default]
    Letter,
    /// 792 x 612 pt (Letter rotated; wide tables)
    LetterLandscape,
    /// 595 x 842 pt
    A4,
}

impl PageSize {
    pub fn dimensions_pt(self) -> (f64, f64) {
        match self {
            PageSize::Letter => (612.0, 792.0),
            PageSize::LetterLandscape => (792.0, 612.0),
            PageSize::A4 => (595.0, 842.0),
        }
    }
}

/// Options shared by the document generators.
#[derive(Debug, Clone)]
pub struct DocOptions {
    pub page_size: PageSize,
    pub margin_pt: f64,
    pub base_font_size_pt: f64,
    /// Optional title rendered as a heading before the content.
    pub title: Option<String>,
}

impl Default for DocOptions {
    fn default() -> Self {
        DocOptions {
            page_size: PageSize::Letter,
            margin_pt: 54.0,
            base_font_size_pt: 10.0,
            title: None,
        }
    }
}

/// Sane clamp range for the base font size (points).
const MIN_BASE_FONT_PT: f64 = 4.0;
const MAX_BASE_FONT_PT: f64 = 72.0;

/// Validates and clamps [`DocOptions`] into ranges every generator can
/// handle: font size [`MIN_BASE_FONT_PT`]..=[`MAX_BASE_FONT_PT`] and margin
/// 0..=(shortest page edge)/3. Non-finite or non-positive values are still
/// rejected as invalid input rather than silently corrected.
fn effective_opts(opts: &DocOptions) -> Result<DocOptions> {
    let (page_w, page_h) = opts.page_size.dimensions_pt();
    if !opts.margin_pt.is_finite() || opts.margin_pt < 0.0 {
        return Err(PdfCoreError::InvalidInput(format!(
            "margin_pt must be a non-negative finite number, got {}",
            opts.margin_pt
        )));
    }
    if !opts.base_font_size_pt.is_finite() || opts.base_font_size_pt <= 0.0 {
        return Err(PdfCoreError::InvalidInput(format!(
            "base_font_size_pt must be positive, got {}",
            opts.base_font_size_pt
        )));
    }
    let mut out = opts.clone();
    out.base_font_size_pt = opts
        .base_font_size_pt
        .clamp(MIN_BASE_FONT_PT, MAX_BASE_FONT_PT);
    out.margin_pt = opts.margin_pt.min(page_w.min(page_h) / 3.0);
    Ok(out)
}

// ---------------------------------------------------------------------------
// WinAnsi-safe text sanitization
// ---------------------------------------------------------------------------

/// Maps text to a printable-ASCII string safe for the standard-14 fonts.
/// Common typography (curly quotes, dashes, ellipsis) is folded to ASCII
/// equivalents; anything else non-ASCII becomes '?'. Newlines/tabs become
/// spaces (single-line prose context).
fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        sanitize_char_into(c, &mut out, ' ');
    }
    out
}

/// Like [`sanitize`] but preserves nothing multi-line either — used for code
/// lines where tabs should become spaces but the caller has already split on
/// newlines.
fn sanitize_code_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if c == '\t' {
            out.push_str("    ");
        } else {
            sanitize_char_into(c, &mut out, ' ');
        }
    }
    out
}

/// Keeps the legacy byte-for-byte WinAnsi path, while preserving Unicode for
/// runs that need an embedded font.
fn sanitize_run(text: &str) -> String {
    if is_winansi(text) {
        return sanitize(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        sanitize_unicode_char_into(c, &mut out, ' ');
    }
    out
}

fn sanitize_code_run(line: &str) -> String {
    if is_winansi(line) {
        return sanitize_code_line(line);
    }
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if c == '\t' {
            out.push_str("    ");
        } else {
            sanitize_unicode_char_into(c, &mut out, ' ');
        }
    }
    out
}

fn sanitize_unicode_char_into(c: char, out: &mut String, ws: char) {
    match c {
        '\n' | '\r' | '\t' => out.push(ws),
        '\u{200B}' | '\u{FEFF}' => {}
        c if c.is_control() => {}
        _ => out.push(c),
    }
}

fn sanitize_char_into(c: char, out: &mut String, ws: char) {
    match c {
        ' '..='~' => out.push(c),
        '\n' | '\r' | '\t' => out.push(ws),
        '\u{2018}' | '\u{2019}' | '\u{02BC}' => out.push('\''),
        '\u{201C}' | '\u{201D}' | '\u{201E}' => out.push('"'),
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2212}' => out.push('-'),
        '\u{2014}' | '\u{2015}' => out.push_str("--"),
        '\u{2026}' => out.push_str("..."),
        '\u{00A0}' | '\u{2009}' | '\u{200A}' | '\u{2002}' | '\u{2003}' => out.push(' '),
        '\u{2022}' | '\u{00B7}' | '\u{2027}' => out.push('-'),
        '\u{200B}' | '\u{FEFF}' => {}
        c if c.is_control() => {}
        _ => out.push('?'),
    }
}

// ---------------------------------------------------------------------------
// Font bookkeeping
// ---------------------------------------------------------------------------

const FONT_COUNT: usize = 6;

const ALL_FONTS: [StandardFont; FONT_COUNT] = [
    StandardFont::Helvetica,
    StandardFont::HelveticaBold,
    StandardFont::HelveticaOblique,
    StandardFont::HelveticaBoldOblique,
    StandardFont::Courier,
    StandardFont::CourierBold,
];

/// Resource names /F0../F5, aligned with [`ALL_FONTS`].
const FONT_RES_NAMES: [&[u8]; FONT_COUNT] = [b"F0", b"F1", b"F2", b"F3", b"F4", b"F5"];

fn font_index(font: StandardFont) -> usize {
    match font {
        StandardFont::Helvetica => 0,
        StandardFont::HelveticaBold => 1,
        StandardFont::HelveticaOblique => 2,
        StandardFont::HelveticaBoldOblique => 3,
        StandardFont::Courier => 4,
        StandardFont::CourierBold => 5,
    }
}

// ---------------------------------------------------------------------------
// Internal layout engine
// ---------------------------------------------------------------------------

/// Raw RGB8 samples of an image destined for a page's XObject.
struct RgbSamples {
    width: u32,
    height: u32,
    samples: Vec<u8>,
}

/// A finished page: its (uncompressed) content stream, which fonts it used,
/// and optionally one embedded image.
struct PageData {
    content: Vec<u8>,
    fonts_used: [bool; FONT_COUNT],
    embedded_fonts_used: BTreeSet<usize>,
    image: Option<RgbSamples>,
}

/// A styled inline run: text set in one font (size comes from the block).
#[derive(Debug, Clone)]
struct Run {
    font: StandardFont,
    text: String,
}

/// A logical run after its standard-vs-embedded choice has been frozen.
#[derive(Debug, Clone)]
struct ResolvedRun {
    font: StandardFont,
    choice: FontChoice,
    text: String,
}

/// Minimal top-down layout engine shared by the csv/markdown/text generators.
/// Tracks a cursor `y` (distance from page bottom), starts new pages when the
/// cursor would cross the bottom margin, and exposes small drawing helpers.
struct LayoutEngine {
    pages: Vec<PageData>,
    content: Content,
    fonts_used: [bool; FONT_COUNT],
    embedded_fonts_used: BTreeSet<usize>,
    font_session: FontSession,
    page_w: f64,
    page_h: f64,
    margin: f64,
    /// Baseline cursor. Starts at `page_h - margin`, decreases downward.
    y: f64,
}

impl LayoutEngine {
    fn new(opts: &DocOptions) -> Result<Self> {
        let opts = effective_opts(opts)?;
        let (page_w, page_h) = opts.page_size.dimensions_pt();
        Ok(LayoutEngine {
            pages: Vec::new(),
            content: Content::new(),
            fonts_used: [false; FONT_COUNT],
            embedded_fonts_used: BTreeSet::new(),
            font_session: FontSession::new()?,
            page_w,
            page_h,
            margin: opts.margin_pt,
            y: page_h - opts.margin_pt,
        })
    }

    fn usable_width(&self) -> f64 {
        self.page_w - 2.0 * self.margin
    }

    fn left(&self) -> f64 {
        self.margin
    }

    /// Finishes the current page and starts a fresh one.
    fn break_page(&mut self) {
        let content = std::mem::replace(&mut self.content, Content::new());
        let fonts_used = std::mem::replace(&mut self.fonts_used, [false; FONT_COUNT]);
        let embedded_fonts_used = std::mem::take(&mut self.embedded_fonts_used);
        self.pages.push(PageData {
            content: content.finish().into_vec(),
            fonts_used,
            embedded_fonts_used,
            image: None,
        });
        self.y = self.page_h - self.margin;
    }

    /// Starts a new page if `needed` points of vertical room are not left.
    fn ensure_room(&mut self, needed: f64) {
        if self.y - needed < self.margin - 0.01 {
            self.break_page();
        }
    }

    fn advance(&mut self, dy: f64) {
        self.y -= dy;
    }

    /// Draws one already-sanitized text run at an absolute baseline.
    fn draw_text_line(
        &mut self,
        font: StandardFont,
        choice: &FontChoice,
        size: f64,
        x: f64,
        y: f64,
        text: &str,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        match choice {
            FontChoice::Standard => {
                self.fonts_used[font_index(font)] = true;
                self.content.begin_text();
                self.content
                    .set_font(Name(FONT_RES_NAMES[font_index(font)]), size as f32);
                self.content.next_line(x as f32, y as f32);
                self.content.show(Str(text.as_bytes()));
                self.content.end_text();
            }
            FontChoice::Embedded(face) => {
                let encoded = self.font_session.encode(face, text)?;
                self.embedded_fonts_used.insert(encoded.resource_index);
                let name = resource_name(encoded.resource_index);
                self.content.begin_text();
                self.content.set_font(Name(name.as_bytes()), size as f32);
                self.content.next_line(x as f32, y as f32);
                self.content.show(Str(&encoded.bytes));
                self.content.end_text();
            }
        }
        Ok(())
    }

    /// Draws a horizontal rule from `x1` to `x2` at height `y`.
    fn draw_rule(&mut self, x1: f64, x2: f64, y: f64, line_width: f64) {
        self.content.save_state();
        self.content.set_line_width(line_width as f32);
        self.content.move_to(x1 as f32, y as f32);
        self.content.line_to(x2 as f32, y as f32);
        self.content.stroke();
        self.content.restore_state();
    }

    /// Fills a rectangle with a gray level (0 = black, 1 = white).
    fn draw_rect_fill(&mut self, x: f64, y: f64, w: f64, h: f64, gray: f64) {
        self.content.save_state();
        self.content.set_fill_gray(gray as f32);
        self.content.rect(x as f32, y as f32, w as f32, h as f32);
        self.content.fill_nonzero();
        self.content.restore_state();
    }

    /// Draws a sequence of styled runs on one baseline, advancing x per run.
    fn draw_runs_line(&mut self, runs: &[ResolvedRun], size: f64, x: f64, y: f64) -> Result<()> {
        let mut cx = x;
        for run in runs {
            self.draw_text_line(run.font, &run.choice, size, cx, y, &run.text)?;
            cx += run.choice.text_width_pt(&run.text, run.font, size)?;
        }
        Ok(())
    }

    /// Word-wraps styled runs into the usable width (minus `indent`) and emits
    /// them line by line with pagination. `marker` (text, x) is drawn once on
    /// the first line (list bullets / numbers).
    fn emit_paragraph(
        &mut self,
        runs: &[Run],
        size: f64,
        indent: f64,
        leading_factor: f64,
        marker: Option<(String, StandardFont)>,
    ) -> Result<()> {
        let max_w = (self.usable_width() - indent).max(size);
        let mut lines = wrap_runs(&self.font_session, runs, size, max_w)?;
        if lines.is_empty() {
            if marker.is_none() {
                return Ok(());
            }
            lines.push(Vec::new());
        }
        let lead = size * leading_factor;
        for (i, line) in lines.iter().enumerate() {
            self.ensure_room(lead);
            self.advance(lead);
            let baseline = self.y;
            if i == 0
                && let Some((mtext, mfont)) = &marker
            {
                let mw = mfont.text_width_pt(mtext, size);
                let mx = (self.left() + indent - mw - 4.0).max(self.left());
                let mtext = mtext.clone();
                let mfont = *mfont;
                self.draw_text_line(mfont, &FontChoice::Standard, size, mx, baseline, &mtext)?;
            }
            self.draw_runs_line(line, size, self.left() + indent, baseline)?;
        }
        Ok(())
    }

    /// Renders `opts.title` (if any) as a bold heading.
    fn emit_title(&mut self, opts: &DocOptions) -> Result<()> {
        if let Some(title) = &opts.title {
            let text = sanitize_run(title);
            if !text.trim().is_empty() {
                let size = opts.base_font_size_pt * 1.8;
                let runs = [Run {
                    font: StandardFont::HelveticaBold,
                    text,
                }];
                self.emit_paragraph(&runs, size, 0.0, 1.25, None)?;
                self.advance(opts.base_font_size_pt * 0.9);
            }
        }
        Ok(())
    }

    /// Finishes the current page and assembles the final PDF bytes.
    fn finish(mut self) -> Result<Vec<u8>> {
        self.break_page();
        let embedded_fonts = self.font_session.finish()?;
        assemble(self.pages, self.page_w, self.page_h, embedded_fonts)
    }
}

/// Greedy word-wrap over styled runs; measures every token with its own font.
/// Words wider than `max_w` are hard-broken by characters.
fn wrap_runs(
    session: &FontSession,
    runs: &[Run],
    size: f64,
    max_w: f64,
) -> Result<Vec<Vec<ResolvedRun>>> {
    let resolved: Vec<ResolvedRun> = runs
        .iter()
        .map(|run| {
            Ok(ResolvedRun {
                font: run.font,
                choice: session.resolve(&run.text)?,
                text: run.text.clone(),
            })
        })
        .collect::<Result<_>>()?;
    wrap_resolved_runs(&resolved, size, max_w)
}

fn wrap_resolved_runs(
    runs: &[ResolvedRun],
    size: f64,
    max_w: f64,
) -> Result<Vec<Vec<ResolvedRun>>> {
    // Tokenize into (font, token) where a token is a maximal run of spaces or
    // of non-space characters within a single styled run.
    let mut tokens: Vec<(StandardFont, FontChoice, String)> = Vec::new();
    for run in runs {
        let mut cur = String::new();
        let mut cur_is_space: Option<bool> = None;
        for c in run.text.chars() {
            let is_space = c == ' ';
            if cur_is_space != Some(is_space) && !cur.is_empty() {
                tokens.push((run.font, run.choice.clone(), std::mem::take(&mut cur)));
            }
            cur_is_space = Some(is_space);
            cur.push(c);
        }
        if !cur.is_empty() {
            tokens.push((run.font, run.choice.clone(), cur));
        }
    }

    let mut lines: Vec<Vec<ResolvedRun>> = Vec::new();
    let mut cur: Vec<ResolvedRun> = Vec::new();
    let mut cur_w = 0.0_f64;

    let flush = |cur: &mut Vec<ResolvedRun>, cur_w: &mut f64, lines: &mut Vec<Vec<ResolvedRun>>| {
        // Trim trailing spaces from the line.
        while let Some(last) = cur.last_mut() {
            let trimmed = last.text.trim_end_matches(' ').len();
            if trimmed == last.text.len() {
                break;
            }
            last.text.truncate(trimmed);
            if last.text.is_empty() {
                cur.pop();
            }
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(cur));
        }
        *cur_w = 0.0;
    };

    for (font, choice, tok) in tokens {
        let is_space = tok.starts_with(' ');
        let w = choice.text_width_pt(&tok, font, size)?;
        if is_space {
            // Never start a line with spaces; allow trailing spaces to
            // overflow (they get trimmed on flush).
            if !cur.is_empty() {
                push_resolved_run(&mut cur, font, choice, &tok);
                cur_w += w;
            }
            continue;
        }
        if cur_w + w > max_w + 0.01 && !cur.is_empty() {
            flush(&mut cur, &mut cur_w, &mut lines);
        }
        if w > max_w + 0.01 {
            // Hard-break an over-long word by characters.
            let mut piece = String::new();
            let mut piece_w = 0.0;
            for c in tok.chars() {
                let cw = choice.char_width_pt(c, font, size)?;
                if piece_w + cw > max_w + 0.01 && !piece.is_empty() {
                    lines.push(vec![ResolvedRun {
                        font,
                        choice: choice.clone(),
                        text: std::mem::take(&mut piece),
                    }]);
                    piece_w = 0.0;
                }
                piece.push(c);
                piece_w += cw;
            }
            if !piece.is_empty() {
                push_resolved_run(&mut cur, font, choice, &piece);
                cur_w += piece_w;
            }
            continue;
        }
        push_resolved_run(&mut cur, font, choice, &tok);
        cur_w += w;
    }
    flush(&mut cur, &mut cur_w, &mut lines);
    Ok(lines)
}

fn push_resolved_run(
    line: &mut Vec<ResolvedRun>,
    font: StandardFont,
    choice: FontChoice,
    text: &str,
) {
    if let Some(last) = line.last_mut()
        && last.font == font
        && last.choice == choice
    {
        last.text.push_str(text);
        return;
    }
    line.push(ResolvedRun {
        font,
        choice,
        text: text.to_string(),
    });
}

/// Appends text to the last run when the font matches, else pushes a new run.
fn push_run(line: &mut Vec<Run>, font: StandardFont, text: &str) {
    if let Some(last) = line.last_mut()
        && last.font == font
    {
        last.text.push_str(text);
        return;
    }
    line.push(Run {
        font,
        text: text.to_string(),
    });
}

// ---------------------------------------------------------------------------
// Document assembly (pdf-writer)
// ---------------------------------------------------------------------------

/// Assembles finished pages into a complete PDF: catalog, page tree, shared
/// Type1 font objects, per-page content streams (uncompressed) and image
/// XObjects (raw RGB8, no filter).
fn assemble(
    pages: Vec<PageData>,
    page_w: f64,
    page_h: f64,
    embedded_fonts: Vec<EmbeddedFont>,
) -> Result<Vec<u8>> {
    if pages.iter().any(|page| {
        page.embedded_fonts_used
            .iter()
            .any(|&index| index >= embedded_fonts.len())
    }) {
        return Err(PdfCoreError::InvalidInput(
            "page references an unavailable embedded font".to_string(),
        ));
    }
    let mut pdf = Pdf::new();
    let mut next_id = 1;
    let mut alloc = || {
        let r = Ref::new(next_id);
        next_id += 1;
        r
    };

    let catalog_id = alloc();
    let tree_id = alloc();

    // One shared font object per font used anywhere in the document.
    let mut font_ids: [Option<Ref>; FONT_COUNT] = [None; FONT_COUNT];
    for (i, font_id) in font_ids.iter_mut().enumerate() {
        if pages.iter().any(|p| p.fonts_used[i]) {
            *font_id = Some(alloc());
        }
    }

    let embedded_font_refs: Vec<PdfFontRefs> = embedded_fonts
        .iter()
        .map(|_| PdfFontRefs {
            type0: alloc(),
            descendant: alloc(),
            descriptor: alloc(),
            file: alloc(),
            to_unicode: alloc(),
        })
        .collect();

    let page_ids: Vec<Ref> = pages.iter().map(|_| alloc()).collect();
    let content_ids: Vec<Ref> = pages.iter().map(|_| alloc()).collect();
    let image_ids: Vec<Option<Ref>> = pages
        .iter()
        .map(|p| p.image.as_ref().map(|_| alloc()))
        .collect();

    pdf.catalog(catalog_id).pages(tree_id);
    pdf.pages(tree_id)
        .kids(page_ids.iter().copied())
        .count(pages.len() as i32);

    for i in 0..FONT_COUNT {
        if let Some(fid) = font_ids[i] {
            pdf.type1_font(fid)
                .base_font(Name(ALL_FONTS[i].base_name().as_bytes()));
        }
    }
    for (font, refs) in embedded_fonts.iter().zip(&embedded_font_refs) {
        font.write_pdf_writer(&mut pdf, *refs);
    }

    let media_box = Rect::new(0.0, 0.0, page_w as f32, page_h as f32);
    for (i, page) in pages.iter().enumerate() {
        let mut p = pdf.page(page_ids[i]);
        p.media_box(media_box);
        p.parent(tree_id);
        p.contents(content_ids[i]);
        {
            let mut resources = p.resources();
            if page.fonts_used.iter().any(|&u| u) || !page.embedded_fonts_used.is_empty() {
                let mut fonts = resources.fonts();
                for j in 0..FONT_COUNT {
                    if page.fonts_used[j]
                        && let Some(fid) = font_ids[j]
                    {
                        fonts.pair(Name(FONT_RES_NAMES[j]), fid);
                    }
                }
                for &index in &page.embedded_fonts_used {
                    let name = resource_name(index);
                    fonts.pair(Name(name.as_bytes()), embedded_font_refs[index].type0);
                }
                fonts.finish();
            }
            if let Some(iid) = image_ids[i] {
                resources.x_objects().pair(Name(b"Im0"), iid);
            }
        }
        p.finish();

        pdf.stream(content_ids[i], &page.content);

        if let (Some(img), Some(iid)) = (&page.image, image_ids[i]) {
            let mut xobj = pdf.image_xobject(iid, &img.samples);
            xobj.width(img.width as i32);
            xobj.height(img.height as i32);
            xobj.color_space().device_rgb();
            xobj.bits_per_component(8);
            xobj.finish();
        }
    }

    Ok(pdf.finish())
}

// ---------------------------------------------------------------------------
// text_to_pdf
// ---------------------------------------------------------------------------

/// Renders plain text with wrapping and pagination.
pub fn text_to_pdf(text: &str, opts: &DocOptions) -> Result<Vec<u8>> {
    let opts = effective_opts(opts)?;
    let mut eng = LayoutEngine::new(&opts)?;
    eng.emit_title(&opts)?;
    let size = opts.base_font_size_pt;

    // Blank lines separate paragraphs; consecutive non-blank lines re-flow.
    let mut paragraph = String::new();
    let flush_para = |eng: &mut LayoutEngine, paragraph: &mut String| -> Result<()> {
        let cleaned = sanitize_run(paragraph);
        *paragraph = String::new();
        if cleaned.trim().is_empty() {
            return Ok(());
        }
        let runs = [Run {
            font: StandardFont::Helvetica,
            text: cleaned,
        }];
        eng.emit_paragraph(&runs, size, 0.0, 1.3, None)?;
        eng.advance(size * 0.65);
        Ok(())
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            flush_para(&mut eng, &mut paragraph)?;
        } else {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(line.trim_end());
        }
    }
    flush_para(&mut eng, &mut paragraph)?;

    eng.finish()
}

// ---------------------------------------------------------------------------
// csv_to_pdf
// ---------------------------------------------------------------------------

/// Renders CSV data as a paginated table (header row repeated per page).
pub fn csv_to_pdf(csv_bytes: &[u8], opts: &DocOptions) -> Result<Vec<u8>> {
    let opts = effective_opts(opts)?;
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .from_reader(csv_bytes);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for record in reader.records() {
        let record = record?;
        rows.push(record.iter().map(sanitize_run).collect());
    }
    if rows.is_empty() {
        return Err(PdfCoreError::InvalidInput(
            "CSV input contains no rows".to_string(),
        ));
    }

    let mut eng = LayoutEngine::new(&opts)?;
    eng.emit_title(&opts)?;
    render_table(&mut eng, &rows, opts.base_font_size_pt)?;

    eng.finish()
}

// ---------------------------------------------------------------------------
// Shared table layout (CSV + GFM tables)
// ---------------------------------------------------------------------------

/// A cell wraps to at most this many lines; overflow ellipsizes the last one.
const MAX_CELL_LINES: usize = 8;

/// Tables never auto-shrink below this font size.
const TABLE_MIN_FONT_PT: f64 = 4.0;

/// Per-column minimum width at `size`: cell padding plus roughly "xx..." in
/// the widest (bold) face — the smallest column that still shows content.
fn table_col_floor(size: f64) -> f64 {
    size + StandardFont::HelveticaBold.text_width_pt("xx...", size)
}

/// Natural (content-driven) column widths at `size`: the widest cell per
/// column (header row measured bold) plus padding on both sides.
fn table_natural_widths(rows: &[Vec<ResolvedRun>], ncols: usize, size: f64) -> Result<Vec<f64>> {
    let pad = size * 0.5;
    let mut widths = vec![2.0 * pad; ncols];
    for row in rows {
        for (ci, cell) in row.iter().enumerate() {
            let w = cell.choice.text_width_pt(&cell.text, cell.font, size)? + 2.0 * pad;
            if w > widths[ci] {
                widths[ci] = w;
            }
        }
    }
    Ok(widths)
}

/// Squeezes `natural` column widths into `usable` points: widths shrink
/// proportionally but never below `min(floor, natural)` per column. When even
/// the floors overflow (extreme column counts at minimum font size), the
/// floors themselves scale down as a last resort.
fn squeeze_widths(natural: &[f64], floor: f64, usable: f64) -> Vec<f64> {
    let total: f64 = natural.iter().sum();
    if total <= usable {
        return natural.to_vec();
    }
    let floors: Vec<f64> = natural.iter().map(|&w| w.min(floor)).collect();
    let floor_sum: f64 = floors.iter().sum();
    if floor_sum >= usable {
        let scale = usable / floor_sum.max(f64::MIN_POSITIVE);
        return floors.iter().map(|w| w * scale).collect();
    }
    // Iterative proportional squeeze: columns whose share would dip below
    // their floor get pinned there; the rest re-share the remaining width.
    let n = natural.len();
    let mut pinned = vec![false; n];
    loop {
        let pinned_sum: f64 = (0..n).filter(|&i| pinned[i]).map(|i| floors[i]).sum();
        let flex_nat: f64 = (0..n).filter(|&i| !pinned[i]).map(|i| natural[i]).sum();
        if flex_nat <= 0.0 {
            return floors;
        }
        let scale = (usable - pinned_sum) / flex_nat;
        let mut changed = false;
        for i in 0..n {
            if !pinned[i] && natural[i] * scale < floors[i] - 1e-9 {
                pinned[i] = true;
                changed = true;
            }
        }
        if !changed {
            return (0..n)
                .map(|i| {
                    if pinned[i] {
                        floors[i]
                    } else {
                        natural[i] * scale
                    }
                })
                .collect();
        }
    }
}

/// Picks the table font size and column widths for `usable` points of width.
/// Natural widths are used when they fit; otherwise columns squeeze down to a
/// readable floor; and when even the floors cannot fit, the whole table's
/// font shrinks by the needed factor (never below [`TABLE_MIN_FONT_PT`]).
fn table_layout(
    rows: &[Vec<ResolvedRun>],
    ncols: usize,
    base_size: f64,
    usable: f64,
) -> Result<(f64, Vec<f64>)> {
    let mut size = base_size;
    let mut natural = table_natural_widths(rows, ncols, size)?;
    let floors_total: f64 = natural.iter().map(|&w| w.min(table_col_floor(size))).sum();
    if floors_total > usable {
        // All metrics are linear in font size, so this factor is exact.
        size = (size * usable / floors_total).max(TABLE_MIN_FONT_PT);
        natural = table_natural_widths(rows, ncols, size)?;
    }
    let widths = squeeze_widths(&natural, table_col_floor(size), usable);
    Ok((size, widths))
}

/// Wraps one cell into lines that fit `avail_w`, capping at
/// [`MAX_CELL_LINES`] with a trailing ellipsis when content is cut.
fn wrap_cell(cell: &ResolvedRun, size: f64, avail_w: f64) -> Result<Vec<String>> {
    if cell.text.is_empty() {
        return Ok(Vec::new());
    }
    let mut lines: Vec<String> = wrap_resolved_runs(std::slice::from_ref(cell), size, avail_w)?
        .into_iter()
        .map(|line| line.into_iter().map(|r| r.text).collect())
        .collect();
    if lines.len() > MAX_CELL_LINES {
        lines.truncate(MAX_CELL_LINES);
        let last = lines.last_mut().expect("MAX_CELL_LINES > 0");
        *last = ellipsize(last, cell, size, avail_w)?;
    }
    Ok(lines)
}

/// Trims `text` so that it plus a trailing "..." fits in `avail_w` points.
fn ellipsize(text: &str, run: &ResolvedRun, size: f64, avail_w: f64) -> Result<String> {
    let ell_w = run.choice.text_width_pt("...", run.font, size)?;
    let mut out = String::new();
    let mut w = 0.0;
    for c in text.chars() {
        let cw = run.choice.char_width_pt(c, run.font, size)?;
        if w + cw + ell_w > avail_w + 0.01 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push_str("...");
    Ok(out)
}

/// Draws `rows` as a zebra-striped table with a bold header repeated on
/// every page. Cells word-wrap within their column; the row grows to its
/// tallest cell.
fn render_table(eng: &mut LayoutEngine, rows: &[Vec<String>], base_size: f64) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let resolved_rows: Vec<Vec<ResolvedRun>> = rows
        .iter()
        .enumerate()
        .map(|(ri, row)| {
            let font = if ri == 0 {
                StandardFont::HelveticaBold
            } else {
                StandardFont::Helvetica
            };
            row.iter()
                .map(|text| {
                    Ok(ResolvedRun {
                        font,
                        choice: eng.font_session.resolve(text)?,
                        text: text.clone(),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<_>>()?;
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0).max(1);
    let usable = eng.usable_width();
    let (size, widths) = table_layout(&resolved_rows, ncols, base_size, usable)?;
    let pad = size * 0.5;
    let lead = size * 1.2;
    let table_w: f64 = widths.iter().sum();
    let left = eng.left();

    let wrap_row = |cells: &[ResolvedRun]| -> Result<Vec<Vec<String>>> {
        widths
            .iter()
            .enumerate()
            .map(|(ci, w)| match cells.get(ci) {
                Some(cell) => wrap_cell(cell, size, (w - 2.0 * pad).max(1.0)),
                None => Ok(Vec::new()),
            })
            .collect()
    };
    let row_height = |cell_lines: &[Vec<String>]| -> f64 {
        let n = cell_lines.iter().map(|l| l.len()).max().unwrap_or(0).max(1);
        lead * n as f64 + size * 0.5
    };

    let draw_row = |eng: &mut LayoutEngine,
                    cell_lines: &[Vec<String>],
                    cells: &[ResolvedRun],
                    bg: Option<f64>|
     -> Result<()> {
        let row_h = row_height(cell_lines);
        let top = eng.y;
        if let Some(gray) = bg {
            eng.draw_rect_fill(left, top - row_h, table_w, row_h, gray);
        }
        let mut cx = left;
        for (ci, width) in widths.iter().enumerate() {
            if let Some(lines) = cell_lines.get(ci) {
                for (li, line) in lines.iter().enumerate() {
                    let baseline = top - lead * (li as f64 + 1.0);
                    if let Some(cell) = cells.get(ci) {
                        eng.draw_text_line(
                            cell.font,
                            &cell.choice,
                            size,
                            cx + pad,
                            baseline,
                            line,
                        )?;
                    }
                }
            }
            cx += width;
        }
        eng.advance(row_h);
        Ok(())
    };

    let header_lines = wrap_row(&resolved_rows[0])?;
    let header_h = row_height(&header_lines);
    let draw_header = |eng: &mut LayoutEngine| -> Result<()> {
        draw_row(eng, &header_lines, &resolved_rows[0], Some(0.88))?;
        eng.draw_rule(left, left + table_w, eng.y, 0.7);
        Ok(())
    };

    eng.ensure_room(header_h + lead + size);
    draw_header(eng)?;

    for (ri, row) in resolved_rows.iter().enumerate().skip(1) {
        let cell_lines = wrap_row(row)?;
        let row_h = row_height(&cell_lines);
        if eng.y - row_h < eng.margin - 0.01 {
            eng.break_page();
            draw_header(eng)?;
        }
        // Zebra stripes on even data rows.
        let bg = if ri % 2 == 0 { Some(0.955) } else { None };
        draw_row(eng, &cell_lines, row, bg)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// markdown_to_pdf
// ---------------------------------------------------------------------------

fn heading_scale(level: HeadingLevel) -> f64 {
    match level {
        HeadingLevel::H1 => 2.0,
        HeadingLevel::H2 => 1.6,
        HeadingLevel::H3 => 1.35,
        HeadingLevel::H4 => 1.2,
        HeadingLevel::H5 => 1.1,
        HeadingLevel::H6 => 1.0,
    }
}

/// Renders Markdown (headings, paragraphs, bold/italic, lists, GFM tables,
/// code blocks, raw-HTML text content, horizontal rules) into a paginated
/// PDF.
pub fn markdown_to_pdf(markdown: &str, opts: &DocOptions) -> Result<Vec<u8>> {
    let opts = effective_opts(opts)?;
    let mut eng = LayoutEngine::new(&opts)?;
    eng.emit_title(&opts)?;
    let base = opts.base_font_size_pt;

    // Inline state.
    let mut runs: Vec<Run> = Vec::new();
    let mut bold: u32 = 0;
    let mut emph: u32 = 0;
    let mut in_heading: Option<HeadingLevel> = None;
    // Each entry is a list level; Some(n) = next ordered index, None = bullet.
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut pending_marker: Option<String> = None;
    let mut in_code_block = false;
    let mut code_buf = String::new();
    let mut skip_depth: u32 = 0; // inside ignored constructs (images)
    // Raw-HTML block accumulator (tag-stripped text renders as a paragraph).
    let mut html_block: Option<String> = None;
    // GFM table accumulators.
    let mut table_rows: Option<Vec<Vec<String>>> = None;
    let mut table_row: Vec<String> = Vec::new();
    let mut table_cell: Option<String> = None;

    let inline_font = |bold: u32, emph: u32, heading: bool| -> StandardFont {
        let b = bold > 0 || heading;
        match (b, emph > 0) {
            (true, true) => StandardFont::HelveticaBoldOblique,
            (true, false) => StandardFont::HelveticaBold,
            (false, true) => StandardFont::HelveticaOblique,
            (false, false) => StandardFont::Helvetica,
        }
    };

    fn flush_block(
        eng: &mut LayoutEngine,
        runs: &mut Vec<Run>,
        size: f64,
        indent: f64,
        leading: f64,
        marker: &mut Option<String>,
        gap_after: f64,
    ) -> Result<()> {
        let marker_arg = marker.take().map(|m| (m, StandardFont::Helvetica));
        if runs.iter().all(|r| r.text.trim().is_empty()) && marker_arg.is_none() {
            runs.clear();
            return Ok(());
        }
        let block = std::mem::take(runs);
        eng.emit_paragraph(&block, size, indent, leading, marker_arg)?;
        eng.advance(gap_after);
        Ok(())
    }

    for event in Parser::new_ext(markdown, Options::ENABLE_TABLES) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    in_heading = Some(level);
                    eng.advance(base * 0.8);
                }
                Tag::Paragraph => {}
                Tag::Strong => bold += 1,
                Tag::Emphasis => emph += 1,
                Tag::List(start) => {
                    // In tight lists the parent item's inline text is still
                    // buffered (no Paragraph wrapper); flush it at the parent
                    // depth so child items start on their own lines.
                    if !runs.is_empty() || pending_marker.is_some() {
                        flush_block(
                            &mut eng,
                            &mut runs,
                            base,
                            18.0 * lists.len() as f64,
                            1.3,
                            &mut pending_marker,
                            base * 0.2,
                        )?;
                    }
                    lists.push(start);
                }
                Tag::Item => {
                    // Defensive: any dangling sibling text flushes first so
                    // items never fuse onto one line.
                    if !runs.is_empty() || pending_marker.is_some() {
                        flush_block(
                            &mut eng,
                            &mut runs,
                            base,
                            18.0 * lists.len() as f64,
                            1.3,
                            &mut pending_marker,
                            base * 0.2,
                        )?;
                    }
                    let marker = match lists.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}.");
                            *n += 1;
                            m
                        }
                        _ => "-".to_string(),
                    };
                    pending_marker = Some(marker);
                }
                Tag::CodeBlock(_) => {
                    // Fenced and indented blocks render the same way.
                    in_code_block = true;
                    code_buf.clear();
                }
                Tag::HtmlBlock => {
                    html_block = Some(String::new());
                }
                Tag::Table(_) => {
                    table_rows = Some(Vec::new());
                }
                Tag::TableHead | Tag::TableRow => {
                    table_row.clear();
                }
                Tag::TableCell => {
                    table_cell = Some(String::new());
                }
                Tag::Image { .. } => skip_depth += 1,
                Tag::Link { .. } => {}
                // Block quotes render as plain content; footnotes and
                // everything else are ignored gracefully.
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(level) => {
                    let size = base * heading_scale(level);
                    flush_block(
                        &mut eng,
                        &mut runs,
                        size,
                        18.0 * lists.len() as f64,
                        1.25,
                        &mut pending_marker,
                        base * 0.55,
                    )?;
                    in_heading = None;
                }
                TagEnd::Paragraph => {
                    let indent = 18.0 * lists.len() as f64;
                    let gap = if lists.is_empty() {
                        base * 0.65
                    } else {
                        base * 0.2
                    };
                    flush_block(
                        &mut eng,
                        &mut runs,
                        base,
                        indent,
                        1.3,
                        &mut pending_marker,
                        gap,
                    )?;
                }
                TagEnd::Item => {
                    // Tight list items carry inline text without a Paragraph.
                    let indent = 18.0 * lists.len() as f64;
                    flush_block(
                        &mut eng,
                        &mut runs,
                        base,
                        indent,
                        1.3,
                        &mut pending_marker,
                        base * 0.2,
                    )?;
                }
                TagEnd::List(_) => {
                    lists.pop();
                    if lists.is_empty() {
                        eng.advance(base * 0.45);
                    }
                }
                TagEnd::Strong => bold = bold.saturating_sub(1),
                TagEnd::Emphasis => emph = emph.saturating_sub(1),
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    let indent = 18.0 * lists.len() as f64;
                    render_code_block(&mut eng, &code_buf, base, indent)?;
                    code_buf.clear();
                }
                TagEnd::HtmlBlock => {
                    if let Some(html) = html_block.take() {
                        let text = sanitize_run(&strip_html_tags(&html));
                        if !text.trim().is_empty() {
                            let block = [Run {
                                font: StandardFont::Helvetica,
                                text,
                            }];
                            let indent = 18.0 * lists.len() as f64;
                            eng.emit_paragraph(&block, base, indent, 1.3, None)?;
                            eng.advance(base * 0.65);
                        }
                    }
                }
                TagEnd::Table => {
                    if let Some(rows) = table_rows.take()
                        && !rows.is_empty()
                    {
                        eng.advance(base * 0.35);
                        render_table(&mut eng, &rows, base)?;
                        eng.advance(base * 0.45);
                    }
                    table_row.clear();
                    table_cell = None;
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some(rows) = table_rows.as_mut() {
                        rows.push(std::mem::take(&mut table_row));
                    }
                }
                TagEnd::TableCell => {
                    if let Some(cell) = table_cell.take() {
                        table_row.push(sanitize_run(&cell));
                    }
                }
                TagEnd::Image => skip_depth = skip_depth.saturating_sub(1),
                TagEnd::Link => {}
                _ => {}
            },
            Event::Text(text) => {
                if skip_depth > 0 {
                    continue;
                }
                if let Some(cell) = table_cell.as_mut() {
                    cell.push_str(&text);
                } else if in_code_block {
                    code_buf.push_str(&text);
                } else {
                    let font = inline_font(bold, emph, in_heading.is_some());
                    push_run(&mut runs, font, &sanitize_run(&text));
                }
            }
            Event::Code(text) => {
                if skip_depth == 0 && !in_code_block {
                    if let Some(cell) = table_cell.as_mut() {
                        cell.push_str(&text);
                    } else {
                        push_run(&mut runs, StandardFont::Courier, &sanitize_run(&text));
                    }
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if skip_depth == 0 {
                    if let Some(cell) = table_cell.as_mut() {
                        cell.push(' ');
                    } else if in_code_block {
                        code_buf.push('\n');
                    } else {
                        let font = inline_font(bold, emph, in_heading.is_some());
                        push_run(&mut runs, font, " ");
                    }
                }
            }
            Event::Rule => {
                eng.ensure_room(base * 1.4);
                eng.advance(base * 0.7);
                let (x1, x2) = (eng.left(), eng.left() + eng.usable_width());
                let y = eng.y;
                eng.draw_rule(x1, x2, y, 0.5);
                eng.advance(base * 0.7);
            }
            // Raw HTML: strip tags and keep the human-visible text.
            Event::Html(html) | Event::InlineHtml(html) => {
                if skip_depth > 0 {
                    continue;
                }
                if let Some(buf) = html_block.as_mut() {
                    buf.push_str(&html);
                } else if let Some(cell) = table_cell.as_mut() {
                    cell.push_str(&strip_html_tags(&html));
                } else {
                    let text = sanitize_run(&strip_html_tags(&html));
                    if !text.trim().is_empty() {
                        let font = inline_font(bold, emph, in_heading.is_some());
                        push_run(&mut runs, font, &text);
                    }
                }
            }
            // Math, footnotes, task-list markers: skip gracefully.
            Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::FootnoteReference(_)
            | Event::TaskListMarker(_) => {}
        }
    }
    // Flush any dangling inline content (defensive; parser normally closes).
    if !runs.is_empty() {
        let indent = 18.0 * lists.len() as f64;
        flush_block(
            &mut eng,
            &mut runs,
            base,
            indent,
            1.3,
            &mut pending_marker,
            0.0,
        )?;
    }

    eng.finish()
}

/// Crude raw-HTML handling: drops `<...>` tag spans (each removed tag leaves
/// one space so words don't fuse) and decodes the common entities, so the
/// human-visible inner text survives as plain paragraph content.
fn strip_html_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' if !in_tag => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if in_tag => {}
            _ => out.push(c),
        }
    }
    // Decode &amp; last so "&amp;lt;" yields the literal "&lt;".
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Renders a code block on per-line light-gray background rectangles. Standard
/// Courier keeps its character-count wrapping; embedded runs use hmtx widths.
fn render_code_block(eng: &mut LayoutEngine, code: &str, base: f64, indent: f64) -> Result<()> {
    let size = base * 0.9;
    let pad = 6.0;
    let lead = size * 1.4;
    let block_w = (eng.usable_width() - indent).max(size);
    // Courier is monospace at 600/1000 em.
    let char_w = 0.6 * size;
    let max_chars = (((block_w - 2.0 * pad) / char_w).floor() as usize).max(1);

    eng.advance(base * 0.35);
    let left = eng.left() + indent;
    for raw_line in code.lines() {
        let line = sanitize_code_run(raw_line);
        let choice = eng.font_session.resolve(&line)?;
        let chunks: Vec<String> = match &choice {
            _ if line.is_empty() => vec![String::new()],
            FontChoice::Standard => line
                .chars()
                .collect::<Vec<_>>()
                .chunks(max_chars)
                .map(|c| c.iter().collect())
                .collect(),
            FontChoice::Embedded(_) => {
                let max_width = (block_w - 2.0 * pad).max(1.0);
                let mut chunks = Vec::new();
                let mut chunk = String::new();
                let mut width = 0.0;
                for ch in line.chars() {
                    let char_width = choice.char_width_pt(ch, StandardFont::Courier, size)?;
                    if width + char_width > max_width + 0.01 && !chunk.is_empty() {
                        chunks.push(std::mem::take(&mut chunk));
                        width = 0.0;
                    }
                    chunk.push(ch);
                    width += char_width;
                }
                if !chunk.is_empty() {
                    chunks.push(chunk);
                }
                chunks
            }
        };
        for chunk in chunks {
            eng.ensure_room(lead);
            let top = eng.y;
            eng.draw_rect_fill(left, top - lead, block_w, lead, 0.93);
            eng.advance(lead);
            let baseline = eng.y + size * 0.32;
            eng.draw_text_line(
                StandardFont::Courier,
                &choice,
                size,
                left + pad,
                baseline,
                &chunk,
            )?;
        }
    }
    eng.advance(base * 0.65);
    Ok(())
}

// ---------------------------------------------------------------------------
// images_to_pdf
// ---------------------------------------------------------------------------

/// One image per page, scaled to fit within the page margins while
/// preserving aspect ratio. Accepts PNG/JPEG/WebP bytes.
pub fn images_to_pdf(images: &[&[u8]], opts: &DocOptions) -> Result<Vec<u8>> {
    if images.is_empty() {
        return Err(PdfCoreError::InvalidInput(
            "images_to_pdf requires at least one image".to_string(),
        ));
    }
    // Validate geometry via the engine constructor (margins, sizes).
    let _ = LayoutEngine::new(opts)?;
    let (page_w, page_h) = opts.page_size.dimensions_pt();
    let margin = opts.margin_pt;
    let avail_w = page_w - 2.0 * margin;
    let avail_h = page_h - 2.0 * margin;

    let mut pages = Vec::with_capacity(images.len());
    for (idx, bytes) in images.iter().enumerate() {
        let decoded = image::load_from_memory(bytes).map_err(|e| {
            PdfCoreError::InvalidInput(format!("failed to decode image {idx}: {e}"))
        })?;
        let rgb = flatten_to_rgb8(decoded);
        let (iw, ih) = (rgb.width(), rgb.height());
        if iw == 0 || ih == 0 {
            return Err(PdfCoreError::InvalidInput(format!(
                "image {idx} has zero width or height"
            )));
        }
        let scale = (avail_w / iw as f64).min(avail_h / ih as f64);
        let draw_w = iw as f64 * scale;
        let draw_h = ih as f64 * scale;
        let x = (page_w - draw_w) / 2.0;
        let y = (page_h - draw_h) / 2.0;

        let mut content = Content::new();
        content.save_state();
        content.transform([draw_w as f32, 0.0, 0.0, draw_h as f32, x as f32, y as f32]);
        content.x_object(Name(b"Im0"));
        content.restore_state();

        pages.push(PageData {
            content: content.finish().into_vec(),
            fonts_used: [false; FONT_COUNT],
            embedded_fonts_used: BTreeSet::new(),
            image: Some(RgbSamples {
                width: iw,
                height: ih,
                samples: rgb.into_raw(),
            }),
        });
    }

    assemble(pages, page_w, page_h, Vec::new())
}

/// Converts any decoded image to RGB8, compositing alpha over white.
fn flatten_to_rgb8(img: image::DynamicImage) -> image::RgbImage {
    if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        let mut out = image::RgbImage::new(w, h);
        for (dst, src) in out.pixels_mut().zip(rgba.pixels()) {
            let a = src[3] as u32;
            for k in 0..3 {
                dst[k] = ((src[k] as u32 * a + 255 * (255 - a)) / 255) as u8;
            }
        }
        out
    } else {
        img.to_rgb8()
    }
}

//! Zone 1 — PDF generation: CSV tables, Markdown documents, plain text, and
//! image-per-page PDFs. Uses standard-14 fonts for WinAnsi and embedded CID
//! fonts for supported Unicode runs; replaces the Chromium html-to-pdf route.

use std::collections::{BTreeMap, BTreeSet};

use pdf_writer::types::{ActionType, AnnotationType};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str, TextStr};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::embed_font::{
    EmbeddedFont, EncodedGlyph, FontChoice, FontSession, PdfFontRefs, encode_winansi_bytes,
    is_winansi, pdfium_actual_text, resource_name, winansi_byte,
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
            base_font_size_pt: 10.5,
            title: None,
        }
    }
}

/// Sane clamp range for the base font size (points).
const MIN_BASE_FONT_PT: f64 = 4.0;
const MAX_BASE_FONT_PT: f64 = 72.0;
const LINK_RGB: [f32; 3] = [29.0 / 255.0, 78.0 / 255.0, 216.0 / 255.0];
/// Wide rows otherwise allocate and measure an attacker-controlled vector.
const MAX_CSV_COLUMNS: usize = 10_000;
/// Two million cells permits large exports while bounding total table work.
const MAX_CSV_CELLS: usize = 2_000_000;
/// pulldown-cmark is iterative, but layout indentation and state remain bounded.
const MAX_MARKDOWN_NESTING: usize = 64;
const DEFAULT_MARGIN_PT: f64 = 54.0;
const MARKDOWN_CONTENT_INSET_PT: f64 = 51.75;
const MARKDOWN_PAGE_MARGIN_PT: f64 = 27.75;
const CSV_CONTENT_INSET_PT: f64 = 30.0;
const CSV_PAGE_MARGIN_PT: f64 = 18.0;
const CSV_DEFAULT_FONT_PT: f64 = 7.5;
const MARKDOWN_LINE_HEIGHT_FACTOR: f64 = 11.0 / 7.0;

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

/// Normalizes whitespace and controls while preserving every WinAnsi scalar.
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

/// Table cells retain explicit line breaks (CSV quoted fields and Markdown
/// hard breaks) while normalizing every other control character.
fn sanitize_table_cell(text: &str) -> String {
    text.split('\n')
        .map(sanitize_run)
        .collect::<Vec<_>>()
        .join("\n")
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
        '\n' | '\r' | '\t' => out.push(ws),
        '\u{00A0}' | '\u{2009}' | '\u{200A}' | '\u{2002}' | '\u{2003}' => out.push(' '),
        '\u{200B}' | '\u{FEFF}' => {}
        c if c.is_control() => {}
        c if winansi_byte(c).is_some() => out.push(c),
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
        // Document generation intentionally exposes six faces. Remaining
        // Standard-14 variants exist for PDF analysis, not layout selection.
        _ => unreachable!("font_index called with a non-generator font"),
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
    links: Vec<LinkAnnotation>,
}

#[derive(Debug, Clone)]
struct LinkAnnotation {
    rect: [f32; 4],
    uri: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TextColor {
    #[default]
    Black,
    Link,
    Muted,
}

#[derive(Debug, Clone, Copy)]
struct TextDrawContext {
    color: TextColor,
    external_ltr_context: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RunStyle {
    link: Option<String>,
    strikethrough: bool,
    superscript: bool,
    color: TextColor,
}

/// A styled inline run: text set in one font (size comes from the block).
#[derive(Debug, Clone)]
struct Run {
    font: StandardFont,
    text: String,
    style: RunStyle,
}

/// A logical run after its standard-vs-embedded choice has been frozen.
#[derive(Debug, Clone)]
struct ResolvedRun {
    font: StandardFont,
    choice: FontChoice,
    text: String,
    style: RunStyle,
}

#[derive(Debug, Clone)]
enum ListMarker {
    Text(String),
    Task(bool),
}

/// Minimal top-down layout engine shared by the csv/markdown/text generators.
/// Tracks a cursor `y` (distance from page bottom), starts new pages when the
/// cursor would cross the bottom margin, and exposes small drawing helpers.
struct LayoutEngine {
    pages: Vec<PageData>,
    content: Content,
    fonts_used: [bool; FONT_COUNT],
    embedded_fonts_used: BTreeSet<usize>,
    links: Vec<LinkAnnotation>,
    font_session: FontSession,
    page_w: f64,
    page_h: f64,
    /// Horizontal content inset. Route-compatible defaults may include the
    /// legacy HTML wrapper's body margin as well as the physical page margin.
    margin: f64,
    /// Physical bottom page margin used by pagination.
    bottom_margin: f64,
    /// Physical top margin on pages after the first. Chromium fragments the
    /// body box across pages, so its body margin applies only to page one.
    continuation_top_margin: f64,
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
            links: Vec::new(),
            font_session: FontSession::new()?,
            page_w,
            page_h,
            margin: opts.margin_pt,
            bottom_margin: opts.margin_pt,
            continuation_top_margin: opts.margin_pt,
            y: page_h - opts.margin_pt,
        })
    }

    fn new_markdown(opts: &DocOptions) -> Result<Self> {
        let mut engine = Self::new(opts)?;
        if (opts.margin_pt - DEFAULT_MARGIN_PT).abs() < f64::EPSILON {
            engine.margin = MARKDOWN_CONTENT_INSET_PT;
            engine.bottom_margin = MARKDOWN_PAGE_MARGIN_PT;
            engine.continuation_top_margin = MARKDOWN_PAGE_MARGIN_PT;
            engine.y = engine.page_h - MARKDOWN_CONTENT_INSET_PT;
        }
        Ok(engine)
    }

    fn new_csv(opts: &DocOptions) -> Result<Self> {
        let mut engine = Self::new(opts)?;
        if (opts.margin_pt - DEFAULT_MARGIN_PT).abs() < f64::EPSILON {
            engine.margin = CSV_CONTENT_INSET_PT;
            engine.bottom_margin = CSV_PAGE_MARGIN_PT;
            engine.continuation_top_margin = CSV_PAGE_MARGIN_PT;
            engine.y = engine.page_h - CSV_CONTENT_INSET_PT;
        }
        Ok(engine)
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
        let links = std::mem::take(&mut self.links);
        self.pages.push(PageData {
            content: content.finish().into_vec(),
            fonts_used,
            embedded_fonts_used,
            image: None,
            links,
        });
        self.y = self.page_h - self.continuation_top_margin;
    }

    /// Starts a new page if `needed` points of vertical room are not left.
    fn ensure_room(&mut self, needed: f64) {
        if self.y - needed < self.bottom_margin - 0.01 {
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
        position: (f64, f64),
        text: &str,
        color: TextColor,
    ) -> Result<()> {
        self.draw_text_line_with_context(
            font,
            choice,
            size,
            position,
            text,
            TextDrawContext {
                color,
                external_ltr_context: false,
            },
        )
    }

    fn draw_text_line_with_context(
        &mut self,
        font: StandardFont,
        choice: &FontChoice,
        size: f64,
        position: (f64, f64),
        text: &str,
        context: TextDrawContext,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let (x, y) = position;
        self.content.save_state();
        match context.color {
            TextColor::Black => self.content.set_fill_gray(0.0),
            TextColor::Link => self
                .content
                .set_fill_rgb(LINK_RGB[0], LINK_RGB[1], LINK_RGB[2]),
            TextColor::Muted => self.content.set_fill_gray(0.45),
        };
        match choice {
            FontChoice::Standard => {
                self.fonts_used[font_index(font)] = true;
                self.content.begin_text();
                self.content
                    .set_font(Name(FONT_RES_NAMES[font_index(font)]), size as f32);
                self.content.next_line(x as f32, y as f32);
                let encoded = encode_winansi_bytes(text);
                self.content.show(Str(&encoded));
                self.content.end_text();
            }
            FontChoice::Embedded(choice) => {
                let encoded = self.font_session.encode(choice, text)?;
                let mut cursor = x;
                // PDFium currently drops UTF-16 surrogate pairs from
                // `/ActualText`. BMP complex-script runs still need a logical
                // replacement because every glyph has its own positioning
                // matrix; astral runs instead extract through ToUnicode.
                let actual_text = (!context.external_ltr_context)
                    .then(|| encoded.directional_actual_text())
                    .flatten()
                    .or_else(|| {
                        text.chars()
                            .all(|ch| u32::from(ch) <= 0xFFFF)
                            .then(|| pdfium_actual_text(text))
                    });
                if let Some(actual_text) = &actual_text {
                    let mut marked = self
                        .content
                        .begin_marked_content_with_properties(Name(b"Span"));
                    {
                        let mut properties = marked.properties();
                        properties.actual_text(TextStr(actual_text));
                    }
                    marked.finish();
                }
                // Keep every positioned glyph in one PDF text object. Individual
                // matrices preserve HarfBuzz offsets, while a single BT/ET span
                // lets extractors reconstruct one line from the per-CID ToUnicode
                // mappings (including non-BMP surrogate pairs).
                self.content.begin_text();
                self.draw_positioned_glyphs(&encoded.glyphs, size, y, &mut cursor);
                self.content.end_text();
                if actual_text.is_some() {
                    self.content.end_marked_content();
                }
            }
        }
        self.content.restore_state();
        Ok(())
    }

    fn draw_positioned_glyphs(
        &mut self,
        glyphs: &[EncodedGlyph],
        size: f64,
        y: f64,
        cursor: &mut f64,
    ) {
        for glyph in glyphs {
            self.embedded_fonts_used.insert(glyph.resource_index);
            let name = resource_name(glyph.resource_index);
            let scale = size / glyph.units_per_em;
            self.content.set_font(Name(name.as_bytes()), size as f32);
            self.content.set_text_matrix([
                1.0,
                0.0,
                0.0,
                1.0,
                (*cursor + f64::from(glyph.x_offset) * scale) as f32,
                (y + f64::from(glyph.y_offset) * scale) as f32,
            ]);
            self.content.show(Str(&glyph.cid.to_be_bytes()));
            *cursor += f64::from(glyph.x_advance) * scale;
        }
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

    fn draw_rect_stroke(&mut self, x: f64, y: f64, w: f64, h: f64, line_width: f64) {
        self.content.save_state();
        self.content.set_line_width(line_width as f32);
        self.content.rect(x as f32, y as f32, w as f32, h as f32);
        self.content.stroke();
        self.content.restore_state();
    }

    /// Draws a sequence of styled runs on one baseline, advancing x per run.
    fn draw_runs_line(&mut self, runs: &[ResolvedRun], size: f64, x: f64, y: f64) -> Result<()> {
        let mut cx = x;
        for run in runs {
            let run_size = if run.style.superscript {
                size * 0.7
            } else {
                size
            };
            let run_y = if run.style.superscript {
                y + size * 0.38
            } else {
                y
            };
            let width = run.choice.text_width_pt(&run.text, run.font, run_size)?;
            self.draw_text_line(
                run.font,
                &run.choice,
                run_size,
                (cx, run_y),
                &run.text,
                run.style.color,
            )?;
            if run.style.strikethrough {
                self.draw_rule(cx, cx + width, run_y + run_size * 0.3, 0.5);
            }
            if let Some(uri) = &run.style.link
                && width > 0.0
            {
                self.links.push(LinkAnnotation {
                    rect: [
                        cx as f32,
                        (run_y - run_size * 0.22).max(0.0) as f32,
                        (cx + width).min(self.page_w) as f32,
                        (run_y + run_size * 0.88).min(self.page_h) as f32,
                    ],
                    uri: uri.clone(),
                });
            }
            cx += width;
        }
        Ok(())
    }

    fn draw_task_marker(&mut self, checked: bool, indent: f64, baseline: f64, size: f64) {
        let side = (size * 0.7).max(4.0);
        let x = (self.left() + indent - side - 4.0).max(self.left());
        let y = baseline + (size - side) * 0.2;
        self.content.save_state();
        self.content.set_line_width(0.7);
        self.content
            .rect(x as f32, y as f32, side as f32, side as f32);
        self.content.stroke();
        if checked {
            let inset = side * 0.2;
            self.content.move_to((x + inset) as f32, (y + inset) as f32);
            self.content
                .line_to((x + side - inset) as f32, (y + side - inset) as f32);
            self.content
                .move_to((x + inset) as f32, (y + side - inset) as f32);
            self.content
                .line_to((x + side - inset) as f32, (y + inset) as f32);
            self.content.stroke();
        }
        self.content.restore_state();
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
        marker: Option<ListMarker>,
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
        let block_height = lead * lines.len() as f64;
        let fresh_page_height = self.page_h - self.continuation_top_margin - self.bottom_margin;
        if block_height <= fresh_page_height + 0.01
            && self.y - block_height < self.bottom_margin - 0.01
        {
            self.break_page();
        }
        for (i, line) in lines.iter().enumerate() {
            self.ensure_room(lead);
            self.advance(lead);
            let baseline = self.y;
            if i == 0
                && let Some(marker) = &marker
            {
                match marker {
                    ListMarker::Text(mtext) => {
                        let font = StandardFont::Helvetica;
                        let mw = font.text_width_pt(mtext, size);
                        let mx = (self.left() + indent - mw - 4.0).max(self.left());
                        self.draw_text_line(
                            font,
                            &FontChoice::Standard,
                            size,
                            (mx, baseline),
                            mtext,
                            TextColor::Black,
                        )?;
                    }
                    ListMarker::Task(checked) => {
                        self.draw_task_marker(*checked, indent, baseline, size);
                    }
                }
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
                    style: RunStyle::default(),
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
                style: run.style.clone(),
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
    // of non-space characters within a single styled run. Newlines are forced
    // line boundaries used by Markdown hard breaks and multi-line table cells.
    let mut tokens: Vec<(StandardFont, FontChoice, RunStyle, String)> = Vec::new();
    for run in runs {
        let mut cur = String::new();
        let mut cur_is_space: Option<bool> = None;
        for c in run.text.chars() {
            if c == '\n' {
                if !cur.is_empty() {
                    tokens.push((
                        run.font,
                        run.choice.clone(),
                        run.style.clone(),
                        std::mem::take(&mut cur),
                    ));
                }
                cur_is_space = None;
                tokens.push((
                    run.font,
                    run.choice.clone(),
                    run.style.clone(),
                    "\n".to_string(),
                ));
                continue;
            }
            let is_space = c == ' ';
            if cur_is_space != Some(is_space) && !cur.is_empty() {
                tokens.push((
                    run.font,
                    run.choice.clone(),
                    run.style.clone(),
                    std::mem::take(&mut cur),
                ));
            }
            cur_is_space = Some(is_space);
            cur.push(c);
        }
        if !cur.is_empty() {
            tokens.push((run.font, run.choice.clone(), run.style.clone(), cur));
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

    for (font, choice, style, tok) in tokens {
        if tok == "\n" {
            let was_empty = cur.is_empty();
            flush(&mut cur, &mut cur_w, &mut lines);
            if was_empty {
                lines.push(Vec::new());
            }
            continue;
        }
        let is_space = tok.starts_with(' ');
        let run_size = if style.superscript { size * 0.7 } else { size };
        let w = choice.text_width_pt(&tok, font, run_size)?;
        if is_space {
            // Never start a line with spaces; allow trailing spaces to
            // overflow (they get trimmed on flush).
            if !cur.is_empty() {
                push_resolved_run(&mut cur, font, choice, style, &tok);
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
                let cw = choice.char_width_pt(c, font, run_size)?;
                if piece_w + cw > max_w + 0.01 && !piece.is_empty() {
                    lines.push(vec![ResolvedRun {
                        font,
                        choice: choice.clone(),
                        text: std::mem::take(&mut piece),
                        style: style.clone(),
                    }]);
                    piece_w = 0.0;
                }
                piece.push(c);
                piece_w += cw;
            }
            if !piece.is_empty() {
                push_resolved_run(&mut cur, font, choice, style, &piece);
                cur_w += piece_w;
            }
            continue;
        }
        push_resolved_run(&mut cur, font, choice, style, &tok);
        cur_w += w;
    }
    flush(&mut cur, &mut cur_w, &mut lines);
    Ok(lines)
}

fn push_resolved_run(
    line: &mut Vec<ResolvedRun>,
    font: StandardFont,
    choice: FontChoice,
    style: RunStyle,
    text: &str,
) {
    if let Some(last) = line.last_mut()
        && last.font == font
        && last.choice == choice
        && last.style == style
    {
        last.text.push_str(text);
        return;
    }
    line.push(ResolvedRun {
        font,
        choice,
        text: text.to_string(),
        style,
    });
}

/// Appends text to the last run when the font matches, else pushes a new run.
fn push_run(line: &mut Vec<Run>, font: StandardFont, style: &RunStyle, text: &str) {
    if let Some(last) = line.last_mut()
        && last.font == font
        && last.style == *style
    {
        last.text.push_str(text);
        return;
    }
    line.push(Run {
        font,
        text: text.to_string(),
        style: style.clone(),
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
            mapping: alloc(),
        })
        .collect();

    let page_ids: Vec<Ref> = pages.iter().map(|_| alloc()).collect();
    let content_ids: Vec<Ref> = pages.iter().map(|_| alloc()).collect();
    let image_ids: Vec<Option<Ref>> = pages
        .iter()
        .map(|p| p.image.as_ref().map(|_| alloc()))
        .collect();
    let annotation_ids: Vec<Vec<Ref>> = pages
        .iter()
        .map(|page| page.links.iter().map(|_| alloc()).collect())
        .collect();

    pdf.catalog(catalog_id).pages(tree_id);
    pdf.pages(tree_id)
        .kids(page_ids.iter().copied())
        .count(pages.len() as i32);

    for i in 0..FONT_COUNT {
        if let Some(fid) = font_ids[i] {
            pdf.type1_font(fid)
                .base_font(Name(ALL_FONTS[i].base_name().as_bytes()))
                .encoding_predefined(Name(b"WinAnsiEncoding"));
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
        if !annotation_ids[i].is_empty() {
            p.annotations(annotation_ids[i].iter().copied());
        }
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

        for (link, annotation_id) in page.links.iter().zip(&annotation_ids[i]) {
            let mut annotation = pdf.annotation(*annotation_id);
            annotation.subtype(AnnotationType::Link);
            annotation.rect(Rect::new(
                link.rect[0],
                link.rect[1],
                link.rect[2],
                link.rect[3],
            ));
            annotation.color_rgb(LINK_RGB[0], LINK_RGB[1], LINK_RGB[2]);
            annotation
                .action()
                .action_type(ActionType::Uri)
                .uri(Str(link.uri.as_bytes()));
            annotation.finish();
        }

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
            style: RunStyle::default(),
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
    let mut cells = 0usize;
    for record in reader.records() {
        let record = record?;
        if record.len() > MAX_CSV_COLUMNS {
            return Err(PdfCoreError::ResourceLimit(format!(
                "CSV column count {} exceeds limit {MAX_CSV_COLUMNS}",
                record.len()
            )));
        }
        cells = cells.checked_add(record.len()).ok_or_else(|| {
            PdfCoreError::ResourceLimit("CSV cell count exceeds limit".to_string())
        })?;
        if cells > MAX_CSV_CELLS {
            return Err(PdfCoreError::ResourceLimit(format!(
                "CSV cell count {cells} exceeds limit {MAX_CSV_CELLS}"
            )));
        }
        rows.push(record.iter().map(sanitize_table_cell).collect());
    }
    if rows.is_empty() {
        return Err(PdfCoreError::InvalidInput(
            "CSV input contains no rows".to_string(),
        ));
    }

    let mut eng = LayoutEngine::new_csv(&opts)?;
    eng.emit_title(&opts)?;
    let font_size = if (opts.base_font_size_pt - DocOptions::default().base_font_size_pt).abs()
        < f64::EPSILON
    {
        CSV_DEFAULT_FONT_PT
    } else {
        opts.base_font_size_pt
    };
    render_table(&mut eng, &rows, font_size, TableStyle::Csv)?;

    eng.finish()
}

// ---------------------------------------------------------------------------
// Shared table layout (CSV + GFM tables)
// ---------------------------------------------------------------------------

/// Tables never auto-shrink below this font size.
const TABLE_MIN_FONT_PT: f64 = 4.0;

#[derive(Clone, Copy)]
enum TableStyle {
    Csv,
    Markdown,
}

impl TableStyle {
    fn pad_factor(self) -> f64 {
        match self {
            Self::Csv => 0.7,
            Self::Markdown => 0.5,
        }
    }

    fn line_height_factor(self) -> f64 {
        match self {
            Self::Csv => 1.4,
            Self::Markdown => 1.2,
        }
    }

    fn row_extra_factor(self) -> f64 {
        match self {
            Self::Csv => 0.6,
            Self::Markdown => 0.5,
        }
    }
}

/// Measures the content-driven width targets for every column at `size`.
///
/// `natural` is the widest explicit line in the column. `header` is the
/// header's widest explicit line. `minimum` is the widest indivisible atom
/// plus cell padding: body words (including dates, numbers, and
/// hyphen/underscore-joined identifiers) stay whole, while snake_case headers
/// may wrap only after an underscore.
struct TableColumnWidths {
    natural: Vec<f64>,
    header: Vec<f64>,
    minimum: Vec<f64>,
    visible: Vec<f64>,
}

fn table_column_widths(
    rows: &[Vec<ResolvedRun>],
    ncols: usize,
    size: f64,
    pad: f64,
) -> Result<TableColumnWidths> {
    let padded_empty = 2.0 * pad;
    let mut natural = vec![padded_empty; ncols];
    let mut header = vec![padded_empty; ncols];
    let mut minimum = vec![padded_empty; ncols];
    let mut visible = vec![padded_empty; ncols];
    for (ri, row) in rows.iter().enumerate() {
        for (ci, cell) in row.iter().enumerate() {
            if cell.text.split_whitespace().next().is_some() {
                let marker_width = cell.choice.text_width_pt("...", cell.font, size)? + 2.0 * pad;
                visible[ci] = visible[ci].max(marker_width);
            }
            for line in cell.text.split_terminator('\n') {
                let line = line.trim_matches(' ');
                let width = cell.choice.text_width_pt(line, cell.font, size)? + 2.0 * pad;
                natural[ci] = natural[ci].max(width);
                if ri == 0 {
                    header[ci] = header[ci].max(width);
                }

                for word in line.split_whitespace() {
                    if ri == 0 {
                        for atom in word.split_inclusive('_') {
                            let width =
                                cell.choice.text_width_pt(atom, cell.font, size)? + 2.0 * pad;
                            minimum[ci] = minimum[ci].max(width);
                        }
                    } else {
                        let width = cell.choice.text_width_pt(word, cell.font, size)? + 2.0 * pad;
                        minimum[ci] = minimum[ci].max(width);
                    }
                }
            }
        }
    }
    for (visible, minimum) in visible.iter_mut().zip(&minimum) {
        *visible = visible.min(*minimum);
    }
    Ok(TableColumnWidths {
        natural,
        header,
        minimum,
        visible,
    })
}

/// Allocates width from `minimum` toward `natural` using capped water-filling.
/// Sorting the remaining demands finds the common water level in O(n log n);
/// columns that reach their natural width return their unused share to wider
/// columns.
fn fair_share_widths(minimum: &[f64], natural: &[f64], usable: f64) -> Vec<f64> {
    let minimum_total: f64 = minimum.iter().sum();
    if minimum_total >= usable {
        let scale = usable / minimum_total.max(f64::MIN_POSITIVE);
        return minimum.iter().map(|width| width * scale).collect();
    }

    let natural_total: f64 = natural.iter().sum();
    let target = natural_total.min(usable);
    let mut remaining = (target - minimum_total).max(0.0);
    if remaining <= f64::EPSILON {
        return minimum.to_vec();
    }

    let mut demands: Vec<f64> = natural
        .iter()
        .zip(minimum)
        .map(|(natural, minimum)| (natural - minimum).max(0.0))
        .filter(|demand| *demand > f64::EPSILON)
        .collect();
    demands.sort_by(f64::total_cmp);
    let mut active_count = demands.len();
    let mut water_level = 0.0;
    for demand in demands {
        if active_count == 0 {
            break;
        }
        let step = (demand - water_level).max(0.0);
        let step_cost = step * active_count as f64;
        if remaining <= step_cost {
            water_level += remaining / active_count as f64;
            remaining = 0.0;
            break;
        }
        remaining -= step_cost;
        water_level = demand;
        active_count -= 1;
    }
    if remaining > f64::EPSILON && active_count > 0 {
        water_level += remaining / active_count as f64;
    }
    natural
        .iter()
        .zip(minimum)
        .map(|(natural, minimum)| minimum + (natural - minimum).max(0.0).min(water_level))
        .collect()
}

/// Picks the table font size and column widths for `usable` points of width.
/// Atom-safe minima are satisfied before slack is distributed toward natural
/// widths. If the minima do not fit, every column shrinks together, never
/// below [`TABLE_MIN_FONT_PT`]. Only a floor-sized table whose minima still
/// overflow is allowed to ellipsize atoms.
fn table_layout(
    rows: &[Vec<ResolvedRun>],
    ncols: usize,
    base_size: f64,
    usable: f64,
    style: TableStyle,
) -> Result<(f64, Vec<f64>, bool)> {
    let mut size = base_size;
    let mut pad = size * style.pad_factor();
    let mut metrics = table_column_widths(rows, ncols, size, pad)?;
    let mut minimum_total: f64 = metrics.minimum.iter().sum();
    let mut preferred_total = match style {
        TableStyle::Csv => metrics
            .header
            .iter()
            .zip(&metrics.minimum)
            .map(|(header, minimum)| header.max(*minimum))
            .sum(),
        TableStyle::Markdown => minimum_total,
    };
    if preferred_total > usable {
        // All metrics are linear in font size, so this factor is exact.
        size = (size * usable / preferred_total).max(TABLE_MIN_FONT_PT);
        pad = size * style.pad_factor();
        metrics = table_column_widths(rows, ncols, size, pad)?;
        minimum_total = metrics.minimum.iter().sum();
        preferred_total = match style {
            TableStyle::Csv => metrics
                .header
                .iter()
                .zip(&metrics.minimum)
                .map(|(header, minimum)| header.max(*minimum))
                .sum(),
            TableStyle::Markdown => minimum_total,
        };
    }
    let clip_oversized_atoms = size <= TABLE_MIN_FONT_PT + 0.01 && minimum_total > usable + 0.01;
    let widths = if clip_oversized_atoms {
        let visible_total: f64 = metrics.visible.iter().sum();
        if visible_total > usable + 0.01 {
            return Err(PdfCoreError::ResourceLimit(format!(
                "table has {ncols} columns whose minimum visible widths \
                 exceed the available page width"
            )));
        }
        fair_share_widths(&metrics.visible, &metrics.minimum, usable)
    } else if matches!(style, TableStyle::Csv) {
        // Body atom minima remain mandatory. Slack first removes avoidable
        // header wrapping, then continues toward overall natural widths.
        let header_targets = metrics
            .header
            .iter()
            .zip(&metrics.minimum)
            .map(|(header, minimum)| header.max(*minimum))
            .collect::<Vec<_>>();
        let header_widths = fair_share_widths(&metrics.minimum, &header_targets, usable);
        fair_share_widths(&header_widths, &metrics.natural, usable)
    } else {
        fair_share_widths(&metrics.minimum, &metrics.natural, usable)
    };
    debug_assert!(
        !matches!(style, TableStyle::Csv)
            || size <= TABLE_MIN_FONT_PT + 0.01
            || preferred_total <= usable + 0.01
    );
    Ok((size, widths, clip_oversized_atoms))
}

fn ellipsize_table_atom(cell: &ResolvedRun, atom: &str, size: f64, avail_w: f64) -> Result<String> {
    const ELLIPSIS: &str = "...";
    let ellipsis_width = cell.choice.text_width_pt(ELLIPSIS, cell.font, size)?;
    let prefix_limit = (avail_w - ellipsis_width).max(0.0);
    let mut prefix = String::new();
    let mut prefix_width = 0.0;
    for ch in atom.chars() {
        let char_width = cell.choice.char_width_pt(ch, cell.font, size)?;
        if prefix_width + char_width > prefix_limit + 0.01 {
            break;
        }
        prefix.push(ch);
        prefix_width += char_width;
    }
    prefix.push_str(ELLIPSIS);
    Ok(prefix)
}

struct TableWrapContext<'a> {
    cell: &'a ResolvedRun,
    size: f64,
    avail_w: f64,
    clip_oversized_atoms: bool,
}

fn push_table_atom(
    lines: &mut Vec<String>,
    current: &mut String,
    current_width: &mut f64,
    context: &TableWrapContext<'_>,
    atom: &str,
    separator: &str,
) -> Result<()> {
    let atom_width = context
        .cell
        .choice
        .text_width_pt(atom, context.cell.font, context.size)?;
    if atom_width > context.avail_w + 0.01 {
        if !current.is_empty() {
            lines.push(std::mem::take(current));
            *current_width = 0.0;
        }
        if context.clip_oversized_atoms {
            lines.push(ellipsize_table_atom(
                context.cell,
                atom,
                context.size,
                context.avail_w,
            )?);
        } else {
            // The measured column minimum makes this unreachable outside
            // floating-point tolerance. Preserve the atom rather than split
            // or silently truncate it if a future metric violates that
            // invariant.
            lines.push(atom.to_string());
        }
        return Ok(());
    }

    let separator_width = if !separator.is_empty() && !current.is_empty() {
        context
            .cell
            .choice
            .text_width_pt(separator, context.cell.font, context.size)?
    } else {
        0.0
    };
    if !current.is_empty() && *current_width + separator_width + atom_width > context.avail_w + 0.01
    {
        lines.push(std::mem::take(current));
        *current_width = 0.0;
    }
    if !separator.is_empty() && !current.is_empty() {
        current.push_str(separator);
        *current_width += separator_width;
    }
    current.push_str(atom);
    *current_width += atom_width;
    Ok(())
}

/// Wraps one table cell only at whitespace, explicit newlines, and (for the
/// header row) after underscores. Indivisible atoms are never character-split.
fn wrap_cell(
    cell: &ResolvedRun,
    size: f64,
    avail_w: f64,
    is_header: bool,
    clip_oversized_atoms: bool,
) -> Result<Vec<String>> {
    if cell.text.is_empty() {
        return Ok(Vec::new());
    }
    let context = TableWrapContext {
        cell,
        size,
        avail_w,
        clip_oversized_atoms,
    };
    let mut lines = Vec::new();
    for explicit_line in cell.text.split_terminator('\n') {
        let mut current = String::new();
        let mut current_width = 0.0;
        let mut saw_word = false;
        let mut rest = explicit_line;
        loop {
            let Some(word_start) = rest
                .char_indices()
                .find(|(_, ch)| !ch.is_whitespace())
                .map(|(index, _)| index)
            else {
                break;
            };
            let separator = if saw_word { &rest[..word_start] } else { "" };
            let word_and_rest = &rest[word_start..];
            let word_end = word_and_rest
                .char_indices()
                .find(|(_, ch)| ch.is_whitespace())
                .map(|(index, _)| index)
                .unwrap_or(word_and_rest.len());
            let word = &word_and_rest[..word_end];
            rest = &word_and_rest[word_end..];

            if is_header {
                for (atom_index, atom) in word.split_inclusive('_').enumerate() {
                    push_table_atom(
                        &mut lines,
                        &mut current,
                        &mut current_width,
                        &context,
                        atom,
                        if atom_index == 0 { separator } else { "" },
                    )?;
                }
            } else {
                push_table_atom(
                    &mut lines,
                    &mut current,
                    &mut current_width,
                    &context,
                    word,
                    separator,
                )?;
            }
            saw_word = true;
        }
        if !current.is_empty() {
            lines.push(current);
        } else if !saw_word {
            lines.push(String::new());
        }
    }
    Ok(lines)
}

/// Draws `rows` as a zebra-striped table with a bold header repeated on
/// every page. Cells word-wrap within their column; over-tall rows are split
/// into bordered fragments without changing their column positions.
fn render_table(
    eng: &mut LayoutEngine,
    rows: &[Vec<String>],
    base_size: f64,
    style: TableStyle,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let resolved_rows: Vec<Vec<ResolvedRun>> = rows
        .iter()
        .enumerate()
        .map(|(ri, row)| {
            let font = match (style, ri) {
                (TableStyle::Csv, _) => StandardFont::Courier,
                (TableStyle::Markdown, 0) => StandardFont::HelveticaBold,
                (TableStyle::Markdown, _) => StandardFont::Helvetica,
            };
            row.iter()
                .map(|text| {
                    Ok(ResolvedRun {
                        font,
                        choice: eng.font_session.resolve(text)?,
                        text: text.clone(),
                        style: RunStyle::default(),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<_>>()?;
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0).max(1);
    let usable = eng.usable_width();
    let (size, widths, clip_oversized_atoms) =
        table_layout(&resolved_rows, ncols, base_size, usable, style)?;
    let pad = size * style.pad_factor();
    let lead = size * style.line_height_factor();
    let table_w: f64 = widths.iter().sum();
    let left = eng.left();

    let wrap_row = |cells: &[ResolvedRun], is_header: bool| -> Result<Vec<Vec<String>>> {
        widths
            .iter()
            .enumerate()
            .map(|(ci, w)| match cells.get(ci) {
                Some(cell) => wrap_cell(
                    cell,
                    size,
                    (w - 2.0 * pad).max(1.0),
                    is_header,
                    clip_oversized_atoms,
                ),
                None => Ok(Vec::new()),
            })
            .collect()
    };
    let row_height =
        |line_count: usize| lead * line_count.max(1) as f64 + size * style.row_extra_factor();

    let draw_row = |eng: &mut LayoutEngine,
                    cell_lines: &[Vec<String>],
                    cells: &[ResolvedRun],
                    start_line: usize,
                    line_count: usize,
                    bg: Option<f64>|
     -> Result<()> {
        let row_h = row_height(line_count);
        let top = eng.y;
        if let Some(gray) = bg {
            eng.draw_rect_fill(left, top - row_h, table_w, row_h, gray);
        }
        let mut cx = left;
        for (ci, width) in widths.iter().enumerate() {
            let border_width = match style {
                TableStyle::Csv => 0.75,
                TableStyle::Markdown => 0.35,
            };
            eng.draw_rect_stroke(cx, top - row_h, *width, row_h, border_width);
            if let Some(lines) = cell_lines.get(ci) {
                for (li, line) in lines.iter().skip(start_line).take(line_count).enumerate() {
                    let baseline = top - lead * (li as f64 + 1.0);
                    if let Some(cell) = cells.get(ci) {
                        eng.draw_text_line_with_context(
                            cell.font,
                            &cell.choice,
                            size,
                            (cx + pad, baseline),
                            line,
                            TextDrawContext {
                                color: TextColor::Black,
                                external_ltr_context: true,
                            },
                        )?;
                    }
                }
            }
            cx += width;
        }
        eng.advance(row_h);
        Ok(())
    };

    let header_lines = wrap_row(&resolved_rows[0], true)?;
    let header_line_count = header_lines.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let header_h = row_height(header_line_count);
    let draw_header = |eng: &mut LayoutEngine| -> Result<()> {
        draw_row(
            eng,
            &header_lines,
            &resolved_rows[0],
            0,
            header_line_count,
            match style {
                TableStyle::Csv => None,
                TableStyle::Markdown => Some(0.88),
            },
        )?;
        if matches!(style, TableStyle::Markdown) {
            eng.draw_rule(left, left + table_w, eng.y, 0.7);
        }
        Ok(())
    };

    eng.ensure_room(header_h + lead + size);
    draw_header(eng)?;

    for (ri, row) in resolved_rows.iter().enumerate().skip(1) {
        let cell_lines = wrap_row(row, false)?;
        let total_lines = cell_lines.iter().map(Vec::len).max().unwrap_or(0).max(1);
        // Zebra stripes on even data rows.
        let bg = if matches!(style, TableStyle::Markdown) && ri % 2 == 0 {
            Some(0.955)
        } else {
            None
        };
        let mut start_line = 0;
        while start_line < total_lines {
            // Leave top/bottom padding within each fragment, and move to a
            // header-bearing page when fewer than one data baseline remains.
            let available = eng.y - eng.margin - size * 0.5;
            let available_lines = (available / lead).floor().max(0.0) as usize;
            if available_lines == 0 {
                eng.break_page();
                draw_header(eng)?;
                continue;
            }
            // Logical rows are atomic across page breaks: a row that does
            // not fit in the remaining space moves whole to the next page
            // (below its repeated header) instead of splitting mid-row.
            // Only rows taller than a full header-bearing page may split.
            if start_line == 0 && total_lines > available_lines {
                let fresh =
                    eng.page_h - eng.continuation_top_margin - header_h - eng.margin - size * 0.5;
                let fresh_lines = (fresh / lead).floor().max(0.0) as usize;
                if total_lines <= fresh_lines {
                    eng.break_page();
                    draw_header(eng)?;
                    continue;
                }
            }
            let line_count = (total_lines - start_line).min(available_lines);
            draw_row(eng, &cell_lines, row, start_line, line_count, bg)?;
            start_line += line_count;
            if start_line < total_lines {
                eng.break_page();
                draw_header(eng)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// markdown_to_pdf
// ---------------------------------------------------------------------------

fn heading_scale(level: HeadingLevel) -> f64 {
    match level {
        HeadingLevel::H1 => 2.0,
        HeadingLevel::H2 => 1.5,
        HeadingLevel::H3 => 1.17,
        HeadingLevel::H4 => 1.0,
        HeadingLevel::H5 => 0.83,
        HeadingLevel::H6 => 0.67,
    }
}

/// Baseline-model gap before an adjacent heading. These factors reproduce
/// Chromium's collapsed user-agent heading margins after accounting for the
/// following heading's measured 1.6 line box.
fn adjacent_heading_gap_factor(next: HeadingLevel) -> f64 {
    match next {
        HeadingLevel::H1 => 1.34,
        HeadingLevel::H2 => 1.64,
        HeadingLevel::H3 => 1.03,
        HeadingLevel::H4 => 1.25,
        HeadingLevel::H5 => 0.75,
        HeadingLevel::H6 => 0.64,
    }
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM
}

fn safe_link_uri(uri: &str) -> Option<String> {
    let uri = uri.trim();
    if uri.is_empty() || uri.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return None;
    }
    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        Some(uri.to_string())
    } else {
        None
    }
}

fn push_markdown_text(
    runs: &mut Vec<Run>,
    font: StandardFont,
    text: &str,
    style: &RunStyle,
    detect_autolinks: bool,
) {
    let text = sanitize_run(text);
    if !detect_autolinks {
        push_run(runs, font, style, &text);
        return;
    }

    for segment in text.split_inclusive(' ') {
        let token = segment.trim_end_matches(' ');
        let spaces = &segment[token.len()..];
        let lower = token.to_ascii_lowercase();
        let is_url = lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("www.");
        if is_url {
            let linked = token.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
            let suffix = &token[linked.len()..];
            let candidate = if lower.starts_with("www.") {
                format!("https://{linked}")
            } else {
                linked.to_string()
            };
            if let Some(uri) = safe_link_uri(&candidate) {
                let mut link_style = style.clone();
                link_style.link = Some(uri);
                link_style.color = TextColor::Link;
                push_run(runs, font, &link_style, linked);
                push_run(runs, font, style, suffix);
            } else {
                push_run(runs, font, style, token);
            }
        } else {
            push_run(runs, font, style, token);
        }
        push_run(runs, font, style, spaces);
    }
}

fn collect_footnotes(markdown: &str) -> Vec<(String, String)> {
    let mut definitions = Vec::new();
    let mut current: Option<(String, String)> = None;
    for event in Parser::new_ext(markdown, markdown_options()) {
        match event {
            Event::Start(Tag::FootnoteDefinition(label)) => {
                current = Some((label.into_string(), String::new()));
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                if let Some((label, text)) = current.take() {
                    definitions.push((label, text.trim().to_string()));
                }
            }
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                if let Some((_, buffer)) = current.as_mut() {
                    buffer.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, buffer)) = current.as_mut() {
                    buffer.push(' ');
                }
            }
            _ => {}
        }
    }
    definitions
}

/// Pre-scans the event stream (same pattern as [`collect_footnotes`]) and
/// returns one keep-with-next reserve per heading occurrence, in document
/// order. The first heading of each contiguous heading group carries the
/// height of the whole group (pre-gap plus one 1.6-leading line per heading)
/// plus one following body line; chained followers carry 0.0. Reserving the
/// group as a unit is deliberate: per-heading reserves would break between
/// chained headings and strand the earlier ones, and content already drawn
/// cannot be rewound.
fn collect_heading_reserves(markdown: &str, base: f64) -> Vec<f64> {
    let mut heights: Vec<f64> = Vec::new();
    let mut group_starts: Vec<usize> = Vec::new();
    let mut pending = false;
    let mut footnote_depth: u32 = 0;
    for event in Parser::new_ext(markdown, markdown_options()) {
        if footnote_depth > 0 {
            match event {
                Event::Start(_) => footnote_depth += 1,
                Event::End(_) => footnote_depth -= 1,
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let pre_gap = if pending {
                    base * adjacent_heading_gap_factor(level)
                } else {
                    group_starts.push(heights.len());
                    base * 0.8
                };
                heights.push(pre_gap + base * heading_scale(level) * 1.6);
                pending = false;
            }
            Event::End(TagEnd::Heading(_)) => pending = true,
            Event::Start(Tag::FootnoteDefinition(_)) => footnote_depth = 1,
            // Any other block start (or rule / inert HTML event) consumes the
            // heading margin in the render loop, closing the group.
            Event::Start(_) | Event::Rule | Event::Html(_) | Event::InlineHtml(_) => {
                pending = false;
            }
            _ => {}
        }
    }
    // One post-heading body line: the consumed heading margin plus a text
    // line at the body leading.
    let body_line = base * 0.85 + base * MARKDOWN_LINE_HEIGHT_FACTOR;
    let mut reserves = vec![0.0; heights.len()];
    for (index, &start) in group_starts.iter().enumerate() {
        let end = group_starts
            .get(index + 1)
            .copied()
            .unwrap_or(heights.len());
        reserves[start] = heights[start..end].iter().sum::<f64>() + body_line;
    }
    reserves
}

/// Renders Markdown (headings, paragraphs, bold/italic, lists, GFM tables,
/// code blocks, raw-HTML text content, horizontal rules) into a paginated
/// PDF.
pub fn markdown_to_pdf(markdown: &str, opts: &DocOptions) -> Result<Vec<u8>> {
    let opts = effective_opts(opts)?;
    let mut eng = LayoutEngine::new_markdown(&opts)?;
    eng.emit_title(&opts)?;
    let base = opts.base_font_size_pt;

    // Inline state.
    let mut runs: Vec<Run> = Vec::new();
    let mut bold: u32 = 0;
    let mut emph: u32 = 0;
    let mut strike: u32 = 0;
    let mut active_link: Option<String> = None;
    let mut in_heading: Option<HeadingLevel> = None;
    let mut pending_heading_margin: Option<HeadingLevel> = None;
    // Each entry is a list level; Some(n) = next ordered index, None = bullet.
    let mut lists: Vec<Option<u64>> = Vec::new();
    // Open block quote levels; each indents its content like a list level.
    let mut blockquote_depth: usize = 0;
    let mut pending_marker: Option<ListMarker> = None;
    let mut in_code_block = false;
    let mut code_buf = String::new();
    let mut skip_depth: u32 = 0; // inside ignored constructs (images)
    let mut footnote_depth: u32 = 0;
    let footnote_definitions = collect_footnotes(markdown);
    // Keep-with-next: per-heading-group reserves from a look-ahead pass.
    let heading_reserves = collect_heading_reserves(markdown, base);
    let mut heading_index = 0_usize;
    let mut footnote_numbers: BTreeMap<String, usize> = BTreeMap::new();
    let mut next_footnote = 1_usize;
    // Raw-HTML block accumulator (source renders literally as a paragraph).
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
        marker: &mut Option<ListMarker>,
        gap_after: f64,
    ) -> Result<()> {
        let marker_arg = marker.take();
        if runs.iter().all(|r| r.text.trim().is_empty()) && marker_arg.is_none() {
            runs.clear();
            return Ok(());
        }
        let block = std::mem::take(runs);
        eng.emit_paragraph(&block, size, indent, leading, marker_arg)?;
        eng.advance(gap_after);
        Ok(())
    }

    fn consume_heading_margin(
        eng: &mut LayoutEngine,
        pending: &mut Option<HeadingLevel>,
        base: f64,
    ) {
        if pending.take().is_some() {
            eng.advance(base * 0.85);
        }
    }

    for event in Parser::new_ext(markdown, markdown_options()) {
        if footnote_depth > 0 {
            match event {
                Event::Start(_) => footnote_depth += 1,
                Event::End(_) => footnote_depth -= 1,
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    // At a group start, reserve room for the whole contiguous
                    // heading group plus one body line (capped at a fresh
                    // page, mirroring emit_paragraph's break-before guard) so
                    // headings never strand at the bottom of a page.
                    if let Some(&reserve) = heading_reserves.get(heading_index)
                        && reserve > 0.0
                    {
                        let fresh_page_height =
                            eng.page_h - eng.continuation_top_margin - eng.bottom_margin;
                        eng.ensure_room(reserve.min(fresh_page_height));
                    }
                    heading_index += 1;
                    in_heading = Some(level);
                    if pending_heading_margin.take().is_some() {
                        eng.advance(base * adjacent_heading_gap_factor(level));
                    } else {
                        eng.advance(base * 0.8);
                    }
                }
                Tag::Paragraph => {
                    consume_heading_margin(&mut eng, &mut pending_heading_margin, base);
                }
                Tag::Strong => bold += 1,
                Tag::Emphasis => emph += 1,
                Tag::Strikethrough => strike += 1,
                Tag::List(start) => {
                    consume_heading_margin(&mut eng, &mut pending_heading_margin, base);
                    if lists.len() >= MAX_MARKDOWN_NESTING {
                        return Err(PdfCoreError::InvalidInput(format!(
                            "Markdown nesting depth exceeds limit {MAX_MARKDOWN_NESTING}"
                        )));
                    }
                    // In tight lists the parent item's inline text is still
                    // buffered (no Paragraph wrapper); flush it at the parent
                    // depth so child items start on their own lines.
                    if !runs.is_empty() || pending_marker.is_some() {
                        flush_block(
                            &mut eng,
                            &mut runs,
                            base,
                            18.0 * (lists.len() + blockquote_depth) as f64,
                            MARKDOWN_LINE_HEIGHT_FACTOR,
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
                            18.0 * (lists.len() + blockquote_depth) as f64,
                            MARKDOWN_LINE_HEIGHT_FACTOR,
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
                    pending_marker = Some(ListMarker::Text(marker));
                }
                Tag::CodeBlock(_) => {
                    consume_heading_margin(&mut eng, &mut pending_heading_margin, base);
                    // Fenced and indented blocks render the same way.
                    in_code_block = true;
                    code_buf.clear();
                }
                Tag::HtmlBlock => {
                    consume_heading_margin(&mut eng, &mut pending_heading_margin, base);
                    html_block = Some(String::new());
                }
                Tag::Table(_) => {
                    consume_heading_margin(&mut eng, &mut pending_heading_margin, base);
                    table_rows = Some(Vec::new());
                }
                Tag::TableHead | Tag::TableRow => {
                    table_row.clear();
                }
                Tag::TableCell => {
                    table_cell = Some(String::new());
                }
                Tag::Image { .. } => {
                    let style = RunStyle {
                        color: TextColor::Muted,
                        ..RunStyle::default()
                    };
                    push_run(
                        &mut runs,
                        StandardFont::HelveticaOblique,
                        &style,
                        "[Image omitted for secure conversion]",
                    );
                    skip_depth += 1;
                }
                Tag::Link { dest_url, .. } => {
                    active_link = safe_link_uri(&dest_url);
                }
                Tag::FootnoteDefinition(_) => footnote_depth = 1,
                Tag::BlockQuote(_) => {
                    consume_heading_margin(&mut eng, &mut pending_heading_margin, base);
                    if lists.len() + blockquote_depth >= MAX_MARKDOWN_NESTING {
                        return Err(PdfCoreError::InvalidInput(format!(
                            "Markdown nesting depth exceeds limit {MAX_MARKDOWN_NESTING}"
                        )));
                    }
                    blockquote_depth += 1;
                }
                // Everything else is ignored gracefully.
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(level) => {
                    let size = base * heading_scale(level);
                    flush_block(
                        &mut eng,
                        &mut runs,
                        size,
                        18.0 * (lists.len() + blockquote_depth) as f64,
                        1.6,
                        &mut pending_marker,
                        0.0,
                    )?;
                    in_heading = None;
                    pending_heading_margin = Some(level);
                }
                TagEnd::Paragraph => {
                    let indent = 18.0 * (lists.len() + blockquote_depth) as f64;
                    let gap = if lists.is_empty() { base } else { 0.0 };
                    flush_block(
                        &mut eng,
                        &mut runs,
                        base,
                        indent,
                        MARKDOWN_LINE_HEIGHT_FACTOR,
                        &mut pending_marker,
                        gap,
                    )?;
                }
                TagEnd::Item => {
                    // Tight list items carry inline text without a Paragraph.
                    let indent = 18.0 * (lists.len() + blockquote_depth) as f64;
                    flush_block(
                        &mut eng,
                        &mut runs,
                        base,
                        indent,
                        MARKDOWN_LINE_HEIGHT_FACTOR,
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
                TagEnd::Strikethrough => strike = strike.saturating_sub(1),
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    let indent = 18.0 * (lists.len() + blockquote_depth) as f64;
                    render_code_block(&mut eng, &code_buf, base, indent)?;
                    code_buf.clear();
                }
                TagEnd::HtmlBlock => {
                    if let Some(html) = html_block.take() {
                        let text = sanitize_run(&html);
                        if !text.trim().is_empty() {
                            let block = [Run {
                                font: StandardFont::Helvetica,
                                text,
                                style: RunStyle::default(),
                            }];
                            let indent = 18.0 * (lists.len() + blockquote_depth) as f64;
                            eng.emit_paragraph(
                                &block,
                                base,
                                indent,
                                MARKDOWN_LINE_HEIGHT_FACTOR,
                                None,
                            )?;
                            eng.advance(base);
                        }
                    }
                }
                TagEnd::Table => {
                    if let Some(rows) = table_rows.take()
                        && !rows.is_empty()
                    {
                        eng.advance(base * 0.35);
                        render_table(&mut eng, &rows, base, TableStyle::Markdown)?;
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
                        table_row.push(sanitize_table_cell(&cell));
                    }
                }
                TagEnd::Image => skip_depth = skip_depth.saturating_sub(1),
                TagEnd::Link => active_link = None,
                TagEnd::BlockQuote(_) => blockquote_depth = blockquote_depth.saturating_sub(1),
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
                    let style = RunStyle {
                        link: active_link.clone(),
                        strikethrough: strike > 0,
                        color: if active_link.is_some() {
                            TextColor::Link
                        } else {
                            TextColor::Black
                        },
                        ..RunStyle::default()
                    };
                    push_markdown_text(&mut runs, font, &text, &style, active_link.is_none());
                }
            }
            Event::Code(text) => {
                if skip_depth == 0 && !in_code_block {
                    if let Some(cell) = table_cell.as_mut() {
                        cell.push_str(&text);
                    } else {
                        let style = RunStyle {
                            link: active_link.clone(),
                            strikethrough: strike > 0,
                            color: if active_link.is_some() {
                                TextColor::Link
                            } else {
                                TextColor::Black
                            },
                            ..RunStyle::default()
                        };
                        push_run(
                            &mut runs,
                            StandardFont::Courier,
                            &style,
                            &sanitize_run(&text),
                        );
                    }
                }
            }
            Event::SoftBreak => {
                if skip_depth == 0 {
                    if let Some(cell) = table_cell.as_mut() {
                        cell.push(' ');
                    } else if in_code_block {
                        code_buf.push('\n');
                    } else {
                        let font = inline_font(bold, emph, in_heading.is_some());
                        let style = RunStyle {
                            link: active_link.clone(),
                            strikethrough: strike > 0,
                            color: if active_link.is_some() {
                                TextColor::Link
                            } else {
                                TextColor::Black
                            },
                            ..RunStyle::default()
                        };
                        push_run(&mut runs, font, &style, " ");
                    }
                }
            }
            Event::HardBreak => {
                if skip_depth == 0 {
                    if let Some(cell) = table_cell.as_mut() {
                        cell.push('\n');
                    } else if in_code_block {
                        code_buf.push('\n');
                    } else {
                        let font = inline_font(bold, emph, in_heading.is_some());
                        let style = RunStyle {
                            link: active_link.clone(),
                            strikethrough: strike > 0,
                            color: if active_link.is_some() {
                                TextColor::Link
                            } else {
                                TextColor::Black
                            },
                            ..RunStyle::default()
                        };
                        push_run(&mut runs, font, &style, "\n");
                    }
                }
            }
            Event::Rule => {
                consume_heading_margin(&mut eng, &mut pending_heading_margin, base);
                eng.ensure_room(base * 1.4);
                eng.advance(base * 0.7);
                let (x1, x2) = (eng.left(), eng.left() + eng.usable_width());
                let y = eng.y;
                eng.draw_rule(x1, x2, y, 0.5);
                eng.advance(base * 0.7);
            }
            // Raw HTML is inert source text, matching allowDangerousHtml:false.
            Event::Html(html) | Event::InlineHtml(html) => {
                consume_heading_margin(&mut eng, &mut pending_heading_margin, base);
                if skip_depth > 0 {
                    continue;
                }
                if let Some(buf) = html_block.as_mut() {
                    buf.push_str(&html);
                } else if let Some(cell) = table_cell.as_mut() {
                    cell.push_str(&html);
                } else {
                    let text = sanitize_run(&html);
                    if !text.trim().is_empty() {
                        let font = inline_font(bold, emph, in_heading.is_some());
                        let style = RunStyle {
                            link: active_link.clone(),
                            strikethrough: strike > 0,
                            color: if active_link.is_some() {
                                TextColor::Link
                            } else {
                                TextColor::Black
                            },
                            ..RunStyle::default()
                        };
                        push_run(&mut runs, font, &style, &text);
                    }
                }
            }
            Event::FootnoteReference(label) => {
                let label = label.into_string();
                let number = *footnote_numbers.entry(label).or_insert_with(|| {
                    let number = next_footnote;
                    next_footnote += 1;
                    number
                });
                let style = RunStyle {
                    superscript: true,
                    ..RunStyle::default()
                };
                let font = inline_font(bold, emph, in_heading.is_some());
                push_run(&mut runs, font, &style, &number.to_string());
            }
            Event::TaskListMarker(checked) => {
                pending_marker = Some(ListMarker::Task(checked));
            }
            Event::InlineMath(_) | Event::DisplayMath(_) => {}
        }
    }
    // Flush any dangling inline content (defensive; parser normally closes).
    if !runs.is_empty() {
        let indent = 18.0 * (lists.len() + blockquote_depth) as f64;
        flush_block(
            &mut eng,
            &mut runs,
            base,
            indent,
            MARKDOWN_LINE_HEIGHT_FACTOR,
            &mut pending_marker,
            0.0,
        )?;
    }

    if !footnote_definitions.is_empty() {
        eng.ensure_room(base * 2.0);
        eng.advance(base * 0.6);
        eng.draw_rule(
            eng.left(),
            eng.left() + eng.usable_width() * 0.35,
            eng.y,
            0.5,
        );
        eng.advance(base * 0.5);
        let heading = [Run {
            font: StandardFont::HelveticaBold,
            text: "Footnotes".to_string(),
            style: RunStyle::default(),
        }];
        eng.emit_paragraph(
            &heading,
            base * heading_scale(HeadingLevel::H2),
            0.0,
            1.6,
            None,
        )?;
        eng.advance(base * 0.85);

        let mut definitions = Vec::with_capacity(footnote_definitions.len());
        for (label, text) in &footnote_definitions {
            let number = *footnote_numbers.entry(label.clone()).or_insert_with(|| {
                let number = next_footnote;
                next_footnote += 1;
                number
            });
            definitions.push((number, text));
        }
        definitions.sort_by_key(|(number, _)| *number);
        for (number, text) in definitions {
            let backreference = match eng.font_session.resolve("↩") {
                Ok(_) => "↩",
                Err(PdfCoreError::TextUnsupported {
                    code: "glyph-coverage",
                    ..
                }) => "<-",
                Err(error) => return Err(error),
            };
            let note_runs = [
                Run {
                    font: StandardFont::HelveticaBold,
                    text: format!("{number}. "),
                    style: RunStyle::default(),
                },
                Run {
                    font: StandardFont::Helvetica,
                    text: format!("{} ", sanitize_run(text)),
                    style: RunStyle::default(),
                },
                Run {
                    font: StandardFont::Helvetica,
                    text: backreference.to_string(),
                    style: RunStyle::default(),
                },
            ];
            eng.emit_paragraph(&note_runs, base * 0.9, 0.0, 1.25, None)?;
            eng.advance(base * 0.2);
        }
    }

    eng.finish()
}

/// Renders a code block on per-line light-gray background rectangles. Standard
/// Courier keeps its character-count wrapping; embedded runs use hmtx widths.
fn render_code_block(eng: &mut LayoutEngine, code: &str, base: f64, indent: f64) -> Result<()> {
    let size = base * 0.9;
    let pad = 9.0;
    let lead = base * MARKDOWN_LINE_HEIGHT_FACTOR;
    let block_w = (eng.usable_width() - indent).max(size);
    // Courier is monospace at 600/1000 em.
    let char_w = 0.6 * size;
    let max_chars = (((block_w - 2.0 * pad) / char_w).floor() as usize).max(1);

    let mut lines: Vec<(String, FontChoice)> = Vec::new();
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
            lines.push((chunk, choice.clone()));
        }
    }
    if lines.is_empty() {
        lines.push((String::new(), FontChoice::Standard));
    }

    eng.advance(base * 0.35);
    // Blocks are atomic across page breaks: a block that does not fit in the
    // remaining space moves whole to the next page instead of splitting
    // mid-block (same policy as CSV logical rows). Only blocks taller than a
    // full page may split in the loop below.
    let block_height = 2.0 * pad + lead * lines.len() as f64;
    let fresh_page_height = eng.page_h - eng.continuation_top_margin - eng.bottom_margin;
    if block_height <= fresh_page_height + 0.01 && eng.y - block_height < eng.bottom_margin - 0.01 {
        eng.break_page();
    }
    let left = eng.left() + indent;
    let mut line_index = 0;
    while line_index < lines.len() {
        let available = eng.y - eng.bottom_margin - 2.0 * pad;
        let available_lines = (available / lead).floor().max(0.0) as usize;
        if available_lines == 0 {
            eng.break_page();
            continue;
        }
        let line_count = (lines.len() - line_index).min(available_lines);
        let fragment_height = 2.0 * pad + lead * line_count as f64;
        eng.draw_rect_fill(
            left,
            eng.y - fragment_height,
            block_w,
            fragment_height,
            0.953,
        );
        eng.advance(pad);
        for (chunk, choice) in &lines[line_index..line_index + line_count] {
            eng.advance(lead);
            let baseline = eng.y + size * 0.32;
            eng.draw_text_line(
                StandardFont::Courier,
                choice,
                size,
                (left + pad, baseline),
                chunk,
                TextColor::Black,
            )?;
        }
        eng.advance(pad);
        line_index += line_count;
        if line_index < lines.len() {
            eng.break_page();
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
            links: Vec::new(),
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

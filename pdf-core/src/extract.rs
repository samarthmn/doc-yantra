//! Zone 2 — text extraction and search via pdfium.

use pdfium_render::prelude::*;

use crate::error::{PdfCoreError, Result};

/// A single search match.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// 0-based page index.
    pub page_index: u32,
    /// Match bounds in PDF points, bottom-left origin: (x, y, width, height).
    ///
    /// Coordinates are in *unrotated* page user space — the coordinate space
    /// of the page's content stream, ignoring any `/Rotate` entry. A match on
    /// a `/Rotate 90` page reports the same rect as on the unrotated page.
    pub rect: (f64, f64, f64, f64),
}

/// Extracts all text in the document, pages separated by "\n\n".
///
/// # Limitations
///
/// Type3-font PDFs without `ToUnicode` CMaps (e.g. old Ghostscript or
/// `pstopdf` output) extract as raw character codes ("symbol soup") even when
/// the fonts' `/Differences` glyph names would allow recovering the real
/// text. This is a pdfium limitation: it does not map Type3 glyph names back
/// to Unicode. Such documents render correctly but extract/search as garbage.
pub fn extract_text(pdf: &[u8]) -> Result<String> {
    Ok(extract_text_pages(pdf)?.join("\n\n"))
}

/// Extracts text per page, in page order.
///
/// See [`extract_text`] for the Type3-font limitation note.
pub fn extract_text_pages(pdf: &[u8]) -> Result<Vec<String>> {
    let pdfium = crate::pdfium::lock()?;
    let doc = pdfium.load_pdf_from_byte_slice(pdf, None)?;
    let mut pages = Vec::with_capacity(doc.pages().len() as usize);
    for page in doc.pages().iter() {
        // A page with no text objects (or whose text layer fails to load)
        // yields an empty string rather than an error.
        let text = page.text().map(|t| page_text(&t)).unwrap_or_default();
        pages.push(text);
    }
    Ok(pages)
}

/// Collects a page's full character sequence in document order.
///
/// Deliberately avoids `PdfPageText::all()`: pdfium-render 0.9.2 implements
/// `all()` as `inside_rect(page.page_size())`, but `FPDFText_GetBoundedText`
/// expects *unrotated* user-space coordinates while `page_size()` is
/// rotation-transformed — on `/Rotate 90`/`270` pages every character outside
/// the dimension-swapped box is silently dropped. Iterating the text-page
/// characters (`FPDFText_GetUnicode` per index) is rotation-independent and
/// includes pdfium's generated line-break characters.
fn page_text(text: &PdfPageText<'_>) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(text.len().max(0) as usize);
    for ch in text.chars().iter() {
        let value = ch.unicode_value();
        match char::from_u32(value) {
            // NUL marks glyphs without a Unicode mapping; emit nothing.
            Some('\0') => {}
            Some(c) => {
                let mut buf = [0u16; 2];
                units.extend_from_slice(c.encode_utf16(&mut buf));
            }
            // Non-BMP characters surface from pdfium as UTF-16 surrogate
            // halves; keep them so adjacent halves recombine below.
            None if value <= 0xFFFF => units.push(value as u16),
            None => {}
        }
    }
    String::from_utf16_lossy(&units)
}

/// Finds all occurrences of `query` and returns their page + bounds.
///
/// Search walks the text-page character index, so matches are found on
/// rotated (`/Rotate 90/180/270`) pages too; hit rects are reported in
/// unrotated page user space (see [`SearchHit::rect`]).
///
/// See [`extract_text`] for the Type3-font limitation note.
pub fn search(pdf: &[u8], query: &str, case_sensitive: bool) -> Result<Vec<SearchHit>> {
    if query.is_empty() {
        return Err(PdfCoreError::InvalidInput(
            "search query must not be empty".to_string(),
        ));
    }

    let pdfium = crate::pdfium::lock()?;
    let doc = pdfium.load_pdf_from_byte_slice(pdf, None)?;
    let options = PdfSearchOptions::new().match_case(case_sensitive);
    let mut hits = Vec::new();

    for (page_index, page) in doc.pages().iter().enumerate() {
        // Pages whose text layer cannot be loaded simply contribute no hits.
        let Ok(text) = page.text() else { continue };
        let search = text.search(query, &options)?;
        for result in search.iter(PdfSearchDirection::SearchForward) {
            if let Some(rect) = union_segment_bounds(&result) {
                hits.push(SearchHit {
                    page_index: page_index as u32,
                    rect,
                });
            }
        }
    }

    Ok(hits)
}

/// Unions the bounding boxes of all text segments in a search result into one
/// rect: (min_x, min_y, width, height) in points, bottom-left origin. Returns
/// `None` when the result contains no segments (should not happen in practice).
fn union_segment_bounds(segments: &PdfPageTextSegments<'_>) -> Option<(f64, f64, f64, f64)> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut seen = false;

    for segment in segments.iter() {
        let bounds = segment.bounds();
        min_x = min_x.min(f64::from(bounds.left().value));
        min_y = min_y.min(f64::from(bounds.bottom().value));
        max_x = max_x.max(f64::from(bounds.right().value));
        max_y = max_y.max(f64::from(bounds.top().value));
        seen = true;
    }

    if seen {
        Some((min_x, min_y, max_x - min_x, max_y - min_y))
    } else {
        None
    }
}

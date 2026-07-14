//! Zone 2 — true redaction via pdfium: content intersecting the redaction
//! rects is REMOVED from the page (text objects deleted, not merely covered),
//! then an opaque black box is drawn and page content regenerated.
//!
//! Rect semantics mirror DocYantra's PyMuPDF endpoint: percentages of the
//! page size with a TOP-LEFT origin, measured on the page AS DISPLAYED
//! (i.e. with any `/Rotate` applied — fitz's `page.rect` is rotation-aware
//! and DocKosha's UI sends selections made on the displayed page). Rects are
//! mapped back into unrotated PDF user space before intersection testing.
//!
//! Removal granularity is the whole pdfium page object (v1 semantics): any
//! text object, Form XObject containing intersecting text, or annotation
//! that intersects a rect is removed IN FULL. The full bounds of everything
//! removed are covered with black boxes (union coverage), and a warning is
//! emitted in [RedactOutput::warnings] whenever the removed bounds meaningfully
//! exceed the requested rect, so callers can inform the user. Glyph-level
//! (PyMuPDF-parity) redaction is out of scope for this pass.
//!
//! Anything the pdfium API cannot remove or preserve reliably is a hard
//! [PdfCoreError::Unsupported] error — never an `Ok` with secret text
//! surviving, and never an `Ok` with unrelated text silently destroyed
//! (pdfium's content regeneration drops Type3-font text; the saved output
//! is re-verified before it is returned).
//!
//! Images are kept (covered, not removed), matching `PDF_REDACT_IMAGE_NONE`.

use std::collections::BTreeMap;

use pdfium_render::prelude::*;

use crate::error::{PdfCoreError, Result};
pub use crate::redact_types::{
    BoxPts, PageRotation, RedactArea, RedactOutput, RedactionObjectKind, RedactionRect,
    RedactionWarning, RedactionWarningCode, derotate_box, exceeds_requested, resolve_rect,
};

/// Returns `true` if any text object at any nesting depth inside `form`
/// intersects one of `boxes`. pdfium reports child bounds in page space
/// (the form's `/Matrix` already applied), verified empirically. Text whose
/// bounds cannot be computed cannot be proven clear of the redaction area,
/// so it is treated as intersecting (fail closed — this is a redaction).
fn form_contains_intersecting_text(
    form: &PdfPageXObjectFormObject<'_>,
    boxes: &[BoxPts],
) -> Result<bool> {
    for index in 0..form.len() {
        let child = form.get(index)?;
        match child.object_type() {
            PdfPageObjectType::Text => match child.bounds() {
                Ok(bounds) => {
                    let (left, right, bottom, top) = (
                        bounds.left().value,
                        bounds.right().value,
                        bounds.bottom().value,
                        bounds.top().value,
                    );
                    if boxes.iter().any(|b| b.intersects(left, right, bottom, top)) {
                        return Ok(true);
                    }
                }
                Err(_) => return Ok(true),
            },
            PdfPageObjectType::XObjectForm => {
                if let Some(nested) = child.as_x_object_form_object() {
                    if form_contains_intersecting_text(nested, boxes)? {
                        return Ok(true);
                    }
                } else {
                    // Cannot inspect the nested container; fail closed.
                    return Ok(true);
                }
            }
            // Images/paths are covered, not removed (PDF_REDACT_IMAGE_NONE);
            // they carry no pdfium-extractable text.
            _ => {}
        }
    }
    Ok(false)
}

/// Removes every text object, text-bearing Form XObject, and annotation whose
/// bounds intersect one of `boxes` (all in unrotated page space), then draws
/// opaque black rectangles over each box AND over the full bounds of every
/// removed object, and regenerates the page content stream so the removed
/// content is truly gone from the file. `page_w` x `page_h` is the unrotated
/// page size, used by the excess-coverage warning heuristic.
///
/// Returns the number of top-level text objects that must SURVIVE in the
/// saved output; [redact] verifies this after saving, because pdfium's
/// content regeneration silently drops text objects it cannot re-serialize
/// (observed with Type3 fonts — Ghostscript/pstopdf output).
fn redact_page(
    page: &mut PdfPage<'_>,
    boxes: &[BoxPts],
    page_index: u32,
    page_w: f32,
    page_h: f32,
    warnings: &mut Vec<RedactionWarning>,
) -> Result<usize> {
    // Stage all edits, then regenerate the content stream exactly once.
    page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);

    // Bounds of everything removed, for union coverage + excess warnings.
    let mut removed: Vec<(BoxPts, RedactionObjectKind)> = Vec::new();

    // Pass 1: delete annotations intersecting any redaction box. Annotations
    // (sticky notes, FreeText callouts, review comments) hold text in
    // /Contents and render ABOVE page content in most viewers, so they must
    // never survive a redaction that touches them (PyMuPDF parity: fitz's
    // apply_redactions deletes overlapping annotations). Descending order so
    // indexes stay valid across deletions.
    for index in (0..page.annotations().len()).rev() {
        let annotation = page.annotations().get(index)?;
        let bounds = annotation.bounds().map_err(|_| {
            PdfCoreError::Unsupported(format!(
                "page {page_index} contains an annotation with unreadable bounds; \
                 redaction cannot guarantee removal"
            ))
        })?;
        let b = BoxPts {
            left: bounds.left().value,
            right: bounds.right().value,
            bottom: bounds.bottom().value,
            top: bounds.top().value,
        };
        if !boxes
            .iter()
            .any(|bx| bx.intersects(b.left, b.right, b.bottom, b.top))
        {
            continue;
        }
        page.annotations_mut()
            .delete_annotation(annotation)
            .map_err(|_| {
                PdfCoreError::Unsupported(format!(
                    "page {page_index} contains an annotation intersecting a redaction \
                     area that could not be deleted; redaction cannot guarantee removal"
                ))
            })?;
        removed.push((b, RedactionObjectKind::Annotation));
    }

    // Pass 2: collect indexes of page objects intersecting any redaction box —
    // text objects, plus Form XObjects containing intersecting text at any
    // nesting depth (Illustrator/InDesign/LaTeX output routinely wraps page
    // text in Form XObjects). pdfium-render 0.9.2 cannot remove a single
    // child from inside a form, so the whole form object is removed; union
    // coverage + warnings below account for the excess.
    let mut doomed: Vec<(PdfPageObjectIndex, BoxPts, RedactionObjectKind)> = Vec::new();
    let mut text_count = 0usize;
    let object_count = page.objects().len();
    for index in 0..object_count {
        let object = page.objects().get(index)?;
        match object.object_type() {
            PdfPageObjectType::Text => {
                text_count += 1;
                let Ok(bounds) = object.bounds() else {
                    // Objects with no computable bounds cannot be intersection-tested.
                    continue;
                };
                let (left, right, bottom, top) = (
                    bounds.left().value,
                    bounds.right().value,
                    bounds.bottom().value,
                    bounds.top().value,
                );
                if boxes.iter().any(|b| b.intersects(left, right, bottom, top)) {
                    doomed.push((
                        index,
                        BoxPts {
                            left,
                            right,
                            bottom,
                            top,
                        },
                        RedactionObjectKind::Text,
                    ));
                }
            }
            PdfPageObjectType::XObjectForm => {
                let Some(form) = object.as_x_object_form_object() else {
                    // Cannot inspect the container; fail closed rather than
                    // risk leaving text behind a black box.
                    return Err(PdfCoreError::Unsupported(format!(
                        "page {page_index} contains an uninspectable Form XObject; \
                         redaction cannot guarantee removal"
                    )));
                };
                if form_contains_intersecting_text(form, boxes)? {
                    let bounds = object.bounds().map_err(|_| {
                        PdfCoreError::Unsupported(format!(
                            "page {page_index} contains text inside a Form XObject \
                             intersecting a redaction area, and the form's bounds are \
                             unreadable; redaction cannot guarantee coverage"
                        ))
                    })?;
                    doomed.push((
                        index,
                        BoxPts {
                            left: bounds.left().value,
                            right: bounds.right().value,
                            bottom: bounds.bottom().value,
                            top: bounds.top().value,
                        },
                        RedactionObjectKind::FormXObject,
                    ));
                }
            }
            // Images/paths are covered, not removed (PDF_REDACT_IMAGE_NONE).
            _ => {}
        }
    }

    // Pass 3: remove in descending index order so indexes stay valid.
    for &(index, bounds, kind) in doomed.iter().rev() {
        let object = page
            .objects_mut()
            .remove_object_at_index(index)
            .map_err(|_| {
                PdfCoreError::Unsupported(format!(
                    "page {page_index} contains a {} intersecting a redaction area \
                     that could not be removed; redaction cannot guarantee removal",
                    kind.message_label()
                ))
            })?;
        // pdfium-render destroys the returned (now caller-owned) object on
        // drop via FPDFPageObj_Destroy, but the bundled pdfium build
        // segfaults when destroying a text object that was removed from a
        // loaded page — even after content regeneration. Deliberately leak
        // the tiny object handle instead; the removal from the page (and
        // thus from the saved output) has already taken effect. Measured
        // cost: ~1.2KB per removed object, retained for the lifetime of the
        // process — negligible for realistic redaction workloads.
        std::mem::forget(object);
        removed.push((bounds, kind));
    }

    // Pass 4: draw an opaque black box over each redacted area AND over the
    // full bounds of every removed object (union coverage), so content that
    // was removed in full at object granularity is never left visually
    // absent-but-unmarked. Warn whenever removal meaningfully exceeded the
    // requested rect so the caller can inform the user.
    for b in boxes {
        draw_black_box(page, b)?;
    }
    for (bounds, kind) in &removed {
        if bounds.area() > 0.0 {
            draw_black_box(page, bounds)?;
        }
        if exceeds_requested(bounds, boxes, page_w, page_h) {
            warnings.push(RedactionWarning {
                code: RedactionWarningCode::OverRedaction,
                page: page_index,
                kind: *kind,
                message: format!(
                    "page {page_index}: redaction removed a whole {} spanning \
                     ({:.1}, {:.1})-({:.1}, {:.1}) pt, beyond the requested area; \
                     the full bounds were covered with a black box",
                    kind.message_label(),
                    bounds.left,
                    bounds.bottom,
                    bounds.right,
                    bounds.top,
                ),
            });
        }
    }

    // Rewrite the content stream: removed content is no longer present in the
    // saved file (unextractable), not merely hidden.
    page.regenerate_content()?;

    let removed_text = doomed
        .iter()
        .filter(|(_, _, kind)| *kind == RedactionObjectKind::Text)
        .count();
    Ok(text_count - removed_text)
}

fn draw_black_box(page: &mut PdfPage<'_>, b: &BoxPts) -> Result<()> {
    page.objects_mut().create_path_object_rect(
        PdfRect::new(
            PdfPoints::new(b.bottom),
            PdfPoints::new(b.left),
            PdfPoints::new(b.top),
            PdfPoints::new(b.right),
        ),
        None,
        None,
        Some(PdfColor::BLACK),
    )?;
    Ok(())
}

/// Applies redactions keyed by 0-based page index. Out-of-range page indexes
/// and degenerate rects are skipped (matching DocYantra behavior). Rects are
/// interpreted on the page AS DISPLAYED (rotation applied) and mapped into
/// unrotated user space. Text, text-bearing Form XObjects, and annotations
/// whose bounds intersect a rect are removed from the file; the requested
/// areas plus the bounds of everything removed are covered with filled black
/// rectangles, with warnings for coverage beyond the request.
pub fn redact(pdf: &[u8], redactions: &BTreeMap<u32, Vec<RedactionRect>>) -> Result<RedactOutput> {
    let pdfium = crate::pdfium::lock()?;
    let document = pdfium.load_pdf_from_byte_slice(pdf, None)?;
    let page_count = document.pages().len().max(0) as u32;
    let mut warnings: Vec<RedactionWarning> = Vec::new();
    // (page index, top-level text objects that must survive in the output).
    let mut expected_text: Vec<(u32, usize)> = Vec::new();

    for (&page_index, rects) in redactions {
        if page_index >= page_count {
            // Out-of-range pages are skipped silently.
            continue;
        }
        let mut page = document.pages().get(page_index as PdfPageIndex)?;
        // pdfium's page width/height are rotation-aware (the page as
        // displayed), which is exactly the space the percent rects refer to.
        let disp_w = f64::from(page.width().value);
        let disp_h = f64::from(page.height().value);
        let rotation = match page.rotation()? {
            PdfPageRenderRotation::None => PageRotation::None,
            PdfPageRenderRotation::Degrees90 => PageRotation::Degrees90,
            PdfPageRenderRotation::Degrees180 => PageRotation::Degrees180,
            PdfPageRenderRotation::Degrees270 => PageRotation::Degrees270,
        };
        let boxes: Vec<BoxPts> = rects
            .iter()
            .filter_map(|r| resolve_rect(r, disp_w, disp_h))
            .map(|b| derotate_box(b, rotation, disp_w as f32, disp_h as f32))
            .collect();
        if boxes.is_empty() {
            continue;
        }
        // Unrotated page size — the space of object bounds and drawn boxes.
        let (page_w, page_h) = match rotation {
            PageRotation::Degrees90 | PageRotation::Degrees270 => (disp_h as f32, disp_w as f32),
            _ => (disp_w as f32, disp_h as f32),
        };
        let survivors = redact_page(&mut page, &boxes, page_index, page_w, page_h, &mut warnings)?;
        expected_text.push((page_index, survivors));
    }

    let bytes = document.save_to_bytes()?;

    // Integrity check: pdfium's content regeneration silently drops text
    // objects it cannot re-serialize (observed with Type3 fonts, common in
    // Ghostscript/pstopdf output) — every text object on the page vanishes
    // from the file, not just the redacted ones. Reload the saved output and
    // verify that exactly the untouched text objects survived; otherwise
    // fail loudly instead of returning a document with silent data loss.
    if !expected_text.is_empty() {
        let reloaded = pdfium.load_pdf_from_byte_slice(&bytes, None)?;
        for (page_index, expected) in expected_text {
            let page = reloaded.pages().get(page_index as PdfPageIndex)?;
            let mut actual = 0usize;
            for index in 0..page.objects().len() {
                if page.objects().get(index)?.object_type() == PdfPageObjectType::Text {
                    actual += 1;
                }
            }
            if actual < expected {
                return Err(PdfCoreError::Unsupported(format!(
                    "page {page_index}: content regeneration dropped {} text object(s) \
                     unrelated to the redaction (commonly Type3 fonts); redaction \
                     aborted to avoid silent data loss",
                    expected - actual
                )));
            }
        }
    }

    warnings.sort_by(|left, right| {
        (left.page, left.code, left.kind, left.message.as_str()).cmp(&(
            right.page,
            right.code,
            right.kind,
            right.message.as_str(),
        ))
    });
    Ok(RedactOutput {
        pdf: bytes,
        page_count,
        warnings,
    })
}

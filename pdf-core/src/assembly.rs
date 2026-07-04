//! Zone 1 — document assembly: merge, split, page extraction/reordering/
//! rotation/deletion. Pure lopdf; no pdfium dependency.
//!
//! Page numbers in this module are 1-based, matching user-facing semantics.

use std::collections::{BTreeMap, BTreeSet};

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::error::{PdfCoreError, Result};

/// Page-tree attributes that pages may inherit from ancestor `Pages` nodes
/// (PDF 32000-1 §7.7.3.4). They must be materialized onto each page dict
/// before re-parenting pages to a flat root, or pages lose fonts/size.
const INHERITABLE_PAGE_KEYS: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Upper bound on `/Parent` chain walks, guarding against reference cycles.
const PARENT_CHAIN_LIMIT: usize = 64;

/// Merges the given PDFs into one document, preserving page order.
pub fn merge(pdfs: &[&[u8]]) -> Result<Vec<u8>> {
    if pdfs.is_empty() {
        return Err(PdfCoreError::InvalidInput(
            "merge requires at least one input document".into(),
        ));
    }

    // Renumber every document into a disjoint object-id range, collect the
    // page objects (in page order) and all other objects.
    let mut max_id = 1u32;
    let mut merged_pages: Vec<(ObjectId, Object)> = Vec::new();
    let mut all_objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut info: Option<Object> = None;
    let mut acroform = AcroFormMerger::default();

    for (index, bytes) in pdfs.iter().enumerate() {
        let mut doc = load_doc(bytes)?;
        // Pages get re-parented to a single flat Pages root below, so any
        // attributes inherited from nested page trees must be copied first.
        materialize_inherited_attributes(&mut doc)?;
        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;
        if index == 0 {
            // Carry the FIRST document's /Info into the merged trailer
            // (references are already renumbered at this point).
            info = document_info(&doc);
        }
        acroform.absorb(&doc);
        for (_, page_id) in doc.get_pages() {
            let page_object = doc.get_object(page_id)?.to_owned();
            merged_pages.push((page_id, page_object));
        }
        all_objects.extend(doc.objects);
    }

    if merged_pages.is_empty() {
        return Err(PdfCoreError::InvalidInput(
            "input documents contain no pages".into(),
        ));
    }

    let mut document = Document::with_version("1.7");
    let mut catalog_id: Option<ObjectId> = None;
    let mut pages_id: Option<ObjectId> = None;

    for (object_id, object) in all_objects {
        match object.type_name().unwrap_or(b"") {
            b"Catalog" => {
                // Keep the first catalog; later ones are dropped (then pruned).
                if catalog_id.is_none() {
                    catalog_id = Some(object_id);
                    document.objects.insert(object_id, object);
                }
            }
            // All Pages nodes are replaced by one fresh flat root, built below
            // under the first document's Pages id.
            b"Pages" => {
                if pages_id.is_none() {
                    pages_id = Some(object_id);
                }
            }
            b"Page" => {}     // Re-inserted below with a fixed /Parent.
            b"Outlines" => {} // Outline merging is not supported; dropped.
            b"Outline" => {}
            _ => {
                document.objects.insert(object_id, object);
            }
        }
    }

    let catalog_id = catalog_id
        .ok_or_else(|| PdfCoreError::InvalidInput("no /Catalog found in input documents".into()))?;
    let pages_id = pages_id.ok_or_else(|| {
        PdfCoreError::InvalidInput("no /Pages tree found in input documents".into())
    })?;

    // Insert every page with its parent fixed to the new flat Pages root.
    let mut kids: Vec<Object> = Vec::with_capacity(merged_pages.len());
    for (page_id, page_object) in &merged_pages {
        let mut page_dict = page_object.as_dict()?.clone();
        page_dict.set("Parent", Object::Reference(pages_id));
        document
            .objects
            .insert(*page_id, Object::Dictionary(page_dict));
        kids.push(Object::Reference(*page_id));
    }

    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Count", merged_pages.len() as i64);
    pages_dict.set("Kids", kids);
    document
        .objects
        .insert(pages_id, Object::Dictionary(pages_dict));

    if let Some(Object::Dictionary(catalog)) = document.objects.get_mut(&catalog_id) {
        catalog.set("Pages", Object::Reference(pages_id));
        catalog.remove(b"Outlines");
        // The kept catalog's own AcroForm (if any) is superseded by the
        // merged one built below from ALL inputs.
        catalog.remove(b"AcroForm");
    }
    document.max_id = max_id;
    if let Some(acroform_dict) = acroform.into_dictionary() {
        let acroform_id = document.add_object(Object::Dictionary(acroform_dict));
        if let Some(Object::Dictionary(catalog)) = document.objects.get_mut(&catalog_id) {
            catalog.set("AcroForm", Object::Reference(acroform_id));
        }
    }
    document.trailer.set("Root", catalog_id);
    if let Some(info) = info {
        document.trailer.set("Info", info);
    }
    document.prune_objects();
    document.renumber_objects();

    save_doc(document)
}

/// Splits a PDF into one single-page PDF per page.
pub fn split(pdf: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut doc = load_doc(pdf)?;
    let page_map = doc.get_pages();
    if page_map.is_empty() {
        return Err(PdfCoreError::InvalidInput("document has no pages".into()));
    }
    materialize_inherited_attributes(&mut doc)?;

    let mut parts = Vec::with_capacity(page_map.len());
    for page_id in page_map.into_values() {
        let mut single = doc.clone();
        rebuild_page_tree(&mut single, &[page_id])?;
        parts.push(save_doc(single)?);
    }
    Ok(parts)
}

/// Returns a new PDF containing only `pages` (1-based, in the order given).
pub fn extract_pages(pdf: &[u8], pages: &[u32]) -> Result<Vec<u8>> {
    let mut doc = load_doc(pdf)?;
    let page_map = doc.get_pages();
    validate_page_numbers(pages, page_map.len())?;
    materialize_inherited_attributes(&mut doc)?;

    let mut selected = page_ids_for(&page_map, pages.iter().copied())?;
    // A page selected more than once must become distinct page OBJECTS: a
    // /Kids array naming the same object twice is no longer a tree (PDF
    // 32000-1 §7.7.3.2), and later per-page edits (e.g. rotation) would hit
    // the shared dict once per occurrence. The page dict is copied; content
    // and resource references may stay shared.
    let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
    for page_id in selected.iter_mut() {
        if !seen.insert(*page_id) {
            let copy = doc.get_dictionary(*page_id)?.clone();
            *page_id = doc.add_object(Object::Dictionary(copy));
        }
    }
    rebuild_page_tree(&mut doc, &selected)?;
    save_doc(doc)
}

/// Returns a new PDF with `pages` (1-based) removed.
pub fn delete_pages(pdf: &[u8], pages: &[u32]) -> Result<Vec<u8>> {
    let mut doc = load_doc(pdf)?;
    let page_map = doc.get_pages();
    validate_page_numbers(pages, page_map.len())?;

    let to_delete: BTreeSet<u32> = pages.iter().copied().collect();
    if to_delete.len() == page_map.len() {
        return Err(PdfCoreError::InvalidInput(
            "cannot delete all pages of a document".into(),
        ));
    }
    materialize_inherited_attributes(&mut doc)?;

    let kept: Vec<ObjectId> = page_map
        .iter()
        .filter(|(number, _)| !to_delete.contains(number))
        .map(|(_, id)| *id)
        .collect();
    rebuild_page_tree(&mut doc, &kept)?;
    save_doc(doc)
}

/// Reorders pages to `order` (1-based; must be a permutation of all pages).
pub fn reorder_pages(pdf: &[u8], order: &[u32]) -> Result<Vec<u8>> {
    let mut doc = load_doc(pdf)?;
    let page_map = doc.get_pages();
    let page_count = page_map.len();

    if order.len() != page_count {
        return Err(PdfCoreError::InvalidInput(format!(
            "order lists {} pages but the document has {page_count}",
            order.len()
        )));
    }
    let mut seen = vec![false; page_count];
    for &number in order {
        let index = number
            .checked_sub(1)
            .map(|i| i as usize)
            .filter(|&i| i < page_count)
            .ok_or_else(|| {
                PdfCoreError::InvalidInput(format!("page {number} out of range 1..={page_count}"))
            })?;
        if seen[index] {
            return Err(PdfCoreError::InvalidInput(format!(
                "page {number} appears more than once; order must be a permutation of 1..={page_count}"
            )));
        }
        seen[index] = true;
    }
    materialize_inherited_attributes(&mut doc)?;

    let ordered = page_ids_for(&page_map, order.iter().copied())?;
    rebuild_page_tree(&mut doc, &ordered)?;
    save_doc(doc)
}

/// Rotates pages by `degrees` (multiple of 90). `pages = None` rotates all.
pub fn rotate_pages(pdf: &[u8], pages: Option<&[u32]>, degrees: i32) -> Result<Vec<u8>> {
    if degrees % 90 != 0 {
        return Err(PdfCoreError::InvalidInput(format!(
            "rotation must be a multiple of 90 degrees, got {degrees}"
        )));
    }

    let mut doc = load_doc(pdf)?;
    let page_map = doc.get_pages();
    let targets: Vec<ObjectId> = match pages {
        Some(numbers) => {
            validate_page_numbers(numbers, page_map.len())?;
            page_ids_for(&page_map, numbers.iter().copied())?
        }
        None => page_map.into_values().collect(),
    };

    // Deduplicate by OBJECT id so a page dict reached twice — the same page
    // listed twice, or two page numbers sharing one object (malformed inputs
    // with duplicate /Kids refs) — is rotated exactly once.
    let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
    for page_id in targets {
        if !seen.insert(page_id) {
            continue;
        }
        let existing = resolve_inherited_attribute(&doc, page_id, b"Rotate")
            .and_then(|object| object_to_i64(&doc, &object))
            .unwrap_or(0);
        let rotation = (existing + i64::from(degrees)).rem_euclid(360);
        doc.get_dictionary_mut(page_id)?.set("Rotate", rotation);
    }
    save_doc(doc)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// AcroForm keys handled structurally by [`AcroFormMerger`]; all other keys
/// are taken from the first input that has an AcroForm (first-wins).
const ACROFORM_MERGED_KEYS: [&[u8]; 3] = [b"Fields", b"DR", b"NeedAppearances"];

/// Accumulates a single merged `/AcroForm` across the input documents:
/// `/Fields` arrays are unioned in input order, `/DR` resource dictionaries
/// are merged best-effort (on a resource-name collision the FIRST document's
/// entry wins), `/NeedAppearances` is set if ANY input sets it, and every
/// other key comes from the first document that carries an AcroForm.
#[derive(Default)]
struct AcroFormMerger {
    base: Option<Dictionary>,
    fields: Vec<Object>,
    resources: Dictionary,
    need_appearances: bool,
}

impl AcroFormMerger {
    /// Folds `doc`'s AcroForm (if any) into the accumulator. Must be called
    /// after `doc` was renumbered into the merged object-id space, so the
    /// collected references stay valid.
    fn absorb(&mut self, doc: &Document) {
        let Some(acroform) = doc
            .catalog()
            .ok()
            .and_then(|catalog| catalog.get(b"AcroForm").ok())
            .and_then(|object| resolve_dict(doc, object))
        else {
            return;
        };

        if let Some(fields) = acroform
            .get(b"Fields")
            .ok()
            .and_then(|object| resolve_array(doc, object))
        {
            self.fields.extend(fields);
        }
        if let Some(dr) = acroform
            .get(b"DR")
            .ok()
            .and_then(|object| resolve_dict(doc, object))
        {
            merge_resource_dicts(&mut self.resources, &dr, doc);
        }
        self.need_appearances |= acroform
            .get(b"NeedAppearances")
            .ok()
            .and_then(|object| doc.dereference(object).ok())
            .and_then(|(_, object)| object.as_bool().ok())
            .unwrap_or(false);

        match &mut self.base {
            None => {
                let mut base = acroform;
                for key in ACROFORM_MERGED_KEYS {
                    base.remove(key);
                }
                self.base = Some(base);
            }
            Some(base) => {
                for (key, value) in acroform.iter() {
                    if !ACROFORM_MERGED_KEYS.contains(&key.as_slice()) && !base.has(key) {
                        base.set(key.clone(), value.clone());
                    }
                }
            }
        }
    }

    /// The merged AcroForm dictionary, or `None` if no input had one.
    fn into_dictionary(self) -> Option<Dictionary> {
        let mut dict = self.base?;
        dict.set("Fields", Object::Array(self.fields));
        if !self.resources.is_empty() {
            dict.set("DR", Object::Dictionary(self.resources));
        }
        if self.need_appearances {
            dict.set("NeedAppearances", true);
        }
        Some(dict)
    }
}

/// Merges resource dictionary `incoming` into `merged`, materializing
/// categories (`/Font`, `/XObject`, …) as direct dictionaries and unioning
/// their per-name entries; on a name collision the existing (first-seen)
/// entry wins. Non-dictionary categories (e.g. `/ProcSet` arrays) are kept
/// whole, first-wins.
fn merge_resource_dicts(merged: &mut Dictionary, incoming: &Dictionary, doc: &Document) {
    for (category, value) in incoming.iter() {
        let Some(incoming_entries) = resolve_dict(doc, value) else {
            if !merged.has(category) {
                merged.set(category.clone(), value.clone());
            }
            continue;
        };
        let mut combined = match merged.get(category) {
            Ok(Object::Dictionary(existing)) => existing.clone(),
            Ok(_) => continue, // First-seen non-dict category wins.
            Err(_) => Dictionary::new(),
        };
        for (name, object) in incoming_entries.iter() {
            if !combined.has(name) {
                combined.set(name.clone(), object.clone());
            }
        }
        merged.set(category.clone(), Object::Dictionary(combined));
    }
}

/// The trailer `/Info` of `doc` — as the original trailer object (reference
/// or inline dictionary) — if it resolves to a dictionary.
fn document_info(doc: &Document) -> Option<Object> {
    let info = doc.trailer.get(b"Info").ok()?;
    matches!(doc.dereference(info), Ok((_, Object::Dictionary(_)))).then(|| info.clone())
}

/// Resolves an object that may be a reference chain to a dictionary.
fn resolve_dict(doc: &Document, object: &Object) -> Option<Dictionary> {
    match doc.dereference(object) {
        Ok((_, Object::Dictionary(dict))) => Some(dict.clone()),
        _ => None,
    }
}

/// Resolves an object that may be a reference chain to an array.
fn resolve_array(doc: &Document, object: &Object) -> Option<Vec<Object>> {
    match doc.dereference(object) {
        Ok((_, Object::Array(array))) => Some(array.clone()),
        _ => None,
    }
}

/// Parses PDF bytes, rejecting empty input with `InvalidInput`.
fn load_doc(bytes: &[u8]) -> Result<Document> {
    if bytes.is_empty() {
        return Err(PdfCoreError::InvalidInput("empty PDF input".into()));
    }
    Ok(Document::load_mem(bytes)?)
}

/// Ensures `pages` is non-empty and every entry is within `1..=page_count`.
fn validate_page_numbers(pages: &[u32], page_count: usize) -> Result<()> {
    if pages.is_empty() {
        return Err(PdfCoreError::InvalidInput("no pages specified".into()));
    }
    for &number in pages {
        if number == 0 || number as usize > page_count {
            return Err(PdfCoreError::InvalidInput(format!(
                "page {number} out of range 1..={page_count}"
            )));
        }
    }
    Ok(())
}

/// Maps 1-based page numbers to their object ids using a `get_pages` map.
fn page_ids_for(
    page_map: &BTreeMap<u32, ObjectId>,
    numbers: impl Iterator<Item = u32>,
) -> Result<Vec<ObjectId>> {
    numbers
        .map(|number| {
            page_map.get(&number).copied().ok_or_else(|| {
                PdfCoreError::InvalidInput(format!("page {number} not found in document"))
            })
        })
        .collect()
}

/// Copies inheritable attributes (Resources, MediaBox, CropBox, Rotate) from
/// ancestor `Pages` nodes onto every page dict that is missing them, so pages
/// can safely be re-parented to a flat root without losing fonts or size.
fn materialize_inherited_attributes(doc: &mut Document) -> Result<()> {
    let page_ids: Vec<ObjectId> = doc.page_iter().collect();
    let mut updates: Vec<(ObjectId, &'static [u8], Object)> = Vec::new();
    for &page_id in &page_ids {
        let page_dict = doc.get_dictionary(page_id)?;
        for key in INHERITABLE_PAGE_KEYS {
            if !page_dict.has(key)
                && let Some(value) = resolve_inherited_attribute(doc, page_id, key)
            {
                updates.push((page_id, key, value));
            }
        }
    }
    for (page_id, key, value) in updates {
        doc.get_dictionary_mut(page_id)?.set(key, value);
    }
    Ok(())
}

/// Resolves `key` on the dict at `start_id` or any ancestor reachable through
/// `/Parent`, returning a clone of the first value found.
fn resolve_inherited_attribute(doc: &Document, start_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut current = start_id;
    for _ in 0..PARENT_CHAIN_LIMIT {
        let dict = doc.get_dictionary(current).ok()?;
        if let Ok(value) = dict.get(key) {
            return Some(value.clone());
        }
        current = dict.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

/// Reads an integer object, following one level of indirection if needed.
fn object_to_i64(doc: &Document, object: &Object) -> Option<i64> {
    match object {
        Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_i64().ok()),
        other => other.as_i64().ok(),
    }
}

/// Points the root `Pages` node's Kids at exactly `page_ids` (in order), fixes
/// each page's `/Parent`, and updates `Count`. Call
/// [`materialize_inherited_attributes`] first if the document may have nested
/// page trees. Orphaned pages/intermediate nodes are pruned at save time.
fn rebuild_page_tree(doc: &mut Document, page_ids: &[ObjectId]) -> Result<()> {
    let root_pages_id = doc
        .catalog()?
        .get(b"Pages")
        .and_then(Object::as_reference)?;
    for &page_id in page_ids {
        doc.get_dictionary_mut(page_id)?
            .set("Parent", Object::Reference(root_pages_id));
    }
    let kids: Vec<Object> = page_ids.iter().map(|&id| Object::Reference(id)).collect();
    let pages_dict = doc.get_dictionary_mut(root_pages_id)?;
    pages_dict.set("Kids", kids);
    pages_dict.set("Count", page_ids.len() as i64);
    Ok(())
}

/// Prunes unreachable objects, compresses streams, and serializes the doc.
fn save_doc(mut doc: Document) -> Result<Vec<u8>> {
    doc.prune_objects();
    doc.compress();
    let mut buffer = Vec::new();
    doc.save_to(&mut buffer)?;
    Ok(buffer)
}

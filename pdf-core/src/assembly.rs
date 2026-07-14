//! Zone 1 — document assembly: merge, split, page extraction/reordering/
//! rotation/deletion. Pure lopdf; no pdfium dependency.
//!
//! Page numbers in this module are 1-based, matching user-facing semantics.

use std::collections::{BTreeMap, BTreeSet};

use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

use crate::error::{PdfCoreError, Result};

/// Page-tree attributes that pages may inherit from ancestor `Pages` nodes
/// (PDF 32000-1 §7.7.3.4). They must be materialized onto each page dict
/// before re-parenting pages to a flat root, or pages lose fonts/size.
const INHERITABLE_PAGE_KEYS: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Upper bound on `/Parent` chain walks, guarding against reference cycles.
const PARENT_CHAIN_LIMIT: usize = 64;

/// Merge input trees may nest at most this many indirect levels. This stays
/// well below lopdf's internal page-tree limit so malformed input cannot be
/// silently truncated before merge validates it.
const MAX_MERGE_TREE_DEPTH: usize = 64;
/// Aggregate nodes visited by recursive merge walks across every input.
const MAX_MERGE_TREE_NODES: usize = 1_000_000;
/// Aggregate bytes retained from destination names, outline titles, label
/// prefixes, and field names.
const MAX_MERGE_NAME_BYTES: usize = 4 * 1024 * 1024;
/// Aggregate named destinations accepted by one merge.
const MAX_MERGE_DESTINATIONS: usize = 50_000;
/// Aggregate outline items accepted by one merge.
const MAX_MERGE_OUTLINES: usize = 50_000;
/// Aggregate page-label entries accepted by one merge.
const MAX_MERGE_PAGE_LABELS: usize = 10_000;
/// Aggregate page annotations accepted by one merge.
const MAX_MERGE_ANNOTATIONS: usize = 100_000;
/// Aggregate AcroForm field/widget nodes accepted by one merge.
const MAX_MERGE_FIELDS: usize = 100_000;
/// Source plus generated indirect objects retained while constructing output.
const MAX_MERGE_OBJECTS: usize = 5_000_000;

#[derive(Default)]
struct MergeBudget {
    tree_nodes: usize,
    name_bytes: usize,
    destinations: usize,
    outlines: usize,
    page_labels: usize,
    annotations: usize,
    fields: usize,
    objects: usize,
}

impl MergeBudget {
    fn tree_node(&mut self, depth: usize, structure: &str) -> Result<()> {
        if depth > MAX_MERGE_TREE_DEPTH {
            return Err(resource_limit(format!(
                "merge {structure} depth exceeds limit {MAX_MERGE_TREE_DEPTH}"
            )));
        }
        charge_limit(
            &mut self.tree_nodes,
            1,
            MAX_MERGE_TREE_NODES,
            "recursive node count",
        )
    }

    fn name_bytes(&mut self, bytes: usize) -> Result<()> {
        charge_limit(
            &mut self.name_bytes,
            bytes,
            MAX_MERGE_NAME_BYTES,
            "collected name bytes",
        )
    }

    fn destination(&mut self) -> Result<()> {
        charge_limit(
            &mut self.destinations,
            1,
            MAX_MERGE_DESTINATIONS,
            "destination count",
        )
    }

    fn outline(&mut self) -> Result<()> {
        charge_limit(&mut self.outlines, 1, MAX_MERGE_OUTLINES, "outline count")
    }

    fn page_label(&mut self) -> Result<()> {
        charge_limit(
            &mut self.page_labels,
            1,
            MAX_MERGE_PAGE_LABELS,
            "page-label count",
        )
    }

    fn annotation(&mut self) -> Result<()> {
        charge_limit(
            &mut self.annotations,
            1,
            MAX_MERGE_ANNOTATIONS,
            "annotation count",
        )
    }

    fn field(&mut self) -> Result<()> {
        charge_limit(&mut self.fields, 1, MAX_MERGE_FIELDS, "form-field count")
    }

    fn objects(&mut self, count: usize) -> Result<()> {
        charge_limit(
            &mut self.objects,
            count,
            MAX_MERGE_OBJECTS,
            "generated object count",
        )
    }
}

fn charge_limit(counter: &mut usize, amount: usize, limit: usize, what: &str) -> Result<()> {
    *counter = counter
        .checked_add(amount)
        .ok_or_else(|| resource_limit(format!("merge {what} overflow")))?;
    if *counter > limit {
        return Err(resource_limit(format!(
            "merge {what} exceeds limit {limit}"
        )));
    }
    Ok(())
}

fn resource_limit(message: String) -> PdfCoreError {
    PdfCoreError::ResourceLimit(message)
}

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
    let mut navigation = NavigationMerger::default();
    let mut page_offset = 0u32;
    let mut budget = MergeBudget::default();

    for (index, bytes) in pdfs.iter().enumerate() {
        let mut doc = load_doc(bytes)?;
        let source_pages = validate_page_tree(&doc, &mut budget)?;
        budget.objects(doc.objects.len())?;
        // Pages get re-parented to a single flat Pages root below, so any
        // attributes inherited from nested page trees must be copied first.
        materialize_inherited_attributes_with_budget(&mut doc, &mut budget)?;
        let object_count = u32::try_from(doc.objects.len())
            .map_err(|_| resource_limit("merge object count exceeds object-id range".into()))?;
        max_id
            .checked_add(object_count)
            .ok_or_else(|| resource_limit("merge object ids exceed supported range".into()))?;
        doc.renumber_objects_with(max_id);
        let source_page_count = u32::try_from(source_pages.len()).map_err(|_| {
            PdfCoreError::ResourceLimit("merge page count exceeds supported range".into())
        })?;
        if index == 0 {
            // Carry the FIRST document's /Info into the merged trailer
            // (references are already renumbered at this point).
            info = document_info(&doc);
        }
        acroform.absorb(&mut doc, &mut budget)?;
        navigation.absorb(&mut doc, page_offset, source_page_count, &mut budget)?;
        for (_, page_id) in doc.get_pages() {
            let page_object = doc.get_object(page_id)?.to_owned();
            merged_pages.push((page_id, page_object));
        }
        all_objects.extend(doc.objects);
        max_id = doc
            .max_id
            .checked_add(1)
            .ok_or_else(|| resource_limit("merge object ids exceed supported range".into()))?;
        page_offset = page_offset.checked_add(source_page_count).ok_or_else(|| {
            PdfCoreError::ResourceLimit("merge page count exceeds supported range".into())
        })?;
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
            b"Outlines" => {} // Replaced by one merged root below.
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
        catalog.remove(b"Names");
        catalog.remove(b"Dests");
        catalog.remove(b"PageLabels");
        // The kept catalog's own AcroForm (if any) is superseded by the
        // merged one built below from ALL inputs.
        catalog.remove(b"AcroForm");
    }
    document.max_id = max_id;
    if let Some(acroform_dict) = acroform.into_dictionary() {
        let acroform_id = add_merge_object(
            &mut document,
            Object::Dictionary(acroform_dict),
            &mut budget,
        )?;
        if let Some(Object::Dictionary(catalog)) = document.objects.get_mut(&catalog_id) {
            catalog.set("AcroForm", Object::Reference(acroform_id));
        }
    }
    navigation.install(&mut document, catalog_id, &mut budget)?;
    document.trailer.set("Root", catalog_id);
    if let Some(info) = info {
        document.trailer.set("Info", info);
    }
    document.prune_objects();
    document.renumber_objects();

    let expected_pages = merged_pages.len();
    let output = save_doc(document)?;
    validate_output_pdf(&output, expected_pages, "merge")?;
    Ok(output)
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
        let mut budget = MergeBudget::default();
        let existing = resolve_inherited_attribute(&doc, page_id, b"Rotate", &mut budget)?
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

/// Navigation from each source is normalized into one merged catalog. Outline
/// top-level lists are concatenated in source order; no synthetic titled nodes
/// are introduced.
#[derive(Default)]
struct NavigationMerger {
    outline_tops: Vec<ObjectId>,
    destinations: Vec<(Vec<u8>, Object)>,
    used_destination_names: BTreeSet<Vec<u8>>,
    names_base: Option<Dictionary>,
    page_labels: Vec<(u32, Object)>,
    label_state_custom: bool,
}

impl NavigationMerger {
    fn absorb(
        &mut self,
        doc: &mut Document,
        page_offset: u32,
        page_count: u32,
        budget: &mut MergeBudget,
    ) -> Result<()> {
        let catalog = doc.catalog()?.clone();
        if self.names_base.is_none()
            && let Ok(names) = catalog.get(b"Names")
            && let Some(mut dict) = resolve_dict(doc, names)
        {
            dict.remove(b"Dests");
            self.names_base = Some(dict);
        }

        let mut source_destinations = Vec::new();
        if let Ok(names) = catalog.get(b"Names")
            && let Some(names_dict) = resolve_dict(doc, names)
            && let Ok(dests) = names_dict.get(b"Dests")
        {
            collect_name_tree(
                doc,
                dests,
                &mut source_destinations,
                &mut BTreeSet::new(),
                0,
                budget,
            )?;
        }
        if let Ok(dests) = catalog.get(b"Dests") {
            let legacy = resolve_dict(doc, dests).ok_or_else(|| {
                invalid_navigation("named destinations", "legacy /Dests is not a dictionary")
            })?;
            for (name, destination) in legacy.iter() {
                budget.destination()?;
                budget.name_bytes(name.len())?;
                source_destinations.push((name.clone(), destination.clone()));
            }
        }

        let mut source_names = BTreeSet::new();
        let mut renames = BTreeMap::new();
        for (name, _) in &source_destinations {
            if !source_names.insert(name.clone()) {
                return Err(invalid_navigation(
                    "named destinations",
                    "duplicate destination name within source",
                ));
            }
            let merged_name = self.unique_destination_name(name)?;
            renames.insert(name.clone(), merged_name);
        }
        for object in doc.objects.values_mut() {
            rewrite_named_navigation(object, &renames, 0, budget)?;
        }
        for (name, destination) in source_destinations {
            let merged_name = renames.get(&name).cloned().ok_or_else(|| {
                invalid_navigation("named destinations", "internal rename lookup failed")
            })?;
            self.destinations.push((merged_name, destination));
        }

        let page_ids: BTreeSet<ObjectId> = doc.get_pages().into_values().collect();
        let valid_names: BTreeSet<Vec<u8>> = renames.into_values().collect();
        for (_, destination) in self.destinations.iter().rev().take(source_names.len()) {
            validate_destination(
                doc,
                destination,
                &page_ids,
                &valid_names,
                "named destinations",
            )?;
        }
        for page_id in &page_ids {
            let page = doc.get_dictionary(*page_id)?;
            let Ok(annots) = page.get(b"Annots") else {
                continue;
            };
            let annots = resolve_array(doc, annots)
                .ok_or_else(|| invalid_navigation("link annotations", "/Annots is not an array"))?;
            for annotation in annots {
                budget.annotation()?;
                let annotation = resolve_dict(doc, &annotation).ok_or_else(|| {
                    invalid_navigation("link annotations", "annotation is dangling")
                })?;
                if annotation.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Link") {
                    validate_navigation_dictionary(
                        doc,
                        &annotation,
                        &page_ids,
                        &valid_names,
                        "link annotations",
                    )?;
                }
            }
        }

        if let Ok(outlines) = catalog.get(b"Outlines") {
            let (root_id, root) = resolve_indirect_dict(doc, outlines).ok_or_else(|| {
                invalid_navigation("outlines", "/Outlines does not resolve to a dictionary")
            })?;
            let first = optional_reference(&root, b"First", "outlines")?;
            let last = optional_reference(&root, b"Last", "outlines")?;
            if first.is_some() != last.is_some() {
                return Err(invalid_navigation(
                    "outlines",
                    "/First and /Last must either both exist or both be absent",
                ));
            }
            if let (Some(first), Some(last)) = (first, last) {
                let tops = OutlineValidator {
                    doc,
                    page_ids: &page_ids,
                    valid_names: &valid_names,
                    seen: BTreeSet::new(),
                    budget,
                }
                .validate_level(root_id, first, last, 1)?;
                self.outline_tops.extend(tops);
            }
        }

        let mut source_labels = Vec::new();
        if let Ok(labels) = catalog.get(b"PageLabels") {
            collect_number_tree(
                doc,
                labels,
                &mut source_labels,
                &mut BTreeSet::new(),
                0,
                budget,
            )?;
            source_labels.sort_by_key(|(index, _)| *index);
            for window in source_labels.windows(2) {
                if window[0].0 == window[1].0 {
                    return Err(invalid_navigation(
                        "page labels",
                        "duplicate /Nums page index",
                    ));
                }
            }
            if source_labels.iter().any(|(index, _)| *index >= page_count) {
                return Err(invalid_navigation(
                    "page labels",
                    "/Nums page index is outside source page range",
                ));
            }
        }
        if page_offset > 0
            && !source_labels.iter().any(|(index, _)| *index == 0)
            && self.label_state_custom
        {
            self.page_labels
                .push((page_offset, Object::Dictionary(Dictionary::new())));
            self.label_state_custom = false;
        }
        for (index, label) in source_labels {
            self.label_state_custom = !page_label_is_default(doc, &label);
            self.page_labels.push((
                page_offset.checked_add(index).ok_or_else(|| {
                    invalid_navigation("page labels", "merged page index overflow")
                })?,
                label,
            ));
        }
        Ok(())
    }

    fn unique_destination_name(&mut self, original: &[u8]) -> Result<Vec<u8>> {
        if self.used_destination_names.insert(original.to_vec()) {
            return Ok(original.to_vec());
        }
        for suffix in 2..=MAX_MERGE_DESTINATIONS + 1 {
            let mut candidate = original.to_vec();
            candidate.extend_from_slice(format!("~{suffix}").as_bytes());
            if self.used_destination_names.insert(candidate.clone()) {
                return Ok(candidate);
            }
        }
        Err(resource_limit(
            "merge destination collision suffix space exhausted".into(),
        ))
    }

    fn install(
        self,
        doc: &mut Document,
        catalog_id: ObjectId,
        budget: &mut MergeBudget,
    ) -> Result<()> {
        if !self.outline_tops.is_empty() {
            let root_id = add_merge_object_id(doc, budget)?;
            let mut visible_count = 0i64;
            for (index, item_id) in self.outline_tops.iter().copied().enumerate() {
                let item = doc.get_dictionary_mut(item_id).map_err(|_| {
                    invalid_navigation("outlines", "top-level item is not loadable")
                })?;
                let open_descendants = item
                    .get(b"Count")
                    .ok()
                    .and_then(|value| value.as_i64().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(0);
                visible_count = visible_count
                    .checked_add(1)
                    .and_then(|count| count.checked_add(open_descendants))
                    .ok_or_else(|| invalid_navigation("outlines", "visible item count overflow"))?;
                item.set("Parent", Object::Reference(root_id));
                if index == 0 {
                    item.remove(b"Prev");
                } else {
                    item.set("Prev", Object::Reference(self.outline_tops[index - 1]));
                }
                if index + 1 == self.outline_tops.len() {
                    item.remove(b"Next");
                } else {
                    item.set("Next", Object::Reference(self.outline_tops[index + 1]));
                }
            }
            doc.objects.insert(
                root_id,
                Object::Dictionary(dictionary! {
                    "Type" => "Outlines",
                    "First" => self.outline_tops[0],
                    "Last" => self.outline_tops[self.outline_tops.len() - 1],
                    "Count" => visible_count,
                }),
            );
            doc.get_dictionary_mut(catalog_id)?
                .set("Outlines", Object::Reference(root_id));
        }

        if !self.destinations.is_empty() {
            let mut destinations = self.destinations;
            destinations.sort_by(|left, right| left.0.cmp(&right.0));
            let mut entries = Vec::with_capacity(destinations.len() * 2);
            for (name, destination) in destinations {
                entries.push(Object::String(name, lopdf::StringFormat::Literal));
                entries.push(destination);
            }
            let dests_id =
                add_merge_object(doc, dictionary! { "Names" => entries }.into(), budget)?;
            let mut names = self.names_base.unwrap_or_default();
            names.set("Dests", Object::Reference(dests_id));
            let names_id = add_merge_object(doc, names.into(), budget)?;
            doc.get_dictionary_mut(catalog_id)?
                .set("Names", Object::Reference(names_id));
        } else if let Some(names) = self.names_base
            && !names.is_empty()
        {
            let names_id = add_merge_object(doc, names.into(), budget)?;
            doc.get_dictionary_mut(catalog_id)?
                .set("Names", Object::Reference(names_id));
        }

        if !self.page_labels.is_empty() {
            let mut labels = self.page_labels;
            labels.sort_by_key(|(index, _)| *index);
            let mut nums = Vec::with_capacity(labels.len() * 2);
            for (index, label) in labels {
                nums.push(Object::Integer(i64::from(index)));
                nums.push(label);
            }
            let labels_id = add_merge_object(doc, dictionary! { "Nums" => nums }.into(), budget)?;
            doc.get_dictionary_mut(catalog_id)?
                .set("PageLabels", Object::Reference(labels_id));
        }
        Ok(())
    }
}

fn invalid_navigation(structure: &str, detail: &str) -> PdfCoreError {
    PdfCoreError::InvalidInput(format!("malformed {structure}: {detail}"))
}

fn collect_name_tree(
    doc: &Document,
    object: &Object,
    output: &mut Vec<(Vec<u8>, Object)>,
    visited: &mut BTreeSet<ObjectId>,
    depth: usize,
    budget: &mut MergeBudget,
) -> Result<()> {
    budget.tree_node(depth, "name tree")?;
    let (id, dict) = resolve_tree_dict(doc, object).ok_or_else(|| {
        invalid_navigation("named destinations", "name-tree node is not a dictionary")
    })?;
    if id.is_some_and(|id| !visited.insert(id)) {
        return Err(invalid_navigation(
            "named destinations",
            "cycle in name tree",
        ));
    }
    if let Ok(names) = dict.get(b"Names") {
        let entries = resolve_array(doc, names)
            .ok_or_else(|| invalid_navigation("named destinations", "/Names is not an array"))?;
        if entries.len() % 2 != 0 {
            return Err(invalid_navigation(
                "named destinations",
                "/Names array has odd length",
            ));
        }
        for pair in entries.chunks_exact(2) {
            let name = named_token(&pair[0]).ok_or_else(|| {
                invalid_navigation("named destinations", "name-tree key is not a string")
            })?;
            budget.destination()?;
            budget.name_bytes(name.len())?;
            output.push((name.to_vec(), pair[1].clone()));
        }
    }
    if let Ok(kids) = dict.get(b"Kids") {
        let kids = resolve_array(doc, kids)
            .ok_or_else(|| invalid_navigation("named destinations", "/Kids is not an array"))?;
        for kid in kids {
            collect_name_tree(doc, &kid, output, visited, depth + 1, budget)?;
        }
    }
    Ok(())
}

fn collect_number_tree(
    doc: &Document,
    object: &Object,
    output: &mut Vec<(u32, Object)>,
    visited: &mut BTreeSet<ObjectId>,
    depth: usize,
    budget: &mut MergeBudget,
) -> Result<()> {
    budget.tree_node(depth, "number tree")?;
    let (id, dict) = resolve_tree_dict(doc, object)
        .ok_or_else(|| invalid_navigation("page labels", "number-tree node is not a dictionary"))?;
    if id.is_some_and(|id| !visited.insert(id)) {
        return Err(invalid_navigation("page labels", "cycle in number tree"));
    }
    if let Ok(nums) = dict.get(b"Nums") {
        let entries = resolve_array(doc, nums)
            .ok_or_else(|| invalid_navigation("page labels", "/Nums is not an array"))?;
        if entries.len() % 2 != 0 {
            return Err(invalid_navigation(
                "page labels",
                "/Nums array has odd length",
            ));
        }
        for pair in entries.chunks_exact(2) {
            budget.page_label()?;
            let index = pair[0]
                .as_i64()
                .ok()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    invalid_navigation("page labels", "/Nums key is not a non-negative integer")
                })?;
            let label = doc
                .dereference(&pair[1])
                .map_err(|_| invalid_navigation("page labels", "label dictionary is dangling"))?;
            if !matches!(label.1, Object::Dictionary(_)) {
                return Err(invalid_navigation(
                    "page labels",
                    "label value is not a dictionary",
                ));
            }
            if let Object::Dictionary(label) = label.1
                && let Ok(prefix) = label.get(b"P")
            {
                let prefix = doc
                    .dereference(prefix)
                    .map_err(|_| invalid_navigation("page labels", "label /P is dangling"))?
                    .1
                    .as_str()
                    .map_err(|_| invalid_navigation("page labels", "label /P is not a string"))?;
                budget.name_bytes(prefix.len())?;
            }
            output.push((index, pair[1].clone()));
        }
    }
    if let Ok(kids) = dict.get(b"Kids") {
        let kids = resolve_array(doc, kids)
            .ok_or_else(|| invalid_navigation("page labels", "/Kids is not an array"))?;
        for kid in kids {
            collect_number_tree(doc, &kid, output, visited, depth + 1, budget)?;
        }
    }
    Ok(())
}

fn page_label_is_default(doc: &Document, label: &Object) -> bool {
    let Some(dict) = resolve_dict(doc, label) else {
        return false;
    };
    let style_is_decimal = match dict.get(b"S") {
        Ok(style) => style.as_name().ok() == Some(b"D"),
        Err(_) => true,
    };
    let starts_at_one = match dict.get(b"St") {
        Ok(start) => start.as_i64().ok() == Some(1),
        Err(_) => true,
    };
    style_is_decimal && starts_at_one && !dict.has(b"P")
}

fn resolve_indirect_dict(doc: &Document, object: &Object) -> Option<(ObjectId, Dictionary)> {
    let id = object.as_reference().ok()?;
    match doc.dereference(object) {
        Ok((_, Object::Dictionary(dict))) => Some((id, dict.clone())),
        _ => None,
    }
}

fn resolve_tree_dict(doc: &Document, object: &Object) -> Option<(Option<ObjectId>, Dictionary)> {
    match object {
        Object::Dictionary(dict) => Some((None, dict.clone())),
        Object::Reference(id) => match doc.dereference(object) {
            Ok((_, Object::Dictionary(dict))) => Some((Some(*id), dict.clone())),
            _ => None,
        },
        _ => None,
    }
}

fn named_token(object: &Object) -> Option<&[u8]> {
    match object {
        Object::String(bytes, _) | Object::Name(bytes) => Some(bytes),
        _ => None,
    }
}

fn rewrite_named_token(object: &mut Object, renames: &BTreeMap<Vec<u8>, Vec<u8>>) {
    let Some(name) = named_token(object) else {
        return;
    };
    let Some(replacement) = renames.get(name) else {
        return;
    };
    match object {
        Object::String(bytes, _) | Object::Name(bytes) => *bytes = replacement.clone(),
        _ => {}
    }
}

fn rewrite_named_navigation(
    object: &mut Object,
    renames: &BTreeMap<Vec<u8>, Vec<u8>>,
    depth: usize,
    budget: &mut MergeBudget,
) -> Result<()> {
    budget.tree_node(depth, "direct object graph")?;
    match object {
        Object::Dictionary(dict) => {
            if let Ok(destination) = dict.get_mut(b"Dest") {
                rewrite_named_token(destination, renames);
            }
            let is_goto =
                dict.get(b"S").ok().and_then(|value| value.as_name().ok()) == Some(b"GoTo");
            if is_goto && let Ok(destination) = dict.get_mut(b"D") {
                rewrite_named_token(destination, renames);
            }
            for (_, value) in dict.iter_mut() {
                rewrite_named_navigation(value, renames, depth + 1, budget)?;
            }
        }
        Object::Array(array) => {
            for value in array {
                rewrite_named_navigation(value, renames, depth + 1, budget)?;
            }
        }
        Object::Stream(stream) => {
            for (_, value) in stream.dict.iter_mut() {
                rewrite_named_navigation(value, renames, depth + 1, budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn optional_reference(dict: &Dictionary, key: &[u8], structure: &str) -> Result<Option<ObjectId>> {
    match dict.get(key) {
        Ok(object) => object.as_reference().map(Some).map_err(|_| {
            invalid_navigation(
                structure,
                &format!("/{} is not a reference", String::from_utf8_lossy(key)),
            )
        }),
        Err(_) => Ok(None),
    }
}

struct OutlineValidator<'a> {
    doc: &'a Document,
    page_ids: &'a BTreeSet<ObjectId>,
    valid_names: &'a BTreeSet<Vec<u8>>,
    seen: BTreeSet<ObjectId>,
    budget: &'a mut MergeBudget,
}

impl OutlineValidator<'_> {
    fn validate_level(
        &mut self,
        parent_id: ObjectId,
        first_id: ObjectId,
        last_id: ObjectId,
        depth: usize,
    ) -> Result<Vec<ObjectId>> {
        let mut siblings = Vec::new();
        let mut current = Some(first_id);
        let mut previous = None;
        while let Some(item_id) = current {
            self.budget.tree_node(depth, "outline tree")?;
            self.budget.outline()?;
            if !self.seen.insert(item_id) {
                return Err(invalid_navigation(
                    "outlines",
                    "cycle or reused outline item",
                ));
            }
            let item = self
                .doc
                .get_dictionary(item_id)
                .map_err(|_| {
                    invalid_navigation("outlines", "outline item is dangling or not a dictionary")
                })?
                .clone();
            let title = self
                .doc
                .dereference(
                    item.get(b"Title").map_err(|_| {
                        invalid_navigation("outlines", "outline item has no /Title")
                    })?,
                )
                .map_err(|_| invalid_navigation("outlines", "outline /Title is dangling"))?
                .1
                .as_str()
                .map_err(|_| invalid_navigation("outlines", "outline /Title is not a string"))?;
            self.budget.name_bytes(title.len())?;
            if item.get(b"Parent").and_then(Object::as_reference).ok() != Some(parent_id) {
                return Err(invalid_navigation(
                    "outlines",
                    "outline item has invalid /Parent",
                ));
            }
            let actual_previous = optional_reference(&item, b"Prev", "outlines")?;
            if actual_previous != previous {
                return Err(invalid_navigation(
                    "outlines",
                    "outline sibling has invalid /Prev",
                ));
            }
            if let Ok(count) = item.get(b"Count")
                && count.as_i64().is_err()
            {
                return Err(invalid_navigation(
                    "outlines",
                    "outline /Count is not an integer",
                ));
            }
            validate_navigation_dictionary(
                self.doc,
                &item,
                self.page_ids,
                self.valid_names,
                "outlines",
            )?;

            let child_first = optional_reference(&item, b"First", "outlines")?;
            let child_last = optional_reference(&item, b"Last", "outlines")?;
            if child_first.is_some() != child_last.is_some() {
                return Err(invalid_navigation(
                    "outlines",
                    "child /First and /Last must both exist",
                ));
            }
            if let (Some(child_first), Some(child_last)) = (child_first, child_last) {
                self.validate_level(item_id, child_first, child_last, depth + 1)?;
            }

            siblings.push(item_id);
            if item_id == last_id {
                if optional_reference(&item, b"Next", "outlines")?.is_some() {
                    return Err(invalid_navigation(
                        "outlines",
                        "last outline item has /Next",
                    ));
                }
                current = None;
            } else {
                current = optional_reference(&item, b"Next", "outlines")?;
                if current.is_none() {
                    return Err(invalid_navigation(
                        "outlines",
                        "/Last is not reachable from /First",
                    ));
                }
            }
            previous = Some(item_id);
        }
        Ok(siblings)
    }
}

fn validate_navigation_dictionary(
    doc: &Document,
    dict: &Dictionary,
    page_ids: &BTreeSet<ObjectId>,
    valid_names: &BTreeSet<Vec<u8>>,
    structure: &str,
) -> Result<()> {
    if let Ok(destination) = dict.get(b"Dest") {
        validate_destination(doc, destination, page_ids, valid_names, structure)?;
    }
    if let Ok(action) = dict.get(b"A") {
        let action = resolve_dict(doc, action)
            .ok_or_else(|| invalid_navigation(structure, "/A is not a dictionary"))?;
        let action_type = action
            .get(b"S")
            .and_then(Object::as_name)
            .map_err(|_| invalid_navigation(structure, "action has no valid /S"))?;
        if action_type == b"GoTo" {
            let destination = action
                .get(b"D")
                .map_err(|_| invalid_navigation(structure, "GoTo action has no /D"))?;
            validate_destination(doc, destination, page_ids, valid_names, structure)?;
        }
    }
    Ok(())
}

fn validate_destination(
    doc: &Document,
    destination: &Object,
    page_ids: &BTreeSet<ObjectId>,
    valid_names: &BTreeSet<Vec<u8>>,
    structure: &str,
) -> Result<()> {
    if let Some(name) = named_token(destination) {
        return valid_names
            .contains(name)
            .then_some(())
            .ok_or_else(|| invalid_navigation(structure, "dangling named destination"));
    }
    let resolved = doc
        .dereference(destination)
        .map_err(|_| invalid_navigation(structure, "dangling destination object"))?
        .1;
    let array = match resolved {
        Object::Array(array) => array,
        Object::Dictionary(dict) => dict
            .get(b"D")
            .and_then(Object::as_array)
            .map_err(|_| invalid_navigation(structure, "destination dictionary has no array /D"))?,
        _ => {
            return Err(invalid_navigation(
                structure,
                "destination is neither a name, array, nor dictionary",
            ));
        }
    };
    let page_id = array
        .first()
        .and_then(|object| object.as_reference().ok())
        .ok_or_else(|| invalid_navigation(structure, "destination has no page reference"))?;
    if !page_ids.contains(&page_id) {
        return Err(invalid_navigation(
            structure,
            "destination page is dangling",
        ));
    }
    Ok(())
}

/// AcroForm keys handled structurally by [`AcroFormMerger`]; all other keys
/// are taken from the first input that has an AcroForm (first-wins).
const ACROFORM_MERGED_KEYS: [&[u8]; 3] = [b"Fields", b"DR", b"NeedAppearances"];
const ACROFORM_RESOURCE_CATEGORIES: [&[u8]; 7] = [
    b"Font",
    b"XObject",
    b"ExtGState",
    b"Pattern",
    b"Shading",
    b"ColorSpace",
    b"Properties",
];

type ResourceRenames = BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, Vec<u8>>>;

/// Accumulates a single merged `/AcroForm` across the input documents:
/// `/Fields` arrays are unioned in input order, `/DR` resource dictionaries
/// share identical indirect resources and deterministically rename conflicting
/// resources, `/NeedAppearances` is set if ANY input sets it, and every other
/// key comes from the first document that carries an AcroForm.
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
    fn absorb(&mut self, doc: &mut Document, budget: &mut MergeBudget) -> Result<()> {
        let catalog = doc.catalog()?;
        let Ok(acroform_object) = catalog.get(b"AcroForm") else {
            return Ok(());
        };
        let mut acroform = resolve_dict(doc, acroform_object)
            .ok_or_else(|| invalid_navigation("AcroForm", "/AcroForm is not a dictionary"))?;

        let fields = match acroform.get(b"Fields") {
            Ok(fields) => resolve_array(doc, fields)
                .ok_or_else(|| invalid_navigation("AcroForm", "/Fields is not an array"))?,
            Err(_) => Vec::new(),
        };
        let (field_ids, has_direct_fields) = {
            let mut validator = FieldValidator {
                doc,
                seen: BTreeSet::new(),
                field_ids: Vec::new(),
                has_direct_fields: false,
                budget,
            };
            for field in &fields {
                validator.validate(field, None, 1)?;
            }
            (validator.field_ids, validator.has_direct_fields)
        };

        let (source_resources, renames) = match acroform.get(b"DR") {
            Ok(dr) => {
                let dr = resolve_dict(doc, dr).ok_or_else(|| {
                    invalid_navigation("AcroForm", "/DR is not a resource dictionary")
                })?;
                merge_resource_dicts(&mut self.resources, &dr, doc, budget)?
            }
            Err(_) => (Dictionary::new(), ResourceRenames::new()),
        };
        if !renames.is_empty() {
            if has_direct_fields {
                return Err(PdfCoreError::Unsupported(
                    "AcroForm resource collision with direct field dictionaries".into(),
                ));
            }
            for field_id in &field_ids {
                if field_has_javascript(doc, *field_id, budget)? {
                    return Err(PdfCoreError::Unsupported(
                        "AcroForm resource collision with field JavaScript".into(),
                    ));
                }
            }
            let source_da = acroform.get(b"DA").ok().cloned();
            materialize_source_default_appearance(doc, &fields, source_da.as_ref())?;
            rewrite_field_default_appearances(doc, &field_ids, &renames)?;
            rewrite_field_appearances(doc, &field_ids, &source_resources, &renames, budget)?;
            if let Some(source_da) = source_da {
                acroform.set(
                    "DA",
                    rewrite_default_appearance(doc, &source_da, font_renames(&renames))?,
                );
            }
        }
        self.fields.extend(fields);
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
        Ok(())
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

struct FieldValidator<'a> {
    doc: &'a Document,
    seen: BTreeSet<ObjectId>,
    field_ids: Vec<ObjectId>,
    has_direct_fields: bool,
    budget: &'a mut MergeBudget,
}

impl FieldValidator<'_> {
    fn validate(
        &mut self,
        object: &Object,
        expected_parent: Option<ObjectId>,
        depth: usize,
    ) -> Result<()> {
        self.budget.tree_node(depth, "field tree")?;
        self.budget.field()?;
        let (id, field) = resolve_tree_dict(self.doc, object).ok_or_else(|| {
            invalid_navigation("AcroForm", "field is dangling or not a dictionary")
        })?;
        if id.is_some_and(|id| !self.seen.insert(id)) {
            return Err(invalid_navigation(
                "AcroForm",
                "cycle or reused field/widget",
            ));
        }
        if let Some(id) = id {
            self.field_ids.push(id);
        } else {
            self.has_direct_fields = true;
        }
        match expected_parent {
            Some(parent_id)
                if field.get(b"Parent").and_then(Object::as_reference).ok() != Some(parent_id) =>
            {
                return Err(invalid_navigation(
                    "AcroForm",
                    "field child has invalid /Parent",
                ));
            }
            None if field.has(b"Parent") => {
                return Err(invalid_navigation(
                    "AcroForm",
                    "top-level field unexpectedly has /Parent",
                ));
            }
            _ => {}
        }
        if let Ok(name) = field.get(b"T") {
            let name = self
                .doc
                .dereference(name)
                .map_err(|_| invalid_navigation("AcroForm", "field /T is dangling"))?
                .1
                .as_str()
                .map_err(|_| invalid_navigation("AcroForm", "field /T is not a string"))?;
            self.budget.name_bytes(name.len())?;
        }
        if let Ok(kids) = field.get(b"Kids") {
            let parent_id = id.ok_or_else(|| {
                invalid_navigation("AcroForm", "direct field dictionary cannot parent /Kids")
            })?;
            let kids = resolve_array(self.doc, kids)
                .ok_or_else(|| invalid_navigation("AcroForm", "field /Kids is not an array"))?;
            for kid in kids {
                self.validate(&kid, Some(parent_id), depth + 1)?;
            }
        }
        Ok(())
    }
}

/// Materializes resource entries as indirect objects, shares identical object
/// identities, and deterministically renames conflicting per-category names.
fn merge_resource_dicts(
    merged: &mut Dictionary,
    incoming: &Dictionary,
    doc: &mut Document,
    budget: &mut MergeBudget,
) -> Result<(Dictionary, ResourceRenames)> {
    let mut normalized = Dictionary::new();
    let mut renames = ResourceRenames::new();
    for (category, value) in incoming.iter() {
        if category == b"ProcSet" {
            let incoming_entries = resolve_array(doc, value).ok_or_else(|| {
                invalid_navigation("AcroForm", "/ProcSet resource category is not an array")
            })?;
            let mut combined = match merged.get(category) {
                Ok(Object::Array(existing)) => existing.clone(),
                Ok(_) => {
                    return Err(invalid_navigation(
                        "AcroForm",
                        "merged /ProcSet resource category is not an array",
                    ));
                }
                Err(_) => Vec::new(),
            };
            for entry in &incoming_entries {
                entry
                    .as_name()
                    .map_err(|_| invalid_navigation("AcroForm", "/ProcSet entry is not a name"))?;
                if !combined.contains(entry) {
                    combined.push(entry.clone());
                }
            }
            normalized.set(category.clone(), Object::Array(incoming_entries));
            merged.set(category.clone(), Object::Array(combined));
            continue;
        }
        if !ACROFORM_RESOURCE_CATEGORIES.contains(&category.as_slice()) {
            // PDF 2.0 resource dictionaries define no other standard keys.
            // Preserve extension keys verbatim and deterministically first-win:
            // their value shapes and content-stream semantics are unknowable,
            // so generic entry merging or rewriting would be unsafe.
            normalized.set(category.clone(), value.clone());
            if !merged.has(category) {
                merged.set(category.clone(), value.clone());
            }
            continue;
        }
        let incoming_entries = resolve_dict(doc, value).ok_or_else(|| {
            invalid_navigation("AcroForm", "resource category is not a dictionary")
        })?;
        let mut combined = match merged.get(category) {
            Ok(Object::Dictionary(existing)) => existing.clone(),
            Ok(_) => {
                return Err(invalid_navigation(
                    "AcroForm",
                    "merged resource category is not a dictionary",
                ));
            }
            Err(_) => Dictionary::new(),
        };
        let mut normalized_entries = Dictionary::new();
        for (name, object) in incoming_entries.iter() {
            let object = materialize_resource(doc, object, budget)?;
            normalized_entries.set(name.clone(), object.clone());
            match combined.get(name) {
                Ok(existing) if same_resource_identity(existing, &object) => {}
                Ok(_) => {
                    let renamed = unique_resource_name(&combined, name)?;
                    combined.set(renamed.clone(), object);
                    renames
                        .entry(category.clone())
                        .or_default()
                        .insert(name.clone(), renamed);
                }
                Err(_) => combined.set(name.clone(), object),
            }
        }
        normalized.set(category.clone(), Object::Dictionary(normalized_entries));
        merged.set(category.clone(), Object::Dictionary(combined));
    }
    Ok((normalized, renames))
}

fn materialize_resource(
    doc: &mut Document,
    object: &Object,
    budget: &mut MergeBudget,
) -> Result<Object> {
    let (id, resolved) = doc
        .dereference(object)
        .map_err(|_| invalid_navigation("AcroForm", "resource entry is dangling"))?;
    if let Some(id) = id {
        return Ok(Object::Reference(id));
    }
    let id = add_merge_object(doc, resolved.clone(), budget)?;
    Ok(Object::Reference(id))
}

fn add_merge_object(
    doc: &mut Document,
    object: Object,
    budget: &mut MergeBudget,
) -> Result<ObjectId> {
    let id = add_merge_object_id(doc, budget)?;
    doc.objects.insert(id, object);
    Ok(id)
}

fn add_merge_object_id(doc: &mut Document, budget: &mut MergeBudget) -> Result<ObjectId> {
    doc.max_id
        .checked_add(1)
        .ok_or_else(|| resource_limit("merge object ids exceed supported range".into()))?;
    budget.objects(1)?;
    Ok(doc.new_object_id())
}

fn same_resource_identity(left: &Object, right: &Object) -> bool {
    matches!(
        (left.as_reference(), right.as_reference()),
        (Ok(left), Ok(right)) if left == right
    )
}

fn unique_resource_name(entries: &Dictionary, original: &[u8]) -> Result<Vec<u8>> {
    for suffix in 2..=MAX_MERGE_OBJECTS + 1 {
        let mut candidate = original.to_vec();
        candidate.extend_from_slice(format!("~{suffix}").as_bytes());
        if !entries.has(&candidate) {
            return Ok(candidate);
        }
    }
    Err(resource_limit(
        "merge resource collision suffix space exhausted".into(),
    ))
}

fn font_renames(renames: &ResourceRenames) -> Option<&BTreeMap<Vec<u8>, Vec<u8>>> {
    renames.get(b"Font".as_slice())
}

fn rewrite_default_appearance(
    doc: &Document,
    object: &Object,
    renames: Option<&BTreeMap<Vec<u8>, Vec<u8>>>,
) -> Result<Object> {
    let Some(renames) = renames else {
        return Ok(object.clone());
    };
    let resolved = doc
        .dereference(object)
        .map_err(|_| invalid_navigation("AcroForm", "/DA is dangling"))?
        .1;
    let (bytes, format) = match resolved {
        Object::String(bytes, format) => (bytes, *format),
        _ => {
            return Err(invalid_navigation("AcroForm", "/DA is not a string"));
        }
    };
    let mut content = Content::decode_strict(bytes)
        .map_err(|_| invalid_navigation("AcroForm", "/DA content is malformed"))?;
    for operation in &mut content.operations {
        if operation.operator != "Tf" {
            continue;
        }
        if operation.operands.len() != 2 {
            return Err(invalid_navigation(
                "AcroForm",
                "/DA Tf must have two operands",
            ));
        }
        let name = operation.operands[0]
            .as_name()
            .map_err(|_| invalid_navigation("AcroForm", "/DA Tf font is not a name"))?;
        if let Some(replacement) = renames.get(name) {
            operation.operands[0] = Object::Name(replacement.clone());
        }
    }
    Ok(Object::String(content.encode()?, format))
}

fn materialize_source_default_appearance(
    doc: &mut Document,
    fields: &[Object],
    source_da: Option<&Object>,
) -> Result<()> {
    let Some(source_da) = source_da else {
        return Ok(());
    };
    for field in fields {
        let field_id = field.as_reference().map_err(|_| {
            PdfCoreError::Unsupported(
                "AcroForm resource collision with direct field dictionaries".into(),
            )
        })?;
        let field = doc
            .get_dictionary_mut(field_id)
            .map_err(|_| invalid_navigation("AcroForm", "top-level field is dangling"))?;
        if !field.has(b"DA") {
            field.set("DA", source_da.clone());
        }
    }
    Ok(())
}

fn rewrite_field_default_appearances(
    doc: &mut Document,
    field_ids: &[ObjectId],
    renames: &ResourceRenames,
) -> Result<()> {
    for field_id in field_ids {
        let da = doc
            .get_dictionary(*field_id)
            .ok()
            .and_then(|field| field.get(b"DA").ok())
            .cloned();
        if let Some(da) = da {
            let rewritten = rewrite_default_appearance(doc, &da, font_renames(renames))?;
            doc.get_dictionary_mut(*field_id)?.set("DA", rewritten);
        }
    }
    Ok(())
}

fn field_has_javascript(
    doc: &Document,
    field_id: ObjectId,
    budget: &mut MergeBudget,
) -> Result<bool> {
    let field = doc
        .get_dictionary(field_id)
        .map_err(|_| invalid_navigation("AcroForm", "field is dangling"))?;
    let mut seen = BTreeSet::new();
    for key in [b"A".as_slice(), b"AA".as_slice()] {
        if let Ok(action) = field.get(key)
            && action_contains_javascript(doc, action, 1, &mut seen, budget)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn action_contains_javascript(
    doc: &Document,
    object: &Object,
    depth: usize,
    seen: &mut BTreeSet<ObjectId>,
    budget: &mut MergeBudget,
) -> Result<bool> {
    budget.tree_node(depth, "field action tree")?;
    let (id, resolved) = doc
        .dereference(object)
        .map_err(|_| invalid_navigation("AcroForm", "field action is dangling"))?;
    if id.is_some_and(|id| !seen.insert(id)) {
        return Err(invalid_navigation("AcroForm", "cycle in field action tree"));
    }
    match resolved {
        Object::Dictionary(dict) => {
            if dict.get(b"S").and_then(Object::as_name).ok() == Some(b"JavaScript") {
                return Ok(true);
            }
            if dict.has(b"S") {
                return match dict.get(b"Next") {
                    Ok(next) => action_contains_javascript(doc, next, depth + 1, seen, budget),
                    Err(_) => Ok(false),
                };
            }
            for (_, value) in dict.iter() {
                if action_contains_javascript(doc, value, depth + 1, seen, budget)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Object::Array(actions) => {
            for action in actions {
                if action_contains_javascript(doc, action, depth + 1, seen, budget)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Err(invalid_navigation(
            "AcroForm",
            "field action is not a dictionary or array",
        )),
    }
}

fn rewrite_field_appearances(
    doc: &mut Document,
    field_ids: &[ObjectId],
    source_resources: &Dictionary,
    renames: &ResourceRenames,
    budget: &mut MergeBudget,
) -> Result<()> {
    let mut rewriter = AppearanceRewriter {
        source_resources,
        renames,
        active: BTreeSet::new(),
        visited: BTreeSet::new(),
        budget,
    };
    for field_id in field_ids {
        let appearance = doc
            .get_dictionary(*field_id)
            .ok()
            .and_then(|field| field.get(b"AP").ok())
            .cloned();
        let Some(mut appearance) = appearance else {
            continue;
        };
        rewriter.rewrite(doc, &mut appearance, 1)?;
        doc.get_dictionary_mut(*field_id)?.set("AP", appearance);
    }
    Ok(())
}

struct AppearanceRewriter<'a> {
    source_resources: &'a Dictionary,
    renames: &'a ResourceRenames,
    active: BTreeSet<ObjectId>,
    visited: BTreeSet<ObjectId>,
    budget: &'a mut MergeBudget,
}

impl AppearanceRewriter<'_> {
    fn rewrite(&mut self, doc: &mut Document, object: &mut Object, depth: usize) -> Result<()> {
        self.budget.tree_node(depth, "appearance tree")?;
        match object {
            Object::Reference(id) => {
                let id = *id;
                if self.visited.contains(&id) {
                    return Ok(());
                }
                if !self.active.insert(id) {
                    return Err(invalid_navigation("AcroForm", "cycle in appearance tree"));
                }
                let mut resolved = doc
                    .objects
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| invalid_navigation("AcroForm", "appearance is dangling"))?;
                self.rewrite(doc, &mut resolved, depth + 1)?;
                doc.objects.insert(id, resolved);
                self.active.remove(&id);
                self.visited.insert(id);
            }
            Object::Dictionary(states) => {
                for (_, state) in states.iter_mut() {
                    self.rewrite(doc, state, depth + 1)?;
                }
            }
            Object::Stream(stream) => {
                *stream =
                    rewrite_appearance_stream(doc, stream, self.source_resources, self.renames)?;
            }
            _ => {
                return Err(invalid_navigation(
                    "AcroForm",
                    "appearance entry is not a stream or state dictionary",
                ));
            }
        }
        Ok(())
    }
}

fn rewrite_appearance_stream(
    doc: &Document,
    stream: &Stream,
    source_resources: &Dictionary,
    renames: &ResourceRenames,
) -> Result<Stream> {
    let original_resources = match stream.dict.get(b"Resources") {
        Ok(resources) => resolve_dict(doc, resources).ok_or_else(|| {
            invalid_navigation("AcroForm", "appearance /Resources is not a dictionary")
        })?,
        Err(_) => Dictionary::new(),
    };
    let (rewritten_resources, effective) =
        rewritten_appearance_resources(doc, &original_resources, source_resources, renames)?;
    if effective.is_empty() {
        return Ok(stream.clone());
    }

    let plain = match stream.get_plain_content() {
        Ok(plain) => plain,
        Err(_) => {
            return isolate_appearance_stream(
                doc,
                stream,
                &original_resources,
                source_resources,
                renames,
            );
        }
    };
    let mut content = match Content::decode_strict(&plain) {
        Ok(content) => content,
        Err(_) => {
            return isolate_appearance_stream(
                doc,
                stream,
                &original_resources,
                source_resources,
                renames,
            );
        }
    };
    rewrite_content_resource_names(&mut content, &effective)?;
    let mut rewritten = stream.clone();
    rewritten
        .dict
        .set("Resources", Object::Dictionary(rewritten_resources));
    rewritten.set_plain_content(content.encode()?);
    Ok(rewritten)
}

fn rewritten_appearance_resources(
    doc: &Document,
    original: &Dictionary,
    source: &Dictionary,
    renames: &ResourceRenames,
) -> Result<(Dictionary, ResourceRenames)> {
    let mut resources = original.clone();
    let mut effective = ResourceRenames::new();
    for (category, category_renames) in renames {
        let source_entries = source
            .get(category)
            .ok()
            .and_then(|object| resolve_dict(doc, object))
            .ok_or_else(|| invalid_navigation("AcroForm", "source resource category is missing"))?;
        let mut local_entries = match resources.get(category) {
            Ok(local) => resolve_dict(doc, local).ok_or_else(|| {
                invalid_navigation(
                    "AcroForm",
                    "appearance resource category is not a dictionary",
                )
            })?,
            Err(_) => Dictionary::new(),
        };
        for (original_name, renamed_name) in category_renames {
            let source_object = source_entries.get(original_name).map_err(|_| {
                invalid_navigation("AcroForm", "source resource binding is missing")
            })?;
            let should_rewrite = match local_entries.get(original_name) {
                Ok(local_object) => {
                    same_resolved_resource_identity(doc, local_object, source_object)?
                }
                Err(_) => true,
            };
            if should_rewrite {
                local_entries.remove(original_name);
                local_entries.set(renamed_name.clone(), source_object.clone());
                effective
                    .entry(category.clone())
                    .or_default()
                    .insert(original_name.clone(), renamed_name.clone());
            }
        }
        resources.set(category.clone(), Object::Dictionary(local_entries));
    }
    Ok((resources, effective))
}

fn same_resolved_resource_identity(doc: &Document, left: &Object, right: &Object) -> Result<bool> {
    let left = doc
        .dereference(left)
        .map_err(|_| invalid_navigation("AcroForm", "appearance resource is dangling"))?
        .0;
    let right = doc
        .dereference(right)
        .map_err(|_| invalid_navigation("AcroForm", "source resource is dangling"))?
        .0;
    Ok(left.is_some() && left == right)
}

fn isolate_appearance_stream(
    doc: &Document,
    stream: &Stream,
    original: &Dictionary,
    source: &Dictionary,
    renames: &ResourceRenames,
) -> Result<Stream> {
    let mut resources = original.clone();
    for (category, category_renames) in renames {
        let source_entries = source
            .get(category)
            .ok()
            .and_then(|object| resolve_dict(doc, object))
            .ok_or_else(|| invalid_navigation("AcroForm", "source resource category is missing"))?;
        let mut local_entries = match resources.get(category) {
            Ok(local) => resolve_dict(doc, local).ok_or_else(|| {
                invalid_navigation(
                    "AcroForm",
                    "appearance resource category is not a dictionary",
                )
            })?,
            Err(_) => Dictionary::new(),
        };
        for original_name in category_renames.keys() {
            if !local_entries.has(original_name) {
                let source_object = source_entries.get(original_name).map_err(|_| {
                    invalid_navigation("AcroForm", "source resource binding is missing")
                })?;
                local_entries.set(original_name.clone(), source_object.clone());
            }
        }
        resources.set(category.clone(), Object::Dictionary(local_entries));
    }
    let mut isolated = stream.clone();
    isolated
        .dict
        .set("Resources", Object::Dictionary(resources));
    Ok(isolated)
}

fn rewrite_content_resource_names(
    content: &mut Content<Vec<lopdf::content::Operation>>,
    renames: &ResourceRenames,
) -> Result<()> {
    for operation in &mut content.operations {
        let (category, operand_index, strict_name) = match operation.operator.as_str() {
            "Tf" => (b"Font".as_slice(), 0, true),
            "Do" => (b"XObject".as_slice(), 0, true),
            "gs" => (b"ExtGState".as_slice(), 0, true),
            "sh" => (b"Shading".as_slice(), 0, true),
            "cs" | "CS" => (b"ColorSpace".as_slice(), 0, true),
            "scn" | "SCN" => {
                let Some(last) = operation.operands.len().checked_sub(1) else {
                    continue;
                };
                (b"Pattern".as_slice(), last, false)
            }
            // Marked-content property lists may be named resources or inline
            // dictionaries. Only named operand 1 participates in /Properties
            // collision rewriting; inline and malformed short forms are left
            // untouched so a best-effort appearance rewrite never panics.
            "BDC" | "DP" => (b"Properties".as_slice(), 1, false),
            _ => continue,
        };
        let Some(category_renames) = renames.get(category) else {
            continue;
        };
        let Some(operand) = operation.operands.get_mut(operand_index) else {
            if strict_name {
                return Err(invalid_navigation(
                    "AcroForm",
                    "appearance resource operator has no name operand",
                ));
            }
            continue;
        };
        let Ok(name) = operand.as_name() else {
            if strict_name {
                return Err(invalid_navigation(
                    "AcroForm",
                    "appearance resource operand is not a name",
                ));
            }
            continue;
        };
        if let Some(replacement) = category_renames.get(name) {
            *operand = Object::Name(replacement.clone());
        }
    }
    Ok(())
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
    let doc = Document::load_mem(bytes)?;
    crate::meta::guard_object_count(&doc)?;
    Ok(doc)
}

fn validate_page_tree(doc: &Document, budget: &mut MergeBudget) -> Result<Vec<ObjectId>> {
    let root_id = doc
        .catalog()
        .map_err(|_| invalid_navigation("page tree", "catalog is not loadable"))?
        .get(b"Pages")
        .and_then(Object::as_reference)
        .map_err(|_| invalid_navigation("page tree", "catalog /Pages is not a reference"))?;
    let mut pages = Vec::new();
    let mut seen = BTreeSet::new();
    validate_page_tree_node(doc, root_id, None, 0, &mut seen, &mut pages, budget)?;
    Ok(pages)
}

fn validate_page_tree_node(
    doc: &Document,
    node_id: ObjectId,
    expected_parent: Option<ObjectId>,
    depth: usize,
    seen: &mut BTreeSet<ObjectId>,
    pages: &mut Vec<ObjectId>,
    budget: &mut MergeBudget,
) -> Result<()> {
    budget.tree_node(depth, "page tree")?;
    if !seen.insert(node_id) {
        return Err(invalid_navigation("page tree", "cycle or reused node"));
    }
    let node = doc
        .get_dictionary(node_id)
        .map_err(|_| invalid_navigation("page tree", "node is dangling or not a dictionary"))?;
    if let Some(parent_id) = expected_parent
        && node.get(b"Parent").and_then(Object::as_reference).ok() != Some(parent_id)
    {
        return Err(invalid_navigation("page tree", "node has invalid /Parent"));
    }
    match node.get(b"Type").and_then(Object::as_name).ok() {
        Some(b"Page") => pages.push(node_id),
        Some(b"Pages") => {
            let kids = node
                .get(b"Kids")
                .ok()
                .and_then(|object| resolve_array(doc, object))
                .ok_or_else(|| invalid_navigation("page tree", "/Kids is not an array"))?;
            for kid in kids {
                let kid_id = kid.as_reference().map_err(|_| {
                    invalid_navigation("page tree", "/Kids entry is not a reference")
                })?;
                validate_page_tree_node(
                    doc,
                    kid_id,
                    Some(node_id),
                    depth + 1,
                    seen,
                    pages,
                    budget,
                )?;
            }
        }
        _ => {
            return Err(invalid_navigation("page tree", "node has no valid /Type"));
        }
    }
    Ok(())
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
    let mut budget = MergeBudget::default();
    materialize_inherited_attributes_with_budget(doc, &mut budget)
}

fn materialize_inherited_attributes_with_budget(
    doc: &mut Document,
    budget: &mut MergeBudget,
) -> Result<()> {
    let page_ids: Vec<ObjectId> = doc.page_iter().collect();
    let mut updates: Vec<(ObjectId, &'static [u8], Object)> = Vec::new();
    for &page_id in &page_ids {
        let page_dict = doc.get_dictionary(page_id)?;
        for key in INHERITABLE_PAGE_KEYS {
            if !page_dict.has(key)
                && let Some(value) = resolve_inherited_attribute(doc, page_id, key, budget)?
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
fn resolve_inherited_attribute(
    doc: &Document,
    start_id: ObjectId,
    key: &[u8],
    budget: &mut MergeBudget,
) -> Result<Option<Object>> {
    let mut current = start_id;
    let mut seen = BTreeSet::new();
    for depth in 0..=PARENT_CHAIN_LIMIT {
        budget.tree_node(depth, "page /Parent chain")?;
        if !seen.insert(current) {
            return Err(invalid_navigation("page tree", "cycle in /Parent chain"));
        }
        let dict = doc
            .get_dictionary(current)
            .map_err(|_| invalid_navigation("page tree", "dangling /Parent chain"))?;
        if let Ok(value) = dict.get(key) {
            return Ok(Some(value.clone()));
        }
        let Ok(parent) = dict.get(b"Parent") else {
            return Ok(None);
        };
        current = parent
            .as_reference()
            .map_err(|_| invalid_navigation("page tree", "/Parent is not a reference"))?;
        if depth == PARENT_CHAIN_LIMIT {
            return Err(resource_limit(format!(
                "merge page /Parent chain depth exceeds limit {PARENT_CHAIN_LIMIT}"
            )));
        }
    }
    Err(resource_limit(format!(
        "merge page /Parent chain depth exceeds limit {PARENT_CHAIN_LIMIT}"
    )))
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

/// Reopens engine output and checks catalog/page-tree structure before bytes
/// cross the wasm boundary.
pub(crate) fn validate_output_pdf(
    bytes: &[u8],
    expected_pages: usize,
    operation: &str,
) -> Result<()> {
    let fail = |detail: &str| {
        PdfCoreError::OutputValidation(format!("output validation failed ({operation}): {detail}"))
    };
    let doc = Document::load_mem(bytes).map_err(|_| fail("output does not parse"))?;
    let catalog = doc.catalog().map_err(|_| fail("catalog is not loadable"))?;
    if catalog
        .get(b"Type")
        .and_then(Object::as_name)
        .map_err(|_| fail("catalog has no /Type /Catalog"))?
        != b"Catalog"
    {
        return Err(fail("catalog has no /Type /Catalog"));
    }
    let pages_object = catalog
        .get(b"Pages")
        .map_err(|_| fail("catalog has no /Pages"))?;
    let pages = resolve_dict(&doc, pages_object)
        .ok_or_else(|| fail("catalog /Pages tree is not loadable"))?;
    if pages
        .get(b"Type")
        .and_then(Object::as_name)
        .map_err(|_| fail("page-tree root has no /Type /Pages"))?
        != b"Pages"
    {
        return Err(fail("page-tree root has no /Type /Pages"));
    }
    let declared_count = pages
        .get(b"Count")
        .and_then(Object::as_i64)
        .ok()
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| fail("page-tree root has invalid /Count"))?;
    if declared_count != expected_pages {
        return Err(fail("page-tree /Count mismatch"));
    }
    if resolve_array(
        &doc,
        pages
            .get(b"Kids")
            .map_err(|_| fail("page-tree root has no /Kids"))?,
    )
    .is_none()
    {
        return Err(fail("page-tree root /Kids is not loadable"));
    }
    let actual_pages = doc.get_pages();
    if actual_pages.len() != expected_pages {
        return Err(fail("resolved page count mismatch"));
    }
    for page_id in actual_pages.values() {
        let page = doc
            .get_dictionary(*page_id)
            .map_err(|_| fail("page object is not loadable"))?;
        if page
            .get(b"Type")
            .and_then(Object::as_name)
            .map_err(|_| fail("page object has no /Type /Page"))?
            != b"Page"
        {
            return Err(fail("page object has no /Type /Page"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod output_validation_tests {
    use super::*;

    #[test]
    fn acroform_resource_merge_shares_same_indirect_object_identity() {
        let mut doc = Document::new();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let incoming = dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        };
        let mut merged = Dictionary::new();
        let mut budget = MergeBudget::default();
        merge_resource_dicts(&mut merged, &incoming, &mut doc, &mut budget)
            .expect("first resource merge");
        let (_, renames) = merge_resource_dicts(&mut merged, &incoming, &mut doc, &mut budget)
            .expect("second resource merge");

        assert!(renames.is_empty(), "same object identity must stay shared");
        let fonts = merged
            .get(b"Font")
            .and_then(Object::as_dict)
            .expect("Font dictionary");
        assert_eq!(fonts.len(), 1);
    }

    #[test]
    fn generated_object_budget_returns_resource_limit() {
        let mut budget = MergeBudget::default();
        budget
            .objects(MAX_MERGE_OBJECTS)
            .expect("fill object budget");
        let error = budget.objects(1).expect_err("object limit");
        assert_eq!(error.stable_code(), "resource_limit");
    }

    #[test]
    fn validator_rejects_crafted_page_count_mismatch() {
        let mut doc = Document::load_mem(&crate::testkit::sample_pdf(&["page"])).expect("load");
        let pages_id = doc
            .catalog()
            .expect("catalog")
            .get(b"Pages")
            .and_then(Object::as_reference)
            .expect("Pages");
        doc.get_dictionary_mut(pages_id)
            .expect("Pages dict")
            .set("Count", 2);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save malformed output");

        let error = validate_output_pdf(&bytes, 1, "test").expect_err("count mismatch");
        assert!(matches!(error, PdfCoreError::OutputValidation(_)));
        assert_eq!(error.stable_code(), "invalid_output");
    }
}

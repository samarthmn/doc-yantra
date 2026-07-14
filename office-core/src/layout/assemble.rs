//! Assembles an OPC package into a fully-resolved [`WordDoc`] the layout engine
//! consumes: fonts (bundled + system + document-embedded), styles, numbering,
//! settings, theme, decoded images, and parsed headers/footers.

use super::{DocDates, EncodedImage, ImageId};
use crate::drawing::chart::Chart;
use crate::drawing::diagram::{DiagramFallback, DiagramStore};
use crate::drawing::image::{add_to_budget, decode_required};
use crate::fonts::FontDb;
use crate::fonts::odttf;
use crate::opc::Package;
use crate::oxml::numbering::Numbering;
use crate::oxml::settings::Settings;
use crate::oxml::styles::StyleResolver;
use crate::oxml::table_style;
use crate::oxml::theme::Theme;
use crate::oxml::wml::{self, BlockContent, FloatingTableDistances, NoteKind, NotesPart, Table};
use crate::oxml::{R, W, attr, child, children};
use crate::{ConvertOptions, OfficeError, Result, UnsupportedCode};
use std::collections::HashMap;

/// A resolved word document ready for layout.
pub struct WordDoc {
    /// The parsed main document.
    pub document: wml::Document,
    /// The style cascade resolver.
    pub styles: StyleResolver,
    /// Numbering definitions.
    pub numbering: Numbering,
    /// Document settings.
    pub settings: Settings,
    /// The document theme.
    pub theme: Theme,
    /// The font database (bundled + system + embedded).
    pub fonts: FontDb,
    /// Decoded images (referenced by [`ImageId`]).
    pub images: Vec<EncodedImage>,
    /// PDF date strings from `docProps/core.xml`.
    pub dates: DocDates,
    main_part: String,
    part_rels: HashMap<String, HashMap<String, String>>,
    image_by_part: HashMap<String, usize>,
    headers: HashMap<String, Vec<BlockContent>>,
    footnotes: NotesPart,
    endnotes: NotesPart,
    charts_by_part: HashMap<String, Chart>,
    diagrams: DiagramStore,
    floating_table_distances: Vec<Option<FloatingTableDistances>>,
}

impl WordDoc {
    /// Opens and resolves a `.docx` package.
    pub fn open(bytes: &[u8], opts: &ConvertOptions) -> Result<WordDoc> {
        let pkg = Package::open(bytes)?;
        let main_part = pkg.main_part()?;
        let (mut document, document_metadata) =
            parse_part(&pkg, &main_part, wml::parse_document_with_metadata)?;

        let styles = optional(&pkg, "word/styles.xml", StyleResolver::parse)?.unwrap_or_default();
        let numbering = optional(&pkg, "word/numbering.xml", Numbering::parse)?.unwrap_or_default();
        let settings = optional(&pkg, "word/settings.xml", Settings::parse)?.unwrap_or_default();
        let theme = optional(&pkg, "word/theme/theme1.xml", Theme::parse)?.unwrap_or_default();

        let mut fonts = FontDb::with_system(&opts.font_dirs);
        for fb in &opts.font_bytes {
            fonts.register(fb.clone())?;
        }
        register_embedded_fonts(&pkg, &mut fonts);

        // Collect the rels for the main part and every header/footer part.
        let mut part_rels = HashMap::new();
        part_rels.insert(main_part.clone(), rel_map(&pkg, &main_part));

        // Parse headers/footers referenced by every section.
        let mut headers = HashMap::new();
        let mut section_refs = Vec::new();
        for block in &document.body {
            if let BlockContent::Paragraph(paragraph) = block
                && let Some(section) = &paragraph.sect_break
            {
                section_refs.extend(section.header_refs.iter().chain(&section.footer_refs));
            }
        }
        section_refs.extend(
            document
                .final_sect
                .header_refs
                .iter()
                .chain(document.final_sect.footer_refs.iter()),
        );
        for hf in section_refs {
            let part = part_rels[&main_part]
                .get(&hf.rel_id)
                .cloned()
                .ok_or_else(|| {
                    OfficeError::Invalid(format!(
                        "header/footer relationship {} is missing",
                        hf.rel_id
                    ))
                })?;
            if headers.contains_key(&part) {
                continue;
            }
            let bytes = pkg
                .part(&part)
                .ok_or_else(|| OfficeError::Invalid(format!("missing part {part}")))?;
            let text = std::str::from_utf8(bytes)
                .map_err(|error| OfficeError::Invalid(format!("header/footer {part}: {error}")))?;
            let (blocks, flags) = wml::parse_hdr_ftr_with_flags(text)
                .map_err(|error| error.context(format_args!("header/footer {part}")))?;
            document.flags.merge(&flags);
            part_rels.insert(part.clone(), rel_map(&pkg, &part));
            headers.insert(part, blocks);
        }

        let footnotes = optional(&pkg, "word/footnotes.xml", |xml| {
            wml::parse_notes(xml, NoteKind::Footnote)
        })?
        .unwrap_or_default();
        let endnotes = optional(&pkg, "word/endnotes.xml", |xml| {
            wml::parse_notes(xml, NoteKind::Endnote)
        })?
        .unwrap_or_default();
        document.flags.merge(&footnotes.flags);
        document.flags.merge(&endnotes.flags);
        for part in ["word/footnotes.xml", "word/endnotes.xml"] {
            if pkg.part(part).is_some() {
                part_rels.insert(part.to_string(), rel_map(&pkg, part));
            }
        }

        // Resolve every table's style before layout, so the layout engine only
        // ever sees fully-resolved table/row/cell properties.
        let mut footnotes = footnotes;
        let mut endnotes = endnotes;
        table_style::apply_table_styles(&styles, &mut document.body);
        for blocks in headers.values_mut() {
            table_style::apply_table_styles(&styles, blocks);
        }
        table_style::apply_notes_table_styles(&styles, &mut footnotes);
        table_style::apply_notes_table_styles(&styles, &mut endnotes);

        // Decode every image part referenced anywhere.
        let (images, image_by_part) = decode_images(&pkg, &part_rels)?;
        let dates = core_dates(&pkg);
        let diagrams = DiagramStore::load(&pkg)?;
        let mut chart_parts: Vec<_> = part_rels
            .values()
            .flat_map(|rels| rels.values())
            .cloned()
            .collect();
        chart_parts.sort();
        chart_parts.dedup();
        let mut charts_by_part = HashMap::new();
        for part in chart_parts {
            let Some(xml) = pkg
                .part(&part)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            else {
                continue;
            };
            if let Ok(chart) = Chart::parse(xml) {
                charts_by_part.insert(part, chart);
            }
        }

        Ok(WordDoc {
            document,
            styles,
            numbering,
            settings,
            theme,
            fonts,
            images,
            dates,
            main_part,
            part_rels,
            image_by_part,
            headers,
            footnotes,
            endnotes,
            charts_by_part,
            diagrams,
            floating_table_distances: document_metadata.floating_table_distances,
        })
    }

    /// Exact clearances for a floating table at a stable main-body block
    /// index. Publicly-mutated or style-supplied tables retain the legacy
    /// symmetric-padding behavior as a compatibility fallback.
    pub(crate) fn floating_table_distances(
        &self,
        body_block_index: Option<usize>,
        table: &Table,
    ) -> FloatingTableDistances {
        body_block_index
            .and_then(|index| self.floating_table_distances.get(index))
            .copied()
            .flatten()
            .unwrap_or_else(|| {
                let padding = table
                    .props
                    .floating
                    .map_or(0.0, |floating| floating.wrap_padding_pt);
                FloatingTableDistances {
                    left_pt: padding,
                    right_pt: padding,
                    bottom_pt: padding,
                }
            })
    }

    /// Resolves note content and relationships by family/id.
    pub fn note_content(
        &self,
        kind: NoteKind,
        id: i64,
    ) -> Option<(&[BlockContent], &HashMap<String, String>)> {
        let (part, notes) = match kind {
            NoteKind::Footnote => ("word/footnotes.xml", &self.footnotes),
            NoteKind::Endnote => ("word/endnotes.xml", &self.endnotes),
        };
        Some((notes.notes.get(&id)?.as_slice(), self.part_rels.get(part)?))
    }

    /// All endnotes in stable id order.
    pub fn endnotes(&self) -> &NotesPart {
        &self.endnotes
    }

    /// The rel map (`rel_id -> target part`) of the main document part.
    pub fn main_rels(&self) -> &HashMap<String, String> {
        &self.part_rels[&self.main_part]
    }

    /// The rel map for an arbitrary part (headers/footers).
    pub fn rels_for(&self, part: &str) -> Option<&HashMap<String, String>> {
        self.part_rels.get(part)
    }

    /// Resolves a blip `rel_id` (against `rels`) to an image index.
    pub fn image_id(&self, rels: &HashMap<String, String>, rel_id: &str) -> Option<ImageId> {
        let target = rels.get(rel_id)?;
        self.image_by_part.get(target).map(|&i| ImageId(i))
    }

    /// Resolves a chart relationship against the current document part.
    pub fn chart(&self, rels: &HashMap<String, String>, rel_id: &str) -> Option<&Chart> {
        self.charts_by_part.get(rels.get(rel_id)?)
    }

    /// Resolves a SmartArt data relationship to its pre-rendered drawing fallback.
    pub fn diagram(
        &self,
        rels: &HashMap<String, String>,
        rel_id: &str,
    ) -> Option<&DiagramFallback> {
        self.diagrams.drawing(rels, rel_id)
    }

    /// Returns a typed reason when a SmartArt drawing fallback cannot be resolved.
    pub fn diagram_error(
        &self,
        rels: &HashMap<String, String>,
        rel_id: &str,
    ) -> Option<(UnsupportedCode, String)> {
        self.diagrams.error(rels, rel_id)
    }

    /// Resolves a hyperlink `rel_id` to its external URL (if external).
    pub fn hyperlink_url(&self, rels: &HashMap<String, String>, rel_id: &str) -> Option<String> {
        rels.get(rel_id).cloned()
    }

    /// The parsed blocks of a header/footer part referenced by `rel_id`.
    pub fn header_content(
        &self,
        rel_id: &str,
    ) -> Option<(&[BlockContent], &HashMap<String, String>)> {
        let part = self.main_rels().get(rel_id)?;
        Some((
            self.headers.get(part)?.as_slice(),
            self.part_rels.get(part)?,
        ))
    }
}

fn parse_part<T>(pkg: &Package, part: &str, parse: impl Fn(&str) -> Result<T>) -> Result<T> {
    let bytes = pkg
        .part(part)
        .ok_or_else(|| OfficeError::Invalid(format!("missing part {part}")))?;
    let text = std::str::from_utf8(bytes)
        .map_err(|_| OfficeError::Invalid(format!("{part} is not UTF-8")))?;
    parse(text)
}

fn optional<T>(pkg: &Package, part: &str, parse: impl Fn(&str) -> Result<T>) -> Result<Option<T>> {
    match pkg.part(part) {
        Some(_) => parse_part(pkg, part, parse).map(Some),
        None => Ok(None),
    }
}

fn rel_map(pkg: &Package, source_part: &str) -> HashMap<String, String> {
    pkg.relationships(source_part)
        .map(|rels| rels.into_iter().map(|r| (r.id, r.target)).collect())
        .unwrap_or_default()
}

/// Parses `word/fontTable.xml` and registers each embedded (ODTTF) font.
fn register_embedded_fonts(pkg: &Package, fonts: &mut FontDb) {
    let Some(bytes) = pkg.part("word/fontTable.xml") else {
        return;
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let Ok(doc) = roxmltree::Document::parse(text) else {
        return;
    };
    let rels = rel_map(pkg, "word/fontTable.xml");

    for font in children(doc.root_element()) {
        if !crate::oxml::is(font, W, "font") {
            continue;
        }
        let Some(name) = attr(font, W, "name") else {
            continue;
        };
        let slots = [
            ("embedRegular", false, false),
            ("embedBold", true, false),
            ("embedItalic", false, true),
            ("embedBoldItalic", true, true),
        ];
        for (elem, bold, italic) in slots {
            if let Some(embed) = child(font, W, elem)
                && let Some(rid) = attr(embed, R, "id")
                && let Some(font_key) = attr(embed, W, "fontKey")
                && let Some(target) = rels.get(rid)
                && let Some(odttf) = pkg.part(target)
                && let Ok(sfnt) = odttf::deobfuscate(font_key, odttf)
            {
                fonts.register_embedded(name, sfnt, bold, italic);
            }
        }
    }
}

fn decode_images(
    pkg: &Package,
    part_rels: &HashMap<String, HashMap<String, String>>,
) -> Result<(Vec<EncodedImage>, HashMap<String, usize>)> {
    let mut images = Vec::new();
    let mut by_part: HashMap<String, usize> = HashMap::new();

    let mut targets: Vec<String> = part_rels
        .values()
        .flat_map(|rels| rels.values().cloned())
        .collect();
    targets.sort();
    targets.dedup();
    let mut total_pixels = 0u64;

    for target in targets {
        if by_part.contains_key(&target) {
            continue;
        }
        let Some(ct) = pkg.content_type(&target) else {
            continue;
        };
        let is_image = ct.starts_with("image/");
        if !is_image {
            continue;
        }
        let bytes = pkg.part(&target).ok_or_else(|| {
            OfficeError::unsupported(
                UnsupportedCode::MissingRelationshipTarget,
                format!("image relationship target {target} is missing"),
            )
        })?;
        let (decoded, pixels) =
            decode_required(ct, bytes).map_err(|error| error.context(&target))?;
        add_to_budget(images.len(), &mut total_pixels, pixels)?;
        by_part.insert(target, images.len());
        images.push(decoded);
    }
    Ok((images, by_part))
}

/// Reads `docProps/core.xml` created/modified and formats PDF date literals.
fn core_dates(pkg: &Package) -> DocDates {
    let mut dates = DocDates::default();
    // Package lookup is case-insensitive, but keep the canonical OPC spelling
    // visible here because ZIP producers conventionally write `docProps/`.
    let Some(bytes) = pkg.part("docProps/core.xml") else {
        return dates;
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return dates;
    };
    let Ok(doc) = roxmltree::Document::parse(text) else {
        return dates;
    };
    for node in doc.root_element().descendants() {
        match node.tag_name().name() {
            "created" => {
                if let Some(d) = node.text().and_then(iso_to_pdf_date) {
                    dates.created = d;
                }
            }
            "modified" => {
                if let Some(d) = node.text().and_then(iso_to_pdf_date) {
                    dates.modified = d;
                }
            }
            _ => {}
        }
    }
    dates
}

/// Converts an ISO-8601 date (`2023-05-01T12:34:56Z`) to a PDF date literal.
fn iso_to_pdf_date(s: &str) -> Option<String> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    let mut out = String::from("D:");
    out.push_str(&digits[..digits.len().min(14)]);
    while out.len() < 16 {
        out.push('0');
    }
    out.push('Z');
    Some(out)
}

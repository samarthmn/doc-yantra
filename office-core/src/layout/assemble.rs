//! Assembles an OPC package into a fully-resolved [`WordDoc`] the layout engine
//! consumes: fonts (bundled + system + document-embedded), styles, numbering,
//! settings, theme, decoded images, and parsed headers/footers.

use super::{DocDates, EncodedImage, ImageId};
use crate::fonts::FontDb;
use crate::fonts::odttf;
use crate::opc::Package;
use crate::oxml::numbering::Numbering;
use crate::oxml::settings::Settings;
use crate::oxml::styles::StyleResolver;
use crate::oxml::theme::Theme;
use crate::oxml::wml::{self, BlockContent};
use crate::oxml::{R, W, attr, child, children};
use crate::{ConvertOptions, OfficeError, Result};
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
}

impl WordDoc {
    /// Opens and resolves a `.docx` package.
    pub fn open(bytes: &[u8], opts: &ConvertOptions) -> Result<WordDoc> {
        let pkg = Package::open(bytes)?;
        let main_part = pkg.main_part()?;
        let document = parse_part(&pkg, &main_part, wml::parse_document)?;

        let styles = optional(&pkg, "word/styles.xml", StyleResolver::parse)?.unwrap_or_default();
        let numbering = optional(&pkg, "word/numbering.xml", Numbering::parse)?.unwrap_or_default();
        let settings = optional(&pkg, "word/settings.xml", Settings::parse)?.unwrap_or_default();
        let theme = optional(&pkg, "word/theme/theme1.xml", Theme::parse)?.unwrap_or_default();

        let mut fonts = FontDb::with_system(&opts.font_dirs);
        for fb in &opts.font_bytes {
            fonts.register(fb.clone()).ok();
        }
        register_embedded_fonts(&pkg, &mut fonts);

        // Collect the rels for the main part and every header/footer part.
        let mut part_rels = HashMap::new();
        part_rels.insert(main_part.clone(), rel_map(&pkg, &main_part));

        // Parse headers/footers referenced by the final section.
        let mut headers = HashMap::new();
        for hf in document
            .final_sect
            .header_refs
            .iter()
            .chain(document.final_sect.footer_refs.iter())
        {
            if let Some(part) = part_rels[&main_part].get(&hf.rel_id).cloned()
                && !headers.contains_key(&part)
                && let Some(bytes) = pkg.part(&part)
                && let Ok(text) = std::str::from_utf8(bytes)
                && let Ok(blocks) = wml::parse_hdr_ftr(text)
            {
                part_rels.insert(part.clone(), rel_map(&pkg, &part));
                headers.insert(part, blocks);
            }
        }

        // Decode every image part referenced anywhere.
        let (images, image_by_part) = decode_images(&pkg, &part_rels);
        let dates = core_dates(&pkg);

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
        })
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

    /// Resolves a hyperlink `rel_id` to its external URL (if external).
    pub fn hyperlink_url(&self, rels: &HashMap<String, String>, rel_id: &str) -> Option<String> {
        rels.get(rel_id).cloned()
    }

    /// The parsed blocks of a header/footer part referenced by `rel_id`.
    pub fn header_blocks(&self, rel_id: &str) -> Option<&[BlockContent]> {
        let part = self.main_rels().get(rel_id)?;
        self.headers.get(part).map(Vec::as_slice)
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

/// Decodes every image part referenced by any collected part's rels.
fn decode_images(
    pkg: &Package,
    part_rels: &HashMap<String, HashMap<String, String>>,
) -> (Vec<EncodedImage>, HashMap<String, usize>) {
    let mut images = Vec::new();
    let mut by_part: HashMap<String, usize> = HashMap::new();

    let mut targets: Vec<String> = part_rels
        .values()
        .flat_map(|rels| rels.values().cloned())
        .collect();
    targets.sort();
    targets.dedup();

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
        if let Some(bytes) = pkg.part(&target)
            && let Some(decoded) = decode_image(ct, bytes)
        {
            by_part.insert(target, images.len());
            images.push(decoded);
        }
    }
    (images, by_part)
}

/// Decodes a supported image part; `None` for unsupported containers.
fn decode_image(content_type: &str, bytes: &[u8]) -> Option<EncodedImage> {
    let is_jpeg = content_type.contains("jpeg") || content_type.contains("jpg");
    if is_jpeg {
        let (width, height) = image::load_from_memory(bytes)
            .ok()
            .map(|img| (img.width(), img.height()))?;
        return Some(EncodedImage::Jpeg {
            data: bytes.to_vec(),
            width,
            height,
        });
    }
    // PNG (and anything else `image` can decode) → RGB8/RGBA8 samples.
    let img = image::load_from_memory(bytes).ok()?;
    let width = img.width();
    let height = img.height();
    if img.color().has_alpha() {
        Some(EncodedImage::Rgba8 {
            width,
            height,
            data: img.into_rgba8().into_raw(),
        })
    } else {
        Some(EncodedImage::Rgb8 {
            width,
            height,
            data: img.into_rgb8().into_raw(),
        })
    }
}

/// Reads `docProps/core.xml` created/modified and formats PDF date literals.
fn core_dates(pkg: &Package) -> DocDates {
    let mut dates = DocDates::default();
    let Some(bytes) = pkg.part("docprops/core.xml") else {
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

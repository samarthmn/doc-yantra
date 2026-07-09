//! Assembles a PPTX package into a [`PptxDoc`]: presentation, each slide with
//! its layout + master, the theme, master text-style defaults, decoded images,
//! and fonts.

use crate::fonts::FontDb;
use crate::layout::{DocDates, EncodedImage, ImageId};
use crate::opc::Package;
use crate::oxml::pml::{Presentation, Slide};
use crate::oxml::theme::Theme;
use crate::oxml::{A, P, attr, child, children, parse_f64};
use crate::{ConvertOptions, OfficeError, Result};
use std::collections::HashMap;

/// A resolved slide plus the layout/master needed for inheritance.
pub struct ResolvedSlide {
    /// The slide.
    pub slide: Slide,
    /// The slide's layout (placeholder geometry/text-style source).
    pub layout: Slide,
    /// Rel map for the slide part (image resolution).
    pub rels: HashMap<String, String>,
}

/// Default run sizes (points) per placeholder category and level (0–8).
#[derive(Debug, Clone, Default)]
pub struct TextDefaults {
    /// Title style sizes by level.
    pub title: Vec<f64>,
    /// Body style sizes by level.
    pub body: Vec<f64>,
    /// Other style sizes by level.
    pub other: Vec<f64>,
}

impl TextDefaults {
    /// The default size for a placeholder category and level.
    pub fn size(&self, category: Category, level: u8) -> f64 {
        let table = match category {
            Category::Title => &self.title,
            Category::Body => &self.body,
            Category::Other => &self.other,
        };
        table
            .get(level as usize)
            .copied()
            .unwrap_or(match category {
                Category::Title => 44.0,
                Category::Body => 18.0,
                Category::Other => 18.0,
            })
    }
}

/// The placeholder category (drives which master style bucket applies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Titles (`title`, `ctrTitle`).
    Title,
    /// Body / content placeholders.
    Body,
    /// Everything else.
    Other,
}

/// A fully-resolved presentation.
pub struct PptxDoc {
    /// The presentation (size + slide order).
    pub presentation: Presentation,
    /// The master shape tree (defaults / background).
    pub master: Slide,
    /// Resolved slides in order.
    pub slides: Vec<ResolvedSlide>,
    /// The theme.
    pub theme: Theme,
    /// Master text-size defaults.
    pub text_defaults: TextDefaults,
    /// Fonts.
    pub fonts: FontDb,
    /// Decoded images.
    pub images: Vec<EncodedImage>,
    /// PDF dates.
    pub dates: DocDates,
    image_by_part: HashMap<String, usize>,
}

impl PptxDoc {
    /// Opens and resolves a `.pptx` package.
    pub fn open(bytes: &[u8], opts: &ConvertOptions) -> Result<PptxDoc> {
        let pkg = Package::open(bytes)?;
        let pres_part = pkg
            .relationships("")?
            .into_iter()
            .find(|r| r.rel_type.ends_with("/officeDocument"))
            .map(|r| r.target)
            .unwrap_or_else(|| "ppt/presentation.xml".to_string());
        // Reject a mismatched-format package (e.g. a .docx routed here): without
        // this the main part parses to zero slides and we emit an invalid 0-page
        // PDF. The main part's content type must be presentationml.
        if !pkg
            .content_type(&pres_part)
            .is_some_and(|ct| ct.contains("presentationml"))
        {
            return Err(OfficeError::Invalid(
                "main part is not a presentation".into(),
            ));
        }
        let presentation = parse_str(&pkg, &pres_part, Presentation::parse)?;

        let pres_rels = rel_map(&pkg, &pres_part);

        // The (single, v1) master + theme.
        let master_part = pres_rels
            .values()
            .find(|t| t.contains("slidemaster"))
            .cloned();
        let master = master_part
            .as_deref()
            .and_then(|p| parse_str(&pkg, p, Slide::parse).ok())
            .unwrap_or_default();
        let master_rels = master_part
            .as_deref()
            .map(|p| rel_map(&pkg, p))
            .unwrap_or_default();
        let theme = master_rels
            .values()
            .find(|t| t.contains("theme"))
            .and_then(|p| parse_str(&pkg, p, Theme::parse).ok())
            .or_else(|| parse_str(&pkg, "ppt/theme/theme1.xml", Theme::parse).ok())
            .unwrap_or_default();
        let text_defaults = master_part
            .as_deref()
            .and_then(|p| pkg.part(p))
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(parse_text_defaults)
            .unwrap_or_default();

        // Resolve slides in presentation order.
        let mut slides = Vec::new();
        for rid in &presentation.slide_rel_ids {
            let Some(slide_part) = pres_rels.get(rid) else {
                continue;
            };
            let Ok(slide) = parse_str(&pkg, slide_part, Slide::parse) else {
                continue;
            };
            let slide_rels = rel_map(&pkg, slide_part);
            let layout = slide_rels
                .values()
                .find(|t| t.contains("slidelayout"))
                .and_then(|p| parse_str(&pkg, p, Slide::parse).ok())
                .unwrap_or_default();
            slides.push(ResolvedSlide {
                slide,
                layout,
                rels: slide_rels,
            });
        }

        // PPTX embedded fonts are rare; bundled + system cover the envelope.
        let mut fonts = FontDb::with_system(&opts.font_dirs);
        for fb in &opts.font_bytes {
            fonts.register(fb.clone()).ok();
        }

        let (images, image_by_part) = decode_images(&pkg);
        let dates = core_dates(&pkg);

        Ok(PptxDoc {
            presentation,
            master,
            slides,
            theme,
            text_defaults,
            fonts,
            images,
            dates,
            image_by_part,
        })
    }

    /// Resolves a blip rel id (against a slide's rels) to an image index.
    pub fn image_id(&self, rels: &HashMap<String, String>, rel_id: &str) -> Option<ImageId> {
        let target = rels.get(rel_id)?;
        self.image_by_part.get(target).map(|&i| ImageId(i))
    }
}

fn parse_str<T>(pkg: &Package, part: &str, parse: impl Fn(&str) -> Result<T>) -> Result<T> {
    let bytes = pkg
        .part(part)
        .ok_or_else(|| OfficeError::Invalid(format!("missing part {part}")))?;
    let text = std::str::from_utf8(bytes)
        .map_err(|_| OfficeError::Invalid(format!("{part} is not UTF-8")))?;
    parse(text)
}

fn rel_map(pkg: &Package, source_part: &str) -> HashMap<String, String> {
    pkg.relationships(source_part)
        .map(|rels| rels.into_iter().map(|r| (r.id, r.target)).collect())
        .unwrap_or_default()
}

/// Parses master `p:txStyles` into default sizes per bucket + level.
fn parse_text_defaults(xml: &str) -> TextDefaults {
    let mut defs = TextDefaults::default();
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return defs;
    };
    for node in doc.root_element().descendants() {
        if !crate::oxml::is(node, P, "txStyles") {
            continue;
        }
        for bucket in children(node) {
            let target = match bucket.tag_name().name() {
                "titleStyle" => &mut defs.title,
                "bodyStyle" => &mut defs.body,
                "otherStyle" => &mut defs.other,
                _ => continue,
            };
            for lvl in children(bucket) {
                let name = lvl.tag_name().name();
                if let Some(rest) = name.strip_prefix("lvl")
                    && rest.ends_with("pPr")
                {
                    let sz = child(lvl, A, "defRPr")
                        .and_then(|r| parse_f64(attr(r, A, "sz")))
                        .map(|s| s / 100.0)
                        .unwrap_or(18.0);
                    target.push(sz);
                }
            }
        }
    }
    defs
}

fn decode_images(pkg: &Package) -> (Vec<EncodedImage>, HashMap<String, usize>) {
    let mut images = Vec::new();
    let mut by_part = HashMap::new();
    let names: Vec<String> = pkg.part_names().map(str::to_string).collect();
    for name in names {
        let Some(ct) = pkg.content_type(&name) else {
            continue;
        };
        if !ct.starts_with("image/") {
            continue;
        }
        if let Some(bytes) = pkg.part(&name)
            && let Some(decoded) = decode_image(ct, bytes)
        {
            by_part.insert(name.clone(), images.len());
            images.push(decoded);
        }
    }
    (images, by_part)
}

fn decode_image(content_type: &str, bytes: &[u8]) -> Option<EncodedImage> {
    if content_type.contains("jpeg") || content_type.contains("jpg") {
        let (w, h) = image::load_from_memory(bytes)
            .ok()
            .map(|i| (i.width(), i.height()))?;
        return Some(EncodedImage::Jpeg {
            data: bytes.to_vec(),
            width: w,
            height: h,
        });
    }
    let img = image::load_from_memory(bytes).ok()?;
    let (w, h) = (img.width(), img.height());
    if img.color().has_alpha() {
        Some(EncodedImage::Rgba8 {
            width: w,
            height: h,
            data: img.into_rgba8().into_raw(),
        })
    } else {
        Some(EncodedImage::Rgb8 {
            width: w,
            height: h,
            data: img.into_rgb8().into_raw(),
        })
    }
}

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
        let d = node.text().and_then(iso_to_pdf_date);
        match node.tag_name().name() {
            "created" => {
                if let Some(d) = d {
                    dates.created = d;
                }
            }
            "modified" => {
                if let Some(d) = d {
                    dates.modified = d;
                }
            }
            _ => {}
        }
    }
    dates
}

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

//! Assembles a PPTX package into a [`PptxDoc`]: presentation, each slide with
//! its layout + master, the theme, master text-style defaults, decoded images,
//! and fonts.

use crate::drawing::chart::Chart;
use crate::drawing::diagram::{DiagramFallback, DiagramStore};
use crate::drawing::image::{add_to_budget, decode_required, validate_crop_units};
use crate::fonts::FontDb;
use crate::layout::{DocDates, EncodedImage, ImageId, LinkTarget};
use crate::opc::Package;
use crate::oxml::pml::{ColorMapOverride, CropRect, Fill, Presentation, ShapeElem, Slide};
use crate::oxml::theme::Theme;
use crate::oxml::{A, P, attr, child, children, parse_f64};
use crate::pptx::table_style::{TableCellStyle, TableStyleList};
use crate::{ConvertOptions, OfficeError, Result, UnsupportedCode};
use std::collections::{BTreeSet, HashMap};

/// A resolved slide plus the layout/master needed for inheritance.
pub struct ResolvedSlide {
    /// The slide.
    pub slide: Slide,
    /// The slide's layout (placeholder geometry/text-style source).
    pub layout: Slide,
    /// The slide's own master.
    pub master: Slide,
    /// Rel map for the slide part (image resolution).
    pub rels: HashMap<String, String>,
    /// Relationship map for layout part.
    pub layout_rels: HashMap<String, String>,
    /// Relationship map for this slide's master part.
    pub master_rels: HashMap<String, String>,
    /// Theme resolved through this slide's layout → master chain and color maps.
    pub theme: Theme,
    /// Text defaults from this slide's own master.
    pub text_defaults: TextDefaults,
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
    /// Relationship map for master part.
    pub master_rels: HashMap<String, String>,
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
    image_errors_by_part: HashMap<String, (UnsupportedCode, String)>,
    cropped_image_by_part: HashMap<(String, CropRect), usize>,
    cropped_image_errors_by_part: HashMap<(String, CropRect), (UnsupportedCode, String)>,
    slide_page_by_part: HashMap<String, u32>,
    charts_by_part: HashMap<String, Chart>,
    chart_errors_by_part: HashMap<String, (UnsupportedCode, String)>,
    diagrams: DiagramStore,
    content_types_by_part: HashMap<String, String>,
    package_parts: BTreeSet<String>,
    table_styles: Option<TableStyleList>,
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

        let fallback_master_part = pres_rels
            .values()
            .find(|t| t.contains("slidemaster"))
            .cloned();

        // Resolve slides in presentation order.
        let mut slides = Vec::new();
        let mut slide_page_by_part = HashMap::new();
        for (index, rid) in presentation.slide_rel_ids.iter().enumerate() {
            let slide_number = index + 1;
            let slide_part = pres_rels.get(rid).ok_or_else(|| {
                OfficeError::Invalid(format!("slide {slide_number}: missing relationship {rid}"))
            })?;
            let slide = parse_str(&pkg, slide_part, Slide::parse)
                .map_err(|error| error.context(format_args!("slide {slide_number}")))?;
            let slide_rels = rel_map(&pkg, slide_part);
            slide_page_by_part.insert(slide_part.clone(), index as u32);
            let layout_part = slide_rels
                .values()
                .find(|target| target.contains("slidelayout"))
                .cloned();
            let layout = match layout_part.as_deref() {
                Some(part) => parse_str(&pkg, part, Slide::parse)
                    .map_err(|error| error.context(format_args!("slide {slide_number} layout")))?,
                None => Slide::default(),
            };
            let layout_rels = layout_part
                .as_deref()
                .map(|part| rel_map(&pkg, part))
                .unwrap_or_default();
            let master_part = layout_rels
                .values()
                .find(|target| target.contains("slidemaster"))
                .cloned()
                .or_else(|| fallback_master_part.clone());
            let master = match master_part.as_deref() {
                Some(part) => parse_str(&pkg, part, Slide::parse)
                    .map_err(|error| error.context(format_args!("slide {slide_number} master")))?,
                None => Slide::default(),
            };
            let master_rels = master_part
                .as_deref()
                .map(|part| rel_map(&pkg, part))
                .unwrap_or_default();
            let base_theme = if let Some(part) =
                master_rels.values().find(|target| target.contains("theme"))
            {
                parse_str(&pkg, part, Theme::parse)
                    .map_err(|error| error.context(format_args!("slide {slide_number} theme")))?
            } else if pkg.part("ppt/theme/theme1.xml").is_some() {
                parse_str(&pkg, "ppt/theme/theme1.xml", Theme::parse)
                    .map_err(|error| error.context(format_args!("slide {slide_number} theme")))?
            } else {
                Theme::default()
            };
            let color_map = effective_color_map(&master, &layout, &slide);
            let theme = base_theme.with_color_map(&color_map);
            let text_defaults = master_part
                .as_deref()
                .and_then(|part| pkg.part(part))
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .map(parse_text_defaults)
                .unwrap_or_default();
            slides.push(ResolvedSlide {
                slide,
                layout,
                master,
                rels: slide_rels,
                layout_rels,
                master_rels,
                theme,
                text_defaults,
            });
        }

        // Retained as first-slide compatibility accessors; all rendering and
        // profiling below uses the per-slide chain on `ResolvedSlide`.
        let master = slides
            .first()
            .map(|slide| slide.master.clone())
            .unwrap_or_default();
        let master_rels = slides
            .first()
            .map(|slide| slide.master_rels.clone())
            .unwrap_or_default();
        let theme = slides
            .first()
            .map(|slide| slide.theme.clone())
            .unwrap_or_default();
        let text_defaults = slides
            .first()
            .map(|slide| slide.text_defaults.clone())
            .unwrap_or_default();

        // PPTX embedded fonts are rare; bundled + system cover the envelope.
        let mut fonts = FontDb::with_sources(&opts.font_dirs, opts.include_system_fonts);
        fonts.set_resolution_mode(opts.resolution_mode);
        for fb in &opts.font_bytes {
            fonts.register(fb.clone())?;
        }
        crate::register_runtime_fonts(&mut fonts)?;
        fonts.mark_hash_pinned(&opts.font_hash_pins);

        let (mut images, image_by_part, image_errors_by_part, mut total_image_pixels) =
            decode_images(&pkg);
        let mut crop_requests = Vec::new();
        for slide in &slides {
            collect_slide_crops(&slide.master, &slide.master_rels, &mut crop_requests);
            collect_slide_crops(&slide.layout, &slide.layout_rels, &mut crop_requests);
            collect_slide_crops(&slide.slide, &slide.rels, &mut crop_requests);
        }
        crop_requests.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| crop_key(left.1).cmp(&crop_key(right.1)))
        });
        crop_requests.dedup();
        let (cropped_image_by_part, cropped_image_errors_by_part) = decode_crops(
            &pkg,
            &crop_requests,
            &image_errors_by_part,
            &mut images,
            &mut total_image_pixels,
        );
        let dates = core_dates(&pkg);
        let mut charts_by_part = HashMap::new();
        let mut chart_errors_by_part = HashMap::new();
        for part in pkg
            .part_names()
            .filter(|part| part.contains("/charts/chart"))
        {
            let result = pkg
                .part(part)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .ok_or_else(|| OfficeError::Invalid(format!("invalid chart part {part}")))
                .and_then(Chart::parse);
            match result {
                Ok(chart) => {
                    charts_by_part.insert(part.to_string(), chart);
                }
                Err(OfficeError::Unsupported { code, detail }) => {
                    chart_errors_by_part.insert(part.to_string(), (code, detail));
                }
                Err(error) => return Err(error.context(part)),
            }
        }
        let diagrams = DiagramStore::load(&pkg)?;
        let content_types_by_part = pkg
            .part_names()
            .filter_map(|part| {
                pkg.content_type(part)
                    .map(|content_type| (part.to_string(), content_type.to_string()))
            })
            .collect();
        let package_parts = pkg.part_names().map(str::to_string).collect();
        let table_styles = pkg
            .part_names()
            .find(|part| part.ends_with("/tablestyles.xml"))
            .map(|part| parse_str(&pkg, part, TableStyleList::parse))
            .transpose()?;

        Ok(PptxDoc {
            presentation,
            master,
            master_rels,
            slides,
            theme,
            text_defaults,
            fonts,
            images,
            dates,
            image_by_part,
            image_errors_by_part,
            cropped_image_by_part,
            cropped_image_errors_by_part,
            slide_page_by_part,
            charts_by_part,
            chart_errors_by_part,
            diagrams,
            content_types_by_part,
            package_parts,
            table_styles,
        })
    }

    /// Resolves a blip rel id (against a slide's rels) to an image index.
    pub fn image_id(&self, rels: &HashMap<String, String>, rel_id: &str) -> Option<ImageId> {
        let target = rels.get(rel_id)?;
        self.image_by_part.get(target).map(|&i| ImageId(i))
    }

    /// Resolves an image relationship and optional crop to a resource id.
    pub fn image_id_with_crop(
        &self,
        rels: &HashMap<String, String>,
        rel_id: &str,
        crop: Option<CropRect>,
    ) -> Option<ImageId> {
        let target = rels.get(rel_id)?;
        crop.and_then(|crop| {
            self.cropped_image_by_part
                .get(&(target.clone(), crop))
                .copied()
        })
        .or_else(|| self.image_by_part.get(target).copied())
        .map(ImageId)
    }

    /// Returns a stable typed reason when a visible image relationship, part,
    /// decode, or requested crop cannot be rendered.
    pub fn image_error(
        &self,
        rels: &HashMap<String, String>,
        rel_id: &str,
        crop: Option<CropRect>,
    ) -> Option<(UnsupportedCode, String)> {
        let Some(part) = rels.get(rel_id) else {
            return Some((
                UnsupportedCode::MissingRelationshipTarget,
                format!("image relationship {rel_id} is missing"),
            ));
        };
        if !self.package_parts.contains(part) {
            return Some((
                UnsupportedCode::MissingRelationshipTarget,
                format!("image relationship target {part} is missing"),
            ));
        }
        if !self.content_types_by_part.contains_key(part) {
            return Some((
                UnsupportedCode::ImageContentTypeMissing,
                format!("image relationship target {part} has no content type"),
            ));
        }
        if let Some(crop) = crop {
            if let Some(error) = self.cropped_image_errors_by_part.get(&(part.clone(), crop)) {
                return Some(error.clone());
            }
            if self
                .cropped_image_by_part
                .contains_key(&(part.clone(), crop))
            {
                return None;
            }
        }
        self.image_errors_by_part.get(part).cloned().or_else(|| {
            (!self.image_by_part.contains_key(part)).then(|| {
                (
                    UnsupportedCode::MissingRelationshipTarget,
                    format!("image relationship target {part} is missing"),
                )
            })
        })
    }

    /// Resolves a hyperlink relationship under PPTX's safe-link policy.
    pub fn link_target(&self, rels: &HashMap<String, String>, rel_id: &str) -> Option<LinkTarget> {
        let target = rels.get(rel_id)?;
        if target.starts_with("https://")
            || target.starts_with("http://")
            || target.starts_with("mailto:")
        {
            return Some(LinkTarget::Url(target.clone()));
        }
        self.slide_page_by_part
            .get(target)
            .copied()
            .map(LinkTarget::Page)
    }

    /// Resolves a chart relationship to parsed cached chart data.
    pub fn chart(&self, rels: &HashMap<String, String>, rel_id: &str) -> Option<&Chart> {
        self.charts_by_part.get(rels.get(rel_id)?)
    }

    /// Returns a stable typed reason for an unrenderable chart relationship.
    pub fn chart_error(
        &self,
        rels: &HashMap<String, String>,
        rel_id: &str,
    ) -> Option<(UnsupportedCode, String)> {
        let Some(part) = rels.get(rel_id) else {
            return Some((
                UnsupportedCode::Chart,
                format!("missing chart relationship {rel_id}"),
            ));
        };
        self.chart_errors_by_part.get(part).cloned().or_else(|| {
            (!self.charts_by_part.contains_key(part))
                .then(|| (UnsupportedCode::Chart, format!("missing chart part {part}")))
        })
    }

    /// Resolves a SmartArt data relationship to its pre-rendered fallback.
    pub fn diagram(
        &self,
        rels: &HashMap<String, String>,
        rel_id: &str,
    ) -> Option<&DiagramFallback> {
        self.diagrams.drawing(rels, rel_id)
    }

    /// Returns a typed reason when SmartArt has no usable drawing fallback.
    pub fn diagram_error(
        &self,
        rels: &HashMap<String, String>,
        rel_id: &str,
    ) -> Option<(UnsupportedCode, String)> {
        self.diagrams.error(rels, rel_id)
    }

    /// Returns a typed reason when an OLE preview is absent or vector-only.
    pub fn ole_preview_error(
        &self,
        rels: &HashMap<String, String>,
        rel_id: &str,
    ) -> Option<(UnsupportedCode, String)> {
        let Some(part) = rels.get(rel_id) else {
            return Some((
                UnsupportedCode::OleObject,
                format!("OLE preview relationship {rel_id} is missing"),
            ));
        };
        let content_type = self.content_types_by_part.get(part).map(String::as_str);
        if matches!(content_type, Some("image/png" | "image/jpeg" | "image/jpg")) {
            return self.image_error(rels, rel_id, None);
        }
        let (code, label) = match content_type {
            Some("image/x-emf" | "image/emf") => (UnsupportedCode::OlePreviewEmf, "EMF"),
            Some("image/x-wmf" | "image/wmf" | "image/x-wmf+xml") => {
                (UnsupportedCode::OlePreviewWmf, "WMF")
            }
            _ => (UnsupportedCode::OleObject, "unsupported or missing"),
        };
        Some((code, format!("OLE preview {part} is {label}")))
    }

    /// Resolves the package's real table-style cascade for one cell.
    pub(crate) fn table_cell_style(
        &self,
        table: &crate::oxml::pml::PmlTable,
        row: usize,
        col: usize,
    ) -> Result<Option<TableCellStyle>> {
        match &self.table_styles {
            Some(styles) => styles.resolve_cell(table, row, col),
            None if table.style_id.is_none() => Ok(None),
            None => Err(OfficeError::unsupported(
                UnsupportedCode::TableStyle,
                format!(
                    "table style {} cannot be resolved because ppt/tableStyles.xml is missing",
                    table.style_id.as_deref().unwrap_or_default()
                ),
            )),
        }
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

fn effective_color_map(master: &Slide, layout: &Slide, slide: &Slide) -> HashMap<String, String> {
    let mut mapping = master.color_map.clone();
    for override_map in [&layout.color_map_override, &slide.color_map_override] {
        if let ColorMapOverride::Override(values) = override_map {
            mapping.extend(
                values
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
        }
    }
    mapping
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

type ImageErrors = HashMap<String, (UnsupportedCode, String)>;
type CroppedImageMap = HashMap<(String, CropRect), usize>;
type CroppedImageErrors = HashMap<(String, CropRect), (UnsupportedCode, String)>;

fn decode_images(pkg: &Package) -> (Vec<EncodedImage>, HashMap<String, usize>, ImageErrors, u64) {
    let mut images = Vec::new();
    let mut by_part = HashMap::new();
    let mut errors = HashMap::new();
    let names: Vec<String> = pkg.part_names().map(str::to_string).collect();
    let mut total_pixels = 0u64;
    for name in names {
        let Some(ct) = pkg.content_type(&name) else {
            continue;
        };
        if !ct.starts_with("image/") {
            continue;
        }
        let Some(bytes) = pkg.part(&name) else {
            errors.insert(
                name,
                (
                    UnsupportedCode::MissingRelationshipTarget,
                    "image part is missing".to_string(),
                ),
            );
            continue;
        };
        match decode_required(ct, bytes)
            .and_then(|decoded| {
                add_to_budget(images.len(), &mut total_pixels, decoded.1)?;
                Ok(decoded)
            })
            .map_err(|error| error.context(&name))
        {
            Ok((decoded, _)) => {
                by_part.insert(name.clone(), images.len());
                images.push(decoded);
            }
            Err(OfficeError::Unsupported { code, detail }) => {
                errors.insert(name, (code, detail));
            }
            Err(error) => {
                errors.insert(
                    name,
                    (UnsupportedCode::ImageDecodeFailure, error.to_string()),
                );
            }
        }
    }
    (images, by_part, errors, total_pixels)
}

fn crop_key(crop: CropRect) -> (i32, i32, i32, i32) {
    (crop.left, crop.top, crop.right, crop.bottom)
}

fn collect_slide_crops(
    slide: &Slide,
    rels: &HashMap<String, String>,
    requests: &mut Vec<(String, CropRect)>,
) {
    if let Some(fill) = &slide.background {
        collect_fill_crop(fill, rels, requests);
    }
    collect_shape_crops(&slide.shapes, rels, requests);
}

fn collect_shape_crops(
    shapes: &[ShapeElem],
    rels: &HashMap<String, String>,
    requests: &mut Vec<(String, CropRect)>,
) {
    for shape in shapes {
        match shape {
            ShapeElem::Shape { props, .. } => {
                if let Some(fill) = &props.fill {
                    collect_fill_crop(fill, rels, requests);
                }
            }
            ShapeElem::Picture { drawing, .. } => {
                if let (Some(rel_id), Some(crop)) = (&drawing.blip_rel_id, drawing.crop)
                    && let Some(target) = rels.get(rel_id)
                {
                    requests.push((target.clone(), crop));
                }
            }
            ShapeElem::Group { children, .. } => collect_shape_crops(children, rels, requests),
            ShapeElem::Table { table, .. } => {
                if let Some(fill) = &table.fill {
                    collect_fill_crop(fill, rels, requests);
                }
                for cell in table.rows.iter().flat_map(|row| &row.cells) {
                    if let Some(fill) = &cell.fill {
                        collect_fill_crop(fill, rels, requests);
                    }
                }
            }
            ShapeElem::Chart { .. } => {}
            ShapeElem::SmartArt { .. } => {}
            ShapeElem::OlePreview { .. } => {}
            ShapeElem::Unsupported(_) => {}
        }
    }
}

fn collect_fill_crop(
    fill: &Fill,
    rels: &HashMap<String, String>,
    requests: &mut Vec<(String, CropRect)>,
) {
    if let Fill::Picture(picture) = fill
        && let Some(crop) = picture.crop
        && let Some(target) = rels.get(&picture.rel_id)
    {
        requests.push((target.clone(), crop));
    }
}

fn decode_crops(
    pkg: &Package,
    requests: &[(String, CropRect)],
    image_errors: &ImageErrors,
    images: &mut Vec<EncodedImage>,
    total_pixels: &mut u64,
) -> (CroppedImageMap, CroppedImageErrors) {
    let mut cropped = HashMap::new();
    let mut errors = HashMap::new();
    for (part, crop) in requests {
        let key = (part.clone(), *crop);
        if let Err(OfficeError::Unsupported { code, detail }) =
            validate_crop_units([crop.left, crop.top, crop.right, crop.bottom])
        {
            errors.insert(key, (code, format!("image {part}: {detail}")));
            continue;
        }
        if let Some(error) = image_errors.get(part) {
            errors.insert(key, error.clone());
            continue;
        }
        let Some(bytes) = pkg.part(part) else {
            errors.insert(
                key,
                (
                    UnsupportedCode::MissingRelationshipTarget,
                    format!("image relationship target {part} is missing"),
                ),
            );
            continue;
        };
        let Ok(image) = image::load_from_memory(bytes) else {
            errors.insert(
                key,
                (
                    UnsupportedCode::ImageDecodeFailure,
                    format!("image {part} cannot be decoded for cropping"),
                ),
            );
            continue;
        };
        let width = image.width();
        let height = image.height();
        let left = (u64::from(width) * crop.left as u64 / 100_000) as u32;
        let top = (u64::from(height) * crop.top as u64 / 100_000) as u32;
        let right = (u64::from(width) * crop.right as u64 / 100_000) as u32;
        let bottom = (u64::from(height) * crop.bottom as u64 / 100_000) as u32;
        let crop_width = width.saturating_sub(left).saturating_sub(right);
        let crop_height = height.saturating_sub(top).saturating_sub(bottom);
        if crop_width == 0 || crop_height == 0 {
            errors.insert(
                key,
                (
                    UnsupportedCode::InvalidImageCrop,
                    format!("a:srcRect removes entire image {part}"),
                ),
            );
            continue;
        }
        let crop_pixels = u64::from(crop_width) * u64::from(crop_height);
        if let Err(OfficeError::Unsupported { code, detail }) =
            add_to_budget(images.len(), total_pixels, crop_pixels)
        {
            errors.insert(key, (code, detail));
            continue;
        }
        let data = image
            .crop_imm(left, top, crop_width, crop_height)
            .into_rgba8()
            .into_raw();
        cropped.insert(key, images.len());
        images.push(EncodedImage::Rgba8 {
            width: crop_width,
            height: crop_height,
            data,
        });
    }
    (cropped, errors)
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

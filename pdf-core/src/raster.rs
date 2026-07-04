//! Zone 2 — rasterization: page thumbnails and previews via pdfium.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, RgbImage};
use pdfium_render::prelude::*;

use crate::error::{PdfCoreError, Result};

/// Output encoding for rasterized pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    #[default]
    Png,
    /// quality 1..=100
    Jpeg(u8),
}

/// Raster options. Exactly one sizing mode applies: `target_width_px` wins if
/// set, otherwise `scale` (1.0 = 72 dpi).
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub target_width_px: Option<u32>,
    pub scale: f64,
    pub format: ImageFormat,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            target_width_px: None,
            scale: 1.0,
            format: ImageFormat::Png,
        }
    }
}

/// Rasterizes a single page (0-based index) to encoded image bytes.
pub fn render_page(pdf: &[u8], page_index: u32, opts: &RenderOptions) -> Result<Vec<u8>> {
    let config = build_config(opts)?;
    let pdfium = crate::pdfium::lock()?;
    let doc = pdfium.load_pdf_from_byte_slice(pdf, None)?;
    let index = validate_page_index(page_index, doc.pages().len())?;
    let page = doc.pages().get(index)?;
    let bitmap = page.render_with_config(&config)?;
    let img = bitmap.as_image()?;
    encode_image(&img, opts.format)
}

/// Rasterizes pages (0-based; `None` = all pages) to thumbnails no wider than
/// `max_width_px`, PNG-encoded, in page order.
pub fn render_thumbnails(
    pdf: &[u8],
    pages: Option<&[u32]>,
    max_width_px: u32,
) -> Result<Vec<Vec<u8>>> {
    if max_width_px == 0 {
        return Err(PdfCoreError::InvalidInput(
            "max_width_px must be at least 1".to_string(),
        ));
    }
    let pdfium = crate::pdfium::lock()?;
    let doc = pdfium.load_pdf_from_byte_slice(pdf, None)?;
    let page_count = doc.pages().len();

    let indices: Vec<PdfPageIndex> = match pages {
        Some(list) => list
            .iter()
            .map(|&i| validate_page_index(i, page_count))
            .collect::<Result<_>>()?,
        None => (0..page_count).collect(),
    };

    let mut out = Vec::with_capacity(indices.len());
    for index in indices {
        let page = doc.pages().get(index)?;
        // Natural width in px at 72 dpi == page width in points. Never
        // upscale a page narrower than the requested thumbnail width.
        let natural_width_px = page.width().value.round().max(1.0) as u32;
        let target_width = max_width_px.min(natural_width_px);
        let target_width = i32::try_from(target_width).map_err(|_| {
            PdfCoreError::InvalidInput(format!("thumbnail width {target_width} is too large"))
        })?;
        let config = PdfRenderConfig::new().set_target_width(target_width);
        let bitmap = page.render_with_config(&config)?;
        let img = bitmap.as_image()?;
        out.push(encode_image(&img, ImageFormat::Png)?);
    }
    Ok(out)
}

/// Validates a 0-based page index against the document's page count,
/// converting it to pdfium's index type.
fn validate_page_index(page_index: u32, page_count: PdfPageIndex) -> Result<PdfPageIndex> {
    let index = PdfPageIndex::try_from(page_index).map_err(|_| {
        PdfCoreError::InvalidInput(format!("page index {page_index} exceeds supported range"))
    })?;
    if index < 0 || index >= page_count {
        return Err(PdfCoreError::InvalidInput(format!(
            "page index {page_index} out of range (document has {page_count} pages)"
        )));
    }
    Ok(index)
}

/// Builds the pdfium render config from options: `target_width_px` wins if
/// set, otherwise `scale` is applied to both dimensions.
fn build_config(opts: &RenderOptions) -> Result<PdfRenderConfig> {
    if let Some(width) = opts.target_width_px {
        if width == 0 {
            return Err(PdfCoreError::InvalidInput(
                "target_width_px must be at least 1".to_string(),
            ));
        }
        let width = i32::try_from(width).map_err(|_| {
            PdfCoreError::InvalidInput(format!("target_width_px {width} is too large"))
        })?;
        Ok(PdfRenderConfig::new().set_target_width(width))
    } else {
        if !opts.scale.is_finite() || opts.scale <= 0.0 {
            return Err(PdfCoreError::InvalidInput(format!(
                "scale must be a finite positive number, got {}",
                opts.scale
            )));
        }
        Ok(PdfRenderConfig::new().scale_page_by_factor(opts.scale as f32))
    }
}

/// Encodes a rendered page image as PNG or JPEG bytes.
fn encode_image(img: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match format {
        ImageFormat::Png => {
            img.write_with_encoder(PngEncoder::new(Cursor::new(&mut buf)))?;
        }
        ImageFormat::Jpeg(quality) => {
            let quality = quality.clamp(1, 100);
            let rgb = flatten_over_white(img);
            rgb.write_with_encoder(JpegEncoder::new_with_quality(
                Cursor::new(&mut buf),
                quality,
            ))?;
        }
    }
    Ok(buf)
}

/// Converts any image to RGB8, compositing alpha over a white background
/// (JPEG has no alpha channel).
fn flatten_over_white(img: &DynamicImage) -> RgbImage {
    let rgba = img.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (dst, src) in rgb.pixels_mut().zip(rgba.pixels()) {
        let alpha = u32::from(src[3]);
        for channel in 0..3 {
            let blended = (u32::from(src[channel]) * alpha + 255 * (255 - alpha) + 127) / 255;
            dst[channel] = blended.min(255) as u8;
        }
    }
    rgb
}

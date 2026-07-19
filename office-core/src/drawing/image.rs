//! Shared fail-closed raster validation and decoding for OOXML image parts.

use crate::layout::EncodedImage;
use crate::oxml::dml::{Drawing, DrawingKind};
use crate::{OfficeError, Result, UnsupportedCode};

/// Alpha/background decision included in every image cache key. Task 1 only
/// ships the evidence-backed preserve path; flattening remains unproved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AlphaBackground {
    /// Preserve source alpha as a PDF soft mask.
    Preserve,
}

/// Every authored state that can change a Word image export. Keeping these
/// fields in the key prevents a shared source part from accidentally reusing a
/// variant produced for a different placement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageVariantKey {
    /// Resolved OPC source part.
    pub source_part: String,
    /// Inline or anchored placement.
    pub disposition: DrawingKind,
    /// Authored display extent in exact EMU.
    pub extent_emu: Option<(i64, i64)>,
    /// Authored source clip in 1/1000-percent units.
    pub crop_units: Option<[i32; 4]>,
    /// Effective image-space clip after the authored crop is resolved.
    pub effective_clip_units: Option<[i32; 4]>,
    /// Authored rotation in 1/60000-degree units.
    pub rotation_units: i32,
    /// Alpha/background handling selected for the variant.
    pub alpha_background: AlphaBackground,
    /// Authored DrawingML compression state.
    pub compression_state: Option<String>,
    /// Authored Microsoft local-DPI switch.
    pub use_local_dpi: Option<bool>,
}

impl ImageVariantKey {
    /// Builds the complete key for a resolved source part and drawing.
    pub fn from_drawing(source_part: &str, drawing: &Drawing) -> Self {
        Self {
            source_part: source_part.to_owned(),
            disposition: drawing.kind,
            extent_emu: drawing.extent_emu,
            crop_units: drawing.crop_units,
            effective_clip_units: drawing.crop_units,
            rotation_units: drawing.rotation_units,
            alpha_background: AlphaBackground::Preserve,
            compression_state: drawing.compression_state.clone(),
            use_local_dpi: drawing.use_local_dpi,
        }
    }
}

/// Produces the evidence-backed Word export variant. JPEG sources are already
/// compressed and transparent PNGs have no proved flattening background, so
/// both remain unchanged. Opaque PNGs carrying `useLocalDpi=0` use Word's
/// baseline quality-75 4:2:0 path at source dimensions.
pub fn export_variant(
    source: &EncodedImage,
    key: &ImageVariantKey,
) -> Result<Option<EncodedImage>> {
    let EncodedImage::Rgb8 {
        width,
        height,
        data,
    } = source
    else {
        return Ok(None);
    };
    if key.use_local_dpi != Some(false) {
        return Ok(None);
    }
    let data = super::jpeg::encode_word_quality_75_420(data, *width, *height)?;
    Ok(Some(EncodedImage::Jpeg {
        data,
        width: *width,
        height: *height,
    }))
}

#[cfg(feature = "fidelity-diagnostics")]
/// Encodes diagnostic RGB candidates through the exact production Word path.
pub fn diagnostics_encode_word_jpeg(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    super::jpeg::encode_word_quality_75_420(rgb, width, height)
}

/// Maximum number of decoded image resources in one document.
pub const MAX_IMAGES: usize = 10_000;
/// Maximum decoded pixels in one image.
pub const MAX_IMAGE_PIXELS: u64 = 50_000_000;
/// Maximum decoded pixels across all image resources in one document.
pub const MAX_TOTAL_IMAGE_PIXELS: u64 = 200_000_000;

/// Decodes a required PNG/JPEG part after validating its declared and actual
/// encodings, full byte stream, dimensions, and per-image allocation.
pub fn decode_required(content_type: &str, bytes: &[u8]) -> Result<(EncodedImage, u64)> {
    let declared = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let expected = match declared.as_str() {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" | "image/jpg" => image::ImageFormat::Jpeg,
        _ => {
            return Err(OfficeError::unsupported(
                UnsupportedCode::UnsupportedImageEncoding,
                format!("unsupported image content type {content_type}"),
            ));
        }
    };

    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| {
            OfficeError::unsupported(
                UnsupportedCode::ImageDecodeFailure,
                format!("image format detection failed: {error}"),
            )
        })?;
    let actual = reader.format().ok_or_else(|| {
        OfficeError::unsupported(
            UnsupportedCode::ImageDecodeFailure,
            "image bytes have no recognized encoding",
        )
    })?;
    if !matches!(actual, image::ImageFormat::Png | image::ImageFormat::Jpeg) || actual != expected {
        return Err(OfficeError::unsupported(
            UnsupportedCode::UnsupportedImageEncoding,
            format!("declared {declared}, detected {actual:?}"),
        ));
    }

    let (width, height) = reader.into_dimensions().map_err(|error| {
        OfficeError::unsupported(
            UnsupportedCode::ImageDecodeFailure,
            format!("image dimensions cannot be decoded: {error}"),
        )
    })?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(image_dimensions_error)?;
    let allocation = pixels.checked_mul(4).ok_or_else(image_dimensions_error)?;
    if width == 0 || height == 0 || pixels > MAX_IMAGE_PIXELS || allocation > MAX_IMAGE_PIXELS * 4 {
        return Err(image_dimensions_error());
    }

    let decoded = image::load_from_memory_with_format(bytes, expected).map_err(|error| {
        OfficeError::unsupported(
            UnsupportedCode::ImageDecodeFailure,
            format!("image bytes cannot be decoded: {error}"),
        )
    })?;
    if decoded.width() != width || decoded.height() != height {
        return Err(OfficeError::unsupported(
            UnsupportedCode::ImageDecodeFailure,
            "decoded image dimensions changed during full decode",
        ));
    }

    let encoded = if expected == image::ImageFormat::Jpeg {
        EncodedImage::Jpeg {
            data: bytes.to_vec(),
            width,
            height,
        }
    } else if decoded.color().has_alpha() {
        EncodedImage::Rgba8 {
            width,
            height,
            data: decoded.into_rgba8().into_raw(),
        }
    } else {
        EncodedImage::Rgb8 {
            width,
            height,
            data: decoded.into_rgb8().into_raw(),
        }
    };
    Ok((encoded, pixels))
}

/// Adds one decoded resource to the document-wide count and pixel budget.
pub fn add_to_budget(image_count: usize, total_pixels: &mut u64, pixels: u64) -> Result<()> {
    if image_count >= MAX_IMAGES {
        return Err(OfficeError::unsupported(
            UnsupportedCode::ImageAllocationExceedsLimit,
            format!("image count exceeds limit {MAX_IMAGES}"),
        ));
    }
    *total_pixels = total_pixels.checked_add(pixels).ok_or_else(|| {
        OfficeError::unsupported(
            UnsupportedCode::ImageAllocationExceedsLimit,
            "total decoded image pixels overflow",
        )
    })?;
    if *total_pixels > MAX_TOTAL_IMAGE_PIXELS {
        return Err(OfficeError::unsupported(
            UnsupportedCode::ImageAllocationExceedsLimit,
            format!(
                "total decoded image pixels {} exceed limit {MAX_TOTAL_IMAGE_PIXELS}",
                *total_pixels
            ),
        ));
    }
    Ok(())
}

/// Validates DrawingML crop fractions without clamping malformed input.
pub fn validate_crop_fractions(crop: [f64; 4]) -> Result<()> {
    if crop
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        || crop[0] + crop[2] >= 1.0
        || crop[1] + crop[3] >= 1.0
    {
        return Err(OfficeError::unsupported(
            UnsupportedCode::InvalidImageCrop,
            "a:srcRect is malformed or removes the entire image",
        ));
    }
    Ok(())
}

/// Validates DrawingML crop values in 1/1000-percent integer units.
pub fn validate_crop_units(crop: [i32; 4]) -> Result<()> {
    if crop.iter().any(|value| !(0..=100_000).contains(value))
        || i64::from(crop[0]) + i64::from(crop[2]) >= 100_000
        || i64::from(crop[1]) + i64::from(crop[3]) >= 100_000
    {
        return Err(OfficeError::unsupported(
            UnsupportedCode::InvalidImageCrop,
            "a:srcRect is malformed or removes the entire image",
        ));
    }
    Ok(())
}

fn image_dimensions_error() -> OfficeError {
    OfficeError::unsupported(
        UnsupportedCode::ImageDimensionsExceedLimit,
        format!("decoded image dimensions exceed {MAX_IMAGE_PIXELS} pixels"),
    )
}

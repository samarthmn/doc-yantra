//! Shared fail-closed raster validation and decoding for OOXML image parts.

use crate::layout::EncodedImage;
use crate::{OfficeError, Result, UnsupportedCode};

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

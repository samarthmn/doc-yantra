//! Shared fail-closed raster validation and decoding for OOXML image parts.

use crate::layout::EncodedImage;
use crate::oxml::dml::Drawing;
use crate::{OfficeError, Result, UnsupportedCode};

/// The fields that can change encoded image bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageEncodingKey {
    /// Resolved OPC source part.
    pub source_part: String,
    /// Authored Microsoft local-DPI switch.
    pub use_local_dpi: Option<bool>,
}

impl ImageEncodingKey {
    /// Builds an encoding-only key; placement, crop, rotation, wrapping, and
    /// disposition remain layout state because they do not change export bytes.
    pub fn from_drawing(source_part: &str, drawing: &Drawing) -> Self {
        Self {
            source_part: source_part.to_owned(),
            use_local_dpi: drawing.use_local_dpi,
        }
    }
}

// Together these caps bound image-owned memory to roughly 576 MiB (128 MiB
// encoded + 192 MiB decoded + 256 MiB transient), materially below the prior
// pixel-only path's approximate 1.6 GiB peak while preserving the corpus.
const MAX_IMAGE_SOURCES: usize = 4_096;
const MAX_IMAGE_VARIANTS: usize = 4_096;
const MAX_RESIDENT_ENCODED_BYTES: usize = 128 * 1024 * 1024;
const MAX_RESIDENT_DECODED_BYTES: usize = 192 * 1024 * 1024;
const MAX_TRANSIENT_IMAGE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageBudgetLimits {
    max_sources: usize,
    max_variants: usize,
    max_resident_encoded_bytes: usize,
    max_resident_decoded_bytes: usize,
    max_transient_bytes: usize,
}

impl ImageBudgetLimits {
    const fn production() -> Self {
        Self {
            max_sources: MAX_IMAGE_SOURCES,
            max_variants: MAX_IMAGE_VARIANTS,
            max_resident_encoded_bytes: MAX_RESIDENT_ENCODED_BYTES,
            max_resident_decoded_bytes: MAX_RESIDENT_DECODED_BYTES,
            max_transient_bytes: MAX_TRANSIENT_IMAGE_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ImageReservation {
    resident_encoded_bytes: usize,
    resident_decoded_bytes: usize,
    transient_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ImageResidentBytes {
    encoded_bytes: usize,
    decoded_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ImageBudget {
    limits: ImageBudgetLimits,
    source_count: usize,
    variant_count: usize,
    resident_encoded_bytes: usize,
    resident_decoded_bytes: usize,
    transient_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
enum BudgetKind {
    Source,
    Variant,
}

impl ImageBudget {
    pub(crate) const fn with_limits(limits: ImageBudgetLimits) -> Self {
        Self {
            limits,
            source_count: 0,
            variant_count: 0,
            resident_encoded_bytes: 0,
            resident_decoded_bytes: 0,
            transient_bytes: 0,
        }
    }

    pub(crate) const fn production() -> Self {
        Self::with_limits(ImageBudgetLimits::production())
    }

    pub(crate) fn ensure_variant_count(&self, count: usize) -> Result<()> {
        if count > self.limits.max_variants {
            return Err(allocation_error(format!(
                "image variant count {count} exceeds limit {}",
                self.limits.max_variants
            )));
        }
        Ok(())
    }

    pub(crate) fn admit_source<T>(
        &mut self,
        reservation: ImageReservation,
        operation: impl FnOnce() -> Result<(T, ImageResidentBytes)>,
    ) -> Result<T> {
        self.admit(BudgetKind::Source, reservation, operation)
    }

    pub(crate) fn admit_variant<T>(
        &mut self,
        reservation: ImageReservation,
        operation: impl FnOnce() -> Result<(T, ImageResidentBytes)>,
    ) -> Result<T> {
        self.admit(BudgetKind::Variant, reservation, operation)
    }

    pub(crate) fn admit_variant_estimate(&mut self, reservation: ImageReservation) -> Result<()> {
        let resident = ImageResidentBytes {
            encoded_bytes: reservation.resident_encoded_bytes,
            decoded_bytes: reservation.resident_decoded_bytes,
        };
        self.admit_variant(reservation, || Ok(((), resident)))
    }

    pub(crate) fn release_resident(&mut self, resident: ImageResidentBytes) {
        self.resident_encoded_bytes = self
            .resident_encoded_bytes
            .saturating_sub(resident.encoded_bytes);
        self.resident_decoded_bytes = self
            .resident_decoded_bytes
            .saturating_sub(resident.decoded_bytes);
    }

    fn admit<T>(
        &mut self,
        kind: BudgetKind,
        reservation: ImageReservation,
        operation: impl FnOnce() -> Result<(T, ImageResidentBytes)>,
    ) -> Result<T> {
        let (count, count_limit, label) = match kind {
            BudgetKind::Source => (self.source_count, self.limits.max_sources, "source"),
            BudgetKind::Variant => (self.variant_count, self.limits.max_variants, "variant"),
        };
        let next_count = image_checked_add(&format!("image {label} count"), count, 1)?;
        if next_count > count_limit {
            return Err(allocation_error(format!(
                "image {label} count {next_count} exceeds limit {count_limit}"
            )));
        }
        self.check_resident(
            reservation.resident_encoded_bytes,
            reservation.resident_decoded_bytes,
        )?;
        let next_transient = image_checked_add(
            "transient image bytes",
            self.transient_bytes,
            reservation.transient_bytes,
        )?;
        if next_transient > self.limits.max_transient_bytes {
            return Err(allocation_error(format!(
                "transient image bytes {next_transient} exceeds limit {}",
                self.limits.max_transient_bytes
            )));
        }

        let prior_transient = self.transient_bytes;
        self.transient_bytes = next_transient;
        let result = operation();
        self.transient_bytes = prior_transient;
        let (value, actual) = result?;
        let (next_encoded, next_decoded) =
            self.check_resident(actual.encoded_bytes, actual.decoded_bytes)?;
        match kind {
            BudgetKind::Source => self.source_count = next_count,
            BudgetKind::Variant => self.variant_count = next_count,
        }
        self.resident_encoded_bytes = next_encoded;
        self.resident_decoded_bytes = next_decoded;
        Ok(value)
    }

    fn check_resident(&self, encoded: usize, decoded: usize) -> Result<(usize, usize)> {
        let next_encoded = image_checked_add(
            "resident encoded image bytes",
            self.resident_encoded_bytes,
            encoded,
        )?;
        let next_decoded = image_checked_add(
            "resident decoded image bytes",
            self.resident_decoded_bytes,
            decoded,
        )?;
        if next_encoded > self.limits.max_resident_encoded_bytes {
            return Err(allocation_error(format!(
                "resident encoded image bytes {next_encoded} exceeds limit {}",
                self.limits.max_resident_encoded_bytes
            )));
        }
        if next_decoded > self.limits.max_resident_decoded_bytes {
            return Err(allocation_error(format!(
                "resident decoded image bytes {next_decoded} exceeds limit {}",
                self.limits.max_resident_decoded_bytes
            )));
        }
        Ok((next_encoded, next_decoded))
    }
}

fn image_checked_add(label: &str, left: usize, right: usize) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| allocation_error(format!("{label} overflow")))
}

fn allocation_error(detail: impl Into<String>) -> OfficeError {
    OfficeError::unsupported(UnsupportedCode::ImageAllocationExceedsLimit, detail)
}

/// Produces the evidence-backed Word export variant. JPEG sources are already
/// compressed and transparent PNGs have no proved flattening background, so
/// both remain unchanged. Opaque PNGs carrying `useLocalDpi=0` use Word's
/// baseline quality-75 4:2:0 path at source dimensions.
pub(crate) fn export_variant(
    source: &EncodedImage,
    key: &ImageEncodingKey,
    budget: &mut ImageBudget,
) -> Result<Option<EncodedImage>> {
    let reservation = ImageSummary::from_image(source).variant_reservation(key)?;
    budget.admit_variant(reservation, || {
        let EncodedImage::Rgb8 {
            width,
            height,
            data,
        } = source
        else {
            return Ok((None, ImageResidentBytes::default()));
        };
        if key.use_local_dpi != Some(false) {
            return Ok((None, ImageResidentBytes::default()));
        }
        let data = super::jpeg::encode_word_quality_75_420(data, *width, *height)?;
        let resident = ImageResidentBytes {
            encoded_bytes: data.len(),
            decoded_bytes: 0,
        };
        Ok((
            Some(EncodedImage::Jpeg {
                data: std::sync::Arc::new(data),
                width: *width,
                height: *height,
            }),
            resident,
        ))
    })
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageSummary {
    width: u32,
    height: u32,
    rgb8: bool,
}

impl ImageSummary {
    pub(crate) fn from_image(image: &EncodedImage) -> Self {
        match image {
            EncodedImage::Jpeg { width, height, .. }
            | EncodedImage::Rgba8 { width, height, .. } => Self {
                width: *width,
                height: *height,
                rgb8: false,
            },
            EncodedImage::Rgb8 { width, height, .. } => Self {
                width: *width,
                height: *height,
                rgb8: true,
            },
        }
    }

    pub(crate) fn variant_reservation(self, key: &ImageEncodingKey) -> Result<ImageReservation> {
        if !self.rgb8 || key.use_local_dpi != Some(false) {
            return Ok(ImageReservation::default());
        }
        let pixels = image_bytes_mul(self.width, self.height, 1, "JPEG export pixels")?;
        let upper_bound = image_checked_add(
            "JPEG export upper bound",
            pixels
                .checked_mul(4)
                .ok_or_else(|| allocation_error("JPEG export byte estimate overflow"))?,
            64 * 1024,
        )?;
        Ok(ImageReservation {
            resident_encoded_bytes: upper_bound,
            resident_decoded_bytes: 0,
            transient_bytes: upper_bound,
        })
    }
}

pub(crate) fn image_resident_bytes(image: &EncodedImage) -> ImageResidentBytes {
    match image {
        EncodedImage::Jpeg { data, .. } => ImageResidentBytes {
            encoded_bytes: data.len(),
            decoded_bytes: 0,
        },
        EncodedImage::Rgb8 { data, .. } | EncodedImage::Rgba8 { data, .. } => ImageResidentBytes {
            encoded_bytes: 0,
            decoded_bytes: data.len(),
        },
    }
}

pub(crate) fn rgba_variant_reservation(
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
) -> Result<ImageReservation> {
    Ok(ImageReservation {
        resident_encoded_bytes: 0,
        resident_decoded_bytes: image_bytes_mul(
            output_width,
            output_height,
            4,
            "cropped image bytes",
        )?,
        transient_bytes: image_bytes_mul(source_width, source_height, 4, "crop decode work bytes")?,
    })
}

#[cfg(feature = "fidelity-diagnostics")]
/// Encodes diagnostic RGB candidates through the exact production Word path.
pub fn diagnostics_encode_word_jpeg(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    super::jpeg::encode_word_quality_75_420(rgb, width, height)
}

/// Maximum decoded pixels in one image.
pub const MAX_IMAGE_PIXELS: u64 = 50_000_000;

/// Decodes a required PNG/JPEG part after validating its declared and actual
/// encodings, full byte stream, dimensions, and per-image allocation.
pub fn decode_required(content_type: &str, bytes: &[u8]) -> Result<(EncodedImage, u64)> {
    let mut budget = ImageBudget::production();
    decode_required_with_budget(content_type, bytes, &mut budget)
}

pub(crate) fn decode_required_with_budget(
    content_type: &str,
    bytes: &[u8],
    budget: &mut ImageBudget,
) -> Result<(EncodedImage, u64)> {
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

    let decoded_upper_bound = usize::try_from(allocation).map_err(|_| image_dimensions_error())?;
    let reservation = if expected == image::ImageFormat::Jpeg {
        ImageReservation {
            resident_encoded_bytes: bytes.len(),
            resident_decoded_bytes: 0,
            transient_bytes: decoded_upper_bound,
        }
    } else {
        ImageReservation {
            resident_encoded_bytes: 0,
            resident_decoded_bytes: decoded_upper_bound,
            transient_bytes: decoded_upper_bound,
        }
    };
    budget.admit_source(reservation, || {
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
            drop(decoded);
            let mut data = Vec::new();
            data.try_reserve_exact(bytes.len()).map_err(|_| {
                allocation_error(format!(
                    "JPEG allocation for {} bytes was refused",
                    bytes.len()
                ))
            })?;
            data.extend_from_slice(bytes);
            EncodedImage::Jpeg {
                data: std::sync::Arc::new(data),
                width,
                height,
            }
        } else if decoded.color().has_alpha() {
            EncodedImage::Rgba8 {
                width,
                height,
                data: std::sync::Arc::new(decoded.into_rgba8().into_raw()),
            }
        } else {
            EncodedImage::Rgb8 {
                width,
                height,
                data: std::sync::Arc::new(decoded.into_rgb8().into_raw()),
            }
        };
        let resident = image_resident_bytes(&encoded);
        Ok(((encoded, pixels), resident))
    })
}

fn image_bytes_mul(width: u32, height: u32, channels: usize, label: &str) -> Result<usize> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| allocation_error(format!("{label} overflow")))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oxml::dml::DrawingKind;
    use std::cell::Cell;

    fn limits() -> ImageBudgetLimits {
        ImageBudgetLimits {
            max_sources: 1,
            max_variants: 1,
            max_resident_encoded_bytes: 5,
            max_resident_decoded_bytes: 7,
            max_transient_bytes: 9,
        }
    }

    fn reservation(encoded: usize, decoded: usize, transient: usize) -> ImageReservation {
        ImageReservation {
            resident_encoded_bytes: encoded,
            resident_decoded_bytes: decoded,
            transient_bytes: transient,
        }
    }

    fn actual(encoded: usize, decoded: usize) -> ImageResidentBytes {
        ImageResidentBytes {
            encoded_bytes: encoded,
            decoded_bytes: decoded,
        }
    }

    fn assert_allocation_limit(error: OfficeError) {
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ImageAllocationExceedsLimit,
                ..
            }
        ));
    }

    #[test]
    fn image_budget_accepts_exact_count_byte_and_transient_boundaries() {
        let mut budget = ImageBudget::with_limits(limits());

        budget
            .admit_source(reservation(2, 3, 9), || Ok(((), actual(2, 3))))
            .unwrap();
        budget
            .admit_variant(reservation(3, 4, 0), || Ok(((), actual(3, 4))))
            .unwrap();

        assert_eq!(budget.source_count, 1);
        assert_eq!(budget.variant_count, 1);
        assert_eq!(budget.resident_encoded_bytes, 5);
        assert_eq!(budget.resident_decoded_bytes, 7);
        assert_eq!(budget.transient_bytes, 0);
    }

    #[test]
    fn image_budget_refuses_source_and_variant_count_overflow() {
        let mut budget = ImageBudget::with_limits(limits());
        budget
            .admit_source(reservation(0, 0, 0), || Ok(((), actual(0, 0))))
            .unwrap();
        budget
            .admit_variant(reservation(0, 0, 0), || Ok(((), actual(0, 0))))
            .unwrap();

        let source = budget
            .admit_source(reservation(0, 0, 0), || Ok(((), actual(0, 0))))
            .expect_err("source count limit");
        let variant = budget
            .admit_variant(reservation(0, 0, 0), || Ok(((), actual(0, 0))))
            .expect_err("variant count limit");

        assert_allocation_limit(source);
        assert_allocation_limit(variant);
    }

    #[test]
    fn image_budget_refuses_resident_byte_excess_and_checked_overflow() {
        let mut encoded = ImageBudget::with_limits(limits());
        let encoded_error = encoded
            .admit_source(reservation(6, 0, 0), || Ok(((), actual(6, 0))))
            .expect_err("encoded byte limit");
        assert_allocation_limit(encoded_error);

        let mut decoded = ImageBudget::with_limits(limits());
        let decoded_error = decoded
            .admit_source(reservation(0, 8, 0), || Ok(((), actual(0, 8))))
            .expect_err("decoded byte limit");
        assert_allocation_limit(decoded_error);

        let mut overflow = ImageBudget::with_limits(ImageBudgetLimits {
            max_sources: 2,
            max_resident_encoded_bytes: usize::MAX,
            ..limits()
        });
        overflow.resident_encoded_bytes = usize::MAX;
        let overflow_error = overflow
            .admit_source(reservation(1, 0, 0), || Ok(((), actual(1, 0))))
            .expect_err("checked resident byte overflow");
        assert_allocation_limit(overflow_error);
    }

    #[test]
    fn image_budget_refuses_transient_overflow_without_invoking_callback() {
        let mut budget = ImageBudget::with_limits(limits());
        let invoked = Cell::new(false);

        let error = budget
            .admit_source(reservation(0, 0, 10), || {
                invoked.set(true);
                Ok(((), actual(0, 0)))
            })
            .expect_err("transient limit");

        assert_allocation_limit(error);
        assert!(!invoked.get());
        assert_eq!(budget.source_count, 0);
        assert_eq!(budget.transient_bytes, 0);
    }

    fn drawing(use_local_dpi: Option<bool>) -> Drawing {
        Drawing {
            kind: DrawingKind::Inline,
            extent_pt: Some((100.0, 50.0)),
            extent_emu: Some((1_270_000, 635_000)),
            effect_extent_pt: [0.0; 4],
            blip_rel_id: Some("rId1".to_string()),
            graphic_rel_id: None,
            cropped: false,
            crop_units: None,
            rotation_units: 0,
            compression_state: None,
            use_local_dpi,
            graphic_uri: None,
        }
    }

    #[test]
    fn encoding_key_ignores_placement_crop_rotation_and_wrapping_state() {
        let first = drawing(Some(false));
        let mut second = drawing(Some(false));
        second.kind = DrawingKind::Anchor;
        second.extent_emu = Some((9, 11));
        second.crop_units = Some([1, 2, 3, 4]);
        second.rotation_units = 5_400_000;
        second.compression_state = Some("print".to_string());

        assert_eq!(
            ImageEncodingKey::from_drawing("word/media/image1.png", &first),
            ImageEncodingKey::from_drawing("word/media/image1.png", &second)
        );
    }

    #[test]
    fn encoding_key_distinguishes_use_local_dpi() {
        assert_ne!(
            ImageEncodingKey::from_drawing("word/media/image1.png", &drawing(None)),
            ImageEncodingKey::from_drawing("word/media/image1.png", &drawing(Some(false)))
        );
    }
}

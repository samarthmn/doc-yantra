//! Image-XObject extraction and side-by-side metadata reporting.

use anyhow::{Context, Result};
use fast_image_resize::{
    FilterType as FastFilter, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image,
};
use image::{DynamicImage, ImageBuffer, Rgb, RgbImage};
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use serde::Serialize;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use crate::probe::{ImageXObjectRecord, extract_content_page_with_bytes};

#[derive(Debug, Serialize)]
pub struct ImageProbeReport {
    pub page: u32,
    pub ours: Vec<ExtractedImageReport>,
    pub truth: Vec<ExtractedImageReport>,
    pub comparisons: Vec<ImagePairComparison>,
}

#[derive(Debug, Serialize)]
pub struct ExtractedImageReport {
    #[serde(flatten)]
    pub image: ImageXObjectRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct ImagePairComparison {
    pub index: usize,
    pub metadata_parity: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_absolute_error: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_error: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_squared_error: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct ImageCandidateMatrixReport {
    pub source_docx: PathBuf,
    pub truth_pdf: PathBuf,
    pub candidates: Vec<ImageCandidate>,
}

#[derive(Debug, Serialize)]
pub struct ImageCandidate {
    pub image_index: usize,
    pub source_name: String,
    pub source_width: u32,
    pub source_height: u32,
    pub truth_width: u32,
    pub truth_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub geometry: &'static str,
    pub kernel: &'static str,
    pub alpha: &'static str,
    pub codec: &'static str,
    pub encoded_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_absolute_error: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_error: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_squared_error: Option<f64>,
}

#[derive(Clone, Copy)]
enum Kernel {
    Nearest,
    Bilinear,
    Bicubic,
    Hamming,
    Lanczos,
}

impl Kernel {
    const ALL: [Self; 5] = [
        Self::Nearest,
        Self::Bilinear,
        Self::Bicubic,
        Self::Hamming,
        Self::Lanczos,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Nearest => "nearest",
            Self::Bilinear => "bilinear",
            Self::Bicubic => "bicubic_catmull_rom",
            Self::Hamming => "hamming",
            Self::Lanczos => "lanczos3",
        }
    }
}

#[derive(Clone, Copy)]
enum Alpha {
    Opaque,
    White,
    Black,
}

impl Alpha {
    fn name(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::White => "flatten_white",
            Self::Black => "flatten_black",
        }
    }
}

pub fn compare(
    ours_pdf: &[u8],
    truth_pdf: &[u8],
    page: u32,
    extract_dir: Option<&Path>,
) -> Result<ImageProbeReport> {
    let (_, ours) = extract_content_page_with_bytes(ours_pdf, page)?;
    let (_, truth) = extract_content_page_with_bytes(truth_pdf, page)?;
    if let Some(directory) = extract_dir {
        std::fs::create_dir_all(directory).with_context(|| {
            format!("create image extraction directory {}", directory.display())
        })?;
    }
    Ok(ImageProbeReport {
        page,
        comparisons: compare_pairs(&ours, &truth),
        ours: write_side("ours", ours, extract_dir)?,
        truth: write_side("truth", truth, extract_dir)?,
    })
}

fn compare_pairs(
    ours: &[crate::probe::ExtractedImageXObject],
    truth: &[crate::probe::ExtractedImageXObject],
) -> Vec<ImagePairComparison> {
    ours.iter()
        .zip(truth)
        .enumerate()
        .map(|(index, (ours, truth))| {
            let metadata_parity = ours.record.width_px == truth.record.width_px
                && ours.record.height_px == truth.record.height_px
                && ours.record.filters == truth.record.filters
                && ours.record.color_space == truth.record.color_space
                && serde_json::to_value(&ours.record.jpeg).ok()
                    == serde_json::to_value(&truth.record.jpeg).ok();
            let metrics = image::load_from_memory(&ours.bytes)
                .ok()
                .zip(image::load_from_memory(&truth.bytes).ok())
                .and_then(|(ours, truth)| error_metrics(&ours.into_rgb8(), &truth.into_rgb8()));
            ImagePairComparison {
                index,
                metadata_parity,
                mean_absolute_error: metrics.map(|metrics| metrics.0),
                maximum_error: metrics.map(|metrics| metrics.1),
                mean_squared_error: metrics.map(|metrics| metrics.2),
            }
        })
        .collect()
}

fn write_side(
    side: &str,
    images: Vec<crate::probe::ExtractedImageXObject>,
    extract_dir: Option<&Path>,
) -> Result<Vec<ExtractedImageReport>> {
    images
        .into_iter()
        .enumerate()
        .map(|(index, image)| {
            let extracted_path = extract_dir.map(|directory| {
                directory.join(format!(
                    "{side}-{index}-{}.bin",
                    image.record.encoded_sha256
                ))
            });
            if let Some(path) = &extracted_path {
                std::fs::write(path, &image.bytes)
                    .with_context(|| format!("write extracted XObject {}", path.display()))?;
            }
            Ok(ExtractedImageReport {
                image: image.record,
                extracted_path,
            })
        })
        .collect()
}

/// Runs the Task-1 resampling/codec matrix. The input contract is deliberately
/// generic: naturally numbered `word/media/imageN` parts correspond to pages
/// whose content contains exactly one truth image XObject.
pub fn candidate_matrix(
    source_docx: &Path,
    truth_pdf: &Path,
) -> Result<ImageCandidateMatrixReport> {
    let source_bytes = std::fs::read(source_docx)
        .with_context(|| format!("read source DOCX {}", source_docx.display()))?;
    let truth_bytes = std::fs::read(truth_pdf)
        .with_context(|| format!("read truth PDF {}", truth_pdf.display()))?;
    let sources = source_images(&source_bytes)?;
    let mut candidates = Vec::new();

    for (offset, (source_name, source_bytes)) in sources.into_iter().enumerate() {
        let image_index = offset + 1;
        let source = image::load_from_memory(&source_bytes)
            .with_context(|| format!("decode source image {source_name}"))?;
        let (_, truth_images) = extract_content_page_with_bytes(&truth_bytes, image_index as u32)?;
        if truth_images.len() != 1 {
            anyhow::bail!(
                "truth page {image_index} has {} placed image XObjects; matrix requires one",
                truth_images.len()
            );
        }
        let truth = image::load_from_memory(&truth_images[0].bytes)
            .with_context(|| format!("decode truth image on page {image_index}"))?
            .into_rgb8();
        let source_dimensions = (source.width(), source.height());
        let alpha_choices: &[Alpha] = if source.color().has_alpha() {
            &[Alpha::White, Alpha::Black]
        } else {
            &[Alpha::Opaque]
        };

        for &alpha in alpha_choices {
            let rgb = flatten(&source, alpha);
            append_candidates(
                &mut candidates,
                image_index,
                &source_name,
                source_dimensions,
                "source_size",
                "none",
                alpha,
                &rgb,
                &truth,
            )?;
            for kernel in Kernel::ALL {
                let resized = resize(&rgb, truth.width(), truth.height(), kernel)?;
                append_candidates(
                    &mut candidates,
                    image_index,
                    &source_name,
                    source_dimensions,
                    "resample_exact",
                    kernel.name(),
                    alpha,
                    &resized,
                    &truth,
                )?;
            }
        }
    }

    Ok(ImageCandidateMatrixReport {
        source_docx: source_docx.to_owned(),
        truth_pdf: truth_pdf.to_owned(),
        candidates,
    })
}

fn source_images(docx: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(docx)).context("open source DOCX ZIP")?;
    let mut images = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).context("read DOCX ZIP entry")?;
        let name = entry.name().to_owned();
        if !name.starts_with("word/media/") {
            continue;
        }
        let extension = Path::new(&name)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default();
        if !matches!(
            extension.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg"
        ) {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).context("read source image")?;
        images.push((name, bytes));
    }
    images.sort_by_key(|(name, _)| {
        Path::new(name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.trim_start_matches("image").parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    if images.is_empty() {
        anyhow::bail!("source DOCX has no PNG/JPEG word/media parts");
    }
    Ok(images)
}

fn flatten(source: &DynamicImage, alpha: Alpha) -> RgbImage {
    let rgba = source.to_rgba8();
    ImageBuffer::from_fn(rgba.width(), rgba.height(), |x, y| {
        let pixel = rgba.get_pixel(x, y).0;
        let background = match alpha {
            Alpha::Opaque | Alpha::White => 255u16,
            Alpha::Black => 0u16,
        };
        let alpha = u16::from(pixel[3]);
        Rgb([
            ((u16::from(pixel[0]) * alpha + background * (255 - alpha) + 127) / 255) as u8,
            ((u16::from(pixel[1]) * alpha + background * (255 - alpha) + 127) / 255) as u8,
            ((u16::from(pixel[2]) * alpha + background * (255 - alpha) + 127) / 255) as u8,
        ])
    })
}

fn resize(source: &RgbImage, width: u32, height: u32, kernel: Kernel) -> Result<RgbImage> {
    let algorithm = match kernel {
        Kernel::Nearest => ResizeAlg::Nearest,
        Kernel::Bilinear => ResizeAlg::Convolution(FastFilter::Bilinear),
        Kernel::Bicubic => ResizeAlg::Convolution(FastFilter::CatmullRom),
        Kernel::Hamming => ResizeAlg::Convolution(FastFilter::Hamming),
        Kernel::Lanczos => ResizeAlg::Convolution(FastFilter::Lanczos3),
    };
    let source_view = Image::from_vec_u8(
        source.width(),
        source.height(),
        source.as_raw().clone(),
        PixelType::U8x3,
    )?;
    let mut destination = Image::new(width, height, PixelType::U8x3);
    Resizer::new().resize(
        &source_view,
        &mut destination,
        &ResizeOptions::new().resize_alg(algorithm),
    )?;
    RgbImage::from_raw(width, height, destination.into_vec())
        .context("resizer returned the wrong RGB byte count")
}

#[expect(
    clippy::too_many_arguments,
    reason = "candidate identity is explicit evidence"
)]
fn append_candidates(
    output: &mut Vec<ImageCandidate>,
    image_index: usize,
    source_name: &str,
    source_dimensions: (u32, u32),
    geometry: &'static str,
    kernel: &'static str,
    alpha: Alpha,
    pixels: &RgbImage,
    truth: &RgbImage,
) -> Result<()> {
    let mut encoded = Vec::new();
    encoded.push((
        "word_q75_420",
        office_core::drawing::image::diagnostics_encode_word_jpeg(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
        )?,
    ));
    for (name, sampling) in [
        ("jpeg_encoder_q75_420", SamplingFactor::F_2_2),
        ("jpeg_encoder_q75_422", SamplingFactor::F_2_1),
    ] {
        let mut bytes = Vec::new();
        let mut encoder = Encoder::new(&mut bytes, 75);
        encoder.set_sampling_factor(sampling);
        encoder.encode(
            pixels.as_raw(),
            u16::try_from(pixels.width()).context("candidate width exceeds JPEG limit")?,
            u16::try_from(pixels.height()).context("candidate height exceeds JPEG limit")?,
            ColorType::Rgb,
        )?;
        encoded.push((name, bytes));
    }

    for (codec, bytes) in encoded {
        let decoded = image::load_from_memory(&bytes)
            .with_context(|| format!("decode {codec} candidate"))?
            .into_rgb8();
        let metrics = error_metrics(&decoded, truth);
        output.push(ImageCandidate {
            image_index,
            source_name: source_name.to_owned(),
            source_width: source_dimensions.0,
            source_height: source_dimensions.1,
            truth_width: truth.width(),
            truth_height: truth.height(),
            output_width: decoded.width(),
            output_height: decoded.height(),
            geometry,
            kernel,
            alpha: alpha.name(),
            codec,
            encoded_bytes: bytes.len(),
            mean_absolute_error: metrics.map(|metrics| metrics.0),
            maximum_error: metrics.map(|metrics| metrics.1),
            mean_squared_error: metrics.map(|metrics| metrics.2),
        });
    }
    Ok(())
}

fn error_metrics(candidate: &RgbImage, truth: &RgbImage) -> Option<(f64, u8, f64)> {
    if candidate.dimensions() != truth.dimensions() {
        return None;
    }
    let mut absolute_sum = 0u64;
    let mut squared_sum = 0u64;
    let mut maximum = 0u8;
    for (&candidate, &truth) in candidate.as_raw().iter().zip(truth.as_raw()) {
        let difference = candidate.abs_diff(truth);
        absolute_sum += u64::from(difference);
        squared_sum += u64::from(difference) * u64::from(difference);
        maximum = maximum.max(difference);
    }
    let count = candidate.as_raw().len() as f64;
    Some((
        absolute_sum as f64 / count,
        maximum,
        squared_sum as f64 / count,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_rejects_zero_based_page_numbers() {
        let error = compare(b"not read", b"not read", 0, None)
            .expect_err("zero-based page must fail before PDF parsing");

        assert!(error.to_string().contains("1-based"), "{error:#}");
    }
}

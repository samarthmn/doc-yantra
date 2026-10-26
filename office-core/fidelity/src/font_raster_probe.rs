//! Pdfium glyph-raster comparison using the unchanged official page scorer.

use anyhow::{Result, bail};
use image::DynamicImage;
use serde::Serialize;

use crate::{raster, ssim};

#[derive(Debug, Serialize)]
pub struct FontRasterProbeReport {
    pub page: u32,
    pub crop: Option<[u32; 4]>,
    pub ours_pixel_dimensions: [u32; 2],
    pub truth_pixel_dimensions: [u32; 2],
    pub ssim: f64,
    pub ssim_unweighted: f64,
    pub diff_pct: f64,
}

pub fn compare(
    ours_pdf: &[u8],
    truth_pdf: &[u8],
    page: u32,
    crop: Option<[u32; 4]>,
) -> Result<FontRasterProbeReport> {
    if page == 0 {
        bail!("font-raster-probe page is 1-based and must be at least 1");
    }
    let mut ours = raster(ours_pdf, page - 1)?;
    let mut truth = raster(truth_pdf, page - 1)?;
    let score = score_crop(&ours, &truth, crop)?;
    if let Some([x, y, width, height]) = crop {
        ours = ours.crop_imm(x, y, width, height);
        truth = truth.crop_imm(x, y, width, height);
    }
    Ok(FontRasterProbeReport {
        page,
        crop,
        ours_pixel_dimensions: [ours.width(), ours.height()],
        truth_pixel_dimensions: [truth.width(), truth.height()],
        ssim: score.ssim,
        ssim_unweighted: score.ssim_unweighted,
        diff_pct: score.diff_pct,
    })
}

/// Scores an already-rendered glyph crop with the unchanged official scorer.
pub(crate) fn score_crop(
    ours: &DynamicImage,
    truth: &DynamicImage,
    crop: Option<[u32; 4]>,
) -> Result<ssim::PageScore> {
    if let Some([x, y, width, height]) = crop {
        if width == 0
            || height == 0
            || x.saturating_add(width) > ours.width()
            || y.saturating_add(height) > ours.height()
            || x.saturating_add(width) > truth.width()
            || y.saturating_add(height) > truth.height()
        {
            bail!("font-raster-probe crop is outside a rendered page");
        }
        let ours = ours.crop_imm(x, y, width, height);
        let truth = truth.crop_imm(x, y, width, height);
        Ok(ssim::score_pages(&ours, &truth))
    } else {
        Ok(ssim::score_pages(ours, truth))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn comparison_rejects_zero_based_page_numbers() {
        let error = compare(b"not read", b"not read", 0, None)
            .expect_err("zero-based page must fail before PDF parsing");

        assert!(error.to_string().contains("1-based"), "{error:#}");
    }

    #[test]
    fn identical_positioned_pdf_crop_uses_the_official_scorer() {
        let pdf = std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/corpus/docx/complex/ground-truth.pdf"),
        )
        .expect("read frozen positioned-glyph PDF");

        let report = compare(&pdf, &pdf, 1, Some([0, 0, 128, 128]))
            .expect("compare identical diagnostic crop");

        assert_eq!(report.ssim_unweighted, 1.0);
    }
}

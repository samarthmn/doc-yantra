//! Image-XObject extraction and side-by-side metadata reporting.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::probe::{ImageXObjectRecord, extract_content_page_with_bytes};

#[derive(Debug, Serialize)]
pub struct ImageProbeReport {
    pub page: u32,
    pub ours: Vec<ExtractedImageReport>,
    pub truth: Vec<ExtractedImageReport>,
}

#[derive(Debug, Serialize)]
pub struct ExtractedImageReport {
    #[serde(flatten)]
    pub image: ImageXObjectRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_path: Option<PathBuf>,
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
        ours: write_side("ours", ours, extract_dir)?,
        truth: write_side("truth", truth, extract_dir)?,
    })
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

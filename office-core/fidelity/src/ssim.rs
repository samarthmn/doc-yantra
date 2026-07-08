//! Grayscale SSIM scoring (no external SSIM crate).
//!
//! Both pages are rasterized at the same scale; SSIM is computed over 8×8
//! windows (stride 8). The page score is the mean window SSIM; a case's score is
//! the min over its pages, and any page-count / geometry mismatch fails outright.

use image::{DynamicImage, GenericImageView, Rgb, RgbImage};

/// SSIM constants (8-bit dynamic range).
const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
/// Per-pixel luma difference threshold counted as a "diff pixel".
const DIFF_THRESHOLD: f64 = 12.0;
/// Window SSIM below this is highlighted in the heatmap.
const HEATMAP_THRESHOLD: f64 = 0.9;

/// The result of scoring one page pair.
pub struct PageScore {
    /// Mean window SSIM (0.0–1.0).
    pub ssim: f64,
    /// Fraction of pixels whose luma differs by more than [`DIFF_THRESHOLD`].
    pub diff_pct: f64,
    /// A PNG heatmap (ours, with red where windows scored below threshold).
    pub heatmap_png: Vec<u8>,
}

/// A grayscale image as row-major `f64` luma.
struct Gray {
    w: usize,
    h: usize,
    px: Vec<f64>,
}

impl Gray {
    fn from_image(img: &DynamicImage) -> Gray {
        let (w, h) = img.dimensions();
        let mut px = Vec::with_capacity((w * h) as usize);
        for (_, _, p) in img.pixels() {
            let [r, g, b, _] = p.0;
            px.push(0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64);
        }
        Gray {
            w: w as usize,
            h: h as usize,
            px,
        }
    }

    fn at(&self, x: usize, y: usize) -> f64 {
        self.px[y * self.w + x]
    }

    /// Pads to `(w, h)` with white (255) at the bottom/right.
    fn padded(&self, w: usize, h: usize) -> Gray {
        let mut px = vec![255.0; w * h];
        for y in 0..self.h.min(h) {
            for x in 0..self.w.min(w) {
                px[y * w + x] = self.at(x, y);
            }
        }
        Gray { w, h, px }
    }
}

/// Scores two page images. Dimensions differing by ≤ 2px are white-padded to
/// match; a larger mismatch is a geometry failure (score 0).
pub fn score_pages(ours: &DynamicImage, truth: &DynamicImage) -> PageScore {
    let a = Gray::from_image(ours);
    let b = Gray::from_image(truth);

    let dw = a.w.abs_diff(b.w);
    let dh = a.h.abs_diff(b.h);
    if dw > 2 || dh > 2 {
        return PageScore {
            ssim: 0.0,
            diff_pct: 1.0,
            heatmap_png: heatmap(ours, &[], 0, 0),
        };
    }
    let w = a.w.max(b.w);
    let h = a.h.max(b.h);
    let a = a.padded(w, h);
    let b = b.padded(w, h);

    let mut total = 0.0;
    let mut windows = 0usize;
    let mut fail_blocks: Vec<(usize, usize)> = Vec::new();
    let mut diff_px = 0usize;

    let bw = w / 8;
    let bh = h / 8;
    for by in 0..bh {
        for bx in 0..bw {
            let (mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for j in 0..8 {
                for i in 0..8 {
                    let x = bx * 8 + i;
                    let y = by * 8 + j;
                    let va = a.at(x, y);
                    let vb = b.at(x, y);
                    sx += va;
                    sy += vb;
                    sxx += va * va;
                    syy += vb * vb;
                    sxy += va * vb;
                    if (va - vb).abs() > DIFF_THRESHOLD {
                        diff_px += 1;
                    }
                }
            }
            let n = 64.0;
            let mx = sx / n;
            let my = sy / n;
            let vx = sxx / n - mx * mx;
            let vy = syy / n - my * my;
            let cov = sxy / n - mx * my;
            let s = ((2.0 * mx * my + C1) * (2.0 * cov + C2))
                / ((mx * mx + my * my + C1) * (vx + vy + C2));
            total += s;
            windows += 1;
            if s < HEATMAP_THRESHOLD {
                fail_blocks.push((bx, by));
            }
        }
    }

    let ssim = if windows == 0 {
        1.0
    } else {
        total / windows as f64
    };
    let diff_pct = diff_px as f64 / (w * h).max(1) as f64;
    PageScore {
        ssim,
        diff_pct,
        heatmap_png: heatmap(ours, &fail_blocks, w, h),
    }
}

/// Builds a heatmap PNG: `ours` with a red tint over failed 8×8 windows.
fn heatmap(ours: &DynamicImage, fail_blocks: &[(usize, usize)], _w: usize, _h: usize) -> Vec<u8> {
    let (iw, ih) = ours.dimensions();
    let mut out = RgbImage::new(iw, ih);
    for (x, y, p) in ours.pixels() {
        let [r, g, b, _] = p.0;
        out.put_pixel(x, y, Rgb([r, g, b]));
    }
    for &(bx, by) in fail_blocks {
        for j in 0..8 {
            for i in 0..8 {
                let x = (bx * 8 + i) as u32;
                let y = (by * 8 + j) as u32;
                if x < iw && y < ih {
                    let p = out.get_pixel(x, y).0;
                    out.put_pixel(x, y, Rgb([255, p[1] / 2, p[2] / 2]));
                }
            }
        }
    }
    let mut png = Vec::new();
    let dynimg = DynamicImage::ImageRgb8(out);
    dynimg
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode heatmap");
    png
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};

    fn solid(w: u32, h: u32, v: u8) -> DynamicImage {
        let mut img = RgbImage::new(w, h);
        for p in img.pixels_mut() {
            *p = Rgb([v, v, v]);
        }
        DynamicImage::ImageRgb8(img)
    }

    fn noise(w: u32, h: u32, seed: u8) -> DynamicImage {
        let mut img = RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let v = ((x.wrapping_mul(31) + y.wrapping_mul(17) + seed as u32) % 256) as u8;
            *p = Rgb([v, v, v]);
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn identical_images_score_one() {
        let a = noise(64, 64, 5);
        let s = score_pages(&a, &a);
        assert!(s.ssim > 0.999, "identical → ~1.0, got {}", s.ssim);
        assert_eq!(s.diff_pct, 0.0);
    }

    #[test]
    fn inverted_images_score_low() {
        let a = solid(64, 64, 0);
        let b = solid(64, 64, 255);
        let s = score_pages(&a, &b);
        assert!(s.ssim < 0.05, "black vs white → ~0, got {}", s.ssim);
    }

    #[test]
    fn shifted_gradient_scores_between() {
        // A horizontal gradient vs the same gradient shifted 1px: strongly but
        // not perfectly correlated — a genuine partial-similarity case.
        let mut a = RgbImage::new(64, 64);
        let mut b = RgbImage::new(64, 64);
        for y in 0..64 {
            for x in 0..64u32 {
                let va = ((x.wrapping_mul(4)) % 256) as u8;
                let vb = (((x + 1).wrapping_mul(4)) % 256) as u8;
                a.put_pixel(x, y, Rgb([va; 3]));
                b.put_pixel(x, y, Rgb([vb; 3]));
            }
        }
        let s = score_pages(&DynamicImage::ImageRgb8(a), &DynamicImage::ImageRgb8(b));
        assert!(
            s.ssim > 0.3 && s.ssim < 0.999,
            "shifted gradient → between, got {}",
            s.ssim
        );
    }

    #[test]
    fn geometry_mismatch_fails() {
        let a = solid(64, 64, 128);
        let b = solid(200, 64, 128);
        let s = score_pages(&a, &b);
        assert_eq!(s.ssim, 0.0);
    }
}

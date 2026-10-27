//! Inline object (image) helpers. Inline drawings are sized from `wp:extent`
//! and placed by the paragraph layout; this module holds the intrinsic-size
//! fallback used when a drawing omits its extent.

use super::EncodedImage;

/// Intrinsic size of an image in points at 96 DPI (used when `wp:extent` is
/// absent — rare).
pub fn intrinsic_size_pt(image: &EncodedImage) -> (f64, f64) {
    let (w, h) = match image {
        EncodedImage::Jpeg { width, height, .. }
        | EncodedImage::Rgb8 { width, height, .. }
        | EncodedImage::Rgba8 { width, height, .. } => (*width, *height),
    };
    (w as f64 * 72.0 / 96.0, h as f64 * 72.0 / 96.0)
}

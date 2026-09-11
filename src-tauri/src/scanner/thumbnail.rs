//! Downscaled page previews sent to the client while a scan runs.

use std::io::Cursor;

use super::ScanError;

/// Longest edge of a generated thumbnail, in pixels.
pub const THUMBNAIL_MAX_EDGE: u32 = 300;

/// JPEG quality for thumbnails — low enough to stay near 20 KB per page.
pub const THUMBNAIL_QUALITY: u8 = 70;

/// Downscale tightly-packed 8-bit pixel data to a JPEG preview whose longest
/// edge is at most `THUMBNAIL_MAX_EDGE`, preserving aspect ratio.
///
/// `pixels` is the output of `raw::normalize`; `channels` is 1 (grayscale),
/// 3 (RGB) or 4 (RGBA). Images already within the cap are not upscaled.
pub fn thumbnail_jpeg(
    pixels: &[u8],
    width: u32,
    height: u32,
    channels: u8,
) -> Result<Vec<u8>, ScanError> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(channels as usize))
        .ok_or_else(|| ScanError::ImageConversion("Thumbnail geometry overflows".to_string()))?;

    if pixels.len() < expected {
        return Err(ScanError::ImageConversion(format!(
            "Thumbnail source too small: got {} bytes, need {} for {}x{} at {} channels",
            pixels.len(),
            expected,
            width,
            height,
            channels
        )));
    }

    let owned = pixels[..expected].to_vec();
    let source = match channels {
        1 => image::GrayImage::from_raw(width, height, owned).map(image::DynamicImage::ImageLuma8),
        3 => image::RgbImage::from_raw(width, height, owned).map(image::DynamicImage::ImageRgb8),
        4 => image::RgbaImage::from_raw(width, height, owned).map(image::DynamicImage::ImageRgba8),
        other => {
            return Err(ScanError::ImageConversion(format!(
                "Unsupported channel count for thumbnail: {}",
                other
            )))
        }
    }
    .ok_or_else(|| ScanError::ImageConversion("Thumbnail buffer rejected by decoder".to_string()))?;

    // Skip scaling if already within the cap; `thumbnail` would still invoke the
    // scaling pipeline even for images smaller than the box, which wastes bytes.
    let scaled = if width <= THUMBNAIL_MAX_EDGE && height <= THUMBNAIL_MAX_EDGE {
        source
    } else {
        source.thumbnail(THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE)
    };

    // JPEG carries no alpha channel.
    let scaled = match scaled {
        image::DynamicImage::ImageRgba8(img) => {
            image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(img).to_rgb8())
        }
        other => other,
    };

    let mut buf = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut buf), THUMBNAIL_QUALITY);
    encoder
        .encode_image(&scaled)
        .map_err(|e: image::ImageError| ScanError::ImageConversion(e.to_string()))?;

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::{thumbnail_jpeg, THUMBNAIL_MAX_EDGE};

    fn solid(width: u32, height: u32, channels: u8) -> Vec<u8> {
        vec![0x80; (width * height * channels as u32) as usize]
    }

    /// Decode the produced JPEG so assertions are about the real image, not
    /// about our own arithmetic.
    fn decoded_size(jpeg: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(jpeg).expect("thumbnail must be a decodable JPEG");
        (img.width(), img.height())
    }

    #[test]
    fn portrait_page_is_capped_on_its_long_edge() {
        // 2550x3300 is a 300dpi letter page.
        let jpeg = thumbnail_jpeg(&solid(2550, 3300, 3), 2550, 3300, 3).unwrap();
        let (w, h) = decoded_size(&jpeg);
        assert_eq!(h, THUMBNAIL_MAX_EDGE, "long edge should be the cap");
        assert!(w < h, "portrait aspect must be preserved");
        let ratio = w as f64 / h as f64;
        let expected = 2550.0 / 3300.0;
        assert!((ratio - expected).abs() < 0.02, "aspect drifted: {ratio} vs {expected}");
    }

    #[test]
    fn landscape_page_is_capped_on_its_long_edge() {
        let jpeg = thumbnail_jpeg(&solid(3300, 2550, 3), 3300, 2550, 3).unwrap();
        let (w, h) = decoded_size(&jpeg);
        assert_eq!(w, THUMBNAIL_MAX_EDGE);
        assert!(h < w, "landscape aspect must be preserved");
    }

    #[test]
    fn grayscale_pages_are_supported() {
        let jpeg = thumbnail_jpeg(&solid(600, 800, 1), 600, 800, 1).unwrap();
        let (_, h) = decoded_size(&jpeg);
        assert_eq!(h, THUMBNAIL_MAX_EDGE);
    }

    #[test]
    fn rgba_pages_drop_alpha_rather_than_failing() {
        // JPEG has no alpha channel; the thumbnail must still be produced.
        let jpeg = thumbnail_jpeg(&solid(400, 400, 4), 400, 400, 4).unwrap();
        let (w, h) = decoded_size(&jpeg);
        assert_eq!((w, h), (THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE));
    }

    #[test]
    fn smaller_than_the_cap_is_not_upscaled() {
        let jpeg = thumbnail_jpeg(&solid(100, 120, 3), 100, 120, 3).unwrap();
        assert_eq!(decoded_size(&jpeg), (100, 120));
    }

    #[test]
    fn unsupported_channel_count_errors() {
        assert!(thumbnail_jpeg(&solid(10, 10, 3), 10, 10, 2).is_err());
    }

    #[test]
    fn truncated_pixel_buffer_errors_rather_than_panicking() {
        assert!(thumbnail_jpeg(&[0u8; 10], 100, 100, 3).is_err());
    }
}

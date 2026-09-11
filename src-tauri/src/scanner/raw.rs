//! Normalization of raw TWAIN memory-transfer buffers into tightly-packed
//! 8-bit-per-sample rows suitable for the `image` encoders.

use super::ScanError;

/// Convert a raw TWAIN memory-transfer buffer into tightly-packed rows.
pub fn normalize(
    raw: &[u8],
    width: u32,
    height: u32,
    bits_per_pixel: u16,
    bytes_per_row: u32,
) -> Result<Vec<u8>, ScanError> {
    let stride = bytes_per_row as usize;
    let width = width as usize;
    let height = height as usize;

    if width == 0 || height == 0 {
        return Err(ScanError::ImageConversion(format!(
            "Empty image geometry: {}x{}",
            width, height
        )));
    }

    // Minimum bytes a single row of pixels occupies, before any padding.
    let min_row = match bits_per_pixel {
        1 => width.div_ceil(8),
        8 => width,
        24 => width * 3,
        32 => width * 4,
        other => {
            return Err(ScanError::ImageConversion(format!(
                "Unsupported bit depth: {}",
                other
            )))
        }
    };

    if stride < min_row {
        return Err(ScanError::ImageConversion(format!(
            "BytesPerRow {} is smaller than a {}bpp row of {} pixels ({} bytes)",
            stride, bits_per_pixel, width, min_row
        )));
    }

    // The final row need not carry trailing padding.
    let required = stride
        .checked_mul(height - 1)
        .and_then(|n| n.checked_add(min_row))
        .ok_or_else(|| ScanError::ImageConversion("Image geometry overflows".to_string()))?;

    if raw.len() < required {
        return Err(ScanError::ImageConversion(format!(
            "Truncated image buffer: got {} bytes, need {} for {}x{} at {}bpp",
            raw.len(),
            required,
            width,
            height,
            bits_per_pixel
        )));
    }

    if bits_per_pixel == 1 {
        // Bit-packed, MSB first. Expand each bit to a full grayscale byte.
        let mut out = Vec::with_capacity(width * height);
        for y in 0..height {
            let row = &raw[y * stride..];
            for x in 0..width {
                let bit = (row[x / 8] >> (7 - (x % 8))) & 1;
                out.push(if bit == 1 { 0xFF } else { 0x00 });
            }
        }
        return Ok(out);
    }

    let mut out = Vec::with_capacity(min_row * height);
    for y in 0..height {
        let start = y * stride;
        out.extend_from_slice(&raw[start..start + min_row]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn strips_row_padding_8bpp() {
        // 3px wide, 2 rows, but the source pads each row to 4 bytes.
        let raw = vec![1, 2, 3, 0xAA, 4, 5, 6, 0xAA];
        let out = normalize(&raw, 3, 2, 8, 4).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn strips_row_padding_24bpp() {
        // 2px wide RGB = 6 bytes of pixels, padded to an 8-byte stride.
        let raw = vec![1, 2, 3, 4, 5, 6, 0xAA, 0xAA, 7, 8, 9, 10, 11, 12, 0xAA, 0xAA];
        let out = normalize(&raw, 2, 2, 24, 8).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn expands_1bpp_to_8bpp_msb_first() {
        // 16px wide, 2 rows, tightly packed at 2 bytes per row.
        // TWPF_CHOCOLATE: a 0 bit is black (0x00), a 1 bit is white (0xFF).
        let raw = vec![0b1010_1010, 0b1100_1100, 0b1111_0000, 0b0000_1111];
        let out = normalize(&raw, 16, 2, 1, 2).unwrap();

        let w = 0xFFu8;
        let b = 0x00u8;
        assert_eq!(
            out,
            vec![
                w, b, w, b, w, b, w, b, w, w, b, b, w, w, b, b,
                w, w, w, w, b, b, b, b, b, b, b, b, w, w, w, w,
            ]
        );
    }

    #[test]
    fn truncated_buffer_errors_instead_of_panicking() {
        // Two rows promised, only one row's worth of bytes delivered.
        let raw = vec![1, 2, 3, 0xAA];
        assert!(normalize(&raw, 3, 2, 8, 4).is_err());
    }

    #[test]
    fn rejects_unsupported_bit_depth() {
        let raw = vec![0u8; 64];
        assert!(normalize(&raw, 4, 2, 4, 4).is_err());
        assert!(normalize(&raw, 4, 2, 16, 8).is_err());
    }

    #[test]
    fn rejects_stride_smaller_than_a_row() {
        // A 4px 24bpp row needs 12 bytes; the source claims 8.
        let raw = vec![0u8; 64];
        assert!(normalize(&raw, 4, 2, 24, 8).is_err());
    }

    #[test]
    fn expands_1bpp_with_row_padding() {
        // 9px wide needs 2 bytes, but the source pads the row to 4.
        let raw = vec![
            0b1010_1010, 0b1000_0000, 0xAA, 0xAA,
            0b0101_0101, 0b0000_0000, 0xAA, 0xAA,
        ];
        let out = normalize(&raw, 9, 2, 1, 4).unwrap();
        let w = 0xFFu8;
        let b = 0x00u8;
        assert_eq!(
            out,
            vec![w, b, w, b, w, b, w, b, w, b, w, b, w, b, w, b, w, b]
        );
    }

    #[test]
    fn strips_row_padding_32bpp() {
        // Two rows, so the padded stride is actually traversed.
        let raw = vec![
            1, 2, 3, 4, 5, 6, 7, 8, 0xAA, 0xAA,
            9, 10, 11, 12, 13, 14, 15, 16, 0xAA, 0xAA,
        ];
        let out = normalize(&raw, 2, 2, 32, 10).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
    }

    #[test]
    fn handles_letter_size_24bpp_with_dword_padded_rows() {
        // 8.5in @ 300dpi = 2550px. A 24bpp row is 7650 bytes, which most
        // sources pad to the next DWORD boundary: 7652. This geometry is
        // what panicked before normalization existed.
        let width = 2550u32;
        let height = 4u32;
        let min_row = width as usize * 3;
        let stride = 7652usize;
        assert_ne!(stride, min_row, "test is pointless without real padding");

        let mut raw = vec![0u8; stride * height as usize];
        for y in 0..height as usize {
            raw[y * stride] = (y + 1) as u8;
        }

        let out = normalize(&raw, width, height, 24, stride as u32).unwrap();
        assert_eq!(out.len(), min_row * height as usize);
        for y in 0..height as usize {
            assert_eq!(out[y * min_row], (y + 1) as u8, "row {} misaligned", y);
        }
    }
}

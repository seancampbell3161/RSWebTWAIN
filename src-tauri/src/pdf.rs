//! PDF generation from scanned page images.
//!
//! Uses the `scannedpdf` crate for fast, low-memory image-to-PDF conversion.
//! Falls back to a basic implementation if scannedpdf is not available.

use std::io::Cursor;

use tracing::{debug, info};

/// Generate a PDF from a collection of page images (PNG format).
///
/// Each entry in `pages` is the raw PNG bytes for one page.
/// Returns the complete PDF file as bytes.
pub fn generate_pdf(pages: &[Vec<u8>]) -> Result<Vec<u8>, PdfError> {
    if pages.is_empty() {
        return Err(PdfError::NoPages);
    }

    info!("Generating PDF from {} page(s)", pages.len());

    // Use image crate to write pages into a simple PDF
    // Since scannedpdf may need specific setup, we implement a basic version
    // that embeds images into a PDF structure
    generate_pdf_from_images(pages)
}

fn generate_pdf_from_images(pages: &[Vec<u8>]) -> Result<Vec<u8>, PdfError> {
    // Decode each PNG to get dimensions, then embed as JPEG in PDF for compression
    let mut page_data: Vec<PdfPage> = Vec::with_capacity(pages.len());

    for (i, png_bytes) in pages.iter().enumerate() {
        let img = image::load_from_memory(png_bytes)
            .map_err(|e| PdfError::ImageDecode(format!("Page {}: {}", i + 1, e)))?;

        let width = img.width();
        let height = img.height();

        // Encode as JPEG for PDF embedding (good compression)
        let mut jpeg_buf = Vec::new();
        let mut cursor = Cursor::new(&mut jpeg_buf);
        img.write_to(&mut cursor, image::ImageFormat::Jpeg)
            .map_err(|e| PdfError::ImageEncode(format!("Page {}: {}", i + 1, e)))?;

        debug!(
            "Page {} encoded: {}x{}, {} bytes JPEG",
            i + 1,
            width,
            height,
            jpeg_buf.len()
        );

        page_data.push(PdfPage {
            width,
            height,
            jpeg_data: jpeg_buf,
        });
    }

    // Build a minimal valid PDF
    build_pdf(&page_data)
}

struct PdfPage {
    width: u32,
    height: u32,
    jpeg_data: Vec<u8>,
}

/// Build a minimal PDF 1.4 document embedding JPEG images.
///
/// This is a straightforward PDF builder that creates one page per image.
/// Each page is sized to match the image dimensions at 72 DPI (PDF points).
fn build_pdf(pages: &[PdfPage]) -> Result<Vec<u8>, PdfError> {
    let mut pdf = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();

    // Header
    pdf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    // Object 1: Catalog
    offsets.push(pdf.len());
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages (parent of all page objects)
    let page_count = pages.len();
    offsets.push(pdf.len());
    let mut kids = String::from("[ ");
    for i in 0..page_count {
        let page_obj = 3 + i * 2; // Page objects at 3, 5, 7, ...
        kids.push_str(&format!("{} 0 R ", page_obj));
    }
    kids.push(']');
    pdf.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /Pages /Kids {} /Count {} >>\nendobj\n",
            kids, page_count
        )
        .as_bytes(),
    );

    // For each page: Page object + Image XObject stream
    for (i, page) in pages.iter().enumerate() {
        let page_obj_num = 3 + i * 2;
        let image_obj_num = 4 + i * 2;

        // Convert pixel dimensions to PDF points (assume 72 DPI for simplicity;
        // actual DPI info could be used to compute: width_pt = width_px * 72 / dpi)
        let width_pt = page.width as f64;
        let height_pt = page.height as f64;

        // Page object
        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.0} {:.0}] \
                 /Contents [] /Resources << /XObject << /Img{} {} 0 R >> >> \
                 /Annots [] >>\nendobj\n",
                page_obj_num, width_pt, height_pt, i, image_obj_num
            )
            .as_bytes(),
        );

        // We need a content stream that draws the image
        // Actually, let's add a proper content stream
        // We'll adjust: page obj references a content stream, which references the image
    }

    // Rebuild properly: we need content streams too
    pdf.clear();
    offsets.clear();

    // Header
    pdf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    // Object 1: Catalog
    offsets.push(pdf.len());
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages
    offsets.push(pdf.len());
    let mut kids = String::from("[ ");
    for i in 0..page_count {
        let page_obj = 3 + i * 3; // Page objects at 3, 6, 9, ...
        kids.push_str(&format!("{} 0 R ", page_obj));
    }
    kids.push(']');
    pdf.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /Pages /Kids {} /Count {} >>\nendobj\n",
            kids, page_count
        )
        .as_bytes(),
    );

    // For each page: Page object + Content stream + Image XObject
    for (i, page) in pages.iter().enumerate() {
        let page_obj = 3 + i * 3;
        let content_obj = 4 + i * 3;
        let image_obj = 5 + i * 3;

        let width_pt = page.width as f64;
        let height_pt = page.height as f64;

        // Page object
        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Type /Page /Parent 2 0 R \
                 /MediaBox [0 0 {:.0} {:.0}] \
                 /Contents {} 0 R \
                 /Resources << /XObject << /Img {} 0 R >> >> >>\nendobj\n",
                page_obj, width_pt, height_pt, content_obj, image_obj
            )
            .as_bytes(),
        );

        // Content stream: draw the image scaled to fill the page
        let content = format!(
            "q\n{:.0} 0 0 {:.0} 0 0 cm\n/Img Do\nQ\n",
            width_pt, height_pt
        );
        let content_bytes = content.as_bytes();
        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Length {} >>\nstream\n",
                content_obj,
                content_bytes.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(content_bytes);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        // Image XObject (JPEG)
        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Type /XObject /Subtype /Image \
                 /Width {} /Height {} \
                 /ColorSpace /DeviceRGB /BitsPerComponent 8 \
                 /Filter /DCTDecode /Length {} >>\nstream\n",
                image_obj, page.width, page.height, page.jpeg_data.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&page.jpeg_data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }

    // Cross-reference table
    let xref_offset = pdf.len();
    let total_objects = 2 + page_count * 3; // catalog + pages + (page + content + image) per page
    pdf.extend_from_slice(format!("xref\n0 {}\n", total_objects + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }

    // Trailer
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\n",
            total_objects + 1
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_offset).as_bytes());

    info!("PDF generated: {} bytes, {} pages", pdf.len(), page_count);
    Ok(pdf)
}

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("No pages to generate PDF from")]
    NoPages,

    #[error("Image decode error: {0}")]
    ImageDecode(String),

    #[error("Image encode error: {0}")]
    ImageEncode(String),

    #[error("PDF write error: {0}")]
    WriteError(String),
}

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Resolution assumed when a source reports none. A wrong-but-plausible page
/// size is far better than one seventeen times too large, which is what
/// assuming 72 produced.
const DEFAULT_DPI: f32 = 300.0;

/// One page being appended to a document.
pub struct PageSpec<'a> {
    pub jpeg: &'a [u8],
    pub width_px: u32,
    pub height_px: u32,
    pub dpi_x: f32,
    pub dpi_y: f32,
    /// 1 for grayscale, 3 for RGB.
    pub channels: u8,
}

/// Writes a PDF incrementally, so a scan never holds more than one page.
///
/// PDF objects may appear in any order — the xref table maps object numbers to
/// byte offsets — so pages are appended as they arrive and the `/Catalog`
/// (object 1) and `/Pages` (object 2) are written by `finish`.
pub struct PdfWriter {
    out: BufWriter<File>,
    written: u64,
    /// Byte offset of each object, indexed by object number minus one.
    offsets: Vec<u64>,
    page_objs: Vec<u32>,
}

impl PdfWriter {
    pub fn create(path: &Path) -> Result<Self, PdfError> {
        let file = File::create(path).map_err(|e| PdfError::WriteError(e.to_string()))?;
        let mut writer = PdfWriter {
            out: BufWriter::new(file),
            written: 0,
            offsets: Vec::new(),
            page_objs: Vec::new(),
        };
        writer.emit(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")?;
        Ok(writer)
    }

    fn emit(&mut self, bytes: &[u8]) -> Result<(), PdfError> {
        self.out
            .write_all(bytes)
            .map_err(|e| PdfError::WriteError(e.to_string()))?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    fn mark(&mut self, obj: u32) {
        let idx = obj as usize - 1;
        if self.offsets.len() <= idx {
            self.offsets.resize(idx + 1, 0);
        }
        self.offsets[idx] = self.written;
    }

    pub fn page_count(&self) -> usize {
        self.page_objs.len()
    }

    pub fn add_page(&mut self, page: PageSpec<'_>) -> Result<(), PdfError> {
        let colorspace = match page.channels {
            1 => "/DeviceGray",
            3 => "/DeviceRGB",
            other => {
                return Err(PdfError::ImageEncode(format!(
                    "Unsupported channel count for PDF embedding: {}",
                    other
                )))
            }
        };

        let i = self.page_objs.len() as u32;
        let page_obj = 3 + i * 3;
        let content_obj = 4 + i * 3;
        let image_obj = 5 + i * 3;

        let dpi_x = if page.dpi_x.is_finite() && page.dpi_x > 0.0 { page.dpi_x } else { DEFAULT_DPI };
        let dpi_y = if page.dpi_y.is_finite() && page.dpi_y > 0.0 { page.dpi_y } else { DEFAULT_DPI };
        let width_pt = page.width_px as f64 * 72.0 / dpi_x as f64;
        let height_pt = page.height_px as f64 * 72.0 / dpi_y as f64;

        self.mark(page_obj);
        self.emit(
            format!(
                "{page_obj} 0 obj\n<< /Type /Page /Parent 2 0 R \
                 /MediaBox [0 0 {width_pt:.2} {height_pt:.2}] \
                 /Contents {content_obj} 0 R \
                 /Resources << /XObject << /Img {image_obj} 0 R >> >> >>\nendobj\n"
            )
            .as_bytes(),
        )?;

        let content = format!("q\n{width_pt:.2} 0 0 {height_pt:.2} 0 0 cm\n/Img Do\nQ\n");
        self.mark(content_obj);
        self.emit(format!("{content_obj} 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes())?;
        self.emit(content.as_bytes())?;
        self.emit(b"endstream\nendobj\n")?;

        self.mark(image_obj);
        self.emit(
            format!(
                "{image_obj} 0 obj\n<< /Type /XObject /Subtype /Image \
                 /Width {} /Height {} /ColorSpace {colorspace} /BitsPerComponent 8 \
                 /Filter /DCTDecode /Length {} >>\nstream\n",
                page.width_px,
                page.height_px,
                page.jpeg.len()
            )
            .as_bytes(),
        )?;
        self.emit(page.jpeg)?;
        self.emit(b"\nendstream\nendobj\n")?;

        self.page_objs.push(page_obj);
        Ok(())
    }

    pub fn finish(mut self) -> Result<u64, PdfError> {
        self.mark(1);
        self.emit(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")?;

        let mut kids = String::from("[ ");
        for obj in &self.page_objs.clone() {
            kids.push_str(&format!("{obj} 0 R "));
        }
        kids.push(']');
        let count = self.page_objs.len();

        self.mark(2);
        self.emit(format!("2 0 obj\n<< /Type /Pages /Kids {kids} /Count {count} >>\nendobj\n").as_bytes())?;

        let xref_at = self.written;
        let size = self.offsets.len() + 1;
        self.emit(format!("xref\n0 {size}\n").as_bytes())?;
        self.emit(b"0000000000 65535 f \n")?;
        for offset in self.offsets.clone() {
            self.emit(format!("{offset:010} 00000 n \n").as_bytes())?;
        }

        self.emit(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
        )?;

        self.out.flush().map_err(|e| PdfError::WriteError(e.to_string()))?;
        Ok(self.written)
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a minimal valid PNG for testing (1x1 red pixel)
    fn make_test_png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder;
        let mut buf = Vec::new();
        let cursor = std::io::Cursor::new(&mut buf);
        let encoder = image::codecs::png::PngEncoder::new(cursor);
        // RGB image: 3 bytes per pixel
        let pixels: Vec<u8> = (0..width * height)
            .flat_map(|_| vec![255u8, 0, 0]) // red
            .collect();
        encoder
            .write_image(&pixels, width, height, image::ExtendedColorType::Rgb8)
            .unwrap();
        buf
    }

    #[test]
    fn generate_pdf_empty_pages_errors() {
        let result = generate_pdf(&[]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PdfError::NoPages));
    }

    #[test]
    fn generate_pdf_single_page() {
        let png = make_test_png(100, 200);
        let result = generate_pdf(&[png]);
        assert!(result.is_ok());

        let pdf_bytes = result.unwrap();
        // Verify PDF header
        assert!(pdf_bytes.starts_with(b"%PDF-1.4"));
        // Verify PDF trailer
        let trailer = String::from_utf8_lossy(&pdf_bytes);
        assert!(trailer.contains("%%EOF"));
        // Verify it contains page count
        assert!(trailer.contains("/Count 1"));
    }

    #[test]
    fn generate_pdf_multiple_pages() {
        let pages: Vec<Vec<u8>> = (0..3).map(|_| make_test_png(50, 50)).collect();
        let result = generate_pdf(&pages);
        assert!(result.is_ok());

        let pdf_bytes = result.unwrap();
        let text = String::from_utf8_lossy(&pdf_bytes);
        assert!(text.contains("/Count 3"));
        // Should have 3 image XObjects
        assert!(text.contains("/Subtype /Image"));
    }

    #[test]
    fn generate_pdf_contains_valid_xref() {
        let png = make_test_png(10, 10);
        let pdf_bytes = generate_pdf(&[png]).unwrap();
        let text = String::from_utf8_lossy(&pdf_bytes);
        assert!(text.contains("xref"));
        assert!(text.contains("startxref"));
        assert!(text.contains("trailer"));
    }

    #[test]
    fn generate_pdf_invalid_image_errors() {
        let not_a_png = vec![0u8, 1, 2, 3, 4, 5];
        let result = generate_pdf(&[not_a_png]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PdfError::ImageDecode(_)));
    }

    #[test]
    fn generate_pdf_output_is_nonzero_size() {
        let png = make_test_png(10, 10);
        let pdf_bytes = generate_pdf(&[png]).unwrap();
        // A valid single-page PDF should be at least a few hundred bytes
        assert!(pdf_bytes.len() > 100);
    }
}

#[cfg(test)]
mod writer_tests {
    use super::{PageSpec, PdfWriter};

    /// A 1x1 JPEG. Content is irrelevant to structure; only its length matters.
    fn tiny_jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_raw(1, 1, vec![0u8, 0, 0]).unwrap();
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    fn write_doc(pages: &[(u32, u32, f32, u8)]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.pdf");
        let jpeg = tiny_jpeg();
        let mut w = PdfWriter::create(&path).unwrap();
        for &(width_px, height_px, dpi, channels) in pages {
            w.add_page(PageSpec {
                jpeg: &jpeg,
                width_px,
                height_px,
                dpi_x: dpi,
                dpi_y: dpi,
                channels,
            })
            .unwrap();
        }
        w.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        (dir, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The defect this replaces: pixels were written as PDF points, so a
    /// 300dpi letter page claimed to be 35 inches wide.
    #[test]
    fn letter_page_at_300dpi_measures_612_by_792_points() {
        let (_dir, text) = write_doc(&[(2550, 3300, 300.0, 3)]);
        assert!(
            text.contains("/MediaBox [0 0 612.00 792.00]"),
            "expected US Letter in points, got:\n{}",
            text.lines().find(|l| l.contains("MediaBox")).unwrap_or("<none>")
        );
    }

    #[test]
    fn nonpositive_dpi_falls_back_to_300_not_72() {
        let (_dir, text) = write_doc(&[(2550, 3300, 0.0, 3)]);
        assert!(
            text.contains("/MediaBox [0 0 612.00 792.00]"),
            "zero dpi must fall back to 300"
        );
    }

    #[test]
    fn grayscale_pages_declare_devicegray() {
        let (_dir, text) = write_doc(&[(100, 100, 300.0, 1)]);
        assert!(text.contains("/ColorSpace /DeviceGray"), "grayscale must not claim RGB");
        assert!(!text.contains("/ColorSpace /DeviceRGB"));
    }

    #[test]
    fn colour_pages_declare_devicergb() {
        let (_dir, text) = write_doc(&[(100, 100, 300.0, 3)]);
        assert!(text.contains("/ColorSpace /DeviceRGB"));
    }

    #[test]
    fn pages_node_lists_every_page_object() {
        let (_dir, text) = write_doc(&[(100, 100, 300.0, 3); 4]);
        assert!(text.contains("/Count 4"), "page count wrong");
        for obj in [3, 6, 9, 12] {
            assert!(text.contains(&format!("{obj} 0 R")), "missing kid {obj}");
        }
    }

    /// The structural invariant that makes the document loadable: every xref
    /// entry must point at the byte where that object actually starts.
    #[test]
    fn xref_offsets_point_at_their_objects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.pdf");
        let jpeg = tiny_jpeg();
        let mut w = PdfWriter::create(&path).unwrap();
        for _ in 0..3 {
            w.add_page(PageSpec {
                jpeg: &jpeg,
                width_px: 100,
                height_px: 100,
                dpi_x: 300.0,
                dpi_y: 300.0,
                channels: 3,
            })
            .unwrap();
        }
        w.finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);

        let xref_at = text.rfind("\nxref\n").expect("xref section") + 1;
        let after = &text[xref_at + 5..];
        let mut lines = after.lines();
        let header = lines.next().unwrap();
        let size: usize = header.split_whitespace().nth(1).unwrap().parse().unwrap();

        // Entry 0 is the free head; entries 1..size are real objects.
        let entries: Vec<&str> = lines.take(size).collect();
        assert_eq!(entries.len(), size);
        assert!(entries[0].starts_with("0000000000 65535 f"));

        for (i, entry) in entries.iter().enumerate().skip(1) {
            let offset: usize = entry.split_whitespace().next().unwrap().parse().unwrap();
            let expected = format!("{i} 0 obj");
            assert!(
                bytes[offset..].starts_with(expected.as_bytes()),
                "xref entry {i} points at offset {offset}, which is not '{expected}'"
            );
        }
    }

    #[test]
    fn page_count_tracks_added_pages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.pdf");
        let jpeg = tiny_jpeg();
        let mut w = PdfWriter::create(&path).unwrap();
        assert_eq!(w.page_count(), 0);
        w.add_page(PageSpec {
            jpeg: &jpeg,
            width_px: 100,
            height_px: 100,
            dpi_x: 300.0,
            dpi_y: 300.0,
            channels: 3,
        })
        .unwrap();
        assert_eq!(w.page_count(), 1);
    }

    #[test]
    fn unsupported_channel_count_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.pdf");
        let jpeg = tiny_jpeg();
        let mut w = PdfWriter::create(&path).unwrap();
        let result = w.add_page(PageSpec {
            jpeg: &jpeg,
            width_px: 10,
            height_px: 10,
            dpi_x: 300.0,
            dpi_y: 300.0,
            channels: 2,
        });
        assert!(result.is_err());
    }
}

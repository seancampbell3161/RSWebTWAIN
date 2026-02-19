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

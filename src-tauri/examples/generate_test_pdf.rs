//! Generates a test PDF to verify the PDF builder produces valid output.
//!
//! Run with:
//!   cargo run --example generate_test_pdf -p scan-agent
//!
//! This creates a multi-page PDF at `output/test-scan.pdf` with synthetic
//! "scanned document" images you can open in any PDF viewer.

use image::ImageEncoder;
use std::io::Cursor;
use std::path::Path;

fn main() {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("output");
    std::fs::create_dir_all(&out_dir).unwrap();

    let pages = vec![
        make_page(612, 792, "Page 1 — Letter size", [240, 240, 255]),
        make_page(612, 792, "Page 2 — Letter size", [255, 240, 230]),
        make_page(595, 842, "Page 3 — A4 size", [230, 255, 230]),
    ];

    let pdf_bytes = scan_agent_lib::pdf::generate_pdf(&pages).expect("PDF generation failed");

    let out_path = out_dir.join("test-scan.pdf");
    std::fs::write(&out_path, &pdf_bytes).unwrap();

    println!("Wrote {} bytes to {}", pdf_bytes.len(), out_path.display());
    println!("Open it in a PDF viewer to verify the output.");
}

/// Create a synthetic "scanned document" PNG with a colored background,
/// header bar, and fake text lines.
fn make_page(width: u32, height: u32, label: &str, bg: [u8; 3]) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 3) as usize];

    // Fill background
    for chunk in pixels.chunks_exact_mut(3) {
        chunk.copy_from_slice(&bg);
    }

    // Draw a dark header bar (top 60px)
    let header_h = 60.min(height);
    for y in 0..header_h {
        for x in 0..width {
            let idx = ((y * width + x) * 3) as usize;
            pixels[idx] = 50;
            pixels[idx + 1] = 50;
            pixels[idx + 2] = 80;
        }
    }

    // Render the label text into the header using a simple pixel font
    draw_text(&mut pixels, width, 20, 20, label, [255, 255, 255]);

    // Draw fake "text lines" — gray horizontal bars to simulate a scanned doc
    let margin = 50;
    let line_height = 18;
    let line_gap = 6;
    let mut y = (header_h + 30) as i32;
    let mut line_num = 0;
    while y + line_height < height as i32 - margin {
        // Vary line width to look like real text (some lines shorter = paragraph ends)
        let line_width = if line_num % 7 == 6 {
            (width as i32 - 2 * margin) * 3 / 5 // short line (paragraph end)
        } else {
            width as i32 - 2 * margin // full-width line
        };

        draw_rect(
            &mut pixels,
            width,
            height,
            margin,
            y,
            line_width,
            line_height - line_gap,
            [70, 70, 70],
        );

        y += line_height;
        line_num += 1;
    }

    // Encode as PNG
    let mut buf = Vec::new();
    let cursor = Cursor::new(&mut buf);
    let encoder = image::codecs::png::PngEncoder::new(cursor);
    encoder
        .write_image(&pixels, width, height, image::ExtendedColorType::Rgb8)
        .unwrap();
    buf
}

fn draw_rect(
    pixels: &mut [u8],
    img_w: u32,
    img_h: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: [u8; 3],
) {
    for dy in 0..h {
        for dx in 0..w {
            let px = x + dx;
            let py = y + dy;
            if px >= 0 && px < img_w as i32 && py >= 0 && py < img_h as i32 {
                let idx = ((py as u32 * img_w + px as u32) * 3) as usize;
                pixels[idx] = color[0];
                pixels[idx + 1] = color[1];
                pixels[idx + 2] = color[2];
            }
        }
    }
}

/// Very simple 5x7 pixel font renderer — enough to label test pages.
fn draw_text(pixels: &mut [u8], img_w: u32, x: i32, y: i32, text: &str, color: [u8; 3]) {
    let mut cx = x;
    for ch in text.chars() {
        let glyph = get_glyph(ch);
        for (row, &bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    let px = cx + col;
                    let py = y + row as i32;
                    // Draw 2x2 for visibility
                    for dy in 0..2i32 {
                        for dx in 0..2i32 {
                            let fx = px * 2 + dx;
                            let fy = py * 2 + dy;
                            if fx >= 0 && fx < img_w as i32 && fy >= 0 {
                                let idx = (fy as u32 * img_w + fx as u32) as usize * 3;
                                if idx + 2 < pixels.len() {
                                    pixels[idx] = color[0];
                                    pixels[idx + 1] = color[1];
                                    pixels[idx + 2] = color[2];
                                }
                            }
                        }
                    }
                }
            }
        }
        cx += 6; // char width + spacing
    }
}

/// Minimal 5x7 bitmap font — uppercase, digits, and a few symbols.
fn get_glyph(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01110, 0b10001, 0b10000, 0b01110, 0b00001, 0b10001, 0b01110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
        '3' => [0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        ' ' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000],
        '-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        _ => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100], // '?'
    }
}

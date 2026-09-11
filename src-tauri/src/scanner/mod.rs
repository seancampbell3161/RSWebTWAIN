//! Scanner abstraction layer.
//!
//! Defines the `Scanner` trait and the `ScanOrchestrator` which tries native 64-bit
//! TWAIN first and falls back to the 32-bit sidecar if no sources are found.

pub mod raw;
pub mod sidecar;
pub mod thumbnail;
pub mod twain;
pub mod twain_ffi;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ::serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::protocol::{AgentMessage, OutputFormat, ScanRequestOptions, ScanStatus};
use crate::ws_server::send_json;
use crate::ws_server::ResponseSender;

// Scanner trait

/// Information about a discovered scanner
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerInfo {
    pub id: String,
    pub name: String,
    pub manufacturer: String,
    pub source: ScannerSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ScannerSource {
    /// Directly accessible via 64-bit TWAIN
    Native,
    /// Accessible via 32-bit sidecar
    Sidecar,
}

/// A scanned page as raw image bytes
#[derive(Debug, Clone)]
pub struct PageData {
    pub page_number: u32,
    pub width: u32,
    pub height: u32,
    pub bits_per_pixel: u16,
    pub dpi_x: f32,
    pub dpi_y: f32,
    /// Stride of `raw_data` in bytes, as reported by the source. Sources
    /// commonly pad rows to a 4-byte boundary, so this is >= the packed row.
    pub bytes_per_row: u32,
    pub raw_data: Vec<u8>,
}

impl PageData {
    /// Tightly-packed 8-bit-per-sample pixels, with source row padding removed
    /// and 1bpp black-and-white expanded to 8bpp grayscale.
    fn pixels(&self) -> Result<(Vec<u8>, image::ExtendedColorType), ScanError> {
        let data = raw::normalize(
            &self.raw_data,
            self.width,
            self.height,
            self.bits_per_pixel,
            self.bytes_per_row,
        )?;

        let color_type = match self.bits_per_pixel {
            1 | 8 => image::ExtendedColorType::L8,
            24 => image::ExtendedColorType::Rgb8,
            32 => image::ExtendedColorType::Rgba8,
            other => {
                return Err(ScanError::ImageConversion(format!(
                    "Unsupported bit depth: {}",
                    other
                )))
            }
        };

        Ok((data, color_type))
    }

    /// Convert raw bitmap data to PNG bytes
    pub fn to_png(&self) -> Result<Vec<u8>, ScanError> {
        use image::ImageEncoder;

        let (data, color_type) = self.pixels()?;

        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut buf))
            .write_image(&data, self.width, self.height, color_type)
            .map_err(|e: image::ImageError| ScanError::ImageConversion(e.to_string()))?;

        Ok(buf)
    }

    /// Convert raw bitmap data to JPEG bytes
    pub fn to_jpeg(&self, quality: u8) -> Result<Vec<u8>, ScanError> {
        use image::ImageEncoder;

        let (data, color_type) = self.pixels()?;

        // JPEG carries no alpha channel.
        if matches!(color_type, image::ExtendedColorType::Rgba8) {
            return Err(ScanError::ImageConversion(
                "JPEG unsupported for 32 bpp".to_string(),
            ));
        }

        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut buf), quality)
            .write_image(&data, self.width, self.height, color_type)
            .map_err(|e: image::ImageError| ScanError::ImageConversion(e.to_string()))?;

        Ok(buf)
    }
}

#[derive(Error, Debug)]
pub enum ScanError {
    #[error("TWAIN error: {0}")]
    Twain(#[from] twain::TwainError),

    #[error("Sidecar error: {0}")]
    Sidecar(String),

    #[error("No scanners found")]
    NoScanners,

    #[error("Scanner not found: {0}")]
    ScannerNotFound(String),

    #[error("Scan cancelled")]
    Cancelled,

    #[error("Image conversion error: {0}")]
    ImageConversion(String),

    #[error("PDF generation error: {0}")]
    PdfGeneration(String),
}

// Scan Orchestrator

/// Orchestrates scanning operations, trying native 64-bit TWAIN first
/// and falling back to the 32-bit sidecar.
pub struct ScanOrchestrator {
    /// Cached list of discovered scanners
    scanners: Vec<ScannerInfo>,
    /// Whether native TWAIN is available
    native_available: bool,
    /// Whether the sidecar is available
    sidecar_available: bool,
    /// Path to the 32-bit sidecar executable (None if unavailable)
    sidecar_path: Option<String>,
}

impl ScanOrchestrator {
    pub fn new(sidecar_path: Option<String>) -> Self {
        Self {
            scanners: Vec::new(),
            native_available: false,
            sidecar_available: false,
            sidecar_path,
        }
    }

    /// Discover all available scanners (both native and sidecar)
    pub fn discover_scanners(&mut self) -> Result<Vec<ScannerInfo>, ScanError> {
        let mut all_scanners = Vec::new();

        // Try native 64-bit TWAIN
        match self.discover_native_scanners() {
            Ok(scanners) => {
                self.native_available = true;
                info!("Found {} native TWAIN scanner(s)", scanners.len());
                all_scanners.extend(scanners);
            }
            Err(e) => {
                warn!("Native TWAIN not available: {}", e);
                self.native_available = false;
            }
        }

        // Try 32-bit sidecar (if native found no sources, or always for completeness)
        match self.discover_sidecar_scanners() {
            Ok(scanners) => {
                self.sidecar_available = true;
                // Only add scanners not already found natively (by name)
                for scanner in scanners {
                    if !all_scanners.iter().any(|s| s.name == scanner.name) {
                        info!("Found sidecar-only scanner: {}", scanner.name);
                        all_scanners.push(scanner);
                    }
                }
            }
            Err(e) => {
                warn!("Sidecar not available: {}", e);
                self.sidecar_available = false;
            }
        }

        self.scanners = all_scanners.clone();
        Ok(all_scanners)
    }

    /// Resolve which scanner to use based on request options.
    /// Returns the scanner info without starting a scan.
    pub fn resolve_scanner(&self, options: &ScanRequestOptions) -> Result<ScannerInfo, ScanError> {
        let scanner_name = match &options.scanner_id {
            Some(name) if !name.is_empty() => name.clone(),
            _ => self
                .scanners
                .first()
                .map(|s| s.name.clone())
                .ok_or(ScanError::NoScanners)?,
        };

        self.scanners
            .iter()
            .find(|s| s.name == scanner_name || s.id == scanner_name)
            .cloned()
            .ok_or(ScanError::ScannerNotFound(scanner_name))
    }

    fn discover_native_scanners(&self) -> Result<Vec<ScannerInfo>, ScanError> {
        let pre = twain::PreSession::new();
        let dsm_loaded = pre.load_dsm()?;
        let hwnd = twain::create_hidden_hwnd()?;
        let mut dsm_opened = dsm_loaded.open_dsm(hwnd)?;

        let sources = dsm_opened.list_sources()?;
        let scanners: Vec<ScannerInfo> = sources
            .into_iter()
            .map(|s| ScannerInfo {
                id: s.id.to_string(),
                name: s.name.clone(),
                manufacturer: s.manufacturer.clone(),
                source: ScannerSource::Native,
            })
            .collect();

        // Close DSM cleanly
        let _ = dsm_opened.close_dsm();

        Ok(scanners)
    }

    fn discover_sidecar_scanners(&self) -> Result<Vec<ScannerInfo>, ScanError> {
        let sidecar_path = match &self.sidecar_path {
            Some(p) => p.clone(),
            None => return Ok(Vec::new()), // No sidecar available
        };

        let mut manager = sidecar::SidecarManager::new_inheriting_env(sidecar_path);
        let scanners = manager.list_scanners()?;
        manager.shutdown();
        Ok(scanners)
    }

    /// Get the sidecar path (for passing to execute_sidecar_scan)
    pub fn sidecar_path(&self) -> Option<&str> {
        self.sidecar_path.as_deref()
    }
}

// Scan Execution (standalone, does not hold orchestrator lock)

/// Consume scanned pages, emit progress and page messages, and assemble a PDF
/// when one was requested.
///
/// Shared by the native and sidecar scan paths, which differ only in how pages
/// are produced. Returns the page count and, for PDF output, the base64 document.
async fn consume_pages(
    request_id: &str,
    scan_id: &str,
    format: OutputFormat,
    page_rx: &mut mpsc::Receiver<PageData>,
    response_tx: &ResponseSender,
    cancel_flag: &AtomicBool,
) -> Result<(u32, Option<String>), ScanError> {
    let mut page_count = 0u32;
    let mut all_pages: Vec<Vec<u8>> = Vec::new();

    while let Some(page_data) = page_rx.recv().await {
        if cancel_flag.load(Ordering::Acquire) {
            info!("Page processing cancelled for scan {}", scan_id);
            page_rx.close();
            while page_rx.try_recv().is_ok() {}
            break;
        }

        page_count += 1;

        send_json(
            response_tx,
            AgentMessage::ScanProgress {
                id: request_id.to_string(),
                scan_id: scan_id.to_string(),
                page: page_count,
                status: ScanStatus::Scanning,
            },
        )
        .await;

        match format {
            OutputFormat::Png => {
                let png_data = page_data.to_png()?;
                let encoded =
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png_data);
                send_json(
                    response_tx,
                    AgentMessage::ScanPage {
                        id: request_id.to_string(),
                        scan_id: scan_id.to_string(),
                        page: page_count,
                        data: encoded,
                        mime: "image/png".to_string(),
                    },
                )
                .await;
            }
            OutputFormat::Jpeg => {
                let jpeg_data = page_data.to_jpeg(85)?;
                let encoded =
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &jpeg_data);
                send_json(
                    response_tx,
                    AgentMessage::ScanPage {
                        id: request_id.to_string(),
                        scan_id: scan_id.to_string(),
                        page: page_count,
                        data: encoded,
                        mime: "image/jpeg".to_string(),
                    },
                )
                .await;
            }
            OutputFormat::Pdf => {
                let png_data = page_data.to_png()?;
                all_pages.push(png_data);

                let preview = page_data.to_jpeg(60)?;
                let encoded =
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &preview);
                send_json(
                    response_tx,
                    AgentMessage::ScanPage {
                        id: request_id.to_string(),
                        scan_id: scan_id.to_string(),
                        page: page_count,
                        data: encoded,
                        mime: "image/jpeg".to_string(),
                    },
                )
                .await;
            }
        }
    }

    let pdf_data = if matches!(format, OutputFormat::Pdf) && !all_pages.is_empty() {
        send_json(
            response_tx,
            AgentMessage::ScanProgress {
                id: request_id.to_string(),
                scan_id: scan_id.to_string(),
                page: page_count,
                status: ScanStatus::Processing,
            },
        )
        .await;

        match crate::pdf::generate_pdf(&all_pages) {
            Ok(pdf_bytes) => Some(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &pdf_bytes,
            )),
            Err(e) => {
                error!("PDF generation failed: {}", e);
                None
            }
        }
    } else {
        None
    };

    Ok((page_count, pdf_data))
}

/// Execute a native TWAIN scan and stream results back via the provided sender.
///
/// This function does NOT hold any mutex during the scan. The caller is responsible
/// for setting/clearing the concurrency guard.
pub async fn execute_native_scan(
    request_id: String,
    scan_id: String,
    scanner_name: &str,
    options: &ScanRequestOptions,
    response_tx: ResponseSender,
    cancel_flag: Arc<AtomicBool>,
) -> Result<(), ScanError> {
    let scanner_name = scanner_name.to_string();
    let options_clone = twain::ScanOptions {
        resolution: options.resolution,
        color_mode: options.color_mode,
        duplex: options.duplex,
        use_adf: options.use_adf,
        show_scanner_ui: options.show_scanner_ui,
    };
    let format = options.format;
    let cancel_for_thread = cancel_flag.clone();

    // TWAIN operations must happen on a dedicated thread (not a tokio task)
    // because TWAIN uses Windows message pumping which blocks
    let (page_tx, mut page_rx) = mpsc::channel::<PageData>(4);

    let scan_thread = std::thread::spawn(move || -> Result<(), ScanError> {
        let pre = twain::PreSession::new();
        let dsm_loaded = pre.load_dsm()?;
        let hwnd = twain::create_hidden_hwnd()?;
        let dsm_opened = dsm_loaded.open_dsm(hwnd)?;

        let mut source_opened = dsm_opened.open_source(&scanner_name)?;
        source_opened.configure(&options_clone)?;

        let source_enabled = source_opened.enable(options_clone.show_scanner_ui)?;

        // Wait for transfer ready (passes cancel flag for polling)
        match source_enabled.wait_for_transfer(Some(&cancel_for_thread))? {
            twain::WaitResult::TransferReady(transfer_ready) => {
                let mut page_num = 1u32;
                let mut current_transfer = transfer_ready;

                loop {
                    // Check for cancellation between page transfers
                    if cancel_for_thread.load(Ordering::Acquire) {
                        info!("Scan cancelled by user between transfers");
                        let source = current_transfer.cancel()?;
                        let dsm = source.close()?;
                        let _ = dsm.close_dsm();
                        return Err(ScanError::Cancelled);
                    }

                    match current_transfer.transfer_memory()? {
                        twain::TransferResult::MorePages { page, next } => {
                            let page_data = PageData {
                                page_number: page_num,
                                width: page.width,
                                height: page.height,
                                bits_per_pixel: page.bits_per_pixel,
                                dpi_x: page.x_resolution,
                                dpi_y: page.y_resolution,
                                bytes_per_row: page.bytes_per_row,
                                raw_data: page.data,
                            };
                            let _ = page_tx.blocking_send(page_data);
                            page_num += 1;
                            current_transfer = next;
                        }
                        twain::TransferResult::Done { page, source } => {
                            let page_data = PageData {
                                page_number: page_num,
                                width: page.width,
                                height: page.height,
                                bits_per_pixel: page.bits_per_pixel,
                                dpi_x: page.x_resolution,
                                dpi_y: page.y_resolution,
                                bytes_per_row: page.bytes_per_row,
                                raw_data: page.data,
                            };
                            let _ = page_tx.blocking_send(page_data);

                            // Close source cleanly
                            let dsm = source.close()?;
                            let _ = dsm.close_dsm();
                            break;
                        }
                    }
                }
            }
            twain::WaitResult::CloseRequested(source) => {
                let dsm = source.close()?;
                let _ = dsm.close_dsm();
            }
        }

        Ok(())
    });

    let (page_count, pdf_data) = consume_pages(
        &request_id,
        &scan_id,
        format,
        &mut page_rx,
        &response_tx,
        &cancel_flag,
    )
    .await?;

    // Wait for scan thread to complete
    match scan_thread.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            error!("Scan thread error: {}", e);
            return Err(e);
        }
        Err(_) => {
            error!("Scan thread panicked");
            return Err(ScanError::Twain(twain::TwainError::InvalidState));
        }
    }

    send_json(
        &response_tx,
        AgentMessage::ScanComplete {
            id: request_id,
            scan_id,
            total_pages: page_count,
            pdf_data,
        },
    )
    .await;

    Ok(())
}

/// Execute a scan via the 32-bit sidecar and stream results back via the provided sender.
///
/// Mirrors the structure of `execute_native_scan`: blocking I/O runs in `spawn_blocking`,
/// page data flows through an mpsc channel, and the async side converts + sends over WebSocket.
pub async fn execute_sidecar_scan(
    request_id: String,
    scan_id: String,
    scanner_name: &str,
    options: &ScanRequestOptions,
    sidecar_path: &str,
    response_tx: ResponseSender,
    cancel_flag: Arc<AtomicBool>,
) -> Result<(), ScanError> {
    let scanner_name = scanner_name.to_string();
    let sidecar_path = sidecar_path.to_string();
    let color_mode_str = match options.color_mode {
        twain::ColorMode::Color => "color",
        twain::ColorMode::Grayscale => "grayscale",
        twain::ColorMode::BlackWhite => "bw",
    }
    .to_string();
    let resolution = options.resolution;
    let duplex = options.duplex;
    let use_adf = options.use_adf;
    let show_ui = options.show_scanner_ui;
    let format = options.format;
    let cancel_for_blocking = cancel_flag.clone();

    let (page_tx, mut page_rx) = mpsc::channel::<PageData>(4);

    // Run sidecar I/O in a blocking task (SidecarManager uses blocking I/O)
    let sidecar_task = tokio::task::spawn_blocking(move || -> Result<(), ScanError> {
        let mut manager = sidecar::SidecarManager::new_inheriting_env(sidecar_path);
        manager.ensure_running()?;
        manager.start_scan(
            &scanner_name,
            resolution,
            &color_mode_str,
            duplex,
            use_adf,
            show_ui,
        )?;

        // Read responses in a loop
        loop {
            // Check for cancel before reading next response
            if cancel_for_blocking.load(Ordering::Acquire) {
                info!("Sidecar scan cancelled, sending Cancel to sidecar");
                let _ = manager.send_cancel();
                // Drain remaining responses until terminal
                loop {
                    match manager.read_response() {
                        Ok(ref resp) if is_sidecar_terminal(resp) => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
                manager.shutdown();
                return Err(ScanError::Cancelled);
            }

            match manager.read_response() {
                Ok(sidecar::SidecarResponse::ScanPage {
                    page,
                    width,
                    height,
                    bits_per_pixel,
                    bytes_per_row,
                    data,
                }) => {
                    // Decode base64 to raw bytes
                    let raw_data = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        &data,
                    )
                    .map_err(|e| {
                        ScanError::Sidecar(format!("Base64 decode error: {}", e))
                    })?;

                    let page_data = PageData {
                        page_number: page,
                        width,
                        height,
                        bits_per_pixel,
                        dpi_x: resolution as f32,
                        dpi_y: resolution as f32,
                        bytes_per_row,
                        raw_data,
                    };
                    let _ = page_tx.blocking_send(page_data);
                }
                Ok(sidecar::SidecarResponse::ScanProgress { .. }) => {
                    // Progress is handled by the async side when it receives PageData
                    continue;
                }
                Ok(sidecar::SidecarResponse::ScanComplete { .. }) => {
                    break;
                }
                Ok(sidecar::SidecarResponse::Error { message }) => {
                    manager.shutdown();
                    return Err(ScanError::Sidecar(message));
                }
                Ok(_) => {
                    // Unexpected response type, continue
                    continue;
                }
                Err(e) => {
                    manager.shutdown();
                    return Err(e);
                }
            }
        }

        manager.shutdown();
        Ok(())
    });

    let (page_count, pdf_data) = consume_pages(
        &request_id,
        &scan_id,
        format,
        &mut page_rx,
        &response_tx,
        &cancel_flag,
    )
    .await?;

    // Wait for sidecar task to complete
    match sidecar_task.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            error!("Sidecar scan error: {}", e);
            return Err(e);
        }
        Err(e) => {
            error!("Sidecar task panicked: {}", e);
            return Err(ScanError::Sidecar("Sidecar task panicked".into()));
        }
    }

    send_json(
        &response_tx,
        AgentMessage::ScanComplete {
            id: request_id,
            scan_id,
            total_pages: page_count,
            pdf_data,
        },
    )
    .await;

    Ok(())
}

/// Check if a sidecar response is terminal (scan finished or errored)
fn is_sidecar_terminal(resp: &sidecar::SidecarResponse) -> bool {
    matches!(
        resp,
        sidecar::SidecarResponse::ScanComplete { .. }
            | sidecar::SidecarResponse::Error { .. }
            | sidecar::SidecarResponse::Shutdown
    )
}

#[cfg(test)]
mod page_data_tests {
    use super::PageData;

    fn padded_24bpp_page(width: u32, height: u32, stride: u32) -> PageData {
        PageData {
            page_number: 1,
            width,
            height,
            bits_per_pixel: 24,
            dpi_x: 300.0,
            dpi_y: 300.0,
            bytes_per_row: stride,
            raw_data: vec![0x40; stride as usize * height as usize],
        }
    }

    #[test]
    fn to_png_handles_dword_padded_rows() {
        // 2550px @ 24bpp is a 7650-byte row, padded to 7652 by most sources.
        let page = padded_24bpp_page(2550, 4, 7652);
        assert!(page.to_png().is_ok());
    }

    #[test]
    fn to_jpeg_handles_dword_padded_rows() {
        let page = padded_24bpp_page(2550, 4, 7652);
        assert!(page.to_jpeg(85).is_ok());
    }

    #[test]
    fn to_png_converts_1bpp_black_and_white() {
        let page = PageData {
            page_number: 1,
            width: 16,
            height: 2,
            bits_per_pixel: 1,
            dpi_x: 300.0,
            dpi_y: 300.0,
            bytes_per_row: 4,
            raw_data: vec![0b1010_1010, 0b1100_1100, 0xAA, 0xAA,
                           0b1111_0000, 0b0000_1111, 0xAA, 0xAA],
        };
        assert!(page.to_png().is_ok());
    }

    #[test]
    fn to_png_reports_error_rather_than_panicking_on_truncated_data() {
        let mut page = padded_24bpp_page(2550, 4, 7652);
        page.raw_data.truncate(100);
        assert!(page.to_png().is_err());
    }
}

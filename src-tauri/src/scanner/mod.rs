//! Scanner abstraction layer.
//!
//! Defines the `Scanner` trait and the `ScanOrchestrator` which tries native 64-bit
//! TWAIN first and falls back to the 32-bit sidecar if no sources are found.

pub mod sidecar;
pub mod twain;
pub mod twain_ffi;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ::serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::protocol::{AgentMessage, OutputFormat, ScanRequestOptions, ScanStatus};

// ---------------------------------------------------------------------------
// Scanner trait
// ---------------------------------------------------------------------------

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
    pub raw_data: Vec<u8>,
}

impl PageData {
    /// Convert raw bitmap data to PNG bytes
    pub fn to_png(&self) -> Result<Vec<u8>, ScanError> {
        use image::ImageEncoder;

        let color_type = match self.bits_per_pixel {
            1 | 8 => image::ExtendedColorType::L8,
            24 => image::ExtendedColorType::Rgb8,
            32 => image::ExtendedColorType::Rgba8,
            _ => {
                return Err(ScanError::ImageConversion(format!(
                    "Unsupported bit depth: {}",
                    self.bits_per_pixel
                )))
            }
        };

        let mut buf = Vec::new();
        let cursor = std::io::Cursor::new(&mut buf);

        let encoder = image::codecs::png::PngEncoder::new(cursor);
        encoder
            .write_image(&self.raw_data, self.width, self.height, color_type)
            .map_err(|e: image::ImageError| ScanError::ImageConversion(e.to_string()))?;

        Ok(buf)
    }

    /// Convert raw bitmap data to JPEG bytes
    pub fn to_jpeg(&self, quality: u8) -> Result<Vec<u8>, ScanError> {
        use image::ImageEncoder;

        let color_type = match self.bits_per_pixel {
            8 => image::ExtendedColorType::L8,
            24 => image::ExtendedColorType::Rgb8,
            _ => {
                return Err(ScanError::ImageConversion(format!(
                    "JPEG unsupported for {} bpp",
                    self.bits_per_pixel
                )))
            }
        };

        let mut buf = Vec::new();
        let cursor = std::io::Cursor::new(&mut buf);

        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(cursor, quality);
        encoder
            .write_image(&self.raw_data, self.width, self.height, color_type)
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

// ---------------------------------------------------------------------------
// Scan Orchestrator
// ---------------------------------------------------------------------------

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
            .ok_or_else(|| ScanError::ScannerNotFound(scanner_name))
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

        let mut manager = sidecar::SidecarManager::new(sidecar_path);
        let scanners = manager.list_scanners()?;
        manager.shutdown();
        Ok(scanners)
    }

    /// Get the sidecar path (for passing to execute_sidecar_scan)
    pub fn sidecar_path(&self) -> Option<&str> {
        self.sidecar_path.as_deref()
    }
}

// ---------------------------------------------------------------------------
// Scan Execution (standalone, does not hold orchestrator lock)
// ---------------------------------------------------------------------------

/// Execute a native TWAIN scan and stream results back via the provided sender.
///
/// This function does NOT hold any mutex during the scan. The caller is responsible
/// for setting/clearing the concurrency guard.
pub async fn execute_native_scan(
    request_id: String,
    scan_id: String,
    scanner_name: &str,
    options: &ScanRequestOptions,
    response_tx: mpsc::UnboundedSender<AgentMessage>,
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

    // Process pages as they arrive from the scan thread
    let mut page_count = 0u32;
    let mut all_pages: Vec<Vec<u8>> = Vec::new();

    while let Some(page_data) = page_rx.recv().await {
        // Check for cancellation before processing
        if cancel_flag.load(Ordering::Acquire) {
            info!("Page processing cancelled for scan {}", scan_id);
            // Drain buffered pages to free memory and unblock scan thread
            page_rx.close();
            while page_rx.try_recv().is_ok() {}
            break;
        }

        page_count += 1;

        // Send progress
        let _ = response_tx.send(AgentMessage::ScanProgress {
            id: request_id.clone(),
            scan_id: scan_id.clone(),
            page: page_count,
            status: ScanStatus::Scanning,
        });

        // Convert page based on requested format
        match format {
            OutputFormat::Png => {
                let png_data = page_data.to_png()?;
                let encoded = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &png_data,
                );

                let _ = response_tx.send(AgentMessage::ScanPage {
                    id: request_id.clone(),
                    scan_id: scan_id.clone(),
                    page: page_count,
                    data: encoded,
                    mime: "image/png".to_string(),
                });
            }
            OutputFormat::Jpeg => {
                let jpeg_data = page_data.to_jpeg(85)?;
                let encoded = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &jpeg_data,
                );

                let _ = response_tx.send(AgentMessage::ScanPage {
                    id: request_id.clone(),
                    scan_id: scan_id.clone(),
                    page: page_count,
                    data: encoded,
                    mime: "image/jpeg".to_string(),
                });
            }
            OutputFormat::Pdf => {
                // Collect pages for PDF generation at the end
                let png_data = page_data.to_png()?;
                all_pages.push(png_data);

                // Still send individual page previews
                let preview = page_data.to_jpeg(60)?;
                let encoded = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &preview,
                );

                let _ = response_tx.send(AgentMessage::ScanPage {
                    id: request_id.clone(),
                    scan_id: scan_id.clone(),
                    page: page_count,
                    data: encoded,
                    mime: "image/jpeg".to_string(),
                });
            }
        }
    }

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

    // Generate PDF if requested
    let pdf_data = if matches!(format, OutputFormat::Pdf) && !all_pages.is_empty() {
        let _ = response_tx.send(AgentMessage::ScanProgress {
            id: request_id.clone(),
            scan_id: scan_id.clone(),
            page: page_count,
            status: ScanStatus::Processing,
        });

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

    // Send completion
    let _ = response_tx.send(AgentMessage::ScanComplete {
        id: request_id,
        scan_id,
        total_pages: page_count,
        pdf_data,
    });

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
    response_tx: mpsc::UnboundedSender<AgentMessage>,
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
        let mut manager = sidecar::SidecarManager::new(sidecar_path);
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

    // Process pages as they arrive (same pipeline as execute_native_scan)
    let mut page_count = 0u32;
    let mut all_pages: Vec<Vec<u8>> = Vec::new();

    while let Some(page_data) = page_rx.recv().await {
        if cancel_flag.load(Ordering::Acquire) {
            info!("Page processing cancelled for sidecar scan {}", scan_id);
            page_rx.close();
            while page_rx.try_recv().is_ok() {}
            break;
        }

        page_count += 1;

        let _ = response_tx.send(AgentMessage::ScanProgress {
            id: request_id.clone(),
            scan_id: scan_id.clone(),
            page: page_count,
            status: ScanStatus::Scanning,
        });

        match format {
            OutputFormat::Png => {
                let png_data = page_data.to_png()?;
                let encoded = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &png_data,
                );
                let _ = response_tx.send(AgentMessage::ScanPage {
                    id: request_id.clone(),
                    scan_id: scan_id.clone(),
                    page: page_count,
                    data: encoded,
                    mime: "image/png".to_string(),
                });
            }
            OutputFormat::Jpeg => {
                let jpeg_data = page_data.to_jpeg(85)?;
                let encoded = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &jpeg_data,
                );
                let _ = response_tx.send(AgentMessage::ScanPage {
                    id: request_id.clone(),
                    scan_id: scan_id.clone(),
                    page: page_count,
                    data: encoded,
                    mime: "image/jpeg".to_string(),
                });
            }
            OutputFormat::Pdf => {
                let png_data = page_data.to_png()?;
                all_pages.push(png_data);

                let preview = page_data.to_jpeg(60)?;
                let encoded = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &preview,
                );
                let _ = response_tx.send(AgentMessage::ScanPage {
                    id: request_id.clone(),
                    scan_id: scan_id.clone(),
                    page: page_count,
                    data: encoded,
                    mime: "image/jpeg".to_string(),
                });
            }
        }
    }

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

    // Generate PDF if requested
    let pdf_data = if matches!(format, OutputFormat::Pdf) && !all_pages.is_empty() {
        let _ = response_tx.send(AgentMessage::ScanProgress {
            id: request_id.clone(),
            scan_id: scan_id.clone(),
            page: page_count,
            status: ScanStatus::Processing,
        });

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

    let _ = response_tx.send(AgentMessage::ScanComplete {
        id: request_id,
        scan_id,
        total_pages: page_count,
        pdf_data,
    });

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

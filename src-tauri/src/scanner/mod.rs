//! Scanner abstraction layer.
//!
//! Defines the `Scanner` trait and the `ScanOrchestrator` which tries native 64-bit
//! TWAIN first and falls back to the 32-bit sidecar if no sources are found.

pub mod raw;
pub mod sidecar;
pub mod thumbnail;
pub mod twain;
pub mod twain_ffi;

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ::serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::protocol::{AgentMessage, OutputFormat, ScanRequestOptions, ScanStatus};
use crate::ws_server::{send_binary, send_json, ResponseSender};

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

    /// Tightly-packed 8-bit samples for this page, for callers that need the
    /// pixels rather than an encoded image.
    pub fn normalized(&self) -> Result<Vec<u8>, ScanError> {
        Ok(self.pixels()?.0)
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

/// How often the cancel flag is re-checked while a queued send waits on a
/// client that has stopped reading.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Await `future`, abandoning it if the cancel flag is raised first.
///
/// The response channel is bounded, so a client that stops reading parks the
/// scan inside a send. Polling the flag alongside the send is what keeps a
/// later `cancel_scan` from having to wait for that client to drain the queue.
async fn until_cancelled<F>(future: F, cancel_flag: &AtomicBool) -> Result<F::Output, ScanError>
where
    F: Future,
{
    tokio::pin!(future);
    loop {
        tokio::select! {
            // Making progress on the send beats re-checking the flag.
            biased;
            output = &mut future => return Ok(output),
            _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => {
                if cancel_flag.load(Ordering::Acquire) {
                    return Err(ScanError::Cancelled);
                }
            }
        }
    }
}

/// Where one scan's output goes, and whether the user has asked it to stop.
///
/// Bundling these keeps the transfer helpers to a readable argument list and
/// means no send can accidentally skip the cancellation race.
struct PageSink<'a> {
    response_tx: &'a ResponseSender,
    request_id: &'a str,
    scan_id: &'a str,
    cancel_flag: &'a AtomicBool,
}

impl PageSink<'_> {
    async fn progress(&self, page: u32, status: ScanStatus) -> Result<(), ScanError> {
        until_cancelled(
            send_json(
                self.response_tx,
                AgentMessage::ScanProgress {
                    id: self.request_id.to_string(),
                    scan_id: self.scan_id.to_string(),
                    page,
                    status,
                },
            ),
            self.cancel_flag,
        )
        .await
    }

    /// Announce a payload and stream it as binary frames.
    ///
    /// A cancellation observed mid-transfer abandons the remaining frames:
    /// the scan is about to fail with `Cancelled`, so the client has no use
    /// for the rest of the bytes.
    async fn transfer(
        &self,
        kind: &str,
        page: Option<u32>,
        mime: &str,
        bytes: &[u8],
    ) -> Result<(), ScanError> {
        until_cancelled(
            send_json(
                self.response_tx,
                AgentMessage::BinaryStart {
                    id: self.request_id.to_string(),
                    scan_id: self.scan_id.to_string(),
                    kind: kind.to_string(),
                    page,
                    mime: mime.to_string(),
                    total_bytes: bytes.len(),
                },
            ),
            self.cancel_flag,
        )
        .await?;

        until_cancelled(send_binary(self.response_tx, bytes), self.cancel_flag)
            .await?
            .map_err(|_| ScanError::Sidecar("client disconnected during transfer".to_string()))
    }
}

/// A document written page by page but not yet closed.
///
/// Fields drop in declaration order, so the writer releases its file handle
/// before `dir` removes the directory — Windows refuses to unlink a directory
/// that still holds an open handle.
struct PendingPdf {
    writer: crate::pdf::PdfWriter,
    path: std::path::PathBuf,
    /// Removed when this drops, on every exit path including a panic.
    dir: tempfile::TempDir,
}

/// Consume scanned pages, streaming each one out as it arrives.
///
/// Shared by the native and sidecar scan paths, which differ only in how
/// pages are produced. Returns the page count and, for PDF output, the
/// still-open document: closing and sending it is the caller's job, because
/// neither may happen until the producer is known to have succeeded.
async fn consume_pages(
    sink: &PageSink<'_>,
    format: OutputFormat,
    page_rx: &mut mpsc::Receiver<PageData>,
) -> Result<(u32, Option<PendingPdf>), ScanError> {
    let mut page_count = 0u32;

    // PDF output writes to a temp file as pages arrive, so a batch never holds
    // more than the page in hand.
    let mut pdf = if matches!(format, OutputFormat::Pdf) {
        let dir =
            tempfile::tempdir().map_err(|e| ScanError::PdfGeneration(format!("temp dir: {e}")))?;
        let path = dir.path().join("scan.pdf");
        let writer = crate::pdf::PdfWriter::create(&path)
            .map_err(|e| ScanError::PdfGeneration(e.to_string()))?;
        Some(PendingPdf { writer, path, dir })
    } else {
        None
    };

    while let Some(page_data) = page_rx.recv().await {
        if sink.cancel_flag.load(Ordering::Acquire) {
            info!("Page processing cancelled for scan {}", sink.scan_id);
            page_rx.close();
            while page_rx.try_recv().is_ok() {}
            break;
        }

        page_count += 1;

        sink.progress(page_count, ScanStatus::Scanning).await?;

        let channels = match page_data.bits_per_pixel {
            1 | 8 => 1u8,
            24 => 3,
            32 => 4,
            other => {
                return Err(ScanError::ImageConversion(format!(
                    "Unsupported bit depth: {other}"
                )))
            }
        };

        match format {
            OutputFormat::Png => {
                let bytes = page_data.to_png()?;
                sink.transfer("page", Some(page_count), "image/png", &bytes)
                    .await?;
            }
            OutputFormat::Jpeg => {
                let bytes = page_data.to_jpeg(85)?;
                sink.transfer("page", Some(page_count), "image/jpeg", &bytes)
                    .await?;
            }
            OutputFormat::Pdf => {
                // Each intermediate buffer is scoped, so the full-resolution
                // page is never alive alongside the next one or across a send.
                {
                    let jpeg = page_data.to_jpeg(85)?;
                    if let Some(pending) = pdf.as_mut() {
                        pending
                            .writer
                            .add_page(crate::pdf::PageSpec {
                                jpeg: &jpeg,
                                width_px: page_data.width,
                                height_px: page_data.height,
                                dpi_x: page_data.dpi_x,
                                dpi_y: page_data.dpi_y,
                                // JPEG embedding is grayscale or RGB; RGBA was
                                // flattened during encoding.
                                channels: if channels == 1 { 1 } else { 3 },
                            })
                            .map_err(|e| ScanError::PdfGeneration(e.to_string()))?;
                    }
                }

                let thumb = {
                    let pixels = page_data.normalized()?;
                    thumbnail::thumbnail_jpeg(&pixels, page_data.width, page_data.height, channels)?
                };
                sink.transfer("thumbnail", Some(page_count), "image/jpeg", &thumb)
                    .await?;
            }
        }
    }

    Ok((page_count, pdf))
}

/// Close a document and hand it to the client.
///
/// Deliberately separate from `consume_pages`: the caller checks the
/// producer's result first, so a failure that surfaces after the last page
/// costs the client neither a `Processing` message nor a document that is
/// about to be thrown away.
async fn finalize_pdf(
    sink: &PageSink<'_>,
    page_count: u32,
    pending: PendingPdf,
) -> Result<(), ScanError> {
    // `pending` is deliberately kept whole across both fallible steps below.
    // Taking it apart first would put the pieces in local bindings, which drop
    // in *reverse* declaration order — dropping the directory while the writer
    // still holds the file open. Windows then refuses to remove the directory
    // and `TempDir::drop` swallows the failure, stranding a half-written scan
    // in %TEMP%. As one value it drops by field order instead: writer first.
    if pending.writer.page_count() == 0 {
        return Ok(());
    }

    sink.progress(page_count, ScanStatus::Processing).await?;

    // Past this point the only exit is through `finish`, which consumes the
    // writer and closes the file, so the pieces are safe to separate.
    let PendingPdf { writer, path, dir } = pending;

    writer
        .finish()
        .map_err(|e| ScanError::PdfGeneration(e.to_string()))?;

    // One buffer, once, bounded by the document size — not by page count.
    let bytes =
        std::fs::read(&path).map_err(|e| ScanError::PdfGeneration(format!("read back: {e}")))?;
    let sent = sink.transfer("pdf", None, "application/pdf", &bytes).await;

    // Explicit, so the directory outlives the read-back above.
    drop(dir);
    sent
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

    let sink = PageSink {
        response_tx: &response_tx,
        request_id: &request_id,
        scan_id: &scan_id,
        cancel_flag: &cancel_flag,
    };

    let (page_count, pending_pdf) = consume_pages(&sink, format, &mut page_rx).await?;

    // The producer's verdict gates everything below: finishing and sending a
    // document the scan is about to report as failed wastes the work and tells
    // the client a story the error then contradicts.
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

    if let Some(pending) = pending_pdf {
        finalize_pdf(&sink, page_count, pending).await?;
    }

    // Raced against cancellation like every other send, so a client that has
    // stopped reading cannot pin the scanner open.
    let _ = until_cancelled(
        send_json(
            &response_tx,
            AgentMessage::ScanComplete {
                id: request_id,
                scan_id,
                total_pages: page_count,
            },
        ),
        &cancel_flag,
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

    let sink = PageSink {
        response_tx: &response_tx,
        request_id: &request_id,
        scan_id: &scan_id,
        cancel_flag: &cancel_flag,
    };

    let (page_count, pending_pdf) = consume_pages(&sink, format, &mut page_rx).await?;

    // As in the native path: the producer's verdict comes before any
    // finalization, so a late failure costs no PDF work and no `Processing`.
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

    if let Some(pending) = pending_pdf {
        finalize_pdf(&sink, page_count, pending).await?;
    }

    let _ = until_cancelled(
        send_json(
            &response_tx,
            AgentMessage::ScanComplete {
                id: request_id,
                scan_id,
                total_pages: page_count,
            },
        ),
        &cancel_flag,
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


#[cfg(test)]
mod transfer_tests {
    use super::*;
    use crate::ws_server::{OutgoingMessage, CHUNK_BYTES, RESPONSE_CHANNEL_CAPACITY};

    /// The regression this guards: the response channel is bounded, so a
    /// client that stops reading parks the scan inside a send. Before the
    /// send was raced against the flag, a `cancel_scan` could not take effect
    /// until that client drained the queue — which it may never do.
    #[tokio::test]
    async fn a_send_blocked_on_a_full_queue_still_observes_cancellation() {
        let (tx, _rx) = mpsc::channel::<OutgoingMessage>(RESPONSE_CHANNEL_CAPACITY);

        // `_rx` is deliberately never read, so once the queue is full every
        // further send blocks for as long as the test cares to wait.
        for _ in 0..RESPONSE_CHANNEL_CAPACITY {
            tx.send(OutgoingMessage::Binary(vec![0])).await.unwrap();
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let raise = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            raise.store(true, Ordering::Release);
        });

        let sink = PageSink {
            response_tx: &tx,
            request_id: "req-1",
            scan_id: "scan-1",
            cancel_flag: &cancel,
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            sink.transfer("page", Some(1), "image/png", &[7u8; 64]),
        )
        .await
        .expect("a blocked transfer must give up once cancellation is raised");

        assert!(
            matches!(result, Err(ScanError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );
    }

    /// The other half of that race: nothing must be abandoned while the
    /// client is keeping up, and the announced length must be the real one.
    #[tokio::test]
    async fn an_uncancelled_transfer_announces_and_delivers_every_byte() {
        let (tx, mut rx) = mpsc::channel::<OutgoingMessage>(RESPONSE_CHANNEL_CAPACITY);

        let reader = tokio::spawn(async move {
            let mut announced = None;
            let mut bytes = Vec::new();
            while let Some(msg) = rx.recv().await {
                match msg {
                    OutgoingMessage::Json(m) => {
                        if let AgentMessage::BinaryStart {
                            kind, total_bytes, ..
                        } = *m
                        {
                            announced = Some((kind, total_bytes));
                        }
                    }
                    OutgoingMessage::Binary(chunk) => bytes.extend_from_slice(&chunk),
                }
            }
            (announced, bytes)
        });

        let payload = vec![3u8; CHUNK_BYTES * 2 + 1];
        let cancel = AtomicBool::new(false);
        {
            let sink = PageSink {
                response_tx: &tx,
                request_id: "req-1",
                scan_id: "scan-1",
                cancel_flag: &cancel,
            };
            sink.transfer("page", Some(1), "image/png", &payload)
                .await
                .expect("an unhindered transfer must succeed");
        }
        drop(tx);

        let (announced, bytes) = reader.await.unwrap();
        assert_eq!(announced, Some(("page".to_string(), payload.len())));
        assert_eq!(bytes, payload, "the transfer lost or reordered bytes");
    }
}

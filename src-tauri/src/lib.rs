//! Scan Agent library root.
//!
//! This module exposes the core functionality of the scanning agent:
//! - TWAIN scanner integration (`scanner`)
//! - WebSocket communication server (`ws_server`)
//! - Message protocol types (`protocol`)
//! - PDF generation (`pdf`)

pub mod pdf;
pub mod protocol;
pub mod scanner;
pub mod ws_server;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{error, info, warn};

use protocol::{AgentMessage, ClientMessage, ErrorCode, ScanStatus};
use scanner::ScanOrchestrator;
use ws_server::ResponseSender;

/// Tracks state of the currently active scan (if any).
struct ActiveScan {
    scan_id: String,
    cancel_flag: Arc<AtomicBool>,
}

/// Shared state for the command handler — concurrency guard + orchestrator.
struct ScanState {
    /// Whether a scan is currently in progress (fast atomic check).
    scanning: AtomicBool,
    /// Details of the active scan (for cancellation targeting).
    active_scan: Mutex<Option<ActiveScan>>,
    /// The scanner orchestrator (locked briefly for discovery/resolution, not during scans).
    orchestrator: Mutex<ScanOrchestrator>,
}

/// Process incoming WebSocket commands and dispatch to the scanner orchestrator.
///
/// This runs as a long-lived tokio task, reading commands from the WebSocket server
/// and executing scanner operations.
pub async fn command_handler(
    mut command_rx: ws_server::CommandReceiver,
    event_tx: ws_server::EventSender,
) {
    let state = Arc::new(ScanState {
        scanning: AtomicBool::new(false),
        active_scan: Mutex::new(None),
        orchestrator: Mutex::new(ScanOrchestrator::new()),
    });

    info!("Command handler started");

    while let Some((message, response_tx)) = command_rx.recv().await {
        let state = state.clone();
        let event_tx = event_tx.clone();

        tokio::spawn(async move {
            handle_command(message, response_tx, state, event_tx).await;
        });
    }

    info!("Command handler stopped");
}

async fn handle_command(
    message: ClientMessage,
    response_tx: ResponseSender,
    state: Arc<ScanState>,
    _event_tx: ws_server::EventSender,
) {
    match message {
        ClientMessage::Ping { id } => {
            let _ = response_tx.send(AgentMessage::Pong { id });
        }

        ClientMessage::ListScanners { id } => {
            let mut orch = state.orchestrator.lock().await;
            match orch.discover_scanners() {
                Ok(scanners) => {
                    let entries = scanners
                        .into_iter()
                        .map(|s| protocol::ScannerListEntry {
                            id: s.id,
                            name: s.name,
                            manufacturer: s.manufacturer,
                        })
                        .collect();

                    let _ = response_tx.send(AgentMessage::ScannerList {
                        id,
                        scanners: entries,
                    });
                }
                Err(e) => {
                    error!("Failed to list scanners: {}", e);
                    let _ = response_tx.send(AgentMessage::Error {
                        id,
                        code: error_to_code(&e),
                        message: e.to_string(),
                    });
                }
            }
        }

        ClientMessage::StartScan { id, options } => {
            // Fast-reject if a scan is already in progress
            if state
                .scanning
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                let _ = response_tx.send(AgentMessage::Error {
                    id,
                    code: ErrorCode::ScannerBusy,
                    message: "A scan is already in progress".to_string(),
                });
                return;
            }

            let scan_id = uuid::Uuid::new_v4().to_string();
            let cancel_flag = Arc::new(AtomicBool::new(false));

            // Register the active scan
            {
                let mut active = state.active_scan.lock().await;
                *active = Some(ActiveScan {
                    scan_id: scan_id.clone(),
                    cancel_flag: cancel_flag.clone(),
                });
            }

            // Resolve which scanner to use (brief lock, then release)
            let scanner_info = {
                let orch = state.orchestrator.lock().await;
                orch.resolve_scanner(&options)
            };

            let result = match scanner_info {
                Ok(info) => {
                    scanner::execute_native_scan(
                        id.clone(),
                        scan_id.clone(),
                        &info.name,
                        &options,
                        response_tx.clone(),
                        cancel_flag,
                    )
                    .await
                }
                Err(e) => Err(e),
            };

            match result {
                Ok(()) => {
                    info!("Scan {} completed successfully", scan_id);
                }
                Err(e) => {
                    error!("Scan {} failed: {}", scan_id, e);
                    let _ = response_tx.send(AgentMessage::Error {
                        id,
                        code: error_to_code(&e),
                        message: e.to_string(),
                    });
                }
            }

            // Always clear scanning state
            {
                let mut active = state.active_scan.lock().await;
                *active = None;
            }
            state.scanning.store(false, Ordering::Release);
        }

        ClientMessage::CancelScan { id, scan_id } => {
            let active = state.active_scan.lock().await;
            match &*active {
                Some(active_scan) if active_scan.scan_id == scan_id => {
                    info!("Cancelling scan {}", scan_id);
                    active_scan.cancel_flag.store(true, Ordering::Release);
                    let _ = response_tx.send(AgentMessage::ScanProgress {
                        id,
                        scan_id,
                        page: 0,
                        status: ScanStatus::Complete,
                    });
                }
                _ => {
                    warn!("Cancel requested for unknown scan: {}", scan_id);
                    let _ = response_tx.send(AgentMessage::Error {
                        id,
                        code: ErrorCode::InvalidRequest,
                        message: format!("No active scan with id: {}", scan_id),
                    });
                }
            }
        }
    }
}

fn error_to_code(e: &scanner::ScanError) -> ErrorCode {
    match e {
        scanner::ScanError::NoScanners => ErrorCode::NoScannersAvailable,
        scanner::ScanError::ScannerNotFound(_) => ErrorCode::ScannerNotFound,
        scanner::ScanError::Cancelled => ErrorCode::ScanCancelled,
        scanner::ScanError::Twain(scanner::twain::TwainError::PaperJam) => ErrorCode::PaperJam,
        scanner::ScanError::Twain(scanner::twain::TwainError::PaperDoubleFeed) => {
            ErrorCode::PaperDoubleFeed
        }
        scanner::ScanError::Twain(scanner::twain::TwainError::DsmLoadFailed(_)) => {
            ErrorCode::TwainNotInstalled
        }
        _ => ErrorCode::InternalError,
    }
}

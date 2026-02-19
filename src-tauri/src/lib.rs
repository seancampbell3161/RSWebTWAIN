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

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{error, info, warn};

use protocol::{AgentMessage, ClientMessage, ErrorCode};
use scanner::ScanOrchestrator;
use ws_server::ResponseSender;

/// Process incoming WebSocket commands and dispatch to the scanner orchestrator.
///
/// This runs as a long-lived tokio task, reading commands from the WebSocket server
/// and executing scanner operations.
pub async fn command_handler(
    mut command_rx: ws_server::CommandReceiver,
    event_tx: ws_server::EventSender,
) {
    let orchestrator = Arc::new(Mutex::new(ScanOrchestrator::new()));

    info!("Command handler started");

    while let Some((message, response_tx)) = command_rx.recv().await {
        let orchestrator = orchestrator.clone();
        let event_tx = event_tx.clone();

        tokio::spawn(async move {
            handle_command(message, response_tx, orchestrator, event_tx).await;
        });
    }

    info!("Command handler stopped");
}

async fn handle_command(
    message: ClientMessage,
    response_tx: ResponseSender,
    orchestrator: Arc<Mutex<ScanOrchestrator>>,
    _event_tx: ws_server::EventSender,
) {
    match message {
        ClientMessage::Ping { id } => {
            let _ = response_tx.send(AgentMessage::Pong { id });
        }

        ClientMessage::ListScanners { id } => {
            let mut orch = orchestrator.lock().await;
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
            let scan_id = uuid::Uuid::new_v4().to_string();
            let orch = orchestrator.lock().await;

            // Clone what we need before dropping the lock
            let req_id = id.clone();
            let s_id = scan_id.clone();

            match orch
                .execute_scan(req_id, s_id, &options, response_tx.clone())
                .await
            {
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
        }

        ClientMessage::CancelScan { id, scan_id } => {
            // TODO: Implement cancellation via a shared cancellation token
            warn!("Cancel scan requested for {} (not yet implemented)", scan_id);
            let _ = response_tx.send(AgentMessage::Error {
                id,
                code: ErrorCode::InternalError,
                message: "Scan cancellation not yet implemented".to_string(),
            });
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

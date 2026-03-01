//! 32-bit sidecar process manager.
//!
//! Spawns and manages the 32-bit TWAIN scanner sidecar executable,
//! communicating via newline-delimited JSON over stdin/stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{ScanError, ScannerInfo, ScannerSource};

// ---------------------------------------------------------------------------
// Sidecar IPC types (mirrors scanner-sidecar/src/main.rs protocol)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
#[allow(dead_code)] // Variants defined for future sidecar scanning support
enum SidecarCommand {
    ListScanners,
    Scan {
        scanner_name: String,
        resolution: u32,
        color_mode: String,
        duplex: bool,
        use_adf: bool,
        show_ui: bool,
    },
    Cancel,
    Shutdown,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)] // Variants/fields defined for future sidecar scanning support
enum SidecarResponse {
    ScannerList {
        scanners: Vec<SidecarScannerEntry>,
    },
    ScanProgress {
        page: u32,
        status: String,
    },
    ScanPage {
        page: u32,
        width: u32,
        height: u32,
        bits_per_pixel: u16,
        data: String,
    },
    ScanComplete {
        total_pages: u32,
    },
    Error {
        message: String,
    },
    Ready,
    Shutdown,
}

#[derive(Debug, Deserialize)]
struct SidecarScannerEntry {
    id: String,
    name: String,
    manufacturer: String,
}

// ---------------------------------------------------------------------------
// Sidecar Manager
// ---------------------------------------------------------------------------

pub struct SidecarManager {
    child: Option<Child>,
    sidecar_path: String,
}

impl SidecarManager {
    pub fn new(sidecar_path: String) -> Self {
        Self {
            child: None,
            sidecar_path,
        }
    }

    /// Spawn the sidecar process if not already running
    pub fn ensure_running(&mut self) -> Result<(), ScanError> {
        if self.is_running() {
            return Ok(());
        }

        info!("Spawning 32-bit sidecar: {}", self.sidecar_path);

        let child = Command::new(&self.sidecar_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ScanError::Sidecar(format!("Failed to spawn sidecar: {}", e)))?;

        self.child = Some(child);

        // Wait for the Ready signal
        let response = self.read_response()?;
        match response {
            SidecarResponse::Ready => {
                info!("Sidecar is ready");
                Ok(())
            }
            SidecarResponse::Error { message } => {
                Err(ScanError::Sidecar(format!("Sidecar startup error: {}", message)))
            }
            _ => Err(ScanError::Sidecar("Unexpected sidecar response".into())),
        }
    }

    fn is_running(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(None) => true,  // Still running
                Ok(Some(_)) => {
                    self.child = None;
                    false
                }
                Err(_) => {
                    self.child = None;
                    false
                }
            }
        } else {
            false
        }
    }

    /// Send a command to the sidecar
    fn send_command(&mut self, cmd: &SidecarCommand) -> Result<(), ScanError> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| ScanError::Sidecar("Sidecar not running".into()))?;

        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| ScanError::Sidecar("Sidecar stdin not available".into()))?;

        let json = serde_json::to_string(cmd)
            .map_err(|e| ScanError::Sidecar(format!("Serialization error: {}", e)))?;

        writeln!(stdin, "{}", json)
            .map_err(|e| ScanError::Sidecar(format!("Failed to write to sidecar: {}", e)))?;

        stdin
            .flush()
            .map_err(|e| ScanError::Sidecar(format!("Failed to flush sidecar stdin: {}", e)))?;

        debug!("Sent command to sidecar: {}", json);
        Ok(())
    }

    /// Read a response from the sidecar
    fn read_response(&mut self) -> Result<SidecarResponse, ScanError> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| ScanError::Sidecar("Sidecar not running".into()))?;

        let stdout = child
            .stdout
            .as_mut()
            .ok_or_else(|| ScanError::Sidecar("Sidecar stdout not available".into()))?;

        let mut line = String::new();
        let mut reader = BufReader::new(stdout);
        reader
            .read_line(&mut line)
            .map_err(|e| ScanError::Sidecar(format!("Failed to read from sidecar: {}", e)))?;

        if line.trim().is_empty() {
            return Err(ScanError::Sidecar("Empty response from sidecar".into()));
        }

        debug!("Received from sidecar: {}", line.trim());

        serde_json::from_str(&line)
            .map_err(|e| ScanError::Sidecar(format!("Invalid sidecar response: {}", e)))
    }

    /// List scanners visible to the 32-bit sidecar
    pub fn list_scanners(&mut self) -> Result<Vec<ScannerInfo>, ScanError> {
        self.ensure_running()?;
        self.send_command(&SidecarCommand::ListScanners)?;

        match self.read_response()? {
            SidecarResponse::ScannerList { scanners } => {
                Ok(scanners
                    .into_iter()
                    .map(|s| ScannerInfo {
                        id: s.id,
                        name: s.name,
                        manufacturer: s.manufacturer,
                        source: ScannerSource::Sidecar,
                    })
                    .collect())
            }
            SidecarResponse::Error { message } => Err(ScanError::Sidecar(message)),
            _ => Err(ScanError::Sidecar("Unexpected response".into())),
        }
    }

    /// Shutdown the sidecar process
    pub fn shutdown(&mut self) {
        if let Err(e) = self.send_command(&SidecarCommand::Shutdown) {
            warn!("Failed to send shutdown to sidecar: {}", e);
        }

        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

impl Drop for SidecarManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

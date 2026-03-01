//! 32-bit sidecar process manager.
//!
//! Spawns and manages the 32-bit TWAIN scanner sidecar executable,
//! communicating via newline-delimited JSON over stdin/stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{ScanError, ScannerInfo, ScannerSource};

// ---------------------------------------------------------------------------
// Sidecar IPC types (mirrors scanner-sidecar/src/main.rs protocol)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
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
#[allow(dead_code)] // Deserialized fields matched with `..` patterns
pub(crate) enum SidecarResponse {
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
pub(crate) struct SidecarScannerEntry {
    pub id: String,
    pub name: String,
    pub manufacturer: String,
}

// ---------------------------------------------------------------------------
// Sidecar Manager
// ---------------------------------------------------------------------------

pub struct SidecarManager {
    child: Option<Child>,
    reader: Option<BufReader<ChildStdout>>,
    sidecar_path: String,
}

impl SidecarManager {
    pub fn new(sidecar_path: String) -> Self {
        Self {
            child: None,
            reader: None,
            sidecar_path,
        }
    }

    /// Spawn the sidecar process if not already running
    pub fn ensure_running(&mut self) -> Result<(), ScanError> {
        if self.is_running() {
            return Ok(());
        }

        info!("Spawning 32-bit sidecar: {}", self.sidecar_path);

        let mut child = Command::new(&self.sidecar_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ScanError::Sidecar(format!("Failed to spawn sidecar: {}", e)))?;

        // Take stdout from child and wrap in BufReader for persistent buffering
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ScanError::Sidecar("Sidecar stdout not available".into()))?;
        self.reader = Some(BufReader::new(stdout));
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
                    self.reader = None;
                    false
                }
                Err(_) => {
                    self.child = None;
                    self.reader = None;
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
    pub(crate) fn read_response(&mut self) -> Result<SidecarResponse, ScanError> {
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| ScanError::Sidecar("Sidecar stdout reader not available".into()))?;

        let mut line = String::new();
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

    /// Start a scan on the sidecar. After calling this, read responses
    /// with `read_response()` in a loop until `ScanComplete` or `Error`.
    pub fn start_scan(
        &mut self,
        scanner_name: &str,
        resolution: u32,
        color_mode: &str,
        duplex: bool,
        use_adf: bool,
        show_ui: bool,
    ) -> Result<(), ScanError> {
        self.send_command(&SidecarCommand::Scan {
            scanner_name: scanner_name.to_string(),
            resolution,
            color_mode: color_mode.to_string(),
            duplex,
            use_adf,
            show_ui,
        })
    }

    /// Send a cancel command to the sidecar during an active scan.
    pub fn send_cancel(&mut self) -> Result<(), ScanError> {
        self.send_command(&SidecarCommand::Cancel)
    }

    /// Shutdown the sidecar process
    pub fn shutdown(&mut self) {
        if let Err(e) = self.send_command(&SidecarCommand::Shutdown) {
            warn!("Failed to send shutdown to sidecar: {}", e);
        }

        // Drop reader before waiting on child
        self.reader = None;

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

//! 32-bit sidecar process manager.
//!
//! Spawns and manages the 32-bit TWAIN scanner sidecar executable,
//! communicating via newline-delimited JSON over stdin/stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use super::{ScanError, ScannerInfo, ScannerSource};

/// Timeout for sidecar to send its initial Ready signal after spawn.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for reading a single response during normal operations.
/// Generous to account for slow scanners at high DPI.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait for the sidecar to exit gracefully after sending Shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

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
    response_rx: Option<mpsc::Receiver<Result<String, ScanError>>>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    sidecar_path: String,
}

impl SidecarManager {
    pub fn new(sidecar_path: String) -> Self {
        Self {
            child: None,
            response_rx: None,
            reader_thread: None,
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

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ScanError::Sidecar("Sidecar stdout not available".into()))?;

        // Spawn a dedicated reader thread that sends lines through a channel.
        // This lets read_response() use recv_timeout() instead of blocking forever.
        let (tx, rx) = mpsc::channel();
        let reader_thread = std::thread::Builder::new()
            .name("sidecar-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => {
                            // EOF — sidecar closed stdout
                            let _ = tx.send(Err(ScanError::Sidecar(
                                "Sidecar closed stdout (process exited)".into(),
                            )));
                            break;
                        }
                        Ok(_) => {
                            if line.trim().is_empty() {
                                continue;
                            }
                            if tx.send(Ok(line)).is_err() {
                                // Receiver dropped — manager is shutting down
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(ScanError::Sidecar(
                                format!("Failed to read from sidecar: {}", e),
                            )));
                            break;
                        }
                    }
                }
            })
            .map_err(|e| ScanError::Sidecar(format!("Failed to spawn reader thread: {}", e)))?;

        self.response_rx = Some(rx);
        self.reader_thread = Some(reader_thread);
        self.child = Some(child);

        // Wait for the Ready signal with a startup timeout
        let response = self.read_response_with_timeout(STARTUP_TIMEOUT)?;
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
                    self.cleanup_dead_sidecar();
                    false
                }
                Err(_) => {
                    self.cleanup_dead_sidecar();
                    false
                }
            }
        } else {
            false
        }
    }

    /// Clean up state after the sidecar process has exited.
    fn cleanup_dead_sidecar(&mut self) {
        self.child = None;
        self.response_rx = None;
        if let Some(thread) = self.reader_thread.take() {
            let _ = thread.join();
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

    /// Read a response with the default timeout (120s, generous for slow scanners).
    pub(crate) fn read_response(&mut self) -> Result<SidecarResponse, ScanError> {
        self.read_response_with_timeout(RESPONSE_TIMEOUT)
    }

    /// Read a response with a custom timeout.
    fn read_response_with_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<SidecarResponse, ScanError> {
        let rx = self
            .response_rx
            .as_ref()
            .ok_or_else(|| ScanError::Sidecar("Sidecar stdout reader not available".into()))?;

        let line = match rx.recv_timeout(timeout) {
            Ok(result) => result?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                error!("Sidecar response timed out after {:?}", timeout);
                self.kill_sidecar();
                return Err(ScanError::Sidecar(format!(
                    "Sidecar response timed out after {}s",
                    timeout.as_secs()
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ScanError::Sidecar(
                    "Sidecar reader disconnected (process likely crashed)".into(),
                ));
            }
        };

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

    /// Kill the sidecar process immediately without graceful shutdown.
    fn kill_sidecar(&mut self) {
        if let Some(mut child) = self.child.take() {
            warn!("Killing sidecar process");
            let _ = child.kill();
            let _ = child.wait();
        }
        self.response_rx = None;
        if let Some(thread) = self.reader_thread.take() {
            let _ = thread.join();
        }
    }

    /// Shutdown the sidecar process gracefully, with a timeout fallback to kill.
    pub fn shutdown(&mut self) {
        if self.child.is_none() {
            return;
        }

        // Send shutdown command (best effort)
        if let Err(e) = self.send_command(&SidecarCommand::Shutdown) {
            warn!("Failed to send shutdown to sidecar: {}", e);
        }

        // Drop receiver so reader thread can exit when sender side closes
        self.response_rx = None;

        // Wait for child with timeout, then kill if it doesn't exit
        if let Some(mut child) = self.child.take() {
            let start = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if start.elapsed() < SHUTDOWN_TIMEOUT => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    _ => {
                        warn!("Sidecar didn't exit within {:?}, killing", SHUTDOWN_TIMEOUT);
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }

        if let Some(thread) = self.reader_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SidecarManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

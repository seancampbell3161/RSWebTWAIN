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

/// Total spawn attempts (1 initial + 2 retries).
#[allow(dead_code)] // wired into ensure_running in a follow-up task
const SPAWN_RETRY_ATTEMPTS: u32 = 3;

/// Backoffs between consecutive spawn attempts (length = SPAWN_RETRY_ATTEMPTS - 1).
#[allow(dead_code)] // wired into ensure_running in a follow-up task
const SPAWN_RETRY_BACKOFFS: [Duration; 2] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
];

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
// Spawn retry policy
// ---------------------------------------------------------------------------

/// Classification for spawn-time failures used by `with_spawn_retry`.
///
/// `Retryable` covers transient failures (case a: `Command::spawn` failed;
/// case b: sidecar exited before sending Ready). `Permanent` covers
/// failures that won't be cured by retrying (case c: Ready timeout;
/// case d: sidecar reported a startup error).
#[allow(dead_code)] // wired into ensure_running in a follow-up task
enum SpawnFailure {
    Retryable(ScanError),
    Permanent(ScanError),
}

/// Run `f` up to `SPAWN_RETRY_ATTEMPTS` times, sleeping `SPAWN_RETRY_BACKOFFS[i]`
/// between attempts. Returns immediately on `Ok` or `Permanent` failure.
#[allow(dead_code)] // wired into ensure_running in a follow-up task
fn with_spawn_retry<F, T>(mut f: F) -> Result<T, ScanError>
where
    F: FnMut(u32) -> Result<T, SpawnFailure>,
{
    for attempt in 1..=SPAWN_RETRY_ATTEMPTS {
        match f(attempt) {
            Ok(value) => return Ok(value),
            Err(SpawnFailure::Permanent(e)) => {
                warn!("Sidecar spawn failed permanently: {}", e);
                return Err(e);
            }
            Err(SpawnFailure::Retryable(e)) => {
                if attempt == SPAWN_RETRY_ATTEMPTS {
                    error!("Sidecar spawn failed after {} attempts: {}", attempt, e);
                    return Err(e);
                }
                let backoff = SPAWN_RETRY_BACKOFFS[(attempt - 1) as usize];
                warn!(
                    "Sidecar spawn attempt {} failed: {}; retrying in {}ms",
                    attempt,
                    e,
                    backoff.as_millis()
                );
                std::thread::sleep(backoff);
            }
        }
    }
    unreachable!("loop body always returns")
}

// ---------------------------------------------------------------------------
// Sidecar Manager
// ---------------------------------------------------------------------------

pub struct SidecarManager {
    child: Option<Child>,
    response_rx: Option<mpsc::Receiver<Result<String, ScanError>>>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    sidecar_path: String,
    env_overrides: Vec<(String, String)>,
}

impl SidecarManager {
    pub fn new(sidecar_path: String) -> Self {
        Self {
            child: None,
            response_rx: None,
            reader_thread: None,
            sidecar_path,
            env_overrides: Vec::new(),
        }
    }

    /// Add an environment variable that will be set on the sidecar child process.
    /// Useful for forwarding RUST_LOG or for driving test fakes.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_overrides.push((key.into(), value.into()));
        self
    }

    /// Spawn the sidecar process if not already running
    pub fn ensure_running(&mut self) -> Result<(), ScanError> {
        if self.is_running() {
            return Ok(());
        }

        info!("Spawning 32-bit sidecar: {}", self.sidecar_path);

        let mut cmd = Command::new(&self.sidecar_path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &self.env_overrides {
            cmd.env(k, v);
        }
        let mut child = cmd
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn with_env_appends_overrides_in_order() {
        let m = SidecarManager::new("/nonexistent/path".to_string())
            .with_env("KEY1", "val1")
            .with_env("KEY2", "val2");
        assert_eq!(
            m.env_overrides,
            vec![
                ("KEY1".to_string(), "val1".to_string()),
                ("KEY2".to_string(), "val2".to_string()),
            ]
        );
    }

    #[test]
    fn with_spawn_retry_succeeds_on_first_attempt() {
        let calls = AtomicU32::new(0);
        let result = with_spawn_retry(|_attempt| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, SpawnFailure>(42)
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn with_spawn_retry_succeeds_after_one_retryable_failure() {
        let calls = AtomicU32::new(0);
        let result = with_spawn_retry(|_attempt| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(SpawnFailure::Retryable(ScanError::Sidecar("transient".into())))
            } else {
                Ok(7)
            }
        });
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn with_spawn_retry_exhausts_attempts_on_repeated_retryable() {
        let calls = AtomicU32::new(0);
        let result: Result<(), ScanError> = with_spawn_retry(|_attempt| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(SpawnFailure::Retryable(ScanError::Sidecar("never works".into())))
        });
        assert!(matches!(result, Err(ScanError::Sidecar(ref m)) if m.contains("never works")));
        assert_eq!(calls.load(Ordering::SeqCst), SPAWN_RETRY_ATTEMPTS);
    }

    #[test]
    fn with_spawn_retry_fails_fast_on_permanent() {
        let calls = AtomicU32::new(0);
        let result: Result<(), ScanError> = with_spawn_retry(|_attempt| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(SpawnFailure::Permanent(ScanError::Sidecar("broken".into())))
        });
        assert!(matches!(result, Err(ScanError::Sidecar(ref m)) if m.contains("broken")));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "should not retry on permanent");
    }

    #[test]
    fn with_spawn_retry_passes_attempt_numbers() {
        let seen = std::sync::Mutex::new(Vec::new());
        let result: Result<(), ScanError> = with_spawn_retry(|attempt| {
            seen.lock().unwrap().push(attempt);
            Err(SpawnFailure::Retryable(ScanError::Sidecar("again".into())))
        });
        assert!(result.is_err());
        assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3]);
    }
}

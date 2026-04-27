//! Integration tests for SidecarManager spawn-retry behaviour.
//!
//! These tests drive the real `ensure_running` path against a controllable
//! fake sidecar binary built from `tests/bin/fake_sidecar.rs`.

use scan_agent_lib::scanner::sidecar::SidecarManager;

const FAKE_SIDECAR: &str = env!("CARGO_BIN_EXE_fake_sidecar");

fn unique_counter_path(label: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "fake_sidecar_counter_{}_{}_{}.txt",
        std::process::id(),
        label,
        seq,
    ))
}

/// RAII guard that removes the counter file on drop, even on test panic.
struct CounterFile(std::path::PathBuf);

impl CounterFile {
    fn new(label: &str) -> Self {
        let path = unique_counter_path(label);
        let _ = std::fs::remove_file(&path);
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for CounterFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn ensure_running_succeeds_on_ready_sidecar() {
    let mut manager = SidecarManager::new(FAKE_SIDECAR.to_string())
        .with_env("FAKE_SIDECAR_BEHAVIOR", "ready");

    let result = manager.ensure_running();
    assert!(result.is_ok(), "ensure_running failed: {:?}", result);

    manager.shutdown();
}

#[test]
fn ensure_running_eventually_succeeds_after_flaky_start() {
    let counter = CounterFile::new("flaky");

    let mut manager = SidecarManager::new(FAKE_SIDECAR.to_string())
        .with_env("FAKE_SIDECAR_BEHAVIOR", "flaky_2")
        .with_env("FAKE_SIDECAR_COUNTER_FILE", counter.path().to_str().unwrap());

    let result = manager.ensure_running();
    assert!(
        result.is_ok(),
        "expected success after retries, got: {:?}",
        result
    );

    let final_count: u32 = std::fs::read_to_string(counter.path())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(final_count, 3, "expected 3 spawn attempts, got {}", final_count);

    manager.shutdown();
}

#[test]
fn ensure_running_gives_up_after_all_retries_fail() {
    let counter = CounterFile::new("exhaust");

    let mut manager = SidecarManager::new(FAKE_SIDECAR.to_string())
        .with_env("FAKE_SIDECAR_BEHAVIOR", "flaky_99")
        .with_env("FAKE_SIDECAR_COUNTER_FILE", counter.path().to_str().unwrap());

    let result = manager.ensure_running();
    assert!(result.is_err(), "expected failure, got: {:?}", result);

    let final_count: u32 = std::fs::read_to_string(counter.path())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(final_count, 3, "expected 3 spawn attempts, got {}", final_count);
}

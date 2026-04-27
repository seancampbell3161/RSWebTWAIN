//! Integration tests for SidecarManager spawn-retry behaviour.
//!
//! These tests drive the real `ensure_running` path against a controllable
//! fake sidecar binary built from `tests/bin/fake_sidecar.rs`.

use scan_agent_lib::scanner::sidecar::SidecarManager;

const FAKE_SIDECAR: &str = env!("CARGO_BIN_EXE_fake_sidecar");

#[test]
fn ensure_running_succeeds_on_ready_sidecar() {
    let mut manager = SidecarManager::new(FAKE_SIDECAR.to_string())
        .with_env("FAKE_SIDECAR_BEHAVIOR", "ready");

    let result = manager.ensure_running();
    assert!(result.is_ok(), "ensure_running failed: {:?}", result);

    manager.shutdown();
}

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

#[test]
fn ensure_running_eventually_succeeds_after_flaky_start() {
    let counter = unique_counter_path("flaky");
    let _ = std::fs::remove_file(&counter);

    let mut manager = SidecarManager::new(FAKE_SIDECAR.to_string())
        .with_env("FAKE_SIDECAR_BEHAVIOR", "flaky_2")
        .with_env("FAKE_SIDECAR_COUNTER_FILE", counter.to_str().unwrap());

    let result = manager.ensure_running();
    assert!(
        result.is_ok(),
        "expected success after retries, got: {:?}",
        result
    );

    let final_count: u32 = std::fs::read_to_string(&counter)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(final_count, 3, "expected 3 spawn attempts, got {}", final_count);

    manager.shutdown();
    let _ = std::fs::remove_file(&counter);
}

#[test]
fn ensure_running_gives_up_after_all_retries_fail() {
    let mut manager = SidecarManager::new(FAKE_SIDECAR.to_string())
        .with_env("FAKE_SIDECAR_BEHAVIOR", "exit_immediately");

    let result = manager.ensure_running();
    assert!(result.is_err(), "expected failure, got: {:?}", result);
}

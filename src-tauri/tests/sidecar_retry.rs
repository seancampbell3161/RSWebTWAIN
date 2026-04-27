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

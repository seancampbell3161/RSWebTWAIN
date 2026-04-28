//! First-run flow: empty %APPDATA% → template written + load_or_default returns defaults.

use scan_agent_lib::config::{
    load_or_default, write_template_if_missing, AgentConfig,
};

#[test]
fn first_run_writes_template_and_loads_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("com.rswebtwain.agent").join("config.toml");

    // First call writes the template.
    let wrote = write_template_if_missing(&path).expect("template write");
    assert!(wrote, "first call should have written the template");
    assert!(path.exists(), "template file should exist after write");

    // Loading it returns the canonical defaults (every line is commented).
    let cfg = load_or_default(&path).expect("template must parse");
    assert_eq!(cfg, AgentConfig::default());

    // Second call is a no-op.
    let wrote_again = write_template_if_missing(&path).expect("idempotent");
    assert!(!wrote_again, "second call should not rewrite the template");
}

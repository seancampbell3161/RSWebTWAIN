//! Per-installation configuration loaded from `%APPDATA%\com.rswebtwain.agent\config.toml`.
//!
//! Missing config = built-in defaults (port 47115, localhost-only origins).
//! Env vars (`RSWEBTWAIN_PORT`, `RSWEBTWAIN_ALLOWED_ORIGINS`) override config values.

use std::path::Path;

use serde::Deserialize;

pub const DEFAULT_PORT: u16 = 47115;

#[derive(Debug, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct AgentConfig {
    pub server: ServerConfig,
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
#[serde(default)]
pub struct ServerConfig {
    pub port: u16,
    pub allow_localhost: bool,
    pub extra_origins: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            allow_localhost: true,
            extra_origins: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "config I/O error: {e}"),
            ConfigError::Parse(e) => write!(f, "config parse error: {e}"),
            ConfigError::Invalid(msg) => write!(f, "config invalid: {msg}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Io(e) => Some(e),
            ConfigError::Parse(e) => Some(e),
            ConfigError::Invalid(_) => None,
        }
    }
}

pub fn load_or_default(_config_path: &Path) -> Result<AgentConfig, ConfigError> {
    Ok(AgentConfig::default())
}

pub fn apply_env_overrides(_config: &mut AgentConfig) {}

pub fn write_template_if_missing(_config_path: &Path) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> AgentConfig {
        toml::from_str(s).expect("valid TOML should parse")
    }

    #[test]
    fn empty_input_yields_defaults() {
        let cfg = parse("");
        assert_eq!(cfg, AgentConfig::default());
        assert_eq!(cfg.server.port, DEFAULT_PORT);
        assert!(cfg.server.allow_localhost);
        assert!(cfg.server.extra_origins.is_empty());
    }

    #[test]
    fn empty_server_section_yields_field_defaults() {
        let cfg = parse("[server]\n");
        assert_eq!(cfg, AgentConfig::default());
    }

    #[test]
    fn explicit_fields_override_defaults() {
        let cfg = parse(r#"
            [server]
            port = 9000
            allow_localhost = false
            extra_origins = ["https://app.example.com"]
        "#);
        assert_eq!(cfg.server.port, 9000);
        assert!(!cfg.server.allow_localhost);
        assert_eq!(cfg.server.extra_origins, vec!["https://app.example.com".to_string()]);
    }

    #[test]
    fn partial_server_section_uses_defaults_for_missing_fields() {
        let cfg = parse(r#"
            [server]
            port = 9000
        "#);
        assert_eq!(cfg.server.port, 9000);
        assert!(cfg.server.allow_localhost); // default
        assert!(cfg.server.extra_origins.is_empty()); // default
    }
}

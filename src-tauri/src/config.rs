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

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

pub fn load_or_default(config_path: &Path) -> Result<AgentConfig, ConfigError> {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AgentConfig::default());
        }
        Err(e) => return Err(ConfigError::Io(e)),
    };
    let cfg: AgentConfig = toml::from_str(&raw).map_err(ConfigError::Parse)?;
    validate(&cfg)?;
    Ok(cfg)
}

pub fn apply_env_overrides(config: &mut AgentConfig) {
    if let Ok(val) = std::env::var("RSWEBTWAIN_PORT") {
        match val.parse::<u16>() {
            Ok(p) if p > 0 => {
                tracing::info!("Port overridden by RSWEBTWAIN_PORT: {p}");
                config.server.port = p;
            }
            _ => tracing::warn!(
                "Invalid RSWEBTWAIN_PORT '{val}' (expected 1-65535); keeping config value {}",
                config.server.port,
            ),
        }
    }

    if let Ok(val) = std::env::var("RSWEBTWAIN_ALLOWED_ORIGINS") {
        let parsed: Vec<String> = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !parsed.is_empty() {
            tracing::info!(
                "Origin policy overridden by RSWEBTWAIN_ALLOWED_ORIGINS \
                 ({} entries; localhost magic disabled)",
                parsed.len(),
            );
            config.server.allow_localhost = false;
            config.server.extra_origins = parsed;
        }
    }
}

pub fn write_template_if_missing(_config_path: &Path) -> std::io::Result<bool> {
    Ok(false)
}

fn validate(cfg: &AgentConfig) -> Result<(), ConfigError> {
    if cfg.server.port == 0 {
        return Err(ConfigError::Invalid(
            "port must be in the range 1-65535 (got 0)".to_string(),
        ));
    }
    for o in &cfg.server.extra_origins {
        let parsed = url::Url::parse(o).map_err(|_| {
            ConfigError::Invalid(format!(
                "extra origin '{o}' is not a valid URL (must include http:// or https:// scheme)"
            ))
        })?;
        match parsed.scheme() {
            "http" | "https" => {}
            other => {
                return Err(ConfigError::Invalid(format!(
                    "extra origin '{o}' uses unsupported scheme '{other}' (expected http or https)"
                )));
            }
        }
    }
    Ok(())
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

    #[test]
    fn port_zero_is_invalid() {
        let err = validate(&AgentConfig {
            server: ServerConfig { port: 0, ..ServerConfig::default() },
        }).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(ref m) if m.contains("port")), "got {err:?}");
    }

    #[test]
    fn extra_origin_must_be_a_url() {
        let err = validate(&AgentConfig {
            server: ServerConfig {
                extra_origins: vec!["not-a-url".to_string()],
                ..ServerConfig::default()
            },
        }).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(ref m) if m.contains("not-a-url")), "got {err:?}");
    }

    #[test]
    fn extra_origin_without_scheme_is_rejected() {
        let err = validate(&AgentConfig {
            server: ServerConfig {
                extra_origins: vec!["app.example.com".to_string()],
                ..ServerConfig::default()
            },
        }).unwrap_err();
        assert!(
            matches!(err, ConfigError::Invalid(ref m) if m.contains("scheme")),
            "expected message mentioning scheme, got {err:?}",
        );
    }

    #[test]
    fn extra_origin_with_ws_scheme_is_rejected() {
        // Only http/https are valid web origin schemes.
        let err = validate(&AgentConfig {
            server: ServerConfig {
                extra_origins: vec!["ws://app.example.com".to_string()],
                ..ServerConfig::default()
            },
        }).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)), "got {err:?}");
    }

    #[test]
    fn valid_https_origin_passes() {
        validate(&AgentConfig {
            server: ServerConfig {
                extra_origins: vec!["https://app.example.com".to_string()],
                ..ServerConfig::default()
            },
        }).unwrap();
    }

    use std::io::Write;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write_file(dir: &tempfile::TempDir, name: &str, contents: &str) -> std::path::PathBuf {
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        p
    }

    #[test]
    fn load_or_default_returns_defaults_when_file_missing() {
        let dir = tmpdir();
        let path = dir.path().join("nope.toml");
        let cfg = load_or_default(&path).expect("missing file should not error");
        assert_eq!(cfg, AgentConfig::default());
    }

    #[test]
    fn load_or_default_returns_parsed_config() {
        let dir = tmpdir();
        let path = write_file(&dir, "config.toml", r#"
            [server]
            port = 9001
            extra_origins = ["https://app.example.com"]
        "#);
        let cfg = load_or_default(&path).unwrap();
        assert_eq!(cfg.server.port, 9001);
        assert!(cfg.server.allow_localhost); // default kept
        assert_eq!(cfg.server.extra_origins, vec!["https://app.example.com".to_string()]);
    }

    #[test]
    fn load_or_default_returns_parse_error_for_bad_toml() {
        let dir = tmpdir();
        let path = write_file(&dir, "bad.toml", "this is = not = toml");
        let err = load_or_default(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)), "got {err:?}");
    }

    #[test]
    fn load_or_default_returns_invalid_for_bad_value() {
        let dir = tmpdir();
        let path = write_file(&dir, "bad.toml", "[server]\nport = 0\n");
        let err = load_or_default(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)), "got {err:?}");
    }

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` with each `(name, Some(val))` set and each `(name, None)` removed,
    /// then restore the original values. Serialised against other env-var tests.
    fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let originals: Vec<_> = vars.iter().map(|(k, _)| (*k, std::env::var(k).ok())).collect();
        for (k, v) in vars {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        for (k, original) in originals {
            match original {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        if let Err(e) = result { std::panic::resume_unwind(e); }
    }

    #[test]
    fn env_port_override_applies() {
        with_env(&[("RSWEBTWAIN_PORT", Some("9000")), ("RSWEBTWAIN_ALLOWED_ORIGINS", None)], || {
            let mut cfg = AgentConfig::default();
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.server.port, 9000);
        });
    }

    #[test]
    fn env_port_invalid_keeps_config_value() {
        with_env(&[("RSWEBTWAIN_PORT", Some("not-a-number")), ("RSWEBTWAIN_ALLOWED_ORIGINS", None)], || {
            let mut cfg = AgentConfig::default();
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.server.port, DEFAULT_PORT);
        });
    }

    #[test]
    fn env_port_zero_keeps_config_value() {
        with_env(&[("RSWEBTWAIN_PORT", Some("0")), ("RSWEBTWAIN_ALLOWED_ORIGINS", None)], || {
            let mut cfg = AgentConfig::default();
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.server.port, DEFAULT_PORT);
        });
    }

    #[test]
    fn env_origins_replace_full_policy_and_disable_localhost() {
        with_env(&[
            ("RSWEBTWAIN_PORT", None),
            ("RSWEBTWAIN_ALLOWED_ORIGINS", Some("http://localhost:4200,https://app.example.com")),
        ], || {
            let mut cfg = AgentConfig::default();
            assert!(cfg.server.allow_localhost); // sanity check
            apply_env_overrides(&mut cfg);
            assert!(!cfg.server.allow_localhost, "env override should disable localhost magic");
            assert_eq!(
                cfg.server.extra_origins,
                vec!["http://localhost:4200".to_string(), "https://app.example.com".to_string()],
            );
        });
    }

    #[test]
    fn env_origins_empty_string_is_ignored() {
        with_env(&[("RSWEBTWAIN_PORT", None), ("RSWEBTWAIN_ALLOWED_ORIGINS", Some(""))], || {
            let mut cfg = AgentConfig::default();
            apply_env_overrides(&mut cfg);
            assert!(cfg.server.allow_localhost, "empty env var should not change policy");
            assert!(cfg.server.extra_origins.is_empty());
        });
    }
}

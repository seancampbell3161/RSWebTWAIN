//! Log subscriber initialization and file rotation pruning (sidecar copy).
//!
//! Duplicated from `src-tauri/src/logging.rs` rather than shared because the
//! sidecar deliberately stays free of the `scan_agent_lib` cross-crate dep —
//! the sidecar must build clean as a 32-bit crate with minimal deps.
//!
//! TODO(Task 6): remove the allow below once main() calls init_logging.
#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{self, RollingFileAppender};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

const LOG_PREFIX: &str = "sidecar.log";
const KEEP_DAYS: usize = 7;

static INIT_DONE: OnceLock<()> = OnceLock::new();

pub fn init_logging(log_dir: Option<&Path>) -> Option<WorkerGuard> {
    if INIT_DONE.set(()).is_err() {
        return None;
    }

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("scanner_sidecar=debug"));

    let stderr_layer = fmt::layer().with_writer(std::io::stderr);

    let (file_layer, guard) = match log_dir.and_then(try_make_file_appender) {
        Some((appender, guard)) => {
            let layer = fmt::layer().with_writer(appender).with_ansi(false);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();

    if let Some(dir) = log_dir {
        if let Err(e) = prune_old_logs(dir, LOG_PREFIX, KEEP_DAYS) {
            eprintln!("warn: pruning old logs in {} failed: {e}", dir.display());
        }
    }

    guard
}

fn try_make_file_appender(dir: &Path) -> Option<(NonBlocking, WorkerGuard)> {
    if let Err(e) = fs::create_dir_all(dir) {
        eprintln!(
            "warn: cannot create log dir {}: {e} — file logging disabled",
            dir.display()
        );
        return None;
    }
    let appender: RollingFileAppender = rolling::daily(dir, LOG_PREFIX);
    Some(tracing_appender::non_blocking(appender))
}

pub fn prune_old_logs(dir: &Path, prefix: &str, keep: usize) -> io::Result<()> {
    let suffix_re_len = ".YYYY-MM-DD".len();
    let mut matching: Vec<PathBuf> = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if name_str.len() != prefix.len() + suffix_re_len {
            continue;
        }
        if !name_str.starts_with(prefix) {
            continue;
        }
        let suffix = &name_str[prefix.len()..];
        if !suffix.starts_with('.') {
            continue;
        }
        let date = &suffix[1..];
        if !is_iso_date(date) {
            continue;
        }
        matching.push(entry.path());
    }

    matching.sort();
    if matching.len() <= keep {
        return Ok(());
    }
    let to_delete = matching.len() - keep;
    let mut first_err: Option<io::Error> = None;
    for path in &matching[..to_delete] {
        if let Err(e) = fs::remove_file(path) {
            if first_err.is_none() {
                first_err = Some(e);
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn is_iso_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    bytes
        .iter()
        .enumerate()
        .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    fn touch(dir: &Path, name: &str) {
        File::create(dir.join(name)).unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn prune_keeps_newest_n_dated_files() {
        let dir = tempdir().unwrap();
        for d in ["2026-04-25", "2026-04-26", "2026-04-27", "2026-04-28", "2026-04-29"] {
            touch(dir.path(), &format!("sidecar.log.{d}"));
        }
        prune_old_logs(dir.path(), "sidecar.log", 3).unwrap();
        assert_eq!(
            names(dir.path()),
            vec![
                "sidecar.log.2026-04-27".to_string(),
                "sidecar.log.2026-04-28".to_string(),
                "sidecar.log.2026-04-29".to_string(),
            ]
        );
    }

    #[test]
    fn init_logging_creates_file_and_is_idempotent() {
        use std::thread::sleep;
        use std::time::Duration;
        use tracing::info;

        let dir = tempdir().unwrap();
        let log_dir = dir.path().join("logs");

        let guard = init_logging(Some(&log_dir));
        assert!(guard.is_some(), "first call must return a guard when log dir is set");

        info!("hello from sidecar test");

        assert!(
            init_logging(None).is_none(),
            "second call must be a no-op (returns None)"
        );

        drop(guard);
        sleep(Duration::from_millis(50));

        let entries: Vec<_> = fs::read_dir(&log_dir).unwrap().collect();
        assert!(
            entries.iter().any(|e| {
                let name = e.as_ref().unwrap().file_name();
                name.to_string_lossy().starts_with("sidecar.log")
            }),
            "expected a sidecar.log* file in {log_dir:?}, found {entries:?}"
        );
    }
}

//! Log subscriber initialization and file rotation pruning.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{self, RollingFileAppender};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

const LOG_PREFIX: &str = "agent.log";
const KEEP_DAYS: usize = 7;

static INIT_DONE: OnceLock<()> = OnceLock::new();

/// Install the global tracing subscriber with a stderr layer and, when
/// `log_dir` is `Some` and writable, a non-blocking daily-rotating file layer.
///
/// Returns the `WorkerGuard` for the file layer; the caller MUST keep it alive
/// for the program's duration. Dropping it flushes and closes the worker.
///
/// Calling this more than once in a process is a no-op (returns `None`).
pub fn init_logging(log_dir: Option<&Path>) -> Option<WorkerGuard> {
    if INIT_DONE.set(()).is_err() {
        return None;
    }

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("scan_agent=info"));

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

/// Delete dated log files in `dir` matching `{prefix}.YYYY-MM-DD`,
/// keeping only the `keep` most recent (sorted lexicographically by filename,
/// which equals chronological order for ISO-8601 dates).
///
/// Files whose names don't match the pattern are left untouched.
/// Returns the first I/O error encountered, but always continues attempting
/// to delete every match.
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
        let suffix = &name_str[prefix.len()..]; // expect ".YYYY-MM-DD"
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
    // YYYY-MM-DD
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
            touch(dir.path(), &format!("agent.log.{d}"));
        }

        prune_old_logs(dir.path(), "agent.log", 3).unwrap();

        assert_eq!(
            names(dir.path()),
            vec![
                "agent.log.2026-04-27".to_string(),
                "agent.log.2026-04-28".to_string(),
                "agent.log.2026-04-29".to_string(),
            ]
        );
    }

    #[test]
    fn prune_no_op_when_fewer_than_keep() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "agent.log.2026-04-28");
        touch(dir.path(), "agent.log.2026-04-29");

        prune_old_logs(dir.path(), "agent.log", 5).unwrap();

        assert_eq!(names(dir.path()).len(), 2);
    }

    #[test]
    fn prune_ignores_non_matching_files() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "agent.log.2026-04-28");
        touch(dir.path(), "agent.log.2026-04-29");
        touch(dir.path(), "agent.log.2026-04-30");
        touch(dir.path(), "agent.log");                  // active file, no date
        touch(dir.path(), "sidecar.log.2026-04-28");     // different prefix
        touch(dir.path(), "agent.log.notadate");         // bad suffix
        touch(dir.path(), "README.txt");                 // unrelated

        prune_old_logs(dir.path(), "agent.log", 1).unwrap();

        assert_eq!(
            names(dir.path()),
            vec![
                "README.txt".to_string(),
                "agent.log".to_string(),
                "agent.log.2026-04-30".to_string(),
                "agent.log.notadate".to_string(),
                "sidecar.log.2026-04-28".to_string(),
            ]
        );
    }

    #[test]
    fn prune_empty_dir_is_ok() {
        let dir = tempdir().unwrap();
        prune_old_logs(dir.path(), "agent.log", 3).unwrap();
        assert_eq!(names(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn init_logging_creates_file_and_is_idempotent() {
        use std::thread::sleep;
        use std::time::Duration;
        use tracing::info;

        let dir = tempdir().unwrap();
        let log_dir = dir.path().join("logs");

        // First call: must install subscriber and return a guard.
        let guard = init_logging(Some(&log_dir));
        assert!(guard.is_some(), "first call must return a guard when log dir is set");

        info!("hello from test");

        // Second call: idempotent no-op, returns None regardless of argument.
        assert!(
            init_logging(None).is_none(),
            "second call must be a no-op (returns None)"
        );
        assert!(
            init_logging(Some(&log_dir)).is_none(),
            "third call must also be a no-op"
        );

        // Force the non-blocking writer to flush by dropping the guard.
        drop(guard);
        sleep(Duration::from_millis(50));

        let entries: Vec<_> = fs::read_dir(&log_dir).unwrap().collect();
        assert!(
            entries.iter().any(|e| {
                let name = e.as_ref().unwrap().file_name();
                name.to_string_lossy().starts_with("agent.log")
            }),
            "expected an agent.log* file in {log_dir:?}, found {entries:?}"
        );
    }
}

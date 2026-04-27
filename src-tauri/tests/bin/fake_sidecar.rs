//! Test-only fake sidecar for the spawn-retry integration tests.
//!
//! Reads `FAKE_SIDECAR_BEHAVIOR` and behaves accordingly:
//!   - `ready`            : print Ready, then read stdin until shutdown command or EOF.
//!   - `exit_immediately` : exit code 1 with no output (case b — retryable).
//!   - `hang`             : sleep forever, never print (case c — permanent).
//!   - `error`            : print an Error response (case d — permanent).
//!   - `flaky_<n>`        : exit immediately on the first <n> invocations
//!     (counter persisted via FAKE_SIDECAR_COUNTER_FILE),
//!     then behave as `ready`.

use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

fn main() {
    let behavior = env::var("FAKE_SIDECAR_BEHAVIOR").unwrap_or_else(|_| "ready".to_string());

    match behavior.as_str() {
        "ready" => print_ready_and_wait(),
        "exit_immediately" => std::process::exit(1),
        "hang" => loop {
            thread::sleep(Duration::from_secs(60));
        },
        "error" => {
            println!(r#"{{"type":"error","message":"fake startup error"}}"#);
            io::stdout().flush().ok();
        }
        s if s.starts_with("flaky_") => handle_flaky(s),
        other => {
            eprintln!("Unknown FAKE_SIDECAR_BEHAVIOR: {}", other);
            std::process::exit(2);
        }
    }
}

fn handle_flaky(behavior: &str) {
    let n: u32 = behavior
        .strip_prefix("flaky_")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let counter_path: PathBuf = env::var("FAKE_SIDECAR_COUNTER_FILE")
        .map(PathBuf::from)
        .expect("flaky_<n> requires FAKE_SIDECAR_COUNTER_FILE");

    let count: u32 = fs::read_to_string(&counter_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    fs::write(&counter_path, (count + 1).to_string()).expect("write counter");

    if count < n {
        std::process::exit(1);
    } else {
        print_ready_and_wait();
    }
}

fn print_ready_and_wait() {
    println!(r#"{{"type":"ready"}}"#);
    io::stdout().flush().ok();

    let stdin = io::stdin();
    let mut lock = stdin.lock();
    let mut buf = String::new();
    loop {
        buf.clear();
        match lock.read_line(&mut buf) {
            Ok(0) => break, // EOF — parent closed stdin
            Ok(_) => {
                if buf.contains("\"command\":\"shutdown\"") {
                    println!(r#"{{"type":"shutdown"}}"#);
                    io::stdout().flush().ok();
                    break;
                }
                // Ignore other commands; this fake only handles startup + shutdown.
            }
            Err(_) => break,
        }
    }
}

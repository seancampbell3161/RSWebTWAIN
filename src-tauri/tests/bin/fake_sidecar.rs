//! Test-only fake sidecar for the spawn-retry integration tests.
//!
//! Reads `FAKE_SIDECAR_BEHAVIOR` and behaves accordingly:
//!   - `ready`            : print Ready, then read stdin and reply to
//!     `list_scanners`/`scan`/`shutdown` (other commands are ignored).
//!     The `scan` handler emits a `scan_progress`, sleeps, then a
//!     `scan_complete` with zero pages — long enough that a concurrent
//!     `start_scan` request to the parent must be rejected as busy.
//!   - `exit_immediately` : exit code 1 with no output (case b — retryable).
//!   - `hang`             : sleep forever, never print (case c — permanent).
//!   - `error`            : print an Error response (case d — permanent).
//!   - `flaky_<n>`        : exit immediately on the first <n> invocations
//!     (counter persisted via FAKE_SIDECAR_COUNTER_FILE),
//!     then behave as `ready`.
//!
//! Tunables for the `ready` behaviour (env vars):
//!   - `FAKE_SIDECAR_SCAN_DELAY_MS` : milliseconds to hold the scan open
//!     (default 1000). Without `FAKE_SIDECAR_EMIT_PAGES` it is one sleep
//!     between `scan_progress` and `scan_complete`; with it, the delay paces
//!     each page, so a test can cancel part-way through a batch.
//!   - `FAKE_SIDECAR_EMIT_PAGES`    : when set, emit real `scan_page`
//!     bitmaps whose rows carry padding (24bpp and 1bpp) before completing,
//!     so the parent's stride handling is exercised end to end.
//!   - `FAKE_SIDECAR_PAGE_COUNT`    : how many pages `FAKE_SIDECAR_EMIT_PAGES`
//!     emits (default 2). Page 1 is 24bpp and page 2 is 1bpp; beyond that the
//!     24bpp bitmap repeats, so a full ADF hopper can be simulated.
//!   - `FAKE_SIDECAR_ERROR_AFTER_PAGES` : when set, the batch ends with an
//!     `error` response instead of `scan_complete` — a producer that fails
//!     only after its last page has already been handed over.
//!
//! These are process-global, so tests that depend on them must serialise —
//! see `SIDECAR_ENV` in `tests/ws_integration.rs`.

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

/// Emit `count` pages whose rows carry real padding, so the parent has to
/// honour `bytes_per_row` rather than assume tightly-packed rows.
///
/// Page 1: 3px x 2 rows of 24bpp colour. A row of pixels is 9 bytes, padded
///         to a 12-byte stride.
/// Page 2: 16px x 2 rows of 1bpp black-and-white. A row is 2 bytes, padded
///         to a 4-byte stride.
/// Page 3 onwards repeat the 24bpp bitmap, so a batch of any size can be
/// asked for without inventing more fixtures.
///
/// `delay` is slept before each page, which is what lets a test cancel a
/// batch part-way rather than racing an instantaneous burst.
fn emit_padded_pages(count: u32, delay: Duration) {
    for page in 1..=count {
        thread::sleep(delay);
        if page == 2 {
            println!(
                r#"{{"type":"scan_page","page":2,"width":16,"height":2,"bits_per_pixel":1,"bytes_per_row":4,"data":"qsyqqvAPqqo="}}"#
            );
        } else {
            println!(
                r#"{{"type":"scan_page","page":{page},"width":3,"height":2,"bits_per_pixel":24,"bytes_per_row":12,"data":"AQIDBAUGBwgJqqqqCgsMDQ4PEBESqqqq"}}"#
            );
        }
        io::stdout().flush().ok();
    }
}

fn print_ready_and_wait() {
    println!(r#"{{"type":"ready"}}"#);
    io::stdout().flush().ok();

    let scan_delay_ms: u64 = env::var("FAKE_SIDECAR_SCAN_DELAY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    let emit_pages = env::var("FAKE_SIDECAR_EMIT_PAGES").is_ok();

    let page_count: u32 = env::var("FAKE_SIDECAR_PAGE_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);

    let error_after_pages = env::var("FAKE_SIDECAR_ERROR_AFTER_PAGES").is_ok();

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
                } else if buf.contains("\"command\":\"list_scanners\"") {
                    println!(
                        r#"{{"type":"scanner_list","scanners":[{{"id":"fake-1","name":"Fake Scanner","manufacturer":"FakeCo"}}]}}"#
                    );
                    io::stdout().flush().ok();
                } else if buf.contains("\"command\":\"scan\"") {
                    println!(r#"{{"type":"scan_progress","page":1,"status":"scanning"}}"#);
                    io::stdout().flush().ok();

                    if emit_pages {
                        emit_padded_pages(page_count, Duration::from_millis(scan_delay_ms));
                        if error_after_pages {
                            println!(
                                r#"{{"type":"error","message":"fake failure after the last page"}}"#
                            );
                        } else {
                            println!(r#"{{"type":"scan_complete","total_pages":{page_count}}}"#);
                        }
                    } else {
                        thread::sleep(Duration::from_millis(scan_delay_ms));
                        println!(r#"{{"type":"scan_complete","total_pages":0}}"#);
                    }
                    io::stdout().flush().ok();
                }
                // Other commands (cancel, etc.) are silently ignored.
            }
            Err(_) => break,
        }
    }
}

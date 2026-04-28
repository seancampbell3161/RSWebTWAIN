# RSWebTWAIN

A headless system tray app that bridges browser-based applications to TWAIN
document scanners on Windows. Designed as an open-source replacement for
commercial WebTWAIN bridges.

> **Status:** alpha. Core scanning, WebSocket protocol, and 32-bit driver
> sidecar work end-to-end on Windows. Not yet code-signed; auto-update is
> not implemented. Production readiness items are tracked in the issues.

## How it works

```
Browser app  ──WebSocket──▶  RSWebTWAIN (64-bit)  ──spawn──▶  twain-scanner-32bit
              localhost:47115                                  (sidecar for legacy drivers)
```

- The 64-bit main app talks TWAIN directly via `TWAINDSM.dll` for modern
  64-bit drivers.
- When a scanner only ships a 32-bit driver, the main app spawns a 32-bit
  sidecar process and proxies commands over JSON-line stdin/stdout IPC.
- The browser communicates with the agent over a local WebSocket on
  `127.0.0.1:47115`. Origin validation and an optional auth token gate
  connections.
- A `rswebtwain://` deep link protocol allows web pages to launch the agent.

The protocol lives in `src-tauri/src/protocol.rs`, the orchestrator in
`src-tauri/src/scanner/mod.rs`, and the TWAIN state machine (a typestate
implementation where invalid transitions are compile-time errors) in
`src-tauri/src/scanner/twain.rs`.

## Building

Requires the Rust toolchain plus both Windows MSVC targets:

```bash
rustup target add x86_64-pc-windows-msvc
rustup target add i686-pc-windows-msvc
```

```bash
# Main app (64-bit)
cargo build --release -p scan-agent

# Sidecar (32-bit)
cargo build --release --target i686-pc-windows-msvc -p scanner-sidecar

# MSI installer (requires Tauri CLI + WiX)
cargo tauri build
```

The sidecar binary must be copied into
`src-tauri/binaries/twain-scanner-32bit-{target-triple}.exe` before the Tauri
build picks it up. CI does this automatically.

## Development

```bash
cargo check -p scan-agent -p scanner-sidecar
cargo clippy -p scan-agent -p scanner-sidecar -- -D warnings
cargo test  -p scan-agent
```

Integration tests in `src-tauri/tests/` spin up a real WebSocket server and a
fake sidecar binary — no scanner hardware required.

## Configuration

The agent ships **safe-by-default**: with no configuration, it accepts WebSocket
connections from any `http(s)://localhost`, `127.0.0.1`, or `[::1]` origin (any
port) and rejects everything else. That covers most local-app scenarios — no
edits required.

### When to edit the config file

Edit `%APPDATA%\com.rswebtwain.agent\config.toml` when you need to:

- Allow a production frontend served from a real domain.
- Lock down localhost (set `allow_localhost = false`).
- Change the listening port.

The file is created automatically on first run with every setting commented out.
After editing, restart the agent (right-click tray → Quit, then relaunch).

### Sample `config.toml`

```toml
[server]
# port = 47115

# Whether to accept connections from http(s)://localhost(:any-port), 127.0.0.1, or [::1].
# allow_localhost = true

# Additional exact-match origins (production frontends).
# extra_origins = ["https://app.example.com"]
```

### Environment variables (override config file)

| Variable                       | Description                                                                                                        | Default        |
|--------------------------------|--------------------------------------------------------------------------------------------------------------------|----------------|
| `RSWEBTWAIN_PORT`              | WebSocket listening port                                                                                           | `47115`        |
| `RSWEBTWAIN_ALLOWED_ORIGINS`   | Comma-separated exact-match origins. **Replaces the entire policy** when set (sets `allow_localhost = false`)      | (config value) |
| `RUST_LOG`                     | Logging filter (e.g., `scan_agent=debug`)                                                                          | (off)          |

To keep localhost in the policy via env, list it explicitly:
`RSWEBTWAIN_ALLOWED_ORIGINS="http://localhost:4200,https://app.example.com"`.

## Protocol

All WebSocket frames are JSON with a correlation `id`. Client → agent:
`ping`, `list_scanners`, `start_scan`, `cancel_scan`. Agent → client:
`pong`, `scanner_list`, `scan_progress`, `scan_page`, `scan_complete`,
`error`, `server_shutdown`, `deep_link`. Full enums in
`src-tauri/src/protocol.rs`.

## Contributing

Issues and PRs welcome. Before opening a PR:

- `cargo clippy -- -D warnings` must pass on both crates
- `cargo test -p scan-agent` must pass
- New behaviour should come with a test (the integration tests use a fake
  sidecar so you don't need real hardware)

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual-licensed as above, without any additional terms or
conditions.

The TWAIN specification headers referenced in `src-tauri/src/scanner/twain_ffi.rs`
are derived from the public TWAIN specification at
<https://github.com/twain/twain-specification>.

# RSWebTWAIN Agent Configuration

The agent reads an optional TOML config file at `%APPDATA%\com.rswebtwain.agent\config.toml`.
With no file, it runs with built-in defaults (port 47115, localhost-only origins).

## Schema

```toml
[server]
# Listening port for the WebSocket server.
port = 47115

# Accept connections from http(s)://localhost(:any-port), 127.0.0.1, or [::1].
allow_localhost = true

# Additional exact-match origins (production frontends).
# Each entry must be a full origin including scheme: http://... or https://...
extra_origins = ["https://app.example.com"]
```

All fields are optional. Missing fields use the built-in defaults shown above.

## Deployment scenarios

### 1. Local development / single-machine deployment (default)

No config file required. Serve your Angular app at `http://localhost:4200`
(or any port) and it will connect immediately.

### 2. Production frontend served from a real domain

Edit the config file:

```toml
[server]
extra_origins = ["https://app.example.com"]
```

Localhost still works for local debugging. Restart the agent after saving.

### 3. Production-only — block localhost entirely

```toml
[server]
allow_localhost = false
extra_origins = ["https://app.example.com"]
```

The agent will reject every connection that doesn't match `extra_origins`.

## Environment variable overrides

| Variable                       | Effect                                                                                                            |
|--------------------------------|-------------------------------------------------------------------------------------------------------------------|
| `RSWEBTWAIN_PORT`              | Overrides `server.port`. Invalid values keep the config value and log a warning.                                  |
| `RSWEBTWAIN_ALLOWED_ORIGINS`   | **Replaces the entire origin policy**: sets `allow_localhost = false` and uses the comma-separated list as `extra_origins`. To keep localhost, list it explicitly. |

## Troubleshooting

### Where are the logs?

The agent writes to stdout via `tracing`. Set `RUST_LOG=scan_agent=debug` to
see startup messages including the resolved config path and the active origin
policy.

### Exit code 2

The agent exits with code 2 when the config file is present but invalid (TOML
parse error, port out of range, malformed origin URL). The error message
includes the file path and the specific problem. Fix the file and relaunch.

A missing config file is **not** an error — the agent uses defaults and logs
a one-line note that the template was written.

### Regenerating the template

Delete `%APPDATA%\com.rswebtwain.agent\config.toml` and relaunch the agent.
The next startup writes a fresh commented template.
